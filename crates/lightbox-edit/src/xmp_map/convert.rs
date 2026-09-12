// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Table-driven `crs:` ↔ Lightbox converters (spec §3.6 / T18).
//!
//! [`FIELD_TABLE`] is the normative in-code mirror of `docs/interop/crs-mapping.md`
//! (each row = one §3.2 global/geometry field with a fidelity class). The
//! converters below are the value transforms the table's `conversion` column
//! names; their unit tests are driven off the table so the doc and the code can
//! never silently drift.

use crate::leaves::{ColorGrade, CurvePoint, ToneCurve, Upright, WbPreset, WhiteBalance};

/// Fidelity class of a mapped field (spec §3.6 `CrsImportReport`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fidelity {
    /// 1:1 numeric / enumerant mapping, no domain loss.
    Exact,
    /// Domain-translated (tone curve quantization, split-toning→grade, enum
    /// remap, differing WB/vignette models).
    Approximate,
    /// Not mapped through `crs:` by design (Lightbox-only field, or content
    /// owned by another epic); carried full-fidelity only via `lb:`.
    Skipped,
}

/// One normative mapping-table row (spec §3.6). Mirrors `crs-mapping.md`.
#[derive(Clone, Copy, Debug)]
pub struct FieldRow {
    /// Lightbox recipe field path (`§3.2`).
    pub lightbox: &'static str,
    /// `crs:` property (empty when [`Fidelity::Skipped`] with no `crs:` peer).
    pub crs: &'static str,
    /// Value domain summary.
    pub domain: &'static str,
    /// Conversion summary (the converter that realizes it).
    pub conversion: &'static str,
    /// Fidelity class.
    pub fidelity: Fidelity,
    /// Notes / rationale (skips carry their "by design" reason here).
    pub notes: &'static str,
}

// ── crs: property names (single source of truth for table + emit + read) ───────

