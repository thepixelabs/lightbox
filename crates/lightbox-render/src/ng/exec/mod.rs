// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The executor — topo walk, ticket store, tile scheduler, priority lanes
//! (spec §2/§3; tasks **A9**/**A12**/**A14**, C, C9).
//!
//! Owner: **A-core** owns [`mod@self`] (topo walk + ticket store,
//! scheduler-facing) and [`cpu`] (rayon tile executor); **A-gpu** owns [`gpu`]
//! (encoder mgmt, dispatch, readback). [`backend`] is the **frozen** seam.
//! **C** owns the tiling / ROI / progressive / priority-lane work layered on
//! top (tasks C1–C10).
//!
//! # v1 walk (this task)
//!
//! [`Executor::evaluate`] topologically orders the [`RenderGraph`], evaluates
//! each node once over the whole request ROI (one tile — **C** layers 256²
//! tiling + apron on top), threading upstream tile handles forward and the
//! [`CancelToken`] to every node boundary. Every eval bumps the
//! [`RecomputeProbe`] so cache behavior is observable (the B5 gate rides on
//! this). Content-keyed caching is task **B**; the v1 walk recomputes every
//! node (`cache` is accepted but not yet consulted).

pub mod backend;
pub mod budget;
pub mod cpu;
pub mod gpu;
pub mod priority;
pub mod progressive;
pub mod roi;
pub mod scale;
pub mod tiling;

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_jobs::CancelToken;

use crate::ng::cache::{Bytes, CacheKey, CacheKeyInputs, NodeCache};
use crate::ng::engine::BackendId;
use crate::ng::error::RenderError;
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::node::CachePolicy;
use crate::ng::stats::RecomputeProbe;
use crate::ng::tile::TileHandle;
use crate::ng::types::{ProcessVersion, RenderScale, Roi, TileCoord, TilePrecision};

pub use backend::{Backend, BackendEvalRequest};

/// The engine default tile edge (spec §4.3).
pub const DEFAULT_TILE_SIZE: u32 = 256;

/// The engine-owned source-stage injection for a walk: the graph node index of
/// `src.decoded`, the uploaded/decoded source tile, and the **content key** the
/// engine derived for it (image-discriminated + scale-independent, task B4).
/// The walk keys the source node with `key` verbatim and propagates it to the
/// downstream tail, so a source that is already resident is served from the
/// cache without re-evaluating the identity copy.
#[derive(Clone)]
pub struct SourceInject {
    /// The `src.decoded` node index in the compiled graph.
    pub idx: NodeIndex,
    /// The uploaded/decoded working tile to inject as its input.
    pub tile: TileHandle,
    /// The content key the engine derived for the source stage.
    pub key: CacheKey,
}

/// Drives a compiled [`RenderGraph`] to a rendered output: topo walk over the
/// nodes, per-node evaluation via a [`Backend`], and (task C) visible-first
/// tiling + progressive ladders + priority lanes.
pub struct Executor {
    backend: Arc<dyn Backend>,
    tile_size: u32,
    probe: Arc<RecomputeProbe>,
}

impl Executor {
    /// An executor driving `backend` (default tile size, fresh probe).
    pub fn new(backend: Arc<dyn Backend>) -> Executor {
        Executor {
            backend,
            tile_size: DEFAULT_TILE_SIZE,
            probe: Arc::new(RecomputeProbe::new()),
        }
    }

    /// An executor sharing an existing recompute probe (so the engine can
    /// snapshot the same counters) with an explicit tile size.
    pub fn with_probe(
        backend: Arc<dyn Backend>,
        tile_size: u32,
        probe: Arc<RecomputeProbe>,
    ) -> Executor {
        Executor {
            backend,
            tile_size: tile_size.max(1),
            probe,
        }
    }

    /// The shared recompute probe (snapshot for [`crate::ng::Engine::stats`]).
    pub fn probe(&self) -> &Arc<RecomputeProbe> {
        &self.probe
    }

