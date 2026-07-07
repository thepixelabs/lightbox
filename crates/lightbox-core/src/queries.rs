// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Queries`] — the read side of seam 1 (spec §3.8).
//!
//! Wraps a checked-out WAL-snapshot [`ReaderHandle`]; the method set mirrors
//! §3.2's `ReaderHandle`, returning the reader DTOs re-exported from
//! `lightbox-core` — **no SQL crosses up** (spec §2.3 seam 1).
//!
//! Threading contract (spec §5.1): queries execute on the calling thread
//! against a pooled read connection — fast and safe from the UI thread at M0
//! grid scale (OQ-5 tracks the prefetch upgrade path). Drop the [`Queries`]
//! value promptly: it holds one of the `min(cores, 4)` pool connections.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_catalog::{
    CatalogCounts, EditBadge, FolderNode, ImageDetail, ImageQuery, ImageSummary, Page, ReaderHandle,
};
use lightbox_edit::{EditState, HistoryStepMeta, PresetId, PresetMeta, Recipe, SnapshotMeta};
use lightbox_meta::xmp::sync::DivergenceStatus;
use lightbox_preview::{CacheStats, PreviewDesc, PreviewService};
use lightbox_types::{AssetId, ImageId};

use crate::edit_hub::EditHub;
use crate::error::Result;

/// A borrowed snapshot view of the catalog (spec §3.8), plus the E09 edit
/// read surface (spec §3.4 — additive methods, hydrated via `lightbox-edit`
/// from raw catalog DTOs; no SQL crosses up) and the E03 Phase D preview
/// read surface (spec §5.6 `Query::CacheStats`/`PreviewState`).
pub struct Queries {
    reader: ReaderHandle,
    edits: Arc<EditHub>,
    previews: PreviewService,
}

impl Queries {
    pub(crate) fn new(
        reader: ReaderHandle,
        edits: Arc<EditHub>,
        previews: PreviewService,
    ) -> Queries {
        Queries {
            reader,
            edits,
            previews,
        }
    }

    /// One page of images by keyset cursor (never OFFSET — spec §3.2).
    pub fn images_page(&self, q: &ImageQuery) -> Result<Page<ImageSummary>> {
        Ok(self.reader.images_page(q)?)
    }

    /// Full detail for one image.
    pub fn image_detail(&self, id: ImageId) -> Result<ImageDetail> {
        Ok(self.reader.image_detail(id)?)
    }

    /// The whole folder tree, flat, sorted by `(root, rel_path)`.
    pub fn folder_tree(&self) -> Result<Vec<FolderNode>> {
        Ok(self.reader.folder_tree()?)
    }

    /// Absolute path of an asset (`root.path ⊕ folder.rel_path ⊕ filename`).
    pub fn asset_abs_path(&self, id: AssetId) -> Result<PathBuf> {
        Ok(self.reader.asset_abs_path(id)?)
    }

    /// Row counts across the spine tables.
    pub fn counts(&self) -> Result<CatalogCounts> {
        Ok(self.reader.counts()?)
    }

    /// FTS5 prefix search over filenames + camera, best match first.
    pub fn search_filenames(&self, query: &str, limit: u32) -> Result<Vec<ImageId>> {
        Ok(self.reader.search_filenames(query, limit)?)
    }

    // ── E09 (spec §3.4) — additive read surface ─────────────────────────────

    /// The durable-or-neutral edit state for `image` (spec §3.4
    /// `Queries::edit_state`): `EditState::persisted` distinguishes "restored
    /// from a row" from "untouched, neutral default" (D1).
    pub fn edit_state(&self, image: ImageId) -> Result<EditState> {
        Ok(self.edits.store().open_state(image)?)
    }

    /// Newest-first history steps for the E08 panel (spec §3.4).
    pub fn edit_history(&self, image: ImageId) -> Result<Vec<HistoryStepMeta>> {
        Ok(lightbox_edit::history::list(
            self.edits.store().catalog(),
            image,
        )?)
    }

    /// Named snapshots for `image` (spec §3.4).
    pub fn snapshots(&self, image: ImageId) -> Result<Vec<SnapshotMeta>> {
        Ok(self.edits.store().snapshots(image)?)
    }

    /// Filmstrip edit badges, same order as `images` (spec §3.4).
    pub fn edit_badges(&self, images: &[ImageId]) -> Result<Vec<EditBadge>> {
        Ok(self.reader.edit_badges(images)?)
    }

    /// Sidecar divergence status, keyed by image (spec §3.4/§3.5's
    /// `xmp_status` names `AssetId` — the reader surface only resolves
    /// image→asset, not the reverse, so this additive method takes `ImageId`
    /// and returns each image's `AssetId` alongside its status; documented
    /// deviation, see `E09-deviations.md` Phase B). Computed now, never
    /// stored (no fs-watcher, spec §0 item 6).
    pub fn xmp_status(&self, images: &[ImageId]) -> Result<Vec<(AssetId, DivergenceStatus)>> {
        let mut out = Vec::with_capacity(images.len());
        for &image in images {
            let detail = self.reader.image_detail(image)?;
            let abs = self.reader.asset_abs_path(detail.asset)?;
            let recipe_hash = match self.edits.store().recipe_of(image)? {
                lightbox_edit::RecipeRead::Ok(r) => r.canonical_hash(),
                lightbox_edit::RecipeRead::NewerSchema { .. } => continue,
            };
            let status = self.edits.compute_status(detail.asset, &abs, recipe_hash)?;
            out.push((detail.asset, status));
        }
        Ok(out)
    }

    /// Grouped, sorted develop-preset metadata (spec §3.4 `Queries::presets`).
    pub fn presets(&self) -> Result<Vec<PresetMeta>> {
        self.edits.preset_list()
    }

    /// Pure hover preview: `preset` applied to `image`'s current recipe
    /// (spec §3.4; no session/store mutation).
    pub fn preset_preview_recipe(&self, image: ImageId, preset: PresetId) -> Result<Recipe> {
        self.edits.preset_preview(image, preset)
    }

    // ── E03 Phase D (spec §5.6) — additive preview read surface ─────────────

    /// Per-tier preview-pyramid counts/bytes, fresh from the catalog (spec
    /// §5.6 `Query::CacheStats`; narrowed — see
    /// `lightbox_preview::service`'s module doc comment for what Phase E/F
    /// still owns).
    pub fn cache_stats(&self) -> Result<CacheStats> {
        Ok(self.previews.stats()?)
    }

    /// The best currently-built preview for `image`, if any (spec §5.6
    /// `Query::PreviewState`; sync, in-memory-only — see
    /// [`lightbox_preview::PreviewService::best_available`]).
    pub fn preview_state(&self, image: ImageId) -> Option<PreviewDesc> {
        self.previews.best_available(image, 0)
    }
}

impl std::fmt::Debug for Queries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queries").finish_non_exhaustive()
    }
}
