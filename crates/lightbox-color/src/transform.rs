// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

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

/// Divisor setting [`Curve1D::tail_slope`]'s measurement window: the slope is
/// read across the last `N / TAIL_WINDOW_DIV` samples rather than the final
/// pair. At `N = 4096` that is a 64-sample window spanning `x ∈ [0.984, 1]`,
/// over which the default look's shoulder rises by ~0.0137, five orders of
/// magnitude above `f32::EPSILON` at that magnitude. Reading the final pair
/// instead would divide a difference of two nearly equal `f32`s by `1/4095`,
/// amplifying its rounding noise by 4095x, the same catastrophic-cancellation
/// trap `nodes::global::tone_recovery`'s `GUIDE_EPS` documents.
const TAIL_WINDOW_DIV: usize = 64;

/// A 1D curve sampled to `N` points for GPU upload (spec §3.4, `N = 4096`).
///
/// # Domain: sampled over `[0, 1]`, evaluated over `[0, ∞)`
///
/// The sample table covers `[0, 1]`, but [`Curve1D::eval`]'s **inputs are
/// scene-linear working RGB, which legitimately exceeds 1**: a sensor
/// highlight that clipped in one channel still carries unclipped signal in the
/// others, and the camera-to-working matrix (which folds in white balance)
/// turns that into working values above 1. On `canon-eos-350d.cr2` 2.29 % of
/// the frame lands above 1 that way, peaking at 2.28.
///
/// That is the raw file's highlight latitude, and it is the whole input to
/// `global.tone_recovery`. So `eval` **extends the curve above 1** rather than
/// clamping (see its docs). Clipping belongs at the display transform, which
/// is where `display.rs`'s own shaper evaluator (`display::eval_curve`, a
/// separate function that does still clamp, correctly, because a display
/// cannot show above white) performs it.
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

    /// Evaluates the sampled curve at `x` with linear interpolation.
    ///
    /// **Below `0`** the result is clamped to the first sample, unchanged:
    /// negative working values come from out-of-gamut matrix results, not from
    /// scene light, and extending a tone curve into them would hand later
    /// stages ever more negative input to take powers of.
    ///
    /// **Above `1`** the curve is *extended*, not clamped, along the straight
    /// line through its endpoint at the curve's own end slope
    /// ([`Curve1D::tail_slope`]). Matching the end slope rather than picking
    /// one makes the extension continuous in both value and first derivative
    /// at `x == 1`, so a smooth scene gradient crossing the sensor's clip
    /// point does not acquire a visible crease there.
    ///
    /// This is the line that decides whether a raw file's highlight latitude
    /// reaches the develop graph at all. It used to read `x.clamp(0.0, 1.0)`,
    /// which flattened every value above 1 onto the curve's endpoint: on
    /// `canon-eos-350d.cr2` that mapped the distinct scene values
    /// `[1.94, 1.20, 1.33]` and `[2.08, 1.15, 1.72]` onto the same
    /// `[1.0, 1.0, 1.0]`, which is what made a blown sky render as flat white
    /// with nothing left in it for `global.tone_recovery` to pull back.
    pub fn eval(&self, x: f32) -> f32 {
        let n = self.samples.len();
        if n == 0 {
            return x;
        }
        if n == 1 {
            return self.samples[0];
        }
        if x > 1.0 {
            return self.samples[n - 1] + self.tail_slope() * (x - 1.0);
        }
        let c = x.clamp(0.0, 1.0) * (n - 1) as f32;
        let i0 = (c.floor() as usize).min(n - 2);
        let f = c - i0 as f32;
        self.samples[i0] * (1.0 - f) + self.samples[i0 + 1] * f
    }

    /// The slope [`Curve1D::eval`] extends the curve with above `x == 1`,
    /// measured across the table's last `N / TAIL_WINDOW_DIV` samples (see
    /// that constant for why a window and not the final pair).
    ///
    /// Falls back to `1.0` (pass the excess through unchanged) whenever the
    /// measured slope is not finite and positive. A flat or falling tail is
    /// reachable, `from_spline_amount` clamps its samples to `[0, 1]`, so a
    /// `look_amount > 1` on a curve that already reaches white can saturate
    /// the last samples to a constant, and a slope of `0` there would silently
    /// reintroduce exactly the clamp this method exists to remove. Passing the
    /// excess through is the conservative failure: it keeps the highlight
    /// latitude, and `global.tone_recovery` still owns what happens to it.
    pub fn tail_slope(&self) -> f32 {
        let n = self.samples.len();
        if n < 2 {
            return 1.0;
        }
        let k = (n / TAIL_WINDOW_DIV).clamp(1, n - 1);
        let dy = self.samples[n - 1] - self.samples[n - 1 - k];
        let dx = k as f32 / (n - 1) as f32;
        let slope = dy / dx;
        if slope.is_finite() && slope > 0.0 {
            slope
        } else {
            1.0
        }
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
    /// (spec §3.4). **Phase B (B8)**, the golden/CPU parity anchor; an identity
    /// transform (identity matrix, no tables/curves) maps every input to itself.
    ///
    /// Stage order per §5.2: `cam_to_working` matrix → `BaselineExposureOffset`
    /// → HueSatMap → LookTable → ProfileToneCurve → Lightbox look.
    ///
    /// **Scene-referred output: the result is not bounded by 1.** Camera-native
    /// input is `[0, 1]` (the sensor's white level normalizes to 1), but
    /// `cam_to_working` folds white balance in, and white balance is exactly
    /// what turns a one-channel sensor clip into working values above 1. None
    /// of the six stages clamps at the top any more: `apply_huesat` preserves
    /// magnitude by construction and both tone curves extend past 1
    /// ([`Curve1D::eval`]). Callers that need display-bounded pixels apply the
    /// display transform, which is where clipping belongs.
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
        // whole look, curve *and* shaping, scales as one, matching the
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

/// Content hash over the resolve *inputs* + the baked matrix, identical inputs
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

/// `f32` matrix-vector product.
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
    // branch, see E02-deviations.md).
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

        // Tier 1 is built from decode metadata alone, no file assets consulted.
        let profile = camera_matrix_base(&raw, &camera).unwrap();
        assert_eq!(profile.source, crate::profile::ProfileSource::MatrixBase);
        assert!(profile.hue_sat_map.is_none() && profile.look_table.is_none());
        assert!(profile.tone_curve.is_none());

        // No default look, no DCP, pure matrix base.
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

    /// The default look's curve, resolved the way the raw source path resolves
    /// it, which is the curve every raw highlight actually goes through.
    fn default_look_curve() -> Curve1D {
        Curve1D::from_spline_amount(&crate::look::author_lightbox_color_v1().tone_curve, 1.0)
    }

    /// Distinct scene values above white must stay distinct.
    ///
    /// This is the property the whole highlight-recovery path rests on. The
    /// curve used to clamp its input to `[0, 1]`, so every above-white value
    /// collapsed onto the endpoint: `eval(1.2)`, `eval(2.0)` and `eval(8.0)`
    /// were all exactly `eval(1.0)`, which is a blown sky rendered as one flat
    /// tone with nothing left to recover.
    #[test]
    fn the_tone_curve_keeps_above_white_values_distinct() {
        let c = default_look_curve();
        let at_one = c.eval(1.0);
        let xs = [1.05f32, 1.2, 1.5, 2.0, 2.28, 8.0];
        let mut prev = at_one;
        for x in xs {
            let y = c.eval(x);
            assert!(
                y > prev,
                "eval({x}) = {y} did not exceed the previous value {prev}: the curve is \
                 flattening the highlight latitude"
            );
            assert!(y.is_finite(), "eval({x}) = {y}");
            prev = y;
        }
        // Not merely distinct: the excess has to survive at a usable scale, or
        // recovery has nothing to spend. The default look's shoulder slope is
        // ~0.87, so a stop of latitude must come through as most of a stop.
        let excess = c.eval(2.0) - at_one;
        assert!(
            excess > 0.7,
            "1.0 stops of latitude above white came through as only {excess}"
        );
    }

    /// The extension has to meet the curve smoothly, not just continuously.
    ///
    /// A slope discontinuity at exactly `x == 1` would put a visible crease
    /// into every smooth gradient that crosses the sensor's clip point, which
    /// is precisely where highlight recovery is looked at closely.
    #[test]
    fn the_extension_matches_the_curve_in_value_and_slope_at_white() {
        let c = default_look_curve();
        let d = 1.0e-3f32;
        // Value: the two one-sided limits agree.
        let below = c.eval(1.0 - d);
        let at = c.eval(1.0);
        let above = c.eval(1.0 + d);
        assert!((at - c.samples[c.samples.len() - 1]).abs() < 1e-6);
        // Slope: the one-sided differences agree to a few percent (the left
        // side is read off the sample table, the right off `tail_slope`, so
        // they are not expected to match to the last bit).
        let left = (at - below) / d;
        let right = (above - at) / d;
        assert!(
            (left - right).abs() / left.abs().max(1e-6) < 0.05,
            "slope jumps at white: {left} below vs {right} above"
        );
        assert!(
            (right - c.tail_slope()).abs() < 1e-3,
            "the extension does not use tail_slope: {right} vs {}",
            c.tail_slope()
        );
    }

    /// A curve whose tail has been flattened must pass the excess through
    /// rather than silently reinstating the clamp.
    #[test]
    fn a_flat_tail_falls_back_to_passing_the_excess_through() {
        let mut c = default_look_curve();
        let n = c.samples.len();
        for s in c.samples.iter_mut().skip(n - 128) {
            *s = 1.0;
        }
        assert_eq!(c.tail_slope(), 1.0, "a flat tail must not yield slope 0");
        assert!((c.eval(1.75) - 1.75).abs() < 1e-5, "got {}", c.eval(1.75));
    }

    /// Below zero is deliberately still clamped, and in-range evaluation is
    /// untouched. Pin both so the change above white cannot quietly widen.
    #[test]
    fn below_zero_stays_clamped_and_the_unit_interval_is_unchanged() {
        let c = default_look_curve();
        assert_eq!(c.eval(-0.5), c.samples[0]);
        assert_eq!(c.eval(-40.0), c.samples[0]);
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            let y = c.eval(x);
            assert!(
                (0.0..=1.0).contains(&y),
                "in-range input {x} left the unit interval at {y}"
            );
        }
    }

    /// A degenerate table must not divide by zero or return a non-finite
    /// slope on the way to answering.
    #[test]
    fn degenerate_curves_extend_without_blowing_up() {
        assert_eq!(Curve1D { samples: vec![] }.eval(3.0), 3.0);
        assert_eq!(Curve1D { samples: vec![0.5] }.eval(3.0), 0.5);
        let two = Curve1D {
            samples: vec![0.0, 1.0],
        };
        assert_eq!(two.tail_slope(), 1.0);
        assert!((two.eval(2.0) - 2.0).abs() < 1e-6);
    }
}
