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

pub mod backend;
pub mod cpu;
pub mod gpu;

use std::sync::Arc;

use lightbox_jobs::CancelToken;

use crate::ng::cache::NodeCache;
use crate::ng::error::RenderError;
use crate::ng::graph::RenderGraph;
use crate::ng::tile::TileHandle;
use crate::ng::types::{RenderScale, Roi};

pub use backend::{Backend, BackendEvalRequest};

/// Drives a compiled [`RenderGraph`] to a rendered output: topo walk over the
/// nodes, per-tile evaluation via a [`Backend`], cache-key propagation, and
/// (task C) visible-first tiling + progressive ladders + priority lanes.
/// **A9 fills the whole-ROI v1; C fills tiling.**
#[derive(Default)]
pub struct Executor {}

impl Executor {
    /// An executor driving `backend`.
    pub fn new(backend: Arc<dyn Backend>) -> Executor {
        let _ = backend;
        unimplemented!("A9 (A-core): Executor::new — bind the backend + tile scheduler")
    }

    /// Evaluate `graph` over `roi` at `scale`, threading `cache` and `cancel`
    /// through the topo walk; returns the terminal node's output tile.
    pub fn evaluate(
        &self,
        graph: &RenderGraph,
        roi: Roi,
        scale: RenderScale,
        cache: &NodeCache,
        cancel: &CancelToken,
    ) -> Result<TileHandle, RenderError> {
        let _ = (graph, roi, scale, cache, cancel);
        unimplemented!("A9/C (A-core): Executor::evaluate — topo walk + tiling")
    }
}
