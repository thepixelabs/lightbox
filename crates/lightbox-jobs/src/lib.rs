// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-jobs` — cancellable job system over a tokio multi-thread runtime.
//!
//! Owned by **E01** as a seed (spec §3.3); implemented in E01 Phase 4 (T14):
//! `Class` (Interactive/Foreground/Background semaphore budgets), `spawn` /
//! `spawn_blocking`, `JobHandle`, hierarchical `CancelToken`, bounded queues.
//! **E06** grows it (priority preemption, pause/resume, activity center);
//! the surface frozen here is E06's stated starting point.
//!
//! **Status: seed in progress.** [`CancelToken`] landed with E01 Phase 2 —
//! the render engine's frozen `SourceResolver`/`GpuCtx` surfaces (spec §3.4,
//! §3.5) carry it, so it could not wait for Phase 4. `JobSystem`, `Class`,
//! `spawn`/`spawn_blocking` and `JobHandle` arrive with Phase 4 (T14).

mod cancel;

pub use cancel::CancelToken;
