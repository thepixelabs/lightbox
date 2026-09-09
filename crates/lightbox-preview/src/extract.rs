// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Embedded-preview extraction (E03 spec §3.1/§5.3, Phase B T06):
//! `lightbox_decode::probe()` (metadata only, never a raw decode) + the
//! largest-covering-rendition selection already proven by
//! [`crate::pipeline::select_preview`] + a ranged [`lightbox_decode::read_embedded`]
//! read of the winning rendition's raw JPEG bytes, verbatim (no transcode
//! T0 stores the bytes exactly as the camera wrote them, spec §3.1).
//!
//! **Reconciliation (task-prompt instruction, not a spec deviation):** the
//! spec's §2 crate table names `rawler` for this task. `rawler` is banned
//! workspace-wide (`deny.toml`, confirmed in `E03-deviations.md` A-1), this
//! module is built entirely on `lightbox-decode`'s own permissive walkers +
//! `kamadak-exif` (already proven at metadata-parse cost by E01/E02's own
//! `probe()`/`read_embedded()`, which this module is a thin, store-facing
//! wrapper over). No new extraction logic is invented here; T06's job is
//! packaging that existing, already-tested probe surface into the shape
//! [`crate::producer::ensure_t0`] (T07) needs.
//!
//! **CPU budget (T06 AC: ≤10ms/file excluding IO).** `probe()` only parses
//! container/IFD structure (a few KiB even on a 45 MB raw, per its own doc
//! comment) and `read_embedded()` is a single ranged read, neither decodes
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
/// an original file, T0's raw material, before it has a store key or a
/// catalog row (spec §3.1: "the camera-embedded JPEG... stored verbatim").
pub(crate) struct T0Extract {
    /// The JPEG byte stream exactly as read from the original file.
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Whether the JPEG stream carries an embedded ICC profile (`APP2
    /// ICC_PROFILE` marker), spec §3.5 "embedded ICC/EXIF colorspace
    /// honored as a tag". EXIF `ColorSpace` (0xA001) is not sniffed here
    /// (out of scope for a container-level presence tag; full ICC handling
    /// is E02's, spec R7), recorded as a deliberate scope limit in
    /// `E03-deviations.md`.
    pub colorspace: PreviewColorspace,
}

/// Guard against absurd probe ranges: an embedded preview larger than this
/// is treated as malformed rather than read into memory (carried over from
/// the E01-seeded `pipeline::decode_class` this module replaces).
const MAX_EMBEDDED_BYTES: u64 = 256 * 1024 * 1024;

/// Extracts the largest usable embedded preview from `path` (spec §3.1/T06).
/// `Err(PreviewError::NoEmbedded)`, a typed, non-panicking outcome, when
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

/// Upper bound on the reassembled ICC profile, mirroring
/// [`lightbox_color::cms::MAX_ICC_BYTES`]. A JPEG can legally spread a profile
/// over 255 `APP2` chunks of ~64 KiB each (≈16 MiB); this cap stops a crafted
/// chunk chain from driving a large allocation *before* the profile is even
/// handed to the parser, which applies its own cap after.
const MAX_ICC_CHUNK_BYTES: usize = 32 * 1024 * 1024;

/// Bounded, never-panicking reassembly of a JPEG's embedded ICC profile:
/// every `APP2` segment whose payload starts with the 12-byte
/// `"ICC_PROFILE\0"` identifier, in marker order, with the 2-byte
/// sequence/count header stripped (ICC1v43_2010-12, §B.4).
///
/// Chunks are concatenated **in the order they appear in the stream** rather
/// than sorted by the sequence byte. Encoders write them in order, and honoring
/// a hostile sequence number would mean trusting untrusted input to index an
/// allocation; a scrambled chain simply produces bytes Little-CMS2 rejects,
/// which degrades to "unnamed profile" like any other malformed input.
///
/// `None` when the stream carries no ICC profile at all, which is the common
/// case and, per spec §5.3, means sRGB.
fn extract_icc_profile(jpeg: &[u8]) -> Option<Vec<u8>> {
    const ICC_MARKER: u8 = 0xE2;
    const ICC_ID: &[u8] = b"ICC_PROFILE\0";
    /// The `ICC_PROFILE\0` identifier plus the 1-byte chunk number and 1-byte
    /// chunk count that precede the profile fragment.
    const ICC_HEADER: usize = ICC_ID.len() + 2;

    // A malformed/adversarial stream just fails this scan (never panics
    // every access is bounds-checked via `get`) and reports no profile.
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return None;
    }
    let mut profile: Vec<u8> = Vec::new();
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
            break; // not a marker where one was expected, stop scanning
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
                if payload.starts_with(ICC_ID) && payload.len() > ICC_HEADER {
                    let fragment = &payload[ICC_HEADER..];
                    // Stop accumulating rather than truncating mid-profile: a
                    // half-copied profile would parse into something arbitrary,
                    // where giving up cleanly means "unnamed" ⇒ sRGB.
                    if profile.len().saturating_add(fragment.len()) > MAX_ICC_CHUNK_BYTES {
                        return None;
                    }
                    profile.extend_from_slice(fragment);
                }
            }
        }
        if payload_end <= i || payload_end > jpeg.len() {
            break; // no forward progress or out-of-range: stop, don't loop
        }
        i = payload_end;
    }
    (!profile.is_empty()).then_some(profile)
}

