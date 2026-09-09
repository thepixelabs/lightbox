// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Thin extraction layer over `kamadak-exif` (spec §3.7 names the crate for
//! the JPEG/TIFF/PNG legs). Only the fields `AssetProbe` carries are pulled;
//! parse failures degrade to "no fields", never to a probe failure, a JPEG
//! without EXIF is a perfectly good JPEG.

use exif::{In, Tag, Value};

/// The probe-relevant EXIF fields.
#[derive(Default, Debug)]
pub(crate) struct ExifFields {
    /// EXIF orientation (raw 1..=8 value, unvalidated).
    pub orientation: Option<u16>,
    /// Camera make.
    pub make: Option<String>,
    /// Camera model.
    pub model: Option<String>,
    /// `DateTimeOriginal` (preferred) / `DateTimeDigitized` / `DateTime`,
    /// EXIF-formatted (`YYYY:MM:DD HH:MM:SS`).
    pub date_time: Option<String>,
    /// `OffsetTimeOriginal` (e.g. `"+01:00"`).
    pub offset_time: Option<String>,
}

impl ExifFields {
    /// Reads EXIF from any container `kamadak-exif` understands (JPEG APP1,
    /// TIFF, PNG `eXIf`, …). Absent/corrupt EXIF ⇒ all-`None` fields.
    pub fn from_reader<R: std::io::BufRead + std::io::Seek>(reader: &mut R) -> ExifFields {
        match exif::Reader::new().read_from_container(reader) {
            Ok(exif) => ExifFields::from_exif(&exif),
            Err(_) => ExifFields::default(),
        }
    }

    fn from_exif(exif: &exif::Exif) -> ExifFields {
        ExifFields {
            orientation: uint(exif, Tag::Orientation).map(|v| v as u16),
            make: ascii(exif, Tag::Make),
            model: ascii(exif, Tag::Model),
            date_time: ascii(exif, Tag::DateTimeOriginal)
                .or_else(|| ascii(exif, Tag::DateTimeDigitized))
                .or_else(|| ascii(exif, Tag::DateTime)),
            offset_time: ascii(exif, Tag::OffsetTimeOriginal),
        }
    }

    /// The RFC3339-ish capture time (see [`crate::probe::exif_datetime_to_rfc3339`]).
    pub fn capture_time(&self) -> Option<String> {
        crate::probe::exif_datetime_to_rfc3339(
            self.date_time.as_deref()?,
            self.offset_time.as_deref(),
        )
    }
}

fn uint(exif: &exif::Exif, tag: Tag) -> Option<u32> {
    exif.get_field(tag, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
}

fn ascii(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    let Value::Ascii(chunks) = &field.value else {
        return None;
    };
    let first = chunks.first()?;
    let end = first.iter().position(|&b| b == 0).unwrap_or(first.len());
    let s = String::from_utf8_lossy(&first[..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}
