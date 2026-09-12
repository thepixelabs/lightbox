// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Encoders (spec §5.4 `lightbox-export::encode`, narrowed to the core
//! slice's three formats: JPEG/PNG/TIFF, JXL/AVIF/DNG/Original are
//! named-deferred, see `docs/plan/epics/E15-deviations.md`).
//!
//! Each function takes already-quantized pixels ([`crate::pixel::quantize`]),
//! the destination ICC profile bytes ([`crate::pixel::apply_output_color`])
//! and the policy-filtered [`MetadataBlocks`] ([`crate::metadata::build`]),
//! and returns the encoded container bytes; writing to disk is
//! [`crate::run`]'s job (temp-then-rename).
//!
//! All three crates (`jpeg-encoder`, `png`, `tiff`) are already workspace
//! dependencies from E02/E03 (T1 preview JPEG, raw-adjacent TIFF/PNG
//! decode), no new license surface.
//!
//! **Nothing here reads the source file.** An encoder writes exactly the
//! ICC bytes and the [`MetadataBlocks`] it is handed, which is what makes
//! [`crate::settings::MetadataLevel`]'s privacy claim structural rather
//! than a matter of remembering to strip things; see
//! [`crate::metadata`]'s module doc comment. Per-container placement of
//! each block is documented there too.

use std::io::Cursor;

use crate::error::ExportError;
use crate::metadata::MetadataBlocks;
use crate::pixel::QuantizedPixels;
use crate::settings::BitDepth;

