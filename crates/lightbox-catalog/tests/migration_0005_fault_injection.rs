// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `kill -9` **mid-migration** fault injection for the `0005_open_in_place`
//! upgrade (E04 spec §5.2/§9.4, T3 AC). Mirrors
//! `migration_0002_fault_injection.rs`'s harness exactly, but for the
//! `rebuilds_tables` procedure specifically: unlike `0002` (a plain `ALTER
//! TABLE ADD COLUMN`), `0005` rebuilds `asset` itself, `DROP TABLE asset`
//! with `foreign_keys` OFF, so a kill landing mid-rebuild is the sharpest
//! test of the FK-off/foreign_key_check/FK-on procedure (spec §5.2 Risk R1):
//! it must never leave the store CASCADE-deleted, half-rebuilt, or with
//! `foreign_keys` stuck off.
//!
//! Each iteration builds a fresh, fully-migrated-to-**0004** catalog, seeds
//! it with real asset/image rows (plus one edit-state row, to prove the
//! two-level `image`→`edit_recipe` cascade chain survives too), then spawns
//! a child (this same test binary, re-invoked filtered to
//! [`migration_0005_fault_child`]) that opens the catalog, running the 0005
//! upgrade inside the runner's rebuild procedure, after the copy-on-write
//! `backups/pre-upgrade-4/` safety copy. The parent SIGKILLs the child at a
//! random moment inside that window and then reopens, asserting:
//!
//! 1. **Integrity-clean**, reopen succeeds and `quick_check` is clean
//!    whether the kill landed before, during, or after the migration
//!    transaction.
//! 2. **`foreign_key_check` clean**, the rebuild's own commit-time gate,
//!    re-checked independently after reopen.
//! 3. **Fully upgraded after reopen**, `Catalog::open` completes any
//!    interrupted upgrade to the latest schema version.
//! 4. **No data loss, no phantom cascade**, every seeded asset AND its
//!    default image survive; the edit-state row seeded on one image is
//!    still there (the two-level cascade never fired).
//! 5. **Copy-on-write safety copy present**, `backups/pre-upgrade-4/`
//!    exists and is itself integrity-clean (the pre-upgrade v4 snapshot,
//!    unmigrated).
//!
//! Iterations: 24 by default (PR gate); `LIGHTBOX_MIGRATE_0005_FAULT_ITERS`
//! overrides for the nightly long leg.

use std::path::Path;
use std::time::Duration;

use lightbox_catalog::{Catalog, IntegrityStatus, NewAsset};
use lightbox_types::{ContentHash, FolderId, Orientation, PV_M0};

const CHILD_DIR_ENV: &str = "LIGHTBOX_MIGRATE_0005_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_MIGRATE_0005_FAULT_ITERS";

/// How many assets to seed into each pre-upgrade catalog.
const SEED_ASSETS: usize = 6;

/// Child-mode entry point. A no-op under normal `cargo test`; the real body
/// runs only when the parent re-invokes this binary with the env var set. It
/// opens the (v4) catalog, triggering the 0005 upgrade, then spins on
/// reads until SIGKILLed.
#[test]
fn migration_0005_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_open_and_migrate(Path::new(&dir)) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("migration 0005 fault child failed: {err:?}");
    }
}

fn child_open_and_migrate(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lbdata = dir.join("cat.lbdata");
    // This open runs apply_pending -> write_pre_upgrade_copy -> the 0005
    // rebuild (foreign_keys OFF / DROP+rebuild+recreate / foreign_key_check
    // / foreign_keys ON), all inside src/migrate.rs's apply_rebuild_migration.
    let catalog = Catalog::open(&lbdata)?;
    // Keep the catalog open and busy so a kill can also land in the
    // post-migration WAL window (recovery must still be clean).
    loop {
        let _ = catalog.reader().counts();
        std::hint::spin_loop();
    }
}

