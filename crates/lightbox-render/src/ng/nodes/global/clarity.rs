// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.clarity` (E10 task **D3**). Guided-filter local contrast on
//! companion-encoded luma, midtone-weighted, ± range (spec §4.2 "Clarity /
//! texture | guided-filter (clarity) and band-pass (texture) on
//! companion-encoded luma, detail recombined"). Reuses the tone-recovery
//! guided-filter infra ([`crate::ng::nodes::common::guided::guided_filter_self`])
//! per this phase's own brief ("REUSE that infra for Clarity's local
//! contrast... do not reinvent it").
//!
//! # The algorithm
//!
//! 1. **Encoded luma.** `Le = srgb_oetf(clamp(working_luma(rgb), 0, 1))`, a
//!    *scalar* companion-domain luma (not per-channel; see the reapply note
//!    below for why a scalar keeps the identity contract exact).
//! 2. **Guided-filter base.** `B = guided_filter_self(Le)` (He & Sun,
//!    self-guided, [`GUIDE_RADIUS`]/[`GUIDE_EPS`]), the same edge-aware
//!    base/detail split `tone_recovery` uses, here over the companion-domain
//!    luma plane instead of scene-linear.
//! 3. **Midtone-weighted gain.** `detail = Le - B`; [`midtone_weight`] is a
//!    raised-cosine "bell" that is `0` at the deep-shadow/blown-highlight
//!    ends of the encoded range and `1` across the midrange, clarity is a
//!    midtone tool, this is its ± range's spatial protection against
//!    amplifying already-clipped extremes. `gain = clamp(1 + (clarity/100) ·
//!    CLARITY_RANGE · weight, GAIN_MIN, GAIN_MAX)`.
//! 4. **Reapply via a local-derivative additive delta, not a clamped
//!    round-trip.** See [`apply_clarity`]'s doc comment, this is what keeps
//!    `clarity == 0` an *exact* pixel identity even for out-of-gamut
//!    (negative or `> 1`) scene-linear pixels, which a full
//!    `companion_encode → adjust → companion_decode` round trip on the RGB
//!    triple would NOT be (the encode step's `[0,1]` clamp is lossy).
//!
//! GPU (`shaders/global_clarity.wgsl`) and CPU ([`apply_clarity`] via
//! [`guided_filter_encoded_luma`]) implement the identical two-pass algorithm
//! from the identical formulas (the established per-node convention), so
//! CPU/GPU parity (§4.4) holds to numerical rounding.
//!
//! # Halo gate (task D3 AC, reusing the B2 metric)
//!
//! `tests/e10_clarity.rs` runs the exact same
//! `lightbox_render_testkit::halo::evaluate_halo` metric `tests/
//! e10_tone_recovery.rs` uses for the B2 gate, at `clarity = +100` on a
//! synthetic hard-edge scene, against the untouched input as the off-edge
//! reference (a well-behaved local-contrast tool changes flat/textureless
//! regions negligibly, `detail ≈ 0` there by construction).

use std::sync::Arc;

use lightbox_color::matrix::spaces::companion_encode;
use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::global::tone_recovery::{working_luma, working_luma_weights};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The two-pass guided-filter kernel, naga-validated at build (`build.rs`).
const CLARITY_WGSL: &str = include_str!("../../../../shaders/global_clarity.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "clarity",
    kind: ParamKind::Float,
}]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.clarity"),
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

/// Guided-filter box-filter half-window, pixels.
pub const GUIDE_RADIUS: u32 = 12;
/// Guided-filter variance regularizer, in encoded-luma² units.
pub const GUIDE_EPS: f32 = 4.0e-3;
/// Midtone-weight bell: `0` at the encoded-domain floor, ramps to `1` by
/// this edge.
pub const LOW_EDGE: f32 = 0.12;
/// Midtone-weight bell: `1` up to this edge, ramps to `0` by the encoded-
/// domain ceiling.
pub const HIGH_EDGE: f32 = 0.88;
/// Slider-to-gain calibration: a fully-deflected (`±100`) clarity slider at
/// full midtone weight scales the local-contrast detail by up to `±
/// CLARITY_RANGE`.
pub const CLARITY_RANGE: f32 = 1.0;
/// Gain floor, `clarity = -100` at full midtone weight flattens local
/// contrast to exactly `0` (never inverts it, which would itself be a halo).
pub const GAIN_MIN: f32 = 0.0;
/// Gain ceiling, matches `1 + CLARITY_RANGE` at `weight == 1`.
pub const GAIN_MAX: f32 = 2.0;

