> **SUPERSEDED (v2.0, 2026-07-05)** — replaced by [E04-working-set-loader.md](E04-working-set-loader.md); retained as historical record.

# E04 — Ingest & Import Pipeline

_Implementation spec. Milestone **M1**. Effort **M ~3–5 pw** (cut-line named in §8). Depends on **E01** (workspace, catalog schema v1, headless core, job-runtime seed), **E03** (preview pyramid & raw cache API). Planner: staff-engineer, per `01-architecture.md` (decision-complete; nothing here re-litigates stack, seams, or data-model ownership)._

**Architecture anchors:** §2.1/§2.2 (`lightbox-ingest` = "import pipeline · probe · checksum copy · 2nd-copy · preview build [enqueue]"), §3.1 (`asset`/`image`/`import_session` ownership, batched-txn crash invariant), §5.3 (job classes, backpressure: "import throttles preview-build enqueue so a 10k-raw card can't OOM the queue"), §6 (disk-full, corrupt-raw, missing-file rows), §7 (import 10k budget), §9 M1 exit ("import 10 k raws non-blocking (< 60 s browsable)").

---

## 1. Summary

E04 turns E01's walking-skeleton add-in-place import into the full front door of the product: a staged, parallel, cancellable, crash-resumable import pipeline with Copy / Move / Add-in-place modes, checksummed copy + verified second-copy backup, skip-duplicates, token-based renaming, date-based destination trees, apply-on-import (develop preset / metadata preset / keywords), per-session audit + undo-import, import presets, a watched auto-import folder, and a minimal import dialog in the shell. Its performance contract is the M1 exit criterion: **a 10k-raw card is browsable in the grid in under 60 seconds with zero UI-thread impact**, achieved by embedded-preview-first ingestion (T0 enqueue to E03) and streaming, batched catalog writes.

**The epic's first failing test (written before any pipeline code):** integration test `import_copy_fixture_card` — importing the 12-file fixture card in Copy mode with a date destination and rename template produces 12 `asset` + 12 `image` + 12 `import_item` rows, files exist at the templated destinations with `content_hash` matching a re-read, and the session report says `12 imported / 0 failed`.

---

## 2. Scope & non-goals

### 2.1 In scope (feature-catalog priority in brackets)

| Item | Priority | Notes |
|---|---|---|
| Import modes: Copy / Move / Add-in-place | M | Move forbidden from removable volumes (data-loss guard, matches LR) |
| Copy-as-DNG mode | M (dialog mode) / S (DNG write) | **Cut-line tasks T30–T31** — the pipeline stage slot ships regardless; see §8 |
| Broad format ingest: raw (rawler-parseable set), JPEG, TIFF, PNG, HEIC, PSD(probe-only), video (catalog-only) | M | Probe/enumerate/catalog; pixel decode is E02/E03 territory |
| Skip-suspected-duplicates (heuristic + exact content-hash) | M | Whole-catalog scope |
| Token-based file renaming templates (shared engine) | M | New leaf crate `lightbox-naming`, reused by E15 export & E07 batch rename |
| Destination panel model: date schemes / flat / preserve-hierarchy, collision policy, dry-run tree preview | M | |
| Apply-on-import: develop preset, metadata preset, keyword list | M | Develop-preset application behind an E09-implemented trait (seam §4) |
| Preview-build choice at import (embedded / standard / 1:1) | M(embedded)/S | E04 only *enqueues* to E03 with backpressure |
| Parallel import pipeline + checksummed second-copy backup | S (arch-mandated) | §5.3/§6 make the parallel + verified-copy shape non-optional |
| Import session audit + undo-import | M | `import_item` table (new, §7) |
| Import presets; card-insert detection **event**; watched auto-import folder | S | Auto-*launch* UI on card insert is E08's (consumes our event) |
| Crash-safe, resumable sessions; disk-full / corrupt-file / vanished-source handling | arch | §6 failure taxonomy rows made concrete |
| Minimal import dialog in `lightbox-shell` + full headless CLI path | M | Dialog is deliberately minimal; E08 may later swap its grid internals |

### 2.2 Explicit non-goals (named seam or deferral for each)

- **Import dialog polish / virtualized 100k thumbnail grid widget** — E08 owns the shell widget kit; our dialog uses a plain egui scroll grid sized for card-scale (≤~5k visible entries) and names the swap point.
- **Preview building itself** (T0/T1/T2 render, pyramid storage, raw cache) — E03. E04 calls `PreviewBuilder::enqueue` only.
- **Job scheduler internals** (priority classes, activity-center model, pause/resume plumbing) — E06. E04 consumes the `lightbox-jobs` API (`Class::Foreground`, `CancelToken`); until E06 lands, E01's tokio runtime seed backs the same signatures.
- **Folder-tree fs-watch reconciliation & folders panel** — E07. E04 creates `library_root`/`folder` rows transactionally at import time; ongoing reconciliation is E07's watcher. (Our watched *auto-import hot-folder* is a different feature and is ours.)
- **Keyword hierarchy semantics, metadata viewer/editor, metadata presets authoring UI** — E07. E04 writes flat keywords through `KeywordDao::ensure_path` and applies a stored metadata preset doc; authoring/managing those presets is E07/E08.
- **XMP RDF parsing, `crs:` mapping, develop presets** — E09. E04 detects sidecars, stores raw bytes, and calls E09 hooks (§4). No RDF code in this epic.
- **Import from Lightroom catalog (.lrcat), import-from-another-catalog** — E16.
- **Tethered capture, publish services, cloud anything** — mandate non-goals / E15+.
- **Perceptual-hash (pHash) duplicate-detection view** — Should, post-M1 (E07/E16 territory); E04's dedupe is exact-hash + heuristic only.
- **Video posters/scrubbing/playback** — video assets are catalogued with probe metadata only; thumbnails/playback are E03/E08/v1.x.
- **Eject-after-import** — stretch inside T25; drops silently if platform cost exceeds ½ day.

---

## 3. Crates & modules touched

