# E01 — Foundation: workspace, catalog, headless core, shell skeleton

| | |
|---|---|
| **Epic id** | E01 `foundation-workspace-catalog` |
| **Milestone** | M0 — Walking skeleton (§9) |
| **Effort** | M, ~4–6 pw (task roll-up below: ~27–29 engineer-days; parallelizable into a catalog/core stream and a render/shell stream → ~3.5–4 wall-clock weeks with 2 engineers) |
| **Depends on** | nothing (first epic) |
| **Blocks** | E02, E03, E04, E05, E06, E07, E08, E09, E13 (every other epic transitively) |
| **Author** | staff-engineer (epic planner) |
| **Inputs** | `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (approved), `docs/research/00-feature-catalog.md`, research reports 01 & 09 |

**Precondition — hard gate, not a formality (§12 of the architecture):** E01 must not begin until the **CEO + operator sign-off on the two near-one-way doors** — Rust as core language (§1.1) and the SQLite on-disk catalog format (§1.4) — is on record. This spec assumes that sign-off exists; if it does not, everything below is planning inventory only.

---

## 1. Scope

E01 delivers the **walking skeleton** of §9 M0: the thing that proves the two scariest integrations of the whole product — (a) the **zero-copy shared-`wgpu::Device` seam** between the render engine and the egui shell, and (b) the **`RenderNode`/Engine trait boundary** — *together*, a full milestone before E05 builds the real DAG, plus the **crash-proof catalog** that everything else writes into.

Concretely, E01 ships:

1. **Cargo workspace + guardrails.** The full §2.1 crate map stubbed; MSRV pinned; 3-OS CI matrix (macOS/Metal, Windows/DX12, Linux/Vulkan); `cargo-deny` license gate (CI surface 1 of §8); tracing/error taxonomy; a pinned-hash test-fixture corpus (CC0 raws from raw.pixls.us).
2. **`lightbox-catalog` v1.** SQLite via `rusqlite`, WAL + `synchronous=NORMAL`, single-writer + WAL-reader pool, versioned forward-only migrations, the **spine schema** (migration `0001`: `schema_version`, `library_root`, `folder`, `asset`, `image`, `import_session`, `assets_fts`), `quick_check` on open, **exit-time verified backup** (integrity_check → zstd → dated dir → prune), and the **`kill -9` fault-injection harness** as a PR-blocking gate (Risk 5).
3. **`lightbox-core` headless façade.** `Core`/`Session` lifecycle, command bus (mutations in single WAL transactions), event broadcast, query API over WAL snapshot readers. **No UI type crosses down; no SQL crosses up** (§2.3 seam 1). Headless-testable end-to-end via `lightbox-cli`.
4. **Minimal add-in-place import.** Walk → probe → xxh3-128 content hash → batched catalog inserts → progress events → cancellation → undo-import. Enough to satisfy the M0 exit ("import 1 k raws"); the full ingest pipeline (copy/move/rename/second-copy/DNG/presets) is E04.
5. **`lightbox-decode::probe` + embedded-preview extraction.** Metadata-only probing (rawler for raws; header/EXIF crates for JPEG/TIFF/PNG) and extraction+decode of the camera's embedded JPEG. **No raw decode, no demosaic, no DCP** — E01 takes zero dependency on E02, per §9 M0.
6. **`EmbeddedPreviewProvider`.** A demand-driven, cancellable, byte-capped in-memory provider behind the `PreviewProvider` trait that E03's tiered on-disk pyramid will implement later. This is the M0 stand-in for the preview store, explicitly a seam, not a competing implementation.
7. **`lightbox-render` Engine seed with one real `RenderNode`.** `NodeRegistry` keyed `(NodeId, ProcessVersion)`, `Engine::submit`/`poll`/`cancel`, latest-wins coalescing, and a **`DisplayTransformNode`** (sRGB decode → linear → orientation → scale → sRGB encode) in WGSL *and* CPU, golden-image tested with CPU/GPU parity (ΔE2000/PSNR per §4.4). The loupe image is produced by `Engine::submit` — **no hardcoded blit path that bypasses the engine** (§9 M0, verbatim requirement).
8. **`lightbox-jobs` seed.** tokio runtime, `Class` enum, `spawn`, `JobHandle`, cooperative `CancelToken`, bounded channels. Priority preemption/pause/activity-center model is E06.
9. **`lightbox-shell` skeleton.** eframe app sharing one `wgpu::Device` with the Engine; virtualized grid MVP (demand-driven thumbnails, cancel-on-scroll, upgrade-in-place); loupe view compositing the Engine's output texture zero-copy; minimal navigation. Full Library UX (culling grammar, filmstrip, compare/survey, panels, keymap) is E08.
10. **`lightbox-cli`.** Headless driver (`create`, `import`, `list`, `render`, `backup`, `check`) used by the E2E integration tests (§8) and by every future epic as the headless proof of seam 1.
11. **Test scaffolding owned by E01 forever after:** the golden-image compare harness (`ΔE2000` + PSNR, bless workflow), the fault-injection harness, and the perf scenario-harness skeleton (nightly).

### 1.1 Explicit non-goals (named seams, not designs)

| Not in E01 | Owning epic | The seam E01 leaves behind |
|---|---|---|
| Raw decode, linearization, demosaic, camera-matrix/DCP color, LCMS2/ICC display transform | **E02** | `lightbox-decode::probe` API frozen; `decode_raw`/`decode_image`/`camera_matrix_base` declared but `unimplemented`; `SourceResolver` trait (§3.5) is where decoded raws enter the Engine |
| Preview pyramid (T0/T1/T2 on-disk store), raw cache, `preview` table, `.lbdata/previews/` layout | **E03** | `PreviewProvider` trait (§3.6); `.lbdata/` directory created by E01 with `catalog.sqlite` + `backups/` only; E03 adds its subtrees + `preview` table via migration |
| Copy/move/rename-template import, second-copy backup, apply-on-import presets, card detection, watched folders, dedup heuristics beyond content-hash | **E04** | `lightbox-ingest` internals (walk/probe/hash/batch-insert) written as reusable primitives; `ImportOptions` struct `#[non_exhaustive]` |
| Multi-node DAG, content-keyed node cache, tail invalidation, ROI/256² tiling, progressive preview→full, device-lost recovery + CPU-fallback *harness* | **E05** (E05.1–E05.5) | The Engine seed's trait surface (§3.4) is E05.1's stated starting point ("no double-build", §10.1); `Engine::on_device_lost` registered but M0 behavior = fail the ticket + emit event |
| Priority preemption, pause/resume, activity-center backing model, backpressure tuning | **E06** | `Class` enum + `CancelToken` + `spawn` signature frozen (§3.3) |
| Folders-panel UX, collections, smart collections, keywords, filter bar, relink, metadata editor | **E07** | Spine tables + migration registry; FTS5 external-content table designed for column expansion |
| Real Library UI: culling grammar, keymap registry, filmstrip, compare/survey, panels/Solo, performance prefs panel | **E08** | Grid/loupe MVP handed over as the starting widget set; selection model seed |
| Recipe schema (§3.2), history, snapshots, XMP, `crs:` import | **E09** | `Recipe` placeholder in `lightbox-edit` freezing only `{schema, pv}` + `Recipe::identity()`; `history_step`/`snapshot` tables are E09 migrations |
| `lightbox-inferd`, ORT, IPC | **E13** | No stub crate created (a process boundary needs no compile-time reservation) |
| CI license surfaces 2 (native-binary SBOM) & 3 (content/data manifest) | **E16** (release-hardening) | Surface 1 (`cargo-deny`) ships in E01 and is PR-blocking from day one; E01 ships **no native C libraries and no bundled content**, so surfaces 2/3 have an empty inventory until E02+ |
| Per-monitor ICC / display color management | **E02.3** | M0 loupe assumes sRGB display — stated honestly in the UI as a dev-build limitation |
| Corruption *repair/restore* UX flow | **E16** | E01 detects (`quick_check` on open), refuses to open a corrupt catalog with a message naming the newest verified backup; guided restore is E16 |

---

## 2. Crates touched (per §2.1 decomposition)

