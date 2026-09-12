// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase A, the global develop toolset's node library scaffolding
//! (spec §4.4 `nodes/global/mod.rs`).
//!
//! Owner: **E10**. This module is the seam between E09's [`GlobalStages`]
//! recipe fields and E05's [`RenderGraph`]/[`NodeRegistry`]: it declares the
//! [`GlobalNode`] convention every develop node follows, registers E10's
//! nodes into the shared registry ([`register_global_nodes`]), and builds the
//! two develop-DAG segments the compiler splices into the PV1 chain
//! ([`build_wb_segment`]/[`build_tone_color_segment`]), spec §4.1's
//! `[E02 decode] → E10 WB → [E02 input transform] → E10 tone/color chain →
//! …` placement, WB node (task A9) included as of Phase A's second landing.
//!
//! # Deviation from the spec's literal signatures
//!
//! The spec sketches `build_wb_segment(g: &mut GraphBuilder, p: &GlobalStages,
//! src: PortId) -> PortId`. The built E05 engine has no `GraphBuilder`/`PortId`
//! types, the compiled DAG is [`RenderGraph`] (built in place via
//! `add_node_full`/`connect`) addressed by [`NodeIndex`]. These functions are
//! adapted to that real seam, threading the [`NodeRegistry`] + [`ProcessVersion`]
//! the compiler already has in hand so each segment can resolve+salt its nodes
//! exactly as [`crate::ng::compile::RecipeCompiler::compile`] does for the
//! fixed engine stages (recorded in `docs/plan/epics/E10-deviations.md`).
//!
//! # Identity elision (spec §4.1)
//!
//! Every node here is added to the graph **only** when its slice of
//! [`GlobalStages`] is non-neutral. A fully-identity `GlobalStages` therefore
//! makes both segment builders pure passthroughs (`Ok(src)`, zero nodes
//! added), proven by the `identity_recipe_elides_every_e10_node` probe test
//! in `tests/e10_global_basic.rs`.

pub mod bw_mix;
pub mod clarity;
pub mod color_grade;
pub mod contrast;
pub mod creative_lut;
pub mod dehaze;
pub mod exposure;
pub mod hsl;
pub mod noise_reduction;
pub mod sharpen;
pub mod texture;
pub mod tone_curve;
pub mod tone_recovery;
pub mod vibrance_sat;
pub mod white_balance;
pub mod whites_blacks;

use std::sync::Arc;

use lightbox_edit::{GlobalStages, ParamId};

use crate::ng::error::{CompileError, RegistryError};
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::node::{KernelSalt, NodeRegistry, ParamBlock, PvRange};
use crate::ng::types::{NodeId, ProcessVersion, RenderScale, Roi};

pub use bw_mix::{BwMixFactory, BwMixNode};
pub use clarity::{ClarityFactory, ClarityNode};
pub use color_grade::{ColorGradeFactory, ColorGradeNode};
pub use contrast::{ContrastFactory, ContrastNode};
pub use creative_lut::{CreativeLutFactory, CreativeLutNode, LookResolver};
pub use dehaze::{DehazeFactory, DehazeNode};
pub use exposure::{ExposureFactory, ExposureNode};
pub use hsl::{HslFactory, HslNode};
pub use noise_reduction::{NoiseReductionFactory, NoiseReductionNode};
pub use sharpen::{SharpenFactory, SharpenNode};
pub use texture::{TextureFactory, TextureNode};
pub use tone_curve::{ToneCurveFactory, ToneCurveNode};
pub use tone_recovery::{ToneRecoveryFactory, ToneRecoveryNode};
pub use vibrance_sat::{VibranceSatFactory, VibranceSatNode};
pub use white_balance::{WhiteBalanceFactory, WhiteBalanceNode};
pub use whites_blacks::{WhitesBlacksFactory, WhitesBlacksNode};

/// The node-authoring convention every E10 develop node follows on top of
/// [`crate::ng::RenderNode`] (spec §4.4).
pub trait GlobalNode: crate::ng::RenderNode {
    /// True iff `p`'s slice of the recipe renders this node as a no-op, the
    /// identity-elision predicate the segment builders use to skip adding it
    /// to the graph at all (spec §4.1).
    fn is_identity(p: &GlobalStages) -> bool
    where
        Self: Sized;