/// Resolves the colour space a JPEG's pixels are encoded in (spec §3.5
/// "embedded ICC/EXIF colorspace honored as a tag", §5.3 "untagged ⇒ assumed
/// sRGB").
///
/// # What changed, and why it matters
///
/// This used to answer only *whether* an `APP2 ICC_PROFILE` marker existed
/// ([`PreviewColorspace::TaggedIcc`] as a presence flag). Since every consumer
/// downstream then fell back to sRGB, a Display-P3 iPhone photo, which is
/// most photos taken in the last several years, was rendered with sRGB
/// primaries and came out over-saturated. It now reassembles the profile and
/// asks [`lightbox_color::classify_icc_source_space`] to name it.
///
/// # Untrusted input
///
/// The bytes come from a user file and are treated as hostile throughout:
/// [`extract_icc_profile`] is bounds-checked and iteration-capped, and the only
/// thing that ever parses the result is `lightbox-color`'s size-capped,
/// error-handler-captured `IccProfile::from_bytes`. There is no second parsing
/// path. Anything unparseable, non-RGB, or simply unrecognized comes back
/// [`PreviewColorspace::TaggedIcc`], which downstream renders as sRGB, the
/// documented fallback, not a failure.
///
/// # Cost
///
/// One marker walk plus (only when a profile is present) one ICC parse and a
/// handful of 18-colour probe transforms. It runs per *preview build* and per
/// *preview decode*, both of which already decode a whole JPEG and are cached
/// behind a RAM LRU, it is not on any per-frame or per-pixel path.
///
/// `pub(crate)` (not just used internally here) since Phase C's T1 pipeline
/// (`producer::ensure_t1`) also needs it for the "non-raw JPEG source, no
/// T0 row" path (T12 AC), it sniffs the source file's own colorspace tag
/// directly rather than going through [`extract_largest_embedded`].
pub(crate) fn sniff_colorspace(jpeg: &[u8]) -> PreviewColorspace {
    match extract_icc_profile(jpeg) {
        // No profile at all: the spec's untagged case (§5.3).
        None => PreviewColorspace::Srgb,
        Some(icc) => match lightbox_color::classify_icc_source_space(&icc) {
            Some(space) => PreviewColorspace::from(space),
            None => {
                tracing::debug!(
                    target: "lightbox_preview",
                    bytes = icc.len(),
                    "embedded ICC profile could not be resolved to a known space; assuming sRGB"
                );
                PreviewColorspace::TaggedIcc
            }
        },
    }
}

