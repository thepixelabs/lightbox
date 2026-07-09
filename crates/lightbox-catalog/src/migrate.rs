// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Forward-only migration runner (spec §4.1 mechanics).
//!
//! Embedded SQL files, applied in order, **one transaction per migration**,
//! recorded in `schema_version`. There are no down-migrations — restore from
//! a verified backup is the rollback story. Before the first pending
//! migration runs on a pre-existing catalog, the original `catalog.sqlite` is
//! copied to `backups/pre-upgrade-<ver>/` (copy-on-write upgrade safety).
//!
//! Migration numbers are reserved workspace-wide in `docs/plan/migrations.md`
//! and cross-checked by `cargo xtask lint-migrations` (spec §4.4, OQ-2).

use std::path::Path;

use rusqlite::Connection;

use crate::clock::now_rfc3339_utc;
use crate::error::{CatalogError, Result};

/// One embedded migration. `sql` must not contain `BEGIN`/`COMMIT` — the
/// runner owns the transaction.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Migration {
    /// Registry number (`docs/plan/migrations.md`); file `NNNN_<name>.sql`.
    pub number: u32,
    /// Short name, recorded as `schema_version.description`.
    pub name: &'static str,
    /// The DDL/DML batch.
    pub sql: &'static str,
    /// True for migrations that rebuild a table with inbound foreign keys
    /// (E04 spec §5.2): the runner sets `PRAGMA foreign_keys = OFF` *outside*
    /// the migration transaction (the pragma is a no-op inside one), runs the
    /// batch, asserts `PRAGMA foreign_key_check` returns ZERO rows before
    /// commit, and restores `foreign_keys = ON` after — always, whether the
    /// migration committed or was aborted. Without this, `DROP TABLE asset`
    /// performs an implicit DELETE that CASCADE-deletes every `image` row —
    /// the failure mode this flag exists to make impossible.
    pub rebuilds_tables: bool,
}

/// Every migration this build ships, ordered. Later epics append here under
/// numbers they reserved in the registry.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        number: 1,
        name: "spine",
        sql: include_str!("../migrations/0001_spine.sql"),
        rebuilds_tables: false,
    },
    // E02 Phase H (H1): the color-foundation schema — the `camera_profile`
    // registry of installed looks/DCPs (synced on open, see profile_sync.rs)
    // and the `asset.decode_backend` diagnostics column.
    Migration {
        number: 2,
        name: "e02_color",
        sql: include_str!("../migrations/0002_e02_color.sql"),
        rebuilds_tables: false,
    },
    // E09 Phase B (T5): the edit state (primary data model) — edit_recipe,
    // edit_index, history_step, snapshot, xmp_sync (§3.1.1 / §4.1).
    Migration {
        number: 3,
        name: "edit_state",
        sql: include_str!("../migrations/0003_edit_state.sql"),
        rebuilds_tables: false,
    },
    // E03 Phase A (T03): the preview pyramid index + raw-cache accounting —
    // preview, raw_cache_entry (§4). Recreates `preview` rather than altering
    // (cache rows are disposable; see the migration file header).
    Migration {
        number: 4,
        name: "preview_pyramid",
        sql: include_str!("../migrations/0004_preview_pyramid.sql"),
        rebuilds_tables: false,
    },
    // E04 (T3): open-in-place intake (spec §3.1.1/§5.1) — rebuilds `asset`
    // with `folder_id` nullable and an `abs_path` path-hint column, drops the
    // managed-tree `UNIQUE(folder_id, filename)` guard. `image`/`edit_recipe`/
    // `edit_index`/`history_step`/`snapshot`/`xmp_sync`/`preview` all chain
    // `ON DELETE CASCADE` off `asset`/`image` — exactly the two-level cascade
    // `rebuilds_tables` exists to make impossible.
    Migration {
        number: 5,
        name: "open_in_place",
        sql: include_str!("../migrations/0005_open_in_place.sql"),
        rebuilds_tables: true,
    },
];

/// Highest schema version a migration set supports.
pub(crate) fn supported_version(migrations: &[Migration]) -> u32 {
    migrations.last().map(|m| m.number).unwrap_or(0)
}

/// `MAX(version)` recorded in the catalog; 0 when the `schema_version` table
/// does not exist yet (fresh file).
pub(crate) fn current_version(conn: &Connection) -> Result<u32> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let v: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;
    u32::try_from(v).map_err(|_| CatalogError::Internal(format!("negative schema version {v}")))
}

