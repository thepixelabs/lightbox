// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Cancellation & pause primitives (E06 spec §4.2, T2).
//!
//! Cancellation is the E01 seed's hierarchical [`CancelToken`]
//! (`cancel.rs`) — the same type `lightbox-render`'s scheduler already
//! threads through every render request (spec §5.3 "the same token type"),
//! so E06 reuses it rather than re-typing the workspace onto
//! `tokio_util::sync::CancellationToken` (see `E06-deviations.md`).
//!
//! Pause is [`PauseGate`]: a cooperative, checkpoint-granular gate that is
//! **cheap when not paused** (one atomic load). The gate word carries three
//! bits — job-paused, class-paused, and the `should_yield` load-shed hint —
//! so the scheduler's control plane (per-job pause, `pause_class`, budget
//! shrink) all publish through one word the hot path reads once.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

use crate::cancel::CancelToken;

/// Why a job was interrupted at a checkpoint: **cancelled** (semantically —
/// pause never errors; it just waits; spec §4.2).
///
/// Converts into [`crate::JobError::Cancelled`] so job bodies can `?` a
/// checkpoint straight out of a `Result<T, JobError>` future.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("interrupted (cancelled)")]
pub struct Interrupted;

impl From<Interrupted> for crate::system::JobError {
    fn from(_: Interrupted) -> crate::system::JobError {
        crate::system::JobError::Cancelled
    }
}

/// Gate-word bit: this specific job was paused (`Scheduler::pause`).
pub(crate) const BIT_JOB_PAUSED: u32 = 1 << 0;
/// Gate-word bit: the job's whole class is paused (`Scheduler::pause_class`).
pub(crate) const BIT_CLASS_PAUSED: u32 = 1 << 1;
/// Gate-word bit: load-shed hint — the lane is over its shrunk budget and
/// would like the job to yield/re-queue voluntarily (spec §4.4, T5). Never
/// blocks anything by itself.
pub(crate) const BIT_SHOULD_YIELD: u32 = 1 << 2;

const PAUSE_MASK: u32 = BIT_JOB_PAUSED | BIT_CLASS_PAUSED;

/// Cooperative pause gate (spec §4.2): the job-gate and class-gate **ANDed**
/// — a job proceeds only when *neither* its own pause bit nor its class's
/// pause bit is set. Cheap when not paused: [`PauseGate::is_paused`] is one
/// atomic load.
///
/// Clone-cheap; all clones observe the same gate.
#[derive(Clone)]
pub struct PauseGate {
    inner: Arc<GateInner>,
}

pub(crate) struct GateInner {
    /// [`BIT_JOB_PAUSED`] | [`BIT_CLASS_PAUSED`] | [`BIT_SHOULD_YIELD`].
    bits: AtomicU32,
    /// Woken on every bit *clear* (resume paths). Pause needs no wake — the
    /// job discovers it at its next checkpoint.
    notify: Notify,
}

impl PauseGate {
    /// A fresh, unpaused gate. `class_paused`: whether the job's class is
    /// already paused at spawn time (spec T10: class-pause halts
    /// subsequently-spawned jobs too).
    pub(crate) fn new(class_paused: bool) -> PauseGate {
        PauseGate {
            inner: Arc::new(GateInner {
                bits: AtomicU32::new(if class_paused { BIT_CLASS_PAUSED } else { 0 }),
                notify: Notify::new(),
            }),
        }
    }

    /// True while either the job or its class is paused. One atomic load.
    pub fn is_paused(&self) -> bool {
        self.inner.bits.load(Ordering::Acquire) & PAUSE_MASK != 0
    }

    /// The load-shed hint (spec §4.4/T5): true while the scheduler would
    /// like this job to voluntarily yield (finish its current work quantum
    /// and re-queue, or simply expect slower dispatch of its siblings).
    /// Purely advisory — throttling itself never aborts a running job.
    pub fn should_yield(&self) -> bool {
        self.inner.bits.load(Ordering::Acquire) & BIT_SHOULD_YIELD != 0
    }

    /// Resolves when unpaused; `Err(Interrupted)` if `cancel` fires while
    /// waiting (T2 AC: cancel-while-paused errors out of the wait — a
    /// paused job never has to resume before it can be cancelled).
    ///
    /// Returns immediately when not paused.
    pub async fn wait_if_paused(&self, cancel: &CancelToken) -> Result<(), Interrupted> {
        loop {
            // Create the Notified future BEFORE re-checking the bits:
            // `notify_waiters` wakes every future created before the call,
            // so a resume landing between check and await cannot be missed
            // (same discipline as `CancelToken::cancelled`).
            let notified = self.inner.notify.notified();
            if !self.is_paused() {
                return Ok(());
            }
            if cancel.is_cancelled() {
                return Err(Interrupted);
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(Interrupted),
                _ = notified => {}
            }
        }
    }

    /// Sets/clears a gate bit. Clearing wakes parked waiters.
    pub(crate) fn set_bit(&self, bit: u32, on: bool) {
        if on {
            self.inner.bits.fetch_or(bit, Ordering::AcqRel);
        } else {
            let prev = self.inner.bits.fetch_and(!bit, Ordering::AcqRel);
            if prev & bit != 0 {
                self.inner.notify.notify_waiters();
            }
        }
    }

    /// The raw gate word (the checkpoint fast path's single load).
    pub(crate) fn bits(&self) -> u32 {
        self.inner.bits.load(Ordering::Acquire)
    }

    /// True while either pause bit is set in `bits` (helper for callers
    /// that already loaded the word).
    pub(crate) fn word_paused(bits: u32) -> bool {
        bits & PAUSE_MASK != 0
    }
}

impl std::fmt::Debug for PauseGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bits = self.bits();
        f.debug_struct("PauseGate")
            .field("job_paused", &(bits & BIT_JOB_PAUSED != 0))
            .field("class_paused", &(bits & BIT_CLASS_PAUSED != 0))
            .field("should_yield", &(bits & BIT_SHOULD_YIELD != 0))
            .finish()
    }
}
