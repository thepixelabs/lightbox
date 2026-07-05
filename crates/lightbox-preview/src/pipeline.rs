// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The embedded-preview decode pipeline (spec §5 T20): pick a probed
//! embedded JPEG → ranged read → `zune-jpeg` decode to RGBA8 →
//! `fast_image_resize` downscale (thumbs) → CPU EXIF-orientation bake
//! (thumbs only — the loupe source stays unrotated; the display-transform
//! node applies orientation, spec §3.6).
//!
//! Pure functions, no scheduling — [`crate::EmbeddedPreviewProvider`] (T21)
//! owns jobs/dedup/caching on top of this.

use std::path::Path;
use std::sync::Arc;

use lightbox_decode::{read_embedded, AssetProbe, EmbeddedPreviewInfo, ProbeError};
use lightbox_jobs::CancelToken;
use lightbox_types::{Orientation, SourceTier};

use crate::{DecodedImage, PreviewClass, PreviewError};

/// Below this long-edge size an embedded preview is "tiny" and worthless
/// beyond micro-thumbnails: the provider reports
/// [`PreviewError::NoEmbedded`] instead of pretending (spec T20 AC — the
/// Olympus E-1 fixture with its 160 px thumbnail is the canonical case),
/// unless the request itself asks for less.
pub(crate) const MIN_USABLE_LONG_EDGE: u32 = 256;

/// Guard against absurd probe ranges: an embedded preview larger than this
/// is treated as malformed rather than read into memory.
const MAX_EMBEDDED_BYTES: u64 = 256 * 1024 * 1024;

impl From<ProbeError> for PreviewError {
    fn from(e: ProbeError) -> PreviewError {
        match e {
            ProbeError::Cancelled => PreviewError::Cancelled,
            ProbeError::Io(io) => PreviewError::Io(io.to_string()),
            ProbeError::Malformed(m) => PreviewError::Decode(m),
            other => PreviewError::Decode(other.to_string()),
        }
    }
}

/// Selects which embedded preview serves `class` (probe lists are sorted
/// largest-first). Thumbs pick the *smallest* rendition that still covers
/// the requested edge (cheapest decode); the loupe picks the largest.
/// `None` when every rendition is unusably tiny (or there are none).
///
/// "Tiny" only disqualifies *surrogates*: a rendition that covers the
/// original's full dimensions IS the image (a small JPEG original serves
/// itself at any size), while a 160 px thumbnail inside a 5 MP raw is a
/// surrogate nobody should loupe (T20 AC, the E-1 fixture).
pub(crate) fn select_preview(
    probe: &AssetProbe,
    class: PreviewClass,
) -> Option<&EmbeddedPreviewInfo> {
    let long_edge = |p: &&EmbeddedPreviewInfo| p.width.max(p.height);
    let largest = probe.embedded.first()?;
    let covers_full = probe.width > 0
        && probe.height > 0
        && largest.width >= probe.width
        && largest.height >= probe.height;
    let need = match class {
        PreviewClass::Loupe => MIN_USABLE_LONG_EDGE,
        PreviewClass::Thumb { max_px } => max_px.min(MIN_USABLE_LONG_EDGE),
    };
    if !covers_full && long_edge(&largest) < need {
        return None; // no/tiny embedded preview → NoEmbedded (T20 AC)
    }
    match class {
        PreviewClass::Loupe => Some(largest),
        PreviewClass::Thumb { max_px } => probe
            .embedded
            .iter()
            .filter(|p| long_edge(p) >= max_px)
            .min_by_key(|p| u64::from(p.width) * u64::from(p.height))
            .or(Some(largest)),
    }
}

/// Runs the whole pipeline for one request. Checkpoints on `cancel` between
/// stages (spec §3.6: cancel wired through).
pub(crate) fn decode_class(
    path: &Path,
    orientation: Orientation,
    class: PreviewClass,
    cancel: &CancelToken,
) -> Result<DecodedImage, PreviewError> {
    let check = |c: &CancelToken| -> Result<(), PreviewError> {
        if c.is_cancelled() {
            Err(PreviewError::Cancelled)
        } else {
            Ok(())
        }
    };

    check(cancel)?;
    let probe = lightbox_decode::probe(path)?;
    let info = select_preview(&probe, class)
        .ok_or(PreviewError::NoEmbedded)?
        .clone();
    let range_len = info.byte_range.end.saturating_sub(info.byte_range.start);
    if range_len > MAX_EMBEDDED_BYTES {
        return Err(PreviewError::Decode(format!(
            "embedded preview claims {range_len} bytes"
        )));
    }

    check(cancel)?;
    let jpeg = read_embedded(path, &info)?;

    check(cancel)?;
    let (px, w, h) = decode_jpeg_rgba(&jpeg)?;
    drop(jpeg);

    check(cancel)?;
    match class {
        PreviewClass::Loupe => Ok(DecodedImage {
            px: Arc::from(px.into_boxed_slice()),
            width: w,
            height: h,
            orientation_applied: false, // the display-transform node applies it
            tier: SourceTier::EmbeddedPreview,
        }),
        PreviewClass::Thumb { max_px } => {
            let (px, w, h) = resize_to_fit(px, w, h, max_px)?;
            check(cancel)?;
            let (px, w, h) = bake_orientation(&px, w, h, orientation);
            Ok(DecodedImage {
                px: Arc::from(px.into_boxed_slice()),
                width: w,
                height: h,
                orientation_applied: true,
                tier: SourceTier::EmbeddedPreview,
            })
        }
    }
}

