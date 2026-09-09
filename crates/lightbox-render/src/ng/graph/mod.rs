// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `RenderGraph`, the typed DAG on petgraph (spec §3.1/§2.2; task **A8**).
//!
//! Owner: **A-core** (task **A8**: typed edges, topo sort, cycle rejection,
//! multi-input support, structural-equality helper for tests; building an
//! ill-typed or cyclic graph returns [`CompileError`], never panics).

use std::sync::Arc;

use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::ng::error::CompileError;
use crate::ng::node::{KernelSalt, ParamBlock, RenderNode};
use crate::ng::types::NodeId;

/// Index of a node within a [`RenderGraph`] (petgraph node handle).
pub use petgraph::graph::NodeIndex;

/// One node in the graph: its instance, validated params, and the kernel salt
/// that keys its cache output (spec §3.5). The salt is the WGSL/CPU algorithm
/// revision the [`crate::ng::NodeFactory`] reports; the compiler stamps the
/// registry salt at build (`registry.kernel_salt(id, pv)`), while directly-built
/// graphs (tests, probes) get a deterministic per-node-id default so distinct
/// node kinds still key distinctly.
struct NodeEntry {
    node: Arc<dyn RenderNode>,
    params: ParamBlock,
    salt: KernelSalt,
}

/// The default kernel salt for a node added without an explicit factory salt
/// a stable hash of the node id, so two distinct node kinds never collide on a
/// cache key while an unchanged node id is deterministic across builds.
fn default_salt(id: NodeId) -> KernelSalt {
    KernelSalt(blake3::hash(id.0.as_bytes()))
}

/// A compiled, typed render DAG (spec §2.2/§3.1): a `petgraph` digraph of node
/// instances joined by type-checked port edges. The edge weight is the target
/// input-port name (the wire's type is validated at [`RenderGraph::connect`]
/// and recoverable from the producer's [`NodeDescriptor`]).
#[derive(Default)]
pub struct RenderGraph {
    g: DiGraph<NodeEntry, &'static str>,
}

impl std::fmt::Debug for RenderGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nodes: Vec<&'static str> = self
            .g
            .node_indices()
            .map(|i| self.g[i].node.descriptor().id.0)
            .collect();
        f.debug_struct("RenderGraph")
            .field("nodes", &nodes)
            .field("edges", &self.edge_count())
            .finish()
    }
}

impl RenderGraph {
    /// An empty graph.
    pub fn new() -> RenderGraph {
        RenderGraph::default()
    }

    /// Adds a node instance with default (empty) params, returning its index.
    /// The kernel salt defaults to a stable hash of the node id.
    pub fn add_node(&mut self, node: Arc<dyn RenderNode>) -> NodeIndex {
        self.add_node_with_params(node, ParamBlock::default())
    }

    /// Adds a node instance with explicit `params` and a default (id-derived)
    /// kernel salt, returning its index.
    pub fn add_node_with_params(
        &mut self,
        node: Arc<dyn RenderNode>,
        params: ParamBlock,
    ) -> NodeIndex {
        let salt = default_salt(node.descriptor().id);
        self.add_node_full(node, params, salt)
    }

    /// Adds a node instance with explicit `params` and kernel `salt`, the
    /// compiler stamps the registry salt (`registry.kernel_salt(id, pv)`) here so
    /// a shipped-kernel change flips the content key (spec §3.5; task B2).
    pub fn add_node_full(
        &mut self,
        node: Arc<dyn RenderNode>,
        params: ParamBlock,
        salt: KernelSalt,
    ) -> NodeIndex {
        self.g.add_node(NodeEntry { node, params, salt })
    }

    /// Connects `from`'s output to `to`'s input named `to_port`, type-checking
    /// the [`PortType`]s. A type mismatch, an unknown target port, or a
    /// resulting cycle returns a [`CompileError`] and leaves the graph
    /// unchanged.
    pub fn connect(
        &mut self,
        from: NodeIndex,
        to: NodeIndex,
        to_port: &'static str,
    ) -> Result<(), CompileError> {
        let out_ty = self.g[from].node.descriptor().output.ty;
        let to_desc = self.g[to].node.descriptor();
        let Some(in_port) = to_desc.inputs.iter().find(|p| p.name == to_port) else {
            return Err(CompileError::TypeMismatch {
                at: format!(
                    "{} -> {}.{}",
                    self.g[from].node.descriptor().id,
                    to_desc.id,
                    to_port
                ),
                detail: format!("node {} has no input port {to_port:?}", to_desc.id),
            });
        };
        if in_port.ty != out_ty {
            return Err(CompileError::TypeMismatch {
                at: format!(
                    "{} -> {}.{}",
                    self.g[from].node.descriptor().id,
                    to_desc.id,
                    to_port
                ),
                detail: format!(
                    "producer emits {out_ty:?} but port {to_port:?} wants {:?}",
                    in_port.ty
                ),
            });
        }
        let edge = self.g.add_edge(from, to, to_port);
        if is_cyclic_directed(&self.g) {
            self.g.remove_edge(edge);
            return Err(CompileError::Cyclic);
        }
        Ok(())
    }

