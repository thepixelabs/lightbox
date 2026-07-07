// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! **C8 — the F32 precision escape hatch.** `precision()` is honored end-to-end
//! (the tile executor allocates `rgba32float` vs `rgba16float` per node), and an
//! accumulation-heavy node stored at F32 matches the f64 reference to ≥ 60 dB
//! while the same node stored at F16 is demonstrably worse — the test that
//! documents why the hatch exists (spec §4.2 / Risk R2).

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::exec::tiling::{TileOrder, TileProbe, TileRender};
use lightbox_render::ng::node::{
    CpuEvalCtx, GpuEvalCtx, NodeDescriptor, ParamsSchema, ParamsSchemaRef,
};
use lightbox_render::ng::types::Extent;
use lightbox_render::ng::{
    CpuTileView, NodeError, NodeId, ParamBlock, PixelBuf, PortDecl, PortType, RenderGraph,
    RenderNode, Roi, TilePrecision, TileView,
};
use lightbox_render_testkit::probes::AccumProbe;

// A test-only F32 source: a horizontal ramp of scene-linear inputs in
// [0.5, 3.5] (highlights above 1.0 where f16 is coarse), emitted as an
// `rgba32float` tile so the accumulator receives full-precision inputs.
struct F32Ramp;
static RAMP_SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
static RAMP_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.f32ramp"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF32,
    },
    params_schema: ParamsSchemaRef(&RAMP_SCHEMA),
};

fn ramp_input(x: u32, w: u32) -> f32 {
    0.5 + (x as f32 / (w.max(1) as f32)) * 3.0
}

impl RenderNode for F32Ramp {
    fn descriptor(&self) -> &NodeDescriptor {
        &RAMP_DESC
    }
    fn precision(&self) -> TilePrecision {
        TilePrecision::F32
    }
    fn eval_gpu(
        &self,
        _: &mut GpuEvalCtx<'_>,
        _: &[TileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        Err(NodeError::Gpu("test.f32ramp is CPU-only".into()))
    }
    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        _: &[CpuTileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        let out_roi = ctx.out_roi;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for ly in 0..h {
            for lx in 0..w {
                let v = ramp_input(out_roi.x as u32 + lx, RAMP_W);
                out.set_rgba_f32(lx, ly, [v, v, v, 1.0]);
            }
        }
        Ok(())
    }
}

const RAMP_W: u32 = 256;
const RAMP_H: u32 = 8;

/// Render `test.f32ramp → test.accum` at the accumulator's `precision`, reading
/// back the terminal tile's R channel.
fn accum_channel(precision: TilePrecision) -> (PixelBuf, TileProbe) {
    let mut g = RenderGraph::new();
    let ramp = g.add_node(Arc::new(F32Ramp));
    let accum = g.add_node(Arc::new(AccumProbe::with_precision(precision)));
    g.connect(ramp, accum, "in").unwrap();

    let seeds = HashMap::new();
    let cancel = CancelToken::new();
    let image = Extent {
        w: RAMP_W,
        h: RAMP_H,
    };
    let r = TileRender {
        graph: &g,
        image_extent: image,
        out_roi: Roi {
            x: 0,
            y: 0,
            w: RAMP_W,
            h: RAMP_H,
        },
        scale: 1.0,
        tile_size: 256,
        order: TileOrder::RowMajor,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    };
    let mut probe = TileProbe::default();
    let px = r.render(&mut probe).expect("accum render");
    (px, probe)
}

/// Peak-signal-to-noise (dB) of `sample` vs the f64 `reference`, peak = max ref.
fn psnr_db(reference: &[f64], sample: &[f64]) -> f64 {
    let n = reference.len() as f64;
    let peak = reference.iter().cloned().fold(0.0f64, f64::max).max(1.0);
    let sse: f64 = reference
        .iter()
        .zip(sample)
        .map(|(r, s)| (r - s) * (r - s))
        .sum();
    let mse = sse / n;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (peak * peak / mse).log10()
}

#[test]
fn f32_precision_matches_f64_reference_and_beats_f16() {
    // The f64 ground truth for each column's accumulated R value.
    let reference: Vec<f64> = (0..RAMP_W)
        .map(|x| AccumProbe::reference_value(ramp_input(x, RAMP_W) as f64))
        .collect();

    // F32 storage tile.
    let (f32_px, f32_probe) = accum_channel(TilePrecision::F32);
    // precision() honored end-to-end: the terminal tile is rgba32float.
    assert_eq!(f32_px.format, lightbox_render::ng::PixelFormat::Rgba32F);
    let f32_vals: Vec<f64> = (0..RAMP_W)
        .map(|x| f32_px.get_rgba_f32(x, 0)[0] as f64)
        .collect();

    // F16 storage tile — same algorithm, coarser storage.
    let (f16_px, _) = accum_channel(TilePrecision::F16);
    assert_eq!(f16_px.format, lightbox_render::ng::PixelFormat::Rgba16F);
    let f16_vals: Vec<f64> = (0..RAMP_W)
        .map(|x| f16_px.get_rgba_f32(x, 0)[0] as f64)
        .collect();

    let f32_psnr = psnr_db(&reference, &f32_vals);
    let f16_psnr = psnr_db(&reference, &f16_vals);
    eprintln!("[C8] accum PSNR vs f64 ref: F32 {f32_psnr:.1} dB, F16 {f16_psnr:.1} dB");

    // F32 matches the f64 reference to ≥ 60 dB.
    assert!(f32_psnr >= 60.0, "F32 PSNR {f32_psnr:.1} dB below 60 dB");
    // F16 is demonstrably worse — the justification for the escape hatch.
    assert!(
        f16_psnr <= f32_psnr - 20.0,
        "F16 ({f16_psnr:.1} dB) not demonstrably worse than F32 ({f32_psnr:.1} dB)"
    );
    // The F32 path really did accumulate (probe saw the node).
    assert!(f32_probe.terminal_pixels > 0);
}
