<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E03 — Preview pyramid & raw cache — deviations log

Append-only. Every departure from the E03 spec
(`E03-preview-cache-pyramid.md`) or a decision the spec left to the
implementer is recorded here with its rationale. Reference: CLAUDE.md exit-bar
rule ("Record spec deviations in `docs/plan/epics/<EPIC>-deviations.md`") and
the E03 task prompt's honest-reporting rule.

---

## Phase A — store foundation (T01–T05)

### A-1 — rawler reconciliation (task-prompt instruction, not a spec deviation)

**What.** The E03 spec's §2 crate table lists `rawler` (MIT, "metadata/preview
extraction only at M0") as a new dependency. The Phase A task prompt overrides
this: rawler is **banned** workspace-wide (LGPL-2.1 in its actual published
license, not MIT as the spec assumed; E02's own deviations log already
records this reconciliation and `deny.toml` already lists `rawler` under
`[bans].deny`). Phase A doesn't touch extraction at all (that's Phase B,
T06), so this entry is a confirmation, not a code change: `deny.toml` already
enforces the ban, and `cargo deny check` (part of the exit bar) passed clean
with zero new dependency-graph changes to that file. Phase B's agent must
build `EmbeddedProducer` on `lightbox-decode`'s own permissive walkers +
`kamadak-exif`, per the task prompt, not on rawler.

### A-2 — migration number 0004, and no prior `preview` stub table existed

**What.** `docs/plan/migrations.md` reserved `preview` for E03 without a
number; Phase A claims **0004** (next-in-sequence after 0001/0002/0003) and
records it as shipped. `cargo xtask lint-migrations` is green.

