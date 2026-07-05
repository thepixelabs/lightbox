// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Canon CR3 probing — a bounded ISO-BMFF (MP4-family) box walker.
//!
//! CR3 stores its metadata as little TIFF blobs inside Canon's `uuid` box in
//! `moov` (`CMT1` = IFD0 tags: make/model/orientation; `CMT2` = Exif tags:
//! capture time, offset, pixel dimensions — both re-parsed by the shared
//! [`crate::probe::tiff`] walker), and its JPEG renditions in three places:
//!
//! - `THMB` box (tiny thumbnail, JPEG at payload+16),
//! - `PRVW` box inside the top-level preview `uuid` (mid-size JPEG at
//!   payload+16),
//! - one full-resolution JPEG as a movie track (`trak` → `stbl`, located via
//!   `co64`/`stco` + `stsz`, dimensioned by `stsd`; identified by an `FFD8`
//!   magic check on the sample, which the CRAW tracks fail).
//!
//! Layouts verified against the R6 fixture; every read is bounds-checked and
//! iteration-capped so malformed boxes yield `Err`, never a panic (T19).

use std::fs::File;
use std::io::Cursor;
use std::ops::Range;

use crate::probe::tiff;
use crate::probe::{Candidate, ProbeParts};
use crate::{ProbeError, ProbedFormat};

/// Canon's metadata uuid (child of `moov`).
const CANON_META_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
/// The top-level preview-container uuid (holds `PRVW`).
const PREVIEW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];

/// Boxes examined per container level (real CR3s have < 20).
const MAX_BOXES_PER_LEVEL: usize = 256;
/// Largest metadata payload read into memory (CMT blobs are a few KiB).
const MAX_META_PAYLOAD: u64 = 4 * 1024 * 1024;

/// One parsed box: tag + absolute payload range.
struct BmffBox {
    tag: [u8; 4],
    payload: Range<u64>,
}

/// Iterates the boxes inside `range`, calling `f` for each. Malformed sizes
/// are an error; unknown tags are simply passed through to `f` (which may
/// ignore them).
fn for_each_box(
    file: &mut File,
    range: Range<u64>,
    mut f: impl FnMut(&mut File, &BmffBox) -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    let mut off = range.start;
    for _ in 0..MAX_BOXES_PER_LEVEL {
        if off + 8 > range.end {
            return Ok(());
        }
        let head = super::read_prefix(file, off, 16)?;
        if head.len() < 8 {
            return Ok(());
        }
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let tag = [head[4], head[5], head[6], head[7]];
        let (size, header_len) = match size32 {
            0 => (range.end - off, 8u64), // box extends to end of container
            1 => {
                if head.len() < 16 {
                    return Err(ProbeError::Malformed("truncated 64-bit box size".into()));
                }
                let s = u64::from_be_bytes([
                    head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15],
                ]);
                (s, 16u64)
            }
            s => (u64::from(s), 8u64),
        };
        if size < header_len || off.checked_add(size).is_none_or(|end| end > range.end) {
            return Err(ProbeError::Malformed(format!(
                "box '{}' with impossible size {size}",
                tag.escape_ascii()
            )));
        }
        f(
            file,
            &BmffBox {
                tag,
                payload: (off + header_len)..(off + size),
            },
        )?;
        off += size;
    }
    Ok(())
}

fn range_len(r: &Range<u64>) -> u64 {
    r.end.saturating_sub(r.start)
}

/// Reads a whole (size-capped) box payload into memory.
fn read_payload(file: &mut File, payload: &Range<u64>) -> Result<Vec<u8>, ProbeError> {
    let len = range_len(payload).min(MAX_META_PAYLOAD);
    super::read_prefix(file, payload.start, usize::try_from(len).unwrap_or(0))
}

/// What the `moov`/preview walks accumulate.
#[derive(Default)]
struct Cr3Scan {
    cmt1: Option<tiff::TiffWalk>,
    cmt2: Option<tiff::TiffWalk>,
    candidates: Vec<Candidate>,
}

