// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Scheduler`] — class-prioritized, pausable, cancellable job dispatch
//! (E06 spec §4.3/§4.4).
//!
//! Three lanes (Interactive / Foreground / Background), each a
//! `BinaryHeap` priority queue with **generation-stamped lazy entries**
//! (re-prioritization pushes a fresh entry and bumps the record's
//! generation; stale entries are skipped at pop — spec T4), a claimed-slot
//! budget, and one dispatch task. CPU-heavy work never runs on the tokio
//! workers ([`JobContext::cpu`] routes to the class-matched rayon pool,
//! `cpu.rs`).
//!
//! **No in-flight preemption** (spec §4.4): preemption is admission-control
//! granular plus cooperative yielding. Budget shrink stops issuing slots —
//! it never aborts a running job.
//!
//! Every state change routes through the pure transition table
//! ([`crate::model::transition`]); there is no second copy of the rules.

use std::collections::{BinaryHeap, HashMap};
use std::future::Future;
use std::num::NonZeroU64;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use tokio::sync::Notify;
use tracing::Instrument;

use crate::config::JobsConfig;
use crate::cpu::{CpuOutcome, CpuPools};
use crate::model::{
    transition, Class, FineState, Input, JobId, JobKey, JobSpec, JobState, Outcome, Priority,
};
use crate::system::JobError;
use crate::token::{Interrupted, PauseGate, BIT_SHOULD_YIELD};

/// Poison-tolerant lock (house convention: a poisoned scheduler lock means a
/// panic already happened elsewhere; the state itself is a plain value).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A type-erased job body: consumes the [`JobContext`], yields the wrapper
/// future the dispatcher spawns (result delivery + panic containment are
/// inside).
type Work = Box<dyn FnOnce(JobContext) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// One lane-queue entry. **Lazy invalidation** (spec T4): entries are never
/// removed in place — re-prioritization/cancel/pause bumps the record's
/// generation or state, and stale entries are skipped at pop.
struct HeapEntry {
    prio: Priority,
    /// Spawn sequence — FIFO tiebreak within a priority (smaller = earlier).
    seq: u64,
    /// Must match the record's current generation to be live.
    gen: u64,
    id: JobId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.prio == other.prio && self.seq == other.seq
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap: higher priority first; then FIFO (smaller seq first).
        self.prio
            .cmp(&other.prio)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// One scheduling lane (spec §4.4): priority queue + slot budget + one
/// dispatch task. `Arc`-shared with its dispatch task so the task can park
/// on `notify` without keeping the whole [`Scheduler`] alive.
pub(crate) struct Lane {
    pub(crate) class: Class,
    queue: Mutex<BinaryHeap<HeapEntry>>,
    /// Jobs currently in `Queued` fine-state (not heap entries — stale
    /// entries don't count). Drives the Foreground-backlog shrink signal
    /// and metrics.
    queued: AtomicUsize,
    /// Slots currently claimed (running jobs + resume claims).
    running: AtomicUsize,
    /// Wakes the dispatch task and paused jobs waiting to re-claim a slot.
    notify: Notify,
    /// Set on scheduler drop/shutdown; parked tasks exit.
    closed: AtomicBool,
}

impl Lane {
    fn new(class: Class) -> Arc<Lane> {
        Arc::new(Lane {
            class,
            queue: Mutex::new(BinaryHeap::new()),
            queued: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        })
    }

    /// Claims a slot iff `running < budget` (CAS loop — resume-waiters and
    /// the dispatcher race for freed slots; over-admission is impossible).
    fn try_claim(&self, budget: usize) -> bool {
        let mut cur = self.running.load(Ordering::Acquire);
        loop {
            if cur >= budget {
                return false;
            }
            match self.running.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(now) => cur = now,
            }
        }
    }

    /// Releases a slot and wakes the dispatcher + any resume-waiters.
    fn release_slot(&self) {
        self.running.fetch_sub(1, Ordering::AcqRel);
        self.notify.notify_waiters();
    }

    /// Releases a claim without waking anyone (the dispatcher claimed but
    /// found no runnable work).
    fn release_claim_quiet(&self) {
        self.running.fetch_sub(1, Ordering::AcqRel);
    }

