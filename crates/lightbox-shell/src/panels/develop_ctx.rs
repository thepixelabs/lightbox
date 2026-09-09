// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E3, the [`EditBinding`] adapter over E09's real edit session (spec
//! §6.5): panels call this trait, **never** `Session`/`Command::Edit`
//! directly, E10/E12 build on this exact seam later.
//!
//! # Gesture lifecycle (spec §6.5 / E09 §3.3 discipline)
//!
//! * [`EditBinding::begin_gesture`] → `EditHub::begin_gesture`, direct,
//!   synchronous, in-memory (a 60 Hz drag never round-trips the command
//!   bus; E09's own D2 rule).
//! * [`EditBinding::preview`] → `EditHub::update_gesture`, mutates the
//!   in-memory working recipe and bumps [`EditBinding::recipe_rev`] iff
//!   the effective recipe actually changed; the canvas's
//!   [`RecipeSource`] read (this same type) then re-submits the engine in
//!   the same frame (C3's `submit_if_changed` key includes the rev).
//! * [`EditBinding::end_gesture`] → `Command::Edit(CommitGesture)`, ONE
//!   durable, coalesced history step per gesture (E09's `take_commit`
//!   makes a no-net-change gesture a typed no-op).
//!
//! # Why this type also implements [`RecipeSource`]
//!
//! Phase C left `canvas::view::RecipeSource` as exactly this seam (see
//! `E08-deviations.md` C3): the binding serves the **bound** (active)
//! image's live in-memory working recipe + its own rev counter, and
//! delegates any other image (transition frames) to the Phase-C
//! [`SessionRecipeSource`] fallback it owns, so the canvas keeps rendering
//! persisted recipes for entries the panels aren't editing.
//!
//! # History/snapshot read caching (spec §7: no per-frame SQL)
//!
//! `history`/`snapshots`/`can_undo`/`can_redo` serve from caches refreshed
//! only when marked dirty, on [`SessionEditBinding::bind`] image change,
//! on `Event::EditCommitted` for the bound image, and on
//! `Event::CatalogChanged` (which every durable edit command emits,
//! including the ones that produce no `EditCommitted`: `ClearHistory`,
//! `CreateSnapshot`, `RenameSnapshot`). Same dirty-flag discipline as
//! `filmstrip::EditedBadges` and `SessionRecipeSource`.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_core::{Command, EditCommand, InstalledLookRow, LookId, Session};
use lightbox_edit::{
    CreativeLut, Crop, HistoryStepMeta, ParamDelta, ParamId, ParamSubset, ParamValue, PresetId,
    PresetMeta, Recipe, SnapshotMeta, StepLabel,
};
// E10 D12: the histogram widget's data type, see `EditBinding::histogram`.
use lightbox_render::ng::HistogramData;
use lightbox_types::{ImageId, ProcessVersion, SnapshotId, SourceKind, PV_M0};

use crate::canvas::gizmo::GizmoLayer;
use crate::canvas::view::{RecipeSnapshot, RecipeSource};
use crate::canvas::SessionRecipeSource;

/// What every panel gets per frame (spec §6.5): the active file's kind +
/// the edit gesture surface, plus the on-canvas gizmo layer.
pub struct DevelopCtx<'a> {
    /// The active entry's probe-derived kind (§4 adaptivity authority).
    pub source_kind: SourceKind,
    /// The gesture/edit surface, panels never touch `Session` directly.
    pub edit: &'a mut dyn EditBinding,
    /// Phase F: the app-owned canvas gizmo layer (`canvas/gizmo.rs`,
    /// spec §6.6). Tool buttons activate/cancel their gizmo here and read
    /// their pressed state from `is_active`, this replaces (subsumes)
    /// Phase E's `EyedropperMount` boolean-armed seam: "armed" now
    /// literally means "the gizmo is active in the layer", so the panel
    /// button, the keymap context push, and Esc/Enter can never disagree.
    /// See `E08-deviations.md` Phase F.
    pub gizmos: &'a mut GizmoLayer,
}

/// The panels' seam into E09 (spec §6.5). Implemented over the real edit
/// session by [`SessionEditBinding`]; panels and `param_slider` only ever
/// see this trait.
///
/// **Deviation note (recorded, Phase E):** the trait carries the E8
/// history/snapshots surface (`history`/`restore_step`/`clear_history`/
/// `snapshots`/`create_snapshot`/`restore_snapshot`/`rename_snapshot`) in
/// addition to the spec's §6.5 gesture-core snippet, the history panel is
/// a panel like any other and must not reach around its own seam.
pub trait EditBinding {
    /// Current recipe value for `p` (including live gesture preview).
    fn value(&self, p: ParamId) -> ParamValue;
    /// Double-click reset target for `p` (the neutral identity value).
    fn default(&self, p: ParamId) -> ParamValue;
    /// Starts (or re-labels) a gesture for `p`, one gesture, one step.
    fn begin_gesture(&mut self, p: ParamId);
    /// In-memory preview; bumps [`EditBinding::recipe_rev`] on effective
    /// change so the canvas resubmits in the same frame.
    fn preview(&mut self, d: ParamDelta);
    /// Ends the gesture: ONE coalesced durable commit (E09 discipline).
    fn end_gesture(&mut self);
    /// Resets `p` to its default as one immediate commit.
    fn reset(&mut self, p: ParamId);
    /// Cheap monotonic change counter (the canvas submit key). No direct
    /// caller yet: the canvas reads the rev through this same adapter's
    /// `RecipeSource::recipe_for`, this accessor is the §6.5 contract
    /// surface for E10/E12 consumers (and the deferred test doubles).
    #[allow(dead_code)]
    fn recipe_rev(&self) -> u64;
    /// True when a history step exists to undo.
    fn can_undo(&self) -> bool;
    /// True when a kept later step exists to redo.
    fn can_redo(&self) -> bool;
    /// `Command::Edit(Undo)` for the bound image.
    fn undo(&mut self);
    /// `Command::Edit(Redo)` for the bound image.
    fn redo(&mut self);

    // ── E8: history & snapshots (over E09's queries/commands) ──────────

    /// Newest-first history steps (cached; refreshed event-driven).
    fn history(&self) -> &[HistoryStepMeta];
    /// Restores the recipe at history position `seq` (`StepTo`).
    fn restore_step(&mut self, seq: u64);
    /// Drops the history log, keeping the current recipe.
    fn clear_history(&mut self);
    /// Named snapshots (cached; refreshed event-driven).
    fn snapshots(&self) -> &[SnapshotMeta];
    /// Materializes the current recipe as a named snapshot.
    fn create_snapshot(&mut self, name: &str);
    /// Restores a named snapshot (itself a history step).
    fn restore_snapshot(&mut self, snapshot: SnapshotId);
    /// Renames a snapshot.
    fn rename_snapshot(&mut self, snapshot: SnapshotId, name: &str);

