// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The engine lifecycle + public request/output DTOs (spec §3.6).
//!
//! Owner: **A-core** (task **A12** submit/poll/cancel + ticket store + render
//! thread; **A1** config/errors). `events`/`active_backend` are wired by **E**
//! (device-lost / degradation). This is the surface **F5** promotes to the
//! crate root (replacing the E01 seed).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;
use tokio::sync::broadcast;

use crate::ng::cache::{CacheKey, CacheKeyInputs, NodeCache, PinLabel};
use crate::ng::colorimetry::{OutputColorimetry, SourceColorimetry, SourceKind};
use crate::ng::compile::{GraphTemplate, RecipeCompiler, SourceDesc};
use crate::ng::config::{BackendPref, EngineConfig, VramBudget};
use crate::ng::error::{EngineInitError, RenderError};
use crate::ng::exec::cpu::CpuBackend;
use crate::ng::exec::gpu::{readback_tile, GpuBackend};
use crate::ng::exec::{Backend, Executor, SourceInject};
use crate::ng::gpu::DeviceCtx;
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::node::NodeRegistry;
use crate::ng::nodes::decoded::SrcDecodedNode;
use crate::ng::nodes::resize::decimate_box_cpu;
use crate::ng::recover::{DegradeState, RecoverStateMachine};
use crate::ng::sched::canvas::CanvasPublisher;
use crate::ng::source::{DeviceProvider, SourceImage, SourceProvider, SourceWant, Uploader};
use crate::ng::stats::{EngineStats, RecomputeProbe};
use crate::ng::tile::{PixelBuf, PixelFormat, TileHandle};
use crate::ng::types::{Extent, ProcessVersion, RenderScale, Roi, TileCoord, TilePrecision};

/// Which backend produced an output, provenance stamped on every
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

/// Output fidelity tier, badge-able by the shell (spec §3.6).
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

/// The live pixel backend + its GPU device context. Swapped at runtime on
/// device-lost rebuild (GPU → fresh GPU) and on degradation (GPU → CPU), the
/// executor is rebuilt per render around whichever backend is live, so the swap
/// needs no executor surgery (the frozen `exec` seam is untouched; task E owns
/// this wiring).
struct LiveBackend {
    /// The backend the next render evaluates on.
    backend: Arc<dyn Backend>,
    /// The GPU device context on the GPU path (source upload + terminal
    /// readback); `None` on the CPU path (`ForceCpu` or degraded-to-CPU).
    gpu: Option<Arc<DeviceCtx>>,
    /// `true` between a device-lost signal and a successful rebuild, submissions
    /// refuse typed until recovery (export-safety, task E8).
    lost: bool,
}

/// Engine state shared with the render worker thread + the device-lost callback.
struct Shared {
    tickets: Mutex<HashMap<u64, TicketEntry>>,
    compiler: RecipeCompiler,
    cache: NodeCache,
    source: Arc<dyn SourceProvider>,
    device: Arc<dyn DeviceProvider>,
    /// The live (swappable) backend + device context (device-lost recovery, E).
    live: RwLock<LiveBackend>,
    /// The shared recompute probe (snapshot by [`Engine::stats`]), threaded into
    /// each per-render [`Executor`].
    probe: Arc<crate::ng::stats::RecomputeProbe>,
    /// Tile edge for the per-render executor.
    tile_size: u32,
    /// rayon pool size for the CPU path (used when degrading to CPU).
    cpu_threads: Option<usize>,
    /// The configured backend preference, distinguishes a deliberate `ForceCpu`
    /// engine from a GPU engine degraded to CPU (the E7 clamp applies only to the
    /// latter).
    backend_pref: BackendPref,
    /// Device-lost degradation policy state machine (task E4).
    recover: RecoverStateMachine,
    /// Broadcasts device-lost / degraded / re-enable events to the shell (§3.6).
    events_tx: broadcast::Sender<EngineEvent>,
    /// Monotonic device generation. A device-lost callback captured at install
    /// time carries the generation of the device it belongs to; a signal from a
    /// superseded (rebuilt-past) device is ignored, so deliberately destroying
    /// the old device during a rebuild never spuriously re-triggers loss.
    device_gen: AtomicU64,
    /// RAM-pinned decoded source keyed by image (`PinLabel::SourceStage`
    /// semantics). Host memory, so it survives GPU cache eviction and device
    /// loss: a post-rebuild render re-uploads from it with **zero**
    /// `SourceProvider::fetch` (re-warm, task E3).
    source_pin: Mutex<HashMap<ImageId, Arc<SourceImage>>>,
    /// The shell's canvas double-buffer publisher (task **F5** M1
    /// integration; installed via [`Engine::install_canvas`]). `None` until
    /// the [`crate::ng::RenderScheduler`] wires one in, a `Canvas` target
    /// request without an installed publisher fails typed rather than
    /// panicking (headless/CLI/test engines never install one and only ever
    /// submit `Buffer` targets).
    canvas: RwLock<Option<Arc<CanvasPublisher>>>,
}

