// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.tone_curve` (E10 task **C3**). Point + parametric tone curve
//! (spec §4.2 "tone curve = companion-encoded; x-axis 0..1 encoded |
//! LR-compatible curve shapes ('Melissa RGB' analogue)"), baked CPU-side
//! into 4×1D LUTs and sampled by both backends, never re-derived from the
//! raw control points per pixel.
//!
//! # The composed remap (task C2's "one 1D remap")
//!
//! For the composite ([`lightbox_edit::ToneCurveSet::rgb`]) channel:
//! `y(x) = clamp01( MonotoneCubic(rgb.points).eval(x) +
//! parametric_delta(x, parametric) )`, the point curve reshapes tones,
//! the parametric sliders add an on-top region-weighted offset evaluated at
//! the SAME input `x` (not the point curve's output, matching Lightroom's
//! own semantics where the region split points partition the *original*
//! luminosity zones, spec §4.3 `ParametricCurve`). Per-channel
//! ([`lightbox_edit::ToneCurveSet::r`]/`g`/`b`) LUTs are point-curve-only
//! **an implementer's decision, recorded in
//! `docs/plan/epics/E10-deviations.md` Phase C**: Lightroom's own Region
//! sliders act only on the master/RGB curve, never per-channel.
//!
//! # Why "composite applies per-channel", not via luma (a clarification of
//! the spec's node-table label)
//!
//! Spec §4.4's node inventory calls this node's LUT count "4×1D LUT bake
//! (luma + R/G/B channels)". This implementation applies the composite LUT
//! **independently to each of R, G, B** (three applications of the same
//! table), not via a luma-preserving ratio (contrast with
//! `ToneRecoveryNode`'s deliberate luma-ratio reapply). This is the
//! historically-correct, LR-compatible behavior, §4.2's own domain note
//! calls out "LR-compatible curve shapes", and is what produces the
//! well-known saturation boost of a strong S-curve; a luma-preserving
//! version would be a materially different (non-LR-compatible) tool.
//! "4×1D LUT" is satisfied literally: one composite table (applied 3×) plus
//! three independent per-channel tables.
//!
//! # LUT rebake cadence (task C3's "rebakes ONLY on a curve-param delta")
//!
//! [`ToneCurveNode::eval_gpu`]/[`ToneCurveNode::eval_cpu`] bake fresh from
//! [`ParamBlock`] on **every call**, but a call only ever happens on a
//! content-cache **miss** (spec §3.5): [`ToneCurveNode::param_block`]
//! encodes the full curve description into the node's [`ParamBlock`], whose
//! canonical bytes are part of the [`crate::ng::CacheKey`], so an unchanged
//! curve always cache-hits upstream of this node's `eval_*` and the bake
//! never runs, the same mechanism every other E10 node's "only recomputes
//! on its own param delta" property rides on (see `tests/e10_tone_curve.rs`'s
//! cache-probe case, which renders the identical recipe twice and asserts
//! zero additional node evaluations, then changes only the curve and asserts
//! exactly the tone-curve + downstream `xform.display` nodes re-evaluate).

use std::sync::Arc;

