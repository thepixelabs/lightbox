// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The activity model (E06 spec §4.5, T9) — E08's data source.
//!
//! Publication discipline (keeps the UI thread cold):
//! - Progress writes are atomic increments on the sink — no lock, no
//!   channel send per item.
//! - A publisher task folds the registry into a fresh [`ActivitySnapshot`]
//!   at `snapshot_publish_hz` (default 10 Hz) **only when something
//!   changed**, and stores it in an `ArcSwap`. Shell cost per frame = one
//!   atomic pointer load ([`crate::Scheduler::activity`]).
//! - Completed entries linger `completed_linger` so fast jobs are visible,
//!   then age out; **failed entries linger until dismissed**
//!   (`JobCommand::Dismiss` → [`crate::Scheduler::dismiss`]).
//! - Groups aggregate their member jobs into **one** entry; member jobs do
//!   not appear individually (they still count in [`ActivityCounts`]).
//! - [`JobEvent`]s ride a `broadcast` channel for `lightbox-cli watch`,
//!   tests, and edge-triggered consumers — a slow subscriber lags, it never
//!   backpressures the scheduler.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};

use crate::model::{ActivityRef, Class, GroupId, JobId, JobState, Outcome};
use crate::progress::{Dirty, ProgressView};
use crate::sched::Scheduler;

/// One row of the activity center (spec §4.5).
#[derive(Clone, Debug)]
pub struct ActivityEntry {
    /// Job or group identity.
    pub id: ActivityRef,
    /// Machine-readable kind (`"preview.extract"`).
    pub kind: &'static str,
    /// Human-readable label; presentation-neutral.
    pub label: String,
    /// Current detail line (e.g. the file being processed), throttled.
    pub detail: Option<String>,
    /// Scheduling class.
    pub class: Class,
    /// Public state (groups derive a representative state from members).
    pub state: JobState,
    /// Whether pause applies (renders the pause button).
    pub pausable: bool,
    /// Progress, when the entry reports any.
    pub progress: Option<ProgressView>,
    /// When the job/group was created.
    pub started_at: SystemTime,
    /// User-facing failure summary, present when `Done(Failed)`; lingers
    /// until dismissed.
    pub error: Option<String>,
}

/// Per-class queued/running/paused counts (a status-bar glyph's worth).
/// Indexed by class in lane order (Interactive, Foreground, Background).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityCounts {
    queued: [u32; 3],
    running: [u32; 3],
    paused: [u32; 3],
}

impl ActivityCounts {
    /// Jobs waiting for dispatch in `class`.
    pub fn queued(&self, class: Class) -> u32 {
        self.queued[class.index()]
    }

    /// Jobs dispatched and not yet terminal in `class` (includes
    /// `Cancelling`).
    pub fn running(&self, class: Class) -> u32 {
        self.running[class.index()]
    }

    /// Jobs paused in `class`.
    pub fn paused(&self, class: Class) -> u32 {
        self.paused[class.index()]
    }

    /// Total non-terminal jobs across all classes.
    pub fn total_live(&self) -> u32 {
        let sum = |a: &[u32; 3]| a.iter().sum::<u32>();
        sum(&self.queued) + sum(&self.running) + sum(&self.paused)
    }

    pub(crate) fn count(&mut self, class: Class, state: JobState) {
        let i = class.index();
        match state {
            JobState::Queued => self.queued[i] += 1,
            JobState::Running | JobState::Cancelling => self.running[i] += 1,
            JobState::Paused => self.paused[i] += 1,
            JobState::Done(_) => {}
        }
    }
}

/// Immutable, cheaply cloneable snapshot; the shell polls once per frame
/// (spec §4.5). Obtained lock-free via [`crate::Scheduler::activity`].
#[derive(Clone, Debug)]
pub struct ActivitySnapshot {
    /// Monotonic publication sequence (strictly increasing; a UI can skip
    /// work when unchanged).
    pub seq: u64,
    /// Rows, groups aggregated (see module docs). Ordered by start time.
    pub entries: Vec<ActivityEntry>,
    /// Whether each class is class-paused, in lane order (use
    /// [`ActivitySnapshot::is_class_paused`]).
    pub class_paused: [bool; 3],
    /// Per-class queued/running/paused counts (members counted
    /// individually).
    pub counts: ActivityCounts,
}

impl ActivitySnapshot {
    pub(crate) fn empty() -> ActivitySnapshot {
        ActivitySnapshot {
            seq: 0,
            entries: Vec::new(),
            class_paused: [false; 3],
            counts: ActivityCounts::default(),
        }
    }

    /// Whether `class` is currently class-paused.
    pub fn is_class_paused(&self, class: Class) -> bool {
        self.class_paused[class.index()]
    }
}

/// Edge-triggered scheduler events (spec §4.5). For `lightbox-cli watch`,
/// tests, and event-driven consumers; the shell needs only the snapshot.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum JobEvent {
    /// A job was admitted.
    Spawned {
        /// The new job.
        id: JobId,
        /// Its kind.
        kind: &'static str,
        /// Its class.
        class: Class,
        /// Its group, if any.
        group: Option<GroupId>,
    },
    /// A job's public state changed.
    StateChanged {
        /// The job.
        id: JobId,
        /// The new state.
        state: JobState,
    },
    /// Coalesced progress tick — read the payload from the snapshot.
    Progress {
        /// The entry that progressed.
        id: ActivityRef,
    },
    /// A group completed (closed + all members terminal).
    GroupFinished {
        /// The group.
        id: GroupId,
        /// Its aggregate outcome.
        outcome: Outcome,
    },
}

/// The publisher task (spec §4.5): waits for the dirty mark (or the next
/// linger expiry), rate-limits to `snapshot_publish_hz`, folds, stores,
/// emits coalesced `Progress` events, and sweeps aged-out entries.
pub(crate) async fn publisher_loop(sched: Weak<Scheduler>, dirty: Arc<Dirty>) {
    // Per-entry progress epochs from the previous publish (coalesced
    // Progress emission).
    let mut last_epochs: HashMap<ActivityRef, u64> = HashMap::new();
    let mut last_publish: Option<tokio::time::Instant> = None;
    loop {
        // Wake on change or on the earliest pending linger expiry.
        let next_expiry = sched.upgrade().and_then(|s| s.earliest_linger_expiry());
        match next_expiry {
            Some(at) => {
                tokio::select! {
                    () = dirty.wait() => {}
                    () = tokio::time::sleep_until(at) => {}
                }
            }
            None => dirty.wait().await,
        }
        if dirty.is_closed() {
            return;
        }
        // Rate limit: at most `hz` publishes per second.
        if let Some(s) = sched.upgrade() {
            let hz = s.publish_hz();
            let min_gap = Duration::from_secs(1) / u32::from(hz);
            if let Some(last) = last_publish {
                let next_allowed = last + min_gap;
                if tokio::time::Instant::now() < next_allowed {
                    tokio::time::sleep_until(next_allowed).await;
                }
            }
        }
        if dirty.is_closed() {
            return;
        }
        dirty.take();
        let Some(s) = sched.upgrade() else { return };
        s.publish_now(&mut last_epochs);
        last_publish = Some(tokio::time::Instant::now());
    }
}