    /// Jobs currently in `Queued` state.
    pub(crate) fn queued_len(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    /// Slots currently claimed.
    pub(crate) fn running_len(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

/// The registry-held, type-erased record of one job. Handles hold it
/// directly (an `Arc`), so registry eviction never invalidates a handle.
pub(crate) struct JobRecord {
    pub(crate) id: JobId,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) class: Class,
    pub(crate) pausable: bool,
    pub(crate) group: Option<crate::model::GroupId>,
    pub(crate) key: Option<JobKey>,
    pub(crate) started_at: SystemTime,
    /// Fine-grained state — every change goes through
    /// [`crate::model::transition`] under this lock.
    pub(crate) state: Mutex<FineState>,
    /// Current within-class priority (re-assignable while queued).
    priority: AtomicI32,
    /// Heap-entry generation (lazy invalidation).
    gen: AtomicU64,
    /// Spawn sequence (FIFO tiebreak).
    seq: u64,
    pub(crate) cancel: crate::CancelToken,
    pub(crate) gate: PauseGate,
    /// User-facing failure summary, set when the job fails.
    pub(crate) error: Mutex<Option<String>>,
    /// The body, present until dispatch (cancel-while-queued drops it
    /// without ever spawning a task).
    work: Mutex<Option<Work>>,
    /// Abort handle for the wrapper task (shutdown stragglers only).
    abort: Mutex<Option<tokio::task::AbortHandle>>,
    /// True while the job owns a lane slot (dispatch → pause-release →
    /// resume-reclaim → final release).
    holds_slot: AtomicBool,
    /// Woken on terminal transition (joiners).
    finished: Notify,
    /// When the job reached `Done(_)` (linger/eviction clock).
    pub(crate) terminal_at: Mutex<Option<tokio::time::Instant>>,
    /// Failed-entry dismissal (`JobCommand::Dismiss`).
    pub(crate) dismissed: AtomicBool,
    /// Millis since the scheduler epoch of the body's last checkpoint
    /// (watchdog input; updated on the checkpoint fast path).
    pub(crate) last_checkpoint_ms: AtomicU64,
}

impl JobRecord {
    pub(crate) fn public_state(&self) -> JobState {
        lock(&self.state).public()
    }

    pub(crate) fn priority(&self) -> Priority {
        Priority(self.priority.load(Ordering::Acquire))
    }
}

/// Where a finished job's typed result lands. Shared by every handle to the
/// job (keyed dedupe hands out many).
struct ResultSlot<T> {
    value: Mutex<Option<Result<T, JobError>>>,
}

/// How a [`JobHandle`] extracts its result: plain handles *take* (no
/// `Clone` bound); keyed handles *clone* (bound enforced by
/// [`Scheduler::spawn_keyed`]).
type Extract<T> =
    Box<dyn FnOnce(&mut Option<Result<T, JobError>>) -> Option<Result<T, JobError>> + Send>;

/// Handle to a spawned job (spec §4.3). **Dropping the handle detaches**
/// (the job keeps running — activity-center jobs outlive their spawner);
/// explicit cancel is [`JobHandle::cancel`] or the core command surface.
pub struct JobHandle<T> {
    record: Arc<JobRecord>,
    slot: Arc<ResultSlot<T>>,
    extract: Option<Extract<T>>,
    sched: Weak<Scheduler>,
}

impl<T> JobHandle<T> {
    /// The job's id.
    pub fn id(&self) -> JobId {
        self.record.id
    }

    /// The job's current public state.
    pub fn state(&self) -> JobState {
        self.record.public_state()
    }

    /// Requests cooperative cancellation (idempotent; a no-op once
    /// terminal).
    pub fn cancel(&self) {
        match self.sched.upgrade() {
            Some(s) => s.cancel_record(&self.record),
            // Scheduler gone (shutdown/drop): the token still reaches a
            // still-running body.
            None => self.record.cancel.cancel(),
        }
    }

    /// The job's cancel token (derive children for sub-stages).
    pub fn cancel_token(&self) -> &crate::CancelToken {
        &self.record.cancel
    }

    /// Awaits the result. [`JobError::Cancelled`] is a normal outcome.
    ///
    /// For keyed handles ([`Scheduler::spawn_keyed`]) the result is cloned —
    /// every handle to the shared job resolves.
    pub async fn join(mut self) -> Result<T, JobError> {
        loop {
            // Notified-before-check so a terminal transition between the
            // check and the await cannot be missed.
            let notified = self.record.finished.notified();
            if lock(&self.record.state).is_terminal() {
                break;
            }
            notified.await;
        }
        let extract = self
            .extract
            .take()
            .expect("join consumes the handle; extract present");
        let taken = extract(&mut lock(&self.slot.value));
        match taken {
            Some(result) => result,
            // Empty slot on a terminal job: it never ran (cancelled while
            // queued, refused at admission, or aborted at shutdown).
            None => match lock(&self.record.state).public() {
                JobState::Done(Outcome::Cancelled) => Err(JobError::Cancelled),
                _ => Err(JobError::Shutdown),
            },
        }
    }
}

impl<T> std::fmt::Debug for JobHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.record.id)
            .field("kind", &self.record.kind)
            .field("state", &self.record.public_state())
            .finish_non_exhaustive()
    }
}

/// What a job body sees (spec §4.3). Clone-cheap.
#[derive(Clone)]
pub struct JobContext {
    record: Arc<JobRecord>,
    lane: Arc<Lane>,
    sched: Weak<Scheduler>,
}

impl JobContext {
    /// This job's id.
    pub fn job_id(&self) -> JobId {
        self.record.id
    }

    /// The job's cancel token (thread it into sub-stages / long kernels).
    pub fn cancel_token(&self) -> &crate::CancelToken {
        &self.record.cancel
    }

