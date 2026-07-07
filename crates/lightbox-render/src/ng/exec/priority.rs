// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Priority lanes — Interactive preempts Batch at tile granularity (spec §4.3 /
//! the E15 no-starvation mechanism; task **C9**).
//!
//! Owner: **C**. E05 provides the **mechanism** (tile-granularity
//! Interactive-over-Batch); E15 sets the export scheduling *policy* (spec §1.1).
//! A single worker drains a two-lane queue, always taking an Interactive tile
//! ahead of any pending Batch tile — so an Interactive submit made while a Batch
//! render is in flight starts within one tile-duration, while the Batch's
//! remaining tiles still run to completion (no starvation in either direction:
//! Batch resumes once the Interactive lane drains).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use crate::ng::engine::RenderPriority;

/// A unit of schedulable work (one tile's evaluation).
type Task = Box<dyn FnOnce() + Send + 'static>;

struct Inner {
    interactive: VecDeque<Task>,
    batch: VecDeque<Task>,
    closed: bool,
}

/// A two-lane cooperative scheduler: a worker pops **Interactive-first** at each
/// task (tile) boundary, giving slider work priority over a running export batch
/// without starving the batch (task C9).
pub struct PriorityLanes {
    inner: Mutex<Inner>,
    signal: Condvar,
}

impl PriorityLanes {
    /// A fresh, empty scheduler.
    pub fn new() -> Arc<PriorityLanes> {
        Arc::new(PriorityLanes {
            inner: Mutex::new(Inner {
                interactive: VecDeque::new(),
                batch: VecDeque::new(),
                closed: false,
            }),
            signal: Condvar::new(),
        })
    }

    /// Enqueue one tile of work into the lane for `priority`.
    pub fn submit<F>(&self, priority: RenderPriority, task: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let mut inner = self.lock();
        match priority {
            RenderPriority::Interactive => inner.interactive.push_back(Box::new(task)),
            RenderPriority::Batch => inner.batch.push_back(Box::new(task)),
        }
        drop(inner);
        self.signal.notify_all();
    }

    /// Close the scheduler: the worker drains remaining work then exits.
    pub fn close(&self) {
        self.lock().closed = true;
        self.signal.notify_all();
    }

    /// The number of tasks currently pending in each lane (diagnostics/tests).
    pub fn pending(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.interactive.len(), inner.batch.len())
    }

    /// Pop the next task Interactive-first, blocking until one is available or
    /// the scheduler is closed and drained. `None` ⇒ the worker should exit.
    fn next(&self) -> Option<Task> {
        let mut inner = self.lock();
        loop {
            if let Some(t) = inner.interactive.pop_front() {
                return Some(t);
            }
            if let Some(t) = inner.batch.pop_front() {
                return Some(t);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .signal
                .wait(inner)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Run the worker loop on this thread until closed+drained. Each popped task
    /// runs to completion (tile granularity) before the next lane check — so an
    /// Interactive tile enqueued mid-batch is serviced after at most the current
    /// tile finishes.
    pub fn run_worker(self: &Arc<Self>) {
        while let Some(task) = self.next() {
            task();
        }
    }

    /// Spawn the worker on a background thread (returns its join handle).
    pub fn spawn_worker(self: &Arc<Self>) -> std::thread::JoinHandle<()> {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("lbx-priority-worker".to_owned())
            .spawn(move || me.run_worker())
            .expect("spawn priority worker")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// **C9 mechanism:** with a Batch render in flight, an Interactive submit's
    /// first tile starts within one tile-duration, and the Batch still completes.
    #[test]
    fn interactive_preempts_batch_at_tile_boundary() {
        const TILE: Duration = Duration::from_millis(8);
        const BATCH_TILES: u64 = 24;

        let lanes = PriorityLanes::new();
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let batch_done = Arc::new(AtomicU64::new(0));

        // A long batch: each tile sleeps one tile-duration.
        for _ in 0..BATCH_TILES {
            let order = Arc::clone(&order);
            let done = Arc::clone(&batch_done);
            lanes.submit(RenderPriority::Batch, move || {
                std::thread::sleep(TILE);
                order.lock().unwrap().push("batch");
                done.fetch_add(1, Ordering::Relaxed);
            });
        }

        let worker = lanes.spawn_worker();

        // Let a few batch tiles run, then fire an Interactive tile.
        std::thread::sleep(TILE * 3);
        let interactive_started = Arc::new(Mutex::new(None::<Instant>));
        let submit_at = Instant::now();
        {
            let order = Arc::clone(&order);
            let started = Arc::clone(&interactive_started);
            lanes.submit(RenderPriority::Interactive, move || {
                *started.lock().unwrap() = Some(Instant::now());
                order.lock().unwrap().push("interactive");
            });
        }

        lanes.close();
        worker.join().unwrap();

        let started = interactive_started
            .lock()
            .unwrap()
            .expect("interactive tile ran");
        let latency = started.duration_since(submit_at);
        // The interactive tile starts within ~one tile-duration (the current
        // batch tile must finish first, then it jumps the queue).
        assert!(
            latency < TILE * 3,
            "interactive tile waited {latency:?} (> ~1 tile @ {TILE:?})"
        );

        let order = order.lock().unwrap();
        let inter_pos = order.iter().position(|s| *s == "interactive").unwrap();
        // It preempted the bulk of the batch (ran well before the last batch tile).
        assert!(
            inter_pos < (BATCH_TILES as usize).saturating_sub(3),
            "interactive ran at {inter_pos} of {} — did not preempt",
            order.len()
        );
        // Batch still completed in full (no starvation).
        assert_eq!(batch_done.load(Ordering::Relaxed), BATCH_TILES);
        assert_eq!(order.len() as u64, BATCH_TILES + 1);
    }

    #[test]
    fn drains_and_exits_when_closed() {
        let lanes = PriorityLanes::new();
        let n = Arc::new(AtomicU64::new(0));
        for _ in 0..5 {
            let n = Arc::clone(&n);
            lanes.submit(RenderPriority::Batch, move || {
                n.fetch_add(1, Ordering::Relaxed);
            });
        }
        lanes.close();
        lanes.run_worker(); // on this thread; returns after draining
        assert_eq!(n.load(Ordering::Relaxed), 5);
        assert_eq!(lanes.pending(), (0, 0));
    }
}
