# E08 — Editor shell, drag-drop entry & develop UI

_Implementation spec, **v2.0**. This document **supersedes and replaces `E08-library-ui-culling.md` in its entirety** (the v1.x library-UI/culling epic is retired per architecture §10; the old file remains only as historical record). Author: staff engineer (epic planner). Inputs: `docs/plan/00-mandate.md` (**v2.0/v2.1**), `docs/plan/01-architecture.md` (**v2.0**, decision-complete — nothing here re-litigates stack, seams, data ownership, or budgets), `docs/plan/epics/E01-deviations.md` + `E01-handoff.md` (normative for the built surfaces), research reports 02 (develop UX), 04 (masking/gizmo conventions), 09 (performance & UX conventions)._

| | |
|---|---|
| **Epic id / slug** | E08 / `editor-shell` |
| **Milestone** | M1 (develop skeleton — the v2.0 headline milestone) |
| **Effort** | **L ~7–9 pw** (re-baselined *down* per §10: task roll-up in §8 sums to 45 dev-days ≈ 9 pw at the ceiling; the named cut-lines — A8, D5, E9, plus B5/H3-polish under M1 pressure — bring the floor to ~35–42 days ≈ 7–8.4 pw) |
| **Depends on** | **E01** (shell skeleton, shared-device zero-copy seam, command/query bus, virtualized-grid machinery, thumb cache), **E03** (preview pyramid, `best_available`, cache limits/relocate/purge/stats API), **E04** (working-set loader: `OpenWorkingSet` command, working-set snapshot + events, `SourceKind`), **E05** (engine: recipe-driven `RenderRequest`, progressive ladder), **E09** (edit session: typed params, gesture-coalesced commits, history/snapshots/undo) |
| **Soft seams** | E06 (job-concurrency knobs, activity model — consumed when it lands; E01's `JobConfig` covers M1), E10 (panel *content* beyond the M1 basic slice; histogram/clipping; preset browser & before/after land with E10 on E08's frameworks), E11/E12 (gizmo consumers), E13 (VRAM gating reads the prefs knob), E15 (export progress UI rides the status-bar activity indicator) |
| **Consumed by** | E10/E11/E12 (panel host + gizmo framework + keymap contexts), E13 (prefs), E15 (activity/status surface), E16 (packaging: file associations point at the launch handler) |
| **Architecture anchors** | §1.2 (egui shell + reversal trigger) · §2.1 v2.0 crate-map annotation (`drop-zone · filmstrip · loupe · develop · panels · gizmos`) · **§2.4 (editor-shell contract — the binding entry-model + raw-vs-non-raw table)** · §3.1.1 (edit store; info panel reads `metadata_cache`) · §3.3 (cache knobs the prefs panel binds) · §5.1/§5.3 (threading, cancellation) · §6 (device-lost, VRAM rows) · §7 v2.0 (surviving budgets) · §9 M1 · §10.0 (disposition audit) · §10 E08 row · Risk 4/6 |
| **Status** | Spec — ready for implementation |

---

## 1. Summary

E08 turns the built E01 walking-skeleton library shell into the **editor shell** of the v2.0 product: there is no library, no grid view, no culling. The window *is* the editor. Files enter by **drag-and-drop or the OS open dialog** (plus the OS launch handler), land in a **session filmstrip** (the E01 grid-virtualization machinery turned horizontal and bounded to tens–hundreds of files), and the active file fills the **loupe canvas** — every pixel still produced by `Engine::submit` on the one shared `wgpu::Device` (seam 2, unchanged). Around the canvas, E08 builds the **develop chrome**: the right-rail panel framework with the M1 basic-panel slice bound to E09's edit session (gesture-coalesced, auto-persisting), the **raw-vs-non-raw surface adaptation of §2.4** (raw-only panels *hidden*, never disabled-but-visible), the **on-canvas gizmo framework** E11/E12 will consume, the **remappable keymap** with cheat-sheet overlay (a named differentiator), a minimal EXIF **info panel** (folded in from retired E07), and the **performance-preferences panel** over a new typed prefs store (cache caps / GPU / VRAM / job knobs).

Everything lives **above the headless boundary** (§2.3 seam 1) except the prefs store and one catalog migration. No UI type crosses down; no SQL crosses up. The §1.2 reversal trigger stands: if egui's ceiling is hit, `lightbox-shell` swaps for a Qt/Slint shell against the same core API.

**The epic's first failing test (written before any rescope code):** `drop_to_loupe_and_edit` — an `egui_kittest` harness injects a synthetic `DroppedFile` (path of a fixture raw) into the empty-state shell. It must (a) submit exactly **one** `Command::OpenWorkingSet` with that path, (b) after the (test-double) working-set events arrive, show a one-entry filmstrip with the entry active in the loupe, and (c) dragging the Exposure slider must issue an engine submit **in the same frame** as the input and exactly **one** E09 gesture commit on release. It fails until the intake, working-set view, filmstrip, panel framework, and edit binding all exist — the epic's riskiest seams, exercised together.

---

## 2. Scope

### 2.1 In scope (all in `lightbox-shell` unless noted)

