// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! HueSat LUT evaluator, shared by the DCP path and the Lightbox look
//! (spec §3.4). **Owner: Phase B (B6).** Trilinear with hue wrap, Linear/sRGB
//! encodings, `val-dims == 1` fast path.

/// The encoding a HueSat/Look table's saturation/value axes use (DNG
/// `ProfileHueSatMapEncoding` / `ProfileLookTableEncoding`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum HueSatEncoding {
    /// Linear encoding.
    Linear,
    /// sRGB encoding.
    Srgb,
}

/// A HueSatMap / LookTable delta cube (spec §3.4). `deltas` is
/// `dims[0] * dims[1] * dims[2]` entries of `[Δhue°, Δsat×, Δval×]`, indexed
/// hue-major.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HueSatLut {
    /// `[hue_divisions, sat_divisions, val_divisions]`.
    pub dims: [u32; 3],
    /// Per-node deltas.
    pub deltas: Vec<[f32; 3]>,
    /// Axis encoding.
    pub encoding: HueSatEncoding,
}

impl HueSatLut {
    /// Applies the table to an `HSV` sample (spec §3.4). **Phase B (B6)** —
    /// trilinear, hue-wrapped (no seam at 0°/360°), `val-dims == 1` fast path.
    pub fn eval(&self, _hsv: [f32; 3]) -> [f32; 3] {
        unimplemented!("B6: trilinear hue-wrapped HueSat LUT evaluation")
    }
}

/// A HueSat table with its encoding already applied and resampled for GPU
/// upload (the `ResolvedInputTransform` form, spec §3.4).
#[derive(Clone, Debug)]
pub struct HueSatTable {
    /// `[hue_divisions, sat_divisions, val_divisions]`.
    pub dims: [u32; 3],
    /// Resolved (encoding-applied) deltas, upload-ready.
    pub deltas: Vec<[f32; 3]>,
}