use lightbox_color::matrix::spaces::{companion_decode, companion_encode};
use lightbox_edit::{CurvePoint, GlobalStages, ParametricCurve, ToneCurve};

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::common::curve1d::{parametric_delta, MonotoneCubic};
use crate::ng::nodes::common::lut1d::Lut1D;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The 4-LUT-sample kernel, naga-validated at build (`build.rs`).
const TONE_CURVE_WGSL: &str = include_str!("../../../../shaders/global_tone_curve.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "rgb_pts",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "r_pts",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "g_pts",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "b_pts",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "p_highlights",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_lights",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_darks",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_shadows",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_split0",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_split1",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "p_split2",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.tone_curve"),
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

// ── ParamBlock <-> curve-description encoding ──────────────────────────────

/// Encodes control points as `"x0,y0;x1,y1;..."`, the compact `Text`
/// carrier [`ParamBlock`]'s `Float`/`Int`/`Bool`/`Text` value set supports
/// (no variable-length array `ParamValue` variant exists, spec's `ParamBlock`
/// is deliberately scalar-only, §3.1). Deterministic per exact `f32` bit
/// pattern (`{}`'s shortest round-trippable formatting), so an unchanged
/// curve always re-encodes to byte-identical canonical CBOR, the cache-key
/// stability the module docs' "rebake cadence" section relies on.
fn encode_points(points: &[CurvePoint]) -> String {
    let mut s = String::new();
    for (i, p) in points.iter().enumerate() {
        if i > 0 {
            s.push(';');
        }
        s.push_str(&p.x.to_string());
        s.push(',');
        s.push_str(&p.y.to_string());
    }
    s
}

/// Inverse of [`encode_points`]. Never panics on malformed input (untrusted
/// only in the sense that a future format change / hand-edited CBOR could
/// desync it): any unparseable token drops that point rather than failing
/// the whole decode, and a totally garbled string decodes to an empty point
/// list, which [`MonotoneCubic::build`] treats as the identity curve.
fn decode_points(s: &str) -> Vec<CurvePoint> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(';')
        .filter_map(|tok| {
            let (x, y) = tok.split_once(',')?;
            let x: f32 = x.parse().ok()?;
            let y: f32 = y.parse().ok()?;
            if x.is_finite() && y.is_finite() {
                Some(CurvePoint { x, y })
            } else {
                None
            }
        })
        .collect()
}

// ── LUT baking (shared by CPU and GPU eval, task C3) ──────────────────────

/// The 4 baked LUTs this node samples per pixel.
struct BakedToneCurve {
    composite: Lut1D,
    r: Lut1D,
    g: Lut1D,
    b: Lut1D,
}

/// Bakes all 4 LUTs from a [`ParamBlock`] built by [`ToneCurveNode::param_block`]
/// the ONE place both `eval_cpu` and `eval_gpu` derive their curve data
/// from, so the two backends can never see different curves.
fn bake_from_params(params: &ParamBlock) -> BakedToneCurve {
    let rgb_points = decode_points(params.get_str("rgb_pts").unwrap_or(""));
    let r_points = decode_points(params.get_str("r_pts").unwrap_or(""));
    let g_points = decode_points(params.get_str("g_pts").unwrap_or(""));
    let b_points = decode_points(params.get_str("b_pts").unwrap_or(""));
    let parametric = ParametricCurve {
        highlights: params.get_f64_or("p_highlights", 0.0) as f32,
        lights: params.get_f64_or("p_lights", 0.0) as f32,
        darks: params.get_f64_or("p_darks", 0.0) as f32,
        shadows: params.get_f64_or("p_shadows", 0.0) as f32,
        splits: [
            params.get_f64_or("p_split0", 0.25) as f32,
            params.get_f64_or("p_split1", 0.50) as f32,
            params.get_f64_or("p_split2", 0.75) as f32,
        ],
    };

    let rgb_spline = MonotoneCubic::build(&rgb_points);
    let r_spline = MonotoneCubic::build(&r_points);
    let g_spline = MonotoneCubic::build(&g_points);
    let b_spline = MonotoneCubic::build(&b_points);

    BakedToneCurve {
        composite: Lut1D::bake(|x| {
            (rgb_spline.eval(x) + parametric_delta(x, &parametric)).clamp(0.0, 1.0)
        }),
        r: Lut1D::bake(|x| r_spline.eval(x).clamp(0.0, 1.0)),
        g: Lut1D::bake(|x| g_spline.eval(x).clamp(0.0, 1.0)),
        b: Lut1D::bake(|x| b_spline.eval(x).clamp(0.0, 1.0)),
    }
}

