// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Event`] — the broadcast side of seam 1 (spec §3.8).
//!
//! Events are fanned out over a `tokio::sync::broadcast` channel; the shell
//! drains its receiver once per frame (spec §5.1 threading contract). A slow
//! subscriber lags (observing `RecvError::Lagged`) — it never back-pressures
//! the writer.

use std::path::PathBuf;

use lightbox_catalog::BackupReport;
use lightbox_edit::StepLabel;
use lightbox_ingest::{ImportReport, OpenReport};
use lightbox_jobs::JobEvent;
use lightbox_meta::xmp::sync::DivergenceStatus;
use lightbox_preview::{CacheKind, PreviewDesc, PreviewError, PurgeReport, Tier};
use lightbox_types::{AssetId, ImageId, ImportSessionId};

use crate::command::CommandTicket;
use crate::working_set::SetEpoch;

/// Coarse invalidation hint (spec §3.8 M0: "folders/images invalidation
/// hints"). E07 refines granularity.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ChangeSet {
    /// Specific images whose rows changed (empty when `all_images`).
    pub images: Vec<ImageId>,
    /// The folder tree may have changed shape.
    pub folders: bool,
    /// Broad invalidation: any image list may have changed (import, undo).
    pub all_images: bool,
}

impl ChangeSet {
    /// A single-image change.
    pub fn image(id: ImageId) -> ChangeSet {
        ChangeSet {
            images: vec![id],
            ..ChangeSet::default()
        }
    }

    /// Bulk change: image lists (and optionally the folder tree) must be
    /// re-queried.
    pub fn bulk(folders: bool) -> ChangeSet {
        ChangeSet {
            images: Vec::new(),
            folders,
            all_images: true,
        }
    }
}

