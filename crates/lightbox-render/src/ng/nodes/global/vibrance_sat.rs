// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.vibrance_sat` (E10 task **C10**). Vibrance + saturation, both
//! evaluated as chroma scaling in OkLCh derived from working RGB (spec §4.2,
//! same domain as `global.hsl`). §4.1 pins this node right after HSL, before
//! (later-phase) color grading.
//!
//! - **Saturation**: a *uniform* chroma multiplier, every hue, every
//!   chroma level scales by the same factor.
//! - **Vibrance**: a *chroma-weighted* multiplier (already-vivid pixels move
//!   less than washed-out ones, [`CHROMA_REF`]'s taper) with a **skin-hue
//!   protection band** ([`SKIN_HUE_CENTER_DEG`]/[`SKIN_HALF_WIDTH_DEG`]) that
//!   suppresses most of the vibrance push for pixels whose hue sits near
//!   documented skin tones, so a portrait's faces don't over-saturate when
//!   the rest of the scene gets a vibrance boost.
//!
//! # Skin-hue protection band, how [`SKIN_HUE_CENTER_DEG`] was derived
//!
//! The center (`54.0°`) is the exact mean Oklab hue of 8 representative
//! skin-tone sRGB8 swatches (light → dark, several undertones), computed via
//! this crate's own [`colorspace::working_to_oklch`]-equivalent math, the
//! same clean-room reproducible-swatch methodology
//! [`colorspace::HUE_BAND_CENTERS_DEG`] uses for the 8 HSL bands (see that
//! constant's doc comment). Table (reproduced/verified by
//! `tests::skin_hue_center_matches_its_swatch_table`):
//!
//! | Swatch (sRGB8)      | Oklab hue |
//! |----------------------|-----------|
//! | `(255, 224, 196)`    | 64.24°    |
//! | `(241, 194, 162)`    | 55.85°    |
//! | `(224, 172, 135)`    | 56.71°    |
//! | `(198, 134, 92)`     | 53.62°    |
//! | `(161, 102, 63)`     | 53.21°    |
//! | `(133, 85, 57)`      | 50.52°    |
//! | `(92, 59, 40)`       | 50.41°    |
//! | `(58, 38, 28)`       | 47.46°    |
//!
//! mean = 54.00° (matches [`colorspace::HUE_BAND_CENTERS_DEG`]'s own
//! "Orange" band center of 52.78° closely, skin tones sit on the
//! red/orange axis, as expected). [`SKIN_HALF_WIDTH_DEG`] (`20°`)
//! comfortably covers the observed swatch spread (47.46°–64.24°, i.e.
//! within ±10.3° of the mean) with a smooth (never hard-edged) taper.

use std::sync::Arc;

use lightbox_edit::{GlobalStages, Treatment};

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::common::colorspace;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The vibrance/saturation remap kernel, naga-validated at build (`build.rs`).
const VIBRANCE_SAT_WGSL: &str = include_str!("../../../../shaders/global_vibrance_sat.wgsl");

/// Chroma multiplier range for the *uniform* saturation slider: `-100` scales
/// chroma toward `0`, `+100` toward `2×`, same shape as `hsl::SAT_RANGE`.
pub const SAT_UNIFORM_RANGE: f32 = 1.0;
/// Chroma multiplier range for the *chroma-weighted* vibrance slider (before
/// the per-pixel chroma-weighting/skin-protection factors are applied).
pub const VIB_RANGE: f32 = 1.5;
/// The Oklab chroma at which vibrance's chroma-weighting has fully tapered
/// to zero (an already-this-vivid-or-more pixel gets no vibrance push at
/// all), an implementer's calibration constant (Oklab chroma for vivid
/// sRGB-gamut colors tops out around `0.3`-`0.4`, see
/// `colorspace::HUE_BAND_CENTERS_DEG`'s own swatch table, whose primaries
/// land at `C ≈ 0.15`-`0.32`).
pub const CHROMA_REF: f32 = 0.30;
/// Skin-hue protection band center, in Oklab hue degrees (see the module
/// docs' swatch-table derivation).
pub const SKIN_HUE_CENTER_DEG: f32 = 54.0;
/// Skin-hue protection band half-width, in degrees (see the module docs).
pub const SKIN_HALF_WIDTH_DEG: f32 = 20.0;
/// Fraction of the vibrance push suppressed for a pixel exactly at
/// [`SKIN_HUE_CENTER_DEG`] (an implementer's calibration constant, tuned so
/// a skin-tone patch's chroma movement stays comfortably under the C10 AC's
/// 30%-of-a-non-skin-patch bound, see `tests::skin_patch_moves_less_than_
/// 30_percent_of_a_non_skin_patch`).
pub const SKIN_PROTECT_STRENGTH: f32 = 0.85;

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "vibrance",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "saturation",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.vibrance_sat"),
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

