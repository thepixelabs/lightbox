// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! White-balance model + preset table (spec §3.3). **Owner: Phase B (B5).**
//! The neutral↔temp/tint solver itself lives on
//! [`ColorimetricSolver`](crate::profile::ColorimetricSolver) (it needs the
//! profile's matrices); this module owns the mode/preset vocabulary and the
//! resolved white point.

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

/// The preset → `(CCT, tint)` table (spec §3.3, Open Question 3: the
/// fluorescent/flash values are sourced from published standard illuminants,
/// committed as data — **Phase B (B5)** fills the real numbers).
pub fn wb_presets() -> &'static [(WbPreset, f64, f64)] {
    unimplemented!("B5: WB preset CCT/tint table (published illuminant values)")
}
