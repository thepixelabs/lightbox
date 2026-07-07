// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The pixels-in / device / tiles-out seam traits (spec §3.8) + the upload path
//! (task **A10**) + progressive ladder (task **C6**).
//!
//! Owner: **A-gpu** owns the engine-side [`Uploader`] (upload path) and the
//! ladder plumbing. The [`DeviceProvider`] / [`SourceProvider`] / [`TileSink`]
//! **traits are implemented by neighbors** (E01/E08 shell, E02/E03) and only
//! *consumed* here — the engine never decodes and never reads the preview store
//! directly.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use crate::ng::colorimetry::{SourceColorimetry, SourceQuality};
use crate::ng::error::{DeviceError, SourceError};
use crate::ng::gpu::DeviceCtx;
use crate::ng::tile::{PixelBuf, TileHandle};
use crate::ng::types::{Extent, TileCoord};
use crate::ng::BoxFuture;

/// A shared device/queue pair (the shell's wgpu handles).
pub type DeviceHandles = (Arc<wgpu::Device>, Arc<wgpu::Queue>);

/// E01/E08 (the shell owns the device): current handles + coordinated rebuild
/// on device-lost (spec §3.8). Implemented by the shell.
pub trait DeviceProvider: Send + Sync {
    /// The current shared device + queue.
    fn current(&self) -> DeviceHandles;
    /// Rebuild after device loss, yielding fresh handles (task E2).
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>>;
}

/// What the engine wants from the source seam (spec §3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceWant {
    /// The best available tier no larger than `max_px` pixels (fast first paint).
    BestAvailable {
        /// Upper bound on the longest edge, pixels.
        max_px: u32,
    },
    /// The full decoded image (already-demosaiced RGB at M1 — E02).
    DecodedFull,
    /// Cached partially-decoded raw state (E03 `rawcache/`).
    CachedRawState,
}

/// Source pixels handed to the engine (spec §3.8). The engine never decodes.
#[derive(Clone, Debug)]
pub struct SourceImage {
    /// The pixels (8/16/f32 depth — [`crate::ng::PixelFormat`]).
    pub pixels: PixelBuf,
    /// The source colorimetry tag (opaque to the engine).
    pub colorimetry: SourceColorimetry,
    /// Full source resolution (may exceed `pixels` for a preview tier).
    pub full_extent: Extent,
    /// Which tier produced these pixels.
    pub quality: SourceQuality,
}

/// E02/E03: the pixels-in seam (spec §3.8). Implemented by the source stack.
pub trait SourceProvider: Send + Sync {
    /// Fetch `want` for `image`, honoring `cancel`.
    fn fetch(
        &self,
        image: ImageId,
        want: SourceWant,
        cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>>;
}

/// E03: completed 1:1 tiles offered for T2 persistence — fire-and-forget; E03
/// owns the store + eviction (spec §3.8). Implemented by the preview stack.
pub trait TileSink: Send + Sync {
    /// Offer a completed post-display-transform tile for T2 caching.
    fn offer_t2(&self, image: ImageId, recipe_hash: blake3::Hash, tile: TileCoord, px: PixelBuf);
}

/// Uploads decoded [`SourceImage`] pixels (8/16/f32) into a working-format
/// tile — colorimetry passes through untouched; conversion is deferred to the
/// `xform` nodes (spec task **A10**). **A10 fills the internals.**
#[derive(Default)]
pub struct Uploader {}

impl Uploader {
    /// A new uploader.
    pub fn new() -> Uploader {
        Uploader::default()
    }

    /// Upload `src` onto `ctx`'s device as a working-format tile (lossless
    /// within format quantization; task A10).
    pub fn upload(&self, ctx: &DeviceCtx, src: &SourceImage) -> TileHandle {
        let _ = (ctx, src);
        unimplemented!("A10 (A-gpu): Uploader::upload — 8/16/f32 → working-format texture")
    }
}