| Crate | Role in E04 | New/modified |
|---|---|---|
| `lightbox-ingest` | **Primary.** Pipeline stages, plan builder, copy executor, dedupe, watched folder, session/resume logic | New (E01 scaffolds the crate name; E04 owns its contents) |
| `lightbox-naming` | Token-template engine (parse/expand filename + folder templates) | **New leaf crate** — justified: shared by E04 import, E07 batch rename, E15 export; depending on `lightbox-ingest` from export would invert the dependency flow |
| `lightbox-catalog` | Migration `+ import_item / import_preset / name_template / watched_folder`, session-state columns, dedupe index; DAOs for the above | Modified |
| `lightbox-core` | `ImportCommands` / `ImportQueries` façade on the command bus; removable-volume + import events on the core event stream | Modified |
| `lightbox-decode` | `probe()` completeness for ingest (metadata-only raw parse via rawler, container probes for TIFF/PNG/HEIC/PSD); new `probe_video()` (FFmpeg, dyn-link) | Modified (E01 owns the seed `probe`; E02 owns pixel decode — we extend metadata only) |
| `lightbox-meta` | `extract_exif()` (kamadak-exif) → `MetadataBundle` for `metadata_cache`; sidecar detection | Modified (read-side only; XMP RDF stays E09) |
| `lightbox-preview` | Consumed via `PreviewBuilder` trait (E03-owned) | Consumed |
| `lightbox-jobs` | Consumed: `spawn(Class::Foreground, …)`, `CancelToken` | Consumed |
| `lightbox-shell` | Minimal import dialog (3 tasks) | Modified |
| `lightbox-cli` | `lightbox-cli import …` headless path (drives §8 E2E per architecture §8 testing row) | Modified |

**License notes for CI surfaces (§8 of the architecture):** xxHash via `xxhash-rust` (BSL-free, MIT-licensed crate) — surface 1. FFmpeg probe (LGPL-2.1, **dynamic link, LGPL-only build**) and libheif+libde265 (LGPL-3, dynamic, **libde265 backend asserted**) enter the surface-2 SBOM the moment T04/T03 land — E04 registers both artifacts in the SBOM manifest as part of those tasks. No bundled data assets → surface 3 untouched.

---

## 4. Seams to neighboring epics (consumed/provided contracts)

