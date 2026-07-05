// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`JobSystem`] — the cancellable job system seed (spec §3.3, E01 T14).
//!
//! A tokio multi-thread runtime with **separate semaphore budgets per
//! [`Class`]**: Interactive work never queues behind Foreground or Background
//! work because each class draws from its own permit pool (M0 semantics;
//! priority preemption and pause/resume are E06).
//!
//! Cancellation is hierarchical and cooperative via [`CancelToken`]:
//! async jobs are additionally raced against their token at every await
//! point (a cancel resolves the job as [`JobError::Cancelled`] without
//! waiting for the next explicit checkpoint); blocking jobs receive
//! `&CancelToken` and must checkpoint themselves (spec T14 budgets 50 ms
//! between checkpoints).
//!
//! Pipelines between job stages use ordinary **bounded** `tokio::sync::mpsc`
//! channels — the seed deliberately adds no channel wrapper of its own.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use tokio::sync::Semaphore;

use crate::cancel::CancelToken;

/// §5.3 job classes (spec §3.3, frozen surface for E06).
///
/// M0 semantics: separate semaphore budgets per class — Interactive never
/// queues behind Background. Preemption/pause is E06.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    /// Blocks what the user is looking at *right now* (loupe render source,
    /// visible thumbnail). Generous budget; jobs must be short.
    Interactive,
    /// User-initiated bulk work with visible progress (imports, exports).
    Foreground,
    /// Opportunistic work nobody is waiting for (cache warming, prefetch).
    Background,
}

impl Class {
    const ALL: [Class; 3] = [Class::Interactive, Class::Foreground, Class::Background];

    fn index(self) -> usize {
        match self {
            Class::Interactive => 0,
            Class::Foreground => 1,
            Class::Background => 2,
        }
    }
}

/// Sizing knobs for [`JobSystem::new`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct JobConfig {
    /// Worker threads for the tokio runtime. `None` = tokio's default
    /// (one per core).
    pub worker_threads: Option<usize>,
    /// Concurrent [`Class::Interactive`] jobs ("unbounded-small": a generous
    /// cap that interactive work at M0 scale never hits).
    pub interactive_slots: usize,
    /// Concurrent [`Class::Foreground`] jobs.
    pub foreground_slots: usize,
    /// Concurrent [`Class::Background`] jobs.
    pub background_slots: usize,
}

impl Default for JobConfig {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4);
        JobConfig {
            worker_threads: None,
            interactive_slots: (2 * cores).max(8),
            foreground_slots: (cores / 2).max(2),
            background_slots: (cores / 4).max(1),
        }
    }
}

/// Why a job did not produce its value. `Cancelled` is a **normal outcome**
/// (spec §3.3), not an incident.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JobError {
    /// The job's [`CancelToken`] was cancelled (before or during the run).
    #[error("job cancelled")]
    Cancelled,
    /// The job body failed. Carries the body's own error message — job
    /// domains keep their typed errors internal and stringify at this seam.
    #[error("{0}")]
    Failed(String),
    /// The job body panicked. The system survives; the panic is contained.
    #[error("job panicked: {0}")]
    Panicked(String),
    /// The job system shut down before the job completed.
    #[error("job system shut down before the job completed")]
    Shutdown,
    /// `join` was called after `try_result` already yielded the result.
    #[error("job result was already taken via try_result")]
    ResultTaken,
}

/// The job system seed (spec §3.3): tokio multi-thread runtime + per-class
/// budgets. E06 grows this into priority preemption, pause/resume, and the
/// activity-center model — `Class`, `CancelToken`, and the `spawn` signatures
/// below are its frozen starting point.
pub struct JobSystem {
    /// `Option` so `Drop` can `shutdown_background()` (a plain runtime drop
    /// panics inside async contexts, e.g. `#[tokio::test]`).
    runtime: Option<tokio::runtime::Runtime>,
    semaphores: [Arc<Semaphore>; 3],
    running: [Arc<AtomicUsize>; 3],
    next_id: AtomicU64,
}

impl JobSystem {
    /// Builds the runtime and the per-class budgets.
    ///
    /// # Panics
    ///
    /// If the tokio runtime cannot be built (worker threads cannot be
    /// spawned) — a startup-fatal condition; the spec's frozen signature
    /// (§3.3) has no error channel.
    pub fn new(cfg: JobConfig) -> JobSystem {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all().thread_name("lightbox-jobs");
        if let Some(n) = cfg.worker_threads {
            builder.worker_threads(n);
        }
        let runtime = builder.build().expect("failed to build the job runtime");
        let slots = |n: usize| Arc::new(Semaphore::new(n.max(1)));
        tracing::debug!(
            target: "lightbox_jobs",
            interactive = cfg.interactive_slots,
            foreground = cfg.foreground_slots,
            background = cfg.background_slots,
            "job system started"
        );
        JobSystem {
            runtime: Some(runtime),
            semaphores: [
                slots(cfg.interactive_slots),
                slots(cfg.foreground_slots),
                slots(cfg.background_slots),
            ],
            running: std::array::from_fn(|_| Arc::new(AtomicUsize::new(0))),
            next_id: AtomicU64::new(1),
        }
    }

