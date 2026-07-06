// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Catalog`] — open/create, connection configuration (spec §4.2), the
//! single writer, the reader pool, integrity, and verified backup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

use crate::backup::{integrity_findings, newest_backup, run_backup, BackupOpts, BackupReport};
use crate::error::{CatalogError, Result};
use crate::migrate::{apply_pending, current_version, Migration, MIGRATIONS};
use crate::reader::{ReaderHandle, ReaderPool};
use crate::writer::{Writer, WriterHandle};

/// Cheap integrity summary (spec §3.2) — `PRAGMA quick_check`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntegrityStatus {
    /// `quick_check` reported no findings.
    Ok,
    /// The findings (or the error that prevented checking).
    Corrupt(Vec<String>),
}

/// A crash-proof SQLite catalog inside a `.lbdata` directory (spec §3.2).
///
/// One dedicated writer thread serializes all mutations (each inside exactly
/// one WAL transaction); `min(cores, 4)` pooled read connections serve
/// WAL-snapshot queries. Dropping the `Catalog` joins the writer after a
/// best-effort checkpoint — crash safety never depends on a clean close.
pub struct Catalog {
    lbdata_dir: PathBuf,
    db_path: PathBuf,
    writer: Writer,
    readers: Arc<ReaderPool>,
    schema_version: u32,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("lbdata_dir", &self.lbdata_dir)
            .field("schema_version", &self.schema_version)
            .finish_non_exhaustive()
    }
}

impl Catalog {
    /// Creates `<name>.lbdata/` (catalog.sqlite + backups/) and applies all
    /// migrations (spec §3.2). Refuses to overwrite an existing catalog.
    pub fn create(lbdata_dir: &Path) -> Result<Catalog> {
        Self::create_with_migrations(lbdata_dir, MIGRATIONS)
    }

    /// Opens WAL, runs `PRAGMA quick_check`, applies pending migrations
    /// (forward-only, copy-on-write pre-upgrade backup). Refuses a catalog
    /// whose schema version is newer than this build (spec §3.2).
    pub fn open(lbdata_dir: &Path) -> Result<Catalog> {
        Self::open_with_migrations(lbdata_dir, MIGRATIONS)
    }

    pub(crate) fn create_with_migrations(
        lbdata_dir: &Path,
        migrations: &[Migration],
    ) -> Result<Catalog> {
        let db_path = lbdata_dir.join("catalog.sqlite");
        if db_path.exists() {
            return Err(CatalogError::AlreadyExists(db_path));
        }
        std::fs::create_dir_all(lbdata_dir)?;
        std::fs::create_dir_all(lbdata_dir.join("backups"))?;
        Self::start(lbdata_dir, migrations)
    }

    pub(crate) fn open_with_migrations(
        lbdata_dir: &Path,
        migrations: &[Migration],
    ) -> Result<Catalog> {
        let db_path = lbdata_dir.join("catalog.sqlite");
        if !db_path.exists() {
            return Err(CatalogError::MissingOnDisk(db_path));
        }
        std::fs::create_dir_all(lbdata_dir.join("backups"))?;
        Self::start(lbdata_dir, migrations)
    }

    /// Shared create/open tail: configure → quick_check → migrate → spin up
    /// the writer thread and reader pool.
    fn start(lbdata_dir: &Path, migrations: &[Migration]) -> Result<Catalog> {
        let db_path = lbdata_dir.join("catalog.sqlite");
        let backups_dir = lbdata_dir.join("backups");

        let mut conn = open_configured(&db_path, false)
            .and_then(|conn| quick_check_clean(&conn).map(|()| conn))
            .map_err(|err| corruption_to_open_error(err, &backups_dir))?;

        apply_pending(&mut conn, migrations, &db_path, Some(&backups_dir))?;
        let schema_version = current_version(&conn)?;

        let readers = {
            let n = std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
                .min(4);
            let mut conns = Vec::with_capacity(n);
            for _ in 0..n {
                conns.push(open_configured(&db_path, true)?);
            }
            Arc::new(ReaderPool::new(conns))
        };

        // The setup connection becomes THE write connection.
        let writer = Writer::spawn(conn)?;
        tracing::info!(
            catalog = %db_path.display(),
            schema_version,
            "catalog open"
        );
        Ok(Catalog {
            lbdata_dir: lbdata_dir.to_path_buf(),
            db_path,
            writer,
            readers,
            schema_version,
        })
    }

    /// The single serialized writer (spec §5.1).
    pub fn writer(&self) -> WriterHandle {
        self.writer.handle()
    }

    /// A WAL-snapshot read connection from the pool. Blocks when all
    /// `min(cores, 4)` connections are checked out — drop handles promptly.
    pub fn reader(&self) -> ReaderHandle {
        ReaderHandle::checkout(&self.readers)
    }

    /// Cheap integrity probe — `PRAGMA quick_check` (spec §3.2).
    pub fn integrity(&self) -> IntegrityStatus {
        let reader = self.reader();
        match quick_check_findings(reader.conn()) {
            Ok(findings) if findings.is_empty() => IntegrityStatus::Ok,
            Ok(findings) => IntegrityStatus::Corrupt(findings),
            Err(e) => IntegrityStatus::Corrupt(vec![e.to_string()]),
        }
    }

