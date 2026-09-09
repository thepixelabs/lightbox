// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! GPU backend, encoder management, compute dispatch, readback (spec §2;
//! tasks **A9** GPU executor v1, C tiling, E15 readback).
//!
//! Owner: **A-gpu**. Implements the frozen [`Backend`] seam.
//!
//! A9 is **whole-ROI-as-one-tile**: the executor hands one node + its input
//! tiles in, the backend acquires the node's output tile from its
//! [`TilePool`], builds a [`GpuEvalCtx`] around it, records the node's compute
//! dispatch onto the shared queue, and hands the finished tile back. Tiling
//! (256², visible-first, apron) layers on top in phase C without changing this
//! seam.

use std::sync::Arc;

use crate::ng::cache::Bytes;
use crate::ng::engine::BackendId;
use crate::ng::error::{NodeError, RenderError};
use crate::ng::exec::backend::{Backend, BackendEvalRequest};
use crate::ng::gpu::{working_format, DeviceCtx, KernelBuilder, TilePool};
use crate::ng::node::GpuEvalCtx;
use crate::ng::tile::{PixelBuf, PixelFormat, TileHandle, TileView};
use crate::ng::types::{Extent, PortType, Roi, TilePrecision};

/// Default GPU tile-pool budget when the backend is built without an explicit
/// one (1 GiB), the engine passes a probed [`crate::ng::VramBudget`] in
/// production.
const DEFAULT_BUDGET: u64 = 1 << 30;

/// The GPU pixel backend: records compute dispatches for each node onto the
/// shared queue and reads back on the `Buffer` path.
pub struct GpuBackend {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    kernels: KernelBuilder,
    pool: TilePool,
}

impl GpuBackend {
    /// A GPU backend on `device` with the default tile-pool budget.
    pub fn new(device: &DeviceCtx) -> GpuBackend {
        GpuBackend::with_budget(device, Bytes(DEFAULT_BUDGET))
    }

    /// A GPU backend on `device` with an explicit tile-pool `budget`.
    pub fn with_budget(device: &DeviceCtx, budget: Bytes) -> GpuBackend {
        GpuBackend {
            device: Arc::clone(&device.device),
            queue: Arc::clone(&device.queue),
            kernels: KernelBuilder::new(device),
            pool: TilePool::new(Arc::clone(&device.device), budget),
        }
    }

    /// The backend's tile pool (shared with the cache/executor at integration).
    pub fn pool(&self) -> &TilePool {
        &self.pool
    }

    /// The backend's pipeline cache.
    pub fn kernels(&self) -> &KernelBuilder {
        &self.kernels
    }

    /// The wgpu format + precision tag a node's output port maps to.
    fn output_target(
        port: PortType,
        requested: TilePrecision,
    ) -> Result<(wgpu::TextureFormat, TilePrecision), NodeError> {
        match port {
            PortType::LinearRgbaF16 | PortType::LinearRgbaF32 => {
                Ok((working_format(requested), requested))
            }
            PortType::DisplayRgba8 => Ok((wgpu::TextureFormat::Rgba8Unorm, requested)),
            PortType::WeightR16 => Ok((wgpu::TextureFormat::R16Float, requested)),
            PortType::MosaicU16 => Err(NodeError::Other(
                "MosaicU16 has no v1 engine producer (reserved for E11)".into(),
            )),
        }
    }
}