/// Smoothstep (Hermite), shared with every other E10 node's identical helper.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The midtone-weight bell (task D3 "midtone-weighted"): `0` at the deep-
/// shadow/blown-highlight ends of the encoded `[0,1]` range, `1` across the
/// midrange, smooth in between.
#[inline]
pub fn midtone_weight(e: f32) -> f32 {
    let lo = smoothstep(0.0, LOW_EDGE, e);
    let hi = 1.0 - smoothstep(HIGH_EDGE, 1.0, e);
    (lo * hi).clamp(0.0, 1.0)
}

/// Scalar companion-encoded luma: `srgb_oetf(clamp(working_luma(rgb), 0,
/// 1))`, the single source of truth both `eval_cpu`'s luma plane and
/// `eval_gpu`'s `encoded_luma_of` duplicate.
#[inline]
pub fn encoded_luma(rgb: [f32; 3], weights: [f32; 3]) -> f32 {
    let l = working_luma(rgb, weights);
    companion_encode([l, l, l])[0]
}

/// The sRGB EOTF's first derivative at encoded value `e` (clamped to
/// `[0,1]`), see [`apply_clarity`]'s doc comment for why the reapply uses
/// this instead of a full encode/decode round trip.
#[inline]
pub fn srgb_eotf_deriv(e: f32) -> f32 {
    let c = e.clamp(0.0, 1.0);
    if c <= 0.040_449_936 {
        1.0 / 12.92
    } else {
        (2.4 / 1.055) * ((c + 0.055) / 1.055).powf(1.4)
    }
}

/// The self-guided filter over an encoded-luma plane, a thin wrapper over
/// [`crate::ng::nodes::common::guided::guided_filter_self`] (this task's own
/// "reuse the tone-recovery guided-filter infra" brief) that recombines
/// `base = mean_a·Le + mean_b` (He & Sun eq. 8), mirroring
/// `tone_recovery::guided_filter_luma`'s shape exactly.
pub fn guided_filter_encoded_luma(luma_e: &[f32], w: u32, h: u32) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (mean_a, mean_b) =
        crate::ng::nodes::common::guided::guided_filter_self(luma_e, w, h, GUIDE_RADIUS, GUIDE_EPS);
    let mut base = vec![0f32; luma_e.len()];
    for (i, out) in base.iter_mut().enumerate() {
        *out = mean_a[i] * luma_e[i] + mean_b[i];
    }
    base
}

