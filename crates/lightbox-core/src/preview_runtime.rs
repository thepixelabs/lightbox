// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`JobsBuildRuntime`] — the [`lightbox_preview::BuildRuntime`] seam,
//! backed by the E06 job scheduler (E06 spec T15; E03 spec §5.6's named
//! M1 upgrade: "`lightbox-jobs` `Class::Background` adapter (pausable,
//! activity-center visible)").
//!
//! **This replaces the M0 `TokioBuildRuntime`** (raw
//! `Handle::spawn_blocking`, no class budget, invisible to any activity
//! surface). Drop-in by construction: the seam trait is unchanged, the
//! preview scheduler (E03 Phase D) still owns build ordering
//! (Visible > Neighbor > Bulk), keying (`BuildKey` dedupe), and its own
//! `concurrency()` bound — this adapter adds what E06 owns:
//!
//! - builds run as **`Class::Background` jobs** (budgeted, load-shed by
//!   interactive activity, class-pausable — "pause all background
//!   analysis" now reaches preview extraction);
//! - one **rolling "Extracting previews" group** aggregates a burst of
//!   builds into a single activity entry with live (done/total) progress:
//!   the group opens on the first build of a burst and closes when the
//!   burst drains, so it completes, lingers, and ages out of the snapshot;
//! - `cancel_group` / class-pause reach in-flight extraction through the
//!   ordinary job surface (`Command::Jobs`).
//!
//! The build future still executes on the **blocking pool** (via
//! `JobContext::io`): `producer::ensure_t0/t1` are synchronous file/CPU
//! work with no internal await points (see `BuildFuture`'s doc comment) —
//! the no-op-waker poll-to-completion loop is unchanged from the M0 impl.
//!
//! **Grid-visibility priority hooks (T15):** at the jobs layer the exposed
//! function is `Scheduler::set_priority_by_key` (E08 binds it later); on
//! THIS path visible-first ordering already happens upstream in the
//! preview scheduler before a build ever reaches `spawn_build`, so the
//! member jobs are unkeyed — see `E06-deviations.md`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use lightbox_jobs::{Class, GroupHandle, GroupSpec, JobSpec, Scheduler};
use lightbox_preview::{BuildFuture, BuildRuntime};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Backs [`lightbox_preview::PreviewService`]'s [`BuildRuntime`] seam on
/// the E06 scheduler.
pub(crate) struct JobsBuildRuntime {
    sched: Arc<Scheduler>,
    concurrency: usize,
    /// The rolling burst group (see module docs).
    burst: Arc<Mutex<Burst>>,
}

#[derive(Default)]
struct Burst {
    group: Option<GroupHandle>,
    live: usize,
}

/// Decrements the burst on any exit path (completion, cancellation before
/// dispatch — the job closure is dropped unrun —, or panic) and closes the
/// group when the burst drains so its activity entry completes.
struct BurstGuard {
    burst: Arc<Mutex<Burst>>,
}

impl Drop for BurstGuard {
    fn drop(&mut self) {
        let mut burst = lock(&self.burst);
        burst.live = burst.live.saturating_sub(1);
        if burst.live == 0 {
            if let Some(group) = burst.group.take() {
                group.close();
            }
        }
    }
}

impl JobsBuildRuntime {
    /// `sched`: the session scheduler (`Session::jobs()`); `concurrency`:
    /// spec §5.7 `workers` resolution, computed by the caller
    /// (`session.rs`) — the preview scheduler treats it as its bound, and
    /// the Background lane budget bounds actual parallelism further.
    pub(crate) fn new(sched: Arc<Scheduler>, concurrency: usize) -> Arc<JobsBuildRuntime> {
        Arc::new(JobsBuildRuntime {
            sched,
            concurrency: concurrency.max(1),
            burst: Arc::new(Mutex::new(Burst::default())),
        })
    }
}

impl BuildRuntime for JobsBuildRuntime {
    fn spawn_build(&self, name: &'static str, mut fut: BuildFuture) {
        // Join (or open) the rolling burst group.
        let group_id = {
            let mut burst = lock(&self.burst);
            if burst.group.is_none() {
                burst.group = Some(self.sched.create_group(GroupSpec::new(
                    "preview.extract",
                    "Extracting previews",
                    Class::Background,
                )));
            }
            burst.live += 1;
            burst.group.as_ref().map(GroupHandle::id)
        };
        let guard = BurstGuard {
            burst: Arc::clone(&self.burst),
        };

        let mut spec = JobSpec::new(name, "Extracting preview", Class::Background);
        spec.group = group_id;
        let handle = self.sched.spawn::<(), _, _>(spec, move |ctx| async move {
            let _guard = guard;
            // Cancel-aware entry; the future itself carries the preview
            // scheduler's own CancelToken internally (E03 Phase D), so a
            // job-level cancel between here and completion is observed at
            // the builder's own checkpoints too.
            ctx.checkpoint().await?;
            // Blocking pool, not a tokio worker: the future wraps one
            // synchronous producer call (no internal awaits) — poll it to
            // completion with a no-op waker there (same loop as the M0
            // TokioBuildRuntime this adapter replaces).
            ctx.io(move || {
                let waker = Waker::noop();
                let mut cx = Context::from_waker(waker);
                loop {
                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(()) => break,
                        Poll::Pending => std::thread::yield_now(),
                    }
                }
            })
            .await?;
            Ok(())
        });
        drop(handle); // detached: the group entry is the observer
    }

    fn concurrency(&self) -> usize {
        self.concurrency
    }
}

impl std::fmt::Debug for JobsBuildRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsBuildRuntime")
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}
