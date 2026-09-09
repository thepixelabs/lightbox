// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.dehaze` (E10 tasks **D5-D6**). Dark-channel-prior dehaze (He,
//! Sun, Tang, *Single Image Haze Removal Using Dark Channel Prior*, CVPR
//! 2009 / PAMI 2011) with a guided-filter transmission refine (He & Sun,
//! *Fast Guided Filter*, 2015, the **general/cross** case,
//! [`crate::ng::nodes::common::guided::guided_filter`]). Clean-room from the
//! published papers; no GPL source consulted. Domain: scene-linear working
//! RGB throughout (spec §4.2 "Dehaze | scene-linear RGB, dark-channel prior
//! + transmission map, guided-filter refined").
//!
//! # D5, dark-channel prior, atmospheric light, transmission map
//!
//! 1. **Per-pixel channel-min** ([`channel_min`]): `min(r,g,b)`, floored at
//!    `0` (scene-linear rounding noise can go slightly negative).
//! 2. **Atmospheric light** ([`atmospheric_light`]), the classic paper
//!    selects the top ~0.1% brightest dark-channel pixels then takes the
//!    highest-intensity pixel among them. This module uses a **documented
//!    simplification**: a soft top-k via power-weighting, `weight =
//!    channel_min(x)^`[`ATMO_POWER`]`, so the estimate is a single
//!    weighted average (`Σ weight·I / Σ weight`) rather than needing a
//!    percentile/sort. This is deliberately GPU-parallel-friendly: it is
//!    exactly the reduction [`crate::ng::nodes::global::dehaze`]'s
//!    `pass_atmo` WGSL entry runs as a single-workgroup shared-memory tree
//!    reduction (no multi-pass percentile search), and CPU/GPU compute the
//!    identical weighted sum so parity holds to summation-order rounding.
//! 3. **Transmission map** ([`transmission_raw`]), [`box_min`] (task D5's
//!    "windowed" dark-channel prior, He, Sun, Tang's own eq. 5 windows a
//!    local **patch**, not a single pixel, so a lone dark object narrower
//!    than the window isn't mistaken for haze-free foreground) applied to
//!    the per-pixel channel-min of the **`A`-normalized** image, then `t =
//!    1 - `[`OMEGA`]`·dark'`.
//!
//! # D6, guided-filter refine, recovery, negative-dehaze
//!
//! 4. **Transmission refine** ([`refine_transmission`]), the general/cross
//!    guided filter (guide = scene luma, target = `t_raw`) smooths the raw
//!    transmission's block-filter artifacts (task D6's "no sky
//!    posterization" AC) while still respecting real depth edges, the
//!    guided filter's own edge-preserving property, reused verbatim from
//!    the shared infra rather than reinvented.
//! 5. **Recovery / negative-dehaze** ([`apply_dehaze`]), `dehaze ∈
//!    [-100,100]`: `>= 0` blends toward the physically-recovered
//!    `J = (I-A)/max(t,T_FLOOR) + A` by `amount/100`; `< 0` **adds** haze by
//!    reusing the SAME estimated `t` (screen-blending toward `A` more
//!    strongly where the scene's own transmission is already lower
//!    already-hazy regions get hazier, a documented, self-consistent
//!    simplification rather than a from-scratch synthetic depth model).
//!    **`dehaze == 0` is an EXACT pixel identity by construction**, the
//!    `amount >= 0` branch's `rgb + k·(j-rgb)` is bit-exact `rgb` at `k=0`
//!    regardless of how `j`/`t`/`A` were computed (`0 · finite == 0` in
//!    IEEE-754), so a fragile upstream estimate (e.g. a degenerate all-
//!    black frame) can never leak a non-identity result at `dehaze==0`
//!    ([`tests::identity_at_zero_dehaze_holds_regardless_of_scene`]).
//!
//! GPU (`shaders/global_dehaze.wgsl`) and CPU implement the identical
//! four-stage algorithm from the identical formulas (the established
//! per-node convention), so CPU/GPU parity (§4.4) holds to numerical
//! rounding, see that WGSL file's header for the exact stage-by-stage
//! correspondence.
//!
//! # Known limitation: atmospheric light needs the WHOLE image (named, not
//! silently wrong)
//!
//! Unlike every other E10 spatial node (which only needs a **local**
//! neighborhood, declared via `roi_in`), [`atmospheric_light`] is a
//! genuinely global reduction. This is correct under the engine's current
//! v1 executor, which evaluates every node over the **whole** requested ROI
//! as a single tile (documented precedent:
//! `tone_recovery`'s own module doc, "The production `ng::exec::Executor`
//! v1 walk does not yet exercise \[a real tiled\] path itself"). `roi_in`
//! still declares the correct LOCAL apron for the windowed dark-channel +
//! guided-filter stages (so a future real-tiling landing degrades gracefully
//! rather than silently, for those two stages); the global-reduction gap is
//! recorded in `docs/plan/epics/E10-deviations.md`, mirroring `tone_recovery`'s
//! own B13 precedent for naming this exact class of issue.