/// `crs:` property-name constants (ACR/LR PV2012 schema).
pub mod crs {
    pub const VERSION: &str = "Version";
    pub const PROCESS_VERSION: &str = "ProcessVersion";
    pub const WHITE_BALANCE: &str = "WhiteBalance";
    pub const TEMPERATURE: &str = "Temperature";
    pub const TINT: &str = "Tint";
    pub const EXPOSURE: &str = "Exposure2012";
    pub const CONTRAST: &str = "Contrast2012";
    pub const HIGHLIGHTS: &str = "Highlights2012";
    pub const SHADOWS: &str = "Shadows2012";
    pub const WHITES: &str = "Whites2012";
    pub const BLACKS: &str = "Blacks2012";
    pub const TEXTURE: &str = "Texture";
    pub const CLARITY: &str = "Clarity2012";
    pub const DEHAZE: &str = "Dehaze";
    pub const VIBRANCE: &str = "Vibrance";
    pub const SATURATION: &str = "Saturation";
    pub const TONE_CURVE: &str = "ToneCurvePV2012";
    pub const TONE_CURVE_RED: &str = "ToneCurvePV2012Red";
    pub const TONE_CURVE_GREEN: &str = "ToneCurvePV2012Green";
    pub const TONE_CURVE_BLUE: &str = "ToneCurvePV2012Blue";
    // Parametric (region-slider) tone curve, E10 task C14.
    pub const PARAMETRIC_SHADOWS: &str = "ParametricShadows";
    pub const PARAMETRIC_DARKS: &str = "ParametricDarks";
    pub const PARAMETRIC_LIGHTS: &str = "ParametricLights";
    pub const PARAMETRIC_HIGHLIGHTS: &str = "ParametricHighlights";
    pub const PARAMETRIC_SHADOW_SPLIT: &str = "ParametricShadowSplit";
    pub const PARAMETRIC_MIDTONE_SPLIT: &str = "ParametricMidtoneSplit";
    pub const PARAMETRIC_HIGHLIGHT_SPLIT: &str = "ParametricHighlightSplit";
    pub const CONVERT_TO_GRAYSCALE: &str = "ConvertToGrayscale";
    pub const SHARPNESS: &str = "Sharpness";
    pub const SHARPEN_RADIUS: &str = "SharpenRadius";
    pub const SHARPEN_DETAIL: &str = "SharpenDetail";
    pub const SHARPEN_EDGE_MASKING: &str = "SharpenEdgeMasking";
    pub const LUMINANCE_SMOOTHING: &str = "LuminanceSmoothing";
    pub const LUMINANCE_NR_DETAIL: &str = "LuminanceNoiseReductionDetail";
    pub const COLOR_NR: &str = "ColorNoiseReduction";
    pub const COLOR_NR_DETAIL: &str = "ColorNoiseReductionDetail";
    pub const AUTO_LATERAL_CA: &str = "AutoLateralCA";
    pub const DEFRINGE_PURPLE: &str = "DefringePurpleAmount";
    pub const VIGNETTE_AMOUNT: &str = "VignetteAmount";
    pub const CAMERA_PROFILE: &str = "CameraProfile";
    pub const CROP_TOP: &str = "CropTop";
    pub const CROP_LEFT: &str = "CropLeft";
    pub const CROP_BOTTOM: &str = "CropBottom";
    pub const CROP_RIGHT: &str = "CropRight";
    pub const HAS_CROP: &str = "HasCrop";
    pub const CROP_ANGLE: &str = "CropAngle";
    pub const PERSPECTIVE_UPRIGHT: &str = "PerspectiveUpright";
    pub const POST_CROP_VIGNETTE_AMOUNT: &str = "PostCropVignetteAmount";
    pub const GRAIN_AMOUNT: &str = "GrainAmount";
    // legacy (PV≤2), read side only, always skipped-with-report (OQ4)
    pub const LEGACY_EXPOSURE: &str = "Exposure";
    pub const LEGACY_BRIGHTNESS: &str = "Brightness";
    pub const LEGACY_CONTRAST: &str = "Contrast";
    // split toning (legacy grading model), approximated into ColorGrade
    pub const SPLIT_SHADOW_HUE: &str = "SplitToningShadowHue";
    pub const SPLIT_SHADOW_SAT: &str = "SplitToningShadowSaturation";
    pub const SPLIT_HIGHLIGHT_HUE: &str = "SplitToningHighlightHue";
    pub const SPLIT_HIGHLIGHT_SAT: &str = "SplitToningHighlightSaturation";
    pub const SPLIT_BALANCE: &str = "SplitToningBalance";
    // structure, skipped by design, preserved verbatim in passthrough (E12/E16)
    pub const MASK_GROUP: &str = "MaskGroupBasedCorrections";
    pub const RETOUCH_AREAS: &str = "RetouchAreas";

    // ── grouped-field names (shared source of truth for emit + read) ───────────
    // HSL 8-band mixer: `<Prefix><BandName>` (BandName ∈ BAND_NAMES).
    pub const HUE_ADJ_PREFIX: &str = "HueAdjustment";
    pub const SAT_ADJ_PREFIX: &str = "SaturationAdjustment";
    pub const LUM_ADJ_PREFIX: &str = "LuminanceAdjustment";
    /// B&W 8-channel grey mixer: `GrayMixer<BandName>`.
    pub const GRAY_MIXER_PREFIX: &str = "GrayMixer";

    // Colour grading (PV2012 modern wheels), flat scalars.
    pub const CG_SHADOW_HUE: &str = "ColorGradeShadowHue";
    pub const CG_SHADOW_SAT: &str = "ColorGradeShadowSat";
    pub const CG_SHADOW_LUM: &str = "ColorGradeShadowLum";
    pub const CG_MIDTONE_HUE: &str = "ColorGradeMidtoneHue";
    pub const CG_MIDTONE_SAT: &str = "ColorGradeMidtoneSat";
    pub const CG_MIDTONE_LUM: &str = "ColorGradeMidtoneLum";
    pub const CG_HIGHLIGHT_HUE: &str = "ColorGradeHighlightHue";
    pub const CG_HIGHLIGHT_SAT: &str = "ColorGradeHighlightSat";
    pub const CG_HIGHLIGHT_LUM: &str = "ColorGradeHighlightLum";
    pub const CG_GLOBAL_HUE: &str = "ColorGradeGlobalHue";
    pub const CG_GLOBAL_SAT: &str = "ColorGradeGlobalSat";
    pub const CG_GLOBAL_LUM: &str = "ColorGradeGlobalLum";
    pub const CG_BLENDING: &str = "ColorGradeBlending";
    pub const CG_BALANCE: &str = "ColorGradeBalance";