| Crate | E01 role | Contents at M0 |
|---|---|---|
| `lightbox-types` *(new, small — see note)* | **owns** | Id newtypes, `ContentHash`, `Orientation`, `ProcessVersion`, shared error/event plumbing. No deps beyond serde/thiserror. |
| `lightbox-catalog` | **owns** | Open/create, WAL, writer/readers, migrations, spine DAOs, FTS5, integrity, verified backup, fault-injection tests. |
| `lightbox-core` | **owns** | `Core`/`Session`, command bus, event broadcast, query façade, exit-time backup orchestration. |
| `lightbox-jobs` | **owns seed** | `Class`, `spawn`, `JobHandle`, `CancelToken`, bounded channels. E06 grows it. |
| `lightbox-decode` | **owns probe only** | `probe()`, `read_embedded()`, `hash_file()`. `decode_raw`/`decode_image`/DCP declared, E02 implements. |
| `lightbox-preview` | **owns seed** | `PreviewProvider` trait + `EmbeddedPreviewProvider` (in-memory LRU). E03 owns the tiered store. |
| `lightbox-render` | **owns seed** | `GpuContext`, `Engine` (submit/poll/cancel/coalesce), `NodeRegistry`, `RenderNode` trait, `DisplayTransformNode` (WGSL + CPU), texture pool. E05 generalizes to the DAG. |
| `lightbox-edit` | **stub + placeholder** | `Recipe { schema, pv }` + `identity()`, `#[non_exhaustive]`. E09 owns the real §3.2 type. |
| `lightbox-ingest` | **owns minimal** | Add-in-place import primitives. E04 owns the full pipeline. |
| `lightbox-shell` | **owns skeleton** | eframe app, shared-device init, grid MVP, loupe-through-Engine, event pump. E08 owns the real Library/Develop UX. |
| `lightbox-cli` | **owns** | Headless driver + E2E harness entry point. |
| `lightbox-color`, `lightbox-mask`, `lightbox-meta`, `lightbox-export`, `lightbox-ml` | **stub only** | Empty crates with crate-level docs pointing at the owning epic (E02, E12, E07/E09, E15, E13). Reserves the §2.1 crate map and the CI wiring; zero logic. |
| `tools/lbx-image-compare` | **owns** | ΔE2000/PSNR compare + golden bless workflow (used by every pixel epic after us). |
| `tools/xtask` | **owns** | Fixture fetch (pinned hashes), CI helpers, migration-registry lint. |

**Note on `lightbox-types`:** §2.1 does not name this crate; it exists to break the dependency cycle between `lightbox-catalog` and `lightbox-core` on shared id/error types without letting leaf crates depend on `lightbox-core`. It is workspace plumbing inside E01's "workspace" mandate, contains no behavior, and is flagged here so the CTO review sees the (small) addition to the crate map explicitly.

Workspace layout:

```
Cargo.toml                 # [workspace], shared lints, profile config
rust-toolchain.toml        # pinned toolchain (MSRV; edition 2021 per §1.1)
deny.toml                  # cargo-deny: license allowlist, GPL/AGPL denied
crates/
  lightbox-types/  lightbox-catalog/  lightbox-core/    lightbox-jobs/
  lightbox-decode/ lightbox-preview/  lightbox-edit/    lightbox-render/
  lightbox-ingest/ lightbox-shell/    lightbox-cli/
  lightbox-color/  lightbox-mask/     lightbox-meta/    lightbox-export/  lightbox-ml/
tools/
  lbx-image-compare/  xtask/
fixtures/                  # not committed: fetched by `cargo xtask fixtures` (pinned xxh3 hashes)
  manifest.toml            # committed: URL + hash + license (CC0) per fixture — provenance from day one
docs/plan/migrations.md    # committed: migration-number registry (see §4.4)
.github/workflows/         # ci.yml (PR gate), nightly.yml (perf + long fault-injection)
```

---

## 3. Interface definitions

These are the **frozen contracts** E01 hands to its neighbors. Signatures follow §2.2 exactly where §2.2 specifies one; deviations (e.g. `Result` return types, explicit cancel tokens) are deliberate hardening of the illustrative sketches and are called out.

### 3.1 `lightbox-types`

```rust
// Ids are catalog rowids behind newtypes; never raw i64 across a crate boundary.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct AssetId(pub i64);
// Same derive set:
pub struct ImageId(pub i64);
pub struct FolderId(pub i64);
pub struct RootId(pub i64);
pub struct ImportSessionId(pub i64);

/// xxh3-128 of the full original file. Keys caches + relink (§3.1 architecture).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ContentHash(pub [u8; 16]);
impl ContentHash { pub fn to_hex(&self) -> String; pub fn from_hex(s: &str) -> Option<Self>; }

/// EXIF orientation 1..=8. Applied in DisplayTransformNode, never baked into stored pixels.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Orientation { O1, O2, O3, O4, O5, O6, O7, O8 }

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcessVersion(pub u16);
pub const PV_M0: ProcessVersion = ProcessVersion(1);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Flag { None, Pick, Reject }
```

### 3.2 `lightbox-catalog`

```rust
pub struct Catalog { /* private: writer thread handle + reader pool */ }

impl Catalog {
    /// Creates `<name>.lbdata/` (catalog.sqlite + backups/), applies all migrations.
    pub fn create(lbdata_dir: &Path) -> Result<Catalog, CatalogError>;
    /// Opens WAL, runs `PRAGMA quick_check`, applies pending migrations (forward-only).
    /// Refuses a catalog whose schema_version is NEWER than this build (clear error).
    pub fn open(lbdata_dir: &Path) -> Result<Catalog, CatalogError>;

    pub fn writer(&self) -> WriterHandle;      // the single serialized writer (§5.1)
    pub fn reader(&self) -> ReaderHandle;      // WAL-snapshot read connection from the pool
    pub fn integrity(&self) -> IntegrityStatus;             // quick_check, cheap
    /// Online-backup → integrity_check ON THE COPY → zstd → temp+rename into
    /// backups/YYYY-MM-DD-HHMMSS/catalog.sqlite.zst → prune to retention.
    pub fn backup_verified(&self, opts: &BackupOpts) -> Result<BackupReport, CatalogError>;
    pub fn schema_version(&self) -> u32;
}

pub enum IntegrityStatus { Ok, Corrupt(Vec<String>) }
pub struct BackupOpts { pub retain: u32 /* default 10 */, pub dest_override: Option<PathBuf> }
pub struct BackupReport { pub path: PathBuf, pub bytes: u64, pub took: Duration }

impl WriterHandle {
    /// Every mutation runs inside exactly one WAL transaction (crash-safety invariant §3.1).
    /// Closure runs on the dedicated writer thread; no SQLITE_BUSY by construction.
    pub fn with_txn<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut CatalogTxn<'_>) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, CatalogError>;
}

// DAOs exposed on CatalogTxn (mutations) and ReaderHandle (queries). No SQL leaks upward.
impl CatalogTxn<'_> {
    pub fn upsert_root(&mut self, volume_uuid: Option<&str>, path: &Path) -> Result<RootId>;
    pub fn upsert_folder(&mut self, root: RootId, parent: Option<FolderId>, rel_path: &str)
        -> Result<FolderId>;
    /// Batched insert (1 txn per call site batches N assets — §7 burst-import mitigation).
    /// Skips rows whose content_hash already exists; returns inserted ids + skip count.
    pub fn insert_assets(&mut self, batch: &[NewAsset]) -> Result<InsertOutcome>;
    pub fn insert_default_images(&mut self, assets: &[AssetId]) -> Result<Vec<ImageId>>;
    pub fn begin_import_session(&mut self, src: &str, opts_json: &str) -> Result<ImportSessionId>;
    pub fn finish_import_session(&mut self, id: ImportSessionId, stats_json: &str) -> Result<()>;
    /// Undo-import: removes catalog rows only; files on disk untouched (add-in-place).
    pub fn remove_import_session(&mut self, id: ImportSessionId) -> Result<RemovedCounts>;
    pub fn set_rating(&mut self, image: ImageId, rating: Option<u8>) -> Result<()>;
    pub fn set_flag(&mut self, image: ImageId, flag: Flag) -> Result<()>;
    pub fn mark_decode_error(&mut self, asset: AssetId, err: &str) -> Result<()>;
}

impl ReaderHandle {
    pub fn images_page(&self, q: &ImageQuery) -> Result<Page<ImageSummary>>;
    pub fn image_detail(&self, id: ImageId) -> Result<ImageDetail>;
    pub fn folder_tree(&self) -> Result<Vec<FolderNode>>;
    pub fn asset_abs_path(&self, id: AssetId) -> Result<PathBuf>;   // root.path ⊕ folder.rel_path ⊕ filename
    pub fn counts(&self) -> Result<CatalogCounts>;
    pub fn search_filenames(&self, query: &str, limit: u32) -> Result<Vec<ImageId>>; // FTS5
}

/// Keyset pagination (never OFFSET) — stable under concurrent inserts, O(page) at 100k+.
pub struct ImageQuery {
    pub folder: Option<FolderId>,
    pub sort: SortOrder,                  // CaptureTimeAsc/Desc | AddedAsc/Desc | FilenameAsc
    pub cursor: Option<PageCursor>,       // opaque (last sort key + id)
    pub limit: u32,                       // clamped to 1..=1000
}
pub struct Page<T> { pub items: Vec<T>, pub next: Option<PageCursor> }
pub struct ImageSummary {
    pub id: ImageId, pub asset: AssetId, pub filename: String,
    pub capture_time: Option<String>, pub rating: Option<u8>, pub flag: Flag,
    pub orientation: Orientation, pub width: u32, pub height: u32,
    pub missing: bool, pub decode_error: bool,
}
```

