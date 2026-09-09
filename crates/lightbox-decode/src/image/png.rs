// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! PNG decode via the `png` codec (MIT/Apache-2.0). Handles 8- and 16-bit
//! grayscale/RGB(A); palette and sub-8-bit images are expanded. ICC is read
//! from the `iCCP` chunk.

use png::Transformations;

use super::{normalize_u16, normalize_u8, RawDecoded};
use crate::error::DecodeError;
use crate::raw::types::DecodeBackend;

/// The 8-byte PNG signature.
pub(crate) const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

fn corrupt(e: impl std::fmt::Debug) -> DecodeError {
    DecodeError::CorruptFile {
        detail: format!("png: {e:?}"),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<RawDecoded, DecodeError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Expand palette → RGB and sub-8-bit → 8-bit, and expand tRNS to a real
    // alpha channel. 16-bit depth is preserved (no STRIP_16).
    decoder.set_transformations(Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(corrupt)?;

    let icc = reader.info().icc_profile.as_ref().map(|c| c.to_vec());

    let size = reader
        .output_buffer_size()
        .ok_or_else(|| corrupt("output buffer size overflow"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(corrupt)?;
    buf.truncate(info.buffer_size());

    let channels = u8::try_from(info.color_type.samples())
        .map_err(|_| corrupt("implausible channel count"))?;

    let data = match info.bit_depth {
        png::BitDepth::Sixteen => {
            // 16-bit output is big-endian sample pairs.
            let samples: Vec<u16> = buf
                .chunks_exact(2)
                .map(|p| u16::from_be_bytes([p[0], p[1]]))
                .collect();
            normalize_u16(&samples)
        }
        _ => normalize_u8(&buf),
    };

    Ok(RawDecoded {
        data,
        width: info.width,
        height: info.height,
        channels,
        icc,
        backend: DecodeBackend::ImagePng,
    })
}
