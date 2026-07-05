// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-preview` — demand-driven preview provision behind
//! [`PreviewProvider`].
//!
//! Owned by **E01 as a seed** (spec §3.6). This crate freezes the trait and
//! its vocabulary types; `EmbeddedPreviewProvider` — a cancellable,
//! request-deduping, byte-capped in-memory LRU over the camera's embedded
//! JPEG — is E01 Phase 5 (T21). **E03** owns the real tiered on-disk pyramid
//! (T0/T1/T2) behind the same trait — this crate is explicitly a seam, not a
//! competing implementation.
//!
//! **Status: Phase-4 slice.** The frozen §3.6 surface below is what
//! `lightbox-core`'s `Session::previews()` (seam 1) hands out; until T21
//! lands, sessions are wired with [`UnavailablePreviewProvider`], which fails
//! every request immediately instead of pretending to have pixels.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lightbox_jobs::Class;
use lightbox_types::{ImageId, SourceTier};

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

/// Wiring placeholder until T21's `EmbeddedPreviewProvider`: every request
/// fails immediately with [`PreviewError::Unavailable`]. Sessions built in
/// Phase 4 hand this out so `Session::previews()` (spec §3.8) exists without
/// pretending pixels are coming.
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
            "embedded preview provider lands in E01 Phase 5 (T21)".to_owned(),
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
