// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `geom.warp`, E11 Phase-E tasks **E1**/**E2**/**E3**: the single-pass
//! inverse-mapped Lanczos-3 resample over the composed [`WarpField`]
//! (reduced-scope: straighten `angle` only, see that type's module docs).
//!
//! Domain: scene-linear working RGBA (post tone/color, pre-crop, per §4.1/§5
//! placement). `eval_cpu`/`eval_gpu` both evaluate
//! [`WarpField::map_dst_to_src`] and the identical Lanczos-3 + anti-ringing
//! kernel, so CPU/GPU parity (§4.4) holds within the standard tolerance.
//! Elided (zero nodes added) whenever `angle == 0` (task E1 AC).

use std::sync::Arc;

use lightbox_edit::Geometry;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::geometry::GeometryNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, Roi};
use crate::ng::warp::{Vec2, WarpField};

/// The warp resample kernel, naga-validated at build (`build.rs`).
const WARP_WGSL: &str = include_str!("../../../../shaders/geom_warp.wgsl");

/// The Lanczos-3 window radius (a = 3), duplicated from
/// `nodes::resize`'s own constant (private there) so this module has no
/// cross-module coupling on an internal resampling detail; both must move
/// together if the algorithm ever changes (CPU/GPU parity tests + the E1
/// reference-Lanczos test catch drift).
const LANCZOS_A: f64 = 3.0;

/// The normalized-sinc Lanczos-3 kernel weight at offset `x` (identical
/// formula to `nodes::resize::lanczos3`).
#[inline]
fn lanczos3(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else if x.abs() < LANCZOS_A {
        let px = std::f64::consts::PI * x;
        LANCZOS_A * px.sin() * (px / LANCZOS_A).sin() / (px * px)
    } else {
        0.0
    }
}

/// Point-samples `pixels` at the continuous local coordinate `(fx, fy)` with
/// a 6×6-tap (support radius 3) Lanczos-3 gather, edge-clamped, then
/// anti-ringing clamped to the local 2×2 neighborhood's min/max per channel
/// (spec §6.2 "anti-ringing (clamp to local min/max of 2×2 neighborhood
/// blend)"), the CPU reference `geom_warp.wgsl`'s kernel mirrors exactly.
pub fn sample_lanczos3(pixels: &PixelBuf, fx: f64, fy: f64) -> [f32; 4] {
    let w = pixels.extent.w;
    let h = pixels.extent.h;
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let x0 = fx.floor() as i64 - 2;
    let y0 = fy.floor() as i64 - 2;
    let mut acc = [0f64; 4];
    let mut wsum = 0f64;
    let mut lo = [f32::INFINITY; 4];
    let mut hi = [f32::NEG_INFINITY; 4];
    for j in 0..6i64 {
        let sy = (y0 + j).clamp(0, h as i64 - 1) as u32;
        let wy = lanczos3(fy - (y0 + j) as f64);
        for i in 0..6i64 {
            let sx = (x0 + i).clamp(0, w as i64 - 1) as u32;
            let wx = lanczos3(fx - (x0 + i) as f64);
            let wgt = wx * wy;
            let p = pixels.get_rgba_f32(sx, sy);
            for c in 0..4 {
                acc[c] += wgt * p[c] as f64;
            }
            wsum += wgt;
            // The immediate 2×2 neighborhood (the two taps nearest the
            // sample point on each axis) bounds the anti-ringing clamp.
            if (2..=3).contains(&i) && (2..=3).contains(&j) {
                for c in 0..4 {
                    lo[c] = lo[c].min(p[c]);
                    hi[c] = hi[c].max(p[c]);
                }
            }
        }
    }
    let inv = if wsum.abs() > 1e-12 { 1.0 / wsum } else { 0.0 };
    let mut out = [0f32; 4];
    for c in 0..4 {
        let raw = (acc[c] * inv) as f32;
        out[c] = raw.clamp(lo[c].min(hi[c]), hi[c].max(lo[c]));
    }
    out
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "angle_deg",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "src_w",
        kind: ParamKind::Int,
    },
    FieldDecl {
        name: "src_h",
        kind: ParamKind::Int,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("geom.warp"),
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

/// Reconstructs the [`WarpField`] both `plan`/`eval_cpu`/`eval_gpu` share,
/// from the params baked at compile time (§the module's `param_block`
/// `plan` has no `inputs: &[Extent]` parameter, so the source extent must
/// ride in `params`, not be recomputed from an upstream tile).
fn warp_field_from_params(params: &ParamBlock) -> WarpField {
    let angle_deg = params.get_f64_or("angle_deg", 0.0);
    let src_w = params.get_i64("src_w").unwrap_or(1).max(1) as u32;
    let src_h = params.get_i64("src_h").unwrap_or(1).max(1) as u32;
    WarpField::from_angle_deg(angle_deg, Extent { w: src_w, h: src_h })
}

/// The `geom.warp` develop node (E11 tasks E1/E2/E3).
#[derive(Default)]
pub struct GeomWarpNode {}

impl GeomWarpNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("geom.warp");

    /// A fresh node.
    pub fn new() -> GeomWarpNode {
        GeomWarpNode::default()
    }
}

