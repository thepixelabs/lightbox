// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.tone_recovery`, the E10 §6 M1-slice **PROVISIONAL**
//! `ToneRecoveryNode` (highlights/shadows recovery). Implements the §4.5
//! sub-project's candidate 1: fast guided-filter base/detail (He & Sun 2015,
//! self-guided on scene-linear luma), clean-room from the published paper
//! math, no GPL source consulted. Domain: scene-linear working RGB, f32
//! accumulation (spec §4.2 "Highlight/shadow recovery | scene-linear luma
//! pyramid, f32").
//!
//! # Why guided filter and not fast local-Laplacian for M1
//!
//! Spec §4.5 names fast local-Laplacian pyramids (Paris/Hasinoff/Kautz 2011)
//! as the **halo-free-by-construction** target algorithm and guided filter as
//! the O(N) fallback with "risk of residual gradient reversal at strong
//! edges". §6 explicitly permits the M1 slice to ship "the spike's chosen
//! algorithm at prototype quality" ahead of the full B3-B6 spike/bake-off
//! this node takes the guided-filter path because it needs no pyramid
//! infrastructure (`common/pyramid.rs`, B8/B9, itself deferred) to make the
//! Highlights/Shadows sliders visibly move pixels. The fast-LL upgrade is
//! named M2 work in `docs/plan/epics/E10-deviations.md`.
//!
//! # The algorithm
//!
//! 1. **Luma.** `L = dot(rgb, working_luma_weights())`, the working space's
//!    (ProPhoto-linear D50) own CIE Y row, computed once per eval via
//!    [`working_luma_weights`] (never hand-copied, see that fn's docs).
//! 2. **Guided-filter base.** `B = guided_filter_luma(L)`, He & Sun's
//!    self-guided (`I = p = L`) box-filter regression
//!    (`(a, b)` per He & Sun eq. 4-6, box-blurred and recombined per eq. 8);
//!    an edge-aware smoothing of `L` that (unlike a Gaussian/box blur) mostly
//!    preserves strong edges, at the documented halo risk above.
//! 3. **Asymmetric range compression of the base.** [`compression_gain`] maps
//!    `(base, highlights, shadows)` to a multiplicative luma gain: `highlights`
//!    compresses (pulls down) bright quarter-tones, `shadows` lifts dark
//!    quarter-tones, via two disjoint-by-construction region weights (task
//!    **B7**, see that fn's docs for the asymmetry-rule proof).
//! 4. **Detail re-add with a gain clamp.** `detail = L - B`; the compressed
//!    base gets the same gain, but clamped to
//!    `[DETAIL_GAIN_MIN, DETAIL_GAIN_MAX]` before re-scaling the detail layer,
//!    so strong recovery can't amplify high-frequency (noise-prone) detail
//!    unboundedly ([`recovered_luma`]).
//! 5. **Reapply to RGB via luma-ratio with a chroma guard.** The output RGB is
//!    `rgb * ratio` for one scalar `ratio = out_luma / luma` (clamped
//!    [`reapply_ratio`]) shared by all three channels, this is *exactly*
//!    hue/chroma-preserving (scaling r, g, b by one common factor never
//!    changes their proportions), which is what makes the node's own
//!    "channel-neutral, no hue shift on gray ramps" property test ([`tests`]
//!    below) exact rather than approximate.
//!
//! GPU (`shaders/global_tone_recovery.wgsl`) and CPU
//! ([`ToneRecoveryNode::eval_cpu`] via [`guided_filter_luma`]/[`apply_recovery`])
//! implement the identical two-pass algorithm from the identical formulas
//! (duplicated literally per the established per-node convention, see the
//! WGSL file's own header), so CPU/GPU parity (task **B12**) holds to
//! numerical rounding; verified in `tests/e10_tone_recovery.rs`.
//!
//! # M2-deferred hardening (named, not faked, see
//! `docs/plan/epics/E10-deviations.md`)
//!
//! B1 (real 20-30-photo corpus; this node's tests use synthetic scenes only),
//! B3-B6 (fast-LL spike/bake-off/ADR, the fast-Laplacian-pyramid quality
//! upgrade `common/pyramid.rs`/B8/B9 would ship), B14 (≤20ms perf gate on
//! **reference** RTX-3060-class hardware, this module reports only a local
//! Apple-silicon number; no such runner is available here), B15
//! (stage-interaction tuning against the rest of the tone/color chain), B16
//! (perceptual review, 2+ human reviewers), B17 (freeze goldens + wire the
//! halo gate into PR-blocking CI as a permanent gate). B13 (ROI/tile-border
//! correctness) is **exercised**, not deferred, by this M1 slice, see
//! `plan`'s doc comment and `tests/e10_tone_recovery.rs`.

