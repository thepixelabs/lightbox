// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The engine lifecycle + public request/output DTOs (spec §3.6).
//!
//! Owner: **A-core** (task **A12** submit/poll/cancel + ticket store + render
//! thread; **A1** config/errors). `events`/`active_backend` are wired by **E**
//! (device-lost / degradation). This is the surface **F5** promotes to the
//! crate root (replacing the E01 seed).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;
use tokio::sync::broadcast;

use crate::ng::cache::NodeCache;
use crate::ng::colorimetry::{OutputColorimetry, SourceColorimetry, SourceKind};
use crate::ng::compile::{GraphTemplate, RecipeCompiler, SourceDesc};
use crate::ng::config::{BackendPref, EngineConfig};
use crate::ng::error::{EngineInitError, RenderError};
use crate::ng::exec::cpu::CpuBackend;
use crate::ng::exec::{Backend, Executor};
use crate::ng::node::NodeRegistry;
use crate::ng::source::{DeviceProvider, SourceProvider};
use crate::ng::stats::EngineStats;
use crate::ng::tile::{PixelBuf, PixelFormat, TileHandle};
use crate::ng::types::{Extent, ProcessVersion, RenderScale, Roi};

/// Which backend produced an output — provenance stamped on every
/// [`RenderOutput`] (spec §3.6/§4.4). Returned by [`crate::ng::Backend::kind`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendId {
    /// The GPU (wgpu) path.
    Gpu,
    /// The CPU (rayon) path.
    Cpu,
}

/// What the engine is currently rendering on (spec §3.6 `active_backend`).
#[derive(Clone, Debug)]
pub enum ActiveBackend {
    /// GPU rendering on the given adapter.
    Gpu(wgpu::AdapterInfo),
    /// Degraded to preview-resolution CPU editing (§4.4).
    CpuPreviewOnly,
}

/// Render priority, mapping to E06 `Class` semantics (spec §3.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderPriority {
    /// Slider-to-screen work; preempts `Batch` at tile granularity.
    Interactive,
    /// Export / background work.
    Batch,
}

/// The pixel format of a `Buffer` render target's CPU readback (spec §3.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum OutFormat {
    /// 8-bit RGBA, sRGB-encoded (the common export/preview readback).
    Rgba8Srgb,
    /// 16-bit RGBA.
    Rgba16,
    /// 32-bit-float RGBA.
    Rgba32F,
}

/// Where a render's output lands (spec §3.6).
#[derive(Clone, Copy, Debug)]
pub enum RenderTarget {
    /// The engine-owned double-buffered texture pair (same device, zero-copy).
    Canvas,
    /// CPU readback: export (E15), preview build (E03), tests.
    Buffer {
        /// The readback pixel format.
        format: OutFormat,
    },
}

/// A render request (spec §3.6).
pub struct RenderRequest {
    /// The image to render.
    pub image: ImageId,
    /// The materialized recipe view (read-only; from `lightbox-edit`).
    pub recipe: Recipe,
    /// The process version to evaluate under.
    pub pv: ProcessVersion,
    /// Region of interest.
    pub roi: Roi,
    /// Output scale.
    pub scale: RenderScale,
    /// Where the output lands.
    pub target: RenderTarget,
    /// Interactive vs Batch (maps to E06 `Class`).
    pub priority: RenderPriority,
    /// Cooperative cancellation (from `lightbox-jobs`, E06).
    pub cancel: CancelToken,
}

/// An opaque handle to a submitted render (spec §3.6). Cheap to clone.
#[derive(Clone, Copy, Debug)]
pub struct RenderTicket(pub u64);

/// Output fidelity tier — badge-able by the shell (spec §3.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputQuality {
    /// A source preview tier passed through display-transform only (§4.3).
    PreviewTier,
    /// A full render at preview resolution.
    PreviewRes,
    /// A full-resolution render.
    FullRes,
}

/// A completed render's pixels or canvas generation (spec §3.6).
#[derive(Clone, Debug)]
pub enum OutputPayload {
    /// A completed canvas generation (double-buffer; sample via the scheduler).
    CanvasGeneration(u64),
    /// CPU pixels (the `Buffer` target).
    Pixels(PixelBuf),
}

