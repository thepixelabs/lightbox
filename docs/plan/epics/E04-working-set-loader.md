# E04 — Working-set loader & drag-drop intake (v2.0)

_Implementation spec, **v2.0 rewrite**. Milestone **M1** · Effort **S ~2–3 pw** · Depends on **E01** (foundation: ingest primitives, catalog, jobs, headless core), **E03** (preview pyramid — consume-only seam, see §2.3)._
_Inputs: `docs/plan/00-mandate.md` (**v2.0/v2.1**), `docs/plan/01-architecture.md` (**v2.0** — §2.4 editor-shell contract, §3.1.1 edit-store rescope, §10.0 disposition audit, §10.1 epic table), `docs/plan/epics/E01-deviations.md`, and the **built code** in `crates/` (this spec is written against the real E01 surfaces, not the v1.x sketches)._

> **Supersedes `E04-ingest-import-pipeline.md` entirely.** The v1.x E04 (managed copy/move import, rename templates, second-copy backup, card detection, watched folders, import sessions) is **retired with the DAM** (mandate v2.0). That document is historical record only — do not implement from it. The walker/probe/hash primitives it was going to grow are **re-pointed** here at the session working-set contract of architecture §2.4.

**One-paragraph thesis.** E04 turns *any* set of user-given paths — a dropped file, a multi-file drop, a folder (flat or recursive), an OS open-dialog selection, or a file-association/CLI launch — into an **ordered, in-memory session working set**: enumerate → probe → order → hash → register in the edit store → publish. Files open **in place**: no copy, no second-copy, no rename, no import session, no folder tree of record. The set itself is **session state, never persisted** — closing the app discards it; what persists is edit-store *plumbing* only (one content-hash-keyed `asset` row + one default `image` row per opened file, so E09's recipes survive reopen and E03's caches key correctly). E04 is headless: it ships the loader in `lightbox-ingest`, the open-registration DAO in `lightbox-catalog`, the working-set model + `OpenWorkingSet` command in `lightbox-core`, and a `lightbox-cli open` harness. The drop target, hover affordance, open dialog, single-instance IPC, and filmstrip UI are **E08**; E04 is everything below that seam.

---

## 1. Scope & non-goals

### 1.1 In scope (E04 owns)

