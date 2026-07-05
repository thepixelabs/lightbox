// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! JPEG probing: SOF marker scan for dimensions + `kamadak-exif` for the
//! EXIF fields (spec §3.7: "JPEG/TIFF/PNG via header parse + kamadak-exif").
//!
//! The SOF scanner is shared: it also sizes and vets the *embedded* JPEG
//! preview candidates that the raw-container walkers report (a candidate
//! whose stream is not a decodable JPEG — e.g. a CR2's lossless-JPEG raw
//! IFD, which also starts `FFD8` — is rejected here by its SOF marker).

use std::fs::File;
use std::io::BufReader;

use crate::probe::exif_fields::ExifFields;
use crate::probe::ProbeParts;
use crate::{ProbeError, ProbedFormat};

/// How many bytes of a JPEG stream the SOF scan may inspect. SOF sits after
/// the APPn segments; camera EXIF blocks stay well under this.
pub(crate) const SOF_SCAN_CAP: usize = 256 * 1024;

/// Dimensions found in a JPEG start-of-frame marker.
#[derive(Copy, Clone, Debug)]
pub(crate) struct SofInfo {
    /// The SOF marker byte (0xC0..).
    pub marker: u8,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
}

impl SofInfo {
    /// Baseline (C0), extended sequential (C1) and progressive (C2) are the
    /// Huffman DCT processes `zune-jpeg` decodes; anything else (notably
    /// SOF3 lossless, the compression inside CR2/DNG raw IFDs) is not an
    /// embedded *preview*.
    pub fn decodable(&self) -> bool {
        matches!(self.marker, 0xC0..=0xC2)
    }
}

/// Scans `buf` (which must start at an `FFD8` SOI) for the first SOF marker
/// and returns its dimensions. `None` when the stream is not a JPEG or no
/// SOF appears before SOS/EOI/end-of-buffer. Never panics on garbage.
pub(crate) fn scan_sof(buf: &[u8]) -> Option<SofInfo> {
    if buf.len() < 4 || buf[0] != 0xFF || buf[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    // Bounded by buffer length; every arm advances `i`.
    while i + 1 < buf.len() {
        if buf[i] != 0xFF {
            // Not positioned at a marker: broken stream.
            return None;
        }
        // Fill bytes: any number of 0xFF may pad before the marker id.
        while i + 1 < buf.len() && buf[i + 1] == 0xFF {
            i += 1;
        }
        if i + 1 >= buf.len() {
            return None;
        }
        let marker = buf[i + 1];
        match marker {
            // Standalone markers (no length field).
            0x01 | 0xD0..=0xD7 => {
                i += 2;
            }
            // SOS / EOI: pixel data begins / stream over — no SOF found.
            0xDA | 0xD9 => return None,
            // SOF family (excluding DHT 0xC4, JPG 0xC8, DAC 0xCC).
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if i + 9 >= buf.len() {
                    return None;
                }
                let height = u32::from(u16::from_be_bytes([buf[i + 5], buf[i + 6]]));
                let width = u32::from(u16::from_be_bytes([buf[i + 7], buf[i + 8]]));
                if width == 0 || height == 0 {
                    return None;
                }
                return Some(SofInfo {
                    marker,
                    width,
                    height,
                });
            }
            // Every other marker carries a big-endian length that includes
            // the two length bytes themselves.
            _ => {
                if i + 3 >= buf.len() {
                    return None;
                }
                let len = usize::from(u16::from_be_bytes([buf[i + 2], buf[i + 3]]));
                if len < 2 {
                    return None;
                }
                i += 2 + len;
            }
        }
    }
    None
}

/// Probes a standalone JPEG file. The whole file doubles as its own
/// "embedded preview" (byte range `0..len`) so the M0 embedded-preview
/// pipeline serves imported JPEGs uniformly (spec §3.6).
pub(crate) fn probe_jpeg(file: &mut File, file_len: u64) -> Result<ProbeParts, ProbeError> {
    let head = super::read_prefix(file, 0, SOF_SCAN_CAP.min(file_len as usize))?;
    let sof = scan_sof(&head)
        .filter(SofInfo::decodable)
        .ok_or_else(|| ProbeError::Malformed("JPEG without a decodable SOF marker".into()))?;

    // EXIF fields; a JPEG without EXIF is perfectly fine.
    let fields = ExifFields::from_reader(&mut BufReader::new(&mut *file));

    let capture_time = fields.capture_time();
    Ok(ProbeParts {
        format: ProbedFormat::Jpeg,
        width: sof.width,
        height: sof.height,
        orientation_exif: fields.orientation,
        make: fields.make,
        model: fields.model,
        capture_time,
        candidates: vec![super::Candidate {
            offset: 0,
            len: file_len,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal JPEG-ish prefix: SOI, APP0 stub, SOF0 640x480.
    pub(crate) fn fake_jpeg_head(marker: u8, w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, len 4
        b.extend_from_slice(&[0xFF, marker, 0x00, 0x0B, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[0x03, 0x01, 0x22, 0x00]);
        b
    }

    #[test]
    fn finds_sof_dims() {
        let head = fake_jpeg_head(0xC0, 640, 480);
        let sof = scan_sof(&head).unwrap();
        assert_eq!((sof.width, sof.height), (640, 480));
        assert!(sof.decodable());
    }

    #[test]
    fn lossless_sof3_is_not_decodable() {
        let head = fake_jpeg_head(0xC3, 100, 50);
        let sof = scan_sof(&head).unwrap();
        assert!(!sof.decodable());
    }

    #[test]
    fn garbage_and_truncation_yield_none() {
        assert!(scan_sof(b"").is_none());
        assert!(scan_sof(b"\xFF\xD8").is_none());
        assert!(scan_sof(b"not a jpeg at all").is_none());
        assert!(scan_sof(&[0xFF, 0xD8, 0x00, 0x00, 0x00]).is_none());
        // SOS before any SOF.
        assert!(scan_sof(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02]).is_none());
        // Truncated mid-SOF.
        let head = fake_jpeg_head(0xC0, 640, 480);
        assert!(scan_sof(&head[..head.len() - 8]).is_none());
        // Zero-length segment must not loop forever.
        assert!(scan_sof(&[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x00, 0xFF, 0xD9]).is_none());
    }
}
