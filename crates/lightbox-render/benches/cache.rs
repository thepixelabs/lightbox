// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase B micro-benches (task B8): tail-only recompute vs full-graph, and
//! cache hit rate under slider churn.
//!
//! These compile and run reduced locally; the p95/regression baselines are the
//! **nightly** job on the reference-GPU runner (§8, perf regressions are
//! tracked, not PR-blocking). `cargo bench -p lightbox-render` runs them.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use lightbox_jobs::CancelToken;
use lightbox_types::PV_M0;

use lightbox_render::ng::cache::NodeCache;
use lightbox_render::ng::exec::cpu::CpuBackend;
use lightbox_render::ng::node::{NodeDescriptor, ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::{
    CpuEvalCtx, CpuTileView, Executor, GpuEvalCtx, NodeError, NodeId, ParamBlock, ParamValue,
    PortDecl, PortType, RenderGraph, RenderNode, RenderScale, Roi, TileView,
};

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
static SRC_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("bench.src"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};
static GAIN_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("bench.gain"),
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

struct Src;
impl RenderNode for Src {
    fn descriptor(&self) -> &NodeDescriptor {
        &SRC_DESC
    }
    fn eval_gpu(
        &self,
        _: &mut GpuEvalCtx<'_>,
        _: &[TileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        Ok(())
    }
    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        _: &[CpuTileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for y in 0..h {
            for x in 0..w {
                out.set_rgba_f32(x, y, [0.5, 0.5, 0.5, 1.0]);
            }
        }
        Ok(())
    }
}

struct Gain;
impl RenderNode for Gain {
    fn descriptor(&self) -> &NodeDescriptor {
        &GAIN_DESC
    }
    fn eval_gpu(
        &self,
        _: &mut GpuEvalCtx<'_>,
        _: &[TileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        Ok(())
    }
    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let gain = params.get_f64_or("gain", 1.0) as f32;
        let input = inputs[0].pixels;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for y in 0..h {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                out.set_rgba_f32(x, y, [p[0] * gain, p[1] * gain, p[2] * gain, p[3]]);
            }
        }
        Ok(())
    }
}

const ROI: Roi = Roi {
    x: 0,
    y: 0,
    w: 256,
    h: 256,
};

fn chain(last_gain: f64, len: usize) -> RenderGraph {
    let mut g = RenderGraph::new();
    let src = g.add_node(Arc::new(Src));
    let mut prev = src;
    for i in 0..len {
        let gain = if i + 1 == len { last_gain } else { 1.01 };
        let params = ParamBlock::from_fields([("gain", ParamValue::Float(gain))]).unwrap();
        let n = g.add_node_with_params(Arc::new(Gain), params);
        g.connect(prev, n, "in").unwrap();
        prev = n;
    }
    g
}

fn run(exec: &Executor, g: &RenderGraph, cache: &NodeCache) {
    let cancel = CancelToken::new();
    let _ = exec
        .evaluate(g, PV_M0, ROI, RenderScale::OneToOne, cache, &cancel, None)
        .expect("render");
}

fn bench_cache(c: &mut Criterion) {
    let exec = Executor::new(Arc::new(CpuBackend::new(None)));
    const LEN: usize = 12;

    // Full-graph recompute: a cold cache every iteration (all LEN+1 nodes).
    c.bench_function("full_graph_cold", |b| {
        b.iter(|| {
            let cache = NodeCache::new();
            run(&exec, black_box(&chain(2.0, LEN)), &cache);
        });
    });

    // Tail-only recompute: warm cache, change only the last node's param each
    // iteration, one node recomputes, the rest hit.
    c.bench_function("tail_only_warm", |b| {
        let cache = NodeCache::new();
        run(&exec, &chain(1.0, LEN), &cache); // warm
        let mut v = 1.0f64;
        b.iter(|| {
            v += 1.0;
            run(&exec, black_box(&chain(v, LEN)), &cache);
        });
    });

    // Slider churn: many small changes to the last node against a warm cache
    // (measures steady-state tail-only cost + hit rate).
    c.bench_function("slider_churn_last_node", |b| {
        let cache = NodeCache::new();
        run(&exec, &chain(1.0, LEN), &cache);
        let mut v = 1.0f64;
        b.iter(|| {
            for _ in 0..16 {
                v += 0.5;
                run(&exec, &chain(v, LEN), &cache);
            }
        });
    });
}

criterion_group!(benches, bench_cache);
criterion_main!(benches);
