// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Content-keyed node-output cache — `CacheKey`, `NodeCache` (spec §3.5).
//!
//! Owner: **A-core** owns the *type surface*; **B** owns the *real impl* (VRAM
//! tier LRU over [`TilePool`](crate::ng::gpu::TilePool) handles, RAM pin tier,
//! in-flight pinning, byte-cost eviction, CacheKey derivation + propagation,
//! integrity under cancellation) — tasks B1–B8.
//!
//! Tail invalidation IS key propagation: a param change on node *k* yields new
//! keys for *k..n* while *1..k-1* stay hittable — there is no dirty-bit walk.
//!
//! # Two tiers
//!
//! - **VRAM tier** — a byte-cost LRU over pooled [`TileHandle`]s bounded by a
//!   configured budget. On [`NodeCache::put`] the least-recently-used entries
//!   are evicted until the resident byte total is within budget; an entry whose
//!   cost alone exceeds the budget is never admitted (so the budget invariant is
//!   never broken). [`NodeCache::get`] both serves a hit and marks the entry
//!   most-recently-used, so a tile just produced upstream is not evicted out
//!   from under a downstream node in the same walk.
//! - **RAM pin tier** — a small non-evictable map for entries that must survive
//!   VRAM eviction and device loss (the `src.decoded` source stage, task B4).
//!   Pinned bytes do not count against the VRAM budget.
//!
//! **In-flight pinning.** [`TileHandle`] is `Arc`-backed; a [`CachedTile`]
//! returned by `get` holds a strong reference, so the underlying tile stays
//! alive even if its map entry is later evicted — a render in flight can never
//! lose a tile it is still consuming.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::ng::node::param::ParamHash;
use crate::ng::node::KernelSalt;
use crate::ng::stats::RecomputeProbe;
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

/// Domain-separator tag: the precision lane folded into the key.
#[inline]
fn precision_tag(p: TilePrecision) -> u8 {
    match p {
        TilePrecision::F16 => 0x10,
        TilePrecision::F32 => 0x20,
    }
}

impl CacheKey {
    /// Derives the content key from its ingredients (spec §3.5; task **B2**).
    ///
    /// Fields are length-prefixed / fixed-width and separated by a tag byte so
    /// no concatenation of two distinct ingredient tuples can collide. The
    /// derivation is a pure function of its inputs: equal ingredients ⇒ equal
    /// key across processes and builds (rides on [`ParamHash`]'s canonical bytes
    /// and the stable [`KernelSalt`]).
    pub fn derive(inputs: &CacheKeyInputs<'_>) -> CacheKey {
        let mut h = blake3::Hasher::new();
        // node id (length-prefixed).
        let id = inputs.node_id.0.as_bytes();
        h.update(&(id.len() as u64).to_le_bytes());
        h.update(id);
        h.update(&[0x01]);
        // process version.
        h.update(&inputs.pv.0.to_le_bytes());
        h.update(&[0x02]);
        // kernel salt (32-byte blake3).
        h.update(inputs.kernel_salt.0.as_bytes());
        h.update(&[0x03]);
        // param hash (32-byte blake3).
        h.update(inputs.param_hash.0.as_bytes());
        h.update(&[0x04]);
        // upstream keys (count-prefixed; each 32 bytes, order-significant).
        h.update(&(inputs.input_keys.len() as u64).to_le_bytes());
        for k in inputs.input_keys {
            h.update(k.0.as_bytes());
        }
        h.update(&[0x05]);
        // tile coordinate + scale + precision.
        h.update(&inputs.tile.tx.to_le_bytes());
        h.update(&inputs.tile.ty.to_le_bytes());
        h.update(&inputs.tile.scale_q.to_le_bytes());
        h.update(&inputs.scale_q.to_le_bytes());
        h.update(&[precision_tag(inputs.precision)]);
        CacheKey(h.finalize())
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
    /// Bytes currently resident (VRAM tier only; pinned bytes excluded).
    pub bytes: u64,
}

/// The default VRAM byte budget when none is configured (512 MiB — a safe cap
/// for the CPU-reference path where no adapter memory can be probed; the engine
/// overrides it from [`crate::ng::EngineConfig::vram_budget`]).
pub const DEFAULT_VRAM_BUDGET: u64 = 512 * 1024 * 1024;

/// A VRAM-tier entry: the tile plus its byte cost and last-use tick.
struct VramEntry {
    tile: TileHandle,
    cost: u64,
    last_used: u64,
}

/// A RAM-pinned entry (non-evictable tier).
struct PinnedEntry {
    tile: TileHandle,
    #[allow(dead_code)] // label is diagnostic; eviction never inspects it
    label: PinLabel,
}

/// Mutable cache interior, guarded by one mutex.
#[derive(Default)]
struct Inner {
    /// VRAM tier: key → entry.
    vram: HashMap<CacheKey, VramEntry>,
    /// LRU index: last-use tick → key (smallest tick = least recently used).
    order: BTreeMap<u64, CacheKey>,
    /// Resident VRAM bytes.
    bytes: u64,
    /// Monotonic access clock.
    tick: u64,
    /// RAM pin tier: key → pinned entry (never evicted; bytes not budgeted).
    pinned: HashMap<CacheKey, PinnedEntry>,
    /// Counters.
    stats: CacheStats,
}

impl Inner {
    /// The next monotonic tick.
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Mark `k`'s VRAM entry most-recently-used.
    fn touch(&mut self, k: CacheKey) {
        let new = self.next_tick();
        if let Some(e) = self.vram.get_mut(&k) {
            self.order.remove(&e.last_used);
            e.last_used = new;
            self.order.insert(new, k);
        }
    }

