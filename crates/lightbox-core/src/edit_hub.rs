// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`EditHub`] — the E09 edit-state session registry (spec §3.4, T8 + T11).
//!
//! Wraps [`EditStore`] (the `lightbox-edit` durable store) with an in-memory
//! registry of open images: one [`EditSession`] (the pure gesture lifecycle)
//! per open image, plus an `Arc<Recipe>` "working" snapshot the render
//! scheduler reads without ever touching the hub's registry lock (D2).
//!
//! # D2 — two speeds of mutation
//!
//! - **Gesture updates** ([`EditHub::begin_gesture`]/[`update_gesture`]/
//!   [`cancel_gesture`]) are direct, synchronous, in-memory calls: a 60 Hz
//!   slider drag must not round-trip the command queue. `update_gesture`
//!   fires [`Event::EditWorkingChanged`] in the same call stack.
//! - **Every durable mutation** goes through [`EditHub::dispatch`], driven
//!   by the `lightbox-core` command bus (`Command::Edit`, one WAL txn each,
//!   `SyncSettings` a `Class::Background` job — see `session.rs`).
//!
//! [`working_recipe`](EditHub::working_recipe) hands out a cloned `Arc`
//! snapshot under a short `RwLock` read guard: readers never block on a
//! writer's `EditSession` mutex, and the write-side critical section (the
//! pointer swap after a gesture update) is a few nanoseconds — practically
//! lock-free for the render scheduler's read path (T8 AC: a contention
//! smoke test in `tests/edit_hub.rs` proves this holds under concurrent
//! readers/writers). This is a deliberate, documented simplification of the
//! spec's literal "atomic swap" wording (no `arc-swap`/atomics dependency
//! was added — R8 names per-param dirty tracking as the escape hatch if
//! profiling ever shows the per-update `Recipe` clone costing real time).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use lightbox_catalog::ImageDetail;
use lightbox_decode::{AssetProbe, ProbedFormat};
use lightbox_edit::{
    self as edit, apply_previous, paste_settings, reset_edits, sync_to, CopiedSettings,
    EditSession, EditState, EditStore, NeverCancel, ParamDelta, ParamSubset, PendingCommit,
    PresetId, PresetImportResult, PresetMeta, PresetStore, Recipe, RecipeRead, StepLabel,
    SyncOptions, SyncReport, XmpWriteCtx,
};
use lightbox_meta::xmp::sidecar;
use lightbox_meta::xmp::sync::{classify, DivergenceStatus, SyncStamps};
use lightbox_types::{AssetId, ImageId};
use tokio::sync::broadcast;

use crate::command::EditCommand;
use crate::error::{CoreError, Result};
use crate::event::{ChangeSet, Event};

/// The registry entry for one open image: the gesture lifecycle plus the
/// lock-free-for-readers working snapshot (D2).
struct HubEntry {
    session: Mutex<EditSession>,
    working: RwLock<Arc<Recipe>>,
}

/// The E09 session registry + durable command dispatcher (spec §3.4, T8/T11).
/// One per open [`crate::Session`] (`Session::edits`); cheap to hold as
/// `Arc<EditHub>`.
pub struct EditHub {
    store: EditStore,
    sessions: RwLock<HashMap<ImageId, Arc<HubEntry>>>,
    preset_store: Mutex<Option<Arc<PresetStore>>>,
    preset_dir: Option<PathBuf>,
    copy_buffer: Mutex<Option<CopiedSettings>>,
    previous: Mutex<Option<CopiedSettings>>,
    auto_write_xmp: AtomicBool,
    events: broadcast::Sender<Event>,
}

impl EditHub {
    pub(crate) fn new(
        store: EditStore,
        preset_dir: Option<PathBuf>,
        events: broadcast::Sender<Event>,
    ) -> Arc<EditHub> {
        Arc::new(EditHub {
            store,
            sessions: RwLock::new(HashMap::new()),
            preset_store: Mutex::new(None),
            preset_dir,
            copy_buffer: Mutex::new(None),
            previous: Mutex::new(None),
            auto_write_xmp: AtomicBool::new(false),
            events,
        })
    }

    /// The backing durable store (E15/tooling reads, the dispatcher's
    /// `EditSink` for sync/preset/paste apply).
    pub fn store(&self) -> &EditStore {
        &self.store
    }

