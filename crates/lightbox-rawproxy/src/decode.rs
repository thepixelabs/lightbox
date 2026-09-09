// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! LibRaw-backed decode (E02 Phase C, task C2), compiled only under the
//! `libraw` feature. Translates the flat FFI output ([`crate::libraw_ffi`]) into
//! the wire types the supervisor client reconstructs `MosaicImage` /
//! `SourceImage` from, and produces the out-of-band `u16` payload bytes.
//!
//! Colorimetry mapping (LibRaw → `RawColorimetry`, factual data only):
//! - `as_shot_neutral` ← normalized `cam_mul` (green = 1).
//! - `color_matrix1`   ← `cam_xyz` (XYZ→camera; ColorMatrix1 semantics).
//! - `illuminant1`     ← D65 (LibRaw's `adobe_coeff` matrices are D65-referenced).
//! - `forward_matrix`  ← none (LibRaw exposes no ForwardMatrix; the tier-1 base
//!   uses the inverse-ColorMatrix + Bradford path).

use lightbox_decode::{
    BlackLevels, CfaColor, CfaPattern, DemosaicedMeta, Illuminant, Mat3Array, MosaicMeta,
    ProxyMeta, ProxyPayloadKind, RawColorimetry, Rect,
};

use crate::libraw_ffi::{self, Meta, RawFile};
use crate::limits;

/// A decoded reply: the wire metadata plus the raw little-endian payload bytes.
pub struct Decoded {
    pub meta: ProxyMeta,
    pub payload: Vec<u8>,
}

