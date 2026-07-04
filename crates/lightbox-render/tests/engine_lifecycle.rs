// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Engine seed ticket-lifecycle tests — E01 spec §5 T6 acceptance criteria:
//! submit→poll reaches `Ready`; two rapid submits on one viewport supersede
//! the first with exactly one eval (probe counter); cancel-before-run yields
//! `Cancelled` with no eval. Plus texture-pool recycling and the CPU-only
//! engine path (which runs on adapterless machines too).
//!
//! GPU-dependent tests skip gracefully when no adapter exists (CI
//! software-adapter fallback per spec §3.4).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::nodes::solid_color::{SolidColorNode, SolidColorPlanner};
use lightbox_render::{
    BackendKind, CpuCtx, Engine, GpuContext, GpuCtx, ImageBufU8, NodeError, NodeId, NodeRegistry,
    NullSourceResolver, ParamDelta, Params, PlannedEval, RenderError, RenderNode, RenderOutput,
    RenderPlanner, RenderRequest, RenderScale, RenderState, RenderTarget, RenderTicket, Roi,
    SourceResolver, TexturePool, Tile, TileCpu, ViewportId,
};
use lightbox_types::{ImageId, PV_M0};

fn request(viewport: u64, target: RenderTarget, w: u32, h: u32) -> RenderRequest {
    RenderRequest {
        image: ImageId(1),
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi::Full,
        scale: RenderScale::FitWithin { w, h },
        target,
        viewport: ViewportId(viewport),
    }
}

/// Polls until the ticket reaches a terminal state (with timeout).
fn wait_terminal(engine: &Engine, ticket: &RenderTicket) -> RenderState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = engine.poll(ticket);
        match state {
            RenderState::Pending | RenderState::Running => {
                assert!(Instant::now() < deadline, "ticket never became terminal");
                std::thread::sleep(Duration::from_millis(1));
            }
            terminal => return terminal,
        }
    }
}

/// A gate the test opens to release a blocked node eval — makes the
/// coalescing/cancel tests deterministic (the worker is provably busy while
/// the interesting submits happen).
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn open(&self) {
        let (lock, cvar) = &*self.0;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    fn wait_open(&self) {
        let (lock, cvar) = &*self.0;
        let mut open = lock.lock().unwrap();
        while !*open {
            let (guard, timeout) = cvar.wait_timeout(open, Duration::from_secs(10)).unwrap();
            open = guard;
            assert!(!timeout.timed_out(), "gate never opened");
        }
    }
}

/// Node that parks until its gate opens, then produces a 1×1 output.
struct BlockingNode {
    gate: Gate,
}

impl RenderNode for BlockingNode {
    fn id(&self) -> NodeId {
        NodeId("test.block")
    }

    fn eval_gpu(&self, ctx: &GpuCtx<'_>, _inputs: &[Tile], _p: &Params) -> Result<Tile, NodeError> {
        self.gate.wait_open();
        let texture = ctx.pool.acquire([1, 1]);
        let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        Ok(Tile {
            extent: [1, 1],
            texture,
            view,
            offset: [0, 0],
        })
    }

    fn eval_cpu(
        &self,
        _ctx: &CpuCtx<'_>,
        _inputs: &[TileCpu],
        _p: &Params,
    ) -> Result<TileCpu, NodeError> {
        self.gate.wait_open();
        Ok(TileCpu {
            buf: ImageBufU8 {
                px: vec![0, 0, 0, 255],
                width: 1,
                height: 1,
            },
            offset: [0, 0],
        })
    }

    fn invalidates(&self, _changed: &ParamDelta) -> bool {
        true
    }
}

/// Planner routing viewport 999 to the blocking node and everything else to
/// `solid.color` — lets tests occupy the single worker deterministically.
struct RoutingPlanner {
    solid: SolidColorPlanner,
}

impl RenderPlanner for RoutingPlanner {
    fn plan(
        &self,
        req: &RenderRequest,
        sources: &dyn SourceResolver,
        cancel: &CancelToken,
    ) -> Result<PlannedEval, RenderError> {
        if req.viewport == ViewportId(999) {
            Ok(PlannedEval {
                node: NodeId("test.block"),
                params: Params::from_cbor(Vec::new()),
                source: None,
            })
        } else {
            self.solid.plan(req, sources, cancel)
        }
    }
}

/// Engine wired with `solid.color` + the gated blocking node.
fn engine_with_gate(gpu: Option<GpuContext>) -> (Engine, Arc<SolidColorNode>, Gate) {
    let node = Arc::new(SolidColorNode::new());
    let gate = Gate::default();
    let mut registry = NodeRegistry::new();
    registry.register(PV_M0, node.clone());
    registry.register(PV_M0, Arc::new(BlockingNode { gate: gate.clone() }));
    let engine = Engine::new(gpu, registry, Arc::new(NullSourceResolver)).unwrap();
    engine.set_planner(Arc::new(RoutingPlanner {
        solid: SolidColorPlanner::new([0.2, 0.4, 0.6, 1.0]),
    }));
    (engine, node, gate)
}

