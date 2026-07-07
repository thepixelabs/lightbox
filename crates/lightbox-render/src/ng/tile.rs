// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The tile currency — `TileHandle` / `TileView` / `CpuTileView` / `PixelBuf`.
//!
//! **SCAFFOLD-FROZEN SEAM.** This is the shared vocabulary that `node`, `exec`,
//! `cache`, `gpu`, and `source` all speak; re-homing or reshaping it needs a
//! deviation entry (it is the counterpart to the [`crate::ng::exec::backend`]
//! `trait Backend` seam — Risk R8). Field internals of the opaque
//! [`TileHandle`] are filled by **A-gpu** (the `gpu::TilePool`).

use std::sync::Arc;

use crate::ng::types::{Extent, Roi, TilePrecision};

/// A reference-counted handle to a working-format tile living in the
/// [`crate::ng::gpu::TilePool`] (VRAM) or, on the CPU path, a host buffer.
///
/// Cloning shares the tile; the pool reclaims it when the last handle drops
/// (the currency threaded through the `exec`↔`cache`↔`backend` seam).
///
/// **A-gpu filled the internals** (E05-deviations §A-gpu D-tile): a GPU variant
/// wrapping a pooled `wgpu::Texture` (reclaimed by the [`crate::ng::gpu::TilePool`]
/// via strong-count once the last handle drops) or a CPU variant wrapping a host
/// [`PixelBuf`] for the rayon path. An empty handle (`Default`) carries no tile —
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

    /// CPU constructor — the rayon-path tile currency (A-core CPU backend).
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

    /// The host pixels, if this is a CPU tile.
    pub fn cpu_pixels(&self) -> Option<&PixelBuf> {
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

/// An owned CPU pixel buffer — the currency of the `Buffer` render target,
/// the source seam, and CPU readback (spec §3.6/§3.8).
#[derive(Clone)]
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

/// Pixel encodings the engine ingests (upload depths) and emits (working /
/// display / weight formats).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8-bit RGBA, non-sRGB-aware values.
    Rgba8Unorm,
    /// 8-bit RGBA, sRGB-encoded values.
    Rgba8Srgb,
    /// 16-bit-float RGBA — the working format (§4.2).
    Rgba16F,
    /// 32-bit-float RGBA — the precision escape hatch (§4.2).
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
