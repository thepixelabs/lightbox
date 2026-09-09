// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`Recipe::read_xmp`] (T20) and [`Recipe::from_lr_crs`] (T21), reading a recipe
//! back out of an [`XmpDoc`].
//!
//! - [`Recipe::read_xmp`] prefers the authoritative **`lb:`** payload (a
//!   struct-equal reconstruction) and falls through to the **`crs:`** mechanism
//!   for a foreign, Lightroom-authored doc.
//! - [`Recipe::from_lr_crs`] is the `crs:` read **mechanism** (Risk 9): **total**.
//!   It never panics or errors on fuzzed `crs:` soup. Every `crs:` key it sees
//!   lands in exactly one bucket of the [`CrsImportReport`]
//!   (`mapped ∪ approximate ∪ skipped`, the §6 exhaustive-partition contract);
//!   unmapped `crs:` keys and all foreign namespaces are preserved verbatim in
//!   `xmp_passthrough`. The sidecar-honoring OPEN flow and legacy-PV coverage are
//!   E16 (M4).

use std::collections::BTreeSet;

use lightbox_meta::xmp::{ns, ArrayKind, XmpDoc, XmpValue};
use lightbox_types::PV_M0;

use crate::leaves::{clampf, LensCorrection, Treatment};
use crate::recipe::Recipe;

use super::convert::{self, crs, BAND_NAMES};
use super::{
    encode_array, encode_scalar, from_hex, is_foreign_ns, lb, pt_key, CrsImport, CrsImportReport,
    FieldFidelity, RecipeFromXmp, XmpMapError, XmpSource,
};

// ── crs: typed getters (a crs: property is stringly-typed under the hood) ───────

fn gf(doc: &XmpDoc, local: &str) -> Option<f64> {
    doc.get(ns::CRS, local).and_then(|v| v.as_f64())
}
fn gb(doc: &XmpDoc, local: &str) -> Option<bool> {
    doc.get(ns::CRS, local).and_then(|v| v.as_bool())
}
fn gs(doc: &XmpDoc, local: &str) -> Option<String> {
    doc.get(ns::CRS, local).map(|v| v.as_str())
}
fn ga(doc: &XmpDoc, local: &str) -> Option<Vec<String>> {
    doc.get_array(ns::CRS, local)
        .map(|xs| xs.iter().map(XmpValue::as_str).collect())
}

fn field_fidelity(lightbox: &str, crs_local: &str) -> FieldFidelity {
    FieldFidelity {
        lightbox: lightbox.to_string(),
        crs: format!("crs:{crs_local}"),
    }
}

impl Recipe {
    /// Read a recipe back from a sidecar/embedded [`XmpDoc`] (spec §3.6 / T20).
    ///
    /// `lb:` is primary (authoritative, struct-equal). A doc with no `lb:` payload
    /// is treated as foreign and routed to [`Recipe::from_lr_crs`]; the
    /// [`RecipeFromXmp::report`] then carries the per-field fidelity.
    pub fn read_xmp(
        doc: &XmpDoc,
        probe: &lightbox_decode::AssetProbe,
    ) -> Result<RecipeFromXmp, XmpMapError> {
        if let Some(hex) = doc.get(ns::LB, lb::RECIPE_CBOR) {
            let bytes = from_hex(&hex.as_str())
                .ok_or_else(|| XmpMapError::Malformed("lb:RecipeCbor is not valid hex".into()))?;
            let recipe = Recipe::from_cbor(&bytes)
                .map_err(|e| XmpMapError::Malformed(format!("lb:RecipeCbor: {e}")))?
                .into_recipe()
                .ok_or_else(|| {
                    XmpMapError::Malformed("lb:RecipeCbor is a newer schema (read-only)".into())
                })?;
            return Ok(RecipeFromXmp {
                recipe,
                source: XmpSource::Lb,
                report: CrsImportReport::default(),
            });
        }
        let CrsImport { recipe, report } = Recipe::from_lr_crs(doc, probe);
        Ok(RecipeFromXmp {
            recipe,
            source: XmpSource::Crs,
            report,
        })
    }

