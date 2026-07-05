// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! White-balance model + preset table (spec §3.3). **Owner: Phase B (B5).**
//! The neutral↔temp/tint solver itself lives on
//! [`ColorimetricSolver`](crate::profile::ColorimetricSolver) (it needs the
//! profile's matrices); this module owns the mode/preset vocabulary, the
//! resolved white point, and the preset → `(CCT, tint)` table.

/// How white balance is specified (spec §3.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WbMode {
    /// Use the file's as-shot neutral.
    AsShot,
    /// A `(Kelvin, tint)` pair.
    TempTint {
        /// Correlated color temperature (Kelvin).
        kelvin: f64,
        /// Green–magenta tint, orthogonal to CCT.
        tint: f64,
    },
    /// A camera-native neutral triple.
    Neutral([f64; 3]),
}

/// A solved white point: the camera-native neutral and its `(CCT, tint)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WhitePoint {
    /// Camera-native neutral RGB.
    pub neutral: [f64; 3],
    /// Correlated color temperature (Kelvin).
    pub cct: f64,
    /// Tint.
    pub tint: f64,
}

/// The named WB presets E10's preset menu offers (spec §3.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WbPreset {
    /// ~5500 K daylight.
    Daylight,
    /// Overcast.
    Cloudy,
    /// Open shade.
    Shade,
    /// Incandescent/tungsten.
    Tungsten,
    /// Fluorescent.
    Fluorescent,
    /// Electronic flash.
    Flash,
}

/// The preset → `(CCT Kelvin, tint)` table (spec §3.3, Open Question 3).
///
/// Values are the correlated color temperatures of published CIE standard
/// illuminants — **sourced, not invented**:
///
/// | preset | illuminant | CCT (K) |
/// |--------|------------|---------|
/// | Daylight | D55 | 5503 |
/// | Cloudy | D65-class overcast | 6504 |
/// | Shade | D75 | 7504 |
/// | Tungsten | Standard A | 2856 |
/// | Fluorescent | F2 (cool white) | 4230 |
/// | Flash | electronic flash ≈ D55 | 5503 |
///
/// Tint is `0` for every preset: each row is the locus point at the
/// illuminant's CCT. (F2 sits marginally off the Planckian locus toward green;
/// its exact tint needs the F2 spectral power distribution, which is out of
/// Phase B scope — E10 may refine per body. Recorded in `E02-deviations.md`.)
pub fn wb_presets() -> &'static [(WbPreset, f64, f64)] {
    &[
        (WbPreset::Daylight, 5503.0, 0.0),
        (WbPreset::Cloudy, 6504.0, 0.0),
        (WbPreset::Shade, 7504.0, 0.0),
        (WbPreset::Tungsten, 2856.0, 0.0),
        (WbPreset::Fluorescent, 4230.0, 0.0),
        (WbPreset::Flash, 5503.0, 0.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_cover_all_six_with_sourced_ccts() {
        let p = wb_presets();
        assert_eq!(p.len(), 6);
        let find = |k: WbPreset| {
            p.iter()
                .find(|(pk, _, _)| *pk == k)
                .map(|(_, c, t)| (*c, *t))
        };
        assert_eq!(find(WbPreset::Tungsten), Some((2856.0, 0.0)));
        assert_eq!(find(WbPreset::Daylight), Some((5503.0, 0.0)));
        assert_eq!(find(WbPreset::Shade), Some((7504.0, 0.0)));
        assert_eq!(find(WbPreset::Fluorescent), Some((4230.0, 0.0)));
        // Every preset CCT is a plausible lighting temperature.
        for (_, cct, _) in p {
            assert!((2000.0..=9000.0).contains(cct));
        }
    }
}