#[test]
fn kill9_mid_0005_migration_leaves_catalog_clean_and_upgraded() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only migration_0005_fault_child runs there
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());
    let mut fully_upgraded_reopens = 0u32;

    // The latest supported schema version, DERIVED (not hardcoded) so a
    // migration shipped after 0005 does not stale this test.
    let latest = {
        let probe = tempfile::TempDir::new().expect("tempdir");
        Catalog::create_at_schema_version_for_tests(&probe.path().join("probe.lbdata"), usize::MAX)
            .expect("create fully-migrated probe catalog")
            .schema_version()
    };

    for iteration in 0..iterations {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let lbdata = dir.join("cat.lbdata");

        // --- build a fresh pre-0005 (v4) catalog and seed real rows,
        // including one edit-state row (the two-level cascade proof) ---
        let seeded_image = {
            let catalog =
                Catalog::create_at_schema_version_for_tests(&lbdata, 4).expect("create v4 catalog");
            assert_eq!(catalog.schema_version(), 4, "seed catalog must be v4");
            let root_path = dir.join("photos");
            let (folder, images) = catalog
                .writer()
                .with_txn(move |txn| {
                    let root = txn.upsert_root(None, &root_path)?;
                    let folder = txn.upsert_folder(root, None, "mig5")?;
                    let batch: Vec<NewAsset> =
                        (0..SEED_ASSETS).map(|i| seed_asset(folder, i)).collect();
                    let outcome = txn.insert_assets(&batch)?;
                    let images = txn.insert_default_images(&outcome.inserted)?;
                    Ok((folder, images))
                })
                .expect("seed rows");
            let _ = folder;
            let seeded_image = images[0];
            catalog
                .writer()
                .with_txn(move |txn| {
                    txn.upsert_edit_recipe(seeded_image, PV_M0, 1, b"doc", 1)?;
                    txn.append_history_step(seeded_image, 1, b"op", b"delta", b"inverse", None)?;
                    Ok(())
                })
                .expect("seed edit-state row");
            seeded_image
        };

        // --- spawn the child that opens+migrates, then kill it mid-window ---
        let mut child = std::process::Command::new(&exe)
            .args(["migration_0005_fault_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn migration 0005 fault child");
        // The open+migrate happens in the first few ms; bias the kill to
        // land in (or just around) that window.
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

        // foreign_key_check clean, independent re-check of the rebuild's own gate.
        let fk_findings = lightbox_catalog::foreign_key_check_file(&lbdata.join("catalog.sqlite"))
            .unwrap_or_else(|e| {
                panic!("iteration {iteration}: foreign_key_check query failed: {e}")
            });
        assert!(
            fk_findings.is_empty(),
            "iteration {iteration}: foreign_key_check found violations post-reopen: {fk_findings:?}"
        );

        // No committed data lost, and no phantom cascade: every seeded
        // asset AND its default image survive.
        let counts = catalog.reader().counts().expect("counts");
        assert_eq!(
            counts.assets as usize, SEED_ASSETS,
            "iteration {iteration}: assets lost during the upgrade"
        );
        assert_eq!(
            counts.images as usize, SEED_ASSETS,
            "iteration {iteration}: images lost — the two-level asset->image cascade fired"
        );
        // The edit-state row on the seeded image is still there, the
        // image->edit_recipe/history_step leg of the two-level cascade
        // never fired either.
        assert!(
            catalog
                .reader()
                .edit_state_row(seeded_image)
                .unwrap_or_else(|e| panic!("iteration {iteration}: edit_state_row: {e}"))
                .is_some(),
            "iteration {iteration}: edit_recipe lost — cascade fired through image"
        );
        if catalog.schema_version() == latest {
            fully_upgraded_reopens += 1;
        }
        drop(catalog);

        // --- copy-on-write pre-upgrade safety copy is present + clean + v4 ---
        let pre = lbdata.join("backups/pre-upgrade-4/catalog.sqlite");
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
        "migration 0005 fault injection: {iterations} mid-migration kills, 0 corruptions, \
         0 lost rows, 0 phantom cascades, {fully_upgraded_reopens} upgrades completed to v{latest}"
    );
}

fn seed_asset(folder: FolderId, i: usize) -> NewAsset {
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
    hash[8..].copy_from_slice(&0xE04_FACE_u64.to_le_bytes());
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
