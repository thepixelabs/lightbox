// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.texture` (E10 task **D4**). Mid-frequency band-pass boost/
//! suppress on companion-encoded luma, a 2-level box-blur pyramid band
//! (spec §4.2 "Clarity / texture | ... band-pass (texture) on companion-
//! encoded luma, detail recombined").
//!
//! # The algorithm
//!
//! 1. **Encoded luma.** Same scalar `srgb_oetf(clamp(working_luma(rgb), 0,
//!    1))` [`crate::ng::nodes::global::clarity::encoded_luma`] uses.
//! 2. **The band.** `blur(R1) - blur(R2)` (`R1 < R2`, two plain box blurs
//!    [`crate::ng::nodes::common::guided::box_blur`], reused as-is), a
//!    "2-level pyramid": content finer than `R1` (single-pixel grain) is
//!    already smoothed away by BOTH blurs so it cancels in the subtraction;
//!    content coarser than `R2` (broad tonal shape) is present in both
//!    blurs at nearly the same value so it also cancels, only the
//!    **mid**-frequency band between the two scales survives.
//! 3. **Uniform gain (no midtone weighting, unlike clarity, spec §4.2 only
//!    calls out clarity as midtone-weighted).** `gain = clamp(1 +
//!    (texture/100) · TEXTURE_RANGE, GAIN_MIN, GAIN_MAX)`.
//! 4. **The same halo guard + local-derivative reapply
//!    [`crate::ng::nodes::global::clarity`] uses** (see that module's doc
//!    comment for the full identity/halo rationale, it applies verbatim
//!    here, band-pass detail boosting has the identical overshoot risk an
//!    unguarded clarity boost does): the boosted value is clamped to
//!    `[box_min, box_max]` of the same `R2` window before deriving the
//!    reapply delta.
//!
//! # Noise-amplification guard (task D4 AC)
//!
//! `tests/e10_texture.rs` measures the standard deviation of a flat i.i.d.
//! noise patch before/after `texture = +100` and asserts growth `< 1.2×`.
//! Analytically: for i.i.d. per-pixel noise of std `σ`, `Var(blur(R1)) =
//! σ²/N1`, `Var(blur(R2)) = σ²/N2`, and, because the `R1` window is a
//! **subset** of the `R2` window at the same center
//! `Cov(blur(R1),blur(R2)) = σ²/N2`, so `Var(band) = σ²(1/N1 - 1/N2)`. With
//! [`R1`]`=2`/[`R2`]`=6` (`N1=25`, `N2=169`), `Var(band) ≈ 0.0341σ²`; at
//! `texture=+100` (`gain=2`, `raw_delta = band`), the output's total
//! variance (accounting for `Cov(luma, band)` too, since `band` is
//! correlated with the original per-pixel sample) works out to
//! `≈ 1.10σ²`, a `√1.10 ≈ 1.05×` std growth, comfortably inside the `1.2×`
//! guard **before** the halo-guard clamp is even applied (which can only
//! shrink it further). This derivation is checked directly (not just
//! asserted) by [`tests::variance_growth_matches_the_analytic_bound`].

use std::sync::Arc;

use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::common::guided::{box_blur, box_max, box_min};
use crate::ng::nodes::global::clarity::{encoded_luma, srgb_eotf_deriv};
use crate::ng::nodes::global::tone_recovery::working_luma_weights;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The single-pass band-pass kernel, naga-validated at build (`build.rs`).
const TEXTURE_WGSL: &str = include_str!("../../../../shaders/global_texture.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "texture",
    kind: ParamKind::Float,
}]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.texture"),
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

/// The fine box-blur radius (excludes single-pixel grain from the band).
pub const R1: u32 = 2;
/// The coarse box-blur radius (excludes broad tonal shape from the band)
/// also the node's apron/`roi_in` requirement.
pub const R2: u32 = 6;
/// Slider-to-gain calibration.
pub const TEXTURE_RANGE: f32 = 1.0;
/// Gain floor, `texture = -100` flattens the band to exactly `0`.
pub const GAIN_MIN: f32 = 0.0;
/// Gain ceiling, matches `1 + TEXTURE_RANGE`.
pub const GAIN_MAX: f32 = 2.0;