fn expect_cpu(state: RenderState) -> ImageBufU8 {
    match state {
        RenderState::Ready(RenderOutput::Cpu(buf)) => buf,
        other => panic!("expected Ready(Cpu), got {other:?}"),
    }
}

// --- CPU-only engine: runs everywhere, adapter or not -----------------------

#[test]
fn cpu_engine_submit_poll_reaches_ready_with_expected_pixels() {
    let (engine, node, _gate) = engine_with_gate(None);
    assert_eq!(engine.backend_kind(), BackendKind::CpuOnly);

    let ticket = engine.submit(request(1, RenderTarget::CpuBuffer, 8, 6));
    let buf = expect_cpu(wait_terminal(&engine, &ticket));
    assert_eq!((buf.width, buf.height), (8, 6));
    assert_eq!(buf.px.len(), 8 * 6 * 4);
    // [0.2, 0.4, 0.6, 1.0] sRGB-encoded → bytes [51, 102, 153, 255].
    for texel in buf.px.chunks_exact(4) {
        assert_eq!(texel, [51, 102, 153, 255]);
    }
    assert_eq!(node.total_evals(), 1);
    assert_eq!(node.cpu_evals(), 1);
}

#[test]
fn cpu_engine_texture_target_fails_with_no_gpu() {
    let (engine, node, _gate) = engine_with_gate(None);
    let ticket = engine.submit(request(1, RenderTarget::Texture, 4, 4));
    match wait_terminal(&engine, &ticket) {
        RenderState::Failed(RenderError::NoGpu) => {}
        other => panic!("expected Failed(NoGpu), got {other:?}"),
    }
    assert_eq!(node.total_evals(), 0, "no eval for an impossible target");
}

#[test]
fn coalescing_two_rapid_submits_supersede_older_with_exactly_one_eval() {
    // Same logic drives GPU and CPU paths; CPU keeps this test universal.
    let (engine, node, gate) = engine_with_gate(None);

    // Occupy the single worker so both interesting submits are observed
    // strictly before either can run.
    let blocker = engine.submit(request(999, RenderTarget::CpuBuffer, 1, 1));

    let first = engine.submit(request(1, RenderTarget::CpuBuffer, 8, 8));
    let second = engine.submit(request(1, RenderTarget::CpuBuffer, 16, 16));

    // The older ticket is superseded at submit time, before any eval.
    assert!(matches!(engine.poll(&first), RenderState::Superseded));

    gate.open();
    let buf = expect_cpu(wait_terminal(&engine, &second));
    assert_eq!((buf.width, buf.height), (16, 16), "latest submit won");
    assert!(matches!(engine.poll(&first), RenderState::Superseded));
    assert!(matches!(
        wait_terminal(&engine, &blocker),
        RenderState::Ready(_)
    ));

    // T6 AC: exactly one solid.color eval ran for the two rapid submits.
    assert_eq!(node.total_evals(), 1);
}

#[test]
fn cancel_before_run_is_cancelled_and_never_evaluates() {
    let (engine, node, gate) = engine_with_gate(None);

    let blocker = engine.submit(request(999, RenderTarget::CpuBuffer, 1, 1));
    let ticket = engine.submit(request(1, RenderTarget::CpuBuffer, 8, 8));

    engine.cancel(&ticket);
    assert!(matches!(engine.poll(&ticket), RenderState::Cancelled));

    gate.open();
    assert!(matches!(
        wait_terminal(&engine, &blocker),
        RenderState::Ready(_)
    ));
    // Give the worker a chance to (incorrectly) run the cancelled job.
    assert!(matches!(
        wait_terminal(&engine, &ticket),
        RenderState::Cancelled
    ));
    assert_eq!(node.total_evals(), 0, "cancelled ticket must never eval");
}

#[test]
fn unregistered_node_fails_with_node_not_registered() {
    struct WrongNodePlanner;
    impl RenderPlanner for WrongNodePlanner {
        fn plan(
            &self,
            _req: &RenderRequest,
            _sources: &dyn SourceResolver,
            _cancel: &CancelToken,
        ) -> Result<PlannedEval, RenderError> {
            Ok(PlannedEval {
                node: NodeId("no.such.node"),
                params: Params::from_cbor(Vec::new()),
                source: None,
            })
        }
    }

    let engine = Engine::new(None, NodeRegistry::new(), Arc::new(NullSourceResolver)).unwrap();
    engine.set_planner(Arc::new(WrongNodePlanner));
    let ticket = engine.submit(request(1, RenderTarget::CpuBuffer, 4, 4));
    match wait_terminal(&engine, &ticket) {
        RenderState::Failed(RenderError::NodeNotRegistered { node, pv }) => {
            assert_eq!(node, NodeId("no.such.node"));
            assert_eq!(pv, PV_M0);
        }
        other => panic!("expected NodeNotRegistered, got {other:?}"),
    }
}