/// Applies every pending migration. Returns the number of migrations applied.
///
/// Refuses (before any write) when the catalog's version is newer than the
/// migration set. When `pre_upgrade_copy_dir` is `Some` and the catalog is a
/// pre-existing one (version > 0) with pending migrations, the WAL is
/// checkpointed and `catalog.sqlite` copied to
/// `<pre_upgrade_copy_dir>/pre-upgrade-<ver>/catalog.sqlite` first.
pub(crate) fn apply_pending(
    conn: &mut Connection,
    migrations: &[Migration],
    db_path: &Path,
    pre_upgrade_copy_dir: Option<&Path>,
) -> Result<u32> {
    debug_assert!(
        migrations.windows(2).all(|w| w[0].number < w[1].number),
        "migration list must be strictly ascending"
    );

    let current = current_version(conn)?;
    let supported = supported_version(migrations);
    if current > supported {
        return Err(CatalogError::SchemaTooNew {
            found: current,
            supported,
        });
    }

    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.number > current).collect();
    if pending.is_empty() {
        return Ok(0);
    }

    // Copy-on-write upgrade safety (spec §4.1): only for catalogs that
    // already have a schema — a fresh create has nothing worth copying.
    if current > 0 {
        if let Some(backups_dir) = pre_upgrade_copy_dir {
            write_pre_upgrade_copy(conn, db_path, backups_dir, current)?;
        }
    }

    let mut applied = 0u32;
    for m in pending {
        let span = tracing::info_span!("apply_migration", number = m.number, name = m.name);
        let _guard = span.enter();
        if m.rebuilds_tables {
            apply_rebuild_migration(conn, m)?;
        } else {
            let txn = conn.transaction()?;
            txn.execute_batch(m.sql)?;
            txn.execute(
                "INSERT INTO schema_version (version, applied_at, description) VALUES (?1, ?2, ?3)",
                rusqlite::params![m.number, now_rfc3339_utc(), m.name],
            )?;
            txn.commit()?;
        }
        tracing::info!(number = m.number, name = m.name, "migration applied");
        applied += 1;
    }
    Ok(applied)
}

/// Runs a [`Migration`] flagged `rebuilds_tables` (spec §5.2): `PRAGMA
/// foreign_keys = OFF` (only legal outside a transaction — the pragma is a
/// no-op inside one) → the migration's own transaction, gated on `PRAGMA
/// foreign_key_check` returning zero rows before commit → `PRAGMA
/// foreign_keys = ON`, unconditionally, whether the migration committed or
/// was aborted. A `foreign_key_check` failure rolls the transaction back
/// (dropped, never committed) — the schema is left exactly as it was, and
/// `schema_version` is not recorded, so the migration is retried on the next
/// open.
fn apply_rebuild_migration(conn: &mut Connection, m: &Migration) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let outcome = (|| -> Result<()> {
        let txn = conn.transaction()?;
        txn.execute_batch(m.sql)?;
        let violations = foreign_key_check_count(&txn)?;
        if violations > 0 {
            return Err(CatalogError::Internal(format!(
                "migration {:04} ({}) left {violations} foreign-key violation(s) after \
                 rebuild; aborted (schema unchanged)",
                m.number, m.name
            )));
        }
        txn.execute(
            "INSERT INTO schema_version (version, applied_at, description) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.number, now_rfc3339_utc(), m.name],
        )?;
        txn.commit()?;
        Ok(())
    })();
    // Always restore enforcement — a rebuild migration must never leave the
    // session connections' `foreign_keys=ON` invariant (catalog.rs's
    // `open_configured`) in doubt, success or failure.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    outcome
}

/// Row count of `PRAGMA foreign_key_check` — independent of whether
/// enforcement (`PRAGMA foreign_keys`) is currently on or off; it always
/// performs the full accounting scan.
fn foreign_key_check_count(txn: &rusqlite::Transaction<'_>) -> Result<usize> {
    let mut stmt = txn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt.query_map([], |_| Ok(()))?;
    let mut n = 0usize;
    for row in rows {
        row?;
        n += 1;
    }
    Ok(n)
}

