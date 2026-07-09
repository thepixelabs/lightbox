// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-jobs` — the class-prioritized, pausable, cancellable job
//! scheduler (E06), grown from the E01 seed.
//!
//! Everything expensive in Lightbox is a **cancellable job**; the UI thread
//! never blocks. This crate owns that sentence's implementation (E06 spec
//! §1): the [`Scheduler`] (three lanes — Interactive ≻ Foreground ≻
//! Background — priority queues, concurrency budgets, load-shed), the
//! cancel/pause primitives ([`CancelToken`], [`PauseGate`],
//! [`JobContext::checkpoint`]), the activity-center backing model, and the
//! backpressure/retry utilities the domain epics (E03 previews, E04 import,
//! E13 ML, E15 export) build their jobs on. E06 is deliberately
//! **domain-free**: it schedules opaque futures.
//!
//! # Two layers, one vocabulary
//!
//! - **E01 seed (frozen):** [`JobSystem`] — the app's tokio runtime with
//!   per-class semaphore budgets, [`JobSystem::spawn`]/`spawn_blocking`,
//!   [`system::JobHandle`]. Still consumed by `lightbox-render`'s scheduler
//!   and remains supported; it owns the runtime the E06 [`Scheduler`]
//!   dispatches onto (`Scheduler::new` takes [`JobSystem::handle`]).
//! - **E06 scheduler:** [`Scheduler`] — job model with a real state machine
//!   ([`JobState`]), within-class [`Priority`] with visible-first
//!   re-prioritization, pause/resume, keyed dedupe, groups, progress, the
//!   [`ActivitySnapshot`] the shell polls, and graceful shutdown.
//!
//! [`Class`], [`CancelToken`], and [`JobError`] are shared by both layers —
//! one vocabulary across the workspace (spec §5.3: the same token type is
//! threaded through every job AND the render scheduler).
//!
//! # Adoption guide (for E03/E04/E13/E15 job authors)
//!
//! See the crate-level adoption guide in [`sched`]'s module docs plus:
//!
//! - **Checkpoint contract** ([`JobContext::checkpoint`]): reach a
//!   cancel-aware await at least every **100 ms** of wall time.
//! - **Pause safety**: never hold a non-reentrant external resource (write
//!   transaction, exclusive file lock) across a checkpoint — a paused job
//!   holds no scheduler resources and may stay parked indefinitely.
//! - **Class choice**: Interactive = the user is waiting on it this
//!   frame/second; Foreground = the user asked and watches progress;
//!   Background = the app decided (spec §4.1).
//! - **Stage idiom** (`stage::bounded`): every pipeline-stage boundary
//!   uses the same cancel-aware bounded channel instead of a hand-rolled
//!   one.

mod cancel;
mod config;
mod cpu;
pub mod model;
pub mod sched;
pub mod system;
mod token;

pub use cancel::CancelToken;
pub use config::JobsConfig;
pub use model::{
    ActivityRef, Class, FineState, GroupId, IllegalTransition, Input, JobError, JobId, JobKey,
    JobSpec, JobState, Outcome, Priority, ProgressStyle,
};
pub use sched::{JobContext, JobHandle, Scheduler};
pub use token::{Interrupted, PauseGate};

// E01 seed (frozen surface): still exported at the root under the seed's
// own names — EXCEPT `JobHandle`, which E06's scheduler handle now owns at
// the root (no code outside this crate named the seed's handle type;
// `system::JobHandle` remains available — see `E06-deviations.md`).
pub use system::{JobConfig, JobSystem};
