// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! White-balance model + preset table (spec §3.3). **Owner: Phase B (B5).**
//! The **per-camera** neutral↔temp/tint solver lives on
//! [`ColorimetricSolver`](crate::profile::ColorimetricSolver) (it needs the
//! profile's matrices); this module owns the mode/preset vocabulary, the
//! resolved white point, and the preset → `(CCT, tint)` table.
//!
//! **E10 task A10** adds a second, **camera-agnostic** pair of solvers
//! referenced against the fixed working-space matrices
//! ([`crate::matrix::spaces`]) rather than a per-image camera calibration
//! no per-image `CameraProfile` reaches the E05 render graph yet (the M1
//! reality: `SourceImage` pixels arrive already demosaiced and already in
//! working space, see `lightbox-render`'s `ng::colorimetry` module docs),
//! so `WhiteBalanceNode` (task A9) and the A11 eyedropper solve both build on
//! this generic pair instead of duplicating color science:
//!
//! - [`non_raw_wb_matrix`] / [`temp_tint_from_working_neutral`], the
//!   **working-space Bradford-CAT pair** `WhiteBalanceNode`'s only reachable
//!   code path actually uses (spec §4.2 "WB (non-raw)"): every pixel that
//!   reaches the render graph today is already working-space RGB, so this is
//!   the domain E10's node operates in, and the two functions are exact
//!   inverses of one another (a patch's own chromaticity, round-tripped
//!   through [`temp_tint_from_working_neutral`] then [`non_raw_wb_matrix`],
//!   maps that same patch to the working space's own white, see the
//!   `eyedropper_solve_neutralizes_the_sampled_patch` test).
//! - [`gains_from_temp_tint`] / [`temp_tint_from_gains`], the **raw-path
//!   per-channel-gain pair** spec §4.2's "WB (raw path)" describes (channel
//!   gains derived from camera matrices). No raw sensor mosaic reaches the
//!   E05 graph yet (`SourceKind::Raw` is E11-reserved), so this pair has no
//!   live node call site today; it is implemented and property-tested
//!   against the A10 AC (round-trip within 0.5%) so E11's future raw-mosaic
//!   seam can wire it in with no redesign. Recorded as a deviation in
//!   `docs/plan/epics/E10-deviations.md`.

use crate::cct::{cct_tint_to_xy, xy_to_cct_tint};
use crate::matrix::{bradford_adaptation, spaces, xy_to_xyz, xyz_to_xy, Mat3, Vec3, D50_WHITE_XYZ};

/// How white balance is specified (spec §3.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WbMode {
    /// Use the file's as-shot neutral.
    AsShot,
    /// A `(Kelvin, tint)` pair.
    TempTint {
        /// Correlated color temperature (Kelvin).
        kelvin: f64,
        /// Green-magenta tint, orthogonal to CCT.
        tint: f64,
    },
    /// A camera-native neutral triple.
    Neutral([f64; 3]),
}

/// A solved white point: the camera-native neutral and its `(CCT, tint)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WhitePoint {
    /// Camera-native neutral RGB.
    pub neutral: [f64; 3],
    /// Correlated color temperature (Kelvin).
    pub cct: f64,
    /// Tint.
    pub tint: f64,
}

/// The named WB presets E10's preset menu offers (spec §3.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WbPreset {
    /// ~5500 K daylight.
    Daylight,
    /// Overcast.
    Cloudy,
    /// Open shade.
    Shade,
    /// Incandescent/tungsten.
    Tungsten,
    /// Fluorescent.
    Fluorescent,
    /// Electronic flash.
    Flash,
}

