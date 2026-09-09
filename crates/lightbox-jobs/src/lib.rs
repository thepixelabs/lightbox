// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-jobs`, cancellable job system over a tokio multi-thread runtime.
//!
//! Owned by **E01** as a seed (spec §3.3); implemented in E01 Phase 4 (T14):
//! `Class` (Interactive/Foreground/Background semaphore budgets), `spawn` /
//! `spawn_blocking`, `JobHandle`, hierarchical `CancelToken`, bounded queues.
//! **E06** grows it (priority preemption, pause/resume, activity center);
//! the surface frozen here is E06's stated starting point.
//!
//! # Shape (spec §3.3, frozen surface for E06)
//!
//! - [`Class`], Interactive / Foreground / Background, each with its own
//!   semaphore budget: Interactive never queues behind Background.
//! - [`JobSystem::spawn`] / [`JobSystem::spawn_blocking`], async and sync
//!   work under a class budget, returning a [`JobHandle`].
//! - [`JobHandle`], `cancel` / `join` (async) / `try_result` (non-blocking
//!   poll for the UI).
//! - [`CancelToken`], hierarchical, cooperative cancellation
//!   (`JobError::Cancelled` is a normal outcome).
//! - Stages talk over ordinary **bounded** `tokio::sync::mpsc` channels; the
//!   seed adds no channel wrapper of its own.

mod cancel;
mod system;

pub use cancel::CancelToken;
pub use system::{Class, JobConfig, JobError, JobHandle, JobSystem};