    /// Evict least-recently-used VRAM entries until within `budget`.
    fn evict_to(&mut self, budget: u64) {
        while self.bytes > budget {
            let Some((&tick, &key)) = self.order.iter().next() else {
                // Nothing left to evict; the invariant holds because an entry
                // whose cost exceeds the budget is never admitted.
                break;
            };
            self.order.remove(&tick);
            if let Some(e) = self.vram.remove(&key) {
                self.bytes = self.bytes.saturating_sub(e.cost);
                self.stats.evictions += 1;
            }
        }
    }
}

/// A two-tier node-output cache: a VRAM LRU tier + a RAM pin tier (spec §3.5).
/// Correctness never depends on it (a crash loses nothing) — it only turns a
/// recompute into a lookup, and tail invalidation into key propagation.
pub struct NodeCache {
    inner: Mutex<Inner>,
    budget: u64,
    probe: Option<Arc<RecomputeProbe>>,
}

impl Default for NodeCache {
    fn default() -> Self {
        NodeCache::with_budget(DEFAULT_VRAM_BUDGET)
    }
}

impl NodeCache {
    /// An empty cache at the [`DEFAULT_VRAM_BUDGET`].
    pub fn new() -> NodeCache {
        NodeCache::default()
    }

    /// An empty cache with an explicit VRAM byte budget.
    pub fn with_budget(budget: u64) -> NodeCache {
        NodeCache {
            inner: Mutex::new(Inner::default()),
            budget: budget.max(1),
            probe: None,
        }
    }

    /// An empty cache with an explicit budget that mirrors its resident-byte
    /// gauge into a shared [`RecomputeProbe`] (so `Engine::stats` reports live
    /// VRAM usage). Hit/miss accounting stays with the executor, which decides
    /// hit vs miss during the walk; eviction totals are read from
    /// [`NodeCache::stats`].
    pub fn with_budget_and_probe(budget: u64, probe: Arc<RecomputeProbe>) -> NodeCache {
        NodeCache {
            inner: Mutex::new(Inner::default()),
            budget: budget.max(1),
            probe: Some(probe),
        }
    }

    /// The configured VRAM byte budget.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reflect the resident-byte gauge into the shared probe, if any.
    fn sync_gauge(&self, inner: &Inner) {
        if let Some(p) = &self.probe {
            p.set_vram_bytes(inner.bytes);
        }
    }

