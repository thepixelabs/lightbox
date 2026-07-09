// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Progress model (E06 spec §4.5, T7): [`ProgressSink`] — atomic
//! increments on the hot path, **no lock, no channel send per item** — and
//! the [`ProgressView`] derivation the activity snapshot publishes.
//!
//! Totals may be unknown at spawn and may **grow** (import discovery):
//! [`ProgressSink::set_total`] is monotonic-agnostic; the derived fraction
//! is always clamped to `[0, 1]`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::model::ProgressStyle;
use crate::sched::lock;

/// Sentinel for "total unknown".
const UNKNOWN: u64 = u64::MAX;

/// Where a job's progress writes land (shared with the activity publisher).
pub(crate) struct ProgressState {
    style: ProgressStyle,
    items: AtomicU64,
    bytes: AtomicU64,
    total_items: AtomicU64,
    total_bytes: AtomicU64,
    detail: Mutex<Option<String>>,
    /// Bumped on every write — the publisher's change detector (coalesced
    /// `JobEvent::Progress` emission).
    epoch: AtomicU64,
}

impl ProgressState {
    pub(crate) fn new(style: ProgressStyle) -> ProgressState {
        let (total_items, total_bytes) = match style {
            ProgressStyle::Items { total } => (total.unwrap_or(UNKNOWN), UNKNOWN),
            ProgressStyle::Bytes { total } => (UNKNOWN, total.unwrap_or(UNKNOWN)),
            ProgressStyle::Indeterminate => (UNKNOWN, UNKNOWN),
        };
        ProgressState {
            style,
            items: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            total_items: AtomicU64::new(total_items),
            total_bytes: AtomicU64::new(total_bytes),
            detail: Mutex::new(None),
            epoch: AtomicU64::new(0),
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn detail(&self) -> Option<String> {
        lock(&self.detail).clone()
    }

    /// The published view (spec §4.5 `ProgressView`).
    pub(crate) fn view(&self) -> ProgressView {
        let total = |raw: u64| (raw != UNKNOWN).then_some(raw);
        let pair = |done: u64, raw_total: u64| total(raw_total).map(|t| (done.min(t), t));
        let frac = |done: u64, raw_total: u64| match total(raw_total) {
            Some(t) if t > 0 => {
                #[allow(clippy::cast_precision_loss)]
                let f = (done.min(t) as f64 / t as f64) as f32;
                Some(f.clamp(0.0, 1.0))
            }
            // A known-zero total is trivially complete (an import that
            // discovered nothing).
            Some(_) => Some(1.0),
            None => None,
        };
        match self.style {
            ProgressStyle::Indeterminate => ProgressView {
                fraction: None,
                items: None,
                bytes: None,
            },
            ProgressStyle::Items { .. } => {
                let done = self.items.load(Ordering::Acquire);
                let t = self.total_items.load(Ordering::Acquire);
                ProgressView {
                    fraction: frac(done, t),
                    items: pair(done, t),
                    bytes: None,
                }
            }
            ProgressStyle::Bytes { .. } => {
                let done = self.bytes.load(Ordering::Acquire);
                let t = self.total_bytes.load(Ordering::Acquire);
                ProgressView {
                    fraction: frac(done, t),
                    items: None,
                    bytes: pair(done, t),
                }
            }
        }
    }
}

/// Coalescing wake for the activity publisher: any progress write / state
/// change marks it; the publisher folds at most `snapshot_publish_hz` times
/// a second **only when marked** (spec §4.5 publication discipline).
pub(crate) struct Dirty {
    flag: AtomicBool,
    notify: Notify,
    closed: AtomicBool,
}

impl Dirty {
    pub(crate) fn new() -> Arc<Dirty> {
        Arc::new(Dirty {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        })
    }

    /// Marks dirty; wakes the publisher on the clean→dirty edge only.
    pub(crate) fn mark(&self) {
        if !self.flag.swap(true, Ordering::AcqRel) {
            self.notify.notify_waiters();
        }
    }

    /// Clears the flag, returning whether it was set.
    pub(crate) fn take(&self) -> bool {
        self.flag.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Retires the publisher (scheduler drop/shutdown).
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Resolves when marked or closed.
    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.flag.load(Ordering::Acquire) || self.is_closed() {
                return;
            }
            notified.await;
        }
    }
}

/// A job's progress reporter (spec §4.5). Hot path = one atomic add + one
/// epoch bump + a clean→dirty edge check; safe to call at arbitrary rates
/// (1 M `advance` calls are budgeted < 10 ms, T7).
#[derive(Clone)]
pub struct ProgressSink {
    state: Arc<ProgressState>,
    dirty: Arc<Dirty>,
}

impl ProgressSink {
    pub(crate) fn new(state: Arc<ProgressState>, dirty: Arc<Dirty>) -> ProgressSink {
        ProgressSink { state, dirty }
    }

    /// Adds completed items (for [`ProgressStyle::Items`] jobs).
    pub fn advance(&self, items: u64) {
        self.state.items.fetch_add(items, Ordering::AcqRel);
        self.touch();
    }

    /// Adds completed bytes (for [`ProgressStyle::Bytes`] jobs).
    pub fn advance_bytes(&self, bytes: u64) {
        self.state.bytes.fetch_add(bytes, Ordering::AcqRel);
        self.touch();
    }

    /// Sets (or grows — import discovery) the total for the job's progress
    /// dimension. Ignored for indeterminate jobs.
    pub fn set_total(&self, total: u64) {
        match self.state.style {
            ProgressStyle::Items { .. } => {
                self.state.total_items.store(total, Ordering::Release);
            }
            ProgressStyle::Bytes { .. } => {
                self.state.total_bytes.store(total, Ordering::Release);
            }
            ProgressStyle::Indeterminate => {
                tracing::trace!(
                    target: "lightbox_jobs",
                    "set_total on an indeterminate job ignored"
                );
                return;
            }
        }
        self.touch();
    }

    /// Sets the human-readable detail line (e.g. the current filename).
    /// Publication is throttled by the snapshot publisher; call freely.
    pub fn set_detail(&self, detail: impl Into<String>) {
        *lock(&self.state.detail) = Some(detail.into());
        self.touch();
    }

    fn touch(&self) {
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.dirty.mark();
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressSink")
            .field("view", &self.state.view())
            .finish_non_exhaustive()
    }
}

/// The published shape of one entry's progress (spec §4.5).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ProgressView {
    /// `[0, 1]` when a total is known; `None` = indeterminate.
    pub fraction: Option<f32>,
    /// `(done, total)` items, when item-styled with a known total.
    pub items: Option<(u64, u64)>,
    /// `(done, total)` bytes, when byte-styled with a known total.
    pub bytes: Option<(u64, u64)>,
}
