// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! T8 acceptance criteria: create→open idempotence, WAL snapshot isolation,
//! newer-schema refusal, exactly-once migrations, copy-on-write upgrade
//! (DoD §8.5 synthetic `0002`), corrupt-catalog refusal naming the newest
//! backup, and writer-thread resilience.

use std::sync::mpsc;

use tempfile::TempDir;

use super::{new_asset, seed_assets, seed_folder, temp_catalog};
use crate::migrate::{Migration, MIGRATIONS};
use crate::{BackupOpts, Catalog, CatalogError, IntegrityStatus};

#[test]
fn create_then_open_is_idempotent_and_migrates_once() {
    let dir = TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");

    let catalog = Catalog::create(&lbdata).unwrap();
    assert_eq!(catalog.schema_version(), MIGRATIONS.len() as u32);
    assert!(lbdata.join("catalog.sqlite").is_file());
    assert!(lbdata.join("backups").is_dir());
    assert_eq!(catalog.integrity(), IntegrityStatus::Ok);
    drop(catalog);

    // Reopen: no re-application, same version, quick_check clean.
    let catalog = Catalog::open(&lbdata).unwrap();
    assert_eq!(catalog.schema_version(), MIGRATIONS.len() as u32);
    let applied: i64 = catalog
        .writer()
        .with_txn(|txn| {
            Ok(txn
                .raw()
                .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
                .unwrap())
        })
        .unwrap();
    assert_eq!(
        applied,
        MIGRATIONS.len() as i64,
        "every shipped migration recorded exactly once"
    );
    drop(catalog);

    // create() refuses to clobber an existing catalog.
    match Catalog::create(&lbdata) {
        Err(CatalogError::AlreadyExists(_)) => {}
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
    // open() of a missing catalog is a clear error.
    match Catalog::open(&dir.path().join("nope.lbdata")) {
        Err(CatalogError::MissingOnDisk(_)) => {}
        other => panic!("expected MissingOnDisk, got {other:?}"),
    }
}

#[test]
fn concurrent_reader_sees_pre_txn_snapshot_during_long_write() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "pre", 3, 100, &[]);

    let (in_txn_tx, in_txn_rx) = mpsc::channel::<()>();
    let (commit_tx, commit_rx) = mpsc::channel::<()>();

    let writer = catalog.writer();
    let write_thread = std::thread::spawn(move || {
        writer.with_txn(move |txn| {
            let outcome = txn.insert_assets(&[new_asset(folder, "during.jpg", 999)])?;
            txn.insert_default_images(&outcome.inserted)?;
            in_txn_tx.send(()).unwrap();
            // Hold the write transaction open until the main thread has read.
            commit_rx.recv().unwrap();
            Ok(())
        })
    });

    in_txn_rx.recv().unwrap();
    // WAL proof: a reader in the middle of the uncommitted write transaction
    // sees the pre-transaction state (and is not blocked by the writer).
    let reader = catalog.reader();
    assert_eq!(reader.counts().unwrap().assets, 3);
    drop(reader);

    commit_tx.send(()).unwrap();
    write_thread.join().unwrap().unwrap();
    assert_eq!(catalog.reader().counts().unwrap().assets, 4);
}