/// `zune-jpeg` decode straight to RGBA8 (sRGB stays sRGB; JPEG has no alpha
/// so A is opaque).
pub(crate) fn decode_jpeg_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder =
        zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options);
    let px = decoder
        .decode()
        .map_err(|e| PreviewError::Decode(e.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| PreviewError::Decode("JPEG decoded without header info".into()))?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let expect = (w as usize) * (h as usize) * 4;
    if px.len() != expect {
        return Err(PreviewError::Decode(format!(
            "decoder returned {} bytes for {w}x{h} RGBA",
            px.len()
        )));
    }
    Ok((px, w, h))
}

/// Downscales so the longest edge fits `max_px` (never upscales), bilinear
/// in the JPEG's own encoding (spec §3.6 names `fast_image_resize`;
/// perceptually-exact linear-light resampling is the render node's job).
pub(crate) fn resize_to_fit(
    px: Vec<u8>,
    w: u32,
    h: u32,
    max_px: u32,
) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    use fast_image_resize::images::Image;
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

    let max_px = max_px.max(1);
    let long = w.max(h);
    if long <= max_px {
        return Ok((px, w, h));
    }
    let scale = f64::from(max_px) / f64::from(long);
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);

    let src = Image::from_vec_u8(w, h, px, PixelType::U8x4)
        .map_err(|e| PreviewError::Decode(format!("resize source: {e}")))?;
    let mut dst = Image::new(tw, th, PixelType::U8x4);
    Resizer::new()
        .resize(
            &src,
            &mut dst,
            &ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                // JPEG previews are opaque; skip the premultiply round-trip.
                .use_alpha(false),
        )
        .map_err(|e| PreviewError::Decode(format!("resize: {e}")))?;
    Ok((dst.into_vec(), tw, th))
}

