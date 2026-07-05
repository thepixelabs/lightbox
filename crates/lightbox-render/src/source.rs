// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `SourceResolver` — the pixels-in seam (spec §3.5, frozen surface).
//!
//! Where the engine gets input pixels for `(image, scale)`. M0 implementation
//! (E01 Phase 6): embedded preview via `lightbox-preview`. E03 swaps in the
//! tiered store; E02+E05 add the raw-decode path.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_types::{ImageId, Orientation};

use crate::engine::RenderScale;
use crate::error::SourceError;

/// Resolves input pixels for a render (spec §3.5, frozen).
pub trait SourceResolver: Send + Sync {
    /// Produces source pixels for `image` at (at least) `scale`'s resolution.
    /// Must honor `cancel` at its checkpoints and return
    /// [`SourceError::Cancelled`] when observed.
    fn resolve(
        &self,
        image: ImageId,
        scale: RenderScale,
        cancel: &CancelToken,
    ) -> Result<SourceImage, SourceError>;
}

/// Source pixels handed to the engine (spec §3.5).
#[derive(Clone)]
pub struct SourceImage {
    /// The pixels, tightly packed rows.
    pub px: Arc<[u8]>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel layout/encoding. M0: `Rgba8Srgb` only.
    pub format: SourcePixelFormat,
    /// EXIF orientation — NOT yet applied; the display-transform node applies
    /// it (spec §3.1: never baked into stored pixels).
    pub orientation: Orientation,
    /// Which tier produced these pixels.
    pub tier: SourceTier,
}

impl std::fmt::Debug for SourceImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("orientation", &self.orientation)
            .field("tier", &self.tier)
            .finish_non_exhaustive()
    }
}

/// Pixel layout of a [`SourceImage`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum SourcePixelFormat {
    /// 8-bit RGBA, sRGB-encoded values (M0's only format).
    Rgba8Srgb,
}

// `SourceTier` is shared vocabulary with `lightbox-preview`'s `DecodedImage`
// (spec §3.6), so it lives in `lightbox-types`; re-exported here so the spec
// §3.5 path (`lightbox_render::SourceTier`) is unchanged.
pub use lightbox_types::SourceTier;

/// A resolver with no sources: every resolve is [`SourceError::NotFound`].
///
/// Placeholder wiring for engines whose pipeline needs no input pixels (the
/// Phase 2 tracer bullet) and for tests. Phase 6 (T23) brings the real
/// `EmbeddedPreviewProvider`-backed resolver.
#[derive(Default, Debug, Clone, Copy)]
pub struct NullSourceResolver;

impl SourceResolver for NullSourceResolver {
    fn resolve(
        &self,
        _image: ImageId,
        _scale: RenderScale,
        _cancel: &CancelToken,
    ) -> Result<SourceImage, SourceError> {
        Err(SourceError::NotFound)
    }
}
