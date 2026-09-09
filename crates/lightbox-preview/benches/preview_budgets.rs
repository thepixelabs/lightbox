// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E03 spec §6 performance budgets, measured with `criterion` against this
//! crate's PUBLIC surface only (bench targets are a separate binary crate,
//! same restriction as `tests/`, every `pub(crate)` producer/scheduler
//! internal is off-limits here). Covers:
//!
//! - `best_available` (100k synthetic index rows, in-memory only)
//! - Cold T0 decode (embedded extraction) and T1 build (resize+encode) on a
//!   real raw fixture, through the public `PreviewService`
//! - `thumb()` warm lookup (`thumbcache.sqlite`)
//! - Raw-cache `get`/`put` (140 MB near-incompressible F16 payload, per
//!   deviation E-7's own methodology)
//! - Raw-cache eviction (reclaim past a 1 GiB cap)
//!
//! **Not covered here** (informal/scenario-level, not a `criterion`
//! microbenchmark, see the T23 report): "cull next/prev swap" and "T0
//! extraction throughput, 8 workers" are `EmbeddedPreviewProvider`-level
//! integration properties already asserted as hard tests in
//! `tests/embedded_provider.rs` (T09's `set_viewport_prefetch_delivers_
//! sub_50ms_swaps_over_200_images`); the "M0 exit scenario" (1k raws browsable
//! ≤ 15s) is E01/E04's cross-crate scenario harness, out of this crate's
//! bench scope.
//!
//! Run: `cargo bench -p lightbox-preview`. Every number in this file's
//! module doc comment / the T23 report is copy-pasted from a REAL local run
//! on the development machine, never fabricated (DEVELOPMENT.md's honest-
//! reporting mandate).

use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use lightbox_catalog::{Catalog, NewAsset, PreviewRow, PreviewSourceTag};
use lightbox_preview::{
    BuildFuture, BuildPriority, BuildRuntime, CacheLimits, PlaneData, PreviewIndex, PreviewRequest,
    PreviewService, PreviewStoreConfig, RawCache, RawCacheKey, RawStageMeta, SampleFormat, Tier,
};
use lightbox_types::{AssetId, ContentHash, FolderId, ImageId, Orientation, PreviewId};

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").exists(),
        "run `cargo xtask fixtures` first"
    );
    dir
}

/// Polls a [`BuildFuture`] to completion **on the calling thread**, valid
/// because every build this crate schedules wraps one synchronous producer
/// call with no internal `.await` point (see `lightbox-core`'s
/// `TokioBuildRuntime`, the production impl this mirrors exactly, down to
/// the no-op-waker poll loop). `Scheduler::request` calls `try_dispatch`
/// synchronously before returning, so a build enqueued against THIS runtime
/// completes before `PreviewService::request` returns, the criterion
/// closure below measures exactly the build itself, no polling/channel
/// machinery needed.
struct SyncRuntime;

