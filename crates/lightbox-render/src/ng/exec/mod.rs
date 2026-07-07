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
pub mod cpu;
pub mod gpu;

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_jobs::CancelToken;

use crate::ng::cache::{CacheKey, NodeCache};
use crate::ng::engine::BackendId;
use crate::ng::error::RenderError;
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::stats::RecomputeProbe;
use crate::ng::tile::TileHandle;
use crate::ng::types::{RenderScale, Roi};

pub use backend::{Backend, BackendEvalRequest};

/// The engine default tile edge (spec §4.3).
pub const DEFAULT_TILE_SIZE: u32 = 256;

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

    /// Evaluate `graph` over `roi` at `scale`, threading `cache` and `cancel`
    /// through the topo walk; returns the terminal node's output tile.
    ///
    /// `source` optionally pre-populates the engine-owned source stage: its
    /// `(index, tile)` names the graph node (`src.decoded`) whose single input
    /// is the uploaded/decoded working tile. `src.decoded` declares zero graph
    /// inputs but consumes the source as its one input (see `nodes::decoded`);
    /// graphs without a source stage (probe generators like `test.const`) pass
    /// `None` and generate their own root tile.
    pub fn evaluate(
        &self,
        graph: &RenderGraph,
        roi: Roi,
        scale: RenderScale,
        cache: &NodeCache,
        cancel: &CancelToken,
        source: Option<(NodeIndex, TileHandle)>,
    ) -> Result<TileHandle, RenderError> {
        // Caching (get/put, tail invalidation) is task B; the v1 walk always
        // recomputes. Accept the handle so the signature is stable for B.
        let _ = cache;

        let topo = graph.topo_order()?;
        let scale_f = scale_ratio(scale);
        let mut outputs: HashMap<NodeIndex, TileHandle> = HashMap::with_capacity(topo.len());

        for idx in topo {
            if cancel.is_cancelled() {
                return Err(RenderError::Cancelled);
            }
            let node = graph.node(idx);
            let node_id = node.descriptor().id;
            let _span = tracing::debug_span!("eval_node", node = %node_id).entered();

            // Source-stage injection: the pre-populated source node takes the
            // uploaded/decoded tile as its sole input; every other node draws
            // its inputs from upstream tiles already evaluated in this walk.
            let inputs: Vec<TileHandle> = match &source {
                Some((src_idx, src_tile)) if *src_idx == idx => vec![src_tile.clone()],
                _ => {
                    let input_idxs = graph.inputs_of(idx);
                    let mut inputs = Vec::with_capacity(input_idxs.len());
                    for src in input_idxs {
                        let tile = outputs.get(&src).cloned().ok_or_else(|| {
                            RenderError::Internal(format!(
                                "node {node_id}: upstream output for {src:?} not yet evaluated"
                            ))
                        })?;
                        inputs.push(tile);
                    }
                    inputs
                }
            };

            let params = graph.params(idx);
            let precision = node.precision();
            self.probe.note_cache_miss();
            self.probe.note_eval(node_id);

            let out = self.backend.eval_node(BackendEvalRequest {
                key: placeholder_key(node_id, idx),
                node,
                params,
                inputs: &inputs,
                roi,
                scale: scale_f,
                precision,
                cancel,
            })?;
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

/// A v1 placeholder content key (node id + graph position). The real §3.5
/// derivation — kernel salt, param hash, input-key propagation, tile/scale — is
/// task **B2**; the CPU backend does not consult the key, so this only needs to
/// be well-formed.
fn placeholder_key(node_id: crate::ng::types::NodeId, idx: NodeIndex) -> CacheKey {
    let mut h = blake3::Hasher::new();
    h.update(node_id.0.as_bytes());
    h.update(&(idx.index() as u64).to_le_bytes());
    CacheKey(h.finalize())
}
