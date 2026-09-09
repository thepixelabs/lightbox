// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`Recipe::to_xmp`], the projection (read-only materialization, §3.1 / T19).
//!
//! A recipe becomes an [`XmpDoc`] in three layers, in this precedence:
//!
//! 1. **`lb:` full fidelity**, the authoritative round-trip. `lb:RecipeCbor`
//!    carries the hex of the canonical CBOR recipe, so [`Recipe::read_xmp`] on the
//!    `lb:` path reconstructs a **struct-equal** recipe (T20). `lb:Schema`,
//!    `lb:ProcessVersion`, and `lb:CreatorTool` record provenance.
//! 2. **`crs:` compatibility**, best-effort Lightroom-readable develop settings,
//!    emitted per the normative [`FIELD_TABLE`](super::convert::FIELD_TABLE) (T18).
//!    Only **non-neutral** fields are written (an absent `crs:` field reads back as
//!    its default in any ACR-family app), keeping the packet compact.
//! 3. **`xmp_passthrough` foreign fields**, re-emitted **verbatim and last** so a
//!    `crs:` import → export round-trips a file's foreign metadata intact (the §6
//!    "foreign properties survive read→write" contract) and overrides the
//!    synthetic `crs:Version`/`crs:ProcessVersion` head with the source's own.

use lightbox_meta::xmp::{ns, ArrayKind, XmpDoc, XmpError, XmpValue};

use crate::leaves::{Crop, GradeWheel, ToneCurve, Treatment, Upright, WhiteBalance};
use crate::recipe::Recipe;

use super::convert::{self, crs, BAND_NAMES};
use super::{decode_passthrough, lb, pt_split, to_hex, PassthroughValue, XmpMapError, XmpWriteCtx};

// ── typed crs: setters (a crs: property is stringly-typed under the hood) ───────

fn cr(doc: &mut XmpDoc, local: &str, v: f32) -> Result<(), XmpError> {
    doc.set(ns::CRS, local, XmpValue::Real(v as f64))
}
fn ci(doc: &mut XmpDoc, local: &str, v: i64) -> Result<(), XmpError> {
    doc.set(ns::CRS, local, XmpValue::Int(v))
}
fn cb(doc: &mut XmpDoc, local: &str, v: bool) -> Result<(), XmpError> {
    doc.set(ns::CRS, local, XmpValue::Bool(v))
}
fn ct(doc: &mut XmpDoc, local: &str, v: &str) -> Result<(), XmpError> {
    doc.set(ns::CRS, local, XmpValue::text(v))
}

fn emit_curve(doc: &mut XmpDoc, local: &str, curve: &ToneCurve) -> Result<(), XmpError> {
    if *curve != ToneCurve::default() {
        let items: Vec<XmpValue> = convert::curve_to_crs(curve)
            .into_iter()
            .map(XmpValue::text)
            .collect();
        doc.set_array(ns::CRS, local, ArrayKind::Seq, &items)?;
    }
    Ok(())
}

fn emit_wheel(
    doc: &mut XmpDoc,
    w: GradeWheel,
    hue: &str,
    sat: &str,
    lum: &str,
) -> Result<(), XmpError> {
    if w.hue != 0.0 {
        cr(doc, hue, w.hue)?;
    }
    if w.sat != 0.0 {
        cr(doc, sat, w.sat)?;
    }
    if w.lum != 0.0 {
        cr(doc, lum, w.lum)?;
    }
    Ok(())
}

impl Recipe {
    /// Project this recipe to an [`XmpDoc`] (spec §3.6 / T19). See the module
    /// docs for the three-layer precedence. Infallible for our own model; the
    /// [`XmpMapError`] is the substrate's error channel.
    pub fn to_xmp(&self, ctx: &XmpWriteCtx<'_>) -> Result<XmpDoc, XmpMapError> {
        let mut doc = XmpDoc::new();

        // (1) lb: authoritative full-fidelity payload + provenance.
        doc.set(
            ns::LB,
            lb::RECIPE_CBOR,
            XmpValue::text(to_hex(&self.to_cbor())),
        )?;
        doc.set(ns::LB, lb::SCHEMA, XmpValue::Int(i64::from(self.schema)))?;
        doc.set(
            ns::LB,
            lb::PROCESS_VERSION,
            XmpValue::Int(i64::from(self.pv.0)),
        )?;
        doc.set(ns::LB, lb::CREATOR_TOOL, XmpValue::text(ctx.app_version))?;

        // (2) crs: compatibility emit (best-effort, LR-readable).
        self.emit_crs(&mut doc)?;

        // (3) foreign passthrough, verbatim and last (overrides synthetic head).
        self.emit_passthrough(&mut doc)?;

        Ok(doc)
    }