| Seam | Direction | Contract |
|---|---|---|
| **E03 preview pyramid** | E04 → E03 | `trait PreviewBuilder { fn enqueue(&self, req: PreviewBuildRequest) -> Result<(), Backpressure>; }` — bounded; `Err(Backpressure)` throttles the import metadata stage (§5.3 of the architecture). E04 requests `T0` (embedded) for every asset immediately, `T1`/`T2` per `PreviewBuildPolicy`. |
| **E06 jobs** | E04 → E06 | Import session = one `Class::Foreground` job; per-file copy fan-out uses the job runtime's worker pool; `CancelToken` checked at every stage boundary and every 8 MiB of copy I/O. Progress reported via `ImportEvent` stream that E06's activity model (and E08's activity center) subscribes to. |
| **E09 edit-state** | E04 → E09 | `trait DevelopPresetApplier { fn apply(&self, image: ImageId, preset: PresetRef, tx: &mut WriterTx) -> Result<()>; }` and `trait SidecarIngestor { fn ingest(&self, asset: AssetId, xmp_bytes: &[u8], tx: &mut WriterTx) -> Result<SidecarOutcome>; }`. Until E09 lands, a no-op impl stores sidecar bytes in `import_item.sidecar_blob_ref` (content-addressed file next to previews) for E09 to backfill; develop-preset option is disabled in UI/CLI. |
| **E07 DAM** | shared | `KeywordDao::ensure_path(&mut tx, path: &str) -> KeywordId` — E04 lands the minimal version (flat + `a|b` hierarchy split); E07 extends semantics without changing the signature. Folder rows created by import are E07-watcher-compatible (same `folder` table invariants). |
| **E08 shell** | E04 → E08 | Core event `VolumeArrived { volume_uuid, label, dcim: bool }`; E08 decides whether to auto-open the import dialog. The dialog's file grid is a plain widget behind `mod import_grid` — E08's virtualized grid can replace its internals without touching dialog logic. |
| **E15 export** | provided | `lightbox-naming` is the shared token engine; E15 consumes the same `NameTemplate` type and token set. |
| **E16 interop** | provided | `import_session`/`import_item` audit rows are the anchor for `.lrcat` migration reporting; `SidecarIngestor` is the same hook the migration importer drives. |
| **E02 decode** | none (by design) | E04 uses rawler **metadata-only** parsing in `probe()`; no demosaic/decode dependency, consistent with M0's "no raw decode" stance. |

---

## 5. Design

### 5.1 Pipeline shape

A staged async pipeline on the job runtime; stages connected by **bounded channels** (default depth 256 entries / 64 MiB in-flight, tunable), each stage cancellable, the whole session one Foreground job:

```
enumerate ─► probe ─► plan-resolve ─► copy (N parallel workers, hash-while-copy)
                                        ├─► second-copy (M workers, re-read verify)
                                        ▼
                                catalog-commit (single writer, batched txns of 64)
                                        ▼
                                metadata stage (EXIF/IPTC → metadata_cache; sidecar hook;
                                                apply-on-import; T0/T1 preview enqueue)
```

- **Streaming, not phased:** assets become visible in the grid as their batch commits — browsability does not wait for the session to finish. `enumerate → first grid row` target: < 2 s on a 10k card.
- **Copy parallelism:** default `min(4, physical_cores/2)` copy workers (card readers saturate at 2–4 streams; tunable, benchmarked in T11). Hash (xxh3-128) is computed **during** the copy read — no second read of the source.
- **Catalog-commit** is the only stage that touches the writer handle; batches of 64 items per transaction (§7 scale-check mitigation), each batch writing `folder`/`asset`/`image`/`import_item` rows together. A batch is the unit of crash atomicity and of resume idempotency.
- **Add-in-place** skips copy/second-copy; **Move** = Copy + verified-delete of source (§5.4).

### 5.2 Crash safety & resume

- Copies land as `dest/.lb-part-<session>-<n>` then atomic-rename to the final name; the rename happens **before** the batch's catalog txn commits. Orphan `.lb-part-*` files are GC'd on session resume/startup.
- `import_session.state ∈ {planned, running, interrupted, completed, cancelled, failed}`. On catalog open, sessions still `running` flip to `interrupted`; core surfaces a resume/discard choice.
- **Resume** rebuilds the plan and skips every source path with an `import_item` outcome of `imported`/`duplicate`; already-renamed-but-uncommitted files are detected by (dest exists ∧ hash matches source) and adopted rather than re-copied.
- `kill -9` at any point leaves: catalog `integrity_check`-clean (E01's WAL invariant), no half-written final-named files (temp-then-rename), at most one batch of copied-but-uncatalogued files (adopted on resume). Fault-injection tested (§9).

### 5.3 Duplicate detection

Two tiers, both scoped to the whole catalog:

1. **Pre-copy heuristic** (cheap, before any I/O-heavy work): `(filename, bytes, capture_time)` probe against the `idx_asset_dedupe` index → `SuspectedDuplicate`.
2. **Exact** (post-hash, authoritative): `content_hash` equality → `ExactDuplicate`.

With `skip_duplicates=on`: suspected duplicates are marked skip-by-default in the plan (user can re-check per file in the dialog); exact duplicates discovered at copy time are always skipped and recorded as `outcome='duplicate'` with the existing `asset_id`. Off: everything imports (LR semantics). Verdicts are recorded per item for the session report.

### 5.4 Move mode & data-loss guards

- Move = Copy pipeline + delete-source **only after** the batch's catalog txn commits and the destination hash equals the source hash computed during copy.
- Move is **refused from removable volumes** (plan preflight error, matches LR's card guard); Add-in-place from removable warns (asset will be `missing` on eject) but is allowed.
- Source deletion uses OS trash where the source volume supports it, permanent delete otherwise (explicit in the plan preview).

### 5.5 Naming & destination

`lightbox-naming` grammar (shared with E15/E07): literal text + `{token}` with optional args/padding —
`{orig}`, `{orig_lower}`, `{seq:4}` (session-scoped), `{import:4}` (catalog-global import counter), `{date:YYYY-MM-DD}` (capture date, EXIF-first, mtime fallback, per-token format), `{camera}`, `{model}`, `{ext}`, `{custom}` (dialog-supplied text). Unknown token → parse error at template-save time, never at run time. Expansion is pure (`TokenCtx` in, `String` out) and property-tested (§9).

Destination resolution: `Organize::{ByDate(scheme), Flat, PreserveHierarchy{source_base}}`; date schemes cover LR's common forms (`YYYY/YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY/MMM DD`, single flat date). Collision policy `{RenameUnique (suffix -1..-n), Skip, Fail}` — "ask" is a dialog-level loop over a re-planned remainder, not a pipeline state. Resolved paths are guaranteed under the destination root (path-traversal property test; templates cannot contain separators except via `Organize`).

### 5.6 Watched auto-import folder

`notify`-based watcher (Background job) on user-designated hot folders. A file is ingested when **quiesced** (size stable across 2 s and openable exclusively — bridges third-party tethering tools that write incrementally). Each quiesced batch spawns a normal Foreground import session tagged `source='watched:<path>'` with the folder's stored `ImportOptions` (always Move-or-Copy out of the hot folder; Add-in-place is disallowed to keep the hot folder empty). Failures quarantine the file into `<hot>/.lightbox-failed/` rather than looping.

### 5.7 Failure semantics (per architecture §6)

| Failure | Behavior |
|---|---|
| Corrupt/unsupported-but-recognized raw | Import anyway, `asset.decode_error=1`, grid badge later; `outcome='imported'` with warning (architecture: "still catalogued, app never crashes") |
| Unreadable file (I/O error, permission) | `outcome='failed'` + error string; session continues; summarized in report |
| Disk full (either destination) | Preflight estimates total bytes ×1.02 + preview headroom and refuses to start; mid-run `ENOSPC` pauses the session (state `interrupted`, resumable after space freed) |
| Source vanishes (card ejected mid-copy) | In-flight items fail, session pauses to `interrupted`; resume re-validates source presence |
| Second-copy verify mismatch | Retry once; then `outcome='imported'` with `second_copy_verified=0` warning (primary import is not blocked by backup failure — but the report shouts) |
| Checksum-of-source changed between plan and copy (file modified) | Re-probe, re-hash, proceed with new hash; note in report |

---

## 6. Interface definitions

Signatures are the contract; field lists may grow additively.

```rust
// ───────────────────────── lightbox-ingest ─────────────────────────

pub struct IngestService {
    catalog: Arc<Catalog>,                    // E01
    previews: Arc<dyn PreviewBuilder>,        // E03 seam
    jobs: JobsHandle,                         // E06 seam (E01 runtime seed until then)
    hooks: IngestHooks,                       // E09/E07 seams, defaultable
}

pub struct IngestHooks {
    pub develop_preset: Arc<dyn DevelopPresetApplier>,   // default: Disabled
    pub sidecar: Arc<dyn SidecarIngestor>,               // default: StoreRawBytes
}

impl IngestService {
    /// Enumerate + probe + resolve into a dry-run plan. Streaming; cheap enough for
    /// live dialog preview (re-runs on option change, memoizing probes).
    pub async fn plan(&self, source: ImportSource, opts: ImportOptions,
                      cancel: CancelToken) -> Result<ImportPlan, IngestError>;

    /// Execute a plan as one Foreground job. Returns immediately with a handle.
    pub fn run(&self, plan: ImportPlan) -> ImportHandle;

    pub fn resume(&self, session: ImportSessionId) -> Result<ImportHandle, IngestError>;
    pub async fn undo_import(&self, session: ImportSessionId, files: UndoFiles)
        -> Result<UndoReport, IngestError>;
}

pub enum ImportSource {
    Folder { path: PathBuf, recursive: bool },
    Files(Vec<PathBuf>),
    Volume { volume_uuid: String },            // card: enumerates DCIM-first
}

pub enum ImportMode {
    AddInPlace,
    Copy { dest: DestinationSpec },
    Move { dest: DestinationSpec },
    CopyAsDng { dest: DestinationSpec, embed_original: bool },  // cut-line, §8
}

pub struct ImportOptions {
    pub mode: ImportMode,
    pub rename: Option<NameTemplate>,          // None = keep original names
    pub second_copy: Option<PathBuf>,          // verified, structure-preserving, unrenamed
    pub skip_duplicates: bool,
    pub include_video: bool,
    pub apply: ApplyOnImport,
    pub previews: PreviewBuildPolicy,          // Embedded | Standard | OneToOne (E03 tiers)
}

pub struct ApplyOnImport {
    pub develop_preset: Option<PresetRef>,     // E09 seam
    pub metadata_preset: Option<MetadataPresetDoc>, // stored doc; authoring is E07
    pub keywords: Vec<String>,                 // flat or `a|b` paths
}

pub struct DestinationSpec {
    pub root: LibraryRootId,
    pub base_subfolder: Option<String>,
    pub organize: Organize,
    pub collision: CollisionPolicy,
}
pub enum Organize { ByDate(DateScheme), Flat, PreserveHierarchy { source_base: PathBuf } }
pub enum CollisionPolicy { RenameUnique, Skip, Fail }

pub struct ImportPlan {
    pub session: ImportSessionId,              // row created in state 'planned'
    pub options: ImportOptions,
    pub items: Vec<PlannedItem>,
    pub preflight: PreflightReport,            // free space, guards, warnings
    pub dest_tree_preview: Vec<PlannedFolder>, // dialog destination panel
}
pub struct PlannedItem {
    pub source: SourceEntry,
    pub include: bool,                         // user-checkable; duplicates default false
    pub action: PlannedAction,                 // Import | SkipDuplicate(AssetId) | Excluded(reason)
    pub dest_rel_path: Option<RelPath>,
    pub duplicate: Option<DuplicateVerdict>,   // Suspected{asset_id} | Exact{asset_id}
}

pub struct ImportHandle {
    pub session: ImportSessionId,
    pub events: broadcast::Receiver<ImportEvent>,
    pub cancel: CancelToken,
}

#[non_exhaustive]
pub enum ImportEvent {
    Started { total_files: u32, total_bytes: u64 },
    FileCopied { item: ImportItemId, bytes: u64 },
    FileImported { item: ImportItemId, asset: AssetId, image: ImageId },
    FileSkipped { item: ImportItemId, verdict: DuplicateVerdict },
    FileFailed { item: ImportItemId, error: String },
    BatchCommitted { assets: u32 },            // grid can refresh on this
    Progress { copied_bytes: u64, done: u32, failed: u32, skipped: u32 },
    Paused { reason: PauseReason },            // DiskFull | SourceGone | User
    Finished(ImportReport),
}

pub struct ImportReport {
    pub imported: u32, pub duplicates: u32, pub failed: u32, pub warnings: Vec<ImportWarning>,
    pub elapsed: Duration, pub bytes_copied: u64, pub second_copy_verified: bool,
}

// enumeration (T02)
pub fn enumerate(source: &ImportSource, filter: &FormatFilter, cancel: &CancelToken)
    -> impl Stream<Item = Result<SourceEntry, IngestError>>;
pub struct SourceEntry {
    pub path: PathBuf, pub bytes: u64, pub mtime: SystemTime,
    pub kind: MediaKind,                       // Raw | Jpeg | Tiff | Png | Heic | Psd | Video | Sidecar
    pub sidecar: Option<PathBuf>,              // paired .xmp discovered during enumeration
}

// watched folders (T24)
pub struct WatchService { /* notify-backed */ }
impl WatchService {
    pub fn set(&self, folder: WatchedFolderConfig) -> Result<()>;
    pub fn remove(&self, id: WatchedFolderId) -> Result<()>;
    pub fn list(&self) -> Vec<WatchedFolderConfig>;
}

// ───────────────────────── lightbox-naming ─────────────────────────

pub struct NameTemplate { /* parsed AST; Display renders canonical text */ }
impl NameTemplate {
    pub fn parse(text: &str) -> Result<NameTemplate, TemplateError>;   // save-time validation
    pub fn expand(&self, ctx: &TokenCtx) -> String;                    // pure, infallible
    pub fn tokens(&self) -> &[Token];
}
pub struct TokenCtx<'a> {
    pub original_stem: &'a str, pub ext: &'a str,
    pub capture: Option<DateTime<Utc>>, pub mtime: DateTime<Utc>,
    pub camera_make: Option<&'a str>, pub camera_model: Option<&'a str>,
    pub seq: u32, pub import_number: u32, pub custom: &'a str,
}

// ───────────────────────── lightbox-decode (additions) ─────────────────────────

/// Metadata-only probe — NO pixel decode (keeps E04 independent of E02).
pub fn probe(path: &Path) -> Result<AssetProbe, ProbeError>;           // E01 seed, extended T03
pub fn probe_video(path: &Path) -> Result<VideoProbe, ProbeError>;     // T04, FFmpeg dyn-link
pub struct VideoProbe { pub width: u32, pub height: u32, pub duration_ms: u64,
                        pub codec: String, pub container: String,
                        pub capture_time: Option<DateTime<Utc>> }

// ───────────────────────── lightbox-meta (additions) ─────────────────────────

pub fn extract_exif(path: &Path, probe: &AssetProbe) -> Result<MetadataBundle, MetaError>;
pub struct MetadataBundle { /* exif/iptc columns for metadata_cache + orientation + json blob */ }

// ───────────────────────── lightbox-core façade ─────────────────────────

impl ImportCommands {
    pub fn import_plan(&self, src: ImportSource, opts: ImportOptions) -> CmdFuture<ImportPlan>;
    pub fn import_run(&self, plan: ImportPlan) -> CmdFuture<ImportSessionId>;
    pub fn import_pause(&self, s: ImportSessionId) -> CmdFuture<()>;
    pub fn import_resume(&self, s: ImportSessionId) -> CmdFuture<()>;
    pub fn import_cancel(&self, s: ImportSessionId) -> CmdFuture<()>;
    pub fn import_undo(&self, s: ImportSessionId, files: UndoFiles) -> CmdFuture<UndoReport>;
    pub fn import_preset_save(&self, name: &str, opts: &ImportOptions) -> CmdFuture<ImportPresetId>;
}
impl ImportQueries {
    pub fn sessions(&self, filter: SessionFilter) -> Vec<ImportSessionRow>;
    pub fn session_report(&self, s: ImportSessionId) -> Option<ImportReport>;
    pub fn import_presets(&self) -> Vec<ImportPresetRow>;
    pub fn interrupted_sessions(&self) -> Vec<ImportSessionRow>;       // startup resume prompt
}
```

CLI (drives the §9 E2E):

```
lightbox-cli import --source <path|volume:UUID> --mode add|copy|move \
  [--dest-root <id> --organize date:YYYY/YYYY-MM-DD|flat|hierarchy] \
  [--rename '{date:YYYYMMDD}-{seq:4}'] [--second-copy <path>] \
  [--skip-duplicates] [--keywords a,b|c] [--metadata-preset <file>] \
  [--previews embedded|standard|1:1] [--include-video] [--dry-run] [--json]
```

---

## 7. Data model & migrations

Schema v1 (E01) already carries `library_root`, `folder`, `asset`, `image`, `import_session`, `metadata_cache`, `preview`, `keyword*`. E04 ships **migration `0004_ingest`** (number per E01's migration ledger at land time):

```sql
-- Session lifecycle + report (additive to E01's import_session: id, ts, source, options)
ALTER TABLE import_session ADD COLUMN state TEXT NOT NULL DEFAULT 'completed'
    CHECK (state IN ('planned','running','interrupted','completed','cancelled','failed'));
ALTER TABLE import_session ADD COLUMN finished_at INTEGER;
ALTER TABLE import_session ADD COLUMN report_doc TEXT;          -- JSON ImportReport
ALTER TABLE import_session ADD COLUMN import_number INTEGER;    -- catalog-global {import} token

-- Per-file audit: the undo-import + resume + report substrate
CREATE TABLE import_item (
    id            INTEGER PRIMARY KEY,
    session_id    INTEGER NOT NULL REFERENCES import_session(id) ON DELETE CASCADE,
    source_path   TEXT    NOT NULL,
    dest_rel_path TEXT,
    asset_id      INTEGER REFERENCES asset(id) ON DELETE SET NULL,
    outcome       TEXT    NOT NULL DEFAULT 'planned'
                  CHECK (outcome IN ('planned','imported','duplicate','skipped','failed')),
    dup_asset_id  INTEGER REFERENCES asset(id) ON DELETE SET NULL,
    error         TEXT,
    bytes         INTEGER,
    content_hash  BLOB,                                          -- xxh3-128, 16 bytes
    second_copy_verified INTEGER NOT NULL DEFAULT 0,
    sidecar_blob_ref TEXT,                                       -- raw XMP bytes for E09 backfill
    finished_at   INTEGER,
    UNIQUE (session_id, source_path)                             -- resume idempotency key
);
CREATE INDEX idx_import_item_session ON import_item(session_id, outcome);

-- Saved import option sets (dialog "Import Preset")
CREATE TABLE import_preset (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    doc TEXT NOT NULL,                                           -- versioned JSON of ImportOptions
    doc_version INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

-- Shared token templates (consumed by E15/E07 too)
CREATE TABLE name_template (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, template TEXT NOT NULL
);

-- Hot folders
CREATE TABLE watched_folder (
    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1,
    options_doc TEXT NOT NULL,                                   -- ImportOptions JSON
    last_event_at INTEGER
);

-- Dedupe heuristic probe (§5.3)
CREATE INDEX idx_asset_dedupe ON asset(filename, bytes, capture_time);
-- Exact-duplicate probe; NON-unique: identical content may legitimately exist twice
CREATE INDEX idx_asset_content_hash ON asset(content_hash);
```

Ownership notes: `import_item` is audit/runtime state — never a second owner of asset facts (the `asset` row is authoritative once written). `sidecar_blob_ref` points into the content-addressed `.lbdata` store (same `<hh>/<hash>` convention as §3.3), not a BLOB in the catalog, keeping the DB lean at 10k-item sessions.

---

## 8. Ordered task breakdown

Every task ≤ 1 day, lands with its tests. **Cut-line:** if the epic trends past 5 pw, drop T30–T31 (Copy-as-DNG) first, then the eject stretch in T25 — both are cleanly severable; nothing downstream in M1 depends on them.

### Phase A — plumbing & pure logic (parallelizable, no catalog writes)

| # | Task | Acceptance criteria |
|---|---|---|
| T01 | Scaffold `lightbox-ingest` (+ `lightbox-naming` skeleton): error taxonomy (`IngestError` with stable variants for the §5.7 table), `ImportOptions`/`ImportMode`/`DestinationSpec` types, workspace + cargo-deny wiring; `lightbox-cli import --dry-run` stub printing a plan skeleton | Compiles on all 3 CI targets; `--dry-run` on a folder lists candidate files as `Excluded(NotYetImplemented)`; error enum round-trips through `anyhow`/serde |
| T02 | Source enumeration: recursive scan stream with format filter, hidden/symlink policy (skip symlinked dirs, follow files), `.xmp` sidecar pairing, DCIM-first ordering for `Volume` sources | Enumerates a 10k-file fixture tree in < 2 s streaming (first item < 50 ms); sidecars paired case-insensitively (`IMG_1.CR3`+`IMG_1.xmp`); cancellation aborts mid-walk cleanly |
| T03 | Extend `lightbox-decode::probe` for ingest: rawler **metadata-only** parse (camera make/model, capture time, dims, embedded-preview ref), container probes for TIFF/PNG/HEIC/PSD; register libheif/libde265 in the surface-2 SBOM manifest | Probe corpus test: every file in the fixture corpus (≥1 per supported family) yields correct dims/camera/capture-time vs committed expectations; corrupt-file fixture returns `ProbeError`, never panics; no pixel decode symbol reachable (link-time check on feature flags) |
| T04 | `probe_video` via FFmpeg (dyn-link, LGPL-only build): container/codec/dims/duration/creation-time for MP4/MOV/AVI; SBOM surface-2 entry + build-flag assertion hook | MP4/MOV fixtures probe correctly; absent/failed FFmpeg degrades to `MediaKind::Video` with `VideoProbe` unavailable (import still catalogs the file); no static FFmpeg symbol in the binary (CI check) |
| T05 | Hashing: streaming xxh3-128 util + fused hash-while-copy reader; criterion bench | Hash of fixture files matches reference vectors; fused copy+hash overhead < 10% vs plain copy at NVMe speed (bench committed, nightly) |
| T06 | `lightbox-naming`: token grammar (§5.5), parse-time validation, pure `expand`, proptest suite | All §5.5 tokens implemented; property tests: parse(render(ast))==ast, expansion never emits path separators or empty names, unknown token rejected at parse; grammar documented in crate docs |
| T07 | Destination resolver: `Organize` schemes, collision policies, dry-run `PlannedFolder` tree derivation | Unit matrix over (scheme × collision × existing-tree) fixtures; property test: resolved path always strictly under destination root (traversal-safe, incl. `..` and absolute-path injection in templates/custom text); NFC-normalized comparison on macOS fixture names |
| T08 | Duplicate detection: heuristic index probe + exact-hash verdicts, `DuplicateVerdict` model, catalog read API | Unit: heuristic hits on (name,size,time) triples; exact hit on content_hash; suspected-but-not-exact downgraded correctly after hash; 10k-probe batch < 200 ms against a 100k-asset fixture catalog |

### Phase B — pipeline & persistence

| # | Task | Acceptance criteria |
|---|---|---|
| T09 | Migration `0004_ingest` (§7) + DAOs (`ImportSessionDao`, `ImportItemDao`, `ImportPresetDao`, `NameTemplateDao`, `WatchedFolderDao`) | Migration up/down clean on a v1-schema fixture catalog; DAO round-trip unit tests; `PRAGMA foreign_key_check` clean; migration guarded by `schema_version` |
| T10 | Plan builder + preflight: assemble `ImportPlan` from enumerate+probe+dedupe+destination; free-space estimate (both destinations), permission probe, removable-Move guard, warnings | `plan()` on the 10k fixture < 5 s cold (probe-bound), deterministic across runs; Move-from-card refused with typed error; disk-full preflight refuses with required-vs-available bytes; plan serializes to JSON for `--dry-run --json` |
| T11 | Copy executor: N parallel workers, temp-then-rename atomic, hash-while-copy, fsync policy (file + parent dir per batch), per-file retry(1), throughput bench | Fault test: kill mid-copy leaves only `.lb-part-*` temp files, no final-named partials; sustained throughput ≥ 80% of `cp -r` baseline on the bench rig with hashing on; cancel token honored within 8 MiB |
| T12 | Second-copy writer: structure-preserving unrenamed duplicate + **re-read checksum verification**, retry-once, `second_copy_verified` recording | Fixture import with second-copy: every byte-identical, verified flag set; injected bit-flip (test hook) detected → retried → warning surfaced in report; second-copy failure never blocks primary import |
| T13 | Catalog-commit stage: batched txns (64/batch) writing `folder`/`asset`/`image`/`import_item` (+ `library_root` resolution incl. volume_uuid for add-in-place outside existing roots); resume idempotency on `(session_id, source_path)` | Fault-injection: `kill -9` between/within batches → `integrity_check` clean, re-run resume imports exactly the remainder (no dupes, adopted already-renamed files); batch commit emits `BatchCommitted`; 10k-item commit adds < 10 s total DB time |
| T14 | Metadata stage: `lightbox-meta::extract_exif` → `metadata_cache` (+orientation on `image`); sidecar bytes stored content-addressed + `SidecarIngestor` hook invoked; capture-time fallback chain (EXIF → video probe → mtime) | Corpus assertions for EXIF fields incl. capture-time TZ handling; sidecar bytes stored + hook called once per asset; missing-EXIF file falls back to mtime; malformed EXIF never fails the import (warning only) |
| T15 | Apply-on-import: keywords via `KeywordDao::ensure_path` (minimal impl landed here), metadata-preset doc application to `metadata_cache`/XMP-pending state, develop preset behind `DevelopPresetApplier` (no-op default) | Keywords `a, b|c` create hierarchy rows + `keyword_asset` links in the same batch txn; metadata preset stamps copyright/creator columns; with no E09 impl registered, develop-preset option is rejected at plan time with a typed "unavailable" error |
| T16 | Preview enqueue: `PreviewBuilder` calls — T0 for every asset at `BatchCommitted`, T1/T2 per policy; `Err(Backpressure)` throttles the metadata stage (bounded, §5.3) | Integration with E03 stub: 10k enqueues never exceed the bounded queue (assert peak depth); backpressure stalls import stage, not the UI; T0 requests precede T1 for the same batch |
| T17 | Pipeline orchestration: wire stages with bounded channels as one Foreground job; `ImportEvent` broadcast; pause/cancel checkpoints at every stage boundary; progress aggregation | E2E on fixture card (the §1 first failing test) goes green; cancel mid-session → state `cancelled`, no temp litter, partial imports remain valid; pause/resume mid-copy loses no file; events arrive in causal order per item |
| T18 | Move mode + add-in-place unification: verified-delete-after-commit (trash where supported), removable guards, add-in-place root/folder adoption | Move fixture: sources gone only for `outcome='imported'` items, byte-verify precedes delete; simulated verify-failure leaves source intact + warning; add-in-place outside known roots creates a `library_root` with correct volume_uuid |
| T19 | Failure semantics end-to-end (§5.7 table): corrupt-raw catalogued with `decode_error`, unreadable→failed-continue, mid-run ENOSPC→`interrupted` (resumable), source-vanish→pause | One integration test per §5.7 row; report aggregates counts + warnings; no failure mode panics or poisons the writer |
| T20 | Crash-resume UX surface: startup detection of `running`→`interrupted` sessions, `resume()`/discard, `.lb-part-*` GC, adopted-file detection | Fault-injection matrix (kill during copy / between batches / during metadata stage) × resume → final catalog identical to uninterrupted run (row-level diff); discard cleans temps and marks session `cancelled` |
| T21 | Undo-import: remove session's assets (catalog rows cascade; copied files → OS trash optional; add-in-place never touches files), guarded against assets referenced by later edits | Undo after fixture import restores pre-import row counts; copied files in trash; add-in-place undo leaves files untouched; images with post-import edit history require `force` flag |

### Phase C — surfaces & integration

| # | Task | Acceptance criteria |
|---|---|---|
| T22 | `lightbox-core` `ImportCommands`/`ImportQueries` façade + full CLI wiring + headless E2E (import → query → undo) in CI | CLI imports the fixture card copy-mode with rename/date/second-copy flags; `--json` report is schema-stable; E2E is PR-blocking per architecture §8 |
| T23 | Import presets: save/load/list `ImportOptions` docs with `doc_version` migration shim; last-used options auto-persisted | Preset round-trip property test; unknown future fields preserved (forward-compat); CLI `--preset <name>` works |
| T24 | Watched auto-import folder: notify watcher, quiesce detection (2 s size-stable + exclusive-open), per-folder options, quarantine on failure, Background→Foreground session spawn | Drop 50 files incrementally (simulated slow writer) → exactly 50 imported once quiesced, none partial; failure file lands in `.lightbox-failed/`; disable stops the watcher within 1 s |
| T25 | Removable-volume arrival events: platform watcher (IOKit disk arbitration / WM_DEVICECHANGE / udev) → core `VolumeArrived{dcim}` event; **stretch:** eject-after-import | Simulated/loopback mount fires the event with correct `dcim` flag on macOS + Windows CI runners; no polling (event-driven); E08 consumption documented |
| T26 | Import dialog 1/3: source picker (volumes + folder browser) + file grid with per-file checkboxes, thumbnails from probe embedded-preview refs (decoded via E03's T0 path when present, gray placeholder otherwise) | Dialog opens against a 2k-file card in < 500 ms with progressive thumbnails; check/uncheck-all + per-file toggle mutate the plan; zero UI-thread stalls > 16 ms during population (frame-time probe) |
| T27 | Import dialog 2/3: options panels (mode, destination tree preview from `dest_tree_preview`, rename template editor with live example, apply-on-import pickers, second-copy, skip-duplicates, preview policy) + import-preset save/load | Every `ImportOptions` field editable; destination tree preview updates live on option change (re-plan < 300 ms memoized); invalid template shows parse error inline; duplicate-marked files render badged + unchecked |
| T28 | Import dialog 3/3: run → progress via activity-center model (E06 seam), pause/cancel buttons, completion report panel, undo entry point, interrupted-session resume prompt on launch | Running import shows live counts/bytes/ETA off `ImportEvent`; cancel/pause reflect within 200 ms; report panel lists failures/warnings with paths; resume prompt appears after simulated crash |
| T29 | Perf scenario `import_10k` in the nightly harness: fixture card (10k mixed raws w/ embedded JPEGs), asserts §7 budget — first grid row < 2 s, **grid browsable < 60 s** (all T0 enqueued + catalog rows committed), UI-latency probe unaffected, DB time < 10 s, peak RSS bound | Harness green on the reference rig; regressions file tracked issues per architecture §8 (nightly, non-blocking) |

### Cut-line tasks (drop first on overrun; slots already exist)

| # | Task | Acceptance criteria |
|---|---|---|
| T30 | Copy-as-DNG conversion fn: dnglab-based raw→DNG (lossless, optional embed-original), validation re-probe of output | Fixture raws convert to DNGs that rawler re-probes with identical dims/metadata; embed-original round-trips byte-identical extraction; conversion errors fall back to plain Copy with warning |
| T31 | Copy-as-DNG pipeline integration: conversion as a copy-stage variant (DNG is the catalogued asset; original kept only via second-copy), dialog/CLI wiring | E2E: CopyAsDng session catalogs `.dng` assets with correct `content_hash`(of the DNG), report notes conversions; throughput documented (conversion is CPU-bound; N workers) |

Nominal total: 31 tasks ≈ 24–28 focused days (several are half-day) → within M with the cut-line honest.

---

## 9. Test plan

Per the architecture's §8 strategy. **No golden-image tests in this epic** — E04 moves bytes and writes rows; pixel correctness lives in E03/E05. The fixture assets below are the epic's one test-data deliverable.

**Fixtures (committed under `fixtures/ingest/`, small + synthetic where possible):**
- *Fixture card:* 12 files — 3 raw families (with embedded JPEG previews), JPEG, TIFF, PNG, HEIC, PSD, MP4, one corrupt raw, one `.xmp`-paired raw, one exact-duplicate pair.
- *10k perf card:* generated (script committed, not the bytes) — synthetic raws with valid metadata + embedded previews.
- *100k-asset catalog:* generated fixture DB for dedupe-probe and scale tests.

| Layer | Tests | Gate |
|---|---|---|
| Unit | `lightbox-naming` grammar; destination resolver matrix; dedupe verdicts; plan builder; DAOs; error taxonomy mapping | PR-blocking |
| Property (`proptest`) | Template parse/render round-trip; expansion emits no separators/empties; resolved path strictly under destination root (traversal injection corpus); unicode/NFC-NFD filename handling; `ImportOptions` doc round-trip incl. unknown-field preservation | PR-blocking |
| Integration | Fixture-card E2E per mode (Add/Copy/Move[/CopyAsDng]); §5.7 failure-row tests; second-copy verify + injected corruption; undo-import; watched-folder quiesce; sidecar hook invocation; keyword/metadata apply | PR-blocking |
| Fault injection | `kill -9` matrix (mid-copy / between batches / mid-metadata) → `integrity_check` clean + resume-equivalence (row-level diff vs uninterrupted run); ENOSPC mid-run → interrupted + resumable; source-vanish | PR-blocking (extends E01's kill-9 gate to the import path) |
| Perf (nightly) | `import_10k` scenario vs §7 budget (first row < 2 s, browsable < 60 s, UI-latency probe, DB time, RSS); copy-throughput bench (≥ 80% of baseline, hash overhead < 10%); dedupe probe at 100k | Nightly; regression → tracked issue |
| Cross-platform | Full unit + fixture E2E on macOS/Windows/Linux CI; NFD filenames (macOS), long paths + case-insensitive collisions (Windows), volume-event smoke tests | PR-blocking |
| License CI | cargo-deny (surface 1) unchanged-clean; **surface-2 SBOM entries for FFmpeg (LGPL-only build-flag assert) and libheif/libde265 land with T03/T04**; no surface-3 impact | PR-blocking |
| E2E headless | `lightbox-cli import` drive in CI (fast subset PR-blocking, full nightly) per architecture §8 | PR-blocking / nightly |

---

## 10. Risks & open questions

**Risks**

| # | Risk | Mitigation |
|---|---|---|
| R1 | **FFmpeg/libheif native-dep timing** — dyn-link artifacts + SBOM build-flag assertions must exist on all 3 CI targets before T03/T04 merge | Both probes degrade gracefully (file catalogued without deep metadata); the SBOM entries are part of the task DoD, not a follow-up; if packaging slips, `MediaKind` still imports the files |
| R2 | **Move-mode data loss** — the one place this epic can destroy user data | Delete only after batch txn commit + hash verify; trash-first; removable-source refusal; dedicated fault tests (T18) |
| R3 | **Copy-as-DNG scope creep** (dnglab coverage per raw family is uneven) | Cut-line tasks T30–T31; pipeline slot ships regardless so re-adding is additive; fallback-to-Copy on conversion error |
| R4 | **Filesystem edge cases** (NFD vs NFC, case-insensitive collisions, Windows long paths, exFAT timestamps at 2 s granularity breaking the dedupe heuristic) | Property tests + platform CI in every phase-A task; dedupe heuristic tolerates ±2 s capture-time slack on FAT-family sources |
| R5 | **Backpressure mistuning** — too-deep queues OOM on 10k cards, too-shallow starves copy throughput | Bounded defaults asserted in T16/T29; depths are tunables surfaced to E08's perf prefs panel (seam) |
| R6 | **E09/E06 landing order** (develop-preset apply, activity model) | Hook traits with no-op defaults (§4); import works headless day one; the only user-visible gap is a disabled develop-preset option |
| R7 | **Probe throughput on slow cards** dominating plan time (dialog feels sluggish) | Probe streams + memoizes; dialog populates progressively (T26 acceptance); plan re-resolution reuses probes |

**Open questions (owner, needed-by)**

1. **Import presets scope** — catalog-scoped table (this spec) vs app-scoped config shared across catalogs. Proposed: catalog table + export-to-file; revisit when multi-catalog workflows (Could) firm up. _(Owner: E04 w/ E08 UX review; before T23.)_
2. **Video content-hash cost** — §3.1 says `content_hash` keys caches/relink for every asset; hashing multi-GB videos during import is pure read cost. Proposed: hash-while-copy makes it free for Copy/Move; for **Add-in-place video only**, hash lazily as a Background job with a `hash_pending` sentinel. Needs catalog sign-off since relink assumes hash presence. _(Owner: E04 → data-engineer; before T13.)_
3. **Duplicate scope UX** — skip against whole catalog (proposed, LR-consistent) vs destination-folder-only; per-session override checkbox? _(Owner: E08 UX; default shipped as whole-catalog, before T27.)_
4. **`ask` collision policy** — modeled as dialog-level re-plan loop (§5.5); confirm E08 wants the loop or a batch conflict-resolution sheet. _(Owner: E08; cosmetic to the pipeline either way.)_
5. **Second-copy layout** — LR writes an unrenamed structure-preserving copy (proposed); some ingest tools apply renaming to the backup too. Ship LR semantics, revisit on feedback. _(Owner: product; before T12.)_
6. **Hot-folder Add-in-place ban** (§5.6) — confirm with product that watched folders always evacuate (Copy/Move), matching LR's auto-import. _(Owner: product; before T24.)_

---

## 11. Definition of done

- [ ] All Phase A–C tasks (T01–T29) merged with their acceptance criteria demonstrably green in CI; cut-line tasks either merged or explicitly descoped in this file with a linked decision note.
- [ ] **M1 exit criterion owned by this epic is green on the nightly rig:** 10k-raw import — first grid row < 2 s, grid browsable < 60 s, zero UI-thread stalls attributable to import (frame-time probe), all via the real `IngestService` → `PreviewBuilder` → catalog path.
- [ ] Fault-injection matrix (kill-9 / ENOSPC / source-vanish / verify-mismatch) PR-blocking and green on all three platforms; `integrity_check` clean in every case; resume-equivalence row-diff passes.
- [ ] Headless `lightbox-cli import` E2E (import → query → undo) green PR-blocking; JSON report schema documented.
- [ ] All three license surfaces green: cargo-deny (surface 1); FFmpeg + libheif/libde265 SBOM entries with build-flag assertions (surface 2); surface 3 untouched (no bundled data added).
- [ ] Seam contracts published in crate docs: `PreviewBuilder` (E03), `DevelopPresetApplier`/`SidecarIngestor` (E09), `KeywordDao::ensure_path` (E07), `VolumeArrived` event + activity-event stream (E06/E08), `lightbox-naming` token set (E15/E07) — each with a doc-tested example.
- [ ] No UI types below `lightbox-core`; no SQL above `lightbox-catalog` (headless-boundary lint clean).
- [ ] Migration `0004_ingest` reversible and covered by the migration-guard test; schema docs in §7 match the shipped DDL.
- [ ] Import dialog demoable end-to-end on macOS + Windows: card in → plan → copy w/ second-copy → cull-ready grid; recorded as the M1 demo artifact.
