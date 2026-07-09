<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E06 — Jobs & background task system — deviations log

Append-only. Every departure from the E06 spec
(`E06-jobs-background-system.md`) or a decision the spec left to the
implementer is recorded here with its rationale. Reference: CLAUDE.md
exit-bar rule ("Record spec deviations in
`docs/plan/epics/<EPIC>-deviations.md`") and the honest-reporting rule.

**E06 was implemented under the owner's no-testing-until-the-end policy**
(see `.tasks/e06-jobs-background/plan.md`): T1–T18's deliverables are
built and compile (`cargo build --workspace` green), but **none of the
per-task acceptance-criteria tests exist yet** — every commit on this
branch ends `[untested]`, and the final section of this file enumerates
exactly what the deferred cross-epic testing pass must verify. Do not read
this file (or the code) as claiming verified concurrency correctness.

---

## `CancelToken` stays the E01 hand-rolled token, not `tokio_util::sync::CancellationToken`

**What.** Spec §4.2 declares `pub type CancelToken =
tokio_util::sync::CancellationToken`. The crate instead keeps E01's
hand-rolled `cancel::CancelToken` (`Arc<AtomicBool>` + `Notify` + weak
child list) as the one cancellation type.

**Why.** The spec was written before E02–E05/E09 landed: the workspace has
since standardized on the E01 token — `lightbox-render` (E05's engine and
its `ng` scheduler, **out of E06's allowed scope**) calls
`CancelToken::child()` in production code (`ng/engine.rs:882`), and
`lightbox-decode`/`lightbox-ingest`/`lightbox-preview`/`lightbox-shell`
all consume it. `CancellationToken`'s equivalent method is spelled
`child_token()`, so the alias swap would not compile without editing
out-of-scope crates. Semantics are equivalent for every documented use
(hierarchical parent→child propagation, cooperative checks, `cancelled()`
awaitable, select-safe). `tokio-util` is therefore **not added** as a
dependency at all.

**Impact.** E05's "same token type" seam (§6) holds — better than the spec
hoped, since it already holds today. The final pass should consider a
targeted `loom`/race test of the hand-rolled token's `child()`-vs-`cancel()`
window (the code re-checks after registration; see `cancel.rs`), which the
spec assumed away by using tokio-util's proven code.

## `JobError` is the E01 enum (`Failed(String)`), not `Failed(anyhow::Error)`

**What.** Spec §4.3 sketches `JobError::Failed(#[from] anyhow::Error)`.
The scheduler reuses the existing `system::JobError` (`Cancelled`,
`Failed(String)`, `Panicked(String)`, `Shutdown`, `ResultTaken`) with a
`Clone` derive added (keyed-dedupe handles all observe one result).

**Why.** (a) The workspace error-taxonomy convention (E01 T4, restated in
the CLI: "library crates use `thiserror`; `anyhow` only in binaries") —
adding anyhow to a library crate would break that rule; (b) the enum is
already matched by name in `lightbox-core`, `lightbox-preview`, and
`lightbox-render` tests — a second `JobError` in the same crate root is
not expressible. Domain epics stringify at the seam, exactly as
`session.rs` already does everywhere (`Err(JobError::Failed(msg))`).

**Impact.** `ActivityEntry.error` carries the stringified summary the spec
wanted; `Clone` on error strings is cheap. No information a UI could
render is lost (typed domain errors stay inside domain crates by design).

## Root `JobHandle` now names the E06 scheduler handle; the E01 seed handle moved to `system::JobHandle`

**What.** The crate root re-exported the seed's `JobHandle` before; both
types cannot share the root name. `lightbox_jobs::JobHandle` is now
`sched::JobHandle<T>` (the spec's type); the seed's is reachable as
`lightbox_jobs::system::JobHandle` (the `system` module went `pub`).

**Why safe.** Grep-verified: no crate outside `lightbox-jobs` names the
seed's `JobHandle` in code (two doc comments in `lightbox-core` /
`lightbox-preview` reference it textually). The rest of the frozen E01
surface (`Class`, `JobConfig`, `JobError`, `JobSystem`, `CancelToken`)
keeps its root spelling; `Class` additionally gained
`PartialOrd`/`Ord` derives (spec §4.1 requires them; ordering documented
as declaration order = precedence order).

## `checkpoint()` hot path is two atomic loads, not one

**What.** Spec T2's AC says "checkpoint() hot path is 1 atomic load when
running". The implementation loads the pause-gate word **and** the cancel
flag (then returns) — two uncontended atomic loads, no allocation, no
await machinery.

**Why.** Fusing cancellation into the gate word would require every cancel
path to write it — but hierarchical cancellation can arrive from a parent
token (e.g. a group token, or a domain-owned parent) without passing
through the scheduler's control plane, so the fused bit could go stale and
a cancelled job would keep running. Correctness beat the micro-count. The
cost difference is unmeasurable at the contract's 100 ms checkpoint cadence
(both loads are uncontended acquire loads).

**Impact.** The deferred T2 micro-bench should assert the real budget
(< 50 ns per §8) rather than count loads.

## `JobsConfig` gained fields the spec's §4.8 sketch lacks

`interactive_workers` (the Interactive lane needs a number; spec calls it
"a large permit budget" without a knob — default `max(8, 2·cores)`),
`foreground_queue_shrink_threshold` (spec §4.4 names the
Foreground-backlog shrink behavior but no knob — default 4),
`event_capacity` (the `JobEvent` broadcast bound — default 1024), and
`watchdog_fatal` (the T18 "test mode: fails" switch — default false, and
watchdog violations are additionally counted in `SchedulerMetrics` so the
deterministic harness can assert on them without panicking machinery).
All additive; `JobsConfig` is `#[non_exhaustive]` with a `Default` built
from the core count, per the `CoreConfig` convention.

## `should_yield()` is advisory-only; no automatic re-queue

Spec §4.4/T5 says shrunk Background jobs "may voluntarily re-queue".
Implemented as the `should_yield()` hint (a gate-word bit flipped
edge-triggered when the Background lane enters/leaves shrink) on
`JobContext`/`PauseGate`; an *automatic* yield-and-requeue would require a
job body protocol for suspending mid-future (a consumer-side contract the
spec itself defers). Throttling is purely admission-granular: budget
shrink stops issuing slots and **never aborts a running job** — that
property is structural (`try_claim` is only consulted at dispatch/resume).

## Group progress aggregates member *completion*, not member item counts

Spec §4.5 says member jobs "roll their progress into the group entry";
T8's AC pins the observable: "group entry reports (done, total) across
members". Implemented as done = terminal members, total = max(members
added, `expected_total`) — a growing total until `close()`. Member-level
`ProgressSink` item/byte counts are **not** summed into the group view in
v1 (members of a group don't get their own entries, so per-item progress
inside a member is invisible until it finishes). For the canonical
consumers (import copy jobs, preview builds — one unit of work per member)
the two definitions coincide. If E04 wants byte-level group rollup for
multi-file copy members, it's an additive change to
`GroupRecord::aggregate_view`.

## Group `close()` vs concurrent member spawn: benign documented race

A `spawn(group=g)` that passes the group-resolution check while `close()`
lands can add one straggler member to a just-closed group; if the group
already finished, the straggler still runs, its terminal tick no-ops on
the completion latch, and the aggregate (done/total) counts it. No
unsoundness, no hang; tightening to a fully atomic membership/close
handshake is a final-pass candidate if the T8 table test demands it.
Spawning into a closed/unknown group otherwise refuses membership loudly
(warn + spawn ungrouped) rather than failing the spawn.

## Job-vs-group id namespaces overlap numerically

`JobId` and `GroupId` draw from separate counters (both start at 1), so a
raw number alone is ambiguous; `ActivityRef` is the disambiguated form
everywhere it matters (commands, activity entries, CLI `--id`/`--group`
flags). The spec implies as much; recorded because the CLI surface makes
the ambiguity user-visible.

## CLI: job state is process-local, so `jobs demo` is the E2E driver

Spec §4.7 sketches `jobs list/watch/cancel|pause|resume <id|class>` and a
global `--wait-idle`. All of those exist, but a fresh CLI invocation opens
a fresh session whose scheduler is empty — there is no cross-process job
observation (no durable queue, spec §2 non-goals; no IPC in scope). So:

- `jobs demo` (new, this epic) is the T12 scenario in one process: spawns
  a synthetic N-item Background group, streams the aggregated activity
  entry, optionally issues `Cancel(Group)`/`PauseClass`/`ResumeClass`
  **through the command bus** mid-run, and exits via wait-idle (non-zero
  exit on timeout).
- `list/watch/cancel/pause/resume` are wired through
  `Command::Jobs`/`Session::activity()` for symmetry and for any future
  long-lived headless mode, with the process-local caveat documented in
  the module doc.
- `--wait-idle` ships as the demo's flag + a reusable helper
  (`jobs.rs::wait_idle`) rather than a global flag on every subcommand;
  wiring it globally is deferred until the §8 E2E scripts
  (import → edit → export) exist to consume it.

## T15 adoption: the seam adopted is `BuildRuntime`, and member jobs are unkeyed

**What.** T15 says "replace the skeleton's ad-hoc spawns … with keyed
Background jobs under a group; wire grid-visibility priority hooks".
Implemented by replacing `lightbox-core`'s `TokioBuildRuntime` (raw
`Handle::spawn_blocking`, no class budget — its own doc comment named E06
as the upgrade) with `JobsBuildRuntime`: every preview build is a
`Class::Background` job under a rolling **"Extracting previews"** group
(opens on the first build of a burst, closes when the burst drains, so the
entry completes → lingers → ages out). `lightbox-preview` itself is
**untouched** (the `BuildRuntime` trait seam was designed for exactly this
swap; adoption-only, per the plan).

