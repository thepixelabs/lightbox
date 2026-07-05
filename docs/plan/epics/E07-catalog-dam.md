> # ⛔ SUPERSEDED (v2.0) — E07 is RETIRED. Lightbox is now an editing-first raw developer with no DAM; the entire library concept (folders/collections/smart-collections/keywords/filter+FTS/relink/metadata-editor) is cut. This spec is retained as historical record only — do not implement. See `docs/plan/00-mandate.md` (v2.0) and `docs/plan/01-architecture.md` §10 (v2.0 epic table) / §3.1.1 (DAM tables keep-dormant).

# E07 — Catalog DAM core

_Implementation spec. Milestone **M1** · Effort **L ~5–8 pw** · Depends on **E01** (foundation: workspace, catalog crate, headless core, migrations, command bus)._
_Inputs: `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (§2, §3.1, §6, §7, §8, §10), `docs/research/01-…dam.md`, `docs/research/09-…performance.md`, `docs/research/00-feature-catalog.md`._

This epic delivers the library's organizational brain: folders/sync, collections & sets, smart collections (rule-AST → SQL), hierarchical keywords, the filter/query engine incl. FTS, missing-file relink, virtual-copy commands, removal semantics, and the EXIF/IPTC metadata viewer/editor backend with metadata presets and sync-metadata. It is **headless**: everything here lives below the shell boundary (§2.3 seam 1) and is exercised via `lightbox-cli` before any UI exists.

---

## 1. Scope

In scope (all Must-tier per the feature catalog unless noted):

1. **Folders (physical organization).** Folder CRUD (create/rename/move) with atomic fs+catalog updates; per-folder **synchronize** scan reconciling disk vs catalog; fs-watcher-driven debounced reconciliation; offline-volume awareness via `library_root.volume_uuid`.
2. **Collections & collection sets.** Virtual albums, nesting via sets, membership ops, **custom drag order** (fractional sort keys), quick/target collection designation (data + commands; the `B`-key UI is E08).
3. **Smart collections.** Versioned `RuleTree` AST (any/all/none groups, nested), serialized as JSON in `smart_collection.rule_tree`, compiled to **parameterized SQL** inside `lightbox-catalog` (no SQL crosses the core boundary, §2.3).
4. **Hierarchical keywords.** Parent/child tree with synonyms, per-keyword export flags, autocomplete + recent-keyword suggestions, assign/remove/merge/move, and `export_paths()` — the data source E09 maps to `dc:subject` / `lr:hierarchicalSubject`.
5. **Ratings / flags / color labels.** Batch-write DAOs and undoable core commands (tiny single-row txns per §7 culling budget); custom label sets (name↔color mapping); the keystroke grammar and badges are E08.
6. **Filter/query engine.** One `ImageQuery` façade combining source (folder/collection/smart/all), free-text (FTS5), attribute filters, and faceted metadata columns with LR column-browser semantics; saved **filter presets**; paged, sorted results for E08's virtualized grid.
7. **Missing files & relink.** `asset.missing` lifecycle, single-file relink (hash-verified), folder-tree re-point, root/volume relocation, find-all-missing scan, and auto-relink candidate proposal (`content_hash` exact → filename+size+time heuristic).
8. **Virtual copies.** `CreateVirtualCopy` / `DeleteVirtualCopy` / rename commands over the `image`-over-`asset` model (the model itself is E01 schema; E07 ships the commands + query surfaces).
9. **Removal semantics.** Remove-from-catalog vs delete-to-OS-trash, and purge-all-rejected — with the collection/keyword/FTS cleanup each implies.
10. **EXIF/IPTC metadata viewer + editor (catalog side).** `metadata_cache` schema + typed read API for the inspector; editable IPTC subset via `MetadataPatch`; **metadata presets** (CRUD + apply); **sync-metadata** across a selection with an explicit field mask. All edits write the catalog and mark assets metadata-dirty; **file/sidecar write-out is E09's write-metadata command through `lightbox-meta`** (§2.3 seam 3).

### Non-goals (named seams, not designs)

| Not in E07 | Owner |
|---|---|
| Grid/loupe/filmstrip, filter-bar UI, badges, keystroke grammar, painter tool, compare/survey | **E08** (consumes E07's query/command API) |
| Import pipeline, apply-on-import, watched folders, card ingest, duplicate-skip at import | **E04** (calls E07's `upsert_metadata()` / FTS refresh; receives folder-sync "new files" handoff) |
| Preview pyramid, raw cache, thumbnail store | **E03** (content-hash keyed; survives relink for free) |
| XMP/RDF parse & write, `crs:` mapping, read-/write-metadata file commands, divergence detection against files on disk | **E09 / `lightbox-meta`** (reads E07's dirty-set + `export_paths()`; writes back through E07 DAOs) |
| Job scheduler, activity center, pause/priority classes | **E06** (E07 exposes cancellable async fns + progress sinks; E06 wraps them in `Class::Background`) |
| Faces, semantic search, auto-tagging (they reuse the keyword tables) | **E14** |
| `.lrcat` migration (writes through E07 DAOs) | **E16** |
| Stacking, capture-time editing, GPS/map, multi-catalog merge, video-specific metadata, AI culling | deferred (Should/Could — post-M1; schema leaves room, no v1 work here) |

---

## 2. Crates & modules touched

Per the §2.1 decomposition. E07 adds modules; it creates no new crates.

```
lightbox-catalog/
  src/migrations/v2_dam.sql          ← this epic's schema delta (§4)
  src/dam/attrs.rs                   ratings/flags/labels DAO + label sets
  src/dam/collections.rs             sets, collections, membership, fractional order
  src/dam/smart.rs                   RuleTree AST, validation, versioning
  src/dam/smart_compile.rs           AST → parameterized SQL (field registry)
  src/dam/keywords.rs                tree + closure, synonyms, export flags, autocomplete
  src/dam/query.rs                   ImageQuery façade, sort/paging, facet counts
  src/dam/fts.rs                     assets_fts maintenance + rebuild
  src/dam/metadata.rs                metadata_cache DAO, MetadataPatch, presets
  src/dam/folders.rs                 folder ops, scan/diff engine, sync report apply
  src/dam/relink.rs                  missing lifecycle, relink engine, candidate search
lightbox-core/
  src/commands/dam.rs                undoable DAM commands over the E01 command bus
  src/services/fs_watch.rs           notify-based watcher → debounced reconcile
  src/queries/dam.rs                 read façade re-exports (no SQL crosses up)
lightbox-cli/
  src/cmd/dam.rs                     headless drivers for every command/query (test + E2E)
