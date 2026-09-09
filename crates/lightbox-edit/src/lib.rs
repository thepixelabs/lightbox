// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-edit`, the versioned edit recipe, param-delta engine, history,
//! snapshots, presets, and settings transfer (spec §3.1.1 / §3.2). **The primary
//! data model** of the editing-first product: everything between "a slider moved"
//! and "a recipe is durably persisted, historized, presetable, and projectable
//! to/from XMP", with **no pixels** (spec §1.1).
//!
//! # Phase A surface (this commit)
//!
//! - [`params`], [`ParamId`] stable `u16` wire ids, [`ParamValue`],
//!   [`ParamGroup`]/[`group_of`], [`ParamSubset`], [`ParamDelta`].
//! - [`recipe`], the full architecture §3.2 [`Recipe`] behind the **E01-frozen
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
//!   source for XMP divergence, deterministic across the 3-OS matrix.
//!
//! [`ParamId`]: crate::params::ParamId
//! [`ParamValue`]: crate::params::ParamValue
//! [`ParamGroup`]: crate::params::ParamGroup
//! [`group_of`]: crate::params::group_of
//! [`ParamSubset`]: crate::params::ParamSubset
//! [`ParamDelta`]: crate::params::ParamDelta

pub mod cbor;
pub mod history;
pub mod leaves;
pub mod params;
pub mod preset;
// The starter preset library (preset-management feature): ~20 original,
// provenance-clean develop-recipe presets, authored against E09's param
// vocabulary. `crates/lightbox-edit/tests/preset_library.rs` blesses the
// committed `assets/presets/**/*.xmp` files from this module.
pub mod preset_library;
// Retiring a starter preset that has left the bundled library, without ever
// touching one the user owns (`PresetStore::seed_bundled`). Seeding used to
// only ever ADD, so a re-authored library left every existing user holding the
// dead families forever.
pub mod preset_retire;
pub mod ranges;
pub mod recipe;
// The edit store (spec §3.1.1/§3.3, Phase B, T6/T7): `EditStore`,
// `EditSession`/`PendingCommit` (the pure gesture lifecycle + commit DAO
// txn), and `StoreError`.
pub mod store;
// Snapshots (spec §3.1/§3.3, Phase B, T10): `SnapshotMeta`, the
// `SnapshotMaterializer` E12 seam, and `EditStore`'s snapshot CRUD +
// `restore_snapshot`.
pub mod snapshot;
pub mod transfer;
pub mod xmp_map;

// ---- Frozen surface: `lightbox_edit::Recipe` resolves at the crate root, as
// `lightbox-render`/`lightbox-core`/`lightbox-cli`/`lightbox-shell` already import it.
pub use recipe::{Applied, Geometry, GlobalStages, Recipe, RecipeError, RecipeRead, RECIPE_SCHEMA};

pub use params::{group_of, ParamDelta, ParamGroup, ParamId, ParamSubset, ParamValue};

// Scalar param range table (spec A2): the single source of truth for E10's
// slider bounds + validation/fuzzing, consumed via `GlobalStages::clamped`.
pub use ranges::{range_of, ParamRange};

// History vocabulary + engine (spec §3.3). Phase E shipped `StepLabel` (the
// label every durable edit txn carries); Phase B (T9) adds the reconstruction
// engine (`history::list`/`recipe_at`/`clear`/`step_to`/`undo`/`redo`,
// accessed via the `history` module path) and `HistoryStepMeta`.
pub use history::{HistoryStepMeta, StepLabel};

// The edit store (spec §3.3, Phase B, T6/T7): session registry + durable
// commit surface for `lightbox-core`'s `EditHub` (T8, follow-up) to build on.
pub use store::{EditSession, EditState, EditStore, PendingCommit, StoreError};

// Snapshots (spec §3.3, Phase B, T10).
pub use snapshot::{IdentityMaterializer, SnapshotMaterializer, SnapshotMeta};

// Presets (spec §3.7 / Phase E): the file-backed store, the preset model, and the
// pure hover `preview_recipe`.
pub use preset::{
    preview_recipe, DevelopPreset, PresetError, PresetId, PresetImportResult, PresetLoadError,
    PresetMeta, PresetOrigin, PresetStore,
};

// Starter-library retirement (`PresetStore::seed_bundled`): what the app's own
// seed removed, and why.
pub use preset_retire::{BundledSeedReport, MarkerSweepSkipped, RetireReason, RetiredPreset};

// Settings transfer (spec §3.8 / Phase E): copy/paste buffer + batched sync engine
// (against the Phase-B `EditSink` seam).
pub use transfer::{
    apply_previous, paste_settings, reset_edits, sync_to, CancelFlag, CancelSignal, CommitStep,
    CopiedSettings, EditSink, NeverCancel, SyncOptions, SyncProgress, SyncReport, TransferError,
};

// The `crs:`/`lb:` mapping layer (spec §3.6 / Phase D): `Recipe::to_xmp`,
// `Recipe::read_xmp`, `Recipe::from_lr_crs`, and the `CrsImportReport` seam.
pub use xmp_map::{
    CrsImport, CrsImportReport, FieldFidelity, RecipeFromXmp, XmpMapError, XmpSource,
    XmpWriteCoalescer, XmpWriteCtx,
};

// Leaf value types (architecture §3.2), the recipe's building blocks.
pub use leaves::{
    BwMix, CborValue, ColorGrade, CreativeLut, Crop, CurvePoint, Detail, Effects, Flip, GradeWheel,
    Grain, HslBand, HslTable, LensCorrection, NoiseReduction, Optics, ParametricCurve,
    PostCropVignette, Presence, ProfileKind, ProfileRef, Sharpen, ToneCurve, ToneCurveSet,
    Transform, Treatment, Upright, WbPreset, WhiteBalance, XmpPassthrough, MAX_CURVE_POINTS,
    MIN_SPLIT_GAP,
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
