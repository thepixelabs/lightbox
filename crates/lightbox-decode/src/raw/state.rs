// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `DecodedRawState` — the E03 raw-cache payload contract (spec §4.2).
//!
//! Binary format v1:
//!
//! ```text
//! [u32 magic 'LBRS'][u16 version=1][u16 flags]
//! [CBOR header: dims, layout (mosaic|rgb), cfa, colorimetry, provenance
//!   { backend, libraw_version?, interim_demosaic, decode_params_hash }]
//! [zstd frame: pixel plane(s), row-major]
//! ```
//!
//! **Phase A ships the format constants + the `decode_params_hash` helper**
//! (the cache-key params component E03 stores under `content_hash`). The full
//! (de)serialization body is Phase C (task C6); it fills
//! [`serialize`]/[`deserialize`] here without touching the header shape.

use std::hash::Hasher;

use twox_hash::XxHash3_64;

use crate::raw::types::{BackendPolicy, DecodeBackend};

/// Magic prefix `'LBRS'` (spec §4.2).
pub const MAGIC: u32 = u32::from_be_bytes(*b"LBRS");
/// Format version.
pub const VERSION: u16 = 1;

/// Bumped whenever the [`crate::linearize`] kernel changes in a way that alters
/// output bytes — a component of [`decode_params_hash`] so a kernel change is a
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
#[derive(Clone, Debug)]
pub struct DecodedRawStateHeader {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// True = mosaic plane; false = interleaved RGB.
    pub mosaic: bool,
    /// Backend that produced the state.
    pub backend: DecodeBackend,
    /// LibRaw version, when the proxy produced it.
    pub libraw_version: Option<String>,
    /// Interim-demosaic segregation bit (spec §4.2).
    pub interim_demosaic: bool,
    /// Cache-key params component.
    pub decode_params_hash: u64,
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
}