**Deviations within that:**

- **Member jobs are not keyed** at the jobs layer. Dedupe and
  visible-first ordering for preview builds happen *upstream* in E03's
  preview scheduler (`BuildKey` in-flight table; Visible > Neighbor >
  Bulk; `set_viewport`) before a build ever reaches `spawn_build` — keying
  again at the jobs layer would be a second copy of the same authority
  with nothing to dedupe (the upstream table guarantees one in-flight
  build per key). `Scheduler::spawn_keyed`/`set_priority_by_key` are the
  exposed, fully implemented hooks (T4/T8) for consumers that *do* keep
  ordering authority at the jobs layer; E08 binds them later.
- Consequently the T15 AC's "scrolling simulation re-prioritizes a key
  set and changes completion order" applies at the jobs layer to keyed
  synthetic jobs (deferred test), while on the real preview path the
  equivalent behavior lives in E03's scheduler (already covered by E03's
  own Phase D tests).
- `EmbeddedPreviewProvider`'s decode jobs (`JobSystem::spawn_blocking`,
  class-budgeted, deduped, cancellable since E01/E03 Phase B) were **not**
  re-plumbed: they are not "ad hoc `tokio::spawn`" work, and swapping the
  provider's constructor would ripple into `lightbox-preview`'s test
  suite, which this epic must not edit. Named as the E03-side follow-up
  when that crate next opens.

