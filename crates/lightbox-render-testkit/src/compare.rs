// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Self-contained f64 Lab / ΔE2000 / PSNR comparators (spec §4.4/§6; task
//! **A16**).
//!
//! Owner: **A-core**. `ciede2000` is validated against the published
//! Sharma-Wu-Dalal ΔE2000 test vectors; the golden tolerance is
//! **ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB** (the §4.4 consistency contract). This is a
//! *reference* implementation (own math, no external color crate) so goldens
//! and cross-backend parity share one ground truth.

/// The consistency-contract ΔE2000 tolerance (§4.4).
pub const TOLERANCE_DELTA_E: f64 = 1.0;

/// The consistency-contract PSNR floor, dB (§4.4).
pub const TOLERANCE_PSNR_DB: f64 = 45.0;

/// A CIE L*a*b* triple (D50), f64 (spec §4.4 reference space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    /// Lightness L*.
    pub l: f64,
    /// a* (green–red).
    pub a: f64,
    /// b* (blue–yellow).
    pub b: f64,
}

/// Aggregate ΔE2000 statistics over an image pair (spec §6 golden row).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeltaEStats {
    /// Mean ΔE2000.
    pub mean: f64,
    /// 99th-percentile ΔE2000.
    pub p99: f64,
    /// Maximum ΔE2000.
    pub max: f64,
}

impl DeltaEStats {
    /// Whether these stats pass the §4.4 tolerance (max ≤ 1.0).
    pub fn within_tolerance(&self) -> bool {
        self.max <= TOLERANCE_DELTA_E
    }
}

/// CIEDE2000 ΔE between two Lab colors (spec §4.4; task A16).
pub fn ciede2000(reference: Lab, sample: Lab) -> f64 {
    let _ = (reference, sample);
    unimplemented!("A16 (A-core): CIEDE2000 — validate against Sharma-Wu-Dalal vectors")
}

/// Convert an sRGB-encoded RGBA8 texel to D50 Lab (spec §4.4; task A16).
pub fn srgb8_to_lab(rgba: [u8; 4]) -> Lab {
    let _ = rgba;
    unimplemented!("A16 (A-core): sRGB8 → Lab reference conversion")
}

/// ΔE2000 stats between two equally-sized RGBA8 images (spec §6; task A16).
pub fn delta_e_stats(reference: &[[u8; 4]], sample: &[[u8; 4]]) -> DeltaEStats {
    let _ = (reference, sample);
    unimplemented!("A16 (A-core): per-pixel ΔE2000 aggregation (mean/p99/max)")
}

/// PSNR (dB) between two equally-sized RGBA8 byte buffers (spec §6; task A16).
pub fn psnr(reference: &[u8], sample: &[u8]) -> f64 {
    let _ = (reference, sample);
    unimplemented!("A16 (A-core): PSNR in dB")
}