/// Checkpoints the WAL (so `catalog.sqlite` alone is the full database) and
/// copies it to `backups/pre-upgrade-<ver>/catalog.sqlite`. An existing copy
/// for the same version is kept — the earliest pre-upgrade state is the one
/// worth preserving if an upgrade is retried.
fn write_pre_upgrade_copy(
    conn: &Connection,
    db_path: &Path,
    backups_dir: &Path,
    current: u32,
) -> Result<()> {
    let dir = backups_dir.join(format!("pre-upgrade-{current}"));
    let dest = dir.join("catalog.sqlite");
    if dest.exists() {
        tracing::info!(path = %dest.display(), "pre-upgrade copy already present; keeping it");
        return Ok(());
    }
    // TRUNCATE fails (busy) rather than silently under-checkpointing when a
    // reader is active; at this point in open() we are the only connection.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(".catalog.sqlite.tmp");
    std::fs::copy(db_path, &tmp)?;
    std::fs::rename(&tmp, &dest)?;
    tracing::info!(from_version = current, path = %dest.display(), "pre-upgrade copy written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_numbers_are_contiguous_from_one() {
        // The runner applies strictly in order; a gap would mean a migration
        // was merged without its registry reservation (docs/plan/migrations.md).
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                m.number,
                u32::try_from(i).unwrap() + 1,
                "migration {} is out of sequence",
                m.name
            );
        }
    }

    #[test]
    fn migration_sql_owns_no_transactions() {
        for m in MIGRATIONS {
            let statements = m
                .sql
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .to_uppercase();
            assert!(
                !statements.contains("BEGIN TRANSACTION") && !statements.contains("COMMIT"),
                "migration {} must not manage its own transaction",
                m.name
            );
        }
    }

    // ── T2: the `rebuilds_tables` FK-off/foreign_key_check/FK-on procedure ──

    /// A minimal parent/child schema with `ON DELETE CASCADE`, matching the
    /// shape `asset`/`image` have in the real catalog: rebuilding `parent`
    /// must not implicitly CASCADE-delete `child` rows.
    const SEED: Migration = Migration {
        number: 1,
        name: "seed",
        sql: "\
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, description TEXT NOT NULL);\n\
            CREATE TABLE parent (id INTEGER PRIMARY KEY, val TEXT);\n\
            CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id) ON DELETE CASCADE, val TEXT);\n",
        rebuilds_tables: false,
    };

    /// A well-formed rebuild: copies every `parent` row across, so `child`'s
    /// foreign keys stay satisfied post-rebuild.
    const GOOD_REBUILD: Migration = Migration {
        number: 2,
        name: "good_rebuild",
        sql: "\
            CREATE TABLE parent_new (id INTEGER PRIMARY KEY, val TEXT, extra TEXT);\n\
            INSERT INTO parent_new (id, val) SELECT id, val FROM parent;\n\
            DROP TABLE parent;\n\
            ALTER TABLE parent_new RENAME TO parent;\n",
        rebuilds_tables: true,
    };

    /// A broken rebuild: drops `parent` and recreates it EMPTY, orphaning
    /// every `child` row — `foreign_key_check` must catch this and abort
    /// before commit.
    const BAD_REBUILD: Migration = Migration {
        number: 2,
        name: "bad_rebuild",
        sql: "\
            CREATE TABLE parent_new (id INTEGER PRIMARY KEY, val TEXT);\n\
            DROP TABLE parent;\n\
            ALTER TABLE parent_new RENAME TO parent;\n",
        rebuilds_tables: true,
    };

    /// A fresh, `foreign_keys=ON` connection (mirrors `catalog.rs`'s
    /// `open_configured`, minus WAL — irrelevant to this unit's behavior).
    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn
    }

    fn fk_enforcement_on(conn: &Connection) -> bool {
        let v: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        v == 1
    }

    #[test]
    fn rebuild_migration_preserves_cascade_children_and_reenables_foreign_keys() {
        let mut conn = fresh_conn();
        apply_pending(&mut conn, &[SEED], Path::new("unused.sqlite"), None).expect("seed");
        conn.execute("INSERT INTO parent (id, val) VALUES (1, 'p')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO child (id, parent_id, val) VALUES (1, 1, 'c1'), (2, 1, 'c2')",
            [],
        )
        .unwrap();
        assert!(fk_enforcement_on(&conn));

        let applied = apply_pending(
            &mut conn,
            &[SEED, GOOD_REBUILD],
            Path::new("unused.sqlite"),
            None,
        )
        .expect("rebuild migration");
        assert_eq!(applied, 1);

        // The child rows survived the parent-table rebuild byte-for-byte —
        // proof the implicit CASCADE delete never fired (FK was off for the
        // DROP TABLE).
        let child_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM child", [], |r| r.get(0))
            .unwrap();
        assert_eq!(child_count, 2, "cascade children must survive the rebuild");
        let child_vals: Vec<String> = {
            let mut stmt = conn.prepare("SELECT val FROM child ORDER BY id").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(child_vals, vec!["c1".to_owned(), "c2".to_owned()]);

        assert_eq!(current_version(&conn).unwrap(), 2);
        assert!(
            fk_enforcement_on(&conn),
            "foreign_keys must be back ON after a successful rebuild"
        );
    }

    #[test]
    fn rebuild_migration_fk_violation_aborts_and_leaves_schema_unchanged() {
        let mut conn = fresh_conn();
        apply_pending(&mut conn, &[SEED], Path::new("unused.sqlite"), None).expect("seed");
        conn.execute("INSERT INTO parent (id, val) VALUES (1, 'p')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO child (id, parent_id, val) VALUES (1, 1, 'c1')",
            [],
        )
        .unwrap();

        let err = apply_pending(
            &mut conn,
            &[SEED, BAD_REBUILD],
            Path::new("unused.sqlite"),
            None,
        )
        .expect_err("a foreign_key_check violation must abort the migration");
        assert!(err.to_string().contains("foreign-key violation"), "{err}");

        // Schema version unchanged — the migration is retried on next open.
        assert_eq!(current_version(&conn).unwrap(), 1);
        // The transaction rolled back: `parent` still has its original row
        // (the rebuild's DROP/RENAME never committed).
        let parent_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM parent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(parent_count, 1, "rolled-back rebuild must not touch parent");
        // No data lost: the orphaned-in-the-attempt child row is untouched
        // (never even reached, since the whole transaction rolled back).
        let child_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM child", [], |r| r.get(0))
            .unwrap();
        assert_eq!(child_count, 1);

        assert!(
            fk_enforcement_on(&conn),
            "foreign_keys must be restored ON even after an aborted rebuild"
        );
    }
}
