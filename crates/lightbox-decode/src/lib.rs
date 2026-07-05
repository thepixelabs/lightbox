// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-decode` — file probing, hashing, and (later) raw decode.
//!
//! Owned by **E01 for the probe surface only** (spec §3.7): `probe()`
//! (container metadata for raws, header/EXIF parse for JPEG/TIFF/PNG),
//! `read_embedded()`, streaming xxh3-128 [`hash_file`]. `decode_raw` /
//! `decode_image` / camera-matrix color are declared by E01 but
//! **implemented by E02** — E01 takes zero dependency on raw decode
//! (architecture §9 M0).
//!
//! **Status: Phase-5 (T19/T20).** [`probe`] is real: content-sniffed
//! dispatch into bounded, never-panicking container walkers — an in-crate
//! TIFF/IFD walker for CR2/NEF/ARW/ORF/DNG/TIFF, an ISO-BMFF walker for CR3,
//! the RAF fixed header, JPEG SOF scan + `kamadak-exif`, PNG IHDR.
//! [`read_embedded`] does the ranged read of a probed preview. `decode_raw`
//! / `decode_image` are declared (E02 implements).
//!
//! **Deviation (recorded in E01-deviations.md):** the spec suggests rawler
//! for raw metadata, but rawler is LGPL-2.1 and the crate graph's license
//! gate (deny.toml, architecture §1.6) forbids copyleft crates — the
//! permissive in-crate walkers above replace it, covering all seven fixture
//! mounts.

use std::io::Read;
use std::ops::Range;
use std::path::Path;

use lightbox_jobs::CancelToken;
use lightbox_types::{ContentHash, Orientation};

mod probe;

/// What kind of file a probe found (spec §3.7, frozen).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProbedFormat {
    /// A camera raw; the tag is the format's canonical short name
    /// (`"CR3"`, `"NEF"`, …) — exactly what the catalog `format` column
    /// stores.
    Raw(&'static str),
    /// JPEG.
    Jpeg,
    /// TIFF.
    Tiff,
    /// PNG.
    Png,
    /// Anything else — still catalogued, badged, never a crash (spec §3.7).
    /// Carries a short human-readable hint (e.g. the file extension).
    Unsupported(String),
}

impl ProbedFormat {
    /// The catalog `format` column tag for this probe result
    /// (`'CR3'`, …, `'JPEG'`, `'UNSUPPORTED'` — spec §4.3).
    pub fn catalog_tag(&self) -> &str {
        match self {
            ProbedFormat::Raw(tag) => tag,
            ProbedFormat::Jpeg => "JPEG",
            ProbedFormat::Tiff => "TIFF",
            ProbedFormat::Png => "PNG",
            ProbedFormat::Unsupported(_) => "UNSUPPORTED",
        }
    }
}

/// One embedded preview inside an original file (spec §3.7).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EmbeddedPreviewInfo {
    /// Preview width in pixels.
    pub width: u32,
    /// Preview height in pixels.
    pub height: u32,
    /// Byte range of the (JPEG) preview stream within the file.
    pub byte_range: Range<u64>,
}

/// Everything metadata-only probing learns about a file (spec §3.7).
#[derive(Clone, Debug)]
pub struct AssetProbe {
    /// Detected format.
    pub format: ProbedFormat,
    /// Full-size pixel width; `0` when unknown.
    pub width: u32,
    /// Full-size pixel height; `0` when unknown.
    pub height: u32,
    /// EXIF orientation (defaults to `O1` when absent).
    pub orientation: Orientation,
    /// EXIF camera make, if present.
    pub camera_make: Option<String>,
    /// EXIF camera model, if present.
    pub camera_model: Option<String>,
    /// Capture time, RFC3339 (with offset when the file carries one).
    pub capture_time: Option<String>,
    /// File size in bytes.
    pub file_bytes: u64,
    /// Embedded previews, sorted largest-first.
    pub embedded: Vec<EmbeddedPreviewInfo>,
}

/// Errors from probing/hashing. Malformed input is an `Err`, **never** a
/// panic (spec §3.7; fuzz-seeded in Phase 5).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// Reading the file failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The operation observed cancellation at a checkpoint and stopped.
    #[error("cancelled")]
    Cancelled,
    /// The file is recognizably of a supported format but too damaged to
    /// probe (truncated, garbage where structure should be).
    #[error("malformed file: {0}")]
    Malformed(String),
}

