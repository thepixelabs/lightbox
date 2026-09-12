// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.noise_reduction`, the Detail panel's noise half: luminance and
//! chroma denoise with Lightroom's four controls, `luma`, `luma_detail`,
//! `chroma` and `chroma_detail` ([`lightbox_edit::NoiseReduction`], whose
//! ranges and neutral this node is built to and does not change).
//!
//! # What this implementation actually is
//!
//! **It is two joint bilateral filters over a luma/chroma decomposition. It
//! is not Adobe's algorithm and it does not claim parity with it.** Their
//! slider denoise is publicly described as multi-scale (wavelet-shaped) and
//! their "AI Denoise" is, by Adobe's own description, a neural network;
//! neither is what runs here, and their implementations are closed, so this
//! comment claims nothing further about them. A bilateral filter is the
//! honest baseline this
//! node's brief names: it is edge-aware, so it is emphatically not the naive
//! box blur that would make the control worse than useless, but it does not
//! separate noise from fine texture as cleanly as a multi-scale method, and
//! at high strength on a low-ISO file it will read as slightly plasticky.
//! That is the known limitation, stated rather than hidden.
//!
//! Concretely, per pixel:
//!
//! 1. **Decompose.** Companion-encode each channel
//!    (`srgb_oetf(clamp(c, 0, 1))`, [`lightbox_color::matrix::spaces::companion_encode`],
//!    the same companion domain every other node in this directory uses),
//!    then `y = dot(e, luma_weights)`, `cb = e.b - y`, `cr = e.r - y`, a
//!    plain opponent triple. See [`ycc`].
//! 2. **Chroma, the wide filter.** A joint bilateral over `(cb, cr)` with
//!    spatial half-window [`CHROMA_RADIUS`] and a range term on the chroma
//!    distance. Chroma noise is the ugly coloured blotching, it is
//!    low-frequency, and human vision carries almost no chroma acuity, which
//!    is exactly why a wide edge-aware average removes it convincingly at a
//!    cost luminance denoising can never match. This is why the chroma
//!    window is the larger of the two.
//! 3. **Luma, the narrow filter.** A bilateral over `y` with half-window
//!    [`LUMA_RADIUS`] and a range term on the luma difference.
//! 4. **The two `*_detail` controls set the range sigma**, interpolating
//!    from `*_RANGE_SIGMA_MAX` at detail `0` (smooth hard, cross more edges)
//!    to `*_RANGE_SIGMA_MIN` at detail `100` (only near-identical neighbours
//!    average, detail survives). This is the same direction Lightroom's
//!    detail sliders run in; the numbers are this implementation's own
//!    calibration, not Adobe's.
//! 5. **The two strength controls are the blend**: `out = mix(orig,
//!    filtered, strength/100)`, per channel group. `strength == 0` is
//!    therefore an exact identity, which is what lets [`GlobalNode::is_identity`]
//!    elide the node on the two strengths alone.
//!
//! Both filters accumulate in ONE window loop on both backends (the luma
//! window is a strict sub-window of the chroma window), the same single-pass
//! trick [`crate::ng::nodes::global::texture`] uses for its two box radii.
//!
//! Reapply is [`crate::ng::nodes::global::clarity`]'s local-derivative
//! additive delta, extended to three independent per-channel deltas because
//! this node moves chroma as well as luma: the `(dy, dcb, dcr)` triple is
//! turned into per-channel ENCODED deltas whose luma-weighted sum is exactly
//! `dy` (so a chroma move never drags luminance with it), and each is scaled
//! by `d(srgb_eotf)/de` at that channel's own encoded value. See
//! [`apply_nr`].
//!
//! # Cost
//!
//! Brute-force windows, `(2*CHROMA_RADIUS+1)^2` taps per pixel, the same
//! M1-provisional choice `clarity`/`tone_recovery` already make (see
//! [`crate::ng::nodes::common::guided`]'s module doc, which names the
//! downsampled fast path as the deferred perf lever). Correct first, fast
//! later; there is no approximation hiding in the cost.
//!
//! GPU (`shaders/global_noise_reduction.wgsl`) and CPU ([`apply_nr`] over
//! [`bilateral_ycc`]) implement the identical algorithm from the identical
//! formulas, so CPU/GPU parity holds to numerical rounding
//! (`tests/e10_detail.rs`).