/// How a render is delivered given the current degrade state (task E7).
#[derive(Clone, Copy, Debug)]
struct RenderPlan {
    /// The fidelity badge stamped on the output.
    quality: OutputQuality,
    /// Whether the terminal tile is halved to a preview footprint (degraded +
    /// Interactive + full-res, the "no silent full-res on the interactive path"
    /// clause).
    preview_halve: bool,
}

impl Shared {
    /// Set a ticket's state, but never overwrite a **terminal** state (Complete /
    /// Failed / Cancelled). This makes a device-lost failure (task E1) stick: if
    /// the worker later reports the same ticket as Cancelled/Complete it is
    /// dropped, so an in-flight ticket resolves `Failed(DeviceLost)` deterministically.
    fn set_state(&self, ticket: u64, state: RenderState) {
        if let Ok(mut map) = self.tickets.lock() {
            if let Some(entry) = map.get_mut(&ticket) {
                if !is_terminal(&entry.state) {
                    entry.state = state;
                }
            }
        }
    }

    /// The engine is on a lost GPU device awaiting rebuild.
    fn is_lost(&self) -> bool {
        self.live.read().map(|l| l.lost).unwrap_or(false)
    }

    /// The engine started on the GPU (`Auto`) and has degraded to CPU-preview
    /// after repeated device loss, the state that arms the E7 clamp. A
    /// deliberate `ForceCpu` engine is **not** "degraded" and is never clamped.
    fn is_degraded(&self) -> bool {
        self.backend_pref == BackendPref::Auto
            && self.recover.state() == DegradeState::CpuPreviewOnly
    }

    /// Deliver plan for `req` under the current degrade state (task E7):
    /// on CpuPreviewOnly an Interactive full-res request is clamped to a preview
    /// footprint and badged `PreviewRes`; full-res only proceeds as `Batch`.
    fn plan_render(&self, req: &RenderRequest) -> RenderPlan {
        if !self.is_degraded() {
            return RenderPlan {
                quality: OutputQuality::FullRes,
                preview_halve: false,
            };
        }
        match req.priority {
            RenderPriority::Interactive => RenderPlan {
                quality: OutputQuality::PreviewRes,
                // Only full-res interactive requests are shrunk; an already-
                // decimated request (Fit/Ratio<1) is simply badged PreviewRes.
                preview_halve: is_full_res(req.scale),
            },
            RenderPriority::Batch => RenderPlan {
                quality: OutputQuality::FullRes,
                preview_halve: false,
            },
        }
    }

    /// Run one job to a terminal state on the current live backend.
    fn process(self: &Arc<Self>, job: Job) {
        let Job {
            ticket,
            req,
            cancel,
        } = job;

        if cancel.is_cancelled() {
            self.set_state(ticket, RenderState::Cancelled);
            return;
        }

        // Recovery preamble (E2/E8): a lost GPU device is rebuilt before we
        // render; if the rebuild fails and we have not degraded to CPU, refuse
        // the submission typed (export-safety, E15 retries after rebuild).
        if self.is_lost() {
            if let Err(e) = self.try_recover() {
                self.set_state(
                    ticket,
                    RenderState::Failed(RenderError::DeviceUnavailable(e)),
                );
                return;
            }
        }

        // Snapshot the live backend + device context for this whole render, so a
        // concurrent device-lost signal cannot swap the device mid-render.
        let (backend, gpu) = {
            let live = self
                .live
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (Arc::clone(&live.backend), live.gpu.clone())
        };
        let backend_kind = backend.kind();
        let plan = self.plan_render(&req);

        self.set_state(
            ticket,
            RenderState::Rendering {
                tiles_done: 0,
                tiles_total: 1,
            },
        );

        // Compile the recipe → typed DAG under the requested PV. The M1 compiler
        // is template-only, so a nominal SourceDesc suffices (real source extent
        // arrives via SourceProvider).
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

        // Source-stage injection: if the compiled graph has the engine-owned
        // source stage (`src.decoded`), derive its (image-discriminated,
        // scale-independent) content key and check whether the uploaded tile is
        // already cache-resident, a warm pin means **zero** work (task B4). On
        // a miss, obtain the decoded source, from the host RAM pin when present
        // (re-warm, zero `SourceProvider::fetch`, task E3) else through the
        // `SourceProvider` seam, lift it to a working tile on the snapshot
        // backend, and cache-pin it so later renders skip the upload. Graphs
        // without a source stage (probe generators) render without a fetch.
        let source_inject = match graph.node_index(SrcDecodedNode::ID) {
            Some(src_idx) => {
                let src_key = self.source_key(&req, &graph, src_idx);
                let tile = match self.cache.peek(&src_key) {
                    Some(hit) => hit.tile,
                    None => match self.fetch_source_tile(&req, &cancel, gpu.as_ref()) {
                        Ok(tile) => {
                            self.cache
                                .pin_tile(src_key, tile.clone(), PinLabel::SourceStage);
                            tile
                        }
                        Err(e) => {
                            self.set_state(ticket, RenderState::Failed(e));
                            return;
                        }
                    },
                };
                Some(SourceInject {
                    idx: src_idx,
                    tile,
                    key: src_key,
                })
            }
            None => None,
        };

        // Evaluate on the snapshot backend (rebuilt per render around whichever
        // backend is live). A node panic (e.g. a still-stubbed kernel reached off
        // the wired path) is caught and surfaced typed rather than aborting the
        // worker.
        let executor = Executor::with_probe(
            Arc::clone(&backend),
            self.tile_size,
            Arc::clone(&self.probe),
        );
        let eval = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.evaluate(
                &graph,
                req.pv,
                req.roi,
                req.scale,
                &self.cache,
                &cancel,
                source_inject.clone(),
            )
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

        let output = match self.finish(&req, tile, gpu.as_ref(), backend_kind, &plan) {
            Ok(o) => o,
            Err(e) => {
                self.set_state(ticket, RenderState::Failed(e));
                return;
            }
        };
        self.set_state(ticket, RenderState::Complete(output));
    }