    // ── Presets (spec §3.7 / preset-management panel) ──────────────────
    //
    // Default (no-op) bodies below: the test doubles in `basic.rs`/
    // `curve.rs`/`hsl.rs`/`grading.rs`/`host.rs` exercise their own panel,
    // never presets, so only `SessionEditBinding` (the real adapter)
    // overrides these.

    /// Grouped, sorted preset metadata (spec §3.7 `Queries::presets`;
    /// cached, refreshed event-driven, same discipline as
    /// [`EditBinding::history`]/[`EditBinding::snapshots`]).
    fn presets(&self) -> &[PresetMeta] {
        &[]
    }
    /// Applies `preset`'s delta to the bound image as ONE durable history
    /// step (`StepLabel::Preset`, spec §3.4 `ApplyPreset`).
    fn apply_preset(&mut self, _preset: PresetId) {}
    /// Creates a preset from the bound image's CURRENT recipe, carrying
    /// only `subset`'s groups (spec §3.4 `CreatePreset`).
    fn create_preset(&mut self, _name: &str, _group: Option<&str>, _subset: ParamSubset) {}
    /// Imports Lightbox- or Lightroom-authored `.xmp` preset files (spec
    /// §3.4 `ImportPresetFiles`).
    fn import_preset_files(&mut self, _paths: Vec<PathBuf>) {}
    /// Deletes a preset (spec §3.4 `DeletePreset`).
    fn delete_preset(&mut self, _preset: PresetId) {}
    /// Renames a preset (spec §3.4 `RenamePreset`).
    fn rename_preset(&mut self, _preset: PresetId, _name: &str) {}
    /// Exports a preset to an arbitrary path (spec §3.4 `ExportPreset`).
    fn export_preset(&mut self, _preset: PresetId, _dest: PathBuf) {}
    /// Sets (`Some`) or clears (`None`) a HOVER-ONLY canvas preview: the
    /// bound image's current recipe with `preset`'s delta applied (spec
    /// §3.7 `preview_recipe`, via `Queries::preset_preview_recipe`), shown
    /// by the canvas but **never** committed, queried back, or written to
    /// history. The real adapter clears this automatically on rebind.
    fn set_hover_preset(&mut self, _preset: Option<PresetId>) {}

    // ── Creative looks (E10 tasks D10/D11, look install + browser) ────
    //
    // Default (no-op) bodies below: the test doubles never exercise the
    // look browser, only `SessionEditBinding` (the real adapter) overrides
    // these, same convention as the presets block above.

    /// Every installed creative-look pack, grouped/sorted `(family, name)`
    /// (D10 `Queries::installed_looks`; cached, refreshed event-driven
    /// same discipline as [`EditBinding::presets`]).
    fn installed_looks(&self) -> &[InstalledLookRow] {
        &[]
    }
    /// Applies `content_hash` as the bound image's creative look at full
    /// strength (`amount = 100`, spec §4.8 unity), ONE durable commit
    /// through the ordinary `ParamId::CreativeLut` gesture path (this repo's
    /// actual param surface: a single `CreativeLut` leaf, not a dedicated
    /// `SetCreativeLook` command).
    fn apply_look(&mut self, _content_hash: &str) {}
    /// Sets (`Some`) or clears (`None`) a HOVER-ONLY canvas preview of
    /// applying `content_hash` at full strength, mirrors
    /// [`EditBinding::set_hover_preset`]'s non-committing contract exactly
    /// (never written to history, never committed, cleared on rebind).
    fn set_hover_look(&mut self, _content_hash: Option<&str>) {}
    /// Installs a `.cube`/HaldCLUT file into the catalog's `installed_look`
    /// registry (D10 `Command::InstallLook`); `family` is the D11 browser's
    /// optional grouping label.
    fn install_look_file(&mut self, _path: PathBuf, _family: Option<String>) {}
    /// Removes an installed look (D10 `Command::RemoveLook`). Recipes still
    /// referencing its hash keep their param and render identity + show the
    /// D11 missing-look badge ([`EditBinding::is_look_installed`]).
    fn remove_look(&mut self, _id: LookId) {}
    /// True iff `content_hash` is currently installed, the D11 missing-
    /// look-badge query. Pure derived default (cross-references
    /// [`EditBinding::installed_looks`]; no override needed).
    fn is_look_installed(&self, content_hash: &str) -> bool {
        self.installed_looks()
            .iter()
            .any(|l| l.content_hash == content_hash)
    }

    /// Sets (`true`) or clears (`false`) the crop-tool-active override
    /// (E11 canvas-display fix, `E11-deviations.md` "geometry canvas
    /// display"): while `true`, [`RecipeSource::recipe_for`] returns the
    /// bound image's recipe with `geometry.crop` suppressed to full-frame
    /// the on-canvas crop gizmo needs the UNCROPPED image to render so
    /// the user can see outside the current crop rect while adjusting it.
    /// Mirrors [`EditBinding::set_hover_preset`]'s override mechanism, but
    /// re-derives from the LIVE working recipe on every `recipe_for` call
    /// rather than a one-time snapshot, so other panels stay editable
    /// while the crop tool stays armed. `lib.rs` syncs this once per frame
    /// from `GizmoLayer::is_active(crop_gizmo::GEOM_CROP)`, after every
    /// activate/cancel this frame has resolved. A no-op default here (only
    /// [`SessionEditBinding`] overrides it), the panel test doubles never
    /// drive the crop gizmo.
    fn set_crop_tool_active(&mut self, _active: bool) {}

    /// E10 D12: the live histogram reduction over the canvas's most
    /// recently displayed frame for the bound image (`None` before the
    /// first frame renders). A no-op default here (only
    /// [`SessionEditBinding`] overrides it, fed by `lib.rs` from
    /// `EditorCanvas::latest_histogram` once per frame), the panel test
    /// doubles exercise the histogram widget with an explicit
    /// `HistogramData` instead (`panels::histogram`'s own tests), same
    /// posture as every other canvas-owned reader on this trait.
    fn histogram(&self) -> Option<&HistogramData> {
        None
    }
}