use std::sync::Arc;

use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The two-pass guided-filter kernel, naga-validated at build (`build.rs`).
const TONE_RECOVERY_WGSL: &str = include_str!("../../../../shaders/global_tone_recovery.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "highlights",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "shadows",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.tone_recovery"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};

// ── algorithm constants (duplicated literally in the WGSL, see that file's
//    header for why; CPU/GPU parity tests catch drift) ─────────────────────

/// Guided-filter box-filter half-window, pixels. A fixed algorithm constant
/// (not a recipe parameter): both guided-filter passes (regression + `(a,b)`
/// smoothing) use it.
pub const GUIDE_RADIUS: u32 = 8;

/// Guided-filter variance regularizer (He & Sun eq. 5's `eps`), in
/// scene-linear luma² units, local luma std below ~6% is treated as flat
/// noise (kept in the base) rather than an edge (kept in the detail).
///
/// **Numerical-stability floor, not just an algorithm knob.** `a =
/// var_l/(var_l+eps)` is computed from `var_l = mean_l2 - mean_l*mean_l`
/// (He & Sun's own two-pass form), a textbook catastrophic-cancellation
/// site: on a smooth low-contrast scene (e.g. the `HighKey` corpus, mean_l ≈
/// 0.85-1.0 so `mean_l*mean_l` ≈ 0.7-1.0) the true `var_l` is a tiny
/// difference of two large near-equal `f32` sums, so its last couple of
/// significant digits are rounding noise, noise that a **separable**
/// box-filter summation (this module's CPU reference, via
/// `nodes::common::guided`) and a **brute-force 2D** summation
/// (`global_tone_recovery.wgsl`'s `pass_a`) accumulate in a different order,
/// and therefore round slightly differently. `a`'s derivative w.r.t. `var_l`
/// peaks at `1/(4·eps)` exactly at `var_l == eps`, i.e. the CPU/GPU
/// rounding-noise gap is *maximally amplified* precisely when a scene's
/// local variance sits near `eps`, which `4.0e-4` did for `HighKey`
/// (measured: a handful of pixels landed 1 sRGB8 LSB apart between backends,
/// enough for CIEDE2000's well-known hue-angle sensitivity on near-neutral
/// colors to read as ΔE2000 ≈ 1.3, see `tests/e10_tone_recovery.rs`'s B12
/// case and its git history for the measurement). Raising `eps` by 10×
/// pushes typical smooth-scene local variance well clear of that peak
/// (`da/dvar` falls off as `eps/(var+eps)²`), which is *also* the
/// perceptually-motivated direction (treat more low-contrast scene content
/// as "flat" rather than "edge," He & Sun's own stated purpose for `eps`)
/// not a threshold fudge in a direction that would trade away quality.
pub const GUIDE_EPS: f32 = 4.0e-3;

/// Reference mid-gray in scene-linear working RGB, the same 18%-gray anchor
/// `global.contrast`'s pivot derives from ([`crate::ng::nodes::global::contrast::contrast_pivot`]),
/// here used directly in linear (not companion-encoded) space as the
/// region-weight functions' zero point.
pub const MID_GRAY: f32 = 0.18;

/// Stops from [`MID_GRAY`] at which the highlight/shadow region weight
/// saturates to 1 (spec §4.5 "quarter-tones").
pub const WEIGHT_KNEE_STOPS: f32 = 2.0;

