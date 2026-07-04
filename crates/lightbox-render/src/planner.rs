// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `RenderPlanner` — M0 scaffolding, **not** part of the frozen §3.4 surface.
//!
//! The seed engine evaluates exactly one node per request; something has to
//! turn a [`RenderRequest`](crate::RenderRequest) into "which node, with which
//! params, over which source pixels". At M0 that is this planner hook:
//! Phase 2's tracer bullet plans `solid.color`, Phase 6 (T23) plans
//! `display.transform` over the embedded-preview resolver. **E05.1 replaces
//! the planner with recipe-driven DAG construction** — downstream code must
//! not depend on it beyond `Engine::set_planner`.

use lightbox_jobs::CancelToken;

use crate::engine::RenderRequest;
use crate::error::RenderError;
use crate::node::{NodeId, Params};
use crate::source::{SourceImage, SourceResolver};

/// A planned single-node evaluation.
pub struct PlannedEval {
    /// The node to evaluate (looked up in the registry under the request's
    /// process version).
    pub node: NodeId,
    /// The node's params, derived from the request (and source, if any).
    pub params: Params,
    /// Input pixels for the node (tile 0), or `None` for source-less nodes.
    pub source: Option<SourceImage>,
}

/// Plans one request into one node evaluation (M0 scaffolding — see module
/// docs; E05.1 owns the real recipe→DAG planning).
pub trait RenderPlanner: Send + Sync {
    /// Plans `req`. Resolves source pixels through `sources` when the planned
    /// node needs input, honoring `cancel`.
    fn plan(
        &self,
        req: &RenderRequest,
        sources: &dyn SourceResolver,
        cancel: &CancelToken,
    ) -> Result<PlannedEval, RenderError>;
}

/// The engine's initial planner: fails every request until a real planner is
/// installed via [`Engine::set_planner`](crate::Engine::set_planner).
#[derive(Default, Debug, Clone, Copy)]
pub struct UnconfiguredPlanner;

impl RenderPlanner for UnconfiguredPlanner {
    fn plan(
        &self,
        _req: &RenderRequest,
        _sources: &dyn SourceResolver,
        _cancel: &CancelToken,
    ) -> Result<PlannedEval, RenderError> {
        Err(RenderError::NoPlanner)
    }
}
