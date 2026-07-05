// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Catalog-backed [`AssetLocator`] — the glue that lets
//! `lightbox-preview`'s [`EmbeddedPreviewProvider`] resolve `ImageId → file
//! path + effective orientation` without a catalog dependency of its own
//! (T21 wiring; no SQL crosses this seam, spec §2.3).
//!
//! [`EmbeddedPreviewProvider`]: lightbox_preview::EmbeddedPreviewProvider

use std::sync::Arc;

use lightbox_catalog::Catalog;
use lightbox_preview::{AssetLocator, LocatedAsset, PreviewError};
use lightbox_types::ImageId;

/// Resolves images through a pooled WAL-snapshot reader. Called from the
/// provider's decode jobs (blocking pool), never the UI thread.
pub(crate) struct CatalogAssetLocator {
    catalog: Arc<Catalog>,
}

impl CatalogAssetLocator {
    pub(crate) fn new(catalog: Arc<Catalog>) -> CatalogAssetLocator {
        CatalogAssetLocator { catalog }
    }
}

impl AssetLocator for CatalogAssetLocator {
    fn locate(&self, image: ImageId) -> Result<LocatedAsset, PreviewError> {
        let reader = self.catalog.reader();
        let detail = reader
            .image_detail(image)
            .map_err(|e| PreviewError::Io(format!("catalog lookup for image {}: {e}", image.0)))?;
        let path = reader
            .asset_abs_path(detail.asset)
            .map_err(|e| PreviewError::Io(format!("asset path for image {}: {e}", image.0)))?;
        Ok(LocatedAsset {
            path,
            orientation: detail.orientation,
        })
    }
}
