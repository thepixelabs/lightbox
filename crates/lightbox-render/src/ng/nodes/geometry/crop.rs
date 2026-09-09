// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `geom.crop`, E11 Phase-E task **E4**: folds the normalized crop rect +
//! orientation (`Flip`) into a **pure canvas/extent change**, no resample.
//! Straighten `angle` is `geom.warp`'s job (spec §5 pipeline order:
//! `geom.warp` then `geom.crop`); this node only selects an axis-aligned
//! pixel window of its input (rounding the normalized rect to whole pixels)
//! and, for `Flip`, reverses row/column order, both are exact index
//! operations, never interpolation, matching the "orientation change is
//! metadata only" fast-path AC (mirror rather than a 90° rotation, see
//! `docs/plan/epics/E11-deviations.md` for why this slice's `Flip` has no
//! 90°-step variant to fast-path).

use std::sync::Arc;

use lightbox_edit::leaves::Flip;
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

/// The crop/flip copy kernel, naga-validated at build (`build.rs`).
const CROP_WGSL: &str = include_str!("../../../../shaders/geom_crop.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "left",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "top",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "right",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "bottom",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "flip_h",
        kind: ParamKind::Bool,
    },
    FieldDecl {
        name: "flip_v",
        kind: ParamKind::Bool,
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
    id: NodeId("geom.crop"),
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

/// The crop rect resolved to whole pixels + orientation flags, the single
/// source of truth `output_extent`/`plan`/`eval_cpu`/`eval_gpu` all derive
/// from (so they can never disagree on where the crop window sits).
struct CropPixels {
    left_px: i64,
    top_px: i64,
    out_w: u32,
    out_h: u32,
    flip_h: bool,
    flip_v: bool,
}

fn crop_pixels_from_params(params: &ParamBlock) -> CropPixels {
    let left = params.get_f64_or("left", 0.0);
    let top = params.get_f64_or("top", 0.0);
    let right = params.get_f64_or("right", 1.0);
    let bottom = params.get_f64_or("bottom", 1.0);
    let src_w = params.get_i64("src_w").unwrap_or(1).max(1);
    let src_h = params.get_i64("src_h").unwrap_or(1).max(1);
    let flip_h = params.get_bool("flip_h").unwrap_or(false);
    let flip_v = params.get_bool("flip_v").unwrap_or(false);

    let round_px =
        |v: f64, dim: i64| -> i64 { (v * dim as f64).round().clamp(0.0, dim as f64) as i64 };
    let mut l = round_px(left, src_w);
    let mut t = round_px(top, src_h);
    let mut r = round_px(right, src_w);
    let mut b = round_px(bottom, src_h);
    // Degenerate rounding (e.g. a crop narrower than half a pixel) folds to
    // a minimal 1px window rather than an empty one.
    if r <= l {
        r = (l + 1).min(src_w);
        l = (r - 1).max(0);
    }
    if b <= t {
        b = (t + 1).min(src_h);
        t = (b - 1).max(0);
    }
    CropPixels {
        left_px: l,
        top_px: t,
        out_w: (r - l).max(1) as u32,
        out_h: (b - t).max(1) as u32,
        flip_h,
        flip_v,
    }
}

/// The `geom.crop` develop node (E11 task E4).
#[derive(Default)]
pub struct GeomCropNode {}

impl GeomCropNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("geom.crop");

    /// A fresh node.
    pub fn new() -> GeomCropNode {
        GeomCropNode::default()
    }
}

