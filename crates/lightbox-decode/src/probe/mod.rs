// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The metadata-only probe (spec §3.7, T19): content sniffing, per-container
//! walkers, embedded-preview vetting, and `AssetProbe` assembly.
//!
//! Dispatch is **content-first**: magic bytes pick the walker; the file
//! extension only (a) names the raw format when the container itself is
//! ambiguous and (b) decides between `Unsupported` (unknown junk, still
//! catalogued) and `Malformed` (a file *claiming* a supported format whose
//! content is unrecognizable, e.g. the corrupt fixture corpus) for
//! unrecognizable content.
//!
//! Every walker reads structures only, never image payloads, so probing a
//! 45 MB raw touches a few KiB (spec T19: metadata-only, no full read).
//! Malformed input is an `Err`, never a panic; the walkers are additionally
//! seeded as a `cargo-fuzz` target (`fuzz/`, non-gating at M0).

pub(crate) mod cr3;
pub(crate) mod exif_fields;
pub(crate) mod jpeg;
pub(crate) mod png;
pub(crate) mod raf;
pub(crate) mod tiff;

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lightbox_types::Orientation;

use crate::{AssetProbe, EmbeddedPreviewInfo, ProbeError, ProbedFormat};

/// Longest prefix read while vetting one embedded-preview candidate. Must
/// match the walkers' own scan budget: Fujifilm writes ~64 KiB of APP1
/// (EXIF + nested thumbnail) before the SOF, so 64 KiB is not enough.
const CANDIDATE_SCAN_CAP: u64 = jpeg::SOF_SCAN_CAP as u64;

/// An unvetted embedded-JPEG candidate reported by a container walker.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    /// Absolute byte offset of the (claimed) JPEG stream.
    pub offset: u64,
    /// Claimed byte length.
    pub len: u64,
}

/// What a per-container walker learns; assembled into [`AssetProbe`] after
/// candidate vetting.
#[derive(Debug)]
pub(crate) struct ProbeParts {
    /// Detected format.
    pub format: ProbedFormat,
    /// Full-size width per metadata (`0` = unknown; falls back to the
    /// largest vetted preview).
    pub width: u32,
    /// Full-size height per metadata.
    pub height: u32,
    /// Raw EXIF orientation value (validated during assembly).
    pub orientation_exif: Option<u16>,
    /// Camera make.
    pub make: Option<String>,
    /// Camera model.
    pub model: Option<String>,
    /// Capture time, already RFC3339-ified.
    pub capture_time: Option<String>,
    /// Unvetted preview candidates.
    pub candidates: Vec<Candidate>,
}

impl ProbeParts {
    fn unsupported(hint: String) -> ProbeParts {
        ProbeParts {
            format: ProbedFormat::Unsupported(hint),
            width: 0,
            height: 0,
            orientation_exif: None,
            make: None,
            model: None,
            capture_time: None,
            candidates: Vec::new(),
        }
    }
}

/// Extensions that *claim* a format this probe understands. Unrecognizable
/// content behind one of these is [`ProbeError::Malformed`] (catalogued with
/// `decode_error` by ingest, T18); behind any other extension it is
/// [`ProbedFormat::Unsupported`] (catalogued and badged).
const SUPPORTED_CLAIM_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "dng", "jpeg", "jpg", "nef", "orf", "pef", "png", "raf", "rw2", "tif",
    "tiff",
];

/// Maps a claimed raw extension to its canonical catalog tag when the
/// TIFF container itself does not identify the mount.
fn raw_tag_for_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "arw" => "ARW",
        "cr2" => "CR2",
        "cr3" => "CR3",
        "dng" => "DNG",
        "nef" => "NEF",
        "orf" => "ORF",
        "pef" => "PEF",
        "raf" => "RAF",
        "rw2" => "RW2",
        _ => return None,
    })
}

fn claims_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            SUPPORTED_CLAIM_EXTENSIONS
                .iter()
                .any(|k| ext.eq_ignore_ascii_case(k))
        })
}

fn extension_hint(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no extension".to_owned())
}

/// Seeks to `off` and reads **up to** `len` bytes (short at EOF, no error).
pub(crate) fn read_prefix(file: &mut File, off: u64, len: usize) -> Result<Vec<u8>, ProbeError> {
    file.seek(SeekFrom::Start(off)).map_err(ProbeError::Io)?;
    let mut buf = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProbeError::Io(e)),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// EXIF `"YYYY:MM:DD HH:MM:SS"` (also tolerating `-`/`T` separators) →
