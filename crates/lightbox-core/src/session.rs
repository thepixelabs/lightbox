// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`Core`] / [`Session`], the headless boundary itself (spec §3.8, seam 1).
//!
//! **No UI type crosses down; no SQL crosses up.** `wgpu::Device` (inside
//! [`GpuContext`]) is the ONE shared GPU type allowed across the boundary
//! (architecture §2.3 seam 2).
//!
//! # Command bus (T16)
//!
//! [`Session::submit`] never blocks: commands are enqueued to a dispatcher
//! task on the job runtime. Trivial writer commands (`SetRating`, `SetFlag`,
//! `UndoImport`) execute strictly in submission order, each as **one WAL
//! transaction** on the catalog's single writer; long-running commands
//! (`ImportAddInPlace`, `BackupNow`) spawn as class-budgeted jobs so the
//! queue never stalls behind them. Outcomes are broadcast [`Event`]s.
//!
//! # Close (T15)
//!
//! [`Session::close`] cancels the session's cancel-token tree (in-flight
//! imports stop at their next checkpoint and flush their stats), waits for
//! in-flight command jobs (bounded by `CoreConfig::close_wait`), then runs
//! the exit-time verified backup per policy (spec OQ-6: skip when the
//! newest verified backup is younger than `backup_max_age`; tests skip via
//! [`CloseOpts`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lightbox_catalog::{BackupOpts, BackupReport, Catalog, CatalogTxn};
use lightbox_edit::EditStore;
use lightbox_ingest::{
    import_add_in_place, load_working_set, plan_open, ImportEvent, ImportOptions, LoadEvent,
    OpenError, OpenRequest,
};
use lightbox_jobs::{CancelToken, Class, JobError, JobSystem};
use lightbox_preview::{
    AssetLocator, EmbeddedPreviewProvider, PreviewEvent as PvEvent, PreviewProvider,
    PreviewService, PreviewStoreConfig, Retention, Store,
};
use lightbox_render::ng::{
    shipping_registry, BackendPref, DeviceProvider, Engine, EngineConfig, EngineEvent, Extent,
    JobsHandle, RenderScheduler, SourceProvider,
};
use lightbox_render::GpuContext;
use lightbox_types::{ImageId, LookId};
use tokio::sync::{broadcast, mpsc};

use crate::command::{Command, CommandTicket, EditCommand};
use crate::config::CoreConfig;
use crate::edit_hub::EditHub;
use crate::error::Result;
use crate::event::{ChangeSet, Event};
use crate::looks::CatalogLookResolver;
use crate::preview_runtime::TokioBuildRuntime;
use crate::previews::CatalogAssetLocator;
use crate::queries::Queries;
use crate::render_source::{NullDeviceProvider, PreviewSourceProvider, SharedDeviceProvider};
use crate::working_set::{WorkingSetModel, WorkingSetSnapshot};

/// The headless core (spec §3.8): owns the job system; opens sessions.
pub struct Core {
    jobs: Arc<JobSystem>,
    cfg: CoreConfig,
}

impl Core {
    /// Starts the core: job runtime up, no catalog yet (spec §3.8).
    pub fn start(cfg: CoreConfig) -> Result<Core> {
        let jobs = Arc::new(JobSystem::new(cfg.jobs.clone()));
        tracing::info!(target: "lightbox_core", "core started");
        Ok(Core { jobs, cfg })
    }

    /// Creates `<lbdata>` (catalog + backups dir, all migrations) and opens
    /// a session on it. `gpu`: the shell's shared device, or `None` for
    /// headless/CPU-only (spec §3.8).
    pub fn create_catalog(&self, lbdata: &Path, gpu: Option<GpuContext>) -> Result<Session> {
        let catalog = Catalog::create(lbdata)?;
        Session::open(self, catalog, gpu)
    }

    /// Opens an existing catalog (quick_check + pending migrations inside)
    /// and starts a session on it.
    pub fn open_catalog(&self, lbdata: &Path, gpu: Option<GpuContext>) -> Result<Session> {
        let catalog = Catalog::open(lbdata)?;
        Session::open(self, catalog, gpu)
    }

    /// The core's job system, for embedders that need to schedule their own
    /// class-budgeted work next to a session (additive; not part of the
    /// frozen §3.8 surface).
    pub fn jobs(&self) -> &Arc<JobSystem> {
        &self.jobs
    }
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

/// Exit options for [`Session::close`] (spec §3.8 `CloseOpts`).
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct CloseOpts {
    /// Backup policy for this close.
    pub backup: ClosePolicy,
}

impl CloseOpts {
    /// `CloseOpts` with the given backup policy (`#[non_exhaustive]`
    /// constructor; tests skip the exit backup via
    /// `ClosePolicy::Skip.into()`, spec §3.8).
    pub fn with_backup(policy: ClosePolicy) -> CloseOpts {
        CloseOpts { backup: policy }
    }
}

impl From<ClosePolicy> for CloseOpts {
    fn from(policy: ClosePolicy) -> CloseOpts {
        CloseOpts::with_backup(policy)
    }
}

/// When to run the exit-time verified backup.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ClosePolicy {
    /// Back up unless the newest verified backup is younger than
    /// `CoreConfig::backup_max_age` (spec T15 policy, OQ-6).
    #[default]
    Auto,
    /// Always back up.
    Always,
    /// Skip the backup (tests, emergency exits).
    Skip,
}

/// What [`Session::close`] did.
#[derive(Debug)]
#[non_exhaustive]
pub struct CloseReport {
    /// The exit-time backup, when one ran.
    pub backup: Option<BackupReport>,
}

/// A live catalog + engine + previews + command bus (spec §3.8, seam 1).
/// Clone-cheap (`Arc` inner); everything the shell (or CLI) may touch.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    catalog: Arc<Catalog>,
    engine: Arc<Engine>,
    scheduler: Arc<RenderScheduler>,
    previews: Arc<dyn PreviewProvider>,
    /// E03 Phase D (T14): the build-scheduler facade, additive alongside
    /// `previews` above.
    preview_service: PreviewService,
    edit_hub: Arc<EditHub>,
    /// E04 (spec §4.5): the session working-set model, `Session::
    /// working_set()`'s backing store.
    working_set: Arc<WorkingSetModel>,
    events: broadcast::Sender<Event>,
    cmd_tx: mpsc::UnboundedSender<Queued>,
    next_ticket: AtomicU64,
    session_cancel: CancelToken,
    in_flight: Arc<InFlight>,
    cfg: CoreConfig,
    lbdata: PathBuf,
}

struct Queued {
    ticket: CommandTicket,
    cmd: Command,
}