### 3.3 `lightbox-jobs` (seed — frozen surface for E06)

```rust
/// §5.3 job classes. M0 semantics: separate tokio semaphore budgets per class
/// (Interactive never queues behind Background). Preemption/pause = E06.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Class { Interactive, Foreground, Background }

#[derive(Clone)]
pub struct CancelToken(/* Arc<AtomicBool> + Notify */);
impl CancelToken {
    pub fn child(&self) -> CancelToken;            // hierarchical cancellation
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub async fn cancelled(&self);                 // await-able for select!
}

pub struct JobSystem { /* tokio multi-thread runtime, bounded per-class queues */ }
impl JobSystem {
    pub fn new(cfg: JobConfig) -> JobSystem;
    pub fn spawn<T: Send + 'static>(
        &self, class: Class, name: &'static str, cancel: CancelToken,
        fut: impl Future<Output = Result<T, JobError>> + Send + 'static,
    ) -> JobHandle<T>;
    /// For sync work (hashing, decode, SQLite): runs on the blocking pool, same handle shape.
    pub fn spawn_blocking<T: Send + 'static>(
        &self, class: Class, name: &'static str, cancel: CancelToken,
        f: impl FnOnce(&CancelToken) -> Result<T, JobError> + Send + 'static,
    ) -> JobHandle<T>;
}
pub struct JobHandle<T> { /* ticket id, join handle, cancel token */ }
impl<T> JobHandle<T> {
    pub fn cancel(&self);
    pub async fn join(self) -> Result<T, JobError>;   // JobError::Cancelled is a normal outcome
    pub fn try_result(&mut self) -> Option<Result<T, JobError>>;  // non-blocking poll for the UI
}
```

### 3.4 `lightbox-render` (Engine seed — E05.1's stated starting point)

```rust
/// The engine NEVER creates the device when running under the shell — it receives the
/// shell's device so the output texture composites zero-copy (§2.3 seam 2).
#[derive(Clone)]
pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub backend: wgpu::Backend,
    pub limits: wgpu::Limits,
    pub adapter_info: wgpu::AdapterInfo,
}

pub struct Engine { /* registry, scheduler, texture pool, SourceResolver */ }
impl Engine {
    /// Shell path: shares the eframe device. Headless path: creates its own device,
    /// or runs CPU-only when no adapter exists (CI software-adapter fallback).
    pub fn new(gpu: Option<GpuContext>, registry: NodeRegistry,
               sources: Arc<dyn SourceResolver>) -> Result<Engine, EngineError>;

    pub fn submit(&self, req: RenderRequest) -> RenderTicket;      // async, never blocks
    pub fn poll(&self, t: &RenderTicket) -> RenderState;           // UI polls per frame
    pub fn cancel(&self, t: &RenderTicket);
    /// M0: registered but degenerate — fails in-flight tickets + fires the callback.
    /// Full rebuild/CPU-degrade harness is E05.5.
    pub fn on_device_lost(&self, cb: Box<dyn Fn(DeviceLostReason) + Send + Sync>);
    pub fn backend_kind(&self) -> BackendKind;                     // Gpu(wgpu::Backend) | CpuOnly
}

/// §2.2 shape, plus `viewport` (identifies the coalescing bucket) — the render scheduler
/// keeps at most one in-flight request per viewport, latest-wins (§4.3 coalescing).
pub struct RenderRequest {
    pub image: ImageId,
    pub recipe: Recipe,                 // lightbox-edit placeholder at M0 (Recipe::identity)
    pub pv: ProcessVersion,
    pub roi: Roi,                       // M0: Roi::Full
    pub scale: RenderScale,             // M0: FitWithin { w, h } | Native
    pub target: RenderTarget,           // Texture (shell) | CpuBuffer (cli/export path)
    pub viewport: ViewportId,           // coalescing key
}

pub enum RenderState {
    Pending,
    Running,
    Ready(RenderOutput),
    Failed(RenderError),
    Cancelled,
    Superseded,                         // a newer request on the same viewport won
}
pub enum RenderOutput {
    Texture { tex: Arc<wgpu::Texture>, view: Arc<wgpu::TextureView>, size: [u32; 2] },
    Cpu(ImageBufU8),                    // RGBA8 sRGB, for CLI/tests
}

/// §2.2 contract with two hardening deviations: Results, and cancel visibility via ctx.
/// M0: one Tile == whole image. The Tile type carries (offset, extent) so E05.3's 256²
/// tiling changes tile PRODUCTION, not this trait.
pub trait RenderNode: Send + Sync {
    fn id(&self) -> NodeId;
    fn eval_gpu(&self, ctx: &GpuCtx, inputs: &[Tile], p: &Params) -> Result<Tile, NodeError>;
    fn eval_cpu(&self, ctx: &CpuCtx, inputs: &[TileCpu], p: &Params) -> Result<TileCpu, NodeError>;
    fn invalidates(&self, changed: &ParamDelta) -> bool;    // consumed by E05.2's cache
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub &'static str);            // e.g. NodeId("display.transform")

/// Params are CBOR bytes + their hash — opaque to the engine, hashed for E05.2's cache key.
pub struct Params { pub cbor: Arc<[u8]>, pub hash: u64 }

pub struct NodeRegistry { /* HashMap<(NodeId, ProcessVersion), Arc<dyn RenderNode>> */ }
impl NodeRegistry {
    pub fn register(&mut self, pv: ProcessVersion, node: Arc<dyn RenderNode>);
    pub fn get(&self, id: NodeId, pv: ProcessVersion) -> Option<Arc<dyn RenderNode>>;
    /// Old PVs stay registered forever (§4.5). Removal API deliberately does not exist.
}
```

**The one real node.** `DisplayTransformNode` (`NodeId("display.transform")`, registered under `PV_M0`):
input = RGBA8 **sRGB-encoded** embedded-preview pixels; params = `{ orientation: u8, out_w: u32, out_h: u32 }`; operation = sRGB EOTF decode → orientation rotate/flip → bilinear resample to output size in linear light → sRGB OETF encode → RGBA8 out. WGSL compute shader (16×16 workgroups) and a rayon CPU path implement the **same written algorithm spec** (committed next to the shader as `display_transform.md`), which is what makes the ΔE parity test meaningful. This is deliberately *almost* trivial math — its job is to prove the trait, the registry, the ticket lifecycle, the golden harness, and the shared-device composite, not to be clever. E02.3 will replace/extend the color math (real display ICC); the node's *shape* is the deliverable.

### 3.5 `SourceResolver` (the pixels-in seam — E02/E03 implement the real ones)

```rust
/// Where the Engine gets input pixels for (image, scale). M0 impl: embedded preview via
/// lightbox-preview. E03 swaps in the tiered store; E02+E05 add the raw-decode path.
pub trait SourceResolver: Send + Sync {
    fn resolve(&self, image: ImageId, scale: RenderScale, cancel: &CancelToken)
        -> Result<SourceImage, SourceError>;
}
pub struct SourceImage {
    pub px: Arc<[u8]>, pub width: u32, pub height: u32,
    pub format: SourcePixelFormat,      // M0: Rgba8Srgb only
    pub orientation: Orientation,       // not yet applied
    pub tier: SourceTier,               // M0: EmbeddedPreview
}
```

