// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `util.resize` — decimation for the scale ladders (spec §1 engine-owned nodes;
//! task **C2**).
//!
//! Owner: **A-gpu** (WGSL + CPU resampler). Phase A ships an **area-average box
//! filter** decimation (`resize.wgsl` / [`decimate_box_cpu`]); real Lanczos is
//! task C2. The output extent is whatever the backend sized the output tile to
//! (identity when unscaled, smaller for the fit/preview ladders); the kernel
//! area-averages the input region each output texel covers. GPU and CPU use the
//! exact same integer region math so the ΔE2000 parity gate (§4.4) holds.

use std::sync::Arc;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::ParamsSchema;
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, PortDecl,
    RenderNode,
};
use crate::ng::nodes::support;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The box-decimation kernel, naga-validated at build (`build.rs`).
const RESIZE_WGSL: &str = include_str!("../../../shaders/resize.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("util.resize"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: crate::ng::node::ParamsSchemaRef(&SCHEMA),
};

/// The `util.resize` decimation node.
#[derive(Default)]
pub struct UtilResizeNode {}

impl UtilResizeNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("util.resize");

    /// A fresh node.
    pub fn new() -> UtilResizeNode {
        UtilResizeNode::default()
    }
}

impl RenderNode for UtilResizeNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("util.resize: missing input tile".into()))?;
        let in_extent = Extent {
            w: input.roi.w,
            h: input.roi.h,
        };
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("util.resize: output is not a GPU tile".into()))?
            .clone();
        let out_extent = ctx.output().extent().unwrap_or(in_extent);

        let params = [in_extent.w, in_extent.h, out_extent.w, out_extent.h];
        let mut ubo_bytes = [0u8; 16];
        for (i, word) in params.iter().enumerate() {
            ubo_bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("util.resize params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(RESIZE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize params"),
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
            out_extent,
            "util.resize",
        );
        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("util.resize: missing input tile".into()))?;
        // The output extent is carried on the pre-sized output buffer.
        let out_extent = ctx.output().extent;
        *ctx.output() = decimate_box_cpu(input.pixels, out_extent);
        Ok(())
    }
}

/// Area-average box decimation of `src` down to `out_extent` (the CPU parity
/// twin of `resize.wgsl`; identical half-open integer region math).
pub fn decimate_box_cpu(src: &PixelBuf, out_extent: Extent) -> PixelBuf {
    let in_w = src.extent.w.max(1);
    let in_h = src.extent.h.max(1);
    let out_w = out_extent.w.max(1);
    let out_h = out_extent.h.max(1);
    let mut out = support::new_rgba16f(Extent { w: out_w, h: out_h });

    for oy in 0..out_h {
        let y0 = (oy * in_h) / out_h;
        let y1 = (((oy + 1) * in_h) / out_h).max(y0 + 1);
        for ox in 0..out_w {
            let x0 = (ox * in_w) / out_w;
            let x1 = (((ox + 1) * in_w) / out_w).max(x0 + 1);
            let mut acc = [0.0f32; 4];
            let mut count = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = support::read_rgba_f32(src, xx as usize, yy as usize);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                    count += 1.0;
                }
            }
            let inv = 1.0 / count.max(1.0);
            support::write_rgba16f(
                &mut out,
                ox as usize,
                oy as usize,
                [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv],
            );
        }
    }
    out
}

/// Factory registering [`UtilResizeNode`].
#[derive(Default)]
pub struct UtilResizeFactory {}

impl NodeFactory for UtilResizeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(UtilResizeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(RESIZE_WGSL.as_bytes()))
    }
}