/// A finished render (spec §3.6).
#[derive(Clone, Debug)]
pub struct RenderOutput {
    /// The output payload.
    pub payload: OutputPayload,
    /// Output colorimetry tag (opaque to the engine).
    pub colorimetry: OutputColorimetry,
    /// Which backend produced it (export records this, §4.4).
    pub backend: BackendId,
    /// The process version rendered under.
    pub pv: ProcessVersion,
    /// The fidelity tier (drives the shell badge).
    pub quality: OutputQuality,
}

/// Lifecycle state of a ticket, snapshot per [`Engine::poll`] (spec §3.6).
#[derive(Clone, Debug)]
pub enum RenderState {
    /// Queued, not yet started.
    Queued,
    /// Rendering, with tile progress.
    Rendering {
        /// Tiles completed.
        tiles_done: u32,
        /// Total tiles.
        tiles_total: u32,
    },
    /// A progressive intermediate is ready (§4.3 ladder).
    PreviewReady(RenderOutput),
    /// The render completed.
    Complete(RenderOutput),
    /// The render failed terminally.
    Failed(RenderError),
    /// The render was cancelled (or superseded latest-wins).
    Cancelled,
}

/// Broadcast events the shell subscribes to (spec §3.6 `events`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum EngineEvent {
    /// The wgpu device was lost.
    DeviceLost {
        /// The driver/backend reason.
        reason: String,
    },
    /// The engine degraded to preview-resolution CPU editing (§4.4).
    DegradedToCpu,
    /// The GPU path was re-enabled (prefs).
    GpuReenabled,
    /// VRAM pressure crossed a threshold.
    VramPressure {
        /// Bytes resident.
        used: u64,
        /// Byte budget.
        budget: u64,
    },
}

/// One tracked render: its latest state and the cooperative-cancel token the
/// worker checks at every tile boundary.
struct TicketEntry {
    state: RenderState,
    cancel: CancelToken,
}

/// A unit of work handed to the render worker.
struct Job {
    ticket: u64,
    req: RenderRequest,
    /// Ticket-scoped cancel (a child of the request's token; cancelling it never
    /// touches the caller's token).
    cancel: CancelToken,
}

/// Engine state shared with the render worker thread.
struct Shared {
    tickets: Mutex<HashMap<u64, TicketEntry>>,
    compiler: RecipeCompiler,
    executor: Executor,
    cache: NodeCache,
    #[allow(dead_code)] // consumed by src.decoded source injection at A-gpu merge
    source: Arc<dyn SourceProvider>,
    #[allow(dead_code)] // consumed by the GPU backend + device-lost recovery (A-gpu / E)
    device: Arc<dyn DeviceProvider>,
}

impl Shared {
    fn set_state(&self, ticket: u64, state: RenderState) {
        if let Ok(mut map) = self.tickets.lock() {
            if let Some(entry) = map.get_mut(&ticket) {
                entry.state = state;
            }
        }
    }

    /// Run one job to a terminal state.
    fn process(&self, job: Job) {
        let Job {
            ticket,
            req,
            cancel,
        } = job;

        if cancel.is_cancelled() {
            self.set_state(ticket, RenderState::Cancelled);
            return;
        }
        self.set_state(
            ticket,
            RenderState::Rendering {
                tiles_done: 0,
                tiles_total: 1,
            },
        );

        // Compile the recipe → typed DAG under the requested PV. The M1 compiler
        // is template-only, so a nominal SourceDesc suffices (real source extent
        // arrives via SourceProvider at the A-gpu merge).
        let src = SourceDesc {
            image: req.image,
            full_extent: Extent {
                w: req.roi.w.max(1),
                h: req.roi.h.max(1),
            },
            source_kind: SourceKind::Rgb,
            colorimetry: SourceColorimetry::default(),
        };
        let graph = match self.compiler.compile(&req.recipe, req.pv, &src) {
            Ok(g) => g,
            Err(e) => {
                self.set_state(ticket, RenderState::Failed(RenderError::Compile(e)));
                return;
            }
        };

        // Evaluate. A node panic (e.g. an A-gpu stub reached on the CPU-only
        // path) is caught and surfaced typed rather than aborting the worker.
        let eval = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.executor
                .evaluate(&graph, req.roi, req.scale, &self.cache, &cancel)
        }));
        let tile = match eval {
            Ok(Ok(tile)) => tile,
            Ok(Err(RenderError::Cancelled)) => {
                self.set_state(ticket, RenderState::Cancelled);
                return;
            }
            Ok(Err(e)) => {
                self.set_state(ticket, RenderState::Failed(e));
                return;
            }
            Err(_) => {
                self.set_state(
                    ticket,
                    RenderState::Failed(RenderError::Internal(
                        "a render node panicked during evaluation".to_owned(),
                    )),
                );
                return;
            }
        };

        let output = match self.finish(&req, tile) {
            Ok(o) => o,
            Err(e) => {
                self.set_state(ticket, RenderState::Failed(e));
                return;
            }
        };
        self.set_state(ticket, RenderState::Complete(output));
    }

    /// Turn a terminal tile into a [`RenderOutput`] for the request's target.
    fn finish(&self, req: &RenderRequest, tile: TileHandle) -> Result<RenderOutput, RenderError> {
        let backend = self.executor.backend_kind();
        let quality = OutputQuality::FullRes;
        let payload = match req.target {
            RenderTarget::Buffer { format } => OutputPayload::Pixels(readback(&tile, format)?),
            RenderTarget::Canvas => {
                // The engine-owned double-buffered canvas texture pair is wired
                // by A-gpu (task A13). The CPU-only A-core path renders to a
                // Buffer target; Canvas over CPU is a merge concern.
                return Err(RenderError::Internal(
                    "Canvas render target is wired by A-gpu (task A13); use a Buffer target on the CPU path"
                        .to_owned(),
                ));
            }
        };
        Ok(RenderOutput {
            payload,
            colorimetry: OutputColorimetry::default(),
            backend,
            pv: req.pv,
            quality,
        })
    }
}

