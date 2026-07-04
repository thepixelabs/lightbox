# E08 — Library UI & Culling

_Implementation spec. Milestone **M1**. Effort **L–XL ~7–11 pw** (re-baselined per §10 of the architecture: a from-scratch immediate-mode UI surface against a thin widget ecosystem). Depends on: **E01** (workspace, headless core, shell skeleton, command/query bus, shared wgpu device + display-transform node), **E03** (preview pyramid, raw cache, thumb cache), **E06** (job system, activity model, cancellation), **E07** (catalog DAM queries/commands, smart-collection AST→SQL, FTS, metadata commands)._

_Authority: `docs/plan/01-architecture.md` (decision-complete; not re-litigated here) — §1.2 (egui shell, reversal trigger), §2.1–§2.3 (crate map, seams), §3.3 (cache layout + retention knobs), §5.3 (job classes), §6 (failure modes incl. VRAM gating), §7 (performance budgets), §8 (testing strategy), §10/§10.1 (epic table + sizing note). Research: `docs/research/01-…` (Library/DAM), `docs/research/09-…` (performance & UX conventions), `docs/research/00-feature-catalog.md` §2.1/§2.9._

---

## 1. Summary

E08 builds the entire user-facing Library experience on top of the headless core: the virtualized grid, loupe, filmstrip, compare and survey views; the one-key culling grammar with auto-advance; the declarative, **user-remappable** keymap registry with cheat-sheet overlay (a named differentiator — Lightroom has never shipped remapping); the reusable on-canvas gizmo framework that E11 (crop/geometry) and E12 (mask gizmos) will consume; the Library panels (folders, collections, keywords, metadata, filter bar) as thin data-bound UI over E07; the activity-center UI over E06's model; and the **performance preferences panel** backed by a typed prefs store that the owning subsystems (E03 caches, E05 GPU mode, E06 job concurrency, later E13 VRAM gating) read via watch channels.

Everything in this epic lives **above the headless boundary** (§2.3 seam 1) except the prefs store and one catalog migration. No UI type crosses down; no SQL crosses up. The epic's product-level acceptance is the M1 exit bar: *import 10 k raws non-blocking (<60 s browsable); cull at keyboard speed; catalog interactive at 100 k* — with the §7 budgets as the measurable form.

