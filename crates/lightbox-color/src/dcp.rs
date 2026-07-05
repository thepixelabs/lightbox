// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! DCP parser + evaluator (spec §3.1 `parse_dcp` / §5.2 stage order).
//! **Owner: Phase F (F1–F6).**
//!
//! **Deviation from §3.1 (recorded in E02-deviations.md):** `parse_dcp` lives
//! here in `lightbox-color`, not in `lightbox-decode`. Its output
//! [`CameraProfile`](crate::profile::CameraProfile) is a `lightbox-color` type;
//! putting the parser in `lightbox-decode` would make decode depend on color
//! and invert the crate dependency. `.dcp` is untrusted user input — the parser
//! is fuzzed and capped (F3) regardless of which crate hosts it.

pub use crate::error::DcpParseError;
use crate::profile::CameraProfile;

/// Parses a `.dcp` container (untrusted input — fuzzed, capped, panic-free)
/// into a [`CameraProfile`] (spec §3.1). **Phase F (F1/F2)** — own TIFF-IFD
/// reader over the DNG camera-profile tags.
pub fn parse_dcp(_bytes: &[u8]) -> Result<CameraProfile, DcpParseError> {
    unimplemented!("F1/F2: TIFF-IFD .dcp parser over the DNG camera-profile tags")
}