    /// The content key for the engine-owned source stage (`src.decoded`).
    ///
    /// Discriminated by image (so a shared cache never confuses two images' sources)
    /// and **scale-independent**, `DecodedFull` is the full source regardless of
    /// preview scale, so the RAM pin is keyed per `(image, pv, kernel_salt,
    /// params)`, not per render scale. Downstream nodes fold `scale_q` into their
    /// own keys, so a scale change still invalidates the tail while the source
    /// stays pinned (zero re-fetch, task B4).
    fn source_key(&self, req: &RenderRequest, graph: &RenderGraph, src_idx: NodeIndex) -> CacheKey {
        let mut seed = blake3::Hasher::new();
        seed.update(b"lbx.src-image");
        seed.update(&req.image.0.to_le_bytes());
        let image_seed = CacheKey(seed.finalize());
        CacheKey::derive(&CacheKeyInputs {
            node_id: SrcDecodedNode::ID,
            pv: req.pv,
            kernel_salt: graph.salt(src_idx),
            param_hash: graph.params(src_idx).hash(),
            input_keys: &[image_seed],
            tile: TileCoord {
                tx: 0,
                ty: 0,
                scale_q: 0,
            },
            scale_q: 0,
            // Deliberately CONSTANT, not `req.roi`/viewport, this doc
            // comment's own "scale-independent" contract: the source pin
            // must stay hittable across every viewport size. (Downstream
            // nodes DO fold in their own request-scoped `content_extent`
            // see that field's doc, so a viewport change still forces
            // those to re-render even though the source stays pinned.)
            content_extent: Extent { w: 0, h: 0 },
            precision: TilePrecision::F16,
        })
    }

    /// Obtain the decoded source for `req` and lift it to a working tile on the
    /// active backend: a pooled GPU texture (via [`Uploader`]) on the GPU path,
    /// or an identical-bytes host buffer on the CPU path. Prefers the RAM pin
    /// (re-warm, task E3) so a repeat render, including after a device-lost
    /// rebuild, performs **zero** `SourceProvider::fetch`. The engine never
    /// decodes; pixels arrive through the [`SourceProvider`] seam (§3.8).
    fn fetch_source_tile(
        &self,
        req: &RenderRequest,
        cancel: &CancelToken,
        gpu: Option<&Arc<DeviceCtx>>,
    ) -> Result<TileHandle, RenderError> {
        let pinned = self
            .source_pin
            .lock()
            .ok()
            .and_then(|m| m.get(&req.image).cloned());
        let image = match pinned {
            Some(img) => img,
            None => {
                let fetched = pollster::block_on(self.source.fetch(
                    req.image,
                    SourceWant::DecodedFull,
                    cancel,
                ))?;
                let arc = Arc::new(fetched);
                if let Ok(mut m) = self.source_pin.lock() {
                    m.insert(req.image, Arc::clone(&arc));
                }
                arc
            }
        };
        match gpu {
            Some(ctx) => Ok(Uploader::new().upload(ctx.as_ref(), &image)),
            None => Ok(TileHandle::from_cpu(
                crate::ng::source::to_working_tile_cpu(&image),
            )),
        }
    }