    /// Spawns an async job under `class`'s budget (spec §3.3).
    ///
    /// The job is raced against `cancel` while waiting for a budget slot
    /// *and* while running: cancellation resolves the job as
    /// [`JobError::Cancelled`] at the next await point (the future is
    /// dropped). Bodies that must not be dropped mid-step should checkpoint
    /// on the token themselves.
    pub fn spawn<T: Send + 'static>(
        &self,
        class: Class,
        name: &'static str,
        cancel: CancelToken,
        fut: impl Future<Output = Result<T, JobError>> + Send + 'static,
    ) -> JobHandle<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let semaphore = Arc::clone(&self.semaphores[class.index()]);
        let running = Arc::clone(&self.running[class.index()]);
        let token = cancel.clone();
        let work = async move {
            let _permit = tokio::select! {
                _ = token.cancelled() => return Err(JobError::Cancelled),
                permit = semaphore.acquire_owned() => {
                    permit.map_err(|_| JobError::Shutdown)?
                }
            };
            let _running = RunningGuard::enter(&running);
            tracing::trace!(target: "lightbox_jobs", job = name, id, ?class, "job running");
            tokio::select! {
                _ = token.cancelled() => Err(JobError::Cancelled),
                out = fut => out,
            }
        };
        let join = self.handle().spawn(work);
        JobHandle {
            id,
            name,
            cancel,
            join: Some(join),
        }
    }

    /// Spawns sync work (hashing, decode, SQLite) on the blocking pool under
    /// `class`'s budget — same handle shape (spec §3.3).
    ///
    /// Cancellation while queued resolves as [`JobError::Cancelled`] without
    /// running `f`; once running, `f` must checkpoint on the token it is
    /// handed (cooperatively, ≤ 50 ms between checkpoints per spec T14).
    /// Panics in `f` are contained as [`JobError::Panicked`].
    pub fn spawn_blocking<T: Send + 'static>(
        &self,
        class: Class,
        name: &'static str,
        cancel: CancelToken,
        f: impl FnOnce(&CancelToken) -> Result<T, JobError> + Send + 'static,
    ) -> JobHandle<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let semaphore = Arc::clone(&self.semaphores[class.index()]);
        let running = Arc::clone(&self.running[class.index()]);
        let token = cancel.clone();
        let work = async move {
            let _permit = tokio::select! {
                _ = token.cancelled() => return Err(JobError::Cancelled),
                permit = semaphore.acquire_owned() => {
                    permit.map_err(|_| JobError::Shutdown)?
                }
            };
            if token.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let _running = RunningGuard::enter(&running);
            tracing::trace!(target: "lightbox_jobs", job = name, id, ?class, "blocking job running");
            let body_token = token.clone();
            let joined = tokio::task::spawn_blocking(move || {
                std::panic::catch_unwind(AssertUnwindSafe(|| f(&body_token)))
            })
            .await;
            match joined {
                Ok(Ok(result)) => result,
                Ok(Err(panic)) => Err(JobError::Panicked(panic_message(&*panic))),
                Err(join_err) if join_err.is_panic() => {
                    Err(JobError::Panicked(join_err.to_string()))
                }
                Err(_) => Err(JobError::Shutdown),
            }
        };
        let join = self.handle().spawn(work);
        JobHandle {
            id,
            name,
            cancel,
            join: Some(join),
        }
    }

    /// The runtime handle, for long-lived system tasks (the core's command
    /// dispatcher, event pumps) that must not consume a class budget slot.
    /// Additive convenience — not part of the frozen §3.3 surface.
    pub fn handle(&self) -> &tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("runtime present until drop")
            .handle()
    }

    /// Jobs of `class` currently *executing* (not queued). Diagnostics/tests.
    pub fn running(&self, class: Class) -> usize {
        self.running[class.index()].load(Ordering::Acquire)
    }

    /// Sum of [`JobSystem::running`] over all classes.
    pub fn running_total(&self) -> usize {
        Class::ALL.iter().map(|c| self.running(*c)).sum()
    }
}

impl Drop for JobSystem {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // Non-blocking shutdown: in-flight blocking work is detached, not
            // joined. Orderly teardown of user-visible work is the session's
            // responsibility (close cancels + drains), not the pool's.
            runtime.shutdown_background();
        }
    }
}

