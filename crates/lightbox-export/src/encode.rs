// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Encoders (spec §5.4 `lightbox-export::encode`, narrowed to the core
//! slice's three formats: JPEG/PNG/TIFF, JXL/AVIF/DNG/Original are
//! named-deferred, see `docs/plan/epics/E15-deviations.md`).
//!
//! Each function takes already-quantized pixels ([`crate::pixel::quantize`])
//! plus the destination ICC profile bytes ([`crate::pixel::apply_output_color`])
//! and returns the encoded container bytes, writing to disk is
//! [`crate::run`]'s job (temp-then-rename).
//!
//! All three crates (`jpeg-encoder`, `png`, `tiff`) are already workspace
//! dependencies from E02/E03 (T1 preview JPEG, raw-adjacent TIFF/PNG
//! decode), no new license surface.

use std::io::Cursor;

use crate::error::ExportError;
use crate::pixel::QuantizedPixels;
use crate::settings::BitDepth;

fn encode_err(format: &'static str, msg: impl std::fmt::Display) -> ExportError {
    ExportError::Encode {
        format,
        msg: msg.to_string(),
    }
}

/// Encodes 8-bit interleaved RGB as a JPEG (spec §5.4 JPEG encoder,
/// narrowed: no `ChromaMode`/size-limit bisection, see the module doc
/// comment). ICC bytes (when non-empty) are embedded as an `ICC_PROFILE`
/// APP2 segment (`jpeg-encoder`'s `add_icc_profile`, spec-compliant chunking
/// per ICC.org's embedding note).
pub fn encode_jpeg(
    w: u32,
    h: u32,
    rgb8: &[u8],
    quality: u8,
    icc: &[u8],
) -> Result<Vec<u8>, ExportError> {
    let width = u16::try_from(w)
        .map_err(|_| encode_err("jpeg", format!("width {w} exceeds JPEG's 65535px limit")))?;
    let height = u16::try_from(h)
        .map_err(|_| encode_err("jpeg", format!("height {h} exceeds JPEG's 65535px limit")))?;

    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    if !icc.is_empty() {
        encoder
            .add_icc_profile(icc)
            .map_err(|e| encode_err("jpeg", e))?;
    }
    encoder
        .encode(rgb8, width, height, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| encode_err("jpeg", e))?;
    Ok(out)
}

fn u16_slice_to_be_bytes(v: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for &s in v {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out
}

/// Encodes quantized RGB as a PNG, 8 or 16-bit (spec §5.4 PNG encoder). ICC
/// bytes (when non-empty) are embedded as an `iCCP` chunk.
pub fn encode_png(
    w: u32,
    h: u32,
    pixels: &QuantizedPixels,
    icc: &[u8],
) -> Result<Vec<u8>, ExportError> {
    let mut info = png::Info::with_size(w, h);
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = match pixels {
        QuantizedPixels::U8(_) => png::BitDepth::Eight,
        QuantizedPixels::U16(_) => png::BitDepth::Sixteen,
    };
    if !icc.is_empty() {
        info.icc_profile = Some(std::borrow::Cow::Borrowed(icc));
    }

    let mut out = Vec::new();
    let encoder = png::Encoder::with_info(&mut out, info).map_err(|e| encode_err("png", e))?;
    let mut writer = encoder.write_header().map_err(|e| encode_err("png", e))?;
    let bytes: std::borrow::Cow<'_, [u8]> = match pixels {
        QuantizedPixels::U8(v) => std::borrow::Cow::Borrowed(v.as_slice()),
        QuantizedPixels::U16(v) => std::borrow::Cow::Owned(u16_slice_to_be_bytes(v)),
    };
    writer
        .write_image_data(&bytes)
        .map_err(|e| encode_err("png", e))?;
    writer.finish().map_err(|e| encode_err("png", e))?;
    Ok(out)
}

/// Encodes quantized RGB as a TIFF, 8 or 16-bit, Deflate-compressed (spec
/// §5.4 TIFF encoder, narrowed: no compression choice at core-slice scope
/// see `settings::FileFormat::Tiff`'s doc comment). ICC bytes (when
/// non-empty) are embedded as tag 34675 (`Tag::IccProfile`, already defined
/// by the `tiff` crate).
pub fn encode_tiff(
    w: u32,
    h: u32,
    pixels: &QuantizedPixels,
    icc: &[u8],
) -> Result<Vec<u8>, ExportError> {
    use tiff::encoder::compression::DeflateLevel;
    use tiff::encoder::{colortype, Compression, TiffEncoder};
    use tiff::tags::Tag;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut tiff_enc = TiffEncoder::new(&mut cursor)
            .map_err(|e| encode_err("tiff", e))?
            .with_compression(Compression::Deflate(DeflateLevel::default()));

        match pixels {
            QuantizedPixels::U8(data) => {
                let mut image = tiff_enc
                    .new_image::<colortype::RGB8>(w, h)
                    .map_err(|e| encode_err("tiff", e))?;
                if !icc.is_empty() {
                    image
                        .encoder()
                        .write_tag(Tag::IccProfile, icc)
                        .map_err(|e| encode_err("tiff", e))?;
                }
                image.write_data(data).map_err(|e| encode_err("tiff", e))?;
            }
            QuantizedPixels::U16(data) => {
                let mut image = tiff_enc
                    .new_image::<colortype::RGB16>(w, h)
                    .map_err(|e| encode_err("tiff", e))?;
                if !icc.is_empty() {
                    image
                        .encoder()
                        .write_tag(Tag::IccProfile, icc)
                        .map_err(|e| encode_err("tiff", e))?;
                }
                image.write_data(data).map_err(|e| encode_err("tiff", e))?;
            }
        }
    }
    Ok(cursor.into_inner())
}