/// The real adapter (E3): E09's `EditHub` gesture registry + the command
/// bus for durable mutations + `Queries` for the cached read surface. Also
/// the canvas's [`RecipeSource`] (see the module docs).
pub struct SessionEditBinding {
    session: Session,
    /// Phase-C fallback for images the binding is not bound to (the canvas
    /// may render one transition frame of a not-yet-bound image).
    fallback: SessionRecipeSource,
    /// The bound (active) image, if any.
    image: Option<ImageId>,
    /// Cached working recipe of the bound image (E09's in-memory truth).
    working: Option<Arc<Recipe>>,
    /// Global monotonic rev, bumped on every effective change AND on
    /// every rebind, so a rev value is never reused across images.
    rev: u64,
    /// Cached durable head position (undo enable state).
    head_seq: u64,
    /// Cached newest-first history (E8 panel).
    history: Vec<HistoryStepMeta>,
    /// Cached snapshots (E8 panel).
    snapshots: Vec<SnapshotMeta>,
    /// Cached, grouped preset metadata (preset panel), GLOBAL, not
    /// per-image, so it refreshes independent of `image` (see `refresh`).
    presets: Vec<PresetMeta>,
    /// A hover-only canvas preview: `(preset, previewed recipe)` for the
    /// bound image, set by [`EditBinding::set_hover_preset`]. Read by
    /// `RecipeSource::recipe_for` in place of `working`; never written to
    /// the store/history (the panel's pure-preview AC).
    hover_preview: Option<(PresetId, Recipe)>,
    /// Cached, sorted installed-look metadata (D11 look-browser panel)
    /// GLOBAL, not per-image, same reasoning as `presets` (refreshes
    /// independent of `image`, see `refresh`).
    looks: Vec<InstalledLookRow>,
    /// A hover-only canvas preview: `(content_hash, previewed recipe)` for
    /// the bound image, set by [`EditBinding::set_hover_look`]. Mirrors
    /// `hover_preview` exactly (same non-committing contract), keyed by a
    /// raw content-hash string instead of a stored [`PresetId`] since a
    /// look reference IS the content hash (spec §4.8), there is no
    /// separate lookup/store round trip the way `preset_preview_recipe`
    /// needs.
    hover_look: Option<(String, Recipe)>,
    /// The crop-tool-active override (see
    /// [`EditBinding::set_crop_tool_active`]'s doc comment). Unlike
    /// `hover_preview`, this carries no snapshot, `recipe_for` re-derives
    /// the suppressed recipe from `working` fresh on every call while
    /// `true`.
    crop_tool_active: bool,
    /// Read caches need a refresh on the next `bind`.
    dirty: bool,
    /// E10 D12: the live histogram, pushed once per frame by `lib.rs` from
    /// `EditorCanvas::latest_histogram` (see
    /// [`SessionEditBinding::set_latest_histogram`]), the canvas, not the
    /// edit session, computes this, but `EditBinding` is the only seam
    /// `DevelopCtx`'s panels read through (mirrors
    /// [`SessionEditBinding::crop_tool_active`]'s same canvas→binding push).
    latest_histogram: Option<HistogramData>,
}

impl SessionEditBinding {
    /// A binding over `session` (cheap `Arc`-backed clone).
    pub fn new(session: Session) -> SessionEditBinding {
        SessionEditBinding {
            fallback: SessionRecipeSource::new(session.clone()),
            session,
            image: None,
            working: None,
            rev: 0,
            head_seq: 0,
            history: Vec::new(),
            snapshots: Vec::new(),
            presets: Vec::new(),
            hover_preview: None,
            looks: Vec::new(),
            hover_look: None,
            crop_tool_active: false,
            dirty: false,
            latest_histogram: None,
        }
    }

    /// D12: `lib.rs`'s once-per-frame push from `EditorCanvas::
    /// latest_histogram`, the canvas computes it (GPU-resident), this
    /// binding only carries the latest snapshot so panels can read it
    /// through the one seam they have (`EditBinding::histogram`).
    pub fn set_latest_histogram(&mut self, data: Option<HistogramData>) {
        self.latest_histogram = data;
    }

    /// Binds the active image (called once per frame from `lib.rs`, after
    /// events drain). Rebinding opens the image's E09 session (idempotent)
    /// and refreshes the read caches; binding the same image is a no-op
    /// unless something marked the caches dirty.
    pub fn bind(&mut self, image: Option<ImageId>) {
        if self.image != image {
            self.image = image;
            self.rev += 1; // never reuse a rev across a rebind
            self.hover_preview = None; // never let a preview leak across images
            self.hover_look = None; // same, a look hover must not leak either
            self.dirty = true;
        }
        if self.dirty {
            self.refresh();
        }
    }

    /// `Event::EditCommitted` hook (`lib.rs::drain_events`): a durable step
    /// landed, the working recipe and history caches may have moved (undo/
    /// redo/StepTo/snapshot-restore all rewrite the working recipe).
    pub fn on_edit_committed(&mut self, image: ImageId) {
        if self.image == Some(image) {
            self.dirty = true;
        }
        // Non-bound images keep the Phase-C persisted-read discipline.
        self.fallback.mark_dirty(image);
    }

    /// `Event::CatalogChanged` hook: covers the durable edit commands that
    /// emit no `EditCommitted` (`ClearHistory`, snapshot create/rename/
    /// delete). Cheap: two small queries on the next `bind`, only for the
    /// bound image.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The bound image's process version (identity/default seed).
    fn pv(&self) -> ProcessVersion {
        self.working.as_ref().map(|r| r.pv).unwrap_or(PV_M0)
    }

    fn submit_edit(&self, cmd: EditCommand) {
        self.session.submit(Command::Edit(cmd));
    }

    /// Re-pulls the working recipe + history/snapshot/preset caches for the
    /// bound image. Never fatal: read errors keep the previous cache (spec
    /// §7).
    fn refresh(&mut self) {
        self.dirty = false;

        // Presets are GLOBAL (not per-image), refresh unconditionally, even
        // with no bound image, so the preset panel has content before any
        // file is opened. Scoped: `Queries` holds a pooled read connection.
        {
            let q = self.session.query();
            match q.presets() {
                Ok(presets) => self.presets = presets,
                Err(err) => tracing::warn!(
                    target: "lightbox_shell",
                    %err,
                    "presets query failed; keeping the previous list"
                ),
            }
        }

        // Installed looks are GLOBAL too (D11), same reasoning as presets.
        {
            let q = self.session.query();
            match q.installed_looks() {
                Ok(looks) => self.looks = looks,
                Err(err) => tracing::warn!(
                    target: "lightbox_shell",
                    %err,
                    "installed_looks query failed; keeping the previous list"
                ),
            }
        }

        let Some(image) = self.image else {
            self.working = None;
            self.history.clear();
            self.snapshots.clear();
            self.head_seq = 0;
            return;
        };
        let hub = self.session.edits();
        if let Err(err) = hub.open(image) {
            tracing::warn!(
                target: "lightbox_shell",
                %err,
                image = image.0,
                "EditHub::open failed; panels fall back to neutral values"
            );
        }
        let fresh = hub.working_recipe(image);
        let changed = match (&self.working, &fresh) {
            (Some(old), Some(new)) => **old != **new,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.rev += 1;
        }
        self.working = fresh;

        // Scoped: `Queries` holds a pooled read connection, drop promptly.
        {
            let q = self.session.query();
            match q.edit_history(image) {
                Ok(history) => self.history = history,
                Err(err) => tracing::warn!(
                    target: "lightbox_shell",
                    %err,
                    image = image.0,
                    "edit_history failed; keeping the previous list"
                ),
            }
            match q.snapshots(image) {
                Ok(snapshots) => self.snapshots = snapshots,
                Err(err) => tracing::warn!(
                    target: "lightbox_shell",
                    %err,
                    image = image.0,
                    "snapshots query failed; keeping the previous list"
                ),
            }
        }
        self.head_seq = self
            .history
            .iter()
            .find(|s| s.is_head)
            .map(|s| s.seq)
            .unwrap_or(0);
    }
}