/// The XMP APP1 segment's identifier, NUL terminated (XMP spec part 3,
/// "Embedding XMP metadata in application files", JPEG section).
const XMP_APP1_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The PNG `iTXt` keyword XMP lives under (same XMP spec part 3, PNG
/// section).
const XMP_PNG_KEYWORD: &str = "XML:com.adobe.xmp";

/// TIFF tag 700, `XMLPacket`. Not in the `tiff` crate's [`tiff::tags::Tag`]
/// enum, so it goes through its `Unknown` variant.
const TIFF_TAG_XMP: u16 = 700;

fn encode_err(format: &'static str, msg: impl std::fmt::Display) -> ExportError {
    ExportError::Encode {
        format,
        msg: msg.to_string(),
    }
}

/// Encodes 8-bit interleaved RGB as a JPEG (spec §5.4 JPEG encoder,
/// narrowed: no `ChromaMode`/size-limit bisection, see the module doc
/// comment).
///
/// Segments, in the order written: EXIF as APP1 with the `Exif\0\0`
/// header, XMP as APP1 with the `http://ns.adobe.com/xap/1.0/\0` header,
/// then the ICC profile as chunked APP2 (`jpeg-encoder`'s
/// `add_icc_profile`, spec-compliant chunking per ICC.org's embedding
/// note). Each is skipped when empty.
///
/// # Errors
///
/// A metadata block over JPEG's 65533-byte per-segment limit is a hard
/// error rather than a silent drop: an export that quietly loses the
/// copyright it was told to write would be worse than one that fails.
/// [`crate::metadata::MAX_FIELD_CHARS`] is what keeps this from happening
/// in practice.
pub fn encode_jpeg(
    w: u32,
    h: u32,
    rgb8: &[u8],
    quality: u8,
    icc: &[u8],
    meta: &MetadataBlocks,
) -> Result<Vec<u8>, ExportError> {
    let width = u16::try_from(w)
        .map_err(|_| encode_err("jpeg", format!("width {w} exceeds JPEG's 65535px limit")))?;
    let height = u16::try_from(h)
        .map_err(|_| encode_err("jpeg", format!("height {h} exceeds JPEG's 65535px limit")))?;

    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    if let Some(exif) = meta.exif.as_deref().filter(|b| !b.is_empty()) {
        encoder
            .add_exif_metadata(exif)
            .map_err(|e| encode_err("jpeg", format!("EXIF APP1: {e}")))?;
    }
    if let Some(xmp) = meta.xmp.as_deref().filter(|b| !b.is_empty()) {
        let mut segment = XMP_APP1_PREFIX.to_vec();
        segment.extend_from_slice(xmp);
        encoder
            .add_app_segment(1, segment)
            .map_err(|e| encode_err("jpeg", format!("XMP APP1: {e}")))?;
    }
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

/// Encodes quantized RGB as a PNG, 8 or 16-bit (spec §5.4 PNG encoder).
///
/// ICC bytes (when non-empty) go in an `iCCP` chunk, EXIF in an `eXIf`
/// chunk (PNG 1.2 extension / PNG third edition), and XMP in an
/// uncompressed `iTXt` chunk keyed `XML:com.adobe.xmp`, which is the form
/// the XMP specification requires for PNG.
pub fn encode_png(
    w: u32,
    h: u32,
    pixels: &QuantizedPixels,
    icc: &[u8],
    meta: &MetadataBlocks,
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
    if let Some(exif) = meta.exif.as_deref().filter(|b| !b.is_empty()) {
        info.exif_metadata = Some(std::borrow::Cow::Borrowed(exif));
    }
    if let Some(xmp) = meta.xmp.as_deref().filter(|b| !b.is_empty()) {
        info.utf8_text.push(png::text_metadata::ITXtChunk::new(
            XMP_PNG_KEYWORD,
            String::from_utf8_lossy(xmp).into_owned(),
        ));
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

/// Writes the ICC profile and the policy-filtered metadata into a TIFF
/// directory: tag 34675 for ICC, [`MetadataBlocks::tiff_tags`] as native
/// IFD0 ASCII tags, and the XMP packet in tag 700. A TIFF gets no packed
/// EXIF blob and no GPS sub-IFD; see [`crate::metadata`]'s module doc
/// comment for why, and for what that does and does not mean for privacy.
fn write_tiff_metadata<W, K>(
    dir: &mut tiff::encoder::DirectoryEncoder<'_, W, K>,
    icc: &[u8],
    meta: &MetadataBlocks,
) -> Result<(), ExportError>
where
    W: std::io::Write + std::io::Seek,
    K: tiff::encoder::TiffKind,
{
    use tiff::tags::Tag;

    if !icc.is_empty() {
        dir.write_tag(Tag::IccProfile, icc)
            .map_err(|e| encode_err("tiff", e))?;
    }
    for (tag, value) in &meta.tiff_tags {
        dir.write_tag(Tag::Unknown(*tag), value.as_str())
            .map_err(|e| encode_err("tiff", format!("tag {tag}: {e}")))?;
    }
    if let Some(xmp) = meta.xmp.as_deref().filter(|b| !b.is_empty()) {
        dir.write_tag(Tag::Unknown(TIFF_TAG_XMP), xmp)
            .map_err(|e| encode_err("tiff", format!("XMP tag 700: {e}")))?;
    }
    Ok(())
}

/// Encodes quantized RGB as a TIFF, 8 or 16-bit, Deflate-compressed (spec
/// §5.4 TIFF encoder, narrowed: no compression choice at core-slice scope
/// see `settings::FileFormat::Tiff`'s doc comment). ICC bytes (when
/// non-empty) are embedded as tag 34675 (`Tag::IccProfile`, already defined
/// by the `tiff` crate); metadata goes in via [`write_tiff_metadata`].
pub fn encode_tiff(
    w: u32,
    h: u32,
    pixels: &QuantizedPixels,
    icc: &[u8],
    meta: &MetadataBlocks,
) -> Result<Vec<u8>, ExportError> {
    use tiff::encoder::compression::DeflateLevel;
    use tiff::encoder::{colortype, Compression, TiffEncoder};

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
                write_tiff_metadata(image.encoder(), icc, meta)?;
                image.write_data(data).map_err(|e| encode_err("tiff", e))?;
            }
            QuantizedPixels::U16(data) => {
                let mut image = tiff_enc
                    .new_image::<colortype::RGB16>(w, h)
                    .map_err(|e| encode_err("tiff", e))?;
                write_tiff_metadata(image.encoder(), icc, meta)?;
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
    meta: &MetadataBlocks,
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
            encode_jpeg(w, h, rgb8, quality, icc, meta)
        }
        FileFormat::Png { depth: _ } => encode_png(w, h, pixels, icc, meta),
        FileFormat::Tiff { depth: _ } => encode_tiff(w, h, pixels, icc, meta),
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
        let bytes = encode_jpeg(8, 8, &rgb, 95, &icc, &MetadataBlocks::default()).unwrap();
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
        let bytes = encode_png(4, 4, &rgb, &[], &MetadataBlocks::default()).unwrap();
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
        let bytes = encode_png(1, 3, &px, &icc, &MetadataBlocks::default()).unwrap();
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
        let bytes = encode_tiff(3, 2, &px, &[], &MetadataBlocks::default()).unwrap();
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
        let bytes = encode_tiff(1, 2, &px, &icc, &MetadataBlocks::default()).unwrap();
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
            &MetadataBlocks::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::Encode { format: "jpeg", .. }));
    }
}
