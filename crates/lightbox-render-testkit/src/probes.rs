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

use lightbox_render::ng::node::{InputRois, NodeFactory, ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::{
    CpuEvalCtx, CpuTileView, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeError, NodeId, ParamBlock,
    PixelBuf, PortDecl, PortType, RenderNode, Roi, TilePrecision, TileView,
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

/// `test.blur_r` — a radius-`r` box blur with the radius-dependent apron
/// `plan()` back-propagation the ROI/tiling gates exercise (spec C1/C3).
///
/// The effective working-pixel radius is `ceil(radius * scale)`; `plan()` grows
/// the required input ROI by exactly that margin, and `eval_cpu` averages the
/// `(2r+1)²` neighbourhood. The kernel is **ROI-aware** — it maps absolute
/// working-pixel coordinates through the input view's covered ROI and
/// edge-replicates at the input's border — so a tiled per-tile eval (reading the
/// apron sub-tile) is bit-identical to the whole-image eval (the C3 gate).
#[derive(Default)]
pub struct BlurRProbe {}

impl BlurRProbe {
    /// Default box-blur radius (working pixels at scale 1.0).
    pub const DEFAULT_RADIUS: f64 = 2.0;

    /// The effective working-pixel radius for `params` at `scale` (spec §3.2
    /// `plan` = `out.expand(ceil(r*scale))`).
    pub fn effective_radius(params: &ParamBlock, scale: f32) -> u32 {
        let r = params.get_f64_or("radius", Self::DEFAULT_RADIUS).max(0.0) as f32;
        (r * scale).ceil() as u32
    }
}

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

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        InputRois(vec![out.expand(Self::effective_radius(params, scale))])
    }

    fn eval_gpu(
        &self,
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        // The GPU box-blur kernel lands with the A-gpu executor; CPU/GPU parity
        // is task E6. On the CPU reference path this returns a typed error.
        Err(gpu_deferred("test.blur_r"))
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("test.blur_r needs one input".to_owned()))?;
        let r = Self::effective_radius(params, ctx.scale) as i64;
        let in_roi = input.roi;
        let out_roi = ctx.out_roi;
        let src = input.pixels;
        // Input-local bounds (absolute → local via the covered ROI).
        let ix_max = src.extent.w as i64 - 1;
        let iy_max = src.extent.h as i64 - 1;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for ly in 0..h {
            let ay = out_roi.y as i64 + ly as i64;
            for lx in 0..w {
                let ax = out_roi.x as i64 + lx as i64;
                let mut acc = [0.0f32; 4];
                let mut count = 0.0f32;
                for dy in -r..=r {
                    // Absolute source Y clamped to the image border (which the
                    // apron guarantees the input tile covers), then to local.
                    let sy = (ay + dy).clamp(0, in_roi.y as i64 + iy_max);
                    let ly_src = (sy - in_roi.y as i64).clamp(0, iy_max) as u32;
                    for dx in -r..=r {
                        let sx = (ax + dx).clamp(0, in_roi.x as i64 + ix_max);
                        let lx_src = (sx - in_roi.x as i64).clamp(0, ix_max) as u32;
                        let p = src.get_rgba_f32(lx_src, ly_src);
                        for c in 0..4 {
                            acc[c] += p[c];
                        }
                        count += 1.0;
                    }
                }
                let inv = 1.0 / count.max(1.0);
                out.set_rgba_f32(
                    lx,
                    ly,
                    [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv],
                );
            }
        }
        Ok(())
    }
}

/// Factory for [`BlurRProbe`].
#[derive(Default)]
pub struct BlurRFactory {}

impl NodeFactory for BlurRFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(BlurRProbe::default())
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.blur_r@v1"))
    }
}

/// `test.accum` — an accumulation-heavy op that demonstrates the F32 precision
/// escape hatch (spec C8, §4.2 / Risk R2).
///
/// Each output channel accumulates [`AccumProbe::STEPS`] weighted terms into a
/// mid-range scene-linear value (well above 1.0, where f16's ~11-bit mantissa is
/// coarse). Storing that result in an `rgba16float` tile quantizes away the fine
/// structure; storing it in `rgba32float` preserves it. The node's `precision()`
/// selects the tile format, so the *same* algorithm run at F32 matches the f64
/// reference to ≥ 60 dB while at F16 it is demonstrably worse — the test that
/// documents why the escape hatch exists.
pub struct AccumProbe {
    precision: TilePrecision,
}

impl Default for AccumProbe {
    fn default() -> Self {
        AccumProbe {
            precision: TilePrecision::F32,
        }
    }
}

impl AccumProbe {
    /// Number of accumulation terms per channel.
    pub const STEPS: u32 = 64;

    /// An accumulator node emitting `precision`-format tiles.
    pub fn with_precision(precision: TilePrecision) -> AccumProbe {
        AccumProbe { precision }
    }

    /// The accumulation, in `f64` — the ground-truth reference the F32/F16 tile
    /// storage is compared against. Mirrors [`AccumProbe::accumulate_f32`] term
    /// for term (only the accumulator type differs).
    pub fn reference_value(channel_input: f64) -> f64 {
        let mut acc = 0.0f64;
        for k in 0..Self::STEPS {
            // Terms grow so the sum lands in the tens–hundreds (HDR highlights).
            acc += channel_input * (4.0 + k as f64 * 0.05);
        }
        acc
    }

    /// The same accumulation in `f32` compute (what both tile-precision variants
    /// run; only the *stored* result differs by tile format).
    pub fn accumulate_f32(channel_input: f32) -> f32 {
        let mut acc = 0.0f32;
        for k in 0..Self::STEPS {
            acc += channel_input * (4.0 + k as f32 * 0.05);
        }
        acc
    }
}

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
        self.precision
    }

    fn eval_gpu(
        &self,
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        Err(gpu_deferred("test.accum"))
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("test.accum needs one input".to_owned()))?;
        let in_roi = input.roi;
        let out_roi = ctx.out_roi;
        let src = input.pixels;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for ly in 0..h {
            let sy = (out_roi.y + ly as i32 - in_roi.y).clamp(0, src.extent.h as i32 - 1) as u32;
            for lx in 0..w {
                let sx =
                    (out_roi.x + lx as i32 - in_roi.x).clamp(0, src.extent.w as i32 - 1) as u32;
                let p = src.get_rgba_f32(sx, sy);
                out.set_rgba_f32(
                    lx,
                    ly,
                    [
                        Self::accumulate_f32(p[0]),
                        Self::accumulate_f32(p[1]),
                        Self::accumulate_f32(p[2]),
                        p[3],
                    ],
                );
            }
        }
        Ok(())
    }
}

/// Factory for [`AccumProbe`] at a fixed precision.
pub struct AccumFactory {
    precision: TilePrecision,
}

impl AccumFactory {
    /// A factory instantiating [`AccumProbe`]s at `precision`.
    pub fn new(precision: TilePrecision) -> AccumFactory {
        AccumFactory { precision }
    }
}

impl NodeFactory for AccumFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(AccumProbe::with_precision(self.precision))
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.accum@v1"))
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
