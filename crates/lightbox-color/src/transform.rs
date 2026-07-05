// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The E05/E10 seam: [`ResolvedInputTransform`], a backend-agnostic, GPU-ready
//! description of the §4.1 input-transform stage (spec §3.4). **Owner: Phase B
//! (B8);** the full DCP path folds in at F5. Plain data (matrix + sampled
//! tables) a WGSL node uploads; [`ResolvedInputTransform::eval_cpu`] is the
//! reference semantics the golden tests and the §4.4 CPU path share.

use lightbox_decode::CameraId;

use crate::error::ColorError;
use crate::look::Look;
use crate::lut::HueSatTable;
use crate::profile::CameraProfile;
use crate::wb::WbMode;

/// A 1D curve sampled to `N` points for GPU upload (spec §3.4, `N = 4096`).
#[derive(Clone, Debug)]
pub struct Curve1D {
    /// Evenly-spaced samples over `[0, 1]`.
    pub samples: Vec<f32>,
}

/// The default-look curve + shaping with its amount pre-applied (spec §3.4).
#[derive(Clone, Debug)]
pub struct ResolvedLook {
    /// Scene-referred tone curve, amount-scaled.
    pub tone_curve: Curve1D,
    /// Optional hue/sat shaping, amount-scaled.
    pub hue_sat: Option<HueSatTable>,
}

/// The resolved §4.1 input transform (spec §3.4). Everything a WGSL node needs
/// to take camera-native linear RGB to working-space linear RGB, as plain data.
#[derive(Clone, Debug)]
pub struct ResolvedInputTransform {
    /// WB ⊕ interpolated matrices ⊕ adaptation ⊕ XYZ→ProPhoto, baked to f32.
    pub cam_to_working: [[f32; 3]; 3],
    /// `BaselineExposureOffset` (stops).
    pub baseline_exposure: f32,
    /// Resolved HueSatMap (encoding applied), upload-ready.
    pub hue_sat_map: Option<HueSatTable>,
    /// Resolved LookTable.
    pub look_table: Option<HueSatTable>,
    /// Sampled profile tone curve (`N = 4096`).
    pub profile_tone_curve: Option<Curve1D>,
    /// Resolved Lightbox look (curve/shaping ⊕ amount).
    pub look: Option<ResolvedLook>,
    /// Content hash → E05 node-cache key component (spec §3.4).
    pub key: u64,
}

impl ResolvedInputTransform {
    /// Reference evaluator: camera-native linear RGB → working-space linear RGB
    /// (spec §3.4). **Phase B (B8)** — the golden/CPU parity anchor; identity
    /// profile ⇒ identity transform.
    pub fn eval_cpu(&self, _rgb_cam: [f32; 3]) -> [f32; 3] {
        unimplemented!("B8: input-transform reference evaluator (§5.2 stage order)")
    }
}

/// Resolves the §4.1 input transform for a profile + WB + optional look
/// (spec §3.4). **Phase B (B8)**; the DCP stages fold in at F5. `look_amount`
/// is `0.0..=2.0` (1.0 = authored strength). Budget: < 1 ms (B8 criterion).
pub fn resolve_input_transform(
    _profile: &CameraProfile,
    _wb: &WbMode,
    _as_shot: Option<[f64; 3]>,
    _look: Option<&Look>,
    _look_amount: f32,
) -> Result<ResolvedInputTransform, ColorError> {
    unimplemented!("B8: bake WB ⊕ matrices ⊕ adaptation ⊕ tables ⊕ look → ResolvedInputTransform")
}

/// A recipe `base_profile` reference (E09 seam, spec §3.4).
#[derive(Clone, Debug)]
pub struct ProfileRef {
    /// Which kind of profile is referenced.
    pub kind: ProfileKind,
    /// The profile id, when one is pinned.
    pub id: Option<crate::profile::ProfileId>,
    /// An optional look reference.
    pub look_ref: Option<crate::profile::ProfileId>,
    /// Look amount (`0.0..=2.0`).
    pub look_amount: f32,
}

/// The kind of profile a [`ProfileRef`] names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileKind {
    /// The license-clean matrix base (§1.7 tier 1).
    MatrixBase,
    /// A curated in-house DCP (tier 3).
    CuratedDcp,
    /// A user-installed DCP.
    UserDcp,
}

/// Why a resolve fell back to the matrix base + default look (Risk 10).
#[derive(Clone, Debug)]
pub enum FallbackReason {
    /// The requested profile was not installed.
    ProfileMissing(String),
    /// The requested look was not installed.
    LookMissing(String),
}

/// A resolved profile + look, plus any fallback that applied (spec §3.4).
#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    /// The resolved camera profile.
    pub profile: CameraProfile,
    /// The resolved look, if any.
    pub look: Option<Look>,
    /// The fallback that applied, if any (user-visible flag, Risk 10).
    pub fallback: Option<FallbackReason>,
}

/// Registry the resolver queries for installed profiles/looks (spec §3.4).
/// **Owner: Phase F (F7);** the catalog-backed impl is H1.
pub trait ProfileRegistry {
    /// Looks up an installed profile by id.
    fn profile(&self, id: &crate::profile::ProfileId) -> Option<CameraProfile>;
    /// Looks up an installed look by id.
    fn look(&self, id: &crate::profile::ProfileId) -> Option<Look>;
    /// The curated default profile for a camera, if one is bundled.
    fn default_for_camera(&self, camera: &CameraId) -> Option<CameraProfile>;
}