/// The render engine (spec §3.6). Holds the registry-backed compiler, the tile
/// cache, the executor (over a pixel [`Backend`]), and a serial render worker.
///
/// A-core wires the **CPU** backend + Buffer readback end-to-end; A-gpu adds the
/// shared-device GPU backend, the canvas double-buffer (A13), and device-lost
/// recovery (E) at merge — the ticket lifecycle below is backend-agnostic.
pub struct Engine {
    shared: Arc<Shared>,
    next_ticket: AtomicU64,
    job_tx: Option<mpsc::Sender<Job>>,
    worker: Option<JoinHandle<()>>,
    events_tx: broadcast::Sender<EngineEvent>,
}

impl Engine {
    /// Construct an engine on the shell's shared device (via `dp`), pulling
    /// pixels through `sp`, dispatching `registry`'s nodes under `cfg`. The
    /// built-in PV1 template ([`GraphTemplate::pv1`]) is registered.
    pub fn new(
        dp: Arc<dyn DeviceProvider>,
        sp: Arc<dyn SourceProvider>,
        registry: NodeRegistry,
        cfg: EngineConfig,
    ) -> Result<Engine, EngineInitError> {
        let mut compiler = RecipeCompiler::with_registry(Arc::new(registry));
        compiler
            .register_template(lightbox_types::PV_M0, GraphTemplate::pv1())
            .map_err(|e| EngineInitError::Warmup(format!("PV1 template registration: {e}")))?;
        Engine::with_compiler(dp, sp, compiler, cfg)
    }

    /// Construct an engine over a fully-configured [`RecipeCompiler`] (registry
    /// and templates). Additive to the spec `new`, used by **D** (extra PVs)
    /// and by tests to drive probe-node graphs; `new` delegates here after
    /// wiring the built-in PV1 template.
    pub fn with_compiler(
        dp: Arc<dyn DeviceProvider>,
        sp: Arc<dyn SourceProvider>,
        compiler: RecipeCompiler,
        cfg: EngineConfig,
    ) -> Result<Engine, EngineInitError> {
        // A-core builds the CPU backend regardless of `BackendPref`; A-gpu adds
        // GPU-backend selection (device ctx from `dp`) at merge. `ForceCpu` is
        // already honored today.
        let _ = cfg.backend == BackendPref::Auto;
        let backend: Arc<dyn Backend> = Arc::new(CpuBackend::new(cfg.cpu_threads));
        let executor = Executor::with_probe(
            backend,
            cfg.tile_size,
            Arc::new(crate::ng::stats::RecomputeProbe::new()),
        );

        let shared = Arc::new(Shared {
            tickets: Mutex::new(HashMap::new()),
            compiler,
            executor,
            cache: NodeCache::new(),
            source: sp,
            device: dp,
        });

        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("lbx-render-worker".to_owned())
            .spawn(move || {
                // Exits when the Sender is dropped (Engine::drop).
                while let Ok(job) = job_rx.recv() {
                    worker_shared.process(job);
                }
            })
            .map_err(|e| EngineInitError::WorkerSpawn(e.to_string()))?;

        let (events_tx, _) = broadcast::channel(64);

        Ok(Engine {
            shared,
            next_ticket: AtomicU64::new(1),
            job_tx: Some(job_tx),
            worker: Some(worker),
            events_tx,
        })
    }

