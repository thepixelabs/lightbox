// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Job model (E06 spec §4.1, T1): ids, classes, priorities, states, keys,
//! specs — and the **state machine as a pure function** ([`transition`]),
//! the single source of truth every scheduler path routes through.
//!
//! The class vocabulary ([`Class`]) and error vocabulary ([`JobError`]) are
//! the E01 seed's own (`system.rs`, frozen surface) — E06 reuses rather than
//! duplicates them (see `E06-deviations.md`).

use std::borrow::Cow;
use std::num::NonZeroU64;

pub use crate::system::{Class, JobError};

/// Process-unique, monotonically increasing job id. Never reused within a
/// session (spec §4.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct JobId(pub NonZeroU64);

impl JobId {
    /// The raw id (CLI/display convenience).
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Process-unique group id (spec §4.1). Same allocation discipline as
/// [`JobId`] (they draw from separate counters; a `JobId` and a `GroupId`
/// may share a numeric value — [`ActivityRef`] is the disambiguated form).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct GroupId(pub NonZeroU64);

impl GroupId {
    /// The raw id (CLI/display convenience).
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Within-class ordering; **higher runs sooner** (spec §4.1). Re-assignable
/// while queued — visible-first scheduling raises the priority of on-screen
/// preview jobs via [`crate::Scheduler::set_priority_by_key`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Priority(pub i32);

/// The outcome half of [`JobState::Done`] (spec §4.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Outcome {
    /// The job body returned `Ok`.
    Completed,
    /// The job body returned a non-`Cancelled` error, or panicked.
    Failed,
    /// The job was cancelled (before dispatch, or observed cooperatively).
    Cancelled,
}

/// Public job state (spec §4.1). `Paused` collapses the machine's two
/// paused flavors (paused-while-queued vs paused-while-running) — the
/// distinction is a scheduler-internal resumption detail ([`FineState`]);
/// observers only need "not making progress, resumable".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState {
    /// Admitted, waiting for dispatch.
    Queued,
    /// Dispatched; the body is executing (or parked in `cpu()`/`io()`).
    Running,
    /// Paused (checkpoint-granular; holds no worker slot while paused).
    Paused,
    /// Cancel requested on a dispatched job; waiting for the body to observe
    /// it at a checkpoint (≤ ~100 ms per the checkpoint contract, §4.2).
    Cancelling,
    /// Terminal. No transitions out (T1: terminal states are sinks).
    Done(Outcome),
}

impl JobState {
    /// True for `Done(_)`.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Done(_))
    }
}

/// Dedupe key: namespaced string, e.g. `"preview.t1:<content_hash>"`
/// (spec §4.1). Two in-flight spawns with equal keys **and equal `kind`**
/// share one execution ([`crate::Scheduler::spawn_keyed`]).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct JobKey(pub Cow<'static, str>);

impl JobKey {
    /// Builds a key from anything string-like.
    pub fn new(key: impl Into<Cow<'static, str>>) -> JobKey {
        JobKey(key.into())
    }

    /// The key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a job reports progress (spec §4.1/§4.5). Totals may be unknown at
/// spawn and may **grow** later (import discovery) via
/// [`crate::ProgressSink::set_total`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProgressStyle {
    /// No meaningful fraction; the activity center shows a spinner.
    #[default]
    Indeterminate,
    /// Item-counted work (files imported, previews built).
    Items {
        /// Total items when known up front.
        total: Option<u64>,
    },
    /// Byte-counted work (copies, downloads).
    Bytes {
        /// Total bytes when known up front.
        total: Option<u64>,
    },
}

