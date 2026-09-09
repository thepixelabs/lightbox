// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Camera profiles: the tier-1 matrix base ([`camera_matrix_base`], **Phase B
//! B7**), the per-profile colorimetric solver ([`ColorimetricSolver`], **B3/B4/
//! B5**), and profile-reference resolution ([`resolve_profile_ref`], **Phase F
//! F7**). The [`CameraProfile`] type is the shared tier-1/2/3 container (spec
//! §3.4), DCP parsing ([`crate::dcp::parse_dcp`]) produces one, the resolver
//! consumes one.
//!
//! **Cross-phase note (recorded in `E02-deviations.md`):** the scaffold owner
//! map lists `profile` under Phase F, but [`camera_matrix_base`] and
//! [`ColorimetricSolver`] are B7 / B3-B5 tasks and are tagged **Phase B** in
//! their own doc comments. Phase B fills only those B-tagged bodies here;
//! [`resolve_profile_ref`] (F7) and the DCP container ([`crate::dcp`]) stay for
//! Phase F, so the two phases still touch disjoint function bodies.

use lightbox_decode::{CameraId, Illuminant, RawColorimetry};

use crate::cct::{cct_tint_to_xy, xy_to_cct_tint};
use crate::error::ColorError;
use crate::lut::HueSatLut;
use crate::matrix::{
    bradford_adaptation, xy_to_xyz, xyz_to_xy, Mat3, Spline1D, Vec3, D50_WHITE_XYZ,
};
use crate::transform::{FallbackReason, ProfileKind, ProfileRef, ProfileRegistry, ResolvedProfile};
use crate::wb::{WbMode, WhitePoint};

/// Content id of a profile/look: xxh3-128 of its canonical serialization
/// (spec §3.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ProfileId(pub [u8; 16]);

/// Where a profile came from (spec §3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileSource {
    /// The license-clean matrix base (§1.7 tier 1).
    MatrixBase,
    /// A curated in-house DCP (tier 3).
    CuratedDcp,
    /// A user-installed DCP.
    UserDcp,
}

/// The DNG `DefaultBlackRender` policy (spec §3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefaultBlackRender {
    /// Auto black render.
    Auto,
    /// No black render.
    None,
}

/// The dual-illuminant calibration block: illuminants + CM1/2, FM1/2, analog
/// balance (spec §3.4).
#[derive(Clone, Debug)]
pub struct DualIlluminant {
    /// First calibration illuminant.
    pub illuminant1: Illuminant,
    /// Second calibration illuminant.
    pub illuminant2: Option<Illuminant>,
    /// `ColorMatrix1` (XYZ→camera).
    pub color_matrix1: Mat3,
    /// `ColorMatrix2`.
    pub color_matrix2: Option<Mat3>,
    /// `ForwardMatrix1` (camera→XYZ(D50)).
    pub forward_matrix1: Option<Mat3>,
    /// `ForwardMatrix2`.
    pub forward_matrix2: Option<Mat3>,
    /// `AnalogBalance` diagonal.
    pub analog_balance: Option<[f64; 3]>,
}

/// A camera color profile, tier 1 (matrix base), tier 2 wiring, or tier 3
/// (DCP) (spec §3.4).
#[derive(Clone, Debug)]
pub struct CameraProfile {
    /// Content id.
    pub id: ProfileId,
    /// Display name.
    pub name: String,
    /// Provenance.
    pub source: ProfileSource,
    /// Dual-illuminant calibration.
    pub calibration: DualIlluminant,
    /// `ProfileHueSatMap` (+ encoding).
    pub hue_sat_map: Option<HueSatLut>,
    /// `ProfileLookTable`.
    pub look_table: Option<HueSatLut>,
    /// `ProfileToneCurve` control points.
    pub tone_curve: Option<Spline1D>,
    /// `BaselineExposureOffset` (stops).
    pub baseline_exposure_offset: f32,
    /// `DefaultBlackRender`.
    pub default_black_render: DefaultBlackRender,
    /// `ProfileCopyright`, surfaced; Adobe-authored content is banned upstream
    /// (surface-3 manifest check, spec §4.3).
    pub copyright: Option<String>,
}

