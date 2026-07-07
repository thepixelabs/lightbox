// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-preview` — demand-driven preview provision behind
//! [`PreviewProvider`].
//!
//! Owned by **E01 as a seed** (spec §3.6). This crate freezes the trait and
//! its vocabulary types; [`EmbeddedPreviewProvider`] — cancellable,
//! request-deduping, byte-capped in-memory LRU over the camera's embedded
//! JPEG — is the M0 implementation (T20/T21). **E03** owns the real tiered
//! on-disk pyramid (T0/T1/T2) behind the same trait — this crate is
//! explicitly a seam, not a competing implementation.
//!
//! Wiring: the provider maps `ImageId → file path + orientation` through the
//! small [`AssetLocator`] seam (implemented by `lightbox-core` over the
//! catalog) so this crate stays SQL-free and unit-testable.

// E03 Phase A (T01-T05): the store foundation — config, store-level errors,
// atomic blob IO + BlobStore, the preview index + touch batcher, and the
// tier/scope/variant-keying vocabulary. Phases B-F build the embedded
// producer, codecs, scheduler, raw cache, and lifecycle on top of these.
mod config;
mod embedded;
mod error;
mod index;
mod pipeline;
mod pyramid;
mod store;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lightbox_jobs::Class;
use lightbox_types::{ImageId, Orientation, SourceTier};

pub use config::{CacheLimits, Codec, PreviewStoreConfig, Retention, StandardSize};
pub use embedded::{EmbeddedPreviewProvider, ProviderStats};
pub use error::StoreError;
pub use index::{now_unix_seconds, PreviewIndex};
pub use pyramid::{
    derive_store_key, t0_rel_path, t1_rel_path, t2_rel_dir, PreviewColorspace, PreviewDesc,
    PreviewScope, PreviewSource, ProducerId, RelPath, StoreKey, Tier, VariantHash, VariantParams,
    VARIANT_PARAMS_ENC_VER,
};
pub use store::{BlobNamespace, BlobRef, BlobStore, Store, StoreManifest, STORE_FORMAT_VERSION};

/// Which rendition of an image is being asked for (spec §3.6).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PreviewClass {
    /// A grid thumbnail, longest edge ≤ `max_px`.
    Thumb {
        /// Longest-edge budget in pixels.
        max_px: u32,
    },
    /// The loupe's source image (largest available embedded preview at M0).
    Loupe,
}

/// Lifecycle of a preview request, snapshot per poll (spec §3.6).
#[derive(Clone, Debug)]
pub enum PreviewState {
    /// Queued or decoding.
    Pending,
    /// Decoded pixels, shared. Hold the `Arc` for as long as they're needed.
    Ready(Arc<DecodedImage>),
    /// Terminally failed.
    Failed(PreviewError),
}

/// Why a preview could not be produced.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PreviewError {
    /// The original has no (or no usable) embedded preview — the grid shows
    /// a placeholder until E03 renders real previews (spec T20).
    #[error("no embedded preview in this file")]
    NoEmbedded,
    /// Reading the original file failed.
    #[error("io: {0}")]
    Io(String),
    /// The embedded preview failed to decode.
    #[error("decode: {0}")]
    Decode(String),
    /// The request was cancelled (grid scroll-out, shutdown).
    #[error("cancelled")]
    Cancelled,
    /// No provider capable of serving previews is wired up (Phase-4 sessions,
    /// before T21's `EmbeddedPreviewProvider`).
    #[error("preview provider unavailable: {0}")]
    Unavailable(String),
    /// The on-disk store manifest (`store.toml`) is unreadable or malformed
    /// (E03 spec §3.2, Phase A T01).
    #[error("store manifest: {0}")]
    Manifest(String),
    /// The store's on-disk format is newer than this build supports
    /// (forward-only, matching the catalog's own `SchemaTooNew` posture).
    #[error("preview store format {found} is newer than this build supports (max {supported})")]
    StoreTooNew {
        /// `store.toml`'s `format`.
        found: u32,
        /// Highest format this build writes/understands.
        supported: u32,
    },
    /// A catalog operation failed while loading/updating the preview index
    /// (E03 spec §5.2, Phase A T04). Carried as a string —
    /// `lightbox_catalog::CatalogError` is not `Clone`, and this enum must
    /// stay `Clone` (`PreviewState::Failed` clones its error, spec T21).
    #[error("catalog: {0}")]
    Catalog(String),
}

