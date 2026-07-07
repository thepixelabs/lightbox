// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Recipe leaf types — the architecture §3.2 field set, typed in full at M1 so
//! E10/E11 never bump the serialization schema for a field the architecture
//! already enumerates (spec §1 in-scope item 1). Every type has a **neutral**
//! `Default` (renders the source unmodified) and, where it holds ranged values,
//! a `clamp` that folds out-of-range values back into their §3.2 domain.
//!
//! No pixels here (spec §1.1): these are inert value carriers.

use serde::{Deserialize, Serialize};

/// The authoritative CBOR value type for extension bags / foreign fields.
/// `lb_extra`, `unknown`, and [`XmpPassthrough`] hold these verbatim.
pub type CborValue = ciborium::value::Value;

/// Clamp `x` into `[lo, hi]`, mapping non-finite inputs to `lo` (untrusted
/// deltas — spec §4.2 "apply validates + clamps"). `lo <= hi` assumed.
pub(crate) fn clampf(x: f32, lo: f32, hi: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        lo
    }
}

// ── profile ────────────────────────────────────────────────────────────────

/// Base rendering profile kind (§3.2 / §1.7). `Matrix` is the license-clean
/// default; `Dcp` is a curated/user DNG camera profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProfileKind {
    /// Matrix base profile (license-clean default, §1.7).
    #[default]
    Matrix,
    /// A DNG camera profile (`.dcp`).
    Dcp,
}

/// Base profile reference + creative look (§3.2). Neutral = the matrix base with
/// no look family and unity amount.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ProfileRef {
    /// Profile kind (matrix default | curated DCP).
    pub kind: ProfileKind,
    /// `"matrix-base"` | a curated/user profile id.
    pub id: String,
    /// Lightbox-authored default look family (§1.7 tier 2), if any.
    pub look_ref: Option<String>,
    /// Look strength, `0.0..=2.0`, neutral `1.0`.
    pub look_amount: f32,
}

impl Default for ProfileRef {
    fn default() -> Self {
        ProfileRef {
            kind: ProfileKind::Matrix,
            id: "matrix-base".to_string(),
            look_ref: None,
            look_amount: 1.0,
        }
    }
}

impl ProfileRef {
    pub(crate) fn clamp(&mut self) {
        self.look_amount = clampf(self.look_amount, 0.0, 2.0);
    }
}

// ── white balance ────────────────────────────────────────────────────────────

/// A named white-balance preset (§3.2 `Preset(WbPreset)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WbPreset {
    /// Daylight.
    #[default]
    Daylight,
    /// Cloudy.
    Cloudy,
    /// Shade.
    Shade,
    /// Tungsten / incandescent.
    Tungsten,
    /// Fluorescent.
    Fluorescent,
    /// Flash.
    Flash,
}

/// White balance (§3.2). Neutral = `AsShot` (renders the source unmodified).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WhiteBalance {
    /// The camera's as-shot white balance (neutral default).
    #[default]
    AsShot,
    /// Auto white balance.
    Auto,
    /// A named preset.
    Preset(WbPreset),
    /// Custom temperature (Kelvin) + tint.
    Custom {
        /// Correlated colour temperature, `2000..=50000` K.
        temp_k: f32,
        /// Green–magenta tint, `-150..=150`.
        tint: f32,
    },
}

impl WhiteBalance {
    pub(crate) fn clamp(&mut self) {
        if let WhiteBalance::Custom { temp_k, tint } = self {
            *temp_k = clampf(*temp_k, 2000.0, 50000.0);
            *tint = clampf(*tint, -150.0, 150.0);
        }
    }
}

// ── tone curve ───────────────────────────────────────────────────────────────

/// A single tone-curve point in normalized `[0,1]` input/output space (§3.2).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct CurvePoint {
    /// Input, `0.0..=1.0`.
    pub x: f32,
    /// Output, `0.0..=1.0`.
    pub y: f32,
}

/// One tone curve: `<=64` points, x-coordinates strictly increasing (monotone-x).
/// Neutral = the two-point linear identity `(0,0)->(1,1)`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Control points; empty is also treated as identity.
    pub points: Vec<CurvePoint>,
}