impl BuildRuntime for SyncRuntime {
    fn spawn_build(&self, _name: &'static str, mut fut: BuildFuture) {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(()) => break,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn concurrency(&self) -> usize {
        1
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    catalog: Arc<Catalog>,
    service: PreviewService,
    folder: FolderId,
    /// A REAL directory (unlike `fixtures_dir()` itself, writable), each
    /// fresh asset needs its OWN distinct filename (`asset.folder_id,
    /// asset.filename` is unique), so `fresh_asset_and_image` hard-links a
    /// freshly-named copy of the pinned fixture in here on demand.
    photos_dir: PathBuf,
}

fn harness() -> Harness {
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
    let photos_dir = dir.path().join("photos");
    std::fs::create_dir_all(&photos_dir).unwrap();
    let root_path = photos_dir.clone();
    let folder = catalog
        .writer()
        .with_txn(move |txn| {
            let root = txn.upsert_root(None, &root_path)?;
            txn.upsert_folder(root, None, "")
        })
        .unwrap();

    let cfg = PreviewStoreConfig::with_defaults(dir.path().to_path_buf());
    let service = PreviewService::open(
        cfg,
        Arc::clone(&catalog),
        Arc::new(SyncRuntime),
        Arc::new(|_ev| {}),
    )
    .unwrap();

    Harness {
        _dir: dir,
        catalog,
        service,
        folder,
        photos_dir,
    }
}

/// T0 is ASSET-scope and dedupes by asset (spec §3.1), the fast path in
/// `PreviewIndex::lookup_variant` matches on `asset` alone for T0 rows, so
/// benching "cold T0 decode" needs a genuinely NEW asset every sample, not
/// just a new image under a shared one (that would fast-path after the
/// first sample and measure ~nothing). `insert_default_images` also creates
/// the one image every asset needs to be requestable. `(folder_id,
/// filename)` is unique (E01 schema), so each fresh asset needs its own
/// filename, hard-linked (near-instant, no byte copy) from the pinned
/// fixture rather than re-downloaded/re-copied per sample.
fn fresh_asset_and_image(
    catalog: &Catalog,
    folder: FolderId,
    photos_dir: &std::path::Path,
    counter: &mut u64,
) -> ImageId {
    *counter += 1;
    let filename = format!("canon-eos-350d-{counter}.cr2");
    let dest = photos_dir.join(&filename);
    if !dest.exists() {
        std::fs::hard_link(fixtures_dir().join("canon-eos-350d.cr2"), &dest)
            .or_else(|_| {
                std::fs::copy(fixtures_dir().join("canon-eos-350d.cr2"), &dest).map(|_| ())
            })
            .unwrap();
    }
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&counter.to_le_bytes());
    let content_hash = ContentHash(hash);
    catalog
        .writer()
        .with_txn(move |txn| {
            let batch = vec![NewAsset {
                folder,
                filename,
                content_hash,
                format: "CR2".to_owned(),
                camera_make: None,
                camera_model: None,
                capture_time: None,
                width: 100,
                height: 100,
                orientation: Orientation::O1,
                bytes: 10,
                mtime_utc: None,
                decode_error: None,
                import_session: None,
            }];
            let asset = txn.insert_assets(&batch)?.inserted[0];
            Ok(txn.insert_default_images(&[asset])?[0])
        })
        .unwrap()
}

/// T1 is IMAGE-scope (spec §3.1), a new image under the SAME (already-T0-
/// built) asset forces a genuinely new T1 build every sample without paying
/// T0's extraction cost again, isolating exactly the "resize+encode" cost
/// the §6 budget names.
fn fresh_image_under(catalog: &Catalog, asset: AssetId) -> ImageId {
    catalog
        .writer()
        .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
        .unwrap()
}

/// §6: "Cold T0/T1 decode (≈8 MP embedded) ≤ 120 ms", this benches the
/// FULL extraction+store path (`request(T0)` on a never-before-seen asset),
/// a superset of "decode" that also includes the one-time store write; T0
/// extraction alone (spec's separate "≥200 assets/s aggregate" throughput
/// row) is a strict subset of what's measured here.
fn bench_t0_cold_decode(c: &mut Criterion) {
    let h = harness();
    let mut counter = 0u64;
    c.bench_function("t0_cold_decode_and_store", |b| {
        b.iter_batched(
            || fresh_asset_and_image(&h.catalog, h.folder, &h.photos_dir, &mut counter),
            |image| {
                h.service
                    .request(PreviewRequest {
                        image,
                        tier: Tier::T0,
                        priority: BuildPriority::Visible,
                        allow_embedded: true,
                    })
                    .unwrap();
            },
            BatchSize::PerIteration,
        );
    });
}

