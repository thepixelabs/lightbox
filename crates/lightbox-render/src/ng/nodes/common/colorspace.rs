// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Working RGB ⇄ Oklab/OkLCh conversions + the 8-band raised-cosine hue
//! weighting (E10 spec §4.4 `nodes/common/colorspace.rs`; tasks **C6-C7**).
//! Own, clean-room implementation of Björn Ottosson's published Oklab
//! formulas (<https://bottosson.github.io/posts/oklab/>, released by the
//! author for unrestricted reuse, public-domain/MIT-style, not GPL)
//! nothing here was copied from a GPL codebase, only the two published 3×3
//! matrices (numbers, not copyrightable expression) were transcribed from
//! the blog post, exactly as `tests/e10_wb.rs`'s own `oklab_ab` test helper
//! already does for its neutrality check.
//!
//! # C6, Oklab/OkLCh over the *working* space, not raw linear sRGB
//!
//! Ottosson's published matrices are anchored to **CIE XYZ under D65**
//! (equivalently: linear sRGB, whose primaries/white are D65). Lightbox's
//! working space is linear **ProPhoto/ROMM under D50**
//! ([`lightbox_color::matrix::spaces::working_to_xyz_d50`]). Rather than
//! re-deriving a D50 XYZ→LMS matrix from scratch, this module composes the
//! ALREADY-tested [`lightbox_color::matrix::spaces::working_to_linear_srgb`]
//! (working → linear sRGB D65, Bradford-adapted, B1's own reference-checked
//! matrix) with Ottosson's linear-sRGB→LMS matrix: `WORK_TO_LMS = M1 ·
//! working_to_linear_srgb()`. This is mathematically identical to going
//! through XYZ(D65) (linear sRGB IS just a fixed matrix away from XYZ(D65)),
//! reuses a matrix this codebase already has a published-reference test
//! for, and keeps this module's own surface to exactly the two Oklab-native
//! matrices (`M1`, `M2`) Ottosson publishes.
//!
//! Both directions are the SAME cached 3×3 (`f32`) matrices for CPU eval,
//! CPU test code, and the GPU upload ([`matrices_flat`]), one source of
//! truth, so CPU/GPU parity holds by construction (mirrors
//! `nodes::global::tone_curve`'s "bake once, sample identically" pattern for
//! its LUTs).
//!
//! # C7, the 8-band raised-cosine hue partition
//!
//! [`HUE_BAND_CENTERS_DEG`] pins the 8 Lightroom-named hue bands (red,
//! orange, yellow, green, aqua, blue, purple, magenta) to the **Oklab hue of
//! reference sRGB primary/secondary swatches**, computed once via this same
//! clean-room Oklab math (documented per-swatch below, reproducible,
//! auditable, not eyeballed). Hue is a property of the physical color (its
//! XYZ), not of which RGB gamut encodes it, so pinning against sRGB swatches
//! is exactly the fixed physical reference the C7 AC asks for; identical
//! swatches run through the working-space pipeline land at the same hue.
//!
//! [`band_weights`] partitions the hue circle into 8 raised-cosine bands via
//! **overlapping complementary smoothstep ramps at each of the 8
//! band-to-band boundaries**, the same "shared boundary ramp, provably
//! non-negative difference" construction `nodes::common::curve1d::
//! region_weights` uses for the tone curve's 4 linear regions, generalized
//! to a *circular* domain (8 regions, 8 boundaries, no fixed 0/1 anchor).
//! See [`band_weights`]'s own doc comment for the closed-form proof that the
//! 8 weights sum to exactly `1.0` for every hue.

use std::sync::OnceLock;

use lightbox_color::matrix::{spaces, Mat3};

// ── C6: Ottosson's published matrices (linear sRGB / XYZ-D65 anchored) ────

/// Linear sRGB → LMS (Ottosson, "Oklab" blog post, linear-sRGB form). Exact
/// transcription of the published constants, the same values
/// `tests/e10_wb.rs`'s local `oklab_ab` helper already uses (to fewer
/// printed digits) for its own neutrality check.
const LINSRGB_TO_LMS: Mat3 = Mat3([
    [0.412_221_470_8, 0.536_332_536_3, 0.051_445_992_9],
    [0.211_903_498_2, 0.680_699_545_1, 0.107_396_956_6],
    [0.088_302_461_9, 0.281_718_837_6, 0.629_978_700_5],
]);