/// Maximum control points per tone curve (§3.2).
pub const MAX_CURVE_POINTS: usize = 64;

impl Default for ToneCurve {
    fn default() -> Self {
        ToneCurve {
            points: vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }],
        }
    }
}

impl ToneCurve {
    /// Clamp every point into `[0,1]^2` (in place). Count/monotone are checked by
    /// [`ToneCurve::validate`], not here.
    pub(crate) fn clamp_points(&mut self) {
        for p in &mut self.points {
            p.x = clampf(p.x, 0.0, 1.0);
            p.y = clampf(p.y, 0.0, 1.0);
        }
    }

    /// `Ok` iff `<=64` points and (post-clamp) x is strictly increasing.
    pub(crate) fn validate(&self) -> Result<(), CurveInvalid> {
        if self.points.len() > MAX_CURVE_POINTS {
            return Err(CurveInvalid::TooMany(self.points.len()));
        }
        // Points are clamped finite before this check, so `>=` is well-defined.
        for w in self.points.windows(2) {
            if w[0].x >= w[1].x {
                return Err(CurveInvalid::NonMonotone);
            }
        }
        Ok(())
    }
}

/// Why a tone curve failed validation (surfaced as a `RecipeError`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CurveInvalid {
    TooMany(usize),
    NonMonotone,
}

/// The full tone-curve set: composite RGB plus per-channel curves, edited as one
/// unit (§3.2). Neutral = all four identity.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ToneCurveSet {
    /// Composite RGB curve.
    pub rgb: ToneCurve,
    /// Red-channel curve.
    pub r: ToneCurve,
    /// Green-channel curve.
    pub g: ToneCurve,
    /// Blue-channel curve.
    pub b: ToneCurve,
}

impl ToneCurveSet {
    pub(crate) fn clamp_points(&mut self) {
        self.rgb.clamp_points();
        self.r.clamp_points();
        self.g.clamp_points();
        self.b.clamp_points();
    }

    pub(crate) fn validate(&self) -> Result<(), CurveInvalid> {
        self.rgb.validate()?;
        self.r.validate()?;
        self.g.validate()?;
        self.b.validate()
    }
}

// ── HSL mixer ────────────────────────────────────────────────────────────────

/// One HSL band's hue/sat/lum offsets, each `-100..=100` (§3.2).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct HslBand {
    /// Hue shift, `-100..=100`.
    pub hue: f32,
    /// Saturation shift, `-100..=100`.
    pub sat: f32,
    /// Luminance shift, `-100..=100`.
    pub lum: f32,
}

impl HslBand {
    pub(crate) fn clamp(&mut self) {
        self.hue = clampf(self.hue, -100.0, 100.0);
        self.sat = clampf(self.sat, -100.0, 100.0);
        self.lum = clampf(self.lum, -100.0, 100.0);
    }
}

/// The 8-band HSL mixer (red, orange, yellow, green, aqua, blue, purple,
/// magenta), §3.2. Neutral = all zero.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct HslTable {
    /// The eight colour bands in fixed order.
    pub bands: [HslBand; 8],
}

impl HslTable {
    pub(crate) fn clamp(&mut self) {
        for b in &mut self.bands {
            b.clamp();
        }
    }
}

// ── colour grading ───────────────────────────────────────────────────────────

/// One colour-grading wheel (§3.2): hue `0..=360`, sat `0..=100`, lum `-100..=100`.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GradeWheel {
    /// Hue angle, `0..=360`.
    pub hue: f32,
    /// Saturation, `0..=100`.
    pub sat: f32,
    /// Luminance, `-100..=100`.
    pub lum: f32,
}

impl GradeWheel {
    pub(crate) fn clamp(&mut self) {
        self.hue = clampf(self.hue, 0.0, 360.0);
        self.sat = clampf(self.sat, 0.0, 100.0);
        self.lum = clampf(self.lum, -100.0, 100.0);
    }
}

/// Colour grading (§3.2): shadow/mid/high/global wheels plus blend + balance.
/// Neutral = all wheels zero, blend/balance zero.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ColorGrade {
    /// Shadows wheel.
    pub shadows: GradeWheel,
    /// Midtones wheel.
    pub midtones: GradeWheel,
    /// Highlights wheel.
    pub highlights: GradeWheel,
    /// Global wheel.
    pub global: GradeWheel,
    /// Shadow/highlight blend, `0..=100`.
    pub blend: f32,
    /// Shadow/highlight balance, `-100..=100`.
    pub balance: f32,
}

