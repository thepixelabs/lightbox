// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.whites_blacks` (E10 task **A8**). Companion-domain endpoint remap
//! (spec §4.2 "Whites/blacks | companion-encoded endpoint remap").
//!
//! # The remap
//!
//! Each of R/G/B is independently encoded via
//! [`lightbox_color::matrix::spaces::companion_encode`], linearly remapped
//! from the input levels `[bp, wp]` to `[0, 1]` (clamped), then decoded back
//! via [`lightbox_color::matrix::spaces::companion_decode`]. `bp`/`wp` are
//! derived from the `blacks`/`whites` sliders ([`wb_bp_wp`]):
//! `blacks > 0` lifts the shadow floor (fog/lifted-blacks look), `blacks < 0`
//! crushes it; `whites > 0` extends the highlight clip point (brighter,
//! clips more), `whites < 0` pulls highlights down (recovers headroom).
//! `blacks = whites = 0 ⇒ (bp, wp) = (0, 1)`, the exact identity. The remap
//! is a **clamped-linear** function of the encoded value, so it is monotone
//! non-decreasing for any `(bp, wp)` with `wp > bp`, no tone reversal is
//! possible by construction (the A8 property test). WGSL
//! (`shaders/global_whites_blacks.wgsl`) and CPU ([`endpoint_remap`])
//! evaluate the identical formula from the identical `(bp, wp)` pair, so
//! CPU/GPU parity (§4.4) holds to float rounding.

use std::sync::Arc;

use lightbox_color::matrix::spaces::{companion_decode, companion_encode};
use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The endpoint-remap kernel, naga-validated at build (`build.rs`).
const WHITES_BLACKS_WGSL: &str = include_str!("../../../../shaders/global_whites_blacks.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "whites",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "blacks",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.whites_blacks"),
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

/// The black/white input-level deflection range (PV1's own choice; §4.2
/// leaves the exact mapping constant to the implementer). `blacks`/`whites`
/// at ±100 move the corresponding endpoint by this much of the encoded
/// `[0,1]` domain.
pub const ENDPOINT_RANGE: f32 = 0.25;

/// Derives the black/white input levels `(bp, wp)` from the `whites`/`blacks`
/// sliders (spec §4.2), the single source of truth both `eval_gpu`'s UBO
/// write and `eval_cpu` read, so the two backends can never drift.
/// `(blacks, whites) = (0, 0) ⇒ (bp, wp) = (0, 1)`, the exact identity.
#[inline]
pub fn wb_bp_wp(whites: f32, blacks: f32) -> (f32, f32) {
    let bp = -(blacks / 100.0) * ENDPOINT_RANGE;
    let wp = 1.0 - (whites / 100.0) * ENDPOINT_RANGE;
    (bp, wp)
}

/// The clamped-linear endpoint remap (spec §4.2), evaluated in the
/// companion-encoded domain. Monotone non-decreasing in `e` for any `wp > bp`
/// no tone reversal is possible by construction.
#[inline]
pub fn endpoint_remap(e: f32, bp: f32, wp: f32) -> f32 {
    let denom = (wp - bp).max(1e-6);
    ((e - bp) / denom).clamp(0.0, 1.0)
}

/// Applies the full companion-domain whites/blacks op to one working-space
/// RGBA pixel (alpha untouched), the CPU parity anchor for
/// `global_whites_blacks.wgsl`.
#[inline]
pub fn apply_whites_blacks(rgba: [f32; 4], bp: f32, wp: f32) -> [f32; 4] {
    let enc = companion_encode([rgba[0], rgba[1], rgba[2]]);
    let remapped = [
        endpoint_remap(enc[0], bp, wp),
        endpoint_remap(enc[1], bp, wp),
        endpoint_remap(enc[2], bp, wp),
    ];
    let dec = companion_decode(remapped);
    [dec[0], dec[1], dec[2], rgba[3]]
}

/// The `global.whites_blacks` develop node.
#[derive(Default)]
pub struct WhitesBlacksNode {}

