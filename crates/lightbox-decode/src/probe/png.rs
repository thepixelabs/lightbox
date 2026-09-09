// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! PNG probing: IHDR header parse for dimensions (the IHDR chunk is
//! mandatory and always first, per the PNG spec) plus a best-effort
//! `kamadak-exif` pass for the rare `eXIf` chunk.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};

use crate::probe::exif_fields::ExifFields;
use crate::probe::ProbeParts;
use crate::{ProbeError, ProbedFormat};

/// The 8-byte PNG signature.
pub(crate) const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Probes a PNG. PNGs carry no embedded JPEG preview; at M0 they catalogue
/// with correct dimensions and render a placeholder until E02 decodes them.
pub(crate) fn probe_png(file: &mut File, _file_len: u64) -> Result<ProbeParts, ProbeError> {
    // Signature (8) + IHDR length/tag (8) + width/height (8).
    let head = super::read_prefix(file, 0, 24)?;
    if head.len() < 24 {
        return Err(ProbeError::Malformed("PNG shorter than its IHDR".into()));
    }
    if &head[12..16] != b"IHDR" {
        return Err(ProbeError::Malformed(
            "PNG signature without a leading IHDR chunk".into(),
        ));
    }
    let width = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
    let height = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
    if width == 0 || height == 0 {
        return Err(ProbeError::Malformed("PNG IHDR with zero dimension".into()));
    }

    // eXIf chunk support is a kamadak feature; virtually all PNGs have none.
    file.seek(SeekFrom::Start(0)).map_err(ProbeError::Io)?;
    let fields = ExifFields::from_reader(&mut BufReader::new(&mut *file));

    let capture_time = fields.capture_time();
    Ok(ProbeParts {
        format: ProbedFormat::Png,
        width,
        height,
        orientation_exif: fields.orientation,
        make: fields.make,
        model: fields.model,
        capture_time,
        candidates: Vec::new(),
    })
}
