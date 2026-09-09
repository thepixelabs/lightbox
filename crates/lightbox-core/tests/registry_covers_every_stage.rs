// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! **Regression: the app shipped a node registry the compiler could outrun.**
//!
//! `Session` used to hand-enumerate its `NodeRegistry`, `src.decoded`,
//! `util.resize`, `xform.display`, plus `register_global_nodes`, with a
//! comment claiming it was "the same registry the shell's editor canvas and
//! `lightbox-cli render` both submit against". It had drifted: E11's
//! `register_geometry_nodes` landed in `lightbox_render`'s own
//! `shipping_registry` but was never added to the copy in `session.rs`.
//!
//! The result reached a user: dragging Straighten produced
//! `render failed: compile: no node registered for NodeId geom.warp under
//! process version ...`, and Crop was broken the same way. Every existing
//! test passed, because they all build their engine from `shipping_registry`
//! / `shipping_compiler`, the one thing the app did *not* do.
//!
//! `Session` now calls `shipping_registry()` directly, so that particular
//! drift is structurally impossible.
//!
//! **What this file does and does not guard, stated plainly, because the
//! distinction is the whole lesson of the bug.** These tests compile against
//! `shipping_compiler`, which always *did* register the geometry nodes.
//! They would therefore have passed throughout the outage; they are **not**
//! the regression test for the reported failure. That one is
//! `render_end_to_end::session_renders_geometry_stages_straighten_and_crop`,
//! which goes through `Session::engine()`, the app's own registry, the
//! thing that was actually broken, and is verified to reproduce
//! `NodeNotRegistered { id: NodeId("geom.warp") }` against the old code.
//!
//! What these tests *do* guard is the other half: that the shipping registry
//! stays able to satisfy every stage the compiler is willing to emit, so a
//! future node added to the compiler without a matching registration fails
//! cheaply here (no GPU, no fixtures, no catalog) instead of only in the
//! slower end-to-end test.

use lightbox_edit::{CurvePoint, Recipe};
use lightbox_render::ng::{
    colorimetry::SourceColorimetry, shipping_compiler, Extent, SourceDesc, SourceKind,
};
use lightbox_types::{ImageId, PV_M0};

/// A recipe with every stage pushed off its identity value, so the compiler
/// splices in the maximum number of nodes. Any stage left at its default is
/// elided by that node's `is_identity`, which is exactly how `geom.warp`
/// stayed invisible to the rest of the suite.
fn maximal_recipe() -> Recipe {
    let mut r = Recipe::identity(PV_M0);

    // Global tone / colour.
    r.global.exposure = 0.75;
    r.global.contrast = 20.0;
    r.global.highlights = -30.0;
    r.global.shadows = 25.0;
    r.global.whites = 10.0;
    r.global.blacks = -10.0;
    r.global.presence.clarity = 15.0;
    r.global.presence.texture = 10.0;
    r.global.presence.dehaze = 5.0;
    r.global.vibrance = 12.0;
    r.global.saturation = -8.0;

    // A non-identity tone curve (a real bend, not just extra points).
    r.global.tone_curve.rgb.points = vec![
        CurvePoint { x: 0.0, y: 0.0 },
        CurvePoint { x: 0.5, y: 0.6 },
        CurvePoint { x: 1.0, y: 1.0 },
    ];

    // Geometry, the stages this test exists for.
    r.geometry.angle = 3.5;
    r.geometry.crop.left = 0.1;
    r.geometry.crop.top = 0.1;
    r.geometry.crop.right = 0.9;
    r.geometry.crop.bottom = 0.9;

    r
}

fn source_desc() -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 4000, h: 3000 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::WORKING_LINEAR,
    }
}

/// The shipping compiler must be able to build a graph for a recipe that
/// exercises every stage, including geometry. A missing registration
/// surfaces as `CompileError` naming the node id.
#[test]
fn shipping_registry_can_build_every_stage_the_compiler_emits() {
    let compiler = shipping_compiler();
    let graph = compiler
        .compile(&maximal_recipe(), PV_M0, &source_desc())
        .unwrap_or_else(|e| {
            panic!(
                "the shipping registry could not satisfy a maximal recipe: {e}\n\
                 This is the `geom.warp` class of bug: the compiler emitted a \
                 node nothing registered. Register it alongside the others in \
                 `shipping_registry`, not in a per-crate copy."
            )
        });
    assert!(
        graph.node_count() > 3,
        "a maximal recipe must splice in develop stages, got {} nodes — \
         if this collapsed to the bare src/resize/display chain, the stages \
         are being identity-elided when they should not be",
        graph.node_count()
    );
}

/// Straighten and crop specifically, each on its own, since those are the
/// two that actually shipped broken. Isolating them means a failure names
/// which one regressed instead of just "the maximal recipe broke".
#[test]
fn geometry_stages_compile_individually() {
    let compiler = shipping_compiler();
    let src = source_desc();

    let mut straighten = Recipe::identity(PV_M0);
    straighten.geometry.angle = 3.5;
    compiler
        .compile(&straighten, PV_M0, &src)
        .expect("straighten (geom.warp) must compile — this is the reported bug");

    let mut crop = Recipe::identity(PV_M0);
    crop.geometry.crop.left = 0.2;
    crop.geometry.crop.right = 0.8;
    compiler
        .compile(&crop, PV_M0, &src)
        .expect("crop (geom.crop) must compile — broken by the same omission");
}