/// Applies the full companion-domain tone curve to one working-space RGBA
/// pixel (alpha untouched): composite LUT to R/G/B, then the matching
/// per-channel LUT, the CPU parity anchor for `global_tone_curve.wgsl`.
fn apply_tone_curve(rgba: [f32; 4], baked: &BakedToneCurve) -> [f32; 4] {
    let enc = companion_encode([rgba[0], rgba[1], rgba[2]]);
    let mr = baked.composite.sample(enc[0]);
    let mg = baked.composite.sample(enc[1]);
    let mb = baked.composite.sample(enc[2]);
    let fr = baked.r.sample(mr);
    let fg = baked.g.sample(mg);
    let fb = baked.b.sample(mb);
    let dec = companion_decode([fr, fg, fb]);
    [dec[0], dec[1], dec[2], rgba[3]]
}

/// The `global.tone_curve` develop node.
#[derive(Default)]
pub struct ToneCurveNode {}

impl ToneCurveNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.tone_curve");

    /// A fresh node.
    pub fn new() -> ToneCurveNode {
        ToneCurveNode::default()
    }
}

impl RenderNode for ToneCurveNode {
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
            .ok_or_else(|| NodeError::Other("global.tone_curve: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.tone_curve: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let baked = bake_from_params(params);
        let n = baked.composite.samples().len() as u32;
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&n.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.tone_curve params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let buf_composite = storage_buffer(
            ctx.device,
            ctx.queue,
            "global.tone_curve lut composite",
            baked.composite.samples(),
        );
        let buf_r = storage_buffer(
            ctx.device,
            ctx.queue,
            "global.tone_curve lut r",
            baked.r.samples(),
        );
        let buf_g = storage_buffer(
            ctx.device,
            ctx.queue,
            "global.tone_curve lut g",
            baked.g.samples(),
        );
        let buf_b = storage_buffer(
            ctx.device,
            ctx.queue,
            "global.tone_curve lut b",
            baked.b.samples(),
        );

        let pipeline = ctx.kernels.compute_pipeline(TONE_CURVE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_curve in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_curve out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_curve params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let bg_lut = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_curve lut"),
            layout: &pipeline.get_bind_group_layout(3),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf_composite.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf_r.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf_g.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf_b.as_entire_binding(),
                },
            ],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params, &bg_lut],
            extent,
            "global.tone_curve",
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
                "global.tone_curve: missing input tile".into(),
            ));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.tone_curve: missing input tile".into()))?
            .pixels;
        let baked = bake_from_params(params);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_tone_curve(p, &baked);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for ToneCurveNode {
    fn is_identity(p: &GlobalStages) -> bool {
        let tc = &p.tone_curve;
        curve_is_identity(&tc.rgb)
            && curve_is_identity(&tc.r)
            && curve_is_identity(&tc.g)
            && curve_is_identity(&tc.b)
            && tc.parametric.is_identity()
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let tc = &p.tone_curve;
        ParamBlock::from_fields([
            ("rgb_pts", ParamValue::Text(encode_points(&tc.rgb.points))),
            ("r_pts", ParamValue::Text(encode_points(&tc.r.points))),
            ("g_pts", ParamValue::Text(encode_points(&tc.g.points))),
            ("b_pts", ParamValue::Text(encode_points(&tc.b.points))),
            (
                "p_highlights",
                ParamValue::Float(tc.parametric.highlights as f64),
            ),
            ("p_lights", ParamValue::Float(tc.parametric.lights as f64)),
            ("p_darks", ParamValue::Float(tc.parametric.darks as f64)),
            ("p_shadows", ParamValue::Float(tc.parametric.shadows as f64)),
            (
                "p_split0",
                ParamValue::Float(tc.parametric.splits[0] as f64),
            ),
            (
                "p_split1",
                ParamValue::Float(tc.parametric.splits[1] as f64),
            ),
            (
                "p_split2",
                ParamValue::Float(tc.parametric.splits[2] as f64),
            ),
        ])
        .expect("tone-curve fields are always finite (clamped on ingest — spec A2/C2)")
    }
}

