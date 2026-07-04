# E03 — Preview pyramid & raw cache

_Implementation spec. Milestone **M0** (store + embedded-preview path; rendered producers plug in at M1). Effort **M ~3–5 pw**. Depends on **E01**. Author: staff engineer, from `docs/plan/01-architecture.md` (decision-complete; not re-litigated here) and research reports 06/09._

**One-paragraph thesis.** E03 builds the cache subsystem that makes the library feel instant and the Develop module open fast: the three-tier preview pyramid (T0 embedded / T1 standard / T2 1:1 tiled) in a relocatable, size-capped, content-hash-keyed store (§3.3 of the architecture), the partially-demosaiced **raw cache** (default 5 GiB LRU), the **demand-driven build scheduler** (visible-first, coalescing, cancellable), and the hot **thumbnail atlas**. At M0 the only preview *producer* is embedded-JPEG extraction — which is exactly what the M0 walking skeleton requires (grid + loupe over decoded embedded previews, fed into E01's single display-transform `RenderNode`). Everything that renders pixels from raws (E02/E05) plugs into E03's frozen producer/consumer traits at M1 without changing this crate's shape.

---

## 1. Scope

### 1.1 In scope (E03 owns)

1. **The `.lbdata` cache store** — directory layout per §3.3, store manifest, two-level hash fan-out, atomic temp-then-rename blob I/O, checksums, orphan/torn-file reconciliation, relocation, purge, size caps + LRU eviction, disk-full behavior.
2. **Preview pyramid, all three tiers:**
   - **T0** — the camera-embedded JPEG, stored verbatim (`previews/<hh>/<hash>.t0.jpg`), flagged `source='embedded'` so the UI can badge it (it does not reflect edits — Lightroom's "Embedded & Sidecar" convention).
   - **T1** — standard preview sized to the display (`<hash>.t1.jxl`). At M0, built by downscaling the embedded JPEG (still `source='embedded'`). From M1, rebuilt by the render engine (`source='rendered'`) via the producer trait.
   - **T2** — 1:1 tiled preview (`<hash>.t2/<tile>.jxl`, 256² tiles per §4.3). E03 ships the tile store, addressing, manifest, and retention; the producer is E05 (M1+). Exercised at E03 time by a synthetic producer in tests.
3. **Embedded-preview extraction at M0** (rawler metadata path + EXIF fallback, orientation handling). This is metadata-level parsing, **not** raw decode — it keeps E03 free of any E02 dependency, per §9's M0 note. Extraction ownership migrates behind `lightbox-decode::probe()` when E02 lands (seam, §10).
4. **Decode-for-display path** — decode a stored preview to an upright RGBA8 CPU buffer (`DecodedPreview`) that `lightbox-core` uploads for E01's display-transform node; plus an in-RAM decoded LRU and neighbor prefetch that deliver the < 50 ms cull-swap budget.
5. **Raw decode cache** (`rawcache/<hh>/<hash>.<params>.zst`) — container format, content-hash + params-hash keying, zstd compression, checksum verification, catalog-indexed accounting, 5 GiB default LRU cap, purge, relocation, startup reconcile. The *payload semantics* (which pipeline stage is snapshotted) belong to E02/E05; E03 stores an opaque, schema-versioned planar payload.
6. **Demand-driven build scheduler** — priority classes (Visible > Neighbor > Bulk), request dedup/coalescing, bounded concurrency, cooperative cancellation, bounded queues with backpressure (the E04 import-throttle seam), bulk "build previews for selection" with progress events.
7. **Catalog schema additions** for the preview index and raw-cache accounting (migration + DAO in `lightbox-catalog`).
8. **`thumbcache.sqlite`** — the hot thumbnail atlas for grid scroll (§3.3).
9. **Generic content-addressed `BlobStore` namespaces** inside `.lbdata` — the mechanism E14 later uses for baked AI-mask rasters (`masks/<hh>/<hash>.png`, §4.5) and E16 for `smartpreview/`. E03 ships the API and reserves the directories; it does not implement those features.
10. **Core façade additions** — commands/queries/events (`BuildPreviews`, `DiscardPreviews`, `SetCacheLimits`, `RelocateCacheStore`, `PurgeCaches`, `CacheStats`, `PreviewReady`, …) and `lightbox-cli` subcommands for headless testing.

### 1.2 Explicit non-goals (named seams, not designs)

- **No raw decode, demosaic, or color science.** Rendering T1/T2 from raws is E02 + E05; E03 freezes the `PreviewProducer` trait they implement. The raw-cache *payload schema* (what stage is cached, channel semantics) is owned by E02/E05 — E03 treats it as opaque bytes + declared geometry.
- **No smart previews** (M4 / E16). E03 reserves the `smartpreview/` namespace and keeps schema headroom (`tier` is an integer, `source` is extensible); nothing else.
- **No job classes, pause, or activity-center UI** (E06). E03 defines a minimal `BuildRuntime` seam; at M0 it is backed by plain tokio inside `lightbox-core`, at M1 E06 supplies the `Class::Background` adapter with pause support. E03's progress events are shaped for the activity center but E03 ships no UI.
- **No import pipeline** (E04): E03 exposes bulk enqueue + backpressure; E04 decides when/what to enqueue, copy semantics, second-copy, etc.
- **No grid/loupe/filmstrip widgets or preferences panel** (E08). E03 exposes `best_available`/`open_pixels`/viewport hints and the limits/relocate/purge API that E08's performance preferences panel binds.
- **No mask raster or model-pack content** (E14/E13) — only the `BlobStore` namespace mechanism.
- **No video cache** (Could-tier, out of v1 scope for E03).
- **No XMP / metadata handling** of any kind.
- **No GPU code.** E03 produces CPU buffers and encoded files; upload/compositing is E01/E05/E08 territory.

### 1.3 Delivery profile across milestones

Everything in §1.1 lands in **M0**. Two integration points activate later without E03 code changes: the engine-backed producer for rendered T1/T2 and the raw-cache put/get calls from the Develop open path (both E05/E02 at M1, coding against traits frozen here). `invalidate_image()` (recipe-revision staleness) is implemented and tested at E03 time with synthetic revisions, since real edits arrive with E09/E05.

---

## 2. Crates / modules touched

| Crate | Change |
|---|---|
| **`lightbox-preview`** (new — owned by E03) | Modules: `store` (layout, manifest, atomic IO, `BlobStore`), `pyramid` (tiers, keys, variants), `extract` (embedded extraction), `codec` (`PreviewCodec`: JPEG via mozjpeg, JXL via own thin libjxl binding + jxl-oxide decode), `decode` (decode-for-display, orientation, decoded LRU), `sched` (priority scheduler, tickets, backpressure), `rawcache` (container, accounting, LRU), `thumbs` (thumbcache.sqlite), `service` (`PreviewService` facade), `verify` (reconcile/fault handling) |
| **`lightbox-catalog`** | One migration (preview index rework + `raw_cache_entry`), DAO modules `preview_dao`, `rawcache_dao` with batched-touch support |
| **`lightbox-core`** | Register `PreviewService` in the session; command/query/event façade additions (§5.6); M0 `BuildRuntime` impl over tokio; wire `DecodedPreview` into the existing E01 display-transform node path |
| **`lightbox-cli`** | `preview build/stat/purge/verify`, `rawcache stat/purge` subcommands for headless integration tests |
| **New third-party deps** | `rawler` (MIT, metadata/preview extraction only at M0), `kamadak-exif` (BSD-2), `zune-jpeg` (MIT, decode), `mozjpeg` (IJG/BSD-ish, encode), `fast_image_resize` (MIT), `zstd` (MIT bindings / BSD zstd), `xxhash-rust` (MIT), `jxl-oxide` (MIT/Apache, decode), **own minimal `libjxl` encode FFI** (libjxl BSD-3, static; see Risk R1 — the existing `jpegxl-rs`/`jpegxl-sys` bindings are GPL-3 and are **banned**), `tokio-util` (MIT, `CancellationToken`) |

All new native artifacts (libjxl, mozjpeg, zstd) get SBOM surface-2 entries per §8; no LGPL/GPL is introduced by this epic.

---

## 3. Design

### 3.1 Tiers, scope, and keying

| Tier | Content | Scope | Reflects edits? | Store path |
|---|---|---|---|---|
| **T0** | verbatim embedded camera JPEG (largest available) | **asset** (shared by all virtual copies) | no — badged `embedded` | `previews/<hh>/<key>.t0.jpg` |
| **T1** | standard preview, display-sized long edge | **image** | M0: no (`embedded`); M1+: yes (`rendered`) | `previews/<hh>/<key>.t1.jxl` |
| **T2** | 1:1 native-resolution, 256² tiles | **image** | yes (`rendered` only) | `previews/<hh>/<key>.t2/<x>_<y>.jxl` + `manifest.cbor` |

- **Key derivation.** `key = hex(xxh3_128(content_hash ‖ scope_discriminator ‖ tier ‖ variant_hash))`; `<hh>` = first byte of `key`, hex. The dominant component is the asset's `content_hash` (xxh3-128 from E01's schema), which is what makes the store survive catalog moves and file relinks (§3.3 of the architecture). The catalog `preview` table remains the authoritative index mapping (asset|image, tier, variant) → store path; a store-side filename is never parsed to recover identity (except by the reconcile scan, which treats the manifest-recorded key as advisory).
- **`variant_hash`** = xxh3-64 over a canonical, versioned CBOR encoding of `VariantParams` (producer id + producer rev, long-edge px, codec, quality, `recipe_rev`, `process_version`, encoding version). Canonicalization is fixed-field-order and covered by committed golden hash vectors so it can never drift silently across platforms or releases.
- **Edit staleness (M1+ behavior, built now).** Rendered T1/T2 variants embed the `recipe_rev` they were rendered from. `invalidate_image(image, new_rev)` marks older rendered rows stale (they remain servable as "best available" until replaced — progressive loading never blanks, §4.3) and lets the scheduler enqueue a rebuild at the caller's priority. Embedded-source rows are never staled by edits; they are superseded, not invalidated.
- **Virtual copies** share T0 (asset scope) and get their own T1/T2 (image scope). Store files are deduplicated by key; eviction refcounts `store_path` across index rows before unlinking.

### 3.2 Store layout & lifecycle (owns `.lbdata` cache surfaces)

```
<catalog-name>.lbdata/
  store.toml                 # store manifest: format=1, store_uuid, created_by, relocated_from
  previews/<hh>/...          # §3.1 above
  rawcache/<hh>/<content_hash>.<params_hash>.zst
  smartpreview/              # RESERVED (E16)
  masks/                     # BlobStore namespace, consumed by E14 (§4.5 baked rasters)
  thumbcache.sqlite          # hot thumb atlas (disposable)
```

- **Atomicity:** every write is temp-file-in-same-directory → fsync → rename. A crash at any point leaves either the old state or the new state, never a torn visible file. (Same-dir temp keeps rename atomic on all supported filesystems; behavior on exFAT/network mounts is documented as best-effort — Risk R6.)
- **Caches are disposable by contract:** the catalog is the source of truth for *what should exist*; the store is reconstructible. `verify_store(Quick)` at open cross-checks manifest + spot-checks; `verify_store(Full)` (idle/background or CLI) detects orphan files (no index row → delete), missing files (row without file → drop row, optionally re-enqueue), and checksum failures (drop both, re-enqueue).
- **Caps & eviction:** total preview store capped (default 20 GiB), raw cache capped (default 5 GiB), both LRU by `last_used_at` with eviction order T2 → T1 → T0 (T0 is small and is the culling floor). T2 additionally has an age-based retention sweep (auto-discard after N days / never, default 30 — the LrC v14 convention). Eviction never unlinks a file with a live read handle (ref-counted `PreviewHandle`s; on Windows, deletion failures go to a deferred-delete queue retried on idle).
- **Relocation:** journaled copy-then-flip-then-delete (`relocated_from` recorded in `store.toml`), resumable after crash, concurrent reads served from the old root until the flip. Config points at the new root via the core prefs store (E08 binds the UI).
- **Accounting write pressure:** `last_used_at` touches are batched in memory and flushed to the catalog writer every ≤ 5 s or ≥ 64 entries. LRU is deliberately approximate — losing unflushed touches in a crash is harmless for a cache and keeps the single-writer (§5.1) uncontended.

### 3.3 Raw cache container

Filename: `rawcache/<hh>/<content_hash_hex32>.<params_hash_hex16>.zst`. The file is a **single zstd frame** (built-in xxh64 content checksum enabled, level 3 default) whose decompressed stream is:

```
[0..4)   magic "LBRC"
[4..6)   container_ver: u16 = 1
[6..8)   payload_schema: u16      // owned by E02/E05; opaque to E03
[8..16)  width: u32, height: u32
[16]     channels: u8
[17]     sample_format: u8        // 0=F16 1=F32 2=U16
[18..20) color_state: u16         // producer-defined tag (e.g. camera-RGB linear)
[20..28) payload_len: u64
[28.. )  planar payload (channels × width × height × sample_size)
```

`params_hash` = xxh3-64 of the producer's canonical early-stage parameter encoding (demosaic algorithm id, PV-relevant pre-WB params — defined by E02/E05; E03 only requires it be canonical and versioned). A checksum failure on `get` returns a typed miss, deletes the entry, and never surfaces as an error to the render path — a raw-cache miss is always recoverable by re-decoding.

### 3.4 Scheduling

- Three in-crate priorities: **Visible** (on-screen cells, loupe target), **Neighbor** (prefetch: ±2 filmstrip neighbors decoded into the RAM LRU, ±N grid rows), **Bulk** (import-triggered and "build previews for selection"). Visible work always dequeues first; a Visible request for an image with a queued Bulk build upgrades it in place (dedup by (image, tier, variant)).
- **Cancellation** is cooperative (`tokio_util::sync::CancellationToken`), checked between coarse steps (read / extract / decode / resize / encode / write). Scrolling cancels or demotes off-screen requests (visible-first, §5.3 of the architecture).
- **Backpressure:** the Bulk queue is bounded (default 4× worker count); `try_enqueue` returns `QueueFull` so E04's import loop throttles instead of ballooning memory (§5.3 "import throttles preview-build enqueue").
- **Concurrency:** worker pool bounded at `min(physical_cores, config)` for extraction/encode; raw-cache and T2 IO share the pool. All builds run on the `BuildRuntime` seam — tokio at M0, `lightbox-jobs` `Class::Background` (pausable) from M1 (E06).

### 3.5 Decode-for-display & the M0 loupe path

`open_pixels()` decodes a stored preview (zune-jpeg / jxl-oxide), bakes EXIF orientation, tags the colorspace (sRGB default; embedded ICC/EXIF colorspace honored as a tag — full color management is E02's), optionally downscales to the caller's target, and returns a `DecodedPreview` CPU buffer. `lightbox-core` uploads it and runs E01's display-transform `RenderNode` through the real `Engine::submit`/`poll` (§9 M0 — no blit bypass). A RAM LRU of decoded buffers (default 512 MiB) plus Neighbor prefetch delivers the < 50 ms cull swap.

---

## 4. Data model & migrations

One migration in `lightbox-catalog` (number assigned as next-in-sequence after E01's head at merge time; coordinate — see Open Questions). E01's schema v1 carries the §3.1 `preview` stub; since preview rows are pure cache index, the migration **recreates** it rather than altering (safe: cache entries are disposable; existing stores are re-adopted by `verify_store(Full)`).

```sql
-- up: NNNN_preview_pyramid.sql
DROP TABLE IF EXISTS preview;
CREATE TABLE preview (
  id            INTEGER PRIMARY KEY,
  asset_id      INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
  image_id      INTEGER REFERENCES image(id) ON DELETE CASCADE,  -- NULL = asset-scope (T0)
  content_hash  BLOB    NOT NULL,          -- xxh3-128, 16 bytes (denormalized for relink survival)
  tier          INTEGER NOT NULL,           -- 0 | 1 | 2
  variant_hash  BLOB    NOT NULL,           -- xxh3-64, 8 bytes
  source        TEXT    NOT NULL CHECK (source IN ('embedded','rendered')),
  recipe_rev    INTEGER NOT NULL DEFAULT 0, -- edit revision reflected; 0 = edit-independent
  stale         INTEGER NOT NULL DEFAULT 0, -- rendered row superseded by newer recipe_rev
  colorspace    TEXT    NOT NULL DEFAULT 'srgb',
  store_path    TEXT    NOT NULL,           -- relative to store root
  width         INTEGER NOT NULL,           -- upright (orientation baked)
  height        INTEGER NOT NULL,
  bytes         INTEGER NOT NULL,           -- on-disk size; T2: sum over tile dir
  checksum      BLOB    NOT NULL,           -- xxh3-64 of encoded payload (T2: manifest hash)
  built_at      INTEGER NOT NULL,           -- unix seconds
  last_used_at  INTEGER NOT NULL
);
-- SQLite treats NULLs as distinct in UNIQUE constraints, so scope uniqueness is two partial indexes:
CREATE UNIQUE INDEX idx_preview_asset_scope ON preview(asset_id, tier, variant_hash) WHERE image_id IS NULL;
CREATE UNIQUE INDEX idx_preview_image_scope ON preview(image_id, tier, variant_hash) WHERE image_id IS NOT NULL;
CREATE INDEX idx_preview_lru   ON preview(tier, last_used_at);
CREATE INDEX idx_preview_hash  ON preview(content_hash);
CREATE INDEX idx_preview_path  ON preview(store_path);          -- refcount check before unlink

CREATE TABLE raw_cache_entry (
  id             INTEGER PRIMARY KEY,
  content_hash   BLOB    NOT NULL,
  params_hash    BLOB    NOT NULL,          -- xxh3-64 canonical early-stage params (E02/E05-owned encoding)
  payload_schema INTEGER NOT NULL,
  store_path     TEXT    NOT NULL,
  bytes          INTEGER NOT NULL,
  built_at       INTEGER NOT NULL,
  last_used_at   INTEGER NOT NULL,
  UNIQUE (content_hash, params_hash)
);
CREATE INDEX idx_rawcache_lru ON raw_cache_entry(last_used_at);
```

`thumbcache.sqlite` (separate DB file, WAL, delete-safe — not the catalog):

```sql
CREATE TABLE thumb (
  image_id     INTEGER NOT NULL,
  recipe_rev   INTEGER NOT NULL,
  px           INTEGER NOT NULL,            -- long edge, e.g. 256
  format       INTEGER NOT NULL,            -- 0 = JPEG
  data         BLOB    NOT NULL,
  last_used_at INTEGER NOT NULL,
  PRIMARY KEY (image_id, recipe_rev, px)
) WITHOUT ROWID;
CREATE INDEX idx_thumb_lru ON thumb(last_used_at);
```

Cache limits/locations live in the core prefs store (E01), not the catalog; the store's own identity lives in `store.toml`.

---

## 5. Interfaces (the contract; signatures are normative, bodies illustrative)

### 5.1 Identity & descriptors

```rust
pub struct ContentHash(pub [u8; 16]);                    // xxh3-128 of original bytes (from catalog)
#[repr(u8)] pub enum Tier { T0 = 0, T1 = 1, T2 = 2 }
pub enum PreviewScope { Asset(AssetId), Image(ImageId) } // T0 = Asset; T1/T2 = Image
pub enum PreviewSource { Embedded, Rendered }
pub enum PreviewColorspace { Srgb, TaggedIcc /* profile bytes travel in-container */ }

/// Canonical, versioned; hashed with xxh3-64 over fixed-order CBOR. Golden hash vectors committed.
pub struct VariantParams {
    pub enc_ver: u16,          // canonical-encoding version; bump on ANY layout change
    pub producer: ProducerId,  // "embedded" | "engine" | test producers
    pub producer_rev: u32,
    pub long_edge_px: u32,     // T0: 0 (verbatim); T2: 0 (native 1:1)
    pub codec: Codec,          // Jpeg | Jxl
    pub quality: u8,
    pub recipe_rev: u64,       // 0 = edit-independent
    pub process_version: u16,  // 0 when producer == embedded
}
pub struct VariantHash(pub u64);

pub struct PreviewDesc {
    pub scope: PreviewScope, pub tier: Tier, pub variant: VariantHash,
    pub source: PreviewSource, pub recipe_rev: u64, pub stale: bool,
    pub width: u32, pub height: u32,           // upright
    pub colorspace: PreviewColorspace,
    pub store_path: RelPath, pub bytes: u64, pub built_at: i64,
}
```

### 5.2 `PreviewService` — the facade `lightbox-core` registers

```rust
pub struct PreviewService { /* store, index cache, scheduler, decoded LRU, thumb atlas */ }

impl PreviewService {
    pub fn open(cfg: PreviewStoreConfig, catalog: Arc<Catalog>,
                rt: Arc<dyn BuildRuntime>, events: EventSink<PreviewEvent>)
        -> Result<Self, PreviewError>;

    /// Sync, in-memory index only. Best existing tier/variant with long edge >= min_long_edge
    /// (or the largest available if none reach it). Never does IO. p99 < 1 ms at 100k.
    pub fn best_available(&self, image: ImageId, min_long_edge: u32) -> Option<PreviewDesc>;

    /// Decode to upright RGBA8 for texture upload (E01 display-transform node input).
    /// Hot path served from the decoded LRU. Marks LRU-touch.
    pub fn open_pixels(&self, desc: &PreviewDesc, max_long_edge: Option<u32>)
        -> Result<DecodedPreview, PreviewError>;

    /// Grid fast path: encoded ~256px thumb from thumbcache.sqlite (build-through on miss).
    pub fn thumb(&self, image: ImageId, px: u32) -> Result<Option<EncodedThumb>, PreviewError>;

    /// Enqueue build/upgrade. Dedupes on (image|asset, tier, variant). Visible/Neighbor always
    /// accepted; Bulk returns Err(EnqueueError::QueueFull) when bounded queue is full (E04 throttles).
    pub fn request(&self, req: PreviewRequest) -> Result<PreviewTicket, EnqueueError>;
    pub fn cancel(&self, ticket: &PreviewTicket);
    pub fn reprioritize(&self, ticket: &PreviewTicket, p: BuildPriority);

    /// Viewport hint from the grid/filmstrip (E08): drives visible-first + Neighbor prefetch,
    /// cancels/demotes off-screen builds.
    pub fn set_viewport(&self, visible: Vec<ImageId>, neighbors: Vec<ImageId>);

    /// Edits changed (core calls on recipe-rev bump, M1+): stale-marks rendered T1/T2 rows.
    /// Embedded rows are never staled. Does not delete; stale rows remain "best available".
    pub fn invalidate_image(&self, image: ImageId, new_recipe_rev: u64);

    /// T2 tile read for the 1:1 loupe (producer: E05, M1+). None = tile not built yet.
    pub fn t2_tile(&self, image: ImageId, variant: VariantHash, tile: TileCoord)
        -> Result<Option<EncodedTile>, PreviewError>;

    pub fn raw_cache(&self) -> &RawCache;                 // §5.4
    pub fn blob_store(&self, ns: BlobNamespace) -> BlobStore; // "masks", "smartpreview" (§5.5)

    pub fn stats(&self) -> CacheStats;                    // per-tier counts/bytes, rawcache, hit rates
    pub fn set_limits(&self, limits: CacheLimits) -> Result<(), PreviewError>;
    pub fn purge(&self, scope: PurgeScope) -> Result<PurgeReport, PreviewError>;
    pub fn relocate(&self, new_root: &Path, progress: ProgressSink) -> Result<(), RelocateError>;
    pub fn verify_store(&self, mode: VerifyMode) -> VerifyReport; // Quick (open) | Full (idle/CLI)
}

pub struct PreviewRequest {
    pub image: ImageId, pub tier: Tier,
    pub priority: BuildPriority,           // Visible | Neighbor | Bulk
    pub allow_embedded: bool,              // permit embedded-source T1 (M0: true)
}
pub enum BuildPriority { Visible, Neighbor, Bulk }

pub enum PreviewEvent {
    Ready   { image: ImageId, tier: Tier, desc: PreviewDesc },
    Failed  { image: ImageId, tier: Tier, error: PreviewErrorKind },
    Evicted { image: ImageId, tier: Tier },
    BulkProgress { done: u64, total: u64 },
    CachePressure { kind: CacheKind, used_bytes: u64, cap_bytes: u64 },
}

pub struct DecodedPreview {
    pub pixels: Arc<ImageBufU8>,   // interleaved RGBA8, upright
    pub width: u32, pub height: u32,
    pub colorspace: PreviewColorspace,
    pub source: PreviewSource, pub tier: Tier,
}
```

### 5.3 Producer seam (E02/E05 implement at M1; embedded impl ships in E03)

```rust
pub trait PreviewProducer: Send + Sync + 'static {
    fn id(&self) -> ProducerId;
    fn rev(&self) -> u32;                              // bump when output semantics change
    fn accepts(&self, job: &ProduceJob) -> bool;
    /// Runs on a BuildRuntime worker. Must poll `cancel` between coarse steps.
    fn produce(&self, job: &ProduceJob, cancel: &CancellationToken)
        -> Result<Produced, ProduceError>;
}

pub struct ProduceJob {
    pub asset: AssetRef,           // abs path, content_hash, format, dims, orientation
    pub image: Option<ImageRef>,   // None for asset-scope (T0)
    pub tier: Tier,
    pub params: VariantParams,
}

pub enum Produced {
    /// Already in target codec — stored verbatim (T0 embedded fast path: zero transcode).
    Encoded { codec: Codec, bytes: Vec<u8>, width: u32, height: u32, colorspace: PreviewColorspace },
    /// Raw pixels; the service resizes (fast_image_resize Lanczos3) + encodes via PreviewCodec.
    Pixels(ImageBufU8),
    /// T2: a set of 256² tiles + grid geometry (engine producer, M1+).
    Tiles { grid: TileGrid, tiles: Vec<(TileCoord, ImageBufU8)> },
}

/// Encoder abstraction (§1.6 names this trait; libvips is a possible future accelerated backend).
pub trait PreviewCodec: Send + Sync {
    fn codec(&self) -> Codec;
    fn encode(&self, img: &ImageBufU8, q: u8) -> Result<Vec<u8>, CodecError>;
    fn decode(&self, bytes: &[u8]) -> Result<ImageBufU8, CodecError>;
}
```

M0 registrations: `EmbeddedProducer` (rawler preview/thumb extraction with kamadak-exif fallback + orientation; for non-raw JPEG/TIFF sources: no T0 — T1 built by downscaling the source file itself, EXIF thumb seeding the thumb atlas). M1: E05 registers `EngineProducer` (renders through the node graph honoring the recipe); E03's dispatcher prefers the highest-capability producer that `accepts()` the job, controlled by `allow_embedded`.

### 5.4 Raw cache (store mechanics here; payload semantics E02/E05)

```rust
pub struct RawCacheKey { pub content_hash: ContentHash, pub params_hash: u64 }
pub struct RawStageMeta {
    pub payload_schema: u16,                  // opaque to E03
    pub width: u32, pub height: u32,
    pub channels: u8, pub sample: SampleFormat, // F16 | F32 | U16
    pub color_state: u16,                     // producer-defined
}
impl RawCache {
    /// Miss on absent OR checksum-failed (failed entries are dropped + deleted). Never blocks render:
    /// a miss is always recoverable by re-decoding upstream.
    pub fn get(&self, key: &RawCacheKey) -> Result<Option<RawCacheHit>, RawCacheError>;
    /// Atomic write (temp+rename), zstd level from config, evicts to cap afterwards.
    pub fn put(&self, key: RawCacheKey, meta: RawStageMeta, planes: PlaneData<'_>)
        -> Result<(), RawCacheError>;
    pub fn contains(&self, key: &RawCacheKey) -> bool;    // index-only
    pub fn evict_to_cap(&self) -> EvictReport;
    pub fn purge_all(&self) -> Result<PurgeReport, RawCacheError>;
}
pub struct RawCacheHit { pub meta: RawStageMeta, pub data: PlanarBuf } // owned, decompressed
```

### 5.5 `BlobStore` namespaces (E14/E16 seam)

```rust
pub struct BlobRef(pub [u8; 16]);                        // xxh3-128 content address
impl BlobStore {
    pub fn put(&self, bytes: &[u8]) -> Result<BlobRef, StoreError>;   // idempotent
    pub fn get(&self, r: &BlobRef) -> Result<Option<Vec<u8>>, StoreError>;
    pub fn remove(&self, r: &BlobRef) -> Result<bool, StoreError>;
}
```

This is the mechanism behind `mask_component.cached_raster_ref` (§4.5: baked AI rasters "content-addressed in the preview store"). E03 ships it + the reserved `masks/` directory; E14 owns raster content, lifecycle, and the pinning policy that exempts referenced blobs from any eviction (blob namespaces are **not** LRU-evicted by E03 — they hold authoritative edit state, not cache).

### 5.6 Runtime seam (E06) and core façade additions

```rust
/// M0: lightbox-core backs this with plain tokio. M1: lightbox-jobs adapter (Class::Background,
/// pausable, activity-center visible). E03 never imports lightbox-jobs types.
pub trait BuildRuntime: Send + Sync {
    fn spawn_build(&self, name: &'static str, fut: BoxFuture<'static, ()>) -> RuntimeTask;
    fn concurrency(&self) -> usize;
}
```

`lightbox-core` façade additions (consumed by E08 prefs panel + E04 import + `lightbox-cli`):

```rust
pub enum Command { /* … */
    BuildPreviews { images: Vec<ImageId>, tier: Tier, priority: BuildPriority },
    DiscardPreviews { images: Vec<ImageId>, tiers: TierSet },
    SetCacheLimits(CacheLimits),
    RelocateCacheStore { new_root: PathBuf },
    PurgeCaches(PurgeScope),
}
pub enum Query { /* … */ CacheStats, PreviewState { image: ImageId } }
// Events: PreviewReady / PreviewFailed / CachePressure re-published on the core event bus.
```

### 5.7 Configuration

```rust
pub struct PreviewStoreConfig {
    pub root: PathBuf,                        // <catalog>.lbdata by default; relocatable
    pub limits: CacheLimits,
    pub standard_px: StandardSize,            // Auto (largest display long edge, clamped 1280..=3840) | Fixed(u32)
    pub t1_codec: Codec,                      // Jxl (feature "jxl") | Jpeg fallback
    pub t1_quality: u8,                       // JXL distance-mapped; default ≈ "high"
    pub t2_tile_px: u32,                      // 256 default (§4.3); store parameter, not API change
    pub t2_retention: Retention,              // Days(30) | Never
    pub decoded_lru_bytes: u64,               // default 512 MiB
    pub workers: Option<usize>,               // default min(physical_cores, 8)
    pub zstd_level: i32,                      // default 3
}
pub struct CacheLimits { pub preview_cap_bytes: u64 /* 20 GiB */, pub rawcache_cap_bytes: u64 /* 5 GiB */ }
```

---

## 6. Performance budgets (E03's share of §7)

Reference machine per §7 (mid-range: ~RTX 3060 / base M-series, NVMe). Asserted by the nightly scenario harness; regressions file issues (non-blocking) per §8.

| Metric | Budget | Backing mechanism |
|---|---|---|
| `best_available` | < 1 ms p99 @ 100k images | in-memory index (RAM mirror of `preview` table) |
| Cull next/prev swap (prefetched) | ≤ 50 ms p95 | Neighbor prefetch ±2 into decoded LRU; LRU hit < 5 ms |
| Cold T0/T1 decode (≈8 MP embedded) | ≤ 120 ms | zune-jpeg + orientation bake |
| T0 extraction throughput | ≥ 200 assets/s aggregate, 8 workers, NVMe | verbatim byte copy, no transcode; per-file CPU ≤ 10 ms |
| M0 exit scenario | 1k raws: grid browsable immediately; all T0 built ≤ 15 s | supports M1's "10k browsable < 60 s" with E04 parallel ingest |
| T1 build (resize+encode, 3840 px) | ≤ 250 ms/image/worker | fast_image_resize SIMD + mozjpeg/libjxl |
| Thumb atlas `thumb()` warm | < 500 µs p99 @ 100k | thumbcache.sqlite WITHOUT ROWID + row LRU |
| Raw cache `get` (24 MP F16 ×3ch ≈ 140 MB) | ≤ 200 ms decompress+verify | zstd ≥ 1 GB/s decompress, mmap'd read |
| Raw cache `put` (same payload) | ≤ 600 ms on a Background worker | zstd-3, never on the interactive path |
| Eviction (reclaim 1 GiB) | ≤ 2 s, concurrent builds unaffected | LRU index walk, batched deletes off-writer |
| Catalog writer load | preview index writes never delay culling writes > 5 ms | batched touch flush, single-row upserts |

---

## 7. Ordered task breakdown (each ≤ 1 day)

Phases are sequential; tasks within a phase are mostly parallelizable across 1–2 engineers. 23 tasks ≈ 4.5 pw nominal — inside the M (~3–5 pw) envelope with the later hardening tasks as the flex.

**Phase A — store foundation**

- **T01. Crate scaffold + config + error taxonomy.** `lightbox-preview` in the workspace; `PreviewStoreConfig`/`CacheLimits`/typed errors; CI wiring (fmt/clippy/deny). *AC:* workspace builds on 3-OS CI; cargo-deny green with new deps; `PreviewService::open` on an empty dir creates the §3.2 layout + `store.toml`.
- **T02. Atomic blob IO + fan-out layout + `BlobStore`.** Temp-then-rename with fsync, xxh3 checksums, `<hh>` fan-out, reserved `masks/`/`smartpreview/` namespaces. *AC:* crash-loop test (SIGKILL during 1k writes) leaves zero torn visible files; `BlobStore` put/get/remove round-trips; put is idempotent.
- **T03. Catalog migration + up/down test.** §4 DDL as next-in-sequence migration; recreate-preview-table policy documented. *AC:* migration applies over E01 schema v-head; partial-unique-index scope semantics proven by tests (asset-scope vs image-scope rows can't collide or duplicate).
- **T04. Preview index DAO + RAM index + batched touches.** `preview_dao` (upsert/lookup/stale-mark/evict candidates), in-memory mirror, touch batching (≤ 5 s / ≥ 64). *AC:* `best_available` p99 < 1 ms against 100k synthetic rows; kill mid-batch loses only unflushed touches (catalog integrity clean).
- **T05. `VariantParams` canonical encoding + key derivation.** Fixed-order CBOR, `enc_ver`, xxh3 hashing, store-key derivation. *AC:* committed golden hash vectors pass on macOS/Windows/Linux CI; any field change without `enc_ver` bump fails the vector test.

**Phase B — embedded path (M0 critical)**

- **T06. Embedded extraction.** rawler metadata/preview API primary, kamadak-exif scan fallback; largest-preview selection; orientation captured. *AC:* 12-body raw fixture corpus each yields T0 bytes or typed `NoEmbeddedPreview`; per-file CPU ≤ 10 ms excluding IO; unsupported files never panic (adversarial fuzz corpus subset).
- **T07. T0 write path + asset-scope dedupe.** Verbatim store, index row `source='embedded'`, content-hash dedupe. *AC:* re-import of an identical file creates zero new store files; two virtual copies over one asset share one T0 row/file; store filename matches §3.1 key scheme.
- **T08. Decode-for-display.** zune-jpeg decode → orientation bake → `DecodedPreview` (+ colorspace tag from EXIF/ICC presence). *AC:* 8-orientation golden corpus renders upright (pixel-compare per platform); integration smoke: core uploads the buffer and E01's display-transform node presents it via `Engine::submit`/`poll`.
- **T09. Decoded LRU + neighbor prefetch.** RAM LRU (byte-capped), `set_viewport` prefetch of filmstrip ±2 / grid ±N. *AC:* scripted next/prev over a prefetched 200-image set: swap p95 ≤ 50 ms; LRU never exceeds cap; eviction is LRU-ordered.

**Phase C — codecs + T1**

- **T10. `PreviewCodec` + JPEG backend + resize.** mozjpeg encode, zune-jpeg decode, fast_image_resize Lanczos3. *AC:* T1 from embedded at 3840 px within quality gate (PSNR ≥ 40 dB vs float reference downscale); encode ≤ 250 ms/image/worker.
- **T11. Own minimal libjxl encode FFI (feature `jxl`).** Thin bindgen binding over libjxl's C encoder API (BSD-3, static); jxl-oxide for decode; SBOM surface-2 entry. **`jpegxl-rs`/`jpegxl-sys` (GPL-3) are banned** — this task exists because of that. *AC:* builds on 3-OS CI; encode→jxl-oxide-decode round-trip within tolerance; cargo-deny + SBOM policy green; with the feature off, T1 falls back to JPEG with no API change.
- **T12. T1 build pipeline + standard-size policy.** Wire `EmbeddedProducer` → resize → codec → store/index; `StandardSize::Auto` (largest display long edge, clamp [1280, 3840], persisted). *AC:* `request(T1)` on a raw fixture yields `.t1` file + index row (`source='embedded'`, correct variant_hash); non-raw JPEG source builds T1 from the source file directly (no T0 row).

**Phase D — service + scheduler**

- **T13. Scheduler core.** Priority queues (Visible/Neighbor/Bulk), dedup + in-place upgrade, bounded concurrency, cancellation, bounded Bulk queue. *AC:* dedup collapses N identical requests to one build; a Visible enqueue overtakes queued Bulk 100% of trials; cancel takes effect at the next step boundary; Bulk `try_enqueue` over bound returns `QueueFull`.
- **T14. `PreviewService` facade + events + core façade.** Tickets, `PreviewEvent` → core bus; `Command`/`Query` additions; `lightbox-cli preview build/stat/verify/purge`. *AC:* headless CLI: import fixture → `preview build --tier 1` → `Ready` events → `stat` reports rows/bytes matching disk.
- **T15. Visible-first viewport integration.** `set_viewport` cancel/demote of off-screen work; prefetch scheduling. *AC:* scroll simulation (viewport sweep over 5k images): ≥ 95% of off-screen queued builds cancelled before start; visible completions strictly precede prefetch completions.
- **T16. Bulk build + progress + backpressure entry point.** "Build previews for selection" command; `BulkProgress` events; E04-facing throttled enqueue helper. *AC:* 1k-image bulk run: queue never exceeds bound, progress monotonic to completion, cancel mid-run stops within worker-step latency and leaves consistent index/store.

**Phase E — raw cache**

- **T17. Raw-cache container + put/get.** §3.3 format, zstd frame + checksum, atomic write, mmap read. *AC:* planar F16 fixture round-trips bit-exact; single-byte corruption → typed miss + entry dropped + file deleted; 140 MB payload `get` ≤ 200 ms on reference machine.
- **T18. Raw-cache accounting + LRU + reconcile.** `rawcache_dao`, evict-to-cap (5 GiB default), purge, startup/idle scan (orphans adopted-or-removed, dangling rows dropped). *AC:* filling 6 GiB synthetic enforces cap within one entry; scan repairs both divergence directions; concurrent get/put during eviction race-free (loom or stress test).

**Phase F — lifecycle, T2, hardening**

- **T19. Preview eviction + retention.** LRU to cap (order T2→T1→T0), `store_path` refcount before unlink, Windows deferred-delete queue, T2 age sweep on idle tick, `DiscardPreviews`. *AC:* eviction respects tier order + never unlinks a file with a live handle (stress: readers during eviction); T2 rows older than retention are swept; discard command removes rows+files for a selection.
- **T20. T2 tiled store.** Tile addressing (`t2_tile_px` grid), per-tile atomic write, `manifest.cbor` (grid geometry + presence bitmap, temp+rename), aggregate-bytes accounting, synthetic producer for tests. *AC:* synthetic 100 MP image: write/read full + partial tile sets (missing tile → `Ok(None)`); manifest survives kill-loop; `Produced::Tiles` path exercised end-to-end.
- **T21. Relocate + purge + disk-full + verify.** Journaled relocation (resume after crash, concurrent reads), `PurgeCaches`, ENOSPC pre-flight + eviction-then-fail with `CachePressure` event, `verify_store(Quick|Full)`. *AC:* kill mid-relocate then reopen → relocation resumes and completes, zero lost entries; simulated ENOSPC during build emits event + typed failure, no panic; `Full` verify detects planted torn/orphan/missing cases.
- **T22. Thumbnail atlas.** `thumbcache.sqlite` schema, build-through `thumb()`, row LRU, delete-safe rebuild. *AC:* warm `thumb()` p99 < 500 µs @ 100k synthetic; deleting the DB file degrades gracefully (rebuilt lazily, no errors surfaced).
- **T23. Perf/scenario + fault-injection suites; M0 exit wiring.** Criterion benches for §6 metrics; scenario harness: import-1k-raws → all-T0 ≤ 15 s + browse-without-stall (with E01's harness); kill -9 loop (100×, random phases) → catalog `integrity_check` clean + `verify_store` clean; nightly baselines committed. *AC:* all §6 budgets green on reference CI runner; fault suite green; suites are PR-blocking (fast subset) / nightly (full) per §8.

---

## 8. Test plan (per §8 of the architecture)

| Layer | Tests | Gate |
|---|---|---|
| **Unit** | Key/variant canonicalization (golden hash vectors, cross-platform); DAO scope-uniqueness + LRU queries; eviction order + refcounting; retention math; orientation matrix (8 cases); container header round-trip; scheduler dedup/priority/cancel/backpressure state machine | PR-blocking |
| **Property (`proptest`)** | `VariantParams` encode→hash stability under field permutations (must be order-independent at the API, fixed in encoding); store put/get round-trip over arbitrary payload sizes incl. 0 and >4 GiB metadata edge; index↔store reconciliation converges from arbitrary divergence (orphans/dangling/torn) | PR-blocking |
| **Golden-image** | Orientation corpus renders upright vs committed references; T1 downscale quality gate (PSNR ≥ 40 dB vs float-reference resize); JXL/JPEG encode→decode within tolerance. (Full render goldens are E05's; E03's goldens cover geometry/orientation/resample only.) | PR-blocking |
| **Fault injection** | kill -9 loop across all write phases → catalog `integrity_check` + `verify_store` clean; truncated/bit-flipped store files → typed miss, drop, re-enqueue, never a crash; ENOSPC (quota shim) → pressure event + graceful failure; kill mid-relocation → resume | PR-blocking (the §7 "0 corruptions" bar applies to the catalog; the store's bar is "always reconcilable") |
| **Integration (headless, `lightbox-cli`)** | import fixture corpus → T0 for all rawler-covered bodies → `preview build` T1 → events/stats consistent with disk; VC sharing; discard/purge/relocate flows; concurrent viewport churn during bulk build | PR-blocking (fast subset), full nightly |
| **Perf (`criterion` + scenario harness)** | Every §6 budget; scenario: 1k-raw import → T0 ≤ 15 s, browse stall-free; 100k-row index lookup; thumb atlas p99; raw-cache get/put; eviction throughput | Nightly; regression files issue (non-blocking) per §8 |
| **Cross-platform** | Full suite on macOS/Windows/Linux CI; Windows-specific: delete-while-open deferral; path/casing; mmap semantics | PR-blocking on all three |
| **License surfaces** | cargo-deny (no GPL: notably **no jpegxl-rs**); SBOM surface-2 entries for libjxl/mozjpeg/zstd static links | PR-blocking |

**Fixtures:** a pinned test-asset pack (~12 small raws spanning CR2/CR3/NEF/ARW/RAF/ORF/DNG/PEF/RW2, an 8-orientation JPEG set, a corrupt/truncated set, one large synthetic TIFF), fetched by hash-pinned download in CI (alignment with E01's fixture strategy — Open Question Q5).

---

## 9. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| **R1** | **JXL encode binding license trap.** The obvious Rust bindings (`jpegxl-rs`/`jpegxl-sys`) are **GPL-3** — in-process linkage would violate the license mandate. | Own minimal bindgen FFI over libjxl's BSD-3 C API (T11), feature-gated; audited on SBOM surface 2. **Reversal trigger:** if the binding proves troublesome on any platform, T1/T2 ship as JPEG (mozjpeg) — a container/quality change behind `PreviewCodec`, zero API impact. |
| **R2** | **rawler embedded-preview coverage gaps** (narrower than LibRaw; Risk 3 treadmill). Some bodies may yield no extractable preview at M0. | kamadak-exif scan fallback; typed `NoEmbeddedPreview` → grid placeholder + T1 queued for the M1 engine producer; exotic formats route to E02's sandboxed LibRaw at M1 (seam S2). M0 exit is measured on a rawler-covered corpus. |
| **R3** | **Catalog single-writer contention** from index churn during import + touch updates. | Batched touches (≤5 s/≥64), batched upserts on the writer task, approximate LRU by design; budgeted in §6 ("culling writes never delayed > 5 ms") and asserted in the perf harness. Escalation path is §7's named mitigation (shard preview-index writes) — not built now. |
| **R4** | **Windows file semantics vs eviction** (can't unlink open files; mmap holds locks). | Ref-counted handles + deferred-delete queue (T19); raw-cache reads copy-out from mmap before returning (no long-lived maps). Covered by Windows CI stress tests. |
| **R5** | **T2 tile-count explosion** (a 100 MP image at 256² ≈ 1.5k files). | Tile size is a store parameter (`t2_tile_px`), default 256 per §4.3; T20's bench measures 256 vs 512 file-count/latency trade and can change the default without API impact (Q2). T2 is also the first eviction tier + age-swept. |
| **R6** | **Non-atomic rename on exotic filesystems** (exFAT, some network mounts) undermines torn-file guarantees. | Same-dir temp+rename (atomic on APFS/NTFS/ext4); store detects filesystem at open and surfaces a warning for best-effort filesystems; checksums make torn files detectable and self-healing regardless. |
| **R7** | **Embedded-preview color fidelity** (some bodies embed AdobeRGB-tagged JPEGs; embedded previews never match edits). | Colorspace tag honored and carried on `DecodedPreview`; `source='embedded'` flag drives the E08 UI badge (the Lightroom convention); full ICC handling arrives with E02 (seam S2) — at M0 the display-transform node treats input as tagged/sRGB. |
| **R8** | **Backpressure mis-tuning between E04 and the Bulk queue** could either starve preview builds or stall import. | Queue bound and worker count are config; the T16/T23 scenario tests assert import-throughput and T0-latency together; final tuning is an E04-integration task on their side of the seam. |

### Open questions (proposed defaults; none block the task breakdown)

- **Q1 — Migration number.** Assigned as next-in-sequence when E03 merges after E01's schema head. Owner: E03 at merge time.
- **Q2 — T2 stored-tile size.** Default 256² (§4.3 alignment with render tiles); T20 benches 512² packing as a store-parameter change. Decide from data, not re-design.
- **Q3 — Display-size change policy for `StandardSize::Auto`.** Proposed: existing T1s are kept (still "best available"); larger variants build on demand when a bigger display appears. No proactive rebuild.
- **Q4 — Thumb atlas payload format.** Proposed: encoded JPEG @ 256 px (simple, compact). GPU-compressed (BC1) rows are a possible E08-era optimization; schema's `format` column reserves it.
- **Q5 — Fixture distribution.** Proposed: hash-pinned downloadable test-asset pack (raw.pixls.us-derived, CC0 files only), shared with E02's corpus. Coordinate with E01/E02 owners.
- **Q6 — `preview_cap_bytes` default.** Proposed 20 GiB (T1 ≈ 1–2 MB/image → comfortable at 10k, prunable at 100k). E08's prefs panel exposes it; nothing architectural rides on the default.

---

## 10. Seams to neighboring epics (named, not designed)

| Seam | Neighbor | Contract |
|---|---|---|
| **S1** Catalog, schema, event bus, prefs store, M0 `BuildRuntime` | **E01** | E03 adds one migration + two DAO modules; consumes the writer/reader handles and the event bus; core hosts the tokio-backed `BuildRuntime` and feeds `DecodedPreview` into the E01 display-transform node. |
| **S2** Probe/extraction ownership; ICC; LibRaw sandbox | **E02** | At M1, embedded extraction migrates behind `lightbox-decode::probe()` (E03's `EmbeddedProducer` swaps its inner source; trait unchanged); exotic-format extraction goes via E02's sandboxed LibRaw; full color management replaces E03's tag-only handling. |
| **S3** Import enqueue + backpressure | **E04** | E04 calls the throttled Bulk enqueue helper (`QueueFull` = throttle signal) after each copied/verified asset; embedded-first policy is the default (research 09's headline win). E04 owns import UX and ordering. |
| **S4** Rendered producers + raw-cache payload | **E05** (with E02) | E05 implements `PreviewProducer` for rendered T1 (`Pixels`) and T2 (`Tiles`), defines `params_hash`/`payload_schema`/`color_state` semantics for `RawCache` put/get on Develop-open (§4.3 progressive contract: best tier < 100 ms via E03, preview render < 1 s via raw-cache hit). |
| **S5** Job classes, pause, activity center | **E06** | E06 ships a `BuildRuntime` adapter mapping builds to `Class::Background` with pause; `BulkProgress`/`CachePressure` events feed the activity model. E03 keeps zero compile-time dependency on `lightbox-jobs`. |
| **S6** Grid/loupe/filmstrip + performance prefs panel | **E08** | E08 consumes `best_available`/`open_pixels`/`thumb`/`set_viewport`/`t2_tile` and the `PreviewReady` upgrade events (thumbnails upgrade in place); binds `SetCacheLimits`/`RelocateCacheStore`/`PurgeCaches`/`CacheStats` and the T2 retention knob in the prefs panel; renders the `embedded` badge. |
| **S7** Edit-revision staleness | **E09** | Core bumps `recipe_rev` on recipe writes and calls `invalidate_image`; E03 never reads recipe content. |
| **S8** Baked mask rasters | **E14** | `BlobStore::namespace("masks")` stores `cached_raster_ref` content (§4.5); blob namespaces are exempt from LRU eviction (authoritative edit state, not cache). |
| **S9** Smart previews | **E16** | `smartpreview/` directory + extensible `tier`/`source` schema headroom reserved; no v1 engineering here. |

Export (E15) intentionally has **no** E03 seam: exports render full-res through the engine, never from previews; any raw-cache benefit flows through S4.

---

## 11. Definition of done

E03 is done when all of the following hold:

1. **Tasks T01–T23 merged**, each meeting its acceptance criteria, on all three CI platforms.
2. **M0 exit criteria (E03's share) green:** importing 1k raws leaves the grid browsable with no UI stall; every rawler-covered asset has a T0 within 15 s; the loupe image reaching the screen is a `DecodedPreview` composited through `Engine::submit`/`poll` (no blit bypass); `kill -9` at any point leaves the catalog `integrity_check`-clean and the store `verify_store`-clean.
3. **Budgets:** every §6 metric passing on the nightly reference runner, with committed criterion baselines.
4. **Crash/fault suite:** 100-iteration kill loop, corruption-injection, ENOSPC, and mid-relocation-kill tests all green and PR-blocking.
5. **License surfaces:** cargo-deny green (explicitly: no `jpegxl-rs`); SBOM surface-2 entries for libjxl/mozjpeg/zstd approved; no new LGPL/GPL introduced by this epic.
6. **Frozen seams:** `PreviewProducer`, `PreviewCodec`, `RawCache` put/get + container v1, `BlobStore`, and `BuildRuntime` are documented (rustdoc + this spec) and versioned such that E02/E05/E06/E14 can build against them without E03 changes; the synthetic-producer tests demonstrate the T2 and rendered-T1 paths end-to-end ahead of E05.
7. **Operability:** `lightbox-cli preview`/`rawcache` subcommands cover build/stat/verify/purge/relocate headlessly; `CacheStats` exposes everything the E08 prefs panel needs.
8. **Docs:** crate README covering store layout, key scheme, container format, eviction/retention policy, and the reconcile model — sufficient for an E04/E05/E08 engineer to integrate without reading E03 source.
