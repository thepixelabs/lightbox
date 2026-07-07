// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `RecipeCompiler` — `Recipe` → executable [`RenderGraph`] (spec §3.4).
//!
//! Owner: **A-core** (task **A11**: compiler + PV1 [`GraphTemplate`] with §4.1
//! slot layout; unknown-pv / unknown-stage typed errors; stable topology per
//! `(schema, pv)`). **D** owns per-PV template selection + PV999 divergent
//! topology (tasks D1–D5).
//!
//! The compiler resolves each template stage against the [`NodeRegistry`] at the
//! requested [`ProcessVersion`] and wires the resulting instances into a typed,
//! type-checked [`RenderGraph`]. Because the M1 [`Recipe`] carries no per-stage
//! parameters yet (E09 fills it), topology is a pure function of `(template, pv)`
//! — dragging a slider never restructures the graph (spec §3.4).

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_types::ImageId;

use crate::ng::colorimetry::{SourceColorimetry, SourceKind};
use crate::ng::error::CompileError;
use crate::ng::graph::RenderGraph;
use crate::ng::node::{KernelSalt, NodeRegistry, ParamBlock};
use crate::ng::nodes::{
    decoded::SrcDecodedNode, display::XformDisplayNode, resize::UtilResizeNode,
};
use crate::ng::types::{Extent, NodeId, ProcessVersion};

/// What the compiler needs to know about the source, independent of decode
/// (the E02 seam; spec §3.4).
pub struct SourceDesc {
    /// The image being rendered.
    pub image: ImageId,
    /// Full source resolution.
    pub full_extent: Extent,
    /// Raw (mosaic) vs already-RGB. v1 templates take `Rgb` (decode upstream).
    pub source_kind: SourceKind,
    /// Source colorimetry tag (opaque to the engine; §E02 guardrail).
    pub colorimetry: SourceColorimetry,
}

/// A per-PV stage layout: the §4.1 stage order with LOCAL/retouch slots
/// (spec §3.4). Adding a node to a slot is **data, not an engine change** — a
/// template is just the ordered list of stage node ids the compiler wires into
/// a linear pipeline.
///
/// The **PV1** template ([`GraphTemplate::pv1`]) declares every §4.1 slot in
/// order — tone, detail, optics, LOCAL, retouch, effects — but at E05-close
/// those slots are **empty** (no registered develop nodes), so the compiled
/// chain is `src.decoded → util.resize → xform.display`. E10/E11/E12 fill slots
/// by registering nodes; the compiler picks them up with no engine change.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct GraphTemplate {
    /// The flattened, in-order stage node ids (empty §4.1 slots contribute
    /// nothing).
    stages: Vec<NodeId>,
}

impl GraphTemplate {
    /// A linear template over an explicit ordered stage list (used by the PV1
    /// builder, by D's divergent PV999 template, and by tests).
    pub fn linear(stages: Vec<NodeId>) -> GraphTemplate {
        GraphTemplate { stages }
    }

    /// The PV1 template (M1): `src.decoded → util.resize(scale) → [tone,
    /// detail, optics, LOCAL, retouch, effects — empty at E05-close] →
    /// xform.display` (spec §3.4 "PV1 template").
    pub fn pv1() -> GraphTemplate {
        GraphTemplate::linear(vec![
            SrcDecodedNode::ID,
            UtilResizeNode::ID,
            // §4.1 develop slots (tone/detail/optics/LOCAL/retouch/effects) are
            // empty at E05-close — E10/E11/E12 register nodes into them.
            XformDisplayNode::ID,
        ])
    }

    /// The ordered stage ids.
    pub fn stages(&self) -> &[NodeId] {
        &self.stages
    }
}

/// Holds per-`ProcessVersion` [`GraphTemplate`]s + the [`NodeRegistry`] used to
/// resolve stages, and compiles recipes against them (spec §3.4).
#[derive(Default)]
pub struct RecipeCompiler {
    registry: Arc<NodeRegistry>,
    templates: HashMap<u16, GraphTemplate>,
}

impl RecipeCompiler {
    /// A compiler with an empty registry and no templates.
    pub fn new() -> RecipeCompiler {
        RecipeCompiler::default()
    }

    /// A compiler resolving stages against `registry`.
    pub fn with_registry(registry: Arc<NodeRegistry>) -> RecipeCompiler {
        RecipeCompiler {
            registry,
            templates: HashMap::new(),
        }
    }

    /// Registers `template` for `pv` (append-only; PV1 by A11, PV999 by D2).
    /// Re-registering a `pv` is rejected — old PVs are never replaced (§4.5).
    pub fn register_template(
        &mut self,
        pv: ProcessVersion,
        template: GraphTemplate,
    ) -> Result<(), CompileError> {
        if self.templates.contains_key(&pv.0) {
            return Err(CompileError::TemplateAlreadyRegistered(pv));
        }
        self.templates.insert(pv.0, template);
        Ok(())
    }

