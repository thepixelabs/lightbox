// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Param range table (E10 task **A2**): the single source of truth for the
//! scalar-`f32` [`ParamId`] domains `Recipe::set` already enforces on `apply`
//! (spec §4.3 `ParamRange`). UI sliders (E10 A12) and validation/fuzzing both
//! read `range_of` instead of re-declaring bounds.
//!
//! Non-scalar params (curves, HSL, white balance, …) carry their own
//! structured `clamp()` on their leaf type (`leaves.rs`) and are not modeled
//! here, `range_of` returns `None` for them.

use crate::params::ParamId;

/// A scalar param's domain: `[min, max]`, its neutral/identity value, and a
/// UI step hint (spec A2 `ParamRange`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamRange {
    /// Minimum value (inclusive).
    pub min: f32,
    /// Maximum value (inclusive).
    pub max: f32,
    /// The neutral/identity value.
    pub identity: f32,
    /// A UI step hint (drag/keyboard nudge granularity); not enforced.
    pub step: f32,
}

impl ParamRange {
    /// Clamp `x` into `[min, max]`, mapping non-finite inputs to `min`, the
    /// same untrusted-ingest rule `leaves::clampf` uses (spec A2 "out-of-range
    /// clamps on ingest").
    pub fn clamp(&self, x: f32) -> f32 {
        if x.is_finite() {
            x.clamp(self.min, self.max)
        } else {
            self.min
        }
    }
}

/// The scalar-`f32` domain for `id`, or `None` for a structured (non-scalar)
/// param. Mirrors exactly the bounds `Recipe::set`'s `set_scalar!` clamps use
/// (spec A2, one source of truth for UI + validation + fuzzing).
pub fn range_of(id: ParamId) -> Option<ParamRange> {
    let r = |min: f32, max: f32, identity: f32, step: f32| {
        Some(ParamRange {
            min,
            max,
            identity,
            step,
        })
    };
    match id {
        ParamId::Exposure => r(-5.0, 5.0, 0.0, 0.01),
        ParamId::Contrast => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Highlights => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Shadows => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Whites => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Blacks => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Vibrance => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Saturation => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Clarity => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Texture => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Dehaze => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Defringe => r(0.0, 100.0, 0.0, 1.0),
        ParamId::VignetteCorr => r(-100.0, 100.0, 0.0, 1.0),
        ParamId::Angle => r(-45.0, 45.0, 0.0, 0.1),
        // Structured / non-scalar params: no single [min,max], each leaf type
        // owns its own `clamp()` (leaves.rs).
        ParamId::BaseProfile
        | ParamId::WhiteBalance
        | ParamId::ToneCurve
        | ParamId::Hsl
        | ParamId::ColorGrade
        | ParamId::BwMix
        | ParamId::Treatment
        | ParamId::Sharpen
        | ParamId::NoiseReduction
        | ParamId::LensProfile
        | ParamId::ChromaticAberration
        | ParamId::Crop
        | ParamId::Flip
        | ParamId::Upright
        | ParamId::Transform
        | ParamId::PostCropVignette
        | ParamId::Grain
        | ParamId::CreativeLut
        | ParamId::MaskList
        | ParamId::RetouchList => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_range_matches_the_recipe_clamp_contract() {
        let r = range_of(ParamId::Exposure).expect("exposure is scalar");
        assert_eq!((r.min, r.max, r.identity), (-5.0, 5.0, 0.0));
        assert_eq!(r.clamp(9.0), 5.0);
        assert_eq!(r.clamp(-9.0), -5.0);
        assert_eq!(r.clamp(f32::NAN), -5.0);
    }

    #[test]
    fn every_tone_slider_has_a_symmetric_pm100_range() {
        for id in [
            ParamId::Contrast,
            ParamId::Highlights,
            ParamId::Shadows,
            ParamId::Whites,
            ParamId::Blacks,
        ] {
            let r = range_of(id).unwrap_or_else(|| panic!("{id:?} must be scalar"));
            assert_eq!((r.min, r.max, r.identity), (-100.0, 100.0, 0.0), "{id:?}");
        }
    }

    #[test]
    fn structured_params_have_no_scalar_range() {
        assert_eq!(range_of(ParamId::WhiteBalance), None);
        assert_eq!(range_of(ParamId::ToneCurve), None);
        assert_eq!(range_of(ParamId::Hsl), None);
    }

    /// A2 property gate: fuzzed out-of-range scalar values clamp into
    /// `[min,max]` for every param this table covers.
    #[test]
    fn fuzzed_values_always_clamp_into_range() {
        let probes = [
            f32::MIN,
            f32::MAX,
            -1.0e12,
            1.0e12,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
        ];
        for id in crate::params::ParamId::ALL {
            let Some(r) = range_of(id) else { continue };
            for &p in &probes {
                let c = r.clamp(p);
                assert!(c >= r.min && c <= r.max, "{id:?}: clamp({p}) = {c}");
            }
        }
    }
}