use std::sync::Arc;

use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::common::guided::{box_min, guided_filter};
use crate::ng::nodes::global::tone_recovery::{working_luma, working_luma_weights};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The four-pass dehaze kernel, naga-validated at build (`build.rs`).
const DEHAZE_WGSL: &str = include_str!("../../../../shaders/global_dehaze.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "dehaze",
    kind: ParamKind::Float,
}]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.dehaze"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};

// ── algorithm constants (duplicated literally in the WGSL, see that file's
//    header for why; CPU/GPU parity tests catch drift) ─────────────────────

/// Soft top-k power for [`atmospheric_light`]'s weighting.
pub const ATMO_POWER: f32 = 8.0;
/// Windowed dark-channel-prior half-window (He, Sun, Tang's own patch
/// radius, spec D5).
pub const R_DARK: u32 = 7;
/// How much haze to deliberately leave in (He, Sun, Tang's own `ω`,
/// typically `0.95`, keeps a touch of atmospheric depth cue).
pub const OMEGA: f32 = 0.95;
/// Transmission floor for the recovery division (avoids blow-up in
/// dense-haze regions).
pub const T_FLOOR: f32 = 0.1;
/// The guided-filter transmission-refine half-window, also the node's
/// local apron contribution alongside [`R_DARK`] (see [`ClarityNode`]/
/// `tone_recovery`'s own `2*radius` precedent: the guided filter chains two
/// box-filter stages).
///
/// [`ClarityNode`]: crate::ng::nodes::global::clarity::ClarityNode
pub const REFINE_RADIUS: u32 = 8;
/// The guided-filter refine's ridge term.
pub const REFINE_EPS: f32 = 1.0e-3;
/// Negative-dehaze (haze-add) strength calibration.
pub const NEG_STRENGTH: f32 = 1.0;
/// Floor on an atmospheric-light channel before it's used as a divisor.
pub const ATMO_MIN_CHANNEL: f32 = 1.0e-3;

// ── D5: dark-channel prior, atmospheric light, transmission map ───────────

/// Per-pixel channel-min, floored at `0` (scene-linear rounding noise can go
/// slightly negative), He, Sun, Tang's dark-channel prior's innermost `min`.
#[inline]
pub fn channel_min(rgb: [f32; 3]) -> f32 {
    rgb[0].min(rgb[1]).min(rgb[2]).max(0.0)
}

/// The atmospheric-light estimate (task D5), see the module doc's "D5"
/// section for the soft-top-k weighting rationale. Falls back to the plain
/// per-channel mean on a degenerate (all-zero-weight, e.g. all-black) scene
/// rather than dividing by ~0.
pub fn atmospheric_light(pixels: &[[f32; 3]]) -> [f32; 3] {
    let mut sum = [0f64; 3];
    let mut sum_w = 0f64;
    for &p in pixels {
        let m = channel_min(p) as f64;
        let w = m.max(0.0).powf(ATMO_POWER as f64);
        sum[0] += w * p[0] as f64;
        sum[1] += w * p[1] as f64;
        sum[2] += w * p[2] as f64;
        sum_w += w;
    }
    if sum_w > 1.0e-12 {
        [
            (sum[0] / sum_w) as f32,
            (sum[1] / sum_w) as f32,
            (sum[2] / sum_w) as f32,
        ]
    } else {
        let n = (pixels.len().max(1)) as f64;
        let mean = |k: usize| (pixels.iter().map(|p| p[k] as f64).sum::<f64>() / n) as f32;
        [mean(0), mean(1), mean(2)]
    }
}

