// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 Phase B integration tests — content-keyed cache, tail invalidation, the
//! source-stage RAM pin, latest-wins scheduling, and cache integrity under
//! cancellation (tasks B2, B4, B5, B6, B7). All run on the CPU reference path
//! (no GPU adapter required), so they are the authoritative single-process
//! gates on any box.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lightbox_edit::Recipe;
use lightbox_jobs::{CancelToken, JobConfig, JobSystem};
use lightbox_types::{ImageId, PV_M0};

use lightbox_render::ng::cache::NodeCache;
use lightbox_render::ng::colorimetry::{SourceColorimetry, SourceQuality};
use lightbox_render::ng::compile::{GraphTemplate, RecipeCompiler};
use lightbox_render::ng::config::{BackendPref, EngineConfig};
use lightbox_render::ng::exec::cpu::CpuBackend;
use lightbox_render::ng::node::{
    KernelSalt, NodeDescriptor, NodeFactory, ParamsSchema, ParamsSchemaRef, PvRange,
};
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::source::{
    DeviceHandles, DeviceProvider, SourceImage, SourceProvider, SourceWant,
};
use lightbox_render::ng::types::Extent;
use lightbox_render::ng::{
    BoxFuture, CpuEvalCtx, CpuTileView, DeviceError, Engine, Executor, GpuEvalCtx, JobsHandle,
    NodeError, NodeId, NodeRegistry, OutFormat, OutputPayload, ParamBlock, ParamValue, PixelBuf,
    PixelFormat, PortDecl, PortType, RenderGraph, RenderNode, RenderPriority, RenderRequest,
    RenderScale, RenderScheduler, RenderState, RenderTarget, Roi, SourceError, TileView, ViewState,
    Zoom,
};

// ─────────────────────────────────────────────────────────────────────────────
// Test nodes (point-wise CPU kernels; eval_gpu unused on the CPU path).
// ─────────────────────────────────────────────────────────────────────────────

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

macro_rules! descriptor {
    ($name:ident, $id:literal, $inputs:expr) => {
        static $name: NodeDescriptor = NodeDescriptor {
            id: NodeId($id),
            inputs: $inputs,
            output: PortDecl {
                name: "out",
                ty: PortType::LinearRgbaF16,
            },
            params_schema: ParamsSchemaRef(&SCHEMA),
        };
    };
}

descriptor!(SRC_DESC, "test.src", &[]);
descriptor!(
    GAIN_DESC,
    "test.gain",
    &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }]
);
descriptor!(
    MERGE_DESC,
    "test.merge",
    &[
        PortDecl {
            name: "a",
            ty: PortType::LinearRgbaF16,
        },
        PortDecl {
            name: "b",
            ty: PortType::LinearRgbaF16,
        },
    ]
);

/// A constant-colour source (fills `[v, v, v, 1]`, `v` default 0.5).
struct Source;
impl RenderNode for Source {
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
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let v = params.get_f64_or("v", 0.5) as f32;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for y in 0..h {
            for x in 0..w {
                out.set_rgba_f32(x, y, [v, v, v, 1.0]);
            }
        }
        Ok(())
    }
}

/// Multiply the input by `gain` (default 1.0), leaving alpha.
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

/// Average two inputs — the diamond's join.
struct Merge;
impl RenderNode for Merge {
    fn descriptor(&self) -> &NodeDescriptor {
        &MERGE_DESC
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
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        let a = inputs[0].pixels;
        let b = inputs[1].pixels;
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for y in 0..h {
            for x in 0..w {
                let pa = a.get_rgba_f32(x, y);
                let pb = b.get_rgba_f32(x, y);
                out.set_rgba_f32(
                    x,
                    y,
                    [
                        (pa[0] + pb[0]) * 0.5,
                        (pa[1] + pb[1]) * 0.5,
                        (pa[2] + pb[2]) * 0.5,
                        pa[3],
                    ],
                );
            }
        }
        Ok(())
    }
}

/// A node that cancels the render token and returns `Cancelled` — used to prove
/// a cancelled eval never poisons the cache (task B7). Same port shape as
/// [`Gain`] so it slots into a chain at any position.
struct SelfCancel;
impl RenderNode for SelfCancel {
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
        _: &[CpuTileView<'_>],
        _: &ParamBlock,
    ) -> Result<(), NodeError> {
        ctx.cancel.cancel();
        Err(NodeError::Cancelled)
    }
}

const ROI: Roi = Roi {
    x: 0,
    y: 0,
    w: 16,
    h: 16,
};

fn gain_params(gain: f64) -> ParamBlock {
    ParamBlock::from_fields([("gain", ParamValue::Float(gain))]).unwrap()
}