use std::sync::Arc;

use lightbox_color::matrix::spaces::companion_encode;
use lightbox_edit::{GlobalStages, NoiseReduction};
use rayon::prelude::*;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::global::clarity::srgb_eotf_deriv;
use crate::ng::nodes::global::tone_recovery::working_luma_weights;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The single-pass joint-bilateral kernel, naga-validated at build
/// (`build.rs`).
const NR_WGSL: &str = include_str!("../../../../shaders/global_noise_reduction.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "luma",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "luma_detail",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "chroma",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "chroma_detail",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.noise_reduction"),
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
//    header for why; the CPU/GPU parity test catches drift) ────────────────

/// Luminance spatial half-window, pixels.
pub const LUMA_RADIUS: u32 = 3;
/// Chroma spatial half-window, pixels. Wider than the luma one on purpose
/// (see this module's doc comment) and therefore the node's apron.
pub const CHROMA_RADIUS: u32 = 5;
/// Luminance spatial Gaussian sigma, roughly half [`LUMA_RADIUS`] so the
/// window's corner taps still carry meaningful weight.
pub const LUMA_SPATIAL_SIGMA: f32 = 1.6;
/// Chroma spatial Gaussian sigma, roughly half [`CHROMA_RADIUS`].
pub const CHROMA_SPATIAL_SIGMA: f32 = 2.6;
/// Luminance range sigma at `luma_detail = 0` (smooth hard).
pub const LUMA_RANGE_SIGMA_MAX: f32 = 0.060;
/// Luminance range sigma at `luma_detail = 100` (preserve detail).
pub const LUMA_RANGE_SIGMA_MIN: f32 = 0.006;
/// Chroma range sigma at `chroma_detail = 0`.
pub const CHROMA_RANGE_SIGMA_MAX: f32 = 0.120;
/// Chroma range sigma at `chroma_detail = 100`.
pub const CHROMA_RANGE_SIGMA_MIN: f32 = 0.010;

