// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Correlated-color-temperature module (spec §3.3). **Owner: Phase B (B2).**
//! DNG-compatible Planckian/daylight locus + orthogonal tint.

/// `xy` chromaticity → `(CCT Kelvin, tint)` (spec §3.3). **Phase B (B2)**:
/// DNG-compatible locus; A/D50/D65 recovered within ±15 K.
pub fn xy_to_cct_tint(_xy: [f64; 2]) -> (f64, f64) {
    unimplemented!("B2: xy → (CCT, tint) on the DNG-compatible locus")
}

/// `(CCT Kelvin, tint)` → `xy` chromaticity (spec §3.3). **Phase B (B2)** —
/// inverse of [`xy_to_cct_tint`], round-trip within tolerance.
pub fn cct_tint_to_xy(_cct: f64, _tint: f64) -> [f64; 2] {
    unimplemented!("B2: (CCT, tint) → xy")
}