impl RenderNode for GeomWarpNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    /// ROI back-propagation (task **E3**): back-maps the requested output ROI
    /// through the warp's inverse (`WarpField::map_roi_dst_to_src`), grown by
    /// the Lanczos-3 support radius, the seam that lets 1:1 zoom into a
    /// corner of a rotated image fetch only the source tiles it actually
    /// needs (proven against the reference tiling harness in
    /// `tests/e11_geom.rs`, mirroring `ToneRecoveryNode::plan`'s own B13
    /// pattern since the production v1 executor doesn't yet route through
    /// real ROI-tiled CPU eval either).
    fn plan(&self, out: Roi, _scale: f32, params: &ParamBlock) -> InputRois {
        let warp = warp_field_from_params(params);
        if warp.is_identity() {
            return InputRois::identity(out, 1);
        }
        InputRois(vec![warp.map_roi_dst_to_src(out, LANCZOS_A)])
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
            .ok_or_else(|| NodeError::Other("geom.warp: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("geom.warp: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let warp = warp_field_from_params(params);
        let u = warp.to_gpu_uniform();
        let mut ubo_bytes = [0u8; 32];
        ubo_bytes[0..4].copy_from_slice(&u.sin_a.to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&u.cos_a.to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&u.center_x.to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&u.center_y.to_le_bytes());
        ubo_bytes[16..20].copy_from_slice(&(input.roi.w).to_le_bytes());
        ubo_bytes[20..24].copy_from_slice(&(input.roi.h).to_le_bytes());
        // The absolute-to-local offset between the output tile's frame and
        // the (possibly apron-grown) input tile's frame.
        ubo_bytes[24..28].copy_from_slice(&(input.roi.x).to_le_bytes());
        ubo_bytes[28..32].copy_from_slice(&(input.roi.y).to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom.warp params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(WARP_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.warp in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.warp out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.warp params"),
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
            "geom.warp",
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
            .ok_or_else(|| NodeError::Cpu("geom.warp: missing input tile".into()))?;
        let warp = warp_field_from_params(params);
        let out_roi = ctx.out_roi;

        // Identity fast path (task E1 AC: "identity warp is bit-exact
        // passthrough"): a straight per-pixel copy, no Lanczos math at all.
        if warp.is_identity() {
            let input = input_view.pixels;
            let ix0 = out_roi.x - input_view.roi.x;
            let iy0 = out_roi.y - input_view.roi.y;
            let out = ctx.output();
            let (w, fmt, bpp) = (
                out.extent.w,
                out.format,
                out.format.bytes_per_pixel() as usize,
            );
            out.par_fill_rows(|ly, row| {
                let iy = (iy0 + ly as i32).clamp(0, input.extent.h as i32 - 1) as u32;
                for lx in 0..w {
                    let ix = (ix0 + lx as i32).clamp(0, input.extent.w as i32 - 1) as u32;
                    let p = input.get_rgba_f32(ix, iy);
                    PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], p);
                }
            });
            return Ok(());
        }

        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|ly, row| {
            let ay = out_roi.y + ly as i32;
            for lx in 0..w {
                let ax = out_roi.x + lx as i32;
                let src = warp.map_dst_to_src(Vec2::new(ax as f64, ay as f64));
                let fx = src.x - in_roi.x as f64;
                let fy = src.y - in_roi.y as f64;
                let p = sample_lanczos3(input, fx, fy);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], p);
            }
        });
        Ok(())
    }
}

impl GeometryNode for GeomWarpNode {
    fn is_identity(p: &Geometry) -> bool {
        p.angle == 0.0
    }

    fn param_block(p: &Geometry, src_extent: Extent) -> ParamBlock {
        ParamBlock::from_fields([
            ("angle_deg", ParamValue::Float(p.angle as f64)),
            ("src_w", ParamValue::Int(src_extent.w as i64)),
            ("src_h", ParamValue::Int(src_extent.h as i64)),
        ])
        .expect("geom.warp params are always finite (angle clamped on ingest)")
    }
}

/// Factory registering [`GeomWarpNode`].
#[derive(Default)]
pub struct GeomWarpFactory {}