    /// Write the non-neutral `crs:` develop settings (spec §3.6 / T18).
    fn emit_crs(&self, doc: &mut XmpDoc) -> Result<(), XmpError> {
        // A modern PV2012 head so LR reads our tone params under the right model.
        ct(doc, crs::VERSION, crs::EMIT_VERSION)?;
        ct(doc, crs::PROCESS_VERSION, crs::EMIT_PROCESS_VERSION)?;

        let g = &self.global;

        // White balance.
        if g.white_balance != WhiteBalance::AsShot {
            let (mode, temp, tint) = convert::wb_to_crs(g.white_balance);
            ct(doc, crs::WHITE_BALANCE, &mode)?;
            if let Some(t) = temp {
                ci(doc, crs::TEMPERATURE, t.round() as i64)?;
            }
            if let Some(ti) = tint {
                ci(doc, crs::TINT, ti.round() as i64)?;
            }
        }

        // Basic tone (±100 sliders, exposure in stops), identity converters.
        for (local, v) in [
            (crs::EXPOSURE, g.exposure),
            (crs::CONTRAST, g.contrast),
            (crs::HIGHLIGHTS, g.highlights),
            (crs::SHADOWS, g.shadows),
            (crs::WHITES, g.whites),
            (crs::BLACKS, g.blacks),
            (crs::TEXTURE, g.presence.texture),
            (crs::CLARITY, g.presence.clarity),
            (crs::DEHAZE, g.presence.dehaze),
            (crs::VIBRANCE, g.vibrance),
            (crs::SATURATION, g.saturation),
        ] {
            if v != 0.0 {
                cr(doc, local, v)?;
            }
        }

        // Tone curves.
        emit_curve(doc, crs::TONE_CURVE, &g.tone_curve.rgb)?;
        emit_curve(doc, crs::TONE_CURVE_RED, &g.tone_curve.r)?;
        emit_curve(doc, crs::TONE_CURVE_GREEN, &g.tone_curve.g)?;
        emit_curve(doc, crs::TONE_CURVE_BLUE, &g.tone_curve.b)?;

        // Parametric (region-slider) tone curve, E10 task C14.
        let pc = &g.tone_curve.parametric;
        if pc.highlights != 0.0 {
            cr(doc, crs::PARAMETRIC_HIGHLIGHTS, pc.highlights)?;
        }
        if pc.lights != 0.0 {
            cr(doc, crs::PARAMETRIC_LIGHTS, pc.lights)?;
        }
        if pc.darks != 0.0 {
            cr(doc, crs::PARAMETRIC_DARKS, pc.darks)?;
        }
        if pc.shadows != 0.0 {
            cr(doc, crs::PARAMETRIC_SHADOWS, pc.shadows)?;
        }
        if pc.splits != [0.25, 0.50, 0.75] {
            cr(
                doc,
                crs::PARAMETRIC_SHADOW_SPLIT,
                convert::parametric_split_to_crs(pc.splits[0]),
            )?;
            cr(
                doc,
                crs::PARAMETRIC_MIDTONE_SPLIT,
                convert::parametric_split_to_crs(pc.splits[1]),
            )?;
            cr(
                doc,
                crs::PARAMETRIC_HIGHLIGHT_SPLIT,
                convert::parametric_split_to_crs(pc.splits[2]),
            )?;
        }

        // HSL mixer, 8 bands × 3.
        for (i, band) in g.hsl.bands.iter().enumerate() {
            let name = BAND_NAMES[i];
            if band.hue != 0.0 {
                cr(doc, &format!("{}{name}", crs::HUE_ADJ_PREFIX), band.hue)?;
            }
            if band.sat != 0.0 {
                cr(doc, &format!("{}{name}", crs::SAT_ADJ_PREFIX), band.sat)?;
            }
            if band.lum != 0.0 {
                cr(doc, &format!("{}{name}", crs::LUM_ADJ_PREFIX), band.lum)?;
            }
        }

        // Treatment + B&W mixer.
        if g.treatment == Treatment::BlackAndWhite {
            cb(doc, crs::CONVERT_TO_GRAYSCALE, true)?;
        }
        for (i, w) in g.bw.weights.iter().enumerate() {
            if *w != 0.0 {
                cr(
                    doc,
                    &format!("{}{}", crs::GRAY_MIXER_PREFIX, BAND_NAMES[i]),
                    *w,
                )?;
            }
        }

        // Colour grading wheels.
        let cg = &g.color_grade;
        emit_wheel(
            doc,
            cg.shadows,
            crs::CG_SHADOW_HUE,
            crs::CG_SHADOW_SAT,
            crs::CG_SHADOW_LUM,
        )?;
        emit_wheel(
            doc,
            cg.midtones,
            crs::CG_MIDTONE_HUE,
            crs::CG_MIDTONE_SAT,
            crs::CG_MIDTONE_LUM,
        )?;
        emit_wheel(
            doc,
            cg.highlights,
            crs::CG_HIGHLIGHT_HUE,
            crs::CG_HIGHLIGHT_SAT,
            crs::CG_HIGHLIGHT_LUM,
        )?;
        emit_wheel(
            doc,
            cg.global,
            crs::CG_GLOBAL_HUE,
            crs::CG_GLOBAL_SAT,
            crs::CG_GLOBAL_LUM,
        )?;
        if cg.blend != 0.0 {
            cr(doc, crs::CG_BLENDING, cg.blend)?;
        }
        if cg.balance != 0.0 {
            cr(doc, crs::CG_BALANCE, cg.balance)?;
        }

        // Detail: sharpen + noise reduction.
        let d = &g.detail;
        if d.sharpen != Default::default() {
            cr(doc, crs::SHARPNESS, d.sharpen.amount)?;
            cr(doc, crs::SHARPEN_RADIUS, d.sharpen.radius)?;
            cr(doc, crs::SHARPEN_DETAIL, d.sharpen.detail)?;
            cr(doc, crs::SHARPEN_EDGE_MASKING, d.sharpen.masking)?;
        }
        if d.nr != Default::default() {
            cr(doc, crs::LUMINANCE_SMOOTHING, d.nr.luma)?;
            cr(doc, crs::LUMINANCE_NR_DETAIL, d.nr.luma_detail)?;
            cr(doc, crs::COLOR_NR, d.nr.chroma)?;
            cr(doc, crs::COLOR_NR_DETAIL, d.nr.chroma_detail)?;
        }

        // Optics.
        let o = &g.optics;
        if let Some(lp) = &o.lens_profile {
            cb(doc, crs::LENS_PROFILE_ENABLE, true)?;
            if !lp.profile_id.is_empty() {
                ct(doc, crs::LENS_PROFILE_NAME, &lp.profile_id)?;
            }
            cr(doc, crs::LENS_PROFILE_DISTORTION_SCALE, lp.distortion)?;
            cr(doc, crs::LENS_PROFILE_VIGNETTING_SCALE, lp.vignetting)?;
        }
        if o.ca {
            cb(doc, crs::AUTO_LATERAL_CA, true)?;
        }
        if o.defringe != 0.0 {
            cr(doc, crs::DEFRINGE_PURPLE, o.defringe)?;
        }
        if o.vignette_corr != 0.0 {
            cr(doc, crs::VIGNETTE_AMOUNT, o.vignette_corr)?;
        }

        // Effects.
        let e = &g.effects;
        let pcv = &e.postcrop_vignette;
        if pcv.amount != 0.0 {
            cr(doc, crs::POST_CROP_VIGNETTE_AMOUNT, pcv.amount)?;
            cr(doc, crs::PCV_MIDPOINT, pcv.midpoint)?;
            cr(doc, crs::PCV_FEATHER, pcv.feather)?;
            cr(doc, crs::PCV_ROUNDNESS, pcv.roundness)?;
            cr(doc, crs::PCV_HIGHLIGHTS, pcv.highlights)?;
        }
        if e.grain.amount != 0.0 {
            cr(doc, crs::GRAIN_AMOUNT, e.grain.amount)?;
            cr(doc, crs::GRAIN_SIZE, e.grain.size)?;
            cr(doc, crs::GRAIN_FREQUENCY, e.grain.roughness)?;
        }

        // Base profile identity (a name hint; full fidelity is lb:).
        if self.base_profile.id != "matrix-base" {
            ct(doc, crs::CAMERA_PROFILE, &self.base_profile.id)?;
        }

        // Geometry.
        let geo = &self.geometry;
        if geo.crop != Crop::default() {
            cb(doc, crs::HAS_CROP, true)?;
            cr(doc, crs::CROP_TOP, geo.crop.top)?;
            cr(doc, crs::CROP_LEFT, geo.crop.left)?;
            cr(doc, crs::CROP_BOTTOM, geo.crop.bottom)?;
            cr(doc, crs::CROP_RIGHT, geo.crop.right)?;
        }
        if geo.angle != 0.0 {
            cr(doc, crs::CROP_ANGLE, geo.angle)?;
        }
        if geo.upright != Upright::Off {
            ci(
                doc,
                crs::PERSPECTIVE_UPRIGHT,
                convert::upright_to_crs(geo.upright),
            )?;
        }
        // geometry.flip has no crs: peer at M1 (skipped-by-design; lb: only).
        let t = &geo.transform;
        for (local, v) in [
            (crs::PERSP_VERTICAL, t.vertical),
            (crs::PERSP_HORIZONTAL, t.horizontal),
            (crs::PERSP_ROTATE, t.rotate),
            (crs::PERSP_ASPECT, t.aspect),
            (crs::PERSP_SCALE, t.scale),
            (crs::PERSP_X, t.x_offset),
            (crs::PERSP_Y, t.y_offset),
        ] {
            if v != 0.0 {
                cr(doc, local, v)?;
            }
        }

        Ok(())
    }

    /// Re-emit captured foreign fields verbatim (spec §3.1.1 passthrough rule).
    fn emit_passthrough(&self, doc: &mut XmpDoc) -> Result<(), XmpError> {
        for (key, val) in &self.xmp_passthrough.fields {
            let Some((nsuri, local)) = pt_split(key) else {
                continue; // not one of our captures, skip defensively
            };
            match decode_passthrough(val) {
                Some(PassthroughValue::Scalar(s)) => {
                    doc.set(nsuri, local, XmpValue::text(s))?;
                }
                Some(PassthroughValue::Array(kind, items)) => {
                    let xs: Vec<XmpValue> = items.into_iter().map(XmpValue::text).collect();
                    doc.set_array(nsuri, local, kind, &xs)?;
                }
                None => {}
            }
        }
        Ok(())
    }
}