/// `a + (b - a) * t`, WGSL's `mix` with `t` clamped, so the CPU reference and
/// the kernel interpolate the range sigmas identically.
#[inline]
fn mix01(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// The luminance range sigma for a `luma_detail` slider position.
#[inline]
pub fn luma_range_sigma(luma_detail: f32) -> f32 {
    mix01(
        LUMA_RANGE_SIGMA_MAX,
        LUMA_RANGE_SIGMA_MIN,
        luma_detail / 100.0,
    )
}

/// The chroma range sigma for a `chroma_detail` slider position.
#[inline]
pub fn chroma_range_sigma(chroma_detail: f32) -> f32 {
    mix01(
        CHROMA_RANGE_SIGMA_MAX,
        CHROMA_RANGE_SIGMA_MIN,
        chroma_detail / 100.0,
    )
}

/// The opponent triple this node filters in: companion-encode each channel,
/// then `[y, cb, cr] = [dot(e, weights), e.b - y, e.r - y]`. The single
/// source of truth both [`bilateral_ycc`] and the kernel's `ycc_of` compute.
#[inline]
pub fn ycc(rgb: [f32; 3], weights: [f32; 3]) -> [f32; 3] {
    let e = companion_encode(rgb);
    let y = e[0] * weights[0] + e[1] * weights[1] + e[2] * weights[2];
    [y, e[2] - y, e[0] - y]
}

/// The joint bilateral filter over a `w`x`h` plane of [`ycc`] triples: the
/// narrow luma filter and the wide chroma filter in one window walk, exactly
/// as `global_noise_reduction.wgsl`'s single dispatch does. Border is
/// clamp-to-edge, the same rule the rest of this directory uses.
///
/// Returns the FILTERED triples, not the blended result; the strength
/// sliders are applied per pixel by [`apply_nr`], which is what keeps a zero
/// strength an exact identity rather than a near-identity.
pub fn bilateral_ycc(plane: &[[f32; 3]], w: u32, h: u32, nr: NoiseReduction) -> Vec<[f32; 3]> {
    debug_assert_eq!(plane.len(), (w as usize) * (h as usize));
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let sl = luma_range_sigma(nr.luma_detail);
    let sc = chroma_range_sigma(nr.chroma_detail);
    let inv_l_range = 1.0 / (2.0 * sl * sl);
    let inv_c_range = 1.0 / (2.0 * sc * sc);
    let inv_l_spatial = 1.0 / (2.0 * LUMA_SPATIAL_SIGMA * LUMA_SPATIAL_SIGMA);
    let inv_c_spatial = 1.0 / (2.0 * CHROMA_SPATIAL_SIGMA * CHROMA_SPATIAL_SIGMA);

    let (lr, cr) = (LUMA_RADIUS as i64, CHROMA_RADIUS as i64);
    let (wi, hi) = (w as i64, h as i64);
    let mut out = vec![[0.0f32; 3]; plane.len()];
    out.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out_px) in row.iter_mut().enumerate() {
                let center = plane[y * w as usize + x];
                let mut acc_y = 0.0f32;
                let mut sum_l = 0.0f32;
                let mut acc_cb = 0.0f32;
                let mut acc_cr = 0.0f32;
                let mut sum_c = 0.0f32;
                for dy in -cr..=cr {
                    let sy = (y as i64 + dy).clamp(0, hi - 1);
                    for dx in -cr..=cr {
                        let sx = (x as i64 + dx).clamp(0, wi - 1);
                        let tap = plane[(sy * wi + sx) as usize];
                        let d2 = (dx * dx + dy * dy) as f32;

                        let dcb = tap[1] - center[1];
                        let dcr = tap[2] - center[2];
                        let wc = (-d2 * inv_c_spatial).exp()
                            * (-(dcb * dcb + dcr * dcr) * inv_c_range).exp();
                        acc_cb += wc * tap[1];
                        acc_cr += wc * tap[2];
                        sum_c += wc;

                        if dx.abs() <= lr && dy.abs() <= lr {
                            let dl = tap[0] - center[0];
                            let wl = (-d2 * inv_l_spatial).exp() * (-dl * dl * inv_l_range).exp();
                            acc_y += wl * tap[0];
                            sum_l += wl;
                        }
                    }
                }
                // The center tap weighs `exp(0) * exp(0) == 1` exactly, so
                // both sums are >= 1 and neither division can be by zero.
                *out_px = [acc_y / sum_l, acc_cb / sum_c, acc_cr / sum_c];
            }
        });
    out
}