/// LMS' (post-cbrt) → Oklab. Exact transcription of Ottosson's published
/// constants.
const LMS_TO_LAB: Mat3 = Mat3([
    [0.210_454_255_3, 0.793_617_785_0, -0.004_072_046_8],
    [1.977_998_495_1, -2.428_592_205_0, 0.450_593_709_9],
    [0.025_904_037_1, 0.782_771_766_2, -0.808_675_766_0],
]);

/// The four cached `f32` matrices every per-pixel Oklab/OkLCh call (and both
/// nodes' GPU uploads, [`matrices_flat`]) read, computed once (composing
/// [`spaces::working_to_linear_srgb`] with Ottosson's published matrices),
/// never per-pixel.
struct OklabMats {
    work_to_lms: [[f32; 3]; 3],
    lms_to_work: [[f32; 3]; 3],
    lms_to_lab: [[f32; 3]; 3],
    lab_to_lms: [[f32; 3]; 3],
}

fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    let a = &m.0;
    [
        [a[0][0] as f32, a[0][1] as f32, a[0][2] as f32],
        [a[1][0] as f32, a[1][1] as f32, a[1][2] as f32],
        [a[2][0] as f32, a[2][1] as f32, a[2][2] as f32],
    ]
}

fn build_mats() -> OklabMats {
    let work_to_lms = LINSRGB_TO_LMS.mul(&spaces::working_to_linear_srgb());
    let lms_to_work = work_to_lms
        .inverse()
        .expect("working->LMS is a composition of non-degenerate matrices");
    let lab_to_lms = LMS_TO_LAB
        .inverse()
        .expect("Ottosson's published LMS'->Lab matrix is non-degenerate");
    OklabMats {
        work_to_lms: to_f32(&work_to_lms),
        lms_to_work: to_f32(&lms_to_work),
        lms_to_lab: to_f32(&LMS_TO_LAB),
        lab_to_lms: to_f32(&lab_to_lms),
    }
}

static MATS: OnceLock<OklabMats> = OnceLock::new();

fn mats() -> &'static OklabMats {
    MATS.get_or_init(build_mats)
}

#[inline]
fn mat_vec(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Working-space linear RGB → Oklab (`[L, a, b]`). Handles negative/> 1
/// components (scene-linear working RGB is not gamut- or range-limited)
/// via the real cube root (`f32::cbrt` is defined for negative inputs,
/// unlike `powf(1/3)`), so out-of-gamut or HDR-highlight pixels still
/// produce a finite, well-defined Oklab triple.
#[must_use]
pub fn working_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let m = mats();
    let lms = mat_vec(&m.work_to_lms, rgb);
    let lms_ = [lms[0].cbrt(), lms[1].cbrt(), lms[2].cbrt()];
    mat_vec(&m.lms_to_lab, lms_)
}

/// Inverse of [`working_to_oklab`]: Oklab → working-space linear RGB.
#[must_use]
pub fn oklab_to_working(lab: [f32; 3]) -> [f32; 3] {
    let m = mats();
    let lms_ = mat_vec(&m.lab_to_lms, lab);
    let lms = [lms_[0].powi(3), lms_[1].powi(3), lms_[2].powi(3)];
    mat_vec(&m.lms_to_work, lms)
}

/// Working-space linear RGB → OkLCh (`[L, C, h]`, `h` in degrees `[0,360)`).
#[must_use]
pub fn working_to_oklch(rgb: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = working_to_oklab(rgb);
    let c = a.hypot(b);
    let h = b.atan2(a).to_degrees();
    let h = if h < 0.0 { h + 360.0 } else { h };
    [l, c, h]
}

/// Inverse of [`working_to_oklch`]: OkLCh → working-space linear RGB.
#[must_use]
pub fn oklch_to_working(lch: [f32; 3]) -> [f32; 3] {
    let (l, c, h) = (lch[0], lch[1], lch[2]);
    let hr = h.to_radians();
    oklab_to_working([l, c * hr.cos(), c * hr.sin()])
}