1. **Intake contract** — one vocabulary type (`OpenRequest`) that represents every §2.4 entry gesture identically: ordered paths + a recursive flag + an origin tag. E08's drop handler, `rfd` dialog, and platform launch handler all construct this one type and submit one command.
2. **Enumeration & classification** — canonicalize paths; classify file vs folder; expand folders via the E01 walker (`discover_files`, flat or recursive); explicit-file vs walk-discovered semantics (an explicitly chosen file is always honored; a walk is extension-filtered and skips hidden entries); skip accounting with reasons; a hard set-size cap.
3. **Probe & order** — metadata-only `lightbox_decode::probe()` per candidate; the deterministic session ordering rules (§6): gesture order for explicit files, capture-time-then-name for folder expansions, folders expanded in place.
4. **Hash & register** — streaming `hash_file` per item (content identity, architecture §3.1.1) and the **open-registration DAO**: content-hash-keyed upsert of the `asset` row + default `image` row, path-as-hint refresh, relocation detection. This is the single point where "opening a file" touches the edit store.
5. **The in-memory working-set model** in `lightbox-core` — epoch-versioned, replace-on-new-drop, per-item states (`Planned → Ready | Failed`), snapshot query for the shell, coalesced change events, `Command::OpenWorkingSet` dispatched as a cancellable Foreground job.
6. **Data-model changes** — migration `000N open_in_place` (asset table rebuild: `folder_id` nullable, `abs_path` path-hint column, per architecture §3.1.1) + the migration-runner extension the rebuild requires (§5.2); `ensure_open_asset` DAO; reader updates (`asset_abs_path` prefers `abs_path`).
7. **`SourceKind`** — the `Raw | Rendered` flag of §2.4, derived from the probe, added to `lightbox-types` and surfaced on every working-set item (E08 gates the develop panels on it).
8. **Headless surface** — `lightbox-cli open <paths…>` driving the full loader end-to-end (the M1 smoke path before E08's UI exists); default edit-store location resolution (`default_store_dir()`), coordinated with E08.

### 1.2 Explicit non-goals (named seam or retirement for each)

| Not in E04 | Disposition / owner |
|---|---|
| Managed copy / move / rename templates, DNG-on-import, second-copy backup, import presets | **RETIRED** (mandate v2.0 — files open in place; no managed import, ever) |
| `import_session` bracketing, undo-import, import history | **RETIRED-DORMANT** (§10.0): the E01 code (`import_files`, `Command::ImportAddInPlace`/`UndoImport`, the `import_session` table) stays compiled and keeps its tests, but the open path never touches it and no UI reaches it |
| Card/device detection, eject-after-import, watched/auto-import folders | **RETIRED** (DAM cut) |
| Duplicate-detection UX, "don't import suspected duplicates" | **RETIRED**; the loader's content-hash collapse (§6.4) is a session-set invariant, not an import feature |
| Drop-zone UI, hover highlight/count, filmstrip, open dialog (`rfd`), keymap (⌘O) | **E08** (constructs `OpenRequest`, submits `Command::OpenWorkingSet`, renders the snapshot) |
| Platform launch handler, "Open With"/file-association registration, single-instance IPC forwarding of paths | **E08** (platform integration); E04 defines the `OpenRequest` it must produce |
| Preview extraction/build scheduling, T0/T1/T2 store, raw cache | **E03** (demand-driven; E04 never enqueues preview builds — the filmstrip requests previews for visible items) |
| Raw/non-raw pixel decode, HEIC support, format-capability growth | **E02** (E04's extension list grows in lockstep — §6.2 seam) |
| Edit recipes, auto-persist of edits, XMP read/write, recipe restore on open | **E09** (keys off the `ImageId` E04 registers; E04 guarantees stable identity per content hash) |
| Job classes/activity-center model beyond what `lightbox-jobs` already ships | **E06** (E04 uses `Class::Foreground` + `CancelToken` as built) |
| Active-item selection, filmstrip scroll state, loupe navigation | **E08** (UI state; the model stores set + order only) |
| Recents list (a permitted Should — session-launched, never a catalog) | **E08**, post-M1; `Event::WorkingSetLoadFinished` carries what it would record |
| Persisting/restoring the working set across launches ("reopen last session") | **Out of v1** (§2.4: the set is not persisted). If ever wanted, it is a prefs-file feature over `OpenRequest`, not a schema |
| Missing-file relink, volume UUIDs, offline-volume awareness | **RETIRED** (DAM cut). An opened path must exist; a vanished file is a per-item failure |

### 1.3 The §2.4 entry table, mapped to this epic

| Entry gesture (§2.4) | E08 produces | E04 behavior |
|---|---|---|
| Drop a single file | `OpenRequest { paths: [f], recursive: false, origin: DragDrop }` | 1-item set; item 0 is the loupe file |
| Drop a multi-file selection | `paths` in drop order | set in drop order; first item loads first |
| Drop a folder | `paths: [dir]`, `recursive: false` | directly-contained supported files, sorted capture-time → name |
| Drop a folder, recursive (modifier / pref) | `paths: [dir]`, `recursive: true` | full tree walk, same sort |
| OS open dialog (multi-select files+folders) | `paths` in selection order, `recursive` from prefs | files keep selection order; folders expand in place |
| File-association / "Open With" / CLI args | `paths` from the OS/argv, `origin: FileAssociation \| Cli` | same construction |

A new `OpenRequest` **replaces** the current set (§2.4). There is no append gesture in the v1 contract (OQ-1).

---

## 2. Crates & modules touched (against the built code)

| Crate | Built state (E01, verified on `main`) | E04 change |
|---|---|---|
| `lightbox-types` | frozen vocabulary (ids, `ContentHash`, `Orientation`, `SourceTier`, `Flag`) | **add** `SourceKind { Raw, Rendered }` (§4.1). Additive to the frozen surface; joint-review note in §11 R6 |
| `lightbox-decode` | `probe()`/`read_embedded`/`hash_file` real (in-crate permissive walkers, xxh3-128 big-endian); `decode_*` declared for E02 | **add** `ProbedFormat::source_kind()` (derived, no struct change — `AssetProbe` stays untouched, §4.2) |
| `lightbox-ingest` | `discover_files` / `import_files` / `import_add_in_place`, `ImportOptions`/`ImportReport`/`ImportEvent`, `KNOWN_EXTENSIONS` (`crates/lightbox-ingest/src/pipeline.rs`) | **add** `working_set` module: `OpenRequest`, `plan_open`, `load_working_set`, `SetPlan`, `OpenReport`, skip/error taxonomy (§4.3). `discover_files` + `KNOWN_EXTENSIONS` are **reused as-is**; the `import_*` path is retired-dormant, untouched |
| `lightbox-catalog` | migration `0001_spine`, single-writer DAO (`insert_assets` dup-skip, `upsert_root/folder`, sessions), keyset readers, verified backup, fault harness | **add** migration `000N_open_in_place` (asset rebuild, §5.1) + runner `rebuilds_tables` extension (§5.2) + `ensure_open_asset` DAO (§4.4) + reader updates (`asset_abs_path` prefers `abs_path`; `ImageDetail.folder` becomes `Option<FolderId>`) |
| `lightbox-core` | `Core`/`Session`, command bus (`ImportAddInPlace`, `SetRating`, …), `Event` broadcast, `Queries`, `EmbeddedPreviewProvider` wiring, `CatalogAssetLocator` | **add** `working_set.rs` model + `Session::working_set()`, `Command::OpenWorkingSet`, `Event::WorkingSet*`, `CoreConfig.working_set`, `default_store_dir()` (§4.5). Existing import commands remain compiled (retired-dormant) |
| `lightbox-cli` | `create/import/list/render/check/backup`, hand-rolled args | **add** `open` subcommand (§4.6); `import` kept for the E01 fault/E2E harness, marked *(legacy, testing only)* in `--help` |
| `lightbox-preview`, `lightbox-render`, `lightbox-shell` | — | **no changes.** The provider/locator/engine seams are consumed unchanged; shell changes are E08 |
| New third-party deps | — | **`dirs`** (MIT/Apache-2.0, allowlisted) for `default_store_dir()` — or a ~30-line hand-roll if the implementer prefers zero deps (either passes the deny gate; record the choice in `E04-deviations.md`) |

### 2.3 Seams to neighboring epics (consumed / provided)

- **E08 → E04 (provided):** `Command::OpenWorkingSet { request }` + `Session::working_set()` snapshot + `Event::WorkingSet*`. E08 never sees paths-to-items logic; it renders the snapshot and submits requests. E04 lands **before** E08's UI; the CLI `open` command is the interim driver.
- **E04 → E03 (consumed, code-frozen):** nothing new. E04 registers `asset`/`image` rows so E03's content-hash-keyed stores and the E01 `AssetLocator` (`ImageId → path + orientation`) resolve. E04 **does not** enqueue preview builds — E03's demand-driven scheduler and the filmstrip's visible-first requests own that (this is why the E03 dependency is consume-only: the loader needs E03's tiers to exist for the M1 exit demo, not for its own code).
- **E04 → E09 (provided):** stable identity. `ensure_open_asset` guarantees: same content hash ⇒ same `AssetId` ⇒ same default `ImageId`, across sessions and across file moves. E09 hangs `edit_recipe` off that `ImageId`; recipe restore on open is E09's read, not E04's.
- **E04 → E02 (seam):** the supported-extension list (`KNOWN_EXTENSIONS`) and `SourceKind` mapping grow when E02 lands decode capability (HEIC/HEIF, JXL, AVIF originals). One const + one match arm per format, owned in `lightbox-ingest`/`lightbox-decode`; E02's spec must name the additions.
- **E04 → E06 (consumed):** `JobSystem::spawn_blocking(Class::Foreground, "working_set.open", …)` + child `CancelToken`, exactly as built. E06's later activity-center model picks the job up by name with no E04 change.
- **E04 → E16 (informational):** the migration + any new dep land in the SBOM/native-inventory surfaces as usual; no new native code is introduced.

---

## 3. Design overview

### 3.1 Pipeline shape (three phases, one Foreground job)

```
OpenRequest (E08 / CLI)
   │  Command::OpenWorkingSet — dispatcher cancels the previous epoch's job,
   │  bumps epoch E, publishes Event::WorkingSetOpening{E}
   ▼
[phase 1: PLAN — fast, no full-file reads]
   canonicalize + classify paths → expand folders (discover_files)
   → probe() each candidate (~0.1–1.3 ms warm, structures only)
   → order (§6.3) → SetPlan { items, skipped, truncated }
   → model swap: items in state Planned; Event::WorkingSetReplaced{E, planned}
   ▼
[phase 2: LOAD — hash-bound, item-granular, first item first]
   for each item in set order:
     hash_file (streaming, cancel between 1 MiB chunks)
     → ensure_open_asset (one small WAL txn)
     → item → Ready{asset,image} | Failed{reason} | collapsed as DuplicateContent
     → coalesced Event::WorkingSetChanged{E}
   ▼
[phase 3: FINISH]
   Event::WorkingSetLoadFinished{E, report} + one Event::CatalogChanged{bulk}
   (compat hint for the not-yet-reworked M0 grid; E08 removes its listener later)
```

The split exists so the filmstrip can render **immediately** after phase 1 (ordered names + probe dims + `SourceKind`, placeholders for pixels) while hashes land progressively — this is how the mandate's "open a 1,000-photo folder without blocking the UI" bar is met with a full-file-hash identity model. The **first** item's hash is on the critical path to first pixels (~10–30 ms for a 25–45 MB raw on NVMe) and is loaded first by construction.

### 3.2 What is persisted, and what is not

Per architecture §3.1.1 — on open, a file gets an `asset` row (content-hash keyed, `abs_path` hint, `folder_id NULL`) and a default `image` row on demand. That is **all**. The new path writes **zero** rows to `folder`, `library_root`, or `import_session` (keep-dormant honored — asserted by test, §9.4). The ordered set, epoch, item states, and skip list live only in the `WorkingSetModel`; `kill -9` at any point leaves the store `integrity_check`-clean and simply forgets the session (by design).

### 3.3 Identity, path hints, relocation

`content_hash` **is** identity (architecture §3.1.1: "path is a hint, not identity"):

- **Same content, new path** (file moved/renamed since last open): the hash lookup hits, `abs_path`/`filename`/`mtime_utc` are refreshed, `missing` cleared → same `AssetId`/`ImageId`, **recipe survives the move**. `EnsureOutcome.relocated = true`.
- **Same path, new content** (file edited in place by another tool): the hash misses → a **new** asset row is created for the new content; any other asset row still claiming that `abs_path` has its hint cleared (it no longer resolves). The old content's recipe stays keyed to the old row — an edit belongs to the pixels it was made on.
- **Two paths, same content** in one request: collapsed to one item (§6.4).
- **No UNIQUE index on `content_hash`** — deliberately. Both write paths (legacy `insert_assets` dup-skip and `ensure_open_asset`) enforce at-most-one-row-per-hash at the application layer; adding a UNIQUE constraint in the migration would create a failure mode (constraint violation aborting the whole upgrade on any pre-existing anomaly) for zero behavioral gain. `ensure_open_asset` reads `ORDER BY id LIMIT 1` so even an anomalous store degrades deterministically.

---

## 4. Interface definitions

Signatures are the contract; bodies are illustrative. Everything marked `#[non_exhaustive]` follows the E01 convention (grow without breaking).

### 4.1 `lightbox-types` — `SourceKind`

```rust
/// Which develop surface a source can expose (architecture §2.4): raw
/// sources get the full toolset; rendered sources get the same pipeline
/// minus the raw-only stages (hidden, not disabled). Derived from the
/// probe; carried on every working-set item; consumed by E08's panel
/// gating and E02/E05's pipeline assembly.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive] // headroom: a video-frame source is a v1.x possibility
pub enum SourceKind {
    /// Mosaic camera raw (CR2/CR3/NEF/ARW/RAF/ORF/DNG/…).
    Raw,
    /// Already-rendered pixels (JPEG/TIFF/PNG/HEIC/…).
    Rendered,
}
```

### 4.2 `lightbox-decode` — derivation (no frozen-struct change)

```rust
impl ProbedFormat {
    /// §2.4 source-kind mapping. `None` for `Unsupported` (no develop
    /// surface at all — the item is badged, never opened in the editor).
    pub fn source_kind(&self) -> Option<SourceKind> {
        match self {
            ProbedFormat::Raw(_) => Some(SourceKind::Raw),
            ProbedFormat::Jpeg | ProbedFormat::Tiff | ProbedFormat::Png => {
                Some(SourceKind::Rendered)
            }
            ProbedFormat::Unsupported(_) => None,
        }
    }
}
```

`AssetProbe` itself is **not** modified (it is a plain all-pub struct constructed in walker code and tests; adding a field is a breaking literal-construction change with no benefit over derivation).

### 4.3 `lightbox-ingest` — the loader (`src/working_set.rs`)

```rust
/// One user "open" gesture (§2.4). Constructed by E08 (drop / dialog /
/// launch handler) or the CLI; consumed by `Command::OpenWorkingSet`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OpenRequest {
    /// Paths exactly as the OS handed them, in gesture order. Files and
    /// folders may mix (the open dialog allows both).
    pub paths: Vec<PathBuf>,
    /// Folder expansion mode for every folder in this request
    /// (drop-modifier or prefs default — §2.4).
    pub recursive: bool,
    /// Where the gesture came from. Tracing/UX copy only — the
    /// construction rules do not vary by origin.
    pub origin: OpenOrigin,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum OpenOrigin { DragDrop, OpenDialog, FileAssociation, Cli }

/// Loader knobs (mirrors the `ImportOptions` pattern).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OpenOptions {
    /// Hard cap on planned items; beyond it the plan truncates and reports
    /// (guards a recursive drop of a home directory). Default 10_000.
    pub max_set_size: usize,
    /// Progress/changed-event coalescing interval. Default 100 ms.
    pub progress_min_interval: Duration,
}

/// Phase-1 output: the ordered session set before any full-file read.
#[derive(Clone, Debug)]
pub struct SetPlan {
    pub items: Vec<PlannedItem>,          // final session order (§6.3)
    pub skipped: Vec<SkippedPath>,        // with reasons; shown by E08 on demand
    pub truncated: bool,                  // max_set_size hit
}

#[derive(Clone, Debug)]
pub struct PlannedItem {
    pub path: PathBuf,                    // canonical, absolute
    pub filename: String,                 // NFC-normalized (E01 convention)
    /// `Ok(probe)` or the probe error string (item still enters the set —
    /// explicit files and malformed known-extension files are *visible
    /// failures*, per the T18 cataloguing convention).
    pub probe: Result<AssetProbe, String>,
    pub source_kind: Option<SourceKind>,  // derived; None = unsupported
    pub explicit: bool,                   // named directly vs walk-discovered
}

#[derive(Clone, Debug)]
pub struct SkippedPath { pub path: PathBuf, pub reason: SkipReason }

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SkipReason {
    UnsupportedExtension,      // walk-discovered only; explicit files always enter
    Hidden,                    // dot-name below a walked root (E01 rule)
    DuplicatePath,             // same canonical path appeared earlier in the request
    DuplicateContent {         // same hash as an earlier item (load phase, §6.4)
        first_index: usize,
    },
    NotFound,                  // vanished between gesture and enumeration
    NonUtf8Path,               // catalog requires UTF-8 (E01 §4.3 / OQ-3)
    WalkError(String),         // unreadable subtree etc.
    SetSizeCap,                // truncated by max_set_size
}

/// Phase-1: enumerate + probe + order. Fast (no full-file reads); honors
/// `cancel` between files; emits throttled progress.
pub fn plan_open(
    req: &OpenRequest,
    opts: &OpenOptions,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(PlanProgress),
) -> Result<SetPlan, OpenError>;

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PlanProgress { pub enumerated: u64, pub probed: u64, pub current: PathBuf }

/// Phase-2: hash + register each planned item, **in set order** (item 0 —
/// the loupe file — completes first). One small WAL txn per item so the
/// first image is never queued behind a batch. Content duplicates collapse
/// (§6.4). Never aborts on per-item failure.
pub fn load_working_set(
    writer: &WriterHandle,
    plan: &SetPlan,
    opts: &OpenOptions,
    cancel: &CancelToken,
    on_event: &mut dyn FnMut(LoadEvent),
) -> Result<OpenReport, OpenError>;

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum LoadEvent {
    ItemReady   { index: usize, asset: AssetId, image: ImageId, reused: bool, relocated: bool },
    ItemFailed  { index: usize, reason: String },
    ItemCollapsed { index: usize, first_index: usize },   // DuplicateContent
    Progress    { done: u64, total: u64 },                // throttled
}

/// What an open did (broadcast in `Event::WorkingSetLoadFinished`; printed
/// by `lightbox-cli open`).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct OpenReport {
    pub planned: usize,
    pub ready: usize,
    pub reused: usize,        // resolved to pre-existing asset rows (reopen)
    pub relocated: usize,     // hash-hit at a new path (file moved)
    pub failed: usize,
    pub collapsed: usize,     // duplicate-content items
    pub truncated: bool,
    pub took: Duration,
}

/// Errors that abort an open outright (per-item problems never do).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    #[error("empty open request")]
    EmptyRequest,
    #[error("catalog: {0}")]
    Catalog(#[from] lightbox_catalog::CatalogError),
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
```

Reuse, not rebuild: folder expansion calls the existing `discover_files(dir, recursive, cancel)` verbatim (hidden-skip, `sort_by_file_name`, cancel checkpoints, error folding all inherited); `KNOWN_EXTENSIONS` stays the single supported-extension const (E02 grows it).

### 4.4 `lightbox-catalog` — open registration (`src/dao.rs` additions)

```rust
/// One opened-in-place file heading into `ensure_open_asset`. Mirrors
/// `NewAsset` minus folder/import-session (the v2.0 open path writes
/// neither) plus the absolute-path hint.
#[derive(Clone, Debug)]
pub struct OpenedFile {
    pub abs_path: String,              // canonical absolute path, UTF-8
    pub filename: String,              // NFC-normalized
    pub content_hash: ContentHash,
    pub format: String,                // 'CR3',…,'JPEG','UNSUPPORTED'
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    /// UTC-normalized when the probe carried an offset; verbatim otherwise
    /// (closes the E01 Phase-5 deviation "UTC-normalizing at the ingest
    /// seam is E04 cleanup").
    pub capture_time: Option<String>,
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub bytes: u64,
    pub mtime_utc: Option<String>,
    pub decode_error: Option<String>,  // probe failed → registered + badged (T18 convention)
}

/// What `ensure_open_asset` resolved.
#[derive(Copy, Clone, Debug)]
pub struct EnsureOutcome {
    pub asset: AssetId,
    pub image: ImageId,        // the default (non-virtual) image row
    pub created: bool,         // false = reopened existing identity
    pub relocated: bool,       // hash hit but abs_path differed → hint refreshed
}

impl CatalogTxn<'_> {
    /// Content-hash-keyed upsert for the open-in-place path (§3.1.1):
    ///
    /// 1. `SELECT id, abs_path FROM asset WHERE content_hash = ?1
    ///     ORDER BY id LIMIT 1`
    /// 2. **Hit** → refresh the path-hint columns (`abs_path`, `filename`,
    ///    `mtime_utc`, `missing = 0`); when the incoming probe succeeded
    ///    (`decode_error IS NULL`) also refresh the probe columns
    ///    (format/camera/capture/dims/orientation) and clear a stale
    ///    `decode_error` — a previously-corrupt file may have been fixed.
    /// 3. **Miss** → `INSERT` the asset with `folder_id NULL`,
    ///    `import_session_id NULL`, `abs_path` set.
    /// 4. Clear stale claimants: `UPDATE asset SET abs_path = NULL
    ///     WHERE abs_path = ?1 AND id != ?2` (the path now belongs to
    ///    this content).
    /// 5. Ensure the default image row (`is_virtual = 0`), creating it
    ///    with `process_version = 1` if absent.
    ///
    /// Never touches `folder` / `library_root` / `import_session`.
    pub fn ensure_open_asset(&mut self, f: &OpenedFile) -> Result<EnsureOutcome>;
}
```

Reader changes (`src/reader.rs`):

- `asset_abs_path(id)` — prefer the `abs_path` column when non-NULL; else the legacy `root.path ⊕ folder.rel_path ⊕ filename` composition (LEFT JOINs so NULL-folder rows resolve). The E01 `AssetLocator`/preview/render paths pick this up with **zero** changes.
- `ImageDetail.folder: FolderId` → **`Option<FolderId>`** (NULL for open-in-place rows). Verified blast radius on `main`: no consumer outside `lightbox-catalog` reads `.folder` (grep-checked); this is the one frozen-DTO change E04 makes, recorded per E01's joint-review rule (§11 R6).
- `images_page` — unchanged (NULL `folder_id` rows simply never match a folder filter; the v2.0 shell drives the filmstrip from the working-set snapshot, not from pages).

### 4.5 `lightbox-core` — model, command, events (`src/working_set.rs`)

```rust
/// Monotone per-session working-set generation. Bumped on every
/// `OpenWorkingSet`; events and snapshots carry it so the shell (and the
/// loader job itself) can discard stale updates.
pub type SetEpoch = u64;