/// The preset → `(CCT Kelvin, tint)` table (spec §3.3, Open Question 3).
///
/// Values are the correlated color temperatures of published CIE standard
/// illuminants, **sourced, not invented**:
///
/// | preset | illuminant | CCT (K) |
/// |--------|------------|---------|
/// | Daylight | D55 | 5503 |
/// | Cloudy | D65-class overcast | 6504 |
/// | Shade | D75 | 7504 |
/// | Tungsten | Standard A | 2856 |
/// | Fluorescent | F2 (cool white) | 4230 |
/// | Flash | electronic flash ≈ D55 | 5503 |
///
/// Tint is `0` for every preset: each row is the locus point at the
/// illuminant's CCT. (F2 sits marginally off the Planckian locus toward green;
/// its exact tint needs the F2 spectral power distribution, which is out of
/// Phase B scope, E10 may refine per body. Recorded in `E02-deviations.md`.)
pub fn wb_presets() -> &'static [(WbPreset, f64, f64)] {
    &[
        (WbPreset::Daylight, 5503.0, 0.0),
        (WbPreset::Cloudy, 6504.0, 0.0),
        (WbPreset::Shade, 7504.0, 0.0),
        (WbPreset::Tungsten, 2856.0, 0.0),
        (WbPreset::Fluorescent, 4230.0, 0.0),
        (WbPreset::Flash, 5503.0, 0.0),
    ]
}

/// `a / b`, guarded against a degenerate (near-zero) denominator (returns
/// `0.0` rather than `inf`/`NaN`, inputs here are always finite chromaticity-
/// derived values, but the guard keeps every solver total).
#[inline]
fn safe_div(a: f64, b: f64) -> f64 {
    if b.abs() > 1e-9 {
        a / b
    } else {
        0.0
    }
}

/// `(Kelvin, tint)` → normalized per-channel white-balance **gain** (E10 task
/// A10, spec §4.2 raw-path math): the camera-agnostic reference for
/// `WhiteBalanceNode`'s raw path once a raw-mosaic seam reaches the render
/// graph (module docs). The "sensor" stand-in is the fixed working-space
/// matrix ([`spaces::xyz_d50_to_working`]). Normalized so the green channel's
/// gain is exactly `1.0` (the camera-multiplier convention): a neutral
/// surface rendered under `(kelvin, tint)` maps to `[n_r, n_g, n_b]` in
/// working RGB; the gain that neutralizes it is `[n_g/n_r, 1, n_g/n_b]`.
pub fn gains_from_temp_tint(kelvin: f64, tint: f64) -> [f64; 3] {
    let xy = cct_tint_to_xy(kelvin, tint);
    let xyz = xy_to_xyz(xy);
    let n = spaces::xyz_d50_to_working().mul_vec(Vec3(xyz)).0;
    let g = n[1];
    [safe_div(g, n[0]), 1.0, safe_div(g, n[2])]
}

/// Inverse of [`gains_from_temp_tint`] (E10 task A10): a normalized gain
/// triple (green = `1.0`) → the `(Kelvin, tint)` whose forward solve produces
/// it. Reconstructs the neutral `[1/g_r, 1, 1/g_b]` the gain was solved from
/// (any positive overall scale is fine, chromaticity is scale-invariant) and
/// runs it back through the same working-space matrix + Robertson locus
/// [`xy_to_cct_tint`] uses for the forward direction, so the pair round-trips
/// to the precision of that shared locus model (task A10 AC: within 0.5%).
pub fn temp_tint_from_gains(gains: [f64; 3]) -> (f64, f64) {
    let n = [safe_div(1.0, gains[0]), 1.0, safe_div(1.0, gains[2])];
    let xyz = spaces::working_to_xyz_d50().mul_vec(Vec3(n)).0;
    xy_to_cct_tint(xyz_to_xy(xyz))
}