**First failing test (the epic's seed):** `cull_keystroke_p16ms` — an `egui_kittest` harness over a fake 10 k-image catalog: pressing `P` on the active grid image must (a) render the pick badge **in the same frame** (optimistic apply), (b) enqueue exactly one single-row `SetFlag` command, and (c) complete input-to-paint in **<16 ms**. It fails until the culling controller, grid badges, and keymap dispatch exist — and it exercises the three riskiest pieces (virtualized grid, keymap, command-bus round-trip) together.

---

## 2. Scope

In scope (all shipping surfaces in `lightbox-shell` unless noted):

1. **Workspace chassis** — two-module (Library/Develop) mode bar per §9 M1; collapsible left/right panel rails with Solo mode, Tab/Shift-Tab chrome toggles, hover reveal. (Develop module *content* is E10; E08 ships the chassis + an empty Develop placeholder that E10 fills.)
2. **Virtualized grid** — 100 k-asset scrolling with demand-driven, visible-first thumbnail loading, upgrade-in-place tiers, badges, sorts (incl. custom drag order for collections), adjustable cell size, selection model, context menu.
3. **Loupe** — fit/fill/100%/custom zoom, scrubby pan, click-to-zoom, T2 tile fetch at 1:1, progressive tier upgrade, cycling info overlay, ±N neighbor prefetch, <50 ms next/prev swap.
4. **Filmstrip** — persistent cross-module strip sharing the grid's model/selection; source breadcrumb + recent sources; quick filters.
5. **Compare & Survey** — select-vs-candidate with linked zoom/pan and swap/promote; N-up survey with knock-out flow. (Sequenced last; see §2.1.)
6. **Culling grammar** — P/X/U, 1–5 stars, 0 clear, 6–9 labels, auto-advance (explicit toggle + best-effort Caps Lock), quick-collection `B`, delete-rejected flow, remove-vs-delete dialog. Writes are tiny single-row txns via the command bus.
7. **Keymap** — declarative action registry, context-scoped resolution, user remapping persisted to `keymap.toml`, conflict detection, keymap editor UI, per-module cheat-sheet overlay (`Cmd+/`).
8. **On-canvas gizmo framework** — image↔screen viewport transform, hit-testing in screen space, drag capture, overlay painting; shipped with a reference gizmo + interaction tests. E11/E12 build their gizmos on this API (seam, not content).
9. **Library panels UI** — folders tree, collections/sets, keywords, metadata viewer/editor, filter bar (text + attribute + faceted metadata columns + saved filter presets). All logic (rename/move/rules/metadata writes) is E07 commands; E08 is the widget layer.
10. **Activity-center UI** — stacked progress over the identity-plate area, per-task cancel/pause, rendering E06's activity model. (Confirm E06's spec doesn't also claim this surface — open question Q1.)
11. **Performance preferences panel + prefs store** — `lightbox-core::prefs` typed store (machine-scope TOML + catalog-scope `catalog_settings` table) with watch channels; panel binds: preview/raw-cache caps, cache relocation + purge, T2 retention (E03/§3.3), GPU enable + VRAM thresholds (§6), job-concurrency knobs (E06).
12. **Accessibility & polish** — AccessKit labeling for all actions/views, DPI/theming pass.

### 2.1 Milestone sequencing note (compare/survey)

The epic table (§10) places compare/survey inside E08/M1; the milestone narrative (§9) lists them under M4's "core Should tier." Resolution adopted here: **grid/loupe/filmstrip/culling/keymap/panels/prefs are the M1-exit-critical path and are ordered first (phases A–E, G)**; compare/survey (phase F) are the epic's tail and may land after the M1 exit gate without blocking it, consistent with §9. They remain in this epic's scope and task list.

---

## 3. Explicit non-goals

- **Develop module content** — panels, sliders, histogram, render-driven canvas beyond the E01 display-transform path (E10). E08 ships the module chassis only.
- **Crop tool and geometry gizmos** (E11.4), **mask gizmos, overlays, pins** (E12.3) — they *consume* the E08 gizmo framework; their gizmo content is theirs.
- **Render engine features** — no dependency on E05 beyond the E01-established one-node `Engine::submit/poll` display path. Library loupe displays **preview tiers**, never live raw renders.
- **Smart-collection AST, FTS indexing, facet SQL, relink logic, folder sync, keyword model, metadata write-out** — E07 owns all query/command semantics; E08 renders and invokes.
- **Preview generation, tier policy internals, thumbcache.sqlite, cache eviction** — E03. E08 sets policy via prefs and requests previews via the provider trait.
- **Import dialog/pipeline UI** (E04), **export dialog** (E15), **presets/history/snapshot UI** (E09/E10).
- **Secondary display window** — M4 (§9). E08 keeps the door open (egui multi-viewport is noted in §1.2) but builds nothing.
- **Screen modes / Lights Out, identity-plate branding, painter (spray) tool, Quick Develop, stacking UI, duplicates view, People view, video playback UI** — Could/Should tier not named in this epic's scope (§10 table); deferred.
- **Catalog-level features** — multiple catalogs, catalog merge, optimize command UI (E07/E16 territory).
- **New RenderNodes, shaders, or color-management code** — the loupe consumes the display transform as configured by E01 (later E02.3 per-monitor ICC arrives through that same seam; assumption A3 below).

---

## 4. Crates & modules touched

Per the §2.1 decomposition. E08 creates no new crate.

| Crate | Modules added/changed | Nature |
|---|---|---|
| `lightbox-shell` | `workspace/` (module bar, panel rails, chrome), `keymap/` (registry, chords, contexts, editor, cheatsheet), `views/grid.rs`, `views/loupe.rs`, `views/filmstrip.rs`, `views/compare.rs`, `views/survey.rs`, `canvas/` (`ViewXform`, `TiledImageView`, `GizmoLayer`), `thumbs/` (thumb service, texture atlas), `culling.rs`, `selection.rs`, `panels/` (folders, collections, keywords, metadata, filter_bar), `activity_ui.rs`, `prefs_ui/` | **Owned — the bulk of the epic** |
| `lightbox-core` | `prefs/` (typed store, scopes, watch channels, TOML persistence); session wiring that binds pref watches to E03/E06 setter APIs | **Owned** |
| `lightbox-catalog` | one migration: `catalog_settings` table | **Owned (schema addition only)** |
| `lightbox-preview` (E03) | consumed: `PreviewProvider`, cache stats/policy/purge/relocate APIs | Seam (contract listed §6.7) |
| `lightbox-jobs` (E06) | consumed: activity model, `set_limits`, cancel/pause handles | Seam |
| `lightbox-catalog`/core query façade (E07) | consumed: id-list query, windowed hydrate, facets, FTS, DAM commands, `CatalogEvent` stream | Seam |
| `lightbox-render` (E01 seed / E05) | consumed: display-transform texture path on the shared `Arc<wgpu::Device>`; `gpu.mode` pref is *read by* the engine (E05's wiring) | Seam |

New third-party deps (all MIT/Apache, cargo-deny-clean, license surface 1 only): `egui_kittest` (dev-dependency, UI harness/snapshots), `lru`, `indexmap`. No native/FFI or data-manifest surface changes.

---

## 5. Data model & persistence additions

### 5.1 Catalog migration — `catalog_settings`

Per-catalog knobs (cache caps, retention, relocation, per-catalog UI state) live with the catalog so they travel with it. Single key-value table, JSON values, versioned like every other setting blob:

```sql
-- migrations/NNNN_catalog_settings.sql  (NNNN = next number after E07's latest; coordinate at merge)
CREATE TABLE catalog_settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,                    -- JSON-encoded typed value
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
```

Writes go through the single catalog writer (§5.1 of the architecture) as one-row transactions. No other schema change. `edit_index`, `preview`, `collection_item.order` etc. are consumed read-only via E07's façade.

### 5.2 Machine-scope files (outside the catalog)

| File | Location | Content | Write discipline |
|---|---|---|---|
| `prefs.toml` | platform config dir (`directories`-resolved) `lightbox/prefs.toml` | machine-scope prefs (§6.6 table), `prefs_version = 1` header | temp-then-rename atomic write; corrupt/missing file ⇒ defaults + non-fatal notice |
| `keymap.toml` | same dir | **user overrides only** (delta from registry defaults), `keymap_version = 1` | same atomic discipline; unknown action ids preserved verbatim (forward-compat) |

Rationale for the split (decision, recorded): GPU mode, job concurrency, and AI-VRAM overrides describe the **machine**; cache caps/retention/relocation describe the **catalog** (its `.lbdata` is a catalog sibling, §3.3). A catalog-scope relocation path that doesn't resolve on the current machine falls back to the default `.lbdata` location with a badge — never an error (mirrors §6 missing-volume handling).

---

## 6. Interface definitions

Contracts, not implementations. Everything here compiles against E01's existing `CommandBus`/`QueryFacade` types; where E08 *needs* a seam from a dependency epic, it is marked **[consumed contract]** and listed as a coordination item in task T00 territory (first task of the relevant phase).

### 6.1 Keymap & actions (`lightbox-shell::keymap`)

```rust
/// Stable, namespaced action identity: "library.flag.pick", "app.module.develop".
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(pub &'static str);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextId(pub &'static str);        // "app", "library", "library.grid", "library.loupe", ...

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord { pub mods: egui::Modifiers, pub key: egui::Key }
impl Chord {
    pub fn parse(s: &str) -> Result<Chord, ChordParseError>;   // "Cmd+Shift+E", "P", "6"
    pub fn display(&self, platform: Platform) -> String;       // ⌘⇧E vs Ctrl+Shift+E
}

pub struct ActionDef {
    pub id: ActionId,
    pub label: &'static str,           // human name, cheat-sheet + editor
    pub category: &'static str,        // cheat-sheet grouping ("Culling", "View", ...)
    pub contexts: &'static [ContextId],// innermost context in the stack wins on conflict
    pub default: Option<Chord>,
    pub repeatable: bool,              // key-repeat fires repeatedly (e.g. next/prev)
}

pub struct KeymapRegistry { /* defs + user overrides + reverse index */ }
impl KeymapRegistry {
    pub fn register(&mut self, def: ActionDef);                          // panics on duplicate id (dev error)
    /// Resolution: walk the live context stack innermost→outermost; first binding wins.
    pub fn resolve(&self, chord: Chord, stack: &[ContextId]) -> Option<ActionId>;
    pub fn rebind(&mut self, id: ActionId, chord: Option<Chord>) -> Result<(), RebindConflict>;
    pub fn conflicts_with(&self, chord: Chord, ctx: ContextId) -> Vec<ActionId>;
    pub fn bindings_for(&self, ctx: ContextId) -> Vec<(&ActionDef, Option<Chord>)>; // cheat-sheet & editor
    pub fn load_overrides(&mut self, path: &Path) -> Result<(), KeymapLoadError>;   // tolerant: bad rows skipped+reported
    pub fn save_overrides(&self, path: &Path) -> Result<()>;
    pub fn reset(&mut self, id: ActionId);
    pub fn reset_all(&mut self);
}

/// Dispatch (once per frame, before widget input): text-input focus suppresses
/// all non-modifier single-key chords (typing "p" in a search box never flags).
pub struct ActionDispatcher { /* handlers: HashMap<ActionId, Box<dyn FnMut(&mut ShellCtx)>> */ }
impl ActionDispatcher {
    pub fn handle(&mut self, id: ActionId, f: impl FnMut(&mut ShellCtx) + 'static);
    pub fn dispatch_frame(&mut self, input: &egui::InputState, stack: &[ContextId], ctx: &mut ShellCtx);
}
```

### 6.2 Grid model, selection, culling (`lightbox-shell::{views::grid, selection, culling}`)

```rust
/// The grid's data spine: a resolved id vector (8 B/row → ~800 KB at 100k, held in RAM)
/// plus a windowed, LRU-hydrated metadata cache. NEVER queries on the UI thread.
pub struct GridModel {
    source: SourceRef,                 // folder / collection / smart collection / all (E07 type)
    filter: FilterSpec, sort: SortSpec,// E07 types
    ids: Vec<ImageId>,
    meta: lru::LruCache<ImageId, GridItemMeta>,
    epoch: u64,                        // bumped on refresh; stale async hydrates are dropped
}
impl GridModel {
    pub fn refresh(&mut self, q: &QueryHandle);                       // async id re-query; latest-wins
    pub fn len(&self) -> usize;
    pub fn id_at(&self, idx: usize) -> Option<ImageId>;
    pub fn index_of(&self, id: ImageId) -> Option<usize>;             // O(1) via side index
    /// Returns what's cached now; enqueues an async hydrate for misses (overscan included).
    pub fn window(&mut self, range: Range<usize>, q: &QueryHandle) -> WindowView<'_>;
    pub fn on_event(&mut self, ev: &CatalogEvent, q: &QueryHandle);   // patch-in-place or refresh
}

/// Hydrated per-cell metadata — exactly the columns the cell chrome needs (E07 hydrate contract).
pub struct GridItemMeta {
    pub id: ImageId, pub asset_id: AssetId,
    pub flag: Flag, pub rating: Option<u8>, pub label: Option<LabelId>,
    pub badges: BadgeBits,             // EDITED|CROPPED|HAS_KEYWORDS|IN_COLLECTION|HAS_GPS|VIRTUAL_COPY|MISSING|EMBEDDED_PREVIEW_ONLY
    pub orientation: Orientation, pub aspect: f32,
    pub filename: SmolStr, pub capture_time: Option<i64>,
}

/// Shared by grid, loupe, filmstrip, compare, survey — one selection everywhere (LR convention).
pub struct SelectionModel {
    active: Option<ImageId>,           // the "most selected" image (loupe target, culling anchor)
    selected: indexmap::IndexSet<ImageId>,
    anchor: Option<usize>,             // shift-range anchor (index into current GridModel order)
}
impl SelectionModel {
    pub fn click(&mut self, idx: usize, model: &GridModel, mods: SelMods); // plain/shift-range/ctrl-toggle
    pub fn nav(&mut self, delta: NavDelta, model: &GridModel, extend: bool); // arrows/home/end/page
    pub fn select_all(&mut self, model: &GridModel); pub fn none(&mut self); pub fn invert(&mut self, model: &GridModel);
    pub fn active(&self) -> Option<ImageId>;
    pub fn ids(&self) -> impl Iterator<Item = ImageId> + '_;
}

/// Culling: optimistic single-frame badge update + coalesced command emission + auto-advance.
pub struct CullingController { pub auto_advance: bool /* pref-backed */ }
impl CullingController {
    pub fn set_flag(&mut self, f: Flag, sel: &mut SelectionModel, m: &mut GridModel, bus: &CommandBus);
    pub fn set_rating(&mut self, r: Option<u8>, ...);
    pub fn set_label(&mut self, l: Option<LabelId>, ...);
    // Semantics: applies to the whole selection; GridItemMeta patched immediately (optimistic);
    // reconciled on the CatalogEvent echo; on command failure the patch reverts + toast.
    // Auto-advance moves `active` to the next image after the *last* selected, single-selection only.
}
```

**[consumed contract — E07 query façade]** (exact signatures E08 codes against; reconcile in T09):

```rust
pub trait LibraryQuery: Send + Sync {
    fn image_ids(&self, src: &SourceRef, f: &FilterSpec, s: &SortSpec) -> QueryFuture<Vec<ImageId>>;
    fn hydrate(&self, ids: &[ImageId]) -> QueryFuture<Vec<GridItemMeta>>;      // batched, indexed
    fn facet_counts(&self, src: &SourceRef, f: &FilterSpec, facet: FacetKind) -> QueryFuture<Vec<(FacetValue, u32)>>;
    fn fts(&self, text: &str, scope: FtsScope) -> QueryFuture<FilterSpec>;     // text row of the filter bar
    fn subscribe(&self) -> broadcast::Receiver<CatalogEvent>;                  // ImagesChanged(SmallVec<ImageId>) | SourceInvalidated | ...
}
```

**[consumed contract — E07 commands]**: `SetFlag/SetRating/SetLabel { images, value }` (single-row txns), `RotateImages`, `RemoveImages { mode: CatalogOnly | ToTrash }`, `DeleteRejected { source }`, `ToggleQuickCollection { images }`, `SetTargetCollection`, `ReorderCollectionItems { collection, moves }`, folder/collection/keyword/metadata commands for the panels.

### 6.3 Thumbnail pipeline (`lightbox-shell::thumbs`)

```rust
/// Shell-side service: binds PreviewProvider (E03) to wgpu textures with visible-first priority.
pub struct ThumbService { /* provider handle, decode pool handle (E06 Background jobs), atlas */ }
impl ThumbService {
    /// Called once per frame with the visible+overscan index window; diffs against in-flight
    /// set: cancels requests that scrolled away, (re)prioritizes by distance-to-viewport-center.
    pub fn want(&mut self, wants: &[ThumbWant]);      // ThumbWant { key: PreviewKey, cell_px: u32, dist: u32 }
    pub fn get(&self, key: PreviewKey) -> ThumbState; // Placeholder | Stale(AtlasSlot) | Ready(AtlasSlot, Tier)
    pub fn frame_end(&mut self);                      // upload budget flush (≤ N ms of queue writes/frame)
}

/// Texture atlas: fixed-size RGBA8 pages (2048²) on the shared device, shelf-packed per
/// cell-size class, LRU-evicted by last-visible frame. Hard VRAM cap (default 256 MB, pref-able).
pub struct ThumbAtlas { /* pages: Vec<AtlasPage>, free lists, lru */ }
pub struct AtlasSlot { pub tex: egui::TextureId, pub uv: egui::Rect }
```

**[consumed contract — E03 `PreviewProvider`]**:

```rust
pub trait PreviewProvider: Send + Sync {
    fn request(&self, key: PreviewKey, goal: TierGoal, prio: PreviewPriority) -> PreviewTicket;
    fn poll(&self, t: &PreviewTicket) -> PreviewPoll;         // Pending | Ready(PreviewPayload) | Failed(PreviewError)
    fn reprioritize(&self, t: &PreviewTicket, prio: PreviewPriority);
    fn cancel(&self, t: PreviewTicket);
    fn best_available(&self, key: PreviewKey) -> Option<Tier>;               // sync, index-only, no I/O
    fn tile(&self, key: PreviewKey, tile: TileCoord, prio: PreviewPriority) -> PreviewTicket; // T2 1:1 tiles
}
pub enum TierGoal { Thumb(u32 /*px*/), Standard, Tile1to1 }
pub enum PreviewPriority { Visible { dist: u32 }, Prefetch, Background }     // maps to E06 classes; Visible preempts
pub enum PreviewPayload { Cpu(Bitmap), /* decoded RGBA8, sized */ }
```

### 6.4 Canvas, viewer, gizmos (`lightbox-shell::canvas`)

```rust
/// Image px ↔ screen pt, including orientation. THE single transform authority for
/// loupe/compare/survey and every gizmo. Pure math, unit-tested exhaustively.
#[derive(Clone, Copy)]
pub struct ViewXform { pub scale: f32, pub offset: egui::Vec2, pub orientation: Orientation, pub image_px: [u32; 2], pub viewport: egui::Rect }
impl ViewXform {
    pub fn img_to_screen(&self, p: ImagePt) -> egui::Pos2;
    pub fn screen_to_img(&self, p: egui::Pos2) -> ImagePt;
    pub fn for_zoom(zoom: ZoomSpec, image_px: [u32; 2], viewport: egui::Rect, center: ImagePt) -> Self;
    pub fn clamped_pan(&self, delta: egui::Vec2) -> Self;    // image edge clamping w/ slack
}
pub enum ZoomSpec { Fit, Fill, Ratio(f32) }                  // Ratio(1.0) = 100%

/// Progressive tiled viewer used by loupe/compare/survey: draws best-available tier now,
/// requests better, crossfades upgrades; at Ratio>=1.0 switches to T2 tile fetch.
pub struct TiledImageView { pub key: PreviewKey, pub viewport: ImageViewport }
pub struct ImageViewport { pub zoom: ZoomSpec, pub center: ImagePt }         // animatable
impl TiledImageView {
    pub fn ui(&mut self, ui: &mut egui::Ui, thumbs: &mut ThumbService,
              link: Option<&ViewportLink>, gizmos: Option<&mut GizmoLayer>) -> ViewResponse;
}
/// Linked zoom/pan for Compare: both panes share one ImageViewport through the link.
pub struct ViewportLink(pub Rc<RefCell<ImageViewport>>);

/// The gizmo framework E11/E12 build on. Hit radii are SCREEN-space (constant-feel handles
/// at any zoom); geometry is IMAGE-space (survives zoom/pan/orientation).
pub trait Gizmo {
    fn hit(&self, cursor: ImagePt, xf: &ViewXform) -> Option<GizmoHit>;      // None = transparent to input
    fn on_event(&mut self, ev: GizmoEvent, xf: &ViewXform) -> GizmoResponse; // Consumed | Ignored | Commit(GizmoEdit)
    fn paint(&self, painter: &egui::Painter, xf: &ViewXform, vis: GizmoVis); // vis: Normal|Hover(hit)|Active(hit)
}
pub enum GizmoEvent { DragStart { hit: GizmoHit, pos: ImagePt }, Drag { pos: ImagePt, delta_img: egui::Vec2 }, DragEnd, Hover(Option<GizmoHit>), Key(Chord) }
pub struct GizmoLayer { /* Vec<Box<dyn Gizmo>>, capture: Option<usize> */ }
impl GizmoLayer {
    pub fn push(&mut self, g: Box<dyn Gizmo>);
    /// Input routing: captured gizmo first; else top-most hit; else pass through to viewer pan/zoom.
    pub fn route(&mut self, resp: &egui::Response, xf: &ViewXform) -> Vec<GizmoEdit>;
    pub fn paint_all(&self, painter: &egui::Painter, xf: &ViewXform);
}
```

### 6.5 Activity center UI (`lightbox-shell::activity_ui`)

**[consumed contract — E06 activity model]**:

```rust
pub struct ActivitySnapshot { pub tasks: Vec<ActivityTask> }                 // cheap clone, polled per frame
pub struct ActivityTask { pub id: JobId, pub label: String, pub class: Class,
                          pub progress: Progress /* Fraction(f32) | Indeterminate | Items{done,total} */,
                          pub cancellable: bool, pub pausable: bool, pub paused: bool }
pub trait ActivityControl { fn cancel(&self, id: JobId); fn pause(&self, id: JobId); fn resume(&self, id: JobId); }
```

E08 renders: compact stacked bar in the top bar (identity-plate slot) cycling active tasks; click → disclosure popover listing all tasks with cancel/pause buttons (pause only on `Background`-class analysis tasks per §5.3).

### 6.6 Prefs store (`lightbox-core::prefs`) and the key table

```rust
pub enum PrefScope { Machine, Catalog }

/// Typed key: compile-time key/scope/default; JSON-serializable value.
pub trait PrefKey: 'static {
    type Value: Serialize + DeserializeOwned + Clone + PartialEq + Send + Sync;
    const KEY: &'static str;                   // "cache.raw.max_bytes"
    const SCOPE: PrefScope;
    fn default_value() -> Self::Value;
    fn validate(v: &Self::Value) -> Result<(), PrefError>;   // range clamp/reject
}

pub struct PrefsStore { /* machine TOML doc + catalog_settings handle + watch senders */ }
impl PrefsStore {
    pub fn open(machine_path: &Path, catalog: Option<CatalogSettingsHandle>) -> Result<Self>;
    pub fn get<K: PrefKey>(&self) -> K::Value;                       // default if unset/invalid
    pub fn set<K: PrefKey>(&self, v: K::Value) -> Result<(), PrefError>; // validate → persist atomically → notify
    pub fn watch<K: PrefKey>(&self) -> tokio::sync::watch::Receiver<K::Value>;
    pub fn attach_catalog(&self, h: CatalogSettingsHandle);          // catalog open/switch re-binds Catalog-scope keys
}
```

**Pref keys defined by E08** (the panel binds exactly these; consumers subscribe via `watch`):

| Key | Scope | Type / default | Validation | Consumer (reader) |
|---|---|---|---|---|
| `perf.gpu.mode` | Machine | `Auto \| Disabled` / `Auto` | — | `lightbox-render` engine (E05 wiring; §4.4 "GPU disabled in prefs") |
| `perf.gpu.ai_vram_override_mb` | Machine | `Option<u32>` / `None` (auto: 8 GB dedicated / 16 GB unified, §6) | 1024..=131072 | `lightbox-ml`/E13 gating (key reserved now; read later) |
| `perf.jobs.background_workers` | Machine | `Option<u8>` / `None` (auto = cores−2, min 1) | 1..=64 | `lightbox-jobs` `set_limits` (E06) |
| `perf.jobs.foreground_io` | Machine | `Option<u8>` / `None` (auto) | 1..=32 | E06 |
| `perf.jobs.analysis_paused` | Machine | `bool` / `false` | — | E06 (Background analysis default pause) |
| `ui.thumb_atlas_cap_mb` | Machine | `u32` / 256 | 64..=2048 | shell `ThumbAtlas` |
| `cache.raw.max_bytes` | Catalog | `u64` / 5 GiB (§3.3) | ≥ 512 MiB | `lightbox-preview` raw cache (E03) |
| `cache.raw.location_override` | Catalog | `Option<PathBuf>` / `None` (= `.lbdata/rawcache`) | must exist+writable at set-time; missing at open ⇒ fallback+badge | E03 |
| `cache.preview.max_bytes` | Catalog | `u64` / 20 GiB | ≥ 1 GiB | E03 (preview LRU cap, §3.3) |
| `cache.preview.t2_retention` | Catalog | `Days(u32) \| Never` / `Days(30)` | 1..=3650 | E03 (T2 auto-evict, §3.3) |
| `cache.preview.standard_max_px` | Catalog | `Auto \| Px(u32)` / `Auto` (display-matched) | 1024..=8192 | E03 |
| `ui.state.*` (grid cell size, filmstrip height, last source, panel layout) | Catalog | JSON blobs | — | shell only |

**Consumer wiring (E08's responsibility, against dependency-epic setter APIs):** core session start binds `watch::Receiver`s to **[consumed contracts]** `PreviewStore::set_policy(CachePolicy)`, `PreviewStore::relocate(dest: Option<PathBuf>) -> JobHandle`, `PreviewStore::purge(PurgeScope) -> JobHandle`, `PreviewStore::stats() -> CacheStats` (E03) and `JobRuntime::set_limits(JobLimits)` (E06). `perf.gpu.mode` is only *published* by E08; the engine subscribes in its own epic (E05) — the panel shows "takes effect: immediately/next render" per that contract.

### 6.7 Default keymap (Library module, shipped defaults — remappable)

| Chord | Action id | Notes |
|---|---|---|
| `G` / `E` / `C` / `N` | `library.view.{grid,loupe,compare,survey}` | LR muscle memory |
| `D` | `app.module.develop` | chassis switch; E10 fills content |
| `P` / `U` / `X` | `library.flag.{pick,unflag,reject}` | selection-wide |
| `1`–`5`, `0` | `library.rating.set{1..5}`, `library.rating.clear` | |
| `6`–`9` | `library.label.{red,yellow,green,blue}` | |
| `B` | `library.quick_collection.toggle` | target-collection aware |
| `I` | `library.loupe.info_cycle` | overlay A → B → off |
| `Space` | `library.loupe.enter_or_zoom_toggle` | grid→loupe; loupe fit↔last zoom |
| `Z` | `library.loupe.zoom_100_toggle` | |
| `+` / `-` | `library.loupe.zoom_{in,out}` | steps through presets |
| Arrows / `Home` / `End` / `PgUp` / `PgDn` | `library.nav.*` | `repeatable: true`; Shift extends |
| `Cmd+A` / `Cmd+Shift+A` | `library.select.{all,none}` | |
| `Cmd+[` / `Cmd+]` | `library.rotate.{ccw,cw}` | |
| `Delete` | `library.remove.dialog` | remove-vs-trash dialog (E07 semantics) |
| `Cmd+Backspace` | `library.delete_rejected` | confirm dialog, source-scoped |
| `Tab` / `Shift+Tab` | `workspace.panels.{toggle_side,toggle_all}` | |
| `Cmd+F` | `library.filter.focus_text` | focuses filter-bar text field |
| `Cmd+/` | `app.cheatsheet.toggle` | context-generated |
| Caps Lock (state) + explicit menu/pref toggle | `library.culling.auto_advance` | Caps Lock is best-effort (risk R4) |

---

## 7. Ordered task breakdown

Each task ≤ 1 engineer-day. Order within a phase is the dependency order; phases A→B→C→D are sequential spine, E can interleave after B, F after D, G after A8, H last. 50 tasks ≈ 50 focused days + review/integration slack ⇒ consistent with 7–11 pw across 1.5–2 engineers.

### Phase A — chassis, keymap, prefs foundations

- **T01 — Workspace/module scaffold.** `Module` trait, module bar (Library active, Develop placeholder), mode-switch actions, per-module context push. *AC:* switching modules swaps panel sets and context stack; kittest snapshot committed; Develop placeholder renders "module not installed" pane.
- **T02 — Panel rail framework.** Left/right collapsible rails, accordion panels, Solo mode, hover auto-reveal, Tab/Shift-Tab chrome toggles. *AC:* Solo opening one panel closes others; **collapsed panels' body closures are not executed** (perceived-speed requirement, research 09); state persists via `ui.state.panel_layout`.
- **T03 — Keymap core.** `Chord` parse/display (platform-aware), `ActionDef`, `KeymapRegistry`, innermost-context resolution. *AC:* unit tests — parse round-trip, resolution precedence, conflict listing; 100 registered actions resolve in <10 µs.
- **T04 — Dispatch + default Library bindings.** Frame-level dispatcher, text-input suppression, key-repeat for `repeatable` actions, §6.7 defaults registered. *AC:* kittest — `P` in a focused text field types "p", not a flag; repeat on `→` navigates repeatedly.
- **T05 — Keymap persistence + rebind.** `keymap.toml` overrides load/save (atomic), conflict detection on rebind, reset one/all, unknown-id passthrough. *AC:* proptest round-trip; corrupt file ⇒ defaults + notice, file preserved as `.bak`.
- **T06 — Cheat-sheet overlay.** `Cmd+/` overlay generated from registry for the active context stack, grouped by category, searchable. *AC:* every registered action with a binding appears; snapshot test; closes on Esc/`Cmd+/`.
- **T07 — Keymap editor UI.** Prefs-window tab: action list w/ search, record-chord capture widget, conflict warning + steal option, reset buttons. *AC:* rebinding `X` persists across app restart (kittest + file assert).
- **T08 — Prefs store + migration.** `lightbox-core::prefs` per §6.6; `catalog_settings` migration; atomic TOML write; watch channels; catalog attach/detach on open/switch. *AC:* unit — set→get→watch fires once per change; fault-injection: kill mid-write leaves prior file readable; invalid value rejected by `validate`; migration up/down tested against E07-current schema.

### Phase B — thumbnails & grid

- **T09 — Query-façade contract reconciliation + `GridModel` spine.** Pin the §6.2 `LibraryQuery` signatures with E07 (compile-time contract test in a shared fixture crate); implement id-vector + epoch + async refresh. *AC:* fake-façade unit tests — refresh latest-wins, stale hydrate dropped; `index_of` O(1).
- **T10 — Windowed hydrate + LRU meta cache.** `window()` returning cached rows + enqueueing batched misses (batch ≤ 256 ids); `on_event` patch-in-place for `ImagesChanged`, full refresh on `SourceInvalidated`. *AC:* scrolling a 100 k fake source issues zero synchronous queries; hydrate batches coalesce.
- **T11 — Thumb service.** `want()` diffing, distance-ordered `PreviewPriority`, cancel-on-scroll, decode-off-thread, per-frame upload budget. *AC:* unit with stub provider — scrolling 1 000 rows cancels all off-screen requests; visible requests reprioritized not re-issued.
- **T12 — Texture atlas.** Shelf packer, cell-size classes, page alloc/evict by last-visible frame, VRAM cap from `ui.thumb_atlas_cap_mb`. *AC:* allocator unit tests incl. fragmentation churn; cap never exceeded under randomized insert/evict (proptest).
- **T13 — Virtualized grid widget.** Layout math (columns from cell size), scroll→visible range + overscan, placeholder cells, tier upgrade-in-place crossfade, scroll-position restore per source. *AC:* kittest — only visible+overscan cells built per frame (probe counter); 100 k synthetic scroll produces no frame with >8 ms shell CPU time on the reference dev machine (recorded baseline, asserted in perf harness later).
- **T14 — Selection model.** §6.2 semantics: click/shift/ctrl, active-vs-selected, keyboard nav w/ extend, all/none/invert. *AC:* exhaustive unit tests (table-driven, incl. LR edge cases: ctrl-click active leaves next-most-selected active).
- **T15 — Grid cell chrome.** Badges per `BadgeBits`, flag/rating/label glyphs, rejected dimming, VC page-curl, missing-file badge, embedded-preview indicator, hover rotation buttons. *AC:* snapshot matrix (each badge on/off); badge hit targets ≥ 20 px at 1×.
- **T16 — Sorts, cell size, custom order.** Sort menu (capture/edit time, rating, filename, custom), cell-size slider (pref-persisted), drag-to-reorder in collections issuing `ReorderCollectionItems`. *AC:* custom order round-trips through catalog and survives refresh; sort change preserves selection.
- **T17 — Grid context menu + commands.** Rotate, remove/delete dialog, delete-rejected, create virtual copy, add-to-collection submenu, set label/rating/flag. *AC:* every menu item routes through the command bus (no direct writes); disabled states match selection.
- **T18 — Grid perf pass + scenario.** Profile; add `perf_grid_scroll_100k` to the §8 scenario harness: scripted scroll, assert p95 frame time ≤ 12 ms shell CPU and zero UI-thread blocking waits (tracing probe). *AC:* scenario green on reference hardware; flamegraph attached to PR.

### Phase C — culling grammar

- **T19 — Culling controller + the epic's first failing test.** Implement `CullingController` (optimistic patch, reconcile-on-event, revert-on-failure), wire `P/U/X`, `0–5`, `6–9`. Land `cull_keystroke_p16ms` (see §1) and make it pass. *AC:* the named test green; multi-selection applies one batched command; badge appears same frame.
- **T20 — Auto-advance.** Pref + menu toggle; best-effort Caps Lock state detection behind a platform shim (feature-flagged per OS; explicit toggle is the guaranteed path). Advance semantics in grid/loupe/filmstrip; keystroke coalescing so held `X` doesn't overrun (§7 budget note). *AC:* rate-and-advance is one keypress; 20 keystrokes/s sustained without command backlog growth (probe).
- **T21 — Quick/target collection.** `B` toggle against E07's target-collection command; badge in grid/filmstrip; "set as target" on any collection. *AC:* toggle reflects in collections panel count within one event round-trip.
- **T22 — Delete-rejected + removal semantics.** Source-scoped confirm dialog (counts fetched async), `RemoveImages` remove-vs-trash dialog w/ "don't ask again" pref. *AC:* integration — rejects flagged in a 1 k session are purged; files untouched in catalog-only mode (fs assert); trash path uses E07's trash command.

### Phase D — loupe, canvas & gizmos

- **T23 — Viewport math.** `ViewXform`/`ImageViewport`/`ZoomSpec` incl. orientation, clamped pan, zoom-to-cursor, fit/fill math, animated transitions. *AC:* pure-math unit suite (all orientations × zooms; screen↔image round-trip < 0.01 px).
- **T24 — `TiledImageView`.** Best-available-now rendering, async upgrade + crossfade, drag pan + scrubby pan, wheel zoom-to-cursor, click-to-toggle zoom. *AC:* kittest — first frame after open never blank when any tier exists (T0 embedded fallback); upgrade swaps without flash.
- **T25 — Loupe view + prefetch.** Loupe over `GridModel` order; arrows next/prev; ±3 neighbor `Prefetch` tickets, drop on direction change. *AC:* **next/prev swap < 50 ms with warm T1** (§7) measured in harness; navigation at key-repeat speed never shows placeholder when T1 exists.
- **T26 — 1:1 / T2 tile path.** At `Ratio ≥ 1.0` request visible T2 tiles (§3.3), tile grid stitch, fade-in, cancel on pan/zoom-out. *AC:* panning at 100 % issues only newly-visible tile requests (probe); zoom-out cancels all tile tickets.
- **T27 — Info overlay + toasts.** `I` cycling (two configurable layouts + off: filename/capture settings/dims), embedded-preview-only indicator, non-modal notice area (device-lost etc. arrive via core events). *AC:* snapshot per layout; overlay fields configurable via prefs UI stub.
- **T28 — Gizmo framework.** §6.4 trait/layer/routing/capture; screen-space hit radii; paint pass; reference gizmo (draggable two-handle line) used by tests only. *AC:* kittest interaction — hover states, drag capture across fast mouse moves, `Esc` cancels drag, zoom mid-drag keeps handle under cursor; API doc for E11/E12 consumers (T50 collects).
- **T29 — Display-path integration.** Loupe/compare/survey composite their textures through the E01-established display-transform `RenderNode` path on the shared device (no new nodes); verify zero-copy (no readback in trace). *AC:* gpu trace shows no texture copy between engine output and egui compositing; sRGB assumption documented + flagged to E02.3 seam (A3).

### Phase E — filmstrip & Library panels

- **T30 — Filmstrip core.** Horizontal virtualization reusing `GridModel`+`ThumbService`+`SelectionModel`; persistent across modules; resizable/collapsible. *AC:* selection/active stay in sync grid↔filmstrip↔loupe; hidden filmstrip costs zero per-frame work.
- **T31 — Filmstrip chrome.** Source breadcrumb + recent-sources menu, quick filters (flag/rating/label), mini badges. *AC:* quick filter mutates the shared `FilterSpec` and both grid + filmstrip update from one query.
- **T32 — Folders panel.** Lazy tree over E07 queries (volumes → roots → folders), counts, missing-volume badge; create/rename/move/delete + "synchronize folder" invoking E07 commands with confirm dialogs. *AC:* fs changes surfaced by E07 events re-render the affected subtree only; no full-tree re-query on expand.
- **T33 — Collections panel.** Sets/collections tree, create/rename/delete/duplicate, drag images to collection (membership add), drag collection into set, target-collection marker. *AC:* drag-drop add of 1 000 selected images issues one batched command.
- **T34 — Keywords panel.** Hierarchy tree w/ counts, autocomplete entry field (comma-batch), apply/remove over selection, recently-used row. *AC:* autocomplete queries are async + debounced (≥ 150 ms); apply round-trips visible in grid badges.
- **T35 — Metadata panel.** Switchable field layouts (default/EXIF/IPTC), editable IPTC fields, metadata-preset picker + sync-metadata entry points (dialog content = E07's contract). *AC:* edits issue E07 metadata commands; read-only EXIF rendered from `metadata_cache` hydrate without touching originals.
- **T36 — Filter bar: text + attribute rows.** FTS text field (`fts()` façade, debounced), attribute row (flag / rating ≥ / label / kind / VC), filter chips, clear-all; filters compose into the shared `FilterSpec`. *AC:* 100 k catalog: text query round-trip renders results without UI stall (async, <100 ms p95 query per §7 — query time is E07's budget, non-blocking is E08's).
- **T37 — Filter bar: facet columns + presets + lock.** Drill-down metadata columns (date/camera/lens/ISO/keyword) from `facet_counts`, multi-select within column, saved filter presets (catalog-scope pref), lock-filter-across-sources toggle. *AC:* facet counts update on filter change without flicker (stale-while-revalidate); presets persist per catalog.

### Phase F — compare & survey (post-M1-gate tail, §2.1)

- **T38 — Compare view.** Select-vs-candidate panes (`TiledImageView` ×2), `ViewportLink` linked zoom/pan w/ unlock toggle, swap + make-select (promote), arrow-advance candidate, culling keys act on focused pane. *AC:* linked pan keeps both panes pixel-aligned at 100 %; promote updates selection model correctly (kittest).
- **T39 — Survey view.** N-up packing layout (aspect-aware rows), knock-out via per-image ✕ and `/` key on focused image, selection shrinks live, focus ring navigation. *AC:* works 2–12+ images; removing one reflows without image reload (atlas reuse).
- **T40 — View-mode plumbing + entry rules.** `G/E/C/N` transitions, per-view zoom memory, entry fallbacks (compare needs 2 → uses active+next; survey needs ≥ 2 → falls back to loupe), Esc-returns-to-grid convention. *AC:* table-driven transition tests; no view ever opens empty.
- **T41 — Compare/survey test pass.** kittest snapshots + interaction suites for both views; culling-in-compare integration test. *AC:* suites green in CI on all three platforms (fast subset PR-blocking per §8).

### Phase G — preferences panel & activity UI

- **T42 — Pref keys + consumer wiring.** Define every §6.6 `PrefKey`; bind watches to E03 `set_policy` and E06 `set_limits` at session start; publish `perf.gpu.mode`. *AC:* integration — changing `cache.raw.max_bytes` is observed by a stub E03 within one notify; invalid values clamped/rejected with UI feedback.
- **T43 — Prefs UI: caches.** Raw cache size/location (relocate flow = pick dir → E03 `relocate()` job w/ progress via activity center → pref update on success), purge buttons w/ confirm + freed-bytes report, usage readouts from `stats()`, T2 retention, preview cap, standard-preview size. *AC:* relocate is cancellable and atomic (old location remains authoritative until job success — E03's contract, asserted via stub); panel reflects live `stats()`.
- **T44 — Prefs UI: GPU & AI thresholds.** Adapter readout (name/backend/VRAM from the E01 `wgpu::Adapter` info), `perf.gpu.mode` control with effect-timing note, detected-VRAM + AI-gate threshold display and `ai_vram_override_mb` override (marked "used by AI features when installed" until E13). *AC:* Disabled mode publishes through watch (observed by stub engine subscriber); readout matches `wgpu::AdapterInfo` on all CI platforms.
- **T45 — Prefs UI: jobs + general plumbing.** Concurrency knobs w/ "auto (N)" resolved display, analysis-pause default toggle; prefs window chrome (tabs: Performance / Keymap / Interface), reset-to-defaults per tab. *AC:* setting `background_workers=1` observably serializes stub background jobs (integration with E06 test runtime).
- **T46 — Activity center UI.** Top-bar stacked progress + disclosure popover per §6.5; cancel/pause controls; empty/overflow states; unobtrusive (no layout shift). *AC:* 10 concurrent fake jobs render at 60 fps; pause only offered on pausable tasks; cancel round-trips to E06 stub within one poll.

### Phase H — integration, performance, accessibility, docs

- **T47 — End-to-end culling integration + fault injection.** `lightbox-cli` imports 1 k fixture raws → kittest-scripted cull session (flags/ratings/labels/auto-advance) → assert catalog rows; `kill -9` mid-burst → `integrity_check` clean and last-committed keystrokes durable (§3.1 invariant, §8 gate). *AC:* PR-blocking test green on all platforms.
- **T48 — Perf harness scenarios.** Add to the §8 nightly harness: `keystroke_to_badge` (<16 ms p95), `loupe_swap_warm_t1` (<50 ms p95), `grid_scroll_100k` (T18 assertion), `import_10k_browsable` (grid interactive <60 s — shared ownership with E04: E08 asserts the UI side never blocks and first-paint-from-T0; the pipeline throughput half lives in E04's spec). *AC:* budgets encoded as harness assertions with recorded reference-hardware baselines; regressions file issues per §8.
- **T49 — Accessibility pass.** AccessKit roles/labels for grid cells, loupe, panels, filter bar, prefs; focus order; every `ActionDef.label` exposed as an accessible action. *AC:* kittest AccessKit queries find grid cells with name/state; keyboard-only cull session possible (scripted).
- **T50 — DPI/theme polish + seam docs.** 1×/2× glyph assets, minimum window size, panel font-size pref; write `docs/plan/epics/E08-seams.md` **content into this spec's appendix instead** (single-file rule): canvas/gizmo API guide for E11/E12, keymap-registration guide for E10+, pref-key registry for E03/E05/E06/E13. *AC:* docs reviewed by one engineer from the pixel stream; no unresolved API questions from E12's planner against the gizmo trait.

---

## 8. Test plan

Per the §8 strategy; E08 adds no golden-image (render) gates — its "goldens" are UI snapshots.

| Layer | Tests (representative, not exhaustive) | Gate |
|---|---|---|
| **Unit** | Chord parse/display round-trip; keymap resolution precedence + conflicts (proptest over random registries); `SelectionModel` table-driven semantics; `GridModel` windowing/epoch/latest-wins; atlas allocator under churn (proptest, cap invariant); `ViewXform` screen↔image round-trip across orientations/zooms; prefs validate/clamp; culling optimistic-apply/revert state machine | PR-blocking |
| **UI harness (`egui_kittest`)** | `cull_keystroke_p16ms` (first failing test); text-input suppression; cheat-sheet completeness; grid virtualization probe (cells built = visible+overscan); badge snapshot matrix; loupe never-blank; gizmo drag/capture/cancel; compare linked-pan alignment; survey knock-out; keymap editor rebind persistence | PR-blocking (fast subset), full nightly |
| **Integration** | cli-import → scripted cull → catalog row assertions; delete-rejected fs semantics; quick-collection round-trip; pref change → E03/E06 stub observation; relocate-cache job flow incl. cancellation; catalog-switch re-binds catalog-scope prefs | PR-blocking |
| **Fault injection** | `kill -9` mid-culling-burst → `integrity_check` clean (§8 crash-safety gate); corrupt `prefs.toml`/`keymap.toml` → defaults + preserved `.bak`; missing cache-relocation volume → fallback + badge | PR-blocking |
| **Performance (nightly harness, §8)** | `keystroke_to_badge` p95 < 16 ms; `loupe_swap_warm_t1` p95 < 50 ms; `grid_scroll_100k` p95 shell CPU ≤ 12 ms/frame, zero UI-thread blocking waits; `import_10k_browsable` UI-side assertions; filter-change → repaint non-blocking at 100 k; thumb-atlas VRAM cap held under scripted stress | Nightly; regression → tracked issue |
| **Cross-platform** | full unit + kittest fast subset on macOS/Windows/Linux CI matrix (§8); Caps Lock shim feature-gated per platform with the explicit toggle path tested everywhere | PR-blocking |
| **Accessibility** | AccessKit query suite (T49) | PR-blocking (subset) |

Test fixtures: a synthetic-catalog generator (100 k rows, deterministic seed) lands in a shared `lightbox-testkit` fixture crate (coordinate with E07, which needs the same for query benchmarks — reuse, don't duplicate).

---

## 9. Performance budgets (restated from §7 as this epic's acceptance numbers)

| Budget | Owner split |
|---|---|
| Flag/rate keystroke → badge **< 16 ms** | E08 (optimistic UI + single-row txn via E07) |
| Next/prev preview swap **< 50 ms** (warm T1) | E08 (prefetch, texture reuse) over E03 (tier availability) |
| Import 10 k → grid browsable **< 60 s**, UI input latency unaffected | E04 (pipeline) + E03 (T0 extraction); E08 asserts non-blocking UI + demand-driven grid |
| Filter/search/smart-collection **< 100 ms p95** at 100 k | E07 (query); E08 (async invocation, stale-while-revalidate, no UI stall) |
| 60 fps grid scroll at 100 k; thumb atlas within cap | E08 |
| Zero interactive starvation from background work | E06 (classes); E08 (visible-first priorities + cancel-on-scroll) |

---

## 10. Risks & open questions

### Risks

- **R1 — egui widget ceiling (the §10 re-baseline reason).** Virtualized 100 k grid + atlas + panels is a from-scratch build on a thin ecosystem; 4K/low-end-GPU frame budgets may pinch. *Mitigation:* T13/T18 land early with hard perf probes; atlas cap pref; the architecture's named reversal trigger (swap `lightbox-shell` for Qt/Slint against the same core API) stays live — E08 keeps **all** logic (grid model, selection, culling, keymap semantics, prefs) in shell-agnostic modules so a shell swap re-uses them.
- **R2 — Immediate-mode global-shortcut vs text-input conflicts.** Single-letter culling keys collide with every text field. *Mitigation:* dispatcher-level focus suppression (T04) tested explicitly; contexts make it structural, not ad-hoc.
- **R3 — Windowed hydrate query cost at 100 k.** `hydrate()` joins image/edit_index/metadata_cache; if E07's indices don't cover the sort orders, scroll hitches. *Mitigation:* contract test + shared fixture benchmarks in T09; batching ≤ 256; escalate to E07 if p95 hydrate > 10 ms/batch.
- **R4 — Caps Lock state detection is not portable.** winit exposes it unevenly across platforms/layouts. *Mitigation:* explicit auto-advance toggle is the guaranteed, documented path; Caps Lock is a best-effort per-platform shim behind a feature flag (T20); never a correctness dependency.
- **R5 — VRAM pressure from thumb atlas + engine sharing one device.** Grid atlas competes with the render engine's tiles. *Mitigation:* hard atlas cap (pref), eviction by last-visible frame, budgeted per-frame uploads; the engine's own tiling (§7) is independent.
- **R6 — Compare/survey milestone tension** (§2.1). *Mitigation:* sequenced last; M1 exit doesn't gate on them.
- **R7 — Cache-relocation flow spans an ownership boundary.** The move-files job must be E03's (it owns the store's atomicity); E08 only drives UI. If E03's spec lacks `relocate()/purge()/stats()/set_policy()`, T43 blocks. *Mitigation:* contracts named in §6.6 and raised at kickoff (Q2).
- **R8 — Optimistic culling vs command failure.** A failed write after optimistic badge paint shows a lie for one round-trip. *Mitigation:* revert-and-toast path unit-tested (T19); writes are single-row txns whose realistic failure mode (disk full) also surfaces via §6's pre-flight checks.

### Open questions

- **Q1 — Activity-center UI ownership.** This spec claims the UI surface (T46) over E06's model. If E06's spec also claims it, drop T46 here and consume theirs. Resolve at spec review; either way the model contract in §6.5 stands.
- **Q2 — E03 cache-management API surface** (`set_policy/relocate/purge/stats`, `TierGoal::Thumb` decoded-bitmap payloads, T2 `tile()` access). Named here as consumed contracts; confirm against E03's spec at kickoff and reconcile signatures in T09/T42.
- **Q3 — Migration number + `catalog_settings` vs any E07 settings table.** If E07 already ships a per-catalog settings table, E08 reuses it and drops §5.1. Coordinate before T08.
- **Q4 — Filter-preset + saved-filter storage shape** (catalog-scope pref blob vs first-class E07 table). Default here: pref blob (`ui.state.filter_presets`); revisit if E16's `.lrcat` importer needs to map LR filter presets into a table.
- **Q5 — `perf.gpu.mode` effect timing.** Immediate engine fallback vs next-session — the contract is E05's (§4.4 names the pref); the panel copy in T44 follows whatever E05 specifies. Default assumption: immediate degrade-to-CPU-preview per §6.2 device-loss machinery.
- **Q6 — Per-monitor ICC in the Library loupe (A3).** E08 renders through the E01 display-transform node with sRGB assumption until E02.3 lands; confirm E02.3 publishes its display-profile config through the same node params so no E08 rework is needed.

### Assumptions

- **A1:** E01's shell skeleton provides the eframe app loop, shared `Arc<wgpu::Device>`, command/query bus handles, and the seed display-transform node path (per §9 M0) — E08 extends, never re-plumbs.
- **A2:** E06 exposes job classes/priorities exactly as §5.3 (Interactive > Foreground > Background) and per-task cancel/pause handles.
- **A3:** Color-managed display beyond sRGB is delivered by E02.3 through the display-transform node's parameters; E08 carries a visible `// COLOR: sRGB-assumed` marker at the single compositing call site.

---

## 11. Definition of done

E08 is done when **all** of the following hold:

1. All 50 tasks' acceptance criteria met; phases A–E + G complete before the M1 exit review; phase F complete before epic close.
2. **M1 exit behaviors demonstrated end-to-end** on all three platforms: import 10 k raws (with E04) → grid browsable < 60 s with no UI stall; keyboard cull session (P/X/U/1–5/6–9 + auto-advance) with keystroke→badge < 16 ms and next/prev < 50 ms warm; 100 k catalog filter/search interaction with no UI-thread blocking.
3. Perf harness scenarios (T48) green against recorded reference-hardware baselines and wired into the §8 nightly run; `cull_keystroke_p16ms` and the fault-injection culling test are PR-blocking in CI.
4. Keymap: every shipped action remappable, persisted, conflict-checked; cheat-sheet auto-generated; keymap editor usable keyboard-only.
5. Prefs: every §6.6 key settable from the panel, validated, atomically persisted, and **observed live** by its owning subsystem (E03/E06 integration tests; `perf.gpu.mode` published on the watch channel with a subscriber test).
6. Gizmo framework API reviewed and signed off by the E12 planner (the first real consumer) — no blocking API gaps.
7. AccessKit pass (T49) green; a full cull session is possible with keyboard only.
8. License surfaces untouched or clean: new crates (`egui_kittest`, `lru`, `indexmap`) pass cargo-deny; no native-binary or data-manifest changes.
9. No modifications outside `lightbox-shell`, `lightbox-core::prefs`, the one `lightbox-catalog` migration, and shared test fixtures; the headless boundary holds (no UI types below core, no SQL above it) — asserted by the existing E01 boundary lint/test.
10. Seam documentation (T50 appendix) delivered to E10/E11/E12/E13 planners; open questions Q1–Q6 resolved and recorded in this file's revision history.