```

**New third-party crates** (all pass cargo-deny surface-1): `notify` (CC0-1.0/Artistic-2.0 dual — take CC0), `walkdir` (MIT/Unlicense), `bitflags` (MIT/Apache-2.0). No native/FFI additions → surfaces 2–3 untouched.

**Dependency note (E06):** the epic table says E07 depends only on E01. E07 therefore consumes only the *type surface* of `lightbox-jobs` (`CancelToken`, progress-sink signature) which E01's workspace skeleton stubs; scheduler semantics (priorities, pause, activity center) arrive with E06 in parallel and are integration-tested at M1, not required to build E07.

---

## 3. Data model & migrations

E07 ships migration **`v2_dam`** (E01 owns `schema_version` + the migration runner + copy-on-write upgrade policy). Assumption (seam to E01, confirm at kickoff): schema v1 contains `library_root`, `folder`, `asset`, `image`, `edit_recipe`, `edit_index`, `preview`, `import_session`, `schema_version` per §3.1. If E01's v1 already stubs any table below, `v2_dam` reduces to the delta.

```sql
-- ============ culling attributes (realizes §3.1 "label/flag/rating on image") ============
ALTER TABLE image ADD COLUMN rating INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5);
ALTER TABLE image ADD COLUMN flag   INTEGER NOT NULL DEFAULT 0 CHECK (flag IN (-1, 0, 1)); -- reject/none/pick
ALTER TABLE image ADD COLUMN label  TEXT;                       -- label NAME (xmp:Label round-trip is textual)
CREATE INDEX idx_image_rating ON image(rating);
CREATE INDEX idx_image_flag   ON image(flag);
CREATE INDEX idx_image_label  ON image(label) WHERE label IS NOT NULL;

