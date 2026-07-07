// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Core`] / [`Session`] — the headless boundary itself (spec §3.8, seam 1).
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
use lightbox_ingest::{import_add_in_place, ImportEvent, ImportOptions};
use lightbox_jobs::{CancelToken, Class, JobError, JobSystem};
use lightbox_preview::{
    AssetLocator, EmbeddedPreviewProvider, PreviewProvider, PreviewStoreConfig, Store,
};
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
use lightbox_render::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
use lightbox_render::ng::{
    BackendPref, DeviceProvider, Engine, EngineConfig, EngineEvent, JobsHandle, NodeRegistry,
    PvRange, RenderScheduler, SourceProvider,
};
use lightbox_render::GpuContext;
use lightbox_types::PV_M0;
use tokio::sync::{broadcast, mpsc};

use crate::command::{Command, CommandTicket, EditCommand};
use crate::config::CoreConfig;
use crate::edit_hub::EditHub;
use crate::error::Result;
use crate::event::{ChangeSet, Event};
use crate::previews::CatalogAssetLocator;
use crate::queries::Queries;
use crate::render_source::{NullDeviceProvider, PreviewSourceProvider, SharedDeviceProvider};

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

    /// The core's job system — for embedders that need to schedule their own
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
    /// `ClosePolicy::Skip.into()` — spec §3.8).
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
    edit_hub: Arc<EditHub>,
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
        // alongside the catalog (spec §3.2 — same directory, not a sibling
        // tree); `Store::open` creates/adopts its reserved subdirectories
        // idempotently (Phase A, T01).
        let preview_store = Arc::new(Store::open(&PreviewStoreConfig::with_defaults(
            lbdata.clone(),
        ))?);

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
        // the `ng` engine — the loupe/canvas is produced by
        // `RenderScheduler`/`Engine::submit` through `lightbox-render::ng`,
        // never a blit path that bypasses the engine (spec §9 M0 verbatim
        // requirement, now satisfied by the E05 engine rather than the E01
        // seed — see `docs/plan/epics/E05-deviations.md`).
        let mut registry = NodeRegistry::new();
        registry
            .register(
                SrcDecodedNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(SrcDecodedFactory::default()),
            )
            .map_err(|e| crate::error::CoreError::Internal(format!("node registry: {e}")))?;
        registry
            .register(
                UtilResizeNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(UtilResizeFactory::default()),
            )
            .map_err(|e| crate::error::CoreError::Internal(format!("node registry: {e}")))?;
        registry
            .register(
                XformDisplayNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(XformDisplayFactory::default()),
            )
            .map_err(|e| crate::error::CoreError::Internal(format!("node registry: {e}")))?;

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
        let engine = Arc::new(Engine::new(
            device_provider,
            source_provider,
            registry,
            EngineConfig {
                backend,
                ..EngineConfig::default()
            },
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

        // E09 T8: the edit-state session registry (wraps `EditStore`, itself
        // just an `Arc<Catalog>` handle — cheap). The preset store underneath
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

        let ctx = DispatchCtx {
            catalog: Arc::clone(&catalog),
            jobs: Arc::clone(&core.jobs),
            events: events.clone(),
            session_cancel: session_cancel.clone(),
            in_flight: Arc::clone(&in_flight),
            backup_retain: core.cfg.backup_retain,
            edit_hub: Arc::clone(&edit_hub),
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
                edit_hub,
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

    /// A snapshot view for queries — executes on the calling thread against
    /// a pooled WAL-snapshot read connection (spec §5.1). Drop it promptly.
    pub fn query(&self) -> Queries {
        Queries::new(
            self.inner.catalog.reader(),
            Arc::clone(&self.inner.edit_hub),
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

    /// Subscribe to the event broadcast. The shell drains this once per
    /// frame; slow subscribers lag, they never wedge the writer (spec §5.1).
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// The preview provider (spec §3.6 seam): the embedded-preview
    /// implementation at M0 (demand-driven, cancellable, request-deduped,
    /// byte-capped LRU per `CoreConfig::preview_cache_bytes`). Callers that
    /// stop polling a ticket must `cancel` it (the grid cancels on
    /// scroll-out, spec §5.3).
    pub fn previews(&self) -> Arc<dyn PreviewProvider> {
        Arc::clone(&self.inner.previews)
    }

    /// The render engine (spec §3.4 seam). **E05 Phase F5:** this is now
    /// `lightbox_render::ng::Engine` — the full E05 node-graph engine, not
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
    /// open). Additive to the frozen §3.8 surface — surfaced for the CLI
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
        // BEFORE the drain/backup — an edit is never held only in memory
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
/// `Arc<Catalog>` (not the `SessionInner`), so the dispatch loop ending —
/// when the last `Session` clone drops its sender — is what releases the
/// catalog; a task holding `SessionInner` would keep itself alive forever.
struct DispatchCtx {
    catalog: Arc<Catalog>,
    jobs: Arc<JobSystem>,
    events: broadcast::Sender<Event>,
    session_cancel: CancelToken,
    in_flight: Arc<InFlight>,
    backup_retain: u32,
    edit_hub: Arc<EditHub>,
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
            Command::BackupNow => spawn_backup(&ctx, ticket),
            Command::ImportAddInPlace {
                source_dir,
                recursive,
            } => spawn_import(&ctx, ticket, source_dir, recursive),
            // `SyncSettings` can span hundreds of targets (spec §3.8 T27):
            // spawn it as a `Class::Background` job so the queue stays free
            // for trivial commands, matching `BackupNow`/`ImportAddInPlace`.
            // Every OTHER `EditCommand` variant — including `SyncSettings`
            // itself, as a synchronous fallback — is handled by
            // `EditHub::dispatch`, awaited in submission order like
            // `SetRating` (spec T11 AC).
            Command::Edit(EditCommand::SyncSettings {
                source,
                targets,
                subset,
            }) => spawn_edit_sync(&ctx, ticket, source, targets, subset),
            Command::Edit(cmd) => run_edit_command(&ctx, ticket, cmd).await,
        }
    }
    tracing::debug!(target: "lightbox_core", "command dispatcher stopped");
}

/// One durable [`EditCommand`] = `EditHub::dispatch`, executed on the
/// blocking pool and **awaited** so edit commands keep submission order
/// (spec T11 AC, mirrors `run_txn_command`). Always emits
/// `Event::CatalogChanged` on success — even a typed no-op (e.g. a second
/// `CommitGesture` with nothing pending) — so a caller correlating on the
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

/// `SyncSettings`: a Background job (spec §3.8/T27 — batched sync can span
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

/// `BackupNow`: a Background job — concurrent-safe against the writer (the
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
