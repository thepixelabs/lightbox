// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `RecipeCompiler`, `Recipe` → executable [`RenderGraph`] (spec §3.4).
//!
//! Owner: **A-core** (task **A11**: compiler + PV1 [`GraphTemplate`] with §4.1
//! slot layout; unknown-pv / unknown-stage typed errors; stable topology per
//! `(schema, pv)`). **D** owns per-PV template selection + PV999 divergent
//! topology (tasks D1-D5).
//!
//! The compiler resolves each template stage against the [`NodeRegistry`] at the
//! requested [`ProcessVersion`] and wires the resulting instances into a typed,
//! type-checked [`RenderGraph`]. Because the M1 [`Recipe`] carries no per-stage
//! parameters yet (E09 fills it), topology is a pure function of `(template, pv)`
//! dragging a slider never restructures the graph (spec §3.4).

pub mod manifest;

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_types::{ImageId, PV_M0};

use crate::ng::colorimetry::{SourceColorimetry, SourceKind};
use crate::ng::error::CompileError;
use crate::ng::graph::RenderGraph;
use crate::ng::node::{KernelSalt, NodeRegistry, ParamBlock, PvRange};
use crate::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use crate::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
use crate::ng::nodes::geometry;
use crate::ng::nodes::global;
use crate::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
use crate::ng::types::{Extent, NodeId, ProcessVersion};

pub use manifest::{PvManifest, StageDigest};

/// What the compiler needs to know about the source, independent of decode
/// (the E02 seam; spec §3.4).
pub struct SourceDesc {
    /// The image being rendered.
    pub image: ImageId,
    /// Full source resolution.
    pub full_extent: Extent,
    /// Raw (mosaic) vs already-RGB. v1 templates take `Rgb` (decode upstream).
    pub source_kind: SourceKind,
    /// The colorimetry of the pixels **arriving at the graph's root port**
    /// which is not the same thing as the colorimetry the provider delivered.
    ///
    /// The source lift ([`crate::ng::source::Uploader::upload`] /
    /// [`crate::ng::source::to_working_tile_cpu`]) applies the input transform
    /// named by [`crate::ng::SourceImage::colorimetry`] *before* `src.decoded`,
    /// so by the time any graph stage runs the pixels are working-space linear.
    /// This field is therefore [`SourceColorimetry::WORKING_LINEAR`] for every
    /// M1 render, and that is the truth rather than a placeholder.
    ///
    /// It deliberately does **not** carry the provider's tag: `Engine::render_now`
    /// compiles the graph *before* it fetches the source (the compiled graph is
    /// what tells it whether a source stage exists at all), so the delivered
    /// tag is not knowable here. A future PV that wants to branch topology on
    /// source colorimetry needs the engine reordered to fetch-before-compile
    /// a seam change, not a compiler change.
    pub colorimetry: SourceColorimetry,
}

/// A per-PV stage layout: the §4.1 stage order with LOCAL/retouch slots
/// (spec §3.4). Adding a node to a slot is **data, not an engine change**, a
/// template is just the ordered list of stage node ids the compiler wires into
/// a linear pipeline.
///
/// The **PV1** template ([`GraphTemplate::pv1`]) declares every §4.1 slot in
/// order, tone, detail, optics, LOCAL, retouch, effects, but at E05-close
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
    /// detail, optics, LOCAL, retouch, effects, empty at E05-close] →
    /// xform.display` (spec §3.4 "PV1 template").
    pub fn pv1() -> GraphTemplate {
        GraphTemplate::linear(vec![
            SrcDecodedNode::ID,
            UtilResizeNode::ID,
            // §4.1 develop slots (tone/detail/optics/LOCAL/retouch/effects) are
            // empty at E05-close, E10/E11/E12 register nodes into them.
            XformDisplayNode::ID,
        ])
    }

    /// A **test-only, divergent-topology** template for the reserved test PV
    /// [`PV_TEST_999`] (task **D2**). It drops the `util.resize` stage from the
    /// PV1 chain, yielding `src.decoded → xform.display`, a *structurally
    /// different* graph (2 nodes / 1 edge vs PV1's 3 nodes / 2 edges) that
    /// resolves against the same registry. It exercises per-PV template
    /// selection, the pv-keyed content cache (no cross-PV pollution, the key
    /// folds in `pv`, spec §3.5), and the compile-under-two-PVs / migrate-preview
    /// primitive (task D5). PV999 never ships, [`Engine::new`] registers only
    /// PV1; PV999 is added via [`crate::ng::Engine::with_compiler`] in tests /
    /// the golden matrix.
    ///
    /// [`Engine::new`]: crate::ng::Engine::new
    pub fn pv_test_999() -> GraphTemplate {
        GraphTemplate::linear(vec![
            SrcDecodedNode::ID,
            // util.resize deliberately absent, the divergence from PV1.
            XformDisplayNode::ID,
        ])
    }

    /// The ordered stage ids.
    pub fn stages(&self) -> &[NodeId] {
        &self.stages
    }
}