/// Maximum base-compression/lift magnitude, in stops, at a full ±100 slider
/// deflection, the M1 provisional mapping constant (§6: PV1 is mutable
/// in the M1→M2 window; a future hardening pass may retune this with a
/// reviewed golden update, not a new PV).
///
/// **Monotonicity bound (derived, not tuned by eye).** `new_base(base) = base
/// * compression_gain(base)` is monotone non-decreasing in `base`, the
///   property that keeps a single strong edge in `base` from being locally
///   *reversed* by the recompression curve itself (independent of the guided
///   filter's own edge-preservation), **iff** `MAX_STOPS * 0.75 < 1.0`: writing
///   `u = ln(base)`, `d(ln new_base)/du = 1 + d(stops_delta)/d(stops)`, and
///   [`highlight_weight`]/[`shadow_weight`] are raised-cosine-style smoothstep
///   ramps over a `WEIGHT_KNEE_STOPS`-wide window whose steepest slope is
///   `1.5 / WEIGHT_KNEE_STOPS = 0.75` (`WEIGHT_KNEE_STOPS == 2.0`); since the two
///   weights' *derivatives* are disjoint by construction (never simultaneously
///   non-zero, see [`compression_gain`]'s docs) the worst case is a single
///   term at full slider deflection: `d(stops_delta)/d(stops) ≥ -MAX_STOPS *
///   0.75`. `MAX_STOPS = 1.0` gives a comfortable analytic floor of `1 - 0.75 =
///   0.25 > 0` (proven exactly by [`tests::base_recompression_is_monotone_across_the_full_range`]
///   via a dense numerical sweep, the M1 slice's version of the B2 halo-metric
///   contract, checked at the curve level before it ever reaches a rendered
///   pixel). **`MAX_STOPS = 1.5` (this constant's original M1-draft value)
///   violates the bound** (`1.5 * 0.75 = 1.125 > 1`) and was found, by this
///   derivation, to produce a small locally-*decreasing* region in the
///   recompression curve near `shadows = 100` at `base ≈ MID_GRAY / 2`, fixed
///   here before this node was wired into the develop chain.
pub const MAX_STOPS: f32 = 1.0;

/// Detail-layer re-add gain clamp (spec §4.5 "detail re-add with a gain
/// clamp"), bounds how much the base's local compression gain also scales
/// the re-added high-frequency detail.
pub const DETAIL_GAIN_MIN: f32 = 0.5;
pub const DETAIL_GAIN_MAX: f32 = 1.8;

/// Numerical floor for luma used as a divisor.
pub const LUMA_EPS: f32 = 1.0e-4;

/// Chroma-guard ceiling on the final luma-ratio RGB reapply (spec §4.5
/// "chroma guard"), bounds the per-pixel RGB gain so a near-zero-luma pixel
/// can never blow up.
pub const RATIO_MAX: f32 = 8.0;

// ── B7: the (highlights, shadows) -> recovery-param mapping ────────────────

/// Smoothstep (Hermite) interpolation, clamped outside `[edge0, edge1]`
/// shared by [`highlight_weight`]/[`shadow_weight`] and
/// `global_tone_recovery.wgsl`'s `smoothstep_01`.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Stops of `luma` from [`MID_GRAY`] (negative = darker than mid-gray).
/// `luma` is floored at [`LUMA_EPS`] so `log2` never sees a non-positive
/// input.
#[inline]
pub fn stops_from_luma(luma: f32) -> f32 {
    (luma.max(LUMA_EPS) / MID_GRAY).log2()
}

/// The highlight region weight: 0 at/below mid-gray, ramping to 1 by
/// [`WEIGHT_KNEE_STOPS`] stops above it. **Exactly** 0 for any `stops <= 0`
/// (the `smoothstep` clamp), the structural half of the "highlights never
/// lift blacks" asymmetry rule (task B7): a positive `highlights` has zero
/// effect anywhere at or below mid-gray, so it cannot lift a shadow pixel by
/// construction, not by clamping the result after the fact.
#[inline]
pub fn highlight_weight(stops: f32) -> f32 {
    smoothstep(0.0, WEIGHT_KNEE_STOPS, stops)
}

/// The shadow region weight: 0 at/above mid-gray, ramping to 1 by
/// [`WEIGHT_KNEE_STOPS`] stops below it. **Exactly** 0 for any `stops >= 0`
/// the structural half of "shadows never pull down whites" (task B7): a
/// positive `shadows` has zero effect anywhere at or above mid-gray.
#[inline]
pub fn shadow_weight(stops: f32) -> f32 {
    smoothstep(0.0, WEIGHT_KNEE_STOPS, -stops)
}

/// The B7 param mapping: `(highlights, shadows) ∈ [-100,100]²` and a
/// guided-filter base luma value `base_luma` -> a multiplicative gain applied
/// to that base. `highlights > 0` darkens (`stops_delta < 0`) only where
/// [`highlight_weight`] is non-zero (above mid-gray); `shadows > 0` brightens
/// (`stops_delta > 0`) only where [`shadow_weight`] is non-zero (below
/// mid-gray). The two weights are disjoint by construction (both are exactly
/// 0 at `stops == 0` and never both non-zero for the same `stops`), so the
/// two sliders can never fight or double up on the same pixel.
///
/// `highlights == shadows == 0.0` ⇒ `stops_delta == 0.0` for **every**
/// `base_luma` ⇒ `gain == 1.0`, the exact identity
/// [`ToneRecoveryNode::is_identity`] elides the node on.
#[inline]
pub fn compression_gain(base_luma: f32, highlights: f32, shadows: f32) -> f32 {
    let stops = stops_from_luma(base_luma);
    let hw = highlight_weight(stops);
    let sw = shadow_weight(stops);
    let stops_delta = (shadows / 100.0) * sw * MAX_STOPS - (highlights / 100.0) * hw * MAX_STOPS;
    stops_delta.exp2()
}

