// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E03 Phase E (T17/T18) acceptance criteria.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightbox_catalog::Catalog;

use super::*;
use crate::config::PreviewStoreConfig;

/// A near-incompressible synthetic payload, real camera-raw planar data has
/// high entropy, and a trivially-compressible buffer (all zeros) would both
/// under-measure `put`/`get` latency and let a corrupted byte slip past
/// undetected far more easily than real data would. Deterministic per
/// `seed` so tests are reproducible.
fn synthetic_payload(len: usize, seed: u64) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    fastrand::Rng::with_seed(seed).fill(&mut buf);
    buf
}

fn sample_meta() -> RawStageMeta {
    RawStageMeta {
        payload_schema: 7,
        width: 64,
        height: 48,
        channels: 3,
        sample: SampleFormat::F16,
        color_state: 2,
    }
}

/// A fresh store+catalog pair, wired the same way `service.rs`'s own test
/// harness wires them (catalog under `<tempdir>/t.lbdata`, preview/rawcache
/// store rooted directly at `<tempdir>`, see that module's harness for
/// why: an existing, already-reviewed test convention this phase mirrors
/// rather than reinvents).
fn harness(rawcache_cap_bytes: u64) -> (tempfile::TempDir, RawCache) {
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
    let cfg = PreviewStoreConfig::with_defaults(dir.path().to_path_buf());
    let store = Arc::new(Store::open(&cfg).unwrap());
    let limits = CacheLimits {
        preview_cap_bytes: u64::MAX,
        rawcache_cap_bytes,
    };
    let rc = RawCache::new(store, catalog, limits, 3);
    (dir, rc)
}

// ── T17: container format ------------------------------------------------

#[test]
fn t17_planar_payload_round_trips_bit_exact() {
    let (_dir, rc) = harness(u64::MAX);
    let key = RawCacheKey {
        content_hash: ContentHash([9; 16]),
        params_hash: 0xdead_beef_cafe_1234,
    };
    let meta = sample_meta();
    let payload = synthetic_payload(meta.width as usize * meta.height as usize * 3 * 2, 1);

    rc.put(key, meta, PlaneData(&payload)).unwrap();
    assert!(rc.contains(&key));

    let hit = rc.get(&key).unwrap().expect("expected a hit");
    assert_eq!(hit.meta, meta, "declared geometry must round-trip exactly");
    assert_eq!(
        hit.data.as_bytes(),
        payload.as_slice(),
        "payload bytes must round-trip bit-exact"
    );
}

#[test]
fn t17_absent_key_is_a_plain_miss_no_error() {
    let (_dir, rc) = harness(u64::MAX);
    let key = RawCacheKey {
        content_hash: ContentHash([1; 16]),
        params_hash: 1,
    };
    assert!(rc.get(&key).unwrap().is_none());
    assert!(!rc.contains(&key));
}

#[test]
fn t17_single_byte_corruption_in_the_checksum_trailer_is_a_typed_miss_drops_entry_and_deletes_file()
{
    let (dir, rc) = harness(u64::MAX);
    let key = RawCacheKey {
        content_hash: ContentHash([2; 16]),
        params_hash: 42,
    };
    let meta = sample_meta();
    let payload = synthetic_payload(meta.width as usize * meta.height as usize * 3 * 2, 5);
    rc.put(key, meta, PlaneData(&payload)).unwrap();
    assert!(rc.contains(&key));

    let abs = key.rel_path().to_path_buf(dir.path());
    let mut bytes = std::fs::read(&abs).unwrap();
    // Flip the LAST byte, for a single-frame zstd stream that is inside the
    // trailing content-checksum, so this specifically exercises checksum
    // verification rather than "the compressed block failed to parse at
    // all" (a different, also-handled, failure mode covered by the next
    // test).
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&abs, &bytes).unwrap();

    let miss = rc.get(&key).unwrap();
    assert!(
        miss.is_none(),
        "a corrupted entry must be a typed miss (Ok(None)), never Err or wrong data"
    );
    assert!(!abs.exists(), "the corrupted file must be deleted");
    assert!(
        !rc.contains(&key),
        "the corrupted entry's catalog row must be dropped"
    );
}