CREATE TABLE label_set (                                        -- custom label sets (LR parity)
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, is_active INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE label_def (
  set_id INTEGER NOT NULL REFERENCES label_set(id) ON DELETE CASCADE,
  color  TEXT NOT NULL CHECK (color IN ('red','yellow','green','blue','purple')),
  name   TEXT NOT NULL,                                         -- user-definable display name, stored on image.label
  PRIMARY KEY (set_id, color)
);

-- ============ collections ============
CREATE TABLE collection_set (
  id INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES collection_set(id) ON DELETE CASCADE,
  name TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0,
  UNIQUE (parent_id, name)
);
CREATE TABLE collection (
  id INTEGER PRIMARY KEY,
  set_id INTEGER REFERENCES collection_set(id) ON DELETE SET NULL,
  name TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0,
  is_target INTEGER NOT NULL DEFAULT 0,                          -- quick/target collection designation
  created_at INTEGER NOT NULL,
  UNIQUE (set_id, name)
);
CREATE TABLE collection_item (
  collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
  image_id INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  sort_key TEXT NOT NULL,                                        -- fractional index (base-62), O(1) reorder
  added_at INTEGER NOT NULL,
  PRIMARY KEY (collection_id, image_id)
);
CREATE INDEX idx_coll_item_order ON collection_item(collection_id, sort_key);
CREATE INDEX idx_coll_item_image ON collection_item(image_id);

CREATE TABLE smart_collection (                                  -- §3.1
  id INTEGER PRIMARY KEY,
  set_id INTEGER REFERENCES collection_set(id) ON DELETE SET NULL,
  name TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0,
  rule_tree TEXT NOT NULL,                                       -- JSON RuleTree, carries its own "version" field
  UNIQUE (set_id, name)
);

-- ============ keywords (§3.1: keyword / keyword_hierarchy / keyword_asset) ============
CREATE TABLE keyword (
  id INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES keyword(id) ON DELETE CASCADE,
  name TEXT NOT NULL COLLATE NOCASE,
  include_on_export INTEGER NOT NULL DEFAULT 1,                  -- per-keyword export inclusion
  export_parents    INTEGER NOT NULL DEFAULT 1,                  -- "export containing keywords"
  export_synonyms   INTEGER NOT NULL DEFAULT 1,
  last_applied_at INTEGER,                                       -- recent-keywords suggestions
  UNIQUE (parent_id, name)
);
CREATE TABLE keyword_synonym (
  keyword_id INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE,
  synonym TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (keyword_id, synonym)
);
CREATE TABLE keyword_hierarchy (                                 -- closure table: subtree queries are one join
  ancestor_id   INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE,
  descendant_id INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE,
  depth INTEGER NOT NULL,
  PRIMARY KEY (ancestor_id, descendant_id)
);
CREATE INDEX idx_kw_closure_desc ON keyword_hierarchy(descendant_id);
CREATE TABLE keyword_asset (                                     -- keywords attach to the ASSET (§3.1; see Risk R6)
  keyword_id INTEGER NOT NULL REFERENCES keyword(id) ON DELETE CASCADE,
  asset_id   INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
  PRIMARY KEY (keyword_id, asset_id)
);
CREATE INDEX idx_kw_asset_asset ON keyword_asset(asset_id);

-- ============ metadata cache (§3.1: fast filter facets; populated by E04 probe, edited here) ============
CREATE TABLE metadata_cache (
  asset_id INTEGER PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
  -- promoted facet/filter columns (read-mostly, indexed)
  camera_make TEXT, camera_model TEXT, lens_model TEXT,
  iso INTEGER, f_number REAL, exposure_s REAL, focal_len_mm REAL,
  capture_ts INTEGER, capture_tz_offset_min INTEGER,
  gps_lat REAL, gps_lon REAL,
  file_format TEXT,                                              -- 'RAW','DNG','JPEG','TIFF','HEIC','PNG','VIDEO'
  -- editable IPTC subset (catalog is source of truth; sidecar is a projection)
  title TEXT, caption TEXT, creator TEXT, creator_job_title TEXT,
  copyright TEXT, rights_usage TEXT,
  city TEXT, state_province TEXT, country TEXT, country_code TEXT,
  event TEXT, job_identifier TEXT,
  -- everything else, verbatim
  exif_json TEXT NOT NULL DEFAULT '{}',
  iptc_json TEXT NOT NULL DEFAULT '{}',
  -- write-out bookkeeping (seam 3: E09 drains this)
  meta_rev INTEGER NOT NULL DEFAULT 0,                           -- bumped on every catalog-side metadata edit
  written_rev INTEGER NOT NULL DEFAULT 0,                        -- last rev E09 projected to file/sidecar
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_meta_camera  ON metadata_cache(camera_model);
CREATE INDEX idx_meta_lens    ON metadata_cache(lens_model);
CREATE INDEX idx_meta_iso     ON metadata_cache(iso);
CREATE INDEX idx_meta_capture ON metadata_cache(capture_ts);
CREATE INDEX idx_meta_dirty   ON metadata_cache(asset_id) WHERE meta_rev > written_rev;

-- ============ presets ============
CREATE TABLE metadata_preset (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
  patch_json TEXT NOT NULL,                                      -- serialized MetadataPatch
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE filter_preset (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
  spec_json TEXT NOT NULL                                        -- serialized ImageQuery filter portion
);

-- ============ free-text search (§3.1 assets_fts) ============
-- Regular FTS5 table, rowid == asset_id, refreshed in the SAME txn as any contributing write.
CREATE VIRTUAL TABLE assets_fts USING fts5(
  filename, folder_path, keywords, title, caption, creator, camera, lens, label,
  tokenize = "unicode61 remove_diacritics 2",
  prefix = '2 3 4'                                               -- as-you-type filter bar
);
```

**Design notes.**
- **FTS maintenance:** a single DAO helper `fts::refresh(asset_ids)` does delete-then-insert by rowid inside the caller's transaction. Chosen over external-content/trigger schemes because the indexed text is assembled from four tables (asset, folder, keyword_asset, metadata_cache, image.label) — one Rust assembly point beats trigger spaghetti. A full `fts::rebuild()` exists as a cancellable background op (used after migration and by "Optimize Catalog").
- **Custom order:** `sort_key` is a base-62 fractional index (`between(a, b)` generates a key strictly between two neighbors). Drag-reorder is O(1) row writes; a rebalance pass rewrites a collection's keys only when a key would exceed 64 chars.
- **Keyword tree:** adjacency (`parent_id`) is authoritative; `keyword_hierarchy` (closure) is derived and maintained in the same transaction — subtree rules and "export containing keywords" become single-join queries at 100k scale.
- **Dirty metadata:** `meta_rev > written_rev` *is* the dirty set. E07 only ever bumps `meta_rev`; E09's write-metadata command sets `written_rev = meta_rev` after a successful file write and compares file state on read-metadata. The divergence badge (§6 failure table) is computed by E09; E07 stores no file hashes.
- All new tables ride E01's WAL + single-writer invariant; nothing here opens its own connection.

---

## 4. Interface definitions

Contracts, not implementations (per §2.2 convention). All types are UI-free (seam 1).

### 4.1 Attributes (ratings / flags / labels)

```rust
// lightbox-catalog::dam::attrs
#[derive(Clone, Copy, PartialEq, Eq)] pub enum Flag { Reject = -1, Unflagged = 0, Pick = 1 }
#[derive(Clone, Copy, PartialEq, Eq)] pub struct Rating(u8);          // invariant 0..=5, ctor-checked

impl WriterHandle {
    /// Tiny single-row txns; batched as one txn per call (culling budget §7).
    pub fn set_rating(&mut self, images: &[ImageId], rating: Rating) -> Result<()>;
    pub fn set_flag(&mut self, images: &[ImageId], flag: Flag) -> Result<()>;
    pub fn set_label(&mut self, images: &[ImageId], label: Option<&str>) -> Result<()>;
    pub fn purge_rejected(&mut self, scope: QuerySource, mode: RemoveMode) -> Result<PurgeReport>;
}
#[derive(Clone, Copy)] pub enum RemoveMode { CatalogOnly, ToTrash }   // OS trash via `trash` crate
```

### 4.2 Collections

```rust
// lightbox-catalog::dam::collections
impl WriterHandle {
    pub fn create_collection_set(&mut self, parent: Option<CollectionSetId>, name: &str) -> Result<CollectionSetId>;
    pub fn create_collection(&mut self, set: Option<CollectionSetId>, name: &str) -> Result<CollectionId>;
    pub fn rename_collection(&mut self, id: CollectionId, name: &str) -> Result<()>;
    pub fn delete_collection(&mut self, id: CollectionId) -> Result<()>;
    pub fn add_to_collection(&mut self, id: CollectionId, images: &[ImageId]) -> Result<usize>; // idempotent
    pub fn remove_from_collection(&mut self, id: CollectionId, images: &[ImageId]) -> Result<usize>;
    /// Move `image` so it sorts between `after` and `before` (either may be None = end/start).
    pub fn reorder_in_collection(&mut self, id: CollectionId, image: ImageId,
                                 after: Option<ImageId>, before: Option<ImageId>) -> Result<()>;
    pub fn set_target_collection(&mut self, id: CollectionId) -> Result<()>;   // exactly one is_target=1
}
impl ReaderHandle {
    pub fn collection_tree(&self) -> Result<Vec<CollectionNode>>;     // sets + collections + smart, positioned
    pub fn collections_of_image(&self, image: ImageId) -> Result<Vec<CollectionId>>; // grid badge feed (E08)
    pub fn target_collection(&self) -> Result<Option<CollectionId>>;
}
```

### 4.3 Smart collections — AST and compiler

```rust
// lightbox-catalog::dam::smart  (serde JSON <-> smart_collection.rule_tree)
#[derive(Serialize, Deserialize, Clone)]
pub struct RuleTree { pub version: u16 /* = 1 */, pub root: RuleGroup }
#[derive(Serialize, Deserialize, Clone)]
pub struct RuleGroup { pub join: GroupJoin, pub nodes: Vec<RuleNode> }   // nesting = condition groups
#[derive(Serialize, Deserialize, Clone)]
pub enum GroupJoin { All, Any, None }                                     // AND / OR / NOT(OR)
#[derive(Serialize, Deserialize, Clone)]
pub enum RuleNode { Group(RuleGroup), Cond(Condition) }
#[derive(Serialize, Deserialize, Clone)]
pub struct Condition { pub field: Field, pub op: Op, pub value: RuleValue }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    // image attrs                     // asset / metadata_cache facets        // relations
    Rating, Flag, Label,               CameraMake, CameraModel, Lens,          Keyword,          // exact tag
    IsVirtualCopy,                     Iso, Aperture, ShutterSpeed, FocalLen,  KeywordSubtree,   // tag or descendant
    // edit state (reads edit_index)   CaptureDate, ImportDate, FileFormat,    InCollection,
    HasEdits, HasMasks, HasAiMask,     Filename, FolderPath, FileSizeMb,       // text
    CropRatio,                         Missing, Title, Caption, Creator,       AnyText,          // routed to FTS
                                       Copyright,
}
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Ne, Lt, Le, Gt, Ge, Between, Contains, NotContains, StartsWith, EndsWith,
              IsEmpty, IsNotEmpty, InLastDays, IsTrue, IsFalse }
#[derive(Serialize, Deserialize, Clone)]
pub enum RuleValue { Int(i64), Float(f64), Text(String), Date(i64), Range(Box<RuleValue>, Box<RuleValue>),
                     KeywordRef(KeywordId), CollectionRef(CollectionId), None }

// lightbox-catalog::dam::smart_compile
pub struct CompiledRules { pub joins: JoinSet, pub where_sql: String, pub params: Vec<rusqlite::types::Value> }
/// Pure function; ALWAYS parameterized output; rejects unknown field/op/value combos.
pub fn compile(rules: &RuleTree) -> Result<CompiledRules, RuleError>;
/// Field registry: one row per Field — target table.column (or EXISTS-subquery template for
/// Keyword/KeywordSubtree/InCollection/AnyText), the set of legal Ops, and the value type.
/// Adding a field is a registry entry + tests, never a compiler change.
pub enum RuleError { UnknownVersion(u16), IllegalOp(Field, Op), TypeMismatch(Field), EmptyGroup, DepthExceeded }
```

Compilation strategy: base query is `image i JOIN asset a ON a.id=i.asset_id LEFT JOIN metadata_cache m … LEFT JOIN edit_index e …` (joins added only when a field in the tree demands them, tracked by `JoinSet`); relation fields compile to `EXISTS(…)` subqueries (`KeywordSubtree` goes through `keyword_hierarchy`); `GroupJoin::None` compiles to `NOT (…)`; `AnyText` compiles to `a.id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?)` with the match string built by the compiler's FTS-escaper (user text is never spliced into MATCH syntax). Nesting depth capped at 8.

### 4.4 Keywords

```rust
// lightbox-catalog::dam::keywords
impl WriterHandle {
    pub fn create_keyword(&mut self, parent: Option<KeywordId>, name: &str) -> Result<KeywordId>;
    pub fn rename_keyword(&mut self, id: KeywordId, name: &str) -> Result<()>;
    pub fn set_keyword_flags(&mut self, id: KeywordId, f: KeywordExportFlags) -> Result<()>;
    pub fn set_synonyms(&mut self, id: KeywordId, synonyms: &[&str]) -> Result<()>;
    /// Re-parents a subtree. Fails with CycleError if new_parent is inside the subtree.
    pub fn move_keyword(&mut self, id: KeywordId, new_parent: Option<KeywordId>) -> Result<()>;
    /// Re-points assignments of `from` to `into`, unions synonyms, deletes `from`.
    pub fn merge_keywords(&mut self, from: KeywordId, into: KeywordId) -> Result<MergeReport>;
    pub fn delete_keyword(&mut self, id: KeywordId) -> Result<()>;    // cascades subtree + assignments
    pub fn assign_keyword(&mut self, kw: KeywordId, assets: &[AssetId]) -> Result<usize>;   // + FTS refresh
    pub fn unassign_keyword(&mut self, kw: KeywordId, assets: &[AssetId]) -> Result<usize>; // + FTS refresh
}
impl ReaderHandle {
    pub fn keyword_tree(&self) -> Result<Vec<KeywordNode>>;           // with per-node asset counts
    pub fn keywords_of_asset(&self, asset: AssetId) -> Result<Vec<KeywordId>>;
    /// Matches names AND synonyms, prefix-first ranking; <10 ms at 10k keywords.
    pub fn autocomplete_keywords(&self, prefix: &str, limit: u32) -> Result<Vec<KeywordSuggestion>>;
    pub fn recent_keywords(&self, limit: u32) -> Result<Vec<KeywordId>>;
    /// SEAM → E09: fully-qualified paths + synonyms honoring export flags,
    /// ready to map onto dc:subject + lr:hierarchicalSubject. E07 emits data; E09 owns RDF.
    pub fn export_paths(&self, asset: AssetId) -> Result<Vec<KeywordExportEntry>>;
}
pub struct KeywordExportEntry { pub path: Vec<String>, pub leaf: String, pub synonyms: Vec<String>,
                                pub export_leaf: bool, pub export_parents: bool }
