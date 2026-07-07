// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The versioned edit [`Recipe`] (spec §3.2) — the full architecture field set,
//! typed behind the **E01-frozen surface** (`{ schema, pv }` fields and
//! [`Recipe::identity`]). `lightbox-render`/`lightbox-core`/`lightbox-cli` compile
//! unmodified against this grown type (T2 frozen-surface proof).
//!
//! Serialization (`to_cbor`/`from_cbor`/`canonical_hash`) lives in [`crate::cbor`].
//! **Persistence MUST go through `to_cbor`/`from_cbor`**, not generic serde — see
//! that module and deviations A-3.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use lightbox_decode::AssetProbe;
use lightbox_types::{MaskId, ProcessVersion, RetouchOpId};

use crate::leaves::{
    clampf, BwMix, CborValue, ColorGrade, Crop, CurveInvalid, Detail, Effects, Flip, HslTable,
    Optics, Presence, ProfileRef, ToneCurveSet, Transform, Treatment, Upright, WhiteBalance,
    XmpPassthrough,
};
use crate::params::{ParamDelta, ParamId, ParamSubset, ParamValue};

/// Recipe serialization schema (spec §3.2). Migrates **independently of `pv`**:
/// bump only for a structural change to the CBOR form, with a documented
/// `from_cbor` n→n+1 upgrade (Risk R6). A doc whose `schema` exceeds this is read
/// **read-only** as [`RecipeRead::NewerSchema`] and never rewritten.
pub const RECIPE_SCHEMA: u16 = 1;

/// The full §3.2 global stage set, typed now so E10/E11 add no schema fields.
/// `Default` is **neutral** (renders the source unmodified).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GlobalStages {
    /// White balance.
    pub white_balance: WhiteBalance,
    /// Exposure in stops, `-5.0..=5.0`.
    pub exposure: f32,
    /// Contrast, `-100..=100`.
    pub contrast: f32,
    /// Highlights, `-100..=100`.
    pub highlights: f32,
    /// Shadows, `-100..=100`.
    pub shadows: f32,
    /// Whites, `-100..=100`.
    pub whites: f32,
    /// Blacks, `-100..=100`.
    pub blacks: f32,
    /// Tone-curve set (composite + per-channel, one unit).
    pub tone_curve: ToneCurveSet,
    /// 8-band HSL mixer.
    pub hsl: HslTable,
    /// Colour grading wheels.
    pub color_grade: ColorGrade,
    /// Colour / B&W treatment.
    pub treatment: Treatment,
    /// Black-and-white channel mixer.
    pub bw: BwMix,
    /// Vibrance, `-100..=100`.
    pub vibrance: f32,
    /// Saturation, `-100..=100`.
    pub saturation: f32,
    /// Presence (clarity/texture/dehaze).
    pub presence: Presence,
    /// Detail (sharpen/NR).
    pub detail: Detail,
    /// Optics corrections.
    pub optics: Optics,
    /// Creative effects.
    pub effects: Effects,
}

/// Geometry stages (spec §3.2). `Default` is neutral (full frame, no rotation).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Geometry {
    /// Crop rectangle (normalized).
    pub crop: Crop,
    /// Straighten angle, `-45..=45`.
    pub angle: f32,
    /// Flip / mirror.
    pub flip: Flip,
    /// Upright auto-perspective mode.
    pub upright: Upright,
    /// Manual perspective transform.
    pub transform: Transform,
}

/// The versioned edit recipe (spec §3.2). `#[non_exhaustive]` (kept from the E01
/// stub) blocks out-of-crate struct literals so the body can grow behind the
/// frozen `{ schema, pv }` surface.
///
/// The derived `Serialize`/`Deserialize` are for **generic/in-memory** use only.
/// The authoritative on-disk codec is [`Recipe::to_cbor`]/[`Recipe::from_cbor`]
/// (deviations A-3): it alone preserves a newer build's unknown top-level keys
/// byte-for-byte.
#[non_exhaustive]
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Recipe {
    // ---- E01-frozen fields: names and types unchanged ----
    /// Serialization schema (see [`RECIPE_SCHEMA`]).
    pub schema: u16,
    /// Process version this recipe renders under (immutable per image, §4.5).
    pub pv: ProcessVersion,
    // ---- E09 body (architecture §3.2) ----
    /// Base profile + creative look.
    pub base_profile: ProfileRef,
    /// Global (non-geometry) stages.
    pub global: GlobalStages,
    /// Geometry stages.
    pub geometry: Geometry,
    /// Ordered mask id refs ONLY (content: E12 tables — §3.1 ownership rule).
    pub masks: Vec<MaskId>,
    /// Ordered retouch-op id refs ONLY (content: E12/E14 tables).
    pub retouch: Vec<RetouchOpId>,
    /// `lb:`-namespace extension bag (additive fields, Risk R6).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lb_extra: BTreeMap<String, CborValue>,
    /// Foreign XMP fields preserved verbatim (§3.1.1 passthrough rule).
    #[serde(default, skip_serializing_if = "xmp_passthrough_empty")]
    pub xmp_passthrough: XmpPassthrough,
    /// Forward-compat: top-level keys a **newer build** wrote, preserved verbatim
    /// by `to_cbor`/`from_cbor` (never surfaced by generic serde).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, CborValue>,
}

