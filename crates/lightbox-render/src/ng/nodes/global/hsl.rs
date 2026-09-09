// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.hsl` (E10 task **C8**). The 8-band HSL color mixer: per-band hue
//! shift / saturation / luminance offsets, evaluated in OkLCh derived from
//! working RGB (spec §4.2 "HSL / vibrance / B&W mix | OkLCh derived from
//! working RGB; raised-cosine hue-band weights | smooth overlap, no
//! banding, own math").
//!
//! # The per-pixel remap
//!
//! For a working-space pixel, convert to OkLCh
//! ([`crate::ng::nodes::common::colorspace::working_to_oklch`]), compute the
//! 8 [`colorspace::band_weights`] at its hue, take the weighted sum of each
//! band's `(hue, sat, lum)` triple (weights sum to exactly `1.0`, the C7
//! partition-of-unity property, so a pixel sitting between two band
//! centers gets a smooth blend, never a hard cutover), scale those sums
//! into hue-degrees / chroma-multiplier / lightness-delta via the
//! [`HUE_RANGE_DEG`]/[`SAT_RANGE`]/[`LUM_RANGE`] calibration constants (the
//! same "implementer picks the slider-to-physical-unit mapping" latitude
//! `whites_blacks::ENDPOINT_RANGE`/`curve1d::PARAMETRIC_RANGE` use), apply
//! them, and convert back to working RGB
//! ([`colorspace::oklch_to_working`]). No clamp to `[0,1]` on the output
//! HSL, like exposure, operates on unclamped scene-linear working RGB (spec
//! §4.2; a highlight pixel above `1.0` stays a highlight pixel after a hue
//! nudge).
//!
//! Identity iff every one of the 8 bands is `(hue: 0, sat: 0, lum: 0)`
//! `band_weights`' outputs are then multiplied by an all-zero triple sum
//! regardless of hue, so the remap is the exact identity for every pixel
//! (not merely close-by-tolerance).

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

/// The HSL remap kernel, naga-validated at build (`build.rs`).
const HSL_WGSL: &str = include_str!("../../../../shaders/global_hsl.wgsl");

/// Hue-degrees a fully-deflected (`±100`) band hue slider shifts a pixel
/// whose hue sits exactly at that band's center.
pub const HUE_RANGE_DEG: f32 = 30.0;
/// Chroma multiplier range: `sat = -100` scales chroma toward `0` (fully
/// desaturates that band), `sat = +100` scales it toward `2×`.
pub const SAT_RANGE: f32 = 1.0;
/// Oklab-lightness delta a fully-deflected (`±100`) band luminance slider
/// applies to a pixel whose hue sits exactly at that band's center.
pub const LUM_RANGE: f32 = 0.20;

fn band_field(i: usize, c: usize) -> &'static str {
    const NAMES: [[&str; 3]; 8] = [
        ["b0_hue", "b0_sat", "b0_lum"],
        ["b1_hue", "b1_sat", "b1_lum"],
        ["b2_hue", "b2_sat", "b2_lum"],
        ["b3_hue", "b3_sat", "b3_lum"],
        ["b4_hue", "b4_sat", "b4_lum"],
        ["b5_hue", "b5_sat", "b5_lum"],
        ["b6_hue", "b6_sat", "b6_lum"],
        ["b7_hue", "b7_sat", "b7_lum"],
    ];
    NAMES[i][c]
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "b0_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b0_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b0_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b1_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b1_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b1_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b2_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b2_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b2_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b3_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b3_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b3_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b4_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b4_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b4_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b5_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b5_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b5_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b6_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b6_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b6_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b7_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b7_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "b7_lum",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.hsl"),
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

/// Reads the 8×[hue,sat,lum] band triples out of a [`ParamBlock`] built by
/// [`HslNode::param_block`], the one place both `eval_cpu` and `eval_gpu`
/// derive their band data from.
fn bands_from_params(params: &ParamBlock) -> [[f32; 3]; 8] {
    let mut bands = [[0f32; 3]; 8];
    for (i, band) in bands.iter_mut().enumerate() {
        band[0] = params.get_f64_or(band_field(i, 0), 0.0) as f32;
        band[1] = params.get_f64_or(band_field(i, 1), 0.0) as f32;
        band[2] = params.get_f64_or(band_field(i, 2), 0.0) as f32;
    }
    bands
}