impl NodeFactory for GeomWarpFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GeomWarpNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(WARP_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::tile::PixelFormat;

    fn fill(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> PixelBuf {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
        for y in 0..h {
            for x in 0..w {
                px.set_rgba_f32(x, y, f(x, y));
            }
        }
        px
    }

    #[test]
    fn is_identity_tracks_the_angle_field() {
        let mut g = Geometry::default();
        assert!(GeomWarpNode::is_identity(&g));
        g.angle = 5.0;
        assert!(!GeomWarpNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_angle_and_extent() {
        let g = Geometry {
            angle: 12.5,
            ..Geometry::default()
        };
        let pb = GeomWarpNode::param_block(&g, Extent { w: 640, h: 480 });
        assert_eq!(pb.get_f64("angle_deg"), Some(12.5));
        assert_eq!(pb.get_i64("src_w"), Some(640));
        assert_eq!(pb.get_i64("src_h"), Some(480));
    }

    /// **Task E1 AC**: identity warp is bit-exact passthrough.
    #[test]
    fn identity_sample_returns_exact_source_pixel() {
        let src = fill(16, 16, |x, y| [x as f32 / 16.0, y as f32 / 16.0, 0.25, 1.0]);
        for y in 0..16u32 {
            for x in 0..16u32 {
                let got = sample_lanczos3(&src, x as f64, y as f64);
                let want = src.get_rgba_f32(x, y);
                for c in 0..4 {
                    assert!(
                        (got[c] - want[c]).abs() < 1e-5,
                        "({x},{y}) ch{c}: got {got:?} want {want:?}"
                    );
                }
            }
        }
    }

    /// **Task E1 AC**: resample vs an independently-structured reference
    /// Lanczos-3 (a plain double loop over the full support with the same
    /// per-tap formula, but no early data-dependent branching) agree within
    /// 1e-4.
    #[test]
    fn resample_matches_a_reference_lanczos_within_tolerance() {
        fn reference(pixels: &PixelBuf, fx: f64, fy: f64) -> [f32; 4] {
            let w = pixels.extent.w as i64;
            let h = pixels.extent.h as i64;
            let mut acc = [0f64; 4];
            let mut wsum = 0f64;
            for sy in (fy.floor() as i64 - 3)..=(fy.floor() as i64 + 3) {
                let cy = sy.clamp(0, h - 1);
                let wy = lanczos3(fy - sy as f64);
                for sx in (fx.floor() as i64 - 3)..=(fx.floor() as i64 + 3) {
                    let cx = sx.clamp(0, w - 1);
                    let wx = lanczos3(fx - sx as f64);
                    let wgt = wx * wy;
                    let p = pixels.get_rgba_f32(cx as u32, cy as u32);
                    for c in 0..4 {
                        acc[c] += wgt * p[c] as f64;
                    }
                    wsum += wgt;
                }
            }
            let inv = if wsum.abs() > 1e-12 { 1.0 / wsum } else { 0.0 };
            [
                (acc[0] * inv) as f32,
                (acc[1] * inv) as f32,
                (acc[2] * inv) as f32,
                (acc[3] * inv) as f32,
            ]
        }

        // A smooth (locally near-linear) gradient sampled well clear of the
        // border (> the Lanczos-3 support radius away from every edge):
        // Lanczos-3 essentially never overshoots the local min/max on such
        // interior data, so the anti-ringing clamp (present in
        // `sample_lanczos3`, absent from `reference`) never engages there,
        // and the two independently-structured implementations must agree
        // tightly (the E1 AC's literal 1e-4). Border-adjacent points are
        // deliberately excluded: edge-clamped taps there create a genuine
        // slope discontinuity the anti-ringing clamp is *designed* to flatten
        // a real, intentional divergence from the unclamped reference, not
        // a bug (see `identity_sample_returns_exact_source_pixel` /
        // `tests/e11_geom.rs` for the border/clamp behavior itself).
        let src = fill(30, 30, |x, y| {
            [x as f32 / 30.0, y as f32 / 30.0, (x + y) as f32 / 60.0, 1.0]
        });
        for &(fx, fy) in &[
            (10.3, 10.3),
            (15.75, 8.2),
            (9.4, 9.4),
            (20.1, 19.9),
            (14.0, 14.0),
        ] {
            let mine = sample_lanczos3(&src, fx, fy);
            let ref_ = reference(&src, fx, fy);
            for c in 0..4 {
                assert!(
                    (mine[c] - ref_[c]).abs() < 1e-4,
                    "({fx},{fy}) ch{c}: mine={:?} ref={:?}",
                    mine,
                    ref_
                );
            }
        }
    }
}