fn xmp_passthrough_empty(x: &XmpPassthrough) -> bool {
    x.fields.is_empty()
}

/// Forward-compat read result (spec §3.2). A doc from a newer build is preserved,
/// never rewritten.
///
/// The `Ok(Recipe)` shape is the frozen §3.2 seam artifact that E04/E05/E15
/// pattern-match; `RecipeRead` is a transient per-read return value (never stored
/// in bulk), so the size skew versus `NewerSchema` is intentional — boxing would
/// pessimize the common path's ergonomics for no memory win.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum RecipeRead {
    /// A recipe at this build's schema (or older), fully decoded.
    Ok(Recipe),
    /// `schema > RECIPE_SCHEMA`: locked read-only; the shell shows "edited in a
    /// newer Lightbox". The store round-trips `raw` unchanged.
    NewerSchema {
        /// The original doc bytes, preserved verbatim.
        raw: Vec<u8>,
        /// The doc's declared schema.
        schema: u16,
    },
}

impl RecipeRead {
    /// The decoded recipe, or `None` for [`RecipeRead::NewerSchema`].
    pub fn recipe(&self) -> Option<&Recipe> {
        match self {
            RecipeRead::Ok(r) => Some(r),
            RecipeRead::NewerSchema { .. } => None,
        }
    }

    /// Consume into the decoded recipe, or `None` for a newer-schema doc.
    pub fn into_recipe(self) -> Option<Recipe> {
        match self {
            RecipeRead::Ok(r) => Some(r),
            RecipeRead::NewerSchema { .. } => None,
        }
    }
}

/// What a [`Recipe::apply`] did (spec §3.2 `Applied`).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Applied {
    /// Params whose value actually changed.
    pub changed: Vec<ParamId>,
    /// Params whose incoming value was clamped into range.
    pub clamped: Vec<ParamId>,
}

/// Errors from recipe mutation / decode (spec §3.2).
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecipeError {
    /// A [`ParamValue`] of the wrong kind was applied to a [`ParamId`].
    #[error("param {0:?} does not accept a value of this kind")]
    TypeMismatch(ParamId),
    /// A tone curve exceeded the point cap.
    #[error("tone curve has {0} points (max {max})", max = crate::leaves::MAX_CURVE_POINTS)]
    TooManyCurvePoints(usize),
    /// A tone curve's x-coordinates were not strictly increasing.
    #[error("tone-curve x-coordinates must be strictly increasing (monotone-x)")]
    NonMonotoneCurve,
    /// CBOR decode failed.
    #[error("recipe CBOR decode failed: {0}")]
    Decode(String),
}

impl From<CurveInvalid> for RecipeError {
    fn from(e: CurveInvalid) -> RecipeError {
        match e {
            CurveInvalid::TooMany(n) => RecipeError::TooManyCurvePoints(n),
            CurveInvalid::NonMonotone => RecipeError::NonMonotoneCurve,
        }
    }
}

impl Recipe {
    /// The identity recipe: renders the source unmodified under `pv` (E01-frozen
    /// constructor, kept). Non-raw sources get the same neutral recipe; raw-only
    /// stages are neutral no-ops the §2.4 surface hides.
    pub fn identity(pv: ProcessVersion) -> Recipe {
        Recipe {
            schema: RECIPE_SCHEMA,
            pv,
            base_profile: ProfileRef::default(),
            global: GlobalStages::default(),
            geometry: Geometry::default(),
            masks: Vec::new(),
            retouch: Vec::new(),
            lb_extra: BTreeMap::new(),
            xmp_passthrough: XmpPassthrough::default(),
            unknown: BTreeMap::new(),
        }
    }

    /// Neutral recipe seeded from a probe (spec §3.2). At M1 this equals
    /// [`Recipe::identity`] (as-shot WB, matrix base, full crop are already the
    /// neutral defaults); `probe` is reserved for E04's as-shot WB / aspect
    /// seeding and is deliberately not yet consumed.
    pub fn default_for(pv: ProcessVersion, probe: &AssetProbe) -> Recipe {
        let _ = probe;
        Recipe::identity(pv)
    }

    /// True iff this recipe is neutral (renders the source unmodified) — drives
    /// `edit_index.is_edited`. Includes the extension bags: a recipe carrying
    /// foreign passthrough or unknown keys is not neutral.
    pub fn is_neutral(&self) -> bool {
        *self == Recipe::identity(self.pv)
    }

