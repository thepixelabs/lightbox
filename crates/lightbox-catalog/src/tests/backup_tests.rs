// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! T12 acceptance criteria: verified backup under writer load (restore →
//! `integrity_check` clean, consistent snapshot), retention pruning, and
//! abort-on-failed-verification atomicity (injected failure).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{new_asset, seed_assets, seed_folder, temp_catalog};
use crate::{BackupOpts, Catalog, CatalogError};

/// Decompresses a backup `.zst` into a fresh `.lbdata` layout and opens it.
fn restore(zst: &std::path::Path, scratch: &std::path::Path) -> Catalog {
    let lbdata = scratch.join("restored.lbdata");
    std::fs::create_dir_all(&lbdata).unwrap();
    let src = std::fs::File::open(zst).unwrap();
    let mut dst = std::fs::File::create(lbdata.join("catalog.sqlite")).unwrap();
    zstd::stream::copy_decode(src, &mut dst).unwrap();
    dst.flush().unwrap();
    drop(dst);
    // Full integrity_check on the restored file (spec §6 restore drill).
    let findings = crate::integrity_check_file(&lbdata.join("catalog.sqlite")).unwrap();
    assert!(
        findings.is_empty(),
        "restored backup not clean: {findings:?}"
    );
    Catalog::open(&lbdata).unwrap()
}

#[test]
fn backup_report_and_restore_round_trip() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "b", 25, 0, &[]);

    let report = catalog.backup_verified(&BackupOpts::default()).unwrap();
    assert!(report.path.is_file());
    assert!(report.bytes > 0);
    assert_eq!(report.bytes, std::fs::metadata(&report.path).unwrap().len());
    assert!(report
        .path
        .starts_with(catalog.lbdata_dir().join("backups")));
    assert_eq!(
        report.path.file_name().unwrap().to_str().unwrap(),
        "catalog.sqlite.zst"
    );

    let restored = restore(&report.path, dir.path());
    assert_eq!(restored.reader().counts().unwrap().assets, 25);
    assert_eq!(
        restored.schema_version(),
        crate::migrate::supported_version(crate::migrate::MIGRATIONS)
    );
}

#[test]
fn backup_under_writer_load_is_consistent() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "pre", 10, 0, &[]);

    // Hammer the writer from another thread while the backup runs.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = catalog.writer();
    let hammer = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut seed = 10_000u64;
            while !stop.load(Ordering::Relaxed) {
                let name = format!("hammer-{seed}.jpg");
                writer
                    .with_txn(move |txn| {
                        let outcome = txn.insert_assets(&[new_asset(folder, &name, seed)])?;
                        txn.insert_default_images(&outcome.inserted)?;
                        Ok(())
                    })
                    .unwrap();
                seed += 1;
            }
        })
    };

    let report = catalog.backup_verified(&BackupOpts::default()).unwrap();
    stop.store(true, Ordering::Relaxed);
    hammer.join().unwrap();

    // The restored snapshot is internally consistent: integrity-clean (in
    // `restore`), every asset has its image, at least the pre-load rows.
    let restored = restore(&report.path, dir.path());
    let counts = restored.reader().counts().unwrap();
    assert!(counts.assets >= 10, "snapshot lost pre-existing rows");
    assert_eq!(
        counts.assets, counts.images,
        "snapshot caught a torn asset/image pair — not a single WAL snapshot"
    );
}

#[test]
fn prune_keeps_newest_n_and_spares_pre_upgrade_copies() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "p", 3, 0, &[]);

    let backups_dir = catalog.lbdata_dir().join("backups");
    // A pre-upgrade safety copy must never be pruned.
    let pre_upgrade = backups_dir.join("pre-upgrade-1");
    std::fs::create_dir_all(&pre_upgrade).unwrap();
    std::fs::write(pre_upgrade.join("catalog.sqlite"), b"not-really").unwrap();

    let opts = BackupOpts {
        retain: 3,
        dest_override: None,
    };
    let mut reports = Vec::new();
    for _ in 0..5 {
        reports.push(catalog.backup_verified(&opts).unwrap());
    }

    let dated: Vec<String> = std::fs::read_dir(&backups_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "pre-upgrade-1")
        .collect();
    assert_eq!(dated.len(), 3, "retention must keep exactly 3: {dated:?}");
    // The three newest survived, i.e. the last three reports' paths exist.
    for r in &reports[2..] {
        assert!(r.path.is_file(), "{} pruned wrongly", r.path.display());
    }
    for r in &reports[..2] {
        assert!(!r.path.exists(), "{} should be pruned", r.path.display());
    }
    assert!(pre_upgrade.join("catalog.sqlite").is_file());
}

#[test]
fn failed_verification_aborts_and_leaves_prior_backups_untouched() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "a", 30, 0, &[]);

    // One good backup first.
    let good = catalog.backup_verified(&BackupOpts::default()).unwrap();
    let backups_dir = catalog.lbdata_dir().join("backups");
    let listing_before = list_sorted(&backups_dir);

    // Injected failure: corrupt the copy after the online backup, before
    // verification (spec §5 T12, "verified with injected failure").
    let err = catalog
        .backup_with_injected_failure(&BackupOpts::default(), &|copy| {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(copy)?;
            // Stomp the SQLite header, deterministic verification failure.
            f.seek(SeekFrom::Start(0))?;
            f.write_all(&[0xFF; 32])?;
            f.sync_all()
        })
        .unwrap_err();
    assert!(
        matches!(&err, CatalogError::BackupFailed(msg) if msg.contains("integrity_check")),
        "{err:?}"
    );

    // Atomicity: nothing new promoted, nothing left behind, the good backup
    // is untouched and still restores.
    assert_eq!(list_sorted(&backups_dir), listing_before);
    let restored = restore(&good.path, dir.path());
    assert_eq!(restored.reader().counts().unwrap().assets, 30);
}

#[test]
fn dest_override_redirects_backups() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "d", 2, 0, &[]);

    let alt = dir.path().join("alt-backups");
    let report = catalog
        .backup_verified(&BackupOpts {
            retain: 10,
            dest_override: Some(alt.clone()),
        })
        .unwrap();
    assert!(report.path.starts_with(&alt));
    assert!(std::fs::read_dir(catalog.lbdata_dir().join("backups"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn newest_backup_time_tracks_verified_backups() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "t", 2, 0, &[]);

    // No backups yet.
    assert_eq!(catalog.newest_backup_time(), None);

    let before = std::time::SystemTime::now();
    catalog.backup_verified(&BackupOpts::default()).unwrap();
    let taken = catalog
        .newest_backup_time()
        .expect("a verified backup exists");
    // Stamp precision is one second; allow that plus a little slack.
    let skew = std::time::Duration::from_secs(2);
    assert!(taken >= before - skew, "backup time in the past: {taken:?}");
    assert!(
        taken <= std::time::SystemTime::now() + skew,
        "backup time in the future: {taken:?}"
    );
}

fn list_sorted(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