/// The correlated color temperature (Kelvin) of a DNG calibration illuminant,
/// used to weight the dual-illuminant interpolation by reciprocal temperature.
fn illuminant_cct(illum: Illuminant) -> f64 {
    match illum {
        Illuminant::StandardA => 2856.0,
        Illuminant::D50 => 5003.0,
        Illuminant::D55 => 5503.0,
        Illuminant::D65 => 6504.0,
        Illuminant::D75 => 7504.0,
        Illuminant::Daylight(k) => k as f64,
        // Unknown/unmodeled: assume a daylight anchor so single-illuminant
        // profiles interpolate to themselves and never divide by a bad temp.
        Illuminant::Other(_) | Illuminant::Unknown => 6504.0,
    }
}

/// Builds the §1.7 tier-1 colorimetric base from file metadata alone
/// (spec §3.3). **Phase B (B7)**, `renders_correct_color_with_zero_bundled_
/// profiles` is the executable proof (the first failing test). Takes **no**
/// filesystem/profile assets: the calibration comes entirely from the decoded
/// [`RawColorimetry`], which is why tier 1 renders correctly with the bundled
/// profile tree absent.
pub fn camera_matrix_base(
    c: &RawColorimetry,
    camera: &CameraId,
) -> Result<CameraProfile, ColorError> {
    let color_matrix1 = Mat3::from(c.color_matrix1);
    // The primary calibration matrix must be invertible, the no-forward-matrix
    // colorimetric path depends on it.
    color_matrix1.inverse()?;

    let calibration = DualIlluminant {
        illuminant1: c.illuminant1,
        illuminant2: c.illuminant2,
        color_matrix1,
        color_matrix2: c.color_matrix2.map(Mat3::from),
        forward_matrix1: c.forward_matrix1.map(Mat3::from),
        forward_matrix2: c.forward_matrix2.map(Mat3::from),
        analog_balance: c.analog_balance,
    };

    let name = if camera.make.is_empty() && camera.model.is_empty() {
        "Matrix Base".to_owned()
    } else {
        format!("{} {} — Matrix Base", camera.make, camera.model)
    };

    let id = matrix_base_id(&name, &calibration, c.baseline_exposure);

    Ok(CameraProfile {
        id,
        name,
        source: ProfileSource::MatrixBase,
        calibration,
        hue_sat_map: None,
        look_table: None,
        tone_curve: None,
        baseline_exposure_offset: c.baseline_exposure,
        default_black_render: DefaultBlackRender::Auto,
        copyright: None,
    })
}

