// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`CoreConfig`] — knobs for [`crate::Core::start`].

use std::path::PathBuf;
use std::time::Duration;

use lightbox_ingest::OpenOptions;
use lightbox_jobs::{JobConfig, JobsConfig};

/// Configuration for a [`crate::Core`]. `#[non_exhaustive]`: build via
/// [`CoreConfig::default`] and override fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CoreConfig {
    /// Job-system sizing (per-class budgets, worker threads) — the E01
    /// seed runtime (`JobSystem`).
    pub jobs: JobConfig,
    /// E06 scheduler knobs (spec §4.8): lane budgets, load-shed window,
    /// CPU-pool threads, activity-publish rate. **Prefs-store seam (T16):**
    /// E08's preferences store loads/saves this and re-applies it live via
    /// `Session::jobs().apply_config` — until that store exists, sessions
    /// start from these defaults (override fields programmatically for
    /// tests/tuning).
    pub jobs_scheduler: JobsConfig,
    /// Capacity of the broadcast event channel (spec §3.8). Slow
    /// subscribers lag (they observe `RecvError::Lagged` and skip ahead);
    /// the writer is never blocked by them.
    pub event_capacity: usize,
    /// Byte cap for the in-memory embedded-preview LRU (spec §3.6;
    /// consumed by T21's `EmbeddedPreviewProvider`).
    pub preview_cache_bytes: u64,
    /// Exit-time backup policy (spec T15 / OQ-6): on close, back up unless
    /// the newest verified backup is younger than this.
    pub backup_max_age: Duration,
    /// How many dated backups to retain (spec §3.2 default 10).
    pub backup_retain: u32,
    /// How long `Session::close` waits for in-flight command jobs
    /// (cancelled imports flushing their final stats) before backing up.
    pub close_wait: Duration,
    /// E06 (spec §4.9): grace for the job-scheduler drain at close —
    /// running jobs get this long to observe cancellation at a checkpoint
    /// before stragglers are aborted (aborted-nonzero = a bug signal).
    pub jobs_shutdown_grace: Duration,
    /// Root directory for the develop-preset store (spec §3.7 / D3:
    /// app-level files, not catalog rows). `None` resolves the platform
    /// default (`<config>/Lightbox/presets`) — but only lazily, on first
    /// preset use (`EditHub`), so a session that never touches presets
    /// never creates the directory. Tests override this to a tempdir to
    /// stay hermetic.
    pub preset_dir: Option<PathBuf>,
    /// Working-set loader knobs (E04 spec §4.5: `max_set_size`,
    /// `progress_min_interval`) — passed straight to
    /// `lightbox_ingest::plan_open`/`load_working_set` on every
    /// `Command::OpenWorkingSet`.
    pub working_set: OpenOptions,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            jobs: JobConfig::default(),
            jobs_scheduler: JobsConfig::default(),
            event_capacity: 1024,
            preview_cache_bytes: 256 * 1024 * 1024,
            backup_max_age: Duration::from_secs(24 * 60 * 60),
            backup_retain: 10,
            close_wait: Duration::from_secs(10),
            jobs_shutdown_grace: Duration::from_secs(5),
            preset_dir: None,
            working_set: OpenOptions::default(),
        }
    }
}