impl Backend for GpuBackend {
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError> {
        let node = req.node;
        let node_id = node.descriptor().id;
        let node_err = |e: NodeError| RenderError::Node {
            node: node_id,
            source: e,
        };

        if req.cancel.is_cancelled() {
            return Err(RenderError::Cancelled);
        }

        // Read-only views for the node (built from each input's real extent
        // the actual GPU texture bound; distinct from `req.target_extent`,
        // this node's own *output* sizing, task E11, see
        // `ng::exec::Executor::evaluate`'s doc comment).
        let mut input_views: Vec<TileView<'_>> = Vec::with_capacity(req.inputs.len());
        for h in req.inputs {
            let view = h
                .texture_view()
                .ok_or_else(|| node_err(NodeError::Gpu("input tile is not GPU-resident".into())))?;
            let ext = h.extent().unwrap_or(Extent { w: 1, h: 1 });
            input_views.push(TileView {
                view,
                precision: h.precision().unwrap_or(TilePrecision::F16),
                roi: Roi {
                    x: 0,
                    y: 0,
                    w: ext.w,
                    h: ext.h,
                },
            });
        }

        // Size + acquire the output tile from the node's output port type, at
        // the executor's propagated target extent (task E11, see
        // `ng::exec::Executor::evaluate`'s doc comment).
        let out_extent = req.target_extent;
        let (format, precision) =
            GpuBackend::output_target(node.descriptor().output.ty, req.precision)
                .map_err(node_err)?;
        let output = self.pool.acquire_format(out_extent, format, precision);

        // Record the node's dispatch into a fresh eval context.
        let mut ctx = GpuEvalCtx::new(
            &self.device,
            &self.queue,
            &self.kernels,
            req.scale,
            req.cancel,
            output,
        );
        node.eval_gpu(&mut ctx, &input_views, req.params)
            .map_err(node_err)?;
        Ok(ctx.into_output())
    }

    fn kind(&self) -> BackendId {
        BackendId::Gpu
    }
}

/// Copies a GPU tile back to a host [`PixelBuf`] (the `Buffer` render target /
/// export / preview-build / test path). Un-pads the 256-byte-aligned copy rows
/// into a tightly packed buffer.
pub fn readback_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tile: &TileHandle,
) -> Result<PixelBuf, RenderError> {
    let texture = tile
        .texture()
        .ok_or_else(|| RenderError::Readback("tile is not GPU-resident".into()))?;
    let extent = tile
        .extent()
        .ok_or_else(|| RenderError::Readback("tile has no extent".into()))?;
    let format = tile
        .format()
        .ok_or_else(|| RenderError::Readback("tile has no format".into()))?;
    let (pixel_format, bpp) = readback_format(format)
        .ok_or_else(|| RenderError::Readback(format!("unsupported readback format {format:?}")))?;

    let w = extent.w.max(1);
    let h = extent.h.max(1);
    let unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lightbox readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lightbox readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Readback(format!("device poll failed: {e}")))?;
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(RenderError::Readback(format!("buffer map failed: {e}"))),
        Err(e) => return Err(RenderError::Readback(format!("map channel closed: {e}"))),
    }

    let mapped = slice.get_mapped_range();
    let mut bytes = vec![0u8; (unpadded as usize) * (h as usize)];
    for y in 0..h as usize {
        let src = &mapped[y * padded as usize..y * padded as usize + unpadded as usize];
        bytes[y * unpadded as usize..(y + 1) * unpadded as usize].copy_from_slice(src);
    }
    drop(mapped);
    buffer.unmap();

    Ok(PixelBuf {
        bytes,
        format: pixel_format,
        extent: Extent { w, h },
        stride: unpadded,
    })
}

fn readback_format(format: wgpu::TextureFormat) -> Option<(PixelFormat, u32)> {
    match format {
        wgpu::TextureFormat::Rgba16Float => Some((PixelFormat::Rgba16F, 8)),
        wgpu::TextureFormat::Rgba32Float => Some((PixelFormat::Rgba32F, 16)),
        wgpu::TextureFormat::Rgba8Unorm => Some((PixelFormat::Rgba8Unorm, 4)),
        wgpu::TextureFormat::Rgba8UnormSrgb => Some((PixelFormat::Rgba8Srgb, 4)),
        wgpu::TextureFormat::R16Float => Some((PixelFormat::R16F, 2)),
        _ => None,
    }
}