/// What to spawn (spec §4.1). Build via [`JobSpec::new`] and override
/// fields; `new` applies the spec's defaults (`pausable` defaults to
/// `class == Class::Background`).
#[derive(Clone, Debug)]
pub struct JobSpec {
    /// Machine-readable kind, namespaced: `"preview.build_t1"`,
    /// `"import.copy"`, `"export.render"`. The stable key for tooling,
    /// tracing, and (later) label localization (spec §9 R5).
    pub kind: &'static str,
    /// Human-readable label for the activity center. Presentation-neutral;
    /// the shell owns styling. English-only in v1 (spec §9 R5).
    pub label: String,
    /// Scheduling class (spec §5.3: Interactive ≻ Foreground ≻ Background).
    pub class: Class,
    /// Within-class priority (higher runs sooner).
    pub priority: Priority,
    /// Whether pause/resume applies. Defaults to `class == Background`;
    /// pausable Foreground jobs are allowed (export).
    pub pausable: bool,
    /// Aggregating group, if any (spec §4.5 groups).
    pub group: Option<GroupId>,
    /// Dedupe key: an identical in-flight `(kind, key)` returns the existing
    /// handle instead of spawning ([`crate::Scheduler::spawn_keyed`]).
    pub key: Option<JobKey>,
    /// Progress reporting style.
    pub progress: ProgressStyle,
}

impl JobSpec {
    /// A spec with the defaults the E06 spec names: default priority,
    /// `pausable = (class == Background)`, no group, no key, indeterminate
    /// progress.
    pub fn new(kind: &'static str, label: impl Into<String>, class: Class) -> JobSpec {
        JobSpec {
            kind,
            label: label.into(),
            class,
            priority: Priority::default(),
            pausable: class == Class::Background,
            group: None,
            key: None,
            progress: ProgressStyle::default(),
        }
    }
}

/// A job or a group — the activity model's entry identity and the command
/// surface's target vocabulary (spec §4.5/§4.7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ActivityRef {
    /// One job.
    Job(JobId),
    /// One group (aggregating its member jobs).
    Group(GroupId),
}

impl std::fmt::Display for ActivityRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivityRef::Job(id) => write!(f, "job:{id}"),
            ActivityRef::Group(id) => write!(f, "group:{id}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The state machine (spec §4.1, T1) — a pure function with exhaustive match.
// ---------------------------------------------------------------------------

/// Scheduler-internal fine-grained state. Public so the (deferred) T1 table
/// tests can enumerate the full `(state, input)` domain; everything outside
/// the scheduler observes the collapsed [`JobState`] via
/// [`FineState::public`].
///
/// The two paused flavors carry the resumption target the public state
/// erases: `PausedQueued` resumes to `Queued` (at its original priority),
/// `PausedRunning` resumes to `Running` (after re-acquiring a worker slot).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FineState {
    /// Admitted, in a lane queue.
    Queued,
    /// Dispatched, body executing.
    Running,
    /// Paused before it ever dispatched (holds no slot; skipped by lanes).
    PausedQueued,
    /// Paused at a checkpoint (its worker slot was released).
    PausedRunning,
    /// Cancel requested on a dispatched job; awaiting cooperative
    /// observation.
    Cancelling,
    /// Terminal sink.
    Done(Outcome),
}

impl FineState {
    /// The collapsed public view.
    pub fn public(self) -> JobState {
        match self {
            FineState::Queued => JobState::Queued,
            FineState::Running => JobState::Running,
            FineState::PausedQueued | FineState::PausedRunning => JobState::Paused,
            FineState::Cancelling => JobState::Cancelling,
            FineState::Done(o) => JobState::Done(o),
        }
    }

    /// True for `Done(_)`.
    pub fn is_terminal(self) -> bool {
        matches!(self, FineState::Done(_))
    }
}