#[test]
fn t17_single_byte_corruption_mid_stream_is_a_typed_miss_drops_entry_and_deletes_file() {
    let (dir, rc) = harness(u64::MAX);
    let key = RawCacheKey {
        content_hash: ContentHash([3; 16]),
        params_hash: 7,
    };
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 100,
        height: 80,
        channels: 3,
        sample: SampleFormat::F32,
        color_state: 1,
    };
    let payload = synthetic_payload(100 * 80 * 3 * 4, 9);
    rc.put(key, meta, PlaneData(&payload)).unwrap();

    let abs = key.rel_path().to_path_buf(dir.path());
    let mut bytes = std::fs::read(&abs).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    std::fs::write(&abs, &bytes).unwrap();

    assert!(rc.get(&key).unwrap().is_none());
    assert!(!abs.exists());
    assert!(!rc.contains(&key));
}

#[test]
fn t17_140mb_payload_get_latency() {
    let (_dir, rc) = harness(u64::MAX);
    const LEN: usize = 140 * 1024 * 1024;
    let key = RawCacheKey {
        content_hash: ContentHash([4; 16]),
        params_hash: 0xABCD,
    };
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 6000,
        height: 4000,
        channels: 3,
        sample: SampleFormat::F16,
        color_state: 0,
    };
    let payload = synthetic_payload(LEN, 42);
    rc.put(key, meta, PlaneData(&payload)).unwrap();

    let start = Instant::now();
    let hit = rc.get(&key).unwrap().expect("expected a hit");
    let elapsed = start.elapsed();
    assert_eq!(hit.data.len(), LEN);

    // Always reported, honestly, regardless of build profile (DEVELOPMENT.md
    // "report the actual number").
    println!(
        "raw cache get() of a {:.1} MB payload took {elapsed:?} (budget 200ms, spec §6/T17 AC)",
        LEN as f64 / (1024.0 * 1024.0)
    );
    // Debug-profile `cargo test` codegen is dramatically slower for
    // zstd's hot loops than `--release` on this machine (same reasoning as
    // `codec.rs`'s own `jpeg_encode_meets_the_250ms_latency_budget`
    // release-gated there for the identical reason, see that test's doc
    // comment); this AC is asserted under `--release`, not unconditionally.
    if !cfg!(debug_assertions) {
        assert!(
            elapsed <= Duration::from_millis(200),
            "get() of a 140MB payload took {elapsed:?}, budget is 200ms (spec §6/T17 AC)"
        );
    }
}

// ── T18: accounting/LRU/reconcile ---------------------------------------

#[test]
fn t18_evict_to_cap_enforces_the_cap_within_one_entry_scaled_synthetic() {
    // Spec T18 AC literally reads "filling 6 GiB of synthetic entries
    // enforces the cap [default 5 GiB] within one entry." Writing 6 GiB of
    // real zstd-compressed files in a PR-blocking unit test is impractical
    // (CI time/disk, this is the exact reasoning `preview_dao`'s own 100k-
    // row test avoided by staying in-memory; a real 6 GiB of file IO has no
    // equivalent shortcut). This test exercises the identical PROPERTY
    // fill to 1.25x a configured cap (the same ratio spec's own 6/5 GiB
    // numbers describe), assert the store never drifts more than one
    // entry's size over cap, and settles back at/under cap once eviction
    // catches up, using an explicit small `CacheLimits::rawcache_cap_bytes`
    // instead of the 5 GiB default. Recorded in
    // docs/plan/epics/E03-deviations.md, Phase E.
    const ENTRY_BYTES: usize = 64 * 1024;
    const CAP: u64 = 512 * 1024; // 8 entries' worth
    const ENTRIES_TO_WRITE: usize = 10; // 1.25x the cap

    let (_dir, rc) = harness(CAP);
    for i in 0..ENTRIES_TO_WRITE {
        let key = RawCacheKey {
            content_hash: ContentHash([i as u8; 16]),
            params_hash: i as u64,
        };
        let meta = RawStageMeta {
            payload_schema: 1,
            width: 1,
            height: 1,
            channels: 1,
            sample: SampleFormat::U16,
            color_state: 0,
        };
        let payload = synthetic_payload(ENTRY_BYTES, i as u64);
        rc.put(key, meta, PlaneData(&payload)).unwrap();

        let total = rc.total_bytes();
        assert!(
            total <= CAP + ENTRY_BYTES as u64,
            "after entry {i}: total {total} exceeds cap {CAP} by more than one entry \
             ({ENTRY_BYTES}) — cap must be enforced within one entry (spec T18 AC)"
        );
    }

    let final_total = rc.total_bytes();
    assert!(
        final_total <= CAP,
        "cap must be enforced once writes settle: {final_total} > {CAP}"
    );
    assert!(
        rc.entry_count() < ENTRIES_TO_WRITE as u64,
        "some entries must actually have been evicted"
    );
}

