// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Param vocabulary and deltas (spec §3.1). [`ParamId`] wire ids are the stable
//! currency behind presets, copy/paste, sync, history, and batch apply — the one
//! mechanism research 02 says buys them "nearly for free". [`ParamDelta`] is an
//! ordered map of changed params to new values.
//!
//! **Wire discipline (T1):** `ParamId` values are serialized as `u16` and must
//! **never be renumbered** — append only. The snapshot test in this module fails
//! on any renumbering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use lightbox_types::{MaskId, RetouchOpId};

use crate::leaves::{
    ColorGrade, CreativeLut, Crop, HslTable, NoiseReduction, Optics, PostCropVignette, ProfileRef,
    Sharpen, ToneCurveSet, Transform, Treatment, Upright, WhiteBalance,
};

/// Stable wire ids for every addressable param (spec §3.1). Serialized as `u16`
/// in CBOR deltas and `history_step` rows. **Never renumber; append only.**
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[non_exhaustive]
#[repr(u16)]
pub enum ParamId {
    /// Base profile + creative look.
    BaseProfile = 1,
    /// White balance.
    WhiteBalance = 10,
    /// Exposure (stops).
    Exposure = 20,
    /// Contrast.
    Contrast = 21,
    /// Highlights.
    Highlights = 22,
    /// Shadows.
    Shadows = 23,
    /// Whites.
    Whites = 24,
    /// Blacks.
    Blacks = 25,
    /// Tone curve (composite + per-channel, one unit).
    ToneCurve = 40,
    /// HSL mixer.
    Hsl = 50,
    /// Colour grading wheels.
    ColorGrade = 51,
    /// Black-and-white channel mixer.
    BwMix = 52,
    /// Vibrance.
    Vibrance = 53,
    /// Saturation.
    Saturation = 54,
    /// Colour / B&W treatment.
    Treatment = 55,
    /// Clarity.
    Clarity = 60,
    /// Texture.
    Texture = 61,
    /// Dehaze.
    Dehaze = 62,
    /// Sharpening.
    Sharpen = 70,
    /// Noise reduction.
    NoiseReduction = 71,
    /// Lens-profile correction.
    LensProfile = 80,
    /// Chromatic-aberration removal.
    ChromaticAberration = 81,
    /// Defringe.
    Defringe = 82,
    /// Manual vignetting correction.
    VignetteCorr = 83,
    /// Crop rectangle.
    Crop = 90,
    /// Straighten angle.
    Angle = 91,
    /// Flip / mirror.
    Flip = 92,
    /// Upright auto-perspective.
    Upright = 93,
    /// Manual perspective transform.
    Transform = 94,
    /// Post-crop vignette.
    PostCropVignette = 100,
    /// Film grain.
    Grain = 101,
    /// Creative LUT.
    CreativeLut = 102,
    /// Ordered mask id list (content owned by E12 — §3.1 ownership rule).
    MaskList = 120,
    /// Ordered retouch-op id list (content owned by E12/E14).
    RetouchList = 121,
}

impl ParamId {
    /// Every `ParamId` variant, in wire-id order. The single source of truth for
    /// iteration, `get`/`diff` scans, and the wire-id snapshot test.
    pub const ALL: [ParamId; 34] = [
        ParamId::BaseProfile,
        ParamId::WhiteBalance,
        ParamId::Exposure,
        ParamId::Contrast,
        ParamId::Highlights,
        ParamId::Shadows,
        ParamId::Whites,
        ParamId::Blacks,
        ParamId::ToneCurve,
        ParamId::Hsl,
        ParamId::ColorGrade,
        ParamId::BwMix,
        ParamId::Vibrance,
        ParamId::Saturation,
        ParamId::Treatment,
        ParamId::Clarity,
        ParamId::Texture,
        ParamId::Dehaze,
        ParamId::Sharpen,
        ParamId::NoiseReduction,
        ParamId::LensProfile,
        ParamId::ChromaticAberration,
        ParamId::Defringe,
        ParamId::VignetteCorr,
        ParamId::Crop,
        ParamId::Angle,
        ParamId::Flip,
        ParamId::Upright,
        ParamId::Transform,
        ParamId::PostCropVignette,
        ParamId::Grain,
        ParamId::CreativeLut,
        ParamId::MaskList,
        ParamId::RetouchList,
    ];

    /// The stable `u16` wire id.
    pub fn to_u16(self) -> u16 {
        self as u16
    }

    /// Inverse of [`ParamId::to_u16`]; `None` for an unknown code.
    pub fn from_u16(v: u16) -> Option<ParamId> {
        ParamId::ALL.into_iter().find(|id| id.to_u16() == v)
    }
}

// Serialize `ParamId` as its `u16` wire id (spec §3.1), not the variant name.
impl Serialize for ParamId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.to_u16())
    }
}

impl<'de> Deserialize<'de> for ParamId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u16::deserialize(d)?;
        ParamId::from_u16(v)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown ParamId wire id {v}")))
    }
}