/// Probes a CR3 file (caller has already sniffed the `ftyp crx ` brand).
pub(crate) fn probe_cr3(file: &mut File, file_len: u64) -> Result<ProbeParts, ProbeError> {
    let mut scan = Cr3Scan::default();

    for_each_box(file, 0..file_len, |file, b| match &b.tag {
        b"moov" => walk_moov(file, b, &mut scan),
        b"uuid" => walk_top_uuid(file, b, &mut scan),
        _ => Ok(()),
    })?;

    let (make, model, orientation) = match &scan.cmt1 {
        Some(w) => (
            w.first(|i| i.make.clone()),
            w.first(|i| i.model.clone()),
            w.first(|i| i.orientation),
        ),
        None => (None, None, None),
    };
    let capture_time = scan.cmt2.as_ref().and_then(|w| {
        let dt = w
            .first(|i| i.date_time_original.clone())
            .or_else(|| w.first(|i| i.date_time_digitized.clone()))
            .or_else(|| w.first(|i| i.date_time.clone()))?;
        let offset = w.first(|i| i.offset_time_original.clone());
        super::exif_datetime_to_rfc3339(&dt, offset.as_deref())
    });
    // Full-size dims: CMT2's PixelX/YDimension, else CMT1's ImageWidth/Length.
    let dims = scan
        .cmt2
        .as_ref()
        .and_then(|w| Some((w.first(|i| i.pixel_x)?, w.first(|i| i.pixel_y)?)))
        .or_else(|| {
            let w = scan.cmt1.as_ref()?;
            Some((w.first(|i| i.width)?, w.first(|i| i.height)?))
        });
    let (width, height) = dims.unwrap_or((0, 0));

    Ok(ProbeParts {
        format: ProbedFormat::Raw("CR3"),
        width,
        height,
        orientation_exif: orientation,
        make,
        model,
        capture_time,
        candidates: scan.candidates,
    })
}

fn walk_moov(file: &mut File, moov: &BmffBox, scan: &mut Cr3Scan) -> Result<(), ProbeError> {
    for_each_box(file, moov.payload.clone(), |file, b| match &b.tag {
        b"uuid" => {
            let head = super::read_prefix(file, b.payload.start, 16)?;
            if head.as_slice() == CANON_META_UUID {
                walk_canon_meta(file, (b.payload.start + 16)..b.payload.end, scan)?;
            }
            Ok(())
        }
        b"trak" => walk_trak(file, b, scan),
        _ => Ok(()),
    })
}