/// Steps 3-4 of the algorithm doc above: compresses `base` by
/// [`compression_gain`] and re-adds `detail` scaled by the same gain, clamped
/// to `[DETAIL_GAIN_MIN, DETAIL_GAIN_MAX]`.
#[inline]
pub fn recovered_luma(base: f32, detail: f32, highlights: f32, shadows: f32) -> f32 {
    let gain = compression_gain(base, highlights, shadows);
    let new_base = base * gain;
    let dgain = gain.clamp(DETAIL_GAIN_MIN, DETAIL_GAIN_MAX);
    new_base + detail * dgain
}

/// Step 5's chroma-guarded ratio: `out_luma / luma`, floored/ceilinged so a
/// near-zero `luma` divisor can never blow the ratio up.
#[inline]
pub fn reapply_ratio(out_luma: f32, luma: f32) -> f32 {
    (out_luma / luma.max(LUMA_EPS)).clamp(0.0, RATIO_MAX)
}

/// The full per-pixel recovery: given a working RGBA pixel, its own working
/// luma, and the guided-filter base value at that pixel (`base_luma == luma`
/// is the valid "no spatial term" degenerate case, the halo metric's
/// curve-only reference uses exactly that), returns the recovered RGBA
/// (alpha untouched). The CPU parity anchor for `global_tone_recovery.wgsl`'s
/// `pass_c`.
#[inline]
pub fn apply_recovery(
    rgba: [f32; 4],
    luma: f32,
    base_luma: f32,
    highlights: f32,
    shadows: f32,
) -> [f32; 4] {
    let detail = luma - base_luma;
    let out_luma = recovered_luma(base_luma, detail, highlights, shadows);
    let ratio = reapply_ratio(out_luma, luma);
    let out = [rgba[0] * ratio, rgba[1] * ratio, rgba[2] * ratio, rgba[3]];
    // Defensive NaN/Inf guard (task B11 "no NaN/Inf on fuzzed inputs"): a
    // non-finite RGB component (e.g. from a NaN `base_luma` the guided
    // filter's box-averaging spread from a corrupted upstream pixel) falls
    // back to the untouched input pixel rather than propagating, mirrors
    // `global_tone_recovery.wgsl`'s `pass_c` NaN guard exactly.
    if !out[0].is_finite() || !out[1].is_finite() || !out[2].is_finite() {
        return rgba;
    }
    out
}

// ── luma weights + guided filter (CPU reference) ────────────────────────────

/// The working space's (ProPhoto-linear, D50) own CIE `Y` row, the luma
/// weight vector both `eval_cpu` and `eval_gpu`'s UBO write use, computed
/// from [`lightbox_color::matrix::spaces::working_to_xyz_d50`] rather than a
/// hand-copied literal so it can never drift from the real working→XYZ
/// matrix (the `working_to_xyz_d50 → XYZ Y row → luma weights` chain is the
/// mathematically exact definition of luminance for this primaries set; the
/// row-1 choice is asserted by [`tests::luma_weights_sum_to_one`]).
pub fn working_luma_weights() -> [f32; 3] {
    let m = lightbox_color::matrix::spaces::working_to_xyz_d50();
    [m.0[1][0] as f32, m.0[1][1] as f32, m.0[1][2] as f32]
}

/// `dot(rgb, weights)`.
#[inline]
pub fn working_luma(rgb: [f32; 3], weights: [f32; 3]) -> f32 {
    rgb[0] * weights[0] + rgb[1] * weights[1] + rgb[2] * weights[2]
}

