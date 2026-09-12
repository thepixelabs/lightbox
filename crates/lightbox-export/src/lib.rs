// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-export`, export/output pipeline. **E15 core slice.**
//!
//! This crate implements the *core export slice* of the E15 spec
//! (`docs/plan/epics/E15-export-output.md`): open → edit → export a
//! finished JPEG/TIFF/PNG, single image or a small batch. It is
//! deliberately **not** the full spec, every cut is named here and in
//! `docs/plan/epics/E15-deviations.md`:
//!
//! **Deferred (named, not built):**
//! - **Graphical (image) watermarks** (spec §5.11's `Watermark::Image`).
//!   Text watermarks are built ([`watermark`]); a PNG watermark would need
//!   an image decoder in this crate, which is a dependency this crate does
//!   not carry.
//! - External-editor round-trip (spec §5.9), no `edit_in` module.
//! - The shared filename-token engine (spec §5.5 `lightbox-templates`)
//!   [`settings::NamingSpec`] is a plain stem+suffix instead.
//! - C2PA content credentials (spec §10 risk list item, Could-tier anyway).
//! - **IPTC IIM** (the legacy `8BIM`/APP13 block) is never written, in any
//!   format, at any [`settings::MetadataLevel`]; the IPTC-defined fields
//!   Lightbox does write are XMP properties. Keywords, ratings and labels
//!   are not carried into an export at all. See [`metadata`].
//! - JPEG XL / AVIF / DNG / Original-passthrough encoders (spec §5.4)
//!   JPEG/PNG/TIFF only.
//! - The `Exporter`/`ExportPlan`/`ExportSession` planner + catalog tables
//!   (spec §5.2/§6), collision policy, presets, multi-preset batch,
//!   Export-with-Previous, re-add-to-catalog. Callers resolve output paths
//!   themselves; [`run::ExportItem`] takes an explicit, already-resolved
//!   path per image.
//!
//! **What's here:** [`settings`] (format/quality/depth/resize/color-space/
//! sharpen/naming/metadata policy/watermark), [`pixel`] (render → resize →
//! output-color-transform → sharpen → quantize), [`watermark`] (a text
//! mark burnt into the pixels, drawn with a built-in stroke font),
//! [`metadata`] (the EXIF and XMP blocks a
//! [`settings::MetadataLevel`] allows out), [`encode`] (JPEG/PNG/TIFF),
//! and [`run`] (the per-image pipeline + a bounded-concurrency batch
//! driver). `lightbox-core` wires this behind
//! `Command::Export`/`Event::Export*`; `lightbox-cli` and `lightbox-shell`
//! are the two callers.
//!
//! **Metadata privacy, stated once.** An export re-encodes from pixels and
//! copies no metadata from the source container. Everything in an exported
//! file's EXIF and XMP was put there by [`metadata::build`] under the
//! chosen [`settings::MetadataLevel`], whose default is the *safe* end
//! ([`settings::MetadataLevel::CopyrightOnly`]), not the maximal one. GPS
//! is written only at [`settings::MetadataLevel::All`]; see
//! `tests/metadata_privacy.rs` for the read-back proof in all three
//! containers.

pub mod encode;
pub mod error;
pub mod metadata;
pub mod pixel;
pub mod run;
pub mod settings;
pub mod watermark;

pub use error::ExportError;
pub use metadata::MetadataBlocks;
pub use run::{export_batch, export_one, ExportItem, ExportItemDone, ExportReport};
pub use settings::{
    ExportSettings, GpsPosition, MetadataLevel, MetadataPolicy, RightsInfo, SourceMetadata,
    Watermark, WatermarkAnchor,
};
