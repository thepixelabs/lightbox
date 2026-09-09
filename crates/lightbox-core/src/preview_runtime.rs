// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`TokioBuildRuntime`], the M0 [`lightbox_preview::BuildRuntime`] seam
//! (E03 spec §5.6, Phase D T14): "M0: `lightbox-core` backs this with plain
//! tokio... M1: `lightbox-jobs` `Class::Background` adapter (pausable,
//! activity-center visible)."
//!
//! **"Plain tokio", read literally.** `Session` already owns a
//! `lightbox_jobs::JobSystem` (a `Class`-budgeted tokio runtime) and could
//! have routed preview builds through `JobSystem::spawn_blocking(Class::
//! Background..)` today, pulling M1's adapter forward. This impl
//! deliberately does NOT do that: it reaches straight for
//! `tokio::task::spawn_blocking` on the SAME runtime handle
//! (`JobSystem::handle()`, already exposed for exactly this kind of
//! long-lived system task, see `session.rs`'s `dispatch_loop`/device-degraded
//! relay), with no `Class` semaphore budget involved. This keeps the M0/M1
//! boundary the spec draws honest: `lightbox-jobs`' pause/activity-center
//! upgrade is real, future, Phase-E06 work, not something silently already
//! done. Concurrency is instead capped by the scheduler itself
//! ([`lightbox_preview::BuildRuntime::concurrency`], see below), which is
//! the seam builds actually respect.
//!
//! **Builds run on the blocking pool, not an async worker thread.**
//! `producer::ensure_t0`/`ensure_t1` (Phase C) are synchronous file/CPU work
//! with no internal `.await` points, running them directly on a tokio
//! multi-thread runtime's worker threads would starve other async tasks
//! (the command dispatcher, the event bus) the moment more than a
//! couple builds overlap. `spawn_blocking` moves them to tokio's dedicated
//! blocking-task pool instead, the same pattern already used everywhere else
//! in this crate for catalog-writer/edit-store work (`session.rs`'s
//! `run_txn_command`, `run_edit_command`).
//!
//! [`lightbox_preview::sched::BuildFuture`] carries no internal `.await`
//! either (it wraps one synchronous producer call): a no-op-waker
//! poll-to-completion loop (the same pattern `lightbox_jobs::JobHandle::
//! try_result` already uses) drives it inside the blocking closure, no
//! extra executor dependency (e.g. `pollster`) needed.

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use lightbox_preview::{BuildFuture, BuildRuntime};

/// Backs [`lightbox_preview::PreviewService`]'s [`BuildRuntime`] seam at M0.
pub(crate) struct TokioBuildRuntime {
    handle: tokio::runtime::Handle,
    concurrency: usize,
}

impl TokioBuildRuntime {
    /// `handle`: `JobSystem::handle()` (the same tokio runtime the rest of
    /// the session's long-lived tasks already run on, no second runtime is
    /// created). `concurrency`: spec §5.7 `workers` resolution
    /// (`min(physical_cores, config)`), computed by the caller
    /// (`session.rs`) since `CoreConfig`/`PreviewStoreConfig` are its own to
    /// read.
    pub(crate) fn new(
        handle: tokio::runtime::Handle,
        concurrency: usize,
    ) -> Arc<TokioBuildRuntime> {
        Arc::new(TokioBuildRuntime {
            handle,
            concurrency: concurrency.max(1),
        })
    }
}

impl BuildRuntime for TokioBuildRuntime {
    fn spawn_build(&self, name: &'static str, mut fut: BuildFuture) {
        self.handle.spawn_blocking(move || {
            // The future is `Send + 'static` and never actually awaits
            // anything (see the module doc comment), poll it to completion
            // with a no-op waker rather than pulling in a separate
            // mini-executor dependency for a loop that in practice runs
            // exactly once.
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);
            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(()) => break,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        });
        let _ = name; // diagnostic label only; no task registry to name here at M0.
    }

    fn concurrency(&self) -> usize {
        self.concurrency
    }
}
