// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`JobsConfig`] — the scheduler's runtime-updatable knobs (E06 spec §4.8,
//! T16). Loaded from the prefs store at session start (the E08 performance
//! panel binds exactly these fields) and re-appliable live via
//! [`crate::Scheduler::apply_config`].
//!
//! Live-apply semantics: worker budgets take effect immediately (growing) or
//! as running jobs finish (shrinking — permit starvation, never aborts);
//! `snapshot_publish_hz`/`completed_linger` apply from the next publisher
//! tick. **`fg_cpu_threads`/`bg_cpu_threads` do NOT re-size live** (rayon
//! pools are fixed at construction — spec §9 R8); they apply on the next
//! session.

use std::time::Duration;

/// Scheduler sizing/behavior knobs (spec §4.8). `#[non_exhaustive]`: build
/// via [`JobsConfig::default`] and override fields (the `CoreConfig`
/// convention).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct JobsConfig {
    /// Concurrent Foreground jobs. Default `max(2, cores/2)`.
    pub foreground_workers: usize,
    /// Concurrent Background jobs. Default `max(1, cores/4)`.
    pub background_workers: usize,
    /// Concurrent Interactive jobs — "effectively unqueued" (spec §4.4): a
    /// generous cap Interactive coordination work never hits. Default
    /// `max(8, 2·cores)`. (Not in the spec's §4.8 sketch; the lane needs a
    /// number — see `E06-deviations.md`.)
    pub interactive_workers: usize,
    /// Background budget while interactive activity is recent and/or
    /// Foreground has a queue backlog. Default 1.
    pub background_min_during_interactive: usize,
    /// How long after [`crate::Scheduler::note_interactive_activity`] the
    /// Background budget stays shrunk. Default 500 ms.
    pub interactive_recent_window: Duration,
    /// Foreground queue depth beyond which the Background budget also
    /// shrinks (spec §4.4 "while Foreground has queued work beyond a
    /// threshold" — the spec names the behavior, not the knob; see
    /// `E06-deviations.md`). Default 4.
    pub foreground_queue_shrink_threshold: usize,
    /// `fg-cpu` rayon pool threads. Default `max(2, cores − 2)`.
    /// **Applies on next session only** (R8).
    pub fg_cpu_threads: usize,
    /// `bg-cpu` rayon pool threads. Default `max(1, cores/4)`.
    /// **Applies on next session only** (R8).
    pub bg_cpu_threads: usize,
    /// Activity-snapshot publish rate when something changed. Default 10.
    pub snapshot_publish_hz: u8,
    /// How long completed entries linger in the snapshot so fast jobs are
    /// visible. Default 5 s. (Failed entries linger until dismissed.)
    pub completed_linger: Duration,
    /// Capacity of the `JobEvent` broadcast channel (slow subscribers lag,
    /// they never backpressure the scheduler — spec T9). Default 1024.
    pub event_capacity: usize,
    /// Debug-watchdog policy (spec §4.2/T18): when a Running job goes > 1 s
    /// without a checkpoint the watchdog logs it; with this set it also
    /// counts as fatal for the deterministic test harness (violations are
    /// always counted either way). Default `false`.
    pub watchdog_fatal: bool,
}

impl Default for JobsConfig {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4);
        JobsConfig {
            foreground_workers: (cores / 2).max(2),
            background_workers: (cores / 4).max(1),
            interactive_workers: (2 * cores).max(8),
            background_min_during_interactive: 1,
            interactive_recent_window: Duration::from_millis(500),
            foreground_queue_shrink_threshold: 4,
            fg_cpu_threads: cores.saturating_sub(2).max(2),
            bg_cpu_threads: (cores / 4).max(1),
            snapshot_publish_hz: 10,
            completed_linger: Duration::from_secs(5),
            event_capacity: 1024,
            watchdog_fatal: false,
        }
    }
}

impl JobsConfig {
    /// A copy with every field clamped to its sane floor (workers ≥ 1,
    /// hz ≥ 1, capacity ≥ 16). Applied at construction and on every
    /// [`crate::Scheduler::apply_config`] — bad prefs can never wedge the
    /// scheduler.
    pub(crate) fn sanitized(&self) -> JobsConfig {
        let mut cfg = self.clone();
        cfg.foreground_workers = cfg.foreground_workers.max(1);
        cfg.background_workers = cfg.background_workers.max(1);
        cfg.interactive_workers = cfg.interactive_workers.max(1);
        cfg.background_min_during_interactive = cfg.background_min_during_interactive.max(1);
        cfg.fg_cpu_threads = cfg.fg_cpu_threads.max(1);
        cfg.bg_cpu_threads = cfg.bg_cpu_threads.max(1);
        cfg.snapshot_publish_hz = cfg.snapshot_publish_hz.max(1);
        cfg.event_capacity = cfg.event_capacity.max(16);
        cfg
    }

    /// The budget for `class`'s lane (before load-shed).
    pub(crate) fn workers(&self, class: crate::Class) -> usize {
        match class {
            crate::Class::Interactive => self.interactive_workers,
            crate::Class::Foreground => self.foreground_workers,
            crate::Class::Background => self.background_workers,
        }
    }
}