```

### 4.5 Filter/query engine

```rust
// lightbox-catalog::dam::query — the single read path E08's grid/filter bar and the CLI use.
pub struct ImageQuery {
    pub source: QuerySource,
    pub text: Option<String>,               // FTS5 across all indexed columns (filter-bar text row)
    pub attrs: AttrFilter,                  // filter-bar attribute row
    pub facets: Vec<FacetSelection>,        // filter-bar metadata columns (selections OR within, AND across)
    pub sort: Sort, pub descending: bool,
    pub page: Page,                         // offset/limit for the virtualized grid
}
pub enum QuerySource { All, Folder { id: FolderId, recursive: bool }, Collection(CollectionId),
                       SmartCollection(SmartCollectionId), Missing }
#[derive(Default)]
pub struct AttrFilter { pub min_rating: Option<Rating>, pub rating_op: CmpOp /* >=, =, <= */,
                        pub flags: Option<EnumSet<Flag>>, pub labels: Option<Vec<Option<String>>>,
                        pub kinds: Option<EnumSet<FileKind>>, pub copy_status: Option<CopyStatus> }
pub enum Sort { CaptureTime, ImportTime, EditTime, Filename, Rating, Custom /* collection sort_key */ }
pub struct FacetSelection { pub field: FacetField, pub values: Vec<FacetValue> }
pub enum FacetField { CaptureDateYmdTree, CameraModel, Lens, Iso, Label, FileFormat, Keyword, FolderPath }

impl ReaderHandle {
    /// The one budgeted call: < 100 ms p95 at 100k assets (§7), any combination of clauses.
    pub fn query_images(&self, q: &ImageQuery) -> Result<QueryResult>;
    /// Column-browser counts: for `facet`, count matches under (source + text + attrs + all OTHER
    /// facet selections) — a column never filters itself. One call per visible column.
    pub fn facet_counts(&self, q: &ImageQuery, facet: FacetField) -> Result<Vec<FacetCount>>;
}
pub struct QueryResult { pub ids: Vec<ImageId>, pub total: u64 }
pub struct FacetCount { pub value: FacetValue, pub count: u64 }
```

### 4.6 Metadata viewer / editor / presets / sync

```rust
// lightbox-catalog::dam::metadata
/// Double-Option patch: None = leave untouched; Some(None) = clear; Some(Some(v)) = set.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct MetadataPatch {
    pub title: Option<Option<String>>, pub caption: Option<Option<String>>,
    pub creator: Option<Option<String>>, pub creator_job_title: Option<Option<String>>,
    pub copyright: Option<Option<String>>, pub rights_usage: Option<Option<String>>,
    pub city: Option<Option<String>>, pub state_province: Option<Option<String>>,
    pub country: Option<Option<String>>, pub country_code: Option<Option<String>>,
    pub event: Option<Option<String>>, pub job_identifier: Option<Option<String>>,
    pub extra_iptc: Option<serde_json::Map<String, serde_json::Value>>,   // spillover → iptc_json
}
bitflags::bitflags! { pub struct MetadataFieldMask: u32 { /* one bit per field above */ } }

