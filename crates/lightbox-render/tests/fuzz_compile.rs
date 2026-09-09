// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase F4, compiler fuzz/property hardening (spec §5 task F4).
//!
//! **In-gate bounded subset.** `RecipeCompiler::compile` fed arbitrary
//! `(Recipe, ProcessVersion, SourceDesc)` combinations must **never panic**
//! every input either compiles to a valid [`RenderGraph`] or returns a
//! typed [`CompileError`]. This runs a few thousand proptest cases in
//! `cargo test` (seconds, PR-blocking); the spec's 24 h continuous fuzz run
//! is a nightly/long-run job (named DEFERRED in
//! `docs/plan/epics/E05-deviations.md`, a standalone multi-hour run is not
//! a PR-gate step, same disposition as the C7/C10/D3/F1/F2 items).
//!
//! `Recipe` is still E01's `{schema, pv}` placeholder at M1 (E09 owns the
//! full recipe body), so "arbitrary recipes" fuzzes `pv` (the only
//! caller-variable field reachable through the `#[non_exhaustive]`
//! `Recipe::identity` constructor) and the `SourceDesc` shape (extent
//! including degenerate 0×0, `Raw`/`Rgb` source kind) against both a
//! fully-registered PV1 compiler and one with a deliberately-missing node,
//! to exercise the whole `UnsupportedPv`/`NodeNotRegistered`/success
//! taxonomy. Extending the strategy to cover real per-stage params is
//! E09/E10's job once the recipe schema lands (F3's engine book documents
//! the seam).

use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_render::ng::colorimetry::{CfaDesc, SourceColorimetry, SourceKind};
use lightbox_render::ng::compile::{GraphTemplate, RecipeCompiler, SourceDesc};
use lightbox_render::ng::error::CompileError;
use lightbox_render::ng::node::NodeRegistry;
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
use lightbox_render::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
use lightbox_render::ng::types::{Extent, NodeId, ProcessVersion};
use lightbox_render::ng::PvRange;
use lightbox_types::{ImageId, PV_M0};
use proptest::prelude::*;

/// A compiler with the real PV1 template fully registered (`PV_M0` only
/// every other pv is `UnsupportedPv`).
fn full_compiler() -> RecipeCompiler {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::single(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        UtilResizeNode::ID,
        PvRange::single(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .unwrap();
    reg.register(
        XformDisplayNode::ID,
        PvRange::single(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(PV_M0, GraphTemplate::pv1())
        .unwrap();
    compiler
}

/// A compiler whose PV1 template references `util.resize`, but only
/// `src.decoded`/`xform.display` are registered, every PV_M0 compile must
/// hit `NodeNotRegistered`, never panic.
fn missing_node_compiler() -> RecipeCompiler {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::single(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        XformDisplayNode::ID,
        PvRange::single(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(PV_M0, GraphTemplate::pv1())
        .unwrap();
    compiler
}

fn source_kind(raw: bool) -> SourceKind {
    if raw {
        SourceKind::Raw {
            cfa: CfaDesc::default(),
        }
    } else {
        SourceKind::Rgb
    }
}

/// Case count: 2000 by default (a few ms, PR-blocking); the nightly
/// workflow overrides `PROPTEST_CASES` to a much larger bounded count as a
/// practical stand-in for the spec's 24 h continuous fuzz run. This is
/// **not** equivalent to real coverage-guided fuzzing (cargo-fuzz/libFuzzer
/// persisting a corpus across runs), that harness is not set up (needs a
/// nightly toolchain + libFuzzer target); named DEFERRED in
/// `docs/plan/epics/E05-deviations.md`. proptest's own `PROPTEST_CASES` env
/// var is honored automatically by `ProptestConfig::default()`, this
/// wrapper only exists to keep the 2000-case PR-blocking default explicit in
/// source rather than relying on an unset env var.
fn cases() -> ProptestConfig {
    let n = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    ProptestConfig::with_cases(n)
}

proptest! {
    #![proptest_config(cases())]

    /// Compiling never panics, for either registry, over arbitrary
    /// `(pv, extent, source_kind)`, including degenerate 0×0 extents and a
    /// `pv` that may or may not match anything registered. (`Recipe::schema`
    /// is `#[non_exhaustive]`-constructed only via `Recipe::identity`, so it
    /// is not independently variable at M1, noted in the module docs.)
    #[test]
    fn compile_never_panics_full_registry(
        pv in any::<u16>(),
        w in 0u32..=200_000,
        h in 0u32..=200_000,
        raw in any::<bool>(),
    ) {
        let compiler = full_compiler();
        let recipe = Recipe::identity(ProcessVersion(pv));
        let src = SourceDesc {
            image: ImageId(1),
            full_extent: Extent { w, h },
            source_kind: source_kind(raw),
            colorimetry: SourceColorimetry::default(),
        };
        // The only property under test: no panic, and the result is exactly
        // one of Ok(graph-with-a-single-sink) or a typed CompileError.
        match compiler.compile(&recipe, recipe.pv, &src) {
            Ok(graph) => {
                prop_assert!(graph.single_sink().is_some(), "a compiled graph must have exactly one terminal node");
            }
            Err(CompileError::UnsupportedPv(got)) => {
                prop_assert_eq!(got, ProcessVersion(pv));
            }
            Err(_) => {} // any other typed CompileError is acceptable, never a panic
        }
    }

    /// Same property against a deliberately-incomplete registry: PV_M0 must
    /// resolve to `NodeNotRegistered`, never panic; other pvs stay
    /// `UnsupportedPv`.
    #[test]
    fn compile_never_panics_missing_node_registry(
        pv in any::<u16>(),
        w in 0u32..=200_000,
        h in 0u32..=200_000,
        raw in any::<bool>(),
    ) {
        let compiler = missing_node_compiler();
        let recipe = Recipe::identity(ProcessVersion(pv));
        let src = SourceDesc {
            image: ImageId(2),
            full_extent: Extent { w, h },
            source_kind: source_kind(raw),
            colorimetry: SourceColorimetry::default(),
        };
        let result = compiler.compile(&recipe, recipe.pv, &src);
        if pv == PV_M0.0 {
            prop_assert!(
                matches!(result, Err(CompileError::NodeNotRegistered { id: NodeId("util.resize"), .. })),
                "expected NodeNotRegistered(util.resize) at PV_M0, got {result:?}"
            );
        } else {
            prop_assert!(matches!(result, Err(CompileError::UnsupportedPv(_))));
        }
    }
}
