<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lightbox-preview`

Demand-driven preview provision and the E03 on-disk cache pyramid: the
tiered preview store (T0/T1/T2), the raw decode cache, the hot thumbnail
atlas, and their lifecycle (build, evict, retain, relocate, verify, purge).
Owned by **E03** (`docs/plan/epics/E03-preview-cache-pyramid.md`); this
README is the E03 spec's §11 DoD deliverable — enough to integrate against
this crate from E04 (import), E05 (rendered producers), E06 (job runtime),
or E08 (shell/prefs) without reading the source.

**Frozen seams other epics build against:** `PreviewProducer`/`PreviewCodec`
(§5.3), `RawCache` put/get + container v1 (§3.3/§5.4), `BlobStore` (§5.5),
and `BuildRuntime` (§5.6). Everything else in this crate is free to change
shape as later phases land.

## Everything this crate owns, in one picture

```
<lbdata>/
  catalog.sqlite          # E01's — never touched by this crate
  store.toml              # this store's manifest (format, uuid, relocated_from)
  previews/<hh>/<key>.t0.jpg        # T0: verbatim embedded JPEG, asset-scope
  previews/<hh>/<key>.t1.{jpg,jxl}  # T1: resized+recoded, image-scope
  previews/<hh>/<key>.t2/           # T2: tiled, image-scope
    manifest.cbor
    <x>_<y>.tile
  rawcache/<hh>/<content_hash>.<params_hash>.zst  # decoded-raw-stage cache
  smartpreview/            # reserved (E16), untouched at M0
  masks/<hh>/<hash>        # BlobStore namespace (E14), untouched at M0
  thumbcache.sqlite        # separate DB — hot grid-thumbnail atlas
