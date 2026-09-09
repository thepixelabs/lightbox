// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The tile currency, `TileHandle` / `TileView` / `CpuTileView` / `PixelBuf`.
//!
//! **SCAFFOLD-FROZEN SEAM.** This is the shared vocabulary that `node`, `exec`,
//! `cache`, `gpu`, and `source` all speak; re-homing or reshaping it needs a
//! deviation entry (it is the counterpart to the [`crate::ng::exec::backend`]
//! `trait Backend` seam, Risk R8). Field internals of the opaque
//! [`TileHandle`] are filled by **A-gpu** (the `gpu::TilePool`).

use std::sync::Arc;

use half::f16;

use crate::ng::types::{Extent, Roi, TilePrecision};

/// A reference-counted handle to a working-format tile living in the
/// [`crate::ng::gpu::TilePool`] (VRAM) or, on the CPU path, a host buffer.
///
/// Cloning shares the tile; the pool reclaims it when the last handle drops
/// (the currency threaded through the `exec`↔`cache`↔`backend` seam).
///
/// # Payload
///
/// The handle carries **whichever backend produced it** (merged frozen seam,
/// `E05-deviations.md` D-A-core-1 + D-tile): a GPU variant
/// wrapping a pooled `wgpu::Texture` (reclaimed by the [`crate::ng::gpu::TilePool`]
/// via strong-count once the last handle drops) or a CPU variant wrapping a host
/// [`PixelBuf`] for the rayon path. An empty handle (`Default`) carries no tile
/// it is the "not yet produced" sentinel the executor replaces.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct TileHandle {
    storage: Option<TileStorage>,
}

#[derive(Clone)]
enum TileStorage {
    Gpu(Arc<GpuTile>),
    Cpu(Arc<PixelBuf>),
}

/// The GPU-resident backing of a [`TileHandle`]: a pooled texture plus its view
/// and shape metadata. The `Arc<wgpu::Texture>` is shared with the owning
/// [`crate::ng::gpu::TilePool`], which detects reuse by its strong count.
struct GpuTile {
    texture: Arc<wgpu::Texture>,
    view: wgpu::TextureView,
    extent: Extent,
    precision: TilePrecision,
    format: wgpu::TextureFormat,
}

impl TileHandle {
    /// GPU constructor used by the [`crate::ng::gpu::TilePool`].
    pub(crate) fn gpu(
        texture: Arc<wgpu::Texture>,
        view: wgpu::TextureView,
        extent: Extent,
        precision: TilePrecision,
        format: wgpu::TextureFormat,
    ) -> TileHandle {
        TileHandle {
            storage: Some(TileStorage::Gpu(Arc::new(GpuTile {
                texture,
                view,
                extent,
                precision,
                format,
            }))),
        }
    }

    /// CPU constructor, the rayon-path tile currency (A-core CPU backend).
    pub fn from_cpu(pixels: PixelBuf) -> TileHandle {
        TileHandle {
            storage: Some(TileStorage::Cpu(Arc::new(pixels))),
        }
    }

    /// True when this handle carries no tile (the `Default` sentinel).
    pub fn is_empty(&self) -> bool {
        self.storage.is_none()
    }

    /// The tile extent, if any.
    pub fn extent(&self) -> Option<Extent> {
        match &self.storage {
            Some(TileStorage::Gpu(g)) => Some(g.extent),
            Some(TileStorage::Cpu(p)) => Some(p.extent),
            None => None,
        }
    }

    /// The tile precision, if this is a working-format tile.
    pub fn precision(&self) -> Option<TilePrecision> {
        match &self.storage {
            Some(TileStorage::Gpu(g)) => Some(g.precision),
            _ => None,
        }
    }

    /// The GPU texture view a node binds as an input, if this is a GPU tile.
    pub fn texture_view(&self) -> Option<&wgpu::TextureView> {
        match &self.storage {
            Some(TileStorage::Gpu(g)) => Some(&g.view),
            _ => None,
        }
    }

