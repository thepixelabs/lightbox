# E01 — Implementation deviations from spec

Deviations from `E01-foundation-workspace-catalog.md`, recorded per phase.
Each entry names the spec point, the deviation, and why.

## Phase 1 — Workspace & guardrails

- **Project license selected: Apache-2.0.** Neither the mandate nor the epic
  spec pins the project's own license (mandate constraint 5 requires
  "permissive or weak-copyleft"). Apache-2.0 chosen (ecosystem default,
  patent grant); LICENSE, `LICENSES/`, SPDX headers, and `REUSE.toml` all use
  it. Flag for CTO review if dual MIT/Apache-2.0 is preferred.
- **Crate versions (July 2026) newer than spec-era assumptions:** wgpu 30.0.0
  (`Instance::new` takes `InstanceDescriptor` by value, no `Default` —
  `new_without_display_handle()` used; `request_adapter` returns
  `Result<Adapter, RequestAdapterError>`), thiserror 2.x, serde 1.0.228,
  tracing-subscriber 0.3.23. Spec interfaces unaffected. Edition **2021 kept**
  as the spec/architecture §1.1 explicitly pins it, despite edition 2024
  being current. Toolchain pinned 1.96.1; MSRV recorded as 1.96.
- **xxh3-128 via `twox-hash` (MIT), not `xxhash-rust`.** The commonly-used
  `xxhash-rust` crate is BSL-1.0, which is not on the spec §5 T3 license
  allowlist; `twox-hash` 2.x provides xxh3-128 under MIT. Pins use the
  canonical (big-endian) hex rendering; `lightbox-decode::hash_file`
  (Phase 5) must adopt the same canonicalization.
- **Fixture corpus: 9 CC0 raws instead of "≈12", plus generated non-raws.**
  All seven spec'd mounts are covered (CR2, CR3, NEF ×2, ARW, RAF ×2 — one
  Bayer, one X-Trans — ORF, DNG), ~116 MB total, chosen to keep the CI cache
  small; the manifest schema makes adding more a one-entry change.
  raw.pixls.us hosts raws only, so the JPEG/TIFF/PNG legs and the two corrupt
  fixtures are **deterministically generated** by `cargo xtask fixtures`
  (spec implied downloads for the whole corpus); their manifest entries use a
  `generator` spec instead of a `url`, still hash-pinned and licensed
  (CC0, self-made). The tiny JPEG is a committed 826-byte self-made asset
  (`tools/xtask/assets/`) so its pin can never drift with an encoder upgrade.
- **`docs/plan/licensing.md` and this file created under `docs/`** — the
  epic-execution ground rules say "do not modify docs/", read as no *edits*
  to approved plan documents; both files are new, spec-mandated artifacts
  (T3, and the deviation-recording rule itself).
