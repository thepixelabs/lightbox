// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-preview`, demand-driven preview provision behind
//! [`PreviewProvider`].
//!
//! Owned by **E01 as a seed** (spec §3.6). This crate freezes the trait and
//! its vocabulary types; [`EmbeddedPreviewProvider`], cancellable,
//! request-deduping, byte-capped in-memory LRU over the camera's embedded
//! JPEG, is the M0 implementation (T20/T21). **E03** owns the real tiered
//! on-disk pyramid (T0/T1/T2) behind the same trait, this crate is
//! explicitly a seam, not a competing implementation.
//!
//! Wiring: the provider maps `ImageId → file path + orientation` through the
//! small [`AssetLocator`] seam (implemented by `lightbox-core` over the
//! catalog) so this crate stays SQL-free and unit-testable.

// E03 Phase A (T01-T05): the store foundation, config, store-level errors,
// atomic blob IO + BlobStore, the preview index + touch batcher, and the
// tier/scope/variant-keying vocabulary. Phase B (T06-T09) builds the
// embedded-preview producer (extract/producer), decode-for-display (decode),
// and wires both, plus the decoded LRU/prefetch, into `embedded.rs`'s
// provider. Phase C (T10-T12) adds the `PreviewCodec` trait + JPEG backend +
// Lanczos3 resize (codec), the feature-gated libjxl encode FFI + jxl-oxide
// decode (codec/jxl.rs, `jxl` feature, OFF by default, libjxl absent on
// this build machine), and the T1 build pipeline (producer::ensure_t1).
// Phase D (T13-T16) adds the priority build scheduler (sched) and the
// `PreviewService` facade (service), tickets, `PreviewEvent`, viewport
// integration, bulk build/progress. Phases E-F build the raw cache and
// lifecycle/T2/hardening on top of these.
mod codec;
mod config;
mod decode;
mod embedded;
mod error;
mod extract;
mod index;
mod pipeline;
mod producer;
mod pyramid;
// E03 Phase E (T17/T18): the raw decode cache, container format
// (content-hash + params-hash keyed, zstd + checksum, mmap'd read) and
// catalog-backed accounting/LRU/reconcile.
mod rawcache;
mod sched;
mod service;
mod store;
// E03 Phase F (T20): the T2 tiled preview store, tile addressing, manifest,
// aggregate accounting, and a synthetic producer for tests (E05 supplies the
// real one at M1+).
mod t2;
// E03 Phase F (T20) AC: "manifest survives kill-loop", a real SIGKILL
// crash-loop test, kept as its own module (not `tests/`) so it can drive
// `t2::ensure_t2_synthetic`/`t2_tile_read` directly without widening this
// crate's public API just for a test harness.
#[cfg(test)]
mod t2_crash_loop;
// E03 Phase F (T19): eviction/retention (tier order, refcount-before-unlink).
mod evict;
// E03 Phase F (T21): journaled relocate.
mod relocate;
// E03 Phase F (T22): the hot thumbnail atlas (`thumbcache.sqlite`, a
// separate, delete-safe SQLite DB, NOT the catalog).
mod thumbs;
// E03 Phase F (T21): verify_store(Quick|Full) + PurgeScope.
mod verify;
// E03 Phase F (T21/T23): real SIGKILL fault injection across T0/T1/raw-cache
// writes, asserting the real `verify_store(Full)`/`RawCache::reconcile`
// seams stay clean after every kill (same convention as `t2_crash_loop.rs`).
#[cfg(test)]
mod store_crash_loop;
// E03 Phase F (T21/T23): real SIGKILL fault injection against journaled
// relocation (`relocate`), kill mid-copy, reopen/resume, zero lost entries.
#[cfg(test)]
mod relocate_crash_loop;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lightbox_jobs::Class;
use lightbox_types::{AssetId, ContentHash, ImageId, Orientation, SourceTier};

pub use config::{CacheLimits, Codec, PreviewStoreConfig, Retention, StandardSize};
pub use decode::DecodedPreview;
pub use embedded::{EmbeddedPreviewProvider, ProviderStats};
pub use error::StoreError;
pub use index::{now_unix_seconds, PreviewIndex};
pub use pyramid::{
    derive_store_key, t0_rel_path, t1_rel_path, t2_rel_dir, PreviewColorspace, PreviewDesc,
    PreviewScope, PreviewSource, ProducerId, RelPath, StoreKey, Tier, TierSet, VariantHash,
    VariantParams, VARIANT_PARAMS_ENC_VER,
};
// E03 Phase F (T20): the T2 tile store's public vocabulary.
pub use t2::{EncodedTile, TileCoord, TileGrid};
// E03 Phase E (T17/T18): the raw decode cache (spec §3.3/§5.4).
pub use rawcache::{
    EvictReport, PlanarBuf, PlaneData, RawCache, RawCacheError, RawCacheHit, RawCacheKey,
    RawCachePurgeReport, RawStageMeta, ReconcileReport, SampleFormat,
};
// E03 Phase D (T13): the priority build scheduler (spec §3.4/§5.6).
pub use sched::{
    BuildFn, BuildFuture, BuildKey, BuildPriority, BuildRuntime, BulkHandle, CacheKind,
    EnqueueError, EventSink, PreviewEvent, Scheduler,
};
// E03 Phase D (T14-T16): the `PreviewService` facade (spec §5.2).
pub use service::{CacheStats, PreviewRequest, PreviewService, PurgeReport, QuickVerifyReport};
pub use store::{BlobNamespace, BlobRef, BlobStore, Store, StoreManifest, STORE_FORMAT_VERSION};
// E03 Phase F (T19): eviction/retention/discard report.
pub use evict::PreviewEvictReport;
// E03 Phase F (T21): journaled relocation.
pub use relocate::{ProgressSink, RelocateError, RelocateProgress};
// E03 Phase F (T22): the thumbnail atlas's payload type.
pub use thumbs::EncodedThumb;
// E03 Phase F (T21): verify_store + PurgeScope.
pub use verify::{PurgeScope, VerifyMode, VerifyReport};

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
    /// The original has no (or no usable) embedded preview, the grid shows
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
    /// (E03 spec §5.2, Phase A T04). Carried as a string
    /// `lightbox_catalog::CatalogError` is not `Clone`, and this enum must
    /// stay `Clone` (`PreviewState::Failed` clones its error, spec T21).
    #[error("catalog: {0}")]
    Catalog(String),
    /// A `PreviewCodec::encode` call failed (E03 spec §5.3, Phase C T10/T11)
    /// e.g. the JPEG/JXL encoder rejected the buffer, or (JPEG backend)
    /// panicked across the mozjpeg FFI boundary (caught, never propagated
    /// as a raw panic, see `codec.rs`).
    #[error("encode: {0}")]
    Encode(String),
}