/// `"YYYY-MM-DDTHH:MM:SS"`, with a validated `±HH:MM` offset appended when
/// the file carries one (spec §3.7: "RFC3339 with offset if present").
pub(crate) fn exif_datetime_to_rfc3339(dt: &str, offset: Option<&str>) -> Option<String> {
    let dt = dt.trim();
    let b = dt.as_bytes();
    if b.len() < 19 {
        return None;
    }
    const DIGITS: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    if !DIGITS.iter().all(|&i| b[i].is_ascii_digit()) {
        return None;
    }
    let seps_ok = matches!(b[4], b':' | b'-')
        && matches!(b[7], b':' | b'-')
        && matches!(b[10], b' ' | b'T')
        && b[13] == b':'
        && b[16] == b':';
    if !seps_ok {
        return None;
    }
    // Field sanity: unset camera clocks write all-zero dates, corrupt ones
    // write nonsense; neither is a capture time worth storing.
    let num = |a: usize, b: usize| dt[a..b].parse::<u32>().ok();
    let in_range = |v: Option<u32>, lo: u32, hi: u32| v.is_some_and(|v| (lo..=hi).contains(&v));
    if !in_range(num(0, 4), 1, 9999)
        || !in_range(num(5, 7), 1, 12)
        || !in_range(num(8, 10), 1, 31)
        || !in_range(num(11, 13), 0, 23)
        || !in_range(num(14, 16), 0, 59)
        || !in_range(num(17, 19), 0, 60)
    {
        return None;
    }
    let mut out = format!("{}-{}-{}T{}", &dt[0..4], &dt[5..7], &dt[8..10], &dt[11..19]);
    if let Some(off) = offset {
        let off = off.trim();
        let ob = off.as_bytes();
        let valid = ob.len() == 6
            && matches!(ob[0], b'+' | b'-')
            && ob[1].is_ascii_digit()
            && ob[2].is_ascii_digit()
            && ob[3] == b':'
            && ob[4].is_ascii_digit()
            && ob[5].is_ascii_digit();
        if valid {
            out.push_str(off);
        }
    }
    Some(out)
}

/// The real body behind [`crate::probe`].
pub(crate) fn probe_impl(path: &Path) -> Result<AssetProbe, ProbeError> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(ProbeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a regular file: {}", path.display()),
        )));
    }
    let file_len = meta.len();
    let mut file = File::open(path)?;
    let head = read_prefix(&mut file, 0, 16)?;

    let parts = sniff_and_walk(&mut file, file_len, path, &head)?;
    let embedded = validate_candidates(&mut file, file_len, &parts.candidates)?;

    // Metadata said nothing about dimensions? The largest vetted preview's
    // SOF is the next-best truth (the RAF path relies on this shape too).
    let (mut width, mut height) = (parts.width, parts.height);
    if (width == 0 || height == 0) && !embedded.is_empty() {
        width = embedded[0].width;
        height = embedded[0].height;
    }
    let orientation = parts
        .orientation_exif
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::O1);

    Ok(AssetProbe {
        format: parts.format,
        width,
        height,
        orientation,
        camera_make: parts.make,
        camera_model: parts.model,
        capture_time: parts.capture_time,
        file_bytes: file_len,
        embedded,
    })
}