    /// The current value of `id` (spec §3.2). Total over every [`ParamId`]; the
    /// id→value slices are disjoint (deviations A-4) so `apply(id, get(id))` is a
    /// no-op and `diff`/`apply` compose exactly.
    pub fn get(&self, id: ParamId) -> ParamValue {
        use ParamValue as V;
        let g = &self.global;
        match id {
            ParamId::BaseProfile => V::Profile(self.base_profile.clone()),
            ParamId::WhiteBalance => V::Wb(g.white_balance),
            ParamId::Exposure => V::F32(g.exposure),
            ParamId::Contrast => V::F32(g.contrast),
            ParamId::Highlights => V::F32(g.highlights),
            ParamId::Shadows => V::F32(g.shadows),
            ParamId::Whites => V::F32(g.whites),
            ParamId::Blacks => V::F32(g.blacks),
            ParamId::ToneCurve => V::Curve(g.tone_curve.clone()),
            ParamId::Hsl => V::Hsl(g.hsl),
            ParamId::ColorGrade => V::ColorGrade(g.color_grade),
            ParamId::BwMix => V::BwMix(g.bw),
            ParamId::Vibrance => V::F32(g.vibrance),
            ParamId::Saturation => V::F32(g.saturation),
            ParamId::Treatment => V::Treatment(g.treatment),
            ParamId::Clarity => V::F32(g.presence.clarity),
            ParamId::Texture => V::F32(g.presence.texture),
            ParamId::Dehaze => V::F32(g.presence.dehaze),
            ParamId::Sharpen => V::Sharpen(g.detail.sharpen),
            ParamId::NoiseReduction => V::Nr(g.detail.nr),
            // Carrier holds ONLY lens_profile; ca/defringe/vignette neutral (A-4).
            ParamId::LensProfile => V::Lens(Optics {
                lens_profile: g.optics.lens_profile.clone(),
                ca: false,
                defringe: 0.0,
                vignette_corr: 0.0,
            }),
            ParamId::ChromaticAberration => V::Bool(g.optics.ca),
            ParamId::Defringe => V::F32(g.optics.defringe),
            ParamId::VignetteCorr => V::F32(g.optics.vignette_corr),
            ParamId::Crop => V::Crop(self.geometry.crop),
            ParamId::Angle => V::F32(self.geometry.angle),
            ParamId::Flip => V::I32(self.geometry.flip.to_i32()),
            ParamId::Upright => V::Upright(self.geometry.upright),
            ParamId::Transform => V::Transform(self.geometry.transform),
            ParamId::PostCropVignette => V::Vignette(g.effects.postcrop_vignette),
            ParamId::Grain => V::Grain(g.effects.grain),
            ParamId::CreativeLut => V::Lut(g.effects.creative_lut.clone()),
            ParamId::MaskList => V::MaskIds(self.masks.clone()),
            ParamId::RetouchList => V::RetouchIds(self.retouch.clone()),
        }
    }

    /// Apply a delta: validate types, clamp ranges, write. Out-of-range scalars
    /// clamp (and report in [`Applied::clamped`]); a wrong-kind [`ParamValue`] or
    /// an invalid tone curve is a [`RecipeError`]. Idempotent per delta.
    ///
    /// The extension bags (`lb_extra`/`xmp_passthrough`/`unknown`) are **not**
    /// param-addressable and are left untouched — applying source settings to a
    /// target preserves the target's foreign metadata (the correct preset/paste
    /// behavior).
    pub fn apply(&mut self, delta: &ParamDelta) -> Result<Applied, RecipeError> {
        let mut report = Applied::default();
        for (id, value) in &delta.0 {
            let before = self.get(*id);
            self.set(*id, value.clone(), &mut report)?;
            if self.get(*id) != before {
                report.changed.push(*id);
            }
        }
        Ok(report)
    }

    /// The delta that turns `from` into `self` (spec §3.2). `self.diff(&self)` is
    /// empty; `a.apply(&b.diff(&a))` yields `b` over the param surface.
    pub fn diff(&self, from: &Recipe) -> ParamDelta {
        let mut d = ParamDelta::new();
        for id in ParamId::ALL {
            let mine = self.get(id);
            if mine != from.get(id) {
                d.0.insert(id, mine);
            }
        }
        d
    }

    /// The delta of this recipe's values for every param in `subset` (spec §3.2).
    pub fn extract(&self, subset: &ParamSubset) -> ParamDelta {
        let mut d = ParamDelta::new();
        for id in ParamId::ALL {
            if subset.contains(id) {
                d.0.insert(id, self.get(id));
            }
        }
        d
    }