/// Whether a stored container carries an ICC profile of its own, i.e. whether
/// [`sniff_colorspace`] on these bytes is authoritative or merely a default.
///
/// The distinction matters for a **stale preview cache**: a T0 file is the
/// camera's verbatim JPEG and still carries its profile, so its recorded tag
/// can be re-derived and upgraded at decode time; a T1 file was re-encoded from
/// pixels and carries none, so only the catalog row knows its space. See
/// [`crate::decode::open_pixels`].
pub(crate) fn carries_icc_profile(jpeg: &[u8]) -> bool {
    extract_icc_profile(jpeg).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_color::RgbSourceSpace;

    /// Wraps `icc` into a minimal JPEG shell as `APP2 ICC_PROFILE` segments,
    /// splitting across `chunks` markers the way a real encoder does for a
    /// profile too large for one 64 KiB segment.
    fn jpeg_tagged_with(icc: &[u8], chunks: usize) -> Vec<u8> {
        assert!(chunks >= 1);
        let mut jpeg = vec![0xFFu8, 0xD8]; // SOI
        let per = icc.len().div_ceil(chunks);
        for (n, fragment) in icc.chunks(per.max(1)).enumerate() {
            let mut payload = b"ICC_PROFILE\0".to_vec();
            payload.push((n + 1) as u8); // chunk number, 1-based
            payload.push(chunks as u8); // chunk count
            payload.extend_from_slice(fragment);
            let seg_len = (payload.len() + 2) as u16;
            jpeg.extend_from_slice(&[0xFF, 0xE2]);
            jpeg.extend_from_slice(&seg_len.to_be_bytes());
            jpeg.extend_from_slice(&payload);
        }
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        jpeg
    }

    fn icc_bytes(space: RgbSourceSpace) -> Vec<u8> {
        space
            .reference_profile()
            .to_icc_bytes()
            .expect("serialise reference profile")
    }

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

    /// **The gap this closes.** A real Display-P3 profile in a real `APP2`
    /// chain resolves to Display-P3, not to a bare "tagged" flag that every
    /// consumer downstream then treats as sRGB.
    #[test]
    fn a_display_p3_tagged_jpeg_resolves_to_display_p3() {
        let jpeg = jpeg_tagged_with(&icc_bytes(RgbSourceSpace::DisplayP3), 1);
        assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::DisplayP3);
    }

    /// Every space the classifier can name survives the container round trip.
    #[test]
    fn every_named_space_survives_the_jpeg_app2_round_trip() {
        for (space, want) in [
            (RgbSourceSpace::Srgb, PreviewColorspace::Srgb),
            (RgbSourceSpace::DisplayP3, PreviewColorspace::DisplayP3),
            (RgbSourceSpace::AdobeRgb, PreviewColorspace::AdobeRgb),
            (RgbSourceSpace::ProPhoto, PreviewColorspace::ProPhoto),
            (RgbSourceSpace::Rec2020, PreviewColorspace::Rec2020),
        ] {
            let jpeg = jpeg_tagged_with(&icc_bytes(space), 1);
            assert_eq!(sniff_colorspace(&jpeg), want, "{space:?}");
        }
    }

    /// A profile larger than one `APP2` segment is written as a numbered chain;
    /// the fragments must be reassembled (headers stripped) before parsing, or
    /// every wide-gamut photo from a camera that writes a fat profile silently
    /// falls back to sRGB.
    #[test]
    fn a_multi_chunk_icc_profile_is_reassembled() {
        let icc = icc_bytes(RgbSourceSpace::AdobeRgb);
        assert!(icc.len() > 16, "reference profile is non-trivial");
        for chunks in [2usize, 3, 7] {
            let jpeg = jpeg_tagged_with(&icc, chunks);
            assert_eq!(
                extract_icc_profile(&jpeg).as_deref(),
                Some(icc.as_slice()),
                "{chunks}-chunk profile did not reassemble byte-for-byte",
            );
            assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::AdobeRgb);
        }
    }

    /// An `APP2 ICC_PROFILE` segment carrying bytes that are not a usable
    /// profile degrades to [`PreviewColorspace::TaggedIcc`], "there is a tag,
    /// we cannot name it", which downstream renders as sRGB. It must never
    /// panic and never guess a space.
    #[test]
    fn a_malformed_profile_degrades_to_an_unnamed_tag() {
        for junk in [vec![0u8; 16], vec![0xABu8; 1024], b"not an icc".to_vec()] {
            let jpeg = jpeg_tagged_with(&junk, 1);
            assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::TaggedIcc);
        }
    }

    /// A truncated profile, the chain starts but the stream ends mid-segment
    /// must not panic or invent a space either.
    #[test]
    fn a_truncated_icc_chain_degrades_without_panicking() {
        let full = jpeg_tagged_with(&icc_bytes(RgbSourceSpace::DisplayP3), 4);
        for cut in [8usize, 32, full.len() / 3, full.len() / 2, full.len() - 1] {
            let space = sniff_colorspace(&full[..cut.min(full.len())]);
            assert!(
                matches!(
                    space,
                    PreviewColorspace::Srgb
                        | PreviewColorspace::TaggedIcc
                        | PreviewColorspace::DisplayP3
                ),
                "truncation at {cut} produced {space:?}",
            );
        }
    }

    #[test]
    fn sniff_colorspace_defaults_to_srgb_without_an_icc_marker() {
        let jpeg = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        assert_eq!(sniff_colorspace(&jpeg), PreviewColorspace::Srgb);
        assert!(!carries_icc_profile(&jpeg));
    }

    /// `carries_icc_profile` is the stale-cache discriminator: true only when
    /// the container itself can be re-read for its space.
    #[test]
    fn carries_icc_profile_distinguishes_tagged_from_untagged_containers() {
        assert!(carries_icc_profile(&jpeg_tagged_with(
            &icc_bytes(RgbSourceSpace::DisplayP3),
            2
        )));
        assert!(!carries_icc_profile(&[
            0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0xFF, 0xD9
        ]));
        assert!(!carries_icc_profile(&[]));
    }
}