/// Metadata-only probe of an original file (spec §3.7). Never panics on
/// malformed input; anything unrecognized is [`ProbedFormat::Unsupported`]
/// — still catalogued, badged, never a crash. Content whose *extension*
/// claims a supported format but whose bytes are unrecognizable (the
/// corrupt corpus) is [`ProbeError::Malformed`].
///
/// Reads structures only (a few KiB even on a 45 MB raw), never image
/// payloads — hashing is separate ([`hash_file`]) and streamed.
pub fn probe(path: &Path) -> Result<AssetProbe, ProbeError> {
    probe::probe_impl(path)
}

/// Reads one embedded preview's byte range from the original file
/// (spec §3.7). The range comes from a prior [`probe`]; a range that no
/// longer fits the file (edited/truncated since probing) is
/// [`ProbeError::Malformed`].
pub fn read_embedded(path: &Path, info: &EmbeddedPreviewInfo) -> Result<Vec<u8>, ProbeError> {
    let len = info
        .byte_range
        .end
        .checked_sub(info.byte_range.start)
        .filter(|l| *l > 0)
        .ok_or_else(|| ProbeError::Malformed("empty embedded-preview range".into()))?;
    let file_len = std::fs::metadata(path)?.len();
    if info.byte_range.end > file_len {
        return Err(ProbeError::Malformed(format!(
            "embedded-preview range {}..{} exceeds file size {file_len}",
            info.byte_range.start, info.byte_range.end
        )));
    }
    let mut file = std::fs::File::open(path)?;
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(info.byte_range.start))?;
    let mut buf = vec![
        0u8;
        usize::try_from(len).map_err(|_| {
            ProbeError::Malformed("embedded-preview range exceeds addressable memory".into())
        })?
    ];
    file.read_exact(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ProbeError::Malformed("embedded preview truncated on disk".into())
        } else {
            ProbeError::Io(e)
        }
    })?;
    Ok(buf)
}

/// Chunk size for [`hash_file`] — 1 MiB per spec §3.7; cancellation is
/// honored between chunks.
const HASH_CHUNK: usize = 1 << 20;

/// Streaming xxh3-128 of the full file (spec §3.7): 1 MiB chunks, honors
/// `cancel` between chunks.
///
/// Canonicalization: the 128-bit value is rendered **big-endian** into
/// [`ContentHash`], matching `xxhsum` and the fixture pins in
/// `fixtures/manifest.toml` (see `tools/xtask/src/hashing.rs`).
pub fn hash_file(path: &Path, cancel: &CancelToken) -> Result<ContentHash, ProbeError> {
    if cancel.is_cancelled() {
        return Err(ProbeError::Cancelled);
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = twox_hash::XxHash3_128::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(ProbeError::Cancelled);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(ContentHash(hasher.finish_128().to_be_bytes()))
}

// ---------------------------------------------------------------------------
// Declared now, implemented by E02 (spec §3.7 / §1.1: the E02 seam). Only
// the *signatures* are frozen; the option/image types below are placeholders
// whose real bodies E02 designs — `#[non_exhaustive]` keeps callers honest.
// ---------------------------------------------------------------------------

/// Options for raw decode (E02 designs the real fields).
#[non_exhaustive]
#[derive(Clone, Debug, Default)]
pub struct DecodeOpts {}

/// A mosaic (pre-demosaic) raw image. E02 designs the real body.
#[non_exhaustive]
#[derive(Debug)]
pub struct MosaicImage {}

/// A linear-light decoded image (non-raw originals). E02 designs the real body.
#[non_exhaustive]
#[derive(Debug)]
pub struct LinearImage {}

/// Errors from the (E02) decode surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// E01 ships probe + embedded previews only (architecture §9 M0: zero
    /// dependency on raw decode); E02 replaces this with the real pipeline.
    #[error("raw/image decode is not implemented until E02")]
    Unimplemented,
}

/// Decodes a raw file to its mosaic image. **E02 implements** (spec §2.2);
/// E01 only freezes the signature.
pub fn decode_raw(path: &Path, opts: DecodeOpts) -> Result<MosaicImage, DecodeError> {
    let _ = (path, opts);
    Err(DecodeError::Unimplemented)
}