    /// Looks up a key, counting the outcome (hit/miss) and marking the entry
    /// most-recently-used. The returned [`CachedTile`] pins the tile while it is
    /// held (spec §3.5 `get`).
    pub fn get(&self, k: &CacheKey) -> Option<CachedTile> {
        let mut inner = self.lock();
        if let Some(p) = inner.pinned.get(k) {
            let tile = p.tile.clone();
            inner.stats.hits += 1;
            return Some(CachedTile { tile });
        }
        if inner.vram.contains_key(k) {
            inner.touch(*k);
            inner.stats.hits += 1;
            let tile = inner
                .vram
                .get(k)
                .expect("just checked present")
                .tile
                .clone();
            return Some(CachedTile { tile });
        }
        inner.stats.misses += 1;
        None
    }

    /// Looks up a key **without** touching the hit/miss counters — the source
    /// pre-check the engine uses to decide whether a `SourceProvider::fetch` is
    /// needed (task B4). Still marks the entry most-recently-used.
    pub fn peek(&self, k: &CacheKey) -> Option<CachedTile> {
        let mut inner = self.lock();
        if let Some(p) = inner.pinned.get(k) {
            return Some(CachedTile {
                tile: p.tile.clone(),
            });
        }
        if inner.vram.contains_key(k) {
            inner.touch(*k);
            let tile = inner
                .vram
                .get(k)
                .expect("just checked present")
                .tile
                .clone();
            return Some(CachedTile { tile });
        }
        None
    }

    /// Whether `k` is resident in either tier (no counter mutation).
    pub fn contains(&self, k: &CacheKey) -> bool {
        let inner = self.lock();
        inner.pinned.contains_key(k) || inner.vram.contains_key(k)
    }

    /// Inserts a tile at `cost`; LRU-evicts to stay within budget (spec §3.5
    /// `put`). An already-pinned key is left as-is (the RAM tier wins). An entry
    /// whose `cost` alone exceeds the budget is **not** admitted, so the budget
    /// invariant is never violated.
    pub fn put(&self, k: CacheKey, t: TileHandle, cost: Bytes) {
        let mut inner = self.lock();
        if inner.pinned.contains_key(&k) {
            return; // already resident in the non-evictable tier
        }
        let cost = cost.0;
        // Drop any prior VRAM entry for this key (a re-put refreshes cost/tile).
        if let Some(old) = inner.vram.remove(&k) {
            inner.order.remove(&old.last_used);
            inner.bytes = inner.bytes.saturating_sub(old.cost);
        }
        if cost > self.budget {
            // Can never fit without breaking the invariant — do not admit it.
            self.sync_gauge(&inner);
            return;
        }
        let tick = inner.next_tick();
        inner.vram.insert(
            k,
            VramEntry {
                tile: t,
                cost,
                last_used: tick,
            },
        );
        inner.order.insert(tick, k);
        inner.bytes += cost;
        let budget = self.budget;
        inner.evict_to(budget);
        self.sync_gauge(&inner);
    }

    /// Promotes an existing entry into the non-evictable RAM pin tier under
    /// `label` (spec §3.5 `pin_ram`; task B4). The pinned tile survives VRAM
    /// eviction and — on the CPU/host path — device loss. A no-op if the key is
    /// resident in neither tier.
    pub fn pin_ram(&self, k: CacheKey, label: PinLabel) {
        let mut inner = self.lock();
        if inner.pinned.contains_key(&k) {
            return;
        }
        // Prefer an existing VRAM entry; moving it out frees its budgeted bytes.
        if let Some(e) = inner.vram.remove(&k) {
            inner.order.remove(&e.last_used);
            inner.bytes = inner.bytes.saturating_sub(e.cost);
            inner.pinned.insert(
                k,
                PinnedEntry {
                    tile: e.tile,
                    label,
                },
            );
            self.sync_gauge(&inner);
        }
    }

