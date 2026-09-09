// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.exposure` (E10 task **A6**, the epic's first-failing-test node).
//!
//! Domain: scene-linear working RGB (spec §4.2 "Exposure | scene-linear
//! working, gain = 2^EV | photographic stops contract"). `gain = 2^ev` is
//! applied to R/G/B; alpha passes through unchanged. WGSL
//! (`shaders/global_exposure.wgsl`) and CPU ([`exposure_gain`]) compute the
//! identical formula, so CPU/GPU parity (§4.4) holds to float rounding.

use std::sync::Arc;

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

/// The exposure gain kernel, naga-validated at build (`build.rs`).
const EXPOSURE_WGSL: &str = include_str!("../../../../shaders/global_exposure.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "ev",
    kind: ParamKind::Float,
}]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.exposure"),
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

/// `gain = 2^ev` (spec §4.2), the single formula both `eval_gpu`'s UBO write
/// and `eval_cpu` evaluate, so the two backends can never drift on the math.
#[inline]
pub fn exposure_gain(ev: f32) -> f32 {
    ev.exp2()
}

/// The `global.exposure` develop node.
#[derive(Default)]
pub struct ExposureNode {}

impl ExposureNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.exposure");

    /// A fresh node.
    pub fn new() -> ExposureNode {
        ExposureNode::default()
    }
}

impl RenderNode for ExposureNode {
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
            .ok_or_else(|| NodeError::Other("global.exposure: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.exposure: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let ev = params.get_f64_or("ev", 0.0) as f32;
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&ev.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.exposure params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(EXPOSURE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.exposure in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.exposure out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.exposure params"),
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
            "global.exposure",
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
            .ok_or_else(|| NodeError::Cpu("global.exposure: missing input tile".into()))?
            .pixels;
        let ev = params.get_f64_or("ev", 0.0) as f32;
        let gain = exposure_gain(ev);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                PixelBuf::encode_pixel(
                    fmt,
                    &mut row[x as usize * bpp..],
                    [p[0] * gain, p[1] * gain, p[2] * gain, p[3]],
                );
            }
        });
        Ok(())
    }
}

impl GlobalNode for ExposureNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.exposure == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([("ev", ParamValue::Float(p.exposure as f64))])
            .expect("exposure_ev is always finite (clamped on ingest — spec A2)")
    }
}

/// Factory registering [`ExposureNode`].
#[derive(Default)]
pub struct ExposureFactory {}

impl NodeFactory for ExposureFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ExposureNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(EXPOSURE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_at_zero_ev() {
        assert_eq!(exposure_gain(0.0), 1.0);
    }

    /// The epic's headline contract: +1 EV doubles linear values (a visible
    /// brighten), -1 EV halves them.
    #[test]
    fn plus_one_ev_doubles_linear_value() {
        assert!((exposure_gain(1.0) - 2.0).abs() < 1e-6);
        assert!((exposure_gain(-1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn is_identity_tracks_the_exposure_field() {
        let mut g = GlobalStages::default();
        assert!(ExposureNode::is_identity(&g));
        g.exposure = 0.5;
        assert!(!ExposureNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_ev() {
        let g = GlobalStages {
            exposure: 1.25,
            ..GlobalStages::default()
        };
        let pb = ExposureNode::param_block(&g);
        assert_eq!(pb.get_f64("ev"), Some(1.25));
    }
}