/// The full per-pixel op: given a working RGBA pixel, the encoded luma at
/// that pixel, the band value there, and the `R2`-window local min/max,
/// returns the texture-adjusted RGBA (alpha untouched). The CPU parity
/// anchor for `global_texture.wgsl`.
#[inline]
pub fn apply_texture(
    rgba: [f32; 4],
    luma_e: f32,
    band: f32,
    local_min: f32,
    local_max: f32,
    texture: f32,
) -> [f32; 4] {
    let gain = (1.0 + (texture / 100.0) * TEXTURE_RANGE).clamp(GAIN_MIN, GAIN_MAX);
    let raw_delta_encoded = band * (gain - 1.0);
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

/// The `global.texture` develop node.
#[derive(Default)]
pub struct TextureNode {}

impl TextureNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.texture");

    /// A fresh node.
    pub fn new() -> TextureNode {
        TextureNode::default()
    }
}

impl RenderNode for TextureNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois(vec![out.expand(R2); 1])
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
            .ok_or_else(|| NodeError::Other("global.texture: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.texture: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let texture = params.get_f64_or("texture", 0.0) as f32;
        let weights = working_luma_weights();
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&weights[0].to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&weights[1].to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&weights[2].to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&texture.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.texture params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(TEXTURE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.texture in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.texture out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.texture params"),
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
            "global.texture",
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
            .ok_or_else(|| NodeError::Cpu("global.texture: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let texture = params.get_f64_or("texture", 0.0) as f32;
        let weights = working_luma_weights();

        let (iw, ih) = (input.extent.w, input.extent.h);
        let mut luma_plane = vec![0f32; (iw as usize) * (ih as usize)];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                luma_plane[(y * iw + x) as usize] = encoded_luma([p[0], p[1], p[2]], weights);
            }
        }
        let blur_r1 = box_blur(&luma_plane, iw, ih, R1);
        let blur_r2 = box_blur(&luma_plane, iw, ih, R2);
        let local_min = box_min(&luma_plane, iw, ih, R2);
        let local_max = box_max(&luma_plane, iw, ih, R2);

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
                let band = blur_r1[i] - blur_r2[i];
                let o = apply_texture(p, luma_plane[i], band, local_min[i], local_max[i], texture);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for TextureNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.presence.texture == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([("texture", ParamValue::Float(p.presence.texture as f64))])
            .expect("texture is always finite (clamped on ingest — spec A2)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(R2)
    }
}

/// Factory registering [`TextureNode`].
#[derive(Default)]
pub struct TextureFactory {}

impl NodeFactory for TextureFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(TextureNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(TEXTURE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_identity_tracks_the_texture_field() {
        let mut g = GlobalStages::default();
        assert!(TextureNode::is_identity(&g));
        g.presence.texture = 15.0;
        assert!(!TextureNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_texture() {
        let g = GlobalStages {
            presence: lightbox_edit::Presence {
                texture: -33.0,
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = TextureNode::param_block(&g);
        assert_eq!(pb.get_f64("texture"), Some(-33.0));
    }

    #[test]
    fn plan_pads_by_r2() {
        let node = TextureNode::new();
        let out = Roi {
            x: 5,
            y: 5,
            w: 40,
            h: 40,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        let rois = node.plan(out, 1.0, &params);
        assert_eq!(rois.0[0], out.expand(R2));
    }

    #[test]
    fn zero_texture_is_exact_identity_at_any_band() {
        let rgba = [0.3f32, 0.6, 0.9, 1.0];
        let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
        for band in [0.0f32, 0.1, -0.1, 0.3] {
            let o = apply_texture(rgba, luma_e, band, 0.0, 1.0, 0.0);
            assert_eq!(o, rgba, "band={band}");
        }
    }

    /// Same identity contract for out-of-gamut pixels as
    /// `clarity::apply_clarity` (see that fn's doc comment for the
    /// rationale, identical construction here).
    #[test]
    fn identity_at_zero_texture_holds_for_out_of_gamut_pixels() {
        for rgba in [[1.6f32, 1.2, 0.9, 1.0], [-0.03f32, 0.02, 0.01, 1.0]] {
            let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
            let o = apply_texture(rgba, luma_e, 0.2, 0.0, 1.0, 0.0);
            assert_eq!(o, rgba, "rgba={rgba:?}");
        }
    }

    #[test]
    fn positive_texture_boosts_the_band_negative_suppresses_it() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.5f32;
        let band = 0.05f32;
        let pos = apply_texture(rgba, luma_e, band, 0.0, 1.0, 100.0);
        let neg = apply_texture(rgba, luma_e, band, 0.0, 1.0, -100.0);
        assert!(pos[0] > rgba[0], "positive texture must boost: {pos:?}");
        assert!(neg[0] < pos[0], "negative texture must suppress: {neg:?}");
        // At texture=-100, gain=0 exactly, so the band is fully removed:
        // output luma == the flat (band-free) luma_e.
        assert!(
            (neg[0] - rgba[0] + band * srgb_eotf_deriv(luma_e)).abs() < 1e-5,
            "neg={neg:?}"
        );
    }

    #[test]
    fn boost_never_exceeds_the_local_extrema() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.55f32;
        let band = 0.10f32;
        let (local_min, local_max) = (0.40f32, 0.60f32);
        let max_encoded_delta = (local_min - luma_e).abs().max((local_max - luma_e).abs());
        let bound = max_encoded_delta * srgb_eotf_deriv(luma_e) + 1e-6;
        for texture in [50.0f32, 100.0, 1.0e6] {
            let o = apply_texture(rgba, luma_e, band, local_min, local_max, texture);
            let delta = (o[0] - rgba[0]).abs();
            assert!(
                delta <= bound,
                "texture={texture}: delta={delta} bound={bound}"
            );
        }
    }

    // ── D4 AC: noise-amplification guard (analytic proof) ─────────────────

    /// Directly measures `Var(band)` and the total output variance for
    /// i.i.d. noise via a dense Monte Carlo sample, and checks both match
    /// the module doc's closed-form derivation to a loose tolerance (the
    /// **quantitative** version of the D4 AC, checked again end-to-end
    /// through the real node in `tests/e10_texture.rs`).
    #[test]
    fn variance_growth_matches_the_analytic_bound() {
        // A tiny deterministic xorshift PRNG (no new crate dependency for
        // one test's synthetic noise, the workspace's own `no new native
        // deps` discipline extends to test-only Rust deps too).
        struct XorShift64(u64);
        impl XorShift64 {
            fn next_unit(&mut self) -> f32 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                // Top 24 bits -> a uniform f32 in [0, 1).
                ((x >> 40) as f32) / (1u64 << 24) as f32
            }
        }
        let mut rng = XorShift64(0x9E3779B97F4A7C15);
        let (w, h) = (64u32, 64u32);
        let sigma = 0.05f32;
        let mean = 0.5f32;
        let plane: Vec<f32> = (0..(w * h))
            .map(|_| mean + sigma * (rng.next_unit() * 2.0 - 1.0) * 3.0f32.sqrt())
            .collect();

        let blur_r1 = box_blur(&plane, w, h, R1);
        let blur_r2 = box_blur(&plane, w, h, R2);
        let band: Vec<f32> = blur_r1
            .iter()
            .zip(blur_r2.iter())
            .map(|(&a, &b)| a - b)
            .collect();

        fn variance(v: &[f32]) -> f64 {
            let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
            v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / v.len() as f64
        }

        let var_plane = variance(&plane);
        let var_band = variance(&band);
        let n1 = ((2 * R1 + 1) * (2 * R1 + 1)) as f64;
        let n2 = ((2 * R2 + 1) * (2 * R2 + 1)) as f64;
        let expected_var_band = var_plane * (1.0 / n1 - 1.0 / n2);
        println!(
            "[D4] measured Var(band)={var_band:.6} expected={expected_var_band:.6} (n1={n1} n2={n2})"
        );
        // Loose tolerance, finite-sample Monte Carlo, border effects.
        assert!(
            (var_band - expected_var_band).abs() < expected_var_band * 0.5 + 1e-6,
            "measured variance {var_band} too far from analytic {expected_var_band}"
        );

        // At texture=+100 (gain=2, raw_delta=band), output_i = plane_i +
        // band_i (unclamped, matching the analytic derivation's own
        // no-clamp assumption). Growth must stay comfortably under 1.2x.
        let boosted: Vec<f32> = plane
            .iter()
            .zip(band.iter())
            .map(|(&p, &b)| p + b)
            .collect();
        let ratio = (variance(&boosted) / var_plane).sqrt();
        println!("[D4] unclamped std growth ratio={ratio:.4}");
        assert!(ratio < 1.2, "std growth {ratio} must stay under 1.2x");
    }
}
