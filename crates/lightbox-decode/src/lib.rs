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
//! **Status: E02 Phase A.** [`probe`] is real: content-sniffed dispatch into
//! bounded, never-panicking container walkers — an in-crate TIFF/IFD walker for
//! CR2/NEF/ARW/ORF/DNG/TIFF, an ISO-BMFF walker for CR3, the RAF fixed header,
//! JPEG SOF scan + `kamadak-exif`, PNG IHDR. [`read_embedded`] does the ranged
//! read of a probed preview. E02 Phase A adds: [`normalize_camera`] (A4),
//! [`linearize`] (A6), [`decode_image`] for JPEG/PNG/TIFF → [`SourceImage`]
//! (A7), the [`DecodeError`] taxonomy + `catalog_code` (A1), and `catch_unwind`
//! panic containment at every decode entry point (A9). [`decode_raw`]'s body is
//! a scaffold — mosaic decode is the Phase-C LibRaw proxy, in-crate linear/mono
//! decode is A5 (see `raw::types` for the shared contract types those phases
//! fill).
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
use lightbox_types::{ContentHash, Orientation, SourceKind};

mod probe;

// E02 Phase A surface (extends the E01 probe surface above).
mod camera;
mod error;
mod image;
mod panic;
mod raw;

pub use camera::{normalize_camera, CameraId};
pub use error::{CapKind, DecodeError};
pub use raw::linearize::linearize;
pub use raw::proxy::{
    err_code as proxy_err, DemosaicedMeta, LibrawParams, MosaicMeta, ProxyMeta, ProxyPayloadKind,
    ProxyRequest, ProxyResponse, ShmRef, PROTO_VERSION,
};
pub use raw::proxy_client::{ProxyClient, ProxyConfig, ProxySupervisor};
pub use raw::state::{
    decode_params_hash, f32_samples_to_le_bytes, le_bytes_to_f32_samples, le_bytes_to_u16_samples,
    u16_samples_to_le_bytes, DecodedRawState, DecodedRawStateHeader, StateError,
    LINEARIZE_IMPL_VERSION, MAGIC as RAW_STATE_MAGIC, VERSION as RAW_STATE_VERSION,
};
pub use raw::types::{
    BackendPolicy, BlackLevels, CfaColor, CfaPattern, DecodeBackend, DecodeOpts, Illuminant,
    LinearMosaic, Mat3Array, MosaicBuffer, MosaicImage, RawColorimetry, RawDecode, Rect,
    SourceColor, SourceImage, SourceProvenance,
};

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

    /// E04 spec §4.2/§2.4 source-kind mapping: which develop surface this
    /// format exposes. `None` for `Unsupported` (no develop surface at
    /// all — the item is badged, never opened in the editor). Derived, not
    /// a struct field: `AssetProbe` itself stays untouched (a plain all-pub
    /// struct constructed in walker code and tests; adding a field there
    /// would be a breaking literal-construction change for no benefit over
    /// this derivation).
    pub fn source_kind(&self) -> Option<SourceKind> {
        match self {
            ProbedFormat::Raw(_) => Some(SourceKind::Raw),
            ProbedFormat::Jpeg | ProbedFormat::Tiff | ProbedFormat::Png => {
                Some(SourceKind::Rendered)
            }
            ProbedFormat::Unsupported(_) => None,
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
// E02 decode surface (spec §3.1). Every entry point runs its worker inside
// `panic::guard` so no unwind crosses the boundary (A9): malformed input is
// always a structured `DecodeError`, never a panic.
// ---------------------------------------------------------------------------

/// Raw decode (spec §3.1). CFA-mosaic → LibRaw proxy (primary, Phase C);
/// linear/mono-DNG → in-crate (A5). **Never panics.**
///
/// Phase A ships the frozen signature + panic containment; the mosaic and
/// in-crate-linear bodies land in Phases C/A5 (until then this returns a
/// structured [`DecodeError::Unimplemented`], recorded in E02-deviations.md).
pub fn decode_raw(path: &Path, opts: &DecodeOpts) -> Result<RawDecode, DecodeError> {
    panic::guard(|| raw::decode_raw_impl(path, opts))
}

/// The M1 develop-open decode (spec §5.1 step 3 / task C5): produces a
/// develop-ready [`RawDecode::DemosaicedInterim`] — linear camera-native RGB via
/// the LibRaw proxy's AHD path — for a CFA-mosaic raw (and, until A5's in-crate
/// linear/mono fast path lands, any raw). `sup` is the warm proxy pool. A proxy
/// crash / timeout is a structured [`DecodeError`], **never a panic** and never
/// an in-process fallback (single mosaic backend by license necessity, R1).
pub fn decode_for_develop(
    sup: &ProxySupervisor,
    path: &Path,
    opts: &DecodeOpts,
) -> Result<RawDecode, DecodeError> {
    panic::guard(|| raw::decode_for_develop_impl(sup, path, opts))
}

/// Non-raw decode (JPEG/PNG/TIFF → [`SourceImage`], spec §3.1): normalized
/// display-referred `f32`, embedded ICC carried as [`SourceColor::Tagged`]
/// (untagged ⇒ [`SourceColor::AssumedSrgb`]), EXIF orientation baked in.
/// **Never panics.**
pub fn decode_image(path: &Path) -> Result<SourceImage, DecodeError> {
    panic::guard(|| image::decode_image_impl(path))
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
    fn decode_raw_missing_file_is_io_not_panic() {
        // decode_raw's mosaic/linear bodies are Phase C/A5; the panic guard
        // still turns real I/O failures into structured errors.
        let dir = tempfile::tempdir().unwrap();
        let out = decode_raw(&dir.path().join("gone.cr3"), &DecodeOpts::default());
        assert!(matches!(out, Err(DecodeError::Io(_))), "{out:?}");
    }

    #[test]
    fn decode_image_missing_file_is_io_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let out = decode_image(&dir.path().join("gone.png"));
        assert!(matches!(out, Err(DecodeError::Io(_))), "{out:?}");
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

    /// E04 T1 AC: every `ProbedFormat` variant maps to the right
    /// `SourceKind` (or `None` for `Unsupported`).
    #[test]
    fn source_kind_mapping() {
        assert_eq!(
            ProbedFormat::Raw("CR3").source_kind(),
            Some(SourceKind::Raw)
        );
        assert_eq!(
            ProbedFormat::Raw("NEF").source_kind(),
            Some(SourceKind::Raw)
        );
        assert_eq!(ProbedFormat::Jpeg.source_kind(), Some(SourceKind::Rendered));
        assert_eq!(ProbedFormat::Tiff.source_kind(), Some(SourceKind::Rendered));
        assert_eq!(ProbedFormat::Png.source_kind(), Some(SourceKind::Rendered));
        assert_eq!(ProbedFormat::Unsupported("x".into()).source_kind(), None);
    }
}