    /// Turn a terminal tile into a [`RenderOutput`] for the request's target,
    /// applying the degraded preview clamp (task E7) and stamping the executing
    /// backend as provenance (§4.4).
    fn finish(
        &self,
        req: &RenderRequest,
        tile: TileHandle,
        gpu: Option<&Arc<DeviceCtx>>,
        backend: BackendId,
        plan: &RenderPlan,
    ) -> Result<RenderOutput, RenderError> {
        // Degraded interactive full-res → deliver a downscaled-whole preview so a
        // full-res frame never leaves the interactive path (task E7). The
        // degraded path is CPU-resident, so the engine-owned box decimator
        // applies directly; the GPU-resident case (not reachable while degraded)
        // rides on C2's in-pipeline resize.
        let tile = if plan.preview_halve {
            match tile.cpu() {
                Some(px) => {
                    let half = Extent {
                        w: (px.extent.w / 2).max(1),
                        h: (px.extent.h / 2).max(1),
                    };
                    TileHandle::from_cpu(decimate_box_cpu(px, half))
                }
                None => tile,
            }
        } else {
            tile
        };
        let payload = match req.target {
            RenderTarget::Buffer { format } => {
                OutputPayload::Pixels(readback(&tile, format, gpu.map(|c| c.as_ref()))?)
            }
            RenderTarget::Canvas => {
                // Publish the GPU-resident terminal tile through the installed
                // CanvasPublisher (task F5): a zero-copy texture-to-texture
                // blit into the shell's compositing ring, no CPU readback
                // (spec §2.3 seam 2 / §3.7). A CPU-resident terminal (degraded
                // to CPU, task E7) has nothing to publish, the shell falls
                // back to compositing the last good generation while degraded
                // (F5 deviation: CPU-degraded Canvas targets are not wired;
                // callers should route degraded sessions through a Buffer
                // target if a fresh degraded frame must reach the UI).
                let publisher = self
                    .canvas
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                match publisher {
                    Some(p) if tile.texture().is_some() => {
                        let gen = p.publish(&tile, plan.quality)?;
                        OutputPayload::CanvasGeneration(gen)
                    }
                    Some(_) => {
                        return Err(RenderError::Internal(
                            "Canvas render target requires a GPU-resident terminal tile \
                             (degraded-to-CPU sessions must use a Buffer target)"
                                .to_owned(),
                        ));
                    }
                    None => {
                        return Err(RenderError::Internal(
                            "Canvas render target requested but no publisher is installed \
                             (F5: RenderScheduler::enable_canvas)"
                                .to_owned(),
                        ));
                    }
                }
            }
        };
        Ok(RenderOutput {
            payload,
            colorimetry: OutputColorimetry::default(),
            backend,
            pv: req.pv,
            quality: plan.quality,
        })
    }

    /// Handle a device-lost signal for the device generation `gen` (task E1).
    /// A stale generation (a superseded device reporting loss during a rebuild)
    /// is ignored; an already-degraded engine ignores further loss (CPU can't be
    /// lost). Otherwise: mark lost, drive the policy SM, emit `DeviceLost`, fail
    /// every in-flight ticket `Failed(DeviceLost)` (never hang), and, if the SM
    /// says degrade, swap to CPU immediately and emit `DegradedToCpu`.
    fn signal_device_lost_gen(&self, gen: u64, reason: &str) {
        if gen != self.device_gen.load(Ordering::Acquire) {
            return; // a stale/superseded device, ignore.
        }
        if self.recover.state() == DegradeState::CpuPreviewOnly {
            return; // already on CPU, nothing to lose.
        }
        {
            let mut live = self
                .live
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if live.gpu.is_none() {
                return; // ForceCpu / no GPU, nothing to lose.
            }
            live.lost = true;
        }
        let state = self.recover.on_device_lost(reason);
        let _ = self.events_tx.send(EngineEvent::DeviceLost {
            reason: reason.to_owned(),
        });
        self.fail_inflight_device_lost(reason);

        if state == DegradeState::CpuPreviewOnly {
            // Retire the lost GPU device generation so its (about-to-drop)
            // callback is ignored, then swap to CPU, always available.
            self.device_gen.fetch_add(1, Ordering::AcqRel);
            {
                let mut live = self
                    .live
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                live.backend = Arc::new(CpuBackend::new(self.cpu_threads));
                live.gpu = None;
                live.lost = false;
            }
            // Drop the cache: its GPU-resident tiles (incl. the RAM-pinned source
            // stage) can't be read back on the CPU path. The host `source_pin`
            // survives, so the degraded CPU render re-warms with zero re-fetch.
            // Integration fix, Wave BCE (B cache × E recovery).
            self.cache.clear();
            let _ = self.events_tx.send(EngineEvent::DegradedToCpu);
            tracing::warn!(
                target: "lightbox_render::recover",
                "degraded to CPU preview-only after repeated device loss"
            );
        }
    }