    /// Submit a render. Never blocks; validation errors surface via
    /// [`Engine::poll`] → `Failed` (spec §3.6).
    pub fn submit(&self, req: RenderRequest) -> RenderTicket {
        let id = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        // A ticket-scoped child token: `Engine::cancel` cancels it without
        // touching the caller's token.
        let cancel = req.cancel.child();
        if let Ok(mut map) = self.shared.tickets.lock() {
            map.insert(
                id,
                TicketEntry {
                    state: RenderState::Queued,
                    cancel: cancel.clone(),
                },
            );
        }
        let job = Job {
            ticket: id,
            req,
            cancel,
        };
        // If the worker is gone (shutting down), fail the ticket immediately.
        if let Some(tx) = &self.job_tx {
            if tx.send(job).is_err() {
                self.shared
                    .set_state(id, RenderState::Failed(RenderError::ShuttingDown));
            }
        } else {
            self.shared
                .set_state(id, RenderState::Failed(RenderError::ShuttingDown));
        }
        RenderTicket(id)
    }

    /// Snapshot a ticket's state (polled once per frame by the UI).
    pub fn poll(&self, t: &RenderTicket) -> RenderState {
        self.shared
            .tickets
            .lock()
            .ok()
            .and_then(|map| map.get(&t.0).map(|e| e.state.clone()))
            .unwrap_or(RenderState::Failed(RenderError::Internal(format!(
                "unknown render ticket {}",
                t.0
            ))))
    }

    /// Cooperatively cancel a ticket (tile-boundary latency).
    pub fn cancel(&self, t: &RenderTicket) {
        if let Ok(map) = self.shared.tickets.lock() {
            if let Some(entry) = map.get(&t.0) {
                entry.cancel.cancel();
            }
        }
    }

    /// Subscribe to engine events (device-lost / degraded / vram; spec §3.6).
    /// The channel exists here; **E** emits the events (device-lost / degrade).
    pub fn events(&self) -> broadcast::Receiver<EngineEvent> {
        self.events_tx.subscribe()
    }

    /// What the engine is currently rendering on.
    pub fn active_backend(&self) -> ActiveBackend {
        unimplemented!("E (E4): Engine::active_backend — GPU adapter / degrade state")
    }

