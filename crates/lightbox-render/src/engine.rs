// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The Engine seed (spec §3.4): `submit`/`poll`/`cancel` ticket lifecycle with
//! per-viewport latest-wins coalescing, on a dedicated render worker thread.
//!
//! Frozen surface (E05.1's starting point): `Engine::new/submit/poll/cancel/
//! on_device_lost/backend_kind`, [`RenderRequest`], [`RenderState`],
//! [`RenderOutput`], [`ViewportId`]. The single-node evaluation internals
//! (worker loop, [`crate::RenderPlanner`], upload/readback helpers) are M0
//! scaffolding E05 replaces with the DAG scheduler.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock, Weak};

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_types::{ImageId, ProcessVersion};

use crate::error::{DeviceLostReason, EngineError, NodeError, RenderError};
use crate::gpu::GpuContext;
use crate::node::{CpuCtx, GpuCtx, ImageBufU8, NodeRegistry, Tile, TileCpu};
use crate::planner::{RenderPlanner, UnconfiguredPlanner};
use crate::pool::TexturePool;
use crate::source::{SourceImage, SourcePixelFormat, SourceResolver};

/// Identifies the coalescing bucket: the render scheduler keeps at most one
/// in-flight request per viewport, latest-wins (spec §3.4, §4.3).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ViewportId(pub u64);

/// Region of interest. M0: always [`Roi::Full`]; E05.3 adds tiled ROIs.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Roi {
    /// The whole image.
    #[default]
    Full,
}

/// Output scale of a render (spec §3.4). M0: fit-within or native.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RenderScale {
    /// Scale to fit within `w`×`h`, preserving aspect ratio.
    FitWithin {
        /// Max output width, pixels.
        w: u32,
        /// Max output height, pixels.
        h: u32,
    },
    /// The source's native resolution.
    Native,
}

/// Where the output lands (spec §3.4).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RenderTarget {
    /// A wgpu texture on the shared device (the shell's zero-copy path).
    Texture,
    /// An RGBA8 sRGB CPU buffer (CLI / export / tests).
    CpuBuffer,
}

/// A render request (spec §3.4 — §2.2's shape plus `viewport`).
#[derive(Clone, Debug)]
pub struct RenderRequest {
    /// The image to render.
    pub image: ImageId,
    /// The edit recipe (M0: `Recipe::identity` placeholder, spec §3.9).
    pub recipe: Recipe,
    /// The process version to evaluate under.
    pub pv: ProcessVersion,
    /// Region of interest. M0: `Roi::Full`.
    pub roi: Roi,
    /// Output scale.
    pub scale: RenderScale,
    /// Where the output lands.
    pub target: RenderTarget,
    /// Coalescing key — newest request per viewport wins.
    pub viewport: ViewportId,
}

/// Opaque handle to a submitted render (spec §3.4). Cheap to clone.
#[derive(Clone, Debug)]
pub struct RenderTicket {
    id: u64,
}

/// Lifecycle state of a ticket, snapshot per [`Engine::poll`] (spec §3.4).
#[derive(Clone, Debug)]
pub enum RenderState {
    /// Queued, not yet picked up by the worker.
    Pending,
    /// Evaluating.
    Running,
    /// Done; the output is yours to keep (hold the `Arc`s for as long as the
    /// pixels must stay alive — the texture pool recycles unreferenced ones).
    Ready(RenderOutput),
    /// Failed terminally.
    Failed(RenderError),
    /// Cancelled via [`Engine::cancel`] before or during evaluation.
    Cancelled,
    /// A newer request on the same viewport won (latest-wins coalescing).
    /// Also reported for tickets old enough to have been evicted.
    Superseded,
}

/// A finished render's pixels (spec §3.4).
#[derive(Clone)]
pub enum RenderOutput {
    /// Output texture on the shared device — composited zero-copy by the
    /// shell (`egui_wgpu::Renderer::register_native_texture`).
    Texture {
        /// The texture. Holding this `Arc` keeps it from being recycled.
        tex: Arc<wgpu::Texture>,
        /// Default view of `tex`.
        view: Arc<wgpu::TextureView>,
        /// Size in pixels.
        size: [u32; 2],
    },
    /// RGBA8 sRGB CPU pixels, for CLI/tests.
    Cpu(ImageBufU8),
}

