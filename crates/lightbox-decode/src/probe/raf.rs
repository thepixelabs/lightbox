// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Fujifilm RAF probing.
//!
//! RAF is a proprietary container: a fixed header whose big-endian pointers
//! at bytes 84/88 locate a full EXIF JPEG rendition inside the file. All
//! metadata (make/model/orientation/capture time) and the full-size
//! dimensions come from that embedded JPEG, for the RAF fixtures the
//! embedded rendition is camera-full-size.

use std::fs::File;
use std::io::Cursor;

use crate::probe::exif_fields::ExifFields;
use crate::probe::jpeg::{scan_sof, SofInfo, SOF_SCAN_CAP};
use crate::probe::{Candidate, ProbeParts};
use crate::{ProbeError, ProbedFormat};

/// The 16-byte RAF magic.
pub(crate) const RAF_MAGIC: &[u8; 16] = b"FUJIFILMCCD-RAW ";

/// Byte offset of the big-endian `u32` embedded-JPEG offset.
const JPEG_OFFSET_AT: u64 = 84;

/// Probes a RAF file via its embedded JPEG.
pub(crate) fn probe_raf(file: &mut File, file_len: u64) -> Result<ProbeParts, ProbeError> {
    let head = super::read_prefix(file, JPEG_OFFSET_AT, 8)?;
    if head.len() < 8 {
        return Err(ProbeError::Malformed("RAF header truncated".into()));
    }
    let jpeg_off = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
    let jpeg_len = u64::from(u32::from_be_bytes([head[4], head[5], head[6], head[7]]));
    if jpeg_len == 0
        || jpeg_off
            .checked_add(jpeg_len)
            .is_none_or(|end| end > file_len)
    {
        return Err(ProbeError::Malformed(
            "RAF embedded-JPEG pointer outside the file".into(),
        ));
    }

    // The JPEG's leading segments carry everything we need; cap the read.
    let take = usize::try_from(jpeg_len.min(SOF_SCAN_CAP as u64)).unwrap_or(SOF_SCAN_CAP);
    let jpeg_head = super::read_prefix(file, jpeg_off, take)?;
    let sof = scan_sof(&jpeg_head)
        .filter(SofInfo::decodable)
        .ok_or_else(|| ProbeError::Malformed("RAF embedded JPEG has no decodable SOF".into()))?;
    let fields = ExifFields::from_reader(&mut Cursor::new(&jpeg_head));

    let capture_time = fields.capture_time();
    Ok(ProbeParts {
        format: ProbedFormat::Raw("RAF"),
        width: sof.width,
        height: sof.height,
        orientation_exif: fields.orientation,
        make: fields.make,
        model: fields.model,
        capture_time,
        candidates: vec![Candidate {
            offset: jpeg_off,
            len: jpeg_len,
        }],
    })
}
