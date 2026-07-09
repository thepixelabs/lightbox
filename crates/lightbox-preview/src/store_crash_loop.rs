// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E03 Phase F (T21/T23) AC: "kill -9 loop across all write phases →
//! catalog `integrity_check` clean + `verify_store` clean." Real `SIGKILL`
//! fault injection against the actual T0/T1 preview-store write path
//! (`producer::ensure_t0`/`ensure_t1`) AND the raw-cache container write
//! path (`RawCache::put`), interleaved in one run — same child/parent
//! pattern as `t2_crash_loop.rs` (T20) and `tests/blob_crash_loop.rs` (T02),
//! but exercising the literal T21 AC surface after every kill: the real
//! public `verify_store(VerifyMode::Full)` seam (not a hand-rolled scan),
//! plus `RawCache::reconcile`'s equivalent "always reconcilable" bar for the
//! raw cache (spec §8 test-plan row: "the store's bar is 'always
//! reconcilable'", not "never diverges").
//!
//! **Why this can assert something stronger than "never panics."**
//! `producer::ensure_t0`/`ensure_t1` and `RawCache::put` all share the
//! identical ordering discipline: the content-addressed file is written via
//! `store::atomic_write` (temp-write + fsync + rename, same-directory) and
//! only THEN does the catalog row get committed (see each function's own
//! source — `atomic_write(...)` always precedes `catalog.writer().with_txn`
//! for the row). A kill can therefore only ever land in one of three places:
//! (a) before any trace exists, (b) after the file lands but before the
//! catalog commit — a harmless orphaned `.tmp-*` or an orphaned *final-named*
//! file with no row yet (both swept by `verify_store(Full)`/`reconcile`), or
//! (c) after both complete. It can **never** produce a catalog row pointing
//! at a missing or torn file. This harness asserts exactly that: zero
//! `missing_files`/`checksum_failures` from `verify_store(Full)` and zero
//! `dangling_rows_dropped` from `RawCache::reconcile`, after every single
//! kill, across a real interleaving of T0/T1/raw-cache writes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightbox_catalog::{Catalog, IntegrityStatus};
use lightbox_types::{AssetId, ContentHash, ImageId, Orientation};

use crate::config::{CacheLimits, Codec, PreviewStoreConfig, StandardSize};
use crate::index::PreviewIndex;
use crate::producer;
use crate::rawcache::{PlaneData, RawCache, RawCacheKey, RawStageMeta, SampleFormat};
use crate::store::Store;
use crate::verify::{self, VerifyMode};

const CHILD_DIR_ENV: &str = "LIGHTBOX_STORE_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_STORE_FAULT_ITERS";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn store_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_workload(Path::new(&dir)) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("store fault child failed: {err:?}");
    }
}

fn child_workload(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lbdata = dir.join("cat.lbdata");
    let catalog = Arc::new(Catalog::open(&lbdata)?);
    let store = Arc::new(Store::open(&PreviewStoreConfig::with_defaults(
        lbdata.clone(),
    ))?);
    let rc = RawCache::new(
        Arc::clone(&store),
        Arc::clone(&catalog),
        CacheLimits::default(),
        3,
    );
    let index: Mutex<PreviewIndex> = Mutex::new(PreviewIndex::empty());

    let asset_id: i64 = std::fs::read_to_string(dir.join("asset-id.txt"))?
        .trim()
        .parse()?;
    let asset = AssetId(asset_id);
    let fixture_path = fixtures_dir().join("canon-eos-350d.cr2");

    let run_id = u64::from(std::process::id()) ^ nanos_now();
    let mut counter = 0u64;
    let mut rng = fastrand::Rng::with_seed(run_id);

    loop {
        counter += 1;
        let image: ImageId = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))?;
        let mut hash = [0u8; 16];
        hash[..8].copy_from_slice(&run_id.to_le_bytes());
        hash[8..].copy_from_slice(&counter.to_le_bytes());
        let content_hash = ContentHash(hash);

        // Biased toward T0/raw-cache (fast: no resize+encode) with a modest
        // T1 share (the resize+encode path is genuinely slow in an
        // unoptimized debug build — C-2's own documented finding — so a
        // heavier T1 share would mostly just burn wall-clock inside "before
        // any trace exists" without adding coverage of the interesting
        // post-write/pre-commit boundary this harness targets).
        let roll = rng.u8(..);
        if roll < 150 {
            // T0 write phase.
            producer::ensure_t0(
                &store,
                &catalog,
                &index,
                image,
                asset,
                content_hash,
                &fixture_path,
            )?;
        } else if roll < 180 {
            // T1 write phase (drives T0 too when missing, real resize+encode).
            producer::ensure_t1(
                &store,
                &catalog,
                &index,
                image,
                asset,
                content_hash,
                &fixture_path,
                StandardSize::Fixed(320),
                Codec::Jpeg,
                85,
            )?;
        } else {
            // Raw-cache container write phase: small synthetic near-
            // incompressible planar payload, distinct key every call.
            let key = RawCacheKey {
                content_hash,
                params_hash: run_id ^ counter,
            };
            let meta = RawStageMeta {
                payload_schema: 1,
                width: 32,
                height: 24,
                channels: 3,
                sample: SampleFormat::F16,
                color_state: 0,
            };
            let mut payload =
                vec![0u8; meta.width as usize * meta.height as usize * meta.channels as usize * 2];
            rng.fill(&mut payload);
            rc.put(key, meta, PlaneData(&payload))?;
        }
    }
}