/// The E10 task A9 non-raw-path matrix: a Bradford chromatic-adaptation
/// transform (own implementation, [`crate::matrix::bradford_adaptation`])
/// applied entirely in the working RGB domain, `working → XYZ(D50) →
/// Bradford-adapt(target white → D50) → working`. At `(kelvin, tint)` equal
/// to the working space's own D50 white point (~5003 K, tint 0) this
/// collapses to the identity matrix (source == destination white); moving
/// away from it shifts color balance toward/away from that illuminant. This
/// is the matrix [`WhiteBalanceNode`]'s only reachable eval path applies
/// (module docs): every pixel reaching the render graph today is already
/// working-space RGB (spec §4.2 "WB (non-raw) | working space, Bradford/CAT02
/// adaptation | display-referred input already in working space").
///
/// [`WhiteBalanceNode`]: https://docs.rs/lightbox-render (nodes::global::white_balance::WhiteBalanceNode)
pub fn non_raw_wb_matrix(kelvin: f64, tint: f64) -> Mat3 {
    let xy = cct_tint_to_xy(kelvin, tint);
    let target_xyz = xy_to_xyz(xy);
    let cat = bradford_adaptation(target_xyz, D50_WHITE_XYZ);
    spaces::xyz_d50_to_working()
        .mul(&cat)
        .mul(&spaces::working_to_xyz_d50())
}