    /// Online-backup → `integrity_check` on the copy → zstd → temp+rename
    /// into `backups/YYYY-MM-DD-HHMMSS/catalog.sqlite.zst` → prune to
    /// retention (spec §3.2). Safe to run while the writer is busy.
    pub fn backup_verified(&self, opts: &BackupOpts) -> Result<BackupReport> {
        let dest_root = opts
            .dest_override
            .clone()
            .unwrap_or_else(|| self.lbdata_dir.join("backups"));
        run_backup(&self.db_path, &dest_root, opts.retain, None)
    }

    /// Highest applied migration number (spec §3.2).
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Idempotently re-registers the bundled color assets (E02 spec §4.1, task
    /// H1). Runs every open in the real app so the `camera_profile` registry
    /// tracks the shipped `.lblook` / `.dcp` set; content drift rewrites in
    /// place, nothing is duplicated. The caller (`lightbox-cli` / the app)
    /// parses each file to derive its [`BundledProfile`] record — this crate
    /// stores identity + provenance only, never blobs. Idempotent: the second
    /// call over an unchanged set reports all-`unchanged`.
    pub fn sync_bundled_profiles(
        &self,
        profiles: &[crate::profile_sync::BundledProfile],
    ) -> Result<crate::profile_sync::SyncReport> {
        let profiles = profiles.to_vec();
        self.writer().with_txn(move |txn| {
            let mut report = crate::profile_sync::SyncReport::default();
            for p in &profiles {
                match txn.upsert_camera_profile(p)? {
                    crate::profile_sync::ProfileUpsert::Inserted => report.inserted += 1,
                    crate::profile_sync::ProfileUpsert::Updated => report.updated += 1,
                    crate::profile_sync::ProfileUpsert::Unchanged => report.unchanged += 1,
                }
            }
            Ok(report)
        })
    }

    /// Test-support constructor: creates a catalog with only the first `upto`
    /// migrations applied, so the migration-fault harness can build a pre-0002
    /// catalog and then exercise a real upgrade under `kill -9`
    /// (`tests/migration_0002_fault_injection.rs`). Not a shipping API.
    #[doc(hidden)]
    pub fn create_at_schema_version_for_tests(lbdata_dir: &Path, upto: usize) -> Result<Catalog> {
        let prefix = &MIGRATIONS[..upto.min(MIGRATIONS.len())];
        Self::create_with_migrations(lbdata_dir, prefix)
    }

    /// When the newest *verified* backup was taken (UTC, parsed from its
    /// dated directory name under `backups/`), or `None` when no verified
    /// backup exists. `lightbox-core`'s exit-time backup policy ("on close,
    /// unless the last backup is fresher than 24 h" — spec §5 T15, OQ-6)
    /// keys on this.
    pub fn newest_backup_time(&self) -> Option<std::time::SystemTime> {
        crate::backup::newest_backup_time(&self.lbdata_dir.join("backups"))
    }

    /// The `.lbdata` directory this catalog lives in.
    pub fn lbdata_dir(&self) -> &Path {
        &self.lbdata_dir
    }

    /// Test-only access to the backup pipeline's injection point
    /// (spec §5 T12: verify abort-on-failed-verification atomicity).
    #[cfg(test)]
    pub(crate) fn backup_with_injected_failure(
        &self,
        opts: &BackupOpts,
        after_copy: &dyn Fn(&Path) -> std::io::Result<()>,
    ) -> Result<BackupReport> {
        let dest_root = opts
            .dest_override
            .clone()
            .unwrap_or_else(|| self.lbdata_dir.join("backups"));
        run_backup(&self.db_path, &dest_root, opts.retain, Some(after_copy))
    }
}

/// Opens one connection with the spec §4.2 configuration.
fn open_configured(db_path: &Path, reader: bool) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // journal_mode returns a row; verify WAL actually took (it is persistent
    // in the file after the first time, but never assume).
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(CatalogError::Sqlite(format!(
            "could not enable WAL (journal_mode = {mode})"
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    if reader {
        conn.pragma_update(None, "query_only", "ON")?;
    }
    Ok(conn)
}

/// `PRAGMA quick_check`; empty = clean.
fn quick_check_findings(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA quick_check")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut findings = Vec::new();
    for row in rows {
        let msg = row?;
        if msg != "ok" {
            findings.push(msg);
        }
    }
    Ok(findings)
}

fn quick_check_clean(conn: &Connection) -> Result<()> {
    let findings = quick_check_findings(conn)?;
    if findings.is_empty() {
        Ok(())
    } else {
        Err(CatalogError::Corrupt {
            messages: findings,
            newest_backup: None, // filled in by corruption_to_open_error
        })
    }
}

/// Any corruption-shaped failure during open (unreadable file, failed
/// quick_check) is reported as [`CatalogError::Corrupt`] naming the newest
/// verified backup (spec §1.1). Genuine non-corruption errors pass through.
fn corruption_to_open_error(err: CatalogError, backups_dir: &Path) -> CatalogError {
    match err {
        CatalogError::Corrupt { messages, .. } => CatalogError::Corrupt {
            messages,
            newest_backup: newest_backup(backups_dir),
        },
        CatalogError::Sqlite(msg)
            if msg.contains("file is not a database") || msg.contains("malformed") =>
        {
            CatalogError::Corrupt {
                messages: vec![msg],
                newest_backup: newest_backup(backups_dir),
            }
        }
        other => other,
    }
}

/// Full `PRAGMA integrity_check` helper for tests and the fault harness —
/// re-exported for the restore drill (unzstd → open → integrity, spec §6).
#[doc(hidden)]
pub fn integrity_check_file(db_path: &Path) -> Result<Vec<String>> {
    let conn = Connection::open(db_path)?;
    integrity_findings(&conn).map_err(CatalogError::from)
}
