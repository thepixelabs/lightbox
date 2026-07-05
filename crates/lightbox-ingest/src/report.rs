// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Import options, progress events, and the report (spec §3.8
//! `ImportReport`; the type lives here and is re-exported by
//! `lightbox-core`, so ingest and core share one definition).

use std::path::PathBuf;
use std::time::Duration;

use lightbox_types::ImportSessionId;

/// Options for an add-in-place import. `#[non_exhaustive]` by contract
/// (spec §1.1): **E04** grows this (copy/move/rename templates, presets,
/// second-copy, …) without breaking callers — build via
/// [`ImportOptions::default`] and override fields.
#[derive(Clone, Debug, serde::Serialize)]
#[non_exhaustive]
pub struct ImportOptions {
    /// Descend into subdirectories.
    pub recursive: bool,
    /// Files per catalog transaction (spec T17: 64). Clamped to ≥ 1.
    pub batch_size: usize,
    /// Minimum interval between two progress events (spec T17: throttled
    /// ≤ 10 Hz). The final progress event is always emitted.
    pub progress_min_interval: Duration,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            recursive: true,
            batch_size: 64,
            progress_min_interval: Duration::from_millis(100),
        }
    }
}

/// What an import did (spec §3.8; stored as the session's `stats` JSON and
/// broadcast in `Event::ImportFinished`).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ImportReport {
    /// Asset rows inserted (each gets its default image row).
    pub imported: u64,
    /// Files skipped because their content hash is already catalogued.
    pub skipped_duplicates: u64,
    /// Subset of `imported` catalogued as `'UNSUPPORTED'` (badged in the
    /// grid, never a crash — spec §3.7).
    pub unsupported: u64,
    /// Per-file failures: `(path, why)`. Probe failures are *also*
    /// catalogued with `decode_error` set; hash/path failures get no row
    /// (see crate docs).
    pub errors: Vec<(PathBuf, String)>,
    /// Wall time of the whole import.
    pub took: Duration,
}

/// Callback events emitted while an import runs. `lightbox-core` translates
/// these 1:1 into its broadcast `Event`s (spec §3.8).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ImportEvent {
    /// The import session is open; discovery is complete.
    Started {
        /// The `import_session` row bracketing this import.
        session: ImportSessionId,
        /// Number of files that passed the extension filter.
        discovered: u64,
    },
    /// Progress heartbeat, throttled to `progress_min_interval`.
    Progress {
        /// The session this import runs under.
        session: ImportSessionId,
        /// Files fully processed so far (staged or failed).
        done: u64,
        /// Total files discovered.
        discovered: u64,
        /// The file just processed.
        current: PathBuf,
    },
}

/// What [`crate::import_files`] returns.
#[derive(Clone, Debug)]
pub struct ImportOutcome {
    /// The `import_session` row (finished, stats recorded — even when
    /// cancelled).
    pub session: ImportSessionId,
    /// The report (also serialized into the session's `stats` JSON).
    pub report: ImportReport,
    /// True when the import stopped at a cancellation checkpoint. Completed
    /// batches stay committed; the partially staged batch was discarded.
    pub cancelled: bool,
}