    /// True once cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.record.cancel.is_cancelled()
    }

    /// The load-shed hint (spec §4.4/T5): true while the scheduler would
    /// like this job to voluntarily finish its work quantum and re-queue.
    /// Advisory only.
    pub fn should_yield(&self) -> bool {
        self.record.gate.should_yield()
    }

    /// The one call every well-behaved job loops through (spec §4.2):
    /// yields while paused (releasing its worker slot for the duration),
    /// `Err(Interrupted)` when cancelled.
    ///
    /// **Checkpoint contract**: reach a cancel-aware await (`checkpoint()`,
    /// `stage` send/recv, a `cpu()`/`io()` boundary) at least every
    /// **100 ms** of wall time. The debug watchdog logs (and in test mode
    /// counts as fatal) any running job that goes > 1 s without one.
    ///
    /// Hot path (running, unpaused): one gate-word load + one cancel-flag
    /// load, no allocation, no await.
    pub async fn checkpoint(&self) -> Result<(), Interrupted> {
        self.note_checkpoint();
        let bits = self.record.gate.bits();
        if !PauseGate::word_paused(bits) {
            if self.record.cancel.is_cancelled() {
                return Err(Interrupted);
            }
            return Ok(());
        }
        self.checkpoint_park().await
    }

    /// Slow path: apply the pause at this checkpoint (spec §4.1 "takes
    /// effect at next checkpoint"), release the slot, park, re-claim.
    async fn checkpoint_park(&self) -> Result<(), Interrupted> {
        if self.record.cancel.is_cancelled() {
            return Err(Interrupted);
        }
        // Running → PausedRunning. If the state is already Cancelling the
        // cancel raced the pause: report interrupted without parking.
        {
            let mut st = lock(&self.record.state);
            match *st {
                FineState::Running => match transition(*st, Input::Pause) {
                    Ok(next) => *st = next,
                    Err(_) => return Err(Interrupted),
                },
                FineState::Cancelling => return Err(Interrupted),
                // Any other state here is a scheduler bug; fail safe by
                // treating it as an interrupt.
                _ => {
                    debug_assert!(false, "checkpoint in state {:?}", *st);
                    return Err(Interrupted);
                }
            }
        }
        if self.record.holds_slot.swap(false, Ordering::AcqRel) {
            self.lane.release_slot();
        }
        if let Some(s) = self.sched.upgrade() {
            s.note_state_changed(&self.record);
        }

        // Park until resumed; cancel-while-paused errors out of the wait.
        let wait = self
            .record
            .gate
            .wait_if_paused(&self.record.cancel)
            .await;
        if wait.is_err() {
            // Cancelled while paused. The control plane has already moved
            // the state to Cancelling (or will); do not re-claim a slot.
            return Err(Interrupted);
        }

        // Resumed: re-claim a slot before continuing (the budget still
        // holds), racing cancellation the whole time.
        loop {
            let budget = match self.sched.upgrade() {
                Some(s) => s.effective_budget(self.record.class).0,
                None => return Err(Interrupted), // scheduler gone
            };
            if self.lane.try_claim(budget) {
                break;
            }
            let notified = self.lane.notify.notified();
            if self.lane.try_claim(budget) {
                break;
            }
            if self.record.cancel.is_cancelled() {
                return Err(Interrupted);
            }
            tokio::select! {
                _ = self.record.cancel.cancelled() => return Err(Interrupted),
                _ = notified => {}
            }
        }
        self.record.holds_slot.store(true, Ordering::Release);

        // PausedRunning → Running (or the cancel won the race).
        {
            let mut st = lock(&self.record.state);
            match *st {
                FineState::PausedRunning => match transition(*st, Input::Resume) {
                    Ok(next) => *st = next,
                    Err(_) => return Err(Interrupted),
                },
                FineState::Cancelling => {
                    drop(st);
                    if self.record.holds_slot.swap(false, Ordering::AcqRel) {
                        self.lane.release_slot();
                    }
                    return Err(Interrupted);
                }
                _ => {
                    debug_assert!(false, "resume in state {:?}", *st);
                    return Err(Interrupted);
                }
            }
        }
        if let Some(s) = self.sched.upgrade() {
            s.note_state_changed(&self.record);
        }
        self.note_checkpoint();
        Ok(())
    }

    /// Runs a CPU-heavy closure on the class-matched rayon pool (spec §4.4,
    /// T6): Background jobs → `bg-cpu`, everything else → `fg-cpu`. Never
    /// executes on a tokio worker.
    ///
    /// Cancel-aware **at entry** (both before submission and before the
    /// closure actually starts on the pool); the closure itself should poll
    /// the token it is handed for long kernels. A panicking closure fails
    /// only this job (the panic is re-thrown here and contained by the
    /// job wrapper as [`JobError::Panicked`]); the pool worker survives.
    pub async fn cpu<R: Send + 'static>(
        &self,
        f: impl FnOnce(&crate::CancelToken) -> R + Send + 'static,
    ) -> Result<R, Interrupted> {
        self.note_checkpoint();
        if self.record.cancel.is_cancelled() {
            return Err(Interrupted);
        }
        let Some(s) = self.sched.upgrade() else {
            return Err(Interrupted);
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<CpuOutcome<R>>();
        let token = self.record.cancel.clone();
        s.cpu.pool(self.record.class).spawn(move || {
            if token.is_cancelled() {
                let _ = tx.send(CpuOutcome::Interrupted);
                return;
            }
            match std::panic::catch_unwind(AssertUnwindSafe(|| f(&token))) {
                Ok(value) => {
                    let _ = tx.send(CpuOutcome::Ok(value));
                }
                Err(panic) => {
                    let _ = tx.send(CpuOutcome::Panicked(panic));
                }
            }
        });
        drop(s);
        let out = match rx.await {
            Ok(out) => out,
            // Sender dropped without sending: pool torn down mid-flight.
            Err(_) => CpuOutcome::Interrupted,
        };
        self.note_checkpoint();
        match out {
            CpuOutcome::Ok(value) => Ok(value),
            CpuOutcome::Interrupted => Err(Interrupted),
            // Re-throw on the job task: the wrapper's catch_unwind turns it
            // into JobError::Panicked for THIS job only.
            CpuOutcome::Panicked(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Runs blocking I/O (file copy, checksum read) on tokio's blocking
    /// pool (spec §4.3, T6). Cancel-aware at entry; panics are re-thrown
    /// here and contained by the job wrapper.
    pub async fn io<R: Send + 'static>(
        &self,
        f: impl FnOnce() -> R + Send + 'static,
    ) -> Result<R, Interrupted> {
        self.note_checkpoint();
        if self.record.cancel.is_cancelled() {
            return Err(Interrupted);
        }
        let Some(s) = self.sched.upgrade() else {
            return Err(Interrupted);
        };
        let joined = s.rt.spawn_blocking(f).await;
        drop(s);
        self.note_checkpoint();
        match joined {
            Ok(value) => Ok(value),
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(_) => Err(Interrupted), // runtime shutting down
        }
    }

    fn note_checkpoint(&self) {
        if let Some(s) = self.sched.upgrade() {
            self.record
                .last_checkpoint_ms
                .store(s.elapsed_ms(), Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for JobContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobContext")
            .field("id", &self.record.id)
            .field("kind", &self.record.kind)
            .finish_non_exhaustive()
    }
}

/// Scheduler-wide throughput counters (nightly perf harness input, T18).
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) spawned: AtomicU64,
    pub(crate) completed: AtomicU64,
    pub(crate) failed: AtomicU64,
    pub(crate) cancelled: AtomicU64,
}

/// The E06 scheduler (spec §4.3). Construct via [`Scheduler::new`]; share as
/// `Arc<Scheduler>` (the session hands it to every domain subsystem).
pub struct Scheduler {
    rt: tokio::runtime::Handle,
    cfg: ArcSwap<JobsConfig>,
    lanes: [Arc<Lane>; 3],
    jobs: Mutex<HashMap<JobId, Arc<JobRecord>>>,
    /// In-flight dedupe index (spec §4.3 `spawn_keyed`): key → newest
    /// non-terminal job. Entries evict on terminal transition.
    keyed: Mutex<HashMap<JobKey, JobId>>,
    /// The class-matched rayon pools (`fg-cpu`/`bg-cpu`, T6).
    cpu: CpuPools,
    /// Millis-since-epoch of the last `note_interactive_activity` call,
    /// **plus one** (0 = never). The load-shed input (spec §4.4, T5).
    interactive_last_ms: AtomicU64,
    /// Whether the Background lane is currently shrunk (drives the
    /// `should_yield` gate bits on running Background jobs; edge-triggered).
    bg_shrunk: AtomicBool,
    next_job: AtomicU64,
    next_seq: AtomicU64,
    /// Jobs not yet `Done(_)` — `is_idle` and shutdown drain on it.
    non_terminal: AtomicUsize,
    idle_notify: Notify,
    /// Admission closed (shutdown started): new spawns are born cancelled.
    closed: AtomicBool,
    /// Manual-dispatch mode (`testing::ManualScheduler`): no background
    /// dispatch tasks; `tick()` dispatches synchronously.
    manual: bool,
    /// Monotonic anchor for millisecond stamps (checkpoint watchdog,
    /// interactive-recency window). `tokio::time::Instant` so paused-time
    /// tests control it.
    epoch: tokio::time::Instant,
    pub(crate) counters: Counters,
}

impl Scheduler {
    /// Builds a scheduler on the app's one tokio runtime (spec §4.4: E06
    /// never creates nested runtimes — `rt` is E01's session runtime
    /// handle). Dispatch runs on background tasks immediately.
    pub fn new(cfg: JobsConfig, rt: tokio::runtime::Handle) -> Arc<Scheduler> {
        Scheduler::build(cfg, rt, false)
    }

    pub(crate) fn build(
        cfg: JobsConfig,
        rt: tokio::runtime::Handle,
        manual: bool,
    ) -> Arc<Scheduler> {
        let cfg = cfg.sanitized();
        let lanes = [
            Lane::new(Class::Interactive),
            Lane::new(Class::Foreground),
            Lane::new(Class::Background),
        ];
        let cpu = CpuPools::new(&cfg);
        let sched = Arc::new(Scheduler {
            rt: rt.clone(),
            cfg: ArcSwap::from_pointee(cfg),
            lanes,
            jobs: Mutex::new(HashMap::new()),
            keyed: Mutex::new(HashMap::new()),
            cpu,
            interactive_last_ms: AtomicU64::new(0),
            bg_shrunk: AtomicBool::new(false),
            next_job: AtomicU64::new(1),
            next_seq: AtomicU64::new(1),
            non_terminal: AtomicUsize::new(0),
            idle_notify: Notify::new(),
            closed: AtomicBool::new(false),
            manual,
            epoch: tokio::time::Instant::now(),
            counters: Counters::default(),
        });
        if !manual {
            for lane in &sched.lanes {
                rt.spawn(dispatch_loop(Arc::downgrade(&sched), Arc::clone(lane)));
            }
        }
        tracing::debug!(
            target: "lightbox_jobs",
            manual,
            "scheduler started"
        );
        sched
    }

    /// Spawns a job (spec §4.3). Never blocks; the handle detaches on drop.
    pub fn spawn<T, F, Fut>(self: &Arc<Self>, spec: JobSpec, f: F) -> JobHandle<T>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, JobError>> + Send + 'static,
        T: Send + 'static,
    {
        self.spawn_inner(spec, f, false)
    }

    fn spawn_inner<T, F, Fut>(self: &Arc<Self>, spec: JobSpec, f: F, shared: bool) -> JobHandle<T>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, JobError>> + Send + 'static,
        T: Send + 'static,
    {
        let slot = Arc::new(ResultSlot {
            value: Mutex::new(None),
        });
        let id = JobId(
            NonZeroU64::new(self.next_job.fetch_add(1, Ordering::Relaxed))
                .expect("job ids start at 1"),
        );
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let born_cancelled = self.closed.load(Ordering::Acquire);
        let record = Arc::new(JobRecord {
            id,
            kind: spec.kind,
            label: spec.label,
            class: spec.class,
            pausable: spec.pausable,
            group: spec.group,
            key: spec.key,
            started_at: SystemTime::now(),
            state: Mutex::new(if born_cancelled {
                FineState::Done(Outcome::Cancelled)
            } else {
                FineState::Queued
            }),
            priority: AtomicI32::new(spec.priority.0),
            gen: AtomicU64::new(0),
            seq,
            cancel: crate::CancelToken::new(),
            gate: PauseGate::new(false),
            error: Mutex::new(None),
            work: Mutex::new(None),
            abort: Mutex::new(None),
            holds_slot: AtomicBool::new(false),
            finished: Notify::new(),
            terminal_at: Mutex::new(None),
            dismissed: AtomicBool::new(false),
            last_checkpoint_ms: AtomicU64::new(0),
        });
        let handle = JobHandle {
            record: Arc::clone(&record),
            slot: Arc::clone(&slot),
            extract: Some(Self::extract_take()),
            sched: Arc::downgrade(self),
        };
        let _ = shared; // keyed spawns build their own clone-extractor (T8)
        if born_cancelled {
            // Admission closed (spec §4.9 step 1): terminal immediately,
            // never registered, never dispatched.
            tracing::debug!(target: "lightbox_jobs", id = id.get(), kind = record.kind, "spawn refused: scheduler shut down");
            return handle;
        }

        // Build the type-erased wrapper: panic containment + result
        // delivery + terminal bookkeeping.
        let work: Work = {
            let record = Arc::clone(&record);
            let slot = Arc::clone(&slot);
            let sched = Arc::downgrade(self);
            let lane = Arc::clone(self.lane(record.class));
            Box::new(move |ctx: JobContext| {
                Box::pin(async move {
                    let mut fut = Box::pin(f(ctx));
                    // Panic containment without `unsafe`: poll through
                    // `catch_unwind` behind the Box's pin (spec §4.4 — a
                    // panicking preview job must never take down the
                    // session).
                    let caught = std::future::poll_fn(move |cx| {
                        match std::panic::catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx)))
                        {
                            Ok(Poll::Ready(out)) => Poll::Ready(Ok(out)),
                            Ok(Poll::Pending) => Poll::Pending,
                            Err(panic) => Poll::Ready(Err(panic)),
                        }
                    })
                    .await;
                    finish_job(&record, &slot, &sched, &lane, caught);
                })
            })
        };

        *lock(&record.work) = Some(work);
        lock(&self.jobs).insert(id, Arc::clone(&record));
        if let Some(key) = &record.key {
            // Newest wins for by-key lookups; `spawn_keyed` (T8) checks the
            // map BEFORE spawning, so an overwrite here only happens for
            // plain `spawn` calls that carry a key.
            lock(&self.keyed).insert(key.clone(), id);
        }
        self.non_terminal.fetch_add(1, Ordering::AcqRel);
        self.counters.spawned.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            target: "lightbox_jobs",
            id = id.get(),
            kind = record.kind,
            class = ?record.class,
            "job spawned"
        );

        // Enqueue + wake the lane.
        self.push_queued(&record);
        handle
    }

    fn extract_take<T: Send + 'static>() -> Extract<T> {
        Box::new(|slot| slot.take())
    }

    /// Pushes a `Queued` record's heap entry and wakes the lane.
    fn push_queued(&self, record: &Arc<JobRecord>) {
        let lane = self.lane(record.class);
        {
            let mut q = lock(&lane.queue);
            q.push(HeapEntry {
                prio: record.priority(),
                seq: record.seq,
                gen: record.gen.load(Ordering::Acquire),
                id: record.id,
            });
        }
        lane.queued.fetch_add(1, Ordering::AcqRel);
        lane.notify.notify_waiters();
    }

    pub(crate) fn lane(&self, class: Class) -> &Arc<Lane> {
        &self.lanes[class.index()]
    }

    /// Requests cancellation of a job by id (spec §4.3). Idempotent; a
    /// no-op on unknown/terminal jobs.
    pub fn cancel(&self, id: JobId) {
        let record = lock(&self.jobs).get(&id).cloned();
        if let Some(record) = record {
            self.cancel_record(&record);
        }
    }

    /// Cancel through the state machine: never-dispatched states are
    /// immediately terminal (the body never ran); dispatched states enter
    /// `Cancelling` until the body observes the token.
    pub(crate) fn cancel_record(&self, record: &Arc<JobRecord>) {
        let outcome = {
            let mut st = lock(&record.state);
            match *st {
                FineState::Queued | FineState::PausedQueued => {
                    let was_queued = *st == FineState::Queued;
                    match transition(*st, Input::Cancel) {
                        Ok(next) => *st = next,
                        Err(_) => return,
                    }
                    if was_queued {
                        self.lane(record.class).queued.fetch_sub(1, Ordering::AcqRel);
                    }
                    Some(Outcome::Cancelled)
                }
                FineState::Running | FineState::PausedRunning => {
                    match transition(*st, Input::Cancel) {
                        Ok(next) => *st = next,
                        Err(_) => return,
                    }
                    None
                }
                FineState::Cancelling | FineState::Done(_) => return, // no-op
            }
        };
        // Cancel the token outside the state lock (child fan-out may
        // recurse into arbitrary registered wakers).
        record.cancel.cancel();
        match outcome {
            Some(outcome) => {
                // Never dispatched: drop the body, resolve joiners.
                *lock(&record.work) = None;
                self.on_terminal(record, outcome);
            }
            None => self.note_state_changed(record),
        }
    }

    /// Lane budget for `class` after load-shed (spec §4.4, T5), plus the
    /// recovery deadline when the shrink is time-windowed (the Background
    /// dispatcher parks until then).
    ///
    /// Background shrinks to `background_min_during_interactive` while:
    /// - the most recent [`Scheduler::note_interactive_activity`] is within
    ///   `interactive_recent_window`, **or**
    /// - Foreground has queued work beyond
    ///   `foreground_queue_shrink_threshold`.
    ///
    /// Shrinking stops issuing slots — it **never aborts a running job**;
    /// running Background jobs additionally see the `should_yield` hint.
    pub(crate) fn effective_budget(&self, class: Class) -> (usize, Option<tokio::time::Instant>) {
        let cfg = self.cfg.load();
        if class != Class::Background {
            return (cfg.workers(class), None);
        }
        let base = cfg.background_workers;
        let min = cfg.background_min_during_interactive.min(base);
        let mut budget = base;
        let mut deadline = None;
        let stamp = self.interactive_last_ms.load(Ordering::Acquire);
        if stamp != 0 {
            let last = stamp - 1;
            let window = u64::try_from(cfg.interactive_recent_window.as_millis())
                .unwrap_or(u64::MAX);
            if self.elapsed_ms().saturating_sub(last) < window {
                budget = min;
                deadline = Some(self.epoch + Duration::from_millis(last.saturating_add(window)));
            }
        }
        if self.lane(Class::Foreground).queued_len() > cfg.foreground_queue_shrink_threshold {
            budget = budget.min(min);
            // Recovery here is event-driven (Foreground dispatch/finish
            // notifies the Background lane), not time-driven.
        }
        // Edge-triggered `should_yield` maintenance: flip the hint on
        // running Background jobs exactly when the shrink state changes.
        let shrunk = budget < base;
        if shrunk != self.bg_shrunk.swap(shrunk, Ordering::AcqRel) {
            self.set_bg_yield_bits(shrunk);
        }
        (budget, deadline)
    }

    /// The load-shed input (spec §4.3, seam to E05/E08): call on each
    /// interactive submission (slider drag, grid scroll). While the most
    /// recent call is within `interactive_recent_window`, the Background
    /// budget shrinks to `background_min_during_interactive`.
    pub fn note_interactive_activity(&self) {
        self.interactive_last_ms
            .store(self.elapsed_ms() + 1, Ordering::Release);
        if !self.bg_shrunk.swap(true, Ordering::AcqRel) {
            self.set_bg_yield_bits(true);
        }
        // Wake the Background dispatcher so it re-evaluates (and arms its
        // recovery timer).
        self.lane(Class::Background).notify.notify_waiters();
    }

    /// Sets/clears the `should_yield` hint on every dispatched Background
    /// job (edge-triggered from `effective_budget`).
    fn set_bg_yield_bits(&self, on: bool) {
        for record in lock(&self.jobs).values() {
            if record.class != Class::Background {
                continue;
            }
            if matches!(
                *lock(&record.state),
                FineState::Running | FineState::PausedRunning | FineState::Cancelling
            ) {
                record.gate.set_bit(BIT_SHOULD_YIELD, on);
            }
        }
    }

    /// Re-keys a queued job's priority (spec §4.3, T4): O(log n) — pushes a
    /// fresh generation-stamped heap entry; the stale entry is skipped at
    /// pop. On running/paused-running jobs only the stored priority changes
    /// (nothing is queued to reorder); on paused-queued jobs the new
    /// priority takes effect when resumed.
    pub fn set_priority(&self, id: JobId, prio: Priority) {
        let record = lock(&self.jobs).get(&id).cloned();
        if let Some(record) = record {
            self.set_priority_record(&record, prio);
        }
    }

    /// [`Scheduler::set_priority`] by dedupe key (visible-first scheduling:
    /// the grid raises on-screen preview jobs without holding handles).
    pub fn set_priority_by_key(&self, key: &JobKey, prio: Priority) {
        let id = lock(&self.keyed).get(key).copied();
        if let Some(id) = id {
            self.set_priority(id, prio);
        }
    }

    fn set_priority_record(&self, record: &Arc<JobRecord>, prio: Priority) {
        record.priority.store(prio.0, Ordering::Release);
        let requeue = { *lock(&record.state) == FineState::Queued };
        if requeue {
            // Invalidate the old entry, push a fresh one. A dispatch racing
            // this sees a gen mismatch on whichever entry it pops and skips
            // it — the job is dispatched exactly once either way.
            let gen = record.gen.fetch_add(1, Ordering::AcqRel) + 1;
            let lane = self.lane(record.class);
            lock(&lane.queue).push(HeapEntry {
                prio,
                seq: record.seq,
                gen,
                id: record.id,
            });
            lane.notify.notify_waiters();
        }
    }

    /// Applies a new configuration live (spec §4.3/T16): lane budgets take
    /// effect immediately (growing) or as running jobs finish (shrinking —
    /// never aborts); publisher rates apply from the next tick.
    /// **`fg_cpu_threads`/`bg_cpu_threads` do NOT re-size live** (rayon
    /// limitation, spec §9 R8) — they apply on the next session.
    pub fn apply_config(&self, cfg: JobsConfig) {
        let cfg = cfg.sanitized();
        let old = self.cfg.load();
        if cfg.fg_cpu_threads != old.fg_cpu_threads || cfg.bg_cpu_threads != old.bg_cpu_threads {
            tracing::info!(
                target: "lightbox_jobs",
                "cpu-pool sizing changed; applies on next session (rayon pools are fixed — R8)"
            );
        }
        drop(old);
        self.cfg.store(Arc::new(cfg));
        for lane in &self.lanes {
            lane.notify.notify_waiters();
        }
    }

    /// Pops the next runnable job of a lane and marks it `Running`
    /// (atomically under its state lock — a cancel/pause landing after this
    /// finds the job already dispatched). Skips stale heap entries (T4 lazy
    /// invalidation).
    fn pop_next(&self, lane: &Lane) -> Option<Arc<JobRecord>> {
        loop {
            let entry = lock(&lane.queue).pop()?;
            let Some(record) = lock(&self.jobs).get(&entry.id).cloned() else {
                continue; // evicted (terminal) — stale entry
            };
            if record.gen.load(Ordering::Acquire) != entry.gen {
                continue; // re-prioritized — a fresher entry exists
            }
            {
                let mut st = lock(&record.state);
                if *st != FineState::Queued {
                    continue; // cancelled/paused since queueing — stale
                }
                match transition(*st, Input::Dispatch) {
                    Ok(next) => *st = next,
                    Err(_) => continue,
                }
            }
            lane.queued.fetch_sub(1, Ordering::AcqRel);
            if lane.class == Class::Foreground {
                // Foreground backlog is a Background shrink input (T5):
                // draining it re-evaluates the Background budget.
                self.lane(Class::Background).notify.notify_waiters();
            }
            return Some(record);
        }
    }

    /// Spawns the (already `Running`) record's wrapper task. The caller has
    /// claimed the lane slot.
    fn start_job(self: &Arc<Self>, record: Arc<JobRecord>, lane: &Arc<Lane>) {
        record.holds_slot.store(true, Ordering::Release);
        record
            .last_checkpoint_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
        if record.class == Class::Background && self.bg_shrunk.load(Ordering::Acquire) {
            // Dispatched into a shrunk lane: born with the yield hint on.
            record.gate.set_bit(BIT_SHOULD_YIELD, true);
        }
        let Some(work) = lock(&record.work).take() else {
            // Double dispatch would be a scheduler bug; fail safe.
            debug_assert!(false, "job {} dispatched twice", record.id);
            if record.holds_slot.swap(false, Ordering::AcqRel) {
                lane.release_slot();
            }
            return;
        };
        self.note_state_changed(&record);
        let ctx = JobContext {
            record: Arc::clone(&record),
            lane: Arc::clone(lane),
            sched: Arc::downgrade(self),
        };
        let span = tracing::info_span!(
            target: "lightbox_jobs",
            "job",
            id = record.id.get(),
            kind = record.kind,
            class = ?record.class,
        );
        let task = self.rt.spawn(work(ctx).instrument(span));
        *lock(&record.abort) = Some(task.abort_handle());
    }

    /// One synchronous dispatch pass over every lane (manual mode / tests).
    /// Returns how many jobs were started.
    pub(crate) fn dispatch_pass(self: &Arc<Self>) -> usize {
        let mut started = 0;
        for lane in &self.lanes {
            loop {
                let (budget, _) = self.effective_budget(lane.class);
                if !lane.try_claim(budget) {
                    break;
                }
                match self.pop_next(lane) {
                    Some(record) => {
                        self.start_job(record, lane);
                        started += 1;
                    }
                    None => {
                        lane.release_claim_quiet();
                        break;
                    }
                }
            }
        }
        started
    }

    /// Terminal bookkeeping shared by every path that reaches `Done(_)`.
    pub(crate) fn on_terminal(&self, record: &Arc<JobRecord>, outcome: Outcome) {
        *lock(&record.terminal_at) = Some(tokio::time::Instant::now());
        if let Some(key) = &record.key {
            // Evict the dedupe entry iff it still points at this job
            // (a re-spawn under the same key may have replaced it).
            let mut keyed = lock(&self.keyed);
            if keyed.get(key) == Some(&record.id) {
                keyed.remove(key);
            }
        }
        if record.class == Class::Foreground {
            // Foreground backlog drain is a Background budget input (T5).
            self.lane(Class::Background).notify.notify_waiters();
        }
        match outcome {
            Outcome::Completed => self.counters.completed.fetch_add(1, Ordering::Relaxed),
            Outcome::Failed => self.counters.failed.fetch_add(1, Ordering::Relaxed),
            Outcome::Cancelled => self.counters.cancelled.fetch_add(1, Ordering::Relaxed),
        };
        record.finished.notify_waiters();
        self.note_state_changed(record);
        if self.non_terminal.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.idle_notify.notify_waiters();
        }
        tracing::debug!(
            target: "lightbox_jobs",
            id = record.id.get(),
            kind = record.kind,
            ?outcome,
            "job terminal"
        );
    }

    /// State-change hook (activity dirty-marking + `JobEvent`s land with
    /// T9; v1 is trace-only).
    pub(crate) fn note_state_changed(&self, record: &Arc<JobRecord>) {
        tracing::trace!(
            target: "lightbox_jobs",
            id = record.id.get(),
            state = ?record.public_state(),
            "state changed"
        );
    }

    /// True when no job is in a non-terminal state (spec §4.3 —
    /// `lightbox-cli --wait-idle`).
    pub fn is_idle(&self) -> bool {
        self.non_terminal.load(Ordering::Acquire) == 0
    }

    /// Millis since this scheduler's construction (monotonic; virtual under
    /// paused tokio time).
    pub(crate) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Wake + retire the lane dispatch tasks (they hold only `Weak`
        // scheduler refs and park on lane notifies).
        for lane in &self.lanes {
            lane.closed.store(true, Ordering::Release);
            lane.notify.notify_waiters();
        }
    }
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("non_terminal", &self.non_terminal.load(Ordering::Acquire))
            .field("manual", &self.manual)
            .finish_non_exhaustive()
    }
}