/// Content-first dispatch (see module docs for the extension's two roles).
fn sniff_and_walk(
    file: &mut File,
    file_len: u64,
    path: &Path,
    head: &[u8],
) -> Result<ProbeParts, ProbeError> {
    if head.len() >= 3 && head[..3] == [0xFF, 0xD8, 0xFF] {
        return jpeg::probe_jpeg(file, file_len);
    }
    if head.len() >= 8 && head[..8] == png::PNG_SIGNATURE {
        return png::probe_png(file, file_len);
    }
    if head.len() >= 16 && head[..16] == *raf::RAF_MAGIC {
        return raf::probe_raf(file, file_len);
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        if &head[8..12] == b"crx " {
            return cr3::probe_cr3(file, file_len);
        }
        // HEIC/AVIF/MP4/…: recognizably *something*, just not ours yet.
        return Ok(ProbeParts::unsupported(format!(
            "ISO-BMFF container '{}'",
            head[8..12].escape_ascii()
        )));
    }
    if head.len() >= 8 && (head[..2] == *b"II" || head[..2] == *b"MM") {
        let le = head[..2] == *b"II";
        let magic = if le {
            u16::from_le_bytes([head[2], head[3]])
        } else {
            u16::from_be_bytes([head[2], head[3]])
        };
        if matches!(
            magic,
            tiff::MAGIC_TIFF | tiff::MAGIC_ORF_RO | tiff::MAGIC_ORF_RS
        ) {
            return probe_tiff_family(file, path, head);
        }
        // e.g. Panasonic RW2 (magic 0x0055): honest Unsupported until a
        // walker exists, still catalogued, badged, never a crash.
        return Ok(ProbeParts::unsupported(format!(
            "TIFF-family container with magic 0x{magic:04x}"
        )));
    }

    if claims_supported_extension(path) {
        return Err(ProbeError::Malformed(format!(
            "content does not match the .{} extension",
            extension_hint(path)
        )));
    }
    Ok(ProbeParts::unsupported(extension_hint(path)))
}

/// TIFF-family probing: one walk, then classification + field extraction.
fn probe_tiff_family(file: &mut File, path: &Path, head: &[u8]) -> Result<ProbeParts, ProbeError> {
    // CR2 brands itself: "CR" + version at bytes 8..12.
    let is_cr2 = head.len() >= 10 && &head[8..10] == b"CR";
    let walk = tiff::walk(file)?;

    let make = walk.first(|i| i.make.clone());
    let model = walk.first(|i| i.model.clone());
    let orientation = walk.first(|i| i.orientation);
    let capture_time = walk
        .first(|i| i.date_time_original.clone())
        .or_else(|| walk.first(|i| i.date_time_digitized.clone()))
        .or_else(|| walk.first(|i| i.date_time.clone()))
        .and_then(|dt| {
            let offset = walk.first(|i| i.offset_time_original.clone());
            exif_datetime_to_rfc3339(&dt, offset.as_deref())
        });

    // Candidate previews + the "is this IFD a preview?" partition that the
    // full-size-dimension fallback needs.
    let mut candidates = Vec::new();
    let mut non_preview_dims_max: Option<(u32, u32)> = None;
    let mut has_raw_ifd = false;
    for ifd in &walk.ifds {
        let cfa = matches!(ifd.photometric, Some(32803) | Some(34892));
        has_raw_ifd |= cfa;
        let mut is_preview_ifd = false;
        if let Some((off, len)) = ifd.jpeg_range {
            candidates.push(Candidate { offset: off, len });
            is_preview_ifd = true;
        } else if matches!(ifd.compression, Some(6) | Some(7)) && !cfa {
            if let Some((off, len)) = ifd.single_strip {
                candidates.push(Candidate { offset: off, len });
                is_preview_ifd = true;
            }
        }
        if !is_preview_ifd {
            if let Some(dims) = ifd.width.zip(ifd.height) {
                let area = |d: (u32, u32)| u64::from(d.0) * u64::from(d.1);
                if non_preview_dims_max.is_none_or(|best| area(dims) > area(best)) {
                    non_preview_dims_max = Some(dims);
                }
            }
        }
    }

    let format = if is_cr2 {
        ProbedFormat::Raw("CR2")
    } else if matches!(walk.magic, tiff::MAGIC_ORF_RO | tiff::MAGIC_ORF_RS) {
        ProbedFormat::Raw("ORF")
    } else if walk.ifds.iter().any(|i| i.dng_version) {
        ProbedFormat::Raw("DNG")
    } else if has_raw_ifd {
        // A CFA/LinearRaw image in a plain-magic TIFF: identify the mount by
        // make, then by extension, then generically.
        let by_make = make.as_deref().and_then(|m| {
            let upper = m.to_ascii_uppercase();
            if upper.starts_with("NIKON") {
                Some("NEF")
            } else if upper.starts_with("SONY") {
                Some("ARW")
            } else if upper.starts_with("PENTAX") || upper.starts_with("RICOH") {
                Some("PEF")
            } else {
                None
            }
        });
        match by_make.or_else(|| raw_tag_for_extension(path)) {
            Some(tag) => ProbedFormat::Raw(tag),
            None => ProbedFormat::Raw("RAW"),
        }
    } else {
        ProbedFormat::Tiff
    };

    // Full-size dimensions: EXIF PixelX/YDimension is authoritative (crop
    // modes!); otherwise the largest non-preview IFD (the raw mosaic's own
    // dims, sensor-area, close enough for aspect at M0); otherwise the
    // assembly falls back to the largest vetted preview.
    let exif_dims = walk
        .first(|i| i.pixel_x)
        .zip(walk.first(|i| i.pixel_y))
        .filter(|&(x, y)| x > 0 && y > 0);
    let (width, height) = exif_dims.or(non_preview_dims_max).unwrap_or((0, 0));

    Ok(ProbeParts {
        format,
        width,
        height,
        orientation_exif: orientation,
        make,
        model,
        capture_time,
        candidates,
    })
}