### 3.6 `lightbox-preview` (seed — trait frozen for E03)

```rust
pub enum PreviewClass { Thumb { max_px: u32 }, Loupe }
pub enum PreviewState { Pending, Ready(Arc<DecodedImage>), Failed(PreviewError) }

pub trait PreviewProvider: Send + Sync {
    fn request(&self, image: ImageId, class: PreviewClass, class_prio: Class) -> PreviewTicket;
    fn poll(&self, t: &PreviewTicket) -> PreviewState;
    fn cancel(&self, t: &PreviewTicket);        // grid MUST cancel on scroll-out (§5.3)
}

pub struct DecodedImage {
    pub px: Arc<[u8]>,                  // RGBA8, sRGB
    pub width: u32, pub height: u32,
    pub orientation_applied: bool,      // thumbnails: true (CPU); loupe source: false (node applies)
    pub tier: SourceTier,
}

/// M0 implementation: extract embedded JPEG (offset from probe) → zune-jpeg decode →
/// fast_image_resize downscale → in-memory LRU capped in BYTES (default 256 MiB).
/// Dedupes concurrent requests for the same (asset, class). Runs on Class::Background
/// for thumbs / Class::Interactive for the loupe source.
pub struct EmbeddedPreviewProvider { /* … */ }
```

### 3.7 `lightbox-decode` (probe surface only)

```rust
pub enum ProbedFormat { Raw(&'static str /* "CR3", "NEF", … */), Jpeg, Tiff, Png,
                        Unsupported(String) }

pub struct AssetProbe {
    pub format: ProbedFormat,
    pub width: u32, pub height: u32,            // full-size dims (0,0 if unknown)
    pub orientation: Orientation,
    pub camera_make: Option<String>, pub camera_model: Option<String>,
    pub capture_time: Option<String>,           // RFC3339 with offset if present
    pub file_bytes: u64,
    pub embedded: Vec<EmbeddedPreviewInfo>,     // sorted largest-first
}
pub struct EmbeddedPreviewInfo { pub width: u32, pub height: u32, pub byte_range: Range<u64> }

/// Never panics on malformed input (fuzz-seeded test corpus). Raw formats via rawler's
/// metadata API; JPEG/TIFF/PNG via header parse + kamadak-exif. Anything else =
/// ProbedFormat::Unsupported — still catalogued, badged, never crashes the app.
pub fn probe(path: &Path) -> Result<AssetProbe, ProbeError>;
pub fn read_embedded(path: &Path, info: &EmbeddedPreviewInfo) -> Result<Vec<u8>, ProbeError>;
/// Streaming xxh3-128 (1 MiB chunks, honors cancel between chunks).
pub fn hash_file(path: &Path, cancel: &CancelToken) -> Result<ContentHash, ProbeError>;

// Declared now, implemented by E02 (signatures per §2.2 — the E02 seam):
pub fn decode_raw(path: &Path, opts: DecodeOpts) -> Result<MosaicImage, DecodeError>;
pub fn decode_image(path: &Path) -> Result<LinearImage, DecodeError>;
```

### 3.8 `lightbox-core` (the headless boundary — seam 1)

```rust
pub struct Core { /* JobSystem, config */ }
impl Core {
    pub fn start(cfg: CoreConfig) -> Result<Core, CoreError>;   // no catalog yet
    pub fn create_catalog(&self, lbdata: &Path, gpu: Option<GpuContext>) -> Result<Session>;
    pub fn open_catalog(&self, lbdata: &Path, gpu: Option<GpuContext>) -> Result<Session>;
}

/// Clone-cheap (Arc inner). Everything the shell (or CLI) may touch. NOTHING here
/// mentions egui, wgpu surfaces/windows, or SQL — wgpu::Device is the ONE shared
/// GPU type allowed across the boundary, by design (§2.3 seam 2).
#[derive(Clone)]
pub struct Session { /* … */ }
impl Session {
    pub fn submit(&self, cmd: Command) -> CommandTicket;         // async; result via Event
    pub fn query(&self) -> Queries;                              // sync, WAL-snapshot reads
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<Event>;
    pub fn previews(&self) -> Arc<dyn PreviewProvider>;
    pub fn engine(&self) -> Arc<Engine>;
    /// Exit-time verified backup per policy (skippable via opts for tests).
    pub fn close(self, opts: CloseOpts) -> Result<CloseReport, CoreError>;
}

#[non_exhaustive]
pub enum Command {
    ImportAddInPlace { source_dir: PathBuf, recursive: bool },
    UndoImport { session: ImportSessionId },
    SetRating { image: ImageId, rating: Option<u8> },   // canonical trivial command;
    SetFlag { image: ImageId, flag: Flag },             // proves the txn+event path (E08 grows UX)
    BackupNow,
}

#[non_exhaustive]
pub enum Event {
    ImportStarted  { session: ImportSessionId, ticket: CommandTicket },
    ImportProgress { session: ImportSessionId, done: u64, discovered: u64, current: PathBuf },
    ImportFinished { session: ImportSessionId, report: ImportReport },
    CatalogChanged { change: ChangeSet },       // coarse at M0: folders/images invalidation hints
    CommandFailed  { ticket: CommandTicket, error: String },
    BackupFinished { report: BackupReport },
    DeviceDegraded { reason: String },          // E05.5 seam
}

pub struct ImportReport {
    pub imported: u64, pub skipped_duplicates: u64,
    pub unsupported: u64, pub errors: Vec<(PathBuf, String)>, pub took: Duration,
}

pub struct Queries { /* wraps ReaderHandle; same method set as §3.2 ReaderHandle,
                        returning core DTOs — the reader types simply re-exported */ }
```

**Threading contract (§5.1):** `Session::query()` executes on the calling thread against a pooled read connection (fast, safe from the UI thread at M0 grid scale; if profiling shows >1 ms reads we move to prefetched pages — named below as a risk). `submit()` never blocks: commands are enqueued to the writer task. Events are a `broadcast` channel; the shell drains it once per frame.

### 3.9 `lightbox-edit` (placeholder — E09 owns)

```rust
/// M0 placeholder. E09 replaces the body with the full §3.2 recipe; ONLY the two fields
/// below and `identity()` are frozen by E01. #[non_exhaustive] keeps downstream honest.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Recipe { pub schema: u16, pub pv: ProcessVersion }
impl Recipe { pub fn identity(pv: ProcessVersion) -> Recipe { Recipe { schema: 1, pv } } }
```

### 3.10 `lightbox-cli`

```
lightbox-cli create  --catalog <dir>.lbdata
lightbox-cli import  --catalog <dir> --add <src-dir> [--recursive]
lightbox-cli list    --catalog <dir> [--folder <id>] [--limit N] [--json]
lightbox-cli render  --catalog <dir> --image <id> --out out.png [--width N] [--cpu]
lightbox-cli backup  --catalog <dir>
lightbox-cli check   --catalog <dir>            # quick_check + schema version, exit code
```
`render` drives the *same* `Engine::submit`/`poll` path as the shell with `RenderTarget::CpuBuffer` (GPU if an adapter exists, `--cpu` forces the CPU node path). This is the headless E2E proof required by §8.

---

## 4. Data model — migration `0001` (the spine)

### 4.1 Decision: spine now, epic-owned tables later

§9 says "SQLite schema v1" at M0; §3.1 lists the full v1 entity set. E01 creates **only the tables E01 itself populates or reads** and ships the **migration framework** that lets every subsequent epic add its own tables (`preview` → E03; `collection*`/`keyword*`/`smart_collection`/`metadata_cache` → E07; `edit_recipe`/`edit_index`/`history_step`/`snapshot` → E09; `mask*`/`retouch_op` → E12; `embedding`/`face*`/`model_pack` → E13/E14). Rationale: the §3.1 table list is the contract, but its *column-level* design belongs to the epic planners who own each entity; freezing their DDL from E01 would be exactly the neighboring-epic design this spec must not do. The `rating`/`flag`/`label` columns live on `image` (per §3.1 "on `image`") and are included now because E01's command bus needs one real mutating command and the columns are architecture-fixed.