fn walk_canon_meta(
    file: &mut File,
    range: Range<u64>,
    scan: &mut Cr3Scan,
) -> Result<(), ProbeError> {
    for_each_box(file, range, |file, b| {
        match &b.tag {
            b"CMT1" | b"CMT2" => {
                let blob = read_payload(file, &b.payload)?;
                // A broken metadata blob must not sink the whole probe: the
                // previews and the other CMT box may still be fine.
                match tiff::walk(&mut Cursor::new(&blob)) {
                    Ok(walked) if b.tag == *b"CMT1" => scan.cmt1 = Some(walked),
                    Ok(walked) => scan.cmt2 = Some(walked),
                    Err(err) => tracing::debug!(
                        target: "lightbox_decode",
                        box_tag = %b.tag.escape_ascii(),
                        %err,
                        "CR3 metadata blob unreadable"
                    ),
                }
                Ok(())
            }
            b"THMB" => {
                // Payload: u32 version/flags, u16 width, u16 height,
                // u32 jpeg_size, u32 unknown, then the JPEG.
                let head = super::read_prefix(file, b.payload.start, 12)?;
                if head.len() == 12 {
                    let len = u64::from(u32::from_be_bytes([head[8], head[9], head[10], head[11]]));
                    push_candidate(scan, b.payload.start + 16, len, &b.payload);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    })
}

fn walk_top_uuid(file: &mut File, uuid: &BmffBox, scan: &mut Cr3Scan) -> Result<(), ProbeError> {
    let head = super::read_prefix(file, uuid.payload.start, 16)?;
    if head.as_slice() != PREVIEW_UUID {
        return Ok(());
    }
    // 8 mystery bytes precede the child boxes in the preview container.
    let children = (uuid.payload.start + 16 + 8)..uuid.payload.end;
    for_each_box(file, children, |file, b| {
        if &b.tag == b"PRVW" {
            // Payload: u32 version/flags, u16 unknown, u16 width, u16 height,
            // u16 unknown, u32 jpeg_size, then the JPEG.
            let head = super::read_prefix(file, b.payload.start, 16)?;
            if head.len() == 16 {
                let len = u64::from(u32::from_be_bytes([head[12], head[13], head[14], head[15]]));
                push_candidate(scan, b.payload.start + 16, len, &b.payload);
            }
        }
        Ok(())
    })
}

/// Candidate previews must stay inside their carrying box. The box-claimed
/// dimensions are deliberately ignored: the vetting pass reads the JPEG's
/// own SOF, which is the truth.
fn push_candidate(scan: &mut Cr3Scan, offset: u64, len: u64, within: &Range<u64>) {
    if len == 0 || offset.checked_add(len).is_none_or(|end| end > within.end) {
        return;
    }
    scan.candidates.push(Candidate { offset, len });
}

/// Track walk: `trak` → `mdia` → `minf` → `stbl` → (`stsd`, `stsz`,
/// `co64`/`stco`). Single-sample tracks whose sample is a JPEG stream are
/// preview candidates (the full-size JPEG rendition lives here).
fn walk_trak(file: &mut File, trak: &BmffBox, scan: &mut Cr3Scan) -> Result<(), ProbeError> {
    let mut stbl: Option<Range<u64>> = None;
    let mut mdia: Option<Range<u64>> = None;
    let mut minf: Option<Range<u64>> = None;

    for_each_box(file, trak.payload.clone(), |_f, b| {
        if &b.tag == b"mdia" {
            mdia = Some(b.payload.clone());
        }
        Ok(())
    })?;
    let Some(mdia) = mdia else { return Ok(()) };
    for_each_box(file, mdia, |_f, b| {
        if &b.tag == b"minf" {
            minf = Some(b.payload.clone());
        }
        Ok(())
    })?;
    let Some(minf) = minf else { return Ok(()) };
    for_each_box(file, minf, |_f, b| {
        if &b.tag == b"stbl" {
            stbl = Some(b.payload.clone());
        }
        Ok(())
    })?;
    let Some(stbl) = stbl else { return Ok(()) };

    let mut has_visual_entry = false;
    let mut sample_size: Option<u64> = None;
    let mut sample_offset: Option<u64> = None;

    for_each_box(file, stbl, |file, b| {
        match &b.tag {
            // stsd: u32 version/flags, u32 entry_count, then the first
            // sample entry: u32 size, u32 format, then the fixed
            // VisualSampleEntry fields — width/height at entry offset 32/34.
            b"stsd" => {
                let head = super::read_prefix(file, b.payload.start, 8 + 36)?;
                if head.len() == 44 {
                    let entry_count = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
                    if entry_count >= 1 {
                        let w = u32::from(u16::from_be_bytes([head[8 + 32], head[8 + 33]]));
                        let h = u32::from(u16::from_be_bytes([head[8 + 34], head[8 + 35]]));
                        has_visual_entry = w > 0 && h > 0;
                    }
                }
                Ok(())
            }
            // stsz: u32 version/flags, u32 fixed_size, u32 count[, sizes…].
            b"stsz" => {
                let head = super::read_prefix(file, b.payload.start, 16)?;
                if head.len() >= 12 {
                    let fixed = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
                    let count = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
                    if count == 1 {
                        sample_size = if fixed > 0 {
                            Some(u64::from(fixed))
                        } else if head.len() >= 16 {
                            Some(u64::from(u32::from_be_bytes([
                                head[12], head[13], head[14], head[15],
                            ])))
                        } else {
                            None
                        };
                    }
                }
                Ok(())
            }
            // co64/stco: u32 version/flags, u32 count, then offsets.
            b"co64" | b"stco" => {
                let head = super::read_prefix(file, b.payload.start, 16)?;
                if head.len() >= 12 {
                    let count = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
                    if count == 1 {
                        sample_offset = if &b.tag == b"co64" && head.len() >= 16 {
                            Some(u64::from_be_bytes([
                                head[8], head[9], head[10], head[11], head[12], head[13], head[14],
                                head[15],
                            ]))
                        } else {
                            Some(u64::from(u32::from_be_bytes([
                                head[8], head[9], head[10], head[11],
                            ])))
                        };
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    })?;

    if has_visual_entry {
        if let (Some(len), Some(offset)) = (sample_size, sample_offset) {
            if len > 0 {
                // The FFD8 + SOF vetting happens with every other candidate
                // in `validate_candidates`; CRAW tracks are rejected there.
                scan.candidates.push(Candidate { offset, len });
            }
        }
    }
    Ok(())
}