    /// The validated [`ParamBlock`] this node's `eval_gpu`/`eval_cpu` read,
    /// extracted from `p` (spec §4.3 field ownership, E10 owns the
    /// semantics, E09 owns the container).
    fn param_block(p: &GlobalStages) -> ParamBlock
    where
        Self: Sized;

    /// ROI back-propagation context this node needs beyond `out` (spec §4.4).
    /// Every A1-A8 node is pointwise (no neighborhood read), so the default
    /// matches [`crate::ng::RenderNode::plan`]'s identity behavior; spatial
    /// nodes (tone recovery, clarity, later phases) override this.
    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out
    }
}

/// Registers every E10 Phase-A node (`global.wb`, `global.exposure`,
/// `global.contrast`, `global.whites_blacks`) for `pv` and every later PV
/// (open range, matching the engine-owned stages' registration style) into
/// `reg` (spec §4.4 `register_global_nodes`, tasks A5/A9).
pub fn register_global_nodes(
    reg: &mut NodeRegistry,
    pv: ProcessVersion,
) -> Result<(), RegistryError> {
    reg.register(
        WhiteBalanceNode::ID,
        PvRange::from_open(pv),
        Arc::new(WhiteBalanceFactory::default()),
    )?;
    reg.register(
        ExposureNode::ID,
        PvRange::from_open(pv),
        Arc::new(ExposureFactory::default()),
    )?;
    reg.register(
        ContrastNode::ID,
        PvRange::from_open(pv),
        Arc::new(ContrastFactory::default()),
    )?;
    reg.register(
        ToneRecoveryNode::ID,
        PvRange::from_open(pv),
        Arc::new(ToneRecoveryFactory::default()),
    )?;
    reg.register(
        WhitesBlacksNode::ID,
        PvRange::from_open(pv),
        Arc::new(WhitesBlacksFactory::default()),
    )?;
    reg.register(
        ToneCurveNode::ID,
        PvRange::from_open(pv),
        Arc::new(ToneCurveFactory::default()),
    )?;
    reg.register(
        HslNode::ID,
        PvRange::from_open(pv),
        Arc::new(HslFactory::default()),
    )?;
    reg.register(
        VibranceSatNode::ID,
        PvRange::from_open(pv),
        Arc::new(VibranceSatFactory::default()),
    )?;
    reg.register(
        ColorGradeNode::ID,
        PvRange::from_open(pv),
        Arc::new(ColorGradeFactory::default()),
    )?;
    reg.register(
        BwMixNode::ID,
        PvRange::from_open(pv),
        Arc::new(BwMixFactory::default()),
    )?;
    reg.register(
        ClarityNode::ID,
        PvRange::from_open(pv),
        Arc::new(ClarityFactory::default()),
    )?;
    reg.register(
        TextureNode::ID,
        PvRange::from_open(pv),
        Arc::new(TextureFactory::default()),
    )?;
    reg.register(
        DehazeNode::ID,
        PvRange::from_open(pv),
        Arc::new(DehazeFactory::default()),
    )?;
    reg.register(
        NoiseReductionNode::ID,
        PvRange::from_open(pv),
        Arc::new(NoiseReductionFactory::default()),
    )?;
    reg.register(
        SharpenNode::ID,
        PvRange::from_open(pv),
        Arc::new(SharpenFactory::default()),
    )?;
    reg.register(
        CreativeLutNode::ID,
        PvRange::from_open(pv),
        Arc::new(CreativeLutFactory::default()),
    )?;
    Ok(())
}

/// Builds the white-balance DAG segment (spec §4.1, §4.4 `build_wb_segment`)
/// from `src`'s output, returning the new tail node: `WhiteBalanceNode`
/// (task A9), elided (`Ok(src)`, zero nodes added) when
/// `WhiteBalanceNode::is_identity(p)`, i.e. `AsShot`/`Auto` (see
/// `white_balance`'s module docs for why every reachable pixel takes the
/// non-raw Bradford-CAT branch at M1).
pub fn build_wb_segment(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    p: &GlobalStages,
    src: NodeIndex,
) -> Result<NodeIndex, CompileError> {
    maybe_add::<WhiteBalanceNode>(g, reg, pv, WhiteBalanceNode::ID, p, src)
}

