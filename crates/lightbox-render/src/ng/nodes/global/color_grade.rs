// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.color_grade` (E10 tasks **C11-C12**). Three-way (shadows /
//! midtones / highlights) plus a global wheel colour-grading node, evaluated
//! in OkLCh derived from working RGB (spec §4.2 "Color grading | luma-zone
//! weights over companion-encoded luma; hue/sat wheels applied as chroma
//! offsets in OkLCh, per-zone lum as gain | ASC-CDL-style behavior with
//! perceptual chroma").
//!
//! # C11, luma-zone weights (blending/balance semantics)
//!
//! [`zone_weights`] partitions the companion-encoded luma axis `y ∈ [0,1]`
//! into three smoothly-blended regions (shadows/midtones/highlights) around
//! two boundaries ([`BASE_BOUNDARY_LOW`]/[`BASE_BOUNDARY_HIGH`]), using the
//! exact same "shared boundary ramp, provably non-negative difference"
//! construction `nodes::common::curve1d::region_weights` uses for the tone
//! curve's 4 linear regions, here specialized to 2 boundaries / 3 regions
//! (see that module's doc comment for the closed-form partition-of-unity
//! proof; the derivation is identical, just one boundary fewer). **Blending**
//! (`0..=100`, id `50`) widens/narrows the transition half-width around each
//! boundary, `0` ⇒ a near-hard cut between zones, `100` ⇒ a wide, soft
//! overlap. **Balance** (`-100..=100`, id `0`) shifts BOTH boundaries by the
//! same signed offset ([`zone_boundaries`]), positive balance pushes the
//! shadow/midtone and midtone/highlight boundaries up (toward `1`), growing
//! the shadow zone at the highlight zone's expense; negative balance does the
//! reverse. The **global** wheel is not part of this 3-region partition, it
//! always carries weight `1.0` (spec's own "Global" wheel semantics: applies
//! everywhere, additively on top of whichever tonal-zone wheel a pixel's luma
//! falls into).
//!
//! # C11, wheel → OkLCh chroma offset + per-zone luminance gain
//!
//! Each wheel (`hue_deg ∈ [0,360)`, `sat ∈ [0,100]`, `lum ∈ [-100,100]`)
//! contributes an **additive Oklab chroma vector**, `sat/100 *
//! [`GRADE_CHROMA_RANGE`] * (cos(hue), sin(hue))` in the Oklab `(a, b)` plane
//! scaled by that wheel's zone weight (the three tonal wheels) or `1.0`
//! (global), and an **additive Oklab lightness delta**, `weight * lum/100 *
//! [`GRADE_LUM_RANGE`]`, the same additive-delta shape `nodes::global::hsl`
//! uses for its own per-band luminance offset (an implementer's calibration
//! choice, the same latitude `whites_blacks::ENDPOINT_RANGE` /
//! `hsl::LUM_RANGE` already establish; spec §4.2 says "per-zone lum as
//! gain" without pinning the exact formula). Because every wheel's
//! contribution is linear in `sat`/`lum`, **`sat == 0` is the exact identity
//! for that wheel regardless of `hue`** (task C11's own AC), `cos`/`sin` are
//! multiplied by a zero chroma magnitude unconditionally.

use std::sync::Arc;

use lightbox_color::matrix::spaces::companion_encode;
use lightbox_edit::{GlobalStages, Treatment};

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::common::colorspace;
use crate::ng::nodes::global::tone_recovery::working_luma_weights;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The colour-grade remap kernel, naga-validated at build (`build.rs`).
const COLOR_GRADE_WGSL: &str = include_str!("../../../../shaders/global_color_grade.wgsl");