    /// A topological order of the nodes; a cycle ⇒ [`CompileError::Cyclic`].
    pub fn topo_order(&self) -> Result<Vec<NodeIndex>, CompileError> {
        toposort(&self.g, None).map_err(|_| CompileError::Cyclic)
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.g.node_count()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.g.edge_count()
    }

    /// The node instance at `idx`.
    pub fn node(&self, idx: NodeIndex) -> &dyn RenderNode {
        &*self.g[idx].node
    }

    /// The node identity at `idx`.
    pub fn node_id(&self, idx: NodeIndex) -> NodeId {
        self.g[idx].node.descriptor().id
    }

    /// The index of the first node with identity `id`, if any. A linear
    /// template emits at most one node per stage id, so this unambiguously
    /// locates the engine-owned source stage (`src.decoded`) for injection.
    pub fn node_index(&self, id: NodeId) -> Option<NodeIndex> {
        self.g
            .node_indices()
            .find(|&i| self.g[i].node.descriptor().id == id)
    }

    /// The validated params at `idx`.
    pub fn params(&self, idx: NodeIndex) -> &ParamBlock {
        &self.g[idx].params
    }

    /// The kernel salt at `idx`, the WGSL/CPU algorithm revision folded into the
    /// node's [`crate::ng::CacheKey`] (spec §3.5; task B2).
    pub fn salt(&self, idx: NodeIndex) -> KernelSalt {
        self.g[idx].salt
    }

    /// The upstream node feeding each of `idx`'s input ports, in port-declaration
    /// order (multi-input support, spec §3.2). A port with no incoming edge is
    /// omitted, so the length equals the number of *connected* inputs.
    pub fn inputs_of(&self, idx: NodeIndex) -> Vec<NodeIndex> {
        let inputs = self.g[idx].node.descriptor().inputs;
        // (port_index, source) pairs, then ordered by port index.
        let mut ordered: Vec<(usize, NodeIndex)> = self
            .g
            .edges_directed(idx, Direction::Incoming)
            .filter_map(|e| {
                let port = *e.weight();
                inputs
                    .iter()
                    .position(|p| p.name == port)
                    .map(|pi| (pi, e.source()))
            })
            .collect();
        ordered.sort_by_key(|(pi, _)| *pi);
        ordered.into_iter().map(|(_, src)| src).collect()
    }

    /// The graph's sink nodes (no outgoing edges).
    pub fn sinks(&self) -> Vec<NodeIndex> {
        self.g
            .node_indices()
            .filter(|&i| {
                self.g
                    .edges_directed(i, Direction::Outgoing)
                    .next()
                    .is_none()
            })
            .collect()
    }

    /// The single terminal (sink) node, if the graph has exactly one.
    pub fn single_sink(&self) -> Option<NodeIndex> {
        let sinks = self.sinks();
        if sinks.len() == 1 {
            Some(sinks[0])
        } else {
            None
        }
    }

    /// Structural equality: same node count, same node ids in insertion order,
    /// and the same set of typed edges, ignoring instance identity. A test
    /// helper for compiler topology assertions (spec §3.1/A8).
    pub fn structurally_eq(&self, other: &RenderGraph) -> bool {
        if self.node_count() != other.node_count() || self.edge_count() != other.edge_count() {
            return false;
        }
        for i in 0..self.node_count() {
            let a = NodeIndex::new(i);
            if self.g[a].node.descriptor().id != other.g[a].node.descriptor().id {
                return false;
            }
        }
        let mut ea = self.edge_triples();
        let mut eb = other.edge_triples();
        ea.sort();
        eb.sort();
        ea == eb
    }