The architecture doc (§3.3) and the E03 spec (§4 preamble: "E01's schema v1
carries the §3.1 `preview` stub... the migration **recreates** it rather than
altering") both narrate a pre-existing stub `preview` table from E01. It does
not exist: `crates/lightbox-catalog/migrations/0001_spine.sql` has no
`preview` table. The migration's `DROP TABLE IF EXISTS preview;` is kept
verbatim from the spec anyway (documented in the migration file's header) —
it's a harmless no-op today and preserves the "recreate, don't ALTER" policy
if a future migration needs to reshape the table again.

### A-3 — `PreviewService::open` (T01 AC) is satisfied by `Store::open`, not a `PreviewService` type

**What.** T01's acceptance criterion literally names `PreviewService::open`
creating the §3.2 layout + `store.toml`. The spec's §5.2 `PreviewService` is
explicitly Phase D's facade (T14): it composes the scheduler, decoded LRU,
and producer dispatch that don't exist until Phases B–D land. Defining a
`PreviewService` type now with a narrower `open(cfg) -> Result<Self, ..>`
signature would either collide with or have to be broken by T14's real
`open(cfg, catalog, rt, events) -> Result<Self, PreviewError>`.

**Resolution.** Phase A ships `Store::open(cfg: &PreviewStoreConfig) ->
Result<Store, PreviewError>` (`crates/lightbox-preview/src/store.rs`) as the
Phase-A-owned primitive underneath the eventual facade: it does exactly what
the AC describes (creates `previews/`, `rawcache/`, `smartpreview/`,
`masks/`, and `store.toml`, idempotently). Phase D's `PreviewService::open`
is expected to call `Store::open` internally as its first step. Covered by
`store::tests::open_on_empty_dir_creates_layout_and_manifest` and
`reopen_is_idempotent_and_preserves_identity`.

### A-4 — `PreviewIndex::best_available` takes an explicit `asset: AssetId`

**What.** The spec's §5.2 signature is `best_available(&self, image: ImageId,
min_long_edge: u32)`. Phase A's `PreviewIndex::best_available` (the RAM-index
half of that method, `crates/lightbox-preview/src/index.rs`) additionally
takes `asset: AssetId`.

**Why.** Every asset-scope (T0) `preview` row carries only `asset_id` — no
`image_id`, by definition of "asset scope" (spec §4 DDL: `image_id` is
`NULL` for T0). An image whose asset has **only** a T0 built (no T1/T2 ever
requested) cannot be resolved to its owning asset from the `preview` table
alone; that `image -> asset` mapping lives in the catalog's `image` table,
which is E01's, not something E03's index mirrors (mirroring it would
duplicate a join E03 doesn't own and isn't asked to keep in sync).

**Resolution / who closes this.** Phase D's `PreviewService::best_available`
(T14) is expected to match the spec's exact two-argument signature by
resolving `asset` from the image context it already has at the call site
(e.g. `ImageDetail::asset`, already fetched by the caller for any other
reason it's looking at that image) and delegating to
`PreviewIndex::best_available` — zero *added* IO on the hot path, since the
resolution is already in hand rather than a fresh catalog query triggered by
this method. Documented in `index.rs`'s module doc comment as well, so a
Phase D implementer sees it without reading this file.

### A-5 — `derive_store_key`'s byte-precise `scope_discriminator` layout (T05)

**What.** Spec §3.1: `key = hex(xxh3_128(content_hash ‖ scope_discriminator ‖
tier ‖ variant_hash))`. The spec states this as prose; it does not pin the
exact bytes of `scope_discriminator`, and a naive reading ("just a byte
saying asset-vs-image") creates a real collision: two virtual copies of one
asset (same `content_hash`) with otherwise-identical `VariantParams` (e.g.
both freshly duplicated at `recipe_rev = 0`) would derive the **same** T1/T2
store key and silently share one file, even though they are different images
that may diverge.

**Resolution.** `derive_store_key` (`crates/lightbox-preview/src/pyramid.rs`)
defines the layout precisely: `content_hash (16B) ‖ scope_tag: u8 (0=Asset,
1=Image) ‖ scope_id: u64 LE ‖ tier: u8 ‖ variant_hash: u64 LE`. Including the
scope **id** (not just the tag) for both scope kinds keeps the formula
uniform while getting the right dedup behavior in each case: T0 rows still
dedupe across assets sharing a `content_hash` when constructed with the
`AssetId` those assets share (the intended "T0 dedupes by content" behavior,
spec §3.1), while T1/T2 rows for two different `ImageId`s never collide even
under identical `VariantParams`. Covered by golden hash vectors
(`pyramid::tests::golden_hash_vectors`) and a dedicated regression test
(`image_scope_keys_never_collide_across_images_sharing_a_content_hash`).

### A-6 — `BlobStore` blob filenames carry no extension

**What.** The architecture doc's §3.3 illustrative tree shows
`masks/<hh>/<hash>.png` for baked mask rasters. The generic
`BlobStore::put(bytes) -> BlobRef` (spec §5.5) is content-type-agnostic by
construction — it hashes raw bytes and has no way to know "this happens to be
a PNG." Phase A's `BlobStore` (`crates/lightbox-preview/src/store.rs`) names
files by the bare 32-hex content address, no extension. A domain owner (E14
for masks) interprets the bytes by its own convention; nothing about the
`BlobStore` primitive needs a filename extension to do that. This is a
narrower, deliberately generic reading of the illustrative path in the
architecture doc, not a contradiction of the frozen §5.5 API (which never
mentions an extension).

### A-7 — `VariantHash` catalog-column byte order: little-endian, not the repo's usual big-endian

**What.** Several xxh3-128 values elsewhere in this codebase
(`lightbox_decode::hash_file`, `Recipe::canonical_hash`, DCP/look content
ids) standardize on **big-endian** byte rendering — chosen historically for
`xxhsum`/cross-tool/hex-display compatibility. `VariantHash` (xxh3-**64**) is
different: it only ever travels as the catalog's `preview.variant_hash` BLOB
column, an internal cache key never hex-displayed to a user or another tool.
Phase A picked **little-endian** (`VariantHash::to_le_bytes`/
`index.rs::to_desc`'s `u64::from_le_bytes`) for that column specifically —
the native, cheap direction, since there is no cross-tool contract to honor
here. `VariantHash::to_be_bytes` is also provided (matching the repo-wide
convention) for any future consumer that *does* need to hex-display a
variant hash. **Action for Phase B/C:** when wiring the real
`EmbeddedProducer` to call `CatalogTxn::upsert_preview`, serialize
`VariantParams::variant_hash()` with `.to_le_bytes()`, not `.to_be_bytes()`,
to match `index.rs`'s reader. Documented inline at both call sites.

### A-8 — `preview`/`raw_cache_entry` timestamps are unix-second `INTEGER`, not RFC3339 `TEXT`

**What.** Every other catalog table stamps `TEXT` RFC3339-UTC via
`clock::now_rfc3339_utc()` (fixed-width, lexicographic-order-is-chronological
convention, E01 Phase 3). The E03 spec's own §4 DDL already specifies
`built_at`/`last_used_at` as `INTEGER` "unix seconds" for `preview` and
`raw_cache_entry` — not a Phase A invention, but flagged here because it's an
intentional, deliberate departure from the rest of the schema: `last_used_at`
is touched on every cull/scroll and batched off the interactive path (spec
§3.2 "Accounting write pressure"), so cheap numeric comparison/storage is
worth the inconsistency. Documented in the migration file header and in
`preview_dao.rs`'s module doc comment so it isn't mistaken for an oversight
by a later reader.

### A-9 — `thumbcache.sqlite` is not created by `Store::open`

**What.** The architecture §3.3 tree lists `thumbcache.sqlite` as a `.lbdata`
sibling. It is explicitly Phase F's (T22) — a separate SQLite DB, its own
schema, "delete-safe — not the catalog." `Store::open`'s `RESERVED_DIRS`
constant creates `previews/`, `rawcache/`, `smartpreview/`, `masks/` only.
Named here per the task prompt's "mark the rest with the owning task" rule.

### A-10 — no `tokio-util` dependency added; no `BuildRuntime`/`CancellationToken` wiring in Phase A

**What.** The spec's §5.6 `BuildRuntime` seam and §3.4 scheduler both name
`tokio_util::sync::CancellationToken`. Phase A adds neither the dependency
nor a `BuildRuntime` trait definition: nothing in T01–T05 performs
cancellable work (that starts at Phase B's embedded-extraction pipeline and
Phase D's scheduler). Adding the dependency now with nothing to use it would
be dead surface area; Phase D (T13, the scheduler) is the natural place to
add `tokio-util` and define `BuildRuntime`. Note `lightbox-jobs::CancelToken`
already exists in-graph (used by the E01-seeded `EmbeddedPreviewProvider`)
and is a plausible alternative Phase B/D could reuse instead of introducing a
second cancellation-token type — left as an open question for that phase,
not decided here.

### A-11 — raw cache and T2 tile paths are NOT routed through the generic `BlobStore`

**What.** `rawcache/<hh>/<content_hash>.<params_hash>.zst` (spec §3.3) and
T2's per-tile paths (spec §4.3, Phase F) encode more than a single content
hash in their filename (content hash **and** params hash; tile coordinates).
They are not instances of the generic content-addressed `BlobStore`
mechanism (spec §5.5, which is explicitly the T0/T1 preview / masks /
smartpreview namespace mechanism) — they get their own key-derivation and
atomic-write logic in Phases E/F. `Store::open` still eagerly creates the
`rawcache/` top-level directory (a Phase-A-owned reserved dir, per §3.2), but
Phase A does not implement `RawCache::get`/`put` or any `<hh>` fan-out logic
for it — that is Phase E's T17.

### A-12 — incidental: `crates/lightbox-cli/tests/e2e.rs` schema-version literals updated 3 -> 4

**What.** Two integration-test assertions hardcoded `"schema version 3"` /
`"ok: schema version 3"` (the pre-E03 schema head). Migration 0004 legitimately
advances the schema head to 4 on every fresh `catalog create`. Updated both
literals; not a spec deviation, just a consequence of adding a migration that
a pre-existing test in another crate happened to hardcode. Verified via
`cargo test --workspace` (both green post-edit).

### A-13 — atomic-write crash test: "zero torn visible files" reinterpreted to exclude harmless orphaned temp files

**What.** T02's AC: "SIGKILL-during-1k-writes crash-loop leaves ZERO torn
**visible** files." A literal "zero `.tmp-*` files ever" reading is
unachievable by construction: a kill between temp-file-create and `rename`
legitimately orphans a `.tmp-*` file (expected residue of any
temp-then-rename scheme; see `store.rs::atomic_write`'s doc comment on Risk
R6). The crash-loop test
(`crates/lightbox-preview/tests/blob_crash_loop.rs`) reads "visible" as "the
final content-addressed name `BlobStore::get` would ever look up" — orphaned
temp files are never addressed by that lookup, so they are not "visible" in
the sense that matters. The test asserts every **finally-named** file
self-verifies (its bytes re-hash to its own filename) across 30 kill
iterations x up to 1000 writes each, and reports (without failing on) the
orphaned-temp-file count as informational — sweeping them is
`verify_store(Full)`'s job (spec §3.2, Phase F), out of Phase A's scope.
Observed: 0 torn files, ~270 harmless orphaned temp files across 1097
file-observations in a representative local run.