```

`Store::open` (`store.rs`) creates the reserved directories + `store.toml`
idempotently; `thumbcache.sqlite` is created lazily by `ThumbCache::open`
the first time anything asks for a thumb.

## Tiers, scope, and the store-key scheme (spec §3.1)

| Tier | Scope | What it holds | Built by |
|---|---|---|---|
| T0 | **Asset** (shared across every virtual copy) | The largest embedded JPEG, verbatim bytes, un-rotated | `producer::ensure_t0` |
| T1 | **Image** | Resized + recoded standard-size preview (JPEG, or JXL behind the `jxl` feature) | `producer::ensure_t1` |
| T2 | **Image** | Tiled 1:1 loupe source, `t2_tile_px` grid (default 256²) | `t2::ensure_t2_synthetic` at M0; a real renderer producer at M1+ (E05) |

A store key is `xxh3_128(content_hash ‖ scope_tag ‖ scope_id ‖ tier ‖
variant_hash)` (`pyramid::derive_store_key`) — `scope_tag`/`scope_id` (0 =
Asset, 1 = Image, then the id itself) are included precisely so that two
virtual copies of one asset with identical edit-independent variant params
never collide on the same T1/T2 file even though they share a
`content_hash` (deviation A-5). Paths fan out one byte deep:
`previews/<hh>/<key_hex>.<tier_ext>` (`pyramid::t0_rel_path`/`t1_rel_path`/
`t2_rel_dir`). `VariantParams` (resize target, codec, quality, recipe
revision, …) canonicalize to a stable CBOR encoding before hashing
(`enc_ver`-tagged — any field change without a version bump fails a golden
hash-vector test, `pyramid::tests::golden_hash_vectors`).

T0 is un-rotated (as-extracted); T1 is un-rotated too (mirrors T0 — resizing
before rotation is safe because "long edge" is transpose-invariant).
**Decode-for-display bakes EXIF orientation uniformly, at read time, for
every tier** (`decode.rs`) — the store never holds pre-rotated pixels.

## Container formats

- **T0/T1**: a plain JPEG (or JXL codestream, `jxl` feature) file — no
  Lightbox-specific header. `checksum` (xxh3-64) lives in the catalog's
  `preview` row, not in the file.
- **T2**: a directory. `manifest.cbor` (fixed-order CBOR, written via the
  same atomic-write primitive as everything else) carries the grid geometry
  and a row-major presence bitmap; each present cell is one `<x>_<y>.tile`
  file. A tile absent from the bitmap is `Ok(None)` from `t2_tile_read`
  without ever touching disk for it.
- **Raw cache** (spec §3.3): `zstd`-framed with the frame's own built-in
  xxh64 checksum enabled (`ChecksumFlag(true)`) — corruption detection is
  that checksum exclusively, no second out-of-band hash column. A fixed
  binary header (`RawStageMeta`: payload schema, width/height/channels/
  sample format/color state) precedes the compressed payload inside the
  frame. Filename is `<content_hash_hex32>.<params_hash_hex16>.zst` — unlike
  the generic `derive_store_key` scheme, a raw-cache entry's identity IS
  literally its `(content_hash, params_hash)` pair.
- **Thumbnail atlas**: a `thumbcache.sqlite` row per `(image, recipe_rev,
  px)` — encoded JPEG bytes in a `BLOB` column, `WITHOUT ROWID` for warm-hit
  speed.

Every file write in this crate — T0/T1/T2 tiles, raw-cache containers,
`store.toml`, the relocation journal — goes through **one** primitive,
`store::atomic_write`: write to a same-directory `.tmp-*` file, `fsync`,
`rename`. A kill mid-write can only ever orphan a harmless `.tmp-*` file (or
a final-named file with no catalog row yet); it can never produce a torn
file under a name any reader would look up (deviation A-13; proven by
`tests/blob_crash_loop.rs`, `src/t2_crash_loop.rs`,
`src/store_crash_loop.rs`, `src/relocate_crash_loop.rs`).

## Eviction, retention, and the write-then-commit ordering

Every build writes its file (fully, atomically) **before** the catalog row
that references it is committed — never the other way around. This is the
single invariant that makes crash recovery simple: a kill can leave an
orphaned file with no row (harmless, swept by `verify_store(Full)`/
`RawCache::reconcile`), but it can never leave a row pointing at a missing
or torn file.

- **Preview pyramid** (`evict.rs`, T19): LRU-to-cap in **tier order** — T2
  evicts before T1 before T0 ("T0 is small and is the culling floor," spec
  §3.2). A candidate row is deleted from the catalog first (the durable,
  authoritative fact), then its file is unlinked **only if** no other row
  still references the same `store_path` (`store_path` refcount) **and**
  no live reader currently holds it open (`Store::unlink_tracked`'s
  live-handle guard; Windows queues the delete instead of failing —
  `Store::retry_deferred_deletes`). T2's age-retention sweep
  (`evict::sweep_t2_retention`, default 30 days, `PreviewStoreConfig::
  t2_retention`) uses the identical refcount-then-unlink primitive.
  `PreviewService::discard` (`Command::DiscardPreviews`) is the same
  mechanism, selection-scoped rather than cap-triggered.
- **Raw cache** (`rawcache.rs`, T17/T18): a single flat LRU by
  `last_used_at`, evicted to `CacheLimits::rawcache_cap_bytes` (default
  5 GiB) via an optimistic-concurrency compare-and-delete
  (`delete_rawcache_entry_if_unchanged`) so a concurrent refresh can't be
  evicted out from under itself. An ENOSPC pre-flight
  (`rawcache/diskspace.rs`) evicts proactively before a `put()` that would
  exceed the free-space margin, publishes `PreviewEvent::CachePressure`, and
  fails typed (`RawCacheError::DiskFull`) — never a panic — if eviction
  can't make room.
- **Thumbnail atlas** (`thumbs.rs`, T22): row-count LRU (no byte-cap column
  in the schema — thumbs are near-uniform JPEGs at one `px`, so row count is
  a fine proxy), capped at `ThumbCache::DEFAULT_MAX_ROWS`.
- **Preview-pyramid cap** default 20 GiB, **raw-cache cap** default 5 GiB
  (`CacheLimits::default()`); both are runtime-settable via
  `PreviewService::set_limits`/`Command::SetCacheLimits`.

## Reconcile / verify model (T21)

`PreviewService::verify_store(mode)`:

- **`Quick`** (cheap, run at open): for every indexed row, does its file
  (T0/T1) or directory (T2) exist? Missing entries are reported, **never**
  mutated.
- **`Full`** (idle/background or CLI): everything `Quick` does, PLUS a
  checksum spot-check on T0/T1 rows (a mismatch drops the row **and** the
  file), an orphan sweep (a file/`.t2` directory under `previews/` with no
  matching row is removed), and an orphaned-`.tmp-*` sweep
  (`Store::sweep_orphan_temp_files`). Every action `Full` takes is on data
  this store's own contract already calls disposable (spec §3.2) — nothing
  it does can lose a catalog-authoritative fact.

`RawCache::reconcile` is the raw cache's own, narrower analog: dangling rows
(row exists, file doesn't) are dropped; orphan files that decode/checksum
cleanly are re-adopted into the catalog; orphans that don't are deleted;
unrecognized filenames are removed outright. The raw cache's stated bar
(spec §8 test plan) is **"always reconcilable,"** not "never diverges" — it
is disposable cache by design, so `reconcile` heals divergence rather than
asserting it never happens.

`PreviewService::verify_quick`/`purge_all` (Phase D) remain as narrower,
pre-Phase-F cousins kept for the existing CLI/tests (deviation D-8);
`verify_store`/`purge(PurgeScope)` are the fuller Phase F primitives.

## Relocation (T21)

`PreviewService::relocate(new_root, progress)` copies the E03-owned surfaces
(`previews/`, `rawcache/`, `smartpreview/`, `masks/`, `store.toml` — never
the catalog database or `thumbcache.sqlite`, both excluded by design; see
`relocate.rs`'s module doc comment) to `new_root`, **journaled**
(`<old_root>/.relocate-journal`, one fsynced `DONE <relpath>` line per
copied file) so a kill mid-copy loses at most the last journal line, never a
file — a resumed run at worst re-copies one file it already had (idempotent:
same source bytes). Only after every file is copied does the new root's
`store.toml` get `relocated_from` set — the durable "this new root is
complete" signal. Concurrent reads against the OLD root are unaffected for
the whole copy phase (nothing at `old_root` is touched until the flip
succeeds); the old root's cache directories are cleared only after that.
The caller is responsible for reopening a fresh session pointed at
`new_root` afterward — `relocate` does not hot-swap the live `Store`.
`src/relocate_crash_loop.rs` proves this with a real `SIGKILL` mid-copy,
not just a hand-seeded journal.

## The facade (`PreviewService`, spec §5.2)

`lightbox-core` registers one `PreviewService` per session
(`Session::preview_service()`), composing the store, the RAM index
(`PreviewIndex`, a mirror of the catalog's `preview` table kept current by
every build this service drives), the priority scheduler (`sched.rs` —
Visible > Neighbor > Bulk, dedup by `(image, tier)`, cooperative
cancellation checked immediately before a queued build is dispatched), the
raw cache, and the thumbnail atlas. Key methods: `best_available`,
`request`/`set_viewport`/`bulk_build`, `thumb`, `t2_tile`, `raw_cache()`,
`stats`/`verify_quick`/`verify_store`, `evict_to_cap`/`discard`/
`sweep_t2_retention`, `set_limits`/`relocate`/`purge`. `lightbox-core`
additionally exposes `Command::{BuildPreviews, DiscardPreviews,
SetCacheLimits, RelocateCacheStore, PurgeCaches}` on the core command bus
and republishes `PreviewEvent::{Evicted, CachePressure}` as core
`Event::{PreviewEvicted, CachePressure}` (plus `CacheRelocated`/
`CachePurged` for the two Phase-F background commands).

Separately, `EmbeddedPreviewProvider` (the E01-seeded `PreviewProvider`
trait implementation) is what the grid/loupe actually poll for pixels — a
second, independent RAM index mirror exists there by design (documented in
`service.rs`'s module doc comment); the two stay eventually consistent
through the same catalog, never directly coupled.

`lightbox-cli preview build|stat|verify|purge` drives the T14-era subset of
this surface headlessly (`crates/lightbox-cli/src/preview.rs`).

## Performance budgets (spec §6) and crash safety

`cargo bench -p lightbox-preview` (`benches/preview_budgets.rs`) measures
every §6 metric this crate's public surface can exercise directly:
`best_available` at 100k synthetic rows, cold T0 decode+store, T1
build (resize+encode at the real 3840px standard size), warm `thumb()`,
raw-cache `get`/`put` at a 140 MB payload, and raw-cache eviction reclaim
latency. See the T23 delivery report (or re-run the bench) for current
numbers on your machine — every number that ships in this repo's docs is a
real local measurement, never estimated.

Fault-injection coverage (`SIGKILL` loops, real subprocess kills, not
simulated): `tests/blob_crash_loop.rs` (T02, atomic blob IO),
`src/t2_crash_loop.rs` (T20, T2 manifest survival), `src/store_crash_loop.rs`
(T21/T23, interleaved T0/T1/raw-cache writes → `verify_store(Full)`/
`RawCache::reconcile` clean), `src/relocate_crash_loop.rs` (T21, mid-relocate
kill → resume → zero lost entries). `crates/lightbox-catalog/tests/
fault_injection.rs` additionally drives the `preview`/`raw_cache_entry`
catalog-DAO write paths from its own shared kill-loop harness. Each has a
`LIGHTBOX_*_FAULT_ITERS` env var (default: a fast PR-blocking iteration
count; set higher for a nightly-strength run — see that epic's deviations
log for exact defaults and the PR/nightly split rationale).