    // Manual perspective transform axes.
    pub const PERSP_VERTICAL: &str = "PerspectiveVertical";
    pub const PERSP_HORIZONTAL: &str = "PerspectiveHorizontal";
    pub const PERSP_ROTATE: &str = "PerspectiveRotate";
    pub const PERSP_ASPECT: &str = "PerspectiveAspect";
    pub const PERSP_SCALE: &str = "PerspectiveScale";
    pub const PERSP_X: &str = "PerspectiveX";
    pub const PERSP_Y: &str = "PerspectiveY";

    // Lens profile.
    pub const LENS_PROFILE_ENABLE: &str = "LensProfileEnable";
    pub const LENS_PROFILE_NAME: &str = "LensProfileName";
    pub const LENS_PROFILE_DISTORTION_SCALE: &str = "LensProfileDistortionScale";
    pub const LENS_PROFILE_VIGNETTING_SCALE: &str = "LensProfileVignettingScale";
    /// Adobe's key for the Manual tab's Distortion slider, the signed
    /// by-hand correction. Distinct from the profile *scale* above: that
    /// one says how much of a measured profile to apply, this one IS the
    /// correction. See [`crate::leaves::LensCorrection`].
    pub const LENS_MANUAL_DISTORTION: &str = "LensManualDistortionAmount";

    // Grain / post-crop vignette extras.
    pub const GRAIN_SIZE: &str = "GrainSize";
    pub const GRAIN_FREQUENCY: &str = "GrainFrequency";
    pub const PCV_MIDPOINT: &str = "PostCropVignetteMidpoint";
    pub const PCV_FEATHER: &str = "PostCropVignetteFeather";
    pub const PCV_ROUNDNESS: &str = "PostCropVignetteRoundness";
    pub const PCV_HIGHLIGHTS: &str = "PostCropVignetteHighlightContrast";

    /// Provenance strings we stamp on a compatibility emit (a modern PV2012 head).
    pub const EMIT_VERSION: &str = "15.0";
    pub const EMIT_PROCESS_VERSION: &str = "11.0";
}

/// The eight HSL/grey-mixer band names, in `HslTable`/`BwMix` array order (§3.2).
pub const BAND_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

use Fidelity::{Approximate, Exact, Skipped};