#[test]
fn engine_without_planner_fails_with_no_planner() {
    let engine = Engine::new(None, NodeRegistry::new(), Arc::new(NullSourceResolver)).unwrap();
    let ticket = engine.submit(request(1, RenderTarget::CpuBuffer, 4, 4));
    match wait_terminal(&engine, &ticket) {
        RenderState::Failed(RenderError::NoPlanner) => {}
        other => panic!("expected Failed(NoPlanner), got {other:?}"),
    }
}

#[test]
fn node_registry_keeps_old_process_versions() {
    use lightbox_types::ProcessVersion;
    let mut registry = NodeRegistry::new();
    let node = Arc::new(SolidColorNode::new());
    registry.register(ProcessVersion(1), node.clone());
    registry.register(ProcessVersion(2), node.clone());
    assert!(registry
        .get(SolidColorNode::ID, ProcessVersion(1))
        .is_some());
    assert!(registry
        .get(SolidColorNode::ID, ProcessVersion(2))
        .is_some());
    assert!(registry
        .get(SolidColorNode::ID, ProcessVersion(3))
        .is_none());
    assert!(registry.get(NodeId("other"), ProcessVersion(1)).is_none());
}

// --- GPU tests: skip gracefully without an adapter ---------------------------

fn gpu_or_skip(test: &str) -> Option<GpuContext> {
    match GpuContext::headless() {
        Some(gpu) => Some(gpu),
        None => {
            eprintln!("SKIP {test}: no wgpu adapter available");
            None
        }
    }
}

#[test]
fn gpu_engine_texture_target_reaches_ready_on_the_given_device() {
    let Some(gpu) = gpu_or_skip("gpu_engine_texture_target_reaches_ready_on_the_given_device")
    else {
        return;
    };
    let (engine, node, _gate) = engine_with_gate(Some(gpu.clone()));
    assert_eq!(engine.backend_kind(), BackendKind::Gpu(gpu.backend));

    // Seam invariant: the engine renders on the exact device it was given.
    assert!(Arc::ptr_eq(
        &engine.gpu().expect("gpu engine").device,
        &gpu.device
    ));

    let ticket = engine.submit(request(1, RenderTarget::Texture, 32, 24));
    match wait_terminal(&engine, &ticket) {
        RenderState::Ready(RenderOutput::Texture { tex, size, .. }) => {
            assert_eq!(size, [32, 24]);
            assert_eq!((tex.width(), tex.height()), (32, 24));
            assert_eq!(tex.format(), lightbox_render::OUTPUT_FORMAT);
        }
        other => panic!("expected Ready(Texture), got {other:?}"),
    }
    assert_eq!(node.gpu_evals(), 1);
}

#[test]
fn gpu_readback_matches_cpu_eval() {
    let Some(gpu) = gpu_or_skip("gpu_readback_matches_cpu_eval") else {
        return;
    };
    let (gpu_engine, _node, _gate) = engine_with_gate(Some(gpu));
    let (cpu_engine, _node2, _gate2) = engine_with_gate(None);

    let gpu_buf = expect_cpu(wait_terminal(
        &gpu_engine,
        &gpu_engine.submit(request(1, RenderTarget::CpuBuffer, 16, 16)),
    ));
    let cpu_buf = expect_cpu(wait_terminal(
        &cpu_engine,
        &cpu_engine.submit(request(1, RenderTarget::CpuBuffer, 16, 16)),
    ));
    // A constant fill must be exact on both paths (parity tolerances only
    // matter once real math lands in Phase 6).
    assert_eq!(gpu_buf, cpu_buf);
}

#[test]
fn texture_pool_recycles_by_size() {
    let Some(gpu) = gpu_or_skip("texture_pool_recycles_by_size") else {
        return;
    };
    let pool = TexturePool::new(gpu.device.clone());

    let first = pool.acquire([16, 16]);
    let first_again = pool.acquire([16, 16]);
    assert!(
        !Arc::ptr_eq(&first, &first_again),
        "a held texture must never be handed out again"
    );

    drop(first_again);
    let recycled = pool.acquire([16, 16]);
    // The dropped 16×16 texture must be reused rather than a fresh allocation,
    // and the still-held one must not be touched.
    assert!(!Arc::ptr_eq(&first, &recycled));
    assert_eq!(pool.len(), 2, "no third texture was created");

    let other_size = pool.acquire([32, 16]);
    assert_eq!((other_size.width(), other_size.height()), (32, 16));
    assert_eq!(pool.len(), 3, "different size allocates");
}
