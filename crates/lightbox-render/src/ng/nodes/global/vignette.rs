// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `fx.vignette`, the **post-crop** vignette
//! ([`lightbox_edit::leaves::PostCropVignette`]: `amount`, `midpoint`,
//! `roundness`, `feather`, `highlights`).
//!
//! # Why "post-crop" is a pipeline fact, not a label
//!
//! The falloff is centred on the **cropped** canvas. A vignette centred on
//! the original frame would sit visibly off-centre on any cropped
//! photograph, so this node is spliced AFTER the geometry segment
//! ([`crate::ng::nodes::global::build_effects_segment`], called from
//! `ng::compile::RecipeCompiler::compile` right after
//! `nodes::geometry::build_geometry_segment`). The canvas it normalizes
//! against is `geom.crop`'s own rounded output extent, computed at compile
//! time by [`super::post_crop_extent`] and baked into the [`ParamBlock`]
//! (`canvas_w`/`canvas_h`), because `RenderNode::eval_*` is handed a tile,
//! not a canvas. The same "the extent has to ride in the params" constraint
//! `nodes::geometry`'s module docs already record for `geom.crop`.
//!
//! # The model
//!
//! 1. **Centred coordinates.** `u, v` in `[-1, 1]` across the post-crop
//!    canvas, sampled at pixel centres.
//! 2. **Roundness as an aspect morph** ([`vignette_axis_scales`]). At
//!    `roundness = 0` the iso-contours are the ellipse fitted to the frame;
//!    positive roundness morphs them toward a circle in pixel space,
//!    negative roundness exaggerates the frame's own elongation. This is
//!    Adobe's documented description of the slider ("positive values make
//!    the vignette more circular, negative values make it more oval") and
//!    nothing more: the contour family stays a pure ellipse (no superellipse
//!    exponent), so on a **square** canvas `roundness` has no geometric
//!    effect at all, because a square frame's fitted ellipse already is a
//!    circle. Approximation, honestly named; see this module's own
//!    `roundness_is_inert_on_a_square_canvas` test.
//! 3. **Midpoint and feather** ([`vignette_mid_radius`],
//!    [`vignette_feather_halfwidth`]) place and widen a smoothstep on the
//!    contour distance, giving the falloff `t` in `[0, 1]`.
//! 4. **Amount** is a linear scene-linear gain, `gain = 1 + (amount/100) *
//!    t`, floored at `0`. `amount = -100` therefore takes the far corners to
//!    black and `amount = +100` lifts them by one stop; that asymmetry is
//!    inherent to a linear gain whose darkening endpoint is `0` and whose
//!    brightening endpoint is `2`, and it matches the fact that "darken to
//!    black" is a reachable extreme while "brighten" has no natural one.
//! 5. **Highlights protection** ([`highlight_weight`]) pulls the gain back
//!    toward `1` on bright pixels, and only while the vignette is darkening.
//!    This is the control that stops a strong vignette crushing a bright sky
//!    into mud; at `highlights = 100` a fully blown pixel is left untouched.
//!    It is inert for positive `amount`, matching Lightroom, where the
//!    Highlights slider is only live for a darkening vignette.
//!
//! GPU (`shaders/global_vignette.wgsl`) and CPU ([`apply_vignette`] +
//! [`vignette_falloff`]) implement the identical formulas from the identical
//! constants, the established per-node convention, so CPU/GPU parity (§4.4)
//! holds to numerical rounding (`tests/e12_effects.rs`).
//!
//! # Node id vs module path
//!
//! The id is `fx.vignette`, spec §4.4's own name for this stage (quoted in
//! `nodes/geometry/mod.rs`'s module docs). The prefix follows the **pipeline
//! segment**, not the module path or the trait: this node is spliced by
//! [`super::build_effects_segment`], which is a segment of its own, in the
//! same way `geom.crop`/`geom.warp` live in `nodes/geometry/`. Node ids feed
//! content keys, so they are cheap to get right before anything has rendered
//! against them and expensive afterwards.
//!
//! The file nonetheless lives under `nodes/global/` and implements
//! [`GlobalNode`], because the effects leaves live in
//! [`lightbox_edit::GlobalStages`] and that is the recipe slice `GlobalNode`
//! is keyed on. If the effects stage ever grows its own `nodes/fx/` module
//! and trait, these two move there and the ids do not change, which is the
//! point of pinning them now.

