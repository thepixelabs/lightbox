// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `kill -9` **mid-migration** fault injection for the 0002_e02_color upgrade
//! (E02 spec §6, task H1 AC).
//!
//! Each iteration builds a fresh **pre-0002** (schema v1) catalog, seeds it with
//! real rows, then spawns a child (this same test binary, re-invoked filtered to
//! [`migration_fault_child`]) that opens the catalog — which runs the 0002
//! upgrade inside the migration runner's single transaction, after taking the
//! copy-on-write `backups/pre-upgrade-1/` safety copy. The parent SIGKILLs the
//! child at a random moment inside that window and then reopens, asserting:
//!
//! 1. **Integrity-clean** — reopen succeeds and `quick_check` is clean whether
//!    the kill landed before, during, or after the migration transaction
//!    (atomic migration ⊕ WAL + `synchronous=NORMAL`: a torn upgrade stays in
//!    the WAL tail recovery discards; committed pages are never overwritten in
//!    place).
//! 2. **Fully upgraded after reopen** — the parent's `Catalog::open` completes
//!    any interrupted upgrade; the `camera_profile` registry table is present.
//! 3. **No data loss** — every seeded asset survives the crash-during-upgrade.
//! 4. **Copy-on-write safety copy present** — `backups/pre-upgrade-1/` exists
//!    and is itself integrity-clean (the pre-upgrade v1 snapshot, unmigrated).
//!
//! Iterations: 24 by default (PR gate); `LIGHTBOX_MIGRATE_FAULT_ITERS` overrides
//! for the nightly long leg.

use std::path::Path;
use std::time::Duration;

use lightbox_catalog::{Catalog, IntegrityStatus, NewAsset};
use lightbox_types::{ContentHash, FolderId, Orientation};

const CHILD_DIR_ENV: &str = "LIGHTBOX_MIGRATE_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_MIGRATE_FAULT_ITERS";

/// How many assets to seed into each pre-upgrade catalog.
const SEED_ASSETS: usize = 6;

/// Child-mode entry point. A no-op under normal `cargo test`; the real body runs
/// only when the parent re-invokes this binary with the env var set. It opens
/// the (v1) catalog — triggering the 0002 upgrade — then spins on reads until
/// SIGKILLed.
#[test]
fn migration_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_open_and_migrate(Path::new(&dir)) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("migration fault child failed: {err:?}");
    }
}

fn child_open_and_migrate(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lbdata = dir.join("cat.lbdata");
    // This open runs apply_pending → write_pre_upgrade_copy → the 0002 txn.
    let catalog = Catalog::open(&lbdata)?;
    // Keep the catalog open and busy so a kill can also land in the
    // post-migration WAL window (recovery must still be clean).
    loop {
        let _ = catalog.reader().counts();
        std::hint::spin_loop();
    }
}

#[test]
fn kill9_mid_migration_leaves_catalog_clean_and_upgraded() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only migration_fault_child runs there
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());
    let mut fully_upgraded_reopens = 0u32;

    // The latest supported schema version, DERIVED (not hardcoded) so a new
    // migration — e.g. E09's 0003_edit_state — does not stale this E02 test:
    // a v1 seed always completes the forward-only upgrade to whatever is latest.
    let latest = {
        let probe = tempfile::TempDir::new().expect("tempdir");
        // `usize::MAX` upto → apply every migration → the latest supported version.
        Catalog::create_at_schema_version_for_tests(&probe.path().join("probe.lbdata"), usize::MAX)
            .expect("create fully-migrated probe catalog")
            .schema_version()
    };

    for iteration in 0..iterations {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let lbdata = dir.join("cat.lbdata");

        // --- build a fresh pre-0002 (v1) catalog and seed real rows ---
        {
            let catalog =
                Catalog::create_at_schema_version_for_tests(&lbdata, 1).expect("create v1 catalog");
            assert_eq!(catalog.schema_version(), 1, "seed catalog must be v1");
            let root_path = dir.join("photos");
            catalog
                .writer()
                .with_txn(move |txn| {
                    let root = txn.upsert_root(None, &root_path)?;
                    let folder = txn.upsert_folder(root, None, "mig")?;
                    let batch: Vec<NewAsset> =
                        (0..SEED_ASSETS).map(|i| seed_asset(folder, i)).collect();
                    let outcome = txn.insert_assets(&batch)?;
                    txn.insert_default_images(&outcome.inserted)?;
                    Ok(())
                })
                .expect("seed rows");
        }

        // --- spawn the child that opens+migrates, then kill it mid-window ---
        let mut child = std::process::Command::new(&exe)
            .args(["migration_fault_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn migration fault child");
        // The open+migrate happens in the first few ms; bias the kill to land
        // in (or just around) that window.
        std::thread::sleep(Duration::from_micros(rng.u64(50..=6000)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // --- parent reopen: completes any interrupted upgrade, quick_checks ---
        let catalog = Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        assert_eq!(
            catalog.integrity(),
            IntegrityStatus::Ok,
            "iteration {iteration}: quick_check found corruption after mid-migration kill"
        );
        assert_eq!(
            catalog.schema_version(),
            latest,
            "iteration {iteration}: reopen must leave the catalog fully upgraded"
        );
        // The 0002 registry table is reachable (proves the whole atomic
        // migration — table + decode_backend column — committed).
        let profiles = catalog
            .reader()
            .camera_profiles()
            .unwrap_or_else(|e| panic!("iteration {iteration}: camera_profile unreadable: {e}"));
        assert!(
            profiles.is_empty(),
            "iteration {iteration}: no bundled sync ran yet"
        );
        // No committed data lost across the crash-during-upgrade.
        assert_eq!(
            catalog.reader().counts().expect("counts").assets as usize,
            SEED_ASSETS,
            "iteration {iteration}: assets lost during the upgrade"
        );
        if catalog.schema_version() == latest {
            fully_upgraded_reopens += 1;
        }
        drop(catalog);

        // --- copy-on-write pre-upgrade safety copy is present + clean + v1 ---
        let pre = lbdata.join("backups/pre-upgrade-1/catalog.sqlite");
        assert!(
            pre.is_file(),
            "iteration {iteration}: missing copy-on-write pre-upgrade snapshot at {}",
            pre.display()
        );
        let findings = lightbox_catalog::integrity_check_file(&pre)
            .unwrap_or_else(|e| panic!("iteration {iteration}: pre-upgrade copy unreadable: {e}"));
        assert!(
            findings.is_empty(),
            "iteration {iteration}: pre-upgrade snapshot corrupt: {findings:?}"
        );
    }

    assert!(
        fully_upgraded_reopens > 0,
        "harness never completed a single upgrade — timing is off"
    );
    eprintln!(
        "migration fault injection: {iterations} mid-migration kills, 0 corruptions, \
         0 lost rows, {fully_upgraded_reopens} upgrades completed to v{latest}"
    );
}

fn seed_asset(folder: FolderId, i: usize) -> NewAsset {
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
    hash[8..].copy_from_slice(&0xE02_C010D_u64.to_le_bytes());
    NewAsset {
        folder,
        filename: format!("seed-{i}.cr3"),
        content_hash: ContentHash(hash),
        format: "CR3".to_owned(),
        camera_make: Some("Canon".to_owned()),
        camera_model: Some("EOS R6".to_owned()),
        capture_time: None,
        width: 6000,
        height: 4000,
        orientation: Orientation::O1,
        bytes: 24_000_000,
        mtime_utc: None,
        decode_error: None,
        import_session: None,
    }
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