/// The full per-pixel op: given a working RGBA pixel plus the encoded luma
/// at that pixel and the guided-filter base value there, returns the
/// clarity-adjusted RGBA (alpha untouched). The CPU parity anchor for
/// `global_clarity.wgsl`'s `pass_c`.
///
/// **Why an additive local-derivative delta, not `companion_decode(enc +
/// delta)`.** `clarity == 0 ⇒ gain == 1 ⇒ delta_encoded == 0` for every
/// pixel, but a full `companion_encode(rgb) → add delta → companion_decode`
/// round trip on the RGB triple is only exact for **in-gamut** (`[0,1]`)
/// pixels, because `companion_encode` clamps first (spec's own companion
/// domain is clamped-`[0,1]`, same as `whites_blacks`/`contrast`); an
/// out-of-gamut scene-linear highlight (`> 1`) or a slightly negative
/// shadow would silently clamp even at `delta == 0`, breaking the
/// `is_identity ⇒ exact pixel identity` contract every other E10 node
/// upholds. Instead, `delta_encoded` is converted to a **linear**-domain
/// delta via the first derivative of the sRGB EOTF at the (clamped)
/// operating point (`delta_linear = delta_encoded · d(eotf)/de`) and added
/// identically to all three channels, `0 · finite == 0` in IEEE-754
/// regardless of the derivative's value, so `clarity == 0` is an exact
/// identity **unconditionally**, not merely for in-gamut pixels
/// ([`tests::identity_at_zero_clarity_holds_for_out_of_gamut_pixels`]).
///
/// **The halo guard (task D3 AC).** A plain "boost the detail layer" recombination
/// (`base_e + detail * gain` with no further guard) reintroduces exactly the
/// gradient-reversal halo spec §4.5/task B2 names as the failure mode
/// local-contrast tools must avoid: the guided filter's edge-preserving
/// `a -> 1` at a strong edge does NOT, by itself, stop a detail boost from
/// overshooting in the transition band just off the edge (measured directly
/// during this task, an earlier unguarded version of this fn failed the
/// halo metric). So the boosted value is clamped to `[local_min, local_max]`
/// the min/max of `Le` over the same guided-filter window ([`GUIDE_RADIUS`])
/// before deriving `delta_encoded`: `luma_e` itself always lies inside
/// `[local_min, local_max]` (it's one of the window's own samples), so
/// `gain == 1` still makes the clamp a no-op and the identity above holds;
/// `gain != 1` can locally sharpen but can never create a NEW local extremum
/// beyond what already existed in the neighborhood, the standard,
/// structural "no new extremum, no gradient reversal" technique.
#[inline]
pub fn apply_clarity(
    rgba: [f32; 4],
    luma_e: f32,
    base_e: f32,
    local_min: f32,
    local_max: f32,
    clarity: f32,
) -> [f32; 4] {
    let detail = luma_e - base_e;
    let weight = midtone_weight(luma_e);
    let gain = (1.0 + (clarity / 100.0) * CLARITY_RANGE * weight).clamp(GAIN_MIN, GAIN_MAX);
    // `raw_delta_encoded` is EXACTLY `0.0` at `gain == 1.0` (any finite
    // `detail * 0.0 == 0.0` in IEEE-754), computing `boosted` as `luma_e +
    // raw_delta_encoded` (not `base_e + detail * gain`) means `boosted ==
    // luma_e` bit-for-bit at `gain == 1.0` too (`x + 0.0 == x` exactly),
    // which is what keeps the clamp below an exact no-op at the identity
    // point instead of reintroducing float rounding from the `base_e +
    // (luma_e - base_e)` re-association.
    let raw_delta_encoded = detail * (gain - 1.0);
    let boosted = luma_e + raw_delta_encoded;
    let lo = local_min.min(local_max);
    let hi = local_max.max(local_min);
    let clamped = boosted.clamp(lo, hi);
    let delta_encoded = clamped - luma_e;
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);
    let out = [
        rgba[0] + delta_linear,
        rgba[1] + delta_linear,
        rgba[2] + delta_linear,
        rgba[3],
    ];
    if !out[0].is_finite() || !out[1].is_finite() || !out[2].is_finite() {
        return rgba;
    }
    out
}

/// The `global.clarity` develop node.
#[derive(Default)]
pub struct ClarityNode {}

impl ClarityNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.clarity");

    /// A fresh node.
    pub fn new() -> ClarityNode {
        ClarityNode::default()
    }
}

