// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `exec` ↔ backend seam — **SCAFFOLD-FROZEN** (Risk R8).
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
use crate::ng::types::{Roi, TilePrecision};

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
    /// The render scale (source→output decimation factor).
    pub scale: f32,
    /// Output tile precision.
    pub precision: TilePrecision,
    /// Cooperative cancellation, checked at tile boundaries.
    pub cancel: &'a CancelToken,
}

/// A pixel evaluation backend — GPU (A-gpu) or CPU (A-core). The frozen seam.
pub trait Backend: Send + Sync {
    /// Evaluate one node at one cache key, producing its output tile.
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError>;

    /// Which backend this is (provenance stamped onto every `RenderOutput`).
    fn kind(&self) -> BackendId;
}
