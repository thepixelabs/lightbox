// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Non-raw decode (A7, spec §3.1 `decode_image` / §5.3): JPEG (`zune-jpeg`),
//! PNG (`png`), TIFF (`tiff`) → [`SourceImage`]. All three codecs are
//! permissive (MIT/Apache-2.0); no LGPL touches this path.
//!
//! Output is **display-referred** samples normalized to `f32` `[0, 1]` — NOT
//! linearized here. The embedded ICC is carried as [`SourceColor::Tagged`]
//! (untagged ⇒ [`SourceColor::AssumedSrgb`], spec §5.3); `lightbox-color` runs
//! the LCMS2 conversion to the working space downstream. EXIF orientation is
//! baked into the pixels here (the pipeline downstream is orientation-agnostic
//! for non-raw sources).
//!
//! HEIC (A8) is DEFERRED behind the off-by-default `heic` feature (libheif is
//! absent on the build machine); the default build never links it.

mod jpeg;
mod orientation;
mod png;
mod tiff;

use std::path::Path;

use lightbox_types::Orientation;

use crate::error::DecodeError;
use crate::raw::state::decode_params_hash;
use crate::raw::types::{BackendPolicy, DecodeBackend, SourceColor, SourceImage, SourceProvenance};

/// A codec's decode result before orientation is applied and provenance is
/// attached. `data` is already normalized to `f32` `[0, 1]`, interleaved.
pub(crate) struct RawDecoded {
    pub data: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub icc: Option<Vec<u8>>,
    pub backend: DecodeBackend,
}

/// Body behind [`crate::decode_image`] (wrapped by the panic guard in
/// `lib.rs`). Never panics; malformed input is a structured [`DecodeError`].
pub(crate) fn decode_image_impl(path: &Path) -> Result<SourceImage, DecodeError> {
    let bytes = std::fs::read(path)?;
    let head = &bytes[..bytes.len().min(16)];

    let decoded = if head.len() >= 3 && head[..3] == [0xFF, 0xD8, 0xFF] {
        jpeg::decode(&bytes)?
    } else if head.len() >= 8 && head[..8] == png::PNG_SIGNATURE {
        png::decode(&bytes)?
    } else if head.len() >= 2 && (head[..2] == *b"II" || head[..2] == *b"MM") {
        tiff::decode(&bytes)?
    } else {
        return Err(DecodeError::UnsupportedFormat {
            format: path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_owned()),
        });
    };

    // EXIF orientation: kamadak reads JPEG/TIFF containers; PNG has none.
    let orientation = exif_orientation(path).unwrap_or(Orientation::O1);
    let (data, width, height) = orientation::apply(
        decoded.data,
        decoded.width,
        decoded.height,
        decoded.channels,
        orientation,
    );

    let color = match decoded.icc {
        Some(bytes) if !bytes.is_empty() => SourceColor::Tagged(bytes),
        _ => SourceColor::AssumedSrgb,
    };

    Ok(SourceImage {
        data,
        width,
        height,
        channels: decoded.channels,
        color,
        provenance: SourceProvenance {
            backend: decoded.backend,
            interim_demosaic: false,
            decode_params_hash: decode_params_hash(BackendPolicy::ForceInCrate, false, None),
        },
    })
}

/// Reads EXIF orientation from a container kamadak understands (JPEG/TIFF).
/// Any read/parse failure (including PNG, which carries no EXIF) is `None`.
fn exif_orientation(path: &Path) -> Option<Orientation> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    let v = field.value.get_uint(0)?;
    Orientation::from_exif(u16::try_from(v).ok()?)
}

/// Normalizes an 8- or 16-bit sample buffer to `f32` `[0, 1]`. 16-bit input is
/// big-endian byte pairs (`png`); the `tiff` path hands native `u16` slices to
/// [`normalize_u16`] directly, preserving full precision.
pub(crate) fn normalize_u8(data: &[u8]) -> Vec<f32> {
    data.iter().map(|&b| f32::from(b) / 255.0).collect()
}

/// Normalizes native `u16` samples to `f32` `[0, 1]` (full 16-bit precision).
pub(crate) fn normalize_u16(data: &[u16]) -> Vec<f32> {
    data.iter().map(|&s| f32::from(s) / 65535.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_helpers() {
        assert_eq!(normalize_u8(&[0, 255])[0], 0.0);
        assert!((normalize_u8(&[0, 255])[1] - 1.0).abs() < 1e-6);
        assert_eq!(normalize_u16(&[0, 65535])[0], 0.0);
        assert!((normalize_u16(&[0, 65535])[1] - 1.0).abs() < 1e-6);
        // 16-bit midpoint keeps precision an 8-bit path would lose.
        assert!((normalize_u16(&[32768])[0] - 0.5000076).abs() < 1e-4);
    }

    #[test]
    fn unknown_bytes_are_unsupported_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, b"not an image at all").unwrap();
        assert!(matches!(
            decode_image_impl(&p),
            Err(DecodeError::UnsupportedFormat { .. })
        ));
    }
}
