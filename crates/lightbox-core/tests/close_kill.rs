// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T15 AC: `kill -9` *during* the close-time backup leaves the previous
//! backup set intact and the catalog clean (atomic temp+rename inherited
//! from T12).
//!
//! Pattern follows `lightbox-catalog/tests/fault_injection.rs`: the parent
//! re-invokes this test binary filtered to [`close_child`] with an env var
//! set; each child opens a session and immediately runs
//! `close(ClosePolicy::Always)`; the parent SIGKILLs it at a random moment
//! around the backup window, then verifies:
//!
//! 1. the catalog reopens clean (`quick_check` runs at open);
//! 2. **every** promoted backup (`…/catalog.sqlite.zst`) still decompresses
//!    to an `integrity_check`-clean catalog — a kill can only lose the
//!    in-flight backup, never corrupt a promoted one;
//! 3. the pre-existing baseline backup is still among them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lightbox_core::{ClosePolicy, Core, CoreConfig};

const CHILD_DIR_ENV: &str = "LIGHTBOX_CLOSE_KILL_CHILD_DIR";
const ITERATIONS: u32 = 6;

/// Child-mode entry point: open → close(Always). A no-op under normal
/// `cargo test`.
#[test]
fn close_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Err(err) = child_body(&dir) {
        let _ = std::fs::write(dir.join("child-error.txt"), format!("{err:?}"));
        panic!("close child failed: {err:?}");
    }
}

fn child_body(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(&dir.join("cat.lbdata"), None)?;
    // Tell the parent the backup window is about to open.
    std::fs::write(dir.join("child-ready.txt"), "ready")?;
    session.close(ClosePolicy::Always.into())?;
    Ok(())
}

#[test]
fn kill9_during_close_backup_leaves_backup_set_intact() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child; only close_child runs there
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let lbdata = dir.join("cat.lbdata");

    // Stage: a catalog with enough rows that the backup window is wide
    // enough to hit (seeded directly through the catalog crate — volume,
    // not semantics), plus one verified baseline backup.
    seed_catalog(&lbdata, 60_000);
    let core = Core::start(CoreConfig::default()).expect("core");
    let session = core.open_catalog(&lbdata, None).expect("open");
    let close_started = std::time::Instant::now();
    let baseline = session
        .close(ClosePolicy::Always.into())
        .expect("baseline close")
        .backup
        .expect("baseline backup ran");
    // How long a full close-with-backup takes on THIS machine — the kill
    // delay is sampled inside that window so kills land mid-backup.
    let window = close_started.elapsed();
    assert!(baseline.path.is_file());

    let exe = std::env::current_exe().expect("test binary");
    let mut rng = fastrand::Rng::with_seed(0x11ce_b0c5_0f00_ba52);
    let mut killed_mid_window = 0u32;

    for iteration in 0..ITERATIONS {
        let _ = std::fs::remove_file(dir.join("child-ready.txt"));
        let mut child = std::process::Command::new(&exe)
            .args(["close_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn close child");

        // Wait for the child to reach the close call…
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !dir.join("child-ready.txt").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "iteration {iteration}: child never became ready"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
        // …then kill somewhere inside the measured backup window.
        let delay = rng.u64(0..window.as_micros().max(1000) as u64);
        std::thread::sleep(Duration::from_micros(delay));
        match child.try_wait().expect("try_wait") {
            Some(_) => {} // finished before the kill landed — still counts
            None => {
                child.kill().expect("SIGKILL child");
                killed_mid_window += 1;
            }
        }
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // 1. Catalog reopens clean (open runs quick_check).
        let catalog = lightbox_catalog::Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        assert_eq!(
            catalog.integrity(),
            lightbox_catalog::IntegrityStatus::Ok,
            "iteration {iteration}"
        );
        drop(catalog);

        // 2. Every promoted backup restores clean.
        let backups = promoted_backups(&lbdata.join("backups"));
        assert!(
            backups.contains(&baseline.path),
            "iteration {iteration}: baseline backup vanished"
        );
        for (i, zst) in backups.iter().enumerate() {
            verify_backup_restores(zst, &dir.join(format!("restore-{iteration}-{i}")));
        }
    }

    // The harness only proves something if kills actually landed while the
    // children were alive (i.e. plausibly mid-close/mid-backup).
    assert!(
        killed_mid_window >= 1,
        "no kill landed inside the backup window (window {window:?}); \
         widen the seed or the window sampling"
    );
    eprintln!(
        "close-kill: {ITERATIONS} children, {killed_mid_window} killed in the window \
         (window {window:?}), backup set verified intact each time"
    );
}

