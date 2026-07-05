// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Output transforms for export (E15 seam, spec §3.5). **Owner: Phase D (D6).**
//! Working → output space at 8/16-bit int + f32, exact LCMS2 path (never the
//! baked LUT), with embeddable profile bytes.
//!
//! The source is always the fixed working space (ProPhoto/ROMM primaries,
//! linear, D50). Each [`OutputTransform`] holds one Little-CMS2 transform built
//! for a specific destination space **and bit depth**, plus the destination's
//! ICC bytes for embedding. The transform is cache-disabled, which makes it
//! `Send + Sync` so E15 can apply it across a rayon export pool.

#![allow(unsafe_code)]

use lcms2::{DisallowCache, Flags, GlobalContext, PixelFormat, Transform};

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
/// The variant must match the [`BitDepth`] the transform was built with.
pub enum OutputBuffer {
    /// 8-bit interleaved.
    Int8(Vec<u8>),
    /// 16-bit interleaved.
    Int16(Vec<u16>),
    /// f32 interleaved.
    F32(Vec<f32>),
}

/// The per-depth Little-CMS2 transform. Input pixels are always
/// working-space linear RGB (`[f32; 3]`); the output pixel type matches the
/// requested depth. Cache-disabled → `Send + Sync`.
enum Baked {
    Int8(Transform<[f32; 3], [u8; 3], GlobalContext, DisallowCache>),
    Int16(Transform<[f32; 3], [u16; 3], GlobalContext, DisallowCache>),
    F32(Transform<[f32; 3], [f32; 3], GlobalContext, DisallowCache>),
}

/// Working → output-space transform for export (spec §3.5). **Owner: Phase D
/// (D6).** Exact LCMS2 path.
pub struct OutputTransform {
    baked: Baked,
    depth: BitDepth,
    profile_bytes: Vec<u8>,
}

impl OutputTransform {
    /// Builds an output transform (spec §3.5). **Phase D (D6).** The source is
    /// the fixed working space; `dest`/`depth`/`intent` pick the destination
    /// profile, output pixel format, and rendering intent (rel-colorimetric
    /// carries black-point compensation).
    pub fn new(
        dest: OutputSpace,
        depth: BitDepth,
        intent: crate::display::Intent,
    ) -> Result<Self, IccError> {
        let working = crate::cms::working_linear();
        let dest_profile = match dest {
            OutputSpace::Srgb => IccProfile::srgb(),
            OutputSpace::AdobeRgb => crate::cms::adobe_rgb(),
            OutputSpace::ProPhoto => crate::cms::prophoto(),
            OutputSpace::DisplayP3 => IccProfile::display_p3(),
            OutputSpace::Rec2020 => crate::cms::rec2020(),
            OutputSpace::UserIcc(p) => p,
        };
        let profile_bytes = dest_profile.to_icc_bytes()?;

        let (lcms_intent, bpc) = intent.to_lcms();
        let flags = if bpc {
            Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION
        } else {
            Flags::NO_CACHE
        };
        let src = working.as_lcms();
        let dst = dest_profile.as_lcms();

        let baked = match depth {
            BitDepth::Int8 => Baked::Int8(
                Transform::new_flags_context(
                    GlobalContext::new(),
                    src,
                    PixelFormat::RGB_FLT,
                    dst,
                    PixelFormat::RGB_8,
                    lcms_intent,
                    flags,
                )
                .map_err(|e| IccError::TransformBuild(e.to_string()))?,
            ),
            BitDepth::Int16 => Baked::Int16(
                Transform::new_flags_context(
                    GlobalContext::new(),
                    src,
                    PixelFormat::RGB_FLT,
                    dst,
                    PixelFormat::RGB_16,
                    lcms_intent,
                    flags,
                )
                .map_err(|e| IccError::TransformBuild(e.to_string()))?,
            ),
            BitDepth::F32 => Baked::F32(
                Transform::new_flags_context(
                    GlobalContext::new(),
                    src,
                    PixelFormat::RGB_FLT,
                    dst,
                    PixelFormat::RGB_FLT,
                    lcms_intent,
                    flags,
                )
                .map_err(|e| IccError::TransformBuild(e.to_string()))?,
            ),
        };

        Ok(OutputTransform {
            baked,
            depth,
            profile_bytes,
        })
    }

    /// The bit depth this transform emits.
    #[must_use]
    pub fn depth(&self) -> BitDepth {
        self.depth
    }

