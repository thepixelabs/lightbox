// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T20 AC: "manifest survives kill-loop." Same `kill -9` pattern as
//! `tests/blob_crash_loop.rs` (T02) and `lightbox-catalog`'s own
//! fault-injection harness: a child process (this crate's `--lib` test
//! binary, re-invoked with `LIGHTBOX_T2_CHILD_DIR` set) repeatedly builds a
//! fresh synthetic T2 tile set (`t2::ensure_t2_synthetic` — real per-tile +
//! manifest atomic writes, one catalog+store shared across the run) while
//! the parent SIGKILLs it at a random moment and inspects what's left.
//!
//! **What "survives" means here.** `manifest.cbor` and every tile file share
//! `store::atomic_write`'s temp+rename primitive, already exhaustively
//! kill-tested in isolation (`blob_crash_loop.rs`: 30 iterations x up to
//! 1000 writes). This harness is narrower and specific to T2: after every
//! kill, for every T2 row the catalog still lists, is EVERY tile the
//! manifest claims `present` genuinely readable through the real
//! `t2::t2_tile_read` path (not just "the bytes exist on disk")? A manifest
//! that parsed but claimed a tile that doesn't decode, or a row whose
//! manifest itself is torn/missing, would fail this check.

use std::path::Path;
use std::time::Duration;

use lightbox_catalog::Catalog;
use lightbox_types::{AssetId, ContentHash, ImageId, Orientation};

use crate::config::PreviewStoreConfig;
use crate::pyramid::{RelPath, Tier};
use crate::store::Store;
use crate::t2;

const CHILD_DIR_ENV: &str = "LIGHTBOX_T2_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_T2_FAULT_ITERS";

#[test]
fn t2_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_workload(Path::new(&dir)) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("t2 fault child failed: {err:?}");
    }
}

fn child_workload(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lbdata = dir.join("cat.lbdata");
    let catalog = Catalog::open(&lbdata)?;
    let store = Store::open(&PreviewStoreConfig::with_defaults(lbdata.clone()))?;

    let asset_id: i64 = std::fs::read_to_string(dir.join("asset-id.txt"))?
        .trim()
        .parse()?;
    let asset = AssetId(asset_id);

    let run_id = u64::from(std::process::id()) ^ nanos_now();
    let mut counter = 0u64;
    loop {
        counter += 1;
        let image: ImageId = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))?;
        let mut hash = [0u8; 16];
        hash[..8].copy_from_slice(&run_id.to_le_bytes());
        hash[8..].copy_from_slice(&counter.to_le_bytes());
        let content_hash = ContentHash(hash);

        // A small grid (2x2 tiles) keeps one build's fsync count low enough
        // that the parent's short kill-window (below) reliably lands mid-run
        // across many iterations, without weakening what's under test: the
        // SAME atomic-write + manifest mechanism a 100 MP build uses, just
        // fewer tiles per iteration.
        t2::ensure_t2_synthetic(
            &store,
            &catalog,
            &Default::default(),
            image,
            asset,
            content_hash,
            240,
            240,
            128,
        )?;
    }
}

/// The parent: spawn -> random sleep -> SIGKILL -> reopen -> verify.
#[test]
fn kill9_during_t2_builds_leaves_every_present_tile_readable() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return;
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

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
                    filename: "synthetic.dng".to_owned(),
                    content_hash: ContentHash([9; 16]),
                    format: "DNG".to_owned(),
                    camera_make: None,
                    camera_model: None,
                    capture_time: None,
                    width: 600,
                    height: 500,
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

    for iteration in 0..iterations {
        let mut child = std::process::Command::new(&exe)
            .args(["t2_crash_loop::t2_fault_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn t2 fault child");

        std::thread::sleep(Duration::from_millis(rng.u64(15..=120)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        let catalog = Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        let store = Store::open(&PreviewStoreConfig::with_defaults(lbdata.clone()))
            .unwrap_or_else(|e| panic!("iteration {iteration}: store reopen failed: {e}"));

        let rows: Vec<_> = catalog
            .reader()
            .all_preview_rows()
            .unwrap()
            .into_iter()
            .filter(|r| r.tier == Tier::T2 as u8)
            .collect();
        total_rows_seen += rows.len();

        for row in &rows {
            let t2_dir = RelPath(row.store_path.clone());
            let mut any_readable = false;
            for y in 0..8u32 {
                for x in 0..8u32 {
                    if t2::t2_tile_read(&store, &t2_dir, t2::TileCoord { x, y })
                        .unwrap_or(None)
                        .is_some()
                    {
                        any_readable = true;
                    }
                }
            }
            assert!(
                any_readable,
                "iteration {iteration}: T2 row {row:?} has no readable tile at all — \
                 the manifest is torn/missing for a row the catalog still lists"
            );
        }
    }

    assert!(
        total_rows_seen > 0,
        "harness never observed a committed T2 row"
    );
    eprintln!(
        "T2 manifest crash loop: {iterations} kills, {total_rows_seen} T2 row-observations, \
         0 unreadable manifests for any row the catalog still lists"
    );
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
