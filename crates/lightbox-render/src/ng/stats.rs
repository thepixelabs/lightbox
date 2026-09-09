// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Engine stats + the recompute-count probe (spec §3.6; task **A15**).
//!
//! Owner: **A-core**. The recompute probe makes cache behavior *observable, not
//! inferred*, the B5 tail-invalidation gate and the R1/R5 discipline ride on
//! it. `tracing` spans per node eval are emitted by the executor; the probe
//! carries the atomic counters those spans annotate.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::ng::types::NodeId;

/// A snapshot of engine counters (spec §3.6 `Engine::stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// The recompute-count probe, atomic counters incremented by the executor and
/// cache, snapshot-able from tests (spec §3.6/A15). Cheap to share (`Arc`).
#[derive(Default)]
pub struct RecomputeProbe {
    nodes_evaluated: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    cache_evictions: AtomicU64,
    vram_bytes: AtomicU64,
}

impl RecomputeProbe {
    /// A zeroed probe.
    pub fn new() -> RecomputeProbe {
        RecomputeProbe::default()
    }

    /// A snapshot of the current counters.
    pub fn snapshot(&self) -> EngineStats {
        EngineStats {
            nodes_evaluated: self.nodes_evaluated.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            cache_evictions: self.cache_evictions.load(Ordering::Relaxed),
            vram_bytes: self.vram_bytes.load(Ordering::Relaxed),
        }
    }

    /// Record that `node` was evaluated (a miss recompute). Emits a `tracing`
    /// event so per-node work is visible under `RUST_LOG=trace`.
    pub fn note_eval(&self, node: NodeId) {
        self.nodes_evaluated.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(node = %node, "render node evaluated");
    }

    /// Record a cache hit (upstream tile served without recompute).
    pub fn note_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss (a lookup that will drive a recompute).
    pub fn note_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record `n` cache evictions.
    pub fn note_evictions(&self, n: u64) {
        self.cache_evictions.fetch_add(n, Ordering::Relaxed);
    }

    /// Set the resident-VRAM-bytes gauge (pool accounting).
    pub fn set_vram_bytes(&self, bytes: u64) {
        self.vram_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Reset the counters (per-render, for probe assertions).
    pub fn reset(&self) {
        self.nodes_evaluated.store(0, Ordering::Relaxed);
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.cache_evictions.store(0, Ordering::Relaxed);
        // vram_bytes is a gauge, not a per-render counter, left intact.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_reset() {
        let p = RecomputeProbe::new();
        p.note_eval(NodeId("test.gain"));
        p.note_eval(NodeId("xform.display"));
        p.note_cache_hit();
        p.note_cache_miss();
        p.set_vram_bytes(4096);
        let s = p.snapshot();
        assert_eq!(s.nodes_evaluated, 2);
        assert_eq!(s.cache_hits, 1);
        assert_eq!(s.cache_misses, 1);
        assert_eq!(s.vram_bytes, 4096);

        p.reset();
        let s2 = p.snapshot();
        assert_eq!(s2.nodes_evaluated, 0);
        assert_eq!(s2.cache_hits, 0);
        // The VRAM gauge survives a per-render reset.
        assert_eq!(s2.vram_bytes, 4096);
    }
}