## `note_interactive_activity()` is exposed but not yet called by E05/E08

The seam function exists (spec §4.3) and the Background shrink machinery
consumes it, but wiring the render scheduler / shell input path to call it
means touching `lightbox-render`/`lightbox-shell` — both out of scope
(E08 runs concurrently in the shell). Left for E05/E08 integration; until
then the interactive-recency shrink is exercised only by direct calls
(tests, embedders).

## Interactive lane dispatches through the same queue machinery

Spec §4.4 calls the Interactive lane "effectively unqueued". Implemented
as the same heap/lane/dispatch-task structure with a generous budget
(`interactive_workers`) rather than a literal bypass — one code path, no
special cases, and R7 already flags that Interactive dispatch through E06
may stay rare. If the spawn→dispatch p50 < 100 µs bench (deferred) shows
the queue hop matters for Interactive, a bypass is a contained follow-up.

## Publisher emits per-entry `Progress` events, tracked publisher-side

Spec §4.5's `JobEvent::Progress { id }` is emitted per *changed entry* per
publish tick, detected via per-record progress epochs remembered in the
publisher task's loop state (`HashMap<ActivityRef, u64>`). Group epochs
fold member count + terminal count (not member sink epochs), matching what
the group view actually displays.

## Retry jitter uses `std::hash::RandomState`, not a `rand` dependency

