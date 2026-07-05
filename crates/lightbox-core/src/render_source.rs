// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The M0 [`SourceResolver`] — embedded preview via `lightbox-preview`
//! (spec §3.5, T23 wiring).
//!
//! Where the engine gets input pixels for `(image, scale)`: the provider's
//! **loupe-class** rendition (the largest embedded JPEG, decoded unoriented
//! — the display-transform node applies orientation, spec §3.6), requested
//! at `Class::Interactive` and awaited on the render worker with cancel
//! checkpoints. Cache hits in the provider's LRU resolve on the first poll.
//!
//! Wiring, not a competing implementation: **E03** swaps the tiered on-disk
//! store in behind the same `SourceResolver` trait; **E02+E05** add the
//! raw-decode path.

use std::sync::Arc;
use std::time::Duration;

use lightbox_jobs::{CancelToken, Class};
use lightbox_preview::{AssetLocator, PreviewClass, PreviewError, PreviewProvider, PreviewState};
use lightbox_render::{RenderScale, SourceError, SourceImage, SourcePixelFormat, SourceResolver};
use lightbox_types::{ImageId, Orientation};

/// Resolves engine source pixels through the session's preview provider
/// (M0: [`lightbox_preview::EmbeddedPreviewProvider`]) and the catalog-backed
/// [`AssetLocator`] (for the effective orientation).
pub(crate) struct PreviewSourceResolver {
    previews: Arc<dyn PreviewProvider>,
    locator: Arc<dyn AssetLocator>,
}

impl PreviewSourceResolver {
    pub(crate) fn new(
        previews: Arc<dyn PreviewProvider>,
        locator: Arc<dyn AssetLocator>,
    ) -> PreviewSourceResolver {
        PreviewSourceResolver { previews, locator }
    }
}

fn map_preview_err(e: PreviewError) -> SourceError {
    match e {
        PreviewError::NoEmbedded => SourceError::NotFound,
        PreviewError::Cancelled => SourceError::Cancelled,
        PreviewError::Io(m) => SourceError::Io(m),
        PreviewError::Decode(m) => SourceError::Decode(m),
        // Unavailable + future variants: a decode-side failure as far as the
        // engine is concerned.
        other => SourceError::Decode(other.to_string()),
    }
}

impl SourceResolver for PreviewSourceResolver {
    /// `scale` is unused at M0: the loupe rendition is always the largest
    /// embedded preview (the engine's display transform downsamples).
    /// E03's tiered store picks a tier by `scale` behind this same seam.
    fn resolve(
        &self,
        image: ImageId,
        _scale: RenderScale,
        cancel: &CancelToken,
    ) -> Result<SourceImage, SourceError> {
        if cancel.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        // Effective orientation (image override or asset EXIF) comes from
        // the catalog; the loupe pixels are decoded UNORIENTED (spec §3.6).
        let located = self.locator.locate(image).map_err(map_preview_err)?;

        let ticket = self
            .previews
            .request(image, PreviewClass::Loupe, Class::Interactive);
        loop {
            if cancel.is_cancelled() {
                self.previews.cancel(&ticket);
                return Err(SourceError::Cancelled);
            }
            match self.previews.poll(&ticket) {
                PreviewState::Pending => std::thread::sleep(Duration::from_micros(500)),
                PreviewState::Ready(img) => {
                    // Defensive: a provider that pre-bakes orientation must
                    // not have it applied twice by the node.
                    let orientation = if img.orientation_applied {
                        Orientation::O1
                    } else {
                        located.orientation
                    };
                    return Ok(SourceImage {
                        px: Arc::clone(&img.px),
                        width: img.width,
                        height: img.height,
                        format: SourcePixelFormat::Rgba8Srgb,
                        orientation,
                        tier: img.tier,
                    });
                }
                PreviewState::Failed(e) => return Err(map_preview_err(e)),
            }
        }
    }
}