    /// The GPU texture, if this is a GPU tile (readback / copy source).
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        match &self.storage {
            Some(TileStorage::Gpu(g)) => Some(&g.texture),
            _ => None,
        }
    }

    /// The GPU texture format, if this is a GPU tile.
    pub fn format(&self) -> Option<wgpu::TextureFormat> {
        match &self.storage {
            Some(TileStorage::Gpu(g)) => Some(g.format),
            _ => None,
        }
    }

    /// The host pixels, if this is a CPU tile (the A-core CPU-backend accessor).
    pub fn cpu(&self) -> Option<&PixelBuf> {
        match &self.storage {
            Some(TileStorage::Cpu(p)) => Some(p),
            _ => None,
        }
    }
}

impl std::fmt::Debug for TileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.storage {
            None => f.write_str("TileHandle(empty)"),
            Some(TileStorage::Gpu(g)) => f
                .debug_struct("TileHandle::Gpu")
                .field("extent", &g.extent)
                .field("precision", &g.precision)
                .field("format", &g.format)
                .finish(),
            Some(TileStorage::Cpu(p)) => f
                .debug_struct("TileHandle::Cpu")
                .field("pixels", p)
                .finish(),
        }
    }
}

/// A GPU node's read-only view of one input tile (spec §3.2 `eval_gpu` inputs).
/// Bound at `@group(0)` per the kernel conventions.
pub struct TileView<'a> {
    /// The texture view the node binds as input.
    pub view: &'a wgpu::TextureView,
    /// The tile's storage precision.
    pub precision: TilePrecision,
    /// The source-pixel ROI this tile covers (apron included).
    pub roi: Roi,
}

/// A CPU node's read-only view of one input tile (spec §3.2 `eval_cpu` inputs).
pub struct CpuTileView<'a> {
    /// The tile pixels (working-format planes).
    pub pixels: &'a PixelBuf,
    /// The source-pixel ROI this tile covers (apron included).
    pub roi: Roi,
    /// The tile's storage precision.
    pub precision: TilePrecision,
}

/// An owned CPU pixel buffer, the currency of the `Buffer` render target,
/// the source seam, and CPU readback (spec §3.6/§3.8).
#[derive(Clone, PartialEq, Eq)]
pub struct PixelBuf {
    /// Tightly/`stride`-packed pixel bytes.
    pub bytes: Vec<u8>,
    /// The pixel encoding.
    pub format: PixelFormat,
    /// Dimensions in pixels.
    pub extent: Extent,
    /// Bytes per row (≥ `extent.w * format.bytes_per_pixel()`).
    pub stride: u32,
}