impl std::fmt::Debug for RenderOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderOutput::Texture { size, .. } => f
                .debug_struct("RenderOutput::Texture")
                .field("size", size)
                .finish_non_exhaustive(),
            RenderOutput::Cpu(buf) => f.debug_tuple("RenderOutput::Cpu").field(buf).finish(),
        }
    }
}

/// What the engine renders on (spec §3.4 `backend_kind`).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BackendKind {
    /// GPU via wgpu on the given backend.
    Gpu(wgpu::Backend),
    /// No adapter — CPU node evaluation only.
    CpuOnly,
}

/// Terminal entries retained for late polls before eviction (evicted tickets
/// read as [`RenderState::Superseded`]).
const MAX_TERMINAL_RETAINED: usize = 256;

/// A registered device-lost observer (spec §3.4 `on_device_lost`).
type DeviceLostCallback = Box<dyn Fn(DeviceLostReason) + Send + Sync>;

enum WorkerMsg {
    Job(u64),
    Stop,
}

struct TicketEntry {
    state: RenderState,
    /// Taken by the worker when evaluation starts.
    request: Option<RenderRequest>,
    viewport: ViewportId,
    cancel: CancelToken,
    /// Set by [`Engine::cancel`]; wins over `superseded` for the final state.
    cancel_requested: bool,
    /// Set when a newer submit landed on the same viewport.
    superseded: bool,
}

impl TicketEntry {
    fn is_terminal(&self) -> bool {
        !matches!(self.state, RenderState::Pending | RenderState::Running)
    }
}

#[derive(Default)]
struct TicketTable {
    next_id: u64,
    entries: HashMap<u64, TicketEntry>,
    latest_per_viewport: HashMap<ViewportId, u64>,
    /// Terminal ids in completion order, for bounded retention. May contain
    /// ids already removed; eviction skips those.
    terminal_order: std::collections::VecDeque<u64>,
}

impl TicketTable {
    /// Records a terminal transition and evicts the oldest terminal entries
    /// beyond the retention cap.
    fn note_terminal(&mut self, id: u64) {
        self.terminal_order.push_back(id);
        while self.terminal_order.len() > MAX_TERMINAL_RETAINED {
            let Some(old) = self.terminal_order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.get(&old) {
                if entry.is_terminal() {
                    self.entries.remove(&old);
                }
            }
        }
    }
}

struct EngineShared {
    gpu: Option<GpuContext>,
    pool: Option<TexturePool>,
    registry: NodeRegistry,
    sources: Arc<dyn SourceResolver>,
    planner: RwLock<Arc<dyn RenderPlanner>>,
    tickets: Mutex<TicketTable>,
    device_lost: AtomicBool,
    device_lost_cbs: Mutex<Vec<DeviceLostCallback>>,
}

