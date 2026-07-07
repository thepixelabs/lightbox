// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `util.resize` — decimation / Lanczos for the scale ladders (spec §1
//! engine-owned nodes; task **C2**).
//!
//! Owner: **A-gpu** (WGSL + CPU resampler). Turns a [`crate::ng::RenderScale`]
//! into decimation at this node so a 45 MP source at `Fit(4K)` evaluates
//! ≤ ~8 MP (the §10.1 E05.3 gate). Passes its own golden.

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

/// The `util.resize` decimation node. **A-gpu fills the bodies.**
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
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A-gpu (C2): UtilResizeNode::eval_gpu — Lanczos/box decimation")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A-gpu (C2): UtilResizeNode::eval_cpu")
    }
}

/// Factory registering [`UtilResizeNode`].
#[derive(Default)]
pub struct UtilResizeFactory {}

impl NodeFactory for UtilResizeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(UtilResizeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        unimplemented!("A-gpu: UtilResizeFactory::kernel_salt")
    }
}
