// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-edit` — the versioned edit recipe, param-delta engine, history,
//! snapshots, presets, and settings transfer (spec §3.1.1 / §3.2). **The primary
//! data model** of the editing-first product: everything between "a slider moved"
//! and "a recipe is durably persisted, historized, presetable, and projectable
//! to/from XMP" — with **no pixels** (spec §1.1).
//!
//! # Phase A surface (this commit)
//!
//! - [`params`] — [`ParamId`] stable `u16` wire ids, [`ParamValue`],
//!   [`ParamGroup`]/[`group_of`], [`ParamSubset`], [`ParamDelta`].
//! - [`recipe`] — the full architecture §3.2 [`Recipe`] behind the **E01-frozen
//!   `{ schema, pv }` surface** and [`Recipe::identity`], plus neutral defaults,
//!   validation/clamping, and the delta engine (`get`/`apply`/`diff`/`extract`).
//! - CBOR serialization ([`Recipe::to_cbor`]/[`Recipe::from_cbor`]) and
//!   [`Recipe::canonical_hash`] (xxh3-128, big-endian).
//!
//! # Ownership rules (restated for callers)
//!
//! - **Single write path (E01 rule):** recipe persistence is one WAL txn through
//!   the catalog's single writer (Phase B); no SQL crosses this crate's boundary.
//! - **ids-only mask/retouch refs:** [`Recipe::masks`]/[`Recipe::retouch`] are
//!   ordered `MaskId`/`RetouchOpId` lists only; content lives in E12/E14 tables.
//! - **CBOR is the authoritative recipe format.** [`Recipe::to_cbor`]/
//!   [`Recipe::from_cbor`] preserve a newer build's unknown top-level keys
//!   byte-for-byte; the derived `serde` impls on [`Recipe`] are for generic
//!   in-memory use and MUST NOT be used for persistence (deviations A-3).
//! - **`canonical_hash` is a cache-key component for E03/E05** and the stamp
//!   source for XMP divergence — deterministic across the 3-OS matrix.
//!
//! [`ParamId`]: crate::params::ParamId
//! [`ParamValue`]: crate::params::ParamValue
//! [`ParamGroup`]: crate::params::ParamGroup
//! [`group_of`]: crate::params::group_of
//! [`ParamSubset`]: crate::params::ParamSubset
//! [`ParamDelta`]: crate::params::ParamDelta

pub mod cbor;
pub mod leaves;
pub mod params;
pub mod recipe;

// ---- Frozen surface: `lightbox_edit::Recipe` resolves at the crate root, as
// `lightbox-render`/`lightbox-core`/`lightbox-cli`/`lightbox-shell` already import it.
pub use recipe::{Applied, Geometry, GlobalStages, Recipe, RecipeError, RecipeRead, RECIPE_SCHEMA};

pub use params::{group_of, ParamDelta, ParamGroup, ParamId, ParamSubset, ParamValue};

// Leaf value types (architecture §3.2) — the recipe's building blocks.
pub use leaves::{
    BwMix, CborValue, ColorGrade, CreativeLut, Crop, CurvePoint, Detail, Effects, Flip, GradeWheel,
    Grain, HslBand, HslTable, LensCorrection, NoiseReduction, Optics, PostCropVignette, Presence,
    ProfileKind, ProfileRef, Sharpen, ToneCurve, ToneCurveSet, Transform, Treatment, Upright,
    WbPreset, WhiteBalance, XmpPassthrough, MAX_CURVE_POINTS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_types::PV_M0;

    /// The E01-frozen shape survives the grown body.
    #[test]
    fn identity_recipe_shape() {
        let r = Recipe::identity(PV_M0);
        assert_eq!(r.schema, RECIPE_SCHEMA);
        assert_eq!(r.pv, PV_M0);
        assert!(r.is_neutral());
    }
}