    /// The process versions this engine can render (spec §3.6).
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        self.shared.compiler.supported_pvs()
    }

    /// Engine counters, including the recompute-count probe (spec §3.6).
    pub fn stats(&self) -> EngineStats {
        self.shared.executor.probe().snapshot()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Drop the sender so the worker's `recv` returns and the thread exits.
        self.job_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Read a terminal working tile back into a CPU [`PixelBuf`] of `format`
/// (Buffer target; spec §3.6). The pack is **linear** — display encoding
/// (sRGB/ICC) is the `xform.display` node's output, not the engine's job
/// (E02 guardrail); a PV1 graph's terminal is already `DisplayRgba8`.
fn readback(tile: &TileHandle, format: OutFormat) -> Result<PixelBuf, RenderError> {
    let src = tile.cpu().ok_or_else(|| {
        RenderError::Readback(
            "terminal tile has no CPU pixels (GPU readback is wired by A-gpu)".to_owned(),
        )
    })?;
    let target = match format {
        OutFormat::Rgba8Srgb => PixelFormat::Rgba8Srgb,
        // No 16-bit-unorm working format in the engine; the float tier carries
        // the same values losslessly for a 16-bit export tier.
        OutFormat::Rgba16 => PixelFormat::Rgba16F,
        OutFormat::Rgba32F => PixelFormat::Rgba32F,
    };
    if src.format == target {
        return Ok(src.clone());
    }
    let mut out = PixelBuf::new_zeroed(target, src.extent);
    for y in 0..src.extent.h {
        for x in 0..src.extent.w {
            out.set_rgba_f32(x, y, src.get_rgba_f32(x, y));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::node::param::ParamsSchema;
    use crate::ng::node::{
        CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamsSchemaRef, PvRange,
        RenderNode,
    };
    use crate::ng::source::DeviceHandles;
    use crate::ng::tile::{CpuTileView, TileView};
    use crate::ng::types::{NodeId, PortType};
    use crate::ng::{BoxFuture, DeviceError, NodeError, ParamBlock, PortDecl};
    use crate::ng::{SourceError, SourceImage, SourceWant};
    use lightbox_types::PV_M0;
    use std::time::{Duration, Instant};

    // ── seam stubs (never called on the CPU-only path) ──────────────────────
    struct NullDevice;
    impl DeviceProvider for NullDevice {
        fn current(&self) -> DeviceHandles {
            unimplemented!("no GPU device on the CPU-only A-core path")
        }
        fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
            Box::pin(async { Err(DeviceError::Rebuild("no device in test".to_owned())) })
        }
    }
    struct NullSource;
    impl SourceProvider for NullSource {
        fn fetch(
            &self,
            _: ImageId,
            _: SourceWant,
            _: &CancelToken,
        ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
            Box::pin(async { Err(SourceError::NotFound) })
        }
    }

    // ── probe nodes: a constant source + a gain multiply ────────────────────
    static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
    static CONST_DESC: NodeDescriptor = NodeDescriptor {
        id: NodeId("test.const"),
        inputs: &[],
        output: PortDecl {
            name: "out",
            ty: PortType::LinearRgbaF16,
        },
        params_schema: ParamsSchemaRef(&SCHEMA),
    };
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
    static BLOCK_DESC: NodeDescriptor = NodeDescriptor {
        id: NodeId("test.block"),
        inputs: &[],
        output: PortDecl {
            name: "out",
            ty: PortType::LinearRgbaF16,
        },
        params_schema: ParamsSchemaRef(&SCHEMA),
    };

    struct ConstSource;
    impl RenderNode for ConstSource {
        fn descriptor(&self) -> &NodeDescriptor {
            &CONST_DESC
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
                    out.set_rgba_f32(x, y, [0.25, 0.5, 0.75, 1.0]);
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
            let gain = params.get_f64_or("gain", 2.0) as f32;
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

    /// A source node that spins until cancelled — proves cancel reaches a tile
    /// boundary.
    struct BlockUntilCancelled;
    impl RenderNode for BlockUntilCancelled {
        fn descriptor(&self) -> &NodeDescriptor {
            &BLOCK_DESC
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
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ctx.cancel.is_cancelled() {
                if Instant::now() > deadline {
                    return Err(NodeError::Other(
                        "block node timed out (cancel never arrived)".to_owned(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(NodeError::Cancelled)
        }
    }

    macro_rules! factory {
        ($name:ident, $node:ty, $salt:literal) => {
            struct $name;
            impl NodeFactory for $name {
                fn instantiate(&self) -> Arc<dyn RenderNode> {
                    Arc::new(<$node>::new_boxed())
                }
                fn kernel_salt(&self) -> KernelSalt {
                    KernelSalt(blake3::hash($salt))
                }
            }
        };
    }
    impl ConstSource {
        fn new_boxed() -> ConstSource {
            ConstSource
        }
    }
    impl Gain {
        fn new_boxed() -> Gain {
            Gain
        }
    }
    impl BlockUntilCancelled {
        fn new_boxed() -> BlockUntilCancelled {
            BlockUntilCancelled
        }
    }
    factory!(ConstFactory, ConstSource, b"const");
    factory!(GainFactory, Gain, b"gain");
    factory!(BlockFactory, BlockUntilCancelled, b"block");

    fn engine_with(template: GraphTemplate, extra: &[(NodeId, Arc<dyn NodeFactory>)]) -> Engine {
        let mut reg = NodeRegistry::new();
        for (id, f) in extra {
            reg.register(*id, PvRange::from_open(PV_M0), Arc::clone(f))
                .unwrap();
        }
        let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
        compiler.register_template(PV_M0, template).unwrap();
        Engine::with_compiler(
            Arc::new(NullDevice),
            Arc::new(NullSource),
            compiler,
            EngineConfig::default(),
        )
        .unwrap()
    }

    fn request(pv: ProcessVersion) -> RenderRequest {
        RenderRequest {
            image: ImageId(1),
            recipe: Recipe::identity(pv),
            pv,
            roi: Roi {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            scale: RenderScale::OneToOne,
            target: RenderTarget::Buffer {
                format: OutFormat::Rgba8Srgb,
            },
            priority: RenderPriority::Interactive,
            cancel: CancelToken::new(),
        }
    }

    fn poll_until<F>(engine: &Engine, ticket: &RenderTicket, pred: F) -> RenderState
    where
        F: Fn(&RenderState) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = engine.poll(ticket);
            if pred(&state) || Instant::now() > deadline {
                return state;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn submit_is_fast_and_walks_to_complete() {
        let engine = engine_with(
            GraphTemplate::linear(vec![NodeId("test.const"), NodeId("test.gain")]),
            &[
                (
                    NodeId("test.const"),
                    Arc::new(ConstFactory) as Arc<dyn NodeFactory>,
                ),
                (
                    NodeId("test.gain"),
                    Arc::new(GainFactory) as Arc<dyn NodeFactory>,
                ),
            ],
        );

        let t0 = Instant::now();
        let ticket = engine.submit(request(PV_M0));
        // submit must not block (well under a millisecond of bookkeeping).
        assert!(t0.elapsed() < Duration::from_millis(50));

        let state = poll_until(&engine, &ticket, |s| {
            matches!(s, RenderState::Complete(_) | RenderState::Failed(_))
        });
        let RenderState::Complete(out) = state else {
            panic!("expected Complete, got {state:?}");
        };
        assert_eq!(out.backend, BackendId::Cpu);
        assert_eq!(out.pv, PV_M0);
        let OutputPayload::Pixels(px) = out.payload else {
            panic!("expected Pixels payload");
        };
        // const 0.25/0.5/0.75 * gain 2.0 = 0.5/1.0/1.0(clamped) → 128/255/255.
        assert_eq!(px.extent, Extent { w: 8, h: 8 });
        let p = px.get_rgba_f32(0, 0);
        assert!((p[0] - 128.0 / 255.0).abs() < 0.01, "r={}", p[0]);
        assert!(p[1] > 0.99); // clamped to 1.0
                              // Two nodes evaluated (const + gain), observable via the recompute probe.
        assert_eq!(engine.stats().nodes_evaluated, 2);
    }

    #[test]
    fn cancel_mid_render_yields_cancelled() {
        let engine = engine_with(
            GraphTemplate::linear(vec![NodeId("test.block")]),
            &[(
                NodeId("test.block"),
                Arc::new(BlockFactory) as Arc<dyn NodeFactory>,
            )],
        );
        let ticket = engine.submit(request(PV_M0));
        // Wait until the worker is actually rendering (the block node spinning).
        let rendering = poll_until(&engine, &ticket, |s| {
            matches!(s, RenderState::Rendering { .. })
        });
        assert!(matches!(rendering, RenderState::Rendering { .. }));

        engine.cancel(&ticket);
        let state = poll_until(&engine, &ticket, |s| {
            matches!(
                s,
                RenderState::Cancelled | RenderState::Complete(_) | RenderState::Failed(_)
            )
        });
        assert!(matches!(state, RenderState::Cancelled), "got {state:?}");
    }

    #[test]
    fn unsupported_pv_fails_typed_via_poll() {
        let engine = engine_with(
            GraphTemplate::linear(vec![NodeId("test.const")]),
            &[(
                NodeId("test.const"),
                Arc::new(ConstFactory) as Arc<dyn NodeFactory>,
            )],
        );
        let ticket = engine.submit(request(ProcessVersion(99)));
        let state = poll_until(&engine, &ticket, |s| matches!(s, RenderState::Failed(_)));
        assert!(matches!(
            state,
            RenderState::Failed(RenderError::Compile(
                crate::ng::error::CompileError::UnsupportedPv(_)
            ))
        ));
    }

    #[test]
    fn poll_of_unknown_ticket_is_failed() {
        let engine = engine_with(
            GraphTemplate::linear(vec![NodeId("test.const")]),
            &[(
                NodeId("test.const"),
                Arc::new(ConstFactory) as Arc<dyn NodeFactory>,
            )],
        );
        assert!(matches!(
            engine.poll(&RenderTicket(9999)),
            RenderState::Failed(RenderError::Internal(_))
        ));
    }
}