    /// Write a single param, type-checking and clamping (helper for [`Recipe::apply`]).
    fn set(&mut self, id: ParamId, v: ParamValue, report: &mut Applied) -> Result<(), RecipeError> {
        use ParamValue as V;

        // Clamp a scalar into `[lo, hi]`, recording a clamp when it bites.
        macro_rules! set_scalar {
            ($field:expr, $lo:expr, $hi:expr) => {{
                let V::F32(x) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                let c = clampf(x, $lo, $hi);
                if c != x {
                    report.clamped.push(id);
                }
                $field = c;
            }};
        }

        match id {
            ParamId::BaseProfile => {
                let V::Profile(mut p) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                p.clamp();
                self.base_profile = p;
            }
            ParamId::WhiteBalance => {
                let V::Wb(mut w) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                w.clamp();
                self.global.white_balance = w;
            }
            ParamId::Exposure => set_scalar!(self.global.exposure, -5.0, 5.0),
            ParamId::Contrast => set_scalar!(self.global.contrast, -100.0, 100.0),
            ParamId::Highlights => set_scalar!(self.global.highlights, -100.0, 100.0),
            ParamId::Shadows => set_scalar!(self.global.shadows, -100.0, 100.0),
            ParamId::Whites => set_scalar!(self.global.whites, -100.0, 100.0),
            ParamId::Blacks => set_scalar!(self.global.blacks, -100.0, 100.0),
            ParamId::ToneCurve => {
                let V::Curve(mut c) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                c.clamp_points();
                c.validate()?;
                self.global.tone_curve = c;
            }
            ParamId::Hsl => {
                let V::Hsl(mut h) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                h.clamp();
                self.global.hsl = h;
            }
            ParamId::ColorGrade => {
                let V::ColorGrade(mut cg) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                cg.clamp();
                self.global.color_grade = cg;
            }
            ParamId::BwMix => {
                let V::BwMix(mut m) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                m.clamp();
                self.global.bw = m;
            }
            ParamId::Vibrance => set_scalar!(self.global.vibrance, -100.0, 100.0),
            ParamId::Saturation => set_scalar!(self.global.saturation, -100.0, 100.0),
            ParamId::Treatment => {
                let V::Treatment(t) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                self.global.treatment = t;
            }
            ParamId::Clarity => set_scalar!(self.global.presence.clarity, -100.0, 100.0),
            ParamId::Texture => set_scalar!(self.global.presence.texture, -100.0, 100.0),
            ParamId::Dehaze => set_scalar!(self.global.presence.dehaze, -100.0, 100.0),
            ParamId::Sharpen => {
                let V::Sharpen(mut s) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                s.clamp();
                self.global.detail.sharpen = s;
            }
            ParamId::NoiseReduction => {
                let V::Nr(mut n) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                n.clamp();
                self.global.detail.nr = n;
            }
            ParamId::LensProfile => {
                let V::Lens(mut o) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                if let Some(lp) = &mut o.lens_profile {
                    lp.clamp();
                }
                // Only lens_profile is authoritative under this id (A-4).
                self.global.optics.lens_profile = o.lens_profile;
            }
            ParamId::ChromaticAberration => {
                let V::Bool(b) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                self.global.optics.ca = b;
            }
            ParamId::Defringe => set_scalar!(self.global.optics.defringe, 0.0, 100.0),
            ParamId::VignetteCorr => set_scalar!(self.global.optics.vignette_corr, -100.0, 100.0),
            ParamId::Crop => {
                let V::Crop(mut c) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                c.clamp();
                self.geometry.crop = c;
            }
            ParamId::Angle => set_scalar!(self.geometry.angle, -45.0, 45.0),
            ParamId::Flip => {
                let V::I32(n) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                let f = Flip::from_i32(n);
                if f.to_i32() != n {
                    report.clamped.push(id);
                }
                self.geometry.flip = f;
            }
            ParamId::Upright => {
                let V::Upright(u) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                self.geometry.upright = u;
            }
            ParamId::Transform => {
                let V::Transform(mut t) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                t.clamp();
                self.geometry.transform = t;
            }
            ParamId::PostCropVignette => {
                let V::Vignette(mut pv) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                pv.clamp();
                self.global.effects.postcrop_vignette = pv;
            }
            ParamId::Grain => {
                let V::Grain(mut gr) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                gr.clamp();
                self.global.effects.grain = gr;
            }
            ParamId::CreativeLut => {
                let V::Lut(mut lut) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                if let Some(l) = &mut lut {
                    l.clamp();
                }
                self.global.effects.creative_lut = lut;
            }
            ParamId::MaskList => {
                let V::MaskIds(ids) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                self.masks = ids;
            }
            ParamId::RetouchList => {
                let V::RetouchIds(ids) = v else {
                    return Err(RecipeError::TypeMismatch(id));
                };
                self.retouch = ids;
            }
        }
        Ok(())
    }
}