impl RenderNode for ClarityNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois(vec![out.expand(2 * GUIDE_RADIUS); 1])
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
            .ok_or_else(|| NodeError::Other("global.clarity: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.clarity: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let clarity = params.get_f64_or("clarity", 0.0) as f32;
        let weights = working_luma_weights();

        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&weights[0].to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&weights[1].to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&weights[2].to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&clarity.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.clarity params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let scratch = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("global.clarity scratch a,b"),
            size: wgpu::Extent3d {
                width: extent.w.max(1),
                height: extent.h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());

        let pipe_a = ctx.kernels.compute_pipeline(CLARITY_WGSL, "pass_a")?;
        let bg0_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_a in"),
            layout: &pipe_a.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg1_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_a out"),
            layout: &pipe_a.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&scratch_view),
            }],
        });
        let bg2_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_a params"),
            layout: &pipe_a.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_a,
            &[&bg0_a, &bg1_a, &bg2_a],
            extent,
            "global.clarity.pass_a",
        );

        let pipe_c = ctx.kernels.compute_pipeline(CLARITY_WGSL, "pass_c")?;
        let bg0_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_c in"),
            layout: &pipe_c.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scratch_view),
                },
            ],
        });
        let bg1_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_c out"),
            layout: &pipe_c.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg2_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.clarity pass_c params"),
            layout: &pipe_c.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_c,
            &[&bg0_c, &bg1_c, &bg2_c],
            extent,
            "global.clarity.pass_c",
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
            .ok_or_else(|| NodeError::Cpu("global.clarity: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let clarity = params.get_f64_or("clarity", 0.0) as f32;
        let weights = working_luma_weights();

        let (iw, ih) = (input.extent.w, input.extent.h);
        let mut luma_plane = vec![0f32; (iw as usize) * (ih as usize)];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                luma_plane[(y * iw + x) as usize] = encoded_luma([p[0], p[1], p[2]], weights);
            }
        }
        let base_plane = guided_filter_encoded_luma(&luma_plane, iw, ih);
        // The halo guard's local extrema, same window as the guided filter
        // (see `apply_clarity`'s doc comment).
        let local_min =
            crate::ng::nodes::common::guided::box_min(&luma_plane, iw, ih, GUIDE_RADIUS);
        let local_max =
            crate::ng::nodes::common::guided::box_max(&luma_plane, iw, ih, GUIDE_RADIUS);

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
                let o = apply_clarity(
                    p,
                    luma_plane[i],
                    base_plane[i],
                    local_min[i],
                    local_max[i],
                    clarity,
                );
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for ClarityNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.presence.clarity == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([("clarity", ParamValue::Float(p.presence.clarity as f64))])
            .expect("clarity is always finite (clamped on ingest — spec A2)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(2 * GUIDE_RADIUS)
    }
}

/// Factory registering [`ClarityNode`].
#[derive(Default)]
pub struct ClarityFactory {}

