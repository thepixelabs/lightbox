// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Output texture pool — recycles engine output textures by size (spec T6).
//!
//! Ownership protocol: the pool retains one `Arc` per texture it ever created.
//! A texture whose strong count has dropped back to 1 (only the pool holds it)
//! is idle and may be handed out again for a matching size. Holders (ticket
//! outputs, the shell's currently-displayed frame) keep their `Arc` alive for
//! as long as the pixels must survive — recycling can never yank a texture
//! that is still referenced.

use std::sync::{Arc, Mutex};

/// Format of every engine output texture at M0.
///
/// `Rgba8Unorm` (NOT `-Srgb`): the pixel *values* are sRGB-encoded by the
/// nodes themselves (the display-transform algorithm ends with an explicit
/// OETF encode), and egui's user-texture path samples textures as "normal"
/// non-sRGB-aware gamma data. A storage-capable format also keeps the door
/// open for compute-shader nodes writing output directly (E01 Phase 6).
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Usage of every engine output texture at M0: rendered or compute-written by
/// nodes, sampled by egui for zero-copy compositing, copyable for the
/// `RenderTarget::CpuBuffer` (CLI/tests) path. NEVER mapped in the frame path.
pub const OUTPUT_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::STORAGE_BINDING)
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC);

/// Keep at most this many idle textures pooled; older idle ones are released.
const MAX_IDLE: usize = 8;

/// Recycling pool for engine output textures (spec T6).
pub struct TexturePool {
    device: Arc<wgpu::Device>,
    /// Every live pool texture, most recently created/reused last.
    textures: Mutex<Vec<Arc<wgpu::Texture>>>,
}

impl TexturePool {
    /// A pool creating textures on `device`.
    pub fn new(device: Arc<wgpu::Device>) -> TexturePool {
        TexturePool {
            device,
            textures: Mutex::new(Vec::new()),
        }
    }

    /// Hands out a `size`d output texture, reusing an idle one when possible.
    ///
    /// The returned `Arc` keeps the texture out of reuse until dropped.
    pub fn acquire(&self, size: [u32; 2]) -> Arc<wgpu::Texture> {
        let [w, h] = [size[0].max(1), size[1].max(1)];
        let mut textures = self.textures.lock().expect("texture pool lock poisoned");

        // Reuse: idle (only the pool holds it) and exactly the right size.
        // The strong-count check is race-free because every other holder came
        // FROM this pool under this same lock.
        if let Some(tex) = textures
            .iter()
            .find(|t| Arc::strong_count(t) == 1 && t.width() == w && t.height() == h)
        {
            return tex.clone();
        }

        // Cap idle inventory (stale sizes after a resize storm).
        let mut idle_seen = 0;
        textures.retain(|t| {
            if Arc::strong_count(t) == 1 {
                idle_seen += 1;
                idle_seen <= MAX_IDLE
            } else {
                true
            }
        });

        let tex = Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lightbox engine output"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: OUTPUT_USAGE,
            view_formats: &[],
        }));
        textures.push(tex.clone());
        tex
    }

    /// Number of textures currently owned by the pool (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.textures
            .lock()
            .expect("texture pool lock poisoned")
            .len()
    }

    /// True when the pool has created no textures yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
