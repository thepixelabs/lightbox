// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Probe nodes for the cache / ROI / parity gates (spec §5; tasks A4, B5, C1,
//! C8).
//!
//! Owner: **A-core** (`test.gain` A4, `test.checker` A16) and **C**
//! (`test.blur_r` radius-dependent apron C1, `test.accum` F32 C8). These are
//! **test-only** nodes — they never register in the shipping engine. Each
//! implements [`lightbox_render::ng::RenderNode`].
//!
//! The GPU kernels for the A-core probes land with the **A-gpu** executor
//! (merge — CPU/GPU parity is task E6); on the CPU-only path `eval_gpu` returns
//! a typed error rather than panicking.

use std::sync::Arc;

use lightbox_render::ng::node::{NodeFactory, ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::{
    CpuEvalCtx, CpuTileView, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeError, NodeId, ParamBlock,
    PixelBuf, PortDecl, PortType, RenderNode, TilePrecision, TileView,
};

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

fn gpu_deferred(node: &str) -> NodeError {
    NodeError::Gpu(format!(
        "{node} GPU kernel lands with the A-gpu executor (merge; parity gate E6)"
    ))
}

/// `test.gain` — multiply each colour channel by a constant `gain` param
/// (default `2.0`), leaving alpha. The cache/tail-invalidation probe (spec
/// A4/B5). Point-wise, so tiled == untiled trivially.
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
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        Err(gpu_deferred("test.gain"))
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("test.gain needs one input".to_owned()))?
            .pixels;
        let gain = params.get_f64_or("gain", 2.0) as f32;
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let g = [p[0] * gain, p[1] * gain, p[2] * gain, p[3]];
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], g);
            }
        });
        Ok(())
    }
}

/// Factory for [`GainProbe`].
#[derive(Default)]
pub struct GainFactory {}

impl NodeFactory for GainFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GainProbe::default())
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.gain@v1"))
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

/// `test.checker` — a source node emitting a fixed 2-colour checker (no inputs);
/// a deterministic graph root for the first golden (spec A16). Cell edge is
/// [`CheckerProbe::CELL`] pixels; the two colours are scene-linear working
/// values.
#[derive(Default)]
pub struct CheckerProbe {}

impl CheckerProbe {
    /// Checker cell edge, pixels.
    pub const CELL: u32 = 8;
    /// The two scene-linear working colours (RGBA).
    pub const COLOR_A: [f32; 4] = [0.20, 0.45, 0.70, 1.0];
    pub const COLOR_B: [f32; 4] = [0.85, 0.15, 0.50, 1.0];

    /// The working colour at pixel `(x, y)`.
    pub fn color_at(x: u32, y: u32) -> [f32; 4] {
        if (x / Self::CELL + y / Self::CELL).is_multiple_of(2) {
            Self::COLOR_A
        } else {
            Self::COLOR_B
        }
    }
}

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
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        Err(gpu_deferred("test.checker"))
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        _inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                PixelBuf::encode_pixel(
                    fmt,
                    &mut row[x as usize * bpp..],
                    CheckerProbe::color_at(x, y),
                );
            }
        });
        Ok(())
    }
}

/// Factory for [`CheckerProbe`].
#[derive(Default)]
pub struct CheckerFactory {}

impl NodeFactory for CheckerFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(CheckerProbe::default())
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.checker@v1"))
    }
}