/// A structured decode failure `(code, detail)`, the codes are `err_code::*`.
pub type DecodeErr = (&'static str, String);

const IDENTITY3: Mat3Array = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The linked LibRaw version.
pub fn libraw_version() -> String {
    libraw_ffi::version()
}

/// `DecodeMosaic`: unpack the CFA plane and return it out-of-band with the
/// geometry + calibration needed to rebuild a `MosaicImage`.
pub fn decode_mosaic(path: &std::path::Path) -> Result<Decoded, DecodeErr> {
    use lightbox_decode::proxy_err as err;
    let file = RawFile::open(path).map_err(|e| (err::OPEN_FAILED, e))?;
    let m = file.meta().map_err(|e| (err::OPEN_FAILED, e))?;

    if !limits::pixels_ok(m.raw_width, m.raw_height) {
        return Err((
            err::RESOURCE_CAP_DIMENSIONS,
            format!("{}x{} exceeds the pixel cap", m.raw_width, m.raw_height),
        ));
    }

    let samples = file
        .raw_plane(m.raw_width, m.raw_height)
        .map_err(|e| (err::OPEN_FAILED, e))?;

    let mosaic = MosaicMeta {
        cfa: cfa_pattern(&m),
        active_area: rect(m.active),
        default_crop: Rect {
            x: m.active.0,
            y: m.active.1,
            width: m.crop.0.min(m.active.2),
            height: m.crop.1.min(m.active.3),
        },
        black_levels: BlackLevels {
            levels: m.black_pos,
        },
        white_levels: [m.white; 4],
        linearization: None,
        colorimetry: colorimetry(&m),
        orientation_exif: flip_to_exif(m.flip),
    };

    let payload = u16_le_bytes(&samples);
    check_payload(payload.len())?;

    Ok(Decoded {
        meta: ProxyMeta {
            width: m.raw_width,
            height: m.raw_height,
            libraw_version: Some(libraw_ffi::version()),
            payload: ProxyPayloadKind::Mosaic(mosaic),
        },
        payload,
    })
}

/// `DecodeDemosaiced`: the interim AHD develop path, 16-bit linear
/// camera-native RGB, no WB / no output color / no gamma.
pub fn decode_demosaiced(path: &std::path::Path) -> Result<Decoded, DecodeErr> {
    use lightbox_decode::proxy_err as err;
    let file = RawFile::open(path).map_err(|e| (err::OPEN_FAILED, e))?;
    let m = file.meta().map_err(|e| (err::OPEN_FAILED, e))?;

    if !limits::pixels_ok(m.raw_width, m.raw_height) {
        return Err((
            err::RESOURCE_CAP_DIMENSIONS,
            format!("{}x{} exceeds the pixel cap", m.raw_width, m.raw_height),
        ));
    }

    let (w, h, channels, samples) = file.process_ahd().map_err(|e| (err::OPEN_FAILED, e))?;

    let demosaiced = DemosaicedMeta {
        channels,
        max_value: 65535,
        colorimetry: colorimetry(&m),
        orientation_exif: flip_to_exif(m.flip),
    };

    let payload = u16_le_bytes(&samples);
    check_payload(payload.len())?;

    Ok(Decoded {
        meta: ProxyMeta {
            width: w,
            height: h,
            libraw_version: Some(libraw_ffi::version()),
            payload: ProxyPayloadKind::Demosaiced(demosaiced),
        },
        payload,
    })
}

fn check_payload(len: usize) -> Result<(), DecodeErr> {
    if len as u64 > limits::MAX_PAYLOAD_BYTES {
        return Err((
            lightbox_decode::proxy_err::RESOURCE_CAP_PAYLOAD,
            format!("payload {len} exceeds cap {}", limits::MAX_PAYLOAD_BYTES),
        ));
    }
    Ok(())
}

fn rect((x, y, w, h): (u32, u32, u32, u32)) -> Rect {
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn u16_le_bytes(samples: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Builds the `RawColorimetry` calibration block from LibRaw metadata.
fn colorimetry(m: &Meta) -> RawColorimetry {
    RawColorimetry {
        as_shot_neutral: as_shot_neutral(m.cam_mul),
        illuminant1: Illuminant::D65,
        illuminant2: None,
        color_matrix1: color_matrix(m.cam_xyz),
        color_matrix2: None,
        forward_matrix1: None,
        forward_matrix2: None,
        analog_balance: None,
        baseline_exposure: 0.0,
    }
}

/// Normalized as-shot neutral from `cam_mul` (green = reference). `None` when the
/// multipliers are missing/degenerate.
fn as_shot_neutral(cam_mul: [f32; 4]) -> Option<[f64; 3]> {
    let g = if cam_mul[1] > 0.0 {
        cam_mul[1] as f64
    } else if cam_mul[3] > 0.0 {
        cam_mul[3] as f64
    } else {
        return None;
    };
    let r = cam_mul[0] as f64;
    let b = cam_mul[2] as f64;
    if r <= 0.0 || b <= 0.0 {
        return None;
    }
    // neutral[c] = green_multiplier / channel_multiplier (green normalized to 1).
    Some([g / r, 1.0, g / b])
}

fn color_matrix(cam_xyz: [[f32; 3]; 3]) -> Mat3Array {
    let any_nonzero = cam_xyz.iter().flatten().any(|&v| v != 0.0);
    if !any_nonzero {
        return IDENTITY3;
    }
    let mut out = IDENTITY3;
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = cam_xyz[r][c] as f64;
        }
    }
    out
}

/// LibRaw color index → [`CfaColor`] via the color descriptor (`"RGBG"`).
fn color_from_index(cdesc: &str, idx: i8) -> CfaColor {
    let bytes = cdesc.as_bytes();
    let ch = if idx >= 0 && (idx as usize) < bytes.len() {
        bytes[idx as usize]
    } else {
        b'G'
    };
    match ch {
        b'R' | b'r' => CfaColor::R,
        b'B' | b'b' => CfaColor::B,
        _ => CfaColor::G,
    }
}

fn cfa_pattern(m: &Meta) -> CfaPattern {
    if m.colors <= 1 || m.filters == 0 {
        return CfaPattern::Mono;
    }
    if m.is_xtrans {
        let mut grid = [[CfaColor::G; 6]; 6];
        for (r, row) in grid.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = color_from_index(&m.cdesc, m.xtrans[r * 6 + c]);
            }
        }
        return CfaPattern::XTrans(Box::new(grid));
    }
    CfaPattern::Bayer([
        color_from_index(&m.cdesc, m.cfa2x2[0]),
        color_from_index(&m.cdesc, m.cfa2x2[1]),
        color_from_index(&m.cdesc, m.cfa2x2[2]),
        color_from_index(&m.cdesc, m.cfa2x2[3]),
    ])
}

/// LibRaw flip code → EXIF orientation value (1..=8).
fn flip_to_exif(flip: i32) -> u16 {
    match flip {
        0 => 1,
        3 => 3,
        5 => 8, // 90° CCW
        6 => 6, // 90° CW
        _ => 1,
    }
}