1. **Editor workspace chassis** — single-mode window: top bar (open button, view controls), central canvas, bottom filmstrip, right develop-panel rail (collapsible, with panel-solo mode), status bar with a minimal background-activity indicator. The E01 `View::{Grid,Loupe}` toggle, import bar, and grid view are removed from the app (see §2.2 retirement).
2. **Empty-state drop zone** (§2.4) — with no working set, the window is a full-bleed drop target ("Drop photos or a folder to start editing — or ⌘O") that reflects `RawInput.hovered_files` **before** the drop lands (highlight + file count; graceful when a platform reports no paths on hover).
3. **Entry intake — all six §2.4 rows, E08's half:** window drag-drop of a file / multi-file selection / folder / folder-with-recursive-modifier; the OS open dialog (`rfd`, multi-select files+folders); the platform **launch handler** (argv paths, file-association / "Open With", second-instance forwarding as a cut-line item). Intake normalizes to one `Command::OpenWorkingSet` (E04's command — §6.1).
4. **Working-set view model** — the shell-side subscriber of E04's working-set snapshot/events: ordered entries, per-entry load state, epoch-guarded replace semantics, no save-prompt ever (edits auto-persist, §3.1.1).
5. **Session filmstrip** — horizontally virtualized strip over the working set, reusing the built grid machinery (`layout`/`visible_indices` math, `ThumbCache` demand-driven cancel-on-scroll-out textures): active highlight, keyboard/click navigation, badges (RAW/edited/error/missing), resize, scroll-to-active.
6. **Loupe / editor canvas** — evolves E01's `LoupeView`: recipe-driven `RenderRequest` (from the E09 edit session, not `Recipe::identity`), fit/100%/stepped zoom + zoom-to-cursor + pan, progressive display (best preview tier immediately, engine result swaps in — never a blank canvas, §4.3), failure placards, `DeviceDegraded` notice, and the **`ViewXform`** image↔screen transform shared with gizmos.
7. **Develop panel framework + M1 basic panel** — `PanelDef` registry on the right rail; **`SourceReq` adaptivity per the §2.4 table (raw-only panels hidden for `Rendered` sources)**; the house `param_slider` control (scrub, fine drag, double-click reset, value entry, keyboard nudge); the M1 basic-panel slice: WB (temp/tint, as-shot, eyedropper mount), exposure/contrast/highlights/shadows/whites/blacks; a point tone-curve widget. Bound to E09 via the `EditBinding` gesture contract (preview-while-dragging, one history step per gesture).
8. **Edit chrome** — undo/redo actions, a compact history panel (restore-to-step, clear), snapshots section (create/restore/rename) over E09's commands/queries.
9. **On-canvas gizmo framework** — `Gizmo` trait, hit-testing, drag capture, paint layer over the composited image, keymap sub-contexts, plus one real reference gizmo (the WB eyedropper shell: emits an image-space `PickedPoint`; sampling semantics are E10's). E11 (crop/geometry) and E12 (mask pins/gizmos) build on this API — seam, not content.
10. **Remappable keymap** — declarative action registry, context-stack resolution, user overrides persisted to `keymap.toml`, conflict detection, rebind editor, per-context cheat-sheet overlay (`Cmd+/`).
11. **Info panel** — minimal EXIF readout for the active file (camera, lens, exposure, ISO, focal length, capture date, dimensions, file info) from `ImageDetail` + the per-open `metadata_cache` (§3.1.1: a read-only per-open cache, **not** a filter facet).
12. **Performance-preferences panel + prefs store** — `lightbox-core::prefs` typed store (machine-scope `prefs.toml` + catalog-scope `catalog_settings` table, watch channels): preview/raw-cache caps, T2 retention, cache relocate/purge with usage bars (E03's API), GPU enable + VRAM threshold knob (§6; stored for E13, displayed with the probe value), job-concurrency knobs (E01 `JobConfig` at M1; live via E06 when it lands), drop-recursive default, filmstrip height, theme.
13. **Accessibility & polish** — AccessKit labels for actions/panels/filmstrip cells; DPI/theming pass; smoke/perf harness rework for the new entry model.

### 2.2 Retired surfaces (v1.x E08 scope this spec deletes from the product)

Per §10.0 the *feature surfaces* are retired; repurposable code is transformed, dormant code is left in place, and nothing outside this epic's files is deleted.

| v1.x surface | Disposition in this epic |
|---|---|
| Library grid view (100k virtualized grid) | **Retired.** `grid.rs`'s virtualization math + cell painting are **repurposed** into `filmstrip.rs`; the grid view, `View` enum, and cell rating-star painting are removed from the shell |
| Culling grammar (P/X/U, stars, labels, auto-advance), compare/survey | **Retired.** No UI issues `SetRating`/`SetFlag` (the frozen core commands stay, dormant) |
| Library panels (folders/collections/keywords/filter bar/FTS), smart-collection UI | **Retired** with E07 (never built) |
| Import bar / import dialog / `ImportAddInPlace` UX | **Retired.** Entry is intake→`OpenWorkingSet`; the core command stays frozen-dormant; `--smoke`/`--perf` drivers migrate off it (§8 A7/H2) |
| Two-module (Library/Develop) mode bar | **Retired.** One editor workspace |
| Activity-center window | **Rescoped** to a status-bar activity indicator (progress + cancel); a full center is E06/E15-era polish |

### 2.3 Explicit non-goals (named seam for each)

- **No develop algorithms, no color science, no nodes.** Panels emit E09 `ParamDelta`s; pixels are E05+E10/E11. E08 never computes an image value (the WB eyedropper reports a *coordinate*; E10 owns sampling → temp/tint math).
- **No panel content beyond the M1 basic slice.** HSL, color grading, parametric curve, B&W, presence, detail, optics, geometry, effects panels are **E10/E11 content** mounted on E08's `PanelDef`/`param_slider`/gizmo frameworks at M2. The framework ships M1-proven with the basic panel.
- **No histogram / clipping indicators.** E10.4 owns them; E08 reserves the panel-rail slot above the basic panel and the canvas overlay hook.
- **No preset browser, copy/paste dialog, batch sync UI, before/after views.** E09 ships the engines; the UI lands with E10 (M2) on E08's panel/keymap frameworks. E08 provides the mounting points (`PanelDef`, actions).
- **No mask management UI** (overlays/pins/amount panel) — E12.3, on E08's gizmo layer + panel host.
- **No working-set loading logic.** Walking/probing/hashing/ordering/asset-row registration is **E04**; E08 turns OS events into one command and renders E04's state. No `lightbox-ingest` dependency from the shell.
- **No edit persistence logic.** Recipe schema, history, snapshots, XMP are **E09**; E08 calls the edit session and never touches `edit_recipe` SQL.
- **No preview/cache internals.** E08 calls `PreviewProvider`/cache APIs (E03) and binds their knobs; it never opens `.lbdata` files.
- **No export UI** (E15), **no AI/model UI** (E13/E14), **no secondary display / dual-monitor** (M4), **no recents list on the empty state** (a §2.4-permitted Should; deferred, seam = intake accepts any path list), **no soft proofing** (deferred tier).
- **No new render targets or engine changes.** The canvas consumes E05's `submit`/`poll`/`cancel` exactly as specced there.

### 2.4 Sequencing & dependency reality (against the M1 critical path)

E08 is the **integration point** of M1: it needs E04's command/events, E05's recipe-driven requests, E09's edit session, and E03's tiers. To avoid a big-bang merge:

- **Phases A–D (chassis, intake, filmstrip, canvas, keymap) need only E01 + E04** (the canvas runs on the E01 planner + identity recipe until E05/E09 land — exactly like today's loupe).
- **Phase E (panels/edit chrome) needs E09**; its end-to-end "slider changes pixels" AC additionally needs **E05 + the E10.1 node slice** — the binding, persistence, and gesture ACs are testable against E09 alone (a recipe change is observable in the store without a renderer).
- **Phase G (prefs) needs E03's** limits/stats/relocate/purge API for the cache section; the store itself needs nothing.
- Coordination duty: task A0 reconciles the §6.1 consumed contracts with E04/E09's planners **before** Phase A merges. Any drift is resolved in *their* specs (they own the seams); E08 records the delta in its deviations file.

---

## 3. Crates & modules touched (against the real code)

| Crate | Modules (existing → new) | Nature |
|---|---|---|
| `lightbox-shell` | `lib.rs` (app rework: single editor view, intake pump, working-set events), `grid.rs` → **`filmstrip.rs`** (horizontal repurpose; `layout`/`visible_indices`/`fit_rect` math retained + re-tested), `loupe.rs` → **`canvas/`** (`view.rs` recipe-driven loupe, `xform.rs` ViewXform, `gizmo.rs` layer + trait, `states.rs` placards), `model.rs` → **`working_set.rs`** (event-folding view model; `Selection` retained, rescoped to active+multi), `thumbs.rs` (retained as-is; filmstrip consumer), **`intake.rs`** (drop/dialog/launch normalization), **`keymap/`** (`chord.rs`, `registry.rs`, `dispatch.rs`, `overrides.rs`, `cheatsheet.rs`, `editor.rs`), **`panels/`** (`host.rs`, `develop_ctx.rs`, `widgets.rs` incl. `param_slider`, `basic.rs`, `curve.rs`, `history.rs`, `info.rs`), **`prefs_ui.rs`**, `smoke.rs`/`perf.rs` (reworked drivers), `main.rs` (argv/launch handler, `--smoke`/`--perf-strip` flags) | **Owned — the bulk of the epic** |
| `lightbox-core` | **`prefs/`** (typed store, two scopes, TOML persistence, watch channels); session wiring that hands `CatalogPrefs` writes to the single writer and binds watches to E03 setter APIs | **Owned** |
| `lightbox-catalog` | one migration: `catalog_settings` (§5.1); tiny DAO (`get/set_setting`) on `CatalogTxn`/`ReaderHandle` | **Owned (schema addition only)** |
| `lightbox-core` `Command`/`Event`/`Session` | consumed: `OpenWorkingSet` + working-set events/snapshot (**E04-owned**, §6.1); `EditCommand`/`EditQuery`/edit session (**E09-owned**); both enums are `#[non_exhaustive]` by design for exactly this growth | Seam |
| `lightbox-render` (E05) | consumed: `Engine::submit/poll/cancel` with recipe + progressive ladder; `RenderRequest` refinements are E05's | Seam |
| `lightbox-preview` (E03) | consumed: `PreviewProvider` (thumb path unchanged), `best_available`-class open path, cache stats/limits/purge/relocate commands | Seam |
| `lightbox-types` | consumed: `SourceKind { Raw, Rendered }` (**added by E04** per §10.0 "probe gains the `SourceKind` flag"); E08 only reads it | Seam |
| `lightbox-jobs` | consumed: `JobConfig` knobs (E01-built) for the prefs jobs section | Seam |

**New third-party dependencies** (all license-surface-1 only; run the deny gate before merge): `rfd` (MIT — native open dialog), `directories` (MIT/Apache — platform config dir for `prefs.toml`/`keymap.toml`), `toml` (MIT/Apache — prefs/keymap serde), `egui_kittest` (MIT/Apache, **dev-dependency** — UI harness). No native/FFI or bundled-data surface changes. Note: `rfd` on Linux may pull `ashpd`/zbus (XDG portal) — audit its feature flags against the allowlist in task A3 and prefer the GTK-free portal backend.

---

## 4. Raw-vs-non-raw develop surface (operationalizing §2.4)

The single authority is the probe-derived `SourceKind` on the working-set entry (E04). The shell rule, verbatim from §2.4: **raw sources expose the full toolset; non-raw sources expose the same toolset minus raw-only stages, which are *hidden* (not disabled)**.

| Surface element | `SourceKind::Raw` | `SourceKind::Rendered` | E08 mechanism |
|---|---|---|---|
| WB (Kelvin+tint, as-shot, eyedropper) | full | present, **relative** (re-labeled "Temp/Tint (relative)"; no Kelvin scale, no "As Shot" camera value) | `PanelDef.source_req = Any` + per-control `raw_variant`/`rendered_variant` labels (§6.5) |
| Camera profile / base look | full (E10.1 mounts) | **hidden** | `source_req = RawOnly` |
| Highlight reconstruction | full (E10 mounts) | **hidden** (only highlight *compression* appears, an E10 control) | `source_req = RawOnly` |
| Demosaic / raw-denoise / bad-pixel (E11) | full | **hidden** | `source_req = RawOnly` |
| Tone / curves / HSL / grading / B&W / presence | full | full | `source_req = Any` |
| Detail / optics / geometry (E10/E11) | full | full | `source_req = Any` |
| Masking / retouch (E12), Export (E15) | full | full | `source_req = Any` |

Switching the active file between a raw and a JPEG re-filters the rail **in one frame** with panel open/collapse state preserved per `PanelId` (so toggling files doesn't lose the user's layout). A mixed working set is the normal case, not an edge case — test it (§9).

---

## 5. Data model & persistence additions

E08 adds **no edit-store tables** (recipes/history/snapshots are E09; working set is deliberately *not persisted*, §2.4 — closing the app discards it, and rollback of this epic drops no data).

### 5.1 Catalog migration — `catalog_settings`

Catalog-scope knobs travel with the catalog (`.lbdata` is a catalog sibling, §3.3). One key-value table, JSON values:

```sql
-- migrations/NNNN_catalog_settings.sql
-- NNNN = next free number in docs/plan/migrations.md (0002 is E03's expected
-- claim; E09 claims its edit tables; reserve ours via a registry PR at merge —
-- `cargo xtask lint-migrations` enforces the sequence).
CREATE TABLE catalog_settings (
    key        TEXT PRIMARY KEY,               -- namespaced: "cache.preview_cap_bytes", ...
    value      TEXT NOT NULL,                  -- JSON-encoded typed value
    updated_at TEXT NOT NULL                   -- RFC3339 UTC, fixed 6-digit subseconds
                                               -- (E01 Phase-3 timestamp convention)
) STRICT;
```

Writes go through the single catalog writer as one-row transactions (one `Command`-less internal path: the prefs store owns a `WriterHandle` via core, mirroring how core wires `AssetLocator`). Reads at session open + on external change are snapshot reads.

### 5.2 Machine-scope files (outside the catalog)

| File | Location | Content | Write discipline |
|---|---|---|---|
| `prefs.toml` | `directories`-resolved config dir, `lightbox/prefs.toml` | machine prefs (§6.7 table), `prefs_version = 1` | temp-then-rename atomic; corrupt/missing ⇒ defaults + non-fatal status notice |
| `keymap.toml` | same dir | **user overrides only** (delta from registry defaults), `keymap_version = 1` | same discipline; unknown action ids preserved verbatim (forward-compat) |

Scope split (decision, recorded): GPU mode, job concurrency, VRAM override, theme, drop-recursive default describe the **machine**; cache caps/retention/location describe the **catalog**. A catalog-scope cache-relocation path that doesn't resolve on this machine falls back to the default location with a status badge — never an error (mirrors §6 missing-volume handling).

---

## 6. Interface definitions

Contracts, not implementations. **[consumed]** marks seams owned by a neighbor epic — reconciled in task A0; the neighbor's spec is authoritative for its side.

### 6.1 Entry intake & the E04 seam

E08's half of every §2.4 entry row: normalize to paths + disposition, submit **one** command, done.

```rust
// lightbox-shell::intake
/// Where paths came from (telemetry/status wording only — behavior identical).
pub enum IntakeSource { Drop, OpenDialog, Launch }

pub struct Intake;
impl Intake {
    /// Folds this frame's egui input into an intake decision.
    /// - `dropped_files` with paths → `Some(OpenRequest)` in drop order
    ///   (entries without a path are skipped + counted for the status line).
    /// - `hovered_files` → hover affordance state (count may be unknown on
    ///   platforms that hide paths/counts until drop — render "Drop to open").
    pub fn pump(input: &egui::InputState, prefs_recursive_default: bool) -> IntakeFrame;
}

pub struct IntakeFrame {
    pub hover: Option<HoverAffordance>,     // Some while files hover the window
    pub open: Option<OpenRequest>,          // Some on the frame a drop landed
}
pub struct HoverAffordance { pub count: Option<usize> }
pub struct OpenRequest {
    pub source: IntakeSource,
    pub paths: Vec<PathBuf>,                // files and/or folders, presentation order
    pub recursive: bool,                    // Alt held at drop-time, or the prefs default (§2.4)
}
```

**[consumed — E04 owns; shape proposed here, reconciled in A0]** the core command + events + snapshot:

```rust
// lightbox-core (E04 grows the #[non_exhaustive] Command/Event enums)
Command::OpenWorkingSet { sources: Vec<PathBuf>, recursive: bool }
// Replaces the current working set (epoch bump). Never prompts: edits
// auto-persist continuously (§3.1.1), so replace is always safe.

pub struct WorkingSetEpoch(pub u64);
Event::WorkingSetReplaced { epoch: WorkingSetEpoch, ticket: CommandTicket, discovered: usize }
Event::WorkingSetEntryLoaded { epoch: WorkingSetEpoch, index: usize }   // entry now Ready/Failed in the snapshot
Event::WorkingSetProgress { epoch: WorkingSetEpoch, done: usize, discovered: usize }
Event::WorkingSetFinished { epoch: WorkingSetEpoch, report: OpenReport }

impl Session {
    /// Cheap Arc snapshot of the current set (order = §2.4 rules: drop order
    /// for files, capture-time-then-name within a dropped folder).
    pub fn working_set(&self) -> Arc<WorkingSetSnapshot>;
}
pub struct WorkingSetSnapshot { pub epoch: WorkingSetEpoch, pub entries: Vec<WorkingSetEntry> }
pub struct WorkingSetEntry { pub path: PathBuf, pub state: EntryState }
pub enum EntryState {
    Loading,
    Ready(ReadyEntry),
    Failed { reason: String },              // unsupported/malformed/io — badged, never fatal
}
pub struct ReadyEntry {
    pub image: ImageId, pub asset: AssetId, pub hash: ContentHash,
    pub source_kind: SourceKind,            // lightbox-types addition (E04)
    pub width: u32, pub height: u32, pub orientation: Orientation,
    pub filename: String, pub capture_time: Option<String>,
    pub is_edited: bool,                    // edit_index projection (E09) — filmstrip badge
}
```

### 6.2 Working-set view model (`lightbox-shell::working_set`)

Replaces `ImageListModel` (which paged the catalog — the working set is session state, no pagination needed at tens–hundreds of files).

```rust
pub struct WorkingSetView {
    epoch: WorkingSetEpoch,
    entries: Arc<WorkingSetSnapshot>,       // refreshed on events / Lagged
    active: Option<usize>,                  // index into entries
    selected: HashSet<usize>,               // multi-select for future batch ops (M2 sync/export)
}
impl WorkingSetView {
    /// Folds one core event; stale-epoch events are dropped. On broadcast
    /// `Lagged`, re-pulls `session.working_set()` wholesale (same discipline
    /// as the E01 model's mark_dirty).
    pub fn on_event(&mut self, ev: &Event, session: &Session);
    pub fn entries(&self) -> &[WorkingSetEntry];
    pub fn active(&self) -> Option<(usize, &WorkingSetEntry)>;
    /// First Ready entry auto-activates when nothing is active (§2.4:
    /// "first is active in the loupe").
    pub fn set_active(&mut self, idx: usize);
    pub fn nav(&mut self, delta: isize);    // clamped; skips nothing (Failed entries are viewable placards)
    pub fn is_empty(&self) -> bool;         // drives the empty-state drop zone
}
```

### 6.3 Filmstrip (`lightbox-shell::filmstrip`)

The grid math, turned 90°:

```rust
/// Pure geometry (unit-tested like grid::layout was).
pub struct StripLayout { pub cell: f32, pub label_h: f32, pub total_w: f32 }
pub fn strip_layout(strip_h: f32, total: usize) -> StripLayout;
/// Index range intersecting the horizontal viewport [left, right).
pub fn visible_range(l: &StripLayout, total: usize, left: f32, right: f32) -> Range<usize>;

/// Renders the strip; reuses ThumbCache verbatim (want/end_frame/cancel-on-
/// scroll-out, bucket snapping). Returns activation requests.
pub fn filmstrip_ui(
    ui: &mut egui::Ui,
    view: &mut WorkingSetView,
    thumbs: &mut ThumbCache,
    visible_out: &mut HashSet<ImageId>,
) -> Option<FilmstripAction>;
pub enum FilmstripAction { Activate(usize) }
```

Cell chrome: thumbnail (or Loading shimmer / Failed placard), active ring, `RAW` tag for `SourceKind::Raw`, edited dot (`is_edited`), error badge with hover reason, filename strip. **No stars, no flags, no labels** (retired).

### 6.4 Editor canvas & ViewXform (`lightbox-shell::canvas`)

```rust
/// Image↔screen mapping for compositing AND gizmos — one authority.
/// Covers zoom, pan, orientation, pixels-per-point.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ViewXform { /* image size, view rect, zoom, pan, ppp */ }
impl ViewXform {
    pub fn image_to_screen(&self, image_px: egui::Vec2) -> egui::Pos2;
    pub fn screen_to_image(&self, screen: egui::Pos2) -> Option<egui::Vec2>; // None outside the image
    pub fn zoom(&self) -> f32;
}

pub enum ZoomMode { Fit, Percent(f32) }     // 100% = Percent(1.0); wheel steps the ladder
                                            // {Fit, 25, 50, 100, 200}, zoom-to-cursor

pub struct EditorCanvas { /* evolves LoupeView: ticket, displayed texture, xform, gizmos */ }
impl EditorCanvas {
    /// One frame: submit-on-change (image, recipe revision, zoom, viewport px),
    /// poll, composite zero-copy, run the gizmo layer, paint overlays.
    /// `recipe_rev` is E09's cheap change counter — the submit key includes it
    /// so a preview delta re-renders without hashing the recipe in the UI.
    pub fn ui(
        &mut self, ui: &mut egui::Ui, engine: &Engine,
        entry: &ReadyEntry, edit: &mut dyn EditBinding,
        gizmos: &mut GizmoLayer,
    ) -> Option<CanvasAction>;
    pub fn busy(&self) -> bool;
}
```

Progressive rule (§4.3, E03/E05 seam): on activation, composite the **best available preview tier immediately** (tier-badged, exactly like the M0 loupe badges "embedded preview"), then swap the engine render when `Ready` — the canvas never blanks and never shows an error while a stale-but-valid image exists (the E01 loupe's Failed-drops-stale behavior is *changed*: a failed re-render of the same image keeps the last good frame + a non-modal error chip; a failed render of a **newly activated** image shows the placard).

### 6.5 Develop panels & the E09 edit binding (`lightbox-shell::panels`)

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct PanelId(pub &'static str);       // "develop.basic", "develop.curve", "editor.info", ...

pub enum SourceReq { Any, RawOnly }         // §2.4: RawOnly panels are HIDDEN for Rendered

pub struct PanelDef {
    pub id: PanelId,
    pub title: &'static str,
    pub source_req: SourceReq,
    pub order: u16,                          // rail order; E10/E11 slot between E08 panels
    pub build: fn(&mut egui::Ui, &mut DevelopCtx<'_>),
}

pub struct PanelHost { /* registry + per-PanelId open/solo state (persisted machine-scope) */ }
impl PanelHost {
    pub fn register(&mut self, def: PanelDef);              // E08 registers M1 panels; E10/E11/E12 add theirs
    pub fn rail_ui(&mut self, ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>); // filters by ctx.source_kind
}

/// What every panel gets: the active file's kind + the edit gesture surface.
pub struct DevelopCtx<'a> {
    pub source_kind: SourceKind,
    pub edit: &'a mut dyn EditBinding,
}

/// [consumed — E09 owns the semantics; trait lives in lightbox-shell and is
/// implemented over E09's EditSession by a thin adapter in lib.rs]
pub trait EditBinding {
    fn value(&self, p: ParamId) -> ParamValue;              // current recipe value (incl. live preview)
    fn default(&self, p: ParamId) -> ParamValue;            // double-click reset target
    fn begin_gesture(&mut self, p: ParamId);
    fn preview(&mut self, d: ParamDelta);                   // in-memory only; bumps recipe_rev → canvas resubmits
    fn end_gesture(&mut self);                              // ONE coalesced commit = one history step (E09 discipline)
    fn reset(&mut self, p: ParamId);                        // immediate commit
    fn recipe_rev(&self) -> u64;
    fn can_undo(&self) -> bool; fn can_redo(&self) -> bool;
    fn undo(&mut self); fn redo(&mut self);
}

/// The house control (research 02 conventions): drag = scrub; Shift = fine;
/// double-click = reset; click value = text entry; ↑/↓ = nudge (Shift = fine).
pub struct SliderSpec { pub min: f64, pub max: f64, pub step: f64, pub fine: f64,
                        pub label: &'static str, pub unit: Option<&'static str> }
pub fn param_slider(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, p: ParamId, spec: &SliderSpec)
    -> egui::Response;
```

`ParamId`/`ParamDelta`/`ParamValue` are **[consumed — E09]** re-exports. The M1 basic panel binds exactly the §9 M1 set: `wb.temp`, `wb.tint`, `wb.mode`, `tone.exposure`, `tone.contrast`, `tone.highlights`, `tone.shadows`, `tone.whites`, `tone.blacks`, `curve.point` — id spellings are E09's; the mapping table is committed as `panels/basic.rs` doc comments + a doctest against E09's registry so drift fails the build.

### 6.6 Gizmo framework (`lightbox-shell::canvas::gizmo`)

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct GizmoId(pub &'static str);       // "wb.eyedropper", later "geom.crop", "mask.pin"…
pub struct HitId(pub u32);                  // gizmo-local handle id

pub enum GizmoEvent<'a> {
    Hover { image_px: Option<egui::Vec2> },
    DragStart { hit: HitId, image_px: egui::Vec2 },
    Drag { hit: HitId, image_px: egui::Vec2, delta_px: egui::Vec2 },
    DragEnd { hit: HitId },
    Click { image_px: egui::Vec2 },
    Key(&'a egui::Event),                    // Esc/Enter routed when the gizmo is active
}
pub enum GizmoEffect {
    Edit(ParamDelta),                        // routed through EditBinding preview/commit per drag lifecycle
    Picked { image_px: egui::Vec2 },         // e.g. WB eyedropper → owner (E10) resolves pixels→params
    Done, Cancelled,
}

pub trait Gizmo: Send {
    fn id(&self) -> GizmoId;
    fn hit(&self, screen: egui::Pos2, xf: &ViewXform) -> Option<HitId>;
    fn on_event(&mut self, ev: GizmoEvent<'_>, xf: &ViewXform, out: &mut Vec<GizmoEffect>);
    fn paint(&self, painter: &egui::Painter, xf: &ViewXform);
    /// Keymap sub-context pushed while active ("editor.gizmo.<id>").
    fn context(&self) -> ContextId;
}

pub struct GizmoLayer { /* active gizmo stack; input routing: gizmo hit > pan/zoom */ }
```

Interaction conventions (frozen for E11/E12): all gizmo geometry is stored in **image space** (survives zoom/pan/resize); hit-testing happens in **screen space** with a ≥ 8 pt tolerance; `Esc` = `Cancelled`, `Enter` = `Done`; a drag maps to exactly one `EditBinding` gesture. These conventions + a worked example (the eyedropper) ship as `canvas/gizmo.md` — the E11/E12 author guide.

### 6.7 Prefs store (`lightbox-core::prefs`)

```rust
pub struct PrefsStore { /* machine TOML + catalog table, watch senders */ }
impl PrefsStore {
    pub fn open_machine(dir: &Path) -> PrefsStore;                    // tolerant load
    pub fn bind_catalog(&mut self, catalog: &Arc<Catalog>);           // loads catalog_settings
    pub fn machine(&self) -> MachinePrefs;                            // current snapshot
    pub fn catalog(&self) -> CatalogPrefs;
    pub fn set_machine(&self, f: impl FnOnce(&mut MachinePrefs));     // atomic write + watch notify
    pub fn set_catalog(&self, f: impl FnOnce(&mut CatalogPrefs));     // writer txn + watch notify
    pub fn watch_machine(&self) -> tokio::sync::watch::Receiver<MachinePrefs>;
    pub fn watch_catalog(&self) -> tokio::sync::watch::Receiver<CatalogPrefs>;
}

#[derive(Clone, Serialize, Deserialize)] #[serde(default)]
pub struct MachinePrefs {
    pub gpu_mode: GpuMode,                  // Auto | Off  (Off ⇒ CPU engine; restart-required at M1 — Q6)
    pub vram_budget_override_mb: Option<u32>, // stored for E13; panel shows probe value next to it
    pub jobs: JobKnobs,                     // per-class worker overrides → CoreConfig.jobs at start
    pub drop_recursive_default: bool,       // §2.4 folder-drop default
    pub filmstrip_height_pt: f32,
    pub theme: Theme,                       // System | Dark | Light
}
#[derive(Clone, Serialize, Deserialize)] #[serde(default)]
pub struct CatalogPrefs {
    pub preview_cache_cap_bytes: u64,
    pub raw_cache_cap_bytes: u64,           // §3.3 default 5 GiB
    pub t2_retention_days: Option<u32>,     // None = never evict
    pub cache_dir_override: Option<PathBuf>,
}
```

Consumers (who reads what — E08 wires, owners act): E03 reads cache caps/retention/location via its `SetCacheLimits`/`RelocateCacheStore` commands **[consumed — E03]**; the engine reads `gpu_mode` at session construction (core passes `None` GPU for `Off`); `JobKnobs` fold into `CoreConfig.jobs` at `Core::start`; E13 will read `vram_budget_override_mb`. The prefs panel also surfaces E03's `CacheStats` usage bars and `PurgeCaches` **[consumed — E03]**.

### 6.8 Keymap (`lightbox-shell::keymap`)

Retained from the v1.x design (the differentiator survives the rescope), contexts rescoped:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)] pub struct ActionId(pub &'static str);
#[derive(Clone, Copy, PartialEq, Eq, Hash)] pub struct ContextId(pub &'static str);
// Context stack (innermost wins): "app" → "editor" → { "editor.loupe" | "editor.filmstrip"
//   | "editor.panels" } → "editor.gizmo.<id>" (while a gizmo is active)

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord { pub mods: egui::Modifiers, pub key: egui::Key }
impl Chord {
    pub fn parse(s: &str) -> Result<Chord, ChordParseError>;   // "Cmd+Shift+Z", "Space"
    pub fn display(&self, platform: Platform) -> String;       // ⌘⇧Z vs Ctrl+Shift+Z
}

pub struct ActionDef {
    pub id: ActionId, pub label: &'static str, pub category: &'static str,
    pub contexts: &'static [ContextId], pub default: Option<Chord>, pub repeatable: bool,
}

pub struct KeymapRegistry { /* defs + overrides + reverse index */ }
impl KeymapRegistry {
    pub fn register(&mut self, def: ActionDef);                              // panics on dup id (dev error)
    pub fn resolve(&self, chord: Chord, stack: &[ContextId]) -> Option<ActionId>;
    pub fn rebind(&mut self, id: ActionId, chord: Option<Chord>) -> Result<(), RebindConflict>;
    pub fn conflicts_with(&self, chord: Chord, ctx: ContextId) -> Vec<ActionId>;
    pub fn bindings_for(&self, ctx: ContextId) -> Vec<(&ActionDef, Option<Chord>)>;
    pub fn load_overrides(&mut self, path: &Path) -> Result<LoadReport, KeymapLoadError>; // tolerant
    pub fn save_overrides(&self, path: &Path) -> std::io::Result<()>;
    pub fn reset(&mut self, id: ActionId); pub fn reset_all(&mut self);
}
```

M1 default action set (registered by E08; E10/E11/E12 register theirs later): `app.open` (⌘O), `app.prefs` (⌘,), `app.cheatsheet` (⌘/), `edit.undo`/`edit.redo` (⌘Z/⇧⌘Z), `nav.next`/`nav.prev` (→/←, repeatable), `view.zoom_toggle` (Z/Space), `view.zoom_in`/`out` (+/−), `panel.toggle_rail` (Tab), `film.toggle` (⇧Tab), `gizmo.cancel` (Esc), `gizmo.commit` (Enter). Text-input focus suppresses all non-modifier single-key chords (typing in a value box never navigates) — the existing `egui_wants_keyboard_input` guard, formalized in the dispatcher.

---

## 7. Threading, budgets, and failure modes E08 owns

- **UI thread never blocks** (§5.1): intake submits commands (never walks folders); the working-set view folds events; `Queries` calls from the frame loop are single-row (`image_detail` for the info panel, on activation only, cached per active image). No SQL, no I/O, no hashing in the shell.
- **Budgets (E08's share of §7 v2.0):**
  - slider input → `Engine::submit` in the **same frame** (the <100 ms end-to-end budget is E05/E10's; E08 must add ≤ 1 frame);
  - filmstrip nav → composited texture swap **< 50 ms p95** (inherited probe from the E01 loupe, kept);
  - shell frame time **< 16 ms p95** with panels open and the filmstrip visible (F1 overlay + `--perf-strip` capture);
  - drop of a single local raw → first (embedded-tier) image composited **< 500 ms p95** on the dev baseline machine, never blocking input (the §7 develop-open <100 ms preview budget applies to *cached* reopen via E03 — measured there).
- **Failure modes** (§6 rows, shell rendering of each): decode-error entry → filmstrip badge + canvas placard, session continues; missing file (deleted mid-session) → placard + badge on next render failure; `DeviceDegraded` → non-modal notice chip, canvas keeps last frame, editing continues on the CPU path (E05.5); cache-relocation target invalid → fallback badge (§5.2); corrupt prefs/keymap file → defaults + notice, never fatal; second drop mid-load → clean epoch replace, stale events dropped.

---

## 8. Ordered task breakdown (every task ≤ 1 day; AC = acceptance criteria)

Estimates are review-effort units per the E01 calibration note. **Σ = 45 days ≈ 9 pw** (the band's ceiling). Named cut-lines: **A8** (second-instance forwarding), **D5** (rebind editor UI), **E9** (info panel) — cutting all three gives 42 d ≈ 8.4 pw; under M1 schedule pressure **B5** (strip polish) and the polish half of **H3** may also slip past the M1 exit gate without blocking it, reaching the ~35 d ≈ 7 pw floor. Nothing on the M1 exit bar (§11 item 1) is behind a cut-line.

### Phase A — chassis rescope & entry (8 d)

- **A0. Seam reconciliation (0.5 d).** Walk §6.1/§6.5/§6.7 consumed contracts with E04/E09/E03 planners; record agreed spellings in each owner's spec; file deltas in `E08-deviations.md`. *AC:* the `OpenWorkingSet`/events/snapshot shapes and `ParamId` spellings referenced by this spec compile against the owners' specs (or the recorded deltas).
- **A1. Editor chassis (1 d).** Remove `View` enum, grid view, import bar from `lib.rs`; single editor layout: top bar, central canvas area, bottom filmstrip strut, right rail placeholder, status bar. Empty-state drop-zone widget with hover affordance. *AC:* kittest snapshot of the empty state; synthetic `hovered_files` shows the highlight + count; no reference to `ImportAddInPlace`/`SetRating`/`SetFlag` remains in the shell (grep gate).
- **A2. Drop intake (1 d).** `intake.rs` per §6.1: dropped-file ordering, path-less entry handling, Alt-modifier recursive detection, one `OpenWorkingSet` per drop. *AC:* kittest-injected drop of [file, folder] emits exactly one command, paths in order, `recursive` reflecting the modifier; path-less drop increments a skipped counter surfaced in the status line.
- **A3. OS open dialog (1 d).** `rfd` behind `app.open`: multi-select files; separate "Open Folder…" affordance (native dialogs can't mix on all platforms — two buttons on the drop zone). License audit of `rfd`'s resolved graph. *AC:* dialog selection → one command; `cargo deny check licenses` green.
- **A4. Launch handler (1 d).** argv paths at startup → intake before first frame; macOS "Open With" via winit/eframe open-file events **investigated and wired if exposed by the pinned version** — otherwise the fallback is recorded (macOS entry = drop/dialog/CLI) and a follow-up filed (Q1). *AC:* `lightbox <raw> <folder>` opens the set (asserted by the reworked smoke, A7); investigation outcome written into `E08-deviations.md`.
- **A5. Working-set view model (1 d).** `working_set.rs` per §6.2 replacing `ImageListModel`; epoch guard; `Lagged` → snapshot re-pull; first-Ready auto-activation. *AC:* unit tests: out-of-order/stale-epoch events dropped; lagged refetch converges; activation rule per §2.4.
- **A6. Replace-set semantics (0.5 d).** New drop replaces the set with **no prompt** (edits auto-persist); in-flight thumb/render tickets for the old epoch cancelled. *AC:* integration test: two rapid `OpenWorkingSet` submissions → view shows only the newer epoch; thumb-cache cancel counters increased; zero dialogs.
- **A7. Smoke driver rework (1 d).** `--smoke` opens the embedded fixture via `OpenWorkingSet` → filmstrip row → canvas seam proof (engine texture composited on the shared device), same wedge/expiry semantics. *AC:* smoke exits 0 on macOS/Metal locally; CI legs stay `continue-on-error` per the E01 open item.
- **A8. Second-instance forwarding — CUT-LINE (1 d).** Single-instance socket (localhost/uds) forwarding launch paths to the running instance; without it, a second launch opens a second independent process (acceptable v1 behavior, Q2). *AC (if built):* second `lightbox <file>` invocation lands the file in the running instance's working set and exits 0.

### Phase B — session filmstrip (5 d)

- **B1. Strip geometry (1 d).** `strip_layout`/`visible_range` (horizontal adaptation of the tested `grid::layout`/`visible_indices`), `fit_rect` reuse. *AC:* unit tests mirror the grid's (1-column clamp, window arithmetic, end clamp, overscroll guard) in the horizontal axis.
- **B2. `filmstrip_ui` (1 d).** Virtualized cells over `WorkingSetView` + `ThumbCache` (unchanged `want`/`end_frame` discipline); Loading shimmer / Failed placard cells; active ring; click activates. *AC:* kittest: only visible cells materialize (visible-set assertion); scroll-out cancels (counter assertion); click activates.
- **B3. Badges & tooltips (1 d).** `RAW` tag, edited dot (`is_edited`), error/missing badges + hover reason; filename strip. *AC:* kittest snapshot with a mixed set (raw ready / JPEG ready+edited / failed) shows the §6.3 chrome; no star/flag glyph exists (grep + snapshot).
- **B4. Navigation & selection (1 d).** `nav.next/prev` actions (repeatable), scroll-to-active, wheel scrolling, multi-select (⌘/⇧ click) retained for M2 batch ops; nav-swap latency probe kept from the E01 loupe. *AC:* arrow-key nav through a 200-entry synthetic set keeps frame p95 < 16 ms and nav-swap p95 < 50 ms on the dev baseline (F1 overlay numbers asserted by the perf harness in H2).
- **B5. Strip polish (1 d).** Drag-resize (48–160 pt, persisted machine pref), `film.toggle` collapse, overflow position indicator ("34/212"). *AC:* kittest: resize persists across app restart (prefs round-trip); collapsed strip restores.

### Phase C — editor canvas (5 d)

- **C1. ViewXform (1 d).** Extract the image↔screen mapping (zoom/pan/orientation/ppp) per §6.4. *AC:* property tests: `screen_to_image ∘ image_to_screen ≈ id` within 0.5 px across zoom/pan/orientation samples; outside-image → `None`.
- **C2. Zoom ladder & pan (1 d).** Fit/100% toggle (Z/Space, double-click), wheel-stepped ladder with zoom-to-cursor, drag pan with clamping (existing logic generalized). *AC:* unit tests on ladder math + clamp; zoom-to-cursor keeps the cursor's image point fixed within 1 px.
- **C3. Recipe-driven submit (1 d).** Canvas submits with the active entry's recipe from `EditBinding` (`recipe_rev` in the submit key) instead of `Recipe::identity`; latest-wins per-viewport coalescing unchanged. *AC:* integration (headless-core + test EditBinding): `preview(delta)` → new submit issued same frame; releasing the gesture issues no extra submit (rev unchanged).
- **C4. Progressive display (1 d).** On activation: best available preview tier composited immediately with tier badge (E03), engine result swaps in on `Ready`; stale-frame-on-same-image-failure rule per §6.4. *AC:* integration: activating an entry with a T0 preview shows pixels on the next frame (no blank), then one texture swap when the engine render lands; render failure of the *same* image keeps the last frame + error chip.
- **C5. Canvas states & degraded notice (1 d).** Failed/missing placards; `DeviceDegraded` non-modal chip; loading shimmer for never-rendered entries. *AC:* kittest snapshots for each state; injected `DeviceDegraded` event shows the chip without stealing focus or blocking input.

### Phase D — keymap (5 d)

- **D1. Chord + registry + resolution (1 d).** §6.8 types; innermost-context-wins resolution; duplicate-id panic; conflict query. *AC:* unit tests: parse/display round-trip (both platforms' display strings), context shadowing, conflict detection.
- **D2. Dispatcher integration (1 d).** Per-frame dispatch before widget input; text-input suppression; repeatable actions on key-repeat; M1 action set registered and wired (nav/zoom/undo/open/prefs/cheatsheet/rail toggles). *AC:* kittest: typing "z" in the prefs search box does not toggle zoom; holding → repeats `nav.next`.
- **D3. Overrides persistence (1 d).** `keymap.toml` delta-only save/load, tolerant parse (bad rows skipped + reported), unknown ids preserved; atomic write. *AC:* round-trip property test; corrupt file → defaults + `LoadReport` surfaced in status; unknown-id row survives save.
- **D4. Cheat-sheet overlay (1 d).** ⌘/ overlay grouped by category, live bindings (reflects rebinds), current-context highlighting. *AC:* kittest snapshot; a rebind (D5 API) is reflected without restart.
- **D5. Rebind editor — CUT-LINE (1 d).** Prefs-panel keymap tab: capture-next-chord, conflict warning with steal/cancel, per-row and global reset. *AC:* kittest: rebinding `nav.next` to a conflicting chord surfaces the conflict; steal rebinds and persists.

### Phase E — develop panels & edit chrome (9 d)

- **E1. Panel host (1 d).** `PanelDef` registry + right rail: collapsible sections, per-panel open state persisted (machine scope), panel-solo mode, rail scroll. *AC:* kittest: registration order + `order` field control layout; open/solo state survives restart.
- **E2. SourceKind adaptivity (1 d).** §4 filtering: `RawOnly` panels absent (not disabled) for `Rendered`; per-control raw/rendered label variants (WB "relative" relabel); open-state preserved across active-file switches. *AC:* kittest snapshots raw-vs-JPEG differ exactly per the §4 table; switching active file between them re-filters in one frame and preserves open panels.
- **E3. EditBinding adapter (1 d).** Adapter over E09's `EditSession` implementing §6.5: begin/preview/end lifecycle, `recipe_rev`, reset, undo/redo. *AC:* integration against real E09: a 60-event synthetic drag = exactly **one** `history_step`; `undo` restores the pre-gesture value; `kill -9` after `end_gesture` → reopen restores the committed value (rides E09's fault harness).
- **E4. `param_slider` (1 d).** The house control per §6.5 (scrub, Shift-fine, double-click reset, value text entry, ↑/↓ nudge) + AccessKit value semantics. *AC:* kittest interaction tests for each affordance; reset targets `EditBinding::default`; text entry clamps to spec range.
- **E5. Basic panel — tone (1 d).** `develop.basic`: exposure/contrast/highlights/shadows/whites/blacks bound to E09 param ids (doctest-pinned mapping per §6.5). *AC:* the epic seed test `drop_to_loupe_and_edit` passes end-to-end (with E05+E10.1 in the tree: pixels change; without: recipe commit + resubmit asserted); slider input→submit ≤ 1 frame (probe).
- **E6. Basic panel — WB (1 d).** Temp/tint sliders (Kelvin scale for raw, relative for rendered per §4), as-shot/custom mode combo (raw only), eyedropper button mounting the F2 gizmo. *AC:* kittest: raw shows Kelvin + As Shot; JPEG shows relative sliders, no As Shot; eyedropper button activates the gizmo context.
- **E7. Point tone-curve widget (1 d).** Curve editor: linear default, click-add / drag / double-click-delete points, monotone-x clamping, keyboard nudge on the selected point; **widget math only** (rendering the curve's effect is the engine's job). Parametric region sliders are E10.3/M2. *AC:* unit tests on point editing invariants (sorted x, clamp, min-gap); kittest drag emits preview deltas + one commit per gesture.
- **E8. History & snapshots panel (1 d).** Compact history list (E09 query): step labels, click-restore, clear-with-confirm; snapshots: create (named), restore, rename. Undo/redo keymap wired app-wide. *AC:* integration: restore-to-step then new edit truncates forward history (asserting against E09's semantics); snapshot create/restore round-trips; ⌘Z outside text fields always reaches `edit.undo`.
- **E9. Info panel — CUT-LINE (1 d).** `editor.info`: camera/lens/exposure/ISO/focal/date/dims/file size + path, from `ImageDetail` + `metadata_cache` (read-only, fetched on activation, cached). *AC:* kittest snapshot for a fixture raw; activation of a JPEG updates within one frame of the query completing; no per-frame queries (probe counter).

### Phase F — gizmo framework (3 d)

- **F1. GizmoLayer plumbing (1 d).** Input routing on the canvas (gizmo hit > pan/zoom), drag capture, paint pass above the image, active-gizmo keymap context push/pop. *AC:* interaction test with a probe gizmo: drag inside its hit region never pans; Esc pops the context and emits `Cancelled`.
- **F2. Reference gizmo — WB eyedropper (1 d).** Crosshair cursor + magnified-region loupe chip (drawn from the composited texture — no readback), click emits `Picked { image_px }` routed to the panel's callback (E10 wires pixel sampling; until then the M1 wiring resolves through E09's WB params via a temporary linear estimate **behind a `debug_assertions`-gated TODO** — the honest seam note, Q5). *AC:* kittest: click at a known screen point yields the expected image-px within 0.5 px across zoom/pan/orientation.
- **F3. Conventions & author guide (1 d).** Freeze §6.6 conventions; write `canvas/gizmo.md` (worked eyedropper example, hit-tolerance, gesture mapping, context rules) for E11/E12; interaction-test templates. *AC:* doc reviewed by E11/E12 planners (or their specs' authors-of-record); templates compile as doctests.

### Phase G — prefs & performance panel (6 d)

- **G1. Prefs store — machine scope (1 d).** §6.7 `PrefsStore`: tolerant TOML load, atomic save, watch channels, defaults. *AC:* round-trip + corrupt-file property tests; watch fires exactly once per `set_machine`.
- **G2. Catalog scope + migration (1 d).** `catalog_settings` migration (number reserved via registry PR, §5.1) + DAO + `bind_catalog` load/write-through. *AC:* migration lint green; round-trip via writer txn; settings survive backup/restore (rides the E01 backup test harness).
- **G3. Panel scaffolding + GPU section (1 d).** Prefs window (⌘,): sectioned layout; GPU mode toggle (restart-required badge at M1 — Q6), adapter/backend readout (`GpuContext::adapter_report`), VRAM probe display + `vram_budget_override_mb` knob (stored-for-E13 labeling). *AC:* toggling GPU Off + restart → session opens with `gpu: None` and the canvas renders via the CPU path (asserted headless); knob persists.
- **G4. Cache section (1 d).** Preview/raw cache caps, T2 retention, usage bars from `CacheStats`, purge + relocate flows (confirm dialog, progress from E03 events, invalid-target fallback badge per §5.2). *AC:* integration against E03: lowering the preview cap triggers observable eviction (stats drop below cap); relocate to a temp dir moves the store and survives restart.
- **G5. Jobs section (1 d).** Per-class worker overrides (`JobKnobs` → `CoreConfig.jobs` at start; restart-required badge; live application is E06's later seam), advanced-collapsed event-capacity knob. *AC:* override persists and is reflected in `JobSystem` sizing on next start (headless assert).
- **G6. Prefs consumers wiring (1 d).** `drop_recursive_default` → intake; filmstrip height; theme (System/Dark/Light via egui visuals); keymap-file location readout; document live-vs-restart per knob in the panel UI itself. *AC:* kittest: theme switch applies without restart; recursive default flips the folder-drop behavior (intake unit test reads the watch).

### Phase H — hardening & exit (4 d)

- **H1. Accessibility pass (1 d).** AccessKit roles/labels for every action-bearing widget (drop zone, filmstrip cells with filename+state, sliders with value semantics, panel headers, prefs controls); focus order sane. *AC:* kittest AccessKit-tree assertions for the main surfaces; every `ActionDef.label` reachable as an accessible name.
- **H2. Perf probes & harness (1 d).** Extend the F1 overlay (slider→submit frames, filmstrip nav p95, frame p95 with rail open); replace `--perf-scroll` with `--perf-strip` (sawtooth horizontal scroll over a 300-entry synthetic set) + `lbx-perf` scenario; nightly wiring + baselines. *AC:* one JSON summary line per run; budgets of §7 asserted budget-first (E01 semantics: regression files a tracked issue, non-blocking).
- **H3. Status bar & polish (1 d).** Status bar final: entry counts, epoch load progress, activity indicator (spinner + label + cancel for the newest Foreground/Background job from core events — the minimal E06/E15 seam), notice chips (prefs/keymap load reports, degraded GPU); DPI audit. *AC:* kittest snapshot; cancel actually cancels (integration with a slow synthetic job).
- **H4. DoD walk & handoff (1 d).** Extend `cargo xtask exit-drill` with the editor leg (open fixture set → edit → `kill -9` → reopen → recipe restored → filmstrip/canvas smoke); write `E08-deviations.md` + handoff notes (panel/gizmo/keymap registration how-to for E10/E11/E12); update `docs/plan/migrations.md`. *AC:* §12 checklist walked with evidence links; drill passes on macOS/Metal (Win/Linux legs recorded pending real hardware, per E01 precedent).

---

## 9. Test plan

| Layer | What | Gate |
|---|---|---|
| Unit | strip geometry, ViewXform round-trip (proptest), chord parse/display/resolve/conflicts, curve-point invariants, prefs (round-trip, corrupt-file, watch semantics), intake ordering/modifiers, working-set event folding (epochs, lag) | PR-blocking |
| Widget/interaction (`egui_kittest`) | empty-state + hover affordance; synthetic drop → single command; filmstrip virtualization/cancel/badges; panel adaptivity snapshots (raw vs JPEG vs mixed-set switching); `param_slider` affordances; curve drag; cheat-sheet; rebind flow; canvas state placards; theme switch; AccessKit tree | PR-blocking |
| Integration (headless core, real deps as they land) | **seed test `drop_to_loupe_and_edit`** (§1); replace-set epoch race; gesture = one history step; undo/redo/history-truncate; snapshot round-trip; progressive display (tier → engine swap, never blank); GPU-off restart path; cache cap/purge/relocate effects; `kill -9` after commit → reopen restores recipe (rides E09 fault harness); smoke (`--smoke`) over `OpenWorkingSet` | PR-blocking (fast subset), full nightly |
| Perf (nightly, budget-first per E01 semantics) | `--perf-strip` frame p95 < 16 ms; nav-swap p95 < 50 ms; slider input→submit ≤ 1 frame; drop→first-pixels < 500 ms (dev baseline) | Nightly; regression → tracked issue |
| Cross-platform | 3-OS CI matrix builds + unit + kittest (kittest runs headless); shell smoke stays `continue-on-error` until fleet-green (E01 open item), then promoted | PR-blocking (build/test), smoke observational |
| License | `cargo deny` green with `rfd`/`directories`/`toml`/`egui_kittest`; no native/data surface changes | PR-blocking |

Pixel goldens are **not** owned here: the canvas composites engine textures, and pixel correctness is E05's per-PV golden gate. E08's visual tests are structural snapshots (widget trees), immune to render-backend drift.

---

## 10. Risks & open questions

| # | Risk / question | Impact | Mitigation / default |
|---|---|---|---|
| R1 | **egui widget ceiling** (curve editor, rebind capture, panel rail ergonomics) | epic-level | The §1.2 reversal trigger stands (swap shell crate against the same headless core). Tactically: every widget here is custom-paint over primitives egui demonstrably supports (E01 shipped a virtualized grid + zero-copy canvas); the curve widget is the riskiest and is scoped to point-edit-only at M1 |
| R2 | **Dependency landing order** — E08 integrates E04/E05/E09 in one milestone | schedule | Phase gating in §2.4: A–D run on E01+E04 alone; E's persistence ACs run on E09 alone; only E5's end-to-end AC needs E05+E10.1. A0 reconciles seams before code |
| R3 | **macOS "Open With" / Apple Events** exposure in the pinned eframe/winit (Q1) | entry-model completeness | A4 investigates; fallback recorded (drop/dialog/CLI cover the mandate's two required entry points; file-association ships when the toolkit exposes it or via a small objc shim under E16 packaging) |
| R4 | **Wayland/portal drag-drop** — path-less or absent `hovered_files`/`dropped_files` on some Linux compositors | Linux entry UX | Hover affordance degrades to a generic highlight (`count: None`); drop still carries paths via winit on X11 and current Wayland; if a target platform drops paths entirely, the open dialog is the guaranteed path (test on the Linux CI leg + real-hardware drill) |
| R5 | **Prefs live-apply deadlocks/races** (watch consumers acting on the writer thread) | subtle bugs | Watches deliver snapshots only; consumers act on their own executors; catalog writes go through the existing single-writer discipline; restart-required knobs (GPU, jobs) deliberately avoid live re-plumbing at M1 (Q6) |
| Q1 | Does pinned winit surface macOS open-file events? | A4 | Investigate in A4; record outcome in deviations; follow-up owner E16 if negative |
| Q2 | Is second-instance forwarding v1-required? | A8 | §2.4 permits "running-instance IPC"; default per this spec: **cut-line** (A8) — second launch opens independently if cut. Confirm with product before M1 exit |
| Q3 | Migration number for `catalog_settings` | G2 | Reserved via `docs/plan/migrations.md` registry PR at merge (E03/E09 claim first) |
| Q4 | `ParamId` spellings for the basic panel | E5/E6/E7 | E09 owns; pinned by doctest against E09's registry (build fails on drift) |
| Q5 | WB eyedropper sampling (picked-point → temp/tint) | F2 | E10 owns the math; M1 ships the gizmo + a gated placeholder resolve; the seam is `Picked{image_px}` |
| Q6 | GPU toggle: restart-required vs live | G3 | M1: restart-required (engine built at session open). Live toggle becomes possible with E05.5 device-recovery plumbing — E05's call |

---

## 11. Definition of done

1. **M1 exit bar (E08's share, §9):** drop a raw **and** a JPEG → each appears in the filmstrip and loupe within the preview budget; the JPEG's develop rail hides every raw-only panel per the §4 table; basic-panel edits render through the engine and **auto-persist** — `kill -9` + reopen restores the recipe (witnessed by the extended exit drill, H4).
2. **Entry model complete:** all §2.4 entry rows work except any recorded A4/A8 cut-line deltas (macOS open-with, second-instance forwarding), each with a deviations entry and follow-up owner.
3. **Budgets green** on the dev baseline + nightly harness: frame p95 < 16 ms with chrome open; nav-swap p95 < 50 ms; slider input→submit ≤ 1 frame; drop→first-pixels < 500 ms.
4. **Retirement verified:** no grid view, culling keystroke, rating/flag/label UI, compare/survey, or library panel is reachable; the shell has no `lightbox-ingest` dependency; grep gates in A1 hold.
5. **Frameworks consumable:** E10/E11/E12 can register a panel, a gizmo, and keymap actions using only `canvas/gizmo.md` + the panel-host docs (reviewed by those planners, F3/H4).
6. **Keymap:** every M1 action rebindable; overrides round-trip `keymap.toml`; cheat-sheet reflects live bindings; text-input suppression proven.
7. **Prefs effective:** cache caps/purge/relocate observably act (G4 integration); GPU-off path renders via CPU; all knobs labeled live-vs-restart.
8. **Test/CI:** §9 suites green on the 3-OS matrix; smoke reworked; `--perf-strip` nightly wired with baselines; deny gate green with the new deps.
9. **Docs:** `E08-deviations.md` written; migration registry updated; handoff notes for E10/E11/E12/E13/E15 committed.

---

## 12. Seams to neighbors (one-line each, for their planners)

- **E04:** owns `OpenWorkingSet`, working-set snapshot/events, ordering rules, `SourceKind` on `lightbox-types`; E08 is its only UI consumer (§6.1, A0).
- **E05:** owns `RenderRequest`/progressive ladder; E08's canvas is the interactive consumer (submit key incl. `recipe_rev`, per-viewport coalescing).
- **E09:** owns params/gesture-commit/history/snapshot semantics behind `EditBinding`; E08 owns the widgets (history panel here; preset browser deferred to E10's execution on this framework).
- **E03:** owns preview tiers + cache policy APIs; E08 binds them in the prefs panel and consumes `best_available` + the unchanged thumb path.
- **E06:** will replace the status-bar activity indicator's data source and make job knobs live; nothing here blocks on it.
- **E10/E11/E12:** register panels (`PanelDef`), controls (`param_slider`), and gizmos (`Gizmo`) — the M1 basic panel + eyedropper are their worked examples.
- **E13:** reads `vram_budget_override_mb`; the prefs panel already displays the probe value beside it.
- **E15/E16:** export progress rides the activity indicator; file-association registration at packaging time points at the A4 launch handler.
