// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! CPU bridges (E06 spec §4.4, T6): two dedicated rayon pools — `fg-cpu`
//! and `bg-cpu` — so decode/encode/checksum kernels never starve the tokio
//! reactor. [`crate::JobContext::cpu`] routes by job class; Interactive CPU
//! work rides the `fg-cpu` pool (the render engine's own `eval_cpu` pool is
//! E05's, not E06's — spec §4.4).
//!
//! **R8 (spec §9): pools are fixed at construction** — `apply_config`
//! re-sizing does not reach them; `fg_cpu_threads`/`bg_cpu_threads` changes
//! apply on the next session.

use crate::config::JobsConfig;
use crate::model::Class;

/// The two class-matched rayon pools.
pub(crate) struct CpuPools {
    fg: rayon::ThreadPool,
    bg: rayon::ThreadPool,
}

/// What a bridged closure sends back: the value, or the contained panic
/// payload (re-thrown on the job task so the job — and only the job —
/// fails as `Panicked`; the pool worker survives).
pub(crate) enum CpuOutcome<R> {
    Ok(R),
    /// The closure never ran (cancelled while queued on the pool).
    Interrupted,
    Panicked(Box<dyn std::any::Any + Send>),
}

impl CpuPools {
    /// Builds both pools. Thread names are load-bearing for the (deferred)
    /// T6 test: `lightbox-fg-cpu-<i>` / `lightbox-bg-cpu-<i>`.
    ///
    /// # Panics
    ///
    /// If a pool cannot be built (threads cannot be spawned) — a
    /// startup-fatal condition, same posture as [`crate::JobSystem::new`].
    pub(crate) fn new(cfg: &JobsConfig) -> CpuPools {
        let build = |name: &'static str, threads: usize| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads.max(1))
                .thread_name(move |i| format!("lightbox-{name}-{i}"))
                .build()
                .expect("failed to build the jobs CPU pool")
        };
        CpuPools {
            fg: build("fg-cpu", cfg.fg_cpu_threads),
            bg: build("bg-cpu", cfg.bg_cpu_threads),
        }
    }

    /// The pool for a job class: Background → `bg-cpu`; Foreground and
    /// Interactive → `fg-cpu`.
    pub(crate) fn pool(&self, class: Class) -> &rayon::ThreadPool {
        match class {
            Class::Interactive | Class::Foreground => &self.fg,
            Class::Background => &self.bg,
        }
    }
}

impl std::fmt::Debug for CpuPools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPools")
            .field("fg_threads", &self.fg.current_num_threads())
            .field("bg_threads", &self.bg.current_num_threads())
            .finish()
    }
}
