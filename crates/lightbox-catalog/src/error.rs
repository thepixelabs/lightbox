// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Error taxonomy of the catalog crate.
//!
//! `rusqlite` types never escape this crate (spec §7 R6): SQLite failures are
//! carried as strings, split into [`CatalogError::Constraint`] (expected,
//! DAO-level outcomes tests assert on) and [`CatalogError::Sqlite`]
//! (everything else).

use std::path::PathBuf;

/// Crate-wide result alias (the spec's DAO signatures say `Result<T>`).
pub type Result<T, E = CatalogError> = std::result::Result<T, E>;

/// Everything that can go wrong opening, mutating, querying, or backing up
/// a catalog.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// Filesystem-level failure (create dirs, copy, rename, …).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// SQLite failure that is not a constraint violation.
    #[error("sqlite: {0}")]
    Sqlite(String),

    /// A schema constraint rejected the mutation (FK, UNIQUE, CHECK).
    #[error("constraint violation: {0}")]
    Constraint(String),

    /// `PRAGMA quick_check` (or opening the file at all) failed. The newest
    /// verified backup is named so the user can be pointed at it (spec §1.1;
    /// guided restore itself is E16).
    #[error("{}", corrupt_message(.messages, .newest_backup))]
    Corrupt {
        /// The findings reported by SQLite (or the open error).
        messages: Vec<String>,
        /// Newest verified backup found under `backups/`, if any.
        newest_backup: Option<PathBuf>,
    },

    /// The catalog was written by a newer build (spec §3.2: forward-only,
    /// clear error, no write).
    #[error(
        "catalog schema version {found} is newer than this build supports \
         (max {supported}); update Lightbox to open this catalog"
    )]
    SchemaTooNew {
        /// `MAX(version)` recorded in the catalog.
        found: u32,
        /// Highest migration number this build ships.
        supported: u32,
    },

    /// `Catalog::create` refuses to overwrite an existing catalog.
    #[error("a catalog already exists at {0}")]
    AlreadyExists(PathBuf),

    /// `Catalog::open` found no `catalog.sqlite` in the `.lbdata` dir.
    #[error("no catalog found at {0}")]
    MissingOnDisk(PathBuf),

    /// The dedicated writer thread is gone (catalog closed or writer
    /// panicked fatally); the mutation was not applied.
    #[error("catalog writer is gone; mutation not applied")]
    WriterGone,

    /// Paths crossing into the catalog must be UTF-8 at M0 (spec OQ-3).
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),

    /// A DAO was pointed at a row that does not exist.
    #[error("{entity} {id} not found")]
    NotFound {
        /// Table/entity name.
        entity: &'static str,
        /// The rowid that was requested.
        id: i64,
    },

    /// Caller-supplied argument rejected before touching SQLite.
    #[error("invalid argument: {0}")]
    InvalidArg(String),

    /// `backup_verified` aborted; prior backups are untouched (spec §5 T12).
    #[error("backup failed: {0}")]
    BackupFailed(String),

    /// Invariant violation inside this crate (bug), including a panicking
    /// `with_txn` closure (the transaction was rolled back).
    #[error("internal catalog error: {0}")]
    Internal(String),
}

fn corrupt_message(messages: &[String], newest_backup: &Option<PathBuf>) -> String {
    let mut s = format!(
        "catalog failed the integrity check ({} finding{}): {}",
        messages.len(),
        if messages.len() == 1 { "" } else { "s" },
        messages.join("; "),
    );
    match newest_backup {
        Some(p) => {
            s.push_str(&format!(
                ". Newest verified backup: {} (restore: decompress it over catalog.sqlite)",
                p.display()
            ));
        }
        None => s.push_str(". No verified backup was found"),
    }
    s
}

impl From<rusqlite::Error> for CatalogError {
    fn from(err: rusqlite::Error) -> Self {
        match &err {
            rusqlite::Error::SqliteFailure(e, _)
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                CatalogError::Constraint(err.to_string())
            }
            _ => CatalogError::Sqlite(err.to_string()),
        }
    }
}
