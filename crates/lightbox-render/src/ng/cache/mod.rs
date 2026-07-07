// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Content-keyed node-output cache — `CacheKey`, `NodeCache` (spec §3.5).
//!
//! Owner: **A-core** owns the *type surface*; **B** owns the *real impl* (VRAM
//! tier LRU over [`TilePool`] handles, RAM pin tier, in-flight pinning,
//! byte-cost eviction, CacheKey derivation + propagation, integrity under
//! cancellation) — tasks B1–B8.
//!
//! Tail invalidation IS key propagation: a param change on node *k* yields new
//! keys for *k..n* while *1..k-1* stay hittable — there is no dirty-bit walk.

use crate::ng::node::param::ParamHash;
use crate::ng::node::KernelSalt;
use crate::ng::tile::TileHandle;
use crate::ng::types::{NodeId, ProcessVersion, TileCoord, TilePrecision};

/// A content-addressed node-output key (spec §3.5):
/// `H(node_id ‖ pv ‖ kernel_salt ‖ param_hash ‖ input_keys[] ‖ tile_coord ‖
/// scale_q ‖ precision)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey(pub blake3::Hash);

/// The ingredients hashed into a [`CacheKey`] (spec §3.5). Passed as a struct so
/// the derivation has one clean call site.
pub struct CacheKeyInputs<'a> {
    /// The node's identity.
    pub node_id: NodeId,
    /// The process version.
    pub pv: ProcessVersion,
    /// The node's kernel salt (WGSL/CPU algorithm rev).
    pub kernel_salt: KernelSalt,
    /// blake3 over the canonical params.
    pub param_hash: ParamHash,
    /// Upstream cache keys (the input tail).
    pub input_keys: &'a [CacheKey],
    /// The tile being produced.
    pub tile: TileCoord,
    /// Quantized render scale (1/64ths).
    pub scale_q: u16,
    /// Output tile precision.
    pub precision: TilePrecision,
}

impl CacheKey {
    /// Derives the content key from its ingredients (spec §3.5; task **B2**).
    pub fn derive(inputs: &CacheKeyInputs<'_>) -> CacheKey {
        let _ = inputs;
        unimplemented!("B2: CacheKey derivation over §3.5 ingredients")
    }
}

/// A byte-cost newtype for cache accounting (spec §3.5 `put(cost: Bytes)`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Bytes(pub u64);

/// Why a cache entry is RAM-pinned (spec §3.5 `pin_ram`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PinLabel {
    /// Post-`src.decoded` output — survives GPU cache eviction and device loss
    /// (task B4).
    SourceStage,
}

/// A cache hit: a pinned tile handle (spec §3.5 `get`).
#[derive(Clone, Debug)]
pub struct CachedTile {
    /// The cached tile; pinned while this value is held.
    pub tile: TileHandle,
}

/// Cache counters (spec §3.5 `stats`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Evictions performed to stay within budget.
    pub evictions: u64,
    /// Bytes currently resident.
    pub bytes: u64,
}

/// A two-tier node-output cache: a VRAM LRU tier + a RAM pin tier (spec §3.5).
/// **B fills the internals.** Correctness never depends on it (a crash loses
/// nothing).
#[derive(Default)]
pub struct NodeCache {}

impl NodeCache {
    /// An empty cache.
    pub fn new() -> NodeCache {
        NodeCache::default()
    }

    /// Looks up a key; pins the tile while the returned value is in flight.
    pub fn get(&self, k: &CacheKey) -> Option<CachedTile> {
        let _ = k;
        unimplemented!("B3: NodeCache::get — VRAM tier lookup + in-flight pin")
    }

    /// Inserts a tile at `cost`; LRU-evicts to stay within budget.
    pub fn put(&self, k: CacheKey, t: TileHandle, cost: Bytes) {
        let _ = (k, t, cost);
        unimplemented!("B3: NodeCache::put — byte-cost LRU insert")
    }

    /// Pins a key in the RAM tier under `label` (survives device loss).
    pub fn pin_ram(&self, k: CacheKey, label: PinLabel) {
        let _ = (k, label);
        unimplemented!("B4: NodeCache::pin_ram — RAM pin tier")
    }

    /// A snapshot of the cache counters.
    pub fn stats(&self) -> CacheStats {
        unimplemented!("B3: NodeCache::stats")
    }
}
