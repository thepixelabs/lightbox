// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `src.decoded`, source injection into the graph (spec §1 engine-owned nodes).
//!
//! Owner: **A-gpu**. Injects the uploaded [`crate::ng::SourceImage`] as the
//! graph's root `LinearRgbaF16` tile; the RAM pin (task B4) lives on this
//! stage's output.
//!
//! In normal operation the executor **pre-populates** the source stage (the
//! [`crate::ng::source::Uploader`] output is cached under this node's key), so
//! `eval_*` here is the defensive identity-copy path: given the uploaded source
//! as its single input it copies it into a working tile. The GPU path uses
//! `copy.wgsl` (`@group(0)` input → `@group(1)` write-only output); the CPU path
//! clones the host pixels.

use std::sync::Arc;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::ParamsSchema;
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, PortDecl,
    RenderNode,
};
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The identity-copy kernel, naga-validated at build (`build.rs`).
const COPY_WGSL: &str = include_str!("../../../shaders/copy.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("src.decoded"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: crate::ng::node::ParamsSchemaRef(&SCHEMA),
};

/// The `src.decoded` source-injection node.
#[derive(Default)]
pub struct SrcDecodedNode {}

impl SrcDecodedNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("src.decoded");

    /// A fresh node.
    pub fn new() -> SrcDecodedNode {
        SrcDecodedNode::default()
    }
}

impl RenderNode for SrcDecodedNode {
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
            .ok_or_else(|| NodeError::Other("src.decoded: no source tile provided".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("src.decoded: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent { w: 1, h: 1 });

        let pipeline = ctx.kernels.compute_pipeline(COPY_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("src.decoded in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("src.decoded out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out],
            extent,
            "src.decoded",
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
            .ok_or_else(|| NodeError::Other("src.decoded: no source tile provided".into()))?;
        *ctx.output() = copy_cpu(input.pixels);
        Ok(())
    }
}

/// The CPU source-injection algorithm: a passthrough clone of the source pixels
/// (the parity anchor for `eval_gpu`'s copy kernel).
pub fn copy_cpu(src: &PixelBuf) -> PixelBuf {
    src.clone()
}

/// Factory registering [`SrcDecodedNode`] with the [`crate::ng::NodeRegistry`].
#[derive(Default)]
pub struct SrcDecodedFactory {}

impl NodeFactory for SrcDecodedFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(SrcDecodedNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(COPY_WGSL.as_bytes()))
    }
}