impl WriterHandle {
    /// SEAM ← E04: import probe lands here (full EXIF + IPTC snapshot). Also refreshes FTS.
    pub fn upsert_metadata(&mut self, asset: AssetId, snap: &MetadataSnapshot) -> Result<()>;
    /// Catalog-side edit: applies patch, bumps meta_rev, refreshes FTS. Never touches files.
    pub fn apply_metadata_patch(&mut self, assets: &[AssetId], patch: &MetadataPatch) -> Result<()>;
    /// Sync-metadata: read `source`'s fields named by `mask` into a patch, apply to targets.
    /// A masked-in empty source field CLEARS targets (LR semantics).
    pub fn sync_metadata(&mut self, source: AssetId, targets: &[AssetId], mask: MetadataFieldMask) -> Result<()>;
    pub fn save_metadata_preset(&mut self, name: &str, patch: &MetadataPatch) -> Result<MetadataPresetId>;
    pub fn delete_metadata_preset(&mut self, id: MetadataPresetId) -> Result<()>;
}
impl ReaderHandle {
    pub fn metadata_view(&self, asset: AssetId) -> Result<MetadataView>;  // typed IPTC + EXIF map for inspector
    pub fn metadata_presets(&self) -> Result<Vec<MetadataPresetInfo>>;
    /// SEAM → E09: assets whose catalog metadata is newer than the last file projection.
    pub fn metadata_dirty_assets(&self, limit: u32) -> Result<Vec<AssetId>>;
}
// SEAM → E09 (write direction): E09's write-metadata command reads metadata_view + export_paths +
// image attrs, projects via lightbox-meta/XMP Toolkit, then calls:
impl WriterHandle { pub fn mark_metadata_written(&mut self, asset: AssetId, rev: u64) -> Result<()>; }
```

### 4.7 Folders, sync, watcher

```rust
// lightbox-catalog::dam::folders
impl WriterHandle {
    /// fs first, then catalog row, one txn; on fs failure nothing changes (see flow §5.1).
    pub fn create_folder(&mut self, parent: FolderId, name: &str) -> Result<FolderId>;
    pub fn rename_folder(&mut self, id: FolderId, new_name: &str) -> Result<()>;
    pub fn move_folder(&mut self, id: FolderId, new_parent: FolderId) -> Result<()>;
}
pub struct SyncOpts { pub hash_verify: HashPolicy /* None | SizeTimeChanged | All */, pub recurse: bool }
pub struct FolderSyncReport {
    pub new_files: Vec<PathBuf>,        // on disk, not in catalog → handed to E04 add-in-place
    pub missing: Vec<AssetId>,          // in catalog, not on disk → mark asset.missing
    pub restored: Vec<AssetId>,         // was missing, reappeared (hash-checked)
    pub modified: Vec<AssetId>,         // size/mtime (optionally hash) changed → surfaced, not auto-mutated
}
impl ReaderHandle {
    /// Read-only diff; cancellable; progress via sink. Runs on a WAL snapshot.
    pub fn scan_folder(&self, folder: FolderId, opts: &SyncOpts, cancel: &CancelToken,
                       progress: &dyn Fn(SyncProgress)) -> Result<FolderSyncReport>;
}
impl WriterHandle {
    /// Applies missing/restored/modified in one txn. new_files are NOT imported here —
    /// core routes them to E04's import entrypoint (seam).
    pub fn apply_sync_report(&mut self, report: &FolderSyncReport) -> Result<SyncApplyStats>;
}

// lightbox-core::services::fs_watch — notify-based, debounced (500 ms quiescence, coalesced per folder),
// emits ReconcileFolder(FolderId) intents that run scan+apply as Background work. Watcher failure
// degrades to manual "Synchronize Folder" only — never blocks anything (failure row §6).
```

### 4.8 Relink & missing files

```rust
// lightbox-catalog::dam::relink
pub enum MatchConfidence { HashExact, NameSizeTime, NameOnly }        // ordered; UI maps to auto vs confirm
pub struct RelinkCandidate { pub asset: AssetId, pub path: PathBuf, pub confidence: MatchConfidence }
pub enum RelinkOutcome { Relinked, HashMismatchConfirmedByUser, Rejected(RelinkError) }