#[test]
fn newer_schema_version_is_refused() {
    let dir = TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");
    let catalog = Catalog::create(&lbdata).unwrap();
    catalog
        .writer()
        .with_txn(|txn| {
            txn.raw()
                .execute(
                    "INSERT INTO schema_version (version, applied_at, description) \
                     VALUES (999, '2099-01-01T00:00:00.000000Z', 'from the future')",
                    [],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    drop(catalog);

    match Catalog::open(&lbdata) {
        Err(CatalogError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, 999);
            assert_eq!(supported, MIGRATIONS.len() as u32);
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
}

/// DoD §8.5: the copy-on-write upgrade path, exercised by migrating a
/// `0001`-only catalog forward with a synthetic `0002`.
#[test]
fn synthetic_0002_upgrade_writes_pre_upgrade_copy() {
    const SYNTHETIC: &[Migration] = &[
        Migration {
            number: 1,
            name: "spine",
            sql: MIGRATIONS[0].sql,
            rebuilds_tables: false,
        },
        Migration {
            number: 2,
            name: "synthetic-test-table",
            sql: "CREATE TABLE synthetic_two (id INTEGER PRIMARY KEY, note TEXT);",
            rebuilds_tables: false,
        },
    ];

    let dir = TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");
    {
        // Start at v1 explicitly: a real 0002 now ships (E02 e02_color), so this
        // synthetic-0002 upgrade drill must build its own pre-upgrade v1 state
        // rather than rely on `create()` stopping at v1.
        let catalog = Catalog::create_with_migrations(&lbdata, &MIGRATIONS[..1]).unwrap();
        assert_eq!(catalog.schema_version(), 1);
        let (_r, folder) = seed_folder(&catalog, dir.path());
        seed_assets(&catalog, folder, "v1", 5, 0, &[]);
    }

    let catalog = Catalog::open_with_migrations(&lbdata, SYNTHETIC).unwrap();
    assert_eq!(catalog.schema_version(), 2);
    // The new table exists…
    catalog
        .writer()
        .with_txn(|txn| {
            txn.raw()
                .execute("INSERT INTO synthetic_two (note) VALUES ('hi')", [])
                .unwrap();
            Ok(())
        })
        .unwrap();
    // …and the pre-upgrade copy is the version-1 catalog, complete with data.
    let copy = lbdata.join("backups/pre-upgrade-1/catalog.sqlite");
    assert!(copy.is_file(), "pre-upgrade copy missing");
    let conn = rusqlite::Connection::open(&copy).unwrap();
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
    let assets: i64 = conn
        .query_row("SELECT COUNT(*) FROM asset", [], |r| r.get(0))
        .unwrap();
    assert_eq!(assets, 5, "checkpointed copy must contain committed data");
    let has_synth: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='synthetic_two')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_synth, "copy must be the PRE-upgrade state");
    drop(catalog);

    // A build without 0002 refuses this catalog. (The real workspace now ships
    // its own 0002, so simulate the older build with a 0001-only migration set.)
    match Catalog::open_with_migrations(&lbdata, &MIGRATIONS[..1]) {
        Err(CatalogError::SchemaTooNew { found, supported }) => {
            assert_eq!((found, supported), (2, 1));
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
}

#[test]
fn corrupt_catalog_is_refused_naming_newest_backup() {
    let dir = TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");
    let expected_backup;
    {
        let catalog = Catalog::create(&lbdata).unwrap();
        let (_r, folder) = seed_folder(&catalog, dir.path());
        seed_assets(&catalog, folder, "x", 50, 0, &[]);
        expected_backup = catalog
            .backup_verified(&BackupOpts::default())
            .unwrap()
            .path;
    }

    // Trash the SQLite header, deterministic, unambiguous corruption.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(lbdata.join("catalog.sqlite"))
            .unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&[0xEu8; 32]).unwrap();
    }
    // A stale -wal would make SQLite fail recovery differently; the corrupt
    // main file is the case under test.
    let _ = std::fs::remove_file(lbdata.join("catalog.sqlite-wal"));
    let _ = std::fs::remove_file(lbdata.join("catalog.sqlite-shm"));

    match Catalog::open(&lbdata) {
        Err(CatalogError::Corrupt {
            messages,
            newest_backup,
        }) => {
            assert!(!messages.is_empty());
            assert_eq!(newest_backup.as_deref(), Some(expected_backup.as_path()));
            // The user-facing message names the backup (spec §1.1).
            let display = CatalogError::Corrupt {
                messages,
                newest_backup,
            }
            .to_string();
            assert!(
                display.contains(expected_backup.to_str().unwrap()),
                "error must name the newest verified backup: {display}"
            );
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn with_txn_rolls_back_on_error_and_survives_panics() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());

    // Err rolls back everything the closure did.
    let err = catalog
        .writer()
        .with_txn(move |txn| {
            let outcome = txn.insert_assets(&[new_asset(folder, "rollback.jpg", 1)])?;
            txn.insert_default_images(&outcome.inserted)?;
            Err::<(), _>(CatalogError::InvalidArg("abort".into()))
        })
        .unwrap_err();
    assert!(matches!(err, CatalogError::InvalidArg(_)));
    assert_eq!(catalog.reader().counts().unwrap().assets, 0);

    // A panicking closure is an Internal error; the transaction rolled back
    // and the writer thread keeps serving.
    let err = catalog
        .writer()
        .with_txn(move |txn| {
            txn.insert_assets(&[new_asset(folder, "panic.jpg", 2)])?;
            panic!("boom");
            #[allow(unreachable_code)]
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(err, CatalogError::Internal(_)), "{err:?}");
    assert_eq!(catalog.reader().counts().unwrap().assets, 0);

    // Writer still alive.
    seed_assets(&catalog, folder, "after", 1, 3, &[]);
    assert_eq!(catalog.reader().counts().unwrap().assets, 1);
}

#[test]
fn empty_migration_list_reports_version_zero() {
    // Degenerate guard: supported_version of an empty set is 0 and a fresh
    // file simply gets no schema (exercises the runner's edge, not a real
    // configuration).
    let dir = TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");
    let catalog = Catalog::create_with_migrations(&lbdata, &[]).unwrap();
    assert_eq!(catalog.schema_version(), 0);
}