    /// True once `enabled` for `SetAutoWriteXmp` (spec §3.1.1 default off;
    /// in-process only — app prefs are E08 territory).
    pub fn auto_write_xmp(&self) -> bool {
        self.auto_write_xmp.load(Ordering::SeqCst)
    }

    fn presets(&self) -> Result<Arc<PresetStore>> {
        let mut slot = self.preset_store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = &*slot {
            return Ok(Arc::clone(p));
        }
        let opened = match &self.preset_dir {
            Some(dir) => PresetStore::open(dir.clone()),
            None => PresetStore::open_default(),
        }?;
        let arc = Arc::new(opened);
        *slot = Some(Arc::clone(&arc));
        Ok(arc)
    }

    // ── T8: registry + gesture lifecycle (D2 — direct, synchronous, in-memory) ──

    /// Registers `image` if not already open (idempotent): loads the
    /// persisted-or-neutral state via `EditStore::open_state` (spec §4.2).
    pub fn open(&self, image: ImageId) -> Result<()> {
        {
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            if sessions.contains_key(&image) {
                return Ok(());
            }
        }
        let state = self.store.open_state(image)?;
        let entry = Arc::new(HubEntry {
            working: RwLock::new(Arc::new(state.recipe.clone())),
            session: Mutex::new(EditSession::new(image, state)),
        });
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.entry(image).or_insert(entry);
        Ok(())
    }

