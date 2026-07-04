# E16 — Interop, migration & v1 hardening

_Epic spec. Milestone: **M4**. Effort: **L, ~6–10 pw** (this plan lands at ~40 task-days ≈ 8 pw). Depends on: **E07** (catalog DAM: rule AST, collections/keywords commands), **E09** (recipe/XMP: `from_lr_crs`, `xmp_passthrough`), **E14** (AI masking platform in place; transitively E12 retouch objects + E13 `lightbox-inferd`), **E15** (export engine, needed by the end-to-end scenario and release validation)._

_Planned from `docs/plan/01-architecture.md` (decision-complete; stack/boundary decisions are not re-litigated here) and `docs/plan/00-mandate.md` v1.1. Research inputs: reports 04 (retouch/remove), 06 (import/migration), 08 (AI/ML), 09 (performance/backup), 10 (landscape)._

---

## 0. What this epic is

E16 is the M4 "ship it" epic. It contains the four workstreams that turn the M3 feature-complete build into v1.0:

1. **Lightroom migration importer** — one-way, guided import of a Lightroom Classic `.lrcat` (organization losslessly, develop settings approximately via E09's `crs:` mapping), plus batch `.xmp` sidecar import for the no-catalog path. The #1 adoption lever (feature catalog §"Migration as a feature").
2. **Content-aware Remove (LaMa)** — the ML fill provider for the `remove` retouch mode: big-lama ONNX via `lightbox-inferd`, baked-result persistence per the §4.5 reproducibility policy, PatchMatch fallback.
3. **Packaging & distribution** — signed/notarized installers for macOS + Windows, best-effort Linux AppImage, LGPL-compliant dynamic-lib layout, component download staging (ORT), attribution bundle.
4. **CI gates & performance hardening** — the three-surface license gate (§8 surfaces 1–3) implemented and release-blocking; golden-image/fault-injection gates wired to the release matrix; the §7 performance-budget scenario harness built and driven to green; the guided corruption repair/restore flow finished.

The epic's exit is the mandate's success scenario, measured: shoot → ingest & cull 3,000 raws → develop with global + AI-masked local adjustments → export delivered JPEGs, entirely offline, on a mid-range laptop.

**First failing test (staff-engineer contract, §12):** `lrcat_fixture_inventory_counts` — `LrcatReader::open()` on the committed LR Classic fixture catalog returns the known entity counts (folders, files, images, virtual copies, keywords, collections). It fails until WS‑A tasks A1–A3 land.

---

## 1. Scope

### 1.1 In scope

- **`.lrcat` migration importer** (guided, one-way, read-only source):
  - Organization: roots/folders, files → assets, images incl. virtual copies, ratings, pick/reject flags, color labels, hierarchical keywords + assignments, collections/sets incl. custom order, smart collections (best-effort rule mapping to E07's `RuleTree`).
  - Develop: per-image `crs:` settings imported **via the embedded XMP documents inside the catalog** (see §4.1.3), mapped through E09's `Recipe::from_lr_crs()`; provenance recorded; an "Imported from Lightroom" snapshot created per image.
  - Path remapping UI for moved volumes/drive letters; missing-file handling; dry-run plan + post-run report; resumable/idempotent execution; `lightbox-cli migrate` headless surface.
- **Batch `.xmp` sidecar `crs:` import at scale**: the importer path that applies sidecar develop settings during/after a folder import (the "user exported XMP from LR, no catalog" route). The single-file mapping is E09's; E16 owns the batch orchestration + report.
- **Content-aware Remove**: `inpaint` op added to the `lightbox-ml`/`lightbox-inferd` protocol (following E13's extension mechanism); `big-lama` model pack (Apache-2.0, hash-pinned, license-manifested); `LamaFillProvider` implementing E12.4's fill-provider seam; sRGB context-window preparation; baked-result persistence + deterministic replay; PatchMatch fallback wiring; re-roll (refresh) support.
- **Packaging**: macOS universal2 `.app`/`.dmg` (codesigned, hardened runtime, notarized, stapled); Windows installer (signed); Linux AppImage (best-effort, non-release-blocking); `cargo xtask dist` staging incl. LGPL dynamic libs as separate replaceable files + THIRD-PARTY-NOTICES + lensfun CC-BY-SA attribution; inferd+ORT staged as a downloadable signed component per §1.5; manual "Check for updates" (static JSON, no auto-update, no telemetry); local-only crash/panic log capture.
- **CI gates**: cargo-deny finalization (surface 1) incl. LGPL-must-be-dynamic linkage assertion; CycloneDX SBOM emitter + native-binary policy checker (surface 2) with the three hard gates (libheif→libde265 never x265; FFmpeg LGPL-only buildconf; ORT EP set = DirectML/CoreML/CPU, no CUDA/TensorRT staged); bundled content/data manifest + checker (surface 3, zero Adobe-authored profile/look assets); release-runner *actual binary/asset-tree* inspection; golden-image per-PV gate and `kill -9` fault-injection suite wired into the 3-OS release matrix.
- **Performance hardening**: scenario harness asserting every §7 budget (import-10k, cull latency, slider p95, catalog-100k interactivity, export throughput + starvation probe, memory ceiling); synthetic 100k catalog generator; tracing/chrome-trace profiling toggle; a bounded fix-to-budget buffer (regressions triaged to owning crates, E16 owns the green gate).
- **v1 hardening**: guided corruption repair/restore flow (auto `.recover` attempt → verified-backup restore picker), built on E01/E07 primitives; `xtask release` one-command release-readiness check.

### 1.2 Explicit non-goals

- **No two-way LR sync, no merge of two Lightbox catalogs, no Apple Photos/Capture One importers** (feature catalog rates other-app importers Could; v1.x at best).
- **No LR develop-history or LR-snapshot import** — current develop state only, plus our own "Imported from Lightroom" snapshot. LR history is serialized Lua of marginal value; skipped and reported.
- **No stack import** (Lightbox v1 schema has no stack entity) — counted and reported, never silently dropped.
- **No publish services / slideshow / book / print / face-region import** from `.lrcat`. Faces are regenerated locally by E14's pipeline.
- **No `.lrdata` preview import** — previews are rebuilt by E03's pyramid (embedded-preview-first covers immediate culling).
- **No pixel-parity promise for migrated develop settings** — organization migrates losslessly; develop migrates approximately, with the expectation set explicitly in UI and report (Risk 9; research report 00 §"Interop fidelity expectations").
- **No generative/diffusion Remove** (Could-tier, v1.x+); LaMa is the non-generative baseline. No object-aware selection expansion (SAM-scribble → object) in v1 — the seam exists via E14 but it ships later.
- **No auto-update mechanism and no telemetry/crash upload** — manual update check + local logs only (local-first mandate).
- **Not designing neighbors' internals**: the retouch object model/coordinate space (E12.4), inferd IPC framing (E13), `crs:` field mapping fidelity (E09), rule-AST semantics (E07), export encoders (E15), smart previews (E03) are consumed through their published interfaces; gaps are filed against those epics, not re-implemented here (see §10).

---

## 2. Crates & modules touched (per §2 decomposition)

| Crate/module | Change | Notes |
|---|---|---|
| **`lightbox-migrate`** (NEW crate) | created | Foreign-catalog readers + migration planner/executor. New bounded responsibility: keeps foreign-schema `rusqlite` code out of `lightbox-catalog` (which owns only our schema). Depends on `lightbox-meta` (XMP), `lightbox-core` (commands), `lightbox-jobs`. No UI types. |
| `lightbox-meta` | modified | Batch sidecar-XMP import orchestration (`apply_sidecars`); already owns single-doc read + `crs:` handoff to E09's mapping. |
| `lightbox-ml` | modified | `InferenceClient::inpaint()` client API; `LamaFillProvider` (implements E12.4's `FillProvider` seam); context-window prep + color conversion helpers. |
| `lightbox-inferd` | modified | `inpaint` request kind added per E13's protocol-extension guide; big-lama session management; VRAM-gate entry. |
| `lightbox-catalog` | modified | Schema migrations: `migration_session`/`migration_map` tables; `retouch_op` bake columns (§5). No engine/API changes. |
| `lightbox-core` | modified | Commands/queries: `MigrateLrcat{Plan,Execute,Cancel}`, `RetouchRemoveCompute/Refresh`, `RepairCatalog`, `RestoreFromBackup`; report queries. |
| `lightbox-shell` | modified | Migration wizard (3 steps), migration report view, Remove-tool provider affordance + refresh, corruption repair/restore guided flow, "Check for updates", crash-log reveal. Reuses E08 widgets. |
| `lightbox-cli` | modified | `migrate lrcat`, `xmp apply`, `repair`, `perf run` subcommands (headless E2E + CI drivers). |
| `xtask` (NEW workspace tool) | created | `dist` (per-OS staging/sign/notarize), `sbom emit/check` (surface 2), `data-manifest check` (surface 3), `release` (all-gates runner). |
| `tools/perf-harness` (NEW, may live under `xtask perf`) | created | Scenario harness + synthetic catalog generator asserting §7 budgets. |
| CI config (`.github/workflows/` or equivalent) | modified | 3-OS release matrix; PR vs nightly vs release-blocking tiers per §8. |
| `data/MANIFEST.toml`, `policy/native-sbom.toml`, `deny.toml` | created/finalized | The three license surfaces' policy inputs. |

Render pipeline note: the **retouch render node** (the thing that composites patches in the §4.1 `retouch` stage) belongs to **E12.4** and exists by M3. E16 adds no render nodes; it supplies baked patch content that E12.4's node composites.

---

## 3. Mandate-compliance position for the `.lrcat` importer (stated up front)

Constraint 4 bars proprietary Adobe product code, reverse-engineering of Adobe **binaries**, and Adobe-authored assets. The importer complies as follows; this position is recorded here for the CTO sign-off that §12's security/threat-model review will also touch:

- A `.lrcat` file is a **standard SQLite database** (an open format; research report 09 §1) containing **the user's own data**. We read it with `rusqlite`, read-only. No Adobe code is linked, executed, or disassembled; no Adobe binary is reverse-engineered.
- Schema knowledge comes from inspecting **user-owned catalog data files** (our own fixture catalogs, created by us in LR Classic with our own photographs) and long-public community documentation — inspection of data, not of binaries.
- **Develop settings are read as XMP** — the ISO 16684 standard — from the catalog's embedded per-image XMP documents (§4.1.3), so the develop-migration path rides the open-format channel the mandate explicitly names, through E09's ISO XMP Toolkit + `crs:` mapping layer.
- Nothing is written to the source catalog, ever (`mode=ro&immutable=1`, plus a defensive snapshot-copy path, §4.1.1).
- The importer parses **untrusted input** (a malicious `.lrcat` is an attack surface): all parsing is memory-safe Rust, the Lua-table grammar is data-only (never executed), and both are fuzz-gated (§7.1). This is one of the three named items in §12's security-engineer design-time review.

---

## 4. Design & interfaces

Interfaces are the contract; field lists may grow, signatures below are binding in shape.

### 4.1 Workstream A — Lightroom migration importer

#### 4.1.1 `LrcatReader` (`lightbox-migrate::lrcat`)

Opens the source read-only and exposes typed, streaming entity readers. If a `-wal`/`-lock` sibling exists (Lightroom open or crashed), the reader refuses with `LrcatError::CatalogInUse` and offers the snapshot path: copy `.lrcat` (+`-wal`) to the app cache dir and open the copy immutable.

```rust
pub struct LrcatReader { /* rusqlite::Connection, opened "file:…?mode=ro&immutable=1" */ }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LrSchemaVersion(pub u32);          // from Adobe_variablesTable; gates per-version readers

impl LrcatReader {
    pub fn open(path: &Path) -> Result<Self, LrcatError>;
    pub fn open_snapshot_copy(path: &Path, scratch: &Path) -> Result<Self, LrcatError>;
    pub fn schema_version(&self) -> LrSchemaVersion;
    pub fn inventory(&self) -> Result<LrcatInventory, LrcatError>;   // counts per entity + LR version

    pub fn root_folders(&self) -> Result<Vec<LrRootFolder>, LrcatError>;
    pub fn folders(&self) -> Result<Vec<LrFolder>, LrcatError>;
    pub fn files(&self) -> impl Iterator<Item = Result<LrFile, LrcatError>> + '_;
    pub fn images(&self) -> impl Iterator<Item = Result<LrImage, LrcatError>> + '_;
    pub fn keyword_tree(&self) -> Result<Vec<LrKeyword>, LrcatError>;
    pub fn keyword_assignments(&self) -> impl Iterator<Item = Result<(LrImageId, LrKeywordId), LrcatError>> + '_;
    pub fn collections(&self) -> Result<Vec<LrCollection>, LrcatError>;          // incl. sets, smart flag
    pub fn collection_items(&self, c: LrCollectionId) -> Result<Vec<LrCollectionItem>, LrcatError>; // incl. positional order
    pub fn smart_rules(&self, c: LrCollectionId) -> Result<Option<LuaValue>, LrcatError>;
    pub fn xmp_for(&self, img: LrImageId) -> Result<Option<String>, LrcatError>; // embedded per-image XMP doc
}

pub struct LrImage {
    pub id: LrImageId,
    pub file: LrFileId,
    pub copy_name: Option<String>,       // Some(_) ⇒ virtual copy
    pub rating: Option<u8>,              // 0..=5
    pub pick: LrPick,                    // Picked | Rejected | None
    pub color_label: Option<String>,     // label *name* string (user-renamable in LR)
    pub orientation: Option<LrOrientation>,
    pub capture_time: Option<String>,    // as stored; re-verified against file EXIF on import
}
```

Iterators stream (bounded memory at 1M-image catalogs). Missing tables/columns in older schemas degrade per-entity with a `MigrationWarning`, never a hard failure, except for the identity tables (files/images) which are required.

#### 4.1.2 Source → target mapping (the data-engineer handoff table, §12)

| LR source (SQLite) | Lightbox target (§3.1) | Fidelity |
|---|---|---|
| root folders / folder tree | `library_root` / `folder` | lossless (after path remap) |
| library files | `asset` (add-in-place via `lightbox-ingest`, no copy; `content_hash` computed on import) | lossless |
| images (master per file) | `image` | lossless |
| images with copy name (virtual copies) | `image` rows, `is_virtual=1`, `name`=copy name | lossless |
| rating / pick / color label | `rating` / `flag` / `label` on `image` | lossless (labels mapped by name; unmatched names create custom labels) |
| keywords + parents | `keyword` + `keyword_hierarchy` | lossless; keyword **synonyms/export-flags dropped + reported** (no v1 entity) |
| keyword ↔ image links | `keyword_asset` | lossless |
| collections & sets (+ custom order) | `collection` / `collection_set` / `collection_item` (order preserved) | lossless |
| smart collections (Lua rule text) | `smart_collection.rule_tree` via `map_smart_rules` | best-effort subset (§4.1.4) |
| per-image embedded XMP (`crs:`, `dc:`, IPTC-ish) | `edit_recipe` via E09 `from_lr_crs`; titles/captions/IPTC → `metadata_cache` via `lightbox-meta`; unknown fields → `xmp_passthrough` | develop **approximate** (Risk 9); organization/metadata lossless |
| develop history, LR snapshots | — | skipped (non-goal, reported) |
| stacks | — | skipped + reported count |
| publish services / books / slideshow / print | — | skipped (non-goal) |
| face regions/people | — | skipped; E14 regenerates locally |
| GPS / EXIF caches | re-read from files by `lightbox-meta` during ingest | regenerated (files are ground truth) |
| `.lrdata` previews, Camera Raw cache | — | rebuilt by E03 |

LR Classic ≥ 11 stores some AI-mask raster data in the `.lrcat-data` helper store; we do **not** read it. Masks migrate only to the extent they are representable in `crs:` XMP mask parameters (E09's mapping decides); AI masks whose semantics can't replay are imported as approximations or dropped **with a per-image report entry** — the Risk 9 expectation, made visible.

#### 4.1.3 Develop-settings channel: embedded XMP, not Lua

Each LR image row has an associated **complete XMP document** stored in the catalog (LR's own metadata cache). The importer feeds that document — or, when absent, the on-disk `.xmp` sidecar — directly into E09's `Recipe::from_lr_crs()`. We deliberately do **not** parse LR's serialized-Lua develop-settings text as the primary channel: the XMP path (a) is the ISO-standard open format the mandate names, (b) reuses E09's tested mapping and `xmp_passthrough` preservation, and (c) makes `.lrcat` import and sidecar import the *same* code path after extraction. The Lua parser (§4.1.4) exists only for smart-collection rules. Fallback order per image: embedded XMP → sidecar `.xmp` → organization-only import + warning.

Per migrated image, the executor also: records provenance `lb_extra.migration = { source: "lrcat", session, lr_id }`, creates snapshot `"Imported from Lightroom"` from the imported recipe, and seeds one `history_step` (`op = "migrate.import"`), so later edits are diffable against the import state.

#### 4.1.4 Data-only Lua-table parser + smart-rule mapping

```rust
// lightbox-migrate::lua — grammar-only parser for LR's serialized Lua tables. NEVER evaluates code.
pub enum LuaValue { Nil, Bool(bool), Num(f64), Str(String), Table(Vec<(LuaKey, LuaValue)>) }
pub fn parse_lua_table(src: &str) -> Result<LuaValue, LuaParseError>;   // fuzz-gated (§7.1)

// lightbox-migrate::rules — maps LR smart-collection criteria onto E07's RuleTree AST.
pub enum RuleMapping {
    Full(RuleTree),
    Partial { tree: RuleTree, dropped: Vec<UnmappedRule> },
    Unsupported { dropped: Vec<UnmappedRule> },
}
pub fn map_smart_rules(rules: &LuaValue) -> RuleMapping;
```

Mapped criteria subset (v1): rating (all comparators), pick flag, color label, keyword (incl. hierarchy option), camera model, lens, ISO, capture date ranges, filename/copy-name text, folder, file type, has-adjustments. Combine modes: all/any (`intersect`/`union`); nested groups map recursively. Anything else (e.g. smart-preview status, sync status, develop-preset name) is `UnmappedRule`. **Default policy:** `Full` imports enabled; `Partial` imports the collection *disabled* with the dropped rules listed in the report (a disabled partial is honest — enabling it is an explicit user act after review); `Unsupported` creates nothing. `MigrationOptions.partial_smart_collections: SkipDisabledOrEnabled` overrides.

#### 4.1.5 Plan / execute / report

```rust
pub struct MigrationOptions {
    pub import_develop: bool,                 // default true
    pub import_smart_collections: bool,       // default true
    pub partial_smart_collections: PartialPolicy,   // Skip | ImportDisabled (default) | ImportEnabled
    pub skip_missing_files: bool,             // default false → import as `asset.missing=1` ghosts
    pub preview_policy: PreviewPolicy,        // Embedded-first (default) | None | Standard  (E03 seam)
}

pub struct MigrationPlan {
    pub inventory: LrcatInventory,
    pub roots: Vec<RootResolution>,           // per LR root: Resolved | Missing | Remapped(user path)
    pub options: MigrationOptions,
    pub warnings: Vec<MigrationWarning>,      // pre-flight: unsupported schema areas, disk space, …
}

pub fn plan(reader: &LrcatReader, opts: MigrationOptions, fs: &dyn VolumeResolver)
    -> Result<MigrationPlan, MigrateError>;

pub async fn execute(plan: &MigrationPlan, core: &CoreHandle,
                     cancel: CancelToken, progress: ProgressSink)
    -> Result<MigrationReport, MigrateError>;

pub struct MigrationReport {
    pub session: MigrationSessionId,
    pub counts: MigrationCounts,              // imported/skipped per entity
    pub develop: DevelopFidelitySummary,      // full/approx/organization-only per image counts
    pub warnings: Vec<MigrationWarning>,      // incl. per-image drop details, capped + spillover file
}
```

Execution runs as an E06 **Foreground** job in three phases — (1) roots/folders/assets via `lightbox-ingest` add-in-place, (2) images/VCs + ratings/flags/labels/keywords/collections/smart collections, (3) develop recipes + snapshots — each in **batched catalog transactions (1 txn / 500 entities)** through the single writer, respecting the §5.3 backpressure rules (preview builds throttle behind it as Background jobs). **Idempotency/resume:** every created entity is recorded in `migration_map` keyed by `(session, entity, lr_src_key)` inside the same txn that creates it; re-running a session whose `source_fingerprint` (size + mtime + SQLite `change_counter`) still matches skips mapped keys and continues. `kill -9` mid-migration leaves a resumable, `integrity_check`-clean catalog (§7.2). Cancel is cooperative at batch boundaries; a cancelled session is resumable, and "undo import" rides `import_session` deletion semantics from E04.

Shell wizard (3 steps, reusing E08 widgets): pick source (+in-use/snapshot handling) → review plan (inventory table, root remap rows, options, expectation-setting copy: *"Your organization imports exactly; develop edits import as a close approximation — originals and your Lightroom catalog are never modified"*) → run (activity-center progress) → report view with per-image warning drill-down.

### 4.2 Workstream B — Content-aware Remove (LaMa)

#### 4.2.1 Protocol + client (`lightbox-inferd`, `lightbox-ml`)

New request kind following E13's protocol-extension mechanism (E13 owns framing, supervision, VRAM gating; E16 adds one op + one model pack):

```rust
// lightbox-ml (client side)
pub struct InpaintRequest {
    pub model: ModelId,                  // "big-lama" v1 pack
    pub image: Srgb8Patch,               // context window, sRGB 8-bit RGB, dims multiple-of-8, ≤ 2048²
    pub mask: MaskRaster8,               // 0/255 remove mask, same dims
    pub seed: u64,                       // re-roll: perturbs mask dilation + window jitter (LaMa itself is deterministic)
}
pub struct InpaintResponse { pub patch: Srgb8Patch, pub model_pin: ModelPin, pub ep: EpKind }

pub trait InferenceClient {                       // existing (§2.2); E16 adds:
    fn inpaint(&self, req: InpaintRequest) -> JobFuture<InpaintResponse>;
    // segment / embed_clip / denoise unchanged
}
```

Model pack `big-lama` (ONNX export, Apache-2.0): registered in `model_pack` with hash pin + license manifest entry (surface 1 model-weights gate + surface 3 cross-check). CPU EP is an acceptable executor for this op (one-shot, seconds-scale OK), so the VRAM gate degrades to CPU-EP rather than disabling the feature; below even that, the PatchMatch fallback applies.

#### 4.2.2 Fill provider (implements E12.4's seam)

E12.4 owns the retouch object model, coordinate space, the retouch render node, and the `FillProvider` trait + its PatchMatch implementation. E16 ships the LaMa provider (final trait shape to be confirmed against E12.4's spec before task B3 — §8 open question O1):

```rust
pub struct LamaFillProvider { client: Arc<dyn InferenceClient>, packs: Arc<ModelPackHandle> }

impl FillProvider for LamaFillProvider {
    fn id(&self) -> FillProviderId;                     // FillProviderId::Lama
    fn availability(&self) -> Availability;            // Ready | NeedsDownload(ModelId) | InferdDown | Gated
    fn fill(&self, ctx: FillContext<'_>) -> JobFuture<FillResult>;
}

pub struct FillContext<'a> {
    pub window: &'a LinearImage,        // working-space context window around the region (E12.4 supplies)
    pub window_to_image: Transform2D,   // placement back into the retouch stage's coordinate space
    pub mask: &'a WeightBuffer,
    pub seed: u64,
}
pub struct FillResult { pub patch: LinearImage, pub provider: FillProviderId, pub model_pin: Option<ModelPin> }
```

**Color/window handling** (E16-owned, inside the provider): context window = mask bbox dilated ×2 (min 256², capped 2048², downscaled if the region exceeds it — larger fills run at reduced resolution and are upsampled on placement; a known v1 quality bound, stated in UI). Working-space linear → display-referred sRGB 8-bit for the model (LaMa is trained on sRGB photos), inverse on return, feathered alpha at the mask boundary; optional `seamlessClone` mixed-gradient harmonization pass at the seam (OpenCV already linked, §1.6).

**Reproducibility (the §4.5 policy, applied to fill):** cross-EP inference is not deterministic, so the fill result is **baked**: the returned patch is written content-addressed to the preview store (`patches/<hh>/<hash>.lbpatch`, zstd-compressed RGBA16F + header, same conventions as `rawcache/`) and referenced from `retouch_op.result_ref` (§5). **The baked patch — never a fresh inference call — is what the E12.4 retouch node composites on every subsequent render.** `model_pin`/`fill_provider` are provenance. **Refresh/re-roll** is an explicit user command (new seed → new bake → history step); `stale` is set only by explicit user action (e.g. editing the region), mirroring `mask_component.stale`. Export and re-render replay bit-identically from the bake (§7.3).

**Fallback ladder:** inferd healthy + pack installed → LaMa; pack absent → PatchMatch fallback now, non-modal affordance "Better results with the Remove model — download (≈200 MB)" (E13 pack manager CTA); inferd crashed → supervisor restart per E13, job retries once, then PatchMatch. Core editing never blocks on ML (§6 model-failure row).

### 4.3 Workstream C — Packaging & distribution

- **Staging (`xtask dist`)**: per-OS bundle layout from a manifest: app binary, `lightbox-inferd` binary, dynamic libs — every LGPL artifact (LibRaw, lensfun, libheif+libde265, FFmpeg) ships as a **separate, user-replaceable dynamic library** with import-linking only (the LGPL relink capability, §1.6) — bundled data (default look family, curated profiles, lensfun data + attribution), `THIRD-PARTY-NOTICES` generated from the license metadata of all three surfaces, license texts, and the data manifest itself.
- **macOS**: universal2 (aarch64+x86_64) `.app`, codesign with hardened runtime + minimal entitlements, notarize + staple, `.dmg`. Gatekeeper-clean on a vanilla VM is the acceptance test.
- **Windows**: x64 installer (NSIS or WiX — pick in C3 by signing-tooling ergonomics), Authenticode-signed binaries + installer, per-user default install, catalog-file association.
- **Linux**: AppImage built in CI to prove the no-architectural-exclusion mandate; explicitly **non-release-blocking** for v1.
- **ORT component**: ONNX Runtime + EP binaries (DirectML/CoreML/CPU **only**, per §1.5 — CUDA/TensorRT never staged) packaged as a signed, hash-pinned downloadable component fetched on first AI-feature use via E13's pack-manager mechanics; the base installer contains no ORT (size + license simplicity). CUDA remains a detected, user-installed bonus.
- **Updates & crash logs**: "Check for updates" fetches a static signed JSON manifest over HTTPS on explicit user action only, compares versions, links to the download page. No auto-update, no background phone-home, no telemetry. Panics/crashes write a local minidump/log with a "reveal in file manager" affordance; nothing is uploaded.

### 4.4 Workstream D — CI license gates (the three surfaces, §8)

- **Surface 1 (crate graph, PR-blocking)**: finalized `deny.toml` (allow MIT/BSD/Apache/Zlib/Unicode; deny GPL/AGPL; documented per-crate exceptions incl. the ISO XMP Toolkit's CTO-signed BSD-3 entry); REUSE/SPDX lint; a linkage assertion that every LGPL-wrapped native dep resolves dynamic (checked against the surface-2 SBOM, not hand-maintained); model-weights manifest gate.
- **Surface 2 (native/FFI SBOM, PR-blocking policy + release-blocking inspection)**: `xtask sbom emit` produces CycloneDX from the *actual dist staging tree* per platform; `xtask sbom check --policy policy/native-sbom.toml` enforces per-artifact `{license, link_mode, build_flags}` policy. Hard gates: (a) libheif's HEVC decoder is libde265 — verified by dependency-walk (`otool -L`/`ldd`/`dumpbin /dependents`) **and** a deny-symbol scan (no `x264_`/`x265_` exports anywhere in the tree); (b) FFmpeg configuration is LGPL-only — verified via its embedded build-configuration string, asserting absence of `--enable-gpl`/`--enable-nonfree`; (c) staged ORT EP set ⊆ {CPU, CoreML, DirectML} with approved redistribution entries; any unlisted native artifact in the tree fails the gate (default-deny).
- **Surface 3 (content/data manifest, PR-blocking policy + release-blocking tree inspection)**: `data/MANIFEST.toml` — one entry per shipped non-code asset `{id, path_glob, source, provenance, content_license, bundled|user-installable, attribution}`. Checker fails on: any staged asset lacking an entry (default-deny); any entry naming Adobe-authored provenance; lensfun data without the attribution file staged; camera-matching profiles whose provenance isn't `dcamprof-from-own-targets` or a cleared CC0/CC-BY pack; mismatch against the `model_pack` table.
- **Correctness/durability gates wired to the release matrix**: per-PV golden-image suite (corpora owned by E05/E10/E11/E12; E16 owns matrix wiring + fast-subset selection: PR = fast subset on 3 OS, nightly = full, release = full × 3 OS), and the E01 `kill -9` fault-injection suite + scripted restore drill promoted to release-blocking on all three platforms.
- **`xtask release`**: single command that runs surfaces 1–3 + goldens + fault-injection + the perf release subset and emits a release-readiness report; the tag pipeline refuses to publish unless it's green.

### 4.5 Workstream E — Performance hardening & v1 hardening

**Scenario harness** (headless, via `lightbox-cli`/core API; no UI automation for v1 — slider latency measured at the `Engine::submit→poll` boundary plus a fixed compositing allowance, documented):

```rust
pub struct Scenario { pub name: &'static str, pub budget: Budget,
                      pub run: fn(&mut ScenarioCtx) -> anyhow::Result<Measurement> }
pub enum Budget { P95Ms(u64), Ms(u64), PerMin(u32), MaxBytes(u64) }
// CLI: lightbox-cli perf run [--scenario NAME] [--assert] [--json OUT] [--baseline FILE]
```

Scenarios ↔ §7 budgets, all asserted: `import_10k` (grid-browsable < 60 s, UI-latency probe unaffected), `cull_latency` (flag write < 16 ms; next/prev preview swap < 50 ms), `slider_latency` (< 100 ms p95 at fit-view, mid-range GPU), `develop_open` (preview < 100 ms, full < 1 s from raw cache), `catalog_100k` (filter/search/smart-collection < 100 ms p95 on the synthetic 100k catalog), `export_throughput` (≥ 20 raws/min CPU baseline + canvas-starvation probe: interactive render p95 unchanged during export), `memory_ceiling` (single 100 MP render within bounded VRAM). Plus E16's own: `migrate_100k` (organization-only migration of the synthetic 100k `.lrcat` ≤ 20 min, develop import ≤ +15 min, previews excluded) and `remove_op` (brush→filled preview ≤ 2 s p95 GPU EP / ≤ 12 s CPU EP at ≤ 1024² window; replay from bake adds ≤ 5 ms to a render). Cadence: nightly with regression tracking; release-blocking subset on release branches. A synthetic-catalog generator (100k assets, plausible metadata distributions) and a pinned raw corpus definition ship with the harness.

**Fix-to-budget**: E16 owns the gate; regressions are triaged with traces (a `tracing` + chrome-trace export toggle lands on hot paths) and filed to owning crates; a bounded buffer (tasks E5–E7) absorbs the integration-level fixes.

**Corruption repair/restore flow** (hardening the §6 row; primitives — quick_check, verified backups, `.recover` — exist from E01/E07): on failed launch `quick_check` → non-destructive auto-`.recover` into a new file (original preserved) → on success, open recovered + report; on failure, guided restore picker over dated verified backups with catalog-stats preview. Scripted drill in CI (§7.2).

---

## 5. Data model additions (SQL migrations, `lightbox-catalog`)

Two migrations, numbered at implementation time against the then-current `schema_version` (copy-on-write upgrade semantics per §6 apply as always).

**Migration A — migration bookkeeping:**

```sql
CREATE TABLE migration_session (
  id                 INTEGER PRIMARY KEY,
  source_kind        TEXT    NOT NULL,             -- 'lrcat' | 'xmp_batch'
  source_path        TEXT    NOT NULL,
  source_fingerprint TEXT    NOT NULL,             -- xxh3(size, mtime, sqlite change_counter)
  started_at         INTEGER NOT NULL,
  finished_at        INTEGER,
  status             TEXT    NOT NULL DEFAULT 'running',  -- running|done|failed|cancelled
  options_json       TEXT    NOT NULL,
  report_json        TEXT
);

CREATE TABLE migration_map (
  session_id INTEGER NOT NULL REFERENCES migration_session(id) ON DELETE CASCADE,
  entity     TEXT    NOT NULL,     -- 'asset'|'image'|'folder'|'keyword'|'collection'|'smart_collection'|…
  src_key    TEXT    NOT NULL,     -- LR id_local (scoped) or id_global
  dst_id     INTEGER NOT NULL,
  PRIMARY KEY (session_id, entity, src_key)
) WITHOUT ROWID;
```

**Migration B — retouch fill bake** (coordinate with E12.4: if its spec already added equivalent columns, this migration reduces to the delta; the *shape* below is authoritative for the LaMa path):

```sql
ALTER TABLE retouch_op ADD COLUMN result_ref    TEXT;                      -- content-addressed baked patch (patches/<hh>/<hash>.lbpatch)
ALTER TABLE retouch_op ADD COLUMN fill_provider TEXT;                      -- 'lama' | 'patchmatch'
ALTER TABLE retouch_op ADD COLUMN model_pin     TEXT;                      -- provenance (model id+version+hash), nullable
ALTER TABLE retouch_op ADD COLUMN stale         INTEGER NOT NULL DEFAULT 0;-- explicit user action only (§4.5 policy)
```

**Preview-store layout addition** (§3.3): `patches/<hh>/<hash>.lbpatch` — zstd-compressed RGBA16F patch + header; content-addressed; participates in the store's relocation but **not** LRU eviction (it is recipe state, like `masks/`, not a cache). Note: bake references live in `retouch_op` (the authoritative retouch store, §3.1) and mutate on their own cadence without touching `edit_recipe.doc` — same single-owner discipline as `mask_component.cached_raster_ref`.

No changes to `edit_recipe.doc` schema, no new recipe fields beyond `lb_extra.migration` (already an open map; E09's schema is untouched).

---

## 6. Ordered task breakdown (each ≤ 1 day)

Workstreams A–E parallelize across 2 engineers; within a workstream, order is binding. ~40 task-days ≈ 8 pw.

### WS-A — `.lrcat` importer (13 d)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | `lightbox-migrate` crate scaffold; `LrcatReader::open` (ro/immutable, in-use detection, snapshot-copy path); schema-version probe; fixture catalogs committed (LR Classic 6, 12, current; small, self-authored) | Opens all fixtures; `CatalogInUse` on `-wal` present; unknown/cloud source → typed error, no panic |
| A2 | Entity readers: roots/folders/files/images (VC, rating, pick, label, orientation, capture time), streaming | **First failing test** `lrcat_fixture_inventory_counts` passes; iterators hold O(batch) memory on a 100k synthetic lrcat |
| A3 | Keyword tree + assignments; collections/sets + items + custom order; smart-collection rule text extraction | Fixture keyword hierarchy and collection order round-trip into test structs exactly |
| A4 | Data-only Lua-table parser + fuzz target | Parses all fixture rule/agprefs samples; fuzzer (1 h CI budget) finds no panic/OOM; malformed → `LuaParseError` |
| A5 | `map_smart_rules` → E07 `RuleTree` (subset per §4.1.4) + partial/unsupported policy | Golden rule-mapping table: ≥ 12 fixture smart collections map `Full`; partial/unsupported produce correct `dropped` lists |
| A6 | XMP extraction (`xmp_for` + sidecar fallback) → E09 `from_lr_crs` handoff; provenance + snapshot + history-seed writes | Fixture image with known LR edits yields a Recipe whose mapped fields match E09's documented mapping; unknowns land in `xmp_passthrough` |
| A7 | `inventory` + `plan()` + `VolumeResolver` path remapping; dry-run report | Plan resolves fixture roots on this machine; a deliberately-broken root shows `Missing` and is remappable; dry run mutates nothing |
| A8 | Catalog migration A (SQL §5) + executor phase 1: roots/folders/assets via ingest add-in-place, batched txns, `migration_map` idempotency | Re-running after interrupt creates zero duplicates; `kill -9` mid-phase leaves `integrity_check` clean + resumable |
| A9 | Executor phase 2: images/VCs, ratings/flags/labels, keywords, collections (+order), smart collections | Full fixture migrates; spot-check queries match LR-side truth table committed with the fixture |
| A10 | Executor phase 3: develop import + missing-file ghosts (`asset.missing=1`) + `skip_missing_files` option | Fixture with 10% missing files migrates per option; develop fidelity counters correct |
| A11 | `MigrationReport` + persistence (`report_json`, spillover file); `lightbox-cli migrate lrcat` | Headless CLI migrates a fixture end-to-end and emits the JSON report; exit codes distinguish warnings vs failure |
| A12 | Shell wizard (3 steps) + report view + expectation-setting copy; activity-center integration | Manual script: full wizard run on fixture; cancel + resume from UI works |
| A13 | Scale pass: synthetic 100k-lrcat generator; `migrate_100k` scenario wired into the perf harness | Organization-only ≤ 20 min, develop ≤ +15 min on reference hardware; memory bounded |

### WS-B — LaMa Remove (5 d)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `big-lama` model pack (hash pin, license manifest entries) + inferd `inpaint` op per E13 extension guide | Round-trip inpaint via inferd on CPU EP in an integration test; pack passes surface-1/3 gates |
| B2 | `InferenceClient::inpaint` client API; context-window prep (dilate/cap/pad, working↔sRGB conversion) | Unit tests: window geometry invariants; color round-trip ΔE within tolerance on synthetic patches |
| B3 | `LamaFillProvider` (E12.4 trait), patch placement + feather + optional seamless-clone harmonization; **bake**: catalog migration B (SQL §5) + `patches/` store + replay path | Fixture remove op renders; re-render replays **bit-identically** from bake with inferd killed; seam artifacts pass perceptual check on fixture set |
| B4 | Fallback ladder + availability affordance + refresh/re-roll (seed) as history-visible edit | Pack absent → PatchMatch result + download CTA; inferd crash mid-op → retry→fallback, session unaffected; refresh creates new bake + history step |
| B5 | Golden + perf: remove-op corpus goldens (per backend, §4.4 tolerance at the seam); `remove_op` scenario | Budgets: ≤ 2 s p95 GPU EP / ≤ 12 s CPU EP @ ≤ 1024² window; replay ≤ 5 ms render overhead |

### WS-C — Packaging (7 d)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | `xtask dist` staging from a bundle manifest; LGPL dylib layout (import-linked, replaceable); THIRD-PARTY-NOTICES + attribution generation | Staged tree on all 3 OS contains every runtime dep; a hand-swapped LibRaw dylib still loads (relink capability proven) |
| C2 | macOS: universal2 build, codesign (hardened runtime + entitlements), notarize + staple, `.dmg` | Clean-VM install passes Gatekeeper; app launches, imports, renders |
| C3 | Windows: installer (NSIS/WiX decision recorded), Authenticode signing, per-user install, file association | Clean-VM install with no SmartScreen hard-block (signed); uninstall removes app, preserves catalogs |
| C4 | Linux AppImage CI job (best-effort, non-blocking) | AppImage launches + imports on stock Ubuntu LTS VM; job failure does not block release |
| C5 | ORT component packaging (DirectML/CoreML/CPU only) + first-run download via E13 pack manager; installer ships without ORT | Fresh install: AI features show download CTA; post-download, segmentation + inpaint work; no CUDA/TensorRT artifact anywhere in staging (gate D3 cross-checks) |
| C6 | Manual update check (signed static JSON, explicit user action) + version plumbing | Airplane-mode: zero network calls ever; update check works when invoked; wrong-signature manifest rejected |
| C7 | Local crash/panic capture + "reveal log"; release build profile (LTO, symbols archived) | Injected panic produces a readable local report; nothing leaves the machine |

### WS-D — CI gates (7 d)

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | Finalize `deny.toml` + REUSE/SPDX lint + model-weights manifest gate (surface 1) | A test crate with a GPL dep fails PR CI; XMP Toolkit exception entry references CTO sign-off record |
| D2 | `xtask sbom emit` (CycloneDX from actual staging tree, per platform) | Emitted SBOM enumerates 100% of staged native artifacts; unlisted artifact in tree → emit fails (default-deny) |
| D3 | `xtask sbom check` + `policy/native-sbom.toml`: license/link-mode/build-flag policy + 3 hard gates (libde265-not-x265 dep-walk + deny-symbol scan; FFmpeg buildconf LGPL-only; ORT EP allowlist) | Seeded violations (x265-linked libheif, `--enable-gpl` FFmpeg, staged CUDA EP) each fail with a pointed message |
| D4 | Release-runner binary inspection job (otool/ldd/dumpbin walks over real artifacts, all 3 OS) | Release pipeline blocks on any policy violation; green on the honest tree |
| D5 | Surface 3: `data/MANIFEST.toml` schema + checker (default-deny, Adobe-provenance ban, attribution presence, `model_pack` cross-check) | Seeded Adobe-named `.dcp` in staging fails; missing lensfun attribution fails; current tree passes |
| D6 | Golden-image gate wiring: fast subset PR-blocking on 3 OS, full nightly, full release-blocking; per-PV immutability assertion in matrix | Deliberate 1-node output perturbation fails PR CI on all 3 OS |
| D7 | Fault-injection (`kill -9`) suite + scripted backup-restore drill promoted to release matrix; `xtask release` all-gates runner | `xtask release` runs surfaces 1–3 + goldens + fault-injection + perf subset and emits the readiness report; tag pipeline requires it |

### WS-E — Perf & v1 hardening (8 d)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | Perf harness skeleton (`lightbox-cli perf run`, JSON output, baseline diff) + synthetic 100k catalog generator + pinned raw corpus definition | Harness runs one trivial scenario in CI nightly and archives results |
| E2 | Scenarios: `import_10k`, `cull_latency`, `develop_open` | Budgets asserted per §4.5; failures emit traces |
| E3 | Scenarios: `slider_latency` (engine-boundary p95 + documented compositing allowance), `catalog_100k`, `export_throughput` + starvation probe, `memory_ceiling` | Same |
| E4 | Tracing spans on hot paths + chrome-trace toggle; perf triage runbook (`docs/perf-triage.md`) | A slider-latency regression is traceable to a node/stage in one capture |
| E5–E7 | Fix-to-budget buffer (3 d): triage nightly failures, land integration-level fixes, file crate-owner issues for the rest | All §7 budgets green on reference hardware for 5 consecutive nightlies before release cut |
| E8 | Guided corruption repair/restore flow (auto-`.recover` → verified-backup picker) + CI drill | Corrupted-catalog fixture: recover path succeeds non-destructively; recovery-impossible fixture: restore picker completes; original file never modified |

---

## 7. Test plan (per §8 strategy)

### 7.1 Unit / property / fuzz (PR-blocking)

- `LrcatReader` per-entity readers against the fixture matrix (LR 6 / 12 / current), incl. missing-table degradation.
- `parse_lua_table`: proptest round-trips + `cargo-fuzz` target (CI-budgeted); no panic/OOM on arbitrary bytes. `LrcatReader` fuzz over a mutated-fixture corpus (malicious `.lrcat` = untrusted input; feeds the §12 security review).
- `map_smart_rules` golden table: rule Lua → expected `RuleTree`/`dropped` for ≥ 12 real fixture rules + synthetic edge cases (nested groups, unknown criteria).
- Inpaint window prep: geometry invariants (dilation/cap/pad), working↔sRGB round-trip tolerance.
- SBOM/data-manifest checkers: table-driven policy tests — every hard gate has a seeded-violation test that must fail and a clean case that must pass.
- Migration SQL: schema-migration up test + `migration_map` idempotency (unique-key contract).

### 7.2 Integration (PR-blocking fast subset; full nightly)

- **Headless E2E migration**: `lightbox-cli migrate lrcat` on the primary fixture → assert catalog contents against a committed truth table (counts, one deep-checked image: VC + rating + label + keywords + collection order + mapped recipe fields + passthrough presence).
- **Resume/idempotency**: interrupt after phase 1 (process kill), re-run, assert zero duplicates + completed report.
- **`kill -9` fault injection** mid-migration and mid-remove-bake → `integrity_check` clean, resumable (extends the E01 suite).
- **Sidecar batch path**: folder of raws + LR-written `.xmp` sidecars → recipes applied, report correct.
- **Remove op through inferd**: brush → LaMa fill → bake → kill inferd → re-render replays bit-identical from `result_ref`; fallback ladder (pack absent / inferd down) produces PatchMatch result without session disruption.
- **Repair/restore drill**: corrupted + unrecoverable fixtures through the guided flow.
- **Packaging smoke** (release matrix): clean-VM install → launch → import fixture folder → develop edit → export JPEG, per OS; airplane-mode network-silence assertion.

### 7.3 Golden-image (PR fast subset; full nightly; release-blocking)

- Remove-op corpus: fixture regions × {LaMa bake, PatchMatch} → committed goldens. **Replay-from-bake must be bit-identical** (it composites a stored raster); the *composite* (feather/harmonization seam) must match goldens within §4.4 tolerance (ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB) across CPU/GPU backends.
- Migrated-recipe rendering: N fixture images with LR edits → import → render → goldens (guards the E09 mapping *as integrated* — drift fails here even if E09's unit mapping passes).
- E16 wires (does not author) the per-PV golden immutability gate into the 3-OS release matrix (D6).

### 7.4 Performance (nightly; release-blocking subset)

All §4.5 scenarios with §7-architecture budgets, plus `migrate_100k` and `remove_op`. Regression policy per §8 architecture: nightly regression → tracked issue; release branch → blocking.

---

## 8. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **`.lrcat` schema variance** across LR versions silently mis-imports | Version-gated readers; per-entity degradation with warnings (never silent drops — everything skipped is counted in the report); fixture matrix spanning LR 6→current; default-deny on identity tables |
| R2 | **Constraint-4 optics** of reading an Adobe-produced file format | Compliance position documented (§3): open SQLite format, user-owned data, no Adobe code/binaries touched, develop channel = ISO XMP. Flagged for explicit CTO acknowledgment (O2) |
| R3 | **Develop-fidelity disappointment** ("imports your catalog" read as "renders identically") — Risk 9 | Expectation copy in wizard + report fidelity counters per image; migrated-recipe golden corpus catches *our* drift; marketing/docs language reviewed against report 00 §"Interop fidelity expectations" |
| R4 | **LaMa quality on large/high-res fills** (2048² window cap → soft large fills) | Stated UI bound; harmonization pass; refinement tiling named as v1.x follow-up; PatchMatch comparison in goldens keeps the floor honest |
| R5 | **Signing/notarization operational lead time** (Apple Developer ID, Windows Authenticode/EV) | Certs requested at epic start (ops dependency, O5); CI uses dummy-signing until real certs land; C2/C3 acceptance requires real certs |
| R6 | **SBOM false confidence** (symbol-scan/dep-walk heuristics miss an exotic linkage) | Default-deny posture (unlisted artifact fails); curated deny-symbol list; per-release manual audit checklist attached to `xtask release` report; release-runner inspects *actual* binaries, never a hand list |
| R7 | **Late cross-crate perf regressions** land after owners have rolled off | Nightly harness from E1 onward (early signal); fix-to-budget buffer bounded at 3 d with escalation to crate owners; release requires 5 consecutive green nightlies |
| R8 | **E12.4 `FillProvider` seam drift** (trait shape differs from §4.2.2) | O1 resolved before B3 starts; provider logic is seam-thin by design (window prep + bake are trait-shape-independent) |
| R9 | **In-use/locked source catalogs** corrupt the migration UX | `CatalogInUse` detection + snapshot-copy path (A1); never open a hot `.lrcat` writable (we never open writable at all) |

### Open questions

- **O1** (blocks B3): final `FillProvider`/`FillContext` trait shape + retouch coordinate space — confirm against E12.4's spec; §4.2.2 is our proposal.
- **O2** (before release): CTO acknowledgment of the §3 `.lrcat`-reading compliance position (routine, mirrors the XMP-Toolkit sign-off pattern).
- **O3**: partial smart-collection default — spec says import-disabled; confirm with product/UX during A12.
- **O4**: update-manifest hosting + signing key custody (static JSON endpoint) — ops decision, needed by C6.
- **O5**: Windows cert class (EV vs OV; SmartScreen reputation implications) — ops decision, needed by C3.
- **O6**: whether `migrate_100k`'s 20-min budget holds on spinning-disk source catalogs — measure in A13; budget is SSD-referenced, document HDD expectation if materially worse.
- **O7**: fixture-catalog redistribution — fixtures are self-authored (our photos, our LR trial catalogs) and contain no Adobe assets; confirm they can live in the public repo vs. a private fixture store (repo-size + provenance review).

---

## 9. Definition of done

- [ ] All WS-A–WS-E tasks complete with acceptance criteria met; first failing test and all §7 suites green.
- [ ] **Mandate success scenario executed end-to-end on reference hardware, offline**: ingest & cull 3,000 raws → global + AI-masked local edits + a LaMa remove → export delivered JPEGs — from a **packaged, signed build**, network disabled throughout.
- [ ] `.lrcat` migration: primary fixture migrates with organization lossless (truth-table match), develop approximate with per-image fidelity report; 100k synthetic within budget; resume + `kill -9` safety proven.
- [ ] Remove tool: LaMa via inferd with baked bit-identical replay, PatchMatch fallback, refresh-as-edit; budgets met.
- [ ] Installers: Gatekeeper-clean notarized `.dmg`; signed Windows installer; Linux AppImage job exists (non-blocking); LGPL relink capability demonstrated; THIRD-PARTY-NOTICES + attributions complete.
- [ ] **All three license surfaces green and release-blocking** on the release matrix, with seeded-violation tests proving each hard gate fires; zero Adobe-authored assets in the staged tree (surface 3).
- [ ] Golden-image (incl. per-PV immutability), fault-injection + restore drill, and perf release subset wired into `xtask release`; tag pipeline refuses without it.
- [ ] All §7-architecture budgets green for 5 consecutive nightlies before the release cut.
- [ ] Guided repair/restore flow shipped and drilled.
- [ ] User-facing migration guide written (incl. the approximate-develop expectation, LR-version support matrix, `.lrcat-data`/AI-mask limitation).
- [ ] O1–O7 resolved or explicitly deferred with owner + date; CTO acknowledgments (O2) on record.

---

## 10. Seams to neighboring epics (named, not designed here)

| Epic | Seam | Direction |
|---|---|---|
| **E07** | `RuleTree` AST + collection/keyword/label command API — `map_smart_rules` targets it; any missing rule primitive is filed to E07, not extended here | E16 consumes |
| **E09** | `Recipe::from_lr_crs()`, `xmp_passthrough`, `lb_extra` — the entire develop-migration fidelity lives behind this call; E16 supplies XMP docs + provenance and reports what E09's mapping returns | E16 consumes; fidelity gaps filed to E09 |
| **E04 / `lightbox-ingest`** | Add-in-place registration (probe, hash, no copy) + `import_session` undo semantics — migration phase 1 is a client | E16 consumes |
| **E03** | Preview pyramid rebuild post-migration (`PreviewPolicy`); smart previews are E03's feature, not migrated | E16 triggers |
| **E06** | Job classes/cancel/pause + activity center for migration and remove jobs | E16 consumes |
| **E12.4** | Retouch object model, coordinate space, retouch render node, `FillProvider` trait + PatchMatch impl — E16 plugs `LamaFillProvider` in (O1) | E16 implements E12's trait |
| **E13** | inferd protocol extension mechanism, model-pack manager (download/hash/license), VRAM gating, supervision — E16 adds the `inpaint` op + `big-lama` pack per its guide; ORT-component download rides its mechanics | E16 extends via published mechanism |
| **E14** | Baked-raster reproducibility pattern (`cached_raster_ref`/`stale`) — E16 mirrors it for fill bakes; object-aware remove-selection (SAM scribble→object) is a named v1.x hook on E14's segmentation API | pattern reuse |
| **E05/E10/E11/E12** | Golden corpora + per-PV goldens are authored by the pixel epics; E16 owns only matrix wiring and release gating | E16 wires |
| **E15** | Export engine — used by E2E scenario, perf `export_throughput`, and backend-provenance recording on exports | E16 consumes |
| **E01** | WAL/backup/`quick_check`/fault-injection primitives; migration framework for §5 SQL — E16 builds the guided repair/restore UX and promotes the suites to release gates | E16 consumes/hardens |
| **E08** | Wizard/report/restore UI built from E08's widget kit; performance-preferences panel already binds the knobs the perf work tunes | E16 consumes |