/// Build `test.src → gain(gains[0]) → gain(gains[1]) → …`. When `poison_at` is
/// `Some(k)` the k-th gain node (1-based graph index) is a [`SelfCancel`].
fn build_chain(gains: &[f64], poison_at: Option<usize>) -> RenderGraph {
    let mut g = RenderGraph::new();
    let src = g.add_node(Arc::new(Source));
    let mut prev = src;
    for (i, &gain) in gains.iter().enumerate() {
        let node_idx = i + 1; // 1-based graph position
        let n = if poison_at == Some(node_idx) {
            g.add_node_with_params(Arc::new(SelfCancel), gain_params(gain))
        } else {
            g.add_node_with_params(Arc::new(Gain), gain_params(gain))
        };
        g.connect(prev, n, "in").unwrap();
        prev = n;
    }
    g
}

fn render(graph: &RenderGraph, cache: &NodeCache, exec: &Executor) -> PixelBuf {
    let cancel = CancelToken::new();
    let tile = exec
        .evaluate(
            graph,
            PV_M0,
            ROI,
            RenderScale::OneToOne,
            cache,
            &cancel,
            None,
        )
        .expect("clean render succeeds");
    tile.cpu().expect("CPU tile").clone()
}

fn bytes_eq(a: &PixelBuf, b: &PixelBuf) -> bool {
    a.extent == b.extent && a.format == b.format && a.bytes == b.bytes
}

// ─────────────────────────────────────────────────────────────────────────────
// B5 — the tail-invalidation gate (PR-blocking, §10.1 E05.2).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn b5_tail_invalidation_recompute_probe_chain() {
    // An n-node chain: 0-based graph index 0 is the source, indices 1..n-1 are
    // gains. In the spec's 1-based numbering (nodes 1..n) changing node j
    // evaluates exactly (n - j + 1) nodes and the upstream j-1 all cache-hit;
    // here `k` is the 0-based index we change, so j = k + 1 and the tail size is
    // n - k with k upstream hits.
    let gains = [1.1, 1.2, 1.3, 1.4, 1.5];
    let n = gains.len() + 1; // 6 total nodes
    let cache = NodeCache::new();
    let exec = Executor::new(Arc::new(CpuBackend::new(None)));

    // Cold render: every node is a miss.
    let graph_a = build_chain(&gains, None);
    let _ = render(&graph_a, &cache, &exec);
    assert_eq!(exec.probe().snapshot().nodes_evaluated, n as u64);
    assert_eq!(exec.probe().snapshot().cache_hits, 0);

    // Change each gain node in turn (0-based indices 1..=n-1). The cold-render
    // entries are never evicted (default budget), so every iteration measures
    // against the same warm upstream.
    for k in 1..n {
        exec.probe().reset();
        let mut changed = gains;
        changed[k - 1] += 10.0; // a distinct param at graph node k (gains[k-1])
        let graph_b = build_chain(&changed, None);
        let _ = render(&graph_b, &cache, &exec);

        let s = exec.probe().snapshot();
        // Exactly the tail (nodes k..n-1) recomputes; the upstream k all hit.
        assert_eq!(
            s.nodes_evaluated,
            (n - k) as u64,
            "changing node k={k} must evaluate exactly the tail (n-k = {}) nodes",
            n - k
        );
        assert_eq!(
            s.cache_hits,
            k as u64,
            "the upstream {k} nodes (source + first {}) must all be cache hits",
            k - 1
        );
    }

    // The source-node edge case: changing node 0 (the source's own param)
    // re-keys the whole chain — all n nodes recompute, zero upstream hits.
    exec.probe().reset();
    let mut src_changed = RenderGraph::new();
    let src = src_changed.add_node_with_params(
        Arc::new(Source),
        ParamBlock::from_fields([("v", ParamValue::Float(0.9))]).unwrap(),
    );
    let mut prev = src;
    for &gain in &gains {
        let node = src_changed.add_node_with_params(Arc::new(Gain), gain_params(gain));
        src_changed.connect(prev, node, "in").unwrap();
        prev = node;
    }
    let _ = render(&src_changed, &cache, &exec);
    assert_eq!(
        exec.probe().snapshot().nodes_evaluated,
        n as u64,
        "changing the source re-keys the entire chain"
    );
    assert_eq!(exec.probe().snapshot().cache_hits, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// B2 — key derivation + propagation through chains AND diamonds.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn b2_propagation_through_a_diamond() {
    // src → a(gain), src → b(gain), merge(a, b).
    fn diamond(gain_a: f64, gain_b: f64) -> RenderGraph {
        let mut g = RenderGraph::new();
        let src = g.add_node(Arc::new(Source));
        let a = g.add_node_with_params(Arc::new(Gain), gain_params(gain_a));
        let b = g.add_node_with_params(Arc::new(Gain), gain_params(gain_b));
        let m = g.add_node(Arc::new(Merge));
        g.connect(src, a, "in").unwrap();
        g.connect(src, b, "in").unwrap();
        g.connect(a, m, "a").unwrap();
        g.connect(b, m, "b").unwrap();
        g
    }

    let cache = NodeCache::new();
    let exec = Executor::new(Arc::new(CpuBackend::new(None)));

    // Cold render: 4 nodes evaluate.
    let _ = render(&diamond(2.0, 3.0), &cache, &exec);
    assert_eq!(exec.probe().snapshot().nodes_evaluated, 4);

    // Change only branch a's gain: a re-keys, so the merge re-keys (it folds a's
    // key); src and branch b stay hittable. Exactly {a, merge} recompute.
    exec.probe().reset();
    let _ = render(&diamond(5.0, 3.0), &cache, &exec);
    let s = exec.probe().snapshot();
    assert_eq!(s.nodes_evaluated, 2, "only branch a + the merge recompute");
    assert_eq!(s.cache_hits, 2, "src + branch b stay cache hits");
}

