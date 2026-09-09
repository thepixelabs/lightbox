// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! TIFF decode via the `tiff` codec (MIT). 8- and 16-bit grayscale/RGB(A);
//! 16-bit samples are read as native `u16` so precision is preserved (A7
//! acceptance). Float/32-bit TIFF is out of M1 scope (spec Open Question 6) and
//! surfaces as [`DecodeError::UnsupportedFormat`], never a panic. ICC is read
//! from tag 34675.

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::ColorType;

use super::{normalize_u16, normalize_u8, RawDecoded};
use crate::error::DecodeError;
use crate::raw::types::DecodeBackend;

fn corrupt(e: impl std::fmt::Debug) -> DecodeError {
    DecodeError::CorruptFile {
        detail: format!("tiff: {e:?}"),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<RawDecoded, DecodeError> {
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes)).map_err(corrupt)?;
    let (width, height) = decoder.dimensions().map_err(corrupt)?;
    let color = decoder.colortype().map_err(corrupt)?;

    let channels: u8 = match color {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        other => {
            return Err(DecodeError::UnsupportedFormat {
                format: format!("tiff colortype {other:?}"),
            })
        }
    };

    // ICC is optional; a read failure just means "untagged".
    let icc = decoder
        .find_tag(Tag::IccProfile)
        .ok()
        .flatten()
        .and_then(|v| v.into_u8_vec().ok())
        .filter(|v| !v.is_empty());

    // Note: do NOT `{:?}`-format the DecodingResult on the reject path, it
    // would dump the whole pixel plane. Report the kind by width only.
    let data = match decoder.read_image().map_err(corrupt)? {
        DecodingResult::U8(v) => normalize_u8(&v),
        DecodingResult::U16(v) => normalize_u16(&v),
        _ => {
            return Err(DecodeError::UnsupportedFormat {
                format: "tiff >16-bit / float sample format (8/16-bit only at M1 — spec OQ6)"
                    .to_owned(),
            })
        }
    };

    Ok(RawDecoded {
        data,
        width,
        height,
        channels,
        icc,
        backend: DecodeBackend::ImageTiff,
    })
}
