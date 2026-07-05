// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-decode` — file probing, hashing, and (later) raw decode.
//!
//! Owned by **E01 for the probe surface only** (spec §3.7): `probe()`
//! (rawler metadata for raws, header/EXIF parse for JPEG/TIFF/PNG),
//! `read_embedded()`, streaming xxh3-128 [`hash_file`]. `decode_raw` /
//! `decode_image` / camera-matrix color are declared by E01 but
//! **implemented by E02** — E01 takes zero dependency on raw decode
//! (architecture §9 M0).
//!
//! **Status: Phase-4 slice.** [`hash_file`] is final (T17 needs content
//! hashes for dup-skip). [`probe`] is the **T19 stub** the spec's task order
//! explicitly tolerates ("T19 stub returns `Unsupported` until Phase 5
//! lands", T17): it stats the file and reports [`ProbedFormat::Unsupported`]
//! for everything, never panicking. Phase 5 (T19/T20) replaces the body with
//! the real rawler/EXIF probing plus `read_embedded`, behind these exact
//! frozen types.

use std::io::Read;
use std::ops::Range;
use std::path::Path;

use lightbox_jobs::CancelToken;
use lightbox_types::{ContentHash, Orientation};

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
/// malformed input; anything unrecognized is [`ProbedFormat::Unsupported`].
///
/// **Phase-4 stub (T19 lands the real body):** stats the file and returns
/// `Unsupported(<extension>)` with unknown dimensions for *every* input —
/// the task order spec T17 explicitly tolerates ("T19 stub returns
/// `Unsupported` until Phase 5 lands"). Files imported while the stub is in
/// force are catalogued as `'UNSUPPORTED'`.
pub fn probe(path: &Path) -> Result<AssetProbe, ProbeError> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(ProbeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a regular file: {}", path.display()),
        )));
    }
    let hint = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no extension".to_owned());
    Ok(AssetProbe {
        format: ProbedFormat::Unsupported(format!("probe pending T19 ({hint})")),
        width: 0,
        height: 0,
        orientation: Orientation::O1,
        camera_make: None,
        camera_model: None,
        capture_time: None,
        file_bytes: meta.len(),
        embedded: Vec::new(),
    })
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
    fn probe_stub_reports_unsupported_with_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0001.jpg");
        std::fs::write(&path, b"not really a jpeg").unwrap();
        let p = probe(&path).unwrap();
        assert!(matches!(p.format, ProbedFormat::Unsupported(_)));
        assert_eq!(p.format.catalog_tag(), "UNSUPPORTED");
        assert_eq!(p.file_bytes, 17);
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