/// The raw (pre-refine) transmission map (task D5): windowed dark-channel
/// prior of the `A`-normalized image, `t = 1 - `[`OMEGA`]`·dark'`, clamped to
/// `[0,1]`.
pub fn transmission_raw(pixels: &[[f32; 3]], w: u32, h: u32, a: [f32; 3]) -> Vec<f32> {
    let ac = [
        a[0].max(ATMO_MIN_CHANNEL),
        a[1].max(ATMO_MIN_CHANNEL),
        a[2].max(ATMO_MIN_CHANNEL),
    ];
    let mn: Vec<f32> = pixels
        .iter()
        .map(|p| channel_min([p[0] / ac[0], p[1] / ac[1], p[2] / ac[2]]))
        .collect();
    let dark_prime = box_min(&mn, w, h, R_DARK);
    dark_prime
        .iter()
        .map(|&d| (1.0 - OMEGA * d).clamp(0.0, 1.0))
        .collect()
}

// ── D6: guided-filter refine, recovery, negative-dehaze ────────────────────

/// The transmission refine (task D6): the general/cross guided filter
/// (guide = scene luma, target = `t_raw`), recombined `t = mean_a·luma +
/// mean_b`.
pub fn refine_transmission(t_raw: &[f32], guide_luma: &[f32], w: u32, h: u32) -> Vec<f32> {
    let (mean_a, mean_b) = guided_filter(guide_luma, t_raw, w, h, REFINE_RADIUS, REFINE_EPS);
    mean_a
        .iter()
        .zip(mean_b.iter())
        .zip(guide_luma.iter())
        .map(|((&a, &b), &l)| a * l + b)
        .collect()
}

/// The full per-pixel recovery/haze-add op (task D6), see the module doc's
/// "D6" section. The CPU parity anchor for `global_dehaze.wgsl`'s
/// `pass_guided_c`.
#[inline]
pub fn apply_dehaze(rgb: [f32; 3], a: [f32; 3], t: f32, amount: f32) -> [f32; 3] {
    let out = if amount >= 0.0 {
        let k = (amount / 100.0).clamp(0.0, 1.0);
        let t_eff = t.max(T_FLOOR);
        let j = [
            (rgb[0] - a[0]) / t_eff + a[0],
            (rgb[1] - a[1]) / t_eff + a[1],
            (rgb[2] - a[2]) / t_eff + a[2],
        ];
        [
            rgb[0] + k * (j[0] - rgb[0]),
            rgb[1] + k * (j[1] - rgb[1]),
            rgb[2] + k * (j[2] - rgb[2]),
        ]
    } else {
        let k = (-amount / 100.0).clamp(0.0, 1.0);
        let t_add = (1.0 - k * NEG_STRENGTH * (1.0 - t)).clamp(T_FLOOR, 1.0);
        [
            a[0] + (rgb[0] - a[0]) * t_add,
            a[1] + (rgb[1] - a[1]) * t_add,
            a[2] + (rgb[2] - a[2]) * t_add,
        ]
    };
    if out.iter().all(|v| v.is_finite()) {
        out
    } else {
        rgb
    }
}

/// The `global.dehaze` develop node.
#[derive(Default)]
pub struct DehazeNode {}

impl DehazeNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.dehaze");

    /// A fresh node.
    pub fn new() -> DehazeNode {
        DehazeNode::default()
    }
}