use std::sync::Arc;

use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::global::tone_recovery::{working_luma, working_luma_weights};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, Roi};

/// The vignette kernel, naga-validated at build (`build.rs`).
const VIGNETTE_WGSL: &str = include_str!("../../../../shaders/global_vignette.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "amount",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "midpoint",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "roundness",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "feather",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "highlights",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "canvas_w",
        kind: ParamKind::Int,
    },
    FieldDecl {
        name: "canvas_h",
        kind: ParamKind::Int,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("fx.vignette"),
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
//    header; the CPU/GPU parity test catches drift) ─────────────────────────

/// `midpoint = 100` puts the falloff's half-strength radius here, past the
/// corner distance of the fitted ellipse (`sqrt(2)`), so the vignette is
/// reduced to a faint corner darkening. `midpoint = 0` puts it at the canvas
/// centre, so the whole frame is affected.
pub const MIDPOINT_SCALE: f32 = 1.5;
/// `feather = 100` gives the smoothstep this half-width in contour-distance
/// units, wide enough to reach from well inside the midpoint radius to well
/// outside it.
pub const FEATHER_SCALE: f32 = 0.75;
/// Floor on the smoothstep half-width. `feather = 0` is a hard-edged
/// vignette, but `edge0 == edge1` is a division by zero in both backends, so
/// the "hard" edge is this narrow instead of infinitely narrow.
pub const FEATHER_MIN: f32 = 0.002;
/// Highlight protection ramps in from this encoded-luma value.
pub const HL_LOW: f32 = 0.5;
/// Highlight protection reaches full strength at this encoded-luma value.
pub const HL_HIGH: f32 = 1.0;

/// Smoothstep (Hermite), the same helper every other global node carries
/// (`clarity::smoothstep`); duplicated rather than imported so this module's
/// formulas read against `global_vignette.wgsl`'s `smoothstep_01` line for
/// line.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The roundness aspect morph (see the module docs): the per-axis scale
/// applied to the centred `[-1, 1]` coordinates before measuring the contour
/// distance.
///
/// `roundness = 0` returns `(1, 1)`, the ellipse fitted to the frame.
/// `roundness = +100` returns `(sqrt(a), 1/sqrt(a))` for aspect `a = w/h`,
/// which makes `du² + dv²` a true circle in pixel space (of the canvas's
/// geometric-mean radius). `roundness = -100` is the reciprocal, exaggerating
/// the frame's elongation instead.
#[inline]
pub fn vignette_axis_scales(canvas: Extent, roundness: f32) -> (f32, f32) {
    let a = canvas.w.max(1) as f32 / canvas.h.max(1) as f32;
    let s = (roundness / 100.0).clamp(-1.0, 1.0);
    let k = a.powf(s * 0.5);
    (k, 1.0 / k)
}

/// The contour distance at which the falloff is at half strength.
#[inline]
pub fn vignette_mid_radius(midpoint: f32) -> f32 {
    MIDPOINT_SCALE * (midpoint / 100.0).clamp(0.0, 1.0)
}

/// The smoothstep's half-width in contour-distance units.
#[inline]
pub fn vignette_feather_halfwidth(feather: f32) -> f32 {
    (FEATHER_SCALE * (feather / 100.0).clamp(0.0, 1.0)).max(FEATHER_MIN)
}

/// Centred normalized coordinates for canvas pixel `(x, y)`: `[-1, 1]` on
/// both axes, sampled at the pixel centre so the field is symmetric about
/// the canvas centre for even and odd extents alike.
#[inline]
pub fn vignette_uv(x: i64, y: i64, canvas: Extent) -> (f32, f32) {
    let w = canvas.w.max(1) as f32;
    let h = canvas.h.max(1) as f32;
    let u = ((x as f32 + 0.5) / w) * 2.0 - 1.0;
    let v = ((y as f32 + 0.5) / h) * 2.0 - 1.0;
    (u, v)
}

/// The falloff `t` in `[0, 1]`: `0` inside the bright core, `1` in the fully
/// vignetted periphery. The CPU parity anchor for `global_vignette.wgsl`'s
/// `smoothstep_01(mid - feather, mid + feather, d)`.
#[inline]
pub fn vignette_falloff(u: f32, v: f32, sx: f32, sy: f32, mid: f32, half_width: f32) -> f32 {
    let du = u * sx;
    let dv = v * sy;
    let d = (du * du + dv * dv).sqrt();
    smoothstep(mid - half_width, mid + half_width, d)
}

/// How much a pixel at encoded luma `e` is shielded from the darkening, at
/// `highlights = 100`.
#[inline]
pub fn highlight_weight(e: f32) -> f32 {
    smoothstep(HL_LOW, HL_HIGH, e)
}

/// The scene-linear gain before highlight protection: `1 + (amount/100) * t`,
/// floored at `0`.
///
/// Exactly `1.0` at `amount == 0` for any finite `t` (`1.0 + 0.0 * t` is
/// exact in IEEE-754), which is what makes [`VignetteNode::is_identity`]'s
/// elision a bit-exact identity rather than an approximation.
#[inline]
pub fn vignette_gain(amount: f32, t: f32) -> f32 {
    (1.0 + (amount / 100.0) * t).max(0.0)
}

/// The full per-pixel op and the numeric parity anchor for
/// `global_vignette.wgsl`'s `main`: given a working RGBA pixel, its encoded
/// luma, and the falloff at that pixel, returns the vignetted RGBA (alpha
/// untouched).
#[inline]
pub fn apply_vignette(
    rgba: [f32; 4],
    luma_e: f32,
    t: f32,
    amount: f32,
    highlights: f32,
) -> [f32; 4] {
    let gain = vignette_gain(amount, t);
    // Protection is a DARKENING-only control (see the module docs): a
    // lightening vignette has no highlights to rescue.
    let protect = if gain < 1.0 {
        (highlights / 100.0).clamp(0.0, 1.0) * highlight_weight(luma_e)
    } else {
        0.0
    };
    let g = gain + (1.0 - gain) * protect;
    let out = [rgba[0] * g, rgba[1] * g, rgba[2] * g, rgba[3]];
    if !out[0].is_finite() || !out[1].is_finite() || !out[2].is_finite() {
        return rgba;
    }
    out
}

/// The resolved per-eval geometry: the canvas the falloff is normalized
/// against, and this tile's origin within it.
struct CanvasFrame {
    canvas: Extent,
    off_x: i64,
    off_y: i64,
}

/// Resolves the baked post-crop canvas, falling back to the tile's own
/// extent when it is absent.
///
/// The fallback is the `GlobalNode::param_block` path (see
/// [`VignetteNode::param_block`]): no compile-time canvas was supplied, so
/// the only defensible reading is "this tile IS the canvas", which is
/// exactly right for the untiled whole-image case and is what the node's own
/// unit tests exercise. [`super::build_effects_segment`], the production
/// splicer, always bakes the real post-crop extent.
fn canvas_frame(params: &ParamBlock, tile: Extent, out_roi: Roi) -> CanvasFrame {
    let w = params.get_i64("canvas_w").unwrap_or(0).max(0) as u32;
    let h = params.get_i64("canvas_h").unwrap_or(0).max(0) as u32;
    if w == 0 || h == 0 {
        CanvasFrame {
            canvas: tile,
            off_x: 0,
            off_y: 0,
        }
    } else {
        CanvasFrame {
            canvas: Extent { w, h },
            off_x: out_roi.x as i64,
            off_y: out_roi.y as i64,
        }
    }
}

/// The `fx.vignette` develop node.
#[derive(Default)]
pub struct VignetteNode {}

impl VignetteNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("fx.vignette");

    /// A fresh node.
    pub fn new() -> VignetteNode {
        VignetteNode::default()
    }

    /// The real production [`ParamBlock`]: the recipe's vignette leaf plus
    /// the post-crop canvas the falloff is normalized against.
    /// [`super::build_effects_segment`] is the only caller that can supply
    /// the canvas, mirroring `maybe_add_creative_lut`'s bespoke
    /// param-block path for the one other stage the generic `maybe_add`
    /// cannot serve.
    pub fn param_block_with_canvas(p: &GlobalStages, canvas: Extent) -> ParamBlock {
        let pv = &p.effects.postcrop_vignette;
        ParamBlock::from_fields([
            ("amount", ParamValue::Float(pv.amount as f64)),
            ("midpoint", ParamValue::Float(pv.midpoint as f64)),
            ("roundness", ParamValue::Float(pv.roundness as f64)),
            ("feather", ParamValue::Float(pv.feather as f64)),
            ("highlights", ParamValue::Float(pv.highlights as f64)),
            ("canvas_w", ParamValue::Int(canvas.w as i64)),
            ("canvas_h", ParamValue::Int(canvas.h as i64)),
        ])
        .expect("post-crop vignette fields are always finite (clamped on ingest, spec A2)")
    }
}