impl EditBinding for SessionEditBinding {
    fn value(&self, p: ParamId) -> ParamValue {
        match &self.working {
            Some(recipe) => recipe.get(p),
            // Unbound: neutral, panels aren't rendered without an active
            // image, so this is a defensive default, not a hot path.
            None => Recipe::identity(PV_M0).get(p),
        }
    }

    fn default(&self, p: ParamId) -> ParamValue {
        // The neutral identity value under the bound image's PV. NOTE:
        // `Recipe::default_for(pv, probe)` equals identity at M1 (E09's own
        // recorded deviation, probe-seeded defaults are deferred), so
        // identity IS the honest reset target today.
        Recipe::identity(self.pv()).get(p)
    }

    fn begin_gesture(&mut self, p: ParamId) {
        let Some(image) = self.image else { return };
        if let Err(err) = self
            .session
            .edits()
            .begin_gesture(image, StepLabel::Param(p))
        {
            tracing::warn!(
                target: "lightbox_shell",
                %err,
                image = image.0,
                "begin_gesture failed"
            );
        }
    }

    fn preview(&mut self, d: ParamDelta) {
        let Some(image) = self.image else { return };
        let hub = self.session.edits();
        if let Err(err) = hub.update_gesture(image, d) {
            // Validation failure (e.g. a non-monotone curve), the working
            // recipe is unchanged; never fatal.
            tracing::warn!(
                target: "lightbox_shell",
                %err,
                image = image.0,
                "update_gesture rejected the delta"
            );
            return;
        }
        let fresh = hub.working_recipe(image);
        let changed = match (&self.working, &fresh) {
            (Some(old), Some(new)) => **old != **new,
            (None, None) => false,
            _ => true,
        };
        if changed {
            // §6.5: bump on every EFFECTIVE change → the canvas's C3
            // submit key sees it and resubmits this same frame.
            self.rev += 1;
            self.working = fresh;
        }
    }

    fn end_gesture(&mut self) {
        let Some(image) = self.image else { return };
        // ONE coalesced durable step; a no-net-change gesture is E09's
        // typed no-op. The resulting `EditCommitted`/`CatalogChanged`
        // events drive the cache refresh.
        self.submit_edit(EditCommand::CommitGesture { image });
    }

    fn reset(&mut self, p: ParamId) {
        // Immediate commit (spec §6.5): a one-shot gesture back to default.
        let default = self.default(p);
        self.begin_gesture(p);
        let mut delta = ParamDelta::new();
        delta.0.insert(p, default);
        self.preview(delta);
        self.end_gesture();
    }

    fn recipe_rev(&self) -> u64 {
        self.rev
    }

    fn can_undo(&self) -> bool {
        self.head_seq > 0
    }

    fn can_redo(&self) -> bool {
        self.history.iter().any(|s| s.seq > self.head_seq)
    }

    fn undo(&mut self) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::Undo { image });
    }

    fn redo(&mut self) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::Redo { image });
    }

    fn history(&self) -> &[HistoryStepMeta] {
        &self.history
    }

    fn restore_step(&mut self, seq: u64) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::StepTo { image, seq });
    }

    fn clear_history(&mut self) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::ClearHistory { image });
    }

    fn snapshots(&self) -> &[SnapshotMeta] {
        &self.snapshots
    }

    fn create_snapshot(&mut self, name: &str) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::CreateSnapshot {
            image,
            name: name.to_owned(),
        });
    }

    fn restore_snapshot(&mut self, snapshot: SnapshotId) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::RestoreSnapshot { image, snapshot });
    }

    fn rename_snapshot(&mut self, snapshot: SnapshotId, name: &str) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::RenameSnapshot {
            image,
            snapshot,
            name: name.to_owned(),
        });
    }

    // ── Presets ──────────────────────────────────────────────────────

    fn presets(&self) -> &[PresetMeta] {
        &self.presets
    }

    fn apply_preset(&mut self, preset: PresetId) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::ApplyPreset {
            images: vec![image],
            preset,
        });
    }

    fn create_preset(&mut self, name: &str, group: Option<&str>, subset: ParamSubset) {
        let Some(image) = self.image else { return };
        self.submit_edit(EditCommand::CreatePreset {
            from: image,
            name: name.to_owned(),
            group: group.map(str::to_owned),
            subset,
        });
    }

    fn import_preset_files(&mut self, paths: Vec<PathBuf>) {
        // Not tied to the bound image (a preset library import applies
        // regardless of what's open), submit unconditionally.
        self.submit_edit(EditCommand::ImportPresetFiles { paths });
    }

    fn delete_preset(&mut self, preset: PresetId) {
        self.submit_edit(EditCommand::DeletePreset { preset });
    }

    fn rename_preset(&mut self, preset: PresetId, name: &str) {
        self.submit_edit(EditCommand::RenamePreset {
            preset,
            name: name.to_owned(),
        });
    }

    fn export_preset(&mut self, preset: PresetId, dest: PathBuf) {
        self.submit_edit(EditCommand::ExportPreset { preset, dest });
    }

    fn set_hover_preset(&mut self, preset: Option<PresetId>) {
        let Some(image) = self.image else {
            self.hover_preview = None;
            return;
        };
        match preset {
            None => {
                if self.hover_preview.take().is_some() {
                    // §6.5: bump on every EFFECTIVE change so the canvas's
                    // C3 submit key resubmits the real working recipe.
                    self.rev += 1;
                }
            }
            Some(id) => {
                if self.hover_preview.as_ref().map(|(pid, _)| pid) == Some(&id) {
                    return; // already showing this preset, no-op
                }
                // Pure query (spec §3.7 `preview_recipe`): reads the CURRENT
                // working recipe, never mutates the session/store/history.
                let q = self.session.query();
                match q.preset_preview_recipe(image, id.clone()) {
                    Ok(previewed) => {
                        self.hover_preview = Some((id, previewed));
                        self.rev += 1;
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "lightbox_shell",
                            %err,
                            image = image.0,
                            "preset_preview_recipe failed; canvas keeps showing the real edit"
                        );
                    }
                }
            }
        }
    }

    // ── Creative looks ───────────────────────────────────────────────

    fn installed_looks(&self) -> &[InstalledLookRow] {
        &self.looks
    }

    fn apply_look(&mut self, content_hash: &str) {
        // ONE durable commit (spec §4.8 unity amount = 100), the same
        // "immediate one-shot gesture" shape `reset` uses.
        self.begin_gesture(ParamId::CreativeLut);
        let mut delta = ParamDelta::new();
        delta.0.insert(
            ParamId::CreativeLut,
            ParamValue::Lut(Some(CreativeLut {
                id: content_hash.to_owned(),
                amount: 100.0,
            })),
        );
        self.preview(delta);
        self.end_gesture();
    }

    fn set_hover_look(&mut self, content_hash: Option<&str>) {
        if self.image.is_none() {
            self.hover_look = None;
            return;
        }
        match content_hash {
            None => {
                if self.hover_look.take().is_some() {
                    self.rev += 1; // resubmit the real working recipe
                }
            }
            Some(hash) => {
                if self.hover_look.as_ref().map(|(h, _)| h.as_str()) == Some(hash) {
                    return; // already showing this look, no-op
                }
                // Pure local transform of the CURRENT working recipe (spec
                // §6.5's non-committing preview contract), unlike
                // `set_hover_preset`, there is no stored-preset lookup: a
                // look reference IS the content hash, so no
                // `Queries::preset_preview_recipe`-shaped round trip exists
                // or is needed.
                let Some(working) = &self.working else { return };
                let mut previewed = (**working).clone();
                previewed.global.effects.creative_lut = Some(CreativeLut {
                    id: hash.to_owned(),
                    amount: 100.0,
                });
                self.hover_look = Some((hash.to_owned(), previewed));
                self.rev += 1;
            }
        }
    }

    fn install_look_file(&mut self, path: PathBuf, family: Option<String>) {
        // Not tied to the bound image (mirrors `import_preset_files`)
        // submit unconditionally.
        self.session.submit(Command::InstallLook { path, family });
    }

    fn remove_look(&mut self, id: LookId) {
        self.session.submit(Command::RemoveLook { id });
    }

    fn set_crop_tool_active(&mut self, active: bool) {
        if self.crop_tool_active == active {
            return;
        }
        self.crop_tool_active = active;
        // §6.5: bump on every EFFECTIVE change so the canvas's C3 submit
        // key resubmits this same frame, armed shows the uncropped
        // full-frame recipe, disarmed reverts to the real cropped one.
        self.rev += 1;
    }

    fn histogram(&self) -> Option<&HistogramData> {
        self.latest_histogram.as_ref()
    }
}