    /// The `crs:` read mechanism (spec §3.6 / T21). Total; see the module docs for
    /// the exhaustive-partition and passthrough guarantees.
    pub fn from_lr_crs(doc: &XmpDoc, probe: &lightbox_decode::AssetProbe) -> CrsImport {
        let mut recipe = Recipe::default_for(PV_M0, probe);
        let mut report = CrsImportReport {
            source_pv: gs(doc, crs::PROCESS_VERSION),
            ..CrsImportReport::default()
        };
        // Every crs: key we translate; the rest go to skipped + passthrough.
        let mut consumed: BTreeSet<String> = BTreeSet::new();

        // A scalar crs: field → a clamped recipe scalar + a fidelity row.
        macro_rules! sc {
            ($local:expr, $target:expr, $lo:expr, $hi:expr, $lb_path:expr, $bucket:ident) => {
                if let Some(v) = gf(doc, $local) {
                    $target = clampf(v as f32, $lo, $hi);
                    report.$bucket.push(field_fidelity($lb_path, $local));
                    consumed.insert(($local).to_string());
                }
            };
        }

        // White balance (mode + optional temp/tint).
        if let Some(mode) = gs(doc, crs::WHITE_BALANCE) {
            let temp = gf(doc, crs::TEMPERATURE).map(|v| v as f32);
            let tint = gf(doc, crs::TINT).map(|v| v as f32);
            recipe.global.white_balance = convert::wb_from_crs(&mode, temp, tint);
            report
                .approximate
                .push(field_fidelity("global.white_balance", crs::WHITE_BALANCE));
            consumed.insert(crs::WHITE_BALANCE.to_string());
            if temp.is_some() {
                consumed.insert(crs::TEMPERATURE.to_string());
                report.approximate.push(field_fidelity(
                    "global.white_balance.temp_k",
                    crs::TEMPERATURE,
                ));
            }
            if tint.is_some() {
                consumed.insert(crs::TINT.to_string());
                report
                    .approximate
                    .push(field_fidelity("global.white_balance.tint", crs::TINT));
            }
        }

        // Basic tone + presence (identity ±100 sliders / stops).
        sc!(
            crs::EXPOSURE,
            recipe.global.exposure,
            -5.0,
            5.0,
            "global.exposure",
            mapped
        );
        sc!(
            crs::CONTRAST,
            recipe.global.contrast,
            -100.0,
            100.0,
            "global.contrast",
            mapped
        );
        sc!(
            crs::HIGHLIGHTS,
            recipe.global.highlights,
            -100.0,
            100.0,
            "global.highlights",
            mapped
        );
        sc!(
            crs::SHADOWS,
            recipe.global.shadows,
            -100.0,
            100.0,
            "global.shadows",
            mapped
        );
        sc!(
            crs::WHITES,
            recipe.global.whites,
            -100.0,
            100.0,
            "global.whites",
            mapped
        );
        sc!(
            crs::BLACKS,
            recipe.global.blacks,
            -100.0,
            100.0,
            "global.blacks",
            mapped
        );
        sc!(
            crs::TEXTURE,
            recipe.global.presence.texture,
            -100.0,
            100.0,
            "global.presence.texture",
            mapped
        );
        sc!(
            crs::CLARITY,
            recipe.global.presence.clarity,
            -100.0,
            100.0,
            "global.presence.clarity",
            mapped
        );
        sc!(
            crs::DEHAZE,
            recipe.global.presence.dehaze,
            -100.0,
            100.0,
            "global.presence.dehaze",
            mapped
        );
        sc!(
            crs::VIBRANCE,
            recipe.global.vibrance,
            -100.0,
            100.0,
            "global.vibrance",
            mapped
        );
        sc!(
            crs::SATURATION,
            recipe.global.saturation,
            -100.0,
            100.0,
            "global.saturation",
            mapped
        );

        // Tone curves (quantized 0-255 grid → [0,1]).
        for (local, target) in [
            (crs::TONE_CURVE, 0usize),
            (crs::TONE_CURVE_RED, 1),
            (crs::TONE_CURVE_GREEN, 2),
            (crs::TONE_CURVE_BLUE, 3),
        ] {
            if let Some(items) = ga(doc, local) {
                let curve = convert::curve_from_crs(&items);
                match target {
                    0 => recipe.global.tone_curve.rgb = curve,
                    1 => recipe.global.tone_curve.r = curve,
                    2 => recipe.global.tone_curve.g = curve,
                    _ => recipe.global.tone_curve.b = curve,
                }
                report
                    .approximate
                    .push(field_fidelity("global.tone_curve", local));
                consumed.insert(local.to_string());
            }
        }

        // Parametric (region-slider) tone curve, E10 task C14 (closes the
        // gap the C1-C5 tone-curve landing flagged as deferred, deviations
        // C5-1: the point-curve landing had no `crs:` row for this field).
        sc!(
            crs::PARAMETRIC_HIGHLIGHTS,
            recipe.global.tone_curve.parametric.highlights,
            -100.0,
            100.0,
            "global.tone_curve.parametric.highlights",
            approximate
        );
        sc!(
            crs::PARAMETRIC_LIGHTS,
            recipe.global.tone_curve.parametric.lights,
            -100.0,
            100.0,
            "global.tone_curve.parametric.lights",
            approximate
        );
        sc!(
            crs::PARAMETRIC_DARKS,
            recipe.global.tone_curve.parametric.darks,
            -100.0,
            100.0,
            "global.tone_curve.parametric.darks",
            approximate
        );
        sc!(
            crs::PARAMETRIC_SHADOWS,
            recipe.global.tone_curve.parametric.shadows,
            -100.0,
            100.0,
            "global.tone_curve.parametric.shadows",
            approximate
        );
        if let Some(v) = gf(doc, crs::PARAMETRIC_SHADOW_SPLIT) {
            recipe.global.tone_curve.parametric.splits[0] =
                clampf(convert::parametric_split_from_crs(v as f32), 0.0, 1.0);
            report.approximate.push(field_fidelity(
                "global.tone_curve.parametric.splits",
                crs::PARAMETRIC_SHADOW_SPLIT,
            ));
            consumed.insert(crs::PARAMETRIC_SHADOW_SPLIT.to_string());
        }
        if let Some(v) = gf(doc, crs::PARAMETRIC_MIDTONE_SPLIT) {
            recipe.global.tone_curve.parametric.splits[1] =
                clampf(convert::parametric_split_from_crs(v as f32), 0.0, 1.0);
            report.approximate.push(field_fidelity(
                "global.tone_curve.parametric.splits",
                crs::PARAMETRIC_MIDTONE_SPLIT,
            ));
            consumed.insert(crs::PARAMETRIC_MIDTONE_SPLIT.to_string());
        }
        if let Some(v) = gf(doc, crs::PARAMETRIC_HIGHLIGHT_SPLIT) {
            recipe.global.tone_curve.parametric.splits[2] =
                clampf(convert::parametric_split_from_crs(v as f32), 0.0, 1.0);
            report.approximate.push(field_fidelity(
                "global.tone_curve.parametric.splits",
                crs::PARAMETRIC_HIGHLIGHT_SPLIT,
            ));
            consumed.insert(crs::PARAMETRIC_HIGHLIGHT_SPLIT.to_string());
        }

        // HSL mixer, 8 bands × 3.
        for (i, name) in BAND_NAMES.iter().enumerate() {
            let hue = format!("{}{name}", crs::HUE_ADJ_PREFIX);
            let sat = format!("{}{name}", crs::SAT_ADJ_PREFIX);
            let lum = format!("{}{name}", crs::LUM_ADJ_PREFIX);
            if let Some(v) = gf(doc, &hue) {
                recipe.global.hsl.bands[i].hue = clampf(v as f32, -100.0, 100.0);
                report.mapped.push(field_fidelity("global.hsl.hue", &hue));
                consumed.insert(hue);
            }
            if let Some(v) = gf(doc, &sat) {
                recipe.global.hsl.bands[i].sat = clampf(v as f32, -100.0, 100.0);
                report.mapped.push(field_fidelity("global.hsl.sat", &sat));
                consumed.insert(sat);
            }
            if let Some(v) = gf(doc, &lum) {
                recipe.global.hsl.bands[i].lum = clampf(v as f32, -100.0, 100.0);
                report.mapped.push(field_fidelity("global.hsl.lum", &lum));
                consumed.insert(lum);
            }
        }

        // Treatment + B&W grey mixer.
        if let Some(b) = gb(doc, crs::CONVERT_TO_GRAYSCALE) {
            recipe.global.treatment = if b {
                Treatment::BlackAndWhite
            } else {
                Treatment::Color
            };
            report.mapped.push(field_fidelity(
                "global.treatment",
                crs::CONVERT_TO_GRAYSCALE,
            ));
            consumed.insert(crs::CONVERT_TO_GRAYSCALE.to_string());
        }
        for (i, name) in BAND_NAMES.iter().enumerate() {
            let local = format!("{}{name}", crs::GRAY_MIXER_PREFIX);
            if let Some(v) = gf(doc, &local) {
                recipe.global.bw.weights[i] = clampf(v as f32, -100.0, 100.0);
                report.mapped.push(field_fidelity("global.bw", &local));
                consumed.insert(local);
            }
        }

        // Legacy split-toning FIRST so modern ColorGrade* below overrides it.
        let cg = &mut recipe.global.color_grade;
        sc!(
            crs::SPLIT_SHADOW_HUE,
            cg.shadows.hue,
            0.0,
            360.0,
            "global.color_grade.shadows.hue",
            approximate
        );
        sc!(
            crs::SPLIT_SHADOW_SAT,
            cg.shadows.sat,
            0.0,
            100.0,
            "global.color_grade.shadows.sat",
            approximate
        );
        sc!(
            crs::SPLIT_HIGHLIGHT_HUE,
            cg.highlights.hue,
            0.0,
            360.0,
            "global.color_grade.highlights.hue",
            approximate
        );
        sc!(
            crs::SPLIT_HIGHLIGHT_SAT,
            cg.highlights.sat,
            0.0,
            100.0,
            "global.color_grade.highlights.sat",
            approximate
        );
        sc!(
            crs::SPLIT_BALANCE,
            cg.balance,
            -100.0,
            100.0,
            "global.color_grade.balance",
            approximate
        );
        // Modern colour grading wheels.
        sc!(
            crs::CG_SHADOW_HUE,
            cg.shadows.hue,
            0.0,
            360.0,
            "global.color_grade.shadows.hue",
            approximate
        );
        sc!(
            crs::CG_SHADOW_SAT,
            cg.shadows.sat,
            0.0,
            100.0,
            "global.color_grade.shadows.sat",
            approximate
        );
        sc!(
            crs::CG_SHADOW_LUM,
            cg.shadows.lum,
            -100.0,
            100.0,
            "global.color_grade.shadows.lum",
            approximate
        );
        sc!(
            crs::CG_MIDTONE_HUE,
            cg.midtones.hue,
            0.0,
            360.0,
            "global.color_grade.midtones.hue",
            approximate
        );
        sc!(
            crs::CG_MIDTONE_SAT,
            cg.midtones.sat,
            0.0,
            100.0,
            "global.color_grade.midtones.sat",
            approximate
        );
        sc!(
            crs::CG_MIDTONE_LUM,
            cg.midtones.lum,
            -100.0,
            100.0,
            "global.color_grade.midtones.lum",
            approximate
        );
        sc!(
            crs::CG_HIGHLIGHT_HUE,
            cg.highlights.hue,
            0.0,
            360.0,
            "global.color_grade.highlights.hue",
            approximate
        );
        sc!(
            crs::CG_HIGHLIGHT_SAT,
            cg.highlights.sat,
            0.0,
            100.0,
            "global.color_grade.highlights.sat",
            approximate
        );
        sc!(
            crs::CG_HIGHLIGHT_LUM,
            cg.highlights.lum,
            -100.0,
            100.0,
            "global.color_grade.highlights.lum",
            approximate
        );
        sc!(
            crs::CG_GLOBAL_HUE,
            cg.global.hue,
            0.0,
            360.0,
            "global.color_grade.global.hue",
            approximate
        );
        sc!(
            crs::CG_GLOBAL_SAT,
            cg.global.sat,
            0.0,
            100.0,
            "global.color_grade.global.sat",
            approximate
        );
        sc!(
            crs::CG_GLOBAL_LUM,
            cg.global.lum,
            -100.0,
            100.0,
            "global.color_grade.global.lum",
            approximate
        );
        sc!(
            crs::CG_BLENDING,
            cg.blend,
            0.0,
            100.0,
            "global.color_grade.blend",
            approximate
        );
        sc!(
            crs::CG_BALANCE,
            cg.balance,
            -100.0,
            100.0,
            "global.color_grade.balance",
            approximate
        );

        // Detail: sharpen + noise reduction.
        sc!(
            crs::SHARPNESS,
            recipe.global.detail.sharpen.amount,
            0.0,
            150.0,
            "global.detail.sharpen.amount",
            mapped
        );
        sc!(
            crs::SHARPEN_RADIUS,
            recipe.global.detail.sharpen.radius,
            0.5,
            3.0,
            "global.detail.sharpen.radius",
            mapped
        );
        sc!(
            crs::SHARPEN_DETAIL,
            recipe.global.detail.sharpen.detail,
            0.0,
            100.0,
            "global.detail.sharpen.detail",
            mapped
        );
        sc!(
            crs::SHARPEN_EDGE_MASKING,
            recipe.global.detail.sharpen.masking,
            0.0,
            100.0,
            "global.detail.sharpen.masking",
            mapped
        );
        sc!(
            crs::LUMINANCE_SMOOTHING,
            recipe.global.detail.nr.luma,
            0.0,
            100.0,
            "global.detail.nr.luma",
            mapped
        );
        sc!(
            crs::LUMINANCE_NR_DETAIL,
            recipe.global.detail.nr.luma_detail,
            0.0,
            100.0,
            "global.detail.nr.luma_detail",
            mapped
        );
        sc!(
            crs::COLOR_NR,
            recipe.global.detail.nr.chroma,
            0.0,
            100.0,
            "global.detail.nr.chroma",
            mapped
        );
        sc!(
            crs::COLOR_NR_DETAIL,
            recipe.global.detail.nr.chroma_detail,
            0.0,
            100.0,
            "global.detail.nr.chroma_detail",
            mapped
        );

        // Optics.
        if let Some(b) = gb(doc, crs::AUTO_LATERAL_CA) {
            recipe.global.optics.ca = b;
            report
                .mapped
                .push(field_fidelity("global.optics.ca", crs::AUTO_LATERAL_CA));
            consumed.insert(crs::AUTO_LATERAL_CA.to_string());
        }
        sc!(
            crs::DEFRINGE_PURPLE,
            recipe.global.optics.defringe,
            0.0,
            100.0,
            "global.optics.defringe",
            approximate
        );
        sc!(
            crs::VIGNETTE_AMOUNT,
            recipe.global.optics.vignette_corr,
            -100.0,
            100.0,
            "global.optics.vignette_corr",
            approximate
        );
        let lens_enable = gb(doc, crs::LENS_PROFILE_ENABLE).unwrap_or(false);
        let lens_name = gs(doc, crs::LENS_PROFILE_NAME);
        if lens_enable || lens_name.is_some() {
            let mut lp = LensCorrection {
                profile_id: lens_name.clone().unwrap_or_default(),
                ..LensCorrection::default()
            };
            if let Some(v) = gf(doc, crs::LENS_PROFILE_DISTORTION_SCALE) {
                lp.distortion = clampf(v as f32, 0.0, 200.0);
            }
            if let Some(v) = gf(doc, crs::LENS_PROFILE_VIGNETTING_SCALE) {
                lp.vignetting = clampf(v as f32, 0.0, 200.0);
            }
            recipe.global.optics.lens_profile = Some(lp);
            // Report every lens key actually present so the partition stays
            // exhaustive (a consumed key must appear in a report bucket).
            for k in [
                crs::LENS_PROFILE_ENABLE,
                crs::LENS_PROFILE_NAME,
                crs::LENS_PROFILE_DISTORTION_SCALE,
                crs::LENS_PROFILE_VIGNETTING_SCALE,
            ] {
                if doc.contains(ns::CRS, k) {
                    consumed.insert(k.to_string());
                    report
                        .approximate
                        .push(field_fidelity("global.optics.lens_profile", k));
                }
            }
        }

        // Effects.
        sc!(
            crs::POST_CROP_VIGNETTE_AMOUNT,
            recipe.global.effects.postcrop_vignette.amount,
            -100.0,
            100.0,
            "global.effects.postcrop_vignette.amount",
            approximate
        );
        sc!(
            crs::PCV_MIDPOINT,
            recipe.global.effects.postcrop_vignette.midpoint,
            0.0,
            100.0,
            "global.effects.postcrop_vignette.midpoint",
            approximate
        );
        sc!(
            crs::PCV_FEATHER,
            recipe.global.effects.postcrop_vignette.feather,
            0.0,
            100.0,
            "global.effects.postcrop_vignette.feather",
            approximate
        );
        sc!(
            crs::PCV_ROUNDNESS,
            recipe.global.effects.postcrop_vignette.roundness,
            -100.0,
            100.0,
            "global.effects.postcrop_vignette.roundness",
            approximate
        );
        sc!(
            crs::PCV_HIGHLIGHTS,
            recipe.global.effects.postcrop_vignette.highlights,
            0.0,
            100.0,
            "global.effects.postcrop_vignette.highlights",
            approximate
        );
        sc!(
            crs::GRAIN_AMOUNT,
            recipe.global.effects.grain.amount,
            0.0,
            100.0,
            "global.effects.grain.amount",
            approximate
        );
        sc!(
            crs::GRAIN_SIZE,
            recipe.global.effects.grain.size,
            0.0,
            100.0,
            "global.effects.grain.size",
            approximate
        );
        sc!(
            crs::GRAIN_FREQUENCY,
            recipe.global.effects.grain.roughness,
            0.0,
            100.0,
            "global.effects.grain.roughness",
            approximate
        );

        // Base profile name hint.
        if let Some(name) = gs(doc, crs::CAMERA_PROFILE) {
            recipe.base_profile.id = name;
            report
                .approximate
                .push(field_fidelity("base_profile", crs::CAMERA_PROFILE));
            consumed.insert(crs::CAMERA_PROFILE.to_string());
        }

        // Geometry: crop edges + angle + upright + manual transform.
        let mut cropped = false;
        for (local, target) in [
            (crs::CROP_TOP, 0usize),
            (crs::CROP_LEFT, 1),
            (crs::CROP_BOTTOM, 2),
            (crs::CROP_RIGHT, 3),
        ] {
            if let Some(v) = gf(doc, local) {
                let e = clampf(v as f32, 0.0, 1.0);
                match target {
                    0 => recipe.geometry.crop.top = e,
                    1 => recipe.geometry.crop.left = e,
                    2 => recipe.geometry.crop.bottom = e,
                    _ => recipe.geometry.crop.right = e,
                }
                report.mapped.push(field_fidelity("geometry.crop", local));
                consumed.insert(local.to_string());
                cropped = true;
            }
        }
        if cropped {
            recipe.geometry.crop.clamp();
        }
        sc!(
            crs::CROP_ANGLE,
            recipe.geometry.angle,
            -45.0,
            45.0,
            "geometry.angle",
            approximate
        );
        if let Some(v) = gf(doc, crs::PERSPECTIVE_UPRIGHT) {
            recipe.geometry.upright = convert::upright_from_crs(v as i64);
            report
                .approximate
                .push(field_fidelity("geometry.upright", crs::PERSPECTIVE_UPRIGHT));
            consumed.insert(crs::PERSPECTIVE_UPRIGHT.to_string());
        }
        sc!(
            crs::PERSP_VERTICAL,
            recipe.geometry.transform.vertical,
            -100.0,
            100.0,
            "geometry.transform.vertical",
            approximate
        );
        sc!(
            crs::PERSP_HORIZONTAL,
            recipe.geometry.transform.horizontal,
            -100.0,
            100.0,
            "geometry.transform.horizontal",
            approximate
        );
        sc!(
            crs::PERSP_ROTATE,
            recipe.geometry.transform.rotate,
            -100.0,
            100.0,
            "geometry.transform.rotate",
            approximate
        );
        sc!(
            crs::PERSP_ASPECT,
            recipe.geometry.transform.aspect,
            -100.0,
            100.0,
            "geometry.transform.aspect",
            approximate
        );
        sc!(
            crs::PERSP_SCALE,
            recipe.geometry.transform.scale,
            -100.0,
            100.0,
            "geometry.transform.scale",
            approximate
        );
        sc!(
            crs::PERSP_X,
            recipe.geometry.transform.x_offset,
            -100.0,
            100.0,
            "geometry.transform.x_offset",
            approximate
        );
        sc!(
            crs::PERSP_Y,
            recipe.geometry.transform.y_offset,
            -100.0,
            100.0,
            "geometry.transform.y_offset",
            approximate
        );

        // Partition tail: every unconsumed crs: key → skipped + verbatim
        // passthrough; every foreign namespace → verbatim passthrough.
        capture_rest(doc, &mut recipe, &mut report, &consumed);

        CrsImport { recipe, report }
    }
}