/// The self-guided (`I = p = luma`) fast guided filter (He & Sun 2015; He,
/// Sun, Tang 2010 eq. 4-8) over a `w`×`h` luma plane, at [`GUIDE_RADIUS`] /
/// [`GUIDE_EPS`], the CPU reference [`ToneRecoveryNode::eval_cpu`] and the
/// halo metric (`lightbox_render_testkit::halo`) share. Border: clamp-to-edge
/// (every window is the full `(2r+1)²` size; no zero-padding bias), the same
/// border rule `slice_subtile`'s apron already establishes at the true image
/// border, so this filter is correct whether `luma` came from a whole-image
/// tile or an apron-padded sub-tile (task B13).
///
/// Delegates to [`crate::ng::nodes::common::guided::guided_filter_self`] (a
/// **separable** box filter, O(w·h·r) per axis rather than this fixed
/// window's brute-force O(w·h·r²)) for the two box-filter passes, then
/// recombines `base = mean_a·L + mean_b` (He & Sun eq. 8) here. All
/// accumulation is `f32` (spec §4.2's "ToneRecoveryNode ... run in f32"),
/// matching WGSL's own `f32` registers so CPU/GPU parity holds to rounding
/// **not** bit-identical to `global_tone_recovery.wgsl`'s brute-force 2D
/// passes (a different summation order over the identical `(2r+1)²` window),
/// which is exactly the tolerance-based (ΔE2000/PSNR, never bit-equality)
/// parity contract spec §4.4 states.
pub fn guided_filter_luma(luma: &[f32], w: u32, h: u32) -> Vec<f32> {
    guided_filter_luma_with(luma, w, h, GUIDE_RADIUS, GUIDE_EPS)
}

/// [`guided_filter_luma`] with an explicit `(radius, eps)`, exposed for the
/// halo metric / algorithm-sensitivity tests; production callers use the
/// fixed-constant [`guided_filter_luma`].
pub fn guided_filter_luma_with(luma: &[f32], w: u32, h: u32, radius: u32, eps: f32) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (mean_a, mean_b) =
        crate::ng::nodes::common::guided::guided_filter_self(luma, w, h, radius, eps);
    let mut base = vec![0f32; luma.len()];
    base.iter_mut().enumerate().for_each(|(i, out)| {
        *out = mean_a[i] * luma[i] + mean_b[i];
    });
    base
}

/// The `global.tone_recovery` develop node (E10 §6 M1-slice provisional
/// `ToneRecoveryNode`).
#[derive(Default)]
pub struct ToneRecoveryNode {}

impl ToneRecoveryNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.tone_recovery");

    /// A fresh node.
    pub fn new() -> ToneRecoveryNode {
        ToneRecoveryNode::default()
    }
}