// ─────────────────────────────────────────────────────────────────────────────
// B7 — cache integrity under cancellation (poisoning fuzz).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn b7_cancelled_evals_never_poison_the_cache() {
    let gains = [1.1, 1.2, 1.3, 1.4, 1.5];
    // Reference: a clean render from an empty cache.
    let reference = {
        let cache = NodeCache::new();
        let exec = Executor::new(Arc::new(CpuBackend::new(None)));
        render(&build_chain(&gains, None), &cache, &exec)
    };

    // Poison a shared cache with cancelled renders at every position, then verify
    // a final clean render equals the reference exactly.
    let cache = NodeCache::new();
    let exec = Executor::new(Arc::new(CpuBackend::new(None)));
    for k in 1..=gains.len() {
        let poison = build_chain(&gains, Some(k));
        let cancel = CancelToken::new();
        let r = exec.evaluate(
            &poison,
            PV_M0,
            ROI,
            RenderScale::OneToOne,
            &cache,
            &cancel,
            None,
        );
        assert!(
            matches!(r, Err(lightbox_render::ng::RenderError::Cancelled)),
            "poison render at {k} must cancel"
        );
    }
    // Also throw in fully-cancelled renders (cancel before eval).
    for _ in 0..8 {
        let clean = build_chain(&gains, None);
        let cancel = CancelToken::new();
        cancel.cancel();
        let _ = exec.evaluate(
            &clean,
            PV_M0,
            ROI,
            RenderScale::OneToOne,
            &cache,
            &cancel,
            None,
        );
    }

    let final_out = render(&build_chain(&gains, None), &cache, &exec);
    assert!(
        bytes_eq(&final_out, &reference),
        "a clean render over a cancellation-poisoned cache must equal the never-cancelled reference"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// B4 — source-stage RAM pin: zero SourceProvider::fetch after a downstream
// invalidation.
// ─────────────────────────────────────────────────────────────────────────────

/// A `SourceProvider` that counts `fetch` calls and returns a fixed gradient.
struct CountingSource {
    count: Arc<AtomicU64>,
    extent: Extent,
}
impl SourceProvider for CountingSource {
    fn fetch(
        &self,
        _image: ImageId,
        _want: SourceWant,
        _cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        let extent = self.extent;
        Box::pin(async move {
            let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, extent);
            for y in 0..extent.h {
                for x in 0..extent.w {
                    let v = (x as f32) / (extent.w.max(1) as f32);
                    px.set_rgba_f32(x, y, [v, v, v, 1.0]);
                }
            }
            Ok(SourceImage {
                pixels: px,
                colorimetry: SourceColorimetry::default(),
                full_extent: extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unimplemented!("CPU-only path never asks for a device")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async { Err(DeviceError::Rebuild("no device in test".to_owned())) })
    }
}

struct GainFactory;
impl NodeFactory for GainFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(Gain)
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.gain@b4"))
    }
}

fn poll_terminal(engine: &Engine, ticket: &lightbox_render::ng::RenderTicket) -> RenderState {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let s = engine.poll(ticket);
        if matches!(
            s,
            RenderState::Complete(_) | RenderState::Failed(_) | RenderState::Cancelled
        ) || std::time::Instant::now() > deadline
        {
            return s;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn b4_request(scale: RenderScale) -> RenderRequest {
    RenderRequest {
        image: ImageId(7),
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: ROI,
        scale,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

#[test]
fn b4_source_pin_means_zero_refetch_on_downstream_change() {
    let count = Arc::new(AtomicU64::new(0));
    let source = Arc::new(CountingSource {
        count: Arc::clone(&count),
        extent: Extent { w: 16, h: 16 },
    });

    // Template: src.decoded → test.gain (a real source stage + a downstream node).
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("test.gain"),
        PvRange::from_open(PV_M0),
        Arc::new(GainFactory),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(
            PV_M0,
            GraphTemplate::linear(vec![SrcDecodedNode::ID, NodeId("test.gain")]),
        )
        .unwrap();

    let engine = Engine::with_compiler(
        Arc::new(NullDevice),
        source,
        compiler,
        EngineConfig {
            backend: BackendPref::ForceCpu,
            ..EngineConfig::default()
        },
    )
    .unwrap();

    // First render (cold): exactly one fetch, source pinned.
    let t1 = engine.submit(b4_request(RenderScale::Ratio(1.0)));
    assert!(matches!(
        poll_terminal(&engine, &t1),
        RenderState::Complete(_)
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1, "cold render fetches once");
    let evals_after_first = engine.stats().nodes_evaluated;

    // Second render at a different scale: the downstream tail re-keys (scale_q
    // differs) and re-evaluates, but the scale-independent source stage stays
    // RAM-pinned — ZERO additional fetches (task B4).
    let t2 = engine.submit(b4_request(RenderScale::Ratio(0.5)));
    assert!(matches!(
        poll_terminal(&engine, &t2),
        RenderState::Complete(_)
    ));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "a downstream-invalidating change must NOT re-fetch the pinned source"
    );
    assert!(
        engine.stats().nodes_evaluated > evals_after_first,
        "the downstream tail must have recomputed on the scale change"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// B6 — RenderScheduler latest-wins coalescing (end-to-end through the engine).
// ─────────────────────────────────────────────────────────────────────────────

descriptor!(SLOW_DESC, "test.slow", &[]);

/// A source node that sleeps so a render stays in flight long enough for a burst
/// of `set_recipe` calls to coalesce.
struct SlowSource;
impl RenderNode for SlowSource {
    fn descriptor(&self) -> &NodeDescriptor {
        &SLOW_DESC
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
        std::thread::sleep(Duration::from_millis(12));
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

struct SlowFactory;
impl NodeFactory for SlowFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(SlowSource)
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.slow@b6"))
    }
}

#[test]
fn b6_latest_wins_coalescing_bounds_submissions_and_final_wins() {
    let mut reg = NodeRegistry::new();
    reg.register(
        NodeId("test.slow"),
        PvRange::from_open(PV_M0),
        Arc::new(SlowFactory),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(PV_M0, GraphTemplate::linear(vec![NodeId("test.slow")]))
        .unwrap();
    let engine = Arc::new(
        Engine::with_compiler(
            Arc::new(NullDevice),
            Arc::new(NullSourceProvider),
            compiler,
            EngineConfig {
                backend: BackendPref::ForceCpu,
                ..EngineConfig::default()
            },
        )
        .unwrap(),
    );

    let jobs = JobsHandle(Arc::new(JobSystem::new(JobConfig::default())));
    let scheduler = RenderScheduler::new(engine, jobs);
    let image = ImageId(3);
    let view = ViewState {
        viewport: Extent { w: 16, h: 16 },
        zoom: Zoom(1.0),
        pan: ROI,
    };
    scheduler.set_view(image, view);

    // A burst of 200 rapid set_recipe calls (microseconds each) against a ~12 ms
    // render: the first is in flight, the other 199 coalesce into one pending.
    const BURST: usize = 200;
    for _ in 0..BURST {
        scheduler.set_recipe(image, Recipe::identity(PV_M0), PV_M0);
    }

    assert!(
        scheduler.wait_idle(Duration::from_secs(5)),
        "scheduler reached quiescence"
    );

    // ≤ 2 engine submissions for the whole burst (the §10.1 E05.2 B6 property).
    let subs = scheduler.submissions();
    assert!(
        subs <= 2,
        "expected ≤ 2 submissions for the burst, got {subs}"
    );

    // The final completed frame corresponds to the LAST request in the burst.
    // set_view is seq 1; the 200 set_recipe calls are seq 2..=201.
    assert_eq!(
        scheduler.last_completed_seq(image),
        Some((BURST + 1) as u64),
        "the final frame must reflect the final request"
    );
    let out = scheduler.last_output(image).expect("a completed frame");
    assert!(matches!(out.payload, OutputPayload::Pixels(_)));
}

/// A source provider that is never consulted (the slow-source graph has no
/// `src.decoded` stage).
struct NullSourceProvider;
impl SourceProvider for NullSourceProvider {
    fn fetch(
        &self,
        _image: ImageId,
        _want: SourceWant,
        _cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        Box::pin(async { Err(SourceError::NotFound) })
    }
}
