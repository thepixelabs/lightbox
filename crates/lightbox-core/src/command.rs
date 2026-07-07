// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Command`] — the mutation vocabulary of seam 1 (spec §3.8).
//!
//! Every command executes as **one WAL transaction** on the catalog's
//! single writer (architecture §3.1); results come back asynchronously as
//! broadcast [`crate::Event`]s correlated by [`CommandTicket`].

use std::path::PathBuf;

use lightbox_edit::{ParamSubset, PresetId};
use lightbox_preview::{BuildPriority, Tier};
use lightbox_types::{Flag, ImageId, ImportSessionId, SnapshotId};

/// Correlates a submitted command with its outcome events. Allocated by
/// [`crate::Session::submit`]; unique within a session.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CommandTicket(pub(crate) u64);

impl CommandTicket {
    /// The session-unique id.
    pub fn id(self) -> u64 {
        self.0
    }
}

/// Mutations the shell/CLI may request (spec §3.8, `#[non_exhaustive]` —
/// E04/E07/E09 grow this).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Add-in-place import of a directory (spec §1 item 4). Progress and
    /// completion arrive as `Import*` events.
    ImportAddInPlace {
        /// Directory to import.
        source_dir: PathBuf,
        /// Descend into subdirectories.
        recursive: bool,
    },
    /// Remove an import session's catalog rows; files on disk untouched.
    UndoImport {
        /// The session to undo.
        session: ImportSessionId,
    },
    /// Set or clear the 1..=5 star rating — the canonical trivial command
    /// proving the txn+event path (spec §3.8; E08 grows the UX).
    SetRating {
        /// Target image.
        image: ImageId,
        /// `None` clears.
        rating: Option<u8>,
    },
    /// Set the pick/reject flag.
    SetFlag {
        /// Target image.
        image: ImageId,
        /// The new flag.
        flag: Flag,
    },
    /// Run a verified backup now (spec §3.2 pipeline); completion arrives
    /// as `Event::BackupFinished`.
    BackupNow,
    /// An E09 edit-state mutation (spec §3.4 `EditCommand`). Gesture
    /// *updates* never ride this bus (D2, `crate::EditHub::update_gesture`
    /// — direct, synchronous, in-memory); every variant here is a durable
    /// mutation, dispatched as one WAL txn each (ordered, like `SetRating`)
    /// except `SyncSettings`, which spawns as a `Class::Background` job.
    Edit(EditCommand),
    /// E03 Phase D (T14/T16, spec §5.6): enqueues preview builds for
    /// `images` at `tier`/`priority` through [`crate::Session::preview_service`].
    /// `priority == BuildPriority::Bulk` drives a tracked
    /// [`lightbox_preview::PreviewService::bulk_build`] run (progress
    /// arrives as `Event::PreviewBulkProgress`); any other priority issues
    /// one `PreviewService::request` per image. Completion/failure of each
    /// individual build arrives as `Event::PreviewReady`/`PreviewFailed` —
    /// this command itself has no separate durable-txn ack (mirrors
    /// `BackupNow`/`ImportAddInPlace`'s progress-event shape, not
    /// `SetRating`'s single-ack shape).
    BuildPreviews {
        /// Targets.
        images: Vec<ImageId>,
        /// Which pyramid tier to build.
        tier: Tier,
        /// Scheduling priority (spec §3.4: Visible > Neighbor > Bulk).
        priority: BuildPriority,
    },
}

