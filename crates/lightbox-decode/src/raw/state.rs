// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `DecodedRawState`, the E03 raw-cache payload contract (spec §4.2).
//!
//! Binary format v1:
//!
//! ```text
//! [u32 magic 'LBRS' big-endian][u16 version=1 LE][u16 flags LE]
//! [u32 header_len LE][CBOR header: dims, layout (mosaic|rgb), cfa, colorimetry,
//!   provenance { backend, libraw_version?, interim_demosaic, decode_params_hash }]
//! [zstd frame: pixel plane(s), row-major, little-endian samples]
//! ```
//!
//! **Phase C** fills the (de)serialization body ([`DecodedRawState::serialize`]
//! / [`DecodedRawState::deserialize`], task C6) on top of Phase A's format
//! constants + [`decode_params_hash`]. The `interim_demosaic` bit segregates M1
//! LibRaw-AHD states from post-E11 mosaic states so the M2 demosaic swap is a
//! clean cache miss (spec §4.2), never a stale hit.
//!
//! **Deviation from §4.2 (recorded in E02-deviations.md):** the format adds a
//! `u32 header_len` between the fixed prefix and the CBOR header so the reader
//! can find the zstd frame boundary without a streaming CBOR probe. The rest of
//! the layout matches §4.2 exactly.

use std::hash::Hasher;

use serde::{Deserialize, Serialize};
use twox_hash::XxHash3_64;

use crate::raw::types::{BackendPolicy, CfaPattern, DecodeBackend, RawColorimetry};

/// Magic prefix `'LBRS'` (spec §4.2).
pub const MAGIC: u32 = u32::from_be_bytes(*b"LBRS");
/// Format version.
pub const VERSION: u16 = 1;

/// `flags` bit 0: set when the pixel plane is a CFA mosaic (`u16` samples);
/// clear when it is interleaved RGB(A) (`f32` samples).
const FLAG_MOSAIC: u16 = 1 << 0;

/// Bumped whenever the [`crate::linearize`] kernel changes in a way that alters
/// output bytes, a component of [`decode_params_hash`] so a kernel change is a
/// clean cache miss (spec §4.2), never a stale hit.
pub const LINEARIZE_IMPL_VERSION: u32 = 1;

/// `decode_params_hash` (spec §4.2): `xxh3(backend policy, interim_demosaic,
/// proxy libraw version, linearize impl version)`. Stable across platforms
/// (xxh3 is endian-independent for byte inputs). The `interim_demosaic` bit is
/// folded in so M1 LibRaw-AHD states never alias post-E11 mosaic states.
pub fn decode_params_hash(
    backend: BackendPolicy,
    interim_demosaic: bool,
    libraw_version: Option<&str>,
) -> u64 {
    let mut h = XxHash3_64::new();
    let backend_tag: u8 = match backend {
        BackendPolicy::Auto => 0,
        BackendPolicy::ForceProxy => 1,
        BackendPolicy::ForceInCrate => 2,
    };
    h.write(&[backend_tag, u8::from(interim_demosaic)]);
    h.write(&LINEARIZE_IMPL_VERSION.to_le_bytes());
    h.write(libraw_version.unwrap_or("").as_bytes());
    // Length-delimit the version string so ("", "x") and ("x", "") differ.
    h.write(&[0xff]);
    h.finish()
}

/// The decoded raw-state header (spec §4.2). Pixel planes live in the trailing
/// zstd frame, not in this struct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecodedRawStateHeader {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// True = mosaic plane (`u16`); false = interleaved RGB(A) (`f32`).
    pub mosaic: bool,
    /// Channels per pixel for the RGB layout (3 or 4); `1` for a mosaic plane.
    pub channels: u8,
    /// CFA layout, when this is a mosaic state.
    pub cfa: Option<CfaPattern>,
    /// Camera calibration, carried through for the color pipeline.
    pub colorimetry: RawColorimetry,
    /// Backend that produced the state.
    pub backend: DecodeBackend,
    /// LibRaw version, when the proxy produced it.
    pub libraw_version: Option<String>,
    /// Interim-demosaic segregation bit (spec §4.2).
    pub interim_demosaic: bool,
    /// Cache-key params component.
    pub decode_params_hash: u64,
}