    /// Resolve every non-terminal ticket as `Failed(DeviceLost)` and cancel its
    /// token so a blocking node unwinds, the "in-flight tickets resolve, never
    /// hang" guarantee (task E1).
    fn fail_inflight_device_lost(&self, reason: &str) {
        if let Ok(mut map) = self.tickets.lock() {
            for entry in map.values_mut() {
                if !is_terminal(&entry.state) {
                    entry.state = RenderState::Failed(RenderError::DeviceLost(reason.to_owned()));
                    entry.cancel.cancel();
                }
            }
        }
    }

    /// Rebuild the GPU backend on a fresh device from the [`DeviceProvider`] seam
    /// (task E2): recreate the [`DeviceCtx`], pipeline cache, and [`TilePool`],
    /// install device-lost callbacks on the new device under a bumped generation,
    /// and clear the lost flag. The VRAM cache tier is dropped with the old
    /// backend; the RAM source pin (host memory) survives and re-uploads onto the
    /// rebuilt device (re-warm, task E3). Returns the seam error string on
    /// failure so `process` can refuse the submission typed (E8).
    fn try_recover(self: &Arc<Self>) -> Result<(), String> {
        let handles = pollster::block_on(self.device.rebuild()).map_err(|e| e.to_string())?;
        let ctx = Arc::new(DeviceCtx::new(handles.0, handles.1));
        let backend: Arc<dyn Backend> = Arc::new(GpuBackend::new(&ctx));
        let gen = self.device_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.install_device_callbacks(&ctx.device, gen);
        {
            let mut live = self
                .live
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            live.backend = backend;
            live.gpu = Some(ctx);
            live.lost = false;
        }
        // Drop the whole node cache: every resident tile, including the
        // RAM-pinned source stage (task B4), references pooled textures on the
        // now-dead device, so serving one would be a use-after-device-loss. The
        // host `source_pin` (decoded pixels) survives, so the next render
        // re-uploads onto the fresh device with **zero** `SourceProvider::fetch`
        // (re-warm, task E3). Integration fix, Wave BCE (B cache × E recovery).
        self.cache.clear();
        Ok(())
    }

    /// Install the device-lost callback + uncaptured-error hook on `device`
    /// (task E1). The device-lost callback routes hard loss into
    /// [`Self::signal_device_lost_gen`] under `gen`; the uncaptured-error hook
    /// surfaces GPU errors so they are neither swallowed nor panicked (a hard
    /// loss still arrives via the device-lost callback, not here).
    fn install_device_callbacks(self: &Arc<Self>, device: &wgpu::Device, gen: u64) {
        let weak: Weak<Shared> = Arc::downgrade(self);
        device.set_device_lost_callback(move |reason, message| {
            if let Some(shared) = weak.upgrade() {
                shared.signal_device_lost_gen(gen, &format!("{reason:?}: {message}"));
            }
        });
        device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
            tracing::error!(
                target: "lightbox_render::recover",
                "uncaptured GPU error (hard device loss arrives via the device-lost callback): {err}"
            );
        }));
    }
}

/// The render engine (spec §3.6). Holds the registry-backed compiler, the tile
/// cache, a live (swappable) pixel [`Backend`], the device-lost recovery state
/// machine, and a serial render worker.
///
/// A-core wired the **CPU** backend + Buffer readback; A-gpu added the
/// shared-device GPU backend and the canvas double-buffer (A13); **E** adds
/// device-lost detection → rebuild → degrade-to-CPU-preview and the degraded
/// contract, the ticket lifecycle below is backend-agnostic.
pub struct Engine {
    shared: Arc<Shared>,
    next_ticket: AtomicU64,
    job_tx: Option<mpsc::Sender<Job>>,
    worker: Option<JoinHandle<()>>,
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