**Migration mechanics:** embedded SQL files (`migrations/0001_spine.sql`, …), applied in order inside a single transaction each, recorded in `schema_version`; forward-only (no down-migrations — restore-from-backup is the rollback story, §6). Schema upgrades on catalogs from older builds run **copy-on-write** (§6): the original file is copied to `backups/pre-upgrade-<ver>/` before the first pending migration runs. A build refuses to open a catalog with `schema_version > SUPPORTED` (clear message, no write). `docs/plan/migrations.md` is the committed **migration-number registry** — one line per reserved number, first-come via PR, so parallel epics never collide.

### 4.2 Connection configuration (every connection)

```sql
PRAGMA journal_mode = WAL;            -- set at create; persistent
PRAGMA synchronous  = NORMAL;         -- §3.1 invariant: safe vs kill -9, torn writes impossible
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store   = MEMORY;
-- readers additionally: PRAGMA query_only = ON;
```

The SQLite amalgamation is **bundled** via `rusqlite`'s `bundled` feature (§1.4: no system dependency; public domain).

### 4.3 `0001_spine.sql`

```sql
CREATE TABLE schema_version (
  version     INTEGER PRIMARY KEY,
  applied_at  TEXT    NOT NULL,                 -- RFC3339 UTC
  description TEXT    NOT NULL
);

CREATE TABLE library_root (
  id           INTEGER PRIMARY KEY,
  volume_uuid  TEXT,                            -- NULL when the platform can't provide one
  path         TEXT    NOT NULL,                -- absolute, platform-native, UTF-8
  UNIQUE (volume_uuid, path)
);

CREATE TABLE folder (
  id        INTEGER PRIMARY KEY,
  root_id   INTEGER NOT NULL REFERENCES library_root(id),
  parent_id INTEGER REFERENCES folder(id),
  rel_path  TEXT    NOT NULL,                   -- '' for the root folder itself; '/'-separated
  UNIQUE (root_id, rel_path)
);
CREATE INDEX folder_parent ON folder(parent_id);

CREATE TABLE import_session (
  id          INTEGER PRIMARY KEY,
  started_at  TEXT NOT NULL,
  finished_at TEXT,
  source      TEXT NOT NULL,                    -- source dir/device description
  mode        TEXT NOT NULL DEFAULT 'add',      -- M0: 'add' only; E04 adds copy/move/dng
  options     TEXT NOT NULL DEFAULT '{}',       -- JSON (ImportOptions is #[non_exhaustive])
  stats       TEXT                              -- JSON ImportReport
);

CREATE TABLE asset (
  id                INTEGER PRIMARY KEY,
  folder_id         INTEGER NOT NULL REFERENCES folder(id),
  filename          TEXT    NOT NULL,           -- as on disk, NFC-normalized for match/display
  content_hash      BLOB    NOT NULL,           -- 16 bytes, xxh3-128 of the full file
  format            TEXT    NOT NULL,           -- 'CR3','NEF','JPEG',…,'UNSUPPORTED'
  camera_make       TEXT,
  camera_model      TEXT,
  capture_time      TEXT,                       -- RFC3339; lexicographic == chronological (UTC-normalized)
  width             INTEGER NOT NULL DEFAULT 0,
  height            INTEGER NOT NULL DEFAULT 0,
  orientation       INTEGER NOT NULL DEFAULT 1, -- EXIF 1..8
  bytes             INTEGER NOT NULL,
  mtime_utc         TEXT,
  missing           INTEGER NOT NULL DEFAULT 0, -- bool; fs-reconciliation is E07
  decode_error      TEXT,                       -- NULL = fine; message otherwise (§6)
  import_session_id INTEGER REFERENCES import_session(id),
  added_at          TEXT    NOT NULL,
  UNIQUE (folder_id, filename)
);
CREATE INDEX asset_hash    ON asset(content_hash);   -- dup-skip + relink + cache keys
CREATE INDEX asset_capture ON asset(capture_time);
CREATE INDEX asset_session ON asset(import_session_id);

CREATE TABLE image (
  id              INTEGER PRIMARY KEY,
  asset_id        INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
  is_virtual      INTEGER NOT NULL DEFAULT 0,   -- VCs (E07) fall out of this row model
  name            TEXT,                         -- virtual-copy name
  orientation     INTEGER,                      -- NULL = inherit asset.orientation
  rating          INTEGER CHECK (rating BETWEEN 1 AND 5),
  flag            INTEGER NOT NULL DEFAULT 0,   -- -1 reject / 0 none / 1 pick
  label           TEXT,
  process_version INTEGER NOT NULL DEFAULT 1,   -- §4.5: immutable per image w/o explicit migrate
  created_at      TEXT    NOT NULL
);
CREATE INDEX image_asset ON image(asset_id);

-- FTS5, external-content over asset. M0 columns: filename + camera. E07 expands
-- (keywords, caption) with a migration that rebuilds the index — designed for that.
CREATE VIRTUAL TABLE assets_fts USING fts5(
  filename, camera,
  content='asset', content_rowid='id',
  tokenize = "unicode61 remove_diacritics 2"
);
CREATE TRIGGER asset_fts_ai AFTER INSERT ON asset BEGIN
  INSERT INTO assets_fts(rowid, filename, camera)
  VALUES (new.id, new.filename,
          trim(coalesce(new.camera_make,'') || ' ' || coalesce(new.camera_model,'')));
END;
CREATE TRIGGER asset_fts_ad AFTER DELETE ON asset BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, filename, camera)
  VALUES ('delete', old.id, old.filename,
          trim(coalesce(old.camera_make,'') || ' ' || coalesce(old.camera_model,'')));
END;
CREATE TRIGGER asset_fts_au AFTER UPDATE OF filename, camera_make, camera_model ON asset BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, filename, camera)
  VALUES ('delete', old.id, old.filename,
          trim(coalesce(old.camera_make,'') || ' ' || coalesce(old.camera_model,'')));
  INSERT INTO assets_fts(rowid, filename, camera)
  VALUES (new.id, new.filename,
          trim(coalesce(new.camera_make,'') || ' ' || coalesce(new.camera_model,'')));
END;
```

Notes: (a) the `embedding` sqlite-vec virtual table is **not** created here — sqlite-vec is a loadable extension E14 ships; creating its virtual table before the extension exists would break `open()`. (b) `capture_time` is stored RFC3339-UTC so `asset_capture` gives correct chronological keyset pagination; timezone-preserving display is E07's `metadata_cache` concern. (c) Non-UTF-8 paths are rejected at import at M0 with a per-file error in `ImportReport` (see open question OQ-3).

### 4.4 On-disk layout created by E01 (subset of §3.3)

```
<catalog-name>.lbdata/
  catalog.sqlite (+ -wal, -shm)
  backups/
    2026-07-04-183000/catalog.sqlite.zst     # verified, dated, pruned (retain 10)
    pre-upgrade-<ver>/catalog.sqlite         # copy-on-write schema-upgrade safety copy
  # previews/, rawcache/, smartpreview/, masks/, models/, thumbcache.sqlite → E03/E13
```

---

## 5. Ordered task breakdown

Every task ≤ 1 engineer-day. Ordering is **risk-first**: the zero-copy seam tracer bullet (Phase 2) lands in week 1, per §9's rationale for the skeleton's shape. Two-engineer split: A = catalog/core/ingest (Phases 3–4, T27), B = render/shell/previews (Phases 2, 5–7); Phase 1 and 8 are shared.

### Phase 1 — Workspace & guardrails (4 d)

**T1. Cargo workspace + crate skeletons + `lightbox-types`.**
Workspace `Cargo.toml` (shared lints: `unsafe_code = "deny"` outside `lightbox-render`/FFI-to-be, `clippy::all` = deny at CI), `rust-toolchain.toml` (pinned stable, MSRV recorded), all §2 crates + stubs with crate-level docs naming owning epics, `lightbox-types` implemented (§3.1).
*AC:* `cargo build --workspace` and `cargo test --workspace` green locally; stub crates compile empty; id newtypes round-trip serde.

