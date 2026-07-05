// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! LCMS2 wrapper (spec §3.5). **Owner: Phase D (D1/D7).** MIT core only — the
//! `fast_float` plugin (GPL-3) is FORBIDDEN and CI-asserted absent (R2, D1).
//! Contexts are thread-safe, the error callback is captured, and
//! [`IccProfile::from_bytes`] treats its input as untrusted (size-capped,
//! fuzzed at D7).

pub use crate::error::IccError;

/// A parsed ICC profile (size-capped, error-callback captured) (spec §3.5).
/// **Phase D** fills the LCMS2-backed body; the field is private so the
/// representation stays D's to choose.
pub struct IccProfile {
    // Phase D: the `lcms2::Profile` handle + captured error state. Kept private
    // and unit-typed for now so the scaffold compiles without the FFI wiring.
    _private: (),
}

impl IccProfile {
    /// Parses ICC bytes with untrusted-input hygiene (spec §3.5). **Phase D
    /// (D1/D7)** — size cap, LCMS2 error capture, no abort on malformed input.
    pub fn from_bytes(_bytes: &[u8]) -> Result<IccProfile, IccError> {
        unimplemented!("D1: LCMS2 profile parse with untrusted-input hygiene")
    }

    /// The built-in sRGB profile.
    pub fn srgb() -> IccProfile {
        unimplemented!("D1: built-in sRGB profile")
    }

    /// The built-in Display P3 profile.
    pub fn display_p3() -> IccProfile {
        unimplemented!("D1: built-in Display P3 profile")
    }
}