impl RenderNode for GeomCropNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    /// The composed crop+angle's output extent (task E4 AC), the crop
    /// window's pixel size. `angle`'s contribution already happened
    /// upstream in `geom.warp` (which never changes extent in this slice);
    /// this is the only node that changes canvas extent.
    fn output_extent(&self, inputs: &[Extent], params: &ParamBlock, request: Extent) -> Extent {
        let _ = (inputs, request);
        let cp = crop_pixels_from_params(params);
        Extent {
            w: cp.out_w,
            h: cp.out_h,
        }
    }

    /// ROI back-propagation: maps the requested crop-canvas output ROI to
    /// the pre-crop input ROI, a pure translation (+ mirrored translation
    /// under `Flip`), never a resample.
    fn plan(&self, out: Roi, _scale: f32, params: &ParamBlock) -> InputRois {
        let cp = crop_pixels_from_params(params);
        let (ix0, iw) = if cp.flip_h {
            let lx0 = out.x as i64;
            let w = out.w as i64;
            (cp.left_px + (cp.out_w as i64 - (lx0 + w)), out.w)
        } else {
            (cp.left_px + out.x as i64, out.w)
        };
        let (iy0, ih) = if cp.flip_v {
            let ly0 = out.y as i64;
            let h = out.h as i64;
            (cp.top_px + (cp.out_h as i64 - (ly0 + h)), out.h)
        } else {
            (cp.top_px + out.y as i64, out.h)
        };
        InputRois(vec![Roi {
            x: ix0 as i32,
            y: iy0 as i32,
            w: iw,
            h: ih,
        }])
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
            .ok_or_else(|| NodeError::Other("geom.crop: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("geom.crop: output is not a GPU tile".into()))?
            .clone();
        let cp = crop_pixels_from_params(params);

        let mut ubo_bytes = [0u8; 32];
        ubo_bytes[0..4].copy_from_slice(&(cp.left_px as i32).to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&(cp.top_px as i32).to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&cp.out_w.to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&cp.out_h.to_le_bytes());
        ubo_bytes[16..20].copy_from_slice(&(cp.flip_h as u32).to_le_bytes());
        ubo_bytes[20..24].copy_from_slice(&(cp.flip_v as u32).to_le_bytes());
        ubo_bytes[24..28].copy_from_slice(&(input.roi.x).to_le_bytes());
        ubo_bytes[28..32].copy_from_slice(&(input.roi.y).to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom.crop params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(CROP_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.crop in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.crop out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.crop params"),
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
            Extent {
                w: cp.out_w,
                h: cp.out_h,
            },
            "geom.crop",
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
            .ok_or_else(|| NodeError::Cpu("geom.crop: missing input tile".into()))?;
        let cp = crop_pixels_from_params(params);
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let in_w_max = input.extent.w as i64 - 1;
        let in_h_max = input.extent.h as i64 - 1;

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|ly, row| {
            let oy = out_roi.y + ly as i32;
            let uy = if cp.flip_v {
                cp.top_px + (cp.out_h as i64 - 1 - oy as i64)
            } else {
                cp.top_px + oy as i64
            };
            let iy = (uy - in_roi.y as i64).clamp(0, in_h_max.max(0)) as u32;
            for lx in 0..w {
                let ox = out_roi.x + lx as i32;
                let ux = if cp.flip_h {
                    cp.left_px + (cp.out_w as i64 - 1 - ox as i64)
                } else {
                    cp.left_px + ox as i64
                };
                let ix = (ux - in_roi.x as i64).clamp(0, in_w_max.max(0)) as u32;
                let p = input.get_rgba_f32(ix, iy);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], p);
            }
        });
        Ok(())
    }
}

impl GeometryNode for GeomCropNode {
    fn is_identity(p: &Geometry) -> bool {
        p.crop == lightbox_edit::leaves::Crop::default() && p.flip == Flip::None
    }

    fn param_block(p: &Geometry, src_extent: Extent) -> ParamBlock {
        let (flip_h, flip_v) = match p.flip {
            Flip::Horizontal => (true, false),
            Flip::Vertical => (false, true),
            Flip::Both => (true, true),
            Flip::None => (false, false),
            _ => (false, false),
        };
        ParamBlock::from_fields([
            ("left", ParamValue::Float(p.crop.left as f64)),
            ("top", ParamValue::Float(p.crop.top as f64)),
            ("right", ParamValue::Float(p.crop.right as f64)),
            ("bottom", ParamValue::Float(p.crop.bottom as f64)),
            ("flip_h", ParamValue::Bool(flip_h)),
            ("flip_v", ParamValue::Bool(flip_v)),
            ("src_w", ParamValue::Int(src_extent.w as i64)),
            ("src_h", ParamValue::Int(src_extent.h as i64)),
        ])
        .expect("geom.crop params are always finite (crop/flip clamped on ingest)")
    }
}

