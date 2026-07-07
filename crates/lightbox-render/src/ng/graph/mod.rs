// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `RenderGraph` — the typed DAG on petgraph (spec §3.1/§2.2; task **A8**).
//!
//! Owner: **A-core** (task **A8**: typed edges, topo sort, cycle rejection,
//! multi-input support, structural-equality helper for tests; building an
//! ill-typed or cyclic graph returns [`CompileError`], never panics).

use std::sync::Arc;

use crate::ng::error::CompileError;
use crate::ng::node::RenderNode;

/// Index of a node within a [`RenderGraph`] (petgraph node handle).
pub use petgraph::graph::NodeIndex;

/// A compiled, typed render DAG (spec §2.2/§3.1). **A8 fills the internals**
/// (a `petgraph::graph::DiGraph` of node instances + typed port edges).
#[derive(Default)]
pub struct RenderGraph {}

impl RenderGraph {
    /// An empty graph.
    pub fn new() -> RenderGraph {
        RenderGraph::default()
    }

    /// Adds a node instance, returning its index.
    pub fn add_node(&mut self, node: Arc<dyn RenderNode>) -> NodeIndex {
        let _ = node;
        unimplemented!("A8 (A-core): RenderGraph::add_node")
    }

    /// Connects `from`'s output to `to`'s input named `to_port`, type-checking
    /// the [`crate::ng::PortType`]s. Mismatch ⇒ [`CompileError::TypeMismatch`].
    pub fn connect(
        &mut self,
        from: NodeIndex,
        to: NodeIndex,
        to_port: &'static str,
    ) -> Result<(), CompileError> {
        let _ = (from, to, to_port);
        unimplemented!("A8 (A-core): RenderGraph::connect — typed edge wiring")
    }

    /// A topological order of the nodes; a cycle ⇒ [`CompileError::Cyclic`].
    pub fn topo_order(&self) -> Result<Vec<NodeIndex>, CompileError> {
        unimplemented!("A8 (A-core): RenderGraph::topo_order — cycle rejection")
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        unimplemented!("A8 (A-core): RenderGraph::node_count")
    }

    /// Structural equality (topology + node ids + edges), ignoring instance
    /// identity — a test helper for compiler assertions (spec §3.1/A8).
    pub fn structurally_eq(&self, other: &RenderGraph) -> bool {
        let _ = other;
        unimplemented!("A8 (A-core): RenderGraph::structurally_eq — test helper")
    }
}