/// The normative field table (spec §3.6 / T18). One row per §3.2 global +
/// geometry field. `crs-mapping.md` is the human-readable projection of this.
#[rustfmt::skip] // one row per line: the table IS the documentation (mirrors crs-mapping.md)
pub const FIELD_TABLE: &[FieldRow] = &[
    FieldRow { lightbox: "base_profile", crs: crs::CAMERA_PROFILE, domain: "profile id ↔ name", conversion: "name hint (identity not resolvable by LR)", fidelity: Approximate, notes: "Lightbox profile identity emitted as a hint; full fidelity via lb:." },
    FieldRow { lightbox: "global.white_balance", crs: crs::WHITE_BALANCE, domain: "mode + temp/tint", conversion: "wb_to_crs / wb_from_crs", fidelity: Approximate, notes: "Custom temp is CCT (K); LR non-raw uses relative temp." },
    FieldRow { lightbox: "global.exposure", crs: crs::EXPOSURE, domain: "stops −5..5", conversion: "identity (stops)", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.contrast", crs: crs::CONTRAST, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.highlights", crs: crs::HIGHLIGHTS, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.shadows", crs: crs::SHADOWS, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.whites", crs: crs::WHITES, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.blacks", crs: crs::BLACKS, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.tone_curve.rgb", crs: crs::TONE_CURVE, domain: "[0,1]² ↔ 0..255 Seq", conversion: "curve_to_crs / curve_from_crs", fidelity: Approximate, notes: "Quantized to the 0–255 integer grid." },
    FieldRow { lightbox: "global.tone_curve.r", crs: crs::TONE_CURVE_RED, domain: "[0,1]² ↔ 0..255 Seq", conversion: "curve_to_crs", fidelity: Approximate, notes: "" },
    FieldRow { lightbox: "global.tone_curve.g", crs: crs::TONE_CURVE_GREEN, domain: "[0,1]² ↔ 0..255 Seq", conversion: "curve_to_crs", fidelity: Approximate, notes: "" },
    FieldRow { lightbox: "global.tone_curve.b", crs: crs::TONE_CURVE_BLUE, domain: "[0,1]² ↔ 0..255 Seq", conversion: "curve_to_crs", fidelity: Approximate, notes: "" },
    FieldRow { lightbox: "global.tone_curve.parametric", crs: "Parametric{Shadows,Darks,Lights,Highlights,ShadowSplit,MidtoneSplit,HighlightSplit}", domain: "sliders −100..100; splits [0,1] ↔ 0..100%", conversion: "identity sliders; parametric_split_to_crs/_from_crs (×100/÷100)", fidelity: Approximate, notes: "E10 task C14: closes the gap the C1-C5 tone-curve landing flagged (deviations C5-1)." },
    FieldRow { lightbox: "global.hsl", crs: "HueAdjustment*/SaturationAdjustment*/LuminanceAdjustment*", domain: "8 bands × 3, −100..100", conversion: "identity per band", fidelity: Exact, notes: "Bands: Red…Magenta." },
    FieldRow { lightbox: "global.color_grade", crs: "ColorGrade{Shadow,Midtone,Highlight,Global}{Hue,Sat,Lum}+Blending+Balance", domain: "hue 0..360, sat/lum, blend, balance", conversion: "identity per wheel; split_toning_to_color_grade on legacy read", fidelity: Approximate, notes: "Legacy SplitToning* is approximated into shadow/highlight wheels." },
    FieldRow { lightbox: "global.treatment", crs: crs::CONVERT_TO_GRAYSCALE, domain: "Color|B&W ↔ bool", conversion: "treatment↔bool", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.bw", crs: "GrayMixer{Red…Magenta}", domain: "8 × −100..100", conversion: "identity per channel", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.vibrance", crs: crs::VIBRANCE, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.saturation", crs: crs::SATURATION, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.presence.clarity", crs: crs::CLARITY, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.presence.texture", crs: crs::TEXTURE, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.presence.dehaze", crs: crs::DEHAZE, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.sharpen.amount", crs: crs::SHARPNESS, domain: "0..150", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.sharpen.radius", crs: crs::SHARPEN_RADIUS, domain: "0.5..3.0", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.sharpen.detail", crs: crs::SHARPEN_DETAIL, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.sharpen.masking", crs: crs::SHARPEN_EDGE_MASKING, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.nr.luma", crs: crs::LUMINANCE_SMOOTHING, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.nr.luma_detail", crs: crs::LUMINANCE_NR_DETAIL, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.nr.chroma", crs: crs::COLOR_NR, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.detail.nr.chroma_detail", crs: crs::COLOR_NR_DETAIL, domain: "0..100", conversion: "identity", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.optics.lens_profile", crs: "LensProfileEnable/Name/DistortionScale/VignettingScale", domain: "profile + scales", conversion: "enable+scales; identity is Lightbox's", fidelity: Approximate, notes: "Profile identity is Lightbox's; not resolvable by LR. The profile SCALES are amounts and are emitted as amounts; the by-hand correction is the separate manual_distortion row." },
    FieldRow { lightbox: "global.optics.lens_profile.manual_distortion", crs: crs::LENS_MANUAL_DISTORTION, domain: "−100..100", conversion: "identity", fidelity: Exact, notes: "Adobe's Manual-tab Distortion; never folded into LensProfileDistortionScale (that key means 'apply N% of a profile')." },
    FieldRow { lightbox: "global.optics.ca", crs: crs::AUTO_LATERAL_CA, domain: "bool", conversion: "bool", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "global.optics.defringe", crs: crs::DEFRINGE_PURPLE, domain: "0..100 (single) ↔ purple/green", conversion: "single→purple", fidelity: Approximate, notes: "LR splits purple/green; Lightbox has one amount at M1 (A-4)." },
    FieldRow { lightbox: "global.optics.vignette_corr", crs: crs::VIGNETTE_AMOUNT, domain: "−100..100", conversion: "identity", fidelity: Approximate, notes: "Manual lens-vignette model differs slightly." },
    FieldRow { lightbox: "global.effects.postcrop_vignette", crs: crs::POST_CROP_VIGNETTE_AMOUNT, domain: "amount + style", conversion: "amount identity; style enum", fidelity: Approximate, notes: "Style enum not fully modeled at M1." },
    FieldRow { lightbox: "global.effects.grain", crs: crs::GRAIN_AMOUNT, domain: "amount/size/frequency", conversion: "amount identity; roughness↔frequency", fidelity: Approximate, notes: "roughness↔GrainFrequency is approximate." },
    FieldRow { lightbox: "global.effects.creative_lut", crs: "", domain: "Lightbox LUT ref", conversion: "—", fidelity: Skipped, notes: "No crs: peer; lb: full-fidelity only." },
    FieldRow { lightbox: "geometry.crop", crs: "CropTop/Left/Bottom/Right + HasCrop", domain: "[0,1] edges", conversion: "identity (normalized)", fidelity: Exact, notes: "" },
    FieldRow { lightbox: "geometry.angle", crs: crs::CROP_ANGLE, domain: "−45..45 deg", conversion: "identity (sign per LR)", fidelity: Approximate, notes: "LR CropAngle sign/convention differs." },
    FieldRow { lightbox: "geometry.flip", crs: "", domain: "None|H|V|Both", conversion: "—", fidelity: Skipped, notes: "LR encodes flip via crop orientation flags; lb: only at M1." },
    FieldRow { lightbox: "geometry.upright", crs: crs::PERSPECTIVE_UPRIGHT, domain: "Off/Auto/Level/Vertical/Full ↔ 0..5", conversion: "upright↔int", fidelity: Approximate, notes: "Enum mode remap." },
    FieldRow { lightbox: "geometry.transform", crs: "Perspective{Vertical,Horizontal,Rotate,Aspect,Scale,X,Y}", domain: "7 × −100..100", conversion: "identity per axis", fidelity: Approximate, notes: "Manual-transform model differs." },
    FieldRow { lightbox: "masks", crs: crs::MASK_GROUP, domain: "id list ↔ mask groups", conversion: "—", fidelity: Skipped, notes: "By design (E12): ids-only; crs: masks preserved verbatim in passthrough." },
    FieldRow { lightbox: "retouch", crs: crs::RETOUCH_AREAS, domain: "id list ↔ retouch areas", conversion: "—", fidelity: Skipped, notes: "By design (E12/E14): ids-only; crs: retouch preserved verbatim in passthrough." },
    FieldRow { lightbox: "(legacy PV≤2 tone)", crs: "Exposure/Brightness/Contrast (no 2012)", domain: "PV2003/2010 tone", conversion: "—", fidelity: Skipped, notes: "By design (OQ4): 2010→2012 conversion unpublished; skipped-with-report, E16 revisits." },
];

// ── converters ────────────────────────────────────────────────────────────────

/// Exposure in stops maps 1:1 onto `crs:Exposure2012` (both are stops).
pub fn exposure_to_crs(stops: f32) -> f32 {
    stops
}

/// Inverse of [`exposure_to_crs`].
pub fn exposure_from_crs(crs: f32) -> f32 {
    crs
}

/// A ±100 slider maps 1:1 onto its `crs:` peer.
pub fn slider_pm100_to_crs(v: f32) -> f32 {
    v
}

/// Classification of a `crs:ProcessVersion` string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PvClass {
    /// PV2012-family (crs:ProcessVersion ≥ 6.7): tone params map.
    Modern,
    /// PV2003/PV2010 (< 6.7): legacy tone params are skipped-with-report (OQ4).
    Legacy,
    /// No/unparseable version: treated as modern (safe default), noted.
    Unknown,
}

