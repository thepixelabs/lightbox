// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `RecipeCompiler` — `Recipe` → executable [`RenderGraph`] (spec §3.4).
//!
//! Owner: **A-core** (task **A11**: compiler + PV1 [`GraphTemplate`] with §4.1
//! slot layout; unknown-pv / unknown-stage typed errors; stable topology per
//! `(schema, pv)`). **D** owns per-PV template selection + PV999 divergent
//! topology (tasks D1–D5).

use lightbox_edit::Recipe;

use crate::ng::colorimetry::{SourceColorimetry, SourceKind};
use crate::ng::error::CompileError;
use crate::ng::graph::RenderGraph;
use crate::ng::types::{Extent, ProcessVersion};
use lightbox_types::ImageId;

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
/// (spec §3.4). Adding a node to a slot is **data, not an engine change**.
/// **A11 builds PV1; D adds further PVs.**
#[derive(Default)]
#[non_exhaustive]
pub struct GraphTemplate {}

impl GraphTemplate {
    /// The PV1 template (M1): `src.decoded → util.resize(scale) → [tone-stage
    /// slots, empty at E05-close] → xform.display` (spec §3.4 "PV1 template").
    pub fn pv1() -> GraphTemplate {
        unimplemented!("A11 (A-core): PV1 GraphTemplate — §4.1 slot layout")
    }
}

/// Holds per-`ProcessVersion` [`GraphTemplate`]s and compiles recipes against
/// them (spec §3.4). **A11 fills the internals.**
#[derive(Default)]
pub struct RecipeCompiler {}

impl RecipeCompiler {
    /// A compiler with no templates registered.
    pub fn new() -> RecipeCompiler {
        RecipeCompiler::default()
    }

    /// Registers `template` for `pv` (append-only; PV1 by A11, PV999 by D2).
    pub fn register_template(
        &mut self,
        pv: ProcessVersion,
        template: GraphTemplate,
    ) -> Result<(), CompileError> {
        let _ = (pv, template);
        unimplemented!("A11/D (A-core): RecipeCompiler::register_template")
    }

    /// Compiles a read-only [`Recipe`] into an executable DAG under `pv`.
    ///
    /// Unknown `pv` ⇒ [`CompileError::UnsupportedPv`] (**never** silently falls
    /// back to latest — §4.5). A recipe field with no node in this pv's
    /// template ⇒ [`CompileError::UnknownStage`]. Topology is stable per
    /// `(recipe schema, pv)` so param drags never restructure the graph.
    pub fn compile(
        &self,
        recipe: &Recipe,
        pv: ProcessVersion,
        src: &SourceDesc,
    ) -> Result<RenderGraph, CompileError> {
        let _ = (recipe, pv, src);
        unimplemented!("A11 (A-core): RecipeCompiler::compile — recipe → typed DAG")
    }
}
