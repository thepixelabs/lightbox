// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `exec` ↔ backend seam, **SCAFFOLD-FROZEN** (Risk R8).
//!
//! This is the interface where **A-core**'s executor/topo-walk meets
//! **A-gpu**'s GPU dispatch and **A-core**'s CPU path: a [`CacheKey`] + graph
//! node in, a [`TileHandle`] out. It is frozen at the end of Wave A (after
//! A9/B2) so a seam mismatch cannot stall both engineers; reshaping it needs a
//! deviation entry. [`crate::ng::exec::gpu`] and [`crate::ng::exec::cpu`]
//! implement it; the [`Executor`](super::Executor) drives it.

use lightbox_jobs::CancelToken;

use crate::ng::cache::CacheKey;
use crate::ng::engine::BackendId;
use crate::ng::error::RenderError;
use crate::ng::node::{ParamBlock, RenderNode};
use crate::ng::tile::TileHandle;
use crate::ng::types::{Extent, Roi, TilePrecision};

/// One node evaluation request handed to a [`Backend`] (spec §R8 seam:
/// CacheKey + graph node in, TileHandle out).
pub struct BackendEvalRequest<'a> {
    /// The content key this evaluation produces (for cache insert/pin).
    pub key: CacheKey,
    /// The node to evaluate.
    pub node: &'a dyn RenderNode,
    /// Its validated params.
    pub params: &'a ParamBlock,
    /// Upstream input tiles (already evaluated), in port order.
    pub inputs: &'a [TileHandle],
    /// The output ROI this evaluation must fill.
    pub roi: Roi,
    /// This node's **propagated target output extent** (task E11 addition
    /// see `ng::exec::Executor::evaluate`'s own doc comment and
    /// `docs/plan/epics/E11-deviations.md` E-scope-6/9): the size the
    /// backend must pre-allocate this node's output tile at. Equals `roi`'s
    /// extent for every node that does not itself change resolution
    /// (matching the pre-E11 "every node lands on the request ROI"
    /// contract `util.resize`'s Fit-scale decimation still relies on); a
    /// node that overrides [`crate::ng::RenderNode::output_extent`] (e.g.
    /// `geom.crop`) changes this value from that point forward in the DAG.
    pub target_extent: Extent,
    /// The render scale (source→output decimation factor).
    pub scale: f32,
    /// Output tile precision.
    pub precision: TilePrecision,
    /// Cooperative cancellation, checked at tile boundaries.
    pub cancel: &'a CancelToken,
}

/// A pixel evaluation backend, GPU (A-gpu) or CPU (A-core). The frozen seam.
pub trait Backend: Send + Sync {
    /// Evaluate one node at one cache key, producing its output tile.
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError>;

    /// Which backend this is (provenance stamped onto every `RenderOutput`).
    fn kind(&self) -> BackendId;
}