    /// Directly pins a tile in the RAM tier (used when the engine wants to pin a
    /// freshly-fetched source stage without a prior `put`; task B4). Overwrites
    /// any VRAM entry for the same key.
    pub fn pin_tile(&self, k: CacheKey, t: TileHandle, label: PinLabel) {
        let mut inner = self.lock();
        if let Some(e) = inner.vram.remove(&k) {
            inner.order.remove(&e.last_used);
            inner.bytes = inner.bytes.saturating_sub(e.cost);
        }
        inner.pinned.insert(k, PinnedEntry { tile: t, label });
        self.sync_gauge(&inner);
    }

    /// Drops the entire VRAM tier (device loss: pooled textures are invalid),
    /// leaving the RAM pin tier intact — the re-warm primitive tasks E2/E3 build
    /// on. Resident-byte accounting resets (pinned bytes are unbudgeted).
    pub fn invalidate_vram(&self) {
        let mut inner = self.lock();
        inner.vram.clear();
        inner.order.clear();
        inner.bytes = 0;
        self.sync_gauge(&inner);
    }

    /// Empties both tiers (test/reset hook).
    pub fn clear(&self) {
        let mut inner = self.lock();
        *inner = Inner::default();
        self.sync_gauge(&inner);
    }

    /// A snapshot of the cache counters.
    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            bytes: inner.bytes,
            ..inner.stats
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::tile::{PixelBuf, PixelFormat};
    use crate::ng::types::Extent;

    fn key(seed: &[u8]) -> CacheKey {
        CacheKey(blake3::hash(seed))
    }

    fn tile(bytes: usize) -> TileHandle {
        // A CPU tile whose byte length is a known cost (1 px = 8 bytes @ f16).
        let px = bytes / 8;
        let w = px.max(1) as u32;
        TileHandle::from_cpu(PixelBuf::new_zeroed(
            PixelFormat::Rgba16F,
            Extent { w, h: 1 },
        ))
    }

    fn inputs(node: &'static str, salt: &[u8], params: &[u8], upstream: &[CacheKey]) -> CacheKey {
        CacheKey::derive(&CacheKeyInputs {
            node_id: NodeId(node),
            pv: ProcessVersion(1),
            kernel_salt: KernelSalt(blake3::hash(salt)),
            param_hash: ParamHash(blake3::hash(params)),
            input_keys: upstream,
            tile: TileCoord {
                tx: 0,
                ty: 0,
                scale_q: 64,
            },
            scale_q: 64,
            precision: TilePrecision::F16,
        })
    }

    // ── B1 / B2: derivation is a deterministic pure function ────────────────
    #[test]
    fn derive_is_deterministic_and_field_sensitive() {
        let base = inputs("test.gain", b"salt", b"params", &[]);
        // Identical ingredients ⇒ identical key.
        assert_eq!(base, inputs("test.gain", b"salt", b"params", &[]));
        // Each ingredient flips the key.
        assert_ne!(base, inputs("test.other", b"salt", b"params", &[]));
        assert_ne!(base, inputs("test.gain", b"salt2", b"params", &[]));
        assert_ne!(base, inputs("test.gain", b"salt", b"params2", &[]));
        assert_ne!(base, inputs("test.gain", b"salt", b"params", &[key(b"up")]));
    }