/// The shadow/midtone boundary at `blending = 50` (id), `balance = 0` (id)
/// PV1's own calibration pick (spec §4.2 leaves the exact zone geometry to
/// the implementer, same latitude `curve1d`'s `TRANSITION_HALF_WIDTH` uses).
pub const BASE_BOUNDARY_LOW: f32 = 1.0 / 3.0;
/// The midtone/highlight boundary at identity blending/balance.
pub const BASE_BOUNDARY_HIGH: f32 = 2.0 / 3.0;
/// The encoded-luma span a fully-deflected (`±100`) balance slider shifts
/// BOTH boundaries by (task C11 "balance shifts zone boundaries
/// monotonically").
pub const BALANCE_RANGE: f32 = 0.25;
/// Transition half-width at `blending = 0` (near-hard zone cut).
pub const BLEND_HALF_WIDTH_MIN: f32 = 0.03;
/// Transition half-width at `blending = 100` (wide, soft zone overlap).
pub const BLEND_HALF_WIDTH_MAX: f32 = 0.28;
/// Oklab chroma a fully-saturated (`sat = 100`) wheel contributes at full
/// (`1.0`) zone weight, calibrated so a warm-highlights/cool-shadows wheel
/// is clearly visible without blowing out the gamut (Oklab chroma for vivid
/// sRGB-gamut colors tops out ~0.3-0.4, see `colorspace::HUE_BAND_CENTERS_DEG`'s
/// swatch table). Kept comfortably below that ceiling (rather than pushed to
/// it) so a highlights/whites-zone wheel pairs with an already-bright,
/// low-headroom pixel without straddling the sRGB gamut boundary, the exact
/// corner where GPU/CPU transcendental rounding (§4.4's own "no bit-identity"
/// contract) is most visible as a parity outlier.
pub const GRADE_CHROMA_RANGE: f32 = 0.11;
/// Oklab lightness delta a fully-deflected (`±100`) wheel luminance slider
/// contributes at full zone weight.
pub const GRADE_LUM_RANGE: f32 = 0.15;

fn wheel_field(i: usize, c: usize) -> &'static str {
    const NAMES: [[&str; 3]; 4] = [
        ["sh_hue", "sh_sat", "sh_lum"],
        ["mid_hue", "mid_sat", "mid_lum"],
        ["hi_hue", "hi_sat", "hi_lum"],
        ["gl_hue", "gl_sat", "gl_lum"],
    ];
    NAMES[i][c]
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "sh_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "sh_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "sh_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "mid_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "mid_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "mid_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "hi_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "hi_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "hi_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "gl_hue",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "gl_sat",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "gl_lum",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "blending",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "balance",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.color_grade"),
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

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let denom = (edge1 - edge0).max(1.0e-6);
    let t = ((x - edge0) / denom).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The two zone boundaries at `balance` (task C11): both shift by the SAME
/// signed offset, so they are strictly monotone non-decreasing functions of
/// `balance` (a linear shift, proven directly by
/// `tests::balance_shifts_both_boundaries_monotonically`).
#[must_use]
pub fn zone_boundaries(balance: f32) -> (f32, f32) {
    let shift = (balance / 100.0) * BALANCE_RANGE;
    (BASE_BOUNDARY_LOW + shift, BASE_BOUNDARY_HIGH + shift)
}

/// The 3 tonal-zone weights `[shadows, midtones, highlights]` at
/// companion-encoded luma `y_enc` (task C11). **Exact partition of unity**
/// (`Σw == 1.0` for every `y_enc`, algebraically, `(1-t0)+(t0-t1)+t1 == 1`
/// unconditionally, and pointwise non-negative because the transition
/// half-width is capped below half the gap between the two boundaries, so
/// `t0(y) >= t1(y)` everywhere; see the module docs for the full derivation,
/// which mirrors `curve1d::region_weights`'s proof one boundary down).
#[must_use]
pub fn zone_weights(y_enc: f32, blending: f32, balance: f32) -> [f32; 3] {
    let (b0, b1) = zone_boundaries(balance);
    let gap = (b1 - b0).max(1.0e-4);
    let t = (blending / 100.0).clamp(0.0, 1.0);
    let hw = (BLEND_HALF_WIDTH_MIN + t * (BLEND_HALF_WIDTH_MAX - BLEND_HALF_WIDTH_MIN))
        .min(0.49 * gap)
        .max(1.0e-4);
    let t0 = smoothstep(b0 - hw, b0 + hw, y_enc);
    let t1 = smoothstep(b1 - hw, b1 + hw, y_enc);
    [1.0 - t0, t0 - t1, t1]
}