/// Immutable snapshot of the session working set (plain data — no locks,
/// no catalog handles). The shell reads one per frame via
/// `Session::working_set()`; internally an `Arc` swap, so this is cheap.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WorkingSetSnapshot {
    pub epoch: SetEpoch,
    pub phase: SetPhase,
    pub items: Vec<WorkingSetItem>,       // session order
    pub skipped: Vec<SkippedPath>,        // re-exported from lightbox-ingest
    pub truncated: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SetPhase { Empty, Planning, Loading, Ready }

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WorkingSetItem {
    pub path: PathBuf,
    pub filename: String,
    pub state: ItemState,
    pub source_kind: Option<SourceKind>,  // E08 panel gating (§2.4)
    pub format: String,                   // catalog tag, for badges
    pub width: u32,
    pub height: u32,
    pub capture_time: Option<String>,
    pub decode_error: Option<String>,     // badged, not hidden
    pub explicit: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ItemState {
    /// Planned; hash/registration pending (placeholder in the filmstrip).
    Planned,
    /// Registered; previews/render/edits may be requested for `image`.
    Ready { asset: AssetId, image: ImageId },
    /// Hash or registration failed; the item stays visible with a badge.
    Failed,
    /// Collapsed into an earlier identical-content item.
    DuplicateOf { index: usize },
}

impl Session {
    /// Current working-set snapshot (cheap Arc clone).
    pub fn working_set(&self) -> Arc<WorkingSetSnapshot>;
}
```

`Command` additions (`src/command.rs`, `#[non_exhaustive]` enum — additive):

```rust
pub enum Command {
    /// v2.0 entry point (§2.4): replace the working set with the files
    /// this request resolves to. Cancels any in-flight open. Progress and
    /// completion arrive as `WorkingSet*` events tagged with the new epoch.
    OpenWorkingSet { request: OpenRequest },
    // … existing variants unchanged; ImportAddInPlace/UndoImport are
    // retired-dormant (compiled, tested, unreachable from the shell).
}
```

`Event` additions (`src/event.rs`, `#[non_exhaustive]` — additive):

```rust
pub enum Event {
    /// A new open gesture was accepted; the previous epoch is cancelled
    /// and the model shows `SetPhase::Planning` for `epoch`.
    WorkingSetOpening  { epoch: SetEpoch, ticket: CommandTicket },
    /// Phase-1 complete: the ordered set is known; filmstrip can render.
    WorkingSetReplaced { epoch: SetEpoch, planned: usize, truncated: bool },
    /// Item states changed (coalesced ≤ 1 per `progress_min_interval`);
    /// poll `Session::working_set()` for the new snapshot.
    WorkingSetChanged  { epoch: SetEpoch },
    /// Phase-2 complete (also emitted when a load is cancelled by
    /// replacement — the report covers what landed).
    WorkingSetLoadFinished { epoch: SetEpoch, report: OpenReport },
    // … existing variants unchanged.
}
```

Dispatch (`src/session.rs`): `OpenWorkingSet` follows the `spawn_import` pattern exactly — a `Class::Foreground` `spawn_blocking` job named `"working_set.open"` under a **per-epoch child** of the session cancel token, guarded by the in-flight counter so `Session::close` drains it. The dispatcher keeps the current epoch's `CancelToken`; a new `OpenWorkingSet` cancels it *before* bumping the epoch, so at most one load runs. Stale-epoch callbacks are dropped at the model boundary (compare-and-ignore), never surfaced.

Config (`src/config.rs`, `#[non_exhaustive]` — additive):

```rust
pub struct CoreConfig {
    // … existing fields …
    /// Working-set loader knobs (max_set_size, progress interval).
    pub working_set: OpenOptions,
}
```

Default store location (small, coordinated with E08 which owns the UX):

```rust
/// The default edit-store directory for a library-less app: a per-user
/// application-data location, created on first use.
///   macOS   ~/Library/Application Support/Lightbox/edits.lbdata
///   Windows %APPDATA%\Lightbox\edits.lbdata
///   Linux   $XDG_DATA_HOME (or ~/.local/share)/lightbox/edits.lbdata
/// Overridable everywhere the old `--catalog` flag was accepted; the store
/// remains plumbing, never a user-facing library (§3.1.1).
pub fn default_store_dir() -> PathBuf;
```

### 4.6 `lightbox-cli` — headless harness

```
lightbox-cli open <PATH>... [--recursive] [--store <dir>.lbdata] [--json]
```

- Opens (or creates) the store — `--store` default: `default_store_dir()`.
- Builds `OpenRequest { paths, recursive, origin: Cli }`, submits `Command::OpenWorkingSet`, drains events until `WorkingSetLoadFinished`.
- Prints the ordered set (index, state, `AssetId`/`ImageId`, content hash, format, `SourceKind`, capture time) — one line per item; `--json` emits one JSON object per line plus a final report object.
- Exit codes follow the E01 convention: `0` all items Ready (or DuplicateOf), `1` any item Failed, `2` usage, `3` store corrupt/refused.
- `import` remains for the E01 fault/E2E harness, labeled *(legacy, testing only)* in `--help`.

---

## 5. Data-model changes (SQL)

### 5.1 Migration `000N_open_in_place.sql`

The number is **reserved in `docs/plan/migrations.md` at implementation start** (registry rule; E03/E09 reserve theirs independently — the lint enforces contiguity at ship time). Content, per architecture §3.1.1 (“`folder_id` nullable, path stored on asset”):

```sql
-- 000N_open_in_place — v2.0 working-set intake (architecture §3.1.1):
-- asset carries its absolute path directly; folder becomes keep-dormant.
-- Table rebuild per the documented SQLite ALTER procedure; the runner
-- wraps this file with PRAGMA foreign_keys=OFF / foreign_key_check /
-- foreign_keys=ON (see §5.2) because image.asset_id REFERENCES asset(id)
-- ON DELETE CASCADE and a naive DROP would fire the implicit delete.

CREATE TABLE asset_new (
  id                INTEGER PRIMARY KEY,
  folder_id         INTEGER REFERENCES folder(id),   -- was NOT NULL; NULL = opened in place
  abs_path          TEXT,                            -- open-in-place path HINT (identity is content_hash);
                                                     -- NULL for legacy managed rows
  filename          TEXT    NOT NULL,
  content_hash      BLOB    NOT NULL,
  format            TEXT    NOT NULL,
  camera_make       TEXT,
  camera_model      TEXT,
  capture_time      TEXT,
  width             INTEGER NOT NULL DEFAULT 0,
  height            INTEGER NOT NULL DEFAULT 0,
  orientation       INTEGER NOT NULL DEFAULT 1,
  bytes             INTEGER NOT NULL,
  mtime_utc         TEXT,
  missing           INTEGER NOT NULL DEFAULT 0,
  decode_error      TEXT,
  import_session_id INTEGER REFERENCES import_session(id),
  added_at          TEXT    NOT NULL,
  camera            TEXT    GENERATED ALWAYS AS
                      (trim(coalesce(camera_make,'') || ' ' || coalesce(camera_model,''))) VIRTUAL
  -- UNIQUE (folder_id, filename) is intentionally DROPPED: it guarded the
  -- managed tree; open-in-place rows have NULL folder_id (vacuous under
  -- SQLite NULL-distinct semantics) and identity is content_hash.
);

INSERT INTO asset_new (id, folder_id, abs_path, filename, content_hash, format,
                       camera_make, camera_model, capture_time, width, height,
                       orientation, bytes, mtime_utc, missing, decode_error,
                       import_session_id, added_at)
  SELECT id, folder_id, NULL, filename, content_hash, format,
         camera_make, camera_model, capture_time, width, height,
         orientation, bytes, mtime_utc, missing, decode_error,
         import_session_id, added_at
  FROM asset;

DROP TABLE asset;                     -- FTS triggers on asset drop with it
ALTER TABLE asset_new RENAME TO asset;

-- Recreate the 0001 indexes (ids preserved, so image.asset_id is intact):
CREATE INDEX asset_hash           ON asset(content_hash);   -- still NON-unique (§3.3 rationale)
CREATE INDEX asset_capture        ON asset(capture_time);
CREATE INDEX asset_session        ON asset(import_session_id);
CREATE INDEX asset_added          ON asset(added_at);
CREATE INDEX asset_filename       ON asset(filename);
CREATE INDEX asset_folder_capture ON asset(folder_id, capture_time);
CREATE INDEX asset_folder_added   ON asset(folder_id, added_at);
CREATE INDEX asset_abs_path       ON asset(abs_path);       -- stale-claimant clearing (§4.4 step 4)

-- Recreate the FTS external-content triggers verbatim from 0001
-- (asset_fts_ai / asset_fts_ad / asset_fts_au), then rebuild the index —
-- external-content FTS references the content table by NAME, so the
-- rename leaves it pointed correctly, but a rebuild re-proves integrity:
-- … (triggers verbatim) …
INSERT INTO assets_fts(assets_fts) VALUES('rebuild');
```

Rollback story: forward-only, as established — the runner's **pre-upgrade copy** (`backups/pre-upgrade-<ver>/catalog.sqlite`, already built in `migrate.rs`) is the restore path, and reverting the E04 feature drops no data (open-in-place rows are inert under the old binary… which will, however, refuse the newer `schema_version` — the standard `SchemaTooNew` guard; restoring the pre-upgrade copy is the documented downgrade).

### 5.2 Migration-runner extension (`migrate.rs`)

`Migration` gains a flag; the runner implements the documented SQLite table-rebuild procedure:

```rust
pub(crate) struct Migration {
    pub number: u32,
    pub name: &'static str,
    pub sql: &'static str,
    /// True for migrations that rebuild a table with inbound foreign keys:
    /// the runner sets `PRAGMA foreign_keys = OFF` *outside* the migration
    /// transaction (the pragma is a no-op inside one), runs the batch,
    /// asserts `PRAGMA foreign_key_check` returns ZERO rows before commit,
    /// and restores `foreign_keys = ON` after. Without this, `DROP TABLE
    /// asset` performs an implicit DELETE that CASCADE-deletes every
    /// `image` row — the failure mode this flag exists to make impossible.
    pub rebuilds_tables: bool,
}
```

Hard tests (§9.2) prove: `image` rows survive byte-for-byte; `foreign_key_check` is clean; a `kill -9` mid-migration leaves either the old schema or the new one, never a hybrid (single-txn batch); `foreign_keys` is back ON for the session connections afterwards (it is set per-connection at open, `catalog.rs:210`, so this is belt-and-braces asserted anyway).

### 5.3 Keep-dormant guarantees (write-path audit)

After E04, the only tables the **open path** writes are `asset` and `image` (plus `schema_version` at migration time). `folder`, `library_root`, `import_session` are written **only** by the retired-dormant import path (tests) — asserted by an integration test that runs a full `open` against a fresh store and checks `counts()` shows `roots == 0`, `folders == 0`, `import_sessions == 0`.

---

## 6. Intake semantics (the normative rules)

### 6.1 Canonicalization & classification

- Every request path is `std::fs::canonicalize`d first (E01 precedent — symlinks resolve; on Windows this yields `\\?\`-prefixed paths, same as the built import path; display prettification is E08's concern).
- Missing path → `SkipReason::NotFound` (per-path, never aborts the request; an all-skipped request still publishes an empty set with the skip list — E08 shows why).
- Non-UTF-8 path: **explicit** file → planned item that fails at registration with a visible reason; **walk-discovered** → `SkipReason::NonUtf8Path` (matches the E01 per-file-error convention and the catalog's UTF-8 requirement).
- `is_dir` → folder unit; else file unit. Deduplicate canonical paths within one request (`DuplicatePath`, first occurrence wins).

### 6.2 Explicit files vs walks

- **Explicit file** (named directly in `paths`): always enters the set — no extension filter, hidden names honored (user intent is explicit). Probe classifies content: `Unsupported` → registered as `'UNSUPPORTED'`, badged, no develop surface; `Malformed` → registered with `decode_error`, badged (the T18 convention: visible failure, never a crash, never silent).
- **Walk-discovered file** (inside a dropped/selected folder): extension-filtered by `KNOWN_EXTENSIONS` (case-insensitive), hidden entries skipped — `discover_files` as built. Known-extension-but-malformed files enter the set badged (same as explicit); unknown extensions are skipped silently in the plan but counted (`UnsupportedExtension`) so E08 can say "312 files opened, 40 non-photo files skipped".

### 6.3 Ordering (deterministic, cross-platform)

The session order is the concatenation, **in gesture order**, of each request unit's expansion:

1. An explicit file contributes itself at its gesture position.
2. A folder contributes its expansion **sorted by (capture-time key, filename, path)** — §2.4's "capture-time then name". Recursive walks sort the whole expansion as one group (no per-directory grouping).
3. The capture-time key: parse the probe's RFC3339 `capture_time`; offset-bearing values normalize to UTC; naive values are compared as-if-UTC (documented approximation — cameras without offset metadata sort in local-time order, which is the photographer's expectation anyway); absent/unparseable → sorts **after** all timed items (then by filename). Ties break by NFC filename bytewise, then full path — a total, platform-independent order (property-tested).
4. `max_set_size` truncates after ordering (`truncated = true`, remainder counted as `SetSizeCap`).

### 6.4 Content-duplicate collapse (load phase)

If an item's hash equals an earlier item's hash in the same epoch, it becomes `ItemState::DuplicateOf { index }` and lands in the report as `collapsed` (one photo, one filmstrip cell, one recipe — two cells resolving to the same `ImageId` would mirror edits confusingly). Case-insensitive-filesystem aliases that survive canonicalization are caught here too.

### 6.5 Registration & capture-time normalization

Per item: `hash_file` (cancel-aware, 1 MiB chunks) → build `OpenedFile` (capture time UTC-normalized when the probe carried an offset — closing the E01 Phase-5 deviation at this seam) → `ensure_open_asset` in **one small WAL txn per item**. Rationale for item-granular txns over the import path's 64-per-batch: the first item's readiness gates first pixels (batching would queue it behind 63 siblings); measured E01 single-row txns are ~0.2 ms, so even the 10k cap costs ~2 s of txn overhead spread across a hash-bound loop. (A batch escape hatch is noted as a cut-line optimization if profiling ever disagrees — T12.)

### 6.6 Replacement, cancellation, close

- A new `OpenWorkingSet` cancels the in-flight epoch's job at its next checkpoint (between files in plan; between chunks/items in load). Rows already committed by the cancelled epoch remain — they are inert edit-store plumbing, exactly what a later reopen wants warm.
- `Session::close` drains the open job via the existing in-flight guard (built `InFlight` mechanism — no change).
- `kill -9` at any instant: WAL guarantees the store is clean; the session set is forgotten (not persisted, by design). Fault-injected in §9.4.

---

## 7. Performance budgets (v2.0 — session scale, not DAM scale)

| Scenario | Budget | Mechanism |
|---|---|---|
| Single raw dropped → item 0 `Ready` | **< 50 ms p95** (dev baseline; logged probe, formal capture in lbx-perf) | hash ~10–30 ms for 25–45 MB on NVMe + ~1 ms probe + ~0.2 ms txn; item 0 loads first by construction |
| …→ first pixels in loupe/filmstrip | < 100 ms preview tier (M1 exit) | E03/E08 own the pixel path; E04's contribution is the `ImageId` above |
| 1,000-file folder drop → ordered filmstrip (phase 1) | **< 1.5 s** (dev baseline) | probe is ~0.1–1.3 ms/file warm, structures only; walk is `walkdir`-bound |
| 1,000-file folder → all items `Ready` | disk-bound (~10–30 s cold NVMe), **UI never blocks**, progress visible | hashing streams on a Foreground job thread; events coalesced ≤ 10 Hz |
| Reopen of an already-registered set | plan-bound (hash still runs — identity must be verified) | `reused` fast path skips inserts; no probe-column churn |
| UI thread | **zero** loader work on it, ever | everything runs in `spawn_blocking(Class::Foreground)`; the shell reads snapshots |

These are logged/nightly numbers in the E01 style (hard-asserting wall-clock in PR CI flakes); the lbx-perf scenario runner gains an `open-1k` scenario (T11).

---

## 8. Ordered task breakdown (each ≤ 1 day)

**Phase A — vocabulary & store**

- **T1 — `SourceKind` + derivation.** Add `SourceKind` to `lightbox-types`; `ProbedFormat::source_kind()` in `lightbox-decode`; unit tests over every `ProbedFormat` variant incl. `Unsupported → None`. Record the additive frozen-surface change.
  *AC:* type serde-round-trips; all seven fixture raw mounts map `Raw`, JPEG/TIFF/PNG map `Rendered`; no existing test changes.
- **T2 — migration-runner `rebuilds_tables`.** Extend `Migration` + `apply_pending` with the FK-off / `foreign_key_check` / FK-on procedure of §5.2; tests with a synthetic rebuild migration.
  *AC:* child rows with `ON DELETE CASCADE` survive a parent-table rebuild byte-for-byte; `foreign_key_check` failure aborts the txn (schema unchanged, version not recorded); pragma state is ON afterwards; existing migration tests green.
- **T3 — migration `000N_open_in_place`.** Reserve the number in `docs/plan/migrations.md`; ship the §5.1 SQL (rebuild + indexes + triggers verbatim + FTS rebuild); upgrade test against a populated 0001-era store (built by running the legacy import in-test).
  *AC:* post-upgrade `integrity_check` + `foreign_key_check` clean; all asset/image ids and row contents preserved; FTS search still hits; pre-upgrade copy written; `lint-migrations` green; `kill -9` mid-upgrade leaves old-or-new schema, never hybrid (fault-harness leg).
- **T4 — `ensure_open_asset` + reader updates.** DAO per §4.4 (steps 1–5); `asset_abs_path` prefers `abs_path` (LEFT-JOIN legacy fallback); `ImageDetail.folder: Option<FolderId>`.
  *AC:* DAO matrix passes — fresh insert / reopen-reuse / relocated (hint refreshed, `missing` cleared) / same-path-new-content (new row + stale claimant's hint cleared) / probe-failure registration (`decode_error` set) / previously-UNSUPPORTED refreshed on healthy re-probe / legacy row without `abs_path` still resolves via folder join; `EmbeddedPreviewProvider` integration test still green (locator path unchanged).

**Phase B — the loader (`lightbox-ingest::working_set`)**

- **T5 — intake & enumeration.** `OpenRequest`/`OpenOptions`/`SkipReason`; canonicalize/classify/dedupe; folder expansion via `discover_files`; explicit-vs-walk semantics (§6.1–6.2); `max_set_size` cap.
  *AC:* unit tests for every `SkipReason`; explicit hidden/odd-extension file enters the set; walk filters and counts; empty request → `OpenError::EmptyRequest`; cancellation stops enumeration at a checkpoint.
- **T6 — probe & order (`plan_open`).** Probe loop with cancel + throttled progress; §6.3 ordering key incl. UTC normalization of offset-bearing capture times; `SetPlan`.
  *AC:* fixture-corpus plan is byte-identically ordered on macOS/Windows/Linux CI; property test — ordering is a total order, permutation-invariant for folder groups, gesture-order-preserving for explicit files; missing-capture-time items sort after timed ones; malformed known-extension file becomes a planned item with `probe: Err`.
- **T7 — `load_working_set`.** Hash→register loop in set order, one txn per item; `LoadEvent` stream; content-dup collapse; `OpenReport`; cancellation mid-load.
  *AC:* item 0 `Ready` before any other item's hash begins (asserted via event order); duplicate-content pair collapses with `first_index` correct; per-item failure never aborts the loop; cancelled load emits a truthful partial report; re-running the same load is idempotent (zero new rows, all `reused`).

**Phase C — core façade & CLI**

- **T8 — working-set model + command + events.** `WorkingSetModel` (epoch, Arc-swap snapshot, stale-epoch guard), `Session::working_set()`, `Command::OpenWorkingSet` dispatch as `"working_set.open"` Foreground job with per-epoch cancel, `Event::WorkingSet*`, `CoreConfig.working_set`, final `CatalogChanged::bulk(false)` compat hint.
  *AC:* submit → `Opening` → `Replaced` → coalesced `Changed` → `LoadFinished`, all epoch-tagged; a second submit mid-load cancels the first (its `LoadFinished` reports partial; no stale-epoch snapshot mutation — asserted with an event-order test); `close()` drains an in-flight open; snapshot is lock-free plain data.
- **T9 — core integration & fault injection.** End-to-end at seam 1: open mixed fixtures (raws + JPEG + corrupt + unsupported + duplicate copies) → states/badges correct; move a file on disk between two opens → same `ImageId`, `relocated`; edit a file in place → new identity; keep-dormant write audit (§5.3); `kill -9` during a 500-file load (fault-harness leg) → `integrity_check` clean, reopen re-registers with `reused`.
  *AC:* all of the above as `lightbox-core` integration tests; fault leg wired into the existing nightly harness.
- **T10 — `default_store_dir()` + CLI `open`.** Store-location helper (three platforms, `dirs` or hand-roll — record choice); CLI subcommand per §4.6 with `--json`; E2E test over the fixture corpus; deprecation label on `import`.
  *AC:* `lightbox-cli open fixtures/ --recursive --json` on a fresh default store exits 0 with a correct ordered manifest; second run shows all `reused`; corrupt fixture → exit 1 with the item marked Failed; `check` still exits 3 on a corrupt store; CLI dependency-tree gate (no UI crates) still green.

**Phase D — proof & polish**

- **T11 — perf scenario + docs.** `lbx-perf` gains `open-1k` (plan time, first-item-ready, all-ready; budgets of §7 as logged baselines, budget-first regression policy per E01 Phase-8 convention); module docs; start `E04-deviations.md` in the E01 style.
  *AC:* nightly job runs `open-1k` on the 3-OS matrix; numbers recorded in `baselines.json`; spec budgets referenced from the runner.
- **T12 — cut-line (drop first on overrun).** Parallel probing across the blocking pool (plan phase only; re-sort after, determinism preserved) and/or `WorkingSetModel::prioritize(range)` viewport hint for hash order; per-item→small-batch txn escape hatch if lbx-perf disagrees with §6.5.
  *AC (if built):* 1k-folder plan time improves ≥ 2× on an 8-core machine with identical output order.

Sum ≈ 10–11.5 engineer-days ≈ **2.2–2.5 pw** — inside the S ~2–3 pw envelope with T12 as slack.

---

## 9. Test plan

1. **Unit (`lightbox-ingest`, `lightbox-decode`, `lightbox-types`):** intake classification and every `SkipReason`; explicit-vs-walk matrix; ordering key (UTC normalization, naive-time comparison, absent-time placement, NFC tie-break); `SourceKind` mapping; `OpenReport` accounting.
2. **Property tests:** session ordering is a deterministic total order (shuffle-invariant for folder groups; gesture-order-stable for files); plan→load→plan idempotence (same paths ⇒ same order, same identities); `ContentHash` reuse across path renames.
3. **DAO tests (`lightbox-catalog`):** the `ensure_open_asset` matrix (T4 AC list); migration upgrade on a populated legacy store incl. FTS and keyset-page behavior post-rebuild; runner FK procedure (T2 AC list); `EXPLAIN QUERY PLAN` re-checked for the recreated indexes (no full scans on the existing sort orders).
4. **Fault injection:** `kill -9` mid-migration and mid-load (extends the existing nightly 1000-iteration harness with an `open` leg); post-kill `integrity_check` clean; reopen behavior (`reused`) verified in the harness parent.
5. **Integration (seam 1, headless):** the T9 suite; event-order and epoch-guard tests; close-drains-open; keep-dormant write audit (`roots/folders/import_sessions` untouched by the open path).
6. **CLI E2E:** `create → open → open(again) → check → backup` round-trip against the pinned fixture corpus on all three CI OSes; exit-code contract; JSON manifest golden-checked (order + states, hashes from the fixture pins).
7. **Perf (nightly, non-blocking):** `open-1k` scenario per §7; budget-first regression policy (files an issue, doesn't block PRs).
8. **License/CI gates:** unchanged and must stay green — cargo-deny (only possible new crate: `dirs`), native-inventory lint (no new native deps), REUSE/SPDX on new files.

---

## 10. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **The asset-table rebuild migration** silently CASCADE-deletes `image` rows (FK implicit-delete on `DROP TABLE`) or leaves FTS stale | The `rebuilds_tables` runner procedure (§5.2) with `foreign_key_check` as a commit gate; trigger recreation verbatim + FTS `'rebuild'`; upgrade test on a populated store; fault-injection leg; pre-upgrade copy is the restore path |
| R2 | **Hash-on-open latency** makes big drops feel slow (identity requires the full-file xxh3) | Two-phase pipeline (filmstrip renders after plan); item-0-first load; progress events; `max_set_size` cap; T12 prioritize/parallel options. Not negotiable away: content-hash identity is the §3.1.1 contract |
| R3 | **Capture-time ordering** differs across cameras (offset vs naive timestamps) or platforms | Normative key in §6.3, cross-platform determinism asserted in CI; naive-as-UTC approximation documented in code and snapshot |
| R4 | **Same-path-new-content / duplicate-content edge cases** confuse identity (stale hints, mirrored edits) | Explicit DAO steps 2–4 (§4.4) + the collapse rule (§6.4), each with a dedicated test; behavior documented in the snapshot so E08 can badge honestly |
| R5 | **Frozen-surface drift**: `ImageDetail.folder → Option`, `Command`/`Event` growth | All additive or grep-verified zero-consumer changes; recorded in `E04-deviations.md` and flagged for the E01 joint-review convention (spec §8.4 there); `#[non_exhaustive]` enums absorb the rest |
| R6 | **Epoch races** (stale load mutating a replaced set; events out of order) | Single dispatcher owns epoch + cancel token; model rejects stale-epoch writes; event-order integration test (T8 AC) |
| R7 | **Windows canonical paths** (`\\?\` prefix) leak into UX or break comparisons | Matches the built E01 import behavior (paths already stored canonicalized); comparisons are canonical-to-canonical; display prettification is E08's; note kept in module docs |
| R8 | **E08 not landed** when E04 completes (no UI to drive it) | CLI `open` is the M1 smoke path by design; the T9 integration suite exercises the full seam headlessly |

### Open questions

- **OQ-1 — append gesture.** §2.4 pins *replace* semantics. If product later wants modifier-drop-to-append, the loader already takes arbitrary path lists; only the model's replace rule changes. Owner: E08/product, post-M1. No E04 work.
- **OQ-2 — folder sort direction.** Ascending capture-time assumed (LR convention). Confirm with E08 UX before the filmstrip ships; a flip is a one-line comparator change + re-golden.
- **OQ-3 — extension list growth timing.** HEIC/HEIF (and later JXL/AVIF originals) join `KNOWN_EXTENSIONS` when E02 can decode them — listing them earlier would fill folder-drops with badged un-openable items. E02's spec must name the handoff.
- **OQ-4 — `max_set_size` default.** 10,000 chosen (10× the mandate's 1,000-folder bar, well under pathological home-dir drops). Revisit with lbx-perf data.
- **OQ-5 — default store naming/location.** `edits.lbdata` under the platform app-data dir (§4.5). Final naming/packaging call sits with E08 (UX) + E16 (packaging); the helper isolates the decision to one function.
- **OQ-6 — UNSUPPORTED rows accumulating.** Explicitly-opened junk registers rows (visible-failure convention). Harmless plumbing; if it ever bothers, a store-hygiene sweep belongs to E16's repair/restore flow, not E04.

---

## 11. Definition of done

1. **The §2.4 contract is real, headlessly:** on all three CI OSes, `lightbox-cli open` over the fixture corpus — single file, multi-file, flat folder, recursive folder, mixed files+folders — produces the normatively-ordered session set with correct states, `SourceKind`s, badges, and skip accounting; exit codes per contract.
2. **Open-in-place semantics proven:** reopen restores identity (`reused`); moving a file preserves identity (`relocated`, hint refreshed); editing a file in place mints a new identity and clears the stale claimant; duplicate content collapses. Each is a green test.
3. **No managed-import residue on the open path:** the keep-dormant write audit passes (`folder`/`library_root`/`import_session` rows untouched); no copy/rename/second-copy code exists in the new module; the retired import path remains compiled with its tests (no deletion, per §10.0).
4. **Migration shipped safely:** `000N_open_in_place` registered, applied via the `rebuilds_tables` procedure; populated-store upgrade test + FK/`integrity_check`/FTS assertions + fault-injection leg green; pre-upgrade copy verified restorable.
5. **Core façade complete:** `Command::OpenWorkingSet`, `Event::WorkingSet*`, `Session::working_set()` snapshot, replace/cancel/close semantics all integration-tested; UI thread provably untouched (loader runs entirely on the job runtime).
6. **Budgets recorded:** `open-1k` scenario in lbx-perf with baselines committed; single-file first-item latency logged under the §7 target on the dev baseline; no PR-gate timing asserts (nightly, budget-first).
7. **Gates green:** cargo-deny / REUSE / native-inventory / clippy `-D warnings` / fmt / golden subset / fault-injection PR gate — all unchanged and passing; any new dep (`dirs`) allowlist-clean.
8. **Record kept:** `E04-deviations.md` started (E01 style) with the frozen-surface notes (R5) and any divergences; module docs name the seams (E08 intake, E03 consume-only, E09 identity, E02 format growth) exactly as §2.3 states them.
