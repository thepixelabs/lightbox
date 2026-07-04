// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Deterministic fixture generators.
//!
//! raw.pixls.us hosts raw files only, so the JPEG/TIFF/PNG legs of the corpus
//! (and the deliberately-corrupt files) are **generated, bit-for-bit
//! deterministically**, by this module — which is what makes their manifest
//! hash pins meaningful. Every generator is a pure function of its spec (plus,
//! for `truncate:`, of an already-pinned source fixture).

use std::path::Path;

use anyhow::{bail, Context};

/// A parsed generator spec from `fixtures/manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Generator {
    /// `png-gradient-8x8` — valid 8×8 RGB PNG (stored-deflate IDAT).
    PngGradient8x8,
    /// `tiff-gradient-8x8` — valid little-endian uncompressed baseline TIFF.
    TiffGradient8x8,
    /// `jpeg-tiny-16x16` — embedded known-good 16×16 JFIF JPEG
    /// (self-made from a generated gradient; CC0).
    JpegTiny16x16,
    /// `truncate:<source-fixture>:<bytes>` — first N bytes of another fixture.
    Truncate {
        /// Name of the source fixture (must appear earlier in the manifest).
        source: String,
        /// How many leading bytes to keep.
        bytes: u64,
    },
    /// `garbage:<bytes>` — seeded xorshift64* noise (never a valid image).
    Garbage {
        /// Output length in bytes.
        bytes: u64,
    },
}

impl Generator {
    /// Parses the manifest spelling of a generator.
    pub fn parse(spec: &str) -> anyhow::Result<Generator> {
        if let Some(rest) = spec.strip_prefix("truncate:") {
            let (source, bytes) = rest
                .rsplit_once(':')
                .with_context(|| format!("bad truncate spec {spec:?}"))?;
            if source.is_empty() {
                bail!("bad truncate spec {spec:?}: empty source fixture name");
            }
            return Ok(Generator::Truncate {
                source: source.to_owned(),
                bytes: bytes
                    .parse()
                    .with_context(|| format!("bad truncate byte count in {spec:?}"))?,
            });
        }
        if let Some(bytes) = spec.strip_prefix("garbage:") {
            return Ok(Generator::Garbage {
                bytes: bytes
                    .parse()
                    .with_context(|| format!("bad garbage byte count in {spec:?}"))?,
            });
        }
        match spec {
            "png-gradient-8x8" => Ok(Generator::PngGradient8x8),
            "tiff-gradient-8x8" => Ok(Generator::TiffGradient8x8),
            "jpeg-tiny-16x16" => Ok(Generator::JpegTiny16x16),
            other => bail!("unknown fixture generator {other:?}"),
        }
    }

    /// Produces the fixture bytes. `fixtures_dir` is consulted only by
    /// `truncate:` (its source must already exist there).
    pub fn run(&self, fixtures_dir: &Path) -> anyhow::Result<Vec<u8>> {
        match self {
            Generator::PngGradient8x8 => Ok(png_gradient_8x8()),
            Generator::TiffGradient8x8 => Ok(tiff_gradient_8x8()),
            Generator::JpegTiny16x16 => Ok(JPEG_TINY_16X16.to_vec()),
            Generator::Truncate { source, bytes } => {
                let src = fixtures_dir.join(source);
                let data = std::fs::read(&src).with_context(|| {
                    format!(
                        "truncate source {} missing — it must appear before its \
                         truncation in the manifest",
                        src.display()
                    )
                })?;
                let n = usize::try_from(*bytes).context("truncate length overflows usize")?;
                if data.len() < n {
                    bail!(
                        "truncate source {} is shorter ({}) than requested prefix ({n})",
                        src.display(),
                        data.len()
                    );
                }
                Ok(data[..n].to_vec())
            }
            Generator::Garbage { bytes } => {
                let n = usize::try_from(*bytes).context("garbage length overflows usize")?;
                Ok(garbage(n))
            }
        }
    }
}

/// 16×16 JFIF JPEG, produced once from a generated gradient PNG and embedded
/// verbatim (CC0 — self-made, no third-party content). Kept as an asset file
/// rather than generated at runtime so the pin can never drift with an
/// encoder-library upgrade.
const JPEG_TINY_16X16: &[u8] = include_bytes!("../assets/lightbox-tiny.jpg");

const GRADIENT_W: usize = 8;
const GRADIENT_H: usize = 8;

fn gradient_rgb(x: usize, y: usize) -> [u8; 3] {
    [(x * 32) as u8, (y * 32) as u8, ((x + y) * 16) as u8]
}