#[test]
fn t18_reconcile_repairs_both_divergence_directions() {
    let (dir, rc) = harness(u64::MAX);
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 2,
        height: 2,
        channels: 1,
        sample: SampleFormat::U16,
        color_state: 0,
    };

    // Case A: a dangling row, catalog row exists, file does not (e.g. the
    // file half of a crash between write and… nothing, here just simulated
    // directly).
    let key_a = RawCacheKey {
        content_hash: ContentHash([10; 16]),
        params_hash: 1,
    };
    rc.put(key_a, meta, PlaneData(&synthetic_payload(16, 1)))
        .unwrap();
    let abs_a = key_a.rel_path().to_path_buf(dir.path());
    std::fs::remove_file(&abs_a).unwrap();
    assert!(rc.contains(&key_a), "row still present before reconcile");

    // Case B: an orphan file, file exists, catalog row does not (the
    // inverse: a crash between the file write and the catalog upsert,
    // simulated by dropping just the row after a normal put()).
    let key_b = RawCacheKey {
        content_hash: ContentHash([11; 16]),
        params_hash: 2,
    };
    rc.put(key_b, meta, PlaneData(&synthetic_payload(16, 2)))
        .unwrap();
    rc.catalog
        .writer()
        .with_txn(move |txn| {
            txn.delete_rawcache_entry_by_key(key_b.content_hash, key_b.params_hash_be())
        })
        .unwrap();
    assert!(!rc.contains(&key_b), "row removed before reconcile");
    let abs_b = key_b.rel_path().to_path_buf(dir.path());
    assert!(abs_b.exists(), "file still present before reconcile");

    // Case C: an unrecognized file (the shape a leftover `.tmp-*` from a
    // killed `atomic_write` would have, no `.zst` suffix), never
    // adoptable, must simply be removed.
    let junk_dir = dir.path().join("rawcache").join("zz");
    std::fs::create_dir_all(&junk_dir).unwrap();
    let junk_path = junk_dir.join(".tmp-12345-0");
    std::fs::write(&junk_path, b"leftover of a killed write").unwrap();

    let report = rc.reconcile();
    assert_eq!(report.dangling_rows_dropped, 1, "{report:?}");
    assert_eq!(report.orphans_adopted, 1, "{report:?}");
    assert_eq!(report.orphans_removed, 0, "{report:?}");
    assert_eq!(report.unrecognized_files_removed, 1, "{report:?}");

    assert!(!rc.contains(&key_a), "the dangling row must be dropped");
    assert!(rc.contains(&key_b), "the orphan file must be re-adopted");
    assert!(!junk_path.exists());

    // The re-adopted entry is fully usable, not just index-present.
    let hit = rc
        .get(&key_b)
        .unwrap()
        .expect("re-adopted entry must serve a real hit");
    assert_eq!(hit.data.as_bytes(), synthetic_payload(16, 2).as_slice());

    // A clean second pass finds nothing left to repair.
    let clean = rc.reconcile();
    assert_eq!(clean, ReconcileReport::default(), "{clean:?}");
}

#[test]
fn t18_reconcile_removes_an_orphan_that_fails_to_decode() {
    let (dir, rc) = harness(u64::MAX);
    let rawcache_root = dir.path().join("rawcache").join("ab");
    std::fs::create_dir_all(&rawcache_root).unwrap();
    // A syntactically valid filename, but garbage content, not a zstd
    // frame at all, so `decode_hit` fails during `get_frame_content_size`.
    let key = RawCacheKey {
        content_hash: ContentHash([0xAB; 16]),
        params_hash: 0x1122_3344_5566_7788,
    };
    let path = key.rel_path().to_path_buf(dir.path());
    std::fs::write(&path, b"not a zstd frame at all").unwrap();

    let report = rc.reconcile();
    assert_eq!(report.orphans_removed, 1, "{report:?}");
    assert_eq!(report.orphans_adopted, 0, "{report:?}");
    assert!(!path.exists());
    assert!(!rc.contains(&key));
}