**T2. CI PR gate: 3-OS matrix.**
GitHub Actions: {macos, windows, ubuntu} × {build, test, fmt --check, clippy -D warnings}. Ubuntu runner installs Mesa (lavapipe) + sets `VK_ICD_FILENAMES` so wgpu has a software Vulkan adapter; Windows relies on DX12 WARP; macOS uses Metal. Cache: cargo registry + target.
*AC:* a PR failing fmt/clippy/test on any OS is blocked; a smoke test that requests a wgpu adapter passes on all three runners (software adapters count).

**T3. License gate — CI surface 1.**
`deny.toml`: allowlist MIT/BSD-2/BSD-3/Apache-2.0/Zlib/Unicode/ISC/CC0; **deny GPL/AGPL/LGPL in the crate graph** (LGPL enters only via dynamic FFI in later epics, never as a crate — §1.6); `cargo deny check licenses bans sources` in CI; REUSE/SPDX headers on all files + check. Document the policy in `docs/plan/licensing.md` pointing at §8 (surfaces 2/3 = E16, empty inventory until native deps arrive).
*AC:* CI fails on a branch that adds a GPL-licensed dev-dependency (verified once, screenshot in PR); REUSE lint green.

**T4. Observability + fixtures.**
`tracing` + `tracing-subscriber` (env-filter; file log in `.lbdata/logs/` rotating), error taxonomy (`thiserror` per crate, `anyhow` only in bins), panic hook that logs + flushes. `cargo xtask fixtures`: downloads the pinned fixture corpus (≈12 CC0 raws from raw.pixls.us covering CR2/CR3/NEF/ARW/RAF/ORF/DNG + JPEG/TIFF/PNG + 2 deliberately truncated/corrupt files), verifies xxh3 pins from committed `fixtures/manifest.toml` (URL + hash + license per file — provenance discipline from day one), caches locally and in CI.
*AC:* `cargo xtask fixtures` idempotent + offline-safe once cached; manifest lists license per fixture; corrupt fixtures present.

### Phase 2 — Zero-copy seam tracer bullet (3 d)

**T5. wgpu bootstrap: `GpuContext` shared and headless.**
Construct `GpuContext` two ways: (a) from eframe's `WgpuConfiguration`/`RenderState` (shell path — verify we can set required features/limits via the device descriptor callback); (b) own instance→adapter→device (headless path), returning `None` gracefully when no adapter. Adapter report (backend, name, limits) logged and exposed.
*AC:* a minimal eframe window and a headless test both obtain a device on all 3 CI OSes; no-adapter environment degrades to `None` without panic.

**T6. Engine seed: registry + ticket lifecycle + coalescing.**
`NodeRegistry`, `Engine::new/submit/poll/cancel`, per-`ViewportId` latest-wins coalescing (newer submit ⇒ older ticket → `Superseded`), texture pool (recycle output textures by size), `on_device_lost` registration (M0 degenerate behavior per §3.4). Test node: `solid.color` fills a texture.
*AC:* unit tests — submit→poll reaches `Ready`; two rapid submits on one viewport: first is `Superseded`, exactly one GPU eval ran (probe counter); cancel before run ⇒ `Cancelled`, no eval.

**T7. Shell compositing spike (the seam-2 proof).**
eframe app: engine texture registered via `egui_wgpu::Renderer::register_native_texture` → drawn with `ui.image` (re-register on texture swap; `PaintCallback` documented as the E05/E08 upgrade path for tiling/gizmos). Window resize + DPI handled; texture updates flicker-free.
*AC:* running app shows the `solid.color` node output produced by `Engine::submit` on the **same device** — verified by asserting device pointer equality in a debug assertion, and **no** `map_async`/CPU readback anywhere in the frame path (code-review checklist item). This closes the scariest unknown in week 1.

### Phase 3 — Catalog (6 d)

**T8. Open/create + WAL + writer/readers + migration runner.**
`Catalog::create/open`, pragmas per §4.2, dedicated writer thread with `with_txn` marshalling, reader pool (N = min(cores, 4), `query_only`), `quick_check` on open, migration runner (embedded SQL, one txn per migration, `schema_version` recording, newer-schema refusal, copy-on-write pre-upgrade backup hook).
*AC:* create→open idempotent; concurrent reader during a long write txn sees pre-txn snapshot (WAL proof test); catalog with `schema_version = 999` refuses with a clear error; migration applied exactly once across reopen.

**T9. Migration `0001` + root/folder DAOs.**
The §4.3 DDL; `upsert_root` (volume UUID via platform APIs where available, else NULL), `upsert_folder` w/ parent chain creation; `folder_tree()`. `docs/plan/migrations.md` registry committed with `0001` reserved.
*AC:* DAO unit tests incl. FK enforcement, duplicate upsert idempotence, `''` root folder convention; registry lint (xtask) fails on duplicate migration numbers.

**T10. Asset/image DAOs + keyset pagination + 100k proof.**
`insert_assets` (batched, dup-skip on `content_hash`, returns `InsertOutcome`), `insert_default_images`, `images_page` with keyset cursors for all `SortOrder`s, `counts`, `image_detail`, `asset_abs_path`. Synthetic-data generator (100 k assets).
*AC:* every `images_page` query plan uses an index (asserted via `EXPLAIN QUERY PLAN` in tests — no full scans); p95 page fetch < 10 ms at 100 k on a dev laptop (criterion bench, informal at PR, formal in T28); pagination stable under concurrent inserts (no skips/dupes across pages).

**T11. FTS5 + filename search.** (0.5 d, pairs with T10 day)
Triggers per §4.3; `search_filenames` with prefix queries; FTS kept consistent under insert/delete/update property test.
*AC:* proptest — arbitrary insert/delete/rename sequences keep FTS row-parity with `asset`; diacritic-insensitive match verified.

**T12. Integrity + verified backup + retention.**
`integrity()`; `backup_verified()` per §3.2 sequence (online-backup → `integrity_check` **on the copy** → zstd → temp+rename → prune to `retain`); `check`/`backup` surfaced in CLI later (T27).
*AC:* backup taken while a writer hammers txns restores (unzstd → open) with `integrity_check` clean and contains a consistent snapshot; prune keeps newest N; a failed integrity check on the copy aborts the backup leaving prior backups untouched (atomicity via temp+rename verified with injected failure).

**T13. `kill -9` fault-injection harness (the Risk-5 gate).**
Test binary: child process performs randomized small txns (imports, rating writes) in a tight loop; parent SIGKILLs it at random intervals; reopen → `quick_check` must pass and the last *committed* txn must be present (child journals committed ids to a side file for verification). PR gate: 50 iterations; nightly: 1 000.
*AC:* 0 corruptions and 0 lost-committed-txns across the nightly run; harness wired as `#[test]` (PR subset) + nightly workflow job; a deliberately-broken variant (`synchronous=OFF`, journal_mode=DELETE + power-cut simulation doc) documented as the negative control we do **not** ship.

### Phase 4 — Jobs & core façade (5 d)

**T14. `lightbox-jobs` seed.**
`JobSystem` on tokio multi-thread; per-`Class` semaphore budgets (Interactive unbounded-small, Foreground/Background bounded); `CancelToken` (hierarchical); `spawn`/`spawn_blocking`/`JobHandle`; bounded mpsc between stages.
*AC:* cancelled blocking job observes token within 50 ms (cooperative checkpoints); Background saturation does not delay an Interactive spawn (test with a probe timer); handle `try_result` never blocks.

**T15. `Core`/`Session` lifecycle + exit backup.**
`Core::start`, `create/open_catalog` (wires Catalog + JobSystem + Engine + `EmbeddedPreviewProvider` into a `Session`), `close()` runs exit-time `backup_verified` (policy: on close, unless last backup < 24 h or opts skip), `CloseReport`.
*AC:* headless test — create, close, reopen, close; kill -9 *during* the close-time backup leaves the previous backup set intact and the catalog clean (atomic temp+rename inherited from T12).

**T16. Command bus + events + queries.**
`Command` dispatch onto the writer (each command = one WAL txn), `CommandTicket`, `Event` broadcast, `Queries` façade wrapping `ReaderHandle`, `CatalogChanged` coarse invalidation events. `SetRating`/`SetFlag` implemented as the canonical trivial commands.
*AC:* integration test (no shell): submit `SetRating` → observe `CatalogChanged` → `images_page` reflects it; a failing command emits `CommandFailed` and leaves no partial txn (verified by inspection query); events dropped under a slow subscriber don't wedge the writer (broadcast lag semantics tested).

