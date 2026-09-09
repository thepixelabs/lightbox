// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! JPEG decode via `zune-jpeg` (MIT). Default output is 8-bit interleaved RGB;
//! ICC is read from the APP2 marker chain.

use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::JpegDecoder;

use super::{normalize_u8, RawDecoded};
use crate::error::DecodeError;
use crate::raw::types::DecodeBackend;

pub(crate) fn decode(bytes: &[u8]) -> Result<RawDecoded, DecodeError> {
    let mut decoder = JpegDecoder::new(ZCursor::new(bytes));
    let pixels = decoder.decode().map_err(|e| DecodeError::CorruptFile {
        detail: format!("jpeg: {e:?}"),
    })?;
    let info = decoder.info().ok_or_else(|| DecodeError::CorruptFile {
        detail: "jpeg: headers missing after decode".to_owned(),
    })?;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    let px = (width as usize) * (height as usize);
    if px == 0 {
        return Err(DecodeError::CorruptFile {
            detail: "jpeg: zero-area image".to_owned(),
        });
    }
    // Derive channels from the decoded buffer rather than trusting a header
    // field, so a mismatch is caught rather than mis-striding.
    if !pixels.len().is_multiple_of(px) {
        return Err(DecodeError::CorruptFile {
            detail: format!("jpeg: buffer {} not divisible by {px} pixels", pixels.len()),
        });
    }
    let channels = u8::try_from(pixels.len() / px).map_err(|_| DecodeError::CorruptFile {
        detail: "jpeg: implausible channel count".to_owned(),
    })?;

    Ok(RawDecoded {
        data: normalize_u8(&pixels),
        width,
        height,
        channels,
        icc: decoder.icc_profile(),
        backend: DecodeBackend::ImageJpeg,
    })
}