/// Classify a `crs:ProcessVersion` value. LR writes numeric versions
/// (`5.0`=PV2003, `5.7`=PV2010, `6.7`=PV2012, later `11.0`/`15.0` = PV2012-family).
pub fn pv_classify(process_version: Option<&str>) -> PvClass {
    match process_version {
        None => PvClass::Unknown,
        Some(s) => {
            let head = s.split_whitespace().next().unwrap_or("");
            match head.parse::<f64>() {
                Ok(v) if v < 6.7 => PvClass::Legacy,
                Ok(_) => PvClass::Modern,
                Err(_) => PvClass::Unknown,
            }
        }
    }
}

/// The crs `WhiteBalance` mode string + optional temp/tint for a Lightbox WB.
pub fn wb_to_crs(wb: WhiteBalance) -> (String, Option<f32>, Option<f32>) {
    match wb {
        WhiteBalance::AsShot => ("As Shot".to_string(), None, None),
        WhiteBalance::Auto => ("Auto".to_string(), None, None),
        WhiteBalance::Preset(p) => (wb_preset_name(p).to_string(), None, None),
        WhiteBalance::Custom { temp_k, tint } => ("Custom".to_string(), Some(temp_k), Some(tint)),
    }
}

/// Inverse of [`wb_to_crs`]. Unknown mode strings fall back to `AsShot`.
pub fn wb_from_crs(mode: &str, temp: Option<f32>, tint: Option<f32>) -> WhiteBalance {
    match mode.trim() {
        "As Shot" | "AsShot" => WhiteBalance::AsShot,
        "Auto" => WhiteBalance::Auto,
        "Custom" => WhiteBalance::Custom {
            temp_k: temp.unwrap_or(5500.0),
            tint: tint.unwrap_or(0.0),
        },
        other => wb_preset_from_name(other)
            .map(WhiteBalance::Preset)
            .unwrap_or(WhiteBalance::AsShot),
    }
}

