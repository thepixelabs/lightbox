// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Embedded-preview extraction (E03 spec §3.1/§5.3, Phase B T06):
//! `lightbox_decode::probe()` (metadata only — never a raw decode) + the
//! largest-covering-rendition selection already proven by
//! [`crate::pipeline::select_preview`] + a ranged [`lightbox_decode::read_embedded`]
//! read of the winning rendition's raw JPEG bytes, verbatim (no transcode —
//! T0 stores the bytes exactly as the camera wrote them, spec §3.1).
//!
//! **Reconciliation (task-prompt instruction, not a spec deviation):** the
//! spec's §2 crate table names `rawler` for this task. `rawler` is banned
//! workspace-wide (`deny.toml`, confirmed in `E03-deviations.md` A-1) — this
//! module is built entirely on `lightbox-decode`'s own permissive walkers +
//! `kamadak-exif` (already proven at metadata-parse cost by E01/E02's own
//! `probe()`/`read_embedded()`, which this module is a thin, store-facing
//! wrapper over). No new extraction logic is invented here; T06's job is
//! packaging that existing, already-tested probe surface into the shape
//! [`crate::producer::ensure_t0`] (T07) needs.
//!
//! **CPU budget (T06 AC: ≤10ms/file excluding IO).** `probe()` only parses
//! container/IFD structure (a few KiB even on a 45 MB raw, per its own doc
//! comment) and `read_embedded()` is a single ranged read — neither decodes
//! image payloads. This module adds no decode step of its own, so the
//! existing probe/read-embedded cost (already budgeted and measured by
//! E01/E02) is the entire cost here.
//!
//! **Never panics (T06 AC).** Every fallible step returns a typed
//! [`PreviewError`]; `probe`/`read_embedded` are themselves proven
//! never-panicking on malformed/adversarial input (E01/E02 spec §3.7, fuzz
//! corpus). This module adds no `unwrap`/indexing of untrusted data.

use std::path::Path;

use crate::pipeline;
use crate::pyramid::PreviewColorspace;
use crate::PreviewError;

/// The verbatim bytes + geometry of the largest usable embedded preview in
/// an original file — T0's raw material, before it has a store key or a
/// catalog row (spec §3.1: "the camera-embedded JPEG... stored verbatim").
pub(crate) struct T0Extract {
    /// The JPEG byte stream exactly as read from the original file.
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Whether the JPEG stream carries an embedded ICC profile (`APP2
    /// ICC_PROFILE` marker) — spec §3.5 "embedded ICC/EXIF colorspace
    /// honored as a tag". EXIF `ColorSpace` (0xA001) is not sniffed here
    /// (out of scope for a container-level presence tag; full ICC handling
    /// is E02's, spec R7) — recorded as a deliberate scope limit in
    /// `E03-deviations.md`.
    pub colorspace: PreviewColorspace,
}

/// Guard against absurd probe ranges: an embedded preview larger than this
/// is treated as malformed rather than read into memory (carried over from
/// the E01-seeded `pipeline::decode_class` this module replaces).
const MAX_EMBEDDED_BYTES: u64 = 256 * 1024 * 1024;

/// Extracts the largest usable embedded preview from `path` (spec §3.1/T06).
/// `Err(PreviewError::NoEmbedded)` — a typed, non-panicking outcome — when
/// the file has no embedded preview or only a surrogate too small to serve
/// as T0 (mirrors [`pipeline::select_preview`]'s loupe-class threshold,
/// spec T20's Olympus E-1 case: a 160 px thumb does not become T0).
pub(crate) fn extract_largest_embedded(path: &Path) -> Result<T0Extract, PreviewError> {
    let probe = lightbox_decode::probe(path)?;
    let info = pipeline::select_preview(&probe, crate::PreviewClass::Loupe)
        .ok_or(PreviewError::NoEmbedded)?
        .clone();
    let range_len = info.byte_range.end.saturating_sub(info.byte_range.start);
    if range_len > MAX_EMBEDDED_BYTES {
        return Err(PreviewError::Decode(format!(
            "embedded preview claims {range_len} bytes"
        )));
    }
    let jpeg = lightbox_decode::read_embedded(path, &info)?;
    let colorspace = sniff_colorspace(&jpeg);
    Ok(T0Extract {
        jpeg,
        width: info.width,
        height: info.height,
        colorspace,
    })
}