impl RecipeSource for SessionEditBinding {
    /// The canvas's C3 seam: the bound image renders the **live in-memory
    /// working recipe** (gesture previews included, same frame), or, while
    /// a preset row is hovered, the PURE hover-preview recipe (never
    /// committed), or, while the crop tool is armed, the working recipe
    /// with `geometry.crop` suppressed to full-frame (see
    /// [`EditBinding::set_crop_tool_active`]), any other image falls back
    /// to the Phase-C persisted-store read.
    fn recipe_for(&mut self, image: ImageId) -> RecipeSnapshot {
        if self.image == Some(image) {
            if let Some((_, preview)) = &self.hover_preview {
                return RecipeSnapshot {
                    recipe: preview.clone(),
                    pv: preview.pv,
                    rev: self.rev,
                };
            }
            if let Some((_, preview)) = &self.hover_look {
                return RecipeSnapshot {
                    recipe: preview.clone(),
                    pv: preview.pv,
                    rev: self.rev,
                };
            }
            if let Some(working) = &self.working {
                if self.crop_tool_active {
                    let mut recipe = (**working).clone();
                    recipe.geometry.crop = Crop::default();
                    return RecipeSnapshot {
                        pv: recipe.pv,
                        rev: self.rev,
                        recipe,
                    };
                }
                return RecipeSnapshot {
                    recipe: (**working).clone(),
                    pv: working.pv,
                    rev: self.rev,
                };
            }
        }
        self.fallback.recipe_for(image)
    }
}

/// Preset-panel wiring proofs (the shell-side seam over an already-proven
/// engine, `lightbox-edit`'s `preset.rs`/`preset_transfer_e2e.rs`): create →
/// apply → undo through the REAL `SessionEditBinding`/command bus, hover
/// preview writes no history, and export → import wiring lands a preset a
/// second, independent session can apply.
#[cfg(test)]
mod preset_wiring_tests {
    use super::*;
    use lightbox_core::{Core, CoreConfig, Event, OpenOrigin, OpenRequest};
    use lightbox_edit::ParamGroup;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tokio::sync::broadcast::error::TryRecvError;

    /// The same tiny, pinned, self-made CC0 JPEG `working_set.rs`'s own
    /// tests embed, no `cargo xtask fixtures` dependency needed here.
    const TINY_JPEG: &[u8] = include_bytes!("../../../../tools/xtask/assets/lightbox-tiny.jpg");
    const EVENT_TIMEOUT: Duration = Duration::from_secs(15);

    /// A fresh `Core`/`Session` over a throwaway catalog AND a throwaway
    /// preset dir (never the developer's real `<config>/Lightbox/presets`).
    fn start_session(tmp: &Path) -> (Core, Session) {
        let mut cfg = CoreConfig::default();
        cfg.preset_dir = Some(tmp.join("presets"));
        let core = Core::start(cfg).expect("core start");
        let session = core
            .create_catalog(&tmp.join("cat.lbdata"), None)
            .expect("create catalog");
        (core, session)
    }

