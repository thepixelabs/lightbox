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
use lightbox_ingest::ImportReport;
use lightbox_meta::xmp::sync::DivergenceStatus;
use lightbox_types::{AssetId, ImageId, ImportSessionId};

use crate::command::CommandTicket;

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
}