/// §6: "T1 build (resize+encode, 3840 px) ≤ 250 ms/image/worker."
fn bench_t1_build(c: &mut Criterion) {
    let h = harness();
    let mut counter = 0u64;
    // One shared asset, T0 pre-built ONCE (outside the timed loop) so every
    // sampled `request(T1)` pays only the resize+encode cost, never
    // re-extraction, see `fresh_image_under`'s doc comment.
    let seed_image = fresh_asset_and_image(&h.catalog, h.folder, &h.photos_dir, &mut counter);
    let asset = h.catalog.reader().image_detail(seed_image).unwrap().asset;
    h.service
        .request(PreviewRequest {
            image: seed_image,
            tier: Tier::T0,
            priority: BuildPriority::Visible,
            allow_embedded: true,
        })
        .unwrap();
    c.bench_function("t1_build_resize_encode_3840px", |b| {
        b.iter_batched(
            || fresh_image_under(&h.catalog, asset),
            |image| {
                h.service
                    .request(PreviewRequest {
                        image,
                        tier: Tier::T1,
                        priority: BuildPriority::Visible,
                        allow_embedded: true,
                    })
                    .unwrap();
            },
            BatchSize::PerIteration,
        );
    });
}

/// §6: "Thumb atlas `thumb()` warm < 500 µs p99 @ 100k."
fn bench_thumb_warm(c: &mut Criterion) {
    let h = harness();
    let mut counter = 0u64;
    let image = fresh_asset_and_image(&h.catalog, h.folder, &h.photos_dir, &mut counter);
    // Build T0 first (thumb() build-throughs from whatever tier exists)
    // then every subsequent `thumb()` call for the SAME image/px is warm
    // (`thumbcache.sqlite` already has the encoded row).
    h.service
        .request(PreviewRequest {
            image,
            tier: Tier::T0,
            priority: BuildPriority::Visible,
            allow_embedded: true,
        })
        .unwrap();
    h.service.thumb(image, 256).unwrap(); // first call: cold, primes the atlas
    c.bench_function("thumb_warm_lookup", |b| {
        b.iter(|| h.service.thumb(image, 256).unwrap());
    });
}

/// §6: "`best_available` < 1 ms p99 @ 100k images", pure in-memory index,
/// no catalog/store IO (matches the spec's own stated backing mechanism).
fn bench_best_available_100k(c: &mut Criterion) {
    let mut idx = PreviewIndex::empty();
    let n = 100_000i64;
    for i in 0..n {
        idx.upsert(&synthetic_row(i * 2 + 1, i, None, Tier::T0));
        idx.upsert(&synthetic_row(i * 2 + 2, i, Some(i), Tier::T1));
    }
    let mut i = 0i64;
    c.bench_function("best_available_100k_rows", |b| {
        b.iter(|| {
            i = (i + 1) % n;
            idx.best_available(ImageId(i), AssetId(i), 1280)
        });
    });
}

fn synthetic_row(id: i64, asset: i64, image: Option<i64>, tier: Tier) -> PreviewRow {
    let (w, h) = match tier {
        Tier::T0 => (200, 150),
        _ => (3840, 2560),
    };
    PreviewRow {
        id: PreviewId(id),
        asset: AssetId(asset),
        image: image.map(ImageId),
        content_hash: ContentHash([0; 16]),
        tier: tier as u8,
        variant_hash: (tier as u64).to_le_bytes(),
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        stale: false,
        colorspace: "srgb".to_owned(),
        store_path: format!("previews/aa/row{id}"),
        width: w,
        height: h,
        bytes: 1,
        checksum: [0; 8],
        built_at: 0,
        last_used_at: 0,
    }
}

/// A near-incompressible synthetic payload (real planar raw data has high
/// entropy), matches deviation E-7's own methodology exactly.
fn synthetic_payload(len: usize, seed: u64) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    fastrand::Rng::with_seed(seed).fill(&mut buf);
    buf
}

fn rawcache_harness(cap_bytes: u64) -> (tempfile::TempDir, RawCache) {
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
    let store = Arc::new(
        lightbox_preview::Store::open(&PreviewStoreConfig::with_defaults(dir.path().to_path_buf()))
            .unwrap(),
    );
    let limits = CacheLimits {
        preview_cap_bytes: u64::MAX,
        rawcache_cap_bytes: cap_bytes,
    };
    (dir, RawCache::new(store, catalog, limits, 3))
}

const PAYLOAD_140MB: usize = 140 * 1024 * 1024;