/// Directly seeds a catalog with `n` synthetic assets (fast; no files).
fn seed_catalog(lbdata: &Path, n: usize) {
    use lightbox_types::{ContentHash, Orientation};

    let catalog = lightbox_catalog::Catalog::create(lbdata).expect("create");
    let root_path = lbdata.parent().unwrap().join("photos");
    let folder = catalog
        .writer()
        .with_txn(move |txn| {
            let root = txn.upsert_root(None, &root_path)?;
            txn.upsert_folder(root, None, "seed")
        })
        .expect("seed folder");

    let per_txn = 5000;
    let mut next = 0u64;
    while (next as usize) < n {
        let batch: Vec<lightbox_catalog::NewAsset> = (0..per_txn)
            .map(|_| {
                next += 1;
                let mut hash = [0u8; 16];
                hash[..8].copy_from_slice(&next.to_le_bytes());
                lightbox_catalog::NewAsset {
                    folder,
                    filename: format!("SEED_{next:07}.jpg"),
                    content_hash: ContentHash(hash),
                    format: "JPEG".to_owned(),
                    camera_make: Some("Lightbox".to_owned()),
                    camera_model: Some("Synthetic".to_owned()),
                    capture_time: None,
                    width: 6000,
                    height: 4000,
                    orientation: Orientation::O1,
                    bytes: 1234,
                    mtime_utc: None,
                    decode_error: None,
                    import_session: None,
                }
            })
            .collect();
        catalog
            .writer()
            .with_txn(move |txn| {
                let outcome = txn.insert_assets(&batch)?;
                txn.insert_default_images(&outcome.inserted)?;
                Ok(())
            })
            .expect("seed batch");
    }
}

/// All promoted (verified + renamed-in) backup files, any order.
///
/// A SIGKILL mid-backup can leave a `.tmp-*` scratch directory behind with a
/// half-written `.zst` inside — that file was never verified or renamed in,
/// so it is *not* part of the backup set (the catalog's own newest/prune
/// logic ignores non-dated names the same way).
fn promoted_backups(backups_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(backups_dir)
        .expect("backups dir")
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue; // in-progress scratch (.tmp-*), never promoted
        }
        let zst = entry.path().join("catalog.sqlite.zst");
        if zst.is_file() {
            out.push(zst);
        }
    }
    out
}

/// unzstd → open → full integrity_check (the §6 restore drill).
fn verify_backup_restores(zst: &Path, scratch: &Path) {
    std::fs::create_dir_all(scratch).unwrap();
    let restored = scratch.join("catalog.sqlite");
    let src = std::fs::File::open(zst).expect("open zst");
    let mut dst = std::fs::File::create(&restored).expect("create restored file");
    zstd::stream::copy_decode(src, &mut dst)
        .unwrap_or_else(|e| panic!("{} does not decompress: {e}", zst.display()));
    dst.flush().unwrap();
    drop(dst);
    let findings = lightbox_catalog::integrity_check_file(&restored)
        .unwrap_or_else(|e| panic!("{} does not open: {e}", zst.display()));
    assert!(
        findings.is_empty(),
        "{}: integrity_check findings on restored backup: {findings:?}",
        zst.display()
    );
    let _ = std::fs::remove_dir_all(scratch);
}