    fn stage_jpeg(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, TINY_JPEG).unwrap();
        path
    }

    /// Opens one image and returns its id once the working set has loaded
    /// and auto-activated it (mirrors `working_set.rs`'s own test harness).
    fn open_one_image(session: &Session, path: PathBuf) -> ImageId {
        let mut rx = session.events();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(vec![path], false, OpenOrigin::Cli),
        });
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the working set to load"
            );
            match rx.try_recv() {
                Ok(Event::WorkingSetLoadFinished { .. }) => break,
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Closed) => panic!("event channel closed while opening"),
            }
        }
        crate::working_set::WorkingSetView::new(session)
            .active_image()
            .expect("the first Ready entry auto-activates (spec §2.4)")
    }

    /// Every durable `Command::Edit` lands as (at least) one
    /// `Event::CatalogChanged` (`edit_hub.rs`'s own dispatch doc comment)
    /// the generic "this mutation's effects are now visible" signal.
    fn drain_until_catalog_changed(rx: &mut tokio::sync::broadcast::Receiver<Event>) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Event::CatalogChanged"
            );
            match rx.try_recv() {
                Ok(Event::CatalogChanged { .. }) => return,
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Closed) => panic!("event channel closed while draining"),
            }
        }
    }

    fn head_seq(binding: &SessionEditBinding) -> u64 {
        binding
            .history()
            .iter()
            .find(|s| s.is_head)
            .map(|s| s.seq)
            .unwrap_or(0)
    }

    fn set_exposure(
        binding: &mut SessionEditBinding,
        rx: &mut tokio::sync::broadcast::Receiver<Event>,
        image: ImageId,
        ev: f32,
    ) {
        binding.begin_gesture(ParamId::Exposure);
        let mut delta = ParamDelta::new();
        delta.0.insert(ParamId::Exposure, ParamValue::F32(ev));
        binding.preview(delta);
        assert_eq!(
            binding.value(ParamId::Exposure),
            ParamValue::F32(ev),
            "D2: the in-memory preview is immediate, no bus round trip"
        );
        binding.end_gesture();
        drain_until_catalog_changed(rx);
        binding.mark_dirty();
        binding.bind(Some(image));
    }

    /// **AC: create → apply → undo round-trips.** A preset created from a
    /// committed exposure edit, applied after resetting back to neutral,
    /// lands the exact value as ONE new history step; `Undo` restores the
    /// pre-apply state exactly.
    #[test]
    fn create_apply_undo_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
        let image = open_one_image(&session, photo);

        let mut rx = session.events();
        let mut binding = SessionEditBinding::new(session.clone());
        binding.bind(Some(image));

        set_exposure(&mut binding, &mut rx, image, 1.5);
        assert_eq!(
            head_seq(&binding),
            1,
            "the exposure gesture landed as ONE step"
        );

        // Create a preset carrying only the Tone group (Exposure lives
        // there, `params::group_of`) from the current edit.
        binding.create_preset(
            "Round Trip Test",
            Some("QA"),
            ParamSubset::from_groups([ParamGroup::Tone]),
        );
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        let created = binding
            .presets()
            .iter()
            .find(|p| p.name == "Round Trip Test")
            .cloned()
            .expect("the created preset appears in the panel's cached list");
        assert_eq!(created.group.as_deref(), Some("QA"));

        // Reset to neutral, then APPLY the preset.
        binding.reset(ParamId::Exposure);
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(binding.value(ParamId::Exposure), ParamValue::F32(0.0));
        let head_before_apply = head_seq(&binding);

        binding.apply_preset(created.id.clone());
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(
            binding.value(ParamId::Exposure),
            ParamValue::F32(1.5),
            "apply landed the preset's exposure value"
        );
        assert_eq!(
            head_seq(&binding),
            head_before_apply + 1,
            "apply landed as exactly ONE new history step"
        );

        // UNDO restores exactly.
        binding.undo();
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(
            binding.value(ParamId::Exposure),
            ParamValue::F32(0.0),
            "undo restored the pre-apply exposure value exactly"
        );
        assert_eq!(head_seq(&binding), head_before_apply);
    }

    /// **AC: hover-preview writes no history (probe).** Setting a hover
    /// preset changes what the CANVAS sees (`RecipeSource::recipe_for`)
    /// immediately, but the durable working value/history head are
    /// untouched, proving `set_hover_preset` never submits a command.
    #[test]
    fn hover_preview_writes_no_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
        let image = open_one_image(&session, photo);

        let mut rx = session.events();
        let mut binding = SessionEditBinding::new(session.clone());
        binding.bind(Some(image));

        set_exposure(&mut binding, &mut rx, image, 1.5);
        binding.create_preset(
            "Hover Probe",
            None,
            ParamSubset::from_groups([ParamGroup::Tone]),
        );
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        let preset = binding
            .presets()
            .iter()
            .find(|p| p.name == "Hover Probe")
            .cloned()
            .unwrap();

        // Reset to neutral so the preview (1.5) is visibly distinct from
        // the current working value (0.0).
        binding.reset(ParamId::Exposure);
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        let head_before_hover = head_seq(&binding);
        let rev_before_hover = binding.recipe_rev();

        binding.set_hover_preset(Some(preset.id.clone()));
        let snapshot = binding.recipe_for(image);
        assert_eq!(
            snapshot.recipe.get(ParamId::Exposure),
            ParamValue::F32(1.5),
            "the canvas sees the previewed value immediately"
        );
        assert_ne!(
            binding.recipe_rev(),
            rev_before_hover,
            "hover bumps rev so the canvas resubmits this frame"
        );
        // No command was ever submitted for a hover, so there is nothing
        // to wait for, assert directly on durable state (never race a
        // shared event channel that also carries unrelated background
        // preview-pipeline traffic).
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(
            head_seq(&binding),
            head_before_hover,
            "hover preview wrote NO history"
        );
        assert_eq!(
            binding.value(ParamId::Exposure),
            ParamValue::F32(0.0),
            "the durable working value is untouched by hover"
        );

        binding.set_hover_preset(None);
        let cleared = binding.recipe_for(image);
        assert_eq!(
            cleared.recipe.get(ParamId::Exposure),
            ParamValue::F32(0.0),
            "clearing the hover restores the real working recipe on the canvas"
        );
    }

    /// **AC (E11 canvas-display fix, Part B):** with a REAL, committed,
    /// non-identity crop, the canvas renders that crop by default; while
    /// `set_crop_tool_active(true)` is set, `recipe_for` returns the SAME
    /// working recipe with `geometry.crop` suppressed to full-frame (so
    /// the crop gizmo can show what's outside the current crop), writing
    /// no history and leaving the durable value untouched, exactly like
    /// `hover_preview_writes_no_history` above, and reverts to the real
    /// cropped recipe the instant the override clears.
    #[test]
    fn crop_tool_active_override_suppresses_crop_then_reverts_on_deactivate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
        let image = open_one_image(&session, photo);

        let mut rx = session.events();
        let mut binding = SessionEditBinding::new(session.clone());
        binding.bind(Some(image));

        // Commit a real, non-identity crop (mirrors
        // `crop_gizmo.rs::real_binding`'s own commit shape).
        let committed = Crop {
            left: 0.1,
            top: 0.05,
            right: 0.9,
            bottom: 0.8,
        };
        binding.begin_gesture(ParamId::Crop);
        let mut delta = ParamDelta::new();
        delta.0.insert(ParamId::Crop, ParamValue::Crop(committed));
        binding.preview(delta);
        binding.end_gesture();
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));

        let default_view = binding.recipe_for(image);
        assert_eq!(
            default_view.recipe.geometry.crop, committed,
            "the canvas renders the REAL committed crop by default"
        );
        let rev_before = binding.recipe_rev();

        // Arm the crop tool: the canvas must see the UNCROPPED full frame.
        binding.set_crop_tool_active(true);
        assert_ne!(
            binding.recipe_rev(),
            rev_before,
            "activating the override bumps rev so the canvas resubmits this frame"
        );
        let suppressed = binding.recipe_for(image);
        assert_eq!(
            suppressed.recipe.geometry.crop,
            Crop::default(),
            "crop suppressed to full-frame while the tool is active"
        );
        assert_eq!(
            binding.value(ParamId::Crop),
            ParamValue::Crop(committed),
            "the durable working value is untouched by the override"
        );

        // A second `set_crop_tool_active(true)` while already active is a
        // no-op (mirrors `set_hover_preset`'s own dedup), no spurious rev
        // bump / resubmit.
        let rev_while_active = binding.recipe_rev();
        binding.set_crop_tool_active(true);
        assert_eq!(binding.recipe_rev(), rev_while_active);

        // Disarm: the REAL cropped recipe renders again.
        binding.set_crop_tool_active(false);
        let restored = binding.recipe_for(image);
        assert_eq!(
            restored.recipe.geometry.crop, committed,
            "reverts to the real committed crop on deactivate"
        );
    }

    /// **AC: export → import wiring.** A preset exported to a file, then
    /// imported into a SECOND, independent session's store, applies to a
    /// fresh image with the exact same value, proving the panel's
    /// export/import buttons are wired to a genuinely portable file (the
    /// byte-level "equal preset" content proof already lives at the engine
    /// level, `lightbox-edit`'s `preset_transfer_e2e.rs`).
    #[test]
    fn export_then_import_applies_the_same_value_in_a_second_session() {
        let tmp1 = tempfile::TempDir::new().unwrap();
        let (_core1, session1) = start_session(tmp1.path());
        let photo1 = stage_jpeg(&tmp1.path().join("photos"), "a.jpg");
        let image1 = open_one_image(&session1, photo1);

        let mut rx1 = session1.events();
        let mut binding1 = SessionEditBinding::new(session1.clone());
        binding1.bind(Some(image1));
        set_exposure(&mut binding1, &mut rx1, image1, 2.25);
        binding1.create_preset(
            "Exported Preset",
            Some("QA"),
            ParamSubset::from_groups([ParamGroup::Tone]),
        );
        drain_until_catalog_changed(&mut rx1);
        binding1.mark_dirty();
        binding1.bind(Some(image1));
        let source = binding1
            .presets()
            .iter()
            .find(|p| p.name == "Exported Preset")
            .cloned()
            .unwrap();

        let dest = tmp1.path().join("exported").join("Exported Preset.xmp");
        binding1.export_preset(source.id.clone(), dest.clone());
        drain_until_catalog_changed(&mut rx1);
        assert!(dest.is_file(), "export wrote the file");

        // A second, INDEPENDENT session/store, proves the file is
        // portable, not just a same-store round trip.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let (_core2, session2) = start_session(tmp2.path());
        let photo2 = stage_jpeg(&tmp2.path().join("photos"), "b.jpg");
        let image2 = open_one_image(&session2, photo2);

        let mut rx2 = session2.events();
        let mut binding2 = SessionEditBinding::new(session2.clone());
        binding2.bind(Some(image2));

        binding2.import_preset_files(vec![dest]);
        drain_until_catalog_changed(&mut rx2);
        binding2.mark_dirty();
        binding2.bind(Some(image2));
        let imported = binding2
            .presets()
            .iter()
            .find(|p| p.name == "Exported Preset")
            .cloned()
            .expect("the imported preset appears in the second session's list");
        assert_eq!(imported.group.as_deref(), Some("QA"));

        binding2.apply_preset(imported.id.clone());
        drain_until_catalog_changed(&mut rx2);
        binding2.mark_dirty();
        binding2.bind(Some(image2));
        assert_eq!(
            binding2.value(ParamId::Exposure),
            ParamValue::F32(2.25),
            "the imported preset applies the exact value exported from session 1"
        );
    }
}

