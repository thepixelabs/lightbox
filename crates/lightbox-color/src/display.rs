// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Display transforms (spec §3.5). **Owner: Phase D (D3/D5).** Working →
//! per-monitor ICC, baked to a shaper + 3D LUT for GPU application, with the
//! exact LCMS2 path retained for the fidelity gate (D4).

use crate::cms::IccProfile;
use crate::error::IccError;
use crate::transform::Curve1D;

/// A monitor identity the shell supplies (spec §3.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MonitorId(pub u64);

/// Rendering intent (spec §3.5; default rel-colorimetric + BPC).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    /// Perceptual.
    Perceptual,
    /// Relative colorimetric (with black-point compensation).
    RelColorimetric,
    /// Saturation.
    Saturation,
    /// Absolute colorimetric.
    AbsColorimetric,
}

/// A baked 3D LUT (spec §3.5, 65³ for display transforms).
#[derive(Clone, Debug)]
pub struct Lut3D {
    /// Nodes per axis (e.g. 65).
    pub size: u32,
    /// `size³` RGB entries, row-major (r fastest).
    pub data: Vec<[f32; 3]>,
}

/// Working → display, baked for GPU: 1D shaper + 65³ LUT (spec §3.5). The exact
/// LCMS2 path is kept for tolerance tests (D4).
#[derive(Clone, Debug)]
pub struct DisplayTransform {
    /// 1D shaper curve.
    pub shaper: Curve1D,
    /// 3D LUT.
    pub lut: Lut3D,
    /// Content hash → cache key.
    pub key: u64,
}

/// Per-OS monitor-profile source (spec §3.5). The shell implements this
/// (macOS ColorSync / Windows ICM / Linux best-effort). **Owner: Phase D (D5).**
pub trait DisplayProfileProvider: Send + Sync {
    /// The ICC profile for a monitor, if one is available.
    fn profile_for_monitor(&self, monitor: MonitorId) -> Option<IccProfile>;
}

/// Bakes a working→display transform (spec §3.5). **Phase D (D3)** — identity
/// profile ⇒ identity LUT within 1e-4; bake ≤ 100 ms then cached by key.
pub fn build_display_transform(
    _display: &IccProfile,
    _intent: Intent,
) -> Result<DisplayTransform, IccError> {
    unimplemented!("D3: bake working→display shaper + 65³ LUT")
}
