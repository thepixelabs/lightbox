// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Canvas double-buffer + [`CanvasFrame`] publisher (spec §3.7; task **A13**).
//!
//! Owner: **A-gpu**. The engine renders the display output into an engine-owned
//! **ring of display textures** and publishes a [`CanvasFrame`] — `{generation,
//! texture, quality, extent}` — on a `tokio::sync::watch` channel the shell
//! subscribes to (§2.3 seam 2, same-device zero-copy). The shell samples only
//! **completed generations**: a frame is published *after* its texture is fully
//! written and submitted, and consecutive generations land in **distinct ring
//! slots**, so the shell never observes a torn (half-written / generation-desync)
//! frame while the engine is already producing the next one.
//!
//! This is the mechanism task **B6** wires into [`super::RenderScheduler`]
//! (latest-wins coalescing publishes through here); it is standalone and
//! independently tested so A13 does not depend on the B6 scheduler body.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::ng::engine::OutputQuality;
use crate::ng::error::RenderError;
use crate::ng::exec::gpu::readback_tile;
use crate::ng::tile::{PixelBuf, TileHandle};
use crate::ng::types::{Extent, TilePrecision};

use super::CanvasFrame;

/// Distinct display textures the publisher cycles through. Three keeps the
/// in-flight write slot different from the last two published generations, so a
/// shell that lags by one frame never reads the slot being overwritten.
const RING: usize = 3;

/// The engine-owned display format the canvas composites (`rgba8unorm`; values
/// are already display-encoded by `xform.display`, sampled non-sRGB-aware —
/// matches the E01 seed's compositing contract).
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const CANVAS_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::RENDER_ATTACHMENT);

struct Ring {
    textures: Vec<Arc<wgpu::Texture>>,
    extent: Extent,
}

impl Ring {
    fn new(device: &wgpu::Device, extent: Extent) -> Ring {
        let w = extent.w.max(1);
        let h = extent.h.max(1);
        let textures = (0..RING)
            .map(|_| {
                Arc::new(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("lightbox canvas"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: CANVAS_FORMAT,
                    usage: CANVAS_USAGE,
                    view_formats: &[],
                }))
            })
            .collect();
        Ring {
            textures,
            extent: Extent { w, h },
        }
    }
}

/// Publishes completed canvas frames on a watch channel (spec §3.7; task A13).
pub struct CanvasPublisher {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    generation: AtomicU64,
    ring: Mutex<Ring>,
    tx: watch::Sender<CanvasFrame>,
}

impl CanvasPublisher {
    /// A publisher whose ring is sized to `extent`; returns the publisher and a
    /// receiver seeded with the initial (cleared) generation 0.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        extent: Extent,
    ) -> (CanvasPublisher, watch::Receiver<CanvasFrame>) {
        let ring = Ring::new(&device, extent);
        let initial = CanvasFrame {
            generation: 0,
            texture: ring.textures[0].create_view(&wgpu::TextureViewDescriptor::default()),
            quality: OutputQuality::PreviewTier,
            extent: ring.extent,
        };
        let (tx, rx) = watch::channel(initial);
        (
            CanvasPublisher {
                device,
                queue,
                generation: AtomicU64::new(0),
                ring: Mutex::new(ring),
                tx,
            },
            rx,
        )
    }

    /// Subscribe to completed canvas frames.
    pub fn subscribe(&self) -> watch::Receiver<CanvasFrame> {
        self.tx.subscribe()
    }

    /// The most recently published generation.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Publish `src` (a completed `rgba8unorm` display tile) as the next canvas
    /// generation: copy it into the next ring slot, submit, then send the frame.
    /// Publishing *after* the copy is submitted is what makes the generation
    /// tear-free — the shell only ever sees a fully-written texture.
    pub fn publish(&self, src: &TileHandle, quality: OutputQuality) -> Result<u64, RenderError> {
        let src_tex = src
            .texture()
            .ok_or_else(|| RenderError::Readback("canvas source is not a GPU tile".into()))?;
        let src_extent = src
            .extent()
            .ok_or_else(|| RenderError::Readback("canvas source has no extent".into()))?;

        let mut ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ring.extent != src_extent {
            *ring = Ring::new(&self.device, src_extent);
        }

        let gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let slot = (gen as usize) % RING;
        let dst = Arc::clone(&ring.textures[slot]);
        let extent = ring.extent;
        drop(ring);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("canvas publish"),
            });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: src_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dst,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: extent.w,
                height: extent.h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let frame = CanvasFrame {
            generation: gen,
            texture: dst.create_view(&wgpu::TextureViewDescriptor::default()),
            quality,
            extent,
        };
        // A watch send never fails unless all receivers dropped; treat that as a
        // benign no-op (nothing is listening).
        let _ = self.tx.send(frame);
        Ok(gen)
    }

    /// Reads back the ring slot holding the most recently published generation
    /// (a test/diagnostic hook — the shell composites the view zero-copy). Used
    /// by the A13 tear-free assertion.
    pub fn readback_current(&self) -> Result<(u64, PixelBuf), RenderError> {
        let gen = self.generation();
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = (gen as usize) % RING;
        let texture = Arc::clone(&ring.textures[slot]);
        let extent = ring.extent;
        drop(ring);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let handle = TileHandle::gpu(texture, view, extent, TilePrecision::F16, CANVAS_FORMAT);
        readback_tile(&self.device, &self.queue, &handle).map(|px| (gen, px))
    }
}