impl ReaderHandle {
    pub fn find_all_missing(&self) -> Result<Vec<AssetId>>;
    /// Walk `search_roots`, propose matches for every missing asset:
    /// pass 1 content_hash exact (previews/raw-cache survive by construction, §3.3);
    /// pass 2 filename+size+capture_time; pass 3 filename only (surfaced, never auto-applied).
    pub fn propose_relinks(&self, search_roots: &[PathBuf], cancel: &CancelToken,
                           progress: &dyn Fn(RelinkProgress)) -> Result<Vec<RelinkCandidate>>;
}
impl WriterHandle {
    /// verify=true recomputes xxh3-128 and refuses on mismatch unless force is set (user confirmed).
    pub fn relink_asset(&mut self, asset: AssetId, new_path: &Path, verify: bool, force: bool)
        -> Result<RelinkOutcome>;
    /// Re-points folder.rel_path (and children) at a moved tree; batch-verifies by sampling hashes.
    pub fn relink_folder_tree(&mut self, folder: FolderId, new_path: &Path) -> Result<TreeRelinkReport>;
    /// Root relocation (drive letter / mount point changed): match by volume_uuid, update root path.
    pub fn relocate_root(&mut self, root: RootId, new_path: &Path) -> Result<()>;
}
```

### 4.9 Core commands (undoable, over E01's command bus)

```rust
// lightbox-core::commands::dam — every mutation above is wrapped as a Command with an inverse,
// so E08 gets DAM undo/redo for free. DAM undo lives on the SESSION undo stack (E01 transaction/
// history manager); it is distinct from per-image develop history (`history_step`) — see OQ-2.
pub enum DamCommand {
    SetRating { images: Vec<ImageId>, rating: Rating },
    SetFlag   { images: Vec<ImageId>, flag: Flag },
    SetLabel  { images: Vec<ImageId>, label: Option<String> },
    AssignKeyword { assets: Vec<AssetId>, keyword: KeywordId },
    UnassignKeyword { assets: Vec<AssetId>, keyword: KeywordId },
    CreateVirtualCopy { source: ImageId, name: Option<String> },   // new image row over same asset
    DeleteVirtualCopy { image: ImageId },                          // refuses on the primary image
    AddToCollection { collection: CollectionId, images: Vec<ImageId> },
    RemoveFromCollection { collection: CollectionId, images: Vec<ImageId> },
    ToggleTargetCollection { images: Vec<ImageId> },               // the "B" key's backend
    ApplyMetadataPatch { assets: Vec<AssetId>, patch: MetadataPatch },
    ApplyMetadataPreset { assets: Vec<AssetId>, preset: MetadataPresetId },
    SyncMetadata { source: AssetId, targets: Vec<AssetId>, mask: MetadataFieldMask },
    RemoveImages { images: Vec<ImageId>, mode: RemoveMode },
    // folder/collection/keyword CRUD & reorder & relink commands elided — same pattern
}
```

---

## 5. Key flows (failure modes named)

### 5.1 Folder rename/move (fs + catalog atomicity)

`std::fs::rename` first; only on success open the catalog txn updating `folder.rel_path` for the subtree. If the process dies between the two, the folder appears "missing" and the next scan/watcher pass reconciles by name+content-hash — self-healing, no torn catalog state (WAL invariant untouched). Cross-volume moves are rejected in v1 (copy+verify+delete is import-pipeline territory, E04 seam).

### 5.2 Synchronize folder

`scan_folder` (reader, WAL snapshot, cancellable) → report → user confirmation (or watcher auto-apply for missing/restored only) → `apply_sync_report` (single txn) → `new_files` routed by core to E04's add-in-place import. `modified` assets are flagged for E09's read-metadata prompt, never silently re-probed (catalog is source of truth; §2.3 seam 3).

### 5.3 Metadata write-out dance (seam 3, restated so no one re-invents it)

E07 edit → `meta_rev++` (catalog txn). E09's *write-metadata* command: read `metadata_view` + `export_paths` + image attrs at rev N → project XMP via `lightbox-meta` → on file-write success `mark_metadata_written(asset, N)`. A rev bumped mid-write stays dirty (N < new meta_rev) — no lost updates, no locks across the seam. E07 never opens an image file for writing; E09 never issues SQL.

### 5.4 Culling write path (budget-critical)

`SetRating`/`SetFlag`/`SetLabel` on one image = one prepared-statement single-row txn on the serialized writer (§5.1 architecture). No FTS refresh for rating/flag (not indexed text); label refreshes FTS. Measured target: command dispatch → txn commit < 5 ms p95 so E08 can hit keystroke-to-badge < 16 ms.

---

## 6. Ordered task breakdown

Each task ≤ 1 day. Total 35 dev-days ≈ 7 pw — inside the L (5–8 pw) envelope. Order respects intra-epic dependencies; phases C–G are parallelizable across two engineers after Phase A lands.

**First failing test of the epic (write it on day 1, red until T12):** seeded fixture catalog; smart collection `{all: [rating ≥ 3, KeywordSubtree("wedding"), CaptureDate InLastDays 30]}` compiled and executed returns exactly the known fixture ids.

### Phase A — schema + attributes (foundation for everything else)

- **T1. Migration `v2_dam`.** All §3 SQL; runs on a v1 catalog and on empty; idempotence + downgrade-copy preserved per E01 CoW policy. _Accept:_ migration test green on empty + v1-fixture catalogs; `PRAGMA foreign_key_check` clean; `quick_check` clean.
- **T2. Typed ids + attribute DAO.** `Rating`/`Flag`/label newtypes, batch `set_rating/flag/label`, `label_set`/`label_def` CRUD. _Accept:_ unit tests incl. 10k-image batch in one txn; invalid rating unconstructible; label write refreshes FTS row.
- **T3. FTS module.** `assets_fts` assembly + `refresh(asset_ids)` in-txn + `rebuild()` cancellable; FTS-escaper for MATCH input. _Accept:_ text indexed from all four source tables; diacritic-insensitive match; escaper property test (arbitrary input never yields FTS syntax error).
- **T4. Removal semantics.** `RemoveImages` (catalog-only vs OS trash via `trash` crate), `purge_rejected`; cascades membership/keyword/FTS. _Accept:_ remove keeps file on disk; trash mode round-trips through OS trash on macOS + Windows CI; purge honors scope.

### Phase B — collections

- **T5. Sets + collections CRUD DAO.** Nesting, unique-name-per-parent, position. _Accept:_ unit tests incl. set-cycle rejection, delete-set cascade behavior.
- **T6. Fractional ordering.** base-62 `between()` keygen + rebalance. _Accept:_ property test — arbitrary reorder sequences keep strict total order; rebalance triggers at len > 64 and preserves order.
- **T7. Membership + reorder + target collection.** APIs of §4.2, `collections_of_image` badge feed. _Accept:_ idempotent adds; reorder is O(1) writes (asserted via statement-count probe); exactly one target collection invariant.
- **T8. Collection core commands + undo.** Command-bus wrappers with inverses. _Accept:_ undo/redo round-trip for every collection command via `lightbox-cli`.

### Phase C — smart collections

- **T9. RuleTree types + serde + validation.** Versioned JSON, depth cap, `RuleError` taxonomy. _Accept:_ serde round-trip property test; malformed/unknown-version JSON rejected with typed errors.
- **T10. Compiler core + field registry (scalar fields).** Rating/flag/label/camera/iso/format/filename/dates incl. `InLastDays`; JoinSet dedup. _Accept:_ each field × legal op emits parameterized SQL that executes; illegal combos → `RuleError::IllegalOp`.
- **T11. Relation + text + edit-state fields.** `Keyword`/`KeywordSubtree`/`InCollection` EXISTS-subqueries; `AnyText` → FTS; `HasEdits/HasMasks/HasAiMask/CropRatio` reading `edit_index` (read-only seam to E09/E05 — columns exist from E01 schema). _Accept:_ subtree rule matches descendants at depth ≥ 3; AnyText fuzz never produces SQL/FTS errors.
- **T12. Group nesting + None-groups + oracle test.** `NOT(…)` strategy; property test: arbitrary RuleTrees over a seeded 2k-asset fixture, compiled-SQL results == in-memory reference evaluator. _Accept:_ oracle property green over ≥ 10k generated cases in CI; **the epic's first failing test goes green here.**
- **T13. `smart_collection` CRUD + query-source integration.** Evaluate as `QuerySource::SmartCollection`; live results (no caching — it's a query, not a table). _Accept:_ CRUD + rename/move-to-set; edit rule → next query reflects it; CLI can create/list/evaluate.

### Phase D — keywords

- **T14. Keyword tables + create/rename/delete + closure maintenance.** _Accept:_ closure invariant test (every path enumerated) after each op; case-insensitive sibling-name collision rejected.
- **T15. Move + merge.** Cycle detection; subtree closure rebuild; merge unions synonyms/assignments. _Accept:_ property test — random op sequences keep closure == recomputed-from-adjacency; merge is assignment-lossless.
- **T16. Synonyms, export flags, `export_paths()`.** The E09 seam data. _Accept:_ golden test — fixture tree with mixed flags yields exactly the documented `KeywordExportEntry` set (including suppressed parents / excluded leaves).
- **T17. Assignment + autocomplete + recents.** Batch assign/unassign with FTS refresh; prefix-ranked autocomplete over names+synonyms; `last_applied_at`. _Accept:_ autocomplete < 10 ms at 10k keywords (bench); assign 5k assets in one txn < 250 ms.
- **T18. Keyword core commands + undo.** _Accept:_ undo of merge restores both keywords and assignments (inverse captured pre-merge); CLI round-trip.

### Phase E — filter/query engine

- **T19. `ImageQuery` model + source resolution + sort/paging.** All `QuerySource` variants; `Sort::Custom` via `collection_item.sort_key`. _Accept:_ each source × sort returns correct page slices on fixtures; recursive-folder uses `folder` subtree in one query.
- **T20. Attribute + text filter compilation.** AttrFilter semantics (rating cmp op, flag set, label list incl. "no label", kinds, copy status); text → FTS `MATCH` joined into the same statement. _Accept:_ combination matrix test (source × attrs × text) against fixture expectations.
- **T21. Facet counts.** Column-browser semantics of §4.5 (a column never filters itself); `CaptureDateYmdTree` year→month→day drill-down. _Accept:_ semantics fixture tests; one `facet_counts` call at 100k synthetic assets < 100 ms (bench, covering indexes verified via `EXPLAIN QUERY PLAN` — no full scans).
- **T22. Filter presets.** CRUD + (de)serialize the filter portion of `ImageQuery`; forward-compat: unknown fields in `spec_json` preserved. _Accept:_ round-trip tests; preset referencing a deleted collection degrades gracefully (typed error, not panic).

### Phase F — metadata viewer/editor/presets/sync

- **T23. `metadata_cache` DAO + `upsert_metadata` (E04 seam) + `metadata_view`.** Typed IPTC getters, EXIF map from `exif_json`. _Accept:_ upsert from a fixture `MetadataSnapshot` populates facet columns + JSON spillover + FTS; view returns typed fields.
- **T24. `MetadataPatch` + `apply_metadata_patch` + dirty bookkeeping.** Double-Option semantics; `meta_rev`/`written_rev`/`metadata_dirty_assets`/`mark_metadata_written`. _Accept:_ patch matrix test (leave/clear/set × every field); rev bumped mid-"write" stays dirty (interleaving test) — the §5.3 contract test that E09 builds against.
- **T25. Metadata presets.** CRUD + apply (a preset is a stored patch). _Accept:_ create-from-current-asset helper; apply to 1k assets in one txn; preset JSON schema versioned.
- **T26. Sync-metadata.** Mask semantics incl. masked-in-empty-clears-targets; mixed-value detection helper for E08's dialog (per-field "values differ" flag across a selection). _Accept:_ LR-semantics fixture tests; 3k-target sync < 500 ms single txn.
- **T27. Metadata + VC core commands + undo.** `ApplyMetadataPatch/Preset`, `SyncMetadata`, `CreateVirtualCopy`/`DeleteVirtualCopy` (VC gets own attrs; shares asset-level keywords/metadata per schema). _Accept:_ undo restores prior field values + prior `meta_rev` dirty-state; VC create/delete round-trip in CLI.

### Phase G — folders, sync, watcher, relink

- **T28. Folder ops.** create/rename/move per §5.1 ordering; cross-volume rejected. _Accept:_ temp-dir integration tests incl. injected fs failure (catalog unchanged) and injected crash-between (next scan self-heals).
- **T29. Scan/diff engine.** `walkdir` + extension filter + `HashPolicy`; cancellable with progress. _Accept:_ fixture tree with adds/deletes/moves/edits produces the exact `FolderSyncReport`; cancel mid-scan returns within 100 ms and leaves no writes.
- **T30. Apply-sync + E04 handoff.** One-txn apply; `new_files` intent routed through core (behind a trait so E07 tests stub the importer). _Accept:_ missing/restored/modified transitions verified; handoff intent emitted with correct paths; kill -9 during apply → `integrity_check` clean, re-apply idempotent.
- **T31. fs-watcher service.** `notify` per-root watchers, 500 ms debounce, per-folder coalescing, overflow → full-scan fallback, watcher-death → manual-sync degradation (never fatal). _Accept:_ integration test with real fs events (rename storm coalesces to ≤ 2 reconciles); watcher failure leaves catalog usable and logs a surfaced notice.
- **T32. Relink singles + tree + root relocation.** §4.8 write APIs; hash verify/force; previews survive hash-exact relink (assert `preview` rows untouched). _Accept:_ temp-fs scenarios — moved file, moved tree, renamed drive-root via `volume_uuid`; mismatch without force refused.
- **T33. Find-all-missing + candidate proposal.** Three-pass matcher with confidence; scans as cancellable background op. _Accept:_ fixture with 100 missing across two roots → hash matches auto-proposed, heuristic matches ranked below, no false `HashExact`; progress + cancel verified.

### Phase H — hardening, perf, E2E

- **T34. 100k synthetic catalog generator + perf suite.** Deterministic generator (assets, folders, 10k keywords, 200 collections, realistic metadata distributions); criterion + scenario harness asserting §7 budgets (query, facets, FTS, smart collections, culling write). _Accept:_ suite runs nightly in CI; all §8-of-this-doc budgets green on the reference runner; regressions file issues per architecture §8.
- **T35. Fault-injection + headless E2E + seam docs.** kill -9 during bulk keyword assign / sync apply / purge (extends E01 harness); `lightbox-cli` E2E: build catalog → keyword/rate/label → smart collection → filter → sync-metadata → relink — asserted output; write the four seam contract notes (E04, E06, E08, E09) into `docs/plan/epics/E07-seams.md` § headers or this file's §7 references. _Accept:_ E2E green in PR CI (fast subset) + nightly (full); fault-injection PR-blocking; seam notes reviewed by the neighboring epic owners.

---

## 7. Test plan

Per the architecture's §8 testing strategy. **Golden-image tests: N/A** — E07 owns no pixels; its "golden" analogue is the oracle-based smart-collection property suite and the fixture-based facet/metadata semantics tests.

| Layer | What | Gate |
|---|---|---|
| Unit | attribute DAOs, label sets, fractional keys, closure ops, patch/mask semantics, FTS escaper, relink confidence, RuleTree validation | PR-blocking |
| Property (`proptest`) | RuleTree serde round-trip; **compiler-vs-oracle equivalence** (≥10k cases, seeded 2k fixture); arbitrary-input FTS/SQL injection safety (compiled SQL must parse + execute, params only); keyword-op sequences preserve closure invariant; fractional-index order under arbitrary reorders | PR-blocking |
| Integration (temp fs + real SQLite) | folder ops incl. injected fs failure; scan/apply scenarios; watcher coalescing; relink single/tree/root; remove-vs-trash; E04-handoff stub | PR-blocking |
| Headless E2E (`lightbox-cli`) | the T35 scenario script; runs the real command bus + undo stack | PR-blocking (fast subset), full nightly |
| Fault injection | kill -9 mid bulk-assign / sync-apply / purge → `PRAGMA integrity_check` clean, re-run idempotent | PR-blocking |
| Perf (nightly, reference runner) | see budget table below, on the T34 100k synthetic catalog | Nightly; regression → tracked issue |
| Cross-platform | full unit+integration on macOS/Windows/Linux CI (fs semantics, trash, volume UUIDs, watcher backends differ per-OS — these tests are the point) | PR-blocking |

**Perf budgets asserted (from §7 of the architecture, made concrete):**

| Operation @ 100k assets | Budget |
|---|---|
| `query_images` — any source × attrs × text combination | < 100 ms p95 |
| `facet_counts` — one column, filters applied | < 100 ms p95 |
| Smart collection — 5 rules, 2 nesting levels incl. subtree + FTS | < 100 ms p95 |
| FTS as-you-type query (prefix) | < 50 ms p95 |
| Culling write (single image rating/flag/label, dispatch→commit) | < 5 ms p95 |
| Keyword autocomplete @ 10k keywords | < 10 ms p95 |
| Bulk assign keyword to 5k assets (one txn incl. FTS refresh) | < 250 ms |
| Folder scan, 10k files (no hashing) | < 5 s, cancellable < 100 ms |

---

## 8. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Facet counts are the hardest 100 ms item** — N columns × aggregation under arbitrary filters can degenerate to scans. | Covering indexes per facet column; `EXPLAIN QUERY PLAN` assertions in tests; compute only visible columns (E08 requests per-column); named fallback: per-facet materialized count cache refreshed on write (bounded change inside `dam/query.rs`). |
| R2 | **Single-writer contention on bulk ops** — a 10k-asset keyword assign in one txn briefly starves culling writes. | Chunked txns (500 rows) for bulk paths with a progress sink; culling writes stay single-row and are queued ahead by E06's Interactive-over-Background policy once integrated. |
| R3 | **fs-watcher platform variance** (FSEvents coalescing vs ReadDirectoryChangesW overflow vs inotify limits). | Watcher is an *optimization only*: overflow/death degrades to manual sync; debounce+coalesce tested per-OS in CI; no correctness depends on event delivery. |
| R4 | **FTS write amplification** on bulk keyword/label churn. | `refresh()` batches per txn; nightly `rebuild()` available via Optimize Catalog; FTS columns kept lean (no EXIF numerics). |
| R5 | **Smart-collection field creep** (LR exposes dozens of rule fields). | Field registry makes each addition a data row + tests; v1 ships the §4.3 enum exactly; anything else is post-M1 registry work, not compiler work. |
| R6 | **Keywords keyed by `asset` (per §3.1) means virtual copies cannot diverge in keywords/IPTC** (LR allows per-copy metadata). | Accepted deviation for v1 — matches the sidecar reality (one XMP per asset) and simplifies E09's projection. Flagged to E09/E16 so `crs:`-import of per-copy LR metadata coalesces deliberately. Escalate only if user research contradicts. |
| R7 | **Relink false positives** on filename-only matches. | `NameOnly` is never auto-applied; `NameSizeTime` requires all three; only `HashExact` may auto-relink; every apply re-verifies hash unless user forces. |

### Open questions (need an answer, none blocks Phase A start)

- **OQ-1 (→ E01 owner):** does schema v1 already create stub DAM tables, or does `v2_dam` own them wholesale? Determines whether T1 is pure-create or delta. Decide at kickoff; the SQL above is authoritative either way.
- **OQ-2 (→ E01 owner):** confirm the transaction/history manager exposes a *session* undo stack distinct from per-image `history_step`, and whether DAM undo depth is bounded (proposal: 200 entries, session-scoped, not persisted). §4.9 assumes yes.
- **OQ-3 (→ E09 owner):** ratify the §5.3 rev-based dirty contract (`meta_rev`/`written_rev`/`mark_metadata_written`) as the seam-3 ABI before T24 merges — it is E09's read side.
- **OQ-4 (→ E04 owner):** shape of the new-files handoff (`Vec<PathBuf>` intent vs an `ImportRequest` builder) and whether folder-sync imports re-use apply-on-import presets. Stubbed behind a trait in T30 either way.
- **OQ-5 (→ E08 owner):** timezone display policy for `CaptureDate` rules/facets — catalog stores UTC ts + original offset (`capture_tz_offset_min`); proposal: rules and facet drill-down evaluate in *capture-local* time (photographer intuition). Needs UI ratification.
- **OQ-6 (product):** rejected-purge default scope (current source vs whole catalog). Spec'd as scope-parameterized (`purge_rejected(scope, …)`); default chosen by E08.

---

## 9. Definition of done

E07 is done when all of the following hold:

1. **All 35 tasks** merged with their acceptance criteria green in PR CI on macOS + Windows + Linux.
2. **The M1 DAM exit criteria** attributable to this epic pass on the reference runner: catalog interactive at 100k (every budget row in §7 of this doc green in the nightly suite), culling write path ≤ 5 ms p95.
3. **Fault-injection green:** kill -9 during any E07 bulk mutation leaves `integrity_check` clean and the operation idempotently re-runnable (Risk 5 of the architecture).
4. **Headless E2E** (`lightbox-cli`) exercises every public command and query in this spec — no capability exists that the CLI cannot drive (seam-1 discipline).
5. **Property suites** (compiler-oracle, injection-safety, closure invariants, patch semantics) run ≥10k cases in nightly CI without failure.
6. **Seam contracts ratified** by neighboring owners: E04 (upsert_metadata + new-files handoff), E06 (cancellable-fn wrapping), E08 (ImageQuery/facets/autocomplete surface), E09 (dirty-rev ABI + `export_paths` shape) — each has a written sign-off note (OQ-1…OQ-4 closed).
7. **No UI types** below the core boundary; **no SQL** above it (enforced by review + the fact that E08 compiles against `lightbox-core` only).
8. **License surfaces untouched or green:** new crates (`notify`, `walkdir`, `bitflags`, `trash`) pass cargo-deny surface 1; no native/FFI or bundled-data additions (surfaces 2–3 unchanged).
9. **Migration discipline:** `v2_dam` upgrades a real M0 catalog copy-on-write with the original preserved, per E01 policy; downgrade path documented.
10. **Docs:** this spec updated to as-built; seam notes published; `RuleTree` JSON schema v1 committed as the forward-compat reference for filter/smart-collection presets.