/// E09 edit-state mutations (spec §3.4). Every variant is durable: it lands
/// as a `history_step` (or snapshot/preset/xmp-sync row) through
/// [`crate::EditHub`]. `#[non_exhaustive]`: E10/E12/E14 grow this.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum EditCommand {
    /// Drains the image's pending gesture (built by `EditHub::begin_gesture`
    /// and `update_gesture`) into one durable `history_step`. A typed no-op
    /// when no gesture is open, or the gesture produced no net change
    /// (spec §3.3 `EditSession::take_commit`).
    CommitGesture {
        /// Target image.
        image: ImageId,
    },
    /// Applies a develop preset's delta to each target as one history step
    /// per image (spec §3.4/T25).
    ApplyPreset {
        /// Targets.
        images: Vec<ImageId>,
        /// The preset to apply.
        preset: PresetId,
    },
    /// Pastes the core-held copy buffer (`EditHub::copy_settings`) onto each
    /// target as one history step per image.
    PasteSettings {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Batched sync-to-many (spec §3.8/T27): `source`'s `subset` onto every
    /// target, ~64/txn, one history step per target.
    SyncSettings {
        /// The settings source.
        source: ImageId,
        /// Targets.
        targets: Vec<ImageId>,
        /// Which param groups to carry.
        subset: ParamSubset,
    },
    /// Applies the last-committed source's full settings ("Previous", spec
    /// §3.8) to each target.
    ApplyPrevious {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Resets each target to its neutral default, one step per image.
    ResetEdits {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Restores the recipe as of history position `seq` (spec §3.3
    /// `history::step_to`); later steps are kept until the next real edit
    /// truncates them.
    StepTo {
        /// Target image.
        image: ImageId,
        /// The history position to restore.
        seq: u64,
    },
    /// `StepTo(head_seq - 1)` sugar.
    Undo {
        /// Target image.
        image: ImageId,
    },
    /// `StepTo(head_seq + 1)` sugar (a no-op past the latest kept step).
    Redo {
        /// Target image.
        image: ImageId,
    },
    /// Drops the history log but keeps the current doc (spec §3.3 `clear`).
    ClearHistory {
        /// Target image.
        image: ImageId,
    },
    /// Materializes the current recipe as a named snapshot.
    CreateSnapshot {
        /// Target image.
        image: ImageId,
        /// Display name (unique per image).
        name: String,
    },
    /// Restores a named snapshot onto `image` as a history step.
    RestoreSnapshot {
        /// Target image.
        image: ImageId,
        /// The snapshot to restore.
        snapshot: SnapshotId,
    },
    /// Deletes a snapshot.
    DeleteSnapshot {
        /// The snapshot's image (informational; the id alone is sufficient
        /// to locate the row).
        image: ImageId,
        /// The snapshot to delete.
        snapshot: SnapshotId,
    },
    /// Renames a snapshot.
    RenameSnapshot {
        /// The snapshot's image (informational, see `DeleteSnapshot`).
        image: ImageId,
        /// The snapshot to rename.
        snapshot: SnapshotId,
        /// The new display name.
        name: String,
    },
    /// Explicit sidecar → recipe (spec §4.2 "Project"/read): applies as an
    /// undoable `StepLabel::XmpRead` history step. Never automatic.
    ReadMetadata {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Explicit recipe → sidecar (spec §4.2 "Project"/write): `to_xmp` →
    /// atomic sidecar write → `xmp_sync` stamp txn.
    WriteMetadata {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Toggles the opt-in auto-write-XMP preference (default off; spec
    /// §3.1.1). Held in-process (app prefs are E08 territory, not the
    /// catalog).
    SetAutoWriteXmp {
        /// The new preference value.
        enabled: bool,
    },
    /// Recomputes sidecar divergence status (no fs-watcher, spec §0 item 6):
    /// `Event::XmpDivergenceChanged` per asset.
    RefreshXmpStatus {
        /// Targets.
        images: Vec<ImageId>,
    },
    /// Creates a develop preset from `from`'s current recipe, carrying only
    /// `subset`'s groups.
    CreatePreset {
        /// The recipe to extract from.
        from: ImageId,
        /// Display name.
        name: String,
        /// Group (folder), or `None` for the ungrouped root.
        group: Option<String>,
        /// Which param groups to carry.
        subset: ParamSubset,
    },
    /// Imports Lightbox- or Lightroom-authored `.xmp` preset files.
    ImportPresetFiles {
        /// Source files.
        paths: Vec<PathBuf>,
    },
    /// Deletes a preset.
    DeletePreset {
        /// The preset to delete.
        preset: PresetId,
    },
    /// Renames a preset.
    RenamePreset {
        /// The preset to rename.
        preset: PresetId,
        /// The new display name.
        name: String,
    },
    /// Exports a preset to an arbitrary path.
    ExportPreset {
        /// The preset to export.
        preset: PresetId,
        /// Destination path.
        dest: PathBuf,
    },
}