    #[test]
    fn salt_change_flips_the_key_b1() {
        // B1 gate: a kernel-salt change (same everything else) ⇒ a new key, so a
        // cached output can never be served for a changed kernel.
        let a = inputs("n", b"kernel@v1", b"p", &[]);
        let b = inputs("n", b"kernel@v2", b"p", &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn input_key_order_is_significant() {
        let u1 = key(b"u1");
        let u2 = key(b"u2");
        let ab = inputs("merge", b"s", b"p", &[u1, u2]);
        let ba = inputs("merge", b"s", b"p", &[u2, u1]);
        assert_ne!(ab, ba, "upstream key order must matter (port order)");
    }

    // ── B3: VRAM tier LRU, budget invariant, exact stats ────────────────────
    #[test]
    fn lru_evicts_least_recently_used_and_tracks_bytes() {
        let cache = NodeCache::with_budget(24); // room for 3 × 8-byte tiles
        let k = |i: u8| key(&[i]);
        cache.put(k(1), tile(8), Bytes(8));
        cache.put(k(2), tile(8), Bytes(8));
        cache.put(k(3), tile(8), Bytes(8));
        assert_eq!(cache.stats().bytes, 24);
        // Touch k(1) so k(2) becomes least-recently-used.
        assert!(cache.get(&k(1)).is_some());
        // A 4th insert must evict exactly one (k(2)).
        cache.put(k(4), tile(8), Bytes(8));
        assert_eq!(cache.stats().bytes, 24, "budget never exceeded");
        assert_eq!(cache.stats().evictions, 1);
        assert!(cache.contains(&k(1)), "recently-used survived");
        assert!(!cache.contains(&k(2)), "LRU victim evicted");
        assert!(cache.contains(&k(3)));
        assert!(cache.contains(&k(4)));
    }

    #[test]
    fn oversized_entry_is_never_admitted() {
        let cache = NodeCache::with_budget(16);
        cache.put(key(b"big"), tile(64), Bytes(64));
        assert_eq!(cache.stats().bytes, 0, "budget invariant preserved");
        assert!(!cache.contains(&key(b"big")));
    }

    #[test]
    fn budget_never_exceeded_under_randomized_fuzz() {
        // Deterministic PRNG (xorshift) — no dependency, reproducible.
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let budget = 200u64;
        let cache = NodeCache::with_budget(budget);
        for _ in 0..5_000 {
            let op = next() % 3;
            let k = key(&(next() % 40).to_le_bytes());
            match op {
                0 => {
                    let cost = 1 + (next() % 50); // 1..=50, never > budget
                    cache.put(k, tile((cost * 8) as usize), Bytes(cost));
                }
                1 => {
                    let _ = cache.get(&k);
                }
                _ => {
                    let _ = cache.peek(&k);
                }
            }
            assert!(
                cache.stats().bytes <= budget,
                "budget {} exceeded: {}",
                budget,
                cache.stats().bytes
            );
        }
    }

    #[test]
    fn stats_hits_and_misses_are_exact() {
        let cache = NodeCache::with_budget(1024);
        cache.put(key(b"a"), tile(8), Bytes(8));
        assert!(cache.get(&key(b"a")).is_some()); // hit
        assert!(cache.get(&key(b"a")).is_some()); // hit
        assert!(cache.get(&key(b"missing")).is_none()); // miss
        let s = cache.stats();
        assert_eq!(s.hits, 2);
        assert_eq!(s.misses, 1);
    }

    // ── B4: RAM pin tier survives VRAM eviction ─────────────────────────────
    #[test]
    fn pinned_entry_survives_eviction_and_is_unbudgeted() {
        let cache = NodeCache::with_budget(16);
        cache.put(key(b"src"), tile(8), Bytes(8));
        cache.pin_ram(key(b"src"), PinLabel::SourceStage);
        // Pinning frees the budgeted bytes (moved to the RAM tier).
        assert_eq!(cache.stats().bytes, 0);
        // Fill the VRAM tier past budget; the pin must not be touched.
        for i in 0..8u8 {
            cache.put(key(&[100 + i]), tile(8), Bytes(8));
        }
        assert!(cache.stats().bytes <= 16);
        assert!(cache.peek(&key(b"src")).is_some(), "pin survived eviction");
    }

    #[test]
    fn invalidate_vram_keeps_pins() {
        let cache = NodeCache::with_budget(1024);
        cache.put(key(b"v"), tile(8), Bytes(8));
        cache.pin_tile(key(b"p"), tile(8), PinLabel::SourceStage);
        cache.invalidate_vram();
        assert!(!cache.contains(&key(b"v")), "VRAM tier dropped");
        assert!(cache.contains(&key(b"p")), "RAM pin survived device loss");
        assert_eq!(cache.stats().bytes, 0);
    }
}