/// Companion-encoded scalar luma of a working-space RGB triple (spec §4.2
/// "luma-zone weights over companion-encoded luma"): the working→XYZ `Y` row
/// dot product (same weights `nodes::global::tone_recovery` uses), encoded
/// via the shared sRGB-shaped companion curve. `companion_encode` applies the
/// SAME curve to all three input channels, so feeding it an equal-RGB triple
/// is exactly the encode of the scalar luma (the same trick
/// `contrast::contrast_pivot` uses for its encoded-18%-gray constant).
#[must_use]
pub fn encoded_luma(rgb: [f32; 3]) -> f32 {
    let w = working_luma_weights();
    let lin = (rgb[0] * w[0] + rgb[1] * w[1] + rgb[2] * w[2]).max(0.0);
    companion_encode([lin, lin, lin])[0]
}

/// Applies the full colour-grade remap to one working-space RGBA pixel
/// (alpha untouched), the CPU parity anchor for `global_color_grade.wgsl`.
/// `wheels` is `[shadows, midtones, highlights, global]`, each `[hue_deg,
/// sat, lum]`.
#[must_use]
pub fn apply_color_grade(
    rgba: [f32; 4],
    wheels: &[[f32; 3]; 4],
    blending: f32,
    balance: f32,
) -> [f32; 4] {
    let y_enc = encoded_luma([rgba[0], rgba[1], rgba[2]]);
    let zw = zone_weights(y_enc, blending, balance);
    let weights = [zw[0], zw[1], zw[2], 1.0]; // + the always-on global wheel

    let [l, a, b] = colorspace::working_to_oklab([rgba[0], rgba[1], rgba[2]]);
    let mut off_a = 0.0f32;
    let mut off_b = 0.0f32;
    let mut l_delta = 0.0f32;
    for (i, &w) in weights.iter().enumerate() {
        if w == 0.0 {
            continue;
        }
        let (hue, sat, lum) = (wheels[i][0], wheels[i][1], wheels[i][2]);
        let chroma = (sat / 100.0) * GRADE_CHROMA_RANGE;
        let hr = hue.to_radians();
        off_a += w * chroma * hr.cos();
        off_b += w * chroma * hr.sin();
        l_delta += w * (lum / 100.0) * GRADE_LUM_RANGE;
    }
    let [r, g, bl] = colorspace::oklab_to_working([l + l_delta, a + off_a, b + off_b]);
    [r, g, bl, rgba[3]]
}

/// Reads the 4×[hue,sat,lum] wheel triples + blending/balance out of a
/// [`ParamBlock`] built by [`ColorGradeNode::param_block`], the one place
/// both `eval_cpu` and `eval_gpu` derive their data from.
fn params_from(params: &ParamBlock) -> ([[f32; 3]; 4], f32, f32) {
    let mut wheels = [[0f32; 3]; 4];
    for (i, wheel) in wheels.iter_mut().enumerate() {
        wheel[0] = params.get_f64_or(wheel_field(i, 0), 0.0) as f32;
        wheel[1] = params.get_f64_or(wheel_field(i, 1), 0.0) as f32;
        wheel[2] = params.get_f64_or(wheel_field(i, 2), 0.0) as f32;
    }
    // `ColorGrade::blend`'s shipped neutral default is `0.0` (E09's plain
    // `#[derive(Default)]`, not the illustrative spec sketch's "id 50", an
    // earlier phase's decision, not relitigated here); the fallback below
    // only matters defensively (`param_block` always supplies the field).
    let blending = params.get_f64_or("blending", 0.0) as f32;
    let balance = params.get_f64_or("balance", 0.0) as f32;
    (wheels, blending, balance)
}

/// The `global.color_grade` develop node.
#[derive(Default)]
pub struct ColorGradeNode {}

impl ColorGradeNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.color_grade");

    /// A fresh node.
    pub fn new() -> ColorGradeNode {
        ColorGradeNode::default()
    }
}

