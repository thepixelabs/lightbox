# E06 — Jobs & background task system

| | |
|---|---|
| **Epic id** | E06 `jobs-background-system` |
| **Milestone** | M1 |
| **Effort** | S–M, ~3–4 pw (18 tasks ≤ 1 day each + slack) |
| **Depends on** | E01 (workspace, `lightbox-core` command/query bus + event bus, prefs store, `lightbox-cli` harness) |
| **Depended on by** | E08 (activity center UI, concurrency prefs), E13 (`lightbox-inferd` client jobs), E15 (export jobs); adopted by E03 (preview builds) and E04 (import pipeline) |
| **Architecture references** | §2.1/§2.2 (`lightbox-jobs`), §2.3 seam 4, §5.1–5.3 (concurrency & job classes), §6 (failure modes), §7 (budgets), §8 (testing) |

---

## 1. Summary & thesis

Everything expensive in Lightbox is a **cancellable job**; the UI thread never blocks. E06 builds the machinery that makes that sentence true: the `lightbox-jobs` crate — a class-prioritized, pausable, cancellable job scheduler on the tokio multi-thread runtime — plus the **activity-center backing model** (the data the E08 UI renders), the **backpressure primitives** that keep a 10k-raw import from OOMing the queue, and the `lightbox-core` command/query façade wiring so the shell and `lightbox-cli` can observe and control jobs without UI types crossing the boundary.

The product motivation is explicit in the research: Lightroom's documented "background work steals my session" and "each export got slower" complaints, and the feature catalog's Must-tier **unified background-task activity center** (per-task cancel, pausable AI/analysis). The architecture's answer (§5.3) is three job classes with preemption — **Interactive ≻ Foreground ≻ Background** — cooperative cancellation threaded everywhere, pause on all Background work, and bounded channels between pipeline stages. E06 is the sole owner of that answer's implementation.

E06 is deliberately **domain-free**: it schedules opaque futures. Import, preview building, export, AI analysis are jobs *defined by other epics* that run on this substrate.

---

## 2. Scope

### In scope