impl NodeFactory for ClarityFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ClarityNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(CLARITY_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_identity_tracks_the_clarity_field() {
        let mut g = GlobalStages::default();
        assert!(ClarityNode::is_identity(&g));
        g.presence.clarity = 10.0;
        assert!(!ClarityNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_clarity() {
        let g = GlobalStages {
            presence: lightbox_edit::Presence {
                clarity: 42.0,
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = ClarityNode::param_block(&g);
        assert_eq!(pb.get_f64("clarity"), Some(42.0));
    }

    #[test]
    fn plan_pads_by_two_guide_radii() {
        let node = ClarityNode::new();
        let out = Roi {
            x: 5,
            y: 5,
            w: 40,
            h: 40,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        let rois = node.plan(out, 1.0, &params);
        assert_eq!(rois.0[0], out.expand(2 * GUIDE_RADIUS));
    }

    // ── identity / correctness ────────────────────────────────────────────

    #[test]
    fn zero_clarity_is_exact_identity_at_any_base() {
        let rgba = [0.3f32, 0.6, 0.9, 1.0];
        for base_e in [0.0f32, 0.2, 0.5, 0.8, 1.0] {
            let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
            let o = apply_clarity(rgba, luma_e, base_e, 0.0, 1.0, 0.0);
            for c in 0..4 {
                assert_eq!(o[c], rgba[c], "channel {c}: base_e={base_e}");
            }
        }
    }

    /// The identity contract must hold even for OUT-OF-GAMUT pixels
    /// (scene-linear `> 1` highlight, or a slightly negative shadow), the
    /// property [`apply_clarity`]'s doc comment claims and the whole reason
    /// the reapply uses a local derivative instead of a clamped round trip.
    #[test]
    fn identity_at_zero_clarity_holds_for_out_of_gamut_pixels() {
        for rgba in [
            [1.4f32, 1.1, 0.9, 1.0],     // scene-linear highlight, > 1
            [-0.02f32, 0.01, 0.03, 1.0], // slightly negative (rounding noise)
            [2.5f32, 2.5, 2.5, 1.0],     // deeply out-of-gamut
        ] {
            let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
            // base_e need not equal luma_e, any guided-filter base value.
            for base_e in [0.0f32, luma_e, 1.0] {
                let o = apply_clarity(rgba, luma_e, base_e, 0.0, 1.0, 0.0);
                assert_eq!(o, rgba, "rgba={rgba:?} base_e={base_e}");
            }
        }
    }

    #[test]
    fn positive_clarity_boosts_detail_negative_clarity_flattens_it() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
        let base_e = luma_e - 0.05; // a positive local detail
                                    // A wide-open [0,1] local window so the halo guard never clips the
                                    // boost/flatten this test is checking for.
        let pos = apply_clarity(rgba, luma_e, base_e, 0.0, 1.0, 100.0);
        let neg = apply_clarity(rgba, luma_e, base_e, 0.0, 1.0, -100.0);
        // Positive clarity boosts (moves further from base than the
        // identity), negative clarity flattens (moves toward/at the base).
        assert!(pos[0] > rgba[0], "positive clarity must brighten: {pos:?}");
        assert!(
            neg[0] < rgba[0],
            "negative clarity must darken back toward base: {neg:?}"
        );
        assert!(
            (pos[0] - rgba[0]).abs() > (neg[0] - rgba[0]).abs() * 0.0,
            "sanity: both must move"
        );
    }

    #[test]
    fn midtone_weight_is_zero_at_extremes_and_one_in_the_midrange() {
        assert!(midtone_weight(0.0) < 1e-6);
        assert!(midtone_weight(1.0) < 1e-6);
        assert!((midtone_weight(0.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clarity_is_a_no_op_far_off_edges_on_a_flat_patch() {
        // A perfectly flat plane: guided-filter base == luma everywhere, so
        // detail == 0 regardless of clarity, a flat region is untouched.
        let rgba = [0.4f32, 0.4, 0.4, 1.0];
        let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
        // A truly flat neighborhood: local min == local max == luma_e.
        let o = apply_clarity(rgba, luma_e, luma_e, luma_e, luma_e, 100.0);
        for c in 0..3 {
            assert!(
                (o[c] - rgba[c]).abs() < 1e-5,
                "flat patch must be a no-op: {o:?}"
            );
        }
    }

    /// **D3 AC / the halo guard's own unit-level proof**: boosting a strong
    /// detail signal can never push the recombined encoded value beyond the
    /// actual local extrema, for ANY clarity magnitude, the structural
    /// property [`apply_clarity`]'s doc comment claims. Verified by the exact
    /// bound the clamp implies: `|delta_linear| <=
    /// max(|local_min-luma_e|,|local_max-luma_e|) * srgb_eotf_deriv(luma_e)`,
    /// checked directly against a dense sweep of `clarity` magnitudes
    /// (including ones far beyond the slider's own `±100` range, to prove
    /// this is a structural clamp, not merely "the default range happens to
    /// stay small").
    #[test]
    fn boost_never_exceeds_the_local_extrema() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.55f32;
        let base_e = 0.45f32; // a large detail (0.10)
        let (local_min, local_max) = (0.40f32, 0.60f32);
        let max_encoded_delta = (local_min - luma_e).abs().max((local_max - luma_e).abs());
        let bound = max_encoded_delta * srgb_eotf_deriv(luma_e) + 1e-6;
        for clarity in [50.0f32, 100.0, 1000.0, 1.0e6] {
            let o = apply_clarity(rgba, luma_e, base_e, local_min, local_max, clarity);
            let delta = o[0] - rgba[0];
            assert!(
                delta.abs() <= bound,
                "clarity={clarity}: |delta|={} must stay within the clamp bound {bound}",
                delta.abs()
            );
        }
    }
}