/// The value carried for a [`ParamId`] (spec §3.1). `#[non_exhaustive]`: the
/// carrier set is deliberately coarser than [`ParamId`]; see deviations A-4 for
/// the disjoint id→value mapping (`Recipe::get`/`Recipe::apply`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ParamValue {
    /// A scalar float param (exposure, contrast, clarity, angle, …).
    F32(f32),
    /// A scalar int param (flip code, …).
    I32(i32),
    /// A boolean param (chromatic-aberration toggle, …).
    Bool(bool),
    /// White balance.
    Wb(WhiteBalance),
    /// Tone-curve set.
    Curve(ToneCurveSet),
    /// HSL mixer.
    Hsl(HslTable),
    /// Colour grading.
    ColorGrade(ColorGrade),
    /// B&W channel mixer.
    BwMix(crate::leaves::BwMix),
    /// Sharpening.
    Sharpen(Sharpen),
    /// Noise reduction.
    Nr(NoiseReduction),
    /// Base profile reference.
    Profile(ProfileRef),
    /// Crop rectangle.
    Crop(Crop),
    /// Upright mode.
    Upright(Upright),
    /// Manual transform.
    Transform(Transform),
    /// Optional creative LUT.
    Lut(Option<CreativeLut>),
    /// Post-crop vignette.
    Vignette(PostCropVignette),
    /// Film grain.
    Grain(crate::leaves::Grain),
    /// Optics block (carries only `lens_profile` under `ParamId::LensProfile`).
    Lens(Optics),
    /// Colour / B&W treatment.
    Treatment(Treatment),
    /// Ordered mask id list.
    MaskIds(Vec<MaskId>),
    /// Ordered retouch-op id list.
    RetouchIds(Vec<RetouchOpId>),
}

/// Group taxonomy for preset checklists and copy/paste subsets (research 02:
/// partial presets). Every [`ParamId`] maps to exactly one group.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ParamGroup {
    /// Base profile + look.
    BaseProfile,
    /// White balance.
    WhiteBalance,
    /// Basic tone (exposure/contrast/highlights/shadows/whites/blacks).
    Tone,
    /// Tone curve.
    Curve,
    /// HSL colour mixer.
    ColorMixer,
    /// Colour grading.
    ColorGrading,
    /// Black-and-white mixer (+ treatment).
    BwMix,
    /// Presence (clarity/texture/dehaze/vibrance/saturation).
    Presence,
    /// Detail (sharpen/NR).
    Detail,
    /// Optics.
    Optics,
    /// Geometry (crop/angle/flip/upright/transform).
    Geometry,
    /// Effects (vignette/grain/LUT).
    Effects,
    /// Masks (ids only at E09).
    Masks,
    /// Retouch (ids only at E09).
    Retouch,
}

/// The group a [`ParamId`] belongs to (spec §3.1). Exhaustive match, no wildcard
/// arm — adding a `ParamId` variant is a compile error until it is grouped here.
pub fn group_of(id: ParamId) -> ParamGroup {
    use ParamGroup as G;
    match id {
        ParamId::BaseProfile => G::BaseProfile,
        ParamId::WhiteBalance => G::WhiteBalance,
        ParamId::Exposure
        | ParamId::Contrast
        | ParamId::Highlights
        | ParamId::Shadows
        | ParamId::Whites
        | ParamId::Blacks => G::Tone,
        ParamId::ToneCurve => G::Curve,
        ParamId::Hsl => G::ColorMixer,
        ParamId::ColorGrade => G::ColorGrading,
        // Treatment travels with the B&W mixer (deviations A-5).
        ParamId::BwMix | ParamId::Treatment => G::BwMix,
        // Vibrance/Saturation sit in the LR "Presence" cluster (deviations A-5).
        ParamId::Vibrance
        | ParamId::Saturation
        | ParamId::Clarity
        | ParamId::Texture
        | ParamId::Dehaze => G::Presence,
        ParamId::Sharpen | ParamId::NoiseReduction => G::Detail,
        ParamId::LensProfile
        | ParamId::ChromaticAberration
        | ParamId::Defringe
        | ParamId::VignetteCorr => G::Optics,
        ParamId::Crop | ParamId::Angle | ParamId::Flip | ParamId::Upright | ParamId::Transform => {
            G::Geometry
        }
        ParamId::PostCropVignette | ParamId::Grain | ParamId::CreativeLut => G::Effects,
        ParamId::MaskList => G::Masks,
        ParamId::RetouchList => G::Retouch,
    }
}

/// A set of param groups — the currency of preset checklists and copy/paste
/// subset selection (spec §3.1).
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ParamSubset {
    /// The selected groups.
    pub groups: BTreeSet<ParamGroup>,
}

impl ParamSubset {
    /// A subset over the given groups.
    pub fn from_groups<I: IntoIterator<Item = ParamGroup>>(groups: I) -> ParamSubset {
        ParamSubset {
            groups: groups.into_iter().collect(),
        }
    }

    /// True iff `id`'s group is selected.
    pub fn contains(&self, id: ParamId) -> bool {
        self.groups.contains(&group_of(id))
    }
}