`rand` is not in the pre-cleared dependency list; the jitter quality
needed (decorrelating retry herds) does not justify a new dependency. The
per-process random keys of `RandomState` provide a uniform-ish factor in
`[1 − j/2, 1 + j/2]`. Noted so nobody mistakes it for a seeded/
deterministic source: tests that need deterministic backoff should set
`jitter: 0.0`.

## `parking_lot` not added

Pre-cleared by the spec but unnecessary: the crate follows the house
convention (`std::sync::Mutex` + `PoisonError::into_inner`, as in
`lightbox-core`/`lightbox-preview`). One fewer dependency; the locks are
all short-critical-section registry/queue locks.

## `stage::send` reports a closed receiver as `Interrupted`

Spec §4.6's signature (`Result<(), Interrupted>`) leaves no channel for
"receiver gone"; semantically "downstream disappeared" means "stop
producing", which is what `Interrupted` tells a producer. The item is
dropped. Documented on the method; `try_send` retains the item-returning
`TrySendError` for callers that need it back.

## Shutdown cancels Interactive like Foreground; report counts all live jobs

Spec §4.9 names Background ("cancel all") and Foreground ("cancel queued,
signal running") but not Interactive. Interactive jobs get the Foreground
treatment (queued → immediate `Done(Cancelled)`, running → token +
checkpoint). `ShutdownReport` also gained a `failed` count (jobs whose
bodies finished with an error during the drain) beyond the spec's
`{completed, cancelled, aborted}` — the information was free and the
report is `#[non_exhaustive]`.

## T17's `kill -9` fault-injection extension: not implemented (it is a test)

T17's drain protocol, `ShutdownReport`, and session-teardown ordering
(scheduler drain → command-job drain → exit backup → close) are built. The
`kill -9`-mid-jobs harness extension is *test code* and therefore deferred
wholesale to the final pass (see rollup below), per the no-testing policy.

## T18's criterion benches: not implemented (they are measurements)

`tracing` spans (`job{id, kind, class}` around every body), state-change
trace events, `SchedulerMetrics` counters (spawned/completed/failed/
cancelled/watchdog violations/queue depth/claimed slots), the debug
watchdog (500 ms sweep, once-per-stall flagging, fatal-mode switch), the
`testing::ManualScheduler` harness, and the rustdoc adoption guide (crate
docs: checkpoint contract, pause-safety rules, class guidance, stage
idiom) are built. The criterion benches and nightly-harness wiring are
deferred with the rest of the measurement work.

## Prefs-store seam: `JobsConfig` defaults only (E08 Phase G not landed)

Spec §4.8 loads `JobsConfig` from the E01 prefs store. That store is being
built concurrently by E08 Phase G and did not exist in this worktree;
`CoreConfig::jobs_scheduler` is the documented seam field (defaults +
doc comment naming the E08 binding), and `Scheduler::apply_config` is the
live re-apply the panel will call. Round-trip-through-prefs is deferred to
E08 integration.

---

## Left for the final testing pass (exhaustive)

Everything below is **claimed by the implementation but verified by
nothing yet**. The spec's §7 AC column and §8 test plan are the source of
truth; this list restates them against what was actually built, plus
implementation-specific risks the tests must cover.

### Unit / property (per-task ACs)

- **T1** — table-driven exhaustive test over every `(FineState, Input)`
  pair of `model::transition` (11 legal edges; everything else
  `IllegalTransition`; `Done(_)` a sink for all five inputs).
- **T2** — child-token fan-out cancels members; `wait_if_paused` resolves
  on resume and errors on cancel-while-paused; checkpoint hot-path cost
  (< 50 ns; see the two-loads deviation above); notified-before-check
  wake-loss races on both `CancelToken::cancelled` and the pause gate.
- **T3** — spawn→`Done(Completed)`; a panicking async body yields
  `Done(Failed)` + `JobError::Panicked` and the worker survives (next job
  runs); dropping a `JobHandle` does not cancel; `join` on a
  never-dispatched cancelled job resolves `Err(Cancelled)` via the
  empty-slot mapping.
- **T4** — priority order under 1 permit; FIFO tiebreak within equal
  priority (seq-reversed heap `Ord` — assert directly too);
  re-prioritizing a queued job reorders it; stale-generation entries are
  skipped; cancel-while-queued never dispatches; `set_priority` on a
  paused-queued job takes effect at resume.
- **T5** — with interactive activity noted, Background in-flight decays to
  `background_min_during_interactive` as checkpoints/finishes release
  slots — **no running job is ever aborted by throttling**; budgets
  recover after the window (deadline wake actually fires!);
  Foreground-backlog shrink engages above the threshold and recovers on
  drain (the notify edges in `pop_next`/`on_terminal`); `should_yield`
  flips on shrink edges (including jobs dispatched mid-shrink).
  Deterministic via `ManualScheduler` + paused time.
- **T6** — `cpu()` runs on the right pool (thread-name asserted:
  `lightbox-fg-cpu-*`/`lightbox-bg-cpu-*`); never on a tokio worker;
  cancel-before-entry and cancel-while-queued-on-pool return `Interrupted`
  without running `f`; a panicking closure fails only its job (resume-
  unwind → wrapper containment) and the rayon worker survives; same for
  `io()` (spawn_blocking panic → `JobError::Panicked`).
- **T7** — property test: arbitrary `advance`/`set_total` interleavings
  keep the derived fraction in [0, 1]; totals may grow; known-zero total
  reads 1.0; 1 M `advance` calls < 10 ms; `set_total` on indeterminate is
  ignored.
- **T8** — group (done, total) across members added while running;
  completes only after `close()` + all members terminal; group outcome
  precedence (any failed → Failed; group-cancelled → Cancelled; else
  Completed); two `spawn_keyed` same key+kind run once, both handles
  resolve with the cloned result; keyed re-spawn after completion runs
  again; keyed map eviction on terminal; the keyed-admission serialization
  (concurrent spawn_keyed race); type-mismatch-on-key warns and runs
  separately; spawn into closed/unknown group refuses membership; the
  documented close-vs-spawn straggler race stays benign.
- **T9** — snapshot reflects state changes within one publish tick; 10 k
  rapid progress updates produce ≤ hz·duration snapshots (dirty-flag
  coalescing + min-gap); `activity()` is a lock-free pointer load; a slow
  `subscribe()` receiver lags without backpressuring the publisher;
  completed entries linger `completed_linger` then evict; failed entries
  linger until `dismiss`; group entries aggregate (members never appear
  individually) while counts include members; snapshot `seq` strictly
  increases; linger-expiry wakes the publisher without new dirt.
- **T10** — running pausable job stops at its next checkpoint and its slot
  is re-issued to another job (running counter proves it); pause on
  `pausable: false` is a no-op; class-pause halts subsequently-spawned
  jobs (born `PausedQueued`); resume re-queues paused-queued jobs at their
  original priority; job-bit vs class-bit independence (resume_class does
  not resume per-job-paused jobs and vice versa); cancel-while-paused
  (both flavors: parked-running via gate-wait error; paused-queued via
  immediate terminal); pause request cleared by resume before the next
  checkpoint leaves state untouched.
- **T13** — fast producer + slow consumer holds queue depth ≤ capacity;
  cancelling a parked producer unblocks with `Interrupted`; upstream close
  drains then yields `None`; consumer-side cancel unblocks `recv`;
  receiver drop causes producer `Interrupted`.
- **T14** — N-flakes-then-success succeeds after N retries; cancel during
  backoff returns `Cancelled` immediately (virtual-time, no real sleeps);
  `max_attempts` respected; backoff ceiling; jitter=0 determinism; a
  permanently failing job surfaces a user-facing `error` string in the
  snapshot until dismissed (via `RetryError → JobError::Failed`).

### Integration (spec §8)

- 10k-import simulation through `stage::bounded(64)`: peak queued memory
  bounded, all jobs complete, snapshot sequence monotonic, ends idle.
- Visible-first: 1 000 keyed Background jobs, re-prioritize a 20-key
  "viewport", those 20 complete before the median of the rest.
- Preemption: Background saturated → Foreground burst dispatch p95
  < 150 ms under `ManualScheduler` virtual time.
- Pause-class mid-run: in-flight stop at checkpoints, none dispatch,
  Foreground unaffected, resume completes all.
- **T11 headless chain**: spawn via a domain command → observe via
  `Session::activity()` → cancel via `Command::Jobs` → `Event::Jobs`
  stream shows the transitions; compile-level assertion that
  `lightbox-jobs` has no UI deps.
- **T12 CLI E2E** on the 3-platform matrix: `jobs demo --items 500`
  (aggregated progress visible), `--cancel-after` stops it,
  `--pause-class-after` pauses+resumes through the bus, wait-idle exits
  promptly / non-zero on timeout; `jobs list --json` schema sanity.
- **T15**: importing the M0 corpus produces one "Extracting previews"
  group entry with live progress; cancel-group stops extraction;
  class-pause(Background) pauses builds; **no regression in M0
  exit-criteria tests** (the whole existing suite must be re-run — this
  epic replaced the preview build runtime under it); burst group closes
  and ages out when the queue drains (and re-opens for the next burst).
- **T16**: shrinking `background_workers` live reduces in-flight only as
  jobs finish; growing takes effect immediately; `apply_config` hz/linger
  changes apply next tick; config round-trips through the prefs store
  once E08 Phase G lands.
- **T17**: shutdown with a busy 200-job mix terminates within grace with
  `aborted` empty when all jobs honor the checkpoint contract; a
  checkpoint-violating job IS aborted and reported; `kill -9` mid-jobs
  leaves the catalog `integrity_check`-clean (extend E01's fault harness);
  session-close ordering (scheduler drain → command drain → backup);
  spawn-after-shutdown is born cancelled and `join` resolves.
- **Event relay**: `Event::Jobs` arrives on the core bus for every
  scheduler event class; relay task ends at scheduler drop.

### Implementation-specific risks the tests must target (not in the spec's list)

- `finish_job` vs shutdown-abort force-terminal race (the terminal-state
  guard): exactly one `on_terminal` per job — `non_terminal` can never
  underflow; `is_idle` never goes true early.
- Slot accounting across the pause park/re-claim path (`holds_slot`
  swaps): no double-release, no leaked claim on cancel-during-park, on
  cancel-after-reclaim, and on scheduler-drop-during-park.
- `checkpoint_park` raced by `resume` before parking (wait returns
  immediately) and by `cancel` between the state flip and the gate wait.
- Dispatcher wake-loss: pushes/releases/config-changes landing between
  `notified()` creation and `.await` are never lost (both dispatch and
  resume-reclaim loops use notified-before-check).
- `dispatch_pass` (manual mode) equivalence with the background
  dispatcher's admission behavior.
- Publisher-loop termination on scheduler drop (dirty.close) and lane
  dispatcher termination on drop (lane.closed) — no leaked parked tasks.
- Group cancel fan-out reaching sub-stage tokens (member tokens are
  children of the group token).
- `JobsBuildRuntime` burst-group lifecycle under cancel-before-dispatch
  (guard drop without body run) — the group always closes; `live` never
  wedges above zero.
- Watchdog once-per-stall flagging and the `watchdog_fatal` counter path.
- `Session::close` drain bridge timeout path (runtime wedged → close
  proceeds with the warning, catalog still closes clean).

### Deferred measurements (spec §8 perf targets)

Spawn→dispatch p50 < 100 µs / p99 < 1 ms; checkpoint < 50 ns; progress
`advance` < 20 ns amortized; snapshot publish at 1 000 entries < 1 ms;
10k-job synthetic import scheduler overhead < 1 s vs raw `tokio::spawn`;
all as criterion benches wired into the nightly harness.

### Deferred quality gates (workspace exit bar)

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo fmt --all --check`, `cargo deny check` (expected clean:
the only new dependency is `arc-swap`, MIT/Apache-2.0; `priority-queue`
was explicitly NOT added; `rayon` was already licensed in-workspace), and
`cargo doc` review of the adoption guide by a consumer-epic owner
(E03/E04/E13/E15) per DoD.
