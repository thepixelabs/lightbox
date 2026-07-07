// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-catalog` — the crash-proof SQLite catalog (single source of
//! truth). Owned by **E01** (spec §3.2, §4), implemented in Phase 3 (T8–T13).
//!
//! # Shape
//!
//! - **One writer.** A dedicated thread owns the sole write connection;
//!   [`WriterHandle::with_txn`] marshals every mutation into exactly one WAL
//!   transaction (architecture §3.1 crash-safety invariant). No
//!   `SQLITE_BUSY` between workspace writers by construction.
//! - **Pooled WAL-snapshot readers.** `min(cores, 4)` `query_only`
//!   connections behind [`Catalog::reader`]; queries run on the calling
//!   thread and never block the writer.
//! - **Forward-only migrations.** Embedded SQL, one transaction per
//!   migration, recorded in `schema_version`; numbers are reserved in
//!   `docs/plan/migrations.md` (linted by `cargo xtask lint-migrations`).
//!   Newer-than-this-build catalogs are refused; pre-existing catalogs get a
//!   copy-on-write `backups/pre-upgrade-<ver>/` safety copy before upgrade.
//! - **Verified backup.** [`Catalog::backup_verified`]: online-backup →
//!   `integrity_check` *on the copy* → zstd → temp+rename into a dated dir →
//!   prune. A backup that fails verification is never promoted.
//! - **Connection config** per spec §4.2: WAL, `synchronous=NORMAL`,
//!   `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`; readers
//!   additionally `query_only=ON`. The SQLite amalgamation is bundled.
//!
//! No SQL and no `rusqlite` type crosses this crate's boundary (spec §2.3
//! seam 1, §7 R6): mutations go through the [`CatalogTxn`] DAOs, queries
//! through [`ReaderHandle`].
//!
//! The `kill -9` fault-injection harness (spec §5 T13, Risk 5 gate) lives in
//! `tests/fault_injection.rs`; the 1000-iteration leg runs nightly.

mod backup;
mod catalog;
mod clock;
mod dao;
// E09 Phase B (T5-DAOs/T6): the edit-state store layer — write DAOs on
// `CatalogTxn` and the matching read surface on `ReaderHandle` for
// edit_recipe/edit_index/history_step/snapshot/xmp_sync (migration 0003).
mod edit_state;
mod error;
mod migrate;
mod pages;
// E03 Phase A (T03/T04): the preview-pyramid index store layer — write DAOs
// on `CatalogTxn` and the matching read surface on `ReaderHandle` for the
// `preview` table (migration 0004_preview_pyramid).
mod preview_dao;
// E02 Phase H (H1): the `camera_profile` registry — bundled-asset sync-on-open
// + install/query DAOs (migration 0002_e02_color).
mod profile_sync;
mod reader;
mod writer;

pub use backup::{BackupOpts, BackupReport};
#[doc(hidden)]
pub use catalog::integrity_check_file;
pub use catalog::{Catalog, IntegrityStatus};
pub use dao::{InsertOutcome, NewAsset, RemovedCounts};
pub use edit_state::{
    EditBadge, EditStateRow, HistoryReplayRange, HistoryStepRow, SnapshotRow, XmpSyncRow,
};
pub use error::{CatalogError, Result};
pub use pages::{ImageQuery, ImageSummary, Page, PageCursor, SortOrder};
pub use preview_dao::{NewPreviewRow, PreviewRow, PreviewSourceTag};
pub use profile_sync::{
    BundledProfile, InstalledProfile, ProfileKind, ProfileSource, ProfileUpsert, SyncReport,
};
pub use reader::{CatalogCounts, FolderNode, ImageDetail, ReaderHandle};
pub use writer::{CatalogTxn, WriterHandle};

#[cfg(test)]
mod tests;