fn wb_preset_name(p: WbPreset) -> &'static str {
    match p {
        WbPreset::Daylight => "Daylight",
        WbPreset::Cloudy => "Cloudy",
        WbPreset::Shade => "Shade",
        WbPreset::Tungsten => "Tungsten",
        WbPreset::Fluorescent => "Fluorescent",
        WbPreset::Flash => "Flash",
    }
}

fn wb_preset_from_name(name: &str) -> Option<WbPreset> {
    Some(match name {
        "Daylight" => WbPreset::Daylight,
        "Cloudy" => WbPreset::Cloudy,
        "Shade" => WbPreset::Shade,
        "Tungsten" => WbPreset::Tungsten,
        "Fluorescent" => WbPreset::Fluorescent,
        "Flash" => WbPreset::Flash,
        _ => return None,
    })
}

/// A parametric-curve split point (Lightbox `0..=1` fraction, spec §4.3
/// `ParametricCurve::splits`) → its `crs:Parametric*Split` peer (LR's
/// `0..=100` percent), E10 task C14.
pub fn parametric_split_to_crs(frac: f32) -> f32 {
    frac * 100.0
}

/// Inverse of [`parametric_split_to_crs`].
pub fn parametric_split_from_crs(pct: f32) -> f32 {
    pct / 100.0
}

/// A tone curve → `crs:` `rdf:Seq` of `"x, y"` on the 0-255 integer grid.
pub fn curve_to_crs(curve: &ToneCurve) -> Vec<String> {
    curve
        .points
        .iter()
        .map(|p| {
            let x = (p.x.clamp(0.0, 1.0) * 255.0).round() as i32;
            let y = (p.y.clamp(0.0, 1.0) * 255.0).round() as i32;
            format!("{x}, {y}")
        })
        .collect()
}

/// Inverse of [`curve_to_crs`]. Malformed items are skipped (fuzz-safe); an empty
/// result yields the identity curve.
pub fn curve_from_crs(items: &[String]) -> ToneCurve {
    let mut points: Vec<CurvePoint> = Vec::new();
    for it in items {
        let mut parts = it.split(',');
        let (Some(xs), Some(ys)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(x), Ok(y)) = (xs.trim().parse::<f32>(), ys.trim().parse::<f32>()) else {
            continue;
        };
        points.push(CurvePoint {
            x: (x / 255.0).clamp(0.0, 1.0),
            y: (y / 255.0).clamp(0.0, 1.0),
        });
    }
    // Enforce strictly-increasing x (drop duplicates/regressions, fuzz-safe).
    points.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    let mut dedup: Vec<CurvePoint> = Vec::with_capacity(points.len());
    for p in points {
        if dedup.last().map(|q| p.x > q.x).unwrap_or(true) {
            dedup.push(p);
        }
    }
    if dedup.len() < 2 {
        return ToneCurve::default();
    }
    dedup.truncate(crate::leaves::MAX_CURVE_POINTS);
    ToneCurve { points: dedup }
}