impl ColorGrade {
    pub(crate) fn clamp(&mut self) {
        self.shadows.clamp();
        self.midtones.clamp();
        self.highlights.clamp();
        self.global.clamp();
        self.blend = clampf(self.blend, 0.0, 100.0);
        self.balance = clampf(self.balance, -100.0, 100.0);
    }
}

// ── treatment / B&W mix ──────────────────────────────────────────────────────

/// Colour vs black-and-white treatment (§3.2). Neutral = `Color`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Treatment {
    /// Colour rendering (neutral default).
    #[default]
    Color,
    /// Black-and-white rendering (drives the B&W mixer).
    BlackAndWhite,
}

/// The 8-channel black-and-white mixer, each `-100..=100` (§3.2). Neutral = zero.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct BwMix {
    /// The eight per-colour luminance weights.
    pub weights: [f32; 8],
}

impl Default for BwMix {
    fn default() -> Self {
        BwMix { weights: [0.0; 8] }
    }
}

impl BwMix {
    pub(crate) fn clamp(&mut self) {
        for w in &mut self.weights {
            *w = clampf(*w, -100.0, 100.0);
        }
    }
}

// ── presence / detail ────────────────────────────────────────────────────────

/// Presence (§3.2): clarity, texture, dehaze — each `-100..=100`, neutral zero.
/// Its three fields are addressed by individual [`crate::params::ParamId`]s
/// (`Clarity`/`Texture`/`Dehaze`) and clamped there, so it has no aggregate clamp.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Presence {
    /// Clarity, `-100..=100`.
    pub clarity: f32,
    /// Texture, `-100..=100`.
    pub texture: f32,
    /// Dehaze, `-100..=100`.
    pub dehaze: f32,
}

/// Sharpening (§3.2). Neutral = amount zero (renders unmodified).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sharpen {
    /// Amount, `0..=150`, neutral `0`.
    pub amount: f32,
    /// Radius, `0.5..=3.0`, canonical `1.0`.
    pub radius: f32,
    /// Detail, `0..=100`.
    pub detail: f32,
    /// Edge masking, `0..=100`.
    pub masking: f32,
}

impl Default for Sharpen {
    fn default() -> Self {
        Sharpen {
            amount: 0.0,
            radius: 1.0,
            detail: 0.0,
            masking: 0.0,
        }
    }
}

impl Sharpen {
    pub(crate) fn clamp(&mut self) {
        self.amount = clampf(self.amount, 0.0, 150.0);
        self.radius = clampf(self.radius, 0.5, 3.0);
        self.detail = clampf(self.detail, 0.0, 100.0);
        self.masking = clampf(self.masking, 0.0, 100.0);
    }
}

/// Noise reduction (§3.2). Neutral = all zero.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct NoiseReduction {
    /// Luminance NR, `0..=100`.
    pub luma: f32,
    /// Luminance detail, `0..=100`.
    pub luma_detail: f32,
    /// Chroma NR, `0..=100`.
    pub chroma: f32,
    /// Chroma detail, `0..=100`.
    pub chroma_detail: f32,
}

impl NoiseReduction {
    pub(crate) fn clamp(&mut self) {
        self.luma = clampf(self.luma, 0.0, 100.0);
        self.luma_detail = clampf(self.luma_detail, 0.0, 100.0);
        self.chroma = clampf(self.chroma, 0.0, 100.0);
        self.chroma_detail = clampf(self.chroma_detail, 0.0, 100.0);
    }
}

/// The detail block: sharpening + noise reduction (§3.2). Its two sub-blocks are
/// addressed by `ParamId::Sharpen`/`NoiseReduction` and clamped there.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Detail {
    /// Sharpening.
    pub sharpen: Sharpen,
    /// Noise reduction.
    pub nr: NoiseReduction,
}

// ── optics ───────────────────────────────────────────────────────────────────