    /// Applies the transform to working-space linear samples (spec §3.5).
    ///
    /// `working_linear` is interleaved RGB (`len` a multiple of 3). `out` is
    /// resized to `len` and filled; its variant **must** match [`Self::depth`]
    /// (mismatched buffers are left untouched — a caller contract violation).
    pub fn apply(&self, working_linear: &[f32], out: &mut OutputBuffer) {
        let n = working_linear.len() / 3;
        // SAFETY: `[f32; 3]` has the same size/alignment layout as three
        // contiguous `f32`, so a flat interleaved slice reinterprets to a slice
        // of pixels of length `n` without copying.
        let src: &[[f32; 3]] =
            unsafe { std::slice::from_raw_parts(working_linear.as_ptr().cast::<[f32; 3]>(), n) };

        match (&self.baked, out) {
            (Baked::Int8(x), OutputBuffer::Int8(buf)) => {
                buf.resize(n * 3, 0);
                // SAFETY: `[u8; 3]` reinterprets a length-`3n` `u8` buffer as
                // `n` pixels (alignment 1, exact size match).
                let dst: &mut [[u8; 3]] = unsafe {
                    std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<[u8; 3]>(), n)
                };
                x.transform_pixels(src, dst);
            }
            (Baked::Int16(x), OutputBuffer::Int16(buf)) => {
                buf.resize(n * 3, 0);
                // SAFETY: `[u16; 3]` reinterprets a length-`3n` `u16` buffer
                // (alignment 2, exact size match).
                let dst: &mut [[u16; 3]] = unsafe {
                    std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<[u16; 3]>(), n)
                };
                x.transform_pixels(src, dst);
            }
            (Baked::F32(x), OutputBuffer::F32(buf)) => {
                buf.resize(n * 3, 0.0);
                // SAFETY: `[f32; 3]` reinterprets a length-`3n` `f32` buffer
                // (alignment 4, exact size match).
                let dst: &mut [[f32; 3]] = unsafe {
                    std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<[f32; 3]>(), n)
                };
                x.transform_pixels(src, dst);
            }
            _ => panic!(
                "OutputBuffer variant does not match the transform bit depth ({:?})",
                self.depth
            ),
        }
    }

    /// The destination profile bytes, for embedding in exported files.
    #[must_use]
    pub fn profile_bytes(&self) -> &[u8] {
        &self.profile_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Intent;

    #[test]
    fn output_transform_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OutputTransform>();
    }

    #[test]
    fn every_space_builds_and_emits_valid_icc() {
        let spaces = [
            OutputSpace::Srgb,
            OutputSpace::AdobeRgb,
            OutputSpace::ProPhoto,
            OutputSpace::DisplayP3,
            OutputSpace::Rec2020,
            OutputSpace::UserIcc(IccProfile::srgb()),
        ];
        for space in spaces {
            let ot = OutputTransform::new(space, BitDepth::Int16, Intent::RelColorimetric)
                .expect("build output transform");
            // The embedded ICC bytes must re-parse as a valid profile (proxy for
            // "validate in external tooling", D6).
            IccProfile::from_bytes(ot.profile_bytes()).expect("emitted ICC is well-formed");
        }
    }

    #[test]
    fn depth_paths_produce_expected_extremes() {
        // White and black through working→sRGB at each depth.
        let input: Vec<f32> = vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0];

        let ot8 = OutputTransform::new(OutputSpace::Srgb, BitDepth::Int8, Intent::RelColorimetric)
            .unwrap();
        let mut b8 = OutputBuffer::Int8(Vec::new());
        ot8.apply(&input, &mut b8);
        let OutputBuffer::Int8(v8) = &b8 else {
            unreachable!()
        };
        assert_eq!(v8.len(), 6);
        assert!(v8[0] >= 254, "sRGB white 8-bit ≈ 255, got {}", v8[0]);
        assert!(v8[3] <= 1, "sRGB black 8-bit ≈ 0, got {}", v8[3]);

        let ot16 =
            OutputTransform::new(OutputSpace::Srgb, BitDepth::Int16, Intent::RelColorimetric)
                .unwrap();
        let mut b16 = OutputBuffer::Int16(Vec::new());
        ot16.apply(&input, &mut b16);
        let OutputBuffer::Int16(v16) = &b16 else {
            unreachable!()
        };
        assert!(
            v16[0] >= 65_400,
            "sRGB white 16-bit ≈ 65535, got {}",
            v16[0]
        );
        assert!(v16[3] <= 64, "sRGB black 16-bit ≈ 0, got {}", v16[3]);

        let otf = OutputTransform::new(OutputSpace::Srgb, BitDepth::F32, Intent::RelColorimetric)
            .unwrap();
        let mut bf = OutputBuffer::F32(Vec::new());
        otf.apply(&input, &mut bf);
        let OutputBuffer::F32(vf) = &bf else {
            unreachable!()
        };
        assert!(
            (vf[0] - 1.0).abs() < 1e-3,
            "sRGB white f32 ≈ 1.0, got {}",
            vf[0]
        );
        assert!(vf[3].abs() < 1e-3, "sRGB black f32 ≈ 0.0, got {}", vf[3]);
    }

    #[test]
    fn working_srgb_round_trip_is_tight_for_in_gamut() {
        use lcms2::{Intent as LIntent, Transform};

        // Forward: working → sRGB (f32). Reverse: sRGB → working (f32).
        let fwd = OutputTransform::new(OutputSpace::Srgb, BitDepth::F32, Intent::RelColorimetric)
            .unwrap();
        let working = crate::cms::working_linear();
        let srgb = IccProfile::srgb();
        let rev: Transform<[f32; 3], [f32; 3]> = Transform::new(
            srgb.as_lcms(),
            PixelFormat::RGB_FLT,
            working.as_lcms(),
            PixelFormat::RGB_FLT,
            LIntent::RelativeColorimetric,
        )
        .unwrap();

        // Near-neutral / low-saturation samples that sit inside the sRGB gamut,
        // so the forward transform does not clip and the round-trip is tight.
        let samples: Vec<[f32; 3]> = vec![
            [0.05, 0.05, 0.05],
            [0.2, 0.2, 0.2],
            [0.5, 0.5, 0.5],
            [0.9, 0.9, 0.9],
            [0.4, 0.42, 0.38],
            [0.6, 0.55, 0.5],
        ];
        for s in samples {
            let flat = vec![s[0], s[1], s[2]];
            let mut fb = OutputBuffer::F32(Vec::new());
            fwd.apply(&flat, &mut fb);
            let OutputBuffer::F32(enc) = &fb else {
                unreachable!()
            };
            let mut back = [[0.0f32; 3]];
            rev.transform_pixels(&[[enc[0], enc[1], enc[2]]], &mut back);
            for c in 0..3 {
                let d = (back[0][c] - s[c]).abs();
                assert!(
                    d < 3e-3,
                    "round-trip {s:?} channel {c}: back={} Δ={d}",
                    back[0][c]
                );
            }
        }
    }
}