/// A serializable decoded raw state: the header plus its raw pixel bytes
/// (little-endian samples, row-major). The E03 cache stores
/// [`serialize`](Self::serialize)'s output under `content_hash` + params.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedRawState {
    /// The state header.
    pub header: DecodedRawStateHeader,
    /// Raw little-endian sample bytes: `u16` for a mosaic, `f32` for RGB.
    pub pixels: Vec<u8>,
}

/// Errors from [`DecodedRawState::deserialize`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// The buffer is shorter than the fixed header.
    #[error("truncated raw-state buffer")]
    Truncated,
    /// The magic prefix is not `'LBRS'`.
    #[error("bad magic (not a DecodedRawState)")]
    BadMagic,
    /// The version byte is one this build does not understand.
    #[error("unsupported raw-state version {0}")]
    BadVersion(u16),
    /// The CBOR header failed to parse.
    #[error("malformed raw-state header: {0}")]
    BadHeader(String),
    /// The zstd pixel frame failed to decompress.
    #[error("malformed raw-state pixel frame: {0}")]
    BadPixels(String),
}

/// zstd level for the pixel frame. Level 3 is the speed/ratio knee the §7.5
/// budget ("serialize 45 MP ≤ 250 ms, zstd level tuned") targets.
const ZSTD_LEVEL: i32 = 3;

impl DecodedRawState {
    /// Serializes to the v1 byte format (spec §4.2). Never fails for a
    /// well-formed header (CBOR of plain data + zstd of a byte slice).
    pub fn serialize(&self) -> Vec<u8> {
        let mut header_cbor = Vec::new();
        // Plain-data header; serialization cannot realistically fail.
        ciborium::into_writer(&self.header, &mut header_cbor)
            .expect("DecodedRawStateHeader is plain data and always serializes");

        let compressed =
            zstd::encode_all(self.pixels.as_slice(), ZSTD_LEVEL).expect("zstd encode of a slice");

        let flags = if self.header.mosaic { FLAG_MOSAIC } else { 0 };
        let mut out = Vec::with_capacity(12 + header_cbor.len() + compressed.len());
        out.extend_from_slice(&MAGIC.to_be_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&(header_cbor.len() as u32).to_le_bytes());
        out.extend_from_slice(&header_cbor);
        out.extend_from_slice(&compressed);
        out
    }