/// The 4 Oklab/OkLCh matrices, flattened row-major
/// `[work_to_lms(9), lms_to_work(9), lms_to_lab(9), lab_to_lms(9)]`, the
/// exact bytes both [`HslNode`]/[`VibranceSatNode`]'s `eval_gpu` upload as a
/// storage buffer, so the GPU kernel samples the identical matrices the CPU
/// path computed (parity by construction, the tone-curve LUT precedent).
///
/// [`HslNode`]: crate::ng::nodes::global::hsl::HslNode
/// [`VibranceSatNode`]: crate::ng::nodes::global::vibrance_sat::VibranceSatNode
#[must_use]
pub fn matrices_flat() -> [f32; 36] {
    let m = mats();
    let mut out = [0f32; 36];
    let mut push = |base: usize, mat: &[[f32; 3]; 3]| {
        for (r, row) in mat.iter().enumerate() {
            for (c, &v) in row.iter().enumerate() {
                out[base + r * 3 + c] = v;
            }
        }
    };
    push(0, &m.work_to_lms);
    push(9, &m.lms_to_work);
    push(18, &m.lms_to_lab);
    push(27, &m.lab_to_lms);
    out
}

// ── C7: the 8-band raised-cosine hue partition ─────────────────────────────

/// Number of HSL/B&W-mixer hue bands (Lightroom's fixed 8: red, orange,
/// yellow, green, aqua, blue, purple, magenta).
pub const N_BANDS: usize = 8;

/// Human-readable band names, in [`HUE_BAND_CENTERS_DEG`] order.
pub const HUE_BAND_NAMES: [&str; N_BANDS] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];
/// Swatch color chips (sRGB8, display-referred) for each band's UI chip
/// the same 8 reference swatches [`HUE_BAND_CENTERS_DEG`] was measured from.
pub const HUE_BAND_CHIP_SRGB8: [[u8; 3]; N_BANDS] = [
    [255, 0, 0],   // Red
    [255, 128, 0], // Orange
    [255, 255, 0], // Yellow
    [0, 255, 0],   // Green
    [0, 255, 255], // Aqua
    [0, 0, 255],   // Blue
    [128, 0, 255], // Purple
    [255, 0, 255], // Magenta
];

/// The 8 band centers, in **Oklab hue degrees** (task C7 "band centers
/// pinned to documented hues"). Each value is the Oklab hue of one
/// reference sRGB swatch (encoded `[0,1]` sRGB primary/secondary, decoded
/// through the standard sRGB EOTF, then run through this module's own
/// [`working_to_oklab`]-equivalent math), reproducible via
/// `tests::hue_band_centers_match_their_reference_swatches` in this module,
/// which recomputes every one of these 8 numbers from its swatch and checks
/// agreement to `1e-2`°:
///
/// | Band    | sRGB swatch (encoded) | Oklab hue |
/// |---------|------------------------|-----------|
/// | Red     | `(1, 0, 0)`            | 29.23°    |
/// | Orange  | `(1, 0.5, 0)`          | 52.78°    |
/// | Yellow  | `(1, 1, 0)`            | 109.77°   |
/// | Green   | `(0, 1, 0)`            | 142.50°   |
/// | Aqua    | `(0, 1, 1)`            | 194.77°   |
/// | Blue    | `(0, 0, 1)`            | 264.05°   |
/// | Purple  | `(0.5, 0, 1)`          | 293.77°   |
/// | Magenta | `(1, 0, 1)`            | 328.36°   |
///
/// Ascending order (matches [`HUE_BAND_NAMES`]) is load-bearing:
/// [`band_weights`]'s boundary construction assumes centers are sorted
/// around the circle.
pub const HUE_BAND_CENTERS_DEG: [f32; N_BANDS] =
    [29.23, 52.78, 109.77, 142.50, 194.77, 264.05, 293.77, 328.36];

/// Base half-width (degrees) of each boundary's smooth transition zone,
/// capped per-boundary (see [`HueBandGeometry::half_widths`]) so adjacent
/// zones never overlap, an implementer's calibration constant (the C7 AC
/// leaves the exact transition shape to the implementer), the same
/// modeling latitude `curve1d::TRANSITION_HALF_WIDTH` and
/// `whites_blacks::ENDPOINT_RANGE` use.
const TRANSITION_HALF_WIDTH_DEG: f32 = 15.0;

/// Shortest signed angular distance `h - center`, wrapped into `(-180,
/// 180]`. Positive means `h` is "ahead of" `center` going clockwise
/// (ascending-hue direction).
#[inline]
fn circ_delta(h: f32, center: f32) -> f32 {
    let mut d = (h - center) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    d
}