    /// Same as [`Engine::new`], but additionally installs `look_resolver` on
    /// the compiler (E10 task D10): `global.creative_lut`'s recipe-carried
    /// `id` → parsed `Lut3D` content resolution at graph-compile time.
    /// `lightbox-core` is the only production caller (its catalog-backed
    /// `installed_look` adapter); `Engine::new` keeps installing no resolver
    /// for every other caller (tests, `lightbox-cli`, …) that hasn't opted
    /// in, those keep the pre-D10 graceful-identity behavior unchanged.
    pub fn new_with_look_resolver(
        dp: Arc<dyn DeviceProvider>,
        sp: Arc<dyn SourceProvider>,
        registry: NodeRegistry,
        cfg: EngineConfig,
        look_resolver: Arc<dyn crate::ng::nodes::global::LookResolver>,
    ) -> Result<Engine, EngineInitError> {
        let mut compiler =
            RecipeCompiler::with_registry(Arc::new(registry)).with_look_resolver(look_resolver);
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
        // Backend selection across the frozen exec↔backend seam (spec §3.6):
        // `ForceCpu` always takes the rayon CPU path; `Auto` builds the GPU
        // backend on the shell's shared device (via the `DeviceProvider` seam)
        // and keeps the device context for source upload + terminal readback.
        // (Device-lost fallback GPU→CPU is task E, wired below; the CPU path is
        // fully wired here.)
        let (backend, gpu): (Arc<dyn Backend>, Option<Arc<DeviceCtx>>) = match cfg.backend {
            BackendPref::ForceCpu => (Arc::new(CpuBackend::new(cfg.cpu_threads)), None),
            BackendPref::Auto => {
                let (device, queue) = dp.current();
                let ctx = Arc::new(DeviceCtx::new(device, queue));
                let backend: Arc<dyn Backend> = Arc::new(GpuBackend::new(&ctx));
                (backend, Some(ctx))
            }
        };
        // One shared recompute probe: the per-render executor counts
        // nodes_evaluated / cache hits+misses during the walk, and the cache
        // mirrors its resident-byte gauge into it (Engine::stats reads both).
        // (The executor is built per render around the live backend, task E.)
        let probe = Arc::new(RecomputeProbe::new());
        let cache = NodeCache::with_budget_and_probe(vram_budget_bytes(&cfg), Arc::clone(&probe));

        let (events_tx, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            tickets: Mutex::new(HashMap::new()),
            compiler,
            cache,
            source: sp,
            device: dp,
            live: RwLock::new(LiveBackend {
                backend,
                gpu,
                lost: false,
            }),
            probe,
            tile_size: cfg.tile_size,
            cpu_threads: cfg.cpu_threads,
            backend_pref: cfg.backend,
            recover: RecoverStateMachine::new(cfg.device_lost_degrade),
            events_tx,
            device_gen: AtomicU64::new(0),
            source_pin: Mutex::new(HashMap::new()),
            canvas: RwLock::new(None),
        });

        // Install device-lost detection on the GPU device, generation 0 (task E1).
        let gpu_device = shared
            .live
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .gpu
            .as_ref()
            .map(|ctx| Arc::clone(&ctx.device));
        if let Some(device) = gpu_device {
            shared.install_device_callbacks(&device, 0);
        }

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