impl std::fmt::Debug for PixelBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBuf")
            .field("format", &self.format)
            .field("extent", &self.extent)
            .field("stride", &self.stride)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl PixelBuf {
    /// A tightly-packed, zero-filled buffer of `extent` at `format`
    /// (`stride == extent.w * bytes_per_pixel`).
    pub fn new_zeroed(format: PixelFormat, extent: Extent) -> PixelBuf {
        let stride = extent.w.saturating_mul(format.bytes_per_pixel());
        let len = (stride as usize).saturating_mul(extent.h as usize);
        PixelBuf {
            bytes: vec![0u8; len],
            format,
            extent,
            stride,
        }
    }

    /// The byte offset of pixel `(x, y)` in [`PixelBuf::bytes`].
    #[inline]
    fn offset(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.stride as usize)
            + (x as usize) * (self.format.bytes_per_pixel() as usize)
    }

    /// Read pixel `(x, y)` as linear `[r, g, b, a]` f32.
    ///
    /// **No transfer function is applied**: 8-bit values are scaled by `1/255`
    /// (linear); `f16`/`f32` are decoded straight. Display encoding (sRGB/ICC)
    /// is the `xform.display` node's job, the engine holds zero color science
    /// (E02 guardrail).
    #[inline]
    pub fn get_rgba_f32(&self, x: u32, y: u32) -> [f32; 4] {
        let o = self.offset(x, y);
        let b = &self.bytes;
        match self.format {
            PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => [
                b[o] as f32 / 255.0,
                b[o + 1] as f32 / 255.0,
                b[o + 2] as f32 / 255.0,
                b[o + 3] as f32 / 255.0,
            ],
            PixelFormat::Rgba16F => {
                let rd = |i: usize| f16::from_le_bytes([b[o + i], b[o + i + 1]]).to_f32();
                [rd(0), rd(2), rd(4), rd(6)]
            }
            PixelFormat::Rgba32F => {
                let rd = |i: usize| {
                    f32::from_le_bytes([b[o + i], b[o + i + 1], b[o + i + 2], b[o + i + 3]])
                };
                [rd(0), rd(4), rd(8), rd(12)]
            }
            PixelFormat::R16F => {
                let v = f16::from_le_bytes([b[o], b[o + 1]]).to_f32();
                [v, v, v, 1.0]
            }
        }
    }

    /// Write linear `[r, g, b, a]` f32 into pixel `(x, y)`, encoding per
    /// [`PixelBuf::format`]. 8-bit lanes clamp to `[0, 1]` and round; `f16`
    /// rounds to half. See [`PixelBuf::get_rgba_f32`] re: no transfer function.
    #[inline]
    pub fn set_rgba_f32(&mut self, x: u32, y: u32, rgba: [f32; 4]) {
        let o = self.offset(x, y);
        PixelBuf::encode_pixel(self.format, &mut self.bytes[o..], rgba);
    }

    /// Encode a linear `[r, g, b, a]` f32 into the first
    /// `format.bytes_per_pixel()` bytes of `dst` (the row-slice writer used by
    /// [`PixelBuf::par_fill_rows`]). Same encoding rules as
    /// [`PixelBuf::set_rgba_f32`].
    #[inline]
    pub fn encode_pixel(format: PixelFormat, dst: &mut [u8], rgba: [f32; 4]) {
        match format {
            PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => {
                for (d, &v) in dst.iter_mut().zip(rgba.iter()) {
                    *d = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
            PixelFormat::Rgba16F => {
                for (lane, &v) in dst.chunks_exact_mut(2).zip(rgba.iter()) {
                    lane.copy_from_slice(&f16::from_f32(v).to_le_bytes());
                }
            }
            PixelFormat::Rgba32F => {
                for (lane, &v) in dst.chunks_exact_mut(4).zip(rgba.iter()) {
                    lane.copy_from_slice(&v.to_le_bytes());
                }
            }
            PixelFormat::R16F => {
                dst[0..2].copy_from_slice(&f16::from_f32(rgba[0]).to_le_bytes());
            }
        }
    }

    /// Fill the buffer row-by-row in parallel (rayon): `f(y, row_bytes)` writes
    /// the `stride`-length byte slice for row `y`. The rayon "tile map" behind
    /// the CPU backend (task A14).
    pub fn par_fill_rows<F>(&mut self, f: F)
    where
        F: Fn(u32, &mut [u8]) + Sync,
    {
        use rayon::prelude::*;
        let stride = self.stride as usize;
        if stride == 0 {
            return;
        }
        self.bytes
            .par_chunks_mut(stride)
            .enumerate()
            .for_each(|(y, row)| f(y as u32, row));
    }
}

/// Pixel encodings the engine ingests (upload depths) and emits (working /
/// display / weight formats).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8-bit RGBA, non-sRGB-aware values.
    Rgba8Unorm,
    /// 8-bit RGBA, sRGB-encoded values.
    Rgba8Srgb,
    /// 16-bit-float RGBA, the working format (§4.2).
    Rgba16F,
    /// 32-bit-float RGBA, the precision escape hatch (§4.2).
    Rgba32F,
    /// 16-bit-float single-channel mask weight (§4.5).
    R16F,
}

impl PixelFormat {
    /// Bytes per pixel for this encoding.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => 4,
            PixelFormat::Rgba16F => 8,
            PixelFormat::Rgba32F => 16,
            PixelFormat::R16F => 2,
        }
    }
}