/// Capture unconsumed `crs:` keys (→ `skipped` + passthrough) and all foreign
/// namespaces (→ passthrough) verbatim. Guarantees the report partition covers
/// every `crs:` key seen.
fn capture_rest(
    doc: &XmpDoc,
    recipe: &mut Recipe,
    report: &mut CrsImportReport,
    consumed: &BTreeSet<String>,
) {
    for (nsuri, local) in doc.property_names() {
        if nsuri == ns::CRS {
            if consumed.contains(local) {
                continue;
            }
            report.skipped.push(format!("crs:{local}"));
            capture_one(doc, recipe, nsuri, local);
        } else if is_foreign_ns(nsuri) {
            capture_one(doc, recipe, nsuri, local);
        }
        // lb: / rdf: / xml:, administrative, ignored.
    }
}

/// Encode one property (scalar or array) into `xmp_passthrough` verbatim.
fn capture_one(doc: &XmpDoc, recipe: &mut Recipe, nsuri: &str, local: &str) {
    let val = if let Some(items) = doc.get_array(nsuri, local) {
        let kind = doc.array_kind(nsuri, local).unwrap_or(ArrayKind::Bag);
        encode_array(kind, &items)
    } else if let Some(scalar) = doc.get(nsuri, local) {
        encode_scalar(&scalar)
    } else {
        return;
    };
    recipe
        .xmp_passthrough
        .fields
        .insert(pt_key(nsuri, local), val);
}
