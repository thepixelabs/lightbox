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

use crate::activity::{ActivitySnapshot, JobEvent};
use crate::config::JobsConfig;
use crate::cpu::{CpuOutcome, CpuPools};
use crate::group::{GroupHandle, GroupRecord, GroupSpec};
use crate::model::{
    transition, ActivityRef, Class, FineState, GroupId, Input, JobId, JobKey, JobSpec, JobState,
    Outcome, Priority,
};
use crate::progress::{Dirty, ProgressSink, ProgressState};
use crate::system::JobError;
use crate::token::{Interrupted, PauseGate, BIT_CLASS_PAUSED, BIT_JOB_PAUSED, BIT_SHOULD_YIELD};

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
    /// The job's progress counters (shared with its [`ProgressSink`]).
    pub(crate) progress: Arc<ProgressState>,
    /// The typed result slot, erased — `spawn_keyed` recovers
    /// `Arc<ResultSlot<T>>` from it to hand out additional handles.
    slot_any: Arc<dyn std::any::Any + Send + Sync>,
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
    progress: ProgressSink,
}

impl JobContext {
    /// This job's id.
    pub fn job_id(&self) -> JobId {
        self.record.id
    }

    /// The job's progress reporter (spec §4.5): atomic increments, safe at
    /// arbitrary rates; the activity publisher coalesces.
    pub fn progress(&self) -> &ProgressSink {
        &self.progress
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
    /// Serializes `spawn_keyed`'s check-then-spawn so two concurrent keyed
    /// spawns of the same key run the work once (T8 AC).
    keyed_admission: Mutex<()>,
    /// Group registry.
    groups: Mutex<HashMap<GroupId, Arc<GroupRecord>>>,
    next_group: AtomicU64,
    /// Class-pause flags (spec §4.3 `pause_class`), lane order.
    class_paused_flags: [AtomicBool; 3],
    /// The published activity snapshot (lock-free load, spec §4.5).
    activity: ArcSwap<ActivitySnapshot>,
    snapshot_seq: AtomicU64,
    /// Edge-triggered event fan-out (`lightbox-cli watch`, tests).
    events: tokio::sync::broadcast::Sender<JobEvent>,
    /// Publisher wake (progress writes, state changes).
    pub(crate) dirty: Arc<Dirty>,
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
        let (events, _) = tokio::sync::broadcast::channel(cfg.event_capacity);
        let dirty = Dirty::new();
        let sched = Arc::new(Scheduler {
            rt: rt.clone(),
            cfg: ArcSwap::from_pointee(cfg),
            lanes,
            jobs: Mutex::new(HashMap::new()),
            keyed: Mutex::new(HashMap::new()),
            keyed_admission: Mutex::new(()),
            groups: Mutex::new(HashMap::new()),
            next_group: AtomicU64::new(1),
            class_paused_flags: std::array::from_fn(|_| AtomicBool::new(false)),
            activity: ArcSwap::from_pointee(ActivitySnapshot::empty()),
            snapshot_seq: AtomicU64::new(0),
            events,
            dirty: Arc::clone(&dirty),
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
            rt.spawn(crate::activity::publisher_loop(
                Arc::downgrade(&sched),
                dirty,
            ));
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
        self.spawn_inner(spec, f, Self::extract_take())
    }

    /// Keyed spawn (spec §4.3, T8): if `spec.key` matches an in-flight
    /// (queued/running/paused) job of the same `kind`, returns a handle to
    /// it instead of spawning — the demand-driven preview path leans on
    /// this (grid scroll re-requests tiles without duplicating work). A
    /// re-spawn after the keyed job completed runs the work again.
    ///
    /// Results are cloned to every handle (hence `T: Clone` — R4:
    /// `Arc`-wrap large outputs).
    pub fn spawn_keyed<T, F, Fut>(self: &Arc<Self>, spec: JobSpec, f: F) -> JobHandle<T>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, JobError>> + Send + 'static,
        T: Clone + Send + 'static,
    {
        let Some(key) = spec.key.clone() else {
            debug_assert!(false, "spawn_keyed requires JobSpec::key");
            return self.spawn_inner(spec, f, Self::extract_clone());
        };
        // Serialize check-then-spawn: two concurrent keyed spawns of one
        // key must run the work once (T8 AC).
        let admission = lock(&self.keyed_admission);
        let existing = lock(&self.keyed).get(&key).copied();
        if let Some(id) = existing {
            let record = lock(&self.jobs).get(&id).cloned();
            if let Some(record) = record {
                if record.kind == spec.kind && !lock(&record.state).is_terminal() {
                    if let Ok(slot) = Arc::downcast::<ResultSlot<T>>(Arc::clone(&record.slot_any))
                    {
                        tracing::trace!(
                            target: "lightbox_jobs",
                            id = id.get(),
                            key = %key,
                            "spawn_keyed deduped onto in-flight job"
                        );
                        return JobHandle {
                            record,
                            slot,
                            extract: Some(Self::extract_clone()),
                            sched: Arc::downgrade(self),
                        };
                    }
                    // Same key, different result type: a caller bug — fall
                    // through and run separately rather than corrupt either.
                    tracing::warn!(
                        target: "lightbox_jobs",
                        key = %key,
                        "spawn_keyed type mismatch on in-flight key; spawning separately"
                    );
                }
            }
        }
        let handle = self.spawn_inner(spec, f, Self::extract_clone());
        drop(admission);
        handle
    }

    fn spawn_inner<T, F, Fut>(self: &Arc<Self>, spec: JobSpec, f: F, extract: Extract<T>) -> JobHandle<T>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, JobError>> + Send + 'static,
        T: Send + 'static,
    {
        let slot = Arc::new(ResultSlot::<T> {
            value: Mutex::new(None),
        });
        let id = JobId(
            NonZeroU64::new(self.next_job.fetch_add(1, Ordering::Relaxed))
                .expect("job ids start at 1"),
        );
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let born_cancelled = self.closed.load(Ordering::Acquire);

        // Group membership: resolve BEFORE building the record. Closed and
        // finished groups refuse new members (close = "no more members will
        // be added") — the job spawns ungrouped, loudly.
        let mut group = None;
        let mut group_id = spec.group;
        if let Some(gid) = spec.group {
            match lock(&self.groups).get(&gid).cloned() {
                Some(g) if !g.closed.load(Ordering::Acquire) => group = Some(g),
                Some(_) => {
                    tracing::warn!(
                        target: "lightbox_jobs",
                        group = gid.get(),
                        kind = spec.kind,
                        "spawn into a closed group refused; spawning ungrouped"
                    );
                    group_id = None;
                }
                None => {
                    tracing::warn!(
                        target: "lightbox_jobs",
                        group = gid.get(),
                        kind = spec.kind,
                        "spawn into an unknown group; spawning ungrouped"
                    );
                    group_id = None;
                }
            }
        }
        // Member cancel tokens are children of the group's (cancel_group
        // fan-out reaches sub-stage tokens too).
        let cancel = match &group {
            Some(g) => g.cancel.child(),
            None => crate::CancelToken::new(),
        };
        // Class-pause halts subsequently-spawned (pausable) jobs too (T10).
        let class_paused = spec.pausable && self.class_paused(spec.class);
        let record = Arc::new(JobRecord {
            id,
            kind: spec.kind,
            label: spec.label,
            class: spec.class,
            pausable: spec.pausable,
            group: group_id,
            key: spec.key,
            started_at: SystemTime::now(),
            state: Mutex::new(if born_cancelled {
                FineState::Done(Outcome::Cancelled)
            } else if class_paused {
                // Born paused-queued (a birth state, not a transition).
                FineState::PausedQueued
            } else {
                FineState::Queued
            }),
            priority: AtomicI32::new(spec.priority.0),
            gen: AtomicU64::new(0),
            seq,
            cancel,
            gate: PauseGate::new(class_paused),
            error: Mutex::new(None),
            work: Mutex::new(None),
            abort: Mutex::new(None),
            holds_slot: AtomicBool::new(false),
            finished: Notify::new(),
            progress: Arc::new(ProgressState::new(spec.progress)),
            slot_any: Arc::clone(&slot) as Arc<dyn std::any::Any + Send + Sync>,
            terminal_at: Mutex::new(None),
            dismissed: AtomicBool::new(false),
            last_checkpoint_ms: AtomicU64::new(0),
        });
        let handle = JobHandle {
            record: Arc::clone(&record),
            slot: Arc::clone(&slot),
            extract: Some(extract),
            sched: Arc::downgrade(self),
        };
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
        if let Some(g) = &group {
            // Known race window (benign, documented): `close()` landing
            // between the resolution check above and this push means one
            // straggler member joins a closed group — it still runs and is
            // still counted; the group may finish before it does, in which
            // case its terminal tick is a harmless no-op on the latch.
            lock(&g.members).push(Arc::clone(&record));
        }
        self.non_terminal.fetch_add(1, Ordering::AcqRel);
        self.counters.spawned.fetch_add(1, Ordering::Relaxed);
        let _ = self.events.send(JobEvent::Spawned {
            id,
            kind: record.kind,
            class: record.class,
            group: record.group,
        });
        self.dirty.mark();
        tracing::debug!(
            target: "lightbox_jobs",
            id = id.get(),
            kind = record.kind,
            class = ?record.class,
            "job spawned"
        );

        if *lock(&record.state) == FineState::Queued {
            // Enqueue + wake the lane.
            self.push_queued(&record);
        }
        if let Some(g) = &group {
            if g.cancelled.load(Ordering::Acquire) {
                // Spawned into an already-cancelled group: fan the cancel
                // out to this straggler too.
                self.cancel_record(&record);
            }
        }
        handle
    }

    fn extract_take<T: Send + 'static>() -> Extract<T> {
        Box::new(|slot| slot.take())
    }

    fn extract_clone<T: Clone + Send + 'static>() -> Extract<T> {
        Box::new(|slot| slot.clone())
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
            progress: ProgressSink::new(Arc::clone(&record.progress), Arc::clone(&self.dirty)),
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
        if let Some(gid) = record.group {
            self.group_member_terminal(gid, outcome);
        }
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

    /// State-change hook: broadcast + dirty-mark the publisher.
    pub(crate) fn note_state_changed(&self, record: &Arc<JobRecord>) {
        let state = record.public_state();
        tracing::trace!(
            target: "lightbox_jobs",
            id = record.id.get(),
            ?state,
            "state changed"
        );
        let _ = self.events.send(JobEvent::StateChanged {
            id: record.id,
            state,
        });
        self.dirty.mark();
    }

    /// True when no job is in a non-terminal state (spec §4.3 —
    /// `lightbox-cli --wait-idle`).
    pub fn is_idle(&self) -> bool {
        self.non_terminal.load(Ordering::Acquire) == 0
    }

    /// Waits until [`Scheduler::is_idle`] (or the timeout). Returns whether
    /// idle was reached (the CLI's `--wait-idle` exits non-zero otherwise).
    pub async fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.idle_notify.notified();
            if self.is_idle() {
                return true;
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => return self.is_idle(),
            }
        }
    }

    // ── Pause / resume (spec §4.3/§4.4, T10) ────────────────────────────────

    /// Pauses one job. **No-op unless `spec.pausable`** (and on terminal /
    /// cancelling jobs). Queued jobs pause immediately; running jobs pause
    /// at their next checkpoint, releasing their worker slot.
    pub fn pause(&self, id: JobId) {
        let record = lock(&self.jobs).get(&id).cloned();
        if let Some(record) = record {
            self.pause_record(&record);
        }
    }

    fn pause_record(&self, record: &Arc<JobRecord>) {
        if !record.pausable {
            tracing::debug!(
                target: "lightbox_jobs",
                id = record.id.get(),
                "pause ignored: job not pausable"
            );
            return;
        }
        record.gate.set_bit(BIT_JOB_PAUSED, true);
        let changed = {
            let mut st = lock(&record.state);
            match *st {
                FineState::Queued => match transition(*st, Input::Pause) {
                    Ok(next) => {
                        *st = next;
                        self.lane(record.class).queued.fetch_sub(1, Ordering::AcqRel);
                        true
                    }
                    Err(_) => false,
                },
                // Running: the state flips at the body's next checkpoint
                // (spec §4.1); the gate bit set above is the whole request.
                _ => false,
            }
        };
        if changed {
            self.note_state_changed(record);
        }
    }

    /// Resumes one job (per-job bit; a class-paused job stays paused until
    /// [`Scheduler::resume_class`]). Paused-queued jobs re-queue at their
    /// original priority; paused-running jobs re-claim a slot and continue.
    pub fn resume(&self, id: JobId) {
        let record = lock(&self.jobs).get(&id).cloned();
        if let Some(record) = record {
            self.resume_record(&record);
        }
    }

    fn resume_record(&self, record: &Arc<JobRecord>) {
        record.gate.set_bit(BIT_JOB_PAUSED, false);
        self.requeue_if_unpaused(record);
    }

    /// PausedQueued + fully-unpaused gate → back to `Queued` (+ heap push).
    fn requeue_if_unpaused(&self, record: &Arc<JobRecord>) {
        if record.gate.is_paused() {
            return; // the other gate (job/class) still holds it
        }
        let changed = {
            let mut st = lock(&record.state);
            if *st == FineState::PausedQueued {
                match transition(*st, Input::Resume) {
                    Ok(next) => {
                        *st = next;
                        true
                    }
                    Err(_) => false,
                }
            } else {
                false
            }
        };
        if changed {
            self.push_queued(record);
            self.note_state_changed(record);
        }
        // PausedRunning wakes itself: `wait_if_paused` resolves on the gate
        // clear and the parked checkpoint re-claims its slot.
    }

    /// Pauses every pausable job of `class`, **including subsequently
    /// spawned ones** ("pause all background analysis", spec §4.3).
    pub fn pause_class(&self, class: Class) {
        self.class_paused_flags[class.index()].store(true, Ordering::Release);
        let records: Vec<Arc<JobRecord>> = lock(&self.jobs)
            .values()
            .filter(|r| r.class == class && r.pausable)
            .cloned()
            .collect();
        for record in &records {
            record.gate.set_bit(BIT_CLASS_PAUSED, true);
            let changed = {
                let mut st = lock(&record.state);
                if *st == FineState::Queued {
                    match transition(*st, Input::Pause) {
                        Ok(next) => {
                            *st = next;
                            self.lane(class).queued.fetch_sub(1, Ordering::AcqRel);
                            true
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                }
            };
            if changed {
                self.note_state_changed(record);
            }
        }
        self.dirty.mark();
        tracing::info!(target: "lightbox_jobs", ?class, "class paused");
    }

    /// Clears the class pause; per-job-paused jobs stay paused.
    pub fn resume_class(&self, class: Class) {
        self.class_paused_flags[class.index()].store(false, Ordering::Release);
        let records: Vec<Arc<JobRecord>> = lock(&self.jobs)
            .values()
            .filter(|r| r.class == class)
            .cloned()
            .collect();
        for record in &records {
            record.gate.set_bit(BIT_CLASS_PAUSED, false);
            self.requeue_if_unpaused(record);
        }
        self.dirty.mark();
        tracing::info!(target: "lightbox_jobs", ?class, "class resumed");
    }

    /// Whether `class` is class-paused.
    pub fn is_class_paused(&self, class: Class) -> bool {
        self.class_paused_flags[class.index()].load(Ordering::Acquire)
    }

    pub(crate) fn class_paused(&self, class: Class) -> bool {
        self.is_class_paused(class)
    }

    // ── Groups (spec §4.5, T8) ──────────────────────────────────────────────

    /// Creates a group: one activity entry aggregating many member jobs.
    /// Spawn members with `JobSpec::group = Some(handle.id())`; call
    /// [`GroupHandle::close`] when no more members will be added.
    pub fn create_group(self: &Arc<Self>, spec: GroupSpec) -> GroupHandle {
        let id = GroupId(
            NonZeroU64::new(self.next_group.fetch_add(1, Ordering::Relaxed))
                .expect("group ids start at 1"),
        );
        let record = GroupRecord::new(id, spec);
        lock(&self.groups).insert(id, Arc::clone(&record));
        self.dirty.mark();
        tracing::debug!(
            target: "lightbox_jobs",
            id = id.get(),
            kind = record.kind,
            "group created"
        );
        GroupHandle {
            record,
            sched: Arc::downgrade(self),
        }
    }

    /// Cancels every non-terminal member (child-token fan-out + state
    /// machine per member).
    pub fn cancel_group(&self, id: GroupId) {
        let Some(group) = lock(&self.groups).get(&id).cloned() else {
            return;
        };
        group.cancelled.store(true, Ordering::Release);
        group.cancel.cancel();
        let members: Vec<Arc<JobRecord>> = lock(&group.members).clone();
        for member in &members {
            self.cancel_record(member);
        }
        self.check_group_completion(&group);
        self.dirty.mark();
        tracing::info!(target: "lightbox_jobs", id = id.get(), "group cancelled");
    }

    /// Pauses every pausable member (the `JobCommand::Pause(Group)` path).
    pub fn pause_group(&self, id: GroupId) {
        let Some(group) = lock(&self.groups).get(&id).cloned() else {
            return;
        };
        let members: Vec<Arc<JobRecord>> = lock(&group.members).clone();
        for member in &members {
            self.pause_record(member);
        }
    }

    /// Resumes every member's per-job pause bit.
    pub fn resume_group(&self, id: GroupId) {
        let Some(group) = lock(&self.groups).get(&id).cloned() else {
            return;
        };
        let members: Vec<Arc<JobRecord>> = lock(&group.members).clone();
        for member in &members {
            self.resume_record(member);
        }
    }

    fn group_member_terminal(&self, gid: GroupId, outcome: Outcome) {
        let Some(group) = lock(&self.groups).get(&gid).cloned() else {
            return;
        };
        group.terminal_members.fetch_add(1, Ordering::AcqRel);
        match outcome {
            Outcome::Failed => {
                group.failed_members.fetch_add(1, Ordering::AcqRel);
            }
            Outcome::Cancelled => {
                group.cancelled_members.fetch_add(1, Ordering::AcqRel);
            }
            Outcome::Completed => {}
        }
        self.dirty.mark();
        self.check_group_completion(&group);
    }

    /// Completion latch: closed + every member terminal ⇒ finished once.
    pub(crate) fn check_group_completion(&self, group: &Arc<GroupRecord>) {
        if !group.closed.load(Ordering::Acquire) {
            return;
        }
        let total = lock(&group.members).len() as u64;
        if group.terminal_members.load(Ordering::Acquire) < total {
            return;
        }
        if group.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let outcome = group.final_outcome();
        *lock(&group.outcome) = Some(outcome);
        *lock(&group.terminal_at) = Some(tokio::time::Instant::now());
        let _ = self.events.send(JobEvent::GroupFinished {
            id: group.id,
            outcome,
        });
        self.dirty.mark();
        tracing::debug!(
            target: "lightbox_jobs",
            id = group.id.get(),
            ?outcome,
            "group finished"
        );
    }

    // ── Observation (spec §4.5, T9) ─────────────────────────────────────────

    /// The current activity snapshot — one lock-free atomic pointer load;
    /// poll once per frame.
    pub fn activity(&self) -> Arc<ActivitySnapshot> {
        self.activity.load_full()
    }

    /// Subscribes to the edge-triggered event stream (CLI `watch`, tests).
    /// Slow subscribers lag; they never backpressure the scheduler.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<JobEvent> {
        self.events.subscribe()
    }

    /// Cancels by activity reference (job or group) — the
    /// `JobCommand::Cancel` fan-in.
    pub fn cancel_ref(&self, target: ActivityRef) {
        match target {
            ActivityRef::Job(id) => self.cancel(id),
            ActivityRef::Group(id) => self.cancel_group(id),
        }
    }

    /// Pauses by activity reference — the `JobCommand::Pause` fan-in.
    pub fn pause_ref(&self, target: ActivityRef) {
        match target {
            ActivityRef::Job(id) => self.pause(id),
            ActivityRef::Group(id) => self.pause_group(id),
        }
    }

    /// Resumes by activity reference — the `JobCommand::Resume` fan-in.
    pub fn resume_ref(&self, target: ActivityRef) {
        match target {
            ActivityRef::Job(id) => self.resume(id),
            ActivityRef::Group(id) => self.resume_group(id),
        }
    }

    /// Dismisses a lingering (typically failed) terminal entry from the
    /// activity snapshot (`JobCommand::Dismiss`). No-op on live entries.
    pub fn dismiss(&self, target: ActivityRef) {
        match target {
            ActivityRef::Job(id) => {
                let record = lock(&self.jobs).get(&id).cloned();
                if let Some(record) = record {
                    if lock(&record.state).is_terminal() {
                        record.dismissed.store(true, Ordering::Release);
                        self.dirty.mark();
                    }
                }
            }
            ActivityRef::Group(id) => {
                let group = lock(&self.groups).get(&id).cloned();
                if let Some(group) = group {
                    if group.finished.load(Ordering::Acquire) {
                        group.dismissed.store(true, Ordering::Release);
                        self.dirty.mark();
                    }
                }
            }
        }
    }

    /// Publisher rate (live-configurable).
    pub(crate) fn publish_hz(&self) -> u8 {
        self.cfg.load().snapshot_publish_hz
    }

    /// The earliest pending linger expiry across terminal entries (the
    /// publisher's age-out wake-up).
    pub(crate) fn earliest_linger_expiry(&self) -> Option<tokio::time::Instant> {
        let linger = self.cfg.load().completed_linger;
        let mut earliest: Option<tokio::time::Instant> = None;
        let mut consider = |at: Option<tokio::time::Instant>| {
            if let Some(at) = at {
                let expiry = at + linger;
                earliest = Some(earliest.map_or(expiry, |e| e.min(expiry)));
            }
        };
        for record in lock(&self.jobs).values() {
            if let JobState::Done(outcome) = record.public_state() {
                let dismissed = record.dismissed.load(Ordering::Acquire);
                if outcome != Outcome::Failed || dismissed {
                    consider(*lock(&record.terminal_at));
                }
            }
        }
        for group in lock(&self.groups).values() {
            if group.finished.load(Ordering::Acquire) {
                let failed = matches!(*lock(&group.outcome), Some(Outcome::Failed));
                if !failed || group.dismissed.load(Ordering::Acquire) {
                    consider(*lock(&group.terminal_at));
                }
            }
        }
        earliest
    }

    /// Folds the registry into a fresh snapshot, publishes it, emits
    /// coalesced `Progress` events, and sweeps aged-out terminal entries.
    pub(crate) fn publish_now(&self, last_epochs: &mut HashMap<ActivityRef, u64>) {
        let now = tokio::time::Instant::now();
        let linger = self.cfg.load().completed_linger;
        let expired = |terminal_at: &Mutex<Option<tokio::time::Instant>>| {
            lock(terminal_at).is_none_or(|at| now >= at + linger)
        };

        // Sweep + collect jobs.
        let mut job_records: Vec<Arc<JobRecord>> = Vec::new();
        {
            let mut jobs = lock(&self.jobs);
            jobs.retain(|_, record| {
                let keep = match record.public_state() {
                    JobState::Done(outcome) => {
                        if record.dismissed.load(Ordering::Acquire) {
                            false
                        } else if outcome == Outcome::Failed {
                            true // lingers until dismissed
                        } else {
                            !expired(&record.terminal_at)
                        }
                    }
                    _ => true,
                };
                if keep {
                    job_records.push(Arc::clone(record));
                }
                keep
            });
        }
        // Sweep + collect groups.
        let mut group_records: Vec<Arc<GroupRecord>> = Vec::new();
        {
            let mut groups = lock(&self.groups);
            groups.retain(|_, group| {
                let keep = if !group.finished.load(Ordering::Acquire) {
                    true
                } else if group.dismissed.load(Ordering::Acquire) {
                    false
                } else if matches!(*lock(&group.outcome), Some(Outcome::Failed)) {
                    true // lingers until dismissed
                } else {
                    !expired(&group.terminal_at)
                };
                if keep {
                    group_records.push(Arc::clone(group));
                }
                keep
            });
        }

        let mut entries = Vec::new();
        let mut counts = crate::activity::ActivityCounts::default();
        let mut epochs: HashMap<ActivityRef, u64> = HashMap::new();
        for record in &job_records {
            let state = record.public_state();
            counts.count(record.class, state);
            if record.group.is_some() {
                continue; // aggregated into the group entry
            }
            let id = ActivityRef::Job(record.id);
            epochs.insert(id, record.progress.epoch());
            entries.push(crate::activity::ActivityEntry {
                id,
                kind: record.kind,
                label: record.label.clone(),
                detail: record.progress.detail(),
                class: record.class,
                state,
                pausable: record.pausable,
                progress: Some(record.progress.view()),
                started_at: record.started_at,
                error: lock(&record.error).clone(),
            });
        }
        for group in &group_records {
            let id = ActivityRef::Group(group.id);
            epochs.insert(id, group.progress_epoch());
            let state = group_public_state(group);
            let error = (matches!(state, JobState::Done(Outcome::Failed))).then(|| {
                let failed = group.failed_members.load(Ordering::Acquire);
                format!("{failed} of {} items failed", lock(&group.members).len())
            });
            entries.push(crate::activity::ActivityEntry {
                id,
                kind: group.kind,
                label: group.label.clone(),
                detail: None,
                class: group.class,
                state,
                pausable: group.pausable,
                progress: Some(group.aggregate_view()),
                started_at: group.started_at,
                error,
            });
        }
        entries.sort_by_key(|e| e.started_at);

        let seq = self.snapshot_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let class_paused = std::array::from_fn(|i| self.class_paused_flags[i].load(Ordering::Acquire));
        self.activity.store(Arc::new(ActivitySnapshot {
            seq,
            entries,
            class_paused,
            counts,
        }));

        // Coalesced Progress ticks: one event per entry whose progress
        // moved since the previous publish.
        for (id, epoch) in &epochs {
            if last_epochs.get(id) != Some(epoch) {
                let _ = self.events.send(JobEvent::Progress { id: *id });
            }
        }
        *last_epochs = epochs;
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
        // scheduler refs and park on lane notifies), and the publisher.
        for lane in &self.lanes {
            lane.closed.store(true, Ordering::Release);
            lane.notify.notify_waiters();
        }
        self.dirty.close();
    }
}

/// A group's representative public state for the activity entry.
fn group_public_state(group: &GroupRecord) -> JobState {
    if group.finished.load(Ordering::Acquire) {
        return JobState::Done(lock(&group.outcome).unwrap_or(Outcome::Completed));
    }
    if group.cancelled.load(Ordering::Acquire) {
        return JobState::Cancelling;
    }
    let members = lock(&group.members).clone();
    let mut any_running = false;
    let mut any_queued = false;
    let mut any_paused = false;
    for member in &members {
        match member.public_state() {
            JobState::Running | JobState::Cancelling => any_running = true,
            JobState::Queued => any_queued = true,
            JobState::Paused => any_paused = true,
            JobState::Done(_) => {}
        }
    }
    if any_running {
        JobState::Running
    } else if any_queued {
        JobState::Queued
    } else if any_paused {
        JobState::Paused
    } else {
        // Empty or all-terminal-but-open: still accepting members.
        JobState::Queued
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