#[test]
fn t18_concurrent_get_put_evict_is_race_free() {
    use std::thread;

    const CAP: u64 = 256 * 1024;
    const KEYS: usize = 12;
    const ENTRY_BYTES: usize = 32 * 1024;
    const ROUNDS_PER_THREAD: u32 = 40;

    let (_dir, rc) = harness(CAP);
    let rc = Arc::new(rc);
    let meta = RawStageMeta {
        payload_schema: 1,
        width: 8,
        height: 8,
        channels: 1,
        sample: SampleFormat::U16,
        color_state: 0,
    };

    let mut handles = Vec::new();
    for t in 0..4usize {
        let rc = Arc::clone(&rc);
        handles.push(thread::spawn(move || {
            for round in 0..ROUNDS_PER_THREAD {
                let i = (t * 7 + round as usize) % KEYS;
                let key = RawCacheKey {
                    content_hash: ContentHash([i as u8; 16]),
                    params_hash: i as u64,
                };
                match round % 3 {
                    0 => {
                        let payload =
                            synthetic_payload(ENTRY_BYTES, (t as u64) * 1000 + round as u64);
                        // A `put()` failure here would be a real bug (races
                        // are handled internally, never surfaced as Err);
                        // let it panic the thread/test if that regresses.
                        rc.put(key, meta, PlaneData(&payload)).unwrap();
                    }
                    1 => {
                        // A hit or a clean miss are both fine outcomes under
                        // concurrent eviction, `get()` itself already
                        // panics on nothing and never returns `Err` for a
                        // corruption/eviction race (see its doc comment).
                        let _ = rc.get(&key).unwrap();
                    }
                    _ => {
                        rc.evict_to_cap();
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join()
            .expect("no worker thread must panic under concurrent load");
    }

    // Single-threaded mop-up: a call here can never lose a race (nothing
    // else is running), so this is the authoritative "did we converge"
    // check regardless of how many races individual concurrent calls above
    // may have bailed out of (see `evict_to_cap`'s bounded-retry doc
    // comment).
    rc.evict_to_cap();
    let final_total = rc.total_bytes();
    assert!(
        final_total <= CAP,
        "final total {final_total} exceeds cap {CAP} after a race-free mop-up pass"
    );

    // Every surviving row must point at a real, checksummable file, i.e.
    // reconcile() finds nothing left to repair. `unrecognized_files_removed`
    // is asserted strictly (this test never writes a non-`.zst` file); the
    // dangling/orphan counts are asserted as small rather than exactly zero
    // see `delete_rawcache_entry_if_unchanged`'s doc comment for the one
    // documented, narrow, self-healing same-wall-clock-second race this
    // store does not fully close, which a tight concurrent stress loop like
    // this one is realistically likely to graze.
    let report = rc.reconcile();
    assert_eq!(report.unrecognized_files_removed, 0, "{report:?}");
    assert!(
        report.dangling_rows_dropped + report.orphans_adopted + report.orphans_removed
            <= KEYS as u64,
        "reconcile found implausibly large divergence after the stress run: {report:?}"
    );

    // And after reconcile, the store is fully self-consistent: every
    // remaining row's file exists and decodes. (`rc.catalog` is reachable
    // here because this `tests` module is a descendant of `rawcache`,
    // the same idiom `store.rs`'s own tests use for its private helpers.)
    for row in rc.catalog.reader().all_rawcache_rows().unwrap() {
        let key = RawCacheKey {
            content_hash: row.content_hash,
            params_hash: u64::from_be_bytes(row.params_hash),
        };
        assert!(
            rc.get(&key).unwrap().is_some(),
            "row {row:?} survived reconcile but its file is not a valid, gettable entry"
        );
    }
}

// ── T21: ENOSPC pre-flight -----------------------------------------------

/// A deterministic, injectable [`DiskSpaceProbe`], the mechanism that makes
/// "simulated ENOSPC during build" a real, reproducible test rather than a
/// root-privileged quota hack.
struct FakeDiskSpaceProbe {
    available: std::sync::atomic::AtomicU64,
}

impl FakeDiskSpaceProbe {
    fn new(available: u64) -> Arc<FakeDiskSpaceProbe> {
        Arc::new(FakeDiskSpaceProbe {
            available: std::sync::atomic::AtomicU64::new(available),
        })
    }
}

impl DiskSpaceProbe for FakeDiskSpaceProbe {
    fn available_bytes(&self, _path: &Path) -> std::io::Result<u64> {
        Ok(self.available.load(std::sync::atomic::Ordering::SeqCst))
    }
}

fn harness_with_probe(
    rawcache_cap_bytes: u64,
    probe: Arc<dyn DiskSpaceProbe>,
    pressure: Option<EventSink>,
) -> (tempfile::TempDir, RawCache) {
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
    let cfg = PreviewStoreConfig::with_defaults(dir.path().to_path_buf());
    let store = Arc::new(Store::open(&cfg).unwrap());
    let limits = CacheLimits {
        preview_cap_bytes: u64::MAX,
        rawcache_cap_bytes,
    };
    let rc = RawCache::with_probe(store, catalog, limits, 3, probe, pressure);
    (dir, rc)
}

/// T21 AC: simulated ENOSPC during a build emits a `CachePressure` event and
/// a typed [`RawCacheError::DiskFull`] failure, never a panic, never a
/// torn write (the file must not exist afterward).
#[test]
fn t21_simulated_enospc_emits_pressure_and_a_typed_failure_no_panic() {
    let probe = FakeDiskSpaceProbe::new(1024); // far less than the payload below
    let events_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev2 = Arc::clone(&events_seen);
    let sink: EventSink = Arc::new(move |ev| ev2.lock().unwrap().push(ev));
    let (_dir, rc) = harness_with_probe(u64::MAX, probe, Some(sink));

    let key = RawCacheKey {
        content_hash: ContentHash([77; 16]),
        params_hash: 1,
    };
    let meta = sample_meta();
    let payload = synthetic_payload(1_000_000, 2); // 1 MB >> the fake 1 KiB available

    let err = rc.put(key, meta, PlaneData(&payload)).unwrap_err();
    assert!(matches!(err, RawCacheError::DiskFull { .. }), "{err:?}");
    assert!(
        !rc.contains(&key),
        "a failed pre-flight must never write anything"
    );
    assert!(
        !rc.store.resolve(&key.rel_path()).exists(),
        "no torn/partial file must be left behind"
    );

    let events = events_seen.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            PreviewEvent::CachePressure {
                kind: CacheKind::RawCache,
                ..
            }
        )),
        "expected at least one CachePressure event, got {events:?}"
    );
}

/// T21 AC (the non-failure half): when the probe reports JUST enough room,
/// `put` succeeds normally, the pre-flight is not a blanket refusal, only a
/// genuine-shortage guard.
#[test]
fn t21_enospc_preflight_does_not_block_a_write_that_fits() {
    let probe = FakeDiskSpaceProbe::new(100 * 1024 * 1024); // plenty (well over the 16 MiB margin)
    let (_dir, rc) = harness_with_probe(u64::MAX, probe, None);
    let key = RawCacheKey {
        content_hash: ContentHash([78; 16]),
        params_hash: 1,
    };
    let meta = sample_meta();
    let payload = synthetic_payload(1024, 3);
    rc.put(key, meta, PlaneData(&payload)).unwrap();
    assert!(rc.contains(&key));
}

/// T21 AC: when eviction alone reclaims enough room, the pre-flight retries
/// and the write proceeds (never fails just because the FIRST check was
/// tight), a rising probe reading simulates "eviction actually freed
/// space" without needing real disk I/O to observe it.
#[test]
fn t21_enospc_preflight_recovers_after_eviction_frees_room() {
    // A probe that reports "0 available" on the FIRST check (triggering
    // evict_to_cap) and "plenty" on the re-check, modeling "the eviction
    // pass itself is what freed room" deterministically, without needing a
    // real evict to actually reclaim anything from an otherwise-empty cache.
    struct RisingProbe(std::sync::atomic::AtomicU32);
    impl DiskSpaceProbe for RisingProbe {
        fn available_bytes(&self, _path: &Path) -> std::io::Result<u64> {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(if n == 0 { 0 } else { 100 * 1024 * 1024 })
        }
    }
    let rising: Arc<dyn DiskSpaceProbe> =
        Arc::new(RisingProbe(std::sync::atomic::AtomicU32::new(0)));
    let (_dir, rc) = harness_with_probe(u64::MAX, rising, None);

    let key = RawCacheKey {
        content_hash: ContentHash([79; 16]),
        params_hash: 1,
    };
    let payload = synthetic_payload(1024, 4);
    rc.put(key, sample_meta(), PlaneData(&payload)).unwrap();
    assert!(rc.contains(&key));
}
