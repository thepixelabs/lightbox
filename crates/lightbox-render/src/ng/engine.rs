// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The engine lifecycle + public request/output DTOs (spec §3.6).
//!
//! Owner: **A-core** (task **A12** submit/poll/cancel + ticket store + render
//! thread; **A1** config/errors). `events`/`active_backend` are wired by **E**
//! (device-lost / degradation). This is the surface **F5** promotes to the
//! crate root (replacing the E01 seed).

use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;
use tokio::sync::broadcast;

use crate::ng::colorimetry::OutputColorimetry;
use crate::ng::config::EngineConfig;
use crate::ng::error::{EngineInitError, RenderError};
use crate::ng::node::NodeRegistry;
use crate::ng::source::{DeviceProvider, SourceProvider};
use crate::ng::stats::EngineStats;
use crate::ng::tile::PixelBuf;
use crate::ng::types::{ProcessVersion, RenderScale, Roi};

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

/// The render engine (spec §3.6). Holds the device ctx, registry, compiler,
/// cache, executor, and recovery state machine. **A-core fills the internals.**
pub struct Engine {}

impl Engine {
    /// Construct an engine on the shell's shared device (via `dp`), pulling
    /// pixels through `sp`, dispatching `registry`'s nodes under `cfg`.
    pub fn new(
        dp: Arc<dyn DeviceProvider>,
        sp: Arc<dyn SourceProvider>,
        registry: NodeRegistry,
        cfg: EngineConfig,
    ) -> Result<Engine, EngineInitError> {
        let _ = (dp, sp, registry, cfg);
        unimplemented!("A12 (A-core): Engine::new — device ctx + executor + cache + recover sm")
    }

    /// Submit a render. Never blocks; validation errors surface via
    /// [`Engine::poll`] → `Failed` (spec §3.6).
    pub fn submit(&self, req: RenderRequest) -> RenderTicket {
        let _ = req;
        unimplemented!("A12 (A-core): Engine::submit — ticket store + render thread")
    }

    /// Snapshot a ticket's state (polled once per frame by the UI).
    pub fn poll(&self, t: &RenderTicket) -> RenderState {
        let _ = t;
        unimplemented!("A12 (A-core): Engine::poll")
    }

    /// Cooperatively cancel a ticket (tile-boundary latency).
    pub fn cancel(&self, t: &RenderTicket) {
        let _ = t;
        unimplemented!("A12 (A-core): Engine::cancel")
    }

    /// Subscribe to engine events (device-lost / degraded / vram; spec §3.6).
    pub fn events(&self) -> broadcast::Receiver<EngineEvent> {
        unimplemented!("E (E1): Engine::events — device-lost / degraded broadcast")
    }

    /// What the engine is currently rendering on.
    pub fn active_backend(&self) -> ActiveBackend {
        unimplemented!("E (E4): Engine::active_backend")
    }

    /// The process versions this engine can render (spec §3.6).
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        unimplemented!("D1 (D): Engine::supported_pvs")
    }

    /// Engine counters, including the recompute-count probe (spec §3.6).
    pub fn stats(&self) -> EngineStats {
        unimplemented!("A15 (A-core): Engine::stats")
    }
}
