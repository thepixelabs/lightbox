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
}

/// Every migration this build ships, ordered. Later epics append here under
/// numbers they reserved in the registry.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        number: 1,
        name: "spine",
        sql: include_str!("../migrations/0001_spine.sql"),
    },
    // E02 Phase H (H1): the color-foundation schema — the `camera_profile`
    // registry of installed looks/DCPs (synced on open, see profile_sync.rs)
    // and the `asset.decode_backend` diagnostics column.
    Migration {
        number: 2,
        name: "e02_color",
        sql: include_str!("../migrations/0002_e02_color.sql"),
    },
    // E09 Phase B (T5): the edit state (primary data model) — edit_recipe,
    // edit_index, history_step, snapshot, xmp_sync (§3.1.1 / §4.1).
    Migration {
        number: 3,
        name: "edit_state",
        sql: include_str!("../migrations/0003_edit_state.sql"),
    },
    // E03 Phase A (T03): the preview pyramid index + raw-cache accounting —
    // preview, raw_cache_entry (§4). Recreates `preview` rather than altering
    // (cache rows are disposable; see the migration file header).
    Migration {
        number: 4,
        name: "preview_pyramid",
        sql: include_str!("../migrations/0004_preview_pyramid.sql"),
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
        let txn = conn.transaction()?;
        txn.execute_batch(m.sql)?;
        txn.execute(
            "INSERT INTO schema_version (version, applied_at, description) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.number, now_rfc3339_utc(), m.name],
        )?;
        txn.commit()?;
        tracing::info!(number = m.number, name = m.name, "migration applied");
        applied += 1;
    }
    Ok(applied)
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
}