impl From<std::io::Error> for PreviewError {
    fn from(e: std::io::Error) -> PreviewError {
        PreviewError::Io(e.to_string())
    }
}

/// Decoded preview pixels (spec §3.6).
#[derive(Clone)]
pub struct DecodedImage {
    /// RGBA8, sRGB-encoded, tightly packed rows.
    pub px: Arc<[u8]>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Thumbnails: `true` (orientation baked on CPU); loupe source: `false`
    /// (the display-transform node applies it — spec §3.6).
    pub orientation_applied: bool,
    /// Which tier produced the pixels (M0: embedded preview only).
    pub tier: SourceTier,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("orientation_applied", &self.orientation_applied)
            .field("tier", &self.tier)
            .finish_non_exhaustive()
    }
}

/// Handle to one preview request. Opaque to callers; allocated by the
/// provider that issued it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PreviewTicket {
    id: u64,
}

impl PreviewTicket {
    /// Wraps a provider-allocated ticket id (for `PreviewProvider`
    /// implementations; callers treat tickets as opaque).
    pub fn new(id: u64) -> PreviewTicket {
        PreviewTicket { id }
    }

    /// The provider-allocated id.
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// Where an image's original file lives on disk (plus its effective
/// orientation). The provider's link back to the catalog **without** a
/// catalog dependency: `lightbox-core` implements this over its readers.
/// Constructor plumbing for [`EmbeddedPreviewProvider`] — not part of the
/// frozen §3.6 surface.
pub trait AssetLocator: Send + Sync {
    /// Resolves an image to its backing original.
    fn locate(&self, image: ImageId) -> Result<LocatedAsset, PreviewError>;
}

/// One located original.
#[derive(Clone, Debug)]
pub struct LocatedAsset {
    /// Absolute path of the original file.
    pub path: PathBuf,
    /// Effective orientation (image override or the asset's EXIF value) —
    /// baked into thumbnails, left to the render node for the loupe source.
    pub orientation: Orientation,
}

/// Demand-driven preview provision (spec §3.6, frozen surface for E03).
pub trait PreviewProvider: Send + Sync {
    /// Requests a preview of `image` at `class`, scheduled under
    /// `class_prio` (thumbs: `Class::Background`; loupe source:
    /// `Class::Interactive` — spec §3.6).
    fn request(&self, image: ImageId, class: PreviewClass, class_prio: Class) -> PreviewTicket;
    /// Snapshot of the request's state — poll once per frame.
    fn poll(&self, t: &PreviewTicket) -> PreviewState;
    /// Cancels the request. The grid MUST cancel on scroll-out (spec §5.3).
    fn cancel(&self, t: &PreviewTicket);
}

/// A provider that fails every request immediately with
/// [`PreviewError::Unavailable`] — for embedders/tests that need a
/// `Session`-shaped object without preview wiring. Real sessions hand out
/// [`EmbeddedPreviewProvider`] since T21.
#[derive(Debug, Default)]
pub struct UnavailablePreviewProvider {
    next_id: AtomicU64,
}

impl PreviewProvider for UnavailablePreviewProvider {
    fn request(&self, _image: ImageId, _class: PreviewClass, _prio: Class) -> PreviewTicket {
        PreviewTicket::new(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn poll(&self, _t: &PreviewTicket) -> PreviewState {
        PreviewState::Failed(PreviewError::Unavailable(
            "no preview provider is wired to this session".to_owned(),
        ))
    }

    fn cancel(&self, _t: &PreviewTicket) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_provider_fails_every_request() {
        let provider = UnavailablePreviewProvider::default();
        let a = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
        let b = provider.request(
            ImageId(2),
            PreviewClass::Thumb { max_px: 256 },
            Class::Background,
        );
        assert_ne!(a, b, "tickets must be distinct");
        match provider.poll(&a) {
            PreviewState::Failed(PreviewError::Unavailable(_)) => {}
            other => panic!("expected Unavailable, got {other:?}"),
        }
        provider.cancel(&a); // no-op, must not panic
    }
}