/// E10 Phase D (tasks D10/D11) wiring proofs, the shell-side seam over the
/// REAL `installed_look` catalog table + the REAL render engine (not just
/// `lightbox-catalog`'s own DAO tests or `lightbox-render`'s own resolver
/// tests): install a real `.cube` → apply → renders through
/// `CreativeLutNode` (non-identity) → `RemoveLook` → the same recipe renders
/// identity again + the missing-look badge state is queryable; hover
/// live-preview writes no history. Deliberately self-contained (its own
/// small harness helpers) rather than sharing `preset_wiring_tests`'
/// module-private ones.
#[cfg(test)]
mod look_wiring_tests {
    use super::*;
    use lightbox_core::{Core, CoreConfig, Event, OpenOrigin, OpenRequest};
    use lightbox_jobs::CancelToken;
    use lightbox_render::ng::{
        OutFormat, OutputPayload, RenderPriority, RenderRequest, RenderScale, RenderState,
        RenderTarget, Roi,
    };
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tokio::sync::broadcast::error::TryRecvError;

    /// The same tiny, pinned, self-made CC0 JPEG `working_set.rs`'s/
    /// `preset_wiring_tests`' own tests embed, 16×16, no `cargo xtask
    /// fixtures` dependency needed here.
    const TINY_JPEG: &[u8] = include_bytes!("../../../../tools/xtask/assets/lightbox-tiny.jpg");
    const EVENT_TIMEOUT: Duration = Duration::from_secs(15);

    fn start_session(tmp: &Path) -> (Core, Session) {
        let mut cfg = CoreConfig::default();
        cfg.preset_dir = Some(tmp.join("presets"));
        let core = Core::start(cfg).expect("core start");
        let session = core
            .create_catalog(&tmp.join("cat.lbdata"), None)
            .expect("create catalog");
        (core, session)
    }