/// A lens-profile correction (§3.2). Present only when a profile is applied.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LensCorrection {
    /// Lens/profile id.
    pub profile_id: String,
    /// Distortion correction amount, `0..=200`, unity `100`.
    pub distortion: f32,
    /// Vignetting correction amount, `0..=200`, unity `100`.
    pub vignetting: f32,
}

impl Default for LensCorrection {
    fn default() -> Self {
        LensCorrection {
            profile_id: String::new(),
            distortion: 100.0,
            vignetting: 100.0,
        }
    }
}

impl LensCorrection {
    pub(crate) fn clamp(&mut self) {
        self.distortion = clampf(self.distortion, 0.0, 200.0);
        self.vignetting = clampf(self.vignetting, 0.0, 200.0);
    }
}

/// Optics corrections (§3.2): lens profile, chromatic aberration, defringe,
/// manual vignetting correction. Neutral = no profile, CA off, zeros.
///
/// **Delta granularity (deviations A-4):** `ParamId::LensProfile` addresses only
/// `lens_profile`; `ChromaticAberration`/`Defringe`/`VignetteCorr` address `ca`/
/// `defringe`/`vignette_corr` respectively (disjoint slices).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Optics {
    /// Applied lens-profile correction, if any.
    pub lens_profile: Option<LensCorrection>,
    /// Auto chromatic-aberration removal.
    pub ca: bool,
    /// Defringe amount, `0..=100`, neutral `0`.
    pub defringe: f32,
    /// Manual vignetting correction, `-100..=100`, neutral `0`.
    pub vignette_corr: f32,
}

// ── effects ──────────────────────────────────────────────────────────────────

/// Post-crop vignette (§3.2). Neutral = amount zero.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct PostCropVignette {
    /// Amount, `-100..=100`, neutral `0`.
    pub amount: f32,
    /// Midpoint, `0..=100`, canonical `50`.
    pub midpoint: f32,
    /// Roundness, `-100..=100`.
    pub roundness: f32,
    /// Feather, `0..=100`, canonical `50`.
    pub feather: f32,
    /// Highlights, `0..=100`.
    pub highlights: f32,
}

impl Default for PostCropVignette {
    fn default() -> Self {
        PostCropVignette {
            amount: 0.0,
            midpoint: 50.0,
            roundness: 0.0,
            feather: 50.0,
            highlights: 0.0,
        }
    }
}

impl PostCropVignette {
    pub(crate) fn clamp(&mut self) {
        self.amount = clampf(self.amount, -100.0, 100.0);
        self.midpoint = clampf(self.midpoint, 0.0, 100.0);
        self.roundness = clampf(self.roundness, -100.0, 100.0);
        self.feather = clampf(self.feather, 0.0, 100.0);
        self.highlights = clampf(self.highlights, 0.0, 100.0);
    }
}

/// Film grain (§3.2). Neutral = amount zero.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Grain {
    /// Amount, `0..=100`, neutral `0`.
    pub amount: f32,
    /// Size, `0..=100`.
    pub size: f32,
    /// Roughness, `0..=100`.
    pub roughness: f32,
}

impl Grain {
    pub(crate) fn clamp(&mut self) {
        self.amount = clampf(self.amount, 0.0, 100.0);
        self.size = clampf(self.size, 0.0, 100.0);
        self.roughness = clampf(self.roughness, 0.0, 100.0);
    }
}

/// A creative LUT reference + strength (§3.2). Neutral = absent (`None`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CreativeLut {
    /// LUT id / path reference.
    pub id: String,
    /// Strength, `0..=100`, unity `100`.
    pub amount: f32,
}

impl Default for CreativeLut {
    fn default() -> Self {
        CreativeLut {
            id: String::new(),
            amount: 100.0,
        }
    }
}

impl CreativeLut {
    pub(crate) fn clamp(&mut self) {
        self.amount = clampf(self.amount, 0.0, 100.0);
    }
}

/// The effects block (§3.2): post-crop vignette, grain, creative LUT. Its three
/// members are addressed by `ParamId::PostCropVignette`/`Grain`/`CreativeLut` and
/// clamped there.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Effects {
    /// Post-crop vignette.
    pub postcrop_vignette: PostCropVignette,
    /// Film grain.
    pub grain: Grain,
    /// Optional creative LUT.
    pub creative_lut: Option<CreativeLut>,
}