    /// The configured tile edge, pixels.
    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }

    /// Which backend this executor drives.
    pub fn backend_kind(&self) -> BackendId {
        self.backend.kind()
    }

    /// Evaluate `graph` under `pv` over `roi` at `scale`, threading `cache` and
    /// `cancel` through the topo walk; returns the terminal node's output tile.
    ///
    /// # Content-keyed caching + tail invalidation (task B2/B5)
    ///
    /// Each node's [`CacheKey`] is derived from `(node_id, pv, kernel_salt,
    /// param_hash, upstream keys, tile, scale, precision)` (spec §3.5) and
    /// propagated forward: a node's key folds in its inputs' keys, so a param
    /// change on node *k* re-keys exactly *k..n* while *1..k-1* stay hittable —
    /// **tail invalidation is key propagation, not a dirty-bit walk.** A hit
    /// serves the cached tile and skips the eval (and the recompute probe's
    /// `note_eval`); a miss evaluates and inserts the full tile. Because the
    /// insert happens **only after a successful eval**, a cancelled or failed
    /// node never poisons the cache with a partial tile (task B7).
    ///
    /// `source` optionally pre-populates the engine-owned source stage: it names
    /// the graph node (`src.decoded`) whose single input is the uploaded/decoded
    /// working tile and carries the content key the engine derived for it.
    /// `src.decoded` declares zero graph inputs but consumes the source as its
    /// one input (see `nodes::decoded`); graphs without a source stage (probe
    /// generators like `test.const`) pass `None` and generate their own root.
    ///
    /// The v1 walk evaluates each node once over the whole ROI as a single tile
    /// (`TileCoord{0,0,scale_q}`); **C** layers 256² tiling + apron over the same
    /// per-node keying (each tile keyed by its own [`TileCoord`]).
    // The render context is intentionally flat here (graph + pv + roi + scale +
    // cache + cancel + source): it is the frozen exec↔cache/compile seam (R8),
    // clearer as positional args than wrapped in a one-off struct.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate(
        &self,
        graph: &RenderGraph,
        pv: ProcessVersion,
        roi: Roi,
        scale: RenderScale,
        cache: &NodeCache,
        cancel: &CancelToken,
        source: Option<SourceInject>,
    ) -> Result<TileHandle, RenderError> {
        let topo = graph.topo_order()?;
        let scale_f = scale_ratio(scale);
        let scale_q = quantize_scale(scale_f);
        // v1: the whole ROI is one tile; C keys per 256² tile coordinate.
        let tile_coord = TileCoord {
            tx: 0,
            ty: 0,
            scale_q,
        };
        let mut outputs: HashMap<NodeIndex, TileHandle> = HashMap::with_capacity(topo.len());
        let mut keys: HashMap<NodeIndex, CacheKey> = HashMap::with_capacity(topo.len());

        for idx in topo {
            if cancel.is_cancelled() {
                return Err(RenderError::Cancelled);
            }
            let node = graph.node(idx);
            let node_id = node.descriptor().id;
            let _span = tracing::debug_span!("eval_node", node = %node_id).entered();

            let is_source = source.as_ref().is_some_and(|s| s.idx == idx);

            // Gather upstream tiles + their content keys (in port order). The
            // source stage has no graph inputs; its "input" is the injected tile
            // and its key is supplied by the engine.
            let (input_tiles, input_keys): (Vec<TileHandle>, Vec<CacheKey>) = if is_source {
                (Vec::new(), Vec::new())
            } else {
                let input_idxs = graph.inputs_of(idx);
                let mut tiles = Vec::with_capacity(input_idxs.len());
                let mut ikeys = Vec::with_capacity(input_idxs.len());
                for src in input_idxs {
                    let tile = outputs.get(&src).cloned().ok_or_else(|| {
                        RenderError::Internal(format!(
                            "node {node_id}: upstream output for {src:?} not yet evaluated"
                        ))
                    })?;
                    let k = *keys.get(&src).ok_or_else(|| {
                        RenderError::Internal(format!(
                            "node {node_id}: upstream key for {src:?} not yet derived"
                        ))
                    })?;
                    tiles.push(tile);
                    ikeys.push(k);
                }
                (tiles, ikeys)
            };

            let params = graph.params(idx);
            let precision = node.precision();

            // Derive this node's content key and record it for downstream nodes.
            let key = if is_source {
                source.as_ref().expect("is_source implies Some").key
            } else {
                CacheKey::derive(&CacheKeyInputs {
                    node_id,
                    pv,
                    kernel_salt: graph.salt(idx),
                    param_hash: params.hash(),
                    input_keys: &input_keys,
                    tile: tile_coord,
                    scale_q,
                    precision,
                })
            };
            keys.insert(idx, key);

            let cacheable = node.cache_policy() == CachePolicy::Cache;

            // Cache probe: a hit serves the tile and skips the eval entirely.
            if cacheable {
                if let Some(hit) = cache.get(&key) {
                    self.probe.note_cache_hit();
                    outputs.insert(idx, hit.tile);
                    continue;
                }
            }

            self.probe.note_cache_miss();
            self.probe.note_eval(node_id);

            // The source node evaluates against the injected tile; every other
            // node against its upstream tiles.
            let backend_inputs: Vec<TileHandle> = if is_source {
                vec![source
                    .as_ref()
                    .expect("is_source implies Some")
                    .tile
                    .clone()]
            } else {
                input_tiles
            };

            let out = self.backend.eval_node(BackendEvalRequest {
                key,
                node,
                params,
                inputs: &backend_inputs,
                roi,
                scale: scale_f,
                precision,
                cancel,
            })?;

            // Insert only after a successful eval — a cancelled/failed node
            // never inserts a partial tile (task B7 integrity).
            if cacheable {
                cache.put(key, out.clone(), Bytes(tile_cost(&out)));
            }
            outputs.insert(idx, out);
        }

        let terminal = graph.single_sink().ok_or_else(|| {
            RenderError::Internal(format!(
                "graph has {} terminal nodes; exactly one required",
                graph.sinks().len()
            ))
        })?;
        outputs
            .remove(&terminal)
            .ok_or_else(|| RenderError::Internal("terminal node produced no output".to_owned()))
    }
}

