// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `src.decoded` — source injection into the graph (spec §1 engine-owned nodes).
//!
//! Owner: **A-gpu**. Injects the uploaded [`crate::ng::SourceImage`] as the
//! graph's root `LinearRgbaF16` tile; the RAM pin (task B4) lives on this
//! stage's output.

use std::sync::Arc;

use crate::ng::error::NodeError;
use crate::ng::node::param::ParamsSchema;
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, PortDecl,
    RenderNode,
};
use crate::ng::tile::{CpuTileView, TileView};
use crate::ng::types::{NodeId, PortType};

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

/// The `src.decoded` source-injection node. **A-gpu fills the bodies.**
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
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A-gpu: SrcDecodedNode::eval_gpu — inject uploaded source tile")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A-gpu: SrcDecodedNode::eval_cpu")
    }
}

/// Factory registering [`SrcDecodedNode`] with the [`crate::ng::NodeRegistry`].
#[derive(Default)]
pub struct SrcDecodedFactory {}

impl NodeFactory for SrcDecodedFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(SrcDecodedNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        unimplemented!("A-gpu: SrcDecodedFactory::kernel_salt — blake3 of algorithm rev")
    }
}