- **`lightbox-edit`'s frozen `Recipe { schema, pv }` placeholder implemented
  in Phase 1** as part of crate stubbing (spec §2 defines the crate's entire
  E01 content as exactly this placeholder; no later E01 phase claims it, and
  Phase 2's `RenderRequest` needs it).
- **`deny.toml` additions beyond the spec'd allowlist mechanics:**
  `allow-wildcard-paths = true` (intra-workspace path deps carry no version),
  `multiple-versions = "warn"`, yanked-crate denial, and an explicit ban on
  `openssl`. The license allowlist itself is exactly the spec's list.
- **CI toolchain install uses `rustup show active-toolchain`** (rustup
  auto-installs the toolchain pinned in `rust-toolchain.toml`) instead of a
  third-party toolchain action — fewer moving parts, same pin.
- **T4 observability lives in `lightbox-core::observability`** (spec doesn't
  name a crate). File logging is synchronous write-through (daily rotation via
  `tracing-appender`) rather than a buffered background writer, which is what
  makes "panic hook that logs + flushes" trivially true at M0 log volumes.
- **nightly.yml not created in Phase 1.** The workspace layout in spec §2
  lists it, but both of its jobs (long fault-injection, perf harness) are
  Phase 3/8 deliverables (T13, T28); an empty workflow would be noise.

## Phase 2 — Zero-copy seam tracer bullet

- **wgpu downgraded 30 → 29 (29.0.4) workspace-wide.** Current stable
  egui/eframe/egui-wgpu (0.35.0) link against wgpu ^29, and the zero-copy
  seam (§2.3 seam 2) requires the shell and the engine to share ONE
  `wgpu::Device` — i.e. one wgpu crate version. Phase 1's wgpu 30 pin was
  provisional; the APIs Phase 1 used are identical in 29. Revisit when egui
  moves majors.
- **`CancelToken` (a T14 item) pulled forward into Phase 2** and implemented
  in `lightbox-jobs` (tokio `sync` feature only). The frozen §3.4/§3.5
  surfaces (`GpuCtx`, `SourceResolver::resolve`) carry it, so the Engine seed
  could not compile without it. The rest of T14 (`JobSystem`, `Class`,
  `spawn`/`spawn_blocking`, `JobHandle`) remains Phase 4.
- **`RenderPlanner` scaffolding added (non-frozen).** §3.4 defines the ticket
  lifecycle but not how the seed engine maps a `RenderRequest` to its single
  node + params. `Engine::set_planner(Arc<dyn RenderPlanner>)` fills that
  hole, documented as M0 scaffolding that E05.1's recipe→DAG builder
  replaces. The frozen `Engine::new(gpu, registry, sources)` signature is
  unchanged. Phase 6 plans `display.transform`; Phase 2's tracer plans
  `solid.color`.
- **Engine output textures are `Rgba8Unorm`, not `Rgba8UnormSrgb`.** egui
  0.35's user-texture path samples registered textures as "normal"
  non-sRGB-aware gamma data (and its docs require `Rgba8Unorm`), and the M0
  node contract already has nodes writing sRGB-*encoded* values explicitly
  (the display-transform algorithm ends with an OETF encode). Same bytes,
  correct compositing, and the format stays storage-capable for Phase 6
  compute output.
- **egui 0.35 API drift from the spec's sketch:** `eframe::App::update` is now
  `App::ui(&mut self, ui: &mut egui::Ui, …)`, and panels are `egui::Panel::top`
  etc. shown inside a parent `Ui`. `egui_wgpu::Renderer::register_native_texture`
  / `free_texture` / `ui.image` exist as the spec assumed; T7 implemented
  verbatim with re-registration on texture swap.
- **T7's "device pointer equality" assertion** is `Arc::ptr_eq` on the
  `GpuContext.device` the shell built from eframe's `RenderState` vs the one
  the engine reports (wgpu 29 `Device` has no public id/PartialEq). The
  stronger runtime proof is structural: wgpu validation would reject
  registering a texture created on any *other* device, so every composited
  frame re-proves the shared-device seam.
- **Shell seam smoke added to CI as `continue-on-error`** (`lightbox --smoke
  N` runs N frames headless-windowed — Xvfb on Linux — and exits nonzero if
  no `Engine::submit` texture was composited). T5's AC wants the eframe
  window proven on all 3 CI OSes; windowed apps on hosted runners are the one
  piece not verifiable from this machine, so the step is observational until
  seen green, then promoted to blocking (tracked for T29). Verified locally
  on macOS/Metal: `seam_proven=true`, exit 0.
- **Scoped cargo-deny exceptions added (deny.toml), global allowlist
  untouched:** `clipboard-win` + `error-code` (BSL-1.0 — Boost, OSI-approved
  permissive; Windows clipboard via egui-winit→arboard) and
  `epaint_default_fonts` (OFL-1.1 AND Ubuntu-font-1.0 — egui's embedded UI
  fonts; font-data licenses, i.e. the first real entry for license surface 3
  (bundled content, E16)). `docs/plan/licensing.md` was NOT updated alongside
  (deny.toml's own rule) because this epic's ground rules forbid editing
  docs/ beyond this file — needs a follow-up licensing-review commit.
- **`solid.color` node + `SolidColorPlanner` are public in
  `lightbox-render::nodes`** (spec T6 calls it a "test node"): the T7 shell
  spike and the T6 integration tests both need it, and it remains useful as
  the minimal example node. Documented as tracer/test-only, not a product
  pipeline node.
- **Ticket-table retention policy (unspecified by the spec):** terminal
  tickets are dropped on the next submit to the same viewport and capped at
  256 globally; polling an evicted ticket reads `Superseded`. This is what
  lets the texture pool actually recycle (a retained `Ready` state would pin
  its texture's refcount forever).

## Phase 3 — Catalog

- **Four keyset-pagination indexes added to migration `0001` beyond the §4.3
  DDL:** `asset_added(added_at)`, `asset_filename(filename)`,
  `asset_folder_capture(folder_id, capture_time)`,
  `asset_folder_added(folder_id, added_at)`. T10's AC ("every `images_page`
  query plan uses an index — no full scans") is unsatisfiable with the §4.3
  index set for the AddedAsc/Desc and global-FilenameAsc sort orders and for
  folder-filtered capture/added sorts. Verified by `EXPLAIN QUERY PLAN`
  tests over every sort × folder-filter × cursor combination (no `SCAN`
  without `USING INDEX`, no `USE TEMP B-TREE`); measured at 100 k synthetic
  assets: worst page fetch ~0.21 ms (criterion, T10's informal proof).
- **Generated column `camera` added to `asset` in `0001`:** FTS5
  external-content tables require the content table to expose a column per
  FTS column, and the spec's `assets_fts` declares `camera` (make ⊕ model)
  with `content='asset'`, which has no such column — integrity-check,
  column reads, and E07's planned rebuild all fail with "no such column:
  T.camera". A `VIRTUAL` generated column (same `trim(coalesce(...))`
  expression as the spec's triggers) keeps §4.3's FTS shape working
  unchanged. The §4.3 triggers themselves are verbatim.
- **Timestamps are RFC3339 UTC with fixed 6-digit subseconds**
  (`…T18:30:00.000000Z`). §4.3 requires lexicographic == chronological;
  variable-precision RFC3339 (e.g. `time`'s default `Rfc3339` formatter)
  violates that across rows whose subsecond digit counts differ.
- **Backup dated dirs get a `-N` suffix on same-second collisions**
  (`2026-07-05-183000-2`); §4.4 names only `YYYY-MM-DD-HHMMSS`. Suffixes are
  allocated max+1 and never reused, and pruning/`newest-backup` order by the
  parsed `(stamp, suffix)` key — naive lexicographic ordering plus name
  reuse let a burst of same-second backups prune the newest one (caught by
  the T12 retention test).
- **`Catalog::create` refuses an existing catalog** (`AlreadyExists`);
  "create→open idempotent" (T8 AC) is read as create-then-reopen applying
  migrations exactly once, not as create-twice.
- **`remove_import_session` deletes the `import_session` row too**, not just
  the session's asset/image rows — "removes catalog rows only" (§3.2) is
  read as "as opposed to files on disk".
- **`NewAsset` carries `decode_error`** so T18's "catalogue probe/hash
  failures" lands in the same single insert transaction;
  `mark_decode_error` exists per §3.2 for post-insert failures.
- **`with_txn` contains closure panics** (`catch_unwind` → rollback →
  `CatalogError::Internal`; the writer thread survives). The spec doesn't
  address panics; a poisoned writer would otherwise wedge every later
  mutation.
- **Crate choices (current versions, July 2026):** rusqlite 0.40
  (`bundled` amalgamation — FTS5 included — + `backup` feature), zstd 0.13
  (statically linked C libzstd, BSD-3-Clause/MIT — first native C dep beside
  SQLite itself, both spec-mandated; noted for E16's SBOM surface), time 0.3
  (timestamp formatting), criterion 0.8 + fastrand 2 (dev-only).
- **`docs/plan/migrations.md` created under `docs/`** — spec-mandated
  artifact (§2 layout, T9 AC), same reading of the ground rules as Phase 1's
  licensing.md. Registry format: markdown table linted by
  `cargo xtask lint-migrations` (duplicate numbers, unregistered or
  misnamed migration files, gaps in the shipped sequence) — wired into CI.
- **`nightly.yml` created with the 1000-iteration fault-injection job only**
  (3-OS matrix); the T28 perf harness joins it in Phase 8. The PR-gate
  fault-injection subset (50 kills) runs inside `cargo test` as spec'd; the
  `synchronous=OFF`/DELETE-journal negative control is documented (not
  shipped) in the harness's module docs, including why demonstrating it
  needs power-cut simulation rather than SIGKILL.
- **Fault-harness journaling protocol hardened:** asset inserts journal
  after commit (journal ⊆ committed), rating overwrites journal
  intent-before/done-after — plain after-commit journaling of overwrites
  would misreport a kill landing in the commit→journal window as a lost or
  phantom write. Parent tolerates a torn final journal line (a kill can
  interrupt the journal append itself).

## Phase 4 — Jobs & core façade

- **`SourceTier` moved to `lightbox-types`** (re-exported from
  `lightbox-render::source`, so the spec §3.5 path is unchanged). §3.6's
  `DecodedImage.tier` and §3.5's `SourceImage.tier` share the type across
  crates; hosting it in the render crate would have dragged wgpu into
  `lightbox-preview`'s dependency tree.
- **The frozen `PreviewProvider` surface (§3.6) landed in Phase 4, its
  implementation stays Phase 5 (T21).** `Session::previews()` (§3.8) needs
  the trait to exist; until `EmbeddedPreviewProvider` lands, sessions hand
  out an `UnavailablePreviewProvider` that fails every request immediately
  (`PreviewError::Unavailable`) instead of pretending pixels are coming.
  `PreviewTicket` (a type §3.6 uses but never defines) is an opaque
  provider-allocated id with a public constructor so E03 can implement the
  trait outside E01's crate.
- **`lightbox-decode`: `hash_file` (a T19 item) implemented in Phase 4;
  `probe()` is the stub T17 explicitly tolerates** ("T19 stub returns
  `Unsupported` until Phase 5 lands"). Import needs real content hashes for
  dup-skip; it does not need real metadata. The full §3.7 type surface
  (`ProbedFormat`/`AssetProbe`/`EmbeddedPreviewInfo`/`ProbeError`) is in
  place; `read_embedded`/`decode_raw`/`decode_image` declarations arrive
  with Phase 5/E02 as spec'd. `hash_file` renders xxh3-128 big-endian —
  the same canonicalization as the fixture pins (Phase 1 note).
- **T18's failure-cataloguing rule refined at the hash boundary:** probe
  failures are catalogued with `decode_error` set *and* reported in
  `ImportReport.errors` (spec verbatim); **hash failures get an errors entry
  but no row** — `content_hash` is `NOT NULL` by the §4.3 schema, and a file
  that cannot be read has no identity to dedupe or relink by. With the
  Phase-4 probe stub, probe *errors* (vs. `Unsupported` results) are nearly
  unreachable, so the corrupt-fixture `decode_error`-row AC is completed by
  Phase 5's real probe; the pipeline mechanism is in place and the
  hash-failure leg is tested now.
- **`ImportReport` lives in `lightbox-ingest` and is re-exported by
  `lightbox-core`** (§3.8 sketches it inside core): one definition, no
  duplicate type to keep in sync. Same for `ImportOptions`. The session's
  `stats` JSON stores `{cancelled, report}` — the `cancelled` marker
  implements T17's "session marked finished-with-stats" for cancelled
  imports without adding a field to the spec's report shape.
- **`InsertOutcome` gained `skipped: Vec<usize>`** (batch indices of
  dup-skipped rows; additive to Phase 3's shape) so the import report can
  attribute per-file outcomes (unsupported counts exclude duplicates)
  without a second catalog query.
- **Import discovery details the spec leaves open:** hidden entries
  (dot-names: `.DS_Store`, AppleDouble `._*.jpg`) are skipped below the
  root; discovered files are sorted by path (deterministic batches);
  `source_dir` is canonicalized before becoming a `library_root` path;
  `volume_uuid` is stored `NULL` at M0 (platform volume-UUID lookup goes
  with E04's device/card work, per T9's "the import path's job" note).
  Known-extension list covers the seven fixture raw mounts + jpg/jpeg,
  tif/tiff, png, pef, rw2.
- **Undo-import leaves `folder` rows behind** (assets/images/import_session
  rows are removed exactly, FTS follows by trigger — asserted in tests).
  `remove_import_session` (Phase 3) never touched folders; empty-folder
  reconciliation is E07's fs-reconciliation territory. T18's "exact
  pre-import row counts" is asserted for asset/image/import_session/FTS.
- **`JobSystem::new` panics if the tokio runtime cannot be built** — the
  frozen §3.3 signature returns `JobSystem`, not a `Result`; a dead runtime
  is startup-fatal anyway. Documented on the method.
- **Async job cancellation is select-based at await points** (the job future
  is dropped, resolving `Cancelled`) *plus* cooperative via the token;
  blocking jobs are purely cooperative (`f(&CancelToken)` checkpoints, 50 ms
  budget per T14). `JobHandle::try_result` consumes the result exactly once;
  `join` after that returns `JobError::ResultTaken` (spec silent on the
  interaction). Panics in job bodies are contained as `JobError::Panicked`.
- **Additive, non-frozen accessors:** `JobSystem::handle()` (runtime handle
  for long-lived system tasks — the core's command dispatcher must not
  consume a class budget slot), `JobSystem::running(class)` (test/diagnostic
  gauge), `Core::jobs()`. E06 may replace all three.
- **Command bus ordering semantics (spec silent):** trivial writer commands
  (`SetRating`/`SetFlag`/`UndoImport`) run strictly in submission order,
  awaited one at a time by the dispatcher; `ImportAddInPlace` (Foreground)
  and `BackupNow` (Background) spawn as jobs so the queue never blocks
  behind them — a rating edit mid-import commits immediately, interleaved
  on the single writer. `submit()` never blocks via an unbounded command
  queue (user-scale traffic; the event side stays bounded/lossy as spec'd).
- **`Session::close` drains before backing up:** close cancels the
  session-scoped token tree, waits (bounded by `CoreConfig::close_wait`,
  default 10 s) for in-flight command jobs — a cancelled import still
  flushes its finished-with-stats session row — then runs the exit backup.
  `CloseOpts` carries a `ClosePolicy` (`Auto` = spec's 24 h rule via new
  `Catalog::newest_backup_time()` (additive, parses the dated dir name);
  `Always`; `Skip` for tests). Since `Session` is clone-cheap, `close`
  consumes one handle and hard shutdown happens at last-drop; commands
  submitted after close fail with `CommandFailed`.
- **Session engine wiring at Phase 4 is deliberately inert:** empty
  `NodeRegistry`, `NullSourceResolver`, unconfigured planner — the §3.8
  `engine()` seam exists and the ticket lifecycle runs, but real rendering
  through the session arrives with Phase 6 (T23) exactly as the task order
  implies. `Event::DeviceDegraded` is wired to `Engine::on_device_lost`.
- **`ChangeSet` (a type §3.8 names but never defines):** coarse
  `{ images: Vec<ImageId>, folders: bool, all_images: bool }`;
  `CommandTicket` is a session-unique `u64` newtype.
- **T17's fixture-corpus AC is deferred to Phase 5 with the probe stub**
  (rows land, but as `UNSUPPORTED` with zero dims until T19 replaces the
  stub); the T17/T18 ACs that don't depend on real metadata — dup-skip
  re-import, cancel-commits-complete-batches, per-file failure cataloguing,
  undo, 1 k-import heartbeat (< 16 ms max gap asserted) — are tested now
  against synthetic files.
- **T15's kill -9-during-close-backup harness self-calibrates:** the parent
  measures a full close-with-backup on a 60 k-row catalog and samples kill
  delays inside that window (asserting ≥ 1 kill actually landed mid-window);
  a fixed spec-style delay was observed to always miss the window on fast
  machines. A kill mid-backup can leave a `.tmp-*` scratch dir under
  `backups/` — never promoted, ignored by newest/prune (verified by the
  harness); cosmetic cleanup is left to future backups' hygiene (E16).
- **New third-party crates:** `walkdir` 2 (Unlicense OR MIT — T17 names it)
  and `unicode-normalization` 0.1 (MIT OR Apache-2.0 — §4.3 NFC filenames).
  Both pass the cargo-deny license gate unchanged.

## Phase 5 — Probe & previews

- **rawler replaced by in-crate permissive container walkers (T19).** The
  spec's "rawler metadata path for raws" collides with its own PR-blocking
  license gate: rawler is **LGPL-2.1** (all published versions, incl. 0.7.2,
  checked July 2026), and architecture §1.6/deny.toml deny copyleft in the
  crate graph ("LGPL only via dynamic FFI, never as a crate"). Following the
  Phase 1 precedent (xxhash-rust → twox-hash), the dependency was replaced,
  not allowlisted: `lightbox-decode` now ships a bounded TIFF/IFD walker
  (CR2/NEF/ARW/ORF/DNG/plain TIFF + embedded TIFF blobs), an ISO-BMFF walker
  for CR3 (CMT1/CMT2 metadata, THMB/PRVW/track-JPEG previews), and a RAF
  header parser — structures only, bounds-checked, iteration-capped,
  cycle-guarded; malformed input is `Err`, never a panic. Verified against
  an independent (python) walk of the pinned corpus at authoring time; all
  seven fixture mounts probe correctly. Coverage risk R4 now applies to our
  walkers instead of rawler; E02's LibRaw-sandbox fallback seam is unchanged.
- **New third-party crates (all pass the deny gate unchanged):**
  `kamadak-exif` 0.6 (BSD-2-Clause; + its dep `mutate_once`, BSD-2-Clause) —
  spec-named, used for JPEG APP1 and PNG eXIf fields; `zune-jpeg` 0.5
  (MIT/Apache-2.0/Zlib; + `zune-core`) and `fast_image_resize` 6 (MIT/
  Apache-2.0) per §3.6. TIFF files ride the same in-crate walker as the
  raws rather than kamadak-exif (one parser, already required for preview
  byte ranges).
- **Content-first sniffing; the extension plays two spec-mandated roles.**
  Magic bytes pick the walker. Unrecognizable content behind an extension
  that *claims* a supported format (`.cr2`, `.jpg`, …) is
  `ProbeError::Malformed` — satisfying the T19 AC that the corrupt corpus
  "returns Err" — and ingest catalogues it as a `decode_error` row (T18);
  unrecognizable content behind unknown extensions stays
  `ProbedFormat::Unsupported` (catalogued, badged) per §3.7. Recognizable
  but unimplemented containers (HEIC-brand BMFF, Panasonic RW2's 0x0055
  TIFF magic) are `Unsupported`, not `Malformed`.
- **JPEG originals are their own "embedded preview"** (whole-file byte
  range in `AssetProbe::embedded`) so the M0 provider serves imported JPEGs
  through the same pipeline; PNG/TIFF originals report none and render the
  grid placeholder until E02 decodes them.
- **capture_time:** probe returns "RFC3339 with offset if present" (§3.7
  verbatim): local time bare when the file has no `OffsetTimeOriginal`,
  `±HH:MM`-suffixed when it does (Z 6/R6/fp fixtures). §4.3's
  "UTC-normalized storage" note is thus only approximated at M0 — ingest
  stores the probe string verbatim (Phase 4 behavior, unchanged);
  UTC-normalizing at the ingest seam is E04/E07 cleanup if wanted.
- **Full-size dims for raws are best-metadata, not decode-derived:** EXIF
  `PixelX/YDimension` when present, else the largest non-preview IFD's dims
  (sensor-area, e.g. E-1 2624×1966 vs 2560×1920 output), else the largest
  embedded preview's SOF dims (the RAF path — its metadata JPEG *is* the
  full-size reference). Committed expectations:
  `crates/lightbox-decode/tests/probe_expectations.toml`.
- **T19's "< 20 ms probe" AC is logged, not hard-asserted** (warm probes
  measure ~0.1–1.3 ms on the dev machine; the test asserts a generous 250 ms
  blowup guard because CI timing asserts flake — the formal budget belongs
  to T28's perf harness).
- **`decode_raw`/`decode_image` declared returning
  `DecodeError::Unimplemented`** (spec: "declared but unimplemented") with
  `#[non_exhaustive]` placeholder types (`DecodeOpts`, `MosaicImage`,
  `LinearImage`) whose real bodies E02 designs; `camera_matrix_base` is NOT
  declared (§3.7 doesn't spell its signature — freezing a guess would bind
  E02 wrongly).
- **"Tiny embedded preview" rule (T20 AC):** a rendition is unusable when
  its long edge < 256 px *and* it doesn't cover the original's full
  dimensions (a small JPEG original is its own image and always usable; the
  E-1's 160×120 thumb inside a 5 MP raw is not). Loupe requires a usable
  rendition; `Thumb{max_px}` relaxes the bar to `min(max_px, 256)`. Below
  it: `PreviewError::NoEmbedded` → grid placeholder. The Sigma fp DNG
  fixture (no JPEG rendition at all) covers the "no" case.
- **`AssetLocator` seam added in `lightbox-preview`** (`ImageId → path +
  effective orientation`): §3.6 leaves `EmbeddedPreviewProvider`'s
  construction unspecified, and the provider must not depend on
  `lightbox-catalog`; `lightbox-core` implements it over the reader pool
  (`image_detail` + `asset_abs_path`). Documented as wiring, not part of
  the frozen surface.
- **Provider semantics the spec leaves open:** `cancel()` also *releases*
  ticket state (idempotent; polling a released ticket = `Cancelled`) —
  callers that stop polling must cancel, which §5.3 already mandates for
  the grid; thumbs bake orientation *after* the downscale (cheaper, same
  pixels); thumb selection picks the smallest rendition covering the
  requested edge (cheapest decode), loupe the largest; a decoded image
  larger than the whole cache budget is served but not cached; per-ticket
  failures (`Io`/`Decode`) don't poison the provider or the job counters.
- **`cargo-fuzz` seed target lives at `crates/lightbox-decode/fuzz`**
  (workspace-`exclude`d so the nightly-only libFuzzer crate never enters
  the PR-gate graph), fuzzing `probe()` + `read_embedded()` with
  extension-varied inputs; non-gating at M0 per T19.
- **Phase 4 ingest tests updated for the real probe:** synthetic import
  trees now write real tiny-JPEG payloads (unique EOI-trailing pads keep
  content hashes distinct) so success-path tests still test success;
  junk-content-behind-supported-extension behavior got its own T18 test
  (row lands with `decode_error` set, per-file report entry, batch not
  aborted). `Session::previews()` now hands out the real provider — the
  Phase 4 placeholder test asserts the request→job→poll lifecycle against
  an unknown image id instead of `Unavailable`.

## Phase 6 — The real node & goldens

- **Golden sources are synthetic, not the fetched raw corpus.** T23's AC
  reads "golden test per fixture orientation case (1,3,6,8 at minimum)", but
  every raw fixture in the pinned corpus carries EXIF orientation 1, and the
  corpus itself is not committed (goldens must verify from a bare checkout,
  offline). The golden matrix therefore renders a deterministic in-test
  source card through `Engine::submit` at **all eight** orientations
  (params-driven) plus two native-scale legs — exceeding the 1/3/6/8
  minimum — with goldens committed at
  `crates/lightbox-render/goldens/display.transform/pv1/` (~0.2–0.4 KB
  each; the `<pv>` path segment is spelled `pv1`). Fixture previews are
  covered end-to-end by a `lightbox-core` integration test (import real
  fixtures → `Session::engine()` → render: CR3 → 240×160, JPEG original →
  its own preview, previewless Sigma fp DNG → `Failed(Source)`, plus
  byte-stability across runs).
- **Goldens are blessed from the CPU path only; the GPU path is verified
  against them** (and against the CPU output — the T24 parity leg). The
  spec leaves bless-source policy open; a GPU-blessed golden would vary by
  backend. `LIGHTBOX_BLESS=1` under CI fails (T22 AC), detected via the
  `CI` env var. Observed on Metal: GPU output byte-identical to CPU.
- **Comparator semantics the spec leaves open:** ΔE2000 is computed over
  RGB (through Lab, D65, CIEDE2000 per Sharma 2005 — validated against the
  paper's published test pairs); PSNR is computed over **all four RGBA**
  channels so alpha regressions cannot hide (Lab has no alpha axis).
  p99 is nearest-rank. Dimension mismatch is a hard error, never a ΔE.
- **Planner policy (non-frozen scaffolding): `FitWithin` never upscales**
  (output = oriented source fitted, capped at native size) — honest about
  M0 embedded-preview resolution, matching T26's loupe wording; `Native` =
  oriented source size. Pinned by `output_size` unit tests + golden sizes.
- **`SourceResolver` M0 impl lives in `lightbox-core`
  (`render_source.rs`), not `lightbox-preview`:** the resolver needs both
  the `PreviewProvider` and `lightbox-render`'s trait, and Phase 4's
  `SourceTier` deviation deliberately keeps wgpu out of
  `lightbox-preview`'s tree. It requests the **loupe-class** rendition
  (largest embedded preview, unoriented) at `Class::Interactive` and polls
  on the render worker with cancel checkpoints; `RenderScale` is unused at
  M0 (documented — E03's tiered store keys tiers off it behind the same
  seam). Orientation comes from the catalog via the Phase-5 `AssetLocator`;
  a provider that reports `orientation_applied` resolves as O1
  (double-rotation guard).
- **GPU/CPU determinism details documented in `display_transform.md`:**
  both paths share written filter math (pixel-center bilinear in oriented
  space, taps mapped through the EXIF table; explicit `a*(1-t)+b*t` lerp —
  not WGSL `mix`); CPU quantizes round-half-up, GPU float→unorm tie
  behavior is backend-defined; `pow` may differ by ULPs — all bounded by
  ±1 LSB, inside the §4.4 perceptual gate. The CPU path (rayon over rows,
  each pixel a pure function) is byte-stable across runs and thread counts
  — asserted at both the render-crate and session level; the
  `lightbox-cli render --cpu` byte-stability AC itself lands with the CLI
  in Phase 7 (T27) on top of this property.
- **New third-party crates (deny gate green, allowlist untouched):**
  `png` 0.18 (MIT/Apache-2.0; pulls fdeflate/miniz_oxide/crc32fast/
  simd-adler32, all allowlisted) for golden + failure-artifact IO, and
  `rayon` 1.12 (MIT/Apache-2.0 — spec-named for the CPU path). Committed
  golden PNGs are covered by a `REUSE.toml` annotation
  (`crates/*/goldens/**`, Apache-2.0 — self-generated content, no
  third-party pixels).
- **CI: golden-failure artifact upload step added to ci.yml**
  (`actions/upload-artifact@v4`, `if: failure()`, over
  `target/tmp/golden-failures/` where the harness writes
  actual/heatmap/golden PNGs) — the "failure artifact uploaded in CI" half
  of the T22 AC.