impl WhitesBlacksNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.whites_blacks");

    /// A fresh node.
    pub fn new() -> WhitesBlacksNode {
        WhitesBlacksNode::default()
    }
}

impl RenderNode for WhitesBlacksNode {
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
            .ok_or_else(|| NodeError::Other("global.whites_blacks: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.whites_blacks: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let whites = params.get_f64_or("whites", 0.0) as f32;
        let blacks = params.get_f64_or("blacks", 0.0) as f32;
        let (bp, wp) = wb_bp_wp(whites, blacks);
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&bp.to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&wp.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.whites_blacks params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(WHITES_BLACKS_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.whites_blacks in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.whites_blacks out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.whites_blacks params"),
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
            "global.whites_blacks",
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
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.whites_blacks: missing input tile".into()))?
            .pixels;
        let whites = params.get_f64_or("whites", 0.0) as f32;
        let blacks = params.get_f64_or("blacks", 0.0) as f32;
        let (bp, wp) = wb_bp_wp(whites, blacks);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_whites_blacks(p, bp, wp);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for WhitesBlacksNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.whites == 0.0 && p.blacks == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([
            ("whites", ParamValue::Float(p.whites as f64)),
            ("blacks", ParamValue::Float(p.blacks as f64)),
        ])
        .expect("whites/blacks are always finite (clamped on ingest — spec A2)")
    }
}

/// Factory registering [`WhitesBlacksNode`].
#[derive(Default)]
pub struct WhitesBlacksFactory {}

impl NodeFactory for WhitesBlacksFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(WhitesBlacksNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(WHITES_BLACKS_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_whites_zero_blacks_is_exact_identity() {
        let (bp, wp) = wb_bp_wp(0.0, 0.0);
        assert_eq!((bp, wp), (0.0, 1.0));
        for e in [0.0f32, 0.2, 0.5, 0.8, 1.0] {
            assert!((endpoint_remap(e, bp, wp) - e).abs() < 1e-6, "e={e}");
        }
    }

    #[test]
    fn positive_blacks_lifts_the_shadow_floor() {
        let (bp, _) = wb_bp_wp(0.0, 100.0);
        assert!(bp < 0.0, "positive blacks must lower bp: {bp}");
        let (bp_neg, _) = wb_bp_wp(0.0, -100.0);
        assert!(bp_neg > 0.0, "negative blacks must raise bp: {bp_neg}");
    }

    #[test]
    fn positive_whites_extends_the_highlight_clip() {
        let (_, wp) = wb_bp_wp(100.0, 0.0);
        assert!(wp < 1.0, "positive whites must lower wp: {wp}");
        let (_, wp_neg) = wb_bp_wp(-100.0, 0.0);
        assert!(wp_neg > 1.0, "negative whites must raise wp: {wp_neg}");
    }

    /// A8 acceptance: monotonicity property test, no tone reversal across a
    /// dense sweep, at every corner of the ±100/±100 slider space.
    #[test]
    fn remap_is_monotone_across_the_full_range() {
        for whites in [-100.0f32, 0.0, 100.0] {
            for blacks in [-100.0f32, 0.0, 100.0] {
                let (bp, wp) = wb_bp_wp(whites, blacks);
                let mut prev = -1.0f32;
                for i in 0..=200 {
                    let e = i as f32 / 200.0;
                    let v = endpoint_remap(e, bp, wp);
                    assert!(
                        v >= prev - 1e-6,
                        "whites={whites} blacks={blacks} non-monotone at e={e}"
                    );
                    prev = v;
                }
            }
        }
    }

    #[test]
    fn is_identity_tracks_both_fields() {
        let mut g = GlobalStages::default();
        assert!(WhitesBlacksNode::is_identity(&g));
        g.whites = 5.0;
        assert!(!WhitesBlacksNode::is_identity(&g));
        g.whites = 0.0;
        g.blacks = -5.0;
        assert!(!WhitesBlacksNode::is_identity(&g));
    }
}