/// What the core broadcasts (spec §3.8, `#[non_exhaustive]`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Event {
    /// A new `OpenWorkingSet` gesture was accepted (E04 spec §4.5); the
    /// previous epoch is cancelled and the model shows `SetPhase::Planning`
    /// for `epoch`.
    WorkingSetOpening {
        /// The new generation.
        epoch: SetEpoch,
        /// The command that started it.
        ticket: CommandTicket,
    },
    /// Phase-1 complete: the ordered set is known; the filmstrip can render
    /// (E04 spec §3.1/§4.5).
    WorkingSetReplaced {
        /// The generation this replaces.
        epoch: SetEpoch,
        /// Items in the plan.
        planned: usize,
        /// `true` when `OpenOptions::max_set_size` was hit.
        truncated: bool,
    },
    /// Item states changed (coalesced ≤ 1 per `OpenOptions::
    /// progress_min_interval`, piggy-backing the loader's own throttled
    /// progress heartbeat); poll `Session::working_set()` for the new
    /// snapshot.
    WorkingSetChanged {
        /// The generation that changed.
        epoch: SetEpoch,
    },
    /// Phase-2 complete (also emitted when a load is cancelled by
    /// replacement — the report covers what actually landed).
    WorkingSetLoadFinished {
        /// The generation this concludes.
        epoch: SetEpoch,
        /// What happened.
        report: OpenReport,
    },
    /// An import session opened (discovery complete, rows incoming).
    ImportStarted {
        /// The bracketing `import_session` row.
        session: ImportSessionId,
        /// The command that started it.
        ticket: CommandTicket,
    },
    /// Import heartbeat, throttled ≤ 10 Hz (spec T17).
    ImportProgress {
        /// The session this import runs under.
        session: ImportSessionId,
        /// Files processed so far.
        done: u64,
        /// Files discovered in total.
        discovered: u64,
        /// The file just processed.
        current: PathBuf,
    },
    /// The import finished (also emitted for cancelled imports — the
    /// report covers what actually landed).
    ImportFinished {
        /// The finished session.
        session: ImportSessionId,
        /// What happened.
        report: ImportReport,
    },
    /// Catalog rows changed; invalidate per the hint.
    CatalogChanged {
        /// What changed, coarsely.
        change: ChangeSet,
    },
    /// A command failed; nothing was committed (single-txn invariant).
    CommandFailed {
        /// The failing command's ticket.
        ticket: CommandTicket,
        /// Human-readable reason.
        error: String,
    },
    /// A verified backup completed (`Command::BackupNow` or exit-time).
    BackupFinished {
        /// Where it landed, size, duration.
        report: BackupReport,
    },
    /// The GPU device degraded/was lost (M0: degenerate handling, spec
    /// §3.4; the full rebuild harness is E05.5).
    DeviceDegraded {
        /// Driver/backend message.
        reason: String,
    },
    /// The in-memory working recipe changed (spec §3.4 — a gesture update).
    /// **Not durable.** Fired synchronously, in the same call stack as
    /// `EditHub::update_gesture` (no bus round-trip — D2); the render
    /// scheduler re-renders from `EditHub::working_recipe` on this signal.
    EditWorkingChanged {
        /// The image whose working recipe changed.
        image: ImageId,
    },
    /// A durable edit txn committed (spec §3.4): a gesture, preset apply,
    /// paste, `StepTo`/undo/redo, snapshot restore, or XMP read.
    EditCommitted {
        /// The image the step landed on.
        image: ImageId,
        /// The new history position.
        seq: u64,
        /// The step's label.
        label: StepLabel,
    },
    /// Sidecar divergence status changed for an asset (spec §3.4/§3.5):
    /// recomputed at open, after write/read, or on `RefreshXmpStatus` —
    /// never from a background watcher (spec §0 item 6).
    XmpDivergenceChanged {
        /// The asset whose sidecar status changed.
        asset: AssetId,
        /// The new status.
        status: DivergenceStatus,
    },
    /// E03 Phase D (T14, spec §5.6): a preview build completed. Re-published
    /// verbatim from [`lightbox_preview::PreviewEvent::Ready`] (via
    /// `Session::open`'s wiring of `PreviewService`'s event sink onto this
    /// bus).
    PreviewReady {
        /// The image the build was for.
        image: ImageId,
        /// Which pyramid tier.
        tier: Tier,
        /// The resulting descriptor (store path, dims, variant, …).
        desc: PreviewDesc,
    },
    /// A preview build failed (spec §5.6). Re-published from
    /// [`lightbox_preview::PreviewEvent::Failed`].
    PreviewFailed {
        /// The image the build was for.
        image: ImageId,
        /// Which pyramid tier.
        tier: Tier,
        /// Why.
        error: PreviewError,
    },
    /// Progress for a [`Command::BuildPreviews`](crate::Command::BuildPreviews)
    /// bulk run (T16). Re-published from
    /// [`lightbox_preview::PreviewEvent::BulkProgress`] — carries no session
    /// id (see that variant's doc comment); at most one concurrently
    /// observed bulk run is disambiguated by this event stream alone.
    PreviewBulkProgress {
        /// Builds completed so far in this run.
        done: u64,
        /// Total builds in this run.
        total: u64,
    },
    /// E03 Phase F (T19, spec §5.2): a preview row (+ its file, if no other
    /// row still referenced it) was reclaimed by cap-based eviction, the T2
    /// retention sweep, or `DiscardPreviews`. Re-published from
    /// [`lightbox_preview::PreviewEvent::Evicted`].
    PreviewEvicted {
        /// The image the reclaimed row belonged to.
        image: ImageId,
        /// Which pyramid tier.
        tier: Tier,
    },
    /// E03 Phase F (T21, spec §5.2): a build (preview or raw-cache) hit disk
    /// pressure. Re-published from
    /// [`lightbox_preview::PreviewEvent::CachePressure`].
    CachePressure {
        /// Which cache.
        kind: CacheKind,
        /// Currently accounted bytes.
        used_bytes: u64,
        /// The configured cap.
        cap_bytes: u64,
    },
    /// `Command::RelocateCacheStore` finished (spec §5.6, T21).
    CacheRelocated {
        /// The command's ticket.
        ticket: CommandTicket,
        /// The new cache-store root.
        new_root: PathBuf,
    },
    /// `Command::PurgeCaches` finished (spec §5.6, T21).
    CachePurged {
        /// The command's ticket.
        ticket: CommandTicket,
        /// What was actually removed.
        report: PurgeReport,
    },
    /// E06 (spec §4.7): a scheduler event, forwarded verbatim from
    /// `lightbox_jobs::Scheduler::subscribe` by the session's relay task.
    /// Already throttled at the source (`Progress` ticks are coalesced to
    /// `snapshot_publish_hz`); the shell needs only the
    /// `Session::activity()` snapshot — this variant serves
    /// `lightbox-cli jobs watch`, tests, and edge-triggered consumers.
    Jobs(JobEvent),
}