/// The events that drive [`transition`]. Each corresponds to exactly one
/// scheduler code path (named in the variant docs) — control-plane calls
/// that do NOT change state (e.g. `pause()` on a *running* job, which only
/// sets the gate bit until the body's next checkpoint) do not route through
/// the machine at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    /// A lane dispatched the job (popped + slot acquired).
    Dispatch,
    /// Pause took effect: on `Queued` immediately (control plane); on
    /// `Running` at the body's next checkpoint (`PauseObserved` semantics —
    /// the checkpoint applies it, spec §4.1 "takes effect at next
    /// checkpoint").
    Pause,
    /// Resume took effect: `PausedQueued` re-queues (control plane);
    /// `PausedRunning` re-enters `Running` once the parked checkpoint
    /// re-acquires a worker slot.
    Resume,
    /// Cancel requested. On never-dispatched states this is immediately
    /// terminal (the body never ran — nothing to observe); on dispatched
    /// states it enters `Cancelling` until the body observes the token.
    Cancel,
    /// The body returned (or the wrapper contained its panic). Carries the
    /// outcome: `Ok` ⇒ `Completed`; `Err(Cancelled)` ⇒ `Cancelled` (this is
    /// the "job observes" edge of spec §4.1's `Cancelling` row); any other
    /// error or a panic ⇒ `Failed`.
    Finish(Outcome),
}

/// A `(state, input)` pair the machine rejects (T1: illegal transitions are
/// errors, terminal states are sinks). Call sites either guard (`cancel` on
/// a `Done` job is a caller-visible no-op) or treat an occurrence as a
/// scheduler bug (debug-asserted).
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("illegal job-state transition: {state:?} on {input:?}")]
pub struct IllegalTransition {
    /// The state the machine was in.
    pub state: FineState,
    /// The rejected input.
    pub input: Input,
}

/// The transition table (spec §4.1), exhaustively matched. Every scheduler
/// state change goes through here — there is no second copy of these rules.
///
/// ```text
/// Queued        ─Dispatch─► Running
/// Queued        ─Pause────► PausedQueued
/// Queued        ─Cancel───► Done(Cancelled)      (immediate; never dispatched)
/// Running       ─Pause────► PausedRunning        (applied at the checkpoint)
/// Running       ─Cancel───► Cancelling
/// Running       ─Finish(o)► Done(o)
/// PausedQueued  ─Resume───► Queued               (original priority)
/// PausedQueued  ─Cancel───► Done(Cancelled)      (immediate; never dispatched)
/// PausedRunning ─Resume───► Running              (after slot re-acquire)
/// PausedRunning ─Cancel───► Cancelling           (parked checkpoint wakes ⇒ observes)
/// Cancelling    ─Finish(o)► Done(o)              (o = Cancelled when observed;
///                                                 Completed/Failed when the body
///                                                 finished before observing)
/// Done(_)       ─ anything ► IllegalTransition   (terminal sink)
/// ```
pub fn transition(state: FineState, input: Input) -> Result<FineState, IllegalTransition> {
    use FineState as S;
    use Input as I;
    match (state, input) {
        (S::Queued, I::Dispatch) => Ok(S::Running),
        (S::Queued, I::Pause) => Ok(S::PausedQueued),
        (S::Queued, I::Cancel) => Ok(S::Done(Outcome::Cancelled)),
        (S::Running, I::Pause) => Ok(S::PausedRunning),
        (S::Running, I::Cancel) => Ok(S::Cancelling),
        (S::Running, I::Finish(o)) => Ok(S::Done(o)),
        (S::PausedQueued, I::Resume) => Ok(S::Queued),
        (S::PausedQueued, I::Cancel) => Ok(S::Done(Outcome::Cancelled)),
        (S::PausedRunning, I::Resume) => Ok(S::Running),
        (S::PausedRunning, I::Cancel) => Ok(S::Cancelling),
        (S::Cancelling, I::Finish(o)) => Ok(S::Done(o)),
        // Everything else — including every input on Done(_) — is illegal.
        (
            state @ (S::Queued | S::Running | S::PausedQueued | S::PausedRunning),
            input @ (I::Dispatch | I::Pause | I::Resume | I::Finish(_)),
        ) => Err(IllegalTransition { state, input }),
        (state @ S::Cancelling, input @ (I::Dispatch | I::Pause | I::Resume | I::Cancel)) => {
            Err(IllegalTransition { state, input })
        }
        (state @ S::Done(_), input) => Err(IllegalTransition { state, input }),
    }
}
