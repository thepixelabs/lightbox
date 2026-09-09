// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E11 Phase-E geometry node library (spec §4.4 `nodes/geometry/*`), the
//! seam between E09's [`Geometry`] recipe fields and E05's
//! [`RenderGraph`]/[`NodeRegistry`], mirroring `nodes::global`'s established
//! convention exactly (identity elision, a `maybe_add`-style segment
//! splicer, an `invalidates` fast-path hint).
//!
//! # Scope
//!
//! This module wires **only** the geometry-engine slice this epic task list
//! covers: `geom.warp` (straighten `angle`, tasks E1-E3) and `geom.crop`
//! (normalized crop rect + `Flip`, task E4). Optics (lens/TCA/vignette),
//! upright, manual transform sliders, and the effects stage (`fx.vignette`/
//! `fx.grain`) are separate, later passes, see
//! `docs/plan/epics/E11-deviations.md`.
//!
//! # Why `GeometryNode` is a distinct trait from `nodes::global::GlobalNode`
//!
//! `GlobalNode::param_block(p: &GlobalStages) -> ParamBlock` never needs the
//! canvas extent, every E10 stage is a pointwise/neighborhood tone or color
//! op. Geometry is different: `geom.warp`'s rotation center and `geom.crop`'s
//! pixel-rounded rect both need the source extent, and `RenderNode::plan`
//! (unlike `output_extent`) receives no `inputs: &[Extent]`, so the extent
//! must ride in the node's own baked [`ParamBlock`], computed once at compile
//! time from [`crate::ng::compile::SourceDesc::full_extent`] (this engine's
//! current `util.resize` is an extent-identity passthrough, see
//! `docs/plan/epics/E11-deviations.md` for the scope note on revisiting this
//! once real scale-ladder decimation lands). `GeometryNode::param_block`
//! therefore takes the extra `src_extent` parameter.
//!
//! # Identity elision (spec §4.1)
//!
//! Each node is added to the graph **only** when its slice of [`Geometry`] is
//! non-neutral, an identity `Geometry` (no crop, no straighten, no flip)
//! makes [`build_geometry_segment`] a pure passthrough (`Ok(src)`, zero nodes
//! added), proven by `identity_geometry_elides_every_node` in
//! `tests/e11_geom.rs`.

pub mod crop;
pub mod warp;

use std::sync::Arc;

use lightbox_edit::{Geometry, ParamId};

use crate::ng::error::{CompileError, RegistryError};
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::node::{KernelSalt, NodeRegistry, ParamBlock, PvRange};
use crate::ng::types::{Extent, NodeId, ProcessVersion};

pub use crop::{GeomCropFactory, GeomCropNode};
pub use warp::{GeomWarpFactory, GeomWarpNode};

/// The node-authoring convention every E11 geometry node follows on top of
/// [`crate::ng::RenderNode`] (mirrors `nodes::global::GlobalNode`; see this
/// module's docs for why `param_block` also takes `src_extent`).
pub trait GeometryNode: crate::ng::RenderNode {
    /// True iff `p`'s slice of the recipe renders this node as a no-op, the
    /// identity-elision predicate [`build_geometry_segment`] uses to skip
    /// adding it to the graph at all (spec §4.1).
    fn is_identity(p: &Geometry) -> bool
    where
        Self: Sized;

    /// The validated [`ParamBlock`] this node's `eval_gpu`/`eval_cpu`/`plan`
    /// read, extracted from `p` and the compile-time-known source extent.
    fn param_block(p: &Geometry, src_extent: Extent) -> ParamBlock
    where
        Self: Sized;
}

/// Registers every E11 geometry node (`geom.warp`, `geom.crop`) for `pv` and
/// every later PV (open range, matching `nodes::global`'s registration
/// style) into `reg`.
pub fn register_geometry_nodes(
    reg: &mut NodeRegistry,
    pv: ProcessVersion,
) -> Result<(), RegistryError> {
    reg.register(
        GeomWarpNode::ID,
        PvRange::from_open(pv),
        Arc::new(GeomWarpFactory::default()),
    )?;
    reg.register(
        GeomCropNode::ID,
        PvRange::from_open(pv),
        Arc::new(GeomCropFactory::default()),
    )?;
    Ok(())
}

/// Builds the geometry DAG segment (spec §4.1/§5: `geom.warp`, straighten
/// angle, then `geom.crop`, crop rect + orientation) from `src`'s output,
/// returning the new tail node. Each stage is elided when its slice of `p`
/// is identity (spec §4.1); an all-neutral `Geometry` therefore returns
/// `Ok(src)` with zero nodes added.
pub fn build_geometry_segment(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    p: &Geometry,
    src_extent: Extent,
    src: NodeIndex,
) -> Result<NodeIndex, CompileError> {
    let cur = maybe_add::<GeomWarpNode>(g, reg, pv, GeomWarpNode::ID, p, src_extent, src)?;
    let cur = maybe_add::<GeomCropNode>(g, reg, pv, GeomCropNode::ID, p, src_extent, cur)?;
    Ok(cur)
}

/// Resolves + connects `N` after `src` unless `N::is_identity(p)`, the
/// shared identity-elision + resolve/salt/connect primitive
/// [`build_geometry_segment`] uses (mirrors `nodes::global::mod::maybe_add`).
fn maybe_add<N: GeometryNode>(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    id: NodeId,
    p: &Geometry,
    src_extent: Extent,
    src: NodeIndex,
) -> Result<NodeIndex, CompileError> {
    if N::is_identity(p) {
        return Ok(src);
    }
    let node = reg
        .resolve(id, pv)
        .ok_or(CompileError::NodeNotRegistered { id, pv })?;
    let salt = reg
        .kernel_salt(id, pv)
        .unwrap_or_else(|| KernelSalt(blake3::hash(id.0.as_bytes())));
    let params = N::param_block(p, src_extent);
    let idx = g.add_node_full(node, params, salt);
    let to_port = g
        .node(idx)
        .descriptor()
        .inputs
        .first()
        .map(|d| d.name)
        .ok_or_else(|| {
            CompileError::UnknownStage(format!("{id} has no input port to splice into"))
        })?;
    g.connect(src, idx, to_port)?;
    Ok(idx)
}

/// Fast-path invalidation hint (mirrors `nodes::global::invalidates`): which
/// `geom.*` node ids a [`ParamId`] delta touches. Every entry names **only**
/// the node(s) whose output the param directly feeds.
pub fn invalidates(id: ParamId) -> &'static [NodeId] {
    match id {
        ParamId::Angle => &[GeomWarpNode::ID],
        ParamId::Crop | ParamId::Flip => &[GeomCropNode::ID],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_delta_invalidates_only_the_warp_node() {
        assert_eq!(invalidates(ParamId::Angle), &[GeomWarpNode::ID]);
        assert!(!invalidates(ParamId::Angle).contains(&GeomCropNode::ID));
    }

    #[test]
    fn crop_and_flip_deltas_invalidate_only_the_crop_node() {
        assert_eq!(invalidates(ParamId::Crop), &[GeomCropNode::ID]);
        assert_eq!(invalidates(ParamId::Flip), &[GeomCropNode::ID]);
        assert!(!invalidates(ParamId::Crop).contains(&GeomWarpNode::ID));
    }

    #[test]
    fn an_unrelated_param_invalidates_no_geometry_node() {
        assert_eq!(invalidates(ParamId::Exposure), &[] as &[NodeId]);
    }
}
