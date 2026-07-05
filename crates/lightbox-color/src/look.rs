// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The Lightbox default look (§1.7 tier 2, spec §3.4). **Owner: Phase E
//! (E1–E2).** `.lblook` is versioned CBOR: tone curve + optional hue/sat
//! shaping + provenance; loader/validator/evaluator with 0–200 % amount
//! semantics.

pub use crate::error::LookError;
use crate::lut::HueSatLut;
use crate::matrix::Spline1D;
use crate::profile::ProfileId;

/// Provenance block for a look: author, license, review record — manifest
/// checked (spec §3.4/§4.3; **no Adobe-derived data at any authoring step**).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LookProvenance {
    /// Authoring entity (e.g. `"lightbox-authored"`).
    pub author: String,
    /// License (e.g. `"project"`).
    pub license: String,
    /// Perceptual-review record id (E5), when present.
    pub review_record: Option<String>,
}

/// A Lightbox look loaded from a versioned `.lblook` (spec §3.4).
#[derive(Clone, Debug)]
pub struct Look {
    /// Content id.
    pub id: ProfileId,
    /// Display name.
    pub name: String,
    /// `.lblook` format version.
    pub version: u16,
    /// Scene-referred tone curve (identity for "Lightbox Neutral").
    pub tone_curve: Spline1D,
    /// Optional hue/sat shaping.
    pub hue_sat: Option<HueSatLut>,
    /// Provenance.
    pub provenance: LookProvenance,
}

/// Loads + validates a `.lblook` (spec §3.4). **Phase E (E1)** — round-trip
/// stable; unknown version → [`LookError::UnsupportedVersion`].
pub fn load_look(_bytes: &[u8]) -> Result<Look, LookError> {
    unimplemented!("E1: .lblook CBOR loader + validator")
}