/// Bakes an EXIF orientation into RGBA8 pixels on the CPU (thumbnails only).
/// Returns the transformed buffer and its (possibly transposed) dimensions.
pub(crate) fn bake_orientation(px: &[u8], w: u32, h: u32, o: Orientation) -> (Vec<u8>, u32, u32) {
    if o == Orientation::O1 {
        return (px.to_vec(), w, h);
    }
    let (sw, sh) = (w as usize, h as usize);
    let (dw, dh) = if o.transposes() { (sh, sw) } else { (sw, sh) };
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        for dx in 0..dw {
            // EXIF orientation describes how the *stored* pixels map onto
            // the scene; these are the standard inverse transforms
            // (destination pixel ← source pixel).
            let (sx, sy) = match o {
                Orientation::O1 => (dx, dy),
                Orientation::O2 => (sw - 1 - dx, dy), // mirror H
                Orientation::O3 => (sw - 1 - dx, sh - 1 - dy), // rotate 180
                Orientation::O4 => (dx, sh - 1 - dy), // mirror V
                Orientation::O5 => (dy, dx),          // transpose
                Orientation::O6 => (dy, sh - 1 - dx), // rotate 90 CW
                Orientation::O7 => (sw - 1 - dy, sh - 1 - dx), // transverse
                Orientation::O8 => (sw - 1 - dy, dx), // rotate 270 CW
            };
            let s = (sy * sw + sx) * 4;
            let d = (dy * dw + dx) * 4;
            out[d..d + 4].copy_from_slice(&px[s..s + 4]);
        }
    }
    (out, dw as u32, dh as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x3 source, one distinct color per pixel (values = row*10+col).
    /// Layout (rows of RGBA where only R varies):
    /// ```text
    ///   0  1
    ///  10 11
    ///  20 21
    /// ```
    fn src_2x3() -> Vec<u8> {
        let mut px = Vec::new();
        for r in [0u8, 1, 10, 11, 20, 21] {
            px.extend_from_slice(&[r, 0, 0, 255]);
        }
        px
    }

    fn reds(px: &[u8]) -> Vec<u8> {
        px.chunks_exact(4).map(|c| c[0]).collect()
    }

    #[test]
    fn orientation_bakes_match_exif_semantics() {
        let px = src_2x3();
        let cases: &[(Orientation, (u32, u32), &[u8])] = &[
            (Orientation::O1, (2, 3), &[0, 1, 10, 11, 20, 21]),
            (Orientation::O2, (2, 3), &[1, 0, 11, 10, 21, 20]),
            (Orientation::O3, (2, 3), &[21, 20, 11, 10, 1, 0]),
            (Orientation::O4, (2, 3), &[20, 21, 10, 11, 0, 1]),
            (Orientation::O5, (3, 2), &[0, 10, 20, 1, 11, 21]),
            // Stored rotated 90 CW to display upright: last row first column.
            (Orientation::O6, (3, 2), &[20, 10, 0, 21, 11, 1]),
            (Orientation::O7, (3, 2), &[21, 11, 1, 20, 10, 0]),
            (Orientation::O8, (3, 2), &[1, 11, 21, 0, 10, 20]),
        ];
        for (o, dims, expect) in cases {
            let (out, w, h) = bake_orientation(&px, 2, 3, *o);
            assert_eq!((w, h), *dims, "{o:?} dims");
            assert_eq!(reds(&out), *expect, "{o:?} pixels");
            assert_eq!(out.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn resize_only_downscales() {
        // 8x4 red-opaque source.
        let px = vec![200u8; 8 * 4 * 4];
        let (out, w, h) = resize_to_fit(px.clone(), 8, 4, 4).unwrap();
        assert_eq!((w, h), (4, 2));
        assert_eq!(out.len(), 4 * 2 * 4);
        // Flat input stays flat through a bilinear kernel.
        assert!(out.chunks_exact(4).all(|c| c == [200, 200, 200, 200]));
        // Already small enough: untouched.
        let (out, w, h) = resize_to_fit(px.clone(), 8, 4, 8).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(out, px);
    }

    #[test]
    fn selection_prefers_cheapest_covering_rendition() {
        use std::ops::Range;
        let info = |w: u32, h: u32, range: Range<u64>| EmbeddedPreviewInfo {
            width: w,
            height: h,
            byte_range: range,
        };
        let probe = AssetProbe {
            format: lightbox_decode::ProbedFormat::Raw("CR3"),
            width: 6000,
            height: 4000,
            orientation: Orientation::O1,
            camera_make: None,
            camera_model: None,
            capture_time: None,
            file_bytes: 0,
            embedded: vec![
                info(6000, 4000, 0..10),
                info(1620, 1080, 10..20),
                info(160, 120, 20..30),
            ],
        };
        // Loupe: the largest.
        let sel = select_preview(&probe, PreviewClass::Loupe).unwrap();
        assert_eq!(sel.width, 6000);
        // Thumb 512: the smallest rendition covering 512 px.
        let sel = select_preview(&probe, PreviewClass::Thumb { max_px: 512 }).unwrap();
        assert_eq!(sel.width, 1620);
        // Thumb larger than everything but the full-size: falls to it.
        let sel = select_preview(&probe, PreviewClass::Thumb { max_px: 2000 }).unwrap();
        assert_eq!(sel.width, 6000);

        // Tiny-only corpus (the E-1 case): nothing usable.
        let tiny = AssetProbe {
            embedded: vec![info(160, 120, 0..10)],
            ..probe
        };
        assert!(select_preview(&tiny, PreviewClass::Loupe).is_none());
        assert!(select_preview(&tiny, PreviewClass::Thumb { max_px: 256 }).is_none());
        // …unless the request itself is micro (a 128 px grid cell).
        assert!(select_preview(&tiny, PreviewClass::Thumb { max_px: 128 }).is_some());

        // A tiny JPEG *original* (preview == full image) serves itself.
        let tiny_original = AssetProbe {
            width: 16,
            height: 16,
            embedded: vec![info(16, 16, 0..826)],
            format: lightbox_decode::ProbedFormat::Jpeg,
            orientation: Orientation::O1,
            camera_make: None,
            camera_model: None,
            capture_time: None,
            file_bytes: 826,
        };
        assert!(select_preview(&tiny_original, PreviewClass::Loupe).is_some());
        assert!(select_preview(&tiny_original, PreviewClass::Thumb { max_px: 512 }).is_some());

        // No previews at all (the Sigma fp DNG case).
        let none = AssetProbe {
            embedded: vec![],
            ..tiny
        };
        assert!(select_preview(&none, PreviewClass::Loupe).is_none());
    }
}