/// The E10 task A11 eyedropper solve: given a **sampled patch** (a picked
/// point's working-RGB value, assumed to be what a neutral/gray surface
/// currently renders as) return the `(Kelvin, tint)` that would neutralize
/// it, i.e., the white-balance setting under which that patch becomes
/// achromatic. The patch is treated directly as the locus-neutral point and
/// converted through XYZ(D50) → `(CCT, tint)`; it is the exact inverse of
/// [`non_raw_wb_matrix`] for this purpose (module docs / the
/// `eyedropper_solve_neutralizes_the_sampled_patch` test): choosing
/// `(kelvin, tint) = temp_tint_from_working_neutral(patch)` makes
/// `non_raw_wb_matrix(kelvin, tint)` map `patch`'s own chromaticity ray
/// exactly onto the working space's white axis.
pub fn temp_tint_from_working_neutral(rgb: [f64; 3]) -> (f64, f64) {
    let xyz = spaces::working_to_xyz_d50().mul_vec(Vec3(rgb)).0;
    xy_to_cct_tint(xyz_to_xy(xyz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_cover_all_six_with_sourced_ccts() {
        let p = wb_presets();
        assert_eq!(p.len(), 6);
        let find = |k: WbPreset| {
            p.iter()
                .find(|(pk, _, _)| *pk == k)
                .map(|(_, c, t)| (*c, *t))
        };
        assert_eq!(find(WbPreset::Tungsten), Some((2856.0, 0.0)));
        assert_eq!(find(WbPreset::Daylight), Some((5503.0, 0.0)));
        assert_eq!(find(WbPreset::Shade), Some((7504.0, 0.0)));
        assert_eq!(find(WbPreset::Fluorescent), Some((4230.0, 0.0)));
        // Every preset CCT is a plausible lighting temperature.
        for (_, cct, _) in p {
            assert!((2000.0..=9000.0).contains(cct));
        }
    }

    /// A10 AC: `gains → (temp,tint) → gains` round-trips within 0.5%
    /// relative error, over the corpus operating band.
    #[test]
    fn gains_round_trip_within_half_a_percent() {
        for &kelvin in &[2856.0, 3500.0, 4230.0, 5503.0, 6504.0, 7504.0, 9000.0] {
            for &tint in &[-30.0, -10.0, 0.0, 10.0, 30.0] {
                let gains1 = gains_from_temp_tint(kelvin, tint);
                let (k2, t2) = temp_tint_from_gains(gains1);
                let gains2 = gains_from_temp_tint(k2, t2);
                for ch in 0..3 {
                    let rel = (gains2[ch] - gains1[ch]).abs() / gains1[ch].abs().max(1e-9);
                    assert!(
                        rel < 0.005,
                        "kelvin={kelvin} tint={tint} ch={ch}: gains1={gains1:?} gains2={gains2:?} rel={rel}"
                    );
                }
            }
        }
    }

    /// The working space's own D50 white, expressed in `(CCT, tint)` terms
    /// via the SAME Robertson-locus round trip [`non_raw_wb_matrix`] and
    /// [`gains_from_temp_tint`] use, the self-consistent "identity point"
    /// for both solvers (not a hardcoded literal, since `xy_to_cct_tint`'s
    /// own published-illuminant recovery is only exact to ~15 K / a few %,
    /// per `cct::tests::standard_illuminants_recovered_within_15k`).
    fn working_white_temp_tint() -> (f64, f64) {
        xy_to_cct_tint(xyz_to_xy(D50_WHITE_XYZ))
    }

    /// `gains_from_temp_tint` returns near-unity gain at the working space's
    /// own white, no correction needed there.
    #[test]
    fn gains_at_working_white_are_near_unity() {
        let (kelvin, tint) = working_white_temp_tint();
        let g = gains_from_temp_tint(kelvin, tint);
        for (i, v) in g.iter().enumerate() {
            assert!((v - 1.0).abs() < 0.01, "gains[{i}]={v}");
        }
    }

    /// A9 non-raw path: at the working space's own white the Bradford
    /// matrix collapses to the identity (source white == destination white)
    /// the exact "as-shot renders byte-identical" anchor `WhiteBalanceNode`
    /// relies on via its `is_identity` elision (a **structural** elision
    /// the node is never added to the graph for `AsShot`/`Auto`, this test
    /// just confirms the underlying math agrees with that elision at the
    /// numeric level too).
    #[test]
    fn non_raw_matrix_is_identity_at_working_white() {
        let (kelvin, tint) = working_white_temp_tint();
        let m = non_raw_wb_matrix(kelvin, tint);
        for i in 0..3 {
            for j in 0..3 {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((m.0[i][j] - want).abs() < 0.01, "m[{i}][{j}]={}", m.0[i][j]);
            }
        }
    }

    /// A9: raising kelvin above the working white renders warmer (matches
    /// the Lightroom convention, a higher Temp slider value warms the
    /// image), lowering it renders cooler. Checked on a mid-gray patch by
    /// comparing the sign of the red-minus-blue channel after applying the
    /// matrix.
    #[test]
    fn higher_kelvin_warms_a_neutral_patch() {
        let gray = [0.5f64, 0.5, 0.5];
        let warm_setting = non_raw_wb_matrix(8000.0, 0.0).mul_vec(Vec3(gray)).0;
        let cool_setting = non_raw_wb_matrix(3000.0, 0.0).mul_vec(Vec3(gray)).0;
        assert!(
            (warm_setting[0] - warm_setting[2]) > (cool_setting[0] - cool_setting[2]),
            "warm_setting(R-B)={} should exceed cool_setting(R-B)={}",
            warm_setting[0] - warm_setting[2],
            cool_setting[0] - cool_setting[2]
        );
    }

    /// A11: `temp_tint_from_working_neutral` is the exact inverse of
    /// `non_raw_wb_matrix` for this purpose, solving from a sampled patch
    /// and applying the resulting matrix back to that SAME patch must
    /// neutralize it (equal R=G=B), the property the eyedropper AC
    /// ("clicking a shot gray card → neutrality") rests on.
    #[test]
    fn eyedropper_solve_neutralizes_the_sampled_patch() {
        for patch in [
            [0.9, 1.0, 1.3], // shot under a cool/blue cast
            [1.2, 1.0, 0.6], // shot under a warm/tungsten cast
            [1.0, 1.0, 1.0], // already neutral
            [0.3, 0.32, 0.28],
        ] {
            let (kelvin, tint) = temp_tint_from_working_neutral(patch);
            let m = non_raw_wb_matrix(kelvin, tint);
            let out = m.mul_vec(Vec3(patch)).0;
            let mean = (out[0] + out[1] + out[2]) / 3.0;
            for (i, v) in out.iter().enumerate() {
                let rel = (v - mean).abs() / mean.max(1e-9);
                assert!(
                    rel < 0.01,
                    "patch={patch:?} solved=({kelvin},{tint}) out[{i}]={v} mean={mean}"
                );
            }
        }
    }
}