impl From<std::io::Error> for PreviewError {
    fn from(e: std::io::Error) -> PreviewError {
        PreviewError::Io(e.to_string())
    }
}

/// Decoded preview pixels (spec §3.6).
#[derive(Clone)]
pub struct DecodedImage {
    /// RGBA8 display-referred pixels, tightly packed rows, encoded in
    /// [`DecodedImage::colorspace`].
    ///
    /// This field's contract used to read "RGBA8, **sRGB-encoded**", which was
    /// only true because the colour space was being dropped at this seam. The
    /// pixels were always in whatever space the file was tagged with; the
    /// promise, not the bytes, is what changed.
    pub px: Arc<[u8]>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// **The colour space `px` is encoded in**, the tag the render engine's
    /// source→working input transform is chosen from (spec §3.5's "embedded
    /// ICC/EXIF colorspace honored as a tag"; §5.3's "untagged ⇒ assumed
    /// sRGB").
    ///
    /// # Why this field exists
    ///
    /// It is the link that was missing. `lightbox-decode` reads the embedded
    /// profile, the preview index persists a colorspace column, and the render
    /// engine has an input transform keyed on colorimetry, but `DecodedImage`
    /// had no colour-space field at all, so the tag died here and every tagged
    /// file reached the engine as "assumed sRGB". A Display-P3 photo (i.e. most
    /// photos from a modern phone) therefore rendered over-saturated.
    ///
    /// An **additive** field on the frozen §3.6 shape: [`PreviewColorspace::Srgb`]
    /// is what every producer meant before it existed, so a caller that ignores
    /// it behaves exactly as it used to. Recorded in
    /// `docs/plan/epics/E02-deviations.md`.
    pub colorspace: PreviewColorspace,
    /// `true` for every [`PreviewClass`] as of E03 Phase B (T08):
    /// decode-for-display always bakes EXIF orientation on the CPU (spec
    /// §3.5), including the loupe source, see `decode.rs`'s module doc
    /// comment. Kept as a field (rather than removed) because it is part of
    /// the frozen §3.6 `DecodedImage` shape; a future producer that ships
    /// pre-rotated bytes some other way could still set it accurately.
    pub orientation_applied: bool,
    /// Which tier produced the pixels (M0: embedded preview only).
    pub tier: SourceTier,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("colorspace", &self.colorspace)
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
/// Constructor plumbing for [`EmbeddedPreviewProvider`], not part of the
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
    /// Effective orientation (image override or the asset's EXIF value)
    /// baked on the CPU by decode-for-display (spec §3.5, T08) for every
    /// [`PreviewClass`], loupe included as of E03 Phase B (see `decode.rs`'s
    /// module doc comment on why the loupe path no longer leaves this to the
    /// render node).
    pub orientation: Orientation,
    /// The owning asset (E03 spec §3.1: T0 is asset-scope), added in Phase
    /// B (T07) so [`EmbeddedPreviewProvider`] can key the on-disk store/
    /// catalog dedupe without a catalog dependency of its own (this trait
    /// stays the SQL-free boundary; `lightbox-core`'s `CatalogAssetLocator`
    /// resolves it).
    pub asset: AssetId,
    /// The asset's content hash, the dominant component of the store key
    /// (spec §3.1) and the denormalized column every `preview` row carries.
    /// Added alongside `asset` in Phase B (T07).
    pub content_hash: ContentHash,
}

/// Demand-driven preview provision (spec §3.6, frozen surface for E03).
pub trait PreviewProvider: Send + Sync {
    /// Requests a preview of `image` at `class`, scheduled under
    /// `class_prio` (thumbs: `Class::Background`; loupe source:
    /// `Class::Interactive`, spec §3.6).
    fn request(&self, image: ImageId, class: PreviewClass, class_prio: Class) -> PreviewTicket;
    /// Snapshot of the request's state, poll once per frame.
    fn poll(&self, t: &PreviewTicket) -> PreviewState;
    /// Cancels the request. The grid MUST cancel on scroll-out (spec §5.3).
    fn cancel(&self, t: &PreviewTicket);
}

/// A provider that fails every request immediately with
/// [`PreviewError::Unavailable`], for embedders/tests that need a
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
