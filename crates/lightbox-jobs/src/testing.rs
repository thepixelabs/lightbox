// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Deterministic manual-dispatch harness (E06 spec §4.10): the same
//! [`Scheduler`] with **no background dispatch/publisher/watchdog tasks** —
//! tests drive admission with [`ManualScheduler::tick`] and snapshot
//! publication with [`ManualScheduler::publish`], under `tokio::time::
//! pause()` virtual time. No sleeps, no flaky thresholds: ordering and
//! preemption tests enumerate exactly which jobs start per tick.
//!
//! Job **bodies** still execute for real on the runtime once dispatched —
//! manual mode controls *when dispatch happens*, not how futures run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::config::JobsConfig;
use crate::model::ActivityRef;
use crate::sched::{lock, Scheduler};

/// Same API as [`Scheduler`] (deref), plus manual dispatch/publish.
pub struct ManualScheduler {
    inner: Arc<Scheduler>,
    /// Publisher-local progress-epoch memory (what the background
    /// publisher task keeps in its loop state).
    epochs: Mutex<HashMap<ActivityRef, u64>>,
}

impl ManualScheduler {
    /// Builds a manual-mode scheduler on `rt`. Nothing dispatches until
    /// [`ManualScheduler::tick`].
    pub fn new(cfg: JobsConfig, rt: tokio::runtime::Handle) -> ManualScheduler {
        ManualScheduler {
            inner: Scheduler::build(cfg, rt, true),
            epochs: Mutex::new(HashMap::new()),
        }
    }

    /// The scheduler under test (spawn/control/observe through it).
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.inner
    }

    /// One dispatch pass over every lane (respecting budgets, priorities,
    /// pauses, and load-shed exactly like the background dispatchers).
    /// Returns how many jobs started.
    pub fn tick(&self) -> usize {
        self.inner.dispatch_pass()
    }

    /// Folds and publishes an activity snapshot now (what the publisher
    /// task does per coalesced tick), emitting coalesced `Progress` events.
    pub fn publish(&self) {
        self.inner.publish_now(&mut lock(&self.epochs));
    }

    /// One watchdog sweep (checkpoint-contract policing); returns newly
    /// flagged violations. Pair with `JobsConfig::watchdog_fatal` and
    /// [`Scheduler::metrics`] for the test-mode-fatal assertion (T18).
    pub fn watchdog_sweep(&self) -> usize {
        self.inner.watchdog_sweep()
    }
}

impl std::ops::Deref for ManualScheduler {
    type Target = Arc<Scheduler>;

    fn deref(&self) -> &Arc<Scheduler> {
        &self.inner
    }
}

impl std::fmt::Debug for ManualScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualScheduler").finish_non_exhaustive()
    }
}