    /// The process versions this compiler has templates for (sorted).
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        let mut pvs: Vec<u16> = self.templates.keys().copied().collect();
        pvs.sort_unstable();
        pvs.into_iter().map(ProcessVersion).collect()
    }

    /// Compiles a read-only [`Recipe`] into an executable DAG under `pv`.
    ///
    /// Unknown `pv` ⇒ [`CompileError::UnsupportedPv`] (**never** silently falls
    /// back to latest — §4.5). A template stage with no registered node under
    /// `pv` ⇒ [`CompileError::NodeNotRegistered`]. Topology is stable per
    /// `(recipe schema, pv)` so param drags never restructure the graph.
    pub fn compile(
        &self,
        recipe: &Recipe,
        pv: ProcessVersion,
        src: &SourceDesc,
    ) -> Result<RenderGraph, CompileError> {
        // The M1 Recipe (schema/pv only, E09 stub) and SourceDesc do not yet
        // drive optional-stage selection; topology is template-only. When E09
        // populates the recipe, stage params + optional-stage presence map here
        // while keeping topology stable per (schema, pv).
        let _ = (recipe, src);

        let template = self
            .templates
            .get(&pv.0)
            .ok_or(CompileError::UnsupportedPv(pv))?;

        let mut graph = RenderGraph::new();
        let mut prev = None;
        for &stage_id in &template.stages {
            let node = self
                .registry
                .resolve(stage_id, pv)
                .ok_or(CompileError::NodeNotRegistered { id: stage_id, pv })?;
            // Stamp the registered kernel salt onto the graph node so the
            // content key (spec §3.5) flips when a shipped kernel changes
            // (task B2; the salt-discipline gate is D4). A resolvable node
            // always has a salt, but fall back to the id-derived default rather
            // than panicking if the two seams ever disagree.
            let salt = self
                .registry
                .kernel_salt(stage_id, pv)
                .unwrap_or_else(|| KernelSalt(blake3::hash(stage_id.0.as_bytes())));
            let idx = graph.add_node_full(node, ParamBlock::default(), salt);
            if let Some(prev_idx) = prev {
                // Wire the upstream output into this stage's (single) input port.
                let to_port = graph.node(idx).descriptor().inputs.first().map(|p| p.name);
                let to_port = to_port.ok_or_else(|| {
                    CompileError::UnknownStage(format!(
                        "stage {stage_id} has no input port to receive upstream output"
                    ))
                })?;
                graph.connect(prev_idx, idx, to_port)?;
            }
            prev = Some(idx);
        }
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::colorimetry::SourceColorimetry;
    use crate::ng::node::PvRange;
    use crate::ng::nodes::{
        decoded::SrcDecodedFactory, display::XformDisplayFactory, resize::UtilResizeFactory,
    };
    use lightbox_types::PV_M0;

    fn source_desc() -> SourceDesc {
        SourceDesc {
            image: ImageId(1),
            full_extent: Extent { w: 64, h: 48 },
            source_kind: SourceKind::Rgb,
            colorimetry: SourceColorimetry::default(),
        }
    }

    fn registry_with_engine_nodes() -> Arc<NodeRegistry> {
        let mut reg = NodeRegistry::new();
        reg.register(
            SrcDecodedNode::ID,
            PvRange::from_open(PV_M0),
            Arc::new(SrcDecodedFactory::default()),
        )
        .unwrap();
        reg.register(
            UtilResizeNode::ID,
            PvRange::from_open(PV_M0),
            Arc::new(UtilResizeFactory::default()),
        )
        .unwrap();
        reg.register(
            XformDisplayNode::ID,
            PvRange::from_open(PV_M0),
            Arc::new(XformDisplayFactory::default()),
        )
        .unwrap();
        Arc::new(reg)
    }

    /// Build the expected `src.decoded → util.resize → xform.display` chain
    /// directly from the same factories for a structural assertion.
    fn expected_pv1_graph(reg: &NodeRegistry) -> RenderGraph {
        let mut g = RenderGraph::new();
        let s = g.add_node(reg.resolve(SrcDecodedNode::ID, PV_M0).unwrap());
        let r = g.add_node(reg.resolve(UtilResizeNode::ID, PV_M0).unwrap());
        let d = g.add_node(reg.resolve(XformDisplayNode::ID, PV_M0).unwrap());
        g.connect(s, r, "in").unwrap();
        g.connect(r, d, "in").unwrap();
        g
    }

    #[test]
    fn pv1_compiles_to_the_expected_topology() {
        let reg = registry_with_engine_nodes();
        let mut compiler = RecipeCompiler::with_registry(reg.clone());
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();

        let graph = compiler
            .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
            .unwrap();
        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.edge_count(), 2);
        assert!(graph.structurally_eq(&expected_pv1_graph(&reg)));
    }

    #[test]
    fn compile_is_stable_across_repeated_calls() {
        let reg = registry_with_engine_nodes();
        let mut compiler = RecipeCompiler::with_registry(reg);
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        let a = compiler
            .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
            .unwrap();
        let b = compiler
            .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
            .unwrap();
        assert!(a.structurally_eq(&b));
    }

    #[test]
    fn unsupported_pv_is_rejected_never_falls_back() {
        let reg = registry_with_engine_nodes();
        let mut compiler = RecipeCompiler::with_registry(reg);
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        let err = compiler
            .compile(&Recipe::identity(PV_M0), ProcessVersion(99), &source_desc())
            .unwrap_err();
        assert!(matches!(
            err,
            CompileError::UnsupportedPv(ProcessVersion(99))
        ));
    }

    #[test]
    fn missing_node_registration_is_typed() {
        // Template references nodes, but the registry has none of them.
        let mut compiler = RecipeCompiler::with_registry(Arc::new(NodeRegistry::new()));
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        let err = compiler
            .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
            .unwrap_err();
        match err {
            CompileError::NodeNotRegistered { id, pv } => {
                assert_eq!(id, SrcDecodedNode::ID);
                assert_eq!(pv, PV_M0);
            }
            other => panic!("expected NodeNotRegistered, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_template_registration_is_rejected() {
        let mut compiler = RecipeCompiler::with_registry(registry_with_engine_nodes());
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        let err = compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap_err();
        assert!(matches!(err, CompileError::TemplateAlreadyRegistered(_)));
        assert_eq!(compiler.supported_pvs(), vec![PV_M0]);
    }
}