/// Builds the tone/color DAG segment (spec §4.1, §4.4
/// `build_tone_color_segment`) from `src`'s output: `ExposureNode →
/// ContrastNode → ToneRecoveryNode → WhitesBlacksNode → ToneCurveNode`
/// (Phase A+B+C's slice of the full §4.1 chain; `HslNode`/… are later
/// phases/tasks), each elided when identity. Returns the new tail node
/// `src` unchanged when every stage is identity (the A5 probe test).
///
/// `ToneRecoveryNode` (E10 §6 M1-slice **PROVISIONAL** node, task group
/// B2/B7/B10-B12) sits exactly where spec §4.1 pins it: after contrast, before
/// whites/blacks, so a recovered highlight/shadow can still be clipped by an
/// aggressive whites/blacks endpoint remap downstream (stage-interaction
/// tuning, B15, is named M2-deferred in `docs/plan/epics/E10-deviations.md`).
///
/// `ToneCurveNode` (task C3) sits exactly where spec §4.1 pins it: right
/// after `WhitesBlacksNode`, before the (later-phase) HSL/color chain.
///
/// `HslNode` (task C8) and `VibranceSatNode` (task C10) sit exactly where
/// spec §4.1 pins them: `… ToneCurveNode ► HslNode(8-band) ►
/// VibranceSatNode ► …`, right after the tone curve, before
/// `ColorGradeNode`.
///
/// `ColorGradeNode` (tasks C11-C12) sits exactly where spec §4.1 pins it:
/// `… VibranceSatNode ► ColorGradeNode ► …`, right after vibrance/
/// saturation, before the B&W mixer.
///
/// `BwMixNode` (task D1) sits exactly where spec §4.1 pins it: `…
/// ColorGradeNode ► BwMixNode ► …`, right after color grading, before the
/// presence trio.
///
/// `ClarityNode` (task D3) sits exactly where spec §4.1 pins it: `…
/// BwMixNode ► ClarityNode ► …`, right after the B&W mixer, before texture
/// (task D4) and dehaze (tasks D5-D6, later Phase D landings).
///
/// `TextureNode` (task D4) sits exactly where spec §4.1 pins it: `…
/// ClarityNode ► TextureNode ► …`, right after clarity, before dehaze.
///
/// `DehazeNode` (tasks D5-D6) sits exactly where spec §4.1 pins it: `…
/// TextureNode ► DehazeNode ► …`, right after texture, the last of the
/// presence trio, before `CreativeLutNode`.
///
/// The Detail pair sits between the presence trio and the creative LUT, in
/// the order `… DehazeNode ► NoiseReductionNode ► SharpenNode ►
/// CreativeLutNode`. Noise reduction is FIRST because sharpening amplifies
/// whatever high-frequency content it is handed, and unremoved sensor noise
/// is high-frequency content: sharpen before you denoise and you sharpen the
/// noise, then ask the denoiser to remove structure it can no longer tell
/// apart from detail. Both sit after the presence trio because clarity and
/// texture are local-CONTRAST tools on a coarser scale, and before the
/// creative LUT because a look is a colour transform that should see the
/// finished, detail-corrected image.
///
/// `CreativeLutNode` (task D9) sits exactly where spec §4.1 pins it: `…
/// DehazeNode ► CreativeLutNode`, the last stage of the tone/color chain.
/// Elided whenever `GlobalStages::effects.creative_lut` is absent or its
/// `amount` is `0` (`GlobalNode::is_identity`, spec §4.1's identity-elision
/// contract). Unlike every other stage above, its `ParamBlock` is NOT built
/// by the generic `maybe_add`/`N::param_block` path, task **D10** resolves
/// the recipe's `CreativeLut.id` (a content-hash reference) to a real parsed
/// `Lut3D` via `look_resolver` (see `maybe_add_creative_lut`, below): with a
/// resolver wired and the hash installed, the node samples the REAL table;
/// with no resolver (every caller that hasn't opted in, e.g. tests) or an
/// unresolved hash (not installed / failed to re-parse), it falls back to
/// `N::param_block`'s amount-only encoding, which is `CreativeLutNode`'s own
/// documented graceful-identity path (`no_resolver_wired_yet_degrades_to_identity`).
pub fn build_tone_color_segment(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    p: &GlobalStages,
    src: NodeIndex,
    look_resolver: Option<&dyn LookResolver>,
) -> Result<NodeIndex, CompileError> {
    let cur = maybe_add::<ExposureNode>(g, reg, pv, ExposureNode::ID, p, src)?;
    let cur = maybe_add::<ContrastNode>(g, reg, pv, ContrastNode::ID, p, cur)?;
    let cur = maybe_add::<ToneRecoveryNode>(g, reg, pv, ToneRecoveryNode::ID, p, cur)?;
    let cur = maybe_add::<WhitesBlacksNode>(g, reg, pv, WhitesBlacksNode::ID, p, cur)?;
    let cur = maybe_add::<ToneCurveNode>(g, reg, pv, ToneCurveNode::ID, p, cur)?;
    let cur = maybe_add::<HslNode>(g, reg, pv, HslNode::ID, p, cur)?;
    let cur = maybe_add::<VibranceSatNode>(g, reg, pv, VibranceSatNode::ID, p, cur)?;
    let cur = maybe_add::<ColorGradeNode>(g, reg, pv, ColorGradeNode::ID, p, cur)?;
    let cur = maybe_add::<BwMixNode>(g, reg, pv, BwMixNode::ID, p, cur)?;
    let cur = maybe_add::<ClarityNode>(g, reg, pv, ClarityNode::ID, p, cur)?;
    let cur = maybe_add::<TextureNode>(g, reg, pv, TextureNode::ID, p, cur)?;
    let cur = maybe_add::<DehazeNode>(g, reg, pv, DehazeNode::ID, p, cur)?;
    let cur = maybe_add::<NoiseReductionNode>(g, reg, pv, NoiseReductionNode::ID, p, cur)?;
    let cur = maybe_add::<SharpenNode>(g, reg, pv, SharpenNode::ID, p, cur)?;
    let cur = maybe_add_creative_lut(g, reg, pv, p, cur, look_resolver)?;
    Ok(cur)
}