/// The render engine seed (spec §3.4, frozen surface).
pub struct Engine {
    shared: Arc<EngineShared>,
    tx: mpsc::Sender<WorkerMsg>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Engine {
    /// Shell path: pass the shell's shared [`GpuContext`]. Headless path:
    /// pass [`GpuContext::headless()`]'s result — `None` runs CPU-only
    /// (CI software-adapter fallback; spec §3.4).
    pub fn new(
        gpu: Option<GpuContext>,
        registry: NodeRegistry,
        sources: Arc<dyn SourceResolver>,
    ) -> Result<Engine, EngineError> {
        let pool = gpu
            .as_ref()
            .map(|g| TexturePool::new(Arc::clone(&g.device)));
        let shared = Arc::new(EngineShared {
            gpu,
            pool,
            registry,
            sources,
            planner: RwLock::new(Arc::new(UnconfiguredPlanner)),
            tickets: Mutex::new(TicketTable::default()),
            device_lost: AtomicBool::new(false),
            device_lost_cbs: Mutex::new(Vec::new()),
        });

        // M0 device-lost behavior (spec §3.4): registered but degenerate —
        // fail in-flight tickets + fire callbacks. E05.5 owns recovery.
        if let Some(gpu) = &shared.gpu {
            let weak: Weak<EngineShared> = Arc::downgrade(&shared);
            gpu.device.set_device_lost_callback(move |reason, message| {
                if let Some(shared) = weak.upgrade() {
                    shared.handle_device_lost(reason, message);
                }
            });
        }

        let (tx, rx) = mpsc::channel::<WorkerMsg>();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("lightbox-render".to_owned())
            .spawn(move || worker_loop(&worker_shared, &rx))
            .map_err(|e| EngineError::WorkerSpawn(e.to_string()))?;

        Ok(Engine {
            shared,
            tx,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Submits a render. Asynchronous — never blocks (spec §3.4). Any older
    /// non-terminal ticket on the same viewport is superseded (latest-wins).
    pub fn submit(&self, req: RenderRequest) -> RenderTicket {
        let viewport = req.viewport;
        let mut table = self.shared.tickets.lock().expect("ticket table poisoned");
        let id = table.next_id;
        table.next_id += 1;

        // Latest-wins: supersede the previous request on this viewport.
        if let Some(&prev) = table.latest_per_viewport.get(&viewport) {
            if let Some(entry) = table.entries.get_mut(&prev) {
                match entry.state {
                    RenderState::Pending => {
                        entry.superseded = true;
                        entry.state = RenderState::Superseded;
                        entry.cancel.cancel();
                        table.note_terminal(prev);
                    }
                    RenderState::Running => {
                        entry.superseded = true;
                        entry.cancel.cancel();
                    }
                    _ => {}
                }
            }
        }

        // Drop already-terminal entries of this viewport promptly so their
        // output textures return to the pool (holders keep their own Arcs).
        table
            .entries
            .retain(|_, e| !(e.viewport == viewport && e.is_terminal()));

        table.latest_per_viewport.insert(viewport, id);
        table.entries.insert(
            id,
            TicketEntry {
                state: RenderState::Pending,
                request: Some(req),
                viewport,
                cancel: CancelToken::new(),
                cancel_requested: false,
                superseded: false,
            },
        );
        drop(table);

        if self.tx.send(WorkerMsg::Job(id)).is_err() {
            // Worker gone (shutdown race): fail the ticket.
            let mut table = self.shared.tickets.lock().expect("ticket table poisoned");
            if let Some(entry) = table.entries.get_mut(&id) {
                entry.state = RenderState::Failed(RenderError::ShuttingDown);
                table.note_terminal(id);
            }
        }

        RenderTicket { id }
    }

    /// Snapshot of the ticket's state — the UI polls this once per frame
    /// (spec §3.4). Evicted (very old) tickets read as `Superseded`.
    pub fn poll(&self, ticket: &RenderTicket) -> RenderState {
        let table = self.shared.tickets.lock().expect("ticket table poisoned");
        table
            .entries
            .get(&ticket.id)
            .map(|e| e.state.clone())
            .unwrap_or(RenderState::Superseded)
    }

    /// Cancels the ticket: a pending ticket becomes `Cancelled` immediately
    /// and never evaluates; a running one has its token cancelled and
    /// finishes as `Cancelled`. Terminal tickets are unaffected.
    pub fn cancel(&self, ticket: &RenderTicket) {
        let mut table = self.shared.tickets.lock().expect("ticket table poisoned");
        if let Some(entry) = table.entries.get_mut(&ticket.id) {
            match entry.state {
                RenderState::Pending => {
                    entry.cancel_requested = true;
                    entry.state = RenderState::Cancelled;
                    entry.cancel.cancel();
                    table.note_terminal(ticket.id);
                }
                RenderState::Running => {
                    entry.cancel_requested = true;
                    entry.cancel.cancel();
                }
                _ => {}
            }
        }
    }

    /// Registers a device-lost callback. M0: registered but degenerate —
    /// in-flight tickets fail and callbacks fire; the full rebuild/CPU-degrade
    /// harness is E05.5 (spec §3.4).
    pub fn on_device_lost(&self, cb: Box<dyn Fn(DeviceLostReason) + Send + Sync>) {
        self.shared
            .device_lost_cbs
            .lock()
            .expect("device-lost callbacks poisoned")
            .push(cb);
    }

    /// What this engine renders on.
    pub fn backend_kind(&self) -> BackendKind {
        match &self.shared.gpu {
            Some(gpu) => BackendKind::Gpu(gpu.backend),
            None => BackendKind::CpuOnly,
        }
    }

    /// The shared GPU context, when running on a GPU. The shell asserts
    /// device identity against this (T7 seam proof).
    pub fn gpu(&self) -> Option<&GpuContext> {
        self.shared.gpu.as_ref()
    }

    /// Installs the M0 single-node planner (scaffolding — see
    /// [`crate::planner`]; E05.1 replaces this with recipe-driven DAG
    /// construction). Takes effect for subsequently evaluated tickets.
    pub fn set_planner(&self, planner: Arc<dyn RenderPlanner>) {
        *self.shared.planner.write().expect("planner lock poisoned") = planner;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerMsg::Stop);
        if let Some(worker) = self.worker.lock().expect("worker handle poisoned").take() {
            let _ = worker.join();
        }
    }
}

impl EngineShared {
    fn handle_device_lost(&self, reason: wgpu::DeviceLostReason, message: String) {
        tracing::error!(
            target: "lightbox_render::engine",
            ?reason,
            message,
            "wgpu device lost — failing in-flight tickets (M0 degenerate behavior; recovery is E05.5)"
        );
        self.device_lost.store(true, Ordering::Release);

        let failed_ids: Vec<u64> = {
            let mut table = self.tickets.lock().expect("ticket table poisoned");
            let ids: Vec<u64> = table
                .entries
                .iter()
                .filter(|(_, e)| !e.is_terminal())
                .map(|(&id, _)| id)
                .collect();
            for &id in &ids {
                if let Some(entry) = table.entries.get_mut(&id) {
                    entry.state = RenderState::Failed(RenderError::DeviceLost(message.clone()));
                    entry.cancel.cancel();
                }
            }
            for &id in &ids {
                table.note_terminal(id);
            }
            ids
        };
        tracing::debug!(target: "lightbox_render::engine", count = failed_ids.len(), "tickets failed by device loss");

        let cb_reason = match reason {
            wgpu::DeviceLostReason::Destroyed => DeviceLostReason::Destroyed(message),
            _ => DeviceLostReason::Unknown(message),
        };
        for cb in self
            .device_lost_cbs
            .lock()
            .expect("device-lost callbacks poisoned")
            .iter()
        {
            cb(cb_reason.clone());
        }
    }
}

fn worker_loop(shared: &Arc<EngineShared>, rx: &mpsc::Receiver<WorkerMsg>) {
    while let Ok(msg) = rx.recv() {
        let id = match msg {
            WorkerMsg::Job(id) => id,
            WorkerMsg::Stop => break,
        };

        // Claim the job; skip tickets already terminal (cancelled/superseded
        // before we got here — T6: no eval runs for those).
        let (req, cancel) = {
            let mut table = shared.tickets.lock().expect("ticket table poisoned");
            let Some(entry) = table.entries.get_mut(&id) else {
                continue;
            };
            if !matches!(entry.state, RenderState::Pending) {
                continue;
            }
            entry.state = RenderState::Running;
            let req = entry
                .request
                .take()
                .expect("pending ticket lost its request");
            (req, entry.cancel.clone())
        };

        let result = evaluate(shared, &req, &cancel);

        let mut table = shared.tickets.lock().expect("ticket table poisoned");
        let Some(entry) = table.entries.get_mut(&id) else {
            continue;
        };
        if entry.is_terminal() {
            continue; // device-lost handler beat us to it
        }
        entry.state = if entry.cancel_requested {
            RenderState::Cancelled
        } else if entry.superseded {
            RenderState::Superseded
        } else {
            match result {
                Ok(output) => RenderState::Ready(output),
                Err(err) => {
                    tracing::warn!(target: "lightbox_render::engine", ticket = id, %err, "render failed");
                    RenderState::Failed(err)
                }
            }
        };
        table.note_terminal(id);
    }
}

fn evaluate(
    shared: &EngineShared,
    req: &RenderRequest,
    cancel: &CancelToken,
) -> Result<RenderOutput, RenderError> {
    if shared.device_lost.load(Ordering::Acquire) {
        return Err(RenderError::DeviceLost("device previously lost".into()));
    }

    let planner = Arc::clone(&shared.planner.read().expect("planner lock poisoned"));
    let plan = planner.plan(req, shared.sources.as_ref(), cancel)?;
    if cancel.is_cancelled() {
        // Final state comes from the cancel/supersede flags; the error is a
        // placeholder that never surfaces.
        return Err(RenderError::Node(NodeError::Cancelled.to_string()));
    }

    let node = shared
        .registry
        .get(plan.node, req.pv)
        .ok_or(RenderError::NodeNotRegistered {
            node: plan.node,
            pv: req.pv,
        })?;

    match &shared.gpu {
        Some(gpu) => {
            let pool = shared.pool.as_ref().expect("GPU engine without pool");
            let inputs: Vec<Tile> = match &plan.source {
                Some(src) => vec![upload_source(gpu, src)],
                None => Vec::new(),
            };
            let ctx = GpuCtx { gpu, pool, cancel };
            let out = node
                .eval_gpu(&ctx, &inputs, &plan.params)
                .map_err(|e| RenderError::Node(e.to_string()))?;
            match req.target {
                RenderTarget::Texture => Ok(RenderOutput::Texture {
                    view: Arc::clone(&out.view),
                    size: out.extent,
                    tex: out.texture,
                }),
                RenderTarget::CpuBuffer => read_back(gpu, &out).map(RenderOutput::Cpu),
            }
        }
        None => {
            if req.target == RenderTarget::Texture {
                return Err(RenderError::NoGpu);
            }
            let inputs: Vec<TileCpu> = match &plan.source {
                Some(src) => vec![source_to_tile_cpu(src)],
                None => Vec::new(),
            };
            let ctx = CpuCtx { cancel };
            let out = node
                .eval_cpu(&ctx, &inputs, &plan.params)
                .map_err(|e| RenderError::Node(e.to_string()))?;
            Ok(RenderOutput::Cpu(out.buf))
        }
    }
}

/// Uploads source pixels into an input texture (tile 0). Input textures are
/// created ad hoc at M0; E05's tiling owns input pooling.
fn upload_source(gpu: &GpuContext, src: &SourceImage) -> Tile {
    debug_assert_eq!(src.format, SourcePixelFormat::Rgba8Srgb);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lightbox engine source"),
        size: wgpu::Extent3d {
            width: src.width,
            height: src.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &src.px,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * src.width),
            rows_per_image: Some(src.height),
        },
        wgpu::Extent3d {
            width: src.width,
            height: src.height,
            depth_or_array_layers: 1,
        },
    );
    let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
    Tile {
        texture: Arc::new(texture),
        view,
        offset: [0, 0],
        extent: [src.width, src.height],
    }
}

fn source_to_tile_cpu(src: &SourceImage) -> TileCpu {
    TileCpu {
        buf: ImageBufU8 {
            px: src.px.to_vec(),
            width: src.width,
            height: src.height,
        },
        offset: [0, 0],
    }
}

/// GPU→CPU readback for `RenderTarget::CpuBuffer` (CLI/tests path). This is
/// the ONLY place in the workspace allowed to map GPU memory back — the shell
/// frame path never does (T7 acceptance criterion).
fn read_back(gpu: &GpuContext, tile: &Tile) -> Result<ImageBufU8, RenderError> {
    let [w, h] = tile.extent;
    let bytes_per_row = 4 * w;
    let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lightbox engine readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lightbox engine readback"),
        });
    encoder.copy_texture_to_buffer(
        tile.texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Readback(format!("device poll: {e:?}")))?;
    rx.recv()
        .map_err(|_| RenderError::Readback("map_async callback dropped".into()))?
        .map_err(|e| RenderError::Readback(format!("map_async: {e}")))?;

    let mut px = Vec::with_capacity((bytes_per_row as usize) * (h as usize));
    {
        let data = buffer.slice(..).get_mapped_range();
        for row in 0..h {
            let start = (row as usize) * (padded as usize);
            px.extend_from_slice(&data[start..start + bytes_per_row as usize]);
        }
    }
    buffer.unmap();

    Ok(ImageBufU8 {
        px,
        width: w,
        height: h,
    })
}