/// Vets candidates: in-bounds, deduped by offset, `FFD8`-headed, and carrying
/// a *decodable* SOF (SOF3 lossless, the compression inside CR2/DNG raw
/// IFDs, is rejected here). SOF dimensions override container claims.
/// Result is sorted largest-first (spec §3.7).
fn validate_candidates(
    file: &mut File,
    file_len: u64,
    candidates: &[Candidate],
) -> Result<Vec<EmbeddedPreviewInfo>, ProbeError> {
    let mut seen = HashSet::new();
    let mut out: Vec<EmbeddedPreviewInfo> = Vec::new();
    for c in candidates {
        if c.len < 16 || c.offset.checked_add(c.len).is_none_or(|end| end > file_len) {
            continue;
        }
        if !seen.insert(c.offset) {
            continue;
        }
        let take = usize::try_from(c.len.min(CANDIDATE_SCAN_CAP)).unwrap_or(0);
        let head = read_prefix(file, c.offset, take)?;
        match jpeg::scan_sof(&head) {
            Some(sof) if sof.decodable() => out.push(EmbeddedPreviewInfo {
                width: sof.width,
                height: sof.height,
                byte_range: c.offset..c.offset + c.len,
            }),
            _ => {
                tracing::trace!(
                    target: "lightbox_decode",
                    offset = c.offset,
                    len = c.len,
                    "preview candidate rejected (not a decodable JPEG stream)"
                );
            }
        }
    }
    out.sort_by(|a, b| {
        let area = |p: &EmbeddedPreviewInfo| u64::from(p.width) * u64::from(p.height);
        area(b)
            .cmp(&area(a))
            .then(
                (b.byte_range.end - b.byte_range.start)
                    .cmp(&(a.byte_range.end - a.byte_range.start)),
            )
            .then(a.byte_range.start.cmp(&b.byte_range.start))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_datetime_conversion() {
        assert_eq!(
            exif_datetime_to_rfc3339("2021:05:12 12:21:21", None).as_deref(),
            Some("2021-05-12T12:21:21")
        );
        assert_eq!(
            exif_datetime_to_rfc3339("2021:05:12 12:21:21", Some("+02:00")).as_deref(),
            Some("2021-05-12T12:21:21+02:00")
        );
        assert_eq!(
            exif_datetime_to_rfc3339("2024-02-29T19:09:57", Some("-04:00")).as_deref(),
            Some("2024-02-29T19:09:57-04:00")
        );
        // Invalid offsets are dropped, not mangled in.
        assert_eq!(
            exif_datetime_to_rfc3339("2021:05:12 12:21:21", Some("junk")).as_deref(),
            Some("2021-05-12T12:21:21")
        );
        // Unset camera clocks, out-of-range fields, and garbage are None.
        assert_eq!(exif_datetime_to_rfc3339("0000:00:00 00:00:00", None), None);
        assert_eq!(exif_datetime_to_rfc3339("", None), None);
        assert_eq!(exif_datetime_to_rfc3339("yesterday-ish", None), None);
        assert_eq!(exif_datetime_to_rfc3339("2021:13:99 99:99:99", None), None);
    }

    #[test]
    fn extension_claims() {
        assert!(claims_supported_extension(Path::new("a/IMG.CR3")));
        assert!(claims_supported_extension(Path::new("a/img.jpeg")));
        assert!(!claims_supported_extension(Path::new("a/notes.txt")));
        assert!(!claims_supported_extension(Path::new("a/noext")));
        assert_eq!(raw_tag_for_extension(Path::new("x.NEF")), Some("NEF"));
        assert_eq!(raw_tag_for_extension(Path::new("x.tiff")), None);
    }
}
