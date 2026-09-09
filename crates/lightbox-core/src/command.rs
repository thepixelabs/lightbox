// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`Command`], the mutation vocabulary of seam 1 (spec §3.8).
//!
//! Every command executes as **one WAL transaction** on the catalog's
//! single writer (architecture §3.1); results come back asynchronously as
//! broadcast [`crate::Event`]s correlated by [`CommandTicket`].

use std::path::PathBuf;

use lightbox_edit::{ParamSubset, PresetId};
use lightbox_ingest::OpenRequest;
use lightbox_preview::{BuildPriority, CacheLimits, PurgeScope, Tier, TierSet};
use lightbox_types::{Flag, ImageId, ImportSessionId, LookId, SnapshotId};

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

/// Mutations the shell/CLI may request (spec §3.8, `#[non_exhaustive]`
/// E04/E07/E09 grow this).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Command {
    /// v2.0 entry point (E04 spec §2.4/§4.5): replace the session working
    /// set with the files this request resolves to. Cancels any in-flight
    /// open *before* bumping the epoch, so at most one load runs. Progress
    /// and completion arrive as `Event::WorkingSet*` events tagged with the
    /// new epoch; `Session::working_set()` is the snapshot query. There is
    /// no append gesture (spec OQ-1), a new request always replaces.
    OpenWorkingSet {
        /// The gesture to resolve (drop / dialog / launch / CLI).
        request: OpenRequest,
    },
    /// Add-in-place import of a directory (spec §1 item 4). Progress and
    /// completion arrive as `Import*` events. **Retired-dormant** as of E04
    /// (mandate v2.0/architecture §10.0): the managed-import path stays
    /// compiled and tested (E01's fault/E2E harness) but the shell never
    /// reaches it, `OpenWorkingSet` above is the v2.0 entry point.
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
    /// Set or clear the 1..=5 star rating, the canonical trivial command
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
    /// direct, synchronous, in-memory); every variant here is a durable
    /// mutation, dispatched as one WAL txn each (ordered, like `SetRating`)
    /// except `SyncSettings`, which spawns as a `Class::Background` job.
    Edit(EditCommand),
    /// E03 Phase D (T14/T16, spec §5.6): enqueues preview builds for
    /// `images` at `tier`/`priority` through [`crate::Session::preview_service`].
    /// `priority == BuildPriority::Bulk` drives a tracked
    /// [`lightbox_preview::PreviewService::bulk_build`] run (progress
    /// arrives as `Event::PreviewBulkProgress`); any other priority issues
    /// one `PreviewService::request` per image. Completion/failure of each
    /// individual build arrives as `Event::PreviewReady`/`PreviewFailed`
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
    /// E03 Phase F (T19, spec §5.6): removes preview rows + files for
    /// `images` at `tiers`. T0 is asset-scope (spec §3.1), discarding it
    /// through one image discards the row shared by every virtual copy of
    /// that asset (see [`lightbox_preview::PreviewService::discard`]'s doc
    /// comment). No durable-txn ack; the effect is immediately visible
    /// through `Query::PreviewState`/`CacheStats` (no dedicated event
    /// unlike `BuildPreviews`, discard is synchronous from the caller's
    /// point of view since eviction is a local filesystem+catalog operation,
    /// not a build).
    DiscardPreviews {
        /// Targets.
        images: Vec<ImageId>,
        /// Which tiers to discard.
        tiers: TierSet,
    },
    /// E03 Phase F (T21, spec §5.6): updates both the preview-pyramid and
    /// raw-cache byte caps at runtime.
    SetCacheLimits(CacheLimits),
    /// E03 Phase F (T21, spec §5.6): journaled relocation of the E03-owned
    /// cache surfaces (see [`lightbox_preview::PreviewService::relocate`]'s
    /// doc comment for exact scope) to `new_root`. Spawns as a
    /// `Class::Background`-shaped job (mirrors `BackupNow`/
    /// `ImportAddInPlace`) since a large store can take a while to copy;
    /// completion arrives as `Event::CacheRelocated`/`Event::CommandFailed`.
    RelocateCacheStore {
        /// The new cache-store root.
        new_root: PathBuf,
    },
    /// E03 Phase F (T21, spec §5.6): scoped purge of the preview and/or raw
    /// caches. Completion arrives as `Event::CachePurged`.
    PurgeCaches(PurgeScope),
    /// E15 core slice (spec §5.10, narrowed, no `ExportPlan`/
    /// `ExportSession`/collision policy at core-slice scope; see
    /// `lightbox-export`'s crate doc comment for the full cut list): exports
    /// `images` into `dest_dir`, one file per image named
    /// `<stem><suffix>.<ext>` per `settings.naming`/`settings.format`
    /// (destination-collision within a batch silently last-writer-wins, no
    /// `Ask`/`RenameUnique`/`Skip` policy yet). A `Class::Foreground` job
    /// (mirrors `ImportAddInPlace`): `Event::ExportStarted` once the batch
    /// is resolved, `Event::ExportProgress` per item, `Event::
    /// ExportFinished` on completion. Exporting is not an edit and never
    /// touches `history_step`.
    Export {
        /// Images to export.
        images: Vec<ImageId>,
        /// Destination directory (created if missing).
        dest_dir: PathBuf,
        /// Format/quality/depth/resize/color-space/sharpen/naming.
        settings: lightbox_export::ExportSettings,
    },
    /// E10 Phase D (task **D10**): installs a `.cube`/HaldCLUT creative-look
    /// file into the catalog's `installed_look` registry, hashes the
    /// bytes (the idempotency + recipe `CreativeLut.id` key), validates it
    /// parses, copies it into `<lbdata>/looks/`, and inserts (or no-ops on
    /// an existing) row. Not a recipe edit (no `EditCommand`/history
    /// involvement), a catalog-registry mutation, like
    /// `Command::Edit(CreatePreset)`'s sibling for looks instead of
    /// presets. Completion arrives as `Event::CatalogChanged`; a malformed/
    /// unreadable/unsupported file arrives as `Event::CommandFailed`.
    InstallLook {
        /// Source file on disk (the user's picked `.cube`/HaldCLUT `.png`).
        path: PathBuf,
        /// Optional family/group for the D11 look-browser panel ("B&W",
        /// "Film", …); `None` = ungrouped.
        family: Option<String>,
    },
    /// E10 Phase D (task **D10**): removes an installed look's registry
    /// row. The on-disk file is left in place (`lightbox-core::looks`'
    /// module doc); any recipe still referencing the hash keeps its param
    /// and renders identity through `CreativeLutNode`'s existing
    /// graceful-degrade contract, this command also invalidates the
    /// engine's look-resolver cache so that takes effect on the very next
    /// render, not just after a restart.
    RemoveLook {
        /// The row to remove.
        id: LookId,
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
    /// Imports Lightbox- or Lightroom-authored `.xmp` preset files **the user
    /// chose**. Purely additive, and never marks or retires anything.
    ImportPresetFiles {
        /// Source files.
        paths: Vec<PathBuf>,
    },
    /// Seeds the preset store from Lightbox's **own** bundled starter library
    /// and retires the starter presets that library no longer ships
    /// (`lightbox_edit::preset_retire`).
    ///
    /// Distinct from [`ImportPresetFiles`](Self::ImportPresetFiles) precisely
    /// *because* it retires: this variant asserts "these paths are the whole
    /// bundled library", which is what makes the set difference meaningful.
    /// Handing it a user-chosen subset would read every absent preset as
    /// removed. Only the shell's launch-time seed may submit it.
    SeedBundledPresets {
        /// Every `.xmp` in the bundled starter library.
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
