// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Output transforms for export (E15 seam, spec §3.5). **Owner: Phase D (D6).**
//! Working → output space at 8/16-bit int + f32, exact LCMS2 path (never the
//! baked LUT), with embeddable profile bytes.

use crate::cms::IccProfile;
use crate::error::IccError;

/// A destination export space (spec §3.5).
pub enum OutputSpace {
    /// sRGB.
    Srgb,
    /// Adobe RGB (1998).
    AdobeRgb,
    /// ProPhoto RGB.
    ProPhoto,
    /// Display P3.
    DisplayP3,
    /// Rec. 2020.
    Rec2020,
    /// A user-supplied ICC profile.
    UserIcc(IccProfile),
}

/// Output bit depth (spec §3.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BitDepth {
    /// 8-bit integer.
    Int8,
    /// 16-bit integer.
    Int16,
    /// 32-bit float.
    F32,
}

/// A destination buffer the transform writes into (int or float; spec §3.5).
pub enum OutputBuffer {
    /// 8-bit interleaved.
    Int8(Vec<u8>),
    /// 16-bit interleaved.
    Int16(Vec<u16>),
    /// f32 interleaved.
    F32(Vec<f32>),
}

/// Working → output-space transform for export (spec §3.5). **Owner: Phase D
/// (D6).** Exact LCMS2 path.
pub struct OutputTransform {
    // Phase D: the LCMS2 transform handle + embeddable profile bytes.
    _private: (),
}

impl OutputTransform {
    /// Builds an output transform (spec §3.5). **Phase D (D6).**
    pub fn new(
        _dest: OutputSpace,
        _depth: BitDepth,
        _intent: crate::display::Intent,
    ) -> Result<Self, IccError> {
        unimplemented!("D6: build working→output LCMS2 transform")
    }

    /// Applies the transform to working-space linear samples (spec §3.5).
    pub fn apply(&self, _working_linear: &[f32], _out: &mut OutputBuffer) {
        unimplemented!("D6: apply working→output transform")
    }

    /// The destination profile bytes, for embedding in exported files.
    pub fn profile_bytes(&self) -> &[u8] {
        unimplemented!("D6: embeddable destination profile bytes")
    }
}