/// Bounded, never-panicking scan for a JPEG `APP2` marker whose payload
/// starts with the 12-byte `"ICC_PROFILE\0"` identifier (ICC1v43_2010-12,
/// §B.4). Presence-only: this crate carries the tag, not the profile bytes
/// (spec §5.1 `PreviewColorspace::TaggedIcc` — "the profile bytes themselves
/// travel with the container", i.e. the decoder that actually needs them
/// reads the stored JPEG directly, not through this enum).
///
/// `pub(crate)` (not just used internally here) since Phase C's T1 pipeline
/// (`producer::ensure_t1`) also needs it for the "non-raw JPEG source, no
/// T0 row" path (T12 AC) — it sniffs the source file's own colorspace tag
/// directly rather than going through [`extract_largest_embedded`].
pub(crate) fn sniff_colorspace(jpeg: &[u8]) -> PreviewColorspace {
    const ICC_MARKER: u8 = 0xE2;
    const ICC_ID: &[u8] = b"ICC_PROFILE\0";
    // A malformed/adversarial stream just fails this scan (never panics —
    // every access is bounds-checked via `get`) and falls back to sRGB.
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return PreviewColorspace::Srgb;
    }
    let mut i = 2usize;
    // Cap iterations so a pathological marker chain can't spin (each
    // segment must be at least 4 bytes, so this bound is already generous
    // relative to `jpeg.len()`, but an explicit cap keeps the cost profile
    // obviously bounded regardless of input shape).
    for _ in 0..4096 {
        let Some(&marker_tag) = jpeg.get(i) else {
            break;
        };
        if marker_tag != 0xFF {
            break; // not a marker where one was expected — stop scanning
        }
        let Some(&kind) = jpeg.get(i + 1) else {
            break;
        };
        if kind == 0xD8 || kind == 0xD9 || (0xD0..=0xD7).contains(&kind) {
            i += 2; // markers with no length field
            continue;
        }
        if kind == 0xDA {
            break; // start-of-scan: entropy-coded data follows, stop
        }
        let Some(len_hi) = jpeg.get(i + 2) else {
            break;
        };
        let Some(len_lo) = jpeg.get(i + 3) else {
            break;
        };
        let seg_len = (u16::from(*len_hi) << 8 | u16::from(*len_lo)) as usize;
        if seg_len < 2 {
            break; // malformed length, never trust it into an underflow
        }
        let payload_start = i + 4;
        let payload_end = i + 2 + seg_len;
        if kind == ICC_MARKER {
            if let Some(payload) = jpeg.get(payload_start..payload_end.min(jpeg.len())) {
                if payload.starts_with(ICC_ID) {
                    return PreviewColorspace::TaggedIcc;
                }
            }
        }
        if payload_end <= i || payload_end > jpeg.len() {
            break; // no forward progress or out-of-range: stop, don't loop
        }
        i = payload_end;
    }
    PreviewColorspace::Srgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_colorspace_never_panics_on_adversarial_bytes() {
        let cases: &[&[u8]] = &[
            &[],
            &[0xFF],
            &[0xFF, 0xD8],
            &[0xFF, 0xD8, 0xFF],
            &[0xFF, 0xD8, 0xFF, 0xE2, 0x00],
            &[0xFF, 0xD8, 0xFF, 0xE2, 0xFF, 0xFF],
            &[0u8; 8],
            &[0xFFu8; 64],
        ];
        for bytes in cases {
            let _ = sniff_colorspace(bytes); // must not panic
        }
    }

    #[test]
    fn sniff_colorspace_finds_a_well_formed_icc_marker() {
        let mut jpeg = vec![0xFF, 0xD8]; // SOI
        let mut app2 = vec![0xFFu8, 0xE2];
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.extend_from_slice(&[1, 1]); // seq/count
        payload.extend_from_slice(&[0u8; 16]); // fake profile bytes
        let seg_len = (payload.len() + 2) as u16;
        app2.extend_from_slice(&seg_len.to_be_bytes());
        app2.extend_from_slice(&payload);
        jpeg.extend_from_slice(&app2);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::TaggedIcc);
    }

    #[test]
    fn sniff_colorspace_defaults_to_srgb_without_an_icc_marker() {
        let jpeg = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::Srgb);
    }
}