/// The full per-pixel op: given a working RGBA pixel, its [`ycc`] triple, the
/// [`bilateral_ycc`] result at the same pixel, and the luma weights, returns
/// the denoised RGBA (alpha untouched). The CPU parity anchor for
/// `global_noise_reduction.wgsl`'s tail.
///
/// **Why per-channel local-derivative deltas.** The scalar version of this
/// argument is [`crate::ng::nodes::global::clarity::apply_clarity`]'s (read
/// that first): a full `companion_encode -> adjust -> companion_decode`
/// round trip is lossy for out-of-gamut scene-linear pixels, so a neutral
/// node would not be an exact identity. Here the same trick runs three
/// times, once per channel. The encoded deltas are
///
/// ```text
/// de_r = dy + dcr
/// de_b = dy + dcb
/// de_g = dy - (w_r * dcr + w_b * dcb) / w_g
/// ```
///
/// which satisfies `dot(de, weights) == dy` exactly (the working-space luma
/// weights sum to 1), so a pure chroma move carries no luminance with it.
/// Each `de` is then scaled by `d(srgb_eotf)/de` at that channel's own
/// encoded value. Every `d*` is exactly `0` when its strength slider is `0`,
/// and `0 * finite == 0` in IEEE-754, so the neutral node is a bit-exact
/// identity for every input including out-of-gamut ones
/// ([`tests::identity_at_zero_strength_holds_for_out_of_gamut_pixels`]).
#[inline]
pub fn apply_nr(
    rgba: [f32; 4],
    center: [f32; 3],
    filtered: [f32; 3],
    weights: [f32; 3],
    nr: NoiseReduction,
) -> [f32; 4] {
    let k_luma = (nr.luma / 100.0).clamp(0.0, 1.0);
    let k_chroma = (nr.chroma / 100.0).clamp(0.0, 1.0);
    let d_y = (filtered[0] - center[0]) * k_luma;
    let d_cb = (filtered[1] - center[1]) * k_chroma;
    let d_cr = (filtered[2] - center[2]) * k_chroma;

    let de_r = d_y + d_cr;
    let de_b = d_y + d_cb;
    let de_g = d_y - (weights[0] * d_cr + weights[2] * d_cb) / weights[1];

    let e = companion_encode([rgba[0], rgba[1], rgba[2]]);
    let out = [
        rgba[0] + de_r * srgb_eotf_deriv(e[0]),
        rgba[1] + de_g * srgb_eotf_deriv(e[1]),
        rgba[2] + de_b * srgb_eotf_deriv(e[2]),
        rgba[3],
    ];
    if !out[0].is_finite() || !out[1].is_finite() || !out[2].is_finite() {
        return rgba;
    }
    out
}

/// The `global.noise_reduction` develop node.
#[derive(Default)]
pub struct NoiseReductionNode {}

impl NoiseReductionNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("global.noise_reduction");

    /// A fresh node.
    pub fn new() -> NoiseReductionNode {
        NoiseReductionNode::default()
    }
}

fn amounts_from(params: &ParamBlock) -> NoiseReduction {
    NoiseReduction {
        luma: params.get_f64_or("luma", 0.0) as f32,
        luma_detail: params.get_f64_or("luma_detail", 0.0) as f32,
        chroma: params.get_f64_or("chroma", 0.0) as f32,
        chroma_detail: params.get_f64_or("chroma_detail", 0.0) as f32,
    }
}

