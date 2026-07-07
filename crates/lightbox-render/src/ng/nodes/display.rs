// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `xform.display` — working→display transform (spec §1 engine-owned nodes),
//! generalized from E01's M0 `display.transform`.
//!
//! Owner: **A-gpu**. The **color math is supplied by `lightbox-color`** (LCMS2
//! working/display transforms) — the engine holds **zero color science**
//! (E02 guardrail). This node produces the `DisplayRgba8` the canvas composites.

// The engine consumes color math from `lightbox-color` here (LCMS2 display
// transform); marked so the dependency edge is explicit while the body is stubbed.
use lightbox_color as _;

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
    id: NodeId("xform.display"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::DisplayRgba8,
    },
    params_schema: crate::ng::node::ParamsSchemaRef(&SCHEMA),
};

/// The `xform.display` working→display node. **A-gpu fills the bodies**
/// (calling `lightbox-color` for the transform).
#[derive(Default)]
pub struct XformDisplayNode {}

impl XformDisplayNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("xform.display");

    /// A fresh node.
    pub fn new() -> XformDisplayNode {
        XformDisplayNode::default()
    }
}

impl RenderNode for XformDisplayNode {
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
        unimplemented!("A-gpu: XformDisplayNode::eval_gpu — working→display (lightbox-color)")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A-gpu: XformDisplayNode::eval_cpu")
    }
}

/// Factory registering [`XformDisplayNode`].
#[derive(Default)]
pub struct XformDisplayFactory {}

impl NodeFactory for XformDisplayFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(XformDisplayNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        unimplemented!("A-gpu: XformDisplayFactory::kernel_salt")
    }
}