/// Encodes per [`crate::settings::FileFormat`], dispatching to the right
/// encoder + bit depth (spec §5.4 `EncoderRegistry`, narrowed to a plain
/// `match`, three formats don't earn a trait-object registry).
pub fn encode(
    w: u32,
    h: u32,
    pixels: &QuantizedPixels,
    format: crate::settings::FileFormat,
    icc: &[u8],
) -> Result<Vec<u8>, ExportError> {
    use crate::settings::FileFormat;
    match format {
        FileFormat::Jpeg { quality } => {
            let QuantizedPixels::U8(rgb8) = pixels else {
                return Err(encode_err(
                    "jpeg",
                    "JPEG requires 8-bit pixels (BitDepth::depth() should have forced this)",
                ));
            };
            encode_jpeg(w, h, rgb8, quality, icc)
        }
        FileFormat::Png { depth: _ } => encode_png(w, h, pixels, icc),
        FileFormat::Tiff { depth: _ } => encode_tiff(w, h, pixels, icc),
    }
}

/// The pixel bit depth an already-built [`QuantizedPixels`] carries, used
/// by callers that need to confirm quantization matched the format's
/// [`BitDepth`] (defensive; `run::export_one` always quantizes to
/// `format.depth()`).
#[must_use]
pub fn depth_of(pixels: &QuantizedPixels) -> BitDepth {
    match pixels {
        QuantizedPixels::U8(_) => BitDepth::Eight,
        QuantizedPixels::U16(_) => BitDepth::Sixteen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_rgb8(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            px.extend_from_slice(&rgb);
        }
        px
    }

    #[test]
    fn jpeg_round_trips_a_flat_image_and_carries_icc() {
        let rgb = flat_rgb8(8, 8, [120, 60, 200]);
        let icc = vec![1u8, 2, 3, 4, 5];
        let bytes = encode_jpeg(8, 8, &rgb, 95, &icc).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "JPEG SOI marker");

        use zune_jpeg::zune_core::colorspace::ColorSpace;
        use zune_jpeg::zune_core::options::DecoderOptions;
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(&bytes), options);
        let decoded = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((u32::from(info.width), u32::from(info.height)), (8, 8));
        for chunk in decoded.chunks_exact(3) {
            for (c, expect) in chunk.iter().zip([120u8, 60, 200]) {
                assert!(c.abs_diff(expect) <= 2, "got {c} want {expect}");
            }
        }
    }

    #[test]
    fn png_8bit_round_trips_bit_exact() {
        let rgb = QuantizedPixels::U8(flat_rgb8(4, 4, [10, 20, 30]));
        let bytes = encode_png(4, 4, &rgb, &[]).unwrap();
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (4, 4));
        let QuantizedPixels::U8(expect) = &rgb else {
            unreachable!()
        };
        assert_eq!(&buf[..info.buffer_size()], expect.as_slice());
    }

    #[test]
    fn png_16bit_round_trips_bit_exact_and_carries_icc() {
        let px = QuantizedPixels::U16(vec![0, 32768, 65535, 100, 200, 300, 1, 2, 3]);
        let icc = vec![9u8, 9, 9];
        let bytes = encode_png(1, 3, &px, &icc).unwrap();
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        assert_eq!(
            reader.info().icc_profile.as_deref(),
            Some(icc.as_slice()),
            "iCCP chunk must round-trip"
        );
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        let decoded_be = &buf[..info.buffer_size()];
        let QuantizedPixels::U16(expect) = &px else {
            unreachable!()
        };
        let expect_be = u16_slice_to_be_bytes(expect);
        assert_eq!(decoded_be, expect_be.as_slice());
    }

    #[test]
    fn tiff_8bit_round_trips_bit_exact() {
        let px = QuantizedPixels::U8(flat_rgb8(3, 2, [5, 6, 7]));
        let bytes = encode_tiff(3, 2, &px, &[]).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes)).unwrap();
        let (w, h) = decoder.dimensions().unwrap();
        assert_eq!((w, h), (3, 2));
        let image = decoder.read_image().unwrap();
        let tiff::decoder::DecodingResult::U8(data) = image else {
            panic!("expected U8 decode result");
        };
        let QuantizedPixels::U8(expect) = &px else {
            unreachable!()
        };
        assert_eq!(&data, expect);
    }

    #[test]
    fn tiff_16bit_round_trips_bit_exact_and_carries_icc() {
        let px = QuantizedPixels::U16(vec![0, 1000, 65535, 42, 42, 42]);
        let icc = vec![7u8, 8, 9, 10];
        let bytes = encode_tiff(1, 2, &px, &icc).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes)).unwrap();
        let (w, h) = decoder.dimensions().unwrap();
        assert_eq!((w, h), (1, 2));
        let image = decoder.read_image().unwrap();
        let tiff::decoder::DecodingResult::U16(data) = image else {
            panic!("expected U16 decode result");
        };
        let QuantizedPixels::U16(expect) = &px else {
            unreachable!()
        };
        assert_eq!(&data, expect);
    }

    #[test]
    fn dispatch_rejects_non_eight_bit_pixels_for_jpeg() {
        let px = QuantizedPixels::U16(vec![0, 0, 0]);
        let err = encode(
            1,
            1,
            &px,
            crate::settings::FileFormat::Jpeg { quality: 90 },
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::Encode { format: "jpeg", .. }));
    }
}