impl RenderNode for ToneRecoveryNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    /// ROI back-propagation (spec §3.2 `plan`, task B13): the two sequential
    /// `GUIDE_RADIUS` box-filter passes need `2 * GUIDE_RADIUS` of upstream
    /// context to be apron-correct at a tile border. `eval_cpu` honors the
    /// apron this returns (maps every output pixel through the `in_roi` ↔
    /// `out_roi` offset, see its own doc comment), so tiled evaluation via
    /// `ng::exec::cpu::tiling::TileRender` is bit-identical to a whole-image
    /// eval to float rounding (`tests/e10_tone_recovery.rs`'s B13 case). The
    /// **production** `ng::exec::Executor` v1 walk does not yet exercise this
    /// path itself, it evaluates every node over the whole request ROI as a
    /// single tile (see that module's doc comment), so `plan`/`roi_in`'s
    /// correctness here is proven by the reference tiling harness ahead of
    /// C's real tiling landing in `Engine::submit`, not by a live caller yet.
    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois(vec![out.expand(2 * GUIDE_RADIUS); 1])
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("global.tone_recovery: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.tone_recovery: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let highlights = params.get_f64_or("highlights", 0.0) as f32;
        let shadows = params.get_f64_or("shadows", 0.0) as f32;
        let weights = working_luma_weights();

        let mut ubo_bytes = [0u8; 32];
        ubo_bytes[0..4].copy_from_slice(&weights[0].to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&weights[1].to_le_bytes());
        ubo_bytes[8..12].copy_from_slice(&weights[2].to_le_bytes());
        ubo_bytes[12..16].copy_from_slice(&highlights.to_le_bytes());
        ubo_bytes[16..20].copy_from_slice(&shadows.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.tone_recovery params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        // Scratch (a, b) texture for the guided-filter regression
        // coefficients, created directly on the shared device (this node's
        // own transient resource, not a pooled working/output tile). `f32`
        // (not the working tile's usual `f16`): `a = var/(var+GUIDE_EPS)` is
        // numerically stiff near `var ≈ GUIDE_EPS`, so rounding it to `f16`
        // between the two passes measurably widened CPU/GPU parity beyond
        // tolerance on smooth low-variance scenes, see
        // `global_tone_recovery.wgsl`'s header for the full account.
        let scratch = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("global.tone_recovery scratch a,b"),
            size: wgpu::Extent3d {
                width: extent.w.max(1),
                height: extent.h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());

        // Pass A: box-filter regression coefficients (a, b) from `src`.
        let pipe_a = ctx.kernels.compute_pipeline(TONE_RECOVERY_WGSL, "pass_a")?;
        let bg0_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_a in"),
            layout: &pipe_a.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg1_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_a out"),
            layout: &pipe_a.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&scratch_view),
            }],
        });
        let bg2_a = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_a params"),
            layout: &pipe_a.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_a,
            &[&bg0_a, &bg1_a, &bg2_a],
            extent,
            "global.tone_recovery.pass_a",
        );

        // Pass C: box-filter (a, b) back to (mean_a, mean_b), recombine +
        // compress + reapply.
        let pipe_c = ctx.kernels.compute_pipeline(TONE_RECOVERY_WGSL, "pass_c")?;
        let bg0_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_c in"),
            layout: &pipe_c.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scratch_view),
                },
            ],
        });
        // `dst_out` is at @group(1)@binding(1) in the WGSL (binding 0 is
        // `dst_ab`, `pass_c`-unreferenced so absent from this pipeline's
        // auto-derived group(1) layout).
        let bg1_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_c out"),
            layout: &pipe_c.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg2_c = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.tone_recovery pass_c params"),
            layout: &pipe_c.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_c,
            &[&bg0_c, &bg1_c, &bg2_c],
            extent,
            "global.tone_recovery.pass_c",
        );

        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input_view = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.tone_recovery: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let highlights = params.get_f64_or("highlights", 0.0) as f32;
        let shadows = params.get_f64_or("shadows", 0.0) as f32;
        let weights = working_luma_weights();

        // Build the luma plane + guided-filter base over the **whole input
        // tile** (which may be apron-padded beyond `out`'s own extent, task
        // B13: `plan()` requests `out.expand(2 * GUIDE_RADIUS)`, so
        // `input.extent` and `out.extent` differ whenever a caller honors
        // that apron, e.g. the tiled `exec::cpu::tiling::TileRender`
        // reference path). Computing over the input's own extent, then
        // mapping each *output* pixel through the `in_roi`↔`out_roi` offset
        // below, is what makes a tiled eval bit-identical to a whole-image
        // eval (the same apron-offset pattern `test.blur_r`/`test.accum`
        // establish in `lightbox-render-testkit::probes`).
        let (iw, ih) = (input.extent.w, input.extent.h);
        let mut luma_plane = vec![0f32; (iw as usize) * (ih as usize)];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                luma_plane[(y * iw + x) as usize] = working_luma([p[0], p[1], p[2]], weights);
            }
        }
        let base_plane = guided_filter_luma(&luma_plane, iw, ih);

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        // The output tile's origin, expressed in the input tile's own local
        // pixel coordinates (zero when `in_roi == out_roi`, i.e. every
        // production call today, see `plan`'s doc comment).
        let ox = out_roi.x as i64 - in_roi.x as i64;
        let oy = out_roi.y as i64 - in_roi.y as i64;
        let ix_max = iw as i64 - 1;
        let iy_max = ih as i64 - 1;
        out.par_fill_rows(|ly, row| {
            let iy = (oy + ly as i64).clamp(0, iy_max) as u32;
            for lx in 0..w {
                let ix = (ox + lx as i64).clamp(0, ix_max) as u32;
                let p = input.get_rgba_f32(ix, iy);
                let i = (iy * iw + ix) as usize;
                let o = apply_recovery(p, luma_plane[i], base_plane[i], highlights, shadows);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for ToneRecoveryNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.highlights == 0.0 && p.shadows == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([
            ("highlights", ParamValue::Float(p.highlights as f64)),
            ("shadows", ParamValue::Float(p.shadows as f64)),
        ])
        .expect("highlights/shadows are always finite (clamped on ingest — spec A2)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(2 * GUIDE_RADIUS)
    }
}

/// Factory registering [`ToneRecoveryNode`].
#[derive(Default)]
pub struct ToneRecoveryFactory {}

impl NodeFactory for ToneRecoveryFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ToneRecoveryNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(TONE_RECOVERY_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_weights_sum_to_one() {
        // White (1,1,1) must map to Y = 1 by the primaries-normalization
        // rgb_to_xyz_matrix performs (the primaries are scaled to sum to the
        // white point), this is the sanity check that row 1 really is Y.
        let w = working_luma_weights();
        let sum = w[0] + w[1] + w[2];
        assert!(
            (sum - 1.0).abs() < 1e-4,
            "luma weights sum to {sum}, want 1"
        );
        assert!(w.iter().all(|&c| c > 0.0), "luma weights: {w:?}");
    }

    // ── B7 param-mapping property tests ─────────────────────────────────────

    #[test]
    fn zero_highlights_zero_shadows_is_exact_identity_gain() {
        for base in [0.0f32, LUMA_EPS, 0.02, 0.18, 0.5, 1.0, 2.0] {
            let gain = compression_gain(base, 0.0, 0.0);
            assert!((gain - 1.0).abs() < 1e-6, "base={base}: gain={gain}");
        }
    }

    /// The full per-pixel op at (highlights, shadows) = (0, 0) is an exact
    /// identity, the property `is_identity`'s elision relies on (identity
    /// even without elision).
    #[test]
    fn apply_recovery_is_exact_identity_at_zero_zero() {
        let rgba = [0.3f32, 0.6, 0.9, 1.0];
        let luma = working_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
        for base in [0.0f32, 0.05, luma, 0.4, 1.5] {
            let o = apply_recovery(rgba, luma, base, 0.0, 0.0);
            for c in 0..3 {
                assert!(
                    (o[c] - rgba[c]).abs() < 1e-4,
                    "channel {c}: base={base} got {o:?} want {rgba:?}"
                );
            }
            assert_eq!(o[3], rgba[3]);
        }
    }

    /// **B7**: highlights never lift blacks, a deep-shadow base sees exactly
    /// zero effect from `highlights` at any magnitude.
    #[test]
    fn highlights_never_move_a_deep_shadow_base() {
        let deep_shadow = MID_GRAY * 2f32.powf(-4.0); // -4 stops, past the knee
        for highlights in [-100.0f32, -50.0, 50.0, 100.0] {
            let gain = compression_gain(deep_shadow, highlights, 0.0);
            assert!(
                (gain - 1.0).abs() < 1e-6,
                "highlights={highlights}: gain={gain} (deep shadow must be untouched)"
            );
        }
    }

    /// **B7**: shadows never pull down whites, a blown-highlight base sees
    /// exactly zero effect from `shadows` at any magnitude.
    #[test]
    fn shadows_never_move_a_blown_highlight_base() {
        let blown = MID_GRAY * 2f32.powf(4.0); // +4 stops, past the knee
        for shadows in [-100.0f32, -50.0, 50.0, 100.0] {
            let gain = compression_gain(blown, 0.0, shadows);
            assert!(
                (gain - 1.0).abs() < 1e-6,
                "shadows={shadows}: gain={gain} (blown highlight must be untouched)"
            );
        }
    }

    /// **B7**: monotone response, increasing `shadows` monotonically
    /// increases the gain (brighter) for a fixed dark base; increasing
    /// `highlights` monotonically decreases the gain (darker) for a fixed
    /// bright base.
    #[test]
    fn monotone_response_to_each_slider() {
        let dark = MID_GRAY * 2f32.powf(-3.0);
        let mut prev = 0.0f32;
        for shadows in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
            let gain = compression_gain(dark, 0.0, shadows);
            assert!(
                gain >= prev - 1e-6,
                "shadows={shadows}: gain={gain} < prev={prev}"
            );
            prev = gain;
        }

        let bright = MID_GRAY * 2f32.powf(3.0);
        let mut prev = f32::INFINITY;
        for highlights in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
            let gain = compression_gain(bright, highlights, 0.0);
            assert!(
                gain <= prev + 1e-6,
                "highlights={highlights}: gain={gain} > prev={prev}"
            );
            prev = gain;
        }
    }

    /// **B7 / halo precondition**: the base-recompression curve
    /// `new_base(base) = base * compression_gain(base, h, s)` is monotone
    /// non-decreasing in `base` for **every** `(highlights, shadows)` corner
    /// of the ±100 slider square, a dense numerical sweep proving the
    /// [`MAX_STOPS`] analytic bound holds in the actual implementation (this
    /// is the property whose violation, at the pre-fix `MAX_STOPS = 1.5`,
    /// would let the recompression curve itself introduce a gradient
    /// reversal, independent of whatever the guided filter's edge-awareness
    /// does, exactly the failure mode the B2 halo metric is built to catch
    /// downstream of a rendered image; this test catches it at the curve
    /// level, before a single pixel is touched).
    #[test]
    fn base_recompression_is_monotone_across_the_full_range() {
        for highlights in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
            for shadows in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
                let mut prev_new_base = f32::NEG_INFINITY;
                let mut prev_base = f32::NEG_INFINITY;
                // Log-spaced sweep from deep shadow to well-blown highlight
                // (roughly ±5 stops either side of mid-gray).
                for i in 0..=400 {
                    let stops = -5.0 + 10.0 * (i as f32 / 400.0);
                    let base = MID_GRAY * 2f32.powf(stops);
                    let gain = compression_gain(base, highlights, shadows);
                    let new_base = base * gain;
                    assert!(
                        new_base >= prev_new_base - 1e-5,
                        "h={highlights} s={shadows}: non-monotone at base={base} \
                         (prev_base={prev_base} -> new_base={new_base} < prev={prev_new_base})"
                    );
                    prev_new_base = new_base;
                    prev_base = base;
                }
            }
        }
    }

    /// **B7**: channel-neutral, no hue shift at all (not merely < 0.5 ΔE) on
    /// a gray ramp, for any (highlights, shadows, base) combination, because
    /// the reapply is a single scalar ratio shared by r, g, b exactly.
    #[test]
    fn channel_neutral_on_a_gray_ramp() {
        for gray in [0.02f32, 0.1, 0.18, 0.4, 0.8, 1.2] {
            let rgba = [gray, gray, gray, 1.0];
            for highlights in [-100.0f32, 0.0, 100.0] {
                for shadows in [-100.0f32, 0.0, 100.0] {
                    for base in [gray * 0.5, gray, gray * 1.5] {
                        let o = apply_recovery(rgba, gray, base, highlights, shadows);
                        assert_eq!(o[0], o[1]);
                        assert_eq!(o[1], o[2]);
                    }
                }
            }
        }
    }

    #[test]
    fn reapply_ratio_is_clamped_and_safe_near_zero_luma() {
        let r = reapply_ratio(10.0, 0.0);
        assert!(r.is_finite() && r <= RATIO_MAX);
        let r_id = reapply_ratio(0.5, 0.5);
        assert!((r_id - 1.0).abs() < 1e-6);
    }

    // ── guided filter sanity ────────────────────────────────────────────────

    #[test]
    fn guided_filter_of_a_flat_image_is_the_flat_value() {
        let (w, h) = (16u32, 16u32);
        let luma = vec![0.42f32; (w * h) as usize];
        let base = guided_filter_luma(&luma, w, h);
        for &b in &base {
            assert!((b - 0.42).abs() < 1e-4, "b={b}");
        }
    }

    #[test]
    fn guided_filter_never_produces_nan_or_negative_on_a_gradient() {
        let (w, h) = (32u32, 32u32);
        let mut luma = vec![0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                luma[(y * w + x) as usize] = x as f32 / w as f32;
            }
        }
        let base = guided_filter_luma(&luma, w, h);
        assert!(base.iter().all(|b| b.is_finite() && *b >= -1e-4));
    }

    // ── node wiring ─────────────────────────────────────────────────────────

    #[test]
    fn is_identity_tracks_both_fields() {
        let mut g = GlobalStages::default();
        assert!(ToneRecoveryNode::is_identity(&g));
        g.highlights = 5.0;
        assert!(!ToneRecoveryNode::is_identity(&g));
        g.highlights = 0.0;
        g.shadows = -5.0;
        assert!(!ToneRecoveryNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_highlights_and_shadows() {
        let g = GlobalStages {
            highlights: 30.0,
            shadows: -20.0,
            ..GlobalStages::default()
        };
        let pb = ToneRecoveryNode::param_block(&g);
        assert_eq!(pb.get_f64("highlights"), Some(30.0));
        assert_eq!(pb.get_f64("shadows"), Some(-20.0));
    }

    #[test]
    fn plan_pads_by_two_guide_radii() {
        let node = ToneRecoveryNode::new();
        let out = Roi {
            x: 10,
            y: 10,
            w: 100,
            h: 100,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        let rois = node.plan(out, 1.0, &params);
        assert_eq!(rois.0.len(), 1);
        assert_eq!(rois.0[0], out.expand(2 * GUIDE_RADIUS));
    }
}