    fn stage_jpeg(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, TINY_JPEG).unwrap();
        path
    }

    fn open_one_image(session: &Session, path: PathBuf) -> ImageId {
        let mut rx = session.events();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(vec![path], false, OpenOrigin::Cli),
        });
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the working set to load"
            );
            match rx.try_recv() {
                Ok(Event::WorkingSetLoadFinished { .. }) => break,
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Closed) => panic!("event channel closed while opening"),
            }
        }
        crate::working_set::WorkingSetView::new(session)
            .active_image()
            .expect("the first Ready entry auto-activates (spec §2.4)")
    }

    fn drain_until_catalog_changed(rx: &mut tokio::sync::broadcast::Receiver<Event>) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Event::CatalogChanged"
            );
            match rx.try_recv() {
                Ok(Event::CatalogChanged { .. }) => return,
                Ok(Event::CommandFailed { error, .. }) => panic!("command failed: {error}"),
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Closed) => panic!("event channel closed while draining"),
            }
        }
    }

    fn head_seq(binding: &SessionEditBinding) -> u64 {
        binding
            .history()
            .iter()
            .find(|s| s.is_head)
            .map(|s| s.seq)
            .unwrap_or(0)
    }

    /// The bound image's current durable working recipe, straight off
    /// `EditHub` (the same source `SessionEditBinding::refresh` reads), the
    /// render input for this module's end-to-end proofs.
    fn working_recipe(session: &Session, image: ImageId) -> Recipe {
        (*session
            .edits()
            .working_recipe(image)
            .expect("working recipe present"))
        .clone()
    }

    /// Renders `recipe` through the REAL engine (`Session::engine`, the
    /// exact seam `lightbox-cli render`/the shell's canvas ride on) at the
    /// tiny fixture's native 16×16 and returns the raw output bytes.
    fn render_bytes(session: &Session, image: ImageId, recipe: Recipe) -> Vec<u8> {
        let engine = session.engine();
        let req = RenderRequest {
            image,
            pv: recipe.pv,
            recipe,
            roi: Roi {
                x: 0,
                y: 0,
                w: 16,
                h: 16,
            },
            scale: RenderScale::OneToOne,
            target: RenderTarget::Buffer {
                format: OutFormat::Rgba8Srgb,
            },
            priority: RenderPriority::Batch,
            cancel: CancelToken::new(),
        };
        let ticket = engine.submit(req);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match engine.poll(&ticket) {
                RenderState::Complete(out) => {
                    let OutputPayload::Pixels(px) = out.payload else {
                        panic!("expected pixels")
                    };
                    return px.bytes;
                }
                RenderState::Failed(e) => panic!("render failed: {e}"),
                RenderState::Cancelled => panic!("render cancelled"),
                _ => {
                    if Instant::now() > deadline {
                        panic!("render did not complete within 20s");
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// A 2-entry `.cube` that swaps R and G in the companion domain, a
    /// small, REAL, non-identity look file (task D10's own "install a real
    /// small .cube" proof requirement; the exact table shape
    /// `lightbox-render`'s/`lightbox-core::looks`' own tests already use).
    fn swap_rg_cube_text() -> &'static str {
        "LUT_3D_SIZE 2\n\
         0 0 0\n0 1 0\n1 0 0\n1 1 0\n0 0 1\n0 1 1\n1 0 1\n1 1 1\n"
    }

    /// **D10/D11 AC, end to end:** install a REAL small `.cube` → apply it
    /// as a look on a real recipe → it renders through `CreativeLutNode`
    /// (non-identity vs. the untouched baseline); then `RemoveLook` → the
    /// SAME recipe (still carrying the param, removal never rewrites the
    /// recipe) renders identity again, and
    /// [`EditBinding::is_look_installed`] (the D11 missing-look badge
    /// query) correctly flips to `false`.
    #[test]
    fn install_apply_render_then_remove_renders_identity_and_badge_is_queryable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
        let image = open_one_image(&session, photo);

        let mut rx = session.events();
        let mut binding = SessionEditBinding::new(session.clone());
        binding.bind(Some(image));

        let baseline = render_bytes(&session, image, working_recipe(&session, image));

        // Install a REAL small .cube.
        let cube_path = tmp.path().join("looks_src").join("swap.cube");
        std::fs::create_dir_all(cube_path.parent().unwrap()).unwrap();
        std::fs::write(&cube_path, swap_rg_cube_text()).unwrap();
        binding.install_look_file(cube_path, Some("Test".to_owned()));
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        let row = binding
            .installed_looks()
            .iter()
            .find(|l| l.name == "swap")
            .cloned()
            .expect("the installed look appears in the panel's cached list");
        assert!(binding.is_look_installed(&row.content_hash));

        // Apply it, ONE durable commit.
        binding.apply_look(&row.content_hash);
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(
            binding.value(ParamId::CreativeLut),
            ParamValue::Lut(Some(CreativeLut {
                id: row.content_hash.clone(),
                amount: 100.0,
            }))
        );

        // Renders NON-identity: the real, resolved look is now visible.
        let with_look = render_bytes(&session, image, working_recipe(&session, image));
        assert_ne!(
            baseline, with_look,
            "applying a REAL, non-identity look must change rendered pixels"
        );

        // Remove it.
        binding.remove_look(LookId(row.id));
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));

        assert!(
            !binding.is_look_installed(&row.content_hash),
            "the D11 missing-look badge state must now be queryable as MISSING"
        );
        assert_eq!(
            binding.value(ParamId::CreativeLut),
            ParamValue::Lut(Some(CreativeLut {
                id: row.content_hash.clone(),
                amount: 100.0,
            })),
            "removal must NOT rewrite the recipe's param (spec §4.8: the parameter survives)"
        );

        // Renders identity again: the removed look's hash no longer
        // resolves, so `CreativeLutNode` degrades gracefully, proven
        // through the REAL engine, not just the pure-function contract.
        let after_remove = render_bytes(&session, image, working_recipe(&session, image));
        assert_eq!(
            baseline, after_remove,
            "after removal, a recipe still referencing the removed hash must render identity again"
        );
    }

    /// **AC: hover-preview writes no history (probe, mirrors
    /// `preset_wiring_tests::hover_preview_writes_no_history` exactly).**
    /// Setting a hover look changes what the CANVAS sees
    /// (`RecipeSource::recipe_for`) immediately, but the durable working
    /// value/history head are untouched, proving `set_hover_look` never
    /// submits a command.
    #[test]
    fn hover_look_writes_no_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
        let image = open_one_image(&session, photo);

        let mut rx = session.events();
        let mut binding = SessionEditBinding::new(session.clone());
        binding.bind(Some(image));

        let cube_path = tmp.path().join("looks_src").join("swap.cube");
        std::fs::create_dir_all(cube_path.parent().unwrap()).unwrap();
        std::fs::write(&cube_path, swap_rg_cube_text()).unwrap();
        binding.install_look_file(cube_path, None);
        drain_until_catalog_changed(&mut rx);
        binding.mark_dirty();
        binding.bind(Some(image));
        let row = binding
            .installed_looks()
            .iter()
            .find(|l| l.name == "swap")
            .cloned()
            .expect("installed");

        let head_before_hover = head_seq(&binding);
        let rev_before_hover = binding.recipe_rev();
        assert_eq!(binding.value(ParamId::CreativeLut), ParamValue::Lut(None));

        binding.set_hover_look(Some(&row.content_hash));
        let snapshot = binding.recipe_for(image);
        assert_eq!(
            snapshot
                .recipe
                .global
                .effects
                .creative_lut
                .as_ref()
                .map(|c| c.id.clone()),
            Some(row.content_hash.clone()),
            "the canvas sees the previewed look immediately"
        );
        assert_ne!(
            binding.recipe_rev(),
            rev_before_hover,
            "hover bumps rev so the canvas resubmits this frame"
        );

        // No command was ever submitted for a hover, so there is nothing to
        // wait for, assert directly on durable state (mirrors
        // `preset_wiring_tests`' own reasoning for the same shape of proof).
        binding.mark_dirty();
        binding.bind(Some(image));
        assert_eq!(
            head_seq(&binding),
            head_before_hover,
            "hover preview wrote NO history"
        );
        assert_eq!(
            binding.value(ParamId::CreativeLut),
            ParamValue::Lut(None),
            "the durable working value is untouched by hover"
        );

        binding.set_hover_look(None);
        let cleared = binding.recipe_for(image);
        assert_eq!(
            cleared.recipe.global.effects.creative_lut, None,
            "clearing the hover restores the real working recipe on the canvas"
        );
    }
}