impl RenderNode for NoiseReductionNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois(vec![out.expand(CHROMA_RADIUS); 1])
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
            .ok_or_else(|| NodeError::Other("global.noise_reduction: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| {
                NodeError::Gpu("global.noise_reduction: output is not a GPU tile".into())
            })?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let nr = amounts_from(params);
        let weights = working_luma_weights();
        let mut ubo_bytes = [0u8; 32];
        for (i, v) in [
            weights[0],
            weights[1],
            weights[2],
            nr.luma,
            nr.luma_detail,
            nr.chroma,
            nr.chroma_detail,
            0.0,
        ]
        .iter()
        .enumerate()
        {
            ubo_bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.noise_reduction params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(NR_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.noise_reduction in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.noise_reduction out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.noise_reduction params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params],
            extent,
            "global.noise_reduction",
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
            .ok_or_else(|| NodeError::Cpu("global.noise_reduction: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let nr = amounts_from(params);
        let weights = working_luma_weights();

        let (iw, ih) = (input.extent.w, input.extent.h);
        let mut plane = vec![[0.0f32; 3]; (iw as usize) * (ih as usize)];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                plane[(y * iw + x) as usize] = ycc([p[0], p[1], p[2]], weights);
            }
        }
        let filtered = bilateral_ycc(&plane, iw, ih, nr);

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
                let o = apply_nr(p, plane[i], filtered[i], weights, nr);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for NoiseReductionNode {
    /// Neutral is `luma == 0 && chroma == 0`. The two `*_detail` sliders are
    /// pure modifiers of a filter whose blend weight is zero, so a recipe
    /// carrying a detail setting with no strength is still a bit-exact pixel
    /// identity (proven by
    /// [`tests::zero_strength_is_exact_identity_at_any_detail`]) and the node
    /// elides rather than paying for a filter nobody asked for.
    fn is_identity(p: &GlobalStages) -> bool {
        p.detail.nr.luma == 0.0 && p.detail.nr.chroma == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let nr = p.detail.nr;
        ParamBlock::from_fields([
            ("luma", ParamValue::Float(nr.luma as f64)),
            ("luma_detail", ParamValue::Float(nr.luma_detail as f64)),
            ("chroma", ParamValue::Float(nr.chroma as f64)),
            ("chroma_detail", ParamValue::Float(nr.chroma_detail as f64)),
        ])
        .expect("noise-reduction fields are always finite (clamped on ingest)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(CHROMA_RADIUS)
    }
}

/// Factory registering [`NoiseReductionNode`].
#[derive(Default)]
pub struct NoiseReductionFactory {}

impl NodeFactory for NoiseReductionFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(NoiseReductionNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(NR_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(nr: NoiseReduction) -> GlobalStages {
        GlobalStages {
            detail: lightbox_edit::Detail {
                nr,
                ..Default::default()
            },
            ..GlobalStages::default()
        }
    }

    /// A deterministic xorshift PRNG. No new crate dependency for a test's
    /// synthetic noise, the same choice `texture.rs`'s own tests make.
    struct XorShift64(u64);
    impl XorShift64 {
        fn next_unit(&mut self) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            ((x >> 40) as f32) / (1u64 << 24) as f32
        }
        /// Uniform noise with the requested standard deviation.
        fn next_noise(&mut self, sigma: f32) -> f32 {
            (self.next_unit() * 2.0 - 1.0) * 3.0f32.sqrt() * sigma
        }
    }

    fn std_dev(v: &[f32]) -> f64 {
        let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
        (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
    }

    // ── is_identity / param_block ────────────────────────────────────────

    #[test]
    fn is_identity_tracks_the_two_strengths_only() {
        assert!(NoiseReductionNode::is_identity(&GlobalStages::default()));

        for nr in [
            NoiseReduction {
                luma: 20.0,
                ..Default::default()
            },
            NoiseReduction {
                chroma: 20.0,
                ..Default::default()
            },
        ] {
            assert!(!NoiseReductionNode::is_identity(&stages(nr)), "{nr:?}");
        }

        // The detail sliders modify a filter with zero blend weight, so on
        // their own they are still neutral.
        for nr in [
            NoiseReduction {
                luma_detail: 100.0,
                ..Default::default()
            },
            NoiseReduction {
                chroma_detail: 100.0,
                ..Default::default()
            },
        ] {
            assert!(NoiseReductionNode::is_identity(&stages(nr)), "{nr:?}");
        }
    }

    #[test]
    fn param_block_carries_all_four_controls() {
        let pb = NoiseReductionNode::param_block(&stages(NoiseReduction {
            luma: 40.0,
            luma_detail: 55.0,
            chroma: 70.0,
            chroma_detail: 30.0,
        }));
        assert_eq!(pb.get_f64("luma"), Some(40.0));
        assert_eq!(pb.get_f64("luma_detail"), Some(55.0));
        assert_eq!(pb.get_f64("chroma"), Some(70.0));
        assert_eq!(pb.get_f64("chroma_detail"), Some(30.0));
    }

    /// `param_block` and the node's declared [`SCHEMA`] must agree: a field
    /// name or type this node emits but does not declare would be rejected
    /// the moment the block is decoded through the schema, which is what
    /// `from_canonical_cbor` does here.
    #[test]
    fn param_block_round_trips_through_the_declared_schema() {
        let pb = NoiseReductionNode::param_block(&stages(NoiseReduction {
            luma: 10.0,
            luma_detail: 20.0,
            chroma: 30.0,
            chroma_detail: 40.0,
        }));
        let decoded = ParamBlock::from_canonical_cbor(pb.canonical_bytes(), &SCHEMA)
            .expect("every field this node emits is declared in its schema");
        assert_eq!(decoded, pb);

        let bogus = ParamBlock::from_fields([("colour", ParamValue::Float(1.0))]).unwrap();
        assert!(
            ParamBlock::from_canonical_cbor(bogus.canonical_bytes(), &SCHEMA).is_err(),
            "the schema must be real, not empty-accept-anything"
        );
    }

    #[test]
    fn plan_pads_by_the_chroma_radius() {
        let node = NoiseReductionNode::new();
        let out = Roi {
            x: 4,
            y: 4,
            w: 32,
            h: 32,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        assert_eq!(node.plan(out, 1.0, &params).0[0], out.expand(CHROMA_RADIUS));
        assert_eq!(
            node.roi_in(out, RenderScale::OneToOne),
            out.expand(CHROMA_RADIUS)
        );
    }

    // ── identity ─────────────────────────────────────────────────────────

    #[test]
    fn zero_strength_is_exact_identity_at_any_detail() {
        let weights = working_luma_weights();
        let rgba = [0.2f32, 0.5, 0.8, 1.0];
        let center = ycc([rgba[0], rgba[1], rgba[2]], weights);
        // A filtered value deliberately far from the center: the blend, not
        // the filter, is what must zero out.
        let filtered = [center[0] + 0.3, center[1] - 0.2, center[2] + 0.15];
        for luma_detail in [0.0f32, 50.0, 100.0] {
            for chroma_detail in [0.0f32, 50.0, 100.0] {
                let nr = NoiseReduction {
                    luma: 0.0,
                    luma_detail,
                    chroma: 0.0,
                    chroma_detail,
                };
                let o = apply_nr(rgba, center, filtered, weights, nr);
                assert_eq!(o, rgba, "{nr:?}");
            }
        }
    }

    #[test]
    fn identity_at_zero_strength_holds_for_out_of_gamut_pixels() {
        let weights = working_luma_weights();
        for rgba in [
            [1.7f32, 1.2, 0.8, 1.0],
            [-0.04f32, 0.02, 0.01, 1.0],
            [4.0f32, 4.0, 4.0, 1.0],
        ] {
            let center = ycc([rgba[0], rgba[1], rgba[2]], weights);
            let filtered = [center[0] + 0.2, center[1] + 0.2, center[2] - 0.2];
            let o = apply_nr(rgba, center, filtered, weights, NoiseReduction::default());
            assert_eq!(o, rgba, "rgba={rgba:?}");
        }
    }

    // ── the reapply's luminance/chroma separation ────────────────────────

    /// The claim [`apply_nr`]'s doc comment makes: a pure CHROMA move must
    /// carry no luminance with it. Checked in the encoded domain, where the
    /// claim is exact, rather than through the first-order linear reapply.
    #[test]
    fn a_pure_chroma_move_leaves_encoded_luma_unchanged() {
        let weights = working_luma_weights();
        let d_cb = 0.05f32;
        let d_cr = -0.03f32;
        let de_r = d_cr;
        let de_b = d_cb;
        let de_g = -(weights[0] * d_cr + weights[2] * d_cb) / weights[1];
        let d_luma = de_r * weights[0] + de_g * weights[1] + de_b * weights[2];
        assert!(
            d_luma.abs() < 1e-6,
            "a chroma-only delta must be luma-neutral: {d_luma}"
        );
    }

    #[test]
    fn ycc_round_trips_a_neutral_grey_to_zero_chroma() {
        let weights = working_luma_weights();
        for v in [0.05f32, 0.2, 0.5, 0.9] {
            let t = ycc([v, v, v], weights);
            assert!(t[1].abs() < 1e-6 && t[2].abs() < 1e-6, "v={v} ycc={t:?}");
        }
    }

    // ── the filter itself ────────────────────────────────────────────────

    #[test]
    fn range_sigmas_run_from_max_at_detail_zero_to_min_at_detail_100() {
        // 1e-6, not 1e-9: `mix` is `a + (b - a) * t`, so the endpoint is
        // reached through a subtraction and lands within f32 rounding of the
        // constant, not exactly on it.
        assert!((luma_range_sigma(0.0) - LUMA_RANGE_SIGMA_MAX).abs() < 1e-6);
        assert!((luma_range_sigma(100.0) - LUMA_RANGE_SIGMA_MIN).abs() < 1e-6);
        assert!((chroma_range_sigma(0.0) - CHROMA_RANGE_SIGMA_MAX).abs() < 1e-6);
        assert!((chroma_range_sigma(100.0) - CHROMA_RANGE_SIGMA_MIN).abs() < 1e-6);
        assert!(luma_range_sigma(50.0) < luma_range_sigma(10.0));
    }

    #[test]
    fn a_flat_patch_is_left_alone_by_the_filter() {
        let weights = working_luma_weights();
        let (w, h) = (16u32, 16u32);
        let plane = vec![ycc([0.4, 0.4, 0.4], weights); (w * h) as usize];
        let filtered = bilateral_ycc(
            &plane,
            w,
            h,
            NoiseReduction {
                luma: 100.0,
                luma_detail: 0.0,
                chroma: 100.0,
                chroma_detail: 0.0,
            },
        );
        for (i, f) in filtered.iter().enumerate() {
            for c in 0..3 {
                assert!(
                    (f[c] - plane[i][c]).abs() < 1e-5,
                    "idx={i} chan={c}: a constant plane must survive the filter"
                );
            }
        }
    }

    /// The headline behaviour: the luma filter must actually remove
    /// luminance noise. Measured as the standard deviation of the encoded
    /// luma plane before and after, at full strength and detail 0.
    #[test]
    fn the_luma_filter_removes_luminance_noise() {
        let weights = working_luma_weights();
        let (w, h) = (48u32, 48u32);
        let mut rng = XorShift64(0x51ED_1234_ABCD_0001);
        let plane: Vec<[f32; 3]> = (0..(w * h))
            .map(|_| {
                let v = 0.4 + rng.next_noise(0.02);
                ycc([v, v, v], weights)
            })
            .collect();
        let nr = NoiseReduction {
            luma: 100.0,
            luma_detail: 0.0,
            chroma: 0.0,
            chroma_detail: 0.0,
        };
        let filtered = bilateral_ycc(&plane, w, h, nr);
        let before: Vec<f32> = plane.iter().map(|t| t[0]).collect();
        let after: Vec<f32> = filtered.iter().map(|t| t[0]).collect();
        let (s0, s1) = (std_dev(&before), std_dev(&after));
        println!(
            "[nr] luma sigma before={s0:.5} after={s1:.5} ratio={:.3}",
            s1 / s0
        );
        assert!(s0 > 1e-3, "the test patch must actually be noisy: {s0}");
        assert!(
            s1 < s0 * 0.5,
            "luma=100/detail=0 must at least halve the luma noise: {s0} -> {s1}"
        );
    }

    /// The same, for chroma, which is the cheaper and more convincing half.
    #[test]
    fn the_chroma_filter_removes_chroma_noise_harder_than_the_luma_one() {
        let weights = working_luma_weights();
        let (w, h) = (48u32, 48u32);
        let mut rng = XorShift64(0x51ED_1234_ABCD_0002);
        let plane: Vec<[f32; 3]> = (0..(w * h))
            .map(|_| {
                // Independent per-channel noise, which is what produces
                // coloured blotching rather than pure luminance grain.
                let r = 0.4 + rng.next_noise(0.02);
                let g = 0.4 + rng.next_noise(0.02);
                let b = 0.4 + rng.next_noise(0.02);
                ycc([r, g, b], weights)
            })
            .collect();
        let nr = NoiseReduction {
            luma: 0.0,
            luma_detail: 0.0,
            chroma: 100.0,
            chroma_detail: 0.0,
        };
        let filtered = bilateral_ycc(&plane, w, h, nr);
        let cb0: Vec<f32> = plane.iter().map(|t| t[1]).collect();
        let cb1: Vec<f32> = filtered.iter().map(|t| t[1]).collect();
        let (s0, s1) = (std_dev(&cb0), std_dev(&cb1));
        println!(
            "[nr] chroma sigma before={s0:.5} after={s1:.5} ratio={:.3}",
            s1 / s0
        );
        assert!(s0 > 1e-3, "the test patch must actually be noisy: {s0}");
        assert!(
            s1 < s0 * 0.25,
            "chroma=100/detail=0 must cut chroma noise to a quarter or better: {s0} -> {s1}"
        );
    }

    /// The `detail` control has to do something honest: at detail 100 the
    /// range term is tight enough that far less of the noise is averaged
    /// away than at detail 0.
    #[test]
    fn higher_luma_detail_preserves_more_of_the_signal() {
        let weights = working_luma_weights();
        let (w, h) = (48u32, 48u32);
        let mut rng = XorShift64(0x51ED_1234_ABCD_0003);
        let plane: Vec<[f32; 3]> = (0..(w * h))
            .map(|_| {
                let v = 0.4 + rng.next_noise(0.02);
                ycc([v, v, v], weights)
            })
            .collect();
        let sigma_at = |detail: f32| {
            let f = bilateral_ycc(
                &plane,
                w,
                h,
                NoiseReduction {
                    luma: 100.0,
                    luma_detail: detail,
                    chroma: 0.0,
                    chroma_detail: 0.0,
                },
            );
            std_dev(&f.iter().map(|t| t[0]).collect::<Vec<_>>())
        };
        let (lo, hi) = (sigma_at(0.0), sigma_at(100.0));
        println!("[nr] luma sigma detail0={lo:.5} detail100={hi:.5}");
        assert!(
            hi > lo * 1.5,
            "detail=100 must leave substantially more signal standing than detail=0: {lo} vs {hi}"
        );
    }

    /// A real edge must survive the luma filter: the bilateral's range term
    /// is the whole reason this is not a box blur.
    #[test]
    fn a_hard_edge_survives_the_luma_filter() {
        let weights = working_luma_weights();
        let (w, h) = (32u32, 32u32);
        let plane: Vec<[f32; 3]> = (0..(w * h))
            .map(|i| {
                let v = if (i % w) < w / 2 { 0.15f32 } else { 0.75 };
                ycc([v, v, v], weights)
            })
            .collect();
        let filtered = bilateral_ycc(
            &plane,
            w,
            h,
            NoiseReduction {
                luma: 100.0,
                luma_detail: 0.0,
                chroma: 0.0,
                chroma_detail: 0.0,
            },
        );
        let row = (h / 2 * w) as usize;
        let left = plane[row + (w / 2 - 1) as usize][0];
        let right = plane[row + (w / 2) as usize][0];
        let f_left = filtered[row + (w / 2 - 1) as usize][0];
        let f_right = filtered[row + (w / 2) as usize][0];
        let step_before = right - left;
        let step_after = f_right - f_left;
        println!("[nr] edge step before={step_before:.4} after={step_after:.4}");
        assert!(
            step_after > step_before * 0.9,
            "the bilateral must keep at least 90% of a hard edge: {step_before} -> {step_after}"
        );
    }
}