/// Shortest signed angular distance `h - center`, wrapped into `(-180,
/// 180]`, a small self-contained copy of
/// `colorspace`'s internal helper (kept node-local, same "each node
/// duplicates the small formula, shares the heavy math" convention
/// `global_whites_blacks.wgsl` uses for its sRGB constants).
#[inline]
fn circ_delta(h: f32, center: f32) -> f32 {
    let mut d = (h - center) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// The skin-hue protection weight at `hue_deg`: `1.0` exactly at
/// [`SKIN_HUE_CENTER_DEG`], smoothly tapering to `0.0` at
/// `±`[`SKIN_HALF_WIDTH_DEG`] (a raised-cosine falloff, so, like the C7 hue
/// bands, there is no hard edge a hue sweep could show as a band).
#[inline]
fn skin_weight(hue_deg: f32) -> f32 {
    let d = circ_delta(hue_deg, SKIN_HUE_CENTER_DEG).abs();
    let denom = SKIN_HALF_WIDTH_DEG.max(1e-6);
    let t = (d / denom).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// Applies the full vibrance/saturation remap to one working-space RGBA
/// pixel (alpha untouched), the CPU parity anchor for
/// `global_vibrance_sat.wgsl`.
pub fn apply_vibrance_sat(rgba: [f32; 4], vibrance: f32, saturation: f32) -> [f32; 4] {
    let [l, c, h] = colorspace::working_to_oklch([rgba[0], rgba[1], rgba[2]]);
    let sat_scale = (1.0 + (saturation / 100.0) * SAT_UNIFORM_RANGE).max(0.0);
    let chroma_weight = (1.0 - c / CHROMA_REF).clamp(0.0, 1.0);
    let skin = skin_weight(h);
    let vib_scale =
        1.0 + (vibrance / 100.0) * VIB_RANGE * chroma_weight * (1.0 - skin * SKIN_PROTECT_STRENGTH);
    let new_c = (c * sat_scale * vib_scale).max(0.0);
    let [r, g, b] = colorspace::oklch_to_working([l, new_c, h]);
    [r, g, b, rgba[3]]
}

/// The `global.vibrance_sat` develop node.
#[derive(Default)]
pub struct VibranceSatNode {}

impl VibranceSatNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.vibrance_sat");

    /// A fresh node.
    pub fn new() -> VibranceSatNode {
        VibranceSatNode::default()
    }
}

impl RenderNode for VibranceSatNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
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
            .ok_or_else(|| NodeError::Other("global.vibrance_sat: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.vibrance_sat: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let vibrance = params.get_f64_or("vibrance", 0.0) as f32;
        let saturation = params.get_f64_or("saturation", 0.0) as f32;
        let mut data = Vec::with_capacity(40);
        data.extend_from_slice(&colorspace::matrices_flat());
        data.extend_from_slice(&[
            vibrance,
            saturation,
            SKIN_HUE_CENTER_DEG,
            SKIN_HALF_WIDTH_DEG,
        ]);
        let buf = storage_buffer(ctx.device, ctx.queue, "global.vibrance_sat data", &data);

        let pipeline = ctx.kernels.compute_pipeline(VIBRANCE_SAT_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.vibrance_sat in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.vibrance_sat out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_data = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.vibrance_sat data"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_data],
            extent,
            "global.vibrance_sat",
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
            return Err(NodeError::Cpu(
                "global.vibrance_sat: missing input tile".into(),
            ));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.vibrance_sat: missing input tile".into()))?
            .pixels;
        let vibrance = params.get_f64_or("vibrance", 0.0) as f32;
        let saturation = params.get_f64_or("saturation", 0.0) as f32;
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_vibrance_sat(p, vibrance, saturation);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for VibranceSatNode {
    fn is_identity(p: &GlobalStages) -> bool {
        // D1's Monochrome-elides-downstream-color-nodes contract (see
        // `hsl::HslNode::is_identity`'s doc comment for the full rationale;
        // proved end-to-end by `tests/e10_bw_mix.rs`).
        p.treatment == Treatment::BlackAndWhite || (p.vibrance == 0.0 && p.saturation == 0.0)
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([
            ("vibrance", ParamValue::Float(p.vibrance as f64)),
            ("saturation", ParamValue::Float(p.saturation as f64)),
        ])
        .expect("vibrance/saturation are always finite (clamped on ingest — spec A2)")
    }
}

/// A GPU storage buffer uploaded with `samples`' `f32` values, mirrors
/// `nodes::global::tone_curve`'s `storage_buffer` helper.
fn storage_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    samples: &[f32],
) -> wgpu::Buffer {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, &bytes);
    buf
}

/// Factory registering [`VibranceSatNode`].
#[derive(Default)]
pub struct VibranceSatFactory {}

impl NodeFactory for VibranceSatFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(VibranceSatNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(VIBRANCE_SAT_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_at_zero_zero_is_an_exact_pixel_identity() {
        for rgba in [
            [0.5, 0.2, 0.8, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.02, 0.9, 0.4, 0.5],
        ] {
            let out = apply_vibrance_sat(rgba, 0.0, 0.0);
            for k in 0..4 {
                assert!((out[k] - rgba[k]).abs() < 1e-4, "rgba={rgba:?} out={out:?}");
            }
        }
    }

    #[test]
    fn is_identity_tracks_both_fields() {
        let mut g = GlobalStages::default();
        assert!(VibranceSatNode::is_identity(&g));
        g.vibrance = 5.0;
        assert!(!VibranceSatNode::is_identity(&g));
        g.vibrance = 0.0;
        g.saturation = -3.0;
        assert!(!VibranceSatNode::is_identity(&g));
    }

    /// Saturation +100 must visibly increase chroma on a mid-chroma pixel.
    #[test]
    fn saturation_boost_increases_chroma() {
        let rgb = colorspace::oklch_to_working([0.6, 0.10, 120.0]);
        let rgba = [rgb[0], rgb[1], rgb[2], 1.0];
        let out = apply_vibrance_sat(rgba, 0.0, 100.0);
        let [_, c_before, _] = colorspace::working_to_oklch([rgba[0], rgba[1], rgba[2]]);
        let [_, c_after, _] = colorspace::working_to_oklch([out[0], out[1], out[2]]);
        assert!(
            c_after > c_before * 1.5,
            "saturation+100 should roughly double chroma: before={c_before:.4} after={c_after:.4}"
        );
    }

    /// Vibrance boosts a LOW-chroma pixel much more than an already-vivid
    /// one (the chroma-weighting half of C10's contract).
    #[test]
    fn vibrance_boosts_low_chroma_more_than_high_chroma() {
        // Both at a hue far from the skin-protection band.
        let hue = 200.0;
        let low_rgb = colorspace::oklch_to_working([0.6, 0.03, hue]);
        let high_rgb = colorspace::oklch_to_working([0.6, 0.28, hue]);
        let low = [low_rgb[0], low_rgb[1], low_rgb[2], 1.0];
        let high = [high_rgb[0], high_rgb[1], high_rgb[2], 1.0];

        let low_out = apply_vibrance_sat(low, 100.0, 0.0);
        let high_out = apply_vibrance_sat(high, 100.0, 0.0);

        let [_, c_low_before, _] = colorspace::working_to_oklch([low[0], low[1], low[2]]);
        let [_, c_low_after, _] =
            colorspace::working_to_oklch([low_out[0], low_out[1], low_out[2]]);
        let [_, c_high_before, _] = colorspace::working_to_oklch([high[0], high[1], high[2]]);
        let [_, c_high_after, _] =
            colorspace::working_to_oklch([high_out[0], high_out[1], high_out[2]]);

        let low_ratio = c_low_after / c_low_before;
        let high_ratio = c_high_after / c_high_before;
        assert!(
            low_ratio > high_ratio,
            "low-chroma pixel should scale up more than a near-saturated one: \
             low_ratio={low_ratio:.4} high_ratio={high_ratio:.4}"
        );
    }

    /// C10 AC: a skin-tone patch (documented hue range) moves < 30% of a
    /// non-skin patch's chroma delta at vibrance +100, holding starting
    /// chroma/lightness equal so the comparison isolates the skin-hue
    /// protection term specifically (not the chroma-weighting term, which
    /// both patches share identically here).
    #[test]
    fn skin_patch_moves_less_than_30_percent_of_a_non_skin_patch() {
        let (l, c0) = (0.6, 0.08); // representative of the swatch table's chroma range
        let skin_rgb = colorspace::oklch_to_working([l, c0, SKIN_HUE_CENTER_DEG]);
        let non_skin_rgb = colorspace::oklch_to_working([l, c0, 200.0]); // far outside the band

        let skin = [skin_rgb[0], skin_rgb[1], skin_rgb[2], 1.0];
        let non_skin = [non_skin_rgb[0], non_skin_rgb[1], non_skin_rgb[2], 1.0];

        let skin_out = apply_vibrance_sat(skin, 100.0, 0.0);
        let non_skin_out = apply_vibrance_sat(non_skin, 100.0, 0.0);

        let [_, c_skin_before, _] = colorspace::working_to_oklch([skin[0], skin[1], skin[2]]);
        let [_, c_skin_after, _] =
            colorspace::working_to_oklch([skin_out[0], skin_out[1], skin_out[2]]);
        let [_, c_non_before, _] =
            colorspace::working_to_oklch([non_skin[0], non_skin[1], non_skin[2]]);
        let [_, c_non_after, _] =
            colorspace::working_to_oklch([non_skin_out[0], non_skin_out[1], non_skin_out[2]]);

        let skin_delta = (c_skin_after - c_skin_before).abs();
        let non_skin_delta = (c_non_after - c_non_before).abs();
        let ratio = skin_delta / non_skin_delta;
        println!(
            "[C10][skin-protection] skin ΔC={skin_delta:.5} non-skin ΔC={non_skin_delta:.5} ratio={ratio:.4}"
        );
        assert!(
            ratio < 0.30,
            "skin patch must move < 30% of the non-skin patch at vibrance +100: ratio={ratio:.4}"
        );
    }

    /// Documents (and pins) the skin-hue-center swatch-table derivation
    /// cited in the module docs: recomputes the mean hue of the 8 listed
    /// swatches and checks it matches [`SKIN_HUE_CENTER_DEG`].
    #[test]
    fn skin_hue_center_matches_its_swatch_table() {
        fn srgb_eotf(u: f32) -> f32 {
            if u <= 0.040_449_936 {
                u / 12.92
            } else {
                ((u + 0.055) / 1.055).powf(2.4)
            }
        }
        use lightbox_color::matrix::{spaces, Vec3};
        let swatches_8bit: [[u8; 3]; 8] = [
            [255, 224, 196],
            [241, 194, 162],
            [224, 172, 135],
            [198, 134, 92],
            [161, 102, 63],
            [133, 85, 57],
            [92, 59, 40],
            [58, 38, 28],
        ];
        let srgb_to_work = spaces::working_to_linear_srgb()
            .inverse()
            .expect("sRGB->working is well-conditioned");
        let mut sum_h = 0.0f32;
        for c8 in swatches_8bit {
            let enc = [
                c8[0] as f32 / 255.0,
                c8[1] as f32 / 255.0,
                c8[2] as f32 / 255.0,
            ];
            let lin = enc.map(srgb_eotf);
            let v = Vec3([lin[0] as f64, lin[1] as f64, lin[2] as f64]);
            let w = srgb_to_work.mul_vec(v);
            let working = [w.0[0] as f32, w.0[1] as f32, w.0[2] as f32];
            let [_, _, h] = colorspace::working_to_oklch(working);
            sum_h += h;
        }
        let mean = sum_h / 8.0;
        assert!(
            (mean - SKIN_HUE_CENTER_DEG).abs() < 0.1,
            "swatch-table mean hue {mean:.3} vs documented center {SKIN_HUE_CENTER_DEG}"
        );
    }
}