**T17. `ImportAddInPlace`.**
In `lightbox-ingest` as reusable primitives: `walkdir` discovery (filter by known extensions), per-file probe (T19 stub returns `Unsupported` until Phase 5 lands — task order tolerated) + `hash_file`, batched `insert_assets` (batch = 64 files/txn), `import_session` bracketing, `ImportProgress` events (throttled ≤ 10 Hz), cooperative cancellation between files, Foreground class.
*AC:* import of the fixture corpus lands correct rows; re-import of the same dir ⇒ `imported=0, skipped_duplicates=n`; cancel mid-import commits completed batches only (no partial rows), session marked finished-with-stats; event-loop heartbeat test shows no core-thread stall > 16 ms during a 1 k-file import against synthetic files.

**T18. Undo-import + failure cataloguing.**
`UndoImport` (session rows removed, files untouched, `CatalogChanged` emitted); probe/hash failures catalogue the asset with `decode_error` set + `ImportReport.errors` entry; unsupported formats catalogued with `format='UNSUPPORTED'` (badged in grid).
*AC:* undo restores exact pre-import row counts (FTS included, via T11 property harness); corrupt fixture files import as `decode_error` rows without aborting the batch; app-level crash does not occur for any file in the malformed corpus.

### Phase 5 — Probe & previews (3 d)

**T19. `lightbox-decode::probe`.**
rawler metadata path for raws (dims, make/model, capture time, orientation, embedded-preview ranges); JPEG/TIFF/PNG via header parsing + `kamadak-exif`; `ProbedFormat::Unsupported` fallback; `hash_file` streaming xxh3-128 with cancellation.
*AC:* probe of every fixture returns expected values (committed expectations file); malformed corpus (truncated/garbage) returns `Err`, never panics (also seeds a `cargo-fuzz` target, fuzzing itself is not CI-gating at M0); probe of a 45 MB raw < 20 ms (metadata-only, no full read — except hashing, which is separate and streamed).

**T20. Embedded preview extraction + decode.**
`read_embedded` (ranged read), zune-jpeg decode → RGBA8, EXIF-orientation application for thumbnails (CPU, via `fast_image_resize`-compatible layout), downscale to requested class size.
*AC:* largest embedded preview of each fixture decodes to expected dims; decode+resize of a typical 8 MP embedded JPEG to 512 px < 25 ms on a dev laptop (bench, informational); raws with no/tiny embedded preview yield `PreviewError::NoEmbedded` → grid placeholder (E03 will render real previews).

**T21. `EmbeddedPreviewProvider`.**
Trait impl per §3.6: request dedup (same image+class shares one job), byte-capped LRU (default 256 MiB, config in `CoreConfig`), thumb requests on `Class::Background`, loupe source on `Class::Interactive`, cancel wired through.
*AC:* 500 rapid thumb requests with immediate cancels leak nothing (job counters return to zero; LRU ≤ cap); cache hit poll returns `Ready` on first poll; concurrent duplicate requests execute one decode (probe counter).

### Phase 6 — The real node & goldens (3 d)

**T22. `lbx-image-compare` + golden conventions.**
CIEDE2000 (through Lab from sRGB) + PSNR comparator; report (mean/p99/max ΔE, PSNR, diff heatmap PNG on failure); golden layout `crates/lightbox-render/goldens/<node>/<pv>/<case>.png` (committed — tiny at M0; LFS decision deferred to E05, OQ-4); bless flow: `LIGHTBOX_BLESS=1 cargo test` regenerates + fails CI if run there.
*AC:* harness flags an injected 2° hue rotation and passes an injected ±1 LSB dither (tolerance sanity in both directions); failure artifact (heatmap) uploaded in CI.

**T23. `DisplayTransformNode` — GPU.**
WGSL compute per §3.4 (sRGB decode, orientation, bilinear resample in linear, sRGB encode); algorithm spec committed as `display_transform.md`; registered `(NodeId("display.transform"), PV_M0)`; `SourceResolver` M0 impl over `EmbeddedPreviewProvider`; Engine renders fixture previews end-to-end.
*AC:* golden test per fixture orientation case (1,3,6,8 at minimum): max ΔE2000 ≤ 1.0 **and** PSNR ≥ 45 dB vs committed goldens on every CI OS with an adapter.

**T24. `DisplayTransformNode` — CPU + parity.**
`eval_cpu` (rayon, same algorithm spec, identical filter coordinates); Engine CPU-only mode (no adapter) renders through the same ticket lifecycle; parity test GPU-vs-CPU.
*AC:* CPU output vs GPU output within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on all OSes (GPU side skipped gracefully where CI lacks an adapter — CPU-vs-golden still runs everywhere); `lightbox-cli render --cpu` byte-stable across two runs (determinism per §4.4).

### Phase 7 — Shell & CLI (3 d)

**T25. Virtualized grid MVP.**
Fixed-size cells (size slider), only visible rows queried/materialized (windowed `images_page` around the scroll position), demand-driven `PreviewProvider` requests with **cancel-on-scroll-out**, placeholder → thumbnail upgrade-in-place, selection (single + shift-range seed), unsupported/decode-error badges.
*AC:* 10 k-image catalog scrolls with p95 frame time < 16 ms on a dev laptop (egui frame-time probe in a debug overlay); scrolling fast issues ≤ visible-count preview jobs (cancellation asserted via job counters); memory stays under LRU cap + O(visible) textures.

**T26. Loupe through the Engine.**
Double-click/Enter → loupe; submits `RenderRequest { recipe: Recipe::identity(PV_M0), scale: FitWithin(viewport), viewport }` on every relevant change (resize, nav) — latest-wins; fit / 100 % toggle / drag pan (100 % re-renders at `Native` scale from the embedded preview — honest about M0 source resolution); ←/→ navigation; G/E view toggle; minimal info overlay (filename, dims, tier badge "embedded preview").
*AC:* the composited loupe texture is the Engine ticket's output (T7 assertion reused); next/prev over cached previews swaps < 50 ms p95 (probe); resize during in-flight render never shows a stale-size frame (superseded tickets discarded); zero CPU readback in the frame path.

**T27. `lightbox-cli` + E2E.**
Subcommands per §3.10 over `lightbox-core` only (proves seam 1: the CLI compiles with **no** dependency on `lightbox-shell`/egui). E2E integration test in CI: `create → import fixtures → list --json → render → check → backup`, asserting exit codes, row counts, and that `render` output passes golden compare.
*AC:* E2E green on all 3 CI OSes (CPU render path); `check` exit code distinguishes ok/corrupt (tested against a bit-flipped catalog file); CLI has no egui/winit in its dependency tree (`cargo tree` assertion in CI).

### Phase 8 — Proof & exit (2 d)

**T28. Perf scenario harness (nightly).**
Criterion micro-benches (images_page @100 k, insert batch, hash throughput) + scenario runner: import-1k (synthetic + fixture mix) wall time, grid-scroll frame-time capture (headless egui harness or scripted app), loupe next/prev latency. Baselines committed as JSON; nightly workflow compares (regression ⇒ tracked issue, non-blocking per §8).
*AC:* nightly job runs and publishes a table; §7-relevant M0 numbers recorded: import-1k browsable time, page-query p95, nav-swap p95.

**T29. M0 exit drill + handoff.**
On each of the 3 platforms with real hardware (not CI): import 1 k real raws, browse grid + loupe, `kill -9` mid-import → reopen clean, run the demo script. Write the handoff notes: frozen-surface list (traits in §3), migration registry state, fixture manifest, known limitations (sRGB-only display, embedded-preview-only loupe) → linked from the E02/E03/E04/E05/E06/E07/E08/E09 planning docs.
*AC:* the §9 M0 exit criteria (reproduced in §8 DoD below) all check off, witnessed in the demo recording; handoff doc merged.

**Roll-up:** 29 tasks ≈ 27–29 d ≈ 5.5 pw single-engineer worst case; with the 2-engineer split, ~3.5–4 wall-clock weeks. Within the M (~4–6 pw) envelope.

---

## 6. Test plan (per §8 strategy)