1. **`lightbox-jobs` crate** — job model (id, kind, class, priority, state machine), spawn/handle API, per-class scheduling lanes with concurrency budgets, priority queues with re-prioritization (visible-first scheduling support), keyed dedupe, job groups with aggregated progress.
2. **Cancellation** — cooperative `CancelToken` (hierarchical; the same token type the E05 render scheduler threads through, per §5.3), a documented cancellation-latency contract, cancel-on-drop policy per handle.
3. **Pause/resume** — per-job and per-class (the "pause all background analysis" control), via a cooperative `PauseGate` checkpoint.
4. **Preemption policy (CPU side)** — Foreground work throttles Background worker budgets; recent Interactive activity shrinks Background further (the load-shed signal). Dedicated rayon CPU pools for Foreground/Background CPU-heavy closures, bridged with cancellation.
5. **Progress & activity model** — throttled progress reporting (items/bytes/indeterminate, growable totals), group aggregation, a lock-free `ActivitySnapshot` the shell polls each frame, and a `JobEvent` stream for tests/CLI.
6. **Backpressure primitives** — cancel-aware bounded stage channels (`stage::bounded`) used between pipeline stages (import → preview-build being the canonical consumer).
7. **Failure handling** — typed `JobError`, per-job panic containment (a panicking job fails; workers survive), a small retry-with-backoff helper for consumers (E13 model downloads).
8. **Core façade integration** — `JobCommand`/activity query/`Event::Jobs` wiring in `lightbox-core`; a `lightbox-cli jobs` subcommand for headless observation (E2E harness per §8).
9. **Runtime-updatable configuration** — `JobsConfig` (worker budgets, throttle windows, coalescing rates) read from the E01 prefs store and re-appliable live (the knobs E08's performance-preferences panel binds).
10. **Graceful shutdown** — drain protocol with grace period; contract documentation for abort-safe job authoring (atomic temp-then-rename stays a domain-epic duty, §6 "disk full").
11. **Instrumentation** — `tracing` spans per job, queue-depth/throughput counters for the nightly perf harness, and a deterministic manual-dispatch test mode.
12. **Reference adoption** — migrate the M0 interim background work (embedded-preview extraction spawned ad hoc in E01/E03 skeleton code) onto the scheduler, proving the API against real load.

### Explicit non-goals

- **The activity-center UI** (stacked progress bars, disclosure, pause buttons) — E08. E06 ships the backing model only; no egui types in this crate, ever.
- **The render scheduler and its coalescing** (§4.3 latest-wins slider debounce) — E05. The render scheduler is a separate unit (§5.1) with its own dispatch; E06 supplies the shared `CancelToken` type, the `Class` vocabulary, and the interactive-load signal, not render dispatch.
- **GPU time-slicing** ("export never starves the interactive canvas", §5.3/§7). The *mechanism* — yielding between tiles, prioritizing Interactive submissions inside the engine — is E05's engine-internal policy. E06 carries the class tag on jobs and the load-shed signal E05 consults. The starvation acceptance test is an E05×E15 integration gate; E06 provides the CPU-side half and the hooks.
- **The `lightbox-inferd` IPC protocol, supervision, restart** — E13 (§5.2, §6). Inference calls are ordinary Background jobs from E06's point of view.
- **Domain job implementations** — import (E04), preview build (E03), export (E15), faces/embeddings (E14), metadata write-back (E07/E09). E06 defines the contract they implement.
- **A durable/persistent job queue.** Jobs do not survive process restart in v1. Rationale: every v1 job is either re-derivable on demand (previews, embeddings, faces re-enqueue from catalog state), user-re-initiated (export), or journaled at the domain level (`import_session` — E04's table). This matches Lightroom behavior and keeps E06 out of the catalog schema. Named open question in §9 if E04 later wants resume-import.
- **Scheduled/recurring tasks** (exit-time backup is triggered by E01's shutdown path calling `Catalog::backup_verified`; it may *run as* a Foreground job but its scheduling trigger is not E06's).
- **Cross-process job scheduling.** `lightbox-inferd` supervision is E13's; the decode sandbox is E02's. Subprocess lifecycles are owned by their epics; the wrapping *job* (cancel/pause/progress) is what E06 sees.

---

## 3. Crates & modules touched

| Crate | Change | Notes |
|---|---|---|
| **`lightbox-jobs`** (new; stub exists from E01 workspace layout) | The epic's body | Modules: `model` (ids, classes, states, errors), `token` (cancel/pause), `sched` (lanes, queues, dispatch), `progress`, `activity`, `group`, `stage` (backpressure), `cpu` (rayon bridges), `retry`, `config`, `testing` (manual-dispatch harness) |
| **`lightbox-core`** | Small, additive | `JobCommand` on the command bus; `activity()` on the query façade; `Event::Jobs` on the event bus; scheduler construction in session bootstrap; shutdown ordering |
| **`lightbox-cli`** | Small, additive | `jobs list/watch/cancel/pause/resume`, `--wait-idle` flag for E2E scripts (§8 integration layer) |
| **`lightbox-preview` / ingest skeleton code from M0** | Adoption only | Replace interim `tokio::spawn` of embedded-preview extraction with `Scheduler::spawn_keyed` Background jobs (T15). No behavioral redesign of E03 |

**Dependency policy (surface-1 license gate, §8):** `tokio` (MIT), `tokio-util` (MIT — source of the cancellation token), `rayon` (MIT/Apache-2.0), `arc-swap` (MIT/Apache-2.0), `parking_lot` (MIT/Apache-2.0), `thiserror` (MIT/Apache-2.0), `tracing` (MIT). **Do not use the `priority-queue` crate** (LGPL-3.0/MPL dual — fails the static-link allowlist); the priority queue is `std::collections::BinaryHeap` with generation-stamped lazy invalidation (T4). `lightbox-jobs` stays low-dependency and UI-free so any crate (including `lightbox-render`) can depend on it cheaply.

---

## 4. Design

### 4.1 Job model

```rust
// lightbox-jobs::model

/// Process-unique, monotonically increasing. Never reused within a session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct JobId(pub NonZeroU64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GroupId(pub NonZeroU64);

/// §5.3: Interactive ≻ Foreground ≻ Background.
///   Interactive — user is waiting on it this frame/second (render-adjacent coordination).
///   Foreground  — user asked for it and watches progress (import, export, metadata write).
///   Background  — the app decided to do it (preview builds, AI analysis, faces, embeddings).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class { Interactive, Foreground, Background }

/// Within-class ordering; higher runs sooner. Re-assignable while queued
/// (visible-first scheduling: the grid raises priority of on-screen preview jobs).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Priority(pub i32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState { Queued, Running, Paused, Cancelling, Done(Outcome) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome { Completed, Failed, Cancelled }

/// Dedupe key: namespaced string, e.g. "preview.t1:<content_hash>".
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct JobKey(pub Cow<'static, str>);

pub struct JobSpec {
    /// Machine-readable kind, namespaced: "preview.build_t1", "import.copy", "export.render".
    pub kind: &'static str,
    /// Human-readable label for the activity center ("Importing from EOS_R5 (312 of 3,041)").
    /// Presentation-neutral string; the shell owns styling. Localization is deferred (see §9).
    pub label: String,
    pub class: Class,
    pub priority: Priority,
    /// Defaults to `class == Class::Background`. Pausable Foreground jobs are allowed (export).
    pub pausable: bool,
    pub group: Option<GroupId>,
    /// If set, an identical in-flight key returns the existing handle instead of spawning.
    pub key: Option<JobKey>,
    pub progress: ProgressStyle, // Indeterminate | Items { total: Option<u64> } | Bytes { total: Option<u64> }
}
```

**State machine** (single source of truth, table-tested in T1):

```
Queued ──dispatch──► Running ──ok/err──► Done(Completed|Failed)
Queued ──pause──► Paused(queued) ──resume──► Queued
Running ──pause──► (takes effect at next checkpoint) Paused(running) ──resume──► Running
Queued ──cancel──► Done(Cancelled)                       (immediate; never dispatched)
Running|Paused ──cancel──► Cancelling ──job observes──► Done(Cancelled)
any Done ──► terminal (no transitions out)
```

### 4.2 Cancellation & pause primitives

```rust
// lightbox-jobs::token

/// Hierarchical cooperative cancellation. This IS tokio_util's token — proven code,
/// child_token() for fan-out, and E05's render scheduler can use the same type without
/// depending on lightbox-jobs internals (§5.3 "threaded through every job AND the
/// render scheduler").
pub type CancelToken = tokio_util::sync::CancellationToken;

/// Why the job was interrupted at a checkpoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interrupted; // semantically: cancelled. Pause never errors; it just waits.

/// Cooperative pause. Cheap when not paused (one atomic load).
pub struct PauseGate { /* watch channel over a Paused/Running bit, class-gate + job-gate ANDed */ }
impl PauseGate {
    pub fn is_paused(&self) -> bool;
    /// Resolves when unpaused; returns Err(Interrupted) if cancelled while waiting.
    pub async fn wait_if_paused(&self, cancel: &CancelToken) -> Result<(), Interrupted>;
}
```

**Cancellation-latency contract (documented on `JobContext`, watchdog-enforced in debug builds):** a job must reach a cancel-aware await (`checkpoint()`, `stage::send/recv`, `ctx.cpu()` boundary) at least every **100 ms** of wall time. A debug watchdog logs (test mode: fails) any Running job that goes > 1 s without a checkpoint. This is what makes "scrolling the grid cancels off-screen preview renders" (§5.3) feel instant, and bounds shutdown time.

**Pause semantics:** pause is checkpoint-granular — a running job pauses at its next `checkpoint()`, holding no scheduler resources while paused (its worker permit is released; see T5). Jobs are contractually forbidden from holding non-reentrant external resources (write transactions, exclusive file locks) across a checkpoint; the rustdoc adoption guide (T18) states this.

### 4.3 Scheduler API

```rust
// lightbox-jobs::sched

pub struct Scheduler { /* lanes, queues, activity publisher, config */ }

pub struct JobHandle<T> {
    pub fn id(&self) -> JobId;
    /// Await the result. Dropping the handle DETACHES (job keeps running) — activity-center
    /// jobs outlive their spawner. Explicit cancel is `handle.cancel()` or the command surface.
    pub async fn join(self) -> Result<T, JobError>;
    pub fn cancel(&self);
    pub fn state(&self) -> JobState;
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("cancelled")]            Cancelled,
    #[error("panicked")]             Panicked { message: String },
    #[error(transparent)]            Failed(#[from] anyhow::Error), // domain error, user-visible via ActivityEntry
}

impl Scheduler {
    pub fn new(cfg: JobsConfig, rt: tokio::runtime::Handle) -> Arc<Scheduler>;

    pub fn spawn<T, F, Fut>(&self, spec: JobSpec, f: F) -> JobHandle<T>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, JobError>> + Send + 'static,
        T: Send + 'static;

    /// Keyed spawn: if `spec.key` matches an in-flight (Queued/Running/Paused) job of the
    /// same `kind`, returns a handle to it instead of spawning. The demand-driven preview
    /// path leans on this: grid scroll re-requests tiles without duplicating work.
    pub fn spawn_keyed<T: Clone + Send + 'static, F, Fut>(&self, spec: JobSpec, f: F) -> JobHandle<T>
        where F: FnOnce(JobContext) -> Fut + Send + 'static,
              Fut: Future<Output = Result<T, JobError>> + Send + 'static;

    /// Groups: one activity entry aggregating many item-jobs ("Building previews — 412/3,041").
    pub fn create_group(&self, spec: GroupSpec) -> GroupHandle;

    // Control surface (mirrored 1:1 by lightbox-core JobCommand):
    pub fn cancel(&self, id: JobId);
    pub fn cancel_group(&self, id: GroupId);
    pub fn pause(&self, id: JobId);      // no-op unless spec.pausable
    pub fn resume(&self, id: JobId);
    pub fn pause_class(&self, class: Class);   // "pause all background analysis"
    pub fn resume_class(&self, class: Class);
    /// Visible-first scheduling: re-key a queued job's priority in O(log n) (lazy heap entry).
    pub fn set_priority(&self, id: JobId, prio: Priority);
    pub fn set_priority_by_key(&self, key: &JobKey, prio: Priority);

    // Observation (see §4.5):
    pub fn activity(&self) -> Arc<ActivitySnapshot>;              // lock-free ArcSwap load
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<JobEvent>;
    pub fn is_idle(&self) -> bool;                                // no non-terminal jobs (CLI --wait-idle)

    /// Load-shed input (seam to E05/E08): the render scheduler / shell calls this on each
    /// interactive submission (slider drag, grid scroll). While the most recent call is
    /// within `cfg.interactive_recent_window`, Background budgets shrink to
    /// `cfg.background_min_during_interactive`.
    pub fn note_interactive_activity(&self);

    pub fn apply_config(&self, cfg: JobsConfig);                  // live, from prefs (E08 knobs)
    pub async fn shutdown(&self, grace: Duration) -> ShutdownReport;
}

pub struct JobContext {
    pub fn job_id(&self) -> JobId;
    pub fn cancel_token(&self) -> &CancelToken;
    pub fn is_cancelled(&self) -> bool;
    /// The one call every well-behaved job loops through: yields while paused,
    /// Err(Interrupted) when cancelled. One atomic load on the hot path.
    pub async fn checkpoint(&self) -> Result<(), Interrupted>;
    pub fn progress(&self) -> &ProgressSink;
    /// CPU-heavy closure on the class-matched rayon pool (§4.4). Cancel-aware at entry;
    /// the closure itself should poll `cancel_token` for long kernels.
    pub async fn cpu<R: Send + 'static>(&self, f: impl FnOnce(&CancelToken) -> R + Send + 'static)
        -> Result<R, Interrupted>;
    /// Blocking I/O (file copy, checksum read) via spawn_blocking, cancel-aware at entry.
    pub async fn io<R: Send + 'static>(&self, f: impl FnOnce() -> R + Send + 'static)
        -> Result<R, Interrupted>;
}
```

### 4.4 Scheduling, lanes & preemption (CPU side)

- **One tokio multi-thread runtime for the whole app** (owned by E01's session bootstrap; §5.1 "job runtime"). E06 does not create nested runtimes; the scheduler takes a `Handle`.
- **Three lanes.** Each lane = a `BinaryHeap` priority queue (generation-stamped entries for lazy re-prioritization/removal) + a `Semaphore` of worker permits + one dispatch task:
  - **Interactive lane:** effectively unqueued — a large permit budget and immediate dispatch. It exists so render-adjacent coordination work is never behind Foreground admission, and so Interactive work appears in the activity accounting. (Per-frame render dispatch itself stays inside E05's render scheduler, §5.1 — that unit does not queue through E06.)
  - **Foreground lane:** default permits `max(2, cores/2)`.
  - **Background lane:** default permits `max(1, cores/4)`; **dynamically shrunk to `background_min_during_interactive` (default 1)** while `note_interactive_activity()` has fired within the window (default 500 ms), and while Foreground has queued work beyond a threshold. Shrinking never aborts running jobs — it stops issuing permits; running Background jobs also observe a `should_yield()` hint at checkpoints and may voluntarily re-queue (T5).
- **CPU-heavy work** never runs on tokio workers. `ctx.cpu()` routes to one of **two dedicated rayon pools** (`fg-cpu`, default `cores − 2`, min 2; `bg-cpu`, default `max(1, cores/4)`), keeping decode/encode/checksum kernels from starving the async reactor. (Interactive CPU work — the render engine's `eval_cpu` — runs on E05's own pool, per §4.4 of the architecture; not E06's concern.)
- **Panic containment:** each job future is wrapped in `AssertUnwindSafe(..).catch_unwind()` (and rayon closures likewise); a panic marks the job `Done(Failed)` with `JobError::Panicked`, publishes the event, and the worker survives. A panicking preview job must never take down the session.
- **No in-flight preemption.** Preemption is admission-control-granular plus cooperative yielding — we deliberately do not implement poll-level priorities (tokio has none). Consequence (accepted, documented): a Foreground burst waits for at most `background_workers` in-flight Background checkpoints intervals (≤ ~100 ms each) before the budget frees. This bounds the "steals my session" tail without exotic machinery.

### 4.5 Progress & the activity model (E08's data source)

```rust
// lightbox-jobs::progress / ::activity

pub struct ProgressSink { /* atomics; coalesced publication */ }
impl ProgressSink {
    pub fn advance(&self, items: u64);
    pub fn advance_bytes(&self, bytes: u64);
    pub fn set_total(&self, total: u64);          // totals may GROW (discovery during import scan)
    pub fn set_detail(&self, detail: impl Into<String>); // e.g. current filename; throttled
}

/// Immutable, cheaply cloneable snapshot; shell polls once per frame (egui immediate mode).
pub struct ActivitySnapshot {
    pub seq: u64,
    pub entries: Vec<ActivityEntry>,   // groups aggregate their member jobs into ONE entry
    pub class_paused: [bool; 3],
    pub counts: ActivityCounts,        // queued/running/paused per class (for a status-bar glyph)
}

pub struct ActivityEntry {
    pub id: ActivityRef,               // Job(JobId) | Group(GroupId)
    pub kind: &'static str,
    pub label: String,
    pub detail: Option<String>,
    pub class: Class,
    pub state: JobState,
    pub pausable: bool,
    pub progress: Option<ProgressView>, // { fraction: Option<f32>, items: Option<(u64,u64)>, bytes: Option<(u64,u64)> }
    pub started_at: SystemTime,
    pub error: Option<String>,          // present when Done(Failed); user-facing summary
}

#[derive(Clone, Debug)]
pub enum JobEvent {
    Spawned { id: JobId, kind: &'static str, class: Class, group: Option<GroupId> },
    StateChanged { id: JobId, state: JobState },
    Progress { id: ActivityRef },      // coalesced tick; payload read from snapshot
    GroupFinished { id: GroupId, outcome: Outcome },
}
```

**Publication discipline (keeps the UI thread cold):**
- Progress writes are **atomic increments** on the sink — no lock, no channel send per item.
- A publisher task folds sinks into a fresh `ActivitySnapshot` at `snapshot_publish_hz` (default 10 Hz) **only when something changed**, and stores it in an `ArcSwap`. Shell cost per frame = one atomic pointer load. Completed entries linger in the snapshot for a few seconds (config) so fast jobs are visible, then age out; failed entries linger until dismissed (`JobCommand::Dismiss`).
- `broadcast` events are for `lightbox-cli watch`, tests, and edge-triggered consumers — the shell needs only the snapshot.

**Groups:** `GroupSpec { kind, label, class, pausable, expected_total: Option<u64> }`. Member jobs spawned with `group: Some(id)` roll their progress into the group entry; the group completes when `GroupHandle::close()` has been called *and* all members are terminal (close = "no more members will be added", so a streaming import can add jobs while earlier ones finish). `cancel_group` fans out to all non-terminal members via a child `CancelToken`.

### 4.6 Backpressure primitive

```rust
// lightbox-jobs::stage — bounded, cancel-aware channels between pipeline stages (§5.3)
pub fn bounded<T: Send>(capacity: usize) -> (StageSender<T>, StageReceiver<T>);

impl<T: Send> StageSender<T> {
    /// Awaits capacity; Err(Interrupted) if `cancel` fires while waiting. This is the
    /// mechanism by which "import throttles preview-build enqueue so a 10k-raw card
    /// can't OOM the queue" — the copy stage blocks when the preview stage lags.
    pub async fn send(&self, item: T, cancel: &CancelToken) -> Result<(), Interrupted>;
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>>;
}
impl<T: Send> StageReceiver<T> {
    pub async fn recv(&mut self, cancel: &CancelToken) -> Result<Option<T>, Interrupted>; // None = upstream closed
}
```

Thin wrapper over `tokio::sync::mpsc` + `select!` with the token — boring by design; its value is that every stage boundary in E04/E03/E15 uses the *same* cancel-aware idiom instead of five hand-rolled ones.

### 4.7 `lightbox-core` façade & CLI

```rust
// lightbox-core additions (command bus / query façade / event bus — E01's shapes)
pub enum JobCommand {
    Cancel(ActivityRef), Pause(ActivityRef), Resume(ActivityRef),
    PauseClass(Class), ResumeClass(Class),
    Dismiss(ActivityRef),               // clear a lingering failed entry
}
impl Session {
    pub fn jobs(&self) -> Arc<Scheduler>;              // for in-core subsystems (ingest, preview…)
    pub fn activity(&self) -> Arc<ActivitySnapshot>;   // query façade, shell-safe
}
// Event bus: Event::Jobs(JobEvent) — forwarded, throttled, from Scheduler::subscribe()
```

Job *control* flows through the command bus (uniform audit/logging path); job *observation* is the snapshot query. **Job commands are not undoable history steps** — they never touch `history_step` (§3.1); the command bus treats them as non-transactional control messages. Domain epics get the `Arc<Scheduler>` from the session and spawn directly (spawning is not a user command; commands like "Import…" are, and their handlers spawn).

`lightbox-cli`: `jobs list` (snapshot table), `jobs watch` (event stream), `jobs cancel|pause|resume <id|class>`, and a global `--wait-idle [timeout]` so E2E scripts (§8: import → edit → export headless) can deterministically wait for background completion.

### 4.8 Configuration (the E08 prefs seam)

```rust
pub struct JobsConfig {
    pub foreground_workers: usize,                  // default max(2, cores/2)
    pub background_workers: usize,                  // default max(1, cores/4)
    pub background_min_during_interactive: usize,   // default 1
    pub interactive_recent_window: Duration,        // default 500 ms
    pub fg_cpu_threads: usize,                      // default max(2, cores − 2)
    pub bg_cpu_threads: usize,                      // default max(1, cores/4)
    pub snapshot_publish_hz: u8,                    // default 10
    pub completed_linger: Duration,                 // default 5 s
}
```

Loaded from the E01 prefs store at session start; `apply_config` re-sizes semaphores live (rayon pools re-size on next session — documented limitation, T16). E08's performance panel binds exactly these fields (§10 E08 note "job-concurrency knobs (E06)").

### 4.9 Shutdown

`Scheduler::shutdown(grace)` (called by `lightbox-core` session teardown, *before* catalog close and exit-time backup):
1. Stop admission (new spawns → immediately `Done(Cancelled)`).
2. Cancel all Background jobs; cancel Queued Foreground jobs.
3. Signal cancel to Running Foreground jobs (they abort at their next checkpoint; domain code guarantees on-disk atomicity via temp-then-rename per §6, so aborting mid-copy is safe).
4. Await all terminal up to `grace` (default 5 s — the checkpoint contract makes this realistic).
5. Abort stragglers via runtime handle; report them in `ShutdownReport { completed, cancelled, aborted: Vec<(JobId, &'static str)> }` (logged; aborted-nonzero is a bug signal in CI).

A `kill -9` at any point in this sequence must leave the catalog `integrity_check`-clean — that invariant is carried by E01's WAL discipline (jobs mutate the catalog only through the single-writer handle); E06 adds a fault-injection test that kills mid-jobs specifically (T17).

### 4.10 Instrumentation & deterministic testing

- Every job runs inside a `tracing` span `job{id, kind, class}`; state transitions are events. Queue depth, dispatch latency, jobs started/completed/failed counters exported for the nightly scenario harness (§8 perf layer).
- `testing::ManualScheduler`: same API, manual `tick()` dispatch and virtual time (`tokio::time::pause`), so unit tests of ordering/preemption are deterministic — no sleeps, no flaky thresholds.

---

## 5. Data model — no catalog changes

**E06 adds no tables, columns, or migrations.** Job state is in-memory by design: jobs are re-derivable (previews, analysis) or re-initiated (export), and durable domain journals (`import_session`, `preview`, `model_pack`) belong to their owning epics per §3.1. This keeps the scheduler out of the catalog's transactional hot path and preserves the single-write-path rule. Anything E06 wrote to SQLite would be a second source of truth about work that the catalog's own state already implies.

---

## 6. Seams to neighboring epics (named, not designed)

| Seam | Direction | Contract |
|---|---|---|
| **E01 core** | E06 consumes | Tokio runtime handle + session bootstrap; command/query/event bus shapes; prefs store; `lightbox-cli` plumbing. E06 registers `JobCommand`, the activity query, `Event::Jobs`. |
| **E03 preview / E04 ingest** | They consume E06 | Preview builds = Background, keyed (`"preview.t1:<hash>"`), visible-first via `set_priority_by_key`; import = Foreground group (copy/verify/second-copy jobs) feeding preview enqueue through `stage::bounded`. E06 ships the reference adoption (T15) on the M0 embedded-preview path; E04 designs its own pipeline stages. |
| **E05 render** | Shared vocabulary + signal | Same `CancelToken` type (`tokio-util`); `Class` vocabulary; render scheduler calls `note_interactive_activity()` per interactive submission. GPU time-slicing policy and the per-frame render dispatch remain inside E05's engine. Export renders (E15) reach the GPU **through E05's `Engine::submit`**, which prioritizes Interactive — E06 never touches wgpu. |
| **E08 shell** | Consumes E06 | Renders `ActivitySnapshot` (poll per frame, lock-free); issues `JobCommand`s; binds `JobsConfig` knobs in the performance prefs panel. No egui types cross into `lightbox-jobs`. |
| **E13 ML platform** | Consumes E06 | Inference/model-download calls run as pausable Background jobs; `retry` helper for downloads; inferd crash surfaces as `JobError::Failed` after E13's supervisor gives up. IPC protocol, heartbeat, restart = E13. |
| **E15 export** | Consumes E06 | Export batch = Foreground group with per-item progress + cancel; collision/cleanup semantics (partial-export cleanup, §6 disk-full) are E15's, expressed inside its job bodies. |
| **E07 DAM** | Consumes E06 | Batch metadata write-back / relink scans run as Foreground or Background jobs; nothing E06-specific beyond the public API. |

---

## 7. Ordered task breakdown (each ≤ 1 day)

| # | Task | Acceptance criteria |
|---|---|---|
| **T1** | **Model & state machine.** `JobId/GroupId/Class/Priority/JobState/Outcome/JobKey/JobSpec/JobError`; transition table as a pure function with exhaustive match. | Unit tests cover every legal transition and assert illegal ones are rejected (table-driven, all `(state, event)` pairs). Terminal states are sinks. |
| **T2** | **Cancel & pause primitives.** `CancelToken` re-export + hierarchy conventions; `PauseGate` (class-gate ∧ job-gate); `Interrupted`; `checkpoint()`. | Tests: child-token fan-out cancels members; `wait_if_paused` resolves on resume and errors on cancel-while-paused; `checkpoint()` hot path is 1 atomic load when running (asserted via a loop micro-bench, no await allocation). |
| **T3** | **Scheduler skeleton.** `Scheduler::new`, `spawn` for one lane, `JobHandle::{join, cancel, state, detach-on-drop}`, panic containment via `catch_unwind`. | A spawned job runs to `Done(Completed)`; a panicking job yields `Done(Failed)` + `JobError::Panicked` and the next job on the same worker runs fine; dropping the handle does not cancel. |
| **T4** | **Priority queues + lanes.** `BinaryHeap` with generation-stamped lazy entries; three lanes; dispatch tasks; `set_priority` / `set_priority_by_key`; cancel-while-queued. | Ordering test: jobs complete in priority order within a class under 1 permit; re-prioritizing a queued job reorders it (O(log n) push, stale entries skipped); cancel-while-queued never dispatches. |
| **T5** | **Concurrency budgets & load-shed.** Per-class semaphores from `JobsConfig`; `note_interactive_activity()` + window; Background shrink (permit starvation, never abort) + `should_yield()` checkpoint hint; Foreground-queued threshold shrink. | Deterministic test (`ManualScheduler`, paused time): with interactive activity noted, Background in-flight decays to `background_min…` as checkpoints release permits; after the window lapses budgets recover. No running job is ever aborted by throttling. |
| **T6** | **CPU & I/O bridges.** `fg-cpu`/`bg-cpu` rayon pools; `ctx.cpu(f)` routes by class, `ctx.io(f)` via `spawn_blocking`; cancel-aware at entry; panic containment inside pools. | A `cpu()` closure runs on the right pool (thread-name asserted); cancellation before entry returns `Interrupted` without running `f`; a panicking closure fails only its job. Tokio workers never execute the closure (thread assert). |
| **T7** | **Progress model.** `ProgressSink` atomics (items/bytes/total-growth/detail); `ProgressStyle`; fraction derivation incl. indeterminate + growing totals. | Property test: any interleaving of `advance`/`set_total` yields monotonic fraction within [0,1] once total is known; 1 M `advance` calls cost < 10 ms total (atomic-only hot path, criterion). |
| **T8** | **Groups & keyed dedupe.** `create_group`/`GroupHandle::close`; group aggregation of member progress/outcome; `spawn_keyed` in-flight map (key → handle) with terminal-state eviction; `cancel_group` fan-out. | Group entry reports (done, total) across members added while running; completes only after `close()` + all members terminal; two `spawn_keyed` with the same key run the work once, both handles resolve with the (Clone) result; re-spawn after completion runs again. |
| **T9** | **Activity snapshot & events.** Publisher task (dirty-flag + `snapshot_publish_hz`), `ArcSwap<ActivitySnapshot>`, linger/age-out, failed-entry linger + `Dismiss`; `broadcast` `JobEvent`s with lagged-receiver tolerance. | Snapshot reflects all state changes within 1 publish tick; `activity()` is one atomic load (no lock — asserted by API type, benched); 10 k rapid progress updates produce ≤ `hz`·duration snapshots; a slow `watch` subscriber cannot block the scheduler (lag drops, no backpressure into publisher). |
| **T10** | **Pause/resume surface.** Per-job pause (queued and running paths), `pause_class`/`resume_class` gates, permit release on pause, `pausable=false` no-op. | A running pausable job stops at its next checkpoint and its permit is re-issued to another job; class-pause halts all Background including subsequently-spawned jobs; resume re-queues paused-queued jobs at their original priority. |
| **T11** | **Core façade wiring.** `JobCommand` on the command bus (non-transactional, no `history_step`), `Session::{jobs, activity}`, `Event::Jobs` forwarding with throttle. | Headless `lightbox-core` test: spawn via a fake domain command → observe activity via query → cancel via `JobCommand` → event stream shows the transitions. No UI types anywhere in the chain (compile-time: `lightbox-jobs` has no shell deps). |
| **T12** | **`lightbox-cli jobs` + `--wait-idle`.** List/watch/cancel/pause/resume subcommands; `is_idle()`; wait-idle with timeout + non-zero exit on timeout. | E2E script: CLI spawns a synthetic 500-item group, `jobs list` shows aggregated progress, `jobs cancel` stops it, `--wait-idle` returns promptly after; exercised in CI on all three platforms. |
| **T13** | **Backpressure stage channels.** `stage::bounded`, cancel-aware `send`/`recv`, close semantics. | Test: a fast producer + slow consumer holds queue depth ≤ capacity (memory-bounded, asserted); cancelling the consumer unblocks a parked producer with `Interrupted`; upstream close drains then yields `None`. |
| **T14** | **Retry helper + failure surfacing.** `retry(policy, cancel, f)` with exponential backoff + jitter, cancel-aware between attempts; `ActivityEntry.error` population; failed-job linger. | Tests: N-flakes-then-success succeeds after N retries; cancel during backoff returns `Cancelled` immediately; a permanently failing job shows a user-facing `error` string in the snapshot until dismissed. |
| **T15** | **Reference adoption: M0 preview extraction.** Replace the skeleton's ad-hoc spawns (embedded-preview extraction from E01/E03 M0 code) with keyed Background jobs under a group; wire grid-visibility priority hooks (function exposed; E08 binds it later). | Importing the M0 test corpus produces one "Extracting previews" group entry with live progress; scrolling simulation (headless: re-prioritize a key set) changes completion order; cancel-group stops extraction; no regression in M0 exit-criteria tests. |
| **T16** | **Config plumbing.** `JobsConfig` defaults from core count; load from prefs store; `apply_config` live re-size (semaphore add/forget permits); documented rayon re-size limitation. | Test: shrinking `background_workers` live reduces in-flight as jobs finish (never aborts); growing takes effect immediately; config round-trips through the prefs store. |
| **T17** | **Shutdown & fault injection.** Drain protocol per §4.9; `ShutdownReport`; integration with session teardown ordering (jobs → catalog backup → close); `kill -9`-mid-jobs fault test. | Shutdown with a busy 200-job mix terminates within grace, `aborted` is empty when all jobs honor the checkpoint contract; a `kill -9` during a running import-simulation leaves the catalog `integrity_check`-clean (extends E01's fault harness). |
| **T18** | **Instrumentation, benches, docs.** `tracing` spans/counters; criterion benches (spawn overhead, checkpoint cost, snapshot publish at 1 k entries, progress hot path); rustdoc adoption guide (checkpoint contract, pause-safe resource rules, class-choice guidance, stage-channel idiom) for E03/E04/E13/E15 authors. | Benches meet §8 targets below and are wired into the nightly perf harness; `cargo doc` guide reviewed by one consumer-epic owner; debug watchdog (checkpoint > 1 s) present and test-mode-fatal. |

Order: T1→T2→T3 are strictly sequential; T4–T6 build the scheduler core; T7–T10 the observability/control surface; T11–T12 the façade; T13–T14 utilities; T15–T18 adoption/hardening. T13 can proceed in parallel any time after T2.

---

## 8. Test plan (per the §8 architecture strategy)

**Unit (PR-blocking):**
- State-machine table exhaustiveness (T1); cancel/pause primitive semantics incl. cancel-while-paused (T2); priority ordering + lazy re-prioritization (T4); dedupe/group aggregation (T8); panic containment (T3/T6); backpressure bounds (T13); retry/backoff (T14). Deterministic via `ManualScheduler` + paused tokio time — **no sleep-based assertions anywhere**.
- Property tests (`proptest`): progress monotonicity under arbitrary `advance`/`set_total` interleavings (T7); random command sequences (spawn/cancel/pause/resume/set-priority) never reach an illegal state transition and always quiesce to idle.

**Concurrency hygiene:** primitives are compositions of `tokio`/`tokio-util`/`std` sync types — no hand-rolled atomics beyond the checkpoint fast-path bit, which gets a targeted `loom` test (single model: pause-bit vs checkpoint race). CI runs the unit suite under `--cfg tokio_unstable` taskdump off; `cargo test` on the three-platform matrix (§8).

**Integration (PR-blocking fast subset; full nightly):**
- **10k-import simulation** (synthetic jobs, no real decode): Foreground copy group + Background preview group through a `stage::bounded(64)` seam. Assert: peak queued-item memory bounded by capacity; all jobs complete; activity snapshot sequence is monotonic and ends idle.
- **Visible-first:** enqueue 1 000 keyed Background jobs, re-prioritize a random 20-key "viewport"; assert those 20 complete before the median of the rest.
- **Preemption:** with Background saturated, start a Foreground burst; assert Foreground dispatch latency p95 < 150 ms (one checkpoint interval + margin) under `ManualScheduler` virtual time.
- **Pause-class:** pause Background mid-run → in-flight stops at checkpoints, none dispatch, Foreground unaffected; resume completes all.
- **CLI E2E:** the T12 script, on macOS/Windows/Linux CI.
- **Fault injection:** T17 `kill -9` mid-jobs → `integrity_check` clean (extends the E01 harness; PR-blocking per §8 "catalog crash safety").

**Performance (nightly, criterion + scenario harness; regression → tracked issue):**
- Spawn→dispatch overhead p50 < 100 µs, p99 < 1 ms (idle lanes).
- `checkpoint()` when running-unpaused: < 50 ns (atomic load + branch).
- Progress `advance()`: < 20 ns amortized (fits "10k raws report progress without measurable cost").
- Snapshot publish at 1 000 live entries: < 1 ms; `activity()` load: pointer-swap cost only.
- 10k-job synthetic import end-to-end scheduler overhead: < 1 s aggregate versus raw `tokio::spawn` baseline (guards the §7 "grid browsable < 60 s" budget from scheduler tax).

**Golden-image:** none — E06 owns no pixels.

---

## 9. Risks & open questions

| # | Risk / question | Exposure | Position |
|---|---|---|---|
| R1 | **Admission-granular preemption may not fully deliver "export never starves the canvas"** — the GPU half lives in E05's engine slicing, and E06 can only throttle CPU-side dispatch. | The §7 zero-starvation budget | Split responsibility is named (§2 non-goals, §6 seam). E06 ships its half (load-shed + budgets) with a measurable contract (Foreground p95 dispatch < 150 ms); the joint starvation test lands in E15×E05 integration. If checkpoint-interval preemption proves too coarse in practice, the bounded next step is smaller work quanta in the offending job kinds — a consumer-side fix, not a scheduler redesign. |
| R2 | **Cooperative contracts rot** — a domain epic ships a job that checkpoints rarely (or holds a write txn across a pause), and cancel/pause latency degrades invisibly. | Cancel latency, shutdown grace, pause correctness | Debug watchdog (test-mode-fatal) + the adoption guide (T18) + `ShutdownReport.aborted` treated as a CI bug signal. This is policed machinery, not hope. |
| R3 | **Priority inversion via shared resources** — a Background job holding the catalog writer handle while starved of permits could block a Foreground writer. | Catalog write path | Convention (documented T18): catalog writes are short single transactions through E01's dedicated writer task (§5.1) and never span a checkpoint; the writer queue is FIFO and fast. If profiling shows inversion, the named fix is a writer-queue priority tier — an E01-seam change, flagged, not silently done here. |
| R4 | **`spawn_keyed` result cloning** constrains job outputs to `Clone` (or `Arc`-wrapped). | API ergonomics for E03 | Accepted: preview-style outputs are naturally `Arc`'d handles/paths. Documented in the guide. |
| R5 | **Label localization.** `JobSpec.label` is a plain string; v1 ships English-only UI, and a later i18n pass would want message keys + params. | Future i18n rework touches every spawn site | Accepted for v1 (matches the catalog's v1 scope); `kind` is already the stable machine key, so a future mapping layer can key off it. Open question for the shell epic, not a blocker. |
| R6 | **Does E04 want resumable imports across restart?** E06 decided *no durable queue* (§2/§5). If E04's spec later requires resume-after-crash for a half-copied card, the journal belongs in `import_session` (E04's table) and E04 re-enqueues jobs on open — no E06 change. | Scope boundary | Position stated; flagged to the E04 planner. |
| R7 | **Interactive lane semantics drift** — E05's render scheduler might never need to spawn through E06, leaving the Interactive lane as dead accounting. | Small API surface waste | Cheap either way: the lane is ~zero code beyond the enum + a permit budget, and the class is load-bearing in the vocabulary (load-shed, activity accounting) even if dispatch through it stays rare. Revisit at E05 integration; removing dispatch support (keeping the class) is a one-day cleanup. |
| R8 | **Live rayon pool re-size is not supported** (rayon limitation); CPU-thread prefs apply on next session. | Prefs UX nit | Documented (T16); E08 panel labels the knob "takes effect after restart". |

---

## 10. Definition of done

- [ ] All 18 tasks merged with their acceptance criteria green on the three-platform CI matrix (macOS/Windows/Linux, §8).
- [ ] `lightbox-jobs` public API rustdoc-complete, with the adoption guide (checkpoint contract, pause-safety rules, class guidance, stage idiom) reviewed by at least one consumer-epic owner (E03/E04/E13/E15).
- [ ] The M0 embedded-preview extraction path runs on the scheduler (T15) and every M0 exit-criteria test still passes — the skeleton has no remaining ad-hoc background `tokio::spawn` for user-visible work.
- [ ] Activity is observable end-to-end headlessly: `lightbox-cli jobs list/watch` + `--wait-idle` work in the E2E harness; `JobCommand` cancel/pause/resume round-trips through the core command bus.
- [ ] The unified activity model satisfies the Must-tier feature: per-task cancel, per-task pause on pausable jobs, class-level pause for Background analysis — verifiable via CLI now, rendered by E08 later.
- [ ] Fault-injection: `kill -9` mid-jobs leaves the catalog `integrity_check`-clean (PR-blocking).
- [ ] Nightly perf benches wired and inside targets (§8 of this spec); scheduler overhead demonstrably negligible against the §7 import budget.
- [ ] `cargo-deny` green: no new dependency outside MIT/BSD/Apache/Zlib/Unicode (notably: **no** `priority-queue` crate); `lightbox-jobs` has no UI, wgpu, or catalog-schema dependencies.
- [ ] Seam contracts published in this doc's §6 acknowledged by the E05, E08, E13, E15 planners (a one-line ack in their specs or the tracking issue).