impl RenderNode for DehazeNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        // R_DARK (the transmission windowed-min) + 2*REFINE_RADIUS (the
        // guided-filter refine's two chained box-filter stages), see the
        // module doc's apron derivation.
        InputRois(vec![out.expand(R_DARK + 2 * REFINE_RADIUS); 1])
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("global.dehaze: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.dehaze: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let amount = params.get_f64_or("dehaze", 0.0) as f32;
        let weights = working_luma_weights();
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&weights[0].to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&weights[1].to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&weights[2].to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&amount.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.dehaze params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        // Scratch textures: 1x1 atmospheric light, full-res transmission,
        // full-res guided-filter (a,b), all f32 (numerically stiff
        // regression coefficients, same rationale `tone_recovery`'s own
        // scratch texture doc comment gives).
        let mk_scratch = |label: &str, w: u32, h: u32| {
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let atmo_tex = mk_scratch("global.dehaze atmo", 1, 1);
        let atmo_view = atmo_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let trans_tex = mk_scratch("global.dehaze trans", extent.w, extent.h);
        let trans_view = trans_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let ab_tex = mk_scratch("global.dehaze ab", extent.w, extent.h);
        let ab_view = ab_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // pass_atmo: ONE workgroup regardless of image size.
        let pipe_atmo = ctx.kernels.compute_pipeline(DEHAZE_WGSL, "pass_atmo")?;
        let bg0_atmo = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_atmo in"),
            layout: &pipe_atmo.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg1_atmo = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_atmo out"),
            layout: &pipe_atmo.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&atmo_view),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_atmo,
            &[&bg0_atmo, &bg1_atmo],
            Extent { w: 1, h: 1 },
            "global.dehaze.pass_atmo",
        );

        // pass_trans.
        let pipe_trans = ctx.kernels.compute_pipeline(DEHAZE_WGSL, "pass_trans")?;
        let bg0_trans = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_trans in"),
            layout: &pipe_trans.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atmo_view),
                },
            ],
        });
        let bg1_trans = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_trans out"),
            layout: &pipe_trans.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&trans_view),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_trans,
            &[&bg0_trans, &bg1_trans],
            extent,
            "global.dehaze.pass_trans",
        );

        // pass_guided_a.
        let pipe_ga = ctx.kernels.compute_pipeline(DEHAZE_WGSL, "pass_guided_a")?;
        let bg0_ga = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_a in"),
            layout: &pipe_ga.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&trans_view),
                },
            ],
        });
        let bg1_ga = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_a out"),
            layout: &pipe_ga.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&ab_view),
            }],
        });
        let bg2_ga = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_a params"),
            layout: &pipe_ga.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_ga,
            &[&bg0_ga, &bg1_ga, &bg2_ga],
            extent,
            "global.dehaze.pass_guided_a",
        );

        // pass_guided_c.
        let pipe_gc = ctx.kernels.compute_pipeline(DEHAZE_WGSL, "pass_guided_c")?;
        let bg0_gc = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_c in"),
            layout: &pipe_gc.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atmo_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&ab_view),
                },
            ],
        });
        let bg1_gc = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_c out"),
            layout: &pipe_gc.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg2_gc = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.dehaze pass_guided_c params"),
            layout: &pipe_gc.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_gc,
            &[&bg0_gc, &bg1_gc, &bg2_gc],
            extent,
            "global.dehaze.pass_guided_c",
        );

        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input_view = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.dehaze: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let amount = params.get_f64_or("dehaze", 0.0) as f32;
        let weights = working_luma_weights();

        let (iw, ih) = (input.extent.w, input.extent.h);
        let n = (iw as usize) * (ih as usize);
        let mut rgb_plane: Vec<[f32; 3]> = Vec::with_capacity(n);
        let mut luma_plane = vec![0f32; n];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                let rgb = [p[0], p[1], p[2]];
                luma_plane[(y * iw + x) as usize] = working_luma(rgb, weights);
                rgb_plane.push(rgb);
            }
        }
        let a_light = atmospheric_light(&rgb_plane);
        let t_raw = transmission_raw(&rgb_plane, iw, ih, a_light);
        let t_refined = refine_transmission(&t_raw, &luma_plane, iw, ih);

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        let ox = out_roi.x as i64 - in_roi.x as i64;
        let oy = out_roi.y as i64 - in_roi.y as i64;
        let ix_max = iw as i64 - 1;
        let iy_max = ih as i64 - 1;
        out.par_fill_rows(|ly, row| {
            let iy = (oy + ly as i64).clamp(0, iy_max) as u32;
            for lx in 0..w {
                let ix = (ox + lx as i64).clamp(0, ix_max) as u32;
                let p = input.get_rgba_f32(ix, iy);
                let i = (iy * iw + ix) as usize;
                let rgb_out = apply_dehaze([p[0], p[1], p[2]], a_light, t_refined[i], amount);
                let o = [rgb_out[0], rgb_out[1], rgb_out[2], p[3]];
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for DehazeNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.presence.dehaze == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([("dehaze", ParamValue::Float(p.presence.dehaze as f64))])
            .expect("dehaze is always finite (clamped on ingest — spec A2)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(R_DARK + 2 * REFINE_RADIUS)
    }
}

/// Factory registering [`DehazeNode`].
#[derive(Default)]
pub struct DehazeFactory {}

impl NodeFactory for DehazeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(DehazeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(DEHAZE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── D5: dark channel / atmospheric light / transmission, unit tests
    //    on synthetic haze (task D5 AC) ──────────────────────────────────

    #[test]
    fn channel_min_floors_at_zero() {
        assert_eq!(channel_min([0.5, 0.3, 0.8]), 0.3);
        assert_eq!(channel_min([-0.1, 0.2, 0.3]), 0.0);
    }

    #[test]
    fn atmospheric_light_of_a_flat_field_is_that_color() {
        let pixels = vec![[0.7f32, 0.75, 0.8]; 64];
        let a = atmospheric_light(&pixels);
        for c in 0..3 {
            assert!((a[c] - pixels[0][c]).abs() < 1e-4, "a={a:?}");
        }
    }

    /// A synthetic hazy scene: a dark foreground object surrounded by a
    /// bright, low-chroma "sky" (the airlight-dominated region), the
    /// atmospheric light must land near the sky color, not the dark
    /// foreground (task D5's synthetic-haze AC).
    #[test]
    fn atmospheric_light_favors_the_bright_hazy_region_over_dark_foreground() {
        let (w, h) = (32u32, 32u32);
        let sky = [0.85f32, 0.87, 0.9];
        let dark = [0.05f32, 0.06, 0.05];
        let mut pixels = vec![sky; (w * h) as usize];
        // A dark "foreground object" block.
        for y in 10..20 {
            for x in 10..20 {
                pixels[(y * w + x) as usize] = dark;
            }
        }
        let a = atmospheric_light(&pixels);
        println!("[D5] atmospheric light estimate={a:?} (sky={sky:?}, dark={dark:?})");
        for c in 0..3 {
            assert!(
                (a[c] - sky[c]).abs() < 0.1,
                "A[{c}]={} should be near the sky color {}, not the dark foreground",
                a[c],
                sky[c]
            );
        }
    }

    /// Synthetic haze: a scene with a real (haze-free) transmission of 1
    /// everywhere blended toward atmospheric light by a KNOWN transmission
    /// profile (the classic haze image formation model `I = J*t + A*(1-t)`)
    /// the recovered transmission map must correlate with the known
    /// profile (low where haze was heavy, high where the scene was clear).
    #[test]
    fn transmission_map_is_lower_where_synthetic_haze_is_heavier() {
        let (w, h) = (48u32, 24u32);
        let j = [0.15f32, 0.5, 0.2]; // the "true" haze-free scene color
        let a = [0.9f32, 0.9, 0.9]; // atmospheric light
                                    // Left half: heavy haze (t=0.2); right half: light haze (t=0.8).
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for _y in 0..h {
            for x in 0..w {
                let t = if x < w / 2 { 0.2f32 } else { 0.8f32 };
                let i = [
                    j[0] * t + a[0] * (1.0 - t),
                    j[1] * t + a[1] * (1.0 - t),
                    j[2] * t + a[2] * (1.0 - t),
                ];
                pixels.push(i);
            }
        }
        let a_est = atmospheric_light(&pixels);
        let t_raw = transmission_raw(&pixels, w, h, a_est);
        // Sample well inside each half (away from the R_DARK-window seam).
        let heavy_sample = t_raw[(h / 2 * w + 5) as usize];
        let light_sample = t_raw[(h / 2 * w + (w - 5)) as usize];
        println!(
            "[D5] transmission: heavy-haze-side={heavy_sample:.3} light-haze-side={light_sample:.3}"
        );
        assert!(
            heavy_sample < light_sample,
            "heavier synthetic haze must yield a LOWER transmission estimate: \
             heavy={heavy_sample:.3} light={light_sample:.3}"
        );
    }

    #[test]
    fn transmission_raw_never_produces_nan_or_out_of_range() {
        let (w, h) = (16u32, 16u32);
        let pixels: Vec<[f32; 3]> = (0..(w * h))
            .map(|i| {
                let v = (i % 7) as f32 / 7.0;
                [v, v * 0.5, v * 1.5]
            })
            .collect();
        let a = atmospheric_light(&pixels);
        let t = transmission_raw(&pixels, w, h, a);
        assert!(t.iter().all(|&v| v.is_finite() && (0.0..=1.0).contains(&v)));
    }

    // ── D6: refine + recovery + negative-dehaze ────────────────────────

    #[test]
    fn zero_dehaze_is_exact_identity() {
        let rgb = [0.3f32, 0.6, 0.9];
        let a = [0.8f32, 0.8, 0.8];
        for t in [0.0f32, 0.1, 0.5, 1.0] {
            let o = apply_dehaze(rgb, a, t, 0.0);
            assert_eq!(o, rgb, "t={t}");
        }
    }

    /// The identity contract holds even when `t`/`A` come from a degenerate
    /// scene, the whole point of the "structural, not coincidental"
    /// identity the module doc claims.
    #[test]
    fn identity_at_zero_dehaze_holds_regardless_of_scene() {
        let rgb = [1.5f32, -0.2, 3.0]; // out-of-gamut too
        for (a, t) in [
            ([0.0f32, 0.0, 0.0], 0.0f32),
            ([1.0f32, 1.0, 1.0], 1.0f32),
            (atmospheric_light(&[[0.0, 0.0, 0.0]]), 0.5f32),
        ] {
            let o = apply_dehaze(rgb, a, t, 0.0);
            assert_eq!(o, rgb, "a={a:?} t={t}");
        }
    }

    #[test]
    fn positive_dehaze_pulls_toward_the_recovered_scene() {
        let rgb = [0.6f32, 0.6, 0.6]; // a hazy mid-gray
        let a = [0.9f32, 0.9, 0.9]; // bright atmospheric light
        let t = 0.4f32;
        let recovered = apply_dehaze(rgb, a, t, 100.0);
        let half = apply_dehaze(rgb, a, t, 50.0);
        // Full recovery should darken/saturate away from the airlight-mixed
        // gray more than the half-strength version does.
        let full_shift = (recovered[0] - rgb[0]).abs();
        let half_shift = (half[0] - rgb[0]).abs();
        println!("[D6] full_shift={full_shift:.4} half_shift={half_shift:.4}");
        assert!(full_shift > half_shift, "100% must move more than 50%");
        assert!(full_shift > 0.01, "recovery must visibly move the pixel");
    }

    #[test]
    fn negative_dehaze_adds_haze_toward_atmospheric_light() {
        let rgb = [0.2f32, 0.3, 0.15]; // a clear, saturated scene color
        let a = [0.9f32, 0.9, 0.9];
        let t = 0.9f32; // a clear (low-haze) transmission
        let hazy = apply_dehaze(rgb, a, t, -100.0);
        // Adding haze must move the pixel TOWARD the atmospheric light.
        for c in 0..3 {
            assert!(
                hazy[c] > rgb[c],
                "channel {c}: hazy={hazy:?} must move toward A={a:?} from rgb={rgb:?}"
            );
            assert!(hazy[c] <= a[c] + 1e-4, "must not overshoot past A");
        }
    }

    #[test]
    fn negative_dehaze_at_max_never_produces_nan() {
        for rgb in [[0.0f32, 0.0, 0.0], [1.0f32, 1.0, 1.0], [2.0f32, -0.5, 0.3]] {
            let o = apply_dehaze(rgb, [0.8, 0.8, 0.8], 0.5, -100.0);
            assert!(o.iter().all(|v| v.is_finite()));
        }
    }

    // ── D6 AC: no sky posterization (transmission-map smoothness) ─────

    /// The raw (unrefined) windowed transmission map has visible block/
    /// plateau structure over a smooth gradient "sky" (a `box_min` window's
    /// well-known staircase artifact, the "posterization" risk the D6 AC
    /// names); the guided-filter-REFINED map must be measurably smoother
    /// there (lower discrete second-derivative, the same "no banding"
    /// smoothness proof `hsl.rs`'s C8 test uses) while still tracking a
    /// real depth edge elsewhere in the same scene.
    #[test]
    fn refined_transmission_is_smoother_than_raw_over_a_gradient_sky() {
        let (w, h) = (80u32, 20u32);
        let a = [0.95f32, 0.95, 0.95];
        // A smooth "sky" gradient (transmission increasing left->right,
        // scene color constant) PLUS a hard depth edge at the very right
        // edge (a sudden foreground object) so the refine's edge-preserving
        // property is exercised in the same scene.
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for _y in 0..h {
            for x in 0..w {
                let t = if x < w - 8 {
                    0.2 + 0.7 * (x as f32 / (w - 8) as f32)
                } else {
                    0.05 // a sudden, deep-haze foreground object
                };
                let j = [0.3f32, 0.4, 0.5];
                pixels.push([
                    j[0] * t + a[0] * (1.0 - t),
                    j[1] * t + a[1] * (1.0 - t),
                    j[2] * t + a[2] * (1.0 - t),
                ]);
            }
        }
        let weights = working_luma_weights();
        let luma: Vec<f32> = pixels.iter().map(|&p| working_luma(p, weights)).collect();
        let t_raw = transmission_raw(&pixels, w, h, a);
        let t_refined = refine_transmission(&t_raw, &luma, w, h);

        fn max_abs_second_diff_row(v: &[f32], w: u32, y: u32, x0: u32, x1: u32) -> f32 {
            let mut m = 0.0f32;
            for x in (x0 + 1)..x1.min(w - 1) {
                let i = (y * w + x) as usize;
                let d2 = (v[i + 1] - 2.0 * v[i] + v[i - 1]).abs();
                m = m.max(d2);
            }
            m
        }
        // Measure roughness over the smooth-gradient region only (excluding
        // the last ~10px where the real depth edge legitimately produces a
        // large second derivative in BOTH raw and refined).
        let y = h / 2;
        let raw_roughness = max_abs_second_diff_row(&t_raw, w, y, 2, w - 12);
        let refined_roughness = max_abs_second_diff_row(&t_refined, w, y, 2, w - 12);
        println!(
            "[D6][sky-smoothness] raw max|d2t|={raw_roughness:.5} refined max|d2t|={refined_roughness:.5}"
        );
        assert!(
            refined_roughness < raw_roughness,
            "guided-filter refine must smooth the raw transmission's block artifacts: \
             raw={raw_roughness:.5} refined={refined_roughness:.5}"
        );

        // The refine must still respond to the real depth edge: refined
        // transmission near the foreground object (right edge) must be
        // meaningfully lower than the sky region right before it.
        let sky_edge = t_refined[(y * w + (w - 14)) as usize];
        let object = t_refined[(y * w + (w - 2)) as usize];
        println!("[D6][edge] sky_edge_t={sky_edge:.3} object_t={object:.3}");
        assert!(
            object < sky_edge,
            "refine must still track the real depth edge: object={object:.3} sky_edge={sky_edge:.3}"
        );
    }

    // ── node wiring ─────────────────────────────────────────────────────

    #[test]
    fn is_identity_tracks_the_dehaze_field() {
        let mut g = GlobalStages::default();
        assert!(DehazeNode::is_identity(&g));
        g.presence.dehaze = 20.0;
        assert!(!DehazeNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_dehaze() {
        let g = GlobalStages {
            presence: lightbox_edit::Presence {
                dehaze: -55.0,
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = DehazeNode::param_block(&g);
        assert_eq!(pb.get_f64("dehaze"), Some(-55.0));
    }

    #[test]
    fn plan_pads_by_r_dark_plus_two_refine_radii() {
        let node = DehazeNode::new();
        let out = Roi {
            x: 5,
            y: 5,
            w: 50,
            h: 50,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        let rois = node.plan(out, 1.0, &params);
        assert_eq!(rois.0[0], out.expand(R_DARK + 2 * REFINE_RADIUS));
    }
}
