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
use crate::matrix::{spaces, Mat3, Spline1D, Vec3};
use crate::profile::{CameraProfile, ColorimetricSolver};
use crate::wb::WbMode;

/// Samples per resolved curve (spec §3.4, `N = 4096`).
const CURVE_SAMPLES: usize = 4096;

/// A 1D curve sampled to `N` points for GPU upload (spec §3.4, `N = 4096`).
#[derive(Clone, Debug)]
pub struct Curve1D {
    /// Evenly-spaced samples over `[0, 1]`.
    pub samples: Vec<f32>,
}

impl Curve1D {
    /// Samples a spline to `N = 4096` evenly-spaced points over `[0, 1]`.
    pub fn from_spline(spline: &Spline1D) -> Curve1D {
        let mut samples = Vec::with_capacity(CURVE_SAMPLES);
        for i in 0..CURVE_SAMPLES {
            let x = i as f32 / (CURVE_SAMPLES - 1) as f32;
            samples.push(spline.eval(x));
        }
        Curve1D { samples }
    }

    /// Samples a spline, blending toward identity by `amount` (`0.0..=2.0`):
    /// identity-lerp below 100 %, bounded extrapolation above, output clamped to
    /// `[0, 1]` (the E2 amount semantics, applied here so B8's resolve is
    /// self-contained; E2 refines the shaping-axis amount).
    pub fn from_spline_amount(spline: &Spline1D, amount: f32) -> Curve1D {
        let a = amount.clamp(0.0, 2.0);
        let mut samples = Vec::with_capacity(CURVE_SAMPLES);
        for i in 0..CURVE_SAMPLES {
            let x = i as f32 / (CURVE_SAMPLES - 1) as f32;
            let y = spline.eval(x);
            samples.push((x + a * (y - x)).clamp(0.0, 1.0));
        }
        Curve1D { samples }
    }

    /// Evaluates the sampled curve at `x` with linear interpolation, clamping
    /// outside `[0, 1]`.
    pub fn eval(&self, x: f32) -> f32 {
        let n = self.samples.len();
        if n == 0 {
            return x;
        }
        if n == 1 {
            return self.samples[0];
        }
        let c = x.clamp(0.0, 1.0) * (n - 1) as f32;
        let i0 = (c.floor() as usize).min(n - 2);
        let f = c - i0 as f32;
        self.samples[i0] * (1.0 - f) + self.samples[i0 + 1] * f
    }
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
    /// (spec §3.4). **Phase B (B8)** — the golden/CPU parity anchor; an identity
    /// transform (identity matrix, no tables/curves) maps every input to itself.
    ///
    /// Stage order per §5.2: `cam_to_working` matrix → `BaselineExposureOffset`
    /// → HueSatMap → LookTable → ProfileToneCurve → Lightbox look.
    pub fn eval_cpu(&self, rgb_cam: [f32; 3]) -> [f32; 3] {
        // 1. Camera-native → working-space matrix.
        let mut rgb = mul_f32(&self.cam_to_working, rgb_cam);

        // 2. Baseline exposure (stops → linear gain).
        if self.baseline_exposure != 0.0 {
            let gain = 2.0f32.powf(self.baseline_exposure);
            rgb = [rgb[0] * gain, rgb[1] * gain, rgb[2] * gain];
        }

        // 3. HueSatMap.
        if let Some(t) = &self.hue_sat_map {
            rgb = apply_huesat(t, rgb);
        }
        // 4. LookTable.
        if let Some(t) = &self.look_table {
            rgb = apply_huesat(t, rgb);
        }
        // 5. Profile tone curve (per channel).
        if let Some(c) = &self.profile_tone_curve {
            rgb = [c.eval(rgb[0]), c.eval(rgb[1]), c.eval(rgb[2])];
        }
        // 6. Lightbox look: tone curve then optional shaping.
        if let Some(look) = &self.look {
            rgb = [
                look.tone_curve.eval(rgb[0]),
                look.tone_curve.eval(rgb[1]),
                look.tone_curve.eval(rgb[2]),
            ];
            if let Some(t) = &look.hue_sat {
                rgb = apply_huesat(t, rgb);
            }
        }
        rgb
    }

