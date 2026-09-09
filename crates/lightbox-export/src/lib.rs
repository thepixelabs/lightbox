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
//! - Watermarking (spec §5.11), no `watermark` module.
//! - External-editor round-trip (spec §5.9), no `edit_in` module.
//! - The shared filename-token engine (spec §5.5 `lightbox-templates`)
//!   [`settings::NamingSpec`] is a plain stem+suffix instead.
//! - C2PA content credentials (spec §10 risk list item, Could-tier anyway).
//! - Full metadata-policy engine (spec §5.6 `lightbox-meta::export`), no
//!   EXIF/IPTC/XMP embedding, no privacy tiers, no GPS/keyword stripping.
//! - JPEG XL / AVIF / DNG / Original-passthrough encoders (spec §5.4)
//!   JPEG/PNG/TIFF only.
//! - The `Exporter`/`ExportPlan`/`ExportSession` planner + catalog tables
//!   (spec §5.2/§6), collision policy, presets, multi-preset batch,
//!   Export-with-Previous, re-add-to-catalog. Callers resolve output paths
//!   themselves; [`run::ExportItem`] takes an explicit, already-resolved
//!   path per image.
//!
//! **What's here:** [`settings`] (format/quality/depth/resize/color-space/
//! sharpen/naming), [`pixel`] (render → resize → output-color-transform →
//! sharpen → quantize), [`encode`] (JPEG/PNG/TIFF), and [`run`] (the
//! per-image pipeline + a bounded-concurrency batch driver). `lightbox-core`
//! wires this behind `Command::Export`/`Event::Export*`; `lightbox-cli`
//! and `lightbox-shell` are the two callers.

pub mod encode;
pub mod error;
pub mod pixel;
pub mod run;
pub mod settings;

pub use error::ExportError;
pub use run::{export_batch, export_one, ExportItem, ExportItemDone, ExportReport};
pub use settings::ExportSettings;