/// The parent: spawn -> random sleep -> `SIGKILL` -> reopen -> verify.
#[test]
fn kill9_across_preview_and_rawcache_writes_leaves_the_store_verify_clean() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only store_fault_child runs there
    }
    if !fixtures_dir().join("manifest.toml").exists() {
        eprintln!("skipping: run `cargo xtask fixtures` first");
        return;
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let lbdata = dir.join("cat.lbdata");

    let asset_id = {
        let catalog = Catalog::create(&lbdata).expect("create");
        let root_path = dir.join("photos");
        let asset = catalog
            .writer()
            .with_txn(move |txn| {
                let root = txn.upsert_root(None, &root_path)?;
                let folder = txn.upsert_folder(root, None, "fault")?;
                let batch = vec![lightbox_catalog::NewAsset {
                    folder,
                    filename: "canon-eos-350d.cr2".to_owned(),
                    content_hash: ContentHash([7; 16]),
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
                Ok(txn.insert_assets(&batch)?.inserted[0])
            })
            .expect("seed asset");
        asset.0
    };
    std::fs::write(dir.join("asset-id.txt"), asset_id.to_string()).unwrap();

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());
    let mut total_rows_seen = 0usize;
    let mut total_orphans_swept = 0u64;
    let mut total_temp_swept = 0u64;

    for iteration in 0..iterations {
        let mut child = std::process::Command::new(&exe)
            .args([
                "store_crash_loop::store_fault_child",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn store fault child");

        std::thread::sleep(Duration::from_millis(rng.u64(20..=150)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        let catalog = Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        assert_eq!(
            catalog.integrity(),
            IntegrityStatus::Ok,
            "iteration {iteration}: quick_check found corruption"
        );

        let store = Store::open(&PreviewStoreConfig::with_defaults(lbdata.clone()))
            .unwrap_or_else(|e| panic!("iteration {iteration}: store reopen failed: {e}"));
        let index = Mutex::new(
            PreviewIndex::load(&catalog.reader()).unwrap_or_else(|_| PreviewIndex::empty()),
        );
        let report = verify::verify_store(&store, &catalog, &index, VerifyMode::Full);
        assert_eq!(
            report.missing_files, 0,
            "iteration {iteration}: a catalog row pointed at a missing preview file — {report:?}"
        );
        assert_eq!(
            report.checksum_failures, 0,
            "iteration {iteration}: a catalog row pointed at a torn/corrupt preview file — {report:?}"
        );
        total_rows_seen += report.rows_checked as usize;
        total_orphans_swept += report.orphans_removed;
        total_temp_swept += report.orphan_temp_files_removed;

        // The raw cache's own "always reconcilable" bar (§8 test-plan): a
        // dangling row (row present, file missing/torn-away) would mean the
        // file-before-row ordering was violated — must never happen.
        let catalog = Arc::new(catalog);
        let store = Arc::new(store);
        let rc = RawCache::new(
            Arc::clone(&store),
            Arc::clone(&catalog),
            CacheLimits::default(),
            3,
        );
        let rc_report = rc.reconcile();
        assert_eq!(
            rc_report.dangling_rows_dropped, 0,
            "iteration {iteration}: a raw-cache catalog row pointed at a missing file — {rc_report:?}"
        );
    }

    assert!(
        total_rows_seen > 0,
        "harness never observed a committed preview row"
    );
    eprintln!(
        "store crash loop: {iterations} kills across interleaved T0/T1/raw-cache writes, \
         {total_rows_seen} preview row-observations, 0 missing/torn files ever indexed, \
         {total_orphans_swept} orphan file(s) swept, {total_temp_swept} orphaned temp file(s) \
         swept (both harmless, expected residue of a kill mid-write — see the module doc comment)"
    );
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