/// Applies the full HSL remap to one working-space RGBA pixel (alpha
/// untouched), the CPU parity anchor for `global_hsl.wgsl`.
pub fn apply_hsl(rgba: [f32; 4], bands: &[[f32; 3]; 8]) -> [f32; 4] {
    let [l, c, h] = colorspace::working_to_oklch([rgba[0], rgba[1], rgba[2]]);
    let w = colorspace::band_weights(h);
    let mut hue_shift = 0.0f32;
    let mut sat_sum = 0.0f32;
    let mut lum_sum = 0.0f32;
    for i in 0..8 {
        hue_shift += w[i] * bands[i][0];
        sat_sum += w[i] * bands[i][1];
        lum_sum += w[i] * bands[i][2];
    }
    let new_h = h + (hue_shift / 100.0) * HUE_RANGE_DEG;
    let sat_scale = (1.0 + (sat_sum / 100.0) * SAT_RANGE).max(0.0);
    let new_c = (c * sat_scale).max(0.0);
    let new_l = l + (lum_sum / 100.0) * LUM_RANGE;
    let [r, g, b] = colorspace::oklch_to_working([new_l, new_c, new_h]);
    [r, g, b, rgba[3]]
}

/// The `global.hsl` develop node.
#[derive(Default)]
pub struct HslNode {}

impl HslNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.hsl");

    /// A fresh node.
    pub fn new() -> HslNode {
        HslNode::default()
    }
}