    /// CPU reference render (B9 color core): camera-native linear RGB → an 8-bit
    /// sRGB triple, through the input transform then working→sRGB (Bradford
    /// D50→D65 ⊕ sRGB OETF). The full `lightbox-cli render-ref` corpus harness
    /// (decode → linearize → demosaic → this) is deferred to the merge/Phase H
    /// integration because it needs the Phase C LibRaw proxy + corpus, which are
    /// not on this branch (recorded in `E02-deviations.md`).
    pub fn render_reference_srgb8(&self, rgb_cam: [f32; 3]) -> [u8; 3] {
        let working = self.eval_cpu(rgb_cam);
        working_linear_to_srgb8(working)
    }
}

/// Working (ProPhoto-linear, D50) → 8-bit sRGB, the B9 output stage.
pub fn working_linear_to_srgb8(working: [f32; 3]) -> [u8; 3] {
    let m = spaces::working_to_linear_srgb();
    let lin = m
        .mul_vec(Vec3([
            working[0] as f64,
            working[1] as f64,
            working[2] as f64,
        ]))
        .0;
    let mut out = [0u8; 3];
    for (o, &l) in out.iter_mut().zip(lin.iter()) {
        let e = spaces::srgb_oetf(l as f32);
        *o = (e * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    out
}

/// Resolves the §4.1 input transform for a profile + WB + optional look
/// (spec §3.4). **Phase B (B8)**; the DCP stages fold in at F5. `look_amount`
/// is `0.0..=2.0` (1.0 = authored strength). Budget: < 1 ms (B8 criterion).
pub fn resolve_input_transform(
    profile: &CameraProfile,
    wb: &WbMode,
    as_shot: Option<[f64; 3]>,
    look: Option<&Look>,
    look_amount: f32,
) -> Result<ResolvedInputTransform, ColorError> {
    let solver = ColorimetricSolver::new(profile)?;
    let wp = solver.white_point(wb, as_shot)?;

    // WB ⊕ dual-illuminant interpolation ⊕ FM-or-inverse-CM ⊕ Bradford→D50.
    let cam_to_xyz_d50 = solver.cam_to_xyz_d50(&wp);
    // ⊕ XYZ(D50)→ProPhoto-linear working space.
    let cam_to_working_f64 = spaces::xyz_d50_to_working().mul(&cam_to_xyz_d50);
    let cam_to_working = to_f32_mat(&cam_to_working_f64);

    let hue_sat_map = profile.hue_sat_map.as_ref().map(HueSatTable::from_lut);
    let look_table = profile.look_table.as_ref().map(HueSatTable::from_lut);
    let profile_tone_curve = profile.tone_curve.as_ref().map(Curve1D::from_spline);

    let resolved_look = look.map(|l| ResolvedLook {
        tone_curve: Curve1D::from_spline_amount(&l.tone_curve, look_amount),
        // E2 refines the shaping-axis amount (Phase B left this hook): the look's
        // hue/sat shaping is lerped toward identity by `look_amount` so the
        // whole look — curve *and* shaping — scales as one, matching the
        // reference `Look::eval`. (See E02-deviations.md.)
        hue_sat: l
            .hue_sat
            .as_ref()
            .map(|hs| crate::look::resolve_look_hue_sat(hs, look_amount)),
    });

    let key = content_key(profile, wb, as_shot, look, look_amount, &cam_to_working);

    Ok(ResolvedInputTransform {
        cam_to_working,
        baseline_exposure: profile.baseline_exposure_offset,
        hue_sat_map,
        look_table,
        profile_tone_curve,
        look: resolved_look,
        key,
    })
}

/// Content hash over the resolve *inputs* + the baked matrix — identical inputs
/// yield an identical key; any WB / look / amount / profile change flips it
/// (spec §3.4, the E05 node-cache key component).
fn content_key(
    profile: &CameraProfile,
    wb: &WbMode,
    as_shot: Option<[f64; 3]>,
    look: Option<&Look>,
    look_amount: f32,
    cam_to_working: &[[f32; 3]; 3],
) -> u64 {
    let mut b: Vec<u8> = Vec::with_capacity(128);
    b.extend_from_slice(&profile.id.0);
    match wb {
        WbMode::AsShot => b.push(0),
        WbMode::TempTint { kelvin, tint } => {
            b.push(1);
            b.extend_from_slice(&kelvin.to_le_bytes());
            b.extend_from_slice(&tint.to_le_bytes());
        }
        WbMode::Neutral(n) => {
            b.push(2);
            for v in n {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    match as_shot {
        None => b.push(0),
        Some(n) => {
            b.push(1);
            for v in n {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    match look {
        None => b.push(0),
        Some(l) => {
            b.push(1);
            b.extend_from_slice(&l.id.0);
            b.extend_from_slice(&look_amount.to_le_bytes());
        }
    }
    for row in cam_to_working {
        for v in row {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b.extend_from_slice(&profile.baseline_exposure_offset.to_le_bytes());
    twox_hash::XxHash3_64::oneshot(&b)
}

/// `f64` `Mat3` → `f32` row-major array.
fn to_f32_mat(m: &Mat3) -> [[f32; 3]; 3] {
    let a = &m.0;
    [
        [a[0][0] as f32, a[0][1] as f32, a[0][2] as f32],
        [a[1][0] as f32, a[1][1] as f32, a[1][2] as f32],
        [a[2][0] as f32, a[2][1] as f32, a[2][2] as f32],
    ]
}

/// `f32` matrix–vector product.
fn mul_f32(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Applies a resolved HueSat table to a working-space RGB triple via HSV.
/// `pub(crate)` so the E2 reference look evaluator ([`crate::look::Look::eval`])
/// shares this exact shaping path with the resolved GPU-upload form.
pub(crate) fn apply_huesat(table: &HueSatTable, rgb: [f32; 3]) -> [f32; 3] {
    let hsv = rgb_to_hsv(rgb);
    let out = table.eval(hsv);
    hsv_to_rgb(out)
}

/// RGB → HSV (`hue ∈ [0, 360)`, `sat`/`value ∈ [0, 1]`). Values above 1 keep
/// their magnitude in `value` (HSV `value` is the channel max).
fn rgb_to_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let mut h = if delta <= 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    let s = if max.abs() <= f32::MIN_POSITIVE {
        0.0
    } else {
        delta / max
    };
    [h, s, max]
}

/// HSV → RGB inverse of [`rgb_to_hsv`].
fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let (h, s, v) = (hsv[0], hsv[1], hsv[2]);
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r1 + m, g1 + m, b1 + m]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::look::{Look, LookProvenance};
    use crate::matrix::{
        bradford_adaptation, rgb_to_xyz_matrix, spaces, Mat3, Vec3, D50_WHITE_XYZ, D65_WHITE_XY,
        D65_WHITE_XYZ, SRGB_PRIMARIES,
    };
    use crate::profile::{camera_matrix_base, ProfileId};
    use lightbox_decode::{normalize_camera, Illuminant, RawColorimetry};

    // A synthetic decoded-raw fixture: a camera whose native space is linear
    // sRGB (D65), calibration matrices exact so the tier-1 chain round-trips.
    // Stands in for a corpus raw (the Phase C proxy + corpus are not on this
    // branch — see E02-deviations.md).
    fn srgb_camera_raw() -> RawColorimetry {
        let srgb_to_xyz_d65 = rgb_to_xyz_matrix(SRGB_PRIMARIES, D65_WHITE_XY);
        let cm1 = srgb_to_xyz_d65.inverse().unwrap();
        let fm1 = bradford_adaptation(D65_WHITE_XYZ, D50_WHITE_XYZ).mul(&srgb_to_xyz_d65);
        let n = cm1.mul_vec(Vec3(D65_WHITE_XYZ)).0;
        let g = n[1];
        RawColorimetry {
            as_shot_neutral: Some([n[0] / g, n[1] / g, n[2] / g]),
            illuminant1: Illuminant::D65,
            illuminant2: None,
            color_matrix1: cm1.0,
            color_matrix2: None,
            forward_matrix1: Some(fm1.0),
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        }
    }

    fn identity_transform() -> ResolvedInputTransform {
        ResolvedInputTransform {
            cam_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            baseline_exposure: 0.0,
            hue_sat_map: None,
            look_table: None,
            profile_tone_curve: None,
            look: None,
            key: 0,
        }
    }

    // ---- CIEDE2000 (test-only, for the golden gate) ----

    fn srgb8_to_lab(c: [u8; 3]) -> [f64; 3] {
        let lin = [
            spaces::srgb_eotf(c[0] as f32 / 255.0) as f64,
            spaces::srgb_eotf(c[1] as f32 / 255.0) as f64,
            spaces::srgb_eotf(c[2] as f32 / 255.0) as f64,
        ];
        let xyz = rgb_to_xyz_matrix(SRGB_PRIMARIES, D65_WHITE_XY)
            .mul_vec(Vec3(lin))
            .0;
        // Lab under D65.
        let wn = [
            D65_WHITE_XYZ[0] * 100.0,
            D65_WHITE_XYZ[1] * 100.0,
            D65_WHITE_XYZ[2] * 100.0,
        ];
        let f = |t: f64| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                (903.3 * t + 16.0) / 116.0
            }
        };
        let fx = f(xyz[0] * 100.0 / wn[0]);
        let fy = f(xyz[1] * 100.0 / wn[1]);
        let fz = f(xyz[2] * 100.0 / wn[2]);
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    fn ciede2000(l1: [f64; 3], l2: [f64; 3]) -> f64 {
        let d2r = std::f64::consts::PI / 180.0;
        let (c1, c2) = (
            (l1[1] * l1[1] + l1[2] * l1[2]).sqrt(),
            (l2[1] * l2[1] + l2[2] * l2[2]).sqrt(),
        );
        let c_bar = (c1 + c2) / 2.0;
        let c7 = c_bar.powi(7);
        let g = 0.5 * (1.0 - (c7 / (c7 + 25f64.powi(7))).sqrt());
        let a1p = (1.0 + g) * l1[1];
        let a2p = (1.0 + g) * l2[1];
        let c1p = (a1p * a1p + l1[2] * l1[2]).sqrt();
        let c2p = (a2p * a2p + l2[2] * l2[2]).sqrt();
        let hp = |b: f64, ap: f64| {
            if b == 0.0 && ap == 0.0 {
                0.0
            } else {
                let mut h = b.atan2(ap) / d2r;
                if h < 0.0 {
                    h += 360.0;
                }
                h
            }
        };
        let h1p = hp(l1[2], a1p);
        let h2p = hp(l2[2], a2p);
        let dlp = l2[0] - l1[0];
        let dcp = c2p - c1p;
        let dhp = if c1p * c2p == 0.0 {
            0.0
        } else {
            let d = h2p - h1p;
            if d > 180.0 {
                d - 360.0
            } else if d < -180.0 {
                d + 360.0
            } else {
                d
            }
        };
        let big_dhp = 2.0 * (c1p * c2p).sqrt() * (dhp / 2.0 * d2r).sin();
        let lbp = (l1[0] + l2[0]) / 2.0;
        let cbp = (c1p + c2p) / 2.0;
        let hbp = if c1p * c2p == 0.0 {
            h1p + h2p
        } else if (h1p - h2p).abs() <= 180.0 {
            (h1p + h2p) / 2.0
        } else if h1p + h2p < 360.0 {
            (h1p + h2p + 360.0) / 2.0
        } else {
            (h1p + h2p - 360.0) / 2.0
        };
        let t = 1.0 - 0.17 * ((hbp - 30.0) * d2r).cos()
            + 0.24 * ((2.0 * hbp) * d2r).cos()
            + 0.32 * ((3.0 * hbp + 6.0) * d2r).cos()
            - 0.20 * ((4.0 * hbp - 63.0) * d2r).cos();
        let dtheta = 30.0 * (-(((hbp - 275.0) / 25.0).powi(2))).exp();
        let cbp7 = cbp.powi(7);
        let rc = 2.0 * (cbp7 / (cbp7 + 25f64.powi(7))).sqrt();
        let sl = 1.0 + (0.015 * (lbp - 50.0).powi(2)) / (20.0 + (lbp - 50.0).powi(2)).sqrt();
        let sc = 1.0 + 0.045 * cbp;
        let sh = 1.0 + 0.015 * cbp * t;
        let rt = -(2.0 * dtheta * d2r).sin() * rc;
        ((dlp / sl).powi(2)
            + (dcp / sc).powi(2)
            + (big_dhp / sh).powi(2)
            + rt * (dcp / sc) * (big_dhp / sh))
            .sqrt()
    }

    /// The first failing test (§10.1 E02.1 / task B7): a decoded raw is white-
    /// balanced as-shot and rendered through `camera_matrix_base()` → working
    /// space → sRGB by the CPU reference pipeline **with no bundled profile
    /// assets**, matching the golden within ΔE2000 ≤ 1.0.
    #[test]
    fn renders_correct_color_with_zero_bundled_profiles() {
        let raw = srgb_camera_raw();
        let camera = normalize_camera("Synthetic", "sRGB-Cam");

        // Tier 1 is built from decode metadata alone — no file assets consulted.
        let profile = camera_matrix_base(&raw, &camera).unwrap();
        assert_eq!(profile.source, crate::profile::ProfileSource::MatrixBase);
        assert!(profile.hue_sat_map.is_none() && profile.look_table.is_none());
        assert!(profile.tone_curve.is_none());

        // No default look, no DCP — pure matrix base.
        let transform =
            resolve_input_transform(&profile, &WbMode::AsShot, raw.as_shot_neutral, None, 1.0)
                .unwrap();
        assert!(transform.look.is_none());

        // A neutral-through-saturated golden patch set (sRGB 8-bit). Because the
        // synthetic camera *is* linear sRGB, each patch's camera-native RGB is
        // its own linear sRGB, so a colorimetrically-correct pipeline recovers
        // the patch. Any error in the matrix base, WB solve, working space, or
        // Bradford adaptation breaks the round-trip and blows the ΔE gate.
        let golden: [[u8; 3]; 8] = [
            [128, 128, 128], // neutral gray
            [242, 242, 242], // near-white
            [30, 30, 30],    // near-black
            [200, 60, 55],   // red
            [70, 150, 80],   // green
            [55, 85, 175],   // blue
            [196, 150, 130], // skin
            [110, 145, 190], // sky
        ];

        let mut max_de = 0.0f64;
        for patch in golden {
            let cam_rgb = [
                spaces::srgb_eotf(patch[0] as f32 / 255.0),
                spaces::srgb_eotf(patch[1] as f32 / 255.0),
                spaces::srgb_eotf(patch[2] as f32 / 255.0),
            ];
            let out = transform.render_reference_srgb8(cam_rgb);
            let de = ciede2000(srgb8_to_lab(patch), srgb8_to_lab(out));
            max_de = max_de.max(de);
            assert!(
                de <= 1.0,
                "patch {patch:?} rendered {out:?}, ΔE2000 {de:.3} > 1.0"
            );
        }
        assert!(max_de <= 1.0, "max ΔE2000 {max_de:.3}");
    }

    #[test]
    fn identity_transform_is_the_identity() {
        let t = identity_transform();
        for rgb in [[0.0, 0.0, 0.0], [0.3, 0.6, 0.9], [1.0, 0.5, 0.25]] {
            let out = t.eval_cpu(rgb);
            for k in 0..3 {
                assert!((out[k] - rgb[k]).abs() < 1e-6, "{rgb:?} -> {out:?}");
            }
        }
    }

    #[test]
    fn prophoto_camera_resolves_to_identity_matrix() {
        // A camera whose native space *is* the working space ⇒ cam_to_working
        // must be identity (the "identity profile ⇒ identity transform" AC).
        let cm1 = spaces::xyz_d50_to_working(); // XYZ(D50) → ProPhoto camera
        let fm1 = spaces::working_to_xyz_d50();
        let n = cm1.mul_vec(Vec3(D50_WHITE_XYZ)).0;
        let g = n[1];
        let raw = RawColorimetry {
            as_shot_neutral: Some([n[0] / g, n[1] / g, n[2] / g]),
            illuminant1: Illuminant::D50,
            illuminant2: None,
            color_matrix1: cm1.0,
            color_matrix2: None,
            forward_matrix1: Some(fm1.0),
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        };
        let camera = normalize_camera("Synthetic", "ProPhoto-Cam");
        let profile = camera_matrix_base(&raw, &camera).unwrap();
        let t = resolve_input_transform(&profile, &WbMode::AsShot, raw.as_shot_neutral, None, 1.0)
            .unwrap();
        let ident = Mat3::IDENTITY.0;
        for (i, (row, irow)) in t.cam_to_working.iter().zip(ident.iter()).enumerate() {
            for (j, (v, iv)) in row.iter().zip(irow.iter()).enumerate() {
                assert!(
                    (*v as f64 - iv).abs() < 1e-3,
                    "cam_to_working[{i}][{j}] = {v}"
                );
            }
        }
        // And eval_cpu round-trips a ProPhoto-linear value.
        let out = t.eval_cpu([0.4, 0.5, 0.6]);
        for (o, e) in out.iter().zip([0.4, 0.5, 0.6]) {
            assert!((o - e).abs() < 2e-3);
        }
    }

    fn test_look() -> Look {
        Look {
            id: ProfileId([9u8; 16]),
            name: "test".into(),
            version: 1,
            tone_curve: crate::matrix::Spline1D::identity(),
            hue_sat: None,
            provenance: LookProvenance {
                author: "lightbox-authored".into(),
                license: "project".into(),
                review_record: None,
            },
        }
    }

    #[test]
    fn key_is_stable_and_sensitive() {
        let raw = srgb_camera_raw();
        let camera = normalize_camera("Synthetic", "sRGB-Cam");
        let profile = camera_matrix_base(&raw, &camera).unwrap();
        let look = test_look();

        let base =
            resolve_input_transform(&profile, &WbMode::AsShot, raw.as_shot_neutral, None, 1.0)
                .unwrap()
                .key;
        // Identical inputs ⇒ identical key.
        let again =
            resolve_input_transform(&profile, &WbMode::AsShot, raw.as_shot_neutral, None, 1.0)
                .unwrap()
                .key;
        assert_eq!(base, again);

        // WB change ⇒ key change.
        let wb_changed = resolve_input_transform(
            &profile,
            &WbMode::TempTint {
                kelvin: 5000.0,
                tint: 0.0,
            },
            raw.as_shot_neutral,
            None,
            1.0,
        )
        .unwrap()
        .key;
        assert_ne!(base, wb_changed);

        // Look presence ⇒ key change.
        let with_look = resolve_input_transform(
            &profile,
            &WbMode::AsShot,
            raw.as_shot_neutral,
            Some(&look),
            1.0,
        )
        .unwrap()
        .key;
        assert_ne!(base, with_look);

        // Amount change ⇒ key change.
        let amount_changed = resolve_input_transform(
            &profile,
            &WbMode::AsShot,
            raw.as_shot_neutral,
            Some(&look),
            1.5,
        )
        .unwrap()
        .key;
        assert_ne!(with_look, amount_changed);
    }

    #[test]
    fn look_amount_zero_is_identity_curve() {
        let raw = srgb_camera_raw();
        let camera = normalize_camera("Synthetic", "sRGB-Cam");
        let profile = camera_matrix_base(&raw, &camera).unwrap();
        // A non-trivial S-curve look at amount 0 must collapse to identity.
        let mut look = test_look();
        look.tone_curve = crate::matrix::Spline1D {
            control_points: vec![[0.0, 0.0], [0.25, 0.18], [0.75, 0.82], [1.0, 1.0]],
        };
        let t = resolve_input_transform(
            &profile,
            &WbMode::AsShot,
            raw.as_shot_neutral,
            Some(&look),
            0.0,
        )
        .unwrap();
        let rl = t.look.unwrap();
        for i in 0..rl.tone_curve.samples.len() {
            let x = i as f32 / (rl.tone_curve.samples.len() - 1) as f32;
            assert!(
                (rl.tone_curve.samples[i] - x).abs() < 1e-4,
                "amount=0 curve not identity at {x}"
            );
        }
    }
}
