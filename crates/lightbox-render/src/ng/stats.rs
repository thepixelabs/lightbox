// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Engine stats + the recompute-count probe (spec §3.6; task **A15**).
//!
//! Owner: **A-core**. The recompute probe makes cache behavior *observable, not
//! inferred* — the B5 tail-invalidation gate and the R1/R5 discipline ride on
//! it. `tracing` spans per node eval are added here too.

use crate::ng::types::NodeId;

/// A snapshot of engine counters (spec §3.6 `Engine::stats`).
#[derive(Clone, Copy, Debug, Default)]
pub struct EngineStats {
    /// Nodes actually evaluated (the recompute count).
    pub nodes_evaluated: u64,
    /// Cache hits during the walk.
    pub cache_hits: u64,
    /// Cache misses during the walk.
    pub cache_misses: u64,
    /// Evictions performed to stay within budget.
    pub cache_evictions: u64,
    /// Resident VRAM bytes (pool accounting).
    pub vram_bytes: u64,
}

/// The recompute-count probe — atomic counters incremented by the executor and
/// cache, snapshot-able from tests (spec §3.6/A15). **A15 fills the internals.**
#[derive(Default)]
pub struct RecomputeProbe {}

impl RecomputeProbe {
    /// A zeroed probe.
    pub fn new() -> RecomputeProbe {
        RecomputeProbe::default()
    }

    /// A snapshot of the current counters.
    pub fn snapshot(&self) -> EngineStats {
        unimplemented!("A15 (A-core): RecomputeProbe::snapshot")
    }

    /// Record that `node` was evaluated (a miss recompute).
    pub fn note_eval(&self, node: NodeId) {
        let _ = node;
        unimplemented!("A15 (A-core): RecomputeProbe::note_eval")
    }

    /// Record a cache hit.
    pub fn note_cache_hit(&self) {
        unimplemented!("A15 (A-core): RecomputeProbe::note_cache_hit")
    }

    /// Reset the counters (per-render, for probe assertions).
    pub fn reset(&self) {
        unimplemented!("A15 (A-core): RecomputeProbe::reset")
    }
}