impl std::fmt::Debug for JobSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSystem")
            .field("running_interactive", &self.running(Class::Interactive))
            .field("running_foreground", &self.running(Class::Foreground))
            .field("running_background", &self.running(Class::Background))
            .finish_non_exhaustive()
    }
}

/// RAII increment of a per-class running counter.
struct RunningGuard {
    counter: Arc<AtomicUsize>,
}

impl RunningGuard {
    fn enter(counter: &Arc<AtomicUsize>) -> RunningGuard {
        counter.fetch_add(1, Ordering::AcqRel);
        RunningGuard {
            counter: Arc::clone(counter),
        }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Handle to a spawned job (spec §3.3): ticket id, join handle, cancel token.
pub struct JobHandle<T> {
    id: u64,
    name: &'static str,
    cancel: CancelToken,
    /// `None` once the result was taken via [`JobHandle::try_result`].
    join: Option<tokio::task::JoinHandle<Result<T, JobError>>>,
}

impl<T> JobHandle<T> {
    /// Requests cancellation (cooperative; see [`CancelToken`]). The job
    /// resolves as [`JobError::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// This job's cancel token (e.g. to derive children for sub-stages).
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Ticket id, unique within this [`JobSystem`].
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The static name the job was spawned with.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Waits for the job. [`JobError::Cancelled`] is a normal outcome
    /// (spec §3.3). Await-able from any async context (or via a lightweight
    /// executor in sync code).
    pub async fn join(mut self) -> Result<T, JobError> {
        match self.join.take() {
            Some(join) => flatten_join(join.await),
            None => Err(JobError::ResultTaken),
        }
    }

    /// Non-blocking poll for the UI (spec §3.3): `None` while the job runs,
    /// `Some(result)` exactly once when finished (subsequent calls return
    /// `None`). Never blocks, never yields.
    pub fn try_result(&mut self) -> Option<Result<T, JobError>> {
        let join = self.join.as_mut()?;
        if !join.is_finished() {
            return None;
        }
        let mut cx = Context::from_waker(Waker::noop());
        match Pin::new(join).poll(&mut cx) {
            Poll::Ready(out) => {
                self.join = None;
                Some(flatten_join(out))
            }
            Poll::Pending => None,
        }
    }

    /// True once the job has finished (result may still be un-taken).
    pub fn is_finished(&self) -> bool {
        self.join
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }
}

impl<T> std::fmt::Debug for JobHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

fn flatten_join<T>(
    out: Result<Result<T, JobError>, tokio::task::JoinError>,
) -> Result<T, JobError> {
    match out {
        Ok(result) => result,
        Err(join_err) if join_err.is_panic() => Err(JobError::Panicked(join_err.to_string())),
        Err(_) => Err(JobError::Shutdown),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn system() -> JobSystem {
        JobSystem::new(JobConfig::default())
    }

    #[test]
    fn spawn_runs_to_completion_and_join_returns_value() {
        let jobs = system();
        let handle = jobs.spawn(Class::Foreground, "answer", CancelToken::new(), async {
            Ok(41 + 1)
        });
        let out = pollster::block_on(handle.join());
        assert!(matches!(out, Ok(42)));
    }

    #[test]
    fn spawn_blocking_runs_on_the_pool_and_returns_value() {
        let jobs = system();
        let handle = jobs.spawn_blocking(
            Class::Background,
            "blocking-answer",
            CancelToken::new(),
            |_cancel| Ok::<_, JobError>("done".to_owned()),
        );
        assert_eq!(pollster::block_on(handle.join()).unwrap(), "done");
    }

    /// T14 AC: a cancelled blocking job observes the token within 50 ms
    /// given cooperative checkpoints.
    #[test]
    fn cancelled_blocking_job_observes_token_within_50ms() {
        let jobs = system();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let cancel = CancelToken::new();
        // The closure never returns Ok, so T needs naming.
        let handle: JobHandle<()> = jobs.spawn_blocking(
            Class::Foreground,
            "cooperative-loop",
            cancel.clone(),
            move |token| {
                let _ = started_tx.send(());
                loop {
                    if token.is_cancelled() {
                        return Err(JobError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            },
        );
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("job started");
        let cancelled_at = Instant::now();
        cancel.cancel();
        let out = pollster::block_on(handle.join());
        let observed_in = cancelled_at.elapsed();
        assert!(matches!(out, Err(JobError::Cancelled)), "{out:?}");
        assert!(
            observed_in <= Duration::from_millis(50),
            "cancel observed after {observed_in:?} (budget 50 ms)"
        );
    }

    /// T14 AC: Background saturation does not delay an Interactive spawn.
    #[test]
    fn background_saturation_does_not_delay_interactive() {
        let jobs = JobSystem::new(JobConfig {
            background_slots: 2,
            ..JobConfig::default()
        });
        let release = CancelToken::new();
        // Saturate Background (2 running + 2 queued on the semaphore).
        let hogs: Vec<_> = (0..4)
            .map(|_| {
                let parked = release.clone();
                jobs.spawn(Class::Background, "hog", CancelToken::new(), async move {
                    parked.cancelled().await;
                    Ok(())
                })
            })
            .collect();

        // Probe timer: the Interactive job must start promptly regardless.
        let spawned_at = Instant::now();
        let interactive = jobs.spawn(
            Class::Interactive,
            "probe",
            CancelToken::new(),
            async move { Ok(spawned_at.elapsed()) },
        );
        let latency = pollster::block_on(interactive.join()).unwrap();
        assert!(
            latency < Duration::from_millis(200),
            "interactive job waited {latency:?} behind background saturation"
        );

        release.cancel(); // un-park the hogs
        for hog in hogs {
            let _ = pollster::block_on(hog.join());
        }
    }

    /// T14 AC: `try_result` never blocks.
    #[test]
    fn try_result_never_blocks_and_yields_exactly_once() {
        let jobs = system();
        let gate = CancelToken::new();
        let open = gate.clone();
        let mut handle = jobs.spawn(Class::Foreground, "gated", CancelToken::new(), async move {
            open.cancelled().await;
            Ok(7)
        });

        // Running: must return None, and fast.
        let t0 = Instant::now();
        assert!(handle.try_result().is_none());
        assert!(t0.elapsed() < Duration::from_millis(50));

        gate.cancel(); // let the job finish
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(r) = handle.try_result() {
                break r;
            }
            assert!(Instant::now() < deadline, "job never finished");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(matches!(result, Ok(7)));
        // Taken: subsequent polls are None; join reports ResultTaken.
        assert!(handle.try_result().is_none());
        assert!(matches!(
            pollster::block_on(handle.join()),
            Err(JobError::ResultTaken)
        ));
    }

    #[test]
    fn cancel_before_run_skips_the_body() {
        let jobs = JobSystem::new(JobConfig {
            background_slots: 1,
            ..JobConfig::default()
        });
        let park = CancelToken::new();
        let parked = park.clone();
        let hog = jobs.spawn(Class::Background, "hog", CancelToken::new(), async move {
            parked.cancelled().await;
            Ok(())
        });

        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = Arc::clone(&ran);
        let cancel = CancelToken::new();
        let queued = jobs.spawn_blocking(
            Class::Background,
            "queued",
            cancel.clone(),
            move |_| -> Result<(), JobError> {
                ran2.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        cancel.cancel(); // while still waiting for the (occupied) slot
        let out = pollster::block_on(queued.join());
        assert!(matches!(out, Err(JobError::Cancelled)), "{out:?}");
        assert_eq!(ran.load(Ordering::SeqCst), 0, "cancelled job body ran");

        park.cancel();
        let _ = pollster::block_on(hog.join());
    }

    #[test]
    fn panicking_blocking_job_is_contained() {
        let jobs = system();
        let handle = jobs.spawn_blocking(
            Class::Foreground,
            "panicker",
            CancelToken::new(),
            |_| -> Result<(), JobError> { panic!("boom") },
        );
        let out = pollster::block_on(handle.join());
        match out {
            Err(JobError::Panicked(msg)) => assert!(msg.contains("boom"), "{msg}"),
            other => panic!("expected Panicked, got {other:?}"),
        }
        // The system survives and runs new jobs.
        let next = jobs.spawn(Class::Foreground, "after", CancelToken::new(), async {
            Ok(1)
        });
        assert!(matches!(pollster::block_on(next.join()), Ok(1)));
    }

    #[test]
    fn async_job_is_cancelled_at_await_points() {
        let jobs = system();
        let cancel = CancelToken::new();
        let handle = jobs.spawn(Class::Foreground, "sleeper", cancel.clone(), async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        });
        cancel.cancel();
        let started = Instant::now();
        let out = pollster::block_on(handle.join());
        assert!(matches!(out, Err(JobError::Cancelled)), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn running_counters_return_to_zero() {
        let jobs = system();
        let gate = CancelToken::new();
        let open = gate.clone();
        let handle = jobs.spawn(
            Class::Interactive,
            "counted",
            CancelToken::new(),
            async move {
                open.cancelled().await;
                Ok(())
            },
        );
        // Wait until it is actually running.
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.running(Class::Interactive) == 0 {
            assert!(Instant::now() < deadline, "job never started");
            std::thread::sleep(Duration::from_millis(1));
        }
        gate.cancel();
        let _ = pollster::block_on(handle.join());
        let deadline = Instant::now() + Duration::from_secs(5);
        while jobs.running_total() != 0 {
            assert!(Instant::now() < deadline, "running counter stuck");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
