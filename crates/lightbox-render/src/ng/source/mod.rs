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

use half::f16;

use crate::ng::colorimetry::{SourceColorimetry, SourceQuality};
use crate::ng::error::{DeviceError, SourceError};
use crate::ng::gpu::DeviceCtx;
use crate::ng::tile::{PixelBuf, PixelFormat, TileHandle};
use crate::ng::types::{Extent, TileCoord, TilePrecision};
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

    /// Upload `src` onto `ctx`'s device as a working-format (`rgba16float`)
    /// tile, lossless within format quantization (task A10). Source depth
    /// (8-bit / 16-bit-float / 32-bit-float) is normalized channel-for-channel;
    /// the **colorimetry tag passes through untouched** — no color conversion
    /// happens here, that is deferred to the `xform` nodes (E02 guardrail).
    ///
    /// The source stage gets a **dedicated** texture (not a tile-pool loan): it
    /// is long-lived and RAM-pinned (task B4), so it must not be reclaimed under
    /// working-set pressure.
    pub fn upload(&self, ctx: &DeviceCtx, src: &SourceImage) -> TileHandle {
        let extent = src.pixels.extent;
        let w = extent.w.max(1);
        let h = extent.h.max(1);
        let working = to_working_f16(&src.pixels);

        let texture = Arc::new(
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lightbox source tile"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    .union(wgpu::TextureUsages::STORAGE_BINDING)
                    .union(wgpu::TextureUsages::COPY_DST)
                    .union(wgpu::TextureUsages::COPY_SRC),
                view_formats: &[],
            }),
        );
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &working,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8), // rgba16float = 8 bytes/px
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        // Ensure the upload has landed before the handle is used as an input.
        ctx.queue.submit(std::iter::empty());
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        TileHandle::gpu(
            texture,
            view,
            Extent { w, h },
            TilePrecision::F16,
            wgpu::TextureFormat::Rgba16Float,
        )
    }
}

/// Lifts a decoded [`SourceImage`] into a CPU working tile — the CPU-path
/// counterpart of [`Uploader::upload`]. Produces the **identical** `rgba16float`
/// bytes the GPU uploader writes (same [`to_working_f16`] packing), so the CPU
/// and GPU source stages carry byte-for-byte identical working pixels (the
/// single-backend-determinism anchor of the A9 gate). No color math (E02).
pub fn to_working_tile_cpu(src: &SourceImage) -> PixelBuf {
    let w = src.pixels.extent.w.max(1);
    let h = src.pixels.extent.h.max(1);
    PixelBuf {
        bytes: to_working_f16(&src.pixels),
        format: PixelFormat::Rgba16F,
        extent: Extent { w, h },
        stride: w * 8, // rgba16float = 8 bytes/px
    }
}

/// Packs a source [`PixelBuf`] (8-bit / 16-bit-float / 32-bit-float) into tightly
/// packed `rgba16float` bytes, honoring the source row `stride`. Values are
/// carried through as-is (8-bit unorm normalized by /255); **no color math**.
fn to_working_f16(src: &PixelBuf) -> Vec<u8> {
    let w = src.extent.w.max(1) as usize;
    let h = src.extent.h.max(1) as usize;
    let stride = src.stride as usize;
    let mut out = vec![0u8; w * h * 8];

    let put = |out: &mut [u8], idx: usize, rgba: [f32; 4]| {
        let base = idx * 8;
        for (c, &v) in rgba.iter().enumerate() {
            let b = f16::from_f32(v).to_le_bytes();
            out[base + c * 2] = b[0];
            out[base + c * 2 + 1] = b[1];
        }
    };

    for y in 0..h {
        let row = &src.bytes[y * stride..];
        for x in 0..w {
            let rgba = read_source_pixel(row, x, src.format);
            put(&mut out, y * w + x, rgba);
        }
    }
    out
}

/// Reads pixel `x` of `row` in `format` as linear-magnitude RGBA f32 (8-bit
/// normalized by 255; 16-/32-bit-float read directly). No transfer/decode.
fn read_source_pixel(row: &[u8], x: usize, format: PixelFormat) -> [f32; 4] {
    match format {
        PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => {
            let i = x * 4;
            [
                f32::from(row[i]) / 255.0,
                f32::from(row[i + 1]) / 255.0,
                f32::from(row[i + 2]) / 255.0,
                f32::from(row[i + 3]) / 255.0,
            ]
        }
        PixelFormat::Rgba16F => {
            let i = x * 8;
            let ch = |o: usize| f16::from_le_bytes([row[i + o], row[i + o + 1]]).to_f32();
            [ch(0), ch(2), ch(4), ch(6)]
        }
        PixelFormat::Rgba32F => {
            let i = x * 16;
            let ch = |o: usize| {
                f32::from_le_bytes([row[i + o], row[i + o + 1], row[i + o + 2], row[i + o + 3]])
            };
            [ch(0), ch(4), ch(8), ch(12)]
        }
        PixelFormat::R16F => {
            let i = x * 2;
            let r = f16::from_le_bytes([row[i], row[i + 1]]).to_f32();
            [r, 0.0, 0.0, 1.0]
        }
    }
}