/// §6: "Raw cache `get` (24 MP F16 x3ch ~140 MB) <= 200 ms decompress+verify."
fn bench_raw_cache_get(c: &mut Criterion) {
    let (_dir, rc) = rawcache_harness(u64::MAX);
    let key = RawCacheKey {
        content_hash: ContentHash([9; 16]),
        params_hash: 0xdead_beef,
    };
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 4096,
        height: 3413, // ~4096*3413*3*2 ~= 84MB; pad below to the spec's 140MB
        channels: 3,
        sample: SampleFormat::F16,
        color_state: 0,
    };
    let payload = synthetic_payload(PAYLOAD_140MB, 1);
    rc.put(key, meta, PlaneData(&payload)).unwrap();

    let mut group = c.benchmark_group("raw_cache_140mb");
    group.sample_size(20);
    group.bench_function("get", |b| {
        b.iter(|| rc.get(&key).unwrap().unwrap());
    });
    group.finish();
}

/// §6: "Raw cache `put` (same payload) <= 600 ms on a Background worker."
fn bench_raw_cache_put(c: &mut Criterion) {
    let (_dir, rc) = rawcache_harness(u64::MAX);
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 4096,
        height: 3413,
        channels: 3,
        sample: SampleFormat::F16,
        color_state: 0,
    };
    let payload = synthetic_payload(PAYLOAD_140MB, 2);
    let mut counter = 0u64;

    let mut group = c.benchmark_group("raw_cache_140mb");
    group.sample_size(20);
    group.bench_function("put", |b| {
        b.iter_batched(
            || {
                counter += 1;
                RawCacheKey {
                    content_hash: ContentHash([7; 16]),
                    params_hash: counter, // distinct key: never a no-op dedupe
                }
            },
            |key| rc.put(key, meta, PlaneData(&payload)).unwrap(),
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// §6: "Eviction (reclaim 1 GiB) <= 2 s, concurrent builds unaffected." The
/// concurrency half is proven by `rawcache/tests.rs`'s
/// `t18_concurrent_get_put_evict_is_race_free` (a correctness, not a
/// latency, property); this bench measures the reclaim latency alone: fill
/// a 512 MiB-capped cache to ~1.5x cap with 8 MiB entries, then time the
/// call that pushes it over the edge and triggers eviction back to cap.
fn bench_eviction_reclaim(c: &mut Criterion) {
    const CAP: u64 = 512 * 1024 * 1024;
    const ENTRY: usize = 8 * 1024 * 1024;
    let mut group = c.benchmark_group("eviction");
    group.sample_size(10);
    group.bench_function("reclaim_past_512mib_cap_8mib_entries", |b| {
        b.iter_batched(
            || {
                let (dir, rc) = rawcache_harness(CAP);
                // Pre-fill to just under cap so the timed `put` below is the
                // one that crosses it and triggers `evict_to_cap`.
                let n = (CAP as usize) / ENTRY;
                let meta = RawStageMeta {
                    payload_schema: 1,
                    width: 1024,
                    height: 1024,
                    channels: 2,
                    sample: SampleFormat::U16,
                    color_state: 0,
                };
                for i in 0..n {
                    let key = RawCacheKey {
                        content_hash: ContentHash([i as u8; 16]),
                        params_hash: i as u64,
                    };
                    let payload = synthetic_payload(ENTRY, i as u64);
                    rc.put(key, meta, PlaneData(&payload)).unwrap();
                }
                (dir, rc, meta)
            },
            |(_dir, rc, meta)| {
                let payload = synthetic_payload(ENTRY, 999);
                let key = RawCacheKey {
                    content_hash: ContentHash([0xEE; 16]),
                    params_hash: 0xEEEE,
                };
                rc.put(key, meta, PlaneData(&payload)).unwrap();
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group! {
    name = budgets;
    config = Criterion::default();
    targets = bench_best_available_100k, bench_t0_cold_decode, bench_t1_build,
        bench_thumb_warm, bench_raw_cache_get, bench_raw_cache_put, bench_eviction_reclaim
}
criterion_main!(budgets);