    /// Parses a v1 buffer. Malformed input is a [`StateError`], never a panic.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, StateError> {
        // Fixed prefix: magic(4) + version(2) + flags(2) + header_len(4) = 12.
        if bytes.len() < 12 {
            return Err(StateError::Truncated);
        }
        let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != MAGIC {
            return Err(StateError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(StateError::BadVersion(version));
        }
        // flags[6..8] mirror header.mosaic; the CBOR header is authoritative.
        let header_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let header_start: usize = 12;
        let header_end = header_start
            .checked_add(header_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(StateError::Truncated)?;
        let header: DecodedRawStateHeader = ciborium::from_reader(&bytes[header_start..header_end])
            .map_err(|e| StateError::BadHeader(e.to_string()))?;
        let pixels = zstd::decode_all(&bytes[header_end..])
            .map_err(|e| StateError::BadPixels(e.to_string()))?;
        Ok(DecodedRawState { header, pixels })
    }
}

/// Reinterprets a `u16` sample slice as little-endian bytes (mosaic plane).
pub fn u16_samples_to_le_bytes(samples: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Reads a little-endian `u16` sample plane back from bytes.
pub fn le_bytes_to_u16_samples(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Reinterprets an `f32` sample slice as little-endian bytes (RGB plane).
pub fn f32_samples_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Reads a little-endian `f32` sample plane back from bytes.
pub fn le_bytes_to_f32_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_lbrs() {
        assert_eq!(MAGIC.to_be_bytes(), *b"LBRS");
    }

    #[test]
    fn params_hash_is_deterministic_and_sensitive() {
        let a = decode_params_hash(BackendPolicy::Auto, false, Some("0.21.2"));
        let b = decode_params_hash(BackendPolicy::Auto, false, Some("0.21.2"));
        assert_eq!(a, b, "same inputs -> same hash");

        // Every input dimension changes the hash.
        assert_ne!(
            a,
            decode_params_hash(BackendPolicy::ForceProxy, false, Some("0.21.2"))
        );
        assert_ne!(
            a,
            decode_params_hash(BackendPolicy::Auto, true, Some("0.21.2"))
        );
        assert_ne!(
            a,
            decode_params_hash(BackendPolicy::Auto, false, Some("0.22.0"))
        );
        assert_ne!(a, decode_params_hash(BackendPolicy::Auto, false, None));

        // The interim bit is the M1↔M2 segregator (spec §4.2).
        let m1 = decode_params_hash(BackendPolicy::Auto, true, Some("0.21.2"));
        let m2 = decode_params_hash(BackendPolicy::Auto, false, Some("0.21.2"));
        assert_ne!(m1, m2);
    }

    fn sample_header(mosaic: bool) -> DecodedRawStateHeader {
        DecodedRawStateHeader {
            width: 4,
            height: 2,
            mosaic,
            channels: if mosaic { 1 } else { 3 },
            cfa: mosaic.then_some(CfaPattern::Bayer([
                crate::raw::types::CfaColor::R,
                crate::raw::types::CfaColor::G,
                crate::raw::types::CfaColor::G,
                crate::raw::types::CfaColor::B,
            ])),
            colorimetry: RawColorimetry::default(),
            backend: if mosaic {
                DecodeBackend::LibrawProxy
            } else {
                DecodeBackend::InCrate
            },
            libraw_version: mosaic.then_some("0.22.1".to_owned()),
            interim_demosaic: !mosaic,
            decode_params_hash: 0xdead_beef,
        }
    }

    #[test]
    fn mosaic_state_round_trips_identically() {
        let samples: Vec<u16> = (0..8u16).map(|i| i * 4096).collect();
        let state = DecodedRawState {
            header: sample_header(true),
            pixels: u16_samples_to_le_bytes(&samples),
        };
        let bytes = state.serialize();
        // Fixed prefix sanity.
        assert_eq!(&bytes[0..4], b"LBRS");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), VERSION);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), FLAG_MOSAIC);

        let back = DecodedRawState::deserialize(&bytes).unwrap();
        assert_eq!(back, state);
        assert_eq!(le_bytes_to_u16_samples(&back.pixels), samples);
    }

    #[test]
    fn rgb_state_round_trips_identically() {
        let samples: Vec<f32> = vec![0.0, 0.25, 0.5, 0.75, 1.0, 0.1, 0.2, 0.3, 0.9, 0.8, 0.7, 0.6];
        let mut header = sample_header(false);
        header.channels = 3;
        let state = DecodedRawState {
            header,
            pixels: f32_samples_to_le_bytes(&samples),
        };
        let bytes = state.serialize();
        assert_eq!(
            u16::from_le_bytes([bytes[6], bytes[7]]),
            0,
            "rgb flag clear"
        );
        let back = DecodedRawState::deserialize(&bytes).unwrap();
        assert_eq!(back, state);
        assert_eq!(le_bytes_to_f32_samples(&back.pixels), samples);
    }

    #[test]
    fn bad_inputs_are_structured_errors_not_panics() {
        assert!(matches!(
            DecodedRawState::deserialize(&[]),
            Err(StateError::Truncated)
        ));
        assert!(matches!(
            DecodedRawState::deserialize(&[0u8; 32]),
            Err(StateError::BadMagic)
        ));
        // Correct magic, wrong version.
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_be_bytes());
        b.extend_from_slice(&99u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            DecodedRawState::deserialize(&b),
            Err(StateError::BadVersion(99))
        ));
        // Correct prefix, header_len points past the buffer.
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_be_bytes());
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&9999u32.to_le_bytes());
        assert!(matches!(
            DecodedRawState::deserialize(&b),
            Err(StateError::Truncated)
        ));
    }

    #[test]
    fn header_carries_the_e03_provenance_fields() {
        let state = DecodedRawState {
            header: sample_header(true),
            pixels: u16_samples_to_le_bytes(&[1, 2, 3, 4, 5, 6, 7, 8]),
        };
        let back = DecodedRawState::deserialize(&state.serialize()).unwrap();
        assert_eq!(back.header.backend, DecodeBackend::LibrawProxy);
        assert_eq!(back.header.libraw_version.as_deref(), Some("0.22.1"));
        assert!(!back.header.interim_demosaic);
        assert_eq!(back.header.decode_params_hash, 0xdead_beef);
    }
}