/// A channel curve is identity iff it has `< 2` points (matches
/// [`MonotoneCubic`]'s own identity treatment) or is structurally the
/// default 2-point linear identity `(0,0)->(1,1)`.
fn curve_is_identity(c: &ToneCurve) -> bool {
    c.points.len() < 2 || *c == ToneCurve::default()
}

/// A GPU storage buffer uploaded with `bytes`'s `f32` samples, the same
/// helper shape `xform.display`'s `storage_buffer` uses for its shaper/LUT
/// uploads.
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

/// Factory registering [`ToneCurveNode`].
#[derive(Default)]
pub struct ToneCurveFactory {}

impl NodeFactory for ToneCurveFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ToneCurveNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(TONE_CURVE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::ToneCurveSet;

    #[test]
    fn encode_decode_points_round_trips() {
        let points = vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.4, y: 0.6 },
            CurvePoint { x: 1.0, y: 1.0 },
        ];
        let s = encode_points(&points);
        let back = decode_points(&s);
        assert_eq!(points.len(), back.len());
        for (a, b) in points.iter().zip(back.iter()) {
            assert!((a.x - b.x).abs() < 1e-6);
            assert!((a.y - b.y).abs() < 1e-6);
        }
    }

    #[test]
    fn decode_points_never_panics_on_garbage() {
        for s in ["", "garbage", "1,2,3", ";;;", "nan,nan", "0.5", "0.5,"] {
            let pts = decode_points(s);
            for p in pts {
                assert!(p.x.is_finite() && p.y.is_finite());
            }
        }
    }

    #[test]
    fn is_identity_tracks_every_field() {
        let mut g = GlobalStages::default();
        assert!(ToneCurveNode::is_identity(&g));

        g.tone_curve.rgb = ToneCurve {
            points: vec![CurvePoint { x: 0.0, y: 0.1 }, CurvePoint { x: 1.0, y: 1.0 }],
        };
        assert!(!ToneCurveNode::is_identity(&g));
        g.tone_curve.rgb = ToneCurve::default();
        assert!(ToneCurveNode::is_identity(&g));

        g.tone_curve.parametric.shadows = 10.0;
        assert!(!ToneCurveNode::is_identity(&g));
    }

    /// Splits-only edits (no slider moved) must stay identity, zero-weight
    /// regions contribute nothing regardless of where the boundaries sit.
    #[test]
    fn moving_only_splits_stays_identity() {
        let mut g = GlobalStages::default();
        g.tone_curve.parametric.splits = [0.1, 0.4, 0.6];
        assert!(ToneCurveNode::is_identity(&g));
    }

    #[test]
    fn param_block_round_trips_through_bake() {
        let g = GlobalStages {
            tone_curve: ToneCurveSet {
                rgb: ToneCurve {
                    points: vec![
                        CurvePoint { x: 0.0, y: 0.0 },
                        CurvePoint { x: 0.5, y: 0.7 },
                        CurvePoint { x: 1.0, y: 1.0 },
                    ],
                },
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = ToneCurveNode::param_block(&g);
        let baked = bake_from_params(&pb);
        assert!((baked.composite.sample(0.5) - 0.7).abs() < 1e-3);
        // Untouched per-channel curves stay identity.
        assert!((baked.r.sample(0.3) - 0.3).abs() < 1e-3);
    }

    /// C3-adjacent sanity: an S-curve visibly increases contrast, a
    /// shadow input darkens further, a highlight input brightens further.
    #[test]
    fn s_curve_increases_contrast_on_a_gray_ramp() {
        let s_curve = ToneCurve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.25, y: 0.15 },
                CurvePoint { x: 0.75, y: 0.85 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        let spline = MonotoneCubic::build(&s_curve.points);
        assert!(spline.eval(0.25) < 0.25, "shadows should darken further");
        assert!(
            spline.eval(0.75) > 0.75,
            "highlights should brighten further"
        );
    }
}
