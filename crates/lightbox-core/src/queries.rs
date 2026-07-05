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

use lightbox_catalog::{
    CatalogCounts, FolderNode, ImageDetail, ImageQuery, ImageSummary, Page, ReaderHandle,
};
use lightbox_types::{AssetId, ImageId};

use crate::error::Result;

/// A borrowed snapshot view of the catalog (spec §3.8).
pub struct Queries {
    reader: ReaderHandle,
}

impl Queries {
    pub(crate) fn new(reader: ReaderHandle) -> Queries {
        Queries { reader }
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
}

impl std::fmt::Debug for Queries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queries").finish_non_exhaustive()
    }
}