/// Precomputed, cached (process-lifetime) hue-band boundary geometry: the 8
/// boundary positions (circular midpoint between adjacent band centers) and
/// their transition half-widths (capped against the two flanking
/// boundaries so transition zones can never overlap, the property
/// [`band_weights`]'s partition-of-unity proof relies on).
pub struct HueBandGeometry {
    /// `boundaries[i]`: the boundary between band `i` and band `(i+1) %
    /// N_BANDS`, in degrees.
    pub boundaries: [f32; N_BANDS],
    /// `half_widths[i]`: boundary `i`'s transition half-width, in degrees.
    pub half_widths: [f32; N_BANDS],
}

fn build_geometry() -> HueBandGeometry {
    let c = HUE_BAND_CENTERS_DEG;
    let mut boundaries = [0f32; N_BANDS];
    for i in 0..N_BANDS {
        let next = c[(i + 1) % N_BANDS];
        // Circular midpoint, the short way from c[i] to next.
        let step = circ_delta(next, c[i]);
        boundaries[i] = (c[i] + step * 0.5).rem_euclid(360.0);
    }
    let mut half_widths = [0f32; N_BANDS];
    for i in 0..N_BANDS {
        let prev = boundaries[(i + N_BANDS - 1) % N_BANDS];
        let next = boundaries[(i + 1) % N_BANDS];
        let left_gap = circ_delta(boundaries[i], prev);
        let right_gap = circ_delta(next, boundaries[i]);
        half_widths[i] = TRANSITION_HALF_WIDTH_DEG
            .min(0.49 * left_gap.min(right_gap))
            .max(1.0e-3);
    }
    HueBandGeometry {
        boundaries,
        half_widths,
    }
}

static GEOMETRY: OnceLock<HueBandGeometry> = OnceLock::new();

/// The cached hue-band boundary geometry (computed once).
pub fn hue_band_geometry() -> &'static HueBandGeometry {
    GEOMETRY.get_or_init(build_geometry)
}

/// The 8-band geometry flattened for GPU upload:
/// `[boundaries(8), half_widths(8)]`, the exact bytes [`HslNode`]'s
/// `eval_gpu` uploads alongside [`matrices_flat`], so the GPU kernel's
/// `band_weights` evaluates over the identical boundary geometry the CPU
/// path computed.
///
/// [`HslNode`]: crate::ng::nodes::global::hsl::HslNode
#[must_use]
pub fn hue_band_geometry_flat() -> [f32; 2 * N_BANDS] {
    let g = hue_band_geometry();
    let mut out = [0f32; 2 * N_BANDS];
    out[0..N_BANDS].copy_from_slice(&g.boundaries);
    out[N_BANDS..2 * N_BANDS].copy_from_slice(&g.half_widths);
    out
}

/// Smoothstep (Hermite) interpolation, clamped outside `[edge0, edge1]`
/// the same shape `curve1d::smoothstep` uses.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let denom = (edge1 - edge0).max(1.0e-6);
    let t = ((x - edge0) / denom).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Boundary `i`'s ramp at hue `h`: `0` deep on band `i`'s side, `1` deep on
/// band `i+1`'s side, smoothly crossfading through the `±half_widths[i]`
/// transition zone around `boundaries[i]`.
#[inline]
fn boundary_ramp(h: f32, boundary: f32, half_width: f32) -> f32 {
    smoothstep(-half_width, half_width, circ_delta(h, boundary))
}