impl Session {
    fn open(core: &Core, catalog: Catalog, gpu: Option<GpuContext>) -> Result<Session> {
        let lbdata = catalog.lbdata_dir().to_path_buf();
        let catalog = Arc::new(catalog);

        // E03 Phase B (T06-T09): the `.lbdata` preview cache store lives
        // alongside the catalog (spec §3.2, same directory, not a sibling
        // tree); `Store::open` creates/adopts its reserved subdirectories
        // idempotently (Phase A, T01).
        //
        // E08 Phase G (G2/G4): the catalog-scope prefs (`catalog_settings`,
        // spec §6.7) shape the store's construction, persisted cache caps,
        // T2 retention, and a relocated cache root all survive a restart
        // this way (`SetCacheLimits`/`RelocateCacheStore` cover the live
        // half). An unusable override root falls back to the catalog's own
        // `.lbdata` dir with a warning (§5.2 fallback), never a fatal open.
        let cat_prefs = crate::prefs::load_catalog_prefs(&catalog);
        let mut store_cfg = PreviewStoreConfig::with_defaults(
            cat_prefs
                .cache_dir_override
                .clone()
                .unwrap_or_else(|| lbdata.clone()),
        );
        store_cfg.limits = cat_prefs.cache_limits();
        store_cfg.t2_retention = match cat_prefs.t2_retention_days {
            Some(days) => Retention::Days(days),
            None => Retention::Never,
        };
        let preview_store = match Store::open(&store_cfg) {
            Ok(store) => Arc::new(store),
            Err(err) if cat_prefs.cache_dir_override.is_some() => {
                tracing::warn!(
                    target: "lightbox_core",
                    root = %store_cfg.root.display(),
                    %err,
                    "cache-dir override unusable; falling back to the catalog dir"
                );
                store_cfg.root = lbdata.clone();
                Arc::new(Store::open(&store_cfg)?)
            }
            Err(err) => return Err(err.into()),
        };

        // T21 wiring: the embedded-preview provider, LRU capped in bytes
        // per CoreConfig (spec §3.6), resolving images via the catalog.
        // Store-backed as of E03 Phase B: every decode now ensures the
        // asset's T0 exists in `preview_store` + the catalog (T06/T07)
        // before decoding it (T08).
        let locator: Arc<dyn AssetLocator> =
            Arc::new(CatalogAssetLocator::new(Arc::clone(&catalog)));
        let previews: Arc<dyn PreviewProvider> = Arc::new(EmbeddedPreviewProvider::new(
            Arc::clone(&core.jobs),
            Arc::clone(&locator),
            core.cfg.preview_cache_bytes,
            preview_store,
            Arc::clone(&catalog),
        ));

        // E05 Phase F5 (M1 integration): the PV1 engine-owned nodes
        // (`src.decoded → util.resize → xform.display`) registered against
        // the `ng` engine, the loupe/canvas is produced by
        // `RenderScheduler`/`Engine::submit` through `lightbox-render::ng`,
        // never a blit path that bypasses the engine (spec §9 M0 verbatim
        // requirement, now satisfied by the E05 engine rather than the E01
        // seed, see `docs/plan/epics/E05-deviations.md`).
        // The engine's OWN shipping registry, deliberately not a hand-rolled
        // copy of it.
        //
        // This used to enumerate `src.decoded` / `util.resize` /
        // `xform.display` plus `register_global_nodes` by hand, with a comment
        // claiming it was "the same registry the shell's editor canvas and
        // `lightbox-cli render` both submit against". It had since drifted:
        // E11's `register_geometry_nodes` landed in `shipping_registry` but
        // never here, so the shipped app carried a registry missing
        // `geom.warp` and `geom.crop`. Straighten and crop compiled to a node
        // the app could not construct and failed at render time with
        // "no node registered for NodeId geom.warp under process version",
        // while every test that went through `shipping_registry`/
        // `shipping_compiler` passed, which is exactly why it went unnoticed.
        //
        // Calling the one constructor makes that class of drift impossible:
        // a node registered for the shipping pipeline is, by construction,
        // registered for the app.
        let registry = shipping_registry();

        let device_provider: Arc<dyn DeviceProvider> = match &gpu {
            Some(gpu) => Arc::new(SharedDeviceProvider::new(gpu.clone())),
            // No adapter: `ForceCpu` below never calls `DeviceProvider`.
            None => Arc::new(NullDeviceProvider),
        };
        let source_provider: Arc<dyn SourceProvider> =
            Arc::new(PreviewSourceProvider::new(Arc::clone(&previews)));
        let backend = if gpu.is_some() {
            BackendPref::Auto
        } else {
            BackendPref::ForceCpu
        };
        // E10 Phase D (D10): the `global.creative_lut` id→content resolver,
        // shared between the engine's compiler (resolves at graph-compile
        // time) and `RemoveLook`'s dispatch (invalidates on removal, below).
        let look_resolver = Arc::new(CatalogLookResolver::new(Arc::clone(&catalog)));
        let engine = Arc::new(Engine::new_with_look_resolver(
            device_provider,
            source_provider,
            registry,
            EngineConfig {
                backend,
                ..EngineConfig::default()
            },
            Arc::clone(&look_resolver) as Arc<dyn lightbox_render::ng::LookResolver>,
        )?);
        let scheduler = Arc::new(RenderScheduler::new(
            Arc::clone(&engine),
            JobsHandle(Arc::clone(&core.jobs)),
        ));
        if let Some(gpu) = &gpu {
            scheduler.enable_canvas(
                gpu.device.clone(),
                gpu.queue.clone(),
                lightbox_render::ng::Extent { w: 64, h: 64 },
            );
        }

        let (events, _) = broadcast::channel::<Event>(core.cfg.event_capacity.max(16));

        // E03 Phase D (T14): the `PreviewService` facade, tickets, the T13
        // scheduler, `set_viewport` (T15), bulk build (T16). Additive
        // alongside `EmbeddedPreviewProvider` above (see
        // `lightbox_preview::service`'s module doc comment on the two
        // independent index mirrors this implies); registered under its own
        // `Session::preview_service()` accessor, not through
        // `PreviewProvider`. The M0 `BuildRuntime` (spec §5.6) runs builds
        // on the SAME tokio runtime `core.jobs` already owns, via
        // `spawn_blocking`, no second runtime, no `Class` budget yet (see
        // `preview_runtime.rs`'s doc comment for why "plain tokio" is read
        // literally here).
        let preview_cfg = PreviewStoreConfig::with_defaults(lbdata.clone());
        let preview_workers = preview_cfg.workers.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(4)
                .min(8)
        });
        let preview_runtime = TokioBuildRuntime::new(core.jobs.handle().clone(), preview_workers);
        let preview_bus = events.clone();
        let preview_events_sink: lightbox_preview::EventSink = Arc::new(move |ev: PvEvent| {
            let translated = match ev {
                PvEvent::Ready { image, tier, desc } => Event::PreviewReady { image, tier, desc },
                PvEvent::Failed { image, tier, error } => {
                    Event::PreviewFailed { image, tier, error }
                }
                PvEvent::BulkProgress { done, total } => Event::PreviewBulkProgress { done, total },
                // E03 Phase F (T19/T21): eviction/retention/discard and
                // ENOSPC pressure now have core meaning.
                PvEvent::Evicted { image, tier } => Event::PreviewEvicted { image, tier },
                PvEvent::CachePressure {
                    kind,
                    used_bytes,
                    cap_bytes,
                } => Event::CachePressure {
                    kind,
                    used_bytes,
                    cap_bytes,
                },
                // `PreviewEvent` is `#[non_exhaustive]`; a future variant
                // gets a core translation when it gets core meaning, same
                // convention as `ImportEvent`/`EngineEvent` elsewhere in this
                // function.
                _ => return,
            };
            let _ = preview_bus.send(translated);
        });
        let preview_service = PreviewService::open(
            preview_cfg,
            Arc::clone(&catalog),
            preview_runtime,
            preview_events_sink,
        )
        .map_err(crate::error::CoreError::Preview)?;

        // E09 T8: the edit-state session registry (wraps `EditStore`, itself
        // just an `Arc<Catalog>` handle, cheap). The preset store underneath
        // is opened lazily on first use (see `CoreConfig::preset_dir`), so a
        // session that never touches presets never creates the directory.
        let edit_hub = EditHub::new(
            EditStore::new(Arc::clone(&catalog)),
            core.cfg.preset_dir.clone(),
            events.clone(),
        );

        // DeviceDegraded seam (spec §3.8): relay the engine's device-lost /
        // degraded-to-CPU events onto the core event bus. `ng::Engine::events`
        // is a broadcast channel (not a callback like the E01 seed's
        // `on_device_lost`), so this is a long-lived relay task rather than a
        // registered closure; it ends when the engine drops (the channel
        // closes) or the session's last clone drops the `events` sender.
        let mut engine_events = engine.events();
        let degraded_events = events.clone();
        core.jobs.handle().spawn(async move {
            loop {
                match engine_events.recv().await {
                    Ok(ev) => {
                        let reason = match ev {
                            EngineEvent::DeviceLost { reason } => Some(reason),
                            EngineEvent::DegradedToCpu => {
                                Some("degraded to CPU preview-only".to_owned())
                            }
                            // EngineEvent is #[non_exhaustive]; future engine
                            // events get core translations when they get core
                            // meaning (same convention as `ImportEvent` below).
                            EngineEvent::GpuReenabled | EngineEvent::VramPressure { .. } => None,
                            _ => None,
                        };
                        if let Some(reason) = reason {
                            let _ = degraded_events.send(Event::DeviceDegraded { reason });
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Queued>();
        let session_cancel = CancelToken::new();
        let in_flight = Arc::new(InFlight::default());
        // E04 (spec §4.5): the working-set model, empty until the first
        // `OpenWorkingSet` (or never, headless callers that only ever
        // import/query).
        let working_set = WorkingSetModel::new();

        let ctx = DispatchCtx {
            catalog: Arc::clone(&catalog),
            jobs: Arc::clone(&core.jobs),
            events: events.clone(),
            session_cancel: session_cancel.clone(),
            in_flight: Arc::clone(&in_flight),
            backup_retain: core.cfg.backup_retain,
            edit_hub: Arc::clone(&edit_hub),
            preview_service: preview_service.clone(),
            working_set: Arc::clone(&working_set),
            working_set_opts: core.cfg.working_set.clone(),
            engine: Arc::clone(&engine),
            look_resolver: Arc::clone(&look_resolver),
        };
        // Long-lived system task, not a class-budgeted job. It ends when the
        // last `Session` clone drops (the sole sender side of `cmd_rx`).
        core.jobs.handle().spawn(dispatch_loop(ctx, cmd_rx));

        tracing::info!(
            target: "lightbox_core",
            catalog = %lbdata.display(),
            schema_version = catalog.schema_version(),
            "session open"
        );

        Ok(Session {
            inner: Arc::new(SessionInner {
                catalog,
                engine,
                scheduler,
                previews,
                preview_service,
                edit_hub,
                working_set,
                events,
                cmd_tx,
                next_ticket: AtomicU64::new(1),
                session_cancel,
                in_flight,
                cfg: core.cfg.clone(),
                lbdata,
            }),
        })
    }

    /// Submits a command; **never blocks** (spec §5.1). The result arrives
    /// as [`Event`]s correlated by the returned ticket.
    pub fn submit(&self, cmd: Command) -> CommandTicket {
        let ticket = CommandTicket(self.inner.next_ticket.fetch_add(1, Ordering::Relaxed));
        tracing::debug!(target: "lightbox_core", ticket = ticket.id(), ?cmd, "command submitted");
        if self.inner.cmd_tx.send(Queued { ticket, cmd }).is_err() {
            let _ = self.inner.events.send(Event::CommandFailed {
                ticket,
                error: "session is closing".to_owned(),
            });
        }
        ticket
    }

    /// A snapshot view for queries, executes on the calling thread against
    /// a pooled WAL-snapshot read connection (spec §5.1). Drop it promptly.
    pub fn query(&self) -> Queries {
        Queries::new(
            self.inner.catalog.reader(),
            Arc::clone(&self.inner.edit_hub),
            self.inner.preview_service.clone(),
        )
    }

    /// The E09 edit hub (spec §3.4 `Session::edits`): the session registry +
    /// synchronous working-recipe path (D2) the render seam (E05/E10) and
    /// the shell's gesture wiring (E08) consume, plus the durable command
    /// dispatcher the bus drives. Mirrors the `previews()`/`engine()`
    /// accessor pattern.
    pub fn edits(&self) -> Arc<EditHub> {
        Arc::clone(&self.inner.edit_hub)
    }

    /// The current session working-set snapshot (E04 spec §4.5): a cheap
    /// `Arc` clone, lock-free plain data, no locks, no catalog handles.
    /// `SetPhase::Empty` with no items until the first `Command::
    /// OpenWorkingSet` (headless callers that only ever import/query never
    /// touch this).
    pub fn working_set(&self) -> Arc<WorkingSetSnapshot> {
        self.inner.working_set.snapshot()
    }

    /// Subscribe to the event broadcast. The shell drains this once per
    /// frame; slow subscribers lag, they never wedge the writer (spec §5.1).
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// E08 Phase G (G2): binds `prefs`'s catalog scope to this session's
    /// catalog, loads its `catalog_settings` rows and arms write-through
    /// (spec §6.7 `PrefsStore::bind_catalog`). A `Session` helper because
    /// `Catalog` itself never crosses seam 1 (§2.3: no SQL crosses up), so
    /// the shell cannot call `bind_catalog` directly.
    pub fn bind_prefs(&self, prefs: &mut crate::prefs::PrefsStore) {
        prefs.bind_catalog(&self.inner.catalog);
    }

    /// The preview provider (spec §3.6 seam): the embedded-preview
    /// implementation at M0 (demand-driven, cancellable, request-deduped,
    /// byte-capped LRU per `CoreConfig::preview_cache_bytes`). Callers that
    /// stop polling a ticket must `cancel` it (the grid cancels on
    /// scroll-out, spec §5.3).
    pub fn previews(&self) -> Arc<dyn PreviewProvider> {
        Arc::clone(&self.inner.previews)
    }

    /// The E03 Phase D preview build-scheduler facade (spec §5.2): tickets,
    /// `best_available`, `request`/`set_viewport` (T15), `bulk_build` (T16),
    /// `stats`/`verify_quick`/`purge_all`. Clone-cheap (`Arc` inner).
    /// Additive alongside [`Self::previews`], see `lightbox_preview::
    /// service`'s module doc comment on why these are two independent
    /// facades at M0, not one.
    pub fn preview_service(&self) -> PreviewService {
        self.inner.preview_service.clone()
    }

    /// The render engine (spec §3.4 seam). **E05 Phase F5:** this is now
    /// `lightbox_render::ng::Engine`, the full E05 node-graph engine, not
    /// the E01 seed (which remains at the `lightbox-render` crate root,
    /// unused by the live app; see `docs/plan/epics/E05-deviations.md`).
    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.inner.engine)
    }

    /// The render scheduler (spec §3.7): latest-wins coalescing + the canvas
    /// double-buffer publisher (task F5 M1 integration). The shell subscribes
    /// to [`RenderScheduler::canvas`] for zero-copy composited frames; a
    /// headless caller (CLI) may submit directly through [`Session::engine`]
    /// instead.
    pub fn render_scheduler(&self) -> Arc<RenderScheduler> {
        Arc::clone(&self.inner.scheduler)
    }

    /// The catalog's schema version (all pending migrations were applied at
    /// open). Additive to the frozen §3.8 surface, surfaced for the CLI
    /// (`create`/`check` report it) and diagnostics overlays.
    pub fn schema_version(&self) -> u32 {
        self.inner.catalog.schema_version()
    }

    /// Closes this handle: cancels session-scoped work, drains in-flight
    /// command jobs (bounded by `CoreConfig::close_wait`), runs the
    /// exit-time verified backup per `opts` (spec §3.8).
    ///
    /// Consumes this clone; the underlying catalog/engine shut down when the
    /// last clone drops. Crash safety never depends on `close` being called
    /// (WAL + single-txn invariant); the backup policy is the only thing a
    /// skipped close forgoes.
    pub fn close(self, opts: CloseOpts) -> Result<CloseReport> {
        let inner = self.inner;
        // E09 T8 (spec §4.2 D1 invariant): auto-commit every open gesture
        // BEFORE the drain/backup, an edit is never held only in memory
        // past its gesture commit, so closing (or replacing) the working
        // set can never lose a committed edit. Direct, synchronous calls
        // (D2); no bus round-trip needed since nothing else may begin a new
        // gesture on this session's images once `close` has been called.
        inner.edit_hub.close_all();
        inner.session_cancel.cancel();
        if !inner.in_flight.wait_zero(inner.cfg.close_wait) {
            tracing::warn!(
                target: "lightbox_core",
                "close_wait elapsed with command jobs still in flight; closing anyway"
            );
        }

        let reason = match opts.backup {
            ClosePolicy::Skip => None,
            ClosePolicy::Always => Some("requested"),
            ClosePolicy::Auto => match inner.catalog.newest_backup_time() {
                None => Some("no verified backup exists"),
                Some(taken) => match std::time::SystemTime::now().duration_since(taken) {
                    Ok(age) if age <= inner.cfg.backup_max_age => None,
                    // A future-dated stamp (clock skew) counts as fresh.
                    Err(_) => None,
                    Ok(_) => Some("newest backup exceeds backup_max_age"),
                },
            },
        };
        let backup = match reason {
            None => None,
            Some(reason) => {
                tracing::info!(target: "lightbox_core", reason, "running exit-time verified backup");
                let report = inner.catalog.backup_verified(&BackupOpts {
                    retain: inner.cfg.backup_retain,
                    dest_override: None,
                })?;
                let _ = inner.events.send(Event::BackupFinished {
                    report: report.clone(),
                });
                Some(report)
            }
        };
        tracing::info!(target: "lightbox_core", catalog = %inner.lbdata.display(), "session closed");
        Ok(CloseReport { backup })
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("lbdata", &self.inner.lbdata)
            .finish_non_exhaustive()
    }
}

/// Everything the dispatcher and its spawned jobs need. Holds its own
/// `Arc<Catalog>` (not the `SessionInner`), so the dispatch loop ending
/// when the last `Session` clone drops its sender, is what releases the
/// catalog; a task holding `SessionInner` would keep itself alive forever.
struct DispatchCtx {
    catalog: Arc<Catalog>,
    jobs: Arc<JobSystem>,
    events: broadcast::Sender<Event>,
    session_cancel: CancelToken,
    in_flight: Arc<InFlight>,
    backup_retain: u32,
    edit_hub: Arc<EditHub>,
    preview_service: PreviewService,
    /// E04 (spec §4.5): the working-set model + its loader knobs.
    working_set: Arc<WorkingSetModel>,
    working_set_opts: lightbox_ingest::OpenOptions,
    /// E15 core slice: the render engine `Command::Export` submits against
    /// (the same `Engine` the shell's loupe/canvas and `lightbox-cli render`
    /// use, spec §5.8 seam, one engine, no export-only fork).
    engine: Arc<Engine>,
    /// E10 Phase D (D10): the SAME resolver instance installed on the
    /// engine's compiler, `RemoveLook`'s dispatch calls `invalidate` on it
    /// so a removed look's cache entry doesn't outlive its registry row.
    look_resolver: Arc<CatalogLookResolver>,
}

async fn dispatch_loop(ctx: DispatchCtx, mut rx: mpsc::UnboundedReceiver<Queued>) {
    while let Some(Queued { ticket, cmd }) = rx.recv().await {
        if ctx.session_cancel.is_cancelled() {
            let _ = ctx.events.send(Event::CommandFailed {
                ticket,
                error: "session is closing".to_owned(),
            });
            continue;
        }
        match cmd {
            Command::SetRating { image, rating } => {
                run_txn_command(&ctx, ticket, ChangeSet::image(image), move |txn| {
                    txn.set_rating(image, rating)
                })
                .await;
            }
            Command::SetFlag { image, flag } => {
                run_txn_command(&ctx, ticket, ChangeSet::image(image), move |txn| {
                    txn.set_flag(image, flag)
                })
                .await;
            }
            Command::UndoImport { session } => {
                run_txn_command(&ctx, ticket, ChangeSet::bulk(false), move |txn| {
                    let removed = txn.remove_import_session(session)?;
                    tracing::info!(
                        target: "lightbox_core",
                        session = session.0,
                        assets = removed.assets,
                        images = removed.images,
                        "import undone (files on disk untouched)"
                    );
                    Ok(())
                })
                .await;
            }
            Command::OpenWorkingSet { request } => spawn_open_working_set(&ctx, ticket, request),
            Command::BackupNow => spawn_backup(&ctx, ticket),
            Command::ImportAddInPlace {
                source_dir,
                recursive,
            } => spawn_import(&ctx, ticket, source_dir, recursive),
            // `SyncSettings` can span hundreds of targets (spec §3.8 T27):
            // spawn it as a `Class::Background` job so the queue stays free
            // for trivial commands, matching `BackupNow`/`ImportAddInPlace`.
            // Every OTHER `EditCommand` variant, including `SyncSettings`
            // itself, as a synchronous fallback, is handled by
            // `EditHub::dispatch`, awaited in submission order like
            // `SetRating` (spec T11 AC).
            Command::Edit(EditCommand::SyncSettings {
                source,
                targets,
                subset,
            }) => spawn_edit_sync(&ctx, ticket, source, targets, subset),
            Command::Edit(cmd) => run_edit_command(&ctx, ticket, cmd).await,
            Command::BuildPreviews {
                images,
                tier,
                priority,
            } => dispatch_build_previews(&ctx, images, tier, priority),
            // E03 Phase F (T19/T23): removes preview rows+files. No
            // durable-txn ack (see the command's own doc comment)
            // `Event::PreviewEvicted` (already wired above) is the
            // caller-visible signal.
            Command::DiscardPreviews { images, tiers } => {
                dispatch_discard_previews(&ctx, images, tiers).await;
            }
            // E03 Phase F (T21/T23): a cheap in-memory cap update (two mutex
            // stores, no file IO), no need for the blocking pool.
            Command::SetCacheLimits(limits) => ctx.preview_service.set_limits(limits),
            Command::RelocateCacheStore { new_root } => {
                spawn_relocate_cache_store(&ctx, ticket, new_root);
            }
            Command::PurgeCaches(scope) => spawn_purge_caches(&ctx, ticket, scope),
            Command::Export {
                images,
                dest_dir,
                settings,
            } => spawn_export(&ctx, ticket, images, dest_dir, settings),
            Command::InstallLook { path, family } => {
                run_install_look(&ctx, ticket, path, family).await;
            }
            Command::RemoveLook { id } => run_remove_look(&ctx, ticket, id).await,
        }
    }
    tracing::debug!(target: "lightbox_core", "command dispatcher stopped");
}

/// `Command::DiscardPreviews` (spec §5.6, T19/T23): unlinks preview files +
/// deletes their catalog rows for `images` at `tiers`. Runs on the blocking
/// pool (real file IO + a catalog write, same class of cost as
/// `run_txn_command`'s work) and is **awaited** so dispatch stays in
/// submission order; unlike `run_txn_command` it emits no ack event on
/// success, `PreviewService::discard` cannot fail in a way this seam
/// surfaces (it returns a report, not a `Result`: per-image/tier failures
/// are absorbed and logged inside `evict::discard`, spec §3.2's "caches are
/// disposable" posture), and `Event::PreviewEvicted` (already wired through
/// `Session::open`'s event sink) is the caller-visible signal per row
/// actually reclaimed.
async fn dispatch_discard_previews(
    ctx: &DispatchCtx,
    images: Vec<lightbox_types::ImageId>,
    tiers: lightbox_preview::TierSet,
) {
    if images.is_empty() {
        return;
    }
    let service = ctx.preview_service.clone();
    let _guard = ctx.in_flight.enter();
    let _ = tokio::task::spawn_blocking(move || service.discard(images, tiers)).await;
}

/// `Command::RelocateCacheStore` (spec §5.6, T21/T23): a `Class::Background`
/// job, mirrors `spawn_backup`/`spawn_import` (a large store can take a
/// while to copy, so this must not stall the trivial-command queue).
/// Completion arrives as `Event::CacheRelocated`/`Event::CommandFailed`. The
/// progress sink is a no-op: this command surface has no dedicated
/// progress event (spec §5.6 lists only the completion event); wiring one
/// through is left to whichever phase's UI first needs a live progress bar
/// for a relocation (E08, most likely).
fn spawn_relocate_cache_store(ctx: &DispatchCtx, ticket: CommandTicket, new_root: PathBuf) {
    let service = ctx.preview_service.clone();
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let handle = ctx.jobs.spawn_blocking(
        Class::Background,
        "preview.relocate_cache_store",
        ctx.session_cancel.child(),
        move |_cancel| {
            let _guard = guard;
            let sink: lightbox_preview::ProgressSink = Arc::new(|_p| {});
            match service.relocate(&new_root, sink) {
                Ok(()) => {
                    let _ = events.send(Event::CacheRelocated { ticket, new_root });
                    Ok(())
                }
                Err(err) => {
                    let msg = err.to_string();
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// `Command::PurgeCaches` (spec §5.6, T21/T23): a `Class::Background` job
/// `PurgeScope::All`/`RawCache` can mean clearing gigabytes of on-disk
/// files, the same "don't stall the trivial-command queue" reasoning as
/// `spawn_relocate_cache_store`. Completion arrives as
/// `Event::CachePurged`/`Event::CommandFailed`.
fn spawn_purge_caches(
    ctx: &DispatchCtx,
    ticket: CommandTicket,
    scope: lightbox_preview::PurgeScope,
) {
    let service = ctx.preview_service.clone();
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let handle = ctx.jobs.spawn_blocking(
        Class::Background,
        "preview.purge_caches",
        ctx.session_cancel.child(),
        move |_cancel| {
            let _guard = guard;
            match service.purge(scope) {
                Ok(report) => {
                    let _ = events.send(Event::CachePurged { ticket, report });
                    Ok(())
                }
                Err(err) => {
                    let msg = err.to_string();
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// `Command::BuildPreviews` (spec §5.6, T14/T16): a fast, non-blocking call
/// `PreviewService::request`/`bulk_build` only lock + enqueue (the actual
/// builds run later on the `BuildRuntime`), so unlike `spawn_backup`/
/// `spawn_import` this needs no `spawn_blocking`/job handle. Completion
/// arrives as `Event::PreviewReady`/`PreviewFailed`/`PreviewBulkProgress`
/// (already wired through `Session::open`'s event sink), this command has
/// no separate durable-txn ack (see `Command::BuildPreviews`'s own doc
/// comment).
fn dispatch_build_previews(
    ctx: &DispatchCtx,
    images: Vec<lightbox_types::ImageId>,
    tier: lightbox_preview::Tier,
    priority: lightbox_preview::BuildPriority,
) {
    if images.is_empty() {
        return;
    }
    if priority == lightbox_preview::BuildPriority::Bulk {
        let _handle = ctx.preview_service.bulk_build(images, tier);
        // The handle is intentionally dropped: nothing in this M0 command
        // surface needs to cancel a specific bulk run by ticket/handle after
        // submission (E08's activity center, E06, would be the eventual
        // owner of a cancel-by-handle UI affordance).
    } else {
        for image in images {
            let _ = ctx
                .preview_service
                .request(lightbox_preview::PreviewRequest {
                    image,
                    tier,
                    priority,
                    allow_embedded: true,
                });
        }
    }
}

/// One durable [`EditCommand`] = `EditHub::dispatch`, executed on the
/// blocking pool and **awaited** so edit commands keep submission order
/// (spec T11 AC, mirrors `run_txn_command`). Always emits
/// `Event::CatalogChanged` on success, even a typed no-op (e.g. a second
/// `CommitGesture` with nothing pending), so a caller correlating on the
/// touched image(s) never hangs waiting for a durable step that was never
/// going to land.
async fn run_edit_command(ctx: &DispatchCtx, ticket: CommandTicket, cmd: EditCommand) {
    let hub = Arc::clone(&ctx.edit_hub);
    let _guard = ctx.in_flight.enter();
    let joined = tokio::task::spawn_blocking(move || hub.dispatch(cmd)).await;
    match joined {
        Ok(Ok((events, change))) => {
            for ev in events {
                let _ = ctx.events.send(ev);
            }
            let _ = ctx.events.send(Event::CatalogChanged { change });
        }
        Ok(Err(err)) => {
            tracing::warn!(target: "lightbox_core", ticket = ticket.id(), %err, "edit command failed");
            let _ = ctx.events.send(Event::CommandFailed {
                ticket,
                error: err.to_string(),
            });
        }
        Err(join_err) => {
            let _ = ctx.events.send(Event::CommandFailed {
                ticket,
                error: format!("edit command task failed: {join_err}"),
            });
        }
    }
}

/// `Command::InstallLook` (E10 task D10): hash/validate/copy/register a
/// `.cube`/HaldCLUT file, on the blocking pool (real file IO + a catalog
/// write, same class of cost as `run_txn_command`'s work), **awaited** so
/// installs keep submission order. Emits `Event::CatalogChanged` on success
/// (the D11 look-browser panel refreshes its cached list the same way the
/// presets panel refreshes on preset commands); a malformed/unreadable/
/// unsupported file arrives as `Event::CommandFailed` with
/// `CoreError::InvalidLook`'s message.
async fn run_install_look(
    ctx: &DispatchCtx,
    ticket: CommandTicket,
    path: PathBuf,
    family: Option<String>,
) {
    let catalog = Arc::clone(&ctx.catalog);
    let _guard = ctx.in_flight.enter();
    let joined = tokio::task::spawn_blocking(move || {
        crate::looks::install_look_file(&catalog, &path, family)
    })
    .await;
    let event = match joined {
        Ok(Ok(_)) => Event::CatalogChanged {
            change: ChangeSet::bulk(false),
        },
        Ok(Err(err)) => {
            tracing::warn!(target: "lightbox_core", ticket = ticket.id(), %err, "install_look failed");
            Event::CommandFailed {
                ticket,
                error: err.to_string(),
            }
        }
        Err(join_err) => Event::CommandFailed {
            ticket,
            error: format!("install_look task failed: {join_err}"),
        },
    };
    let _ = ctx.events.send(event);
}

/// `Command::RemoveLook` (E10 task D10): deletes the registry row, then
/// on success, invalidates the SAME [`CatalogLookResolver`] instance the
/// engine's compiler resolves against, so a recipe still referencing the
/// removed hash renders identity on its very next compile (not just after a
/// process restart). Awaited, like `run_install_look`.
async fn run_remove_look(ctx: &DispatchCtx, ticket: CommandTicket, id: LookId) {
    let catalog = Arc::clone(&ctx.catalog);
    let _guard = ctx.in_flight.enter();
    let joined = tokio::task::spawn_blocking(move || crate::looks::remove_look(&catalog, id)).await;
    let event = match joined {
        Ok(Ok(removed_hash)) => {
            if let Some(hash) = removed_hash {
                ctx.look_resolver.invalidate(&hash);
            }
            Event::CatalogChanged {
                change: ChangeSet::bulk(false),
            }
        }
        Ok(Err(err)) => {
            tracing::warn!(target: "lightbox_core", ticket = ticket.id(), %err, "remove_look failed");
            Event::CommandFailed {
                ticket,
                error: err.to_string(),
            }
        }
        Err(join_err) => Event::CommandFailed {
            ticket,
            error: format!("remove_look task failed: {join_err}"),
        },
    };
    let _ = ctx.events.send(event);
}

/// `SyncSettings`: a Background job (spec §3.8/T27, batched sync can span
/// hundreds of targets, so it must not hold the trivial-command queue).
fn spawn_edit_sync(
    ctx: &DispatchCtx,
    ticket: CommandTicket,
    source: lightbox_types::ImageId,
    targets: Vec<lightbox_types::ImageId>,
    subset: lightbox_edit::ParamSubset,
) {
    let hub = Arc::clone(&ctx.edit_hub);
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let handle = ctx.jobs.spawn_blocking(
        Class::Background,
        "edit.sync_settings",
        ctx.session_cancel.child(),
        move |_cancel| {
            let _guard = guard;
            let cmd = EditCommand::SyncSettings {
                source,
                targets,
                subset,
            };
            match hub.dispatch(cmd) {
                Ok((evs, change)) => {
                    for ev in evs {
                        let _ = events.send(ev);
                    }
                    let _ = events.send(Event::CatalogChanged { change });
                    Ok(())
                }
                Err(err) => {
                    let msg = err.to_string();
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// One trivial command = one WAL transaction on the single writer, executed
/// on the blocking pool (the writer call blocks), **awaited** so trivial
/// commands keep submission order (T16).
async fn run_txn_command<F>(ctx: &DispatchCtx, ticket: CommandTicket, change: ChangeSet, f: F)
where
    F: FnOnce(&mut CatalogTxn<'_>) -> lightbox_catalog::Result<()> + Send + 'static,
{
    let writer = ctx.catalog.writer();
    let _guard = ctx.in_flight.enter();
    let joined = tokio::task::spawn_blocking(move || writer.with_txn(f)).await;
    let event = match joined {
        Ok(Ok(())) => Event::CatalogChanged { change },
        Ok(Err(err)) => {
            tracing::warn!(target: "lightbox_core", ticket = ticket.id(), %err, "command failed");
            Event::CommandFailed {
                ticket,
                error: err.to_string(),
            }
        }
        Err(join_err) => Event::CommandFailed {
            ticket,
            error: format!("command task failed: {join_err}"),
        },
    };
    let _ = ctx.events.send(event);
}

/// `BackupNow`: a Background job, concurrent-safe against the writer (the
/// online-backup API snapshots), so it does not hold the command queue.
fn spawn_backup(ctx: &DispatchCtx, ticket: CommandTicket) {
    let catalog = Arc::clone(&ctx.catalog);
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let opts = BackupOpts {
        retain: ctx.backup_retain,
        dest_override: None,
    };
    let handle = ctx.jobs.spawn_blocking(
        Class::Background,
        "catalog.backup",
        ctx.session_cancel.child(),
        move |_cancel| {
            let _guard = guard;
            match catalog.backup_verified(&opts) {
                Ok(report) => {
                    let _ = events.send(Event::BackupFinished { report });
                    Ok(())
                }
                Err(err) => {
                    let msg = err.to_string();
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// `OpenWorkingSet` (E04 spec §4.5/§6.6): the epoch bump + previous-epoch
/// cancellation happen **synchronously here**, in the dispatcher, before the
/// job is even spawned, so "a new `OpenWorkingSet` cancels the in-flight
/// one before bumping the epoch" holds even though `dispatch_loop` never
/// blocks waiting for the old job to actually stop. A `Class::Foreground`
/// job (mirrors `spawn_import`) named `"working_set.open"`, running phase 1
/// ([`plan_open`]) then phase 2 ([`load_working_set`]) back to back on the
/// blocking pool; every state change lands in the model via `WorkingSetModel
/// ::apply_plan`/`apply_load_event`/`finish`, each epoch-gated so a
/// straggling callback from an already-cancelled job can never mutate a
/// newer epoch's snapshot (R6).
fn spawn_open_working_set(ctx: &DispatchCtx, ticket: CommandTicket, request: OpenRequest) {
    if request.paths.is_empty() {
        // Reject before touching the model/epoch at all, an empty request
        // is a caller bug, not a "nothing to load" open (spec: `plan_open`
        // itself refuses this too; the dispatcher pre-checks to avoid
        // leaving the model stuck mid-epoch for a request that was never
        // going to plan).
        let _ = ctx.events.send(Event::CommandFailed {
            ticket,
            error: "empty open request".to_owned(),
        });
        return;
    }

    let (epoch, cancel) = ctx.working_set.begin_epoch(&ctx.session_cancel);
    let _ = ctx.events.send(Event::WorkingSetOpening { epoch, ticket });

    let writer = ctx.catalog.writer();
    let events = ctx.events.clone();
    let model = Arc::clone(&ctx.working_set);
    let opts = ctx.working_set_opts.clone();
    let guard = ctx.in_flight.enter();
    let handle = ctx.jobs.spawn_blocking(
        Class::Foreground,
        "working_set.open",
        cancel,
        move |cancel| {
            let _guard = guard;
            let plan = match plan_open(&request, &opts, cancel, &mut |_progress| {
                // Plan-phase progress has no dedicated core event (spec
                // §4.5 names only Opening/Replaced/Changed/LoadFinished);
                // phase 1 is fast by design (§7) so a throttled heartbeat
                // here would have nothing to say that WorkingSetReplaced
                // doesn't already say a few ms later.
            }) {
                Ok(p) => p,
                // A cancelled plan (superseded by a newer epoch) is a
                // normal outcome, not a command failure, the newer
                // epoch's own events are what the caller sees instead.
                Err(OpenError::Cancelled) => return Err(JobError::Cancelled),
                Err(e) => {
                    let msg = e.to_string();
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    return Err(JobError::Failed(msg));
                }
            };
            model.apply_plan(epoch, &plan);
            let _ = events.send(Event::WorkingSetReplaced {
                epoch,
                planned: plan.items.len(),
                truncated: plan.truncated,
            });

            let load_events = events.clone();
            let load_model = Arc::clone(&model);
            let mut on_event = move |ev: LoadEvent| {
                load_model.apply_load_event(epoch, &ev);
                // Coalesce Event::WorkingSetChanged onto the loader's own
                // throttled Progress heartbeat (spec §4.5: "coalesced ≤ 1
                // per progress_min_interval") rather than re-implementing
                // throttling here.
                if matches!(ev, LoadEvent::Progress { .. }) {
                    let _ = load_events.send(Event::WorkingSetChanged { epoch });
                }
            };
            match load_working_set(&writer, &plan, &opts, cancel, &mut on_event) {
                Ok(report) => {
                    model.finish(epoch);
                    let _ = events.send(Event::WorkingSetLoadFinished { epoch, report });
                    // Compat hint for the not-yet-reworked M0 grid (spec
                    // §3.1 phase 3), E08 removes its listener once it
                    // drives the filmstrip from the working-set snapshot.
                    let _ = events.send(Event::CatalogChanged {
                        change: ChangeSet::bulk(false),
                    });
                    Ok(())
                }
                Err(e) => {
                    let msg = e.to_string();
                    model.finish(epoch);
                    let _ = events.send(Event::CommandFailed {
                        ticket,
                        error: msg.clone(),
                    });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// `ImportAddInPlace`: a Foreground job (spec T17). The queue stays free for
/// trivial commands while the import runs; the import's own batches
/// interleave with them on the single writer.
fn spawn_import(ctx: &DispatchCtx, ticket: CommandTicket, source_dir: PathBuf, recursive: bool) {
    let writer = ctx.catalog.writer();
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let mut opts = ImportOptions::default();
    opts.recursive = recursive;
    let handle = ctx.jobs.spawn_blocking(
        Class::Foreground,
        "import.add_in_place",
        ctx.session_cancel.child(),
        move |cancel| {
            let _guard = guard;
            let progress_events = events.clone();
            let mut on_event = move |ev: ImportEvent| {
                let event = match ev {
                    ImportEvent::Started { session, .. } => Event::ImportStarted { session, ticket },
                    ImportEvent::Progress {
                        session,
                        done,
                        discovered,
                        current,
                    } => Event::ImportProgress {
                        session,
                        done,
                        discovered,
                        current,
                    },
                    // ImportEvent is #[non_exhaustive]; future ingest events
                    // get core translations when they get core meaning.
                    _ => return,
                };
                let _ = progress_events.send(event);
            };
            match import_add_in_place(&writer, &source_dir, &opts, cancel, &mut on_event) {
                Ok(outcome) => {
                    let _ = events.send(Event::ImportFinished {
                        session: outcome.session,
                        report: outcome.report,
                    });
                    let _ = events.send(Event::CatalogChanged {
                        change: ChangeSet::bulk(true),
                    });
                    Ok(())
                }
                Err(err) => {
                    let msg = err.to_string();
                    tracing::warn!(target: "lightbox_core", ticket = ticket.id(), %msg, "import failed");
                    let _ = events.send(Event::CommandFailed { ticket, error: msg.clone() });
                    Err(JobError::Failed(msg))
                }
            }
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// `Command::Export` (E15 core slice, spec §5.10 narrowed): a `Class::
/// Foreground` job (mirrors `spawn_import`) that resolves each image's
/// recipe/dimensions/output path, then drives
/// [`lightbox_export::run::export_batch`] with bounded concurrency.
/// `Event::ExportStarted` fires once resolution is done; `Event::
/// ExportProgress` fires per item; `Event::ExportFinished` closes the
/// batch.
fn spawn_export(
    ctx: &DispatchCtx,
    ticket: CommandTicket,
    images: Vec<ImageId>,
    dest_dir: PathBuf,
    settings: lightbox_export::ExportSettings,
) {
    if let Err(err) = settings.validate() {
        let _ = ctx.events.send(Event::CommandFailed {
            ticket,
            error: err.to_string(),
        });
        return;
    }

    let catalog = Arc::clone(&ctx.catalog);
    let edit_hub = Arc::clone(&ctx.edit_hub);
    let engine = Arc::clone(&ctx.engine);
    let events = ctx.events.clone();
    let guard = ctx.in_flight.enter();
    let cancel = ctx.session_cancel.child();
    let concurrency = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .min(4);

    let handle = ctx.jobs.spawn(
        Class::Foreground,
        "export.run",
        cancel.clone(),
        async move {
            let _guard = guard;
            let resolve_cancel = cancel.clone();
            let resolve_settings = settings.clone();
            let (resolved, mut failed) = tokio::task::spawn_blocking(move || {
                resolve_export_items(
                    &catalog,
                    &edit_hub,
                    &images,
                    &dest_dir,
                    &resolve_settings,
                    &resolve_cancel,
                )
            })
            .await
            .unwrap_or_else(|join_err| {
                (
                    Vec::new(),
                    vec![(
                        ImageId(0),
                        format!("export resolution task failed: {join_err}"),
                    )],
                )
            });

            let total = (resolved.len() + failed.len()) as u32;
            let _ = events.send(Event::ExportStarted { ticket, total });

            let progress_events = events.clone();
            let on_item: Arc<dyn Fn(lightbox_export::ExportItemDone) + Send + Sync> =
                Arc::new(move |done: lightbox_export::ExportItemDone| {
                    let _ = progress_events.send(Event::ExportProgress {
                        ticket,
                        image: done.image,
                        out_path: done.out_path,
                        error: done.error,
                        done: done.done,
                        total: done.total,
                    });
                });

            let mut report = lightbox_export::run::export_batch(
                engine,
                resolved,
                settings,
                concurrency,
                cancel,
                on_item,
            )
            .await;
            // Immediate resolution failures (e.g. an unknown image id)
            // never entered the batch driver's own progress stream, fold
            // them into the same report so `total == ok + failed.len()`
            // holds from the caller's point of view.
            report.failed.append(&mut failed);

            let _ = events.send(Event::ExportFinished { ticket, report });
            Ok(())
        },
    );
    drop(handle); // detached; close() drains via the in-flight guard
}

/// Resolves each image to a full [`lightbox_export::ExportItem`] (recipe,
/// full extent, destination path) or an immediate per-item failure (missing
/// image, unreadable edit state), run on the blocking pool (catalog
/// reads). Pure: no pixels move here (spec §5.2 `plan`'s preflight spirit,
/// narrowed to what a session-less helper can check).
fn resolve_export_items(
    catalog: &Catalog,
    edit_hub: &EditHub,
    images: &[ImageId],
    dest_dir: &Path,
    settings: &lightbox_export::ExportSettings,
    cancel: &CancelToken,
) -> (Vec<lightbox_export::ExportItem>, Vec<(ImageId, String)>) {
    let reader = catalog.reader();
    let mut resolved = Vec::with_capacity(images.len());
    let mut failed = Vec::new();
    for &image in images {
        if cancel.is_cancelled() {
            failed.push((image, "export cancelled".to_owned()));
            continue;
        }
        let detail = match reader.image_detail(image) {
            Ok(d) => d,
            Err(err) => {
                failed.push((image, format!("image detail: {err}")));
                continue;
            }
        };
        let state = match edit_hub.store().open_state(image) {
            Ok(s) => s,
            Err(err) => {
                failed.push((image, format!("edit state: {err}")));
                continue;
            }
        };
        let stem = lightbox_export::settings::stem_of(Path::new(&detail.filename));
        let file_name = settings.naming.file_name(&stem, settings.format);
        resolved.push(lightbox_export::ExportItem {
            image,
            pv: state.recipe.pv,
            recipe: state.recipe,
            full_extent: Extent {
                w: detail.width.max(1),
                h: detail.height.max(1),
            },
            out_path: dest_dir.join(file_name),
        });
    }
    (resolved, failed)
}

/// Counts in-flight command jobs so [`Session::close`] can drain them
/// (cancelled imports still need the writer to flush their final stats
/// before the exit backup runs).
#[derive(Default)]
struct InFlight {
    count: Mutex<usize>,
    zero: Condvar,
}

impl InFlight {
    fn enter(self: &Arc<Self>) -> InFlightGuard {
        *self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        InFlightGuard {
            in_flight: Arc::clone(self),
        }
    }

    /// True when the count reached zero within `timeout`.
    fn wait_zero(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut count = self
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *count > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (guard, _timed_out) = self
                .zero
                .wait_timeout(count, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            count = guard;
        }
        true
    }
}

struct InFlightGuard {
    in_flight: Arc<InFlight>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut count = self
            .in_flight
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.in_flight.zero.notify_all();
        }
    }
}