/// Lightbox [`Upright`] mode → `crs:PerspectiveUpright` integer code (LR's
/// enum). Approximate: LR's `5 = Guided` has no Lightbox peer.
pub fn upright_to_crs(u: Upright) -> i64 {
    match u {
        Upright::Off => 0,
        Upright::Auto => 1,
        Upright::Level => 2,
        Upright::Vertical => 3,
        Upright::Full => 4,
    }
}

/// Inverse of [`upright_to_crs`]. Unknown codes (incl. LR's `5 = Guided`) fold
/// to [`Upright::Off`] (fuzz-safe).
pub fn upright_from_crs(code: i64) -> Upright {
    match code {
        1 => Upright::Auto,
        2 => Upright::Level,
        3 => Upright::Vertical,
        4 => Upright::Full,
        _ => Upright::Off,
    }
}

/// Approximate legacy `crs:SplitToning*` into a Lightbox [`ColorGrade`]
/// (shadows/highlights wheels + balance). Midtone/global wheels stay neutral.
pub fn split_toning_to_color_grade(
    shadow_hue: f32,
    shadow_sat: f32,
    highlight_hue: f32,
    highlight_sat: f32,
    balance: f32,
) -> ColorGrade {
    let mut cg = ColorGrade::default();
    cg.shadows.hue = shadow_hue.clamp(0.0, 360.0);
    cg.shadows.sat = shadow_sat.clamp(0.0, 100.0);
    cg.highlights.hue = highlight_hue.clamp(0.0, 360.0);
    cg.highlights.sat = highlight_sat.clamp(0.0, 100.0);
    cg.balance = balance.clamp(-100.0, 100.0);
    cg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table completeness (T18 AC): every §3.2 global + geometry field, plus the
    /// masks/retouch/legacy skip rows, is present with a fidelity class.
    #[test]
    fn field_table_covers_every_field() {
        // A curated checklist of the §3.2 field paths the table MUST classify.
        let required = [
            "base_profile",
            "global.white_balance",
            "global.exposure",
            "global.contrast",
            "global.highlights",
            "global.shadows",
            "global.whites",
            "global.blacks",
            "global.tone_curve.rgb",
            "global.tone_curve.r",
            "global.tone_curve.g",
            "global.tone_curve.b",
            "global.tone_curve.parametric",
            "global.hsl",
            "global.color_grade",
            "global.treatment",
            "global.bw",
            "global.vibrance",
            "global.saturation",
            "global.presence.clarity",
            "global.presence.texture",
            "global.presence.dehaze",
            "global.detail.sharpen.amount",
            "global.detail.sharpen.radius",
            "global.detail.sharpen.detail",
            "global.detail.sharpen.masking",
            "global.detail.nr.luma",
            "global.detail.nr.luma_detail",
            "global.detail.nr.chroma",
            "global.detail.nr.chroma_detail",
            "global.optics.lens_profile",
            "global.optics.ca",
            "global.optics.defringe",
            "global.optics.vignette_corr",
            "global.effects.postcrop_vignette",
            "global.effects.grain",
            "global.effects.creative_lut",
            "geometry.crop",
            "geometry.angle",
            "geometry.flip",
            "geometry.upright",
            "geometry.transform",
            "masks",
            "retouch",
        ];
        for path in required {
            assert!(
                FIELD_TABLE.iter().any(|r| r.lightbox == path),
                "mapping table is missing §3.2 field `{path}`"
            );
        }
        // Masks/retouch/creative_lut/flip/legacy are Skipped-by-design.
        for path in [
            "masks",
            "retouch",
            "global.effects.creative_lut",
            "geometry.flip",
        ] {
            let row = FIELD_TABLE.iter().find(|r| r.lightbox == path).unwrap();
            assert_eq!(row.fidelity, Skipped, "`{path}` must be skipped-by-design");
            assert!(
                !row.notes.is_empty(),
                "skip `{path}` must carry a rationale"
            );
        }
    }

    /// Converter identities generated off the table's "identity" Exact rows.
    #[test]
    fn exact_scalar_converters_are_identity() {
        for v in [-100.0f32, -1.0, 0.0, 1.0, 100.0] {
            assert_eq!(slider_pm100_to_crs(v), v);
        }
        for s in [-5.0f32, 0.0, 2.5, 5.0] {
            assert_eq!(exposure_from_crs(exposure_to_crs(s)), s);
        }
    }

    /// C14: parametric-curve split converters round-trip exactly (a pure
    /// unit scale, no quantization loss unlike the point-curve grid).
    #[test]
    fn parametric_split_converters_round_trip() {
        for frac in [0.0f32, 0.05, 0.25, 0.5, 0.75, 0.95, 1.0] {
            let pct = parametric_split_to_crs(frac);
            assert!((0.0..=100.0).contains(&pct), "pct={pct}");
            assert!((parametric_split_from_crs(pct) - frac).abs() < 1e-6);
        }
        assert_eq!(parametric_split_to_crs(0.25), 25.0);
        assert_eq!(parametric_split_from_crs(75.0), 0.75);
    }

    #[test]
    fn tone_curve_quantizes_and_recovers() {
        let curve = ToneCurve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 0.5, y: 0.6 },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
        let crs = curve_to_crs(&curve);
        assert_eq!(crs, vec!["0, 0", "128, 153", "255, 255"]);
        let back = curve_from_crs(&crs);
        assert_eq!(back.points.len(), 3);
        // Within one 0-255 quantization step.
        assert!((back.points[1].x - 0.5).abs() < 1.0 / 255.0 + 1e-6);
        assert!((back.points[1].y - 0.6).abs() < 1.0 / 255.0 + 1e-3);
    }

    #[test]
    fn tone_curve_from_garbage_is_safe() {
        let junk = vec![
            "not a point".to_string(),
            "5".to_string(),
            "300, -10".to_string(),
            "10, 20".to_string(),
        ];
        let c = curve_from_crs(&junk);
        // strictly-increasing x, clamped to [0,1], no panic.
        for w in c.points.windows(2) {
            assert!(w[0].x < w[1].x);
        }
        assert!(c.validate().is_ok());
    }

    #[test]
    fn wb_modes_round_trip() {
        for wb in [
            WhiteBalance::AsShot,
            WhiteBalance::Auto,
            WhiteBalance::Preset(WbPreset::Shade),
            WhiteBalance::Custom {
                temp_k: 5200.0,
                tint: 8.0,
            },
        ] {
            let (mode, t, ti) = wb_to_crs(wb);
            assert_eq!(wb_from_crs(&mode, t, ti), wb);
        }
    }

    #[test]
    fn pv_classification() {
        assert_eq!(pv_classify(Some("5.0")), PvClass::Legacy);
        assert_eq!(pv_classify(Some("5.7")), PvClass::Legacy);
        assert_eq!(pv_classify(Some("6.7")), PvClass::Modern);
        assert_eq!(pv_classify(Some("11.0")), PvClass::Modern);
        assert_eq!(pv_classify(Some("15.0")), PvClass::Modern);
        assert_eq!(pv_classify(None), PvClass::Unknown);
        assert_eq!(pv_classify(Some("garbage")), PvClass::Unknown);
    }

    #[test]
    fn split_toning_maps_into_grade() {
        let cg = split_toning_to_color_grade(210.0, 40.0, 45.0, 30.0, -20.0);
        assert_eq!(cg.shadows.hue, 210.0);
        assert_eq!(cg.shadows.sat, 40.0);
        assert_eq!(cg.highlights.hue, 45.0);
        assert_eq!(cg.highlights.sat, 30.0);
        assert_eq!(cg.balance, -20.0);
        assert_eq!(cg.midtones, Default::default());
    }
}