// ── geometry ─────────────────────────────────────────────────────────────────

/// A normalized crop rectangle in `[0,1]` image space (§3.2). Neutral = full
/// frame `(0,0)-(1,1)`.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Crop {
    /// Left edge, `0..=1`.
    pub left: f32,
    /// Top edge, `0..=1`.
    pub top: f32,
    /// Right edge, `0..=1`.
    pub right: f32,
    /// Bottom edge, `0..=1`.
    pub bottom: f32,
}

impl Default for Crop {
    fn default() -> Self {
        Crop {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        }
    }
}

impl Crop {
    pub(crate) fn clamp(&mut self) {
        self.left = clampf(self.left, 0.0, 1.0);
        self.top = clampf(self.top, 0.0, 1.0);
        self.right = clampf(self.right, 0.0, 1.0);
        self.bottom = clampf(self.bottom, 0.0, 1.0);
        // Degenerate/inverted rects fold back to the full frame (values are
        // clamped finite above, so `>=` is well-defined).
        if self.left >= self.right || self.top >= self.bottom {
            *self = Crop::default();
        }
    }
}

/// Flip / mirror (§3.2). Neutral = `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Flip {
    /// No flip (neutral default).
    #[default]
    None,
    /// Mirror horizontally.
    Horizontal,
    /// Mirror vertically.
    Vertical,
    /// Mirror both axes.
    Both,
}

impl Flip {
    /// Wire form for `ParamValue::I32` (deviations A-4).
    pub(crate) fn to_i32(self) -> i32 {
        match self {
            Flip::None => 0,
            Flip::Horizontal => 1,
            Flip::Vertical => 2,
            Flip::Both => 3,
        }
    }

    /// Inverse of [`Flip::to_i32`]; unknown codes fold to `None`.
    pub(crate) fn from_i32(v: i32) -> Flip {
        match v {
            1 => Flip::Horizontal,
            2 => Flip::Vertical,
            3 => Flip::Both,
            _ => Flip::None,
        }
    }
}

/// Upright auto-perspective mode (§3.2). Neutral = `Off`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Upright {
    /// No upright correction (neutral default).
    #[default]
    Off,
    /// Auto balanced correction.
    Auto,
    /// Level (horizon) correction.
    Level,
    /// Vertical correction.
    Vertical,
    /// Full auto correction.
    Full,
}

/// Manual perspective transform (§3.2). Neutral = all zero (no transform); the
/// rendering interpretation of these axes is E05/E10's.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Transform {
    /// Vertical keystone, `-100..=100`.
    pub vertical: f32,
    /// Horizontal keystone, `-100..=100`.
    pub horizontal: f32,
    /// Rotation, `-100..=100`.
    pub rotate: f32,
    /// Aspect, `-100..=100`.
    pub aspect: f32,
    /// Scale offset, `-100..=100` (neutral `0`).
    pub scale: f32,
    /// X offset, `-100..=100`.
    pub x_offset: f32,
    /// Y offset, `-100..=100`.
    pub y_offset: f32,
}

impl Transform {
    pub(crate) fn clamp(&mut self) {
        self.vertical = clampf(self.vertical, -100.0, 100.0);
        self.horizontal = clampf(self.horizontal, -100.0, 100.0);
        self.rotate = clampf(self.rotate, -100.0, 100.0);
        self.aspect = clampf(self.aspect, -100.0, 100.0);
        self.scale = clampf(self.scale, -100.0, 100.0);
        self.x_offset = clampf(self.x_offset, -100.0, 100.0);
        self.y_offset = clampf(self.y_offset, -100.0, 100.0);
    }
}

// ── XMP passthrough ──────────────────────────────────────────────────────────

/// Foreign XMP properties preserved verbatim (§3.1.1 passthrough rule). Populated
/// by Phase D; empty at M1 so it never perturbs the neutral recipe. Storage form
/// (full packet vs pruned subtree) is OQ3; modeled here as a keyed value map.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct XmpPassthrough {
    /// Foreign fields keyed by an opaque `"ns:local"` path.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, CborValue>,
}