    /// `(source_index, target_index, to_port)` for every edge, the structural
    /// fingerprint.
    fn edge_triples(&self) -> Vec<(usize, usize, &'static str)> {
        self.g
            .edge_references()
            .map(|e| (e.source().index(), e.target().index(), *e.weight()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::node::{NodeDescriptor, ParamsSchema, ParamsSchemaRef};
    use crate::ng::tile::{CpuTileView, TileView};
    use crate::ng::types::PortType;
    use crate::ng::{CpuEvalCtx, GpuEvalCtx, NodeError, PortDecl};

    static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

    /// A configurable test node: fixed id, inputs, and output type.
    /// (Auto `Send + Sync`: every [`NodeDescriptor`] field is `'static` + `Sync`.)
    struct TestNode {
        desc: NodeDescriptor,
    }

    impl RenderNode for TestNode {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.desc
        }
        fn eval_gpu(
            &self,
            _: &mut GpuEvalCtx<'_>,
            _: &[TileView<'_>],
            _: &ParamBlock,
        ) -> Result<(), NodeError> {
            Ok(())
        }
        fn eval_cpu(
            &self,
            _: &mut CpuEvalCtx<'_>,
            _: &[CpuTileView<'_>],
            _: &ParamBlock,
        ) -> Result<(), NodeError> {
            Ok(())
        }
    }

    fn node(
        id: &'static str,
        inputs: &'static [PortDecl],
        out_ty: PortType,
    ) -> Arc<dyn RenderNode> {
        Arc::new(TestNode {
            desc: NodeDescriptor {
                id: NodeId(id),
                inputs,
                output: PortDecl {
                    name: "out",
                    ty: out_ty,
                },
                params_schema: ParamsSchemaRef(&SCHEMA),
            },
        })
    }

    static IN_F16: &[PortDecl] = &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }];
    static IN_F32: &[PortDecl] = &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF32,
    }];
    static TWO_IN_F16: &[PortDecl] = &[
        PortDecl {
            name: "a",
            ty: PortType::LinearRgbaF16,
        },
        PortDecl {
            name: "b",
            ty: PortType::LinearRgbaF16,
        },
    ];

    fn linear_chain() -> (RenderGraph, NodeIndex, NodeIndex) {
        let mut g = RenderGraph::new();
        let src = g.add_node(node("src", &[], PortType::LinearRgbaF16));
        let mid = g.add_node(node("mid", IN_F16, PortType::LinearRgbaF16));
        g.connect(src, mid, "in").unwrap();
        (g, src, mid)
    }

    #[test]
    fn topo_order_respects_edges() {
        let (g, src, mid) = linear_chain();
        let topo = g.topo_order().unwrap();
        let ps = topo.iter().position(|&i| i == src).unwrap();
        let pm = topo.iter().position(|&i| i == mid).unwrap();
        assert!(ps < pm);
        assert_eq!(g.single_sink(), Some(mid));
        assert_eq!(g.inputs_of(mid), vec![src]);
    }

    #[test]
    fn ill_typed_edge_is_rejected() {
        let mut g = RenderGraph::new();
        let src = g.add_node(node("src", &[], PortType::LinearRgbaF16));
        let f32node = g.add_node(node("acc", IN_F32, PortType::LinearRgbaF32));
        let err = g.connect(src, f32node, "in").unwrap_err();
        assert!(matches!(err, CompileError::TypeMismatch { .. }));
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn unknown_port_is_rejected() {
        let mut g = RenderGraph::new();
        let src = g.add_node(node("src", &[], PortType::LinearRgbaF16));
        let mid = g.add_node(node("mid", IN_F16, PortType::LinearRgbaF16));
        let err = g.connect(src, mid, "nope").unwrap_err();
        assert!(matches!(err, CompileError::TypeMismatch { .. }));
    }

    #[test]
    fn cycle_is_rejected_and_edge_rolled_back() {
        let mut g = RenderGraph::new();
        let a = g.add_node(node("a", IN_F16, PortType::LinearRgbaF16));
        let b = g.add_node(node("b", IN_F16, PortType::LinearRgbaF16));
        g.connect(a, b, "in").unwrap();
        let err = g.connect(b, a, "in").unwrap_err();
        assert!(matches!(err, CompileError::Cyclic));
        assert_eq!(g.edge_count(), 1); // rolled back
    }

    #[test]
    fn multi_input_ordering_matches_port_declaration() {
        let mut g = RenderGraph::new();
        let a = g.add_node(node("a", &[], PortType::LinearRgbaF16));
        let b = g.add_node(node("b", &[], PortType::LinearRgbaF16));
        let comp = g.add_node(node("comp", TWO_IN_F16, PortType::LinearRgbaF16));
        // Connect port "b" first, then "a", order of the inputs_of result must
        // still follow the port declaration order (a, b).
        g.connect(b, comp, "b").unwrap();
        g.connect(a, comp, "a").unwrap();
        assert_eq!(g.inputs_of(comp), vec![a, b]);
    }

    #[test]
    fn structural_equality() {
        let (g1, _, _) = linear_chain();
        let (g2, _, _) = linear_chain();
        assert!(g1.structurally_eq(&g2));

        // A graph with a differently-named node is not structurally equal.
        let mut g3 = RenderGraph::new();
        let s = g3.add_node(node("src", &[], PortType::LinearRgbaF16));
        let m = g3.add_node(node("OTHER", IN_F16, PortType::LinearRgbaF16));
        g3.connect(s, m, "in").unwrap();
        assert!(!g1.structurally_eq(&g3));
    }
}
