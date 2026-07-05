// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! A7 acceptance: non-raw decode round-trips embedded ICC bytes and preserves
//! 16-bit precision. Hermetic — images are encoded in-memory with the same
//! permissive codecs, so this needs no external fixture corpus.

use std::borrow::Cow;

use lightbox_decode::{decode_image, SourceColor};

/// Encodes an 8-bit RGB PNG carrying an iCCP chunk, decodes it back, and
/// asserts the ICC bytes survive and the pixels normalize correctly.
#[test]
fn png_embedded_icc_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tagged.png");

    // A distinctive (not-real-but-opaque) ICC payload; decode must return it
    // verbatim.
    let icc: Vec<u8> = (0u16..600).map(|i| (i % 256) as u8).collect();

    let (w, h) = (2u32, 1u32);
    // Pixel A = (0,128,255), pixel B = (255,0,0).
    let pixels: [u8; 6] = [0, 128, 255, 255, 0, 0];

    let file = std::fs::File::create(&path).unwrap();
    let mut info = png::Info::with_size(w, h);
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Eight;
    info.icc_profile = Some(Cow::Owned(icc.clone()));
    let encoder = png::Encoder::with_info(std::io::BufWriter::new(file), info).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();

    let img = decode_image(&path).unwrap();
    assert_eq!((img.width, img.height, img.channels), (2, 1, 3));
    match &img.color {
        SourceColor::Tagged(bytes) => assert_eq!(bytes, &icc, "ICC bytes must round-trip"),
        other => panic!("expected Tagged ICC, got {other:?}"),
    }
    // Normalization: first pixel green ≈ 128/255, blue = 1.0.
    assert!((img.data[1] - 128.0 / 255.0).abs() < 1e-4);
    assert!((img.data[2] - 1.0).abs() < 1e-6);
}

/// Encodes a 16-bit grayscale TIFF and asserts decode preserves precision an
/// 8-bit path would lose (0x8000 → ~0.5000, not 128/255).
#[test]
fn tiff_16bit_precision_preserved() {
    use tiff::encoder::{colortype, TiffEncoder};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep.tiff");

    let (w, h) = (2u32, 1u32);
    // 0x8000 = 32768; 8-bit rounding could never represent this exactly.
    let samples: [u16; 2] = [32768, 65535];

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = TiffEncoder::new(std::io::BufWriter::new(file)).unwrap();
        enc.write_image::<colortype::Gray16>(w, h, &samples)
            .unwrap();
    }

    let img = decode_image(&path).unwrap();
    assert_eq!((img.width, img.height, img.channels), (2, 1, 1));
    assert!(
        (img.data[0] - 32768.0 / 65535.0).abs() < 1e-6,
        "16-bit value must keep sub-8-bit precision, got {}",
        img.data[0]
    );
    assert!((img.data[1] - 1.0).abs() < 1e-6);
    // Untagged TIFF ⇒ AssumedSrgb (spec §5.3).
    assert!(matches!(img.color, SourceColor::AssumedSrgb));
}

/// A truncated image is a structured error, never a panic (A9).
#[test]
fn truncated_png_is_structured_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.png");
    // Valid signature, garbage after.
    std::fs::write(
        &path,
        [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 1, 2, 3,
        ],
    )
    .unwrap();
    assert!(decode_image(&path).is_err());
}
