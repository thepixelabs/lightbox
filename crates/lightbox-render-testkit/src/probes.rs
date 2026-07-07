// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Probe nodes for the cache / ROI / parity gates (spec §5; tasks A4, B5, C1,
//! C8).
//!
//! Owner: **A-core** (`test.gain` A4, `test.accum` C8, `test.checker`) and
//! **C** (`test.blur_r` radius-dependent apron, C1). These are **test-only**
//! nodes — they never register in the shipping engine. Each implements
//! [`lightbox_render::ng::RenderNode`]; bodies are stubs until their gate lands.

use lightbox_render::ng::node::{ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::{
    CpuEvalCtx, CpuTileView, GpuEvalCtx, NodeDescriptor, NodeError, NodeId, ParamBlock, PortDecl,
    PortType, RenderNode, TilePrecision, TileView,
};

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

/// `test.gain` — multiply each channel by a constant. The cache/tail-invalidation
/// probe (spec A4/B5). **A4 fills the bodies.**
#[derive(Default)]
pub struct GainProbe {}

static GAIN_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.gain"),
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

impl RenderNode for GainProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &GAIN_DESC
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A4 (A-core): GainProbe::eval_gpu")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A4/A14 (A-core): GainProbe::eval_cpu")
    }
}

/// `test.blur_r` — a radius-`r` box blur with the apron `plan()` back-propagation
/// the tiling gate exercises (spec C1/C3). **C1 fills the bodies + `plan`.**
#[derive(Default)]
pub struct BlurRProbe {}

static BLUR_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.blur_r"),
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

impl RenderNode for BlurRProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &BLUR_DESC
    }

    // NOTE: C1 overrides `plan()` here with `out.expand(ceil(r * scale))`.

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("C1 (C): BlurRProbe::eval_gpu — apron-correct box blur")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("C1 (C): BlurRProbe::eval_cpu")
    }
}

/// `test.accum` — an accumulation-heavy op used to demonstrate the F32 precision
/// escape hatch (spec C8). Requests `F32` output. **C8 fills the bodies.**
#[derive(Default)]
pub struct AccumProbe {}

static ACCUM_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.accum"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF32,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF32,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};

impl RenderNode for AccumProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &ACCUM_DESC
    }

    fn precision(&self) -> TilePrecision {
        TilePrecision::F32
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("C8 (C): AccumProbe::eval_gpu — F32 accumulation")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("C8 (C): AccumProbe::eval_cpu")
    }
}

/// `test.checker` — a source node emitting a checker pattern (no inputs); a
/// deterministic graph root for tests (spec A16). **A16 fills the bodies.**
#[derive(Default)]
pub struct CheckerProbe {}

static CHECKER_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.checker"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};

impl RenderNode for CheckerProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &CHECKER_DESC
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A16 (A-core): CheckerProbe::eval_gpu")
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let _ = (ctx, inputs, params);
        unimplemented!("A16 (A-core): CheckerProbe::eval_cpu")
    }
}