/// Deterministic content id for a matrix-base profile: xxh3-128 over its
/// canonical calibration serialization (spec §3.4, `ProfileId`).
fn matrix_base_id(name: &str, cal: &DualIlluminant, baseline: f32) -> ProfileId {
    let mut bytes: Vec<u8> = Vec::with_capacity(256);
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    let push_mat = |b: &mut Vec<u8>, m: &Mat3| {
        for row in &m.0 {
            for v in row {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
    };
    let push_opt_mat = |b: &mut Vec<u8>, m: &Option<Mat3>| {
        b.push(m.is_some() as u8);
        if let Some(m) = m {
            push_mat(b, m);
        }
    };
    bytes.extend_from_slice(&illuminant_cct(cal.illuminant1).to_le_bytes());
    bytes.extend_from_slice(
        &illuminant_cct(cal.illuminant2.unwrap_or(Illuminant::Unknown)).to_le_bytes(),
    );
    push_mat(&mut bytes, &cal.color_matrix1);
    push_opt_mat(&mut bytes, &cal.color_matrix2);
    push_opt_mat(&mut bytes, &cal.forward_matrix1);
    push_opt_mat(&mut bytes, &cal.forward_matrix2);
    let ab = cal.analog_balance.unwrap_or([1.0, 1.0, 1.0]);
    for v in ab {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&baseline.to_le_bytes());

    let h = twox_hash::XxHash3_128::oneshot(&bytes);
    ProfileId(h.to_le_bytes())
}

/// Per-profile colorimetric solver (DNG spec ch. 6 semantics, spec §3.3).
/// **Owner: Phase B (B3/B4/B5).** Precomputes the interpolation anchors so
/// white-point iteration and the camera→XYZ matrix build are allocation-free.
pub struct ColorimetricSolver<'p> {
    /// The profile this solver is bound to.
    pub profile: &'p CameraProfile,
    /// `ColorMatrix1` and its illuminant CCT.
    cm1: Mat3,
    t1: f64,
    /// `ColorMatrix2` + its CCT, when the profile is dual-illuminant.
    cm2: Option<Mat3>,
    t2: Option<f64>,
    fm1: Option<Mat3>,
    fm2: Option<Mat3>,
    /// Analog-balance diagonal (default identity).
    ab: [f64; 3],
}

/// Reciprocal-temperature blend of two matrices anchored at `t_lo`/`t_hi`,
/// clamped to the endpoints outside `[t_lo, t_hi]`.
fn interp_by_temp(m1: Mat3, t1: f64, m2: Mat3, t2: f64, temp: f64) -> Mat3 {
    let (t_lo, m_lo, t_hi, m_hi) = if t1 <= t2 {
        (t1, m1, t2, m2)
    } else {
        (t2, m2, t1, m1)
    };
    if temp <= t_lo {
        return m_lo;
    }
    if temp >= t_hi {
        return m_hi;
    }
    let inv = 1.0 / temp;
    let inv_lo = 1.0 / t_lo;
    let inv_hi = 1.0 / t_hi;
    // g = 1 at t_lo, 0 at t_hi.
    let g = (inv - inv_hi) / (inv_lo - inv_hi);
    m_lo.scale(g).add(&m_hi.scale(1.0 - g))
}

impl<'p> ColorimetricSolver<'p> {
    /// Binds a solver to a profile, precomputing the interpolation anchors (B3).
    pub fn new(profile: &'p CameraProfile) -> Result<Self, ColorError> {
        let cal = &profile.calibration;
        // Guard: CM1 must be invertible for the no-forward-matrix path.
        cal.color_matrix1.inverse()?;
        Ok(ColorimetricSolver {
            profile,
            cm1: cal.color_matrix1,
            t1: illuminant_cct(cal.illuminant1),
            cm2: cal.color_matrix2,
            t2: cal.illuminant2.map(illuminant_cct),
            fm1: cal.forward_matrix1,
            fm2: cal.forward_matrix2,
            ab: cal.analog_balance.unwrap_or([1.0, 1.0, 1.0]),
        })
    }

    /// Interpolated `ColorMatrix` (XYZ→camera) at temperature `temp`, with the
    /// analog balance folded in (`AB · CM`).
    fn color_matrix_at(&self, temp: f64) -> Mat3 {
        let cm = match (self.cm2, self.t2) {
            (Some(cm2), Some(t2)) => interp_by_temp(self.cm1, self.t1, cm2, t2, temp),
            _ => self.cm1,
        };
        Mat3::from_diag(self.ab).mul(&cm)
    }

    /// Interpolated `ForwardMatrix` (camera→XYZ(D50)) at `temp`, when present.
    fn forward_matrix_at(&self, temp: f64) -> Option<Mat3> {
        match (self.fm1, self.fm2, self.t2) {
            (Some(f1), Some(f2), Some(t2)) => Some(interp_by_temp(f1, self.t1, f2, t2, temp)),
            (Some(f1), _, _) => Some(f1),
            (None, Some(f2), _) => Some(f2),
            (None, None, _) => None,
        }
    }

    /// Self-consistent iteration from a camera-native neutral to its white-point
    /// chromaticity (DNG `NeutralToXY`). Non-convergence → structured error.
    fn neutral_to_xy(&self, neutral: [f64; 3]) -> Result<[f64; 2], ColorError> {
        const MAX_ITERS: u32 = 32;
        const TOL: f64 = 1e-8;
        // Start on the D50 locus; any reasonable seed converges in a few steps.
        let mut xy = xyz_to_xy(D50_WHITE_XYZ);
        for i in 0..MAX_ITERS {
            let (cct, _) = xy_to_cct_tint(xy);
            let cm_eff = self.color_matrix_at(cct);
            let cm_inv = cm_eff.inverse()?;
            let xyz = cm_inv.mul_vec(Vec3(neutral)).0;
            let new_xy = xyz_to_xy(xyz);
            let d = (new_xy[0] - xy[0]).abs() + (new_xy[1] - xy[1]).abs();
            xy = new_xy;
            if d < TOL {
                return Ok(xy);
            }
            if i + 1 == MAX_ITERS {
                return Err(ColorError::NonConvergent {
                    iterations: MAX_ITERS,
                });
            }
        }
        Ok(xy)
    }

    /// White-point self-consistent iteration for `AsShot`/`Neutral`; direct for
    /// `TempTint` (spec §3.3 B3). Non-convergence → [`ColorError::NonConvergent`].
    pub fn white_point(
        &self,
        wb: &WbMode,
        as_shot: Option<[f64; 3]>,
    ) -> Result<WhitePoint, ColorError> {
        match wb {
            WbMode::TempTint { kelvin, tint } => {
                let neutral = self.neutral_from_temp_tint(*kelvin, *tint);
                Ok(WhitePoint {
                    neutral,
                    cct: *kelvin,
                    tint: *tint,
                })
            }
            WbMode::Neutral(n) => {
                let xy = self.neutral_to_xy(*n)?;
                let (cct, tint) = xy_to_cct_tint(xy);
                Ok(WhitePoint {
                    neutral: *n,
                    cct,
                    tint,
                })
            }
            WbMode::AsShot => {
                let n = as_shot.ok_or_else(|| {
                    ColorError::InvalidInput("AsShot white balance needs an as-shot neutral".into())
                })?;
                let xy = self.neutral_to_xy(n)?;
                let (cct, tint) = xy_to_cct_tint(xy);
                Ok(WhitePoint {
                    neutral: n,
                    cct,
                    tint,
                })
            }
        }
    }

    /// `(Kelvin, tint)` → camera-native neutral (spec §3.3 B5). Normalized so the
    /// green channel is `1` (the canonical camera-multiplier form).
    pub fn neutral_from_temp_tint(&self, kelvin: f64, tint: f64) -> [f64; 3] {
        let xy = cct_tint_to_xy(kelvin, tint);
        let xyz = xy_to_xyz(xy);
        let n = self.color_matrix_at(kelvin).mul_vec(Vec3(xyz)).0;
        let g = if n[1].abs() > f64::MIN_POSITIVE {
            n[1]
        } else {
            1.0
        };
        [n[0] / g, n[1] / g, n[2] / g]
    }

    /// Camera-native neutral → `(Kelvin, tint)` (spec §3.3 B5, the eyedropper
    /// solve of §5.4). Falls back to the D50 locus if the neutral is degenerate.
    pub fn temp_tint_from_neutral(&self, neutral: [f64; 3]) -> (f64, f64) {
        match self.neutral_to_xy(neutral) {
            Ok(xy) => xy_to_cct_tint(xy),
            Err(_) => xy_to_cct_tint(xyz_to_xy(D50_WHITE_XYZ)),
        }
    }

    /// Interpolated (by inverse CCT) camera→XYZ(D50): ForwardMatrix path when
    /// present, else inverse-ColorMatrix + Bradford to D50 (spec §3.3 B4). The
    /// white point is baked in, the as-shot neutral maps to D50 white, so this
    /// single matrix carries white balance.
    pub fn cam_to_xyz_d50(&self, wp: &WhitePoint) -> Mat3 {
        let temp = wp.cct;
        let xy = cct_tint_to_xy(temp, wp.tint);
        let white_xyz = xy_to_xyz(xy);
        let cm_eff = self.color_matrix_at(temp);
        // Camera coordinates of the white point.
        let camera_white = cm_eff.mul_vec(Vec3(white_xyz)).0;

        match self.forward_matrix_at(temp) {
            Some(fm) => {
                // FM maps reference-white-balanced camera values to XYZ(D50);
                // diag(1/cameraWhite) performs that reference white balance.
                let d = Mat3::from_diag([
                    reciprocal(camera_white[0]),
                    reciprocal(camera_white[1]),
                    reciprocal(camera_white[2]),
                ]);
                fm.mul(&d)
            }
            None => {
                // inv(CM) lands in XYZ adapted to the white point; Bradford-adapt
                // that to the D50 PCS.
                let cm_inv = cm_eff.inverse().unwrap_or(Mat3::IDENTITY);
                let cat = bradford_adaptation(white_xyz, D50_WHITE_XYZ);
                cat.mul(&cm_inv)
            }
        }
    }
}

/// `1/x` with a guard against a zero camera-white channel.
fn reciprocal(x: f64) -> f64 {
    if x.abs() > f64::MIN_POSITIVE {
        1.0 / x
    } else {
        0.0
    }
}

/// Lowercase hex of a [`ProfileId`] for user-visible fallback messages.
fn profile_id_hex(id: &ProfileId) -> String {
    let mut s = String::with_capacity(32);
    for b in id.0 {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Resolves a recipe `base_profile` reference with the Risk-10 fallback chain:
/// requested → (missing) → matrix base + default look, with a user-visible flag
/// (spec §3.4). **Owner: Phase F (F7).**
///
/// The camera profile resolves per [`ProfileRef::kind`]: `MatrixBase` builds the
/// tier-1 base from `colorimetry`; a curated/user DCP is looked up by `id` (or,
/// when no id is pinned, the camera's curated default). A missing profile is
/// **not** an error, it falls back to the matrix base and records a
/// [`FallbackReason::ProfileMissing`] the UI surfaces (R10). The look is resolved
/// independently via [`ProfileRef::look_ref`]; a missing look degrades to no look
/// with [`FallbackReason::LookMissing`] (reported only when no profile fallback
/// already applies, the struct carries a single, most-significant reason).
pub fn resolve_profile_ref(
    reg: &dyn ProfileRegistry,
    colorimetry: &RawColorimetry,
    camera: &CameraId,
    r: &ProfileRef,
) -> Result<ResolvedProfile, ColorError> {
    let mut fallback: Option<FallbackReason> = None;

    let profile = match r.kind {
        ProfileKind::MatrixBase => camera_matrix_base(colorimetry, camera)?,
        ProfileKind::CuratedDcp | ProfileKind::UserDcp => {
            let found = match &r.id {
                Some(id) => reg.profile(id),
                None => reg.default_for_camera(camera),
            };
            match found {
                Some(p) => p,
                None => {
                    let what = match &r.id {
                        Some(id) => profile_id_hex(id),
                        None => format!("{} {}", camera.make, camera.model),
                    };
                    fallback = Some(FallbackReason::ProfileMissing(what));
                    // Risk-10: fall back to the license-clean matrix base so the
                    // image still renders correct color.
                    camera_matrix_base(colorimetry, camera)?
                }
            }
        }
    };

    let look = match &r.look_ref {
        Some(look_id) => match reg.look(look_id) {
            Some(l) => Some(l),
            None => {
                if fallback.is_none() {
                    fallback = Some(FallbackReason::LookMissing(profile_id_hex(look_id)));
                }
                None
            }
        },
        None => None,
    };

    Ok(ResolvedProfile {
        profile,
        look,
        fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{
        bradford_adaptation, rgb_to_xyz_matrix, xyz_to_xy, D50_WHITE_XY, D50_WHITE_XYZ,
        D65_WHITE_XY, D65_WHITE_XYZ, SRGB_PRIMARIES,
    };
    use lightbox_decode::normalize_camera;

    fn mat_arr(m: Mat3) -> [[f64; 3]; 3] {
        m.0
    }

    /// A synthetic self-consistent camera whose native space is linear sRGB
    /// (D65). ColorMatrix / ForwardMatrix are exact inverses so the colorimetric
    /// chain round-trips; the as-shot neutral is the camera-RGB of D65 white.
    fn srgb_camera(with_forward: bool) -> RawColorimetry {
        let srgb_to_xyz_d65 = rgb_to_xyz_matrix(SRGB_PRIMARIES, D65_WHITE_XY);
        let cm1 = srgb_to_xyz_d65.inverse().unwrap(); // XYZ(D65) → camera
        let fm1 = bradford_adaptation(D65_WHITE_XYZ, D50_WHITE_XYZ).mul(&srgb_to_xyz_d65);
        let n = cm1.mul_vec(Vec3(D65_WHITE_XYZ)).0;
        let g = n[1];
        RawColorimetry {
            as_shot_neutral: Some([n[0] / g, n[1] / g, n[2] / g]),
            illuminant1: Illuminant::D65,
            illuminant2: None,
            color_matrix1: mat_arr(cm1),
            color_matrix2: None,
            forward_matrix1: with_forward.then_some(mat_arr(fm1)),
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        }
    }

    #[test]
    fn matrix_base_carries_zero_bundled_content() {
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let profile = camera_matrix_base(&srgb_camera(true), &cam).unwrap();
        assert_eq!(profile.source, ProfileSource::MatrixBase);
        // Tier 1 needs no bundled profile assets: no maps, tables, or tone curve.
        assert!(profile.hue_sat_map.is_none());
        assert!(profile.look_table.is_none());
        assert!(profile.tone_curve.is_none());
        assert!(profile.copyright.is_none());
    }

    #[test]
    fn matrix_base_rejects_singular_color_matrix() {
        let mut c = srgb_camera(true);
        c.color_matrix1 = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [3.0, 6.0, 9.0]];
        let cam = normalize_camera("x", "y");
        assert!(matches!(
            camera_matrix_base(&c, &cam),
            Err(ColorError::SingularMatrix(_))
        ));
    }

    #[test]
    fn as_shot_white_point_recovers_d65() {
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let raw = srgb_camera(true);
        let profile = camera_matrix_base(&raw, &cam).unwrap();
        let solver = ColorimetricSolver::new(&profile).unwrap();
        let wp = solver
            .white_point(&WbMode::AsShot, raw.as_shot_neutral)
            .unwrap();
        assert!(
            (wp.cct - 6504.0).abs() < 60.0,
            "as-shot CCT {} not near D65",
            wp.cct
        );
        // D65 sits just off the Planckian locus, so a faithful solve gives a
        // small (~10) tint, not exactly zero, see the cct module tests.
        assert!(wp.tint.abs() < 12.0, "as-shot tint {}", wp.tint);
    }

    #[test]
    fn neutral_temp_tint_round_trips() {
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let profile = camera_matrix_base(&srgb_camera(true), &cam).unwrap();
        let solver = ColorimetricSolver::new(&profile).unwrap();
        for &cct in &[3000.0, 4500.0, 5500.0, 6500.0, 8000.0] {
            for &tint in &[-20.0, 0.0, 20.0] {
                let n = solver.neutral_from_temp_tint(cct, tint);
                let (c2, t2) = solver.temp_tint_from_neutral(n);
                assert!(
                    (c2 - cct).abs() / cct < 0.03,
                    "CCT {cct}->{c2} (tint {tint})"
                );
                assert!((t2 - tint).abs() < 2.0, "tint {tint}->{t2} (cct {cct})");
            }
        }
    }

    #[test]
    fn forward_matrix_path_maps_neutral_to_d50() {
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let raw = srgb_camera(true);
        let profile = camera_matrix_base(&raw, &cam).unwrap();
        let solver = ColorimetricSolver::new(&profile).unwrap();
        let wp = solver
            .white_point(&WbMode::AsShot, raw.as_shot_neutral)
            .unwrap();
        let m = solver.cam_to_xyz_d50(&wp);
        let xyz = m.mul_vec(Vec3(raw.as_shot_neutral.unwrap())).0;
        let xy = xyz_to_xy(xyz);
        assert!(
            (xy[0] - D50_WHITE_XY[0]).abs() < 1e-3 && (xy[1] - D50_WHITE_XY[1]).abs() < 1e-3,
            "FM path white {xy:?} not D50"
        );
    }

    #[test]
    fn inverse_cm_path_maps_neutral_to_d50() {
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let raw = srgb_camera(false); // no forward matrix ⇒ inverse-CM + Bradford
        let profile = camera_matrix_base(&raw, &cam).unwrap();
        let solver = ColorimetricSolver::new(&profile).unwrap();
        let wp = solver
            .white_point(&WbMode::AsShot, raw.as_shot_neutral)
            .unwrap();
        let m = solver.cam_to_xyz_d50(&wp);
        let xyz = m.mul_vec(Vec3(raw.as_shot_neutral.unwrap())).0;
        let xy = xyz_to_xy(xyz);
        assert!(
            (xy[0] - D50_WHITE_XY[0]).abs() < 1e-3 && (xy[1] - D50_WHITE_XY[1]).abs() < 1e-3,
            "no-FM path white {xy:?} not D50"
        );
    }

    #[test]
    fn dual_illuminant_interpolation_is_continuous_and_converges() {
        // Two illuminants: StdA and D65 with distinct (but invertible) matrices.
        let srgb_to_xyz_d65 = rgb_to_xyz_matrix(SRGB_PRIMARIES, D65_WHITE_XY);
        let cm_d65 = srgb_to_xyz_d65.inverse().unwrap();
        // A perturbed matrix for the StdA anchor.
        let warm = Mat3([[1.05, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.9]]);
        let cm_a = warm.mul(&cm_d65);
        let raw = RawColorimetry {
            as_shot_neutral: None,
            illuminant1: Illuminant::StandardA,
            illuminant2: Some(Illuminant::D65),
            color_matrix1: mat_arr(cm_a),
            color_matrix2: Some(mat_arr(cm_d65)),
            forward_matrix1: None,
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        };
        let cam = normalize_camera("Synthetic", "Dual");
        let profile = camera_matrix_base(&raw, &cam).unwrap();
        let solver = ColorimetricSolver::new(&profile).unwrap();
        // A temp/tint white point in the interpolation band resolves and its
        // neutral round-trips through the (interpolated) matrices → D50.
        for &k in &[3000.0, 4000.0, 5000.0, 6000.0] {
            let wp = solver.white_point(
                &WbMode::TempTint {
                    kelvin: k,
                    tint: 0.0,
                },
                None,
            );
            let wp = wp.expect("dual-illuminant white point converges");
            let m = solver.cam_to_xyz_d50(&wp);
            let xyz = m.mul_vec(Vec3(wp.neutral)).0;
            let xy = xyz_to_xy(xyz);
            assert!(
                (xy[0] - D50_WHITE_XY[0]).abs() < 2e-3 && (xy[1] - D50_WHITE_XY[1]).abs() < 2e-3,
                "dual-illum {k}K neutral {xy:?} not D50"
            );
        }
    }

    // ------------------------- F7: resolve_profile_ref -------------------------

    use crate::look::{Look, LookProvenance};
    use crate::matrix::Spline1D;
    use crate::transform::{FallbackReason, ProfileKind, ProfileRef, ProfileRegistry};

    /// An in-memory registry for the F7 fallback-chain tests.
    #[derive(Default)]
    struct MockRegistry {
        profiles: Vec<CameraProfile>,
        looks: Vec<Look>,
        camera_default: Option<CameraProfile>,
    }

    impl ProfileRegistry for MockRegistry {
        fn profile(&self, id: &ProfileId) -> Option<CameraProfile> {
            self.profiles.iter().find(|p| p.id == *id).cloned()
        }
        fn look(&self, id: &ProfileId) -> Option<Look> {
            self.looks.iter().find(|l| l.id == *id).cloned()
        }
        fn default_for_camera(&self, _camera: &lightbox_decode::CameraId) -> Option<CameraProfile> {
            self.camera_default.clone()
        }
    }

    fn dcp_like_profile(id: ProfileId) -> CameraProfile {
        let mut p = camera_matrix_base(
            &srgb_camera(true),
            &normalize_camera("Synthetic", "sRGB-Cam"),
        )
        .unwrap();
        p.id = id;
        p.source = ProfileSource::CuratedDcp;
        p.name = "Curated DCP".into();
        p
    }

    fn make_look(id: ProfileId) -> Look {
        Look {
            id,
            name: "Look".into(),
            version: 1,
            tone_curve: Spline1D::identity(),
            hue_sat: None,
            provenance: LookProvenance {
                author: "lightbox-authored".into(),
                license: "project".into(),
                review_record: None,
            },
        }
    }

    fn matrix_ref() -> ProfileRef {
        ProfileRef {
            kind: ProfileKind::MatrixBase,
            id: None,
            look_ref: None,
            look_amount: 1.0,
        }
    }

    #[test]
    fn resolve_matrix_base_has_no_fallback() {
        let reg = MockRegistry::default();
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &matrix_ref()).unwrap();
        assert_eq!(out.profile.source, ProfileSource::MatrixBase);
        assert!(out.look.is_none());
        assert!(out.fallback.is_none());
    }

    #[test]
    fn resolve_installed_curated_dcp_by_id() {
        let id = ProfileId([7u8; 16]);
        let reg = MockRegistry {
            profiles: vec![dcp_like_profile(id)],
            ..Default::default()
        };
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let r = ProfileRef {
            kind: ProfileKind::CuratedDcp,
            id: Some(id),
            look_ref: None,
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        assert_eq!(out.profile.source, ProfileSource::CuratedDcp);
        assert_eq!(out.profile.id, id);
        assert!(out.fallback.is_none());
    }

    #[test]
    fn resolve_missing_dcp_falls_back_to_matrix_base() {
        let reg = MockRegistry::default();
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let r = ProfileRef {
            kind: ProfileKind::UserDcp,
            id: Some(ProfileId([0xAB; 16])),
            look_ref: None,
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        // R10: still renders via the license-clean matrix base, flag surfaced.
        assert_eq!(out.profile.source, ProfileSource::MatrixBase);
        match out.fallback {
            Some(FallbackReason::ProfileMissing(ref s)) => assert!(s.contains("ab")),
            other => panic!("expected ProfileMissing, got {other:?}"),
        }
    }

    #[test]
    fn resolve_curated_default_for_camera_when_no_id() {
        let id = ProfileId([3u8; 16]);
        let reg = MockRegistry {
            camera_default: Some(dcp_like_profile(id)),
            ..Default::default()
        };
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let r = ProfileRef {
            kind: ProfileKind::CuratedDcp,
            id: None,
            look_ref: None,
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        assert_eq!(out.profile.id, id);
        assert!(out.fallback.is_none());
    }

    #[test]
    fn resolve_no_default_camera_falls_back() {
        let reg = MockRegistry::default();
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let r = ProfileRef {
            kind: ProfileKind::CuratedDcp,
            id: None,
            look_ref: None,
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        assert_eq!(out.profile.source, ProfileSource::MatrixBase);
        assert!(matches!(
            out.fallback,
            Some(FallbackReason::ProfileMissing(_))
        ));
    }

    #[test]
    fn resolve_look_present_and_missing() {
        let look_id = ProfileId([5u8; 16]);
        let reg = MockRegistry {
            looks: vec![make_look(look_id)],
            ..Default::default()
        };
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        // Look present.
        let r = ProfileRef {
            kind: ProfileKind::MatrixBase,
            id: None,
            look_ref: Some(look_id),
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        assert!(out.look.is_some());
        assert!(out.fallback.is_none());

        // Look missing → degrade to no look with a LookMissing flag.
        let r_missing = ProfileRef {
            kind: ProfileKind::MatrixBase,
            id: None,
            look_ref: Some(ProfileId([9u8; 16])),
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r_missing).unwrap();
        assert!(out.look.is_none());
        assert!(matches!(out.fallback, Some(FallbackReason::LookMissing(_))));
    }

    #[test]
    fn resolve_profile_missing_wins_over_look_missing() {
        let reg = MockRegistry::default();
        let cam = normalize_camera("Synthetic", "sRGB-Cam");
        let r = ProfileRef {
            kind: ProfileKind::CuratedDcp,
            id: Some(ProfileId([1u8; 16])),
            look_ref: Some(ProfileId([2u8; 16])),
            look_amount: 1.0,
        };
        let out = resolve_profile_ref(&reg, &srgb_camera(true), &cam, &r).unwrap();
        // Both are missing; the profile fallback is the reported (dominant) one.
        assert!(matches!(
            out.fallback,
            Some(FallbackReason::ProfileMissing(_))
        ));
        assert!(out.look.is_none());
    }

    proptest::proptest! {
        /// §7.1 WB round-trip property: neutral ↔ temp/tint over the corpus
        /// operating band recovers CCT within ±3 % and tint within ±2.
        #[test]
        fn wb_round_trip_property(cct in 3000.0f64..8000.0, tint in -30.0f64..30.0) {
            let cam = normalize_camera("Synthetic", "sRGB-Cam");
            let profile = camera_matrix_base(&srgb_camera(true), &cam).unwrap();
            let solver = ColorimetricSolver::new(&profile).unwrap();
            let n = solver.neutral_from_temp_tint(cct, tint);
            let (c2, t2) = solver.temp_tint_from_neutral(n);
            proptest::prop_assert!((c2 - cct).abs() / cct < 0.03, "cct {cct}->{c2}");
            proptest::prop_assert!((t2 - tint).abs() < 2.0, "tint {tint}->{t2}");
        }
    }
}
