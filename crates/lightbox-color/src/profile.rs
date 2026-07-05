// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Camera profiles: the tier-1 matrix base ([`camera_matrix_base`], **Phase B
//! B7**), the per-profile colorimetric solver ([`ColorimetricSolver`], **B3/B4/
//! B5**), and profile-reference resolution ([`resolve_profile_ref`], **Phase F
//! F7**). The [`CameraProfile`] type is the shared tier-1/2/3 container (spec
//! §3.4) — DCP parsing ([`crate::dcp::parse_dcp`]) produces one, the resolver
//! consumes one.

use lightbox_decode::{CameraId, Illuminant, RawColorimetry};

use crate::error::ColorError;
use crate::lut::HueSatLut;
use crate::matrix::{Mat3, Spline1D};
use crate::transform::{ProfileRef, ProfileRegistry, ResolvedProfile};
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

/// A camera color profile — tier 1 (matrix base), tier 2 wiring, or tier 3
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
    /// `ProfileCopyright` — surfaced; Adobe-authored content is banned upstream
    /// (surface-3 manifest check, spec §4.3).
    pub copyright: Option<String>,
}

/// Builds the §1.7 tier-1 colorimetric base from file metadata alone
/// (spec §3.3). **Phase B (B7)** — `renders_correct_color_with_zero_bundled_
/// profiles` is the executable proof (the first failing test).
pub fn camera_matrix_base(
    _c: &RawColorimetry,
    _camera: &CameraId,
) -> Result<CameraProfile, ColorError> {
    unimplemented!("B7: license-clean matrix base from decode metadata")
}

/// Per-profile colorimetric solver (DNG spec ch. 6 semantics, spec §3.3).
/// **Owner: Phase B (B3/B4/B5).**
pub struct ColorimetricSolver<'p> {
    /// The profile this solver is bound to (plus precomputed state B fills).
    pub profile: &'p CameraProfile,
}

impl<'p> ColorimetricSolver<'p> {
    /// Binds a solver to a profile, precomputing invariant state (B3).
    pub fn new(profile: &'p CameraProfile) -> Result<Self, ColorError> {
        // The binding itself is cheap; the precompute is B3. Returned as-is so
        // downstream modules can construct the type today.
        Ok(ColorimetricSolver { profile })
    }

    /// White-point self-consistent iteration for `AsShot`; direct for
    /// `TempTint` (spec §3.3 B3). Non-convergence → [`ColorError::NonConvergent`].
    pub fn white_point(
        &self,
        _wb: &WbMode,
        _as_shot: Option<[f64; 3]>,
    ) -> Result<WhitePoint, ColorError> {
        unimplemented!("B3: white-point self-consistent iteration")
    }

    /// `(Kelvin, tint)` → camera-native neutral (spec §3.3 B5).
    pub fn neutral_from_temp_tint(&self, _kelvin: f64, _tint: f64) -> [f64; 3] {
        unimplemented!("B5: neutral from temp/tint")
    }

    /// Camera-native neutral → `(Kelvin, tint)` (spec §3.3 B5, the eyedropper
    /// solve of §5.4).
    pub fn temp_tint_from_neutral(&self, _neutral: [f64; 3]) -> (f64, f64) {
        unimplemented!("B5: temp/tint from neutral")
    }

    /// Interpolated (by inverse CCT) camera→XYZ(D50): ForwardMatrix path when
    /// present, else inverse-ColorMatrix + Bradford to D50 (spec §3.3 B4).
    pub fn cam_to_xyz_d50(&self, _wp: &WhitePoint) -> Mat3 {
        unimplemented!("B4: interpolated camera→XYZ(D50)")
    }
}

/// Resolves a recipe `base_profile` reference with the Risk-10 fallback chain:
/// requested → (missing) → matrix base + default look, with a user-visible flag
/// (spec §3.4). **Owner: Phase F (F7).**
pub fn resolve_profile_ref(
    _reg: &dyn ProfileRegistry,
    _colorimetry: &RawColorimetry,
    _camera: &CameraId,
    _r: &ProfileRef,
) -> Result<ResolvedProfile, ColorError> {
    unimplemented!("F7: profile-ref resolution with matrix-base fallback")
}