        Ok(Engine {
            shared,
            next_ticket: AtomicU64::new(1),
            job_tx: Some(job_tx),
            worker: Some(worker),
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

    /// Subscribe to engine events, device-lost / degraded / re-enabled (§3.6).
    /// **E** emits `DeviceLost` on a detected loss, `DegradedToCpu` on the
    /// policy-driven fall to CPU-preview, and `GpuReenabled` on explicit
    /// re-enable. Subscribe **before** the event you want to observe (a broadcast
    /// channel only delivers to live receivers).
    pub fn events(&self) -> broadcast::Receiver<EngineEvent> {
        self.shared.events_tx.subscribe()
    }

    /// What the engine is currently rendering on (spec §3.6; task E4). Reports
    /// `CpuPreviewOnly` when degraded (or CPU-only), else `Gpu(AdapterInfo)`, the
    /// adapter identity from the [`DeviceProvider`] seam, or a labelled unknown
    /// placeholder when the seam does not expose it.
    pub fn active_backend(&self) -> ActiveBackend {
        if self.shared.is_degraded() {
            return ActiveBackend::CpuPreviewOnly;
        }
        let on_gpu = self
            .shared
            .live
            .read()
            .map(|l| l.gpu.is_some())
            .unwrap_or(false);
        if on_gpu {
            let info = self
                .shared
                .device
                .adapter_info()
                .unwrap_or_else(unknown_adapter_info);
            ActiveBackend::Gpu(info)
        } else {
            ActiveBackend::CpuPreviewOnly
        }
    }

    /// Explicitly re-enable the GPU path after a degrade (prefs, E08; task E4):
    /// reset the degrade policy and rebuild the GPU backend. On an `Auto` engine
    /// a failed rebuild returns the seam error typed; a `ForceCpu` engine has no
    /// GPU to re-enable and returns `Ok` unchanged.
    pub fn reenable_gpu(&self) -> Result<(), RenderError> {
        self.shared.recover.reenable_gpu();
        if self.shared.backend_pref == BackendPref::Auto {
            self.shared
                .try_recover()
                .map_err(RenderError::DeviceUnavailable)?;
            let _ = self.shared.events_tx.send(EngineEvent::GpuReenabled);
        }
        Ok(())
    }

    /// Installs the shell's canvas double-buffer publisher (task **F5** M1
    /// integration; spec §3.7). Once installed, `RenderTarget::Canvas`
    /// requests publish their GPU-resident terminal tile through `publisher`
    /// (zero-copy) instead of failing typed. Owned/constructed by
    /// [`crate::ng::RenderScheduler::enable_canvas`], call that instead of
    /// this directly unless you are wiring a custom scheduler.
    pub fn install_canvas(&self, publisher: Arc<CanvasPublisher>) {
        if let Ok(mut c) = self.shared.canvas.write() {
            *c = Some(publisher);
        }
    }

    /// **Test-only fault injector** (spec E1): simulate a device-lost signal
    /// exactly as the wgpu `device_lost` callback would, driving detection →
    /// (rebuild on next submit) / degrade. Not part of the shipping API surface
    /// it lets the E05.5 gates run headlessly and deterministically without
    /// relying on the driver to actually drop the device.
    pub fn inject_device_lost(&self, reason: &str) {
        let gen = self.shared.device_gen.load(Ordering::Acquire);
        self.shared.signal_device_lost_gen(gen, reason);
    }

    /// The process versions this engine can render (spec §3.6).
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        self.shared.compiler.supported_pvs()
    }

    /// Engine counters, including the recompute-count probe (spec §3.6).
    ///
    /// `nodes_evaluated` and cache hits/misses come from the executor probe
    /// (which decides hit vs miss during the walk); `cache_evictions` and
    /// `vram_bytes` come from the cache's own accounting.
    pub fn stats(&self) -> EngineStats {
        let mut s = self.shared.probe.snapshot();
        let cs = self.shared.cache.stats();
        s.cache_evictions = cs.evictions;
        s.vram_bytes = cs.bytes;
        s
    }
}

/// Resolve a [`VramBudget`] policy to a concrete byte budget for the node cache.
/// `Bytes(n)` is used verbatim; `Auto` uses the cache's default cap (adapter
/// probing lands with the GPU device-lost work, the CPU-reference path has no
/// adapter memory to probe).
fn vram_budget_bytes(cfg: &EngineConfig) -> u64 {
    match cfg.vram_budget {
        VramBudget::Bytes(n) => n,
        VramBudget::Auto => crate::ng::cache::DEFAULT_VRAM_BUDGET,
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

/// Whether a ticket state is terminal (no further transition). Used to make
/// device-lost failures sticky (task E1) and to drop late worker updates.
fn is_terminal(state: &RenderState) -> bool {
    matches!(
        state,
        RenderState::Complete(_) | RenderState::Failed(_) | RenderState::Cancelled
    )
}

/// Whether `scale` requests a full-resolution render (the class the degraded
/// interactive path clamps, task E7). `Fit`/`Ratio(<1)` are already previews.
fn is_full_res(scale: RenderScale) -> bool {
    match scale {
        RenderScale::OneToOne => true,
        RenderScale::Ratio(f) => f >= 0.999,
        RenderScale::Fit(_) => false,
    }
}

/// A labelled placeholder adapter identity for [`Engine::active_backend`] when a
/// [`DeviceProvider`] does not report [`DeviceProvider::adapter_info`]. Honest
/// "unknown" metadata (never a fabricated adapter); the shell (F5) and GPU test
/// providers supply the real info.
fn unknown_adapter_info() -> wgpu::AdapterInfo {
    wgpu::AdapterInfo {
        name: "unknown (DeviceProvider did not report adapter_info)".to_owned(),
        vendor: 0,
        device: 0,
        device_type: wgpu::DeviceType::Other,
        device_pci_bus_id: String::new(),
        driver: String::new(),
        driver_info: String::new(),
        backend: wgpu::Backend::Noop,
        subgroup_min_size: 0,
        subgroup_max_size: 0,
        transient_saves_memory: false,
    }
}

/// Read a terminal working tile back into a CPU [`PixelBuf`] of `format`
/// (Buffer target; spec §3.6). The tile lives on whichever backend produced it:
/// a host buffer on the CPU path, or a pooled GPU texture copied back via
/// [`readback_tile`] on the GPU path. The pack is **linear**, display encoding
/// (sRGB/ICC) is the `xform.display` node's output, not the engine's job
/// (E02 guardrail); a PV1 graph's terminal is already `DisplayRgba8`.
fn readback(
    tile: &TileHandle,
    format: OutFormat,
    gpu: Option<&DeviceCtx>,
) -> Result<PixelBuf, RenderError> {
    let src: PixelBuf = if let Some(px) = tile.cpu() {
        px.clone()
    } else if let Some(ctx) = gpu {
        readback_tile(&ctx.device, &ctx.queue, tile)?
    } else {
        return Err(RenderError::Readback(
            "terminal tile is GPU-resident but no device context is available for readback"
                .to_owned(),
        ));
    };
    let target = match format {
        OutFormat::Rgba8Srgb => PixelFormat::Rgba8Srgb,
        // No 16-bit-unorm working format in the engine; the float tier carries
        // the same values losslessly for a 16-bit export tier.
        OutFormat::Rgba16 => PixelFormat::Rgba16F,
        OutFormat::Rgba32F => PixelFormat::Rgba32F,
    };
    if src.format == target {
        return Ok(src);
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

    /// A source node that spins until cancelled, proves cancel reaches a tile
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
        // These probe graphs render on the CPU path over a `NullDevice`, so
        // force the CPU backend (`Auto` would ask the null device for handles).
        Engine::with_compiler(
            Arc::new(NullDevice),
            Arc::new(NullSource),
            compiler,
            EngineConfig {
                backend: BackendPref::ForceCpu,
                ..EngineConfig::default()
            },
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

    /// **Regression (user-reported bug): a plain color edit must never
    /// collapse the render extent.** Root cause: `RenderScale::Fit`/
    /// `OneToOne` both quantize `scale_q` to a CONSTANT `64` regardless of
    /// the actual requested viewport (`exec::scale_ratio`'s documented
    /// simplification), and before this fix no node's `CacheKey` reflected
    /// the literal requested pixel extent either, so two `Fit` renders of
    /// the SAME image+recipe at DIFFERENT viewport sizes (e.g. the shell's
    /// own first-frame layout not yet settled, then a develop-rail
    /// open/window resize, all of which resubmit `set_view` with an
    /// UNCHANGED recipe) derived IDENTICAL cache keys for every
    /// recipe-invariant node and silently reused the FIRST request's
    /// stale, differently-shaped output, exactly the mechanism that made
    /// a large image render "zoomed into a tiny part" after the engine
    /// frame caught up with a since-superseded viewport. Fails before the
    /// `content_extent` fix (both assertions below saw `{ w: 40, h: 30 }`);
    /// passes after.
    #[test]
    fn distinct_viewports_do_not_collide_on_a_stale_cached_extent() {
        let engine = engine_with(
            GraphTemplate::linear(vec![NodeId("test.const")]),
            &[(
                NodeId("test.const"),
                Arc::new(ConstFactory) as Arc<dyn NodeFactory>,
            )],
        );

        let req_at = |w: u32, h: u32| RenderRequest {
            image: ImageId(1),
            recipe: Recipe::identity(PV_M0), // SAME recipe both times, no param change at all
            pv: PV_M0,
            roi: Roi { x: 0, y: 0, w, h },
            scale: RenderScale::Fit(Extent { w, h }),
            target: RenderTarget::Buffer {
                format: OutFormat::Rgba8Srgb,
            },
            priority: RenderPriority::Interactive,
            cancel: CancelToken::new(),
        };

        let t1 = engine.submit(req_at(40, 30));
        let s1 = poll_until(&engine, &t1, |s| {
            matches!(s, RenderState::Complete(_) | RenderState::Failed(_))
        });
        let RenderState::Complete(out1) = s1 else {
            panic!("first render: expected Complete, got {s1:?}");
        };
        let OutputPayload::Pixels(px1) = out1.payload else {
            panic!("expected Pixels payload");
        };
        assert_eq!(px1.extent, Extent { w: 40, h: 30 });

        // A SECOND request for the SAME image + IDENTICAL recipe, but a
        // DIFFERENT viewport, mirrors a pure `RenderScheduler::set_view`
        // resubmit (no edit at all). Before the fix this silently served
        // the FIRST request's cached (40×30) tile.
        let t2 = engine.submit(req_at(17, 11));
        let s2 = poll_until(&engine, &t2, |s| {
            matches!(s, RenderState::Complete(_) | RenderState::Failed(_))
        });
        let RenderState::Complete(out2) = s2 else {
            panic!("second render: expected Complete, got {s2:?}");
        };
        let OutputPayload::Pixels(px2) = out2.payload else {
            panic!("expected Pixels payload");
        };
        assert_eq!(
            px2.extent,
            Extent { w: 17, h: 11 },
            "a viewport-only change must never reuse a differently-sized cached tile"
        );
    }
}