/// One lane's dispatch task (spec §4.4): claims a slot, pops the
/// highest-priority live entry, starts it; parks when the lane is saturated
/// or empty.
async fn dispatch_loop(sched: Weak<Scheduler>, lane: Arc<Lane>) {
    loop {
        if lane.closed.load(Ordering::Acquire) {
            return;
        }
        // Notified-before-check: any push/release/config-change after this
        // point wakes the await below even if we lose the race.
        let notified = lane.notify.notified();
        let deadline = {
            let Some(s) = sched.upgrade() else { return };
            let mut dispatched = false;
            loop {
                let (budget, _) = s.effective_budget(lane.class);
                if !lane.try_claim(budget) {
                    break;
                }
                match s.pop_next(&lane) {
                    Some(record) => {
                        s.start_job(record, &lane);
                        dispatched = true;
                    }
                    None => {
                        lane.release_claim_quiet();
                        break;
                    }
                }
            }
            if dispatched {
                continue; // re-evaluate immediately with a fresh notified
            }
            s.effective_budget(lane.class).1
        };
        match deadline {
            Some(d) => {
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep_until(d) => {}
                }
            }
            None => notified.await,
        }
    }
}

/// The wrapper's landing: outcome classification, result delivery, slot
/// release, terminal bookkeeping. Runs on the job's own task — panics were
/// already contained by the caller.
fn finish_job<T>(
    record: &Arc<JobRecord>,
    slot: &Arc<ResultSlot<T>>,
    sched: &Weak<Scheduler>,
    lane: &Arc<Lane>,
    caught: Result<Result<T, JobError>, Box<dyn std::any::Any + Send>>,
) {
    let (outcome, result) = match caught {
        Ok(Ok(value)) => (Outcome::Completed, Ok(value)),
        Ok(Err(JobError::Cancelled)) => (Outcome::Cancelled, Err(JobError::Cancelled)),
        Ok(Err(err)) => (Outcome::Failed, Err(err)),
        Err(panic) => {
            let msg = crate::system::panic_message(&*panic);
            tracing::error!(
                target: "lightbox_jobs",
                id = record.id.get(),
                kind = record.kind,
                panic = %msg,
                "job panicked (contained)"
            );
            (Outcome::Failed, Err(JobError::Panicked(msg)))
        }
    };
    if outcome == Outcome::Failed {
        let msg = match &result {
            Err(e) => e.to_string(),
            Ok(_) => unreachable!("failed outcome always carries an error"),
        };
        *lock(&record.error) = Some(msg);
    }
    *lock(&slot.value) = Some(result);

    {
        let mut st = lock(&record.state);
        match transition(*st, Input::Finish(outcome)) {
            Ok(next) => *st = next,
            Err(illegal) => {
                // A finish from an unexpected state is a scheduler bug —
                // fail safe into the terminal sink so joiners resolve.
                debug_assert!(false, "{illegal}");
                *st = FineState::Done(outcome);
            }
        }
    }
    if record.holds_slot.swap(false, Ordering::AcqRel) {
        lane.release_slot();
    }
    match sched.upgrade() {
        Some(s) => s.on_terminal(record, outcome),
        None => record.finished.notify_waiters(),
    }
}