/// Minimal valid PNG: IHDR + IDAT (zlib "stored" deflate) + IEND.
fn png_gradient_8x8() -> Vec<u8> {
    // Raw scanlines: filter byte 0 + RGB triples.
    let mut raw = Vec::with_capacity(GRADIENT_H * (1 + GRADIENT_W * 3));
    for y in 0..GRADIENT_H {
        raw.push(0u8);
        for x in 0..GRADIENT_W {
            raw.extend_from_slice(&gradient_rgb(x, y));
        }
    }

    // zlib stream with a single stored (uncompressed) deflate block.
    let mut zlib = vec![0x78, 0x01];
    let len = raw.len() as u16;
    zlib.push(0x01); // BFINAL=1, BTYPE=00 (stored)
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(GRADIENT_W as u32).to_be_bytes());
    ihdr.extend_from_slice(&(GRADIENT_H as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, adaptive, no interlace

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Minimal valid little-endian baseline TIFF: one uncompressed RGB strip.
fn tiff_gradient_8x8() -> Vec<u8> {
    const STRIP_OFFSET: u32 = 8;
    let strip_len = (GRADIENT_W * GRADIENT_H * 3) as u32;
    let bps_offset = STRIP_OFFSET + strip_len; // BitsPerSample [8,8,8] lives here
    let ifd_offset = bps_offset + 6;

    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&ifd_offset.to_le_bytes());

    for y in 0..GRADIENT_H {
        for x in 0..GRADIENT_W {
            out.extend_from_slice(&gradient_rgb(x, y));
        }
    }
    for _ in 0..3 {
        out.extend_from_slice(&8u16.to_le_bytes()); // BitsPerSample values
    }

    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    let entries: [(u16, u16, u32, u32); 9] = [
        (256, SHORT, 1, GRADIENT_W as u32), // ImageWidth
        (257, SHORT, 1, GRADIENT_H as u32), // ImageLength
        (258, SHORT, 3, bps_offset),        // BitsPerSample (external)
        (259, SHORT, 1, 1),                 // Compression: none
        (262, SHORT, 1, 2),                 // Photometric: RGB
        (273, LONG, 1, STRIP_OFFSET),       // StripOffsets
        (277, SHORT, 1, 3),                 // SamplesPerPixel
        (278, SHORT, 1, GRADIENT_H as u32), // RowsPerStrip
        (279, LONG, 1, strip_len),          // StripByteCounts
    ];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, ty, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if ty == SHORT && count == 1 {
            // SHORT values are left-justified in the 4-byte value field.
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    out
}

/// Seeded xorshift64* noise — deterministic, never a valid image.
fn garbage(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let mut x = state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        state = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let need = (len - out.len()).min(8);
        out.extend_from_slice(&v.to_le_bytes()[..need]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_all_specs() {
        assert_eq!(
            Generator::parse("png-gradient-8x8").unwrap(),
            Generator::PngGradient8x8
        );
        assert_eq!(
            Generator::parse("tiff-gradient-8x8").unwrap(),
            Generator::TiffGradient8x8
        );
        assert_eq!(
            Generator::parse("jpeg-tiny-16x16").unwrap(),
            Generator::JpegTiny16x16
        );
        assert_eq!(
            Generator::parse("truncate:canon-eos-350d.cr2:4096").unwrap(),
            Generator::Truncate {
                source: "canon-eos-350d.cr2".into(),
                bytes: 4096
            }
        );
        assert_eq!(
            Generator::parse("garbage:65536").unwrap(),
            Generator::Garbage { bytes: 65536 }
        );
        assert!(Generator::parse("nonsense").is_err());
        assert!(Generator::parse("truncate:x").is_err());
        assert!(Generator::parse("truncate::12").is_err());
        assert!(Generator::parse("garbage:many").is_err());
    }

    #[test]
    fn generators_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        for spec in [
            "png-gradient-8x8",
            "tiff-gradient-8x8",
            "jpeg-tiny-16x16",
            "garbage:65536",
        ] {
            let g = Generator::parse(spec).unwrap();
            let a = g.run(dir.path()).unwrap();
            let b = g.run(dir.path()).unwrap();
            assert_eq!(a, b, "{spec} not deterministic");
            assert!(!a.is_empty());
        }
    }

    #[test]
    fn png_has_valid_signature_and_structure() {
        let png = png_gradient_8x8();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn tiff_has_valid_header() {
        let tiff = tiff_gradient_8x8();
        assert_eq!(&tiff[..2], b"II");
        assert_eq!(u16::from_le_bytes([tiff[2], tiff[3]]), 42);
        // IFD offset points inside the file.
        let ifd = u32::from_le_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize;
        assert!(ifd + 2 < tiff.len());
    }

    #[test]
    fn jpeg_asset_is_a_jfif_jpeg() {
        assert_eq!(&JPEG_TINY_16X16[..3], &[0xFF, 0xD8, 0xFF]);
        assert_eq!(
            &JPEG_TINY_16X16[JPEG_TINY_16X16.len() - 2..],
            &[0xFF, 0xD9],
            "missing EOI marker"
        );
    }

    #[test]
    fn truncate_takes_exact_prefix_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("src.bin"), [7u8; 100]).unwrap();
        let g = Generator::parse("truncate:src.bin:40").unwrap();
        assert_eq!(g.run(dir.path()).unwrap(), vec![7u8; 40]);
        // Longer than source → error, missing source → error.
        assert!(Generator::parse("truncate:src.bin:101")
            .unwrap()
            .run(dir.path())
            .is_err());
        assert!(Generator::parse("truncate:absent.bin:1")
            .unwrap()
            .run(dir.path())
            .is_err());
    }

    #[test]
    fn crc32_and_adler32_known_vectors() {
        // CRC-32/ISO-HDLC of "123456789" and adler32 of "Wikipedia".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