/// The reserved **test-only** process version whose template
/// ([`GraphTemplate::pv_test_999`]) diverges in topology from PV1 (task D2). It
/// is never registered by [`crate::ng::Engine::new`]; only tests and the per-PV
/// golden matrix register it.
pub const PV_TEST_999: ProcessVersion = ProcessVersion(999);

/// Holds per-`ProcessVersion` [`GraphTemplate`]s + the [`NodeRegistry`] used to
/// resolve stages, and compiles recipes against them (spec §3.4).
#[derive(Default)]
pub struct RecipeCompiler {
    registry: Arc<NodeRegistry>,
    templates: HashMap<u16, GraphTemplate>,
    /// E10 task D10: the `global.creative_lut` id→content resolver, if any
    /// (`lightbox-core`'s catalog-backed adapter in production; `None` for
    /// every caller that hasn't opted in via
    /// [`RecipeCompiler::with_look_resolver`], tests, `lightbox-cli`, …).
    look_resolver: Option<Arc<dyn global::LookResolver>>,
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
            look_resolver: None,
        }
    }

    /// Installs `resolver` as this compiler's `global.creative_lut` id→
    /// content resolver (task D10). Builder-style so production wiring reads
    /// `RecipeCompiler::with_registry(reg).with_look_resolver(resolver)`.
    pub fn with_look_resolver(mut self, resolver: Arc<dyn global::LookResolver>) -> RecipeCompiler {
        self.look_resolver = Some(resolver);
        self
    }

    /// Registers `template` for `pv` (append-only; PV1 by A11, PV999 by D2).
    /// Re-registering a `pv` is rejected, old PVs are never replaced (§4.5).
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

    /// The registered [`GraphTemplate`] for `pv`, if any (used by the PV
    /// [`manifest`] builder and per-PV selection tests, task D1/D2).
    pub fn template(&self, pv: ProcessVersion) -> Option<&GraphTemplate> {
        self.templates.get(&pv.0)
    }

    /// The [`NodeRegistry`] this compiler resolves stages against (used by the
    /// PV [`manifest`] builder to read per-stage kernel salts, task D1).
    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// Compiles a read-only [`Recipe`] into an executable DAG under `pv`.
    ///
    /// Unknown `pv` ⇒ [`CompileError::UnsupportedPv`] (**never** silently falls
    /// back to latest, §4.5). A template stage with no registered node under
    /// `pv` ⇒ [`CompileError::NodeNotRegistered`]. Topology is stable per
    /// `(recipe schema, pv)` so param drags never restructure the graph
    /// **with one deliberate exception**: E10's develop segments
    /// (`global::build_wb_segment`/`build_tone_color_segment`, spliced in
    /// right after `src.decoded` and `util.resize` respectively) add nodes
    /// only for the recipe's non-identity `GlobalStages` fields (identity
    /// elision, spec §4.1), so *which* nodes appear is recipe-driven, while
    /// the fixed engine-owned stages' topology stays template-only. This
    /// keeps the per-PV manifest (task D1/D4), which pins only the fixed
    /// `template.stages()`, completely unaffected by E10 node additions.
    pub fn compile(
        &self,
        recipe: &Recipe,
        pv: ProcessVersion,
        src: &SourceDesc,
    ) -> Result<RenderGraph, CompileError> {
        // `src` drives extent-dependent stage params (the geometry segment
        // below reads `src.full_extent`). It does NOT drive *stage selection*
        // (raw vs non-raw WB path, E10 task A9), `recipe.global` does, via
        // the two develop segments spliced in below. See `SourceDesc`'s
        // `colorimetry` doc for why a colorimetry-driven branch cannot be
        // added here without reordering the engine.
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

            // E10 §4.1 placement: WB right after decode, the tone/color chain
            // right after the scale-ladder resize (both before xform.display).
            // Each splice is a pure passthrough (zero nodes, `prev` unchanged)
            // when its slice of `recipe.global` is identity.
            if stage_id == SrcDecodedNode::ID {
                prev = Some(global::build_wb_segment(
                    &mut graph,
                    &self.registry,
                    pv,
                    &recipe.global,
                    prev.expect("just inserted src.decoded"),
                )?);
            } else if stage_id == UtilResizeNode::ID {
                prev = Some(global::build_tone_color_segment(
                    &mut graph,
                    &self.registry,
                    pv,
                    &recipe.global,
                    prev.expect("just inserted util.resize"),
                    self.look_resolver.as_deref(),
                )?);
                // E11 §4.1/§5 placement: geometry (`geom.warp` then
                // `geom.crop`) sits after the tone/color chain and before
                // xform.display, detail/optics/LOCAL/retouch/effects are
                // still empty §4.1 slots at E11-close, so geometry directly
                // follows tone/color in the compiled chain today (E10/E11
                // deviations note this splice point moves once those slots
                // fill). `src.full_extent` is the source extent baked into
                // `geom.warp`/`geom.crop`'s params (`GeometryNode::param_block`)
                // correct as long as `util.resize` stays extent-identity
                // (see `docs/plan/epics/E11-deviations.md`).
                prev = Some(geometry::build_geometry_segment(
                    &mut graph,
                    &self.registry,
                    pv,
                    &recipe.geometry,
                    &recipe.global.optics,
                    src.full_extent,
                    prev.expect("tone/color segment always returns a node"),
                )?);
                // Effects (`fx.vignette` then `fx.grain`) sit after
                // geometry and before xform.display. The vignette is a
                // POST-CROP vignette, so it must see the cropped canvas or
                // a cropped photograph gets an off-centre vignette; grain
                // goes last because film grain sits on top of everything,
                // the vignette included. See
                // `nodes::global::build_effects_segment`.
                prev = Some(global::build_effects_segment(
                    &mut graph,
                    &self.registry,
                    pv,
                    &recipe.global,
                    &recipe.geometry,
                    src.full_extent,
                    prev.expect("geometry segment always returns a node"),
                )?);
            }
        }
        Ok(graph)
    }
}