/// Decodes a non-raw image (JPEG/TIFF/PNG) to linear light. **E02
/// implements**; E01 only freezes the signature.
pub fn decode_image(path: &Path) -> Result<LinearImage, DecodeError> {
    let _ = path;
    Err(DecodeError::Unimplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_file_matches_oneshot_and_canonical_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        // Larger than one chunk to exercise the streaming loop.
        let data: Vec<u8> = (0..(2 * HASH_CHUNK + 313))
            .map(|i| (i % 249) as u8)
            .collect();
        std::fs::write(&path, &data).unwrap();

        let hash = hash_file(&path, &CancelToken::new()).unwrap();
        let oneshot = twox_hash::XxHash3_128::oneshot(&data);
        assert_eq!(hash.0, oneshot.to_be_bytes());
        // Canonical rendering == xtask's fixture-pin rendering ({:032x}).
        assert_eq!(hash.to_hex(), format!("{oneshot:032x}"));
    }

    #[test]
    fn hash_file_honors_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, vec![7u8; 64]).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            hash_file(&path, &cancel),
            Err(ProbeError::Cancelled)
        ));
    }

    #[test]
    fn hash_file_missing_file_is_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.raw");
        assert!(matches!(
            hash_file(&missing, &CancelToken::new()),
            Err(ProbeError::Io(_))
        ));
    }

    #[test]
    fn probe_of_junk_claiming_a_supported_extension_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0001.jpg");
        std::fs::write(&path, b"not really a jpeg").unwrap();
        assert!(matches!(probe(&path), Err(ProbeError::Malformed(_))));
    }

    #[test]
    fn probe_of_unknown_junk_is_unsupported_with_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"not an image and not claiming to be").unwrap();
        let p = probe(&path).unwrap();
        assert!(matches!(p.format, ProbedFormat::Unsupported(_)));
        assert_eq!(p.format.catalog_tag(), "UNSUPPORTED");
        assert_eq!(p.file_bytes, 35);
        assert_eq!((p.width, p.height), (0, 0));
        assert_eq!(p.orientation, Orientation::O1);
        assert!(p.embedded.is_empty());
    }

    #[test]
    fn probe_missing_file_errors_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            probe(&dir.path().join("gone.cr3")),
            Err(ProbeError::Io(_))
        ));
    }

    #[test]
    fn read_embedded_validates_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, (0u8..=99).collect::<Vec<_>>()).unwrap();
        let ok = read_embedded(
            &path,
            &EmbeddedPreviewInfo {
                width: 1,
                height: 1,
                byte_range: 10..14,
            },
        )
        .unwrap();
        assert_eq!(ok, vec![10, 11, 12, 13]);
        // Out-of-file and empty ranges are malformed, never a panic.
        #[allow(clippy::reversed_empty_ranges)] // inverted range is the point
        for range in [90..110u64, 5..5u64, 7..3u64] {
            let out = read_embedded(
                &path,
                &EmbeddedPreviewInfo {
                    width: 1,
                    height: 1,
                    byte_range: range,
                },
            );
            assert!(matches!(out, Err(ProbeError::Malformed(_))), "{out:?}");
        }
    }

    #[test]
    fn e02_decode_surface_is_declared_but_unimplemented() {
        assert!(matches!(
            decode_raw(Path::new("x.cr3"), DecodeOpts::default()),
            Err(DecodeError::Unimplemented)
        ));
        assert!(matches!(
            decode_image(Path::new("x.png")),
            Err(DecodeError::Unimplemented)
        ));
    }

    #[test]
    fn catalog_tags() {
        assert_eq!(ProbedFormat::Raw("CR3").catalog_tag(), "CR3");
        assert_eq!(ProbedFormat::Jpeg.catalog_tag(), "JPEG");
        assert_eq!(ProbedFormat::Tiff.catalog_tag(), "TIFF");
        assert_eq!(ProbedFormat::Png.catalog_tag(), "PNG");
        assert_eq!(
            ProbedFormat::Unsupported("x".into()).catalog_tag(),
            "UNSUPPORTED"
        );
    }
}