/// The universal currency (spec §3.1): an ordered map of changed params to new
/// values. Ordered by [`ParamId`] wire id, so encoding is deterministic.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParamDelta(pub BTreeMap<ParamId, ParamValue>);

impl ParamDelta {
    /// Empty delta.
    pub fn new() -> ParamDelta {
        ParamDelta(BTreeMap::new())
    }

    /// Merge `later` into `self`; on a key collision `later` wins (spec §3.1).
    pub fn merge(&mut self, later: ParamDelta) {
        for (k, v) in later.0 {
            self.0.insert(k, v);
        }
    }

    /// The sub-delta whose params fall in `subset` (spec §3.1: `restrict ⊆ subset`).
    pub fn restrict(&self, subset: &ParamSubset) -> ParamDelta {
        ParamDelta(
            self.0
                .iter()
                .filter(|(id, _)| subset.contains(**id))
                .map(|(id, v)| (*id, v.clone()))
                .collect(),
        )
    }

    /// True iff no params are carried.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of params carried.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T1 wire-id snapshot: fails on ANY renumbering (append-only discipline).
    #[test]
    fn param_id_wire_ids_are_frozen() {
        let expect: &[(ParamId, u16)] = &[
            (ParamId::BaseProfile, 1),
            (ParamId::WhiteBalance, 10),
            (ParamId::Exposure, 20),
            (ParamId::Contrast, 21),
            (ParamId::Highlights, 22),
            (ParamId::Shadows, 23),
            (ParamId::Whites, 24),
            (ParamId::Blacks, 25),
            (ParamId::ToneCurve, 40),
            (ParamId::Hsl, 50),
            (ParamId::ColorGrade, 51),
            (ParamId::BwMix, 52),
            (ParamId::Vibrance, 53),
            (ParamId::Saturation, 54),
            (ParamId::Treatment, 55),
            (ParamId::Clarity, 60),
            (ParamId::Texture, 61),
            (ParamId::Dehaze, 62),
            (ParamId::Sharpen, 70),
            (ParamId::NoiseReduction, 71),
            (ParamId::LensProfile, 80),
            (ParamId::ChromaticAberration, 81),
            (ParamId::Defringe, 82),
            (ParamId::VignetteCorr, 83),
            (ParamId::Crop, 90),
            (ParamId::Angle, 91),
            (ParamId::Flip, 92),
            (ParamId::Upright, 93),
            (ParamId::Transform, 94),
            (ParamId::PostCropVignette, 100),
            (ParamId::Grain, 101),
            (ParamId::CreativeLut, 102),
            (ParamId::MaskList, 120),
            (ParamId::RetouchList, 121),
        ];
        assert_eq!(
            expect.len(),
            ParamId::ALL.len(),
            "ALL must list every variant"
        );
        for (id, wire) in expect {
            assert_eq!(id.to_u16(), *wire, "wire id changed for {id:?}");
            assert_eq!(ParamId::from_u16(*wire), Some(*id));
        }
    }

    /// T1: `ALL` covers every variant and round-trips through `u16`.
    #[test]
    fn param_id_all_round_trips_u16() {
        for id in ParamId::ALL {
            assert_eq!(ParamId::from_u16(id.to_u16()), Some(id));
        }
        // Distinct wire ids.
        let mut seen = std::collections::BTreeSet::new();
        for id in ParamId::ALL {
            assert!(seen.insert(id.to_u16()), "duplicate wire id for {id:?}");
        }
        assert_eq!(ParamId::from_u16(9999), None);
    }

    /// T1: every `ParamId` maps to exactly one group (total; the exhaustive
    /// match makes "exactly one" a compile-time guarantee).
    #[test]
    fn group_of_is_total() {
        for id in ParamId::ALL {
            let _ = group_of(id);
        }
    }

    #[test]
    fn param_id_serializes_as_u16() {
        // CBOR key form must be the numeric wire id, not the variant name.
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&ParamId::Exposure, &mut buf).unwrap();
        let v: ciborium::value::Value = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(v.as_integer(), Some(20.into()));
        let back: ParamId = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back, ParamId::Exposure);
    }

    #[test]
    fn delta_merge_last_wins_and_restrict_in_subset() {
        let mut a = ParamDelta::new();
        a.0.insert(ParamId::Exposure, ParamValue::F32(1.0));
        a.0.insert(ParamId::Contrast, ParamValue::F32(5.0));
        let mut b = ParamDelta::new();
        b.0.insert(ParamId::Exposure, ParamValue::F32(2.0));
        a.merge(b);
        assert_eq!(a.0.get(&ParamId::Exposure), Some(&ParamValue::F32(2.0)));
        assert_eq!(a.0.get(&ParamId::Contrast), Some(&ParamValue::F32(5.0)));

        let subset = ParamSubset::from_groups([ParamGroup::Tone]);
        let r = a.restrict(&subset);
        for id in r.0.keys() {
            assert!(subset.contains(*id));
        }
        // Both exposure and contrast are Tone → both survive.
        assert_eq!(r.len(), 2);
        let empty = a.restrict(&ParamSubset::from_groups([ParamGroup::Masks]));
        assert!(empty.is_empty());
    }
}