/// Register the three engine-owned scaffold nodes (`src.decoded`, `util.resize`,
/// `xform.display`) into a fresh [`NodeRegistry`] over the open PV range from
/// [`PV_M0`] (task D1). This is the shipping node set every PV1 render resolves
/// against; it is the registry the PV [`manifest`] freezes.
///
/// Registration cannot fail here, the ids are distinct and the ranges do not
/// overlap, so a failure is a programming error and panics.
pub fn shipping_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .expect("src.decoded registers");
    reg.register(
        UtilResizeNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .expect("util.resize registers");
    reg.register(
        XformDisplayNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .expect("xform.display registers");
    global::register_global_nodes(&mut reg, PV_M0).expect("E10 global nodes register");
    geometry::register_geometry_nodes(&mut reg, PV_M0).expect("E11 geometry nodes register");
    reg
}

/// A compiler wired with the shipping [`shipping_registry`] and the PV1 template
/// ([`GraphTemplate::pv1`]), the shipping configuration [`crate::ng::Engine::new`]
/// assembles, exposed so the PV [`manifest`] gate and per-PV tests build the same
/// thing (task D1).
pub fn shipping_compiler() -> RecipeCompiler {
    let mut compiler = RecipeCompiler::with_registry(Arc::new(shipping_registry()));
    compiler
        .register_template(PV_M0, GraphTemplate::pv1())
        .expect("PV1 template registers on a fresh compiler");
    compiler
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

    /// The shipping helpers build the same PV1 topology the manual registry does
    /// (task D1), the config the manifest gate and `Engine::new` share.
    #[test]
    fn shipping_compiler_matches_the_manual_pv1_config() {
        let compiler = shipping_compiler();
        assert_eq!(compiler.supported_pvs(), vec![PV_M0]);
        let graph = compiler
            .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
            .unwrap();
        let reg = registry_with_engine_nodes();
        assert!(graph.structurally_eq(&expected_pv1_graph(&reg)));
    }

    /// **D2: per-PV template selection, PV1 and PV999 compile to *divergent*
    /// topologies from one compiler.** The same recipe resolves to a 3-node /
    /// 2-edge chain under PV1 and a 2-node / 1-edge chain under PV999 (the
    /// `util.resize` stage dropped), proving topology is a function of `(template,
    /// pv)` and that adding a PV is data, not an engine change.
    #[test]
    fn per_pv_template_selection_diverges_topology() {
        let mut compiler = RecipeCompiler::with_registry(registry_with_engine_nodes());
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        compiler
            .register_template(PV_TEST_999, GraphTemplate::pv_test_999())
            .unwrap();
        assert_eq!(compiler.supported_pvs(), vec![PV_M0, PV_TEST_999]);

        // The SAME recipe (identity, schema-only at M1) under two PVs.
        let recipe = Recipe::identity(PV_M0);
        let g1 = compiler.compile(&recipe, PV_M0, &source_desc()).unwrap();
        let g999 = compiler
            .compile(&recipe, PV_TEST_999, &source_desc())
            .unwrap();

        assert_eq!((g1.node_count(), g1.edge_count()), (3, 2));
        assert_eq!((g999.node_count(), g999.edge_count()), (2, 1));
        assert!(
            !g1.structurally_eq(&g999),
            "PV1 and PV999 must be structurally divergent"
        );
        // PV999 drops util.resize: src.decoded → xform.display.
        assert!(g999.node_index(UtilResizeNode::ID).is_none());
        assert!(g999.node_index(SrcDecodedNode::ID).is_some());
        assert!(g999.node_index(XformDisplayNode::ID).is_some());
    }

    /// One recipe compiles under two PVs in one session, each stable across
    /// repeated calls (the compile-under-different-pv primitive, task D5, at the
    /// compiler layer; the engine-level cache-independence proof is in
    /// `tests/ng_pv.rs`).
    #[test]
    fn one_recipe_compiles_under_two_pvs_repeatably() {
        let mut compiler = RecipeCompiler::with_registry(registry_with_engine_nodes());
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        compiler
            .register_template(PV_TEST_999, GraphTemplate::pv_test_999())
            .unwrap();
        let recipe = Recipe::identity(PV_M0);
        let a1 = compiler.compile(&recipe, PV_M0, &source_desc()).unwrap();
        let b1 = compiler.compile(&recipe, PV_M0, &source_desc()).unwrap();
        let a999 = compiler
            .compile(&recipe, PV_TEST_999, &source_desc())
            .unwrap();
        let b999 = compiler
            .compile(&recipe, PV_TEST_999, &source_desc())
            .unwrap();
        assert!(a1.structurally_eq(&b1));
        assert!(a999.structurally_eq(&b999));
        assert!(!a1.structurally_eq(&a999));
    }
}