/// D10's own resolve+connect primitive for `global.creative_lut`, the one
/// stage `maybe_add`'s generic `N::param_block(p)` call isn't enough for,
/// because the real LUT table content lives outside `GlobalStages` (in the
/// `installed_look` catalog, resolved by content hash). Identity elision is
/// unchanged (`CreativeLutNode::is_identity`, spec §4.1); when the node IS
/// added, this resolves `p.effects.creative_lut`'s `id` through
/// `look_resolver` (if any) and builds the `ParamBlock` via
/// `CreativeLutNode::param_block_with_lut` on a hit, falling back to
/// `CreativeLutNode::param_block`'s own amount-only (graceful-identity)
/// encoding on any miss, never touching the node's `eval_cpu`/`eval_gpu`/
/// math (D9, already merged; this function only ever calls the node's own
/// public constructors).
fn maybe_add_creative_lut(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    p: &GlobalStages,
    src: NodeIndex,
    look_resolver: Option<&dyn LookResolver>,
) -> Result<NodeIndex, CompileError> {
    if CreativeLutNode::is_identity(p) {
        return Ok(src);
    }
    let id = CreativeLutNode::ID;
    let node = reg
        .resolve(id, pv)
        .ok_or(CompileError::NodeNotRegistered { id, pv })?;
    let salt = reg
        .kernel_salt(id, pv)
        .unwrap_or_else(|| KernelSalt(blake3::hash(id.0.as_bytes())));
    let params = match (&p.effects.creative_lut, look_resolver) {
        (Some(cl), Some(resolver)) => match resolver.resolve(&cl.id) {
            Some(lut) => CreativeLutNode::param_block_with_lut(
                creative_lut::recipe_amount_to_fraction(cl.amount),
                &lut,
            ),
            None => CreativeLutNode::param_block(p),
        },
        _ => CreativeLutNode::param_block(p),
    };
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

/// Resolves + connects `N` after `src` unless `N::is_identity(p)`, the shared
/// identity-elision + resolve/salt/connect primitive both segment builders use.
fn maybe_add<N: GlobalNode>(
    g: &mut RenderGraph,
    reg: &NodeRegistry,
    pv: ProcessVersion,
    id: NodeId,
    p: &GlobalStages,
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
    let params = N::param_block(p);
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

/// Fast-path invalidation hint (E10 task A4): which `global.*` node ids a
/// [`ParamId`] delta touches. Pure data, ready for E05 Phase B's
/// `RenderNode::affected_by`/executor wiring (not yet consumed live, the
/// content-keyed cache stays correct without it, spec §3.2 "content keys make
/// correctness independent of this hint"); exercised directly by this
/// module's own `tests` (below).
///
/// Every entry here names **only** the node(s) whose output the param
/// directly feeds, never anything upstream (the A4 acceptance criterion).
pub fn invalidates(id: ParamId) -> &'static [NodeId] {
    match id {
        ParamId::WhiteBalance => &[WhiteBalanceNode::ID],
        ParamId::Exposure => &[ExposureNode::ID],
        ParamId::Contrast => &[ContrastNode::ID],
        ParamId::Highlights | ParamId::Shadows => &[ToneRecoveryNode::ID],
        ParamId::Whites | ParamId::Blacks => &[WhitesBlacksNode::ID],
        ParamId::ToneCurve => &[ToneCurveNode::ID],
        ParamId::Hsl => &[HslNode::ID],
        ParamId::Vibrance | ParamId::Saturation => &[VibranceSatNode::ID],
        ParamId::ColorGrade => &[ColorGradeNode::ID],
        ParamId::BwMix | ParamId::Treatment => &[BwMixNode::ID],
        ParamId::Clarity => &[ClarityNode::ID],
        ParamId::Texture => &[TextureNode::ID],
        ParamId::Dehaze => &[DehazeNode::ID],
        ParamId::Sharpen => &[SharpenNode::ID],
        ParamId::NoiseReduction => &[NoiseReductionNode::ID],
        ParamId::CreativeLut => &[CreativeLutNode::ID],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A4 acceptance: an `ExposureEv` (`ParamId::Exposure`) delta invalidates
    /// `global.exposure` and **nothing upstream** (nor any sibling node).
    #[test]
    fn exposure_delta_invalidates_only_the_exposure_node() {
        assert_eq!(invalidates(ParamId::Exposure), &[ExposureNode::ID]);
        assert!(!invalidates(ParamId::Exposure).contains(&ContrastNode::ID));
        assert!(!invalidates(ParamId::Exposure).contains(&WhitesBlacksNode::ID));
    }

    /// A9's own A4-style hint: a `WhiteBalance` delta invalidates
    /// `global.wb` and nothing downstream.
    #[test]
    fn white_balance_delta_invalidates_only_the_wb_node() {
        assert_eq!(invalidates(ParamId::WhiteBalance), &[WhiteBalanceNode::ID]);
        assert!(!invalidates(ParamId::WhiteBalance).contains(&ExposureNode::ID));
    }

    #[test]
    fn contrast_delta_invalidates_only_the_contrast_node() {
        assert_eq!(invalidates(ParamId::Contrast), &[ContrastNode::ID]);
    }

    /// E10 §6/B7: `Highlights`/`Shadows` deltas both invalidate exactly
    /// `global.tone_recovery` (the combined recovery node), nothing else.
    #[test]
    fn highlights_and_shadows_both_invalidate_the_tone_recovery_node() {
        assert_eq!(invalidates(ParamId::Highlights), &[ToneRecoveryNode::ID]);
        assert_eq!(invalidates(ParamId::Shadows), &[ToneRecoveryNode::ID]);
        assert!(!invalidates(ParamId::Highlights).contains(&WhitesBlacksNode::ID));
    }

    #[test]
    fn whites_and_blacks_both_invalidate_the_combined_endpoint_node() {
        assert_eq!(invalidates(ParamId::Whites), &[WhitesBlacksNode::ID]);
        assert_eq!(invalidates(ParamId::Blacks), &[WhitesBlacksNode::ID]);
    }

    /// C3's own A4-style hint: a `ToneCurve` delta invalidates
    /// `global.tone_curve` and nothing upstream.
    #[test]
    fn tone_curve_delta_invalidates_only_the_tone_curve_node() {
        assert_eq!(invalidates(ParamId::ToneCurve), &[ToneCurveNode::ID]);
        assert!(!invalidates(ParamId::ToneCurve).contains(&WhitesBlacksNode::ID));
    }

    #[test]
    fn an_unrelated_param_invalidates_no_e10_node() {
        assert_eq!(invalidates(ParamId::Crop), &[] as &[NodeId]);
    }

    /// C8's own A4-style hint: an `Hsl` delta invalidates `global.hsl` and
    /// nothing upstream.
    #[test]
    fn hsl_delta_invalidates_only_the_hsl_node() {
        assert_eq!(invalidates(ParamId::Hsl), &[HslNode::ID]);
        assert!(!invalidates(ParamId::Hsl).contains(&ToneCurveNode::ID));
    }

    /// C10's own A4-style hint: `Vibrance`/`Saturation` deltas both
    /// invalidate exactly `global.vibrance_sat`.
    #[test]
    fn vibrance_and_saturation_both_invalidate_the_vibrance_sat_node() {
        assert_eq!(invalidates(ParamId::Vibrance), &[VibranceSatNode::ID]);
        assert_eq!(invalidates(ParamId::Saturation), &[VibranceSatNode::ID]);
        assert!(!invalidates(ParamId::Vibrance).contains(&HslNode::ID));
    }

    /// C11/C12's own A4-style hint: a `ColorGrade` delta invalidates
    /// `global.color_grade` and nothing upstream.
    #[test]
    fn color_grade_delta_invalidates_only_the_color_grade_node() {
        assert_eq!(invalidates(ParamId::ColorGrade), &[ColorGradeNode::ID]);
        assert!(!invalidates(ParamId::ColorGrade).contains(&VibranceSatNode::ID));
    }

    /// D1's own A4-style hint: `BwMix`/`Treatment` deltas both invalidate
    /// exactly `global.bw_mix`.
    #[test]
    fn bw_mix_and_treatment_both_invalidate_the_bw_mix_node() {
        assert_eq!(invalidates(ParamId::BwMix), &[BwMixNode::ID]);
        assert_eq!(invalidates(ParamId::Treatment), &[BwMixNode::ID]);
        assert!(!invalidates(ParamId::BwMix).contains(&ColorGradeNode::ID));
    }

    /// D3's own A4-style hint: a `Clarity` delta invalidates `global.clarity`
    /// and nothing upstream.
    #[test]
    fn clarity_delta_invalidates_only_the_clarity_node() {
        assert_eq!(invalidates(ParamId::Clarity), &[ClarityNode::ID]);
        assert!(!invalidates(ParamId::Clarity).contains(&BwMixNode::ID));
    }

    /// D4's own A4-style hint: a `Texture` delta invalidates
    /// `global.texture` and nothing upstream.
    #[test]
    fn texture_delta_invalidates_only_the_texture_node() {
        assert_eq!(invalidates(ParamId::Texture), &[TextureNode::ID]);
        assert!(!invalidates(ParamId::Texture).contains(&ClarityNode::ID));
    }

    /// D5/D6's own A4-style hint: a `Dehaze` delta invalidates
    /// `global.dehaze` and nothing upstream.
    #[test]
    fn dehaze_delta_invalidates_only_the_dehaze_node() {
        assert_eq!(invalidates(ParamId::Dehaze), &[DehazeNode::ID]);
        assert!(!invalidates(ParamId::Dehaze).contains(&TextureNode::ID));
    }

    /// The Detail pair's own A4-style hint: each of the two leaves
    /// invalidates exactly its own node, and neither reaches the other (they
    /// are adjacent stages, so a stale hint here would be silently wrong in
    /// the most expensive direction).
    #[test]
    fn the_detail_leaves_each_invalidate_only_their_own_node() {
        assert_eq!(invalidates(ParamId::Sharpen), &[SharpenNode::ID]);
        assert_eq!(
            invalidates(ParamId::NoiseReduction),
            &[NoiseReductionNode::ID]
        );
        assert!(!invalidates(ParamId::Sharpen).contains(&NoiseReductionNode::ID));
        assert!(!invalidates(ParamId::NoiseReduction).contains(&SharpenNode::ID));
        assert!(!invalidates(ParamId::Sharpen).contains(&DehazeNode::ID));
    }

    /// D9's own A4-style hint: a `CreativeLut` delta invalidates
    /// `global.creative_lut` and nothing upstream.
    #[test]
    fn creative_lut_delta_invalidates_only_the_creative_lut_node() {
        assert_eq!(invalidates(ParamId::CreativeLut), &[CreativeLutNode::ID]);
        assert!(!invalidates(ParamId::CreativeLut).contains(&DehazeNode::ID));
    }
}