    /// True iff `image` has a registered session.
    pub fn is_open(&self, image: ImageId) -> bool {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&image)
    }

    /// Every currently-open image (registry snapshot).
    pub fn open_images(&self) -> Vec<ImageId> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    /// Unregisters `image`, **auto-committing an in-flight gesture first**
    /// (spec §4.2 D1 invariant: "an edit is never held only in memory past
    /// its gesture commit"; the T8 kill-after-close AC). A commit failure is
    /// logged, never silently dropped, and the image is still unregistered
    /// (closing must not wedge on a store error).
    pub fn close(&self, image: ImageId) {
        let entry = {
            let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            sessions.remove(&image)
        };
        let Some(entry) = entry else { return };
        let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
        if !session.has_open_gesture() {
            return;
        }
        let Some(commit) = session.take_commit() else {
            return;
        };
        let label = commit.label.clone();
        match self.store.commit(commit) {
            Ok(state) => {
                let seq = state.head_seq;
                session.apply_committed(state);
                let _ = self.events.send(Event::EditCommitted { image, seq, label });
            }
            Err(err) => {
                tracing::warn!(
                    target: "lightbox_core",
                    image = image.0,
                    %err,
                    "EditHub::close: auto-commit of an open gesture failed \
                     (the gesture is lost — the §3.1.1 bound is at most ONE \
                     uncommitted gesture, never a committed one)"
                );
            }
        }
    }

    /// Auto-commits every open gesture across every registered image (spec
    /// §4.2: `Session::close` calls this before the drain/backup — the
    /// "working-set replacement" leg of the T12 auto-persist proof).
    pub fn close_all(&self) {
        for image in self.open_images() {
            self.close(image);
        }
    }

    /// The live in-memory working recipe for `image`, or `None` if not open
    /// (spec §3.4: the render seam — E05/E10 — never waits on the DB).
    pub fn working_recipe(&self, image: ImageId) -> Option<Arc<Recipe>> {
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        let entry = sessions.get(&image)?;
        let snapshot = entry.working.read().unwrap_or_else(|e| e.into_inner());
        Some(Arc::clone(&snapshot))
    }

    /// Opens `image` if needed and returns its entry.
    fn entry(&self, image: ImageId) -> Result<Arc<HubEntry>> {
        self.open(image)?;
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        Ok(Arc::clone(
            sessions
                .get(&image)
                .expect("EditHub::open just registered this image"),
        ))
    }

    fn publish_working(&self, entry: &HubEntry, session: &EditSession) {
        *entry.working.write().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(session.working().clone());
    }

    /// Starts (or re-labels) a gesture: one gesture = one durable history
    /// step (spec §3.3). Auto-opens `image` if it wasn't already.
    pub fn begin_gesture(&self, image: ImageId, label: StepLabel) -> Result<()> {
        let entry = self.entry(image)?;
        let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
        session.begin_gesture(label);
        Ok(())
    }

    /// Applies `delta` to the in-memory working recipe (D2: synchronous,
    /// DB-free) and fires [`Event::EditWorkingChanged`] in the same call
    /// stack (T8 AC — no bus round-trip).
    pub fn update_gesture(&self, image: ImageId, delta: ParamDelta) -> Result<()> {
        let entry = self.entry(image)?;
        {
            let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
            session.update(delta)?;
            self.publish_working(&entry, &session);
        }
        let _ = self.events.send(Event::EditWorkingChanged { image });
        Ok(())
    }

    /// Discards the in-progress gesture, reverting `working` to `committed`.
    pub fn cancel_gesture(&self, image: ImageId) -> Result<()> {
        let entry = self.entry(image)?;
        let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
        session.cancel_gesture();
        self.publish_working(&entry, &session);
        Ok(())
    }

    /// Loads `subset` of `image`'s working recipe into the core-held copy
    /// buffer (spec §3.8). Non-durable — like the gesture calls, this never
    /// touches the command bus.
    pub fn copy_settings(&self, image: ImageId, subset: ParamSubset) -> Result<()> {
        let recipe = self
            .working_recipe(image)
            .map(|r| (*r).clone())
            .unwrap_or(self.store.open_state(image)?.recipe);
        *self.copy_buffer.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(CopiedSettings::copy_from(&recipe, &subset));
        Ok(())
    }

    fn refresh_session(&self, image: ImageId, state: EditState) {
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = sessions.get(&image) {
            let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
            session.apply_committed(state);
            self.publish_working(entry, &session);
        }
    }

    fn refresh_open(&self, images: &[ImageId]) {
        for &image in images {
            if self.is_open(image) {
                if let Ok(state) = self.store.open_state(image) {
                    self.refresh_session(image, state);
                }
            }
        }
    }

    // ── T11: the durable command dispatcher ─────────────────────────────────

    /// Executes one durable [`EditCommand`] (spec §3.4). Called from the
    /// `lightbox-core` command bus on the blocking pool — never from the
    /// async runtime thread. Returns the events to fan out plus the coarse
    /// [`ChangeSet`] hint; the caller (`session.rs`) always additionally
    /// emits `Event::CatalogChanged { change }` so a caller waiting on a
    /// no-op (e.g. a second `CommitGesture` with nothing pending) still
    /// observes completion.
    ///
    /// `SyncSettings` runs here too (a synchronous fallback, spec T22
    /// wording) — `session.rs`'s dispatcher special-cases it to spawn as a
    /// `Class::Background` job instead on the live bus, since a sync can
    /// span hundreds of targets.
    pub fn dispatch(&self, cmd: EditCommand) -> Result<(Vec<Event>, ChangeSet)> {
        match cmd {
            EditCommand::CommitGesture { image } => self.dispatch_commit_gesture(image),
            EditCommand::StepTo { image, seq } => {
                let label = StepLabel::HistoryRestore { seq };
                let state = edit::history::step_to(self.store.catalog(), image, seq)?;
                self.refresh_session(image, state.clone());
                Ok((
                    vec![Event::EditCommitted {
                        image,
                        seq: state.head_seq,
                        label,
                    }],
                    ChangeSet::image(image),
                ))
            }
            EditCommand::Undo { image } => {
                let state = edit::history::undo(self.store.catalog(), image)?;
                self.refresh_session(image, state.clone());
                Ok((
                    vec![Event::EditCommitted {
                        image,
                        seq: state.head_seq,
                        label: StepLabel::HistoryRestore {
                            seq: state.head_seq,
                        },
                    }],
                    ChangeSet::image(image),
                ))
            }
            EditCommand::Redo { image } => {
                let state = edit::history::redo(self.store.catalog(), image)?;
                self.refresh_session(image, state.clone());
                Ok((
                    vec![Event::EditCommitted {
                        image,
                        seq: state.head_seq,
                        label: StepLabel::HistoryRestore {
                            seq: state.head_seq,
                        },
                    }],
                    ChangeSet::image(image),
                ))
            }
            EditCommand::ClearHistory { image } => {
                edit::history::clear(self.store.catalog(), image)?;
                Ok((vec![], ChangeSet::image(image)))
            }
            EditCommand::CreateSnapshot { image, name } => {
                self.store.create_snapshot(image, &name)?;
                Ok((vec![], ChangeSet::image(image)))
            }
            EditCommand::RestoreSnapshot { image, snapshot } => {
                let name = self
                    .store
                    .snapshots(image)?
                    .into_iter()
                    .find(|s| s.id == snapshot)
                    .map(|s| s.name)
                    .unwrap_or_default();
                let state = self.store.restore_snapshot(image, snapshot)?;
                self.refresh_session(image, state.clone());
                Ok((
                    vec![Event::EditCommitted {
                        image,
                        seq: state.head_seq,
                        label: StepLabel::SnapshotRestore { name },
                    }],
                    ChangeSet::image(image),
                ))
            }
            EditCommand::DeleteSnapshot { image, snapshot } => {
                self.store.delete_snapshot(snapshot)?;
                Ok((vec![], ChangeSet::image(image)))
            }
            EditCommand::RenameSnapshot {
                image,
                snapshot,
                name,
            } => {
                self.store.rename_snapshot(snapshot, &name)?;
                Ok((vec![], ChangeSet::image(image)))
            }
            EditCommand::ApplyPreset { images, preset } => {
                let presets = self.presets()?;
                let p = presets
                    .get(&preset)
                    .ok_or_else(|| CoreError::Internal(format!("no such preset: {preset:?}")))?;
                let _report: SyncReport = sync_to(
                    &self.store,
                    &p.delta,
                    &images,
                    StepLabel::Preset {
                        name: p.name.clone(),
                    },
                    &NeverCancel,
                    SyncOptions::default(),
                    |_| {},
                )?;
                self.refresh_open(&images);
                Ok((vec![], ChangeSet::bulk(false)))
            }
            EditCommand::PasteSettings { images } => {
                let buf = self
                    .copy_buffer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let Some(copied) = buf else {
                    return Ok((vec![], ChangeSet::default()));
                };
                paste_settings(
                    &self.store,
                    &copied,
                    &images,
                    &NeverCancel,
                    SyncOptions::default(),
                    |_| {},
                )?;
                self.refresh_open(&images);
                Ok((vec![], ChangeSet::bulk(false)))
            }
            EditCommand::SyncSettings {
                source,
                targets,
                subset,
            } => {
                let source_recipe = self.current_recipe(source)?;
                let delta = source_recipe.extract(&subset);
                sync_to(
                    &self.store,
                    &delta,
                    &targets,
                    StepLabel::Sync,
                    &NeverCancel,
                    SyncOptions::default(),
                    |_| {},
                )?;
                self.refresh_open(&targets);
                Ok((vec![], ChangeSet::bulk(false)))
            }
            EditCommand::ApplyPrevious { images } => {
                let prev = self
                    .previous
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let Some(previous) = prev else {
                    return Ok((vec![], ChangeSet::default()));
                };
                apply_previous(
                    &self.store,
                    &previous,
                    &images,
                    &NeverCancel,
                    SyncOptions::default(),
                    |_| {},
                )?;
                self.refresh_open(&images);
                Ok((vec![], ChangeSet::bulk(false)))
            }
            EditCommand::ResetEdits { images } => {
                reset_edits(
                    &self.store,
                    &images,
                    &NeverCancel,
                    SyncOptions::default(),
                    |_| {},
                )?;
                self.refresh_open(&images);
                Ok((vec![], ChangeSet::bulk(false)))
            }
            EditCommand::ReadMetadata { images } => self.dispatch_read_metadata(&images),
            EditCommand::WriteMetadata { images } => self.dispatch_write_metadata(&images),
            EditCommand::SetAutoWriteXmp { enabled } => {
                self.auto_write_xmp.store(enabled, Ordering::SeqCst);
                Ok((vec![], ChangeSet::default()))
            }
            EditCommand::RefreshXmpStatus { images } => self.dispatch_refresh_xmp(&images),
            EditCommand::CreatePreset {
                from,
                name,
                group,
                subset,
            } => {
                let recipe = self.current_recipe(from)?;
                let presets = self.presets()?;
                presets.create_from(&recipe, &name, group.as_deref(), &subset)?;
                Ok((vec![], ChangeSet::default()))
            }
            EditCommand::ImportPresetFiles { paths } => {
                let presets = self.presets()?;
                let results: Vec<PresetImportResult> = presets.import_files(&paths);
                if let Some(first_err) = results.iter().find_map(|r| r.error.as_ref()) {
                    // Best-effort semantics (spec §3.7): a per-file failure
                    // never aborts the batch. Surface the first failure as
                    // the command's own error only when EVERY file failed —
                    // otherwise the caller can't tell "partially imported"
                    // from "failed", so it re-inspects via `Queries::presets`.
                    if results.iter().all(|r| r.error.is_some()) {
                        return Err(CoreError::Internal(first_err.clone()));
                    }
                }
                Ok((vec![], ChangeSet::default()))
            }
            EditCommand::DeletePreset { preset } => {
                self.presets()?.delete(&preset)?;
                Ok((vec![], ChangeSet::default()))
            }
            EditCommand::RenamePreset { preset, name } => {
                self.presets()?.rename(&preset, &name)?;
                Ok((vec![], ChangeSet::default()))
            }
            EditCommand::ExportPreset { preset, dest } => {
                self.presets()?.export(&preset, &dest)?;
                Ok((vec![], ChangeSet::default()))
            }
        }
    }

    fn dispatch_commit_gesture(&self, image: ImageId) -> Result<(Vec<Event>, ChangeSet)> {
        let entry = self.entry(image)?;
        let mut session = entry.session.lock().unwrap_or_else(|e| e.into_inner());
        let Some(commit) = session.take_commit() else {
            // Typed no-op (spec T11 AC): no open gesture, or no net change.
            return Ok((vec![], ChangeSet::image(image)));
        };
        let label = commit.label.clone();
        let new_recipe_for_previous = commit.new_recipe.clone();
        let state = self.store.commit(commit)?;
        let seq = state.head_seq;
        session.apply_committed(state);
        self.publish_working(&entry, &session);
        drop(session);
        // "Previous" (spec §3.8): the last-committed source's full settings.
        *self.previous.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(CopiedSettings::all_of(&new_recipe_for_previous));
        Ok((
            vec![Event::EditCommitted { image, seq, label }],
            ChangeSet::image(image),
        ))
    }

    /// The recipe to read for a "source" role (preset/create-preset/sync):
    /// the open working recipe if the image has one, else the durable state.
    fn current_recipe(&self, image: ImageId) -> Result<Recipe> {
        if let Some(r) = self.working_recipe(image) {
            return Ok((*r).clone());
        }
        Ok(self.store.open_state(image)?.recipe)
    }

    fn asset_and_path(&self, image: ImageId) -> Result<(AssetId, PathBuf, ImageDetail)> {
        let reader = self.store.catalog().reader();
        let detail = reader.image_detail(image)?;
        let abs = reader.asset_abs_path(detail.asset)?;
        Ok((detail.asset, abs, detail))
    }

    fn dispatch_read_metadata(&self, images: &[ImageId]) -> Result<(Vec<Event>, ChangeSet)> {
        let mut events = Vec::new();
        for &image in images {
            let (asset, abs, detail) = self.asset_and_path(image)?;
            let probe = probe_from_detail(&detail);
            let Some(from) = edit::xmp_map::read_sidecar(&abs, &probe)
                .map_err(|e| CoreError::Internal(format!("xmp read {image:?}: {e}")))?
            else {
                continue; // no sidecar (spec §4.2: nothing to read)
            };
            let state = self.store.open_state(image)?;
            let delta = from.recipe.diff(&state.recipe);
            let inverse = state.recipe.diff(&from.recipe);
            let new_state = self.store.commit(PendingCommit {
                image,
                new_recipe: from.recipe,
                prev_head_seq: state.head_seq,
                label: StepLabel::XmpRead,
                delta,
                inverse,
            })?;
            let recipe_hash = new_state.recipe.canonical_hash();
            self.refresh_session(image, new_state.clone());
            events.push(Event::EditCommitted {
                image,
                seq: new_state.head_seq,
                label: StepLabel::XmpRead,
            });
            if let Some((_, stamp)) = sidecar::read_with_stamp(&sidecar::sidecar_path(&abs))
                .map_err(|e| CoreError::Internal(e.to_string()))?
            {
                let hash = stamp.hash;
                let mtime = system_time_to_string(stamp.mtime);
                self.store.catalog().writer().with_txn(move |txn| {
                    txn.upsert_xmp_sync_read(asset, &hash, &mtime, &recipe_hash)
                })?;
                let status = self.compute_status(asset, &abs, recipe_hash)?;
                events.push(Event::XmpDivergenceChanged { asset, status });
            }
        }
        Ok((events, ChangeSet::bulk(false)))
    }

    fn dispatch_write_metadata(&self, images: &[ImageId]) -> Result<(Vec<Event>, ChangeSet)> {
        let mut events = Vec::new();
        for &image in images {
            let (asset, abs, _detail) = self.asset_and_path(image)?;
            let recipe = match self.store.recipe_of(image)? {
                RecipeRead::Ok(r) => r,
                RecipeRead::NewerSchema { schema, .. } => {
                    return Err(CoreError::Internal(format!(
                        "image {image:?}: recipe schema {schema} is newer than this build"
                    )))
                }
            };
            let ctx = XmpWriteCtx::default();
            let stamp = edit::xmp_map::write_sidecar(&recipe, &abs, &ctx)
                .map_err(|e| CoreError::Internal(format!("xmp write {image:?}: {e}")))?;
            let recipe_hash = recipe.canonical_hash();
            let hash = stamp.hash;
            let mtime = system_time_to_string(stamp.mtime);
            self.store.catalog().writer().with_txn(move |txn| {
                txn.upsert_xmp_sync_write(asset, &hash, &mtime, &recipe_hash)
            })?;
            let status = self.compute_status(asset, &abs, recipe_hash)?;
            events.push(Event::XmpDivergenceChanged { asset, status });
        }
        Ok((events, ChangeSet::default()))
    }

    fn dispatch_refresh_xmp(&self, images: &[ImageId]) -> Result<(Vec<Event>, ChangeSet)> {
        let mut events = Vec::new();
        for &image in images {
            let (asset, abs, _detail) = self.asset_and_path(image)?;
            let recipe_hash = match self.store.recipe_of(image)? {
                RecipeRead::Ok(r) => r.canonical_hash(),
                RecipeRead::NewerSchema { .. } => continue,
            };
            let status = self.compute_status(asset, &abs, recipe_hash)?;
            events.push(Event::XmpDivergenceChanged { asset, status });
        }
        Ok((events, ChangeSet::default()))
    }

    /// The §3.5 divergence entry point (`sync::status`, deferred at T17 to
    /// "once the `xmp_sync` DAO lands" — landed here): compares the on-disk
    /// sidecar hash and the recipe's `canonical_hash` against the last
    /// `xmp_sync` stamps via the pure `classify` state machine.
    pub fn compute_status(
        &self,
        asset: AssetId,
        original: &Path,
        recipe_hash: [u8; 16],
    ) -> Result<DivergenceStatus> {
        let side = sidecar::sidecar_path(original);
        let disk_hash = sidecar::read_with_stamp(&side)
            .map_err(|e| CoreError::Internal(e.to_string()))?
            .map(|(_, s)| s.hash);
        let row = self.store.catalog().reader().xmp_sync_row(asset)?;
        let stamps = SyncStamps {
            sidecar_hash: row
                .as_ref()
                .and_then(|r| r.sidecar_hash.as_deref())
                .and_then(to_arr16),
            recipe_hash: row
                .as_ref()
                .and_then(|r| r.recipe_hash.as_deref())
                .and_then(to_arr16),
        };
        Ok(classify(disk_hash, recipe_hash, stamps))
    }

    /// The preset browser index (spec §3.4 `Queries::presets`).
    pub fn preset_list(&self) -> Result<Vec<PresetMeta>> {
        Ok(self.presets()?.list())
    }

    /// Pure hover preview: `preset` applied onto `image`'s current recipe
    /// (spec §3.4 `Queries::preset_preview_recipe`; no session/store mutation).
    pub fn preset_preview(&self, image: ImageId, preset: PresetId) -> Result<Recipe> {
        let presets = self.presets()?;
        let p = presets
            .get(&preset)
            .ok_or_else(|| CoreError::Internal(format!("no such preset: {preset:?}")))?;
        let base = self.current_recipe(image)?;
        Ok(edit::preview_recipe(&base, &p))
    }
}

fn to_arr16(b: &[u8]) -> Option<[u8; 16]> {
    b.try_into().ok()
}

fn system_time_to_string(t: Option<SystemTime>) -> String {
    t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
}

/// Builds an [`AssetProbe`] from the catalog's own [`ImageDetail`] (never
/// faked): `Recipe::default_for`/`from_lr_crs` currently ignore most probe
/// fields (an E09 Phase D deviation, `let _ = probe;` in `recipe.rs`), so
/// this is future-proofing, not load-bearing, at M1.
fn probe_from_detail(detail: &ImageDetail) -> AssetProbe {
    AssetProbe {
        format: ProbedFormat::Unsupported(detail.format.clone()),
        width: detail.width,
        height: detail.height,
        orientation: detail.orientation,
        camera_make: detail.camera_make.clone(),
        camera_model: detail.camera_model.clone(),
        capture_time: detail.capture_time.clone(),
        file_bytes: detail.bytes,
        embedded: Vec::new(),
    }
}
