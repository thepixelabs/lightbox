// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-ingest` — import pipeline primitives.
//!
//! Owned by **E01 for the minimal add-in-place path** (spec §1 item 4,
//! T17–T18): discovery walk → per-file probe + content hash → batched
//! catalog inserts (one WAL transaction per batch) → import-session
//! bracketing → throttled progress events → cooperative cancellation.
//! Undo-import is a catalog DAO (`remove_import_session`) driven by
//! `lightbox-core`'s `Command::UndoImport`; files on disk are never touched.
//!
//! **E04** owns the full pipeline (copy/move/rename templates, second-copy
//! backup, presets, card detection, watched folders). The primitives here —
//! [`discover_files`], [`import_files`], [`import_add_in_place`] — are
//! written for its reuse, and [`ImportOptions`] stays `#[non_exhaustive]`.
//!
//! # Failure cataloguing (T18)
//!
//! - **Probe failure, hash OK** → the file is catalogued anyway with
//!   `decode_error` set (badged in the grid) *and* listed in
//!   [`ImportReport::errors`].
//! - **Hash failure** (unreadable file) → [`ImportReport::errors`] entry
//!   only: `content_hash` is `NOT NULL` by schema — a file we cannot read
//!   cannot be identified, deduplicated, or relinked, so it gets no row.
//! - **Non-UTF-8 path** → per-file error (spec §4.3 note c / OQ-3).
//!
//! No per-file error aborts the batch; cancellation commits completed
//! batches only.

// E04 (mandate v2.2 addendum): the headless, live-filesystem folder-explorer
// primitive — Finder/Explorer-style immediate-children navigation. Reads
// live, persists nothing; not managed import, not a library.
mod browse;
mod pipeline;
mod report;
// E04: the v2.0 working-set loader (spec §4.3) — enumerate -> probe -> order
// -> hash -> register, replacing the retired v1.x managed-import E04. Files
// open in place; the set itself is session state, never persisted.
mod working_set;

pub use browse::{browse_dir, DirListing, ImageEntry};
pub use pipeline::{
    discover_files, import_add_in_place, import_files, Discovery, KNOWN_EXTENSIONS,
};
pub use report::{ImportEvent, ImportOptions, ImportOutcome, ImportReport};
pub use working_set::{
    load_working_set, plan_open, LoadEvent, OpenError, OpenOptions, OpenOrigin, OpenReport,
    OpenRequest, PlanProgress, PlannedItem, SetPlan, SkipReason, SkippedPath,
};

/// Errors that abort an import outright (per-file problems never do — they
/// land in [`ImportReport::errors`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IngestError {
    /// The source directory is missing, not a directory, or unreadable.
    #[error("invalid import source {path}: {reason}")]
    InvalidSource {
        /// The offending path.
        path: std::path::PathBuf,
        /// Why it was rejected.
        reason: String,
    },
    /// A catalog transaction failed (the batch was rolled back).
    #[error("catalog: {0}")]
    Catalog(#[from] lightbox_catalog::CatalogError),
    /// Filesystem error walking the source tree.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