impl RenderNode for ColorGradeNode {
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
            .ok_or_else(|| NodeError::Other("global.color_grade: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.color_grade: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let (wheels, blending, balance) = params_from(params);
        let mut data = Vec::with_capacity(53);
        data.extend_from_slice(&colorspace::matrices_flat());
        data.extend_from_slice(&working_luma_weights());
        data.push(blending);
        data.push(balance);
        for wheel in &wheels {
            data.extend_from_slice(wheel);
        }
        let buf = storage_buffer(ctx.device, ctx.queue, "global.color_grade data", &data);

        let pipeline = ctx.kernels.compute_pipeline(COLOR_GRADE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.color_grade in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.color_grade out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_data = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.color_grade data"),
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
            "global.color_grade",
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
                "global.color_grade: missing input tile".into(),
            ));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.color_grade: missing input tile".into()))?
            .pixels;
        let (wheels, blending, balance) = params_from(params);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_color_grade(p, &wheels, blending, balance);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for ColorGradeNode {
    fn is_identity(p: &GlobalStages) -> bool {
        // D1's Monochrome-elides-downstream-color-nodes contract (see
        // `hsl::HslNode::is_identity`'s doc comment for the full rationale;
        // proved end-to-end by `tests/e10_bw_mix.rs`).
        if p.treatment == Treatment::BlackAndWhite {
            return true;
        }
        let cg = &p.color_grade;
        [cg.shadows, cg.midtones, cg.highlights, cg.global]
            .iter()
            .all(|w| w.sat == 0.0 && w.lum == 0.0)
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let cg = &p.color_grade;
        let mut fields: Vec<(&'static str, ParamValue)> = Vec::with_capacity(14);
        for (i, wheel) in [cg.shadows, cg.midtones, cg.highlights, cg.global]
            .iter()
            .enumerate()
        {
            fields.push((wheel_field(i, 0), ParamValue::Float(wheel.hue as f64)));
            fields.push((wheel_field(i, 1), ParamValue::Float(wheel.sat as f64)));
            fields.push((wheel_field(i, 2), ParamValue::Float(wheel.lum as f64)));
        }
        fields.push(("blending", ParamValue::Float(cg.blend as f64)));
        fields.push(("balance", ParamValue::Float(cg.balance as f64)));
        ParamBlock::from_fields(fields)
            .expect("color-grade fields are always finite (clamped on ingest — spec A2)")
    }
}

/// A GPU storage buffer uploaded with `samples`' `f32` values, mirrors
/// `nodes::global::hsl`'s `storage_buffer` helper.
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

/// Factory registering [`ColorGradeNode`].
#[derive(Default)]
pub struct ColorGradeFactory {}

impl NodeFactory for ColorGradeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ColorGradeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(COLOR_GRADE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::GradeWheel;

    fn neutral_wheels() -> [[f32; 3]; 4] {
        [[0.0; 3]; 4]
    }

    // ── C11: zone weights ────────────────────────────────────────────────

    /// C11 AC: zones sum to exactly 1.0 for every encoded luma, at every
    /// corner of the blending/balance slider space.
    #[test]
    fn zone_weights_always_sum_to_one() {
        for blending in [0.0f32, 25.0, 50.0, 75.0, 100.0] {
            for balance in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
                for i in 0..=200 {
                    let y = i as f32 / 200.0;
                    let w = zone_weights(y, blending, balance);
                    let sum: f32 = w.iter().sum();
                    assert!(
                        (sum - 1.0).abs() < 1e-4,
                        "blending={blending} balance={balance} y={y}: weights {w:?} sum to {sum}"
                    );
                    for &c in &w {
                        assert!(
                            (-1e-4..=1.0 + 1e-4).contains(&c),
                            "blending={blending} balance={balance} y={y}: weight {c} out of [0,1]"
                        );
                    }
                }
            }
        }
    }

    /// C11 AC: balance shifts the zone boundaries monotonically.
    #[test]
    fn balance_shifts_both_boundaries_monotonically() {
        let mut prev = zone_boundaries(-100.0);
        for i in 1..=40 {
            let balance = -100.0 + i as f32 * 5.0;
            let cur = zone_boundaries(balance);
            assert!(
                cur.0 >= prev.0 - 1e-6 && cur.1 >= prev.1 - 1e-6,
                "balance={balance}: boundaries {cur:?} regressed from {prev:?}"
            );
            // Strictly increasing over a non-trivial step (not just
            // non-decreasing), the shift is a genuine linear function.
            assert!(cur.0 > prev.0 && cur.1 > prev.1);
            prev = cur;
        }
    }

    /// C11 AC: saturation 0 = identity regardless of hue, sweeping the hue
    /// of every wheel at sat=0/lum=0 never moves a pixel, at any luma zone.
    #[test]
    fn saturation_zero_is_identity_regardless_of_hue() {
        for zone_idx in 0..4 {
            for hue in [0.0f32, 45.0, 120.0, 200.0, 300.0, 359.0] {
                let mut wheels = neutral_wheels();
                wheels[zone_idx] = [hue, 0.0, 0.0];
                for rgba in [
                    [0.05, 0.05, 0.06, 1.0], // shadow-ish
                    [0.5, 0.45, 0.4, 1.0],   // mid-ish
                    [0.9, 0.92, 0.88, 1.0],  // highlight-ish
                ] {
                    let out = apply_color_grade(rgba, &wheels, 50.0, 0.0);
                    for k in 0..4 {
                        assert!(
                            (out[k] - rgba[k]).abs() < 1e-4,
                            "zone={zone_idx} hue={hue} rgba={rgba:?} out={out:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_wheel_neutral_is_an_exact_pixel_identity() {
        let wheels = neutral_wheels();
        for rgba in [
            [0.5, 0.2, 0.8, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.02, 0.9, 0.4, 0.5],
        ] {
            let out = apply_color_grade(rgba, &wheels, 50.0, 0.0);
            for k in 0..4 {
                assert!((out[k] - rgba[k]).abs() < 1e-4, "rgba={rgba:?} out={out:?}");
            }
        }
    }

    #[test]
    fn is_identity_tracks_every_wheels_sat_and_lum_but_not_hue() {
        let mut g = GlobalStages::default();
        assert!(ColorGradeNode::is_identity(&g));
        // A non-default hue with sat=lum=0 is STILL identity (C11 AC).
        g.color_grade.highlights.hue = 210.0;
        assert!(ColorGradeNode::is_identity(&g));
        g.color_grade.highlights.sat = 20.0;
        assert!(!ColorGradeNode::is_identity(&g));
        g.color_grade.highlights.sat = 0.0;
        g.color_grade.shadows.lum = -5.0;
        assert!(!ColorGradeNode::is_identity(&g));
    }

    #[test]
    fn param_block_round_trips_wheel_and_blend_balance_values() {
        let mut g = GlobalStages::default();
        g.color_grade.highlights = GradeWheel {
            hue: 40.0,
            sat: 60.0,
            lum: 5.0,
        };
        g.color_grade.blend = 70.0;
        g.color_grade.balance = -15.0;
        let pb = ColorGradeNode::param_block(&g);
        assert_eq!(pb.get_f64("hi_hue"), Some(40.0));
        assert_eq!(pb.get_f64("hi_sat"), Some(60.0));
        assert_eq!(pb.get_f64("hi_lum"), Some(5.0));
        assert_eq!(pb.get_f64("blending"), Some(70.0));
        assert_eq!(pb.get_f64("balance"), Some(-15.0));
        assert_eq!(pb.get_f64("sh_hue"), Some(0.0));
    }

    /// C11/C12 proof: a warm-highlights wheel visibly warms a bright
    /// (highlight-zone) pixel far more than a dark (shadow-zone) one, the
    /// "color grading changes pixels" honest-reporting proof at the pure-math
    /// level (the full render-pipeline proof lives in
    /// `tests/e10_color_grade.rs`).
    #[test]
    fn warm_highlights_wheel_warms_bright_pixels_more_than_dark_ones() {
        let mut wheels = neutral_wheels();
        wheels[2] = [40.0, 80.0, 0.0]; // highlights: warm orange, strong sat
        let dark = [0.02, 0.02, 0.02, 1.0];
        let bright = [0.95, 0.95, 0.95, 1.0];
        let dark_out = apply_color_grade(dark, &wheels, 50.0, 0.0);
        let bright_out = apply_color_grade(bright, &wheels, 50.0, 0.0);
        let warm_axis = |p: [f32; 4]| p[0] - p[2]; // R-B: warm/cool axis
        let dark_shift = warm_axis(dark_out) - warm_axis(dark);
        let bright_shift = warm_axis(bright_out) - warm_axis(bright);
        println!("[C11/C12] dark R-B shift={dark_shift:.5} bright R-B shift={bright_shift:.5}");
        assert!(
            bright_shift > dark_shift + 0.01,
            "a highlights-only warm wheel should warm bright pixels much more than dark ones: \
             dark_shift={dark_shift:.5} bright_shift={bright_shift:.5}"
        );
    }
}