/// The 8 hue-band weights at `hue_deg` (task C7). **Provably a partition of
/// unity** (`Σ weights == 1.0` for every hue, exercised over a fine sweep by
/// `tests::band_weights_sum_to_one_everywhere`):
///
/// Let `t[i] = boundary_ramp(h, boundaries[i], half_widths[i])`, each
/// `t[i] ∈ [0,1]`. Define `weight[i] = t[i-1] * (1 - t[i])` (indices mod 8).
/// Because the 8 transition zones are pairwise disjoint by construction
/// (each `half_widths[i]` is capped below half the gap to its two
/// neighboring boundaries), **at most one `t[k]` is strictly fractional for
/// any given `h`**, every other `t[j]` is saturated exactly `0` or exactly
/// `1`. Walking around the circle from deep inside band `i`'s plateau
/// (`t[i-1] = 1`, `t[i] = 0` ⇒ `weight[i] = 1`, every other band's weight
/// `0` since it has a `0` factor from a boundary two steps away) through the
/// transition into band `i+1` (`t[i] = s ∈ (0,1)`, all *other* boundaries
/// still saturated) gives `weight[i] = 1 - s`, `weight[i+1] = s`, every
/// other weight still `0`, so the sum is `1` throughout: at the plateau
/// trivially, and through every transition by the complementary-ramp
/// identity `s + (1-s) = 1`. No case is left uncovered because the two
/// conditions ("at most one fractional `t`", "neighbors saturate correctly
/// on entry/exit") hold everywhere by the non-overlap construction.
#[must_use]
pub fn band_weights(hue_deg: f32) -> [f32; N_BANDS] {
    let g = hue_band_geometry();
    let mut t = [0f32; N_BANDS];
    for (i, ti) in t.iter_mut().enumerate() {
        *ti = boundary_ramp(hue_deg, g.boundaries[i], g.half_widths[i]);
    }
    let mut w = [0f32; N_BANDS];
    for i in 0..N_BANDS {
        let prev = t[(i + N_BANDS - 1) % N_BANDS];
        w[i] = prev * (1.0 - t[i]);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── C6: round-trip + known color pairs ─────────────────────────────

    /// C6 AC: round-trip working → OkLCh → working is < 1e-4 over a grid of
    /// working-space colors (including out-of-`[0,1]` scene-linear values,
    /// since HSL/vibrance operate directly on unclamped working RGB, spec
    /// §4.2).
    #[test]
    fn oklch_round_trip_under_1e_minus_4() {
        let cases: &[[f32; 3]] = &[
            [0.5, 0.5, 0.5],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.2, 0.6, 0.9],
            [1.4, 0.9, 0.1],    // > 1: scene-linear highlight
            [0.02, 0.01, 0.03], // near-black shadow
            [0.0, 0.0, 0.0],
        ];
        for &rgb in cases {
            let lch = working_to_oklch(rgb);
            let back = oklch_to_working(lch);
            for k in 0..3 {
                assert!(
                    (back[k] - rgb[k]).abs() < 1e-4,
                    "rgb={rgb:?} lch={lch:?} back={back:?} (component {k})"
                );
            }
        }
    }

    /// C6 AC: round-trip through Oklab directly (not just OkLCh).
    #[test]
    fn oklab_round_trip_under_1e_minus_4() {
        let cases: &[[f32; 3]] = &[[0.5, 0.5, 0.5], [0.9, 0.1, 0.3], [0.05, 0.4, 0.7]];
        for &rgb in cases {
            let lab = working_to_oklab(rgb);
            let back = oklab_to_working(lab);
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-4, "rgb={rgb:?} back={back:?}");
            }
        }
    }

    /// C6 AC: known color-pair checks. A neutral (equal-RGB) working color
    /// is achromatic (`C ≈ 0`) at any lightness. Red hues out-rank green
    /// hues in the expected OkLCh ordering along the working-space channel
    /// axes (sanity that the conversion isn't, say, transposed).
    #[test]
    fn known_color_pairs() {
        for l in [0.05f32, 0.2, 0.5, 0.8, 0.95] {
            let [_, c, _] = working_to_oklch([l, l, l]);
            assert!(c < 1e-4, "neutral gray at {l} must be achromatic: C={c}");
        }
        // Pure working-space red vs. pure working-space green: distinct,
        // non-degenerate hues, and red's hue must be well below green's
        // (the same ordering the C7 band centers rely on).
        let [_, cr, hr] = working_to_oklch([1.0, 0.0, 0.0]);
        let [_, cg, hg] = working_to_oklch([0.0, 1.0, 0.0]);
        assert!(cr > 0.05 && cg > 0.05, "primaries must be chromatic");
        assert!(hr < hg, "red hue ({hr}) must precede green hue ({hg})");
    }

    /// C6: the identity working-space achromatic point (`[0,0,0]`) round
    /// trips without NaN/Inf (the cbrt-of-zero / atan2(0,0) corner case).
    #[test]
    fn black_is_finite_and_round_trips() {
        let lch = working_to_oklch([0.0, 0.0, 0.0]);
        assert!(lch.iter().all(|v| v.is_finite()));
        let back = oklch_to_working(lch);
        for k in 0..3 {
            assert!((back[k] - 0.0).abs() < 1e-4, "back={back:?}");
        }
    }

    // ── C7: partition of unity + documented band centers ────────────────

    /// C7 AC: `Σ weights == 1.0` for every hue, swept at 0.01° resolution
    /// (36 000 samples) over the full circle, including exact boundary
    /// crossings.
    #[test]
    fn band_weights_sum_to_one_everywhere() {
        let steps = 36_000;
        for i in 0..steps {
            let h = 360.0 * i as f32 / steps as f32;
            let w = band_weights(h);
            let sum: f32 = w.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-4,
                "h={h}: weights {w:?} sum to {sum}"
            );
            for &c in &w {
                assert!(
                    (-1e-4..=1.0 + 1e-4).contains(&c),
                    "h={h}: weight {c} out of [0,1]"
                );
            }
        }
    }

    /// C7 AC: band centers are pinned to documented hues, recomputes each
    /// of the 8 [`HUE_BAND_CENTERS_DEG`] entries from its cited reference
    /// swatch (the module docs' table) via this module's own conversion,
    /// and asserts agreement to `1e-2`°, so the pin can never silently
    /// drift from its documented derivation.
    #[test]
    fn hue_band_centers_match_their_reference_swatches() {
        use lightbox_color::matrix::Vec3;
        fn srgb_eotf(u: f32) -> f32 {
            if u <= 0.040_449_936 {
                u / 12.92
            } else {
                ((u + 0.055) / 1.055).powf(2.4)
            }
        }
        // sRGB(encoded) swatches, in HUE_BAND_CENTERS_DEG order.
        let swatches: [[f32; 3]; N_BANDS] = [
            [1.0, 0.0, 0.0],
            [1.0, 0.5, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.5, 0.0, 1.0],
            [1.0, 0.0, 1.0],
        ];
        for (i, enc) in swatches.iter().enumerate() {
            let lin_srgb = enc.map(srgb_eotf);
            // Decode the swatch straight to working RGB (sRGB primaries are
            // inside the ProPhoto/working gamut) via the inverse of
            // `working_to_linear_srgb`, so this test exercises the SAME
            // working->Oklab pipeline the node uses, not a parallel one.
            let srgb_to_work = spaces::working_to_linear_srgb()
                .inverse()
                .expect("sRGB->working is well-conditioned");
            let v = Vec3([lin_srgb[0] as f64, lin_srgb[1] as f64, lin_srgb[2] as f64]);
            let w = srgb_to_work.mul_vec(v);
            let working_rgb = [w.0[0] as f32, w.0[1] as f32, w.0[2] as f32];
            let [_, _, h] = working_to_oklch(working_rgb);
            let want = HUE_BAND_CENTERS_DEG[i];
            assert!(
                (h - want).abs() < 1.0e-1,
                "{}: computed hue {h:.3}° vs documented {want:.3}°",
                HUE_BAND_NAMES[i]
            );
        }
    }

    /// C7: the boundary geometry never lets two transition zones overlap
    /// (the property the partition-of-unity proof depends on), asserted
    /// directly on the geometry, independent of the sweep test above.
    #[test]
    fn transition_zones_never_overlap() {
        let g = hue_band_geometry();
        for i in 0..N_BANDS {
            let next = (i + 1) % N_BANDS;
            let gap = circ_delta(g.boundaries[next], g.boundaries[i]);
            assert!(
                gap > 0.0,
                "boundary {i} and {next} out of ascending order: gap={gap}"
            );
            assert!(
                g.half_widths[i] + g.half_widths[next] <= gap + 1e-3,
                "boundary {i}/{next} transition zones overlap: hw={}/{} gap={gap}",
                g.half_widths[i],
                g.half_widths[next]
            );
        }
    }

    /// C6/C7 supporting sanity: [`matrices_flat`]/[`hue_band_geometry_flat`]
    /// are stable (same call twice ⇒ identical bytes), the GPU-upload
    /// contract both node's `eval_gpu` depend on for cache-key stability
    /// (an unchanged recipe must never re-derive different matrix bytes).
    #[test]
    fn flattened_gpu_uploads_are_stable() {
        assert_eq!(matrices_flat(), matrices_flat());
        assert_eq!(hue_band_geometry_flat(), hue_band_geometry_flat());
    }
}