impl RenderNode for VignetteNode {
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
            .ok_or_else(|| NodeError::Other("fx.vignette: missing input tile".into()))?;
        let in_roi = input.roi;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("fx.vignette: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: in_roi.w,
            h: in_roi.h,
        });

        let amount = params.get_f64_or("amount", 0.0) as f32;
        let midpoint = params.get_f64_or("midpoint", 50.0) as f32;
        let roundness = params.get_f64_or("roundness", 0.0) as f32;
        let feather = params.get_f64_or("feather", 50.0) as f32;
        let highlights = params.get_f64_or("highlights", 0.0) as f32;
        // The GPU backend hands every input tile a zero-origin ROI
        // (`ng/exec/gpu/mod.rs:109-119`), so this is the whole-canvas
        // dispatch's own origin today; threading it anyway keeps the kernel
        // correct the day that backend starts reporting real ROIs, the same
        // shape `geom.crop`'s `in_off_x`/`in_off_y` already uses.
        let frame = canvas_frame(params, extent, in_roi);
        let (sx, sy) = vignette_axis_scales(frame.canvas, roundness);
        let weights = working_luma_weights();

        let mut ubo_bytes = [0u8; 64];
        let mut put_f32 = |slot: usize, v: f32| {
            ubo_bytes[slot * 4..slot * 4 + 4].copy_from_slice(&v.to_le_bytes());
        };
        put_f32(0, weights[0]);
        put_f32(1, weights[1]);
        put_f32(2, weights[2]);
        put_f32(3, amount);
        put_f32(4, sx);
        put_f32(5, sy);
        put_f32(6, vignette_mid_radius(midpoint));
        put_f32(7, vignette_feather_halfwidth(feather));
        put_f32(8, highlights);
        put_f32(9, frame.canvas.w.max(1) as f32);
        put_f32(10, frame.canvas.h.max(1) as f32);
        ubo_bytes[48..52].copy_from_slice(&(frame.off_x as i32).to_le_bytes());
        ubo_bytes[52..56].copy_from_slice(&(frame.off_y as i32).to_le_bytes());

        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx.vignette params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(VIGNETTE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.vignette in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.vignette out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.vignette params"),
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
            "fx.vignette",
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
            .ok_or_else(|| NodeError::Cpu("fx.vignette: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;

        let amount = params.get_f64_or("amount", 0.0) as f32;
        let midpoint = params.get_f64_or("midpoint", 50.0) as f32;
        let roundness = params.get_f64_or("roundness", 0.0) as f32;
        let feather = params.get_f64_or("feather", 50.0) as f32;
        let highlights = params.get_f64_or("highlights", 0.0) as f32;
        let weights = working_luma_weights();

        let out = ctx.output();
        let out_extent = out.extent;
        let frame = canvas_frame(params, out_extent, out_roi);
        let (sx, sy) = vignette_axis_scales(frame.canvas, roundness);
        let mid = vignette_mid_radius(midpoint);
        let half_width = vignette_feather_halfwidth(feather);

        let (w, fmt, bpp) = (
            out_extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        // The output tile's origin in the input tile's own local coordinates
        // (zero whenever `in_roi == out_roi`, i.e. every pointwise splice;
        // the clamp is the same border rule `clarity::eval_cpu` uses).
        let ox = out_roi.x as i64 - in_roi.x as i64;
        let oy = out_roi.y as i64 - in_roi.y as i64;
        let ix_max = input.extent.w as i64 - 1;
        let iy_max = input.extent.h as i64 - 1;
        out.par_fill_rows(|ly, row| {
            let iy = (oy + ly as i64).clamp(0, iy_max.max(0)) as u32;
            let cy = frame.off_y + ly as i64;
            for lx in 0..w {
                let ix = (ox + lx as i64).clamp(0, ix_max.max(0)) as u32;
                let cx = frame.off_x + lx as i64;
                let p = input.get_rgba_f32(ix, iy);
                let (u, v) = vignette_uv(cx, cy, frame.canvas);
                let t = vignette_falloff(u, v, sx, sy, mid, half_width);
                let luma_e = encoded_luma(p, weights);
                let o = apply_vignette(p, luma_e, t, amount, highlights);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

/// Companion-encoded scalar luma, the domain highlight protection is
/// measured in (same definition `clarity::encoded_luma` and
/// `global_vignette.wgsl`'s `encoded_luma_of` use).
#[inline]
fn encoded_luma(rgba: [f32; 4], weights: [f32; 3]) -> f32 {
    let l = working_luma([rgba[0], rgba[1], rgba[2]], weights);
    lightbox_color::matrix::spaces::companion_encode([l, l, l])[0]
}

impl GlobalNode for VignetteNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.effects.postcrop_vignette.amount == 0.0
    }

    /// The canvas-less block. The generic `maybe_add` path cannot supply the
    /// post-crop extent, so this bakes `canvas_w = canvas_h = 0`, which
    /// [`canvas_frame`] reads as "use the tile's own extent". Production
    /// splicing goes through [`VignetteNode::param_block_with_canvas`].
    fn param_block(p: &GlobalStages) -> ParamBlock {
        VignetteNode::param_block_with_canvas(p, Extent { w: 0, h: 0 })
    }
}

/// Factory registering [`VignetteNode`].
#[derive(Default)]
pub struct VignetteFactory {}

impl NodeFactory for VignetteFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(VignetteNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(VIGNETTE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::leaves::PostCropVignette;

    fn stages_with(pv: PostCropVignette) -> GlobalStages {
        let mut g = GlobalStages::default();
        g.effects.postcrop_vignette = pv;
        g
    }

    // ── trait wiring ──────────────────────────────────────────────────────

    #[test]
    fn is_identity_tracks_only_the_amount_field() {
        let mut g = GlobalStages::default();
        assert!(VignetteNode::is_identity(&g));
        // A non-default midpoint/feather/roundness with amount 0 renders
        // nothing, so it must NOT force the node into the graph.
        g.effects.postcrop_vignette.midpoint = 10.0;
        g.effects.postcrop_vignette.feather = 90.0;
        g.effects.postcrop_vignette.roundness = -80.0;
        assert!(VignetteNode::is_identity(&g));
        g.effects.postcrop_vignette.amount = -40.0;
        assert!(!VignetteNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_every_leaf_field_and_the_canvas() {
        let g = stages_with(PostCropVignette {
            amount: -55.0,
            midpoint: 30.0,
            roundness: 20.0,
            feather: 70.0,
            highlights: 45.0,
        });
        let pb = VignetteNode::param_block_with_canvas(&g, Extent { w: 1200, h: 800 });
        assert_eq!(pb.get_f64("amount"), Some(-55.0));
        assert_eq!(pb.get_f64("midpoint"), Some(30.0));
        assert_eq!(pb.get_f64("roundness"), Some(20.0));
        assert_eq!(pb.get_f64("feather"), Some(70.0));
        assert_eq!(pb.get_f64("highlights"), Some(45.0));
        assert_eq!(pb.get_i64("canvas_w"), Some(1200));
        assert_eq!(pb.get_i64("canvas_h"), Some(800));
    }

    #[test]
    fn canvas_less_param_block_falls_back_to_the_tile_extent() {
        let g = stages_with(PostCropVignette {
            amount: -50.0,
            ..PostCropVignette::default()
        });
        let pb = VignetteNode::param_block(&g);
        assert_eq!(pb.get_i64("canvas_w"), Some(0));
        let tile = Extent { w: 64, h: 32 };
        let frame = canvas_frame(
            &pb,
            tile,
            Roi {
                x: 7,
                y: 9,
                w: 64,
                h: 32,
            },
        );
        assert_eq!(frame.canvas, tile);
        assert_eq!((frame.off_x, frame.off_y), (0, 0));
    }

    #[test]
    fn a_baked_canvas_keeps_the_tile_origin() {
        let g = stages_with(PostCropVignette {
            amount: -50.0,
            ..PostCropVignette::default()
        });
        let pb = VignetteNode::param_block_with_canvas(&g, Extent { w: 512, h: 256 });
        let frame = canvas_frame(
            &pb,
            Extent { w: 64, h: 32 },
            Roi {
                x: 128,
                y: 64,
                w: 64,
                h: 32,
            },
        );
        assert_eq!(frame.canvas, Extent { w: 512, h: 256 });
        assert_eq!((frame.off_x, frame.off_y), (128, 64));
    }

    // ── identity ──────────────────────────────────────────────────────────

    #[test]
    fn zero_amount_is_an_exact_pixel_identity_at_any_falloff() {
        for rgba in [
            [0.3f32, 0.6, 0.9, 1.0],
            [1.9f32, 1.2, 0.4, 0.5],     // out of gamut, > 1
            [-0.01f32, 0.02, 0.03, 1.0], // slightly negative
        ] {
            for t in [0.0f32, 0.25, 0.5, 1.0] {
                for highlights in [0.0f32, 100.0] {
                    let o = apply_vignette(rgba, 0.7, t, 0.0, highlights);
                    assert_eq!(o, rgba, "rgba={rgba:?} t={t} highlights={highlights}");
                }
            }
        }
    }

    // ── geometry: the post-crop centring contract ─────────────────────────

    #[test]
    fn falloff_is_zero_at_the_canvas_centre_and_maximal_at_the_corners() {
        let canvas = Extent { w: 101, h: 101 };
        let (sx, sy) = vignette_axis_scales(canvas, 0.0);
        let mid = vignette_mid_radius(50.0);
        let hw = vignette_feather_halfwidth(50.0);

        let (cu, cv) = vignette_uv(50, 50, canvas); // the exact centre pixel
        assert!(cu.abs() < 1e-6 && cv.abs() < 1e-6, "centre uv: {cu},{cv}");
        assert_eq!(vignette_falloff(cu, cv, sx, sy, mid, hw), 0.0);

        let (ku, kv) = vignette_uv(100, 100, canvas); // bottom-right corner
        assert!(
            vignette_falloff(ku, kv, sx, sy, mid, hw) > 0.99,
            "the far corner must be fully vignetted"
        );
    }

    /// The whole point of "post-crop": the falloff is symmetric about the
    /// centre of the canvas it is handed, so a cropped canvas gets a
    /// centred vignette rather than one inherited from the original frame.
    #[test]
    fn falloff_is_symmetric_about_the_canvas_centre() {
        let canvas = Extent { w: 200, h: 120 };
        let (sx, sy) = vignette_axis_scales(canvas, 0.0);
        let mid = vignette_mid_radius(50.0);
        let hw = vignette_feather_halfwidth(50.0);
        let f = |x: i64, y: i64| {
            let (u, v) = vignette_uv(x, y, canvas);
            vignette_falloff(u, v, sx, sy, mid, hw)
        };
        for (x, y) in [(0i64, 0i64), (13, 41), (7, 119), (199, 0)] {
            let mirrored = f(199 - x, 119 - y);
            assert!(
                (f(x, y) - mirrored).abs() < 1e-6,
                "({x},{y}) vs its mirror: {} vs {mirrored}",
                f(x, y)
            );
        }
    }

    #[test]
    fn roundness_zero_is_the_frame_fitted_ellipse() {
        let (sx, sy) = vignette_axis_scales(Extent { w: 1000, h: 250 }, 0.0);
        assert!((sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6);
    }

    /// Positive roundness makes the contour a circle in PIXEL space: on a
    /// 4:1 canvas the long axis is squeezed and the short axis stretched by
    /// the same factor.
    #[test]
    fn positive_roundness_morphs_toward_a_pixel_space_circle() {
        let canvas = Extent { w: 400, h: 100 };
        let (sx, sy) = vignette_axis_scales(canvas, 100.0);
        assert!((sx - 2.0).abs() < 1e-5, "sx={sx}");
        assert!((sy - 0.5).abs() < 1e-5, "sy={sy}");
        // A point on the horizontal axis at pixel distance r from the
        // centre and one on the vertical axis at the same pixel distance
        // must land on the same contour.
        let r_px = 50.0f32;
        let u = r_px / (canvas.w as f32 / 2.0);
        let v = r_px / (canvas.h as f32 / 2.0);
        let d_h = (u * sx).abs();
        let d_v = (v * sy).abs();
        assert!((d_h - d_v).abs() < 1e-5, "d_h={d_h} d_v={d_v}");
    }

    #[test]
    fn negative_roundness_exaggerates_the_frame_elongation() {
        let canvas = Extent { w: 400, h: 100 };
        let (sx, sy) = vignette_axis_scales(canvas, -100.0);
        assert!((sx - 0.5).abs() < 1e-5, "sx={sx}");
        assert!((sy - 2.0).abs() < 1e-5, "sy={sy}");
    }

    /// Honesty check on the documented approximation: the contour family is
    /// a pure ellipse, so on a square canvas `roundness` cannot change
    /// anything, the fitted ellipse already IS a circle.
    #[test]
    fn roundness_is_inert_on_a_square_canvas() {
        let canvas = Extent { w: 256, h: 256 };
        for r in [-100.0f32, -25.0, 0.0, 25.0, 100.0] {
            let (sx, sy) = vignette_axis_scales(canvas, r);
            assert!((sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6, "r={r}");
        }
    }

    // ── midpoint / feather ────────────────────────────────────────────────

    #[test]
    fn a_higher_midpoint_shrinks_the_vignetted_area() {
        let canvas = Extent { w: 100, h: 100 };
        let (sx, sy) = vignette_axis_scales(canvas, 0.0);
        let hw = vignette_feather_halfwidth(50.0);
        let (u, v) = vignette_uv(20, 20, canvas);
        let low = vignette_falloff(u, v, sx, sy, vignette_mid_radius(20.0), hw);
        let high = vignette_falloff(u, v, sx, sy, vignette_mid_radius(80.0), hw);
        assert!(low > high, "low-midpoint={low} high-midpoint={high}");
    }

    #[test]
    fn feather_zero_still_has_a_finite_transition_width() {
        let hw = vignette_feather_halfwidth(0.0);
        assert_eq!(hw, FEATHER_MIN);
        assert!(hw > 0.0, "a zero half-width would divide by zero");
    }

    #[test]
    fn more_feather_softens_the_edge() {
        let mid = vignette_mid_radius(50.0);
        let just_inside = mid - 0.1;
        let hard = vignette_falloff(
            just_inside,
            0.0,
            1.0,
            1.0,
            mid,
            vignette_feather_halfwidth(5.0),
        );
        let soft = vignette_falloff(
            just_inside,
            0.0,
            1.0,
            1.0,
            mid,
            vignette_feather_halfwidth(90.0),
        );
        assert!(hard < soft, "hard={hard} soft={soft}");
    }

    // ── amount + highlight protection ─────────────────────────────────────

    #[test]
    fn negative_amount_darkens_and_positive_amount_brightens() {
        let rgba = [0.4f32, 0.4, 0.4, 1.0];
        let dark = apply_vignette(rgba, 0.4, 1.0, -60.0, 0.0);
        let light = apply_vignette(rgba, 0.4, 1.0, 60.0, 0.0);
        assert!(dark[0] < rgba[0], "dark={dark:?}");
        assert!(light[0] > rgba[0], "light={light:?}");
    }

    #[test]
    fn amount_minus_100_takes_the_far_corner_to_black() {
        let o = apply_vignette([0.8f32, 0.8, 0.8, 1.0], 0.4, 1.0, -100.0, 0.0);
        assert_eq!([o[0], o[1], o[2]], [0.0, 0.0, 0.0]);
        assert_eq!(o[3], 1.0);
    }

    #[test]
    fn gain_never_goes_negative_even_past_full_deflection() {
        // `amount` is clamped to -100 on ingest, but the floor is
        // structural, not a happy accident of the clamp.
        assert_eq!(vignette_gain(-400.0, 1.0), 0.0);
    }

    /// `highlights` is the control that stops a vignette crushing a bright
    /// sky: at full strength a blown pixel comes through untouched while a
    /// midtone at the same falloff is still darkened.
    #[test]
    fn highlights_protects_bright_pixels_and_leaves_midtones_alone() {
        let bright = [0.95f32, 0.95, 0.95, 1.0];
        let mid = [0.18f32, 0.18, 0.18, 1.0];
        let bright_e = 1.0f32; // fully blown in the encoded domain
        let mid_e = 0.46f32; // below HL_LOW, no protection at all

        let bright_unprotected = apply_vignette(bright, bright_e, 1.0, -80.0, 0.0);
        let bright_protected = apply_vignette(bright, bright_e, 1.0, -80.0, 100.0);
        assert!(
            bright_protected[0] > bright_unprotected[0],
            "protection must lift the highlight back up: {bright_protected:?} vs \
             {bright_unprotected:?}"
        );
        assert!(
            (bright_protected[0] - bright[0]).abs() < 1e-6,
            "highlights=100 on a blown pixel must be a no-op: {bright_protected:?}"
        );

        let mid_unprotected = apply_vignette(mid, mid_e, 1.0, -80.0, 0.0);
        let mid_protected = apply_vignette(mid, mid_e, 1.0, -80.0, 100.0);
        assert_eq!(
            mid_protected, mid_unprotected,
            "a pixel below HL_LOW gets no protection"
        );
        assert!(mid_protected[0] < mid[0], "the midtone is still darkened");
    }

    /// Lightroom's Highlights slider is only live for a darkening vignette;
    /// so is this one.
    #[test]
    fn highlights_is_inert_for_a_lightening_vignette() {
        let bright = [0.9f32, 0.9, 0.9, 1.0];
        let a = apply_vignette(bright, 1.0, 1.0, 70.0, 0.0);
        let b = apply_vignette(bright, 1.0, 1.0, 70.0, 100.0);
        assert_eq!(a, b);
    }

    #[test]
    fn alpha_is_never_touched() {
        let rgba = [0.5f32, 0.5, 0.5, 0.25];
        let o = apply_vignette(rgba, 0.5, 1.0, -100.0, 0.0);
        assert_eq!(o[3], 0.25);
    }
}