| Layer (§8) | E01 tests | Gate |
|---|---|---|
| Per-crate unit | Catalog DAOs, migration runner, keyset pagination, FTS triggers, probe expectations, LRU/dedup, CancelToken, NodeRegistry, ticket lifecycle | PR-blocking |
| Property | FTS row-parity under arbitrary mutate sequences (T11); pagination completeness/no-dupes under interleaved inserts (T10); `ContentHash` hex round-trip | PR-blocking |
| **Golden-image** | `DisplayTransformNode` vs committed goldens, per orientation case, keyed by `(node, pv)` — the per-PV golden scaffold every pixel epic extends; max ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB | PR-blocking |
| **CPU/GPU parity** | Same node, CPU vs GPU within the §4.4 tolerance; CPU determinism (byte-stable repeat runs) | PR-blocking (GPU legs auto-skip where CI has no adapter; CPU-vs-golden runs everywhere) |
| **Catalog crash safety** | `kill -9` fault injection (T13): 50 iters PR / 1 000 nightly; restore-from-backup drill (unzstd → open → integrity) in the same harness | PR-blocking |
| Integration / E2E | `lightbox-cli` create→import→list→render→check→backup on 3 OSes; UI-free command-bus integration tests | PR-blocking (fast subset), full nightly |
| Performance | Criterion + scenario harness (T28): import-1k, 100 k page queries, grid frame times, nav swap | Nightly, regression-tracked (non-blocking per §8) |
| License surface 1 | `cargo-deny` licenses/bans/sources + REUSE | PR-blocking |
| Cross-platform | Full PR matrix on macOS/Windows/Linux incl. golden subset | PR-blocking |
| Fuzz (seeded) | `cargo-fuzz` target for `probe()` over the malformed corpus | Non-gating at M0; corpus grows in E02 |

Performance budgets E01 is accountable for (M0 slice of §7): import **1 k** raws with grid browsable during import and no UI stall; keystroke/nav preview swap < 50 ms p95 (from cache); catalog page queries indexed and < 100 ms p95 at 100 k synthetic; **0** corruptions under `kill -9`. The 10 k-import < 60 s budget belongs to E04/M1; the < 100 ms slider budget belongs to E05/E10 — E01 only proves the ticket lifecycle they'll ride on.

---

## 7. Risks & open questions

### Risks

| # | Risk | Likelihood/Impact | Mitigation |
|---|---|---|---|
| R1 | **eframe device-sharing friction** — eframe controls device creation; Engine needs specific features/limits, and egui's renderer assumptions (texture formats, present mode) could fight the compositing path | Med / High (it's seam 2) | Tracer bullet in week 1 (T5–T7); `WgpuConfiguration` device-descriptor hook verified before anything is built on it; fallback documented: run our own instance and hand the device *to* eframe (eframe supports external wgpu setup); worst case is bounded to `lightbox-shell` (§1.2 reversibility) |
| R2 | **egui virtualized-grid ceiling** — immediate mode at 100 k rows is a build-it-ourselves widget (§1.2 known cost) | Med / Med | M0 grid only needs 10 k-smooth; windowed queries + texture LRU designed in; E08 owns the real widget and the §1.2 shell-swap reversal trigger stays live |
| R3 | **CI software adapters diverge from real GPUs** (lavapipe/WARP precision, missing features) | Med / Med | Golden tolerance is perceptual (ΔE), not bit-exact; T29 exit drill runs on real hardware on all 3 OSes; GPU-specific failures logged with adapter info |
| R4 | **rawler probe coverage gaps** (narrower than LibRaw — Risk 3 catalog) | Med / Low at M0 | Unsupported formats catalogue cleanly with badges; E02 owns the LibRaw sandbox fallback; fixture corpus spans 7 mounts to find gaps early |
| R5 | **Full-file xxh3 hashing cost at import** on slow media (30 GB/1 k raws) | Low / Med | Streamed, cancellable, overlapped with probe in Foreground jobs; measured in T28; E04 may add a fast-path (size+mtime) pre-filter — seam noted, not built |
| R6 | **Writer-thread closure API ergonomics** (`with_txn` + Send bounds) turning contributors toward raw SQL | Low / Med | DAO methods are the only public mutation surface; `rusqlite` types never escape the crate (enforced by privacy + a CI `cargo tree`/API-surface check) |
| R7 | **Scope creep from neighboring epics** ("just add collections while you're in there") | High / Med | Non-goals table (§1.1) is normative; migration registry forces additions through owning epics' PRs |
| R8 | **Procedural**: CEO/operator sign-off (§12) not on record when staffing starts | — / blocks start | Named as the epic's precondition; PM tracks it as task 0 |

### Open questions

- **OQ-1 (E01 decision, review invited):** spine-only schema at `0001` vs full §3.1 DDL. Decided spine-only (§4.1 rationale); if the CTO prefers full-schema-day-one, the delta is mechanical but transfers column-design authority from epic planners to E01 — flag at review.
- **OQ-2:** migration-number coordination across parallel epics — proposed: committed registry file + xtask lint (T9). Needs a nod from E03/E07/E09 planners.
- **OQ-3:** non-UTF-8 paths (possible on Linux) are rejected per-file at import at M0. Acceptable long-term? E04/E07 may want a `BLOB` path column + lossy display name. Decision owner: E04 planner.
- **OQ-4:** golden PNG storage — in-repo at M0 (KBs); decide plain-git vs LFS when E05/E10 corpora grow to MBs. Decision owner: E05 planner, with infra.
- **OQ-5:** `Session::query()` on the UI thread — fine at M0 scale (<1 ms reads); if E08's 100 k grid profiling disagrees, move to prefetched async pages behind the same `Queries` façade (no API break). Decision owner: E08 planner with E01 handoff data.
- **OQ-6:** exit-backup policy default (on close if last backup > 24 h, retain 10) — product call, cheap to change; confirm with product before M1.
- **OQ-7:** macOS NFD vs NFC filename normalization — E01 stores NFC-normalized `filename` for display/match and reads via the OS-native form; verify against a stress fixture (task T19 includes a normalization case). If mismatches surface, E07's relink work inherits the fix.

---

## 8. Definition of done

E01 is done when **all** of the following are true (this is §9 M0's exit criteria plus this spec's gates):

1. **M0 exit (verbatim §9):** import 1 k raws; browse grid + loupe with no UI stall; **the loupe image is produced by `Engine::submit` returning a texture composited zero-copy in the egui frame**; `kill -9` mid-import leaves the catalog `integrity_check`-clean. Demonstrated on real hardware on macOS, Windows, and Linux (T29 drill).
2. **All PR gates green on the 3-OS matrix:** build/test/fmt/clippy, cargo-deny + REUSE (surface 1), golden-image + CPU/GPU parity for `display.transform`, fault-injection PR subset, CLI E2E.
3. **Nightly gates running:** 1 000-iteration fault injection at 0 corruptions; perf scenario harness publishing baselines for import-1k, 100 k page-query p95 (< 100 ms), nav-swap p95 (< 50 ms).
4. **Frozen surfaces documented and consumed headlessly:** `lightbox-cli` drives create→import→list→render→backup→check with zero shell dependencies (seam 1 proof); the trait/type surfaces in §3 are marked `#[non_exhaustive]`/documented as frozen, and the handoff note (T29) is linked from each dependent epic's planning doc.
5. **Catalog invariants hold:** every mutation is a single WAL txn; exit-time verified backup produces a dated, integrity-checked, pruned `.zst`; a corrupt catalog is refused at open with the newest verified backup named; copy-on-write upgrade path exercised by a test that migrates a `0001`-only catalog forward with a synthetic `0002`.
6. **No native C dependencies and no bundled content shipped** (keeps license surfaces 2/3 legitimately empty until E02+), verified by the SBOM-inventory placeholder check in CI.
7. **Effort honesty:** actuals recorded against the T1–T29 plan in the handoff note, so E02–E09 planners can calibrate their own estimates.

---

*Seam summary for reviewers: E01 freezes — `PreviewProvider` (E03), ingest primitives + `ImportOptions` (E04), `RenderNode`/`NodeRegistry`/`Engine` ticket lifecycle + `SourceResolver` (E02/E05), `Class`/`CancelToken`/`spawn` (E06), spine tables + migration registry (E07/E09/E12/E14), `Recipe{schema,pv}` placeholder (E09), and the headless `Session` command/query/event boundary (E08 + everyone). Changes to any frozen surface after M0 require a joint review with the owning epic's planner.*