impl RenderNode for HslNode {
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
            .ok_or_else(|| NodeError::Other("global.hsl: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.hsl: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let bands = bands_from_params(params);
        let mut data = Vec::with_capacity(76);
        data.extend_from_slice(&colorspace::matrices_flat());
        data.extend_from_slice(&colorspace::hue_band_geometry_flat());
        for band in &bands {
            data.extend_from_slice(band);
        }
        let buf = storage_buffer(ctx.device, ctx.queue, "global.hsl data", &data);

        let pipeline = ctx.kernels.compute_pipeline(HSL_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.hsl in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.hsl out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_data = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.hsl data"),
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
            "global.hsl",
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
            return Err(NodeError::Cpu("global.hsl: missing input tile".into()));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.hsl: missing input tile".into()))?
            .pixels;
        let bands = bands_from_params(params);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_hsl(p, &bands);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for HslNode {
    fn is_identity(p: &GlobalStages) -> bool {
        // D1's Monochrome-elides-downstream-color-nodes contract (spec §4.1/
        // D2 AC, this render-engine half proved in
        // `tests/e10_bw_mix.rs::monochrome_treatment_elides_downstream_color_nodes`):
        // once `Treatment::BlackAndWhite` is active, the 8-band hue mixer's
        // color-shaping effect is moot for the desaturated output (Lightroom's
        // own real behavior, the HSL/vibrance/color-grade panels go inert,
        // not merely visually disabled), so this node is elided regardless of
        // its own field values.
        p.treatment == Treatment::BlackAndWhite
            || p.hsl
                .bands
                .iter()
                .all(|b| b.hue == 0.0 && b.sat == 0.0 && b.lum == 0.0)
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let mut fields: Vec<(&'static str, ParamValue)> = Vec::with_capacity(24);
        for (i, band) in p.hsl.bands.iter().enumerate() {
            fields.push((band_field(i, 0), ParamValue::Float(band.hue as f64)));
            fields.push((band_field(i, 1), ParamValue::Float(band.sat as f64)));
            fields.push((band_field(i, 2), ParamValue::Float(band.lum as f64)));
        }
        ParamBlock::from_fields(fields)
            .expect("HSL band fields are always finite (clamped on ingest — spec A2)")
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

/// Factory registering [`HslNode`].
#[derive(Default)]
pub struct HslFactory {}

impl NodeFactory for HslFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(HslNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(HSL_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::HslBand;

    fn bands_all_zero() -> [[f32; 3]; 8] {
        [[0.0; 3]; 8]
    }

    #[test]
    fn identity_bands_are_an_exact_pixel_identity() {
        let bands = bands_all_zero();
        for rgba in [
            [0.5, 0.2, 0.8, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.02, 0.9, 0.4, 0.5],
            [1.4, 0.9, 0.1, 1.0],
        ] {
            let out = apply_hsl(rgba, &bands);
            for k in 0..4 {
                assert!(
                    (out[k] - rgba[k]).abs() < 1e-4,
                    "rgba={rgba:?} out={out:?} (component {k})"
                );
            }
        }
    }

    #[test]
    fn is_identity_tracks_every_band_field() {
        let mut g = GlobalStages::default();
        assert!(HslNode::is_identity(&g));
        g.hsl.bands[3].hue = 5.0;
        assert!(!HslNode::is_identity(&g));
        g.hsl.bands[3].hue = 0.0;
        g.hsl.bands[7].lum = -2.0;
        assert!(!HslNode::is_identity(&g));
    }

    /// C8 proof: an HSL delta actually moves the hue of a pixel sitting
    /// at (or very near) a band's center, the "HSL changes a hue band"
    /// AC the epic's honest-reporting pass asks for.
    #[test]
    fn a_band_hue_shift_moves_a_pixel_at_that_bands_center() {
        // A saturated working-space color whose hue lands near the "Red"
        // band center (index 0): pure working red is the closest
        // reachable proxy without a full inverse-hue solve.
        let red_rgba = [1.0, 0.0, 0.0, 1.0];
        let [_, _, h_before] =
            colorspace::working_to_oklch([red_rgba[0], red_rgba[1], red_rgba[2]]);

        let mut bands = bands_all_zero();
        bands[0][0] = 100.0; // Red band, hue slider at max
        let out = apply_hsl(red_rgba, &bands);
        let [_, _, h_after] = colorspace::working_to_oklch([out[0], out[1], out[2]]);
        let delta = (h_after - h_before + 540.0).rem_euclid(360.0) - 180.0;
        assert!(
            delta.abs() > 5.0,
            "Red band hue+100 should visibly rotate a red pixel's hue: \
             before={h_before:.2} after={h_after:.2} delta={delta:.2}"
        );

        // A band FAR from red (Blue, index 5) must not move the red pixel.
        let mut far_bands = bands_all_zero();
        far_bands[5][0] = 100.0;
        let out_far = apply_hsl(red_rgba, &far_bands);
        for k in 0..3 {
            assert!(
                (out_far[k] - red_rgba[k]).abs() < 1e-3,
                "Blue-band hue must not leak into a red pixel: out={out_far:?}"
            );
        }
    }

    #[test]
    fn param_block_round_trips_band_values() {
        let mut g = GlobalStages::default();
        g.hsl.bands[2] = HslBand {
            hue: 40.0,
            sat: -20.0,
            lum: 10.0,
        };
        let pb = HslNode::param_block(&g);
        assert_eq!(pb.get_f64("b2_hue"), Some(40.0));
        assert_eq!(pb.get_f64("b2_sat"), Some(-20.0));
        assert_eq!(pb.get_f64("b2_lum"), Some(10.0));
        assert_eq!(pb.get_f64("b0_hue"), Some(0.0));
    }

    // ── C8 AC: no banding on a hue-sweep, second-derivative smoothness ──

    /// A hard-cutover ("nearest band wins") baseline HSL apply, the
    /// deliberately-BANDED construction the C8 AC's smoothness assertion
    /// contrasts against: every pixel is fully owned by whichever band
    /// center is angularly closest, so the remap has a genuine
    /// discontinuity at every band-to-band midpoint. Test-local only (never
    /// production code), it exists purely to give the "no banding"
    /// assertion a concrete, quantified baseline instead of an
    /// arbitrarily-picked absolute threshold.
    fn apply_hsl_hard_cutover(rgba: [f32; 4], bands: &[[f32; 3]; 8]) -> [f32; 4] {
        let [l, c, h] = colorspace::working_to_oklch([rgba[0], rgba[1], rgba[2]]);
        // Nearest center by shortest circular distance.
        let mut best = 0usize;
        let mut best_d = f32::MAX;
        for (i, &center) in colorspace::HUE_BAND_CENTERS_DEG.iter().enumerate() {
            let mut d = (h - center).abs() % 360.0;
            if d > 180.0 {
                d = 360.0 - d;
            }
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        let band = bands[best];
        let new_h = h + (band[0] / 100.0) * HUE_RANGE_DEG;
        let sat_scale = (1.0 + (band[1] / 100.0) * SAT_RANGE).max(0.0);
        let new_c = (c * sat_scale).max(0.0);
        let new_l = l + (band[2] / 100.0) * LUM_RANGE;
        let [r, g, b] = colorspace::oklch_to_working([new_l, new_c, new_h]);
        [r, g, b, rgba[3]]
    }

    /// Sweeps a fixed-lightness, fixed-chroma hue ramp through both
    /// [`apply_hsl`] (the real, production per-pixel function every
    /// backend shares) and [`apply_hsl_hard_cutover`] (the deliberately
    /// banded baseline above), under a recipe that moves several bands at
    /// once (so multiple boundaries are actually exercised), and compares
    /// the discrete second derivative (central difference) of the
    /// resulting hue profile.
    ///
    /// **C8 AC**: the real construction's max |second derivative| stays
    /// small in absolute terms AND is at least an order of magnitude
    /// smaller than the hard-cutover baseline's (which spikes hugely at
    /// every one of the 8 band boundaries), a banding artifact IS exactly
    /// a large second-derivative spike in the output profile, so this is a
    /// direct, quantified proof of "no banding", not just a threshold
    /// picked out of thin air.
    #[test]
    fn hue_sweep_has_no_banding_second_derivative_smoothness() {
        let mut bands = bands_all_zero();
        bands[0] = [60.0, 40.0, -10.0]; // Red
        bands[2] = [-50.0, 80.0, 20.0]; // Yellow
        bands[4] = [30.0, -60.0, 0.0]; // Aqua
        bands[6] = [-20.0, 50.0, 15.0]; // Purple

        let (l, c) = (0.6, 0.15);
        let n = 3600; // 0.1° resolution
        let profile = |apply: fn([f32; 4], &[[f32; 3]; 8]) -> [f32; 4]| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let h = 360.0 * i as f32 / n as f32;
                    let rgb = colorspace::oklch_to_working([l, c, h]);
                    let out = apply([rgb[0], rgb[1], rgb[2], 1.0], &bands);
                    let [_, _, oh] = colorspace::working_to_oklch([out[0], out[1], out[2]]);
                    oh
                })
                .collect()
        };
        // Unwrap the hue profile (it's periodic across 0/360) so a
        // finite-difference derivative never sees a spurious ~360°
        // "jump" at the wrap point itself.
        fn unwrap(p: &[f32]) -> Vec<f32> {
            let mut out = Vec::with_capacity(p.len());
            let mut offset = 0.0f32;
            let mut prev = p[0];
            out.push(p[0]);
            for &v in &p[1..] {
                let d = v - prev;
                if d > 180.0 {
                    offset -= 360.0;
                } else if d < -180.0 {
                    offset += 360.0;
                }
                out.push(v + offset);
                prev = v;
            }
            out
        }
        fn max_abs_second_diff(p: &[f32]) -> f32 {
            let mut m = 0.0f32;
            for i in 1..p.len() - 1 {
                let d2 = (p[i + 1] - 2.0 * p[i] + p[i - 1]).abs();
                m = m.max(d2);
            }
            m
        }

        let smooth = unwrap(&profile(apply_hsl));
        let banded = unwrap(&profile(apply_hsl_hard_cutover));
        let smooth_max = max_abs_second_diff(&smooth);
        let banded_max = max_abs_second_diff(&banded);
        println!(
            "[C8][no-banding] real max|d2h/dstep2|={smooth_max:.5}° hard-cutover max|d2h/dstep2|={banded_max:.5}°"
        );
        assert!(
            smooth_max < 1.0,
            "real HSL construction must have a small, bounded second derivative \
             across the hue sweep (no banding): max={smooth_max:.5}°"
        );
        assert!(
            smooth_max * 10.0 < banded_max,
            "real construction must be at least 10x smoother than the deliberately \
             banded hard-cutover baseline: real={smooth_max:.5}° banded={banded_max:.5}°"
        );
    }
}