/// Factory registering [`GeomCropNode`].
#[derive(Default)]
pub struct GeomCropFactory {}

impl NodeFactory for GeomCropFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GeomCropNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(CROP_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::leaves::Crop;

    #[test]
    fn is_identity_tracks_crop_and_flip() {
        let mut g = Geometry::default();
        assert!(GeomCropNode::is_identity(&g));
        g.crop = Crop {
            left: 0.1,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
        assert!(!GeomCropNode::is_identity(&g));
        g.crop = Crop::default();
        g.flip = Flip::Horizontal;
        assert!(!GeomCropNode::is_identity(&g));
    }

    #[test]
    fn param_block_round_trips_crop_and_flip() {
        let g = Geometry {
            crop: Crop {
                left: 0.25,
                top: 0.1,
                right: 0.9,
                bottom: 0.8,
            },
            flip: Flip::Both,
            ..Geometry::default()
        };
        let pb = GeomCropNode::param_block(&g, Extent { w: 1000, h: 500 });
        assert_eq!(pb.get_f64("left"), Some(0.25));
        // `Crop`'s fields are `f32`; comparing through the same `as f64`
        // widening the node itself uses avoids a spurious precision mismatch
        // (0.8f32 as f64 != 0.8f64 bit-for-bit).
        assert_eq!(pb.get_f64("bottom"), Some(0.8f32 as f64));
        assert_eq!(pb.get_bool("flip_h"), Some(true));
        assert_eq!(pb.get_bool("flip_v"), Some(true));
        assert_eq!(pb.get_i64("src_w"), Some(1000));
    }

    #[test]
    fn output_extent_matches_the_rounded_crop_rect() {
        let node = GeomCropNode::new();
        let g = Geometry {
            crop: Crop {
                left: 0.25,
                top: 0.0,
                right: 0.75,
                bottom: 1.0,
            },
            ..Geometry::default()
        };
        let pb = GeomCropNode::param_block(&g, Extent { w: 400, h: 200 });
        // The crop rect wins over both the input extent and the request's
        // this is the one node whose output extent is neither.
        let ext = node.output_extent(&[Extent { w: 400, h: 200 }], &pb, Extent { w: 64, h: 64 });
        assert_eq!(ext, Extent { w: 200, h: 200 });
    }

    #[test]
    fn plan_offsets_by_the_crop_origin_without_flip() {
        let node = GeomCropNode::new();
        let g = Geometry {
            crop: Crop {
                left: 0.25,
                top: 0.1,
                right: 0.75,
                bottom: 0.9,
            },
            ..Geometry::default()
        };
        let pb = GeomCropNode::param_block(&g, Extent { w: 400, h: 200 });
        // left_px = 100, top_px = 20.
        let out = Roi {
            x: 5,
            y: 5,
            w: 10,
            h: 10,
        };
        let rois = node.plan(out, 1.0, &pb);
        assert_eq!(
            rois.0[0],
            Roi {
                x: 105,
                y: 25,
                w: 10,
                h: 10
            }
        );
    }

    #[test]
    fn plan_mirrors_under_horizontal_flip() {
        let node = GeomCropNode::new();
        let g = Geometry {
            crop: Crop::default(), // left_px=0, out_w=400
            flip: Flip::Horizontal,
            ..Geometry::default()
        };
        let pb = GeomCropNode::param_block(&g, Extent { w: 400, h: 200 });
        // Requesting the leftmost 10 output columns must fetch the
        // rightmost 10 input columns.
        let out = Roi {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let rois = node.plan(out, 1.0, &pb);
        assert_eq!(rois.0[0].x, 390);
        assert_eq!(rois.0[0].w, 10);
    }
}