/// The source→output decimation ratio implied by a [`RenderScale`] (v1). `Fit`
/// derivation over the true source extent is task **C2**; the probe graphs this
/// task exercises render 1:1, so `Fit`/`OneToOne` map to `1.0` here.
fn scale_ratio(scale: RenderScale) -> f32 {
    match scale {
        RenderScale::OneToOne | RenderScale::Fit(_) => 1.0,
        RenderScale::Ratio(f) => f.clamp(f32::MIN_POSITIVE, 1.0),
    }
}

/// Quantize a scale ratio in `(0, 1]` to 1/64ths so micro zoom jitter does not
/// shred the cache (spec §3.5). The floor of 1 keeps a distinct, non-zero
/// quantum for every real scale.
fn quantize_scale(scale: f32) -> u16 {
    let q = (scale.clamp(0.0, 1.0) * 64.0).round();
    (q as u16).clamp(1, 64)
}

/// The byte cost of a produced tile, for VRAM budget accounting (spec §3.5).
/// CPU tiles report their host-buffer length; GPU tiles are sized from extent ×
/// per-pixel bytes at the tile's precision.
fn tile_cost(t: &TileHandle) -> u64 {
    if let Some(px) = t.cpu() {
        return px.bytes.len() as u64;
    }
    if let Some(ext) = t.extent() {
        let bpp = match t.precision() {
            Some(TilePrecision::F32) => 16u64,
            _ => 8u64,
        };
        return (ext.w as u64) * (ext.h as u64) * bpp;
    }
    0
}
