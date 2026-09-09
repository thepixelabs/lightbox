// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D task **D13**, the `J`-key canvas clip-overlay pass:
//! tints highlight-clipped pixels red and shadow-clipped pixels blue over
//! the canvas's published display-space frame (mirrors
//! [`super::histogram::HistogramPass`]'s clip thresholds exactly, so the
//! histogram's clip triangles and this overlay always agree).
//!
//! Unlike [`super::histogram::HistogramPass`] this pass needs **no CPU
//! readback at all**: it is a pure texture→texture GPU compute pass (input
//! view in, a fresh `rgba8unorm` storage-texture view out), so
//! [`ClipOverlayPass::run`] is synchronous and returns the finished view the
//! same call, the shell registers it with egui exactly like the plain
//! canvas frame (`EditorCanvas::swap_displayed`), never touching a pixel on
//! the CPU.
//!
//! `clip_overlay_pixel`/[`clip_overlay_cpu`] are the pure CPU reference this
//! module's own tests hold the GPU kernel to pixel-exactly (D13's "overlays
//! match ClipStats thresholds pixel-exactly on synthetic ramps" AC), the
//! shell has no separate CPU path (the pass requires a GPU device by
//! construction, same as the canvas it overlays).

use std::sync::Arc;

use crate::ng::gpu::{DeviceCtx, KernelBuilder};
use crate::ng::tile::PixelBuf;
use crate::ng::types::Extent;

const CLIP_OVERLAY_WGSL: &str = include_str!("../../../shaders/clip_overlay.wgsl");

/// Highlight-clip tint (opaque red), matches `clip_overlay.wgsl`.
pub const HIGHLIGHT_TINT: [u8; 3] = [255, 0, 0];
/// Shadow-clip tint (opaque blue), matches `clip_overlay.wgsl`.
pub const SHADOW_TINT: [u8; 3] = [0, 0, 255];

/// The CPU reference for one texel (D13's ground truth): highlight-clipped
/// (`r|g|b == 255`) wins over shadow-clipped (`r|g|b == 0`) when a pixel is
/// (degenerately) both, a documented, deterministic tie-break, not an
/// ambiguity. Alpha passes through unchanged.
#[inline]
pub fn clip_overlay_pixel(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    if r == 255 || g == 255 || b == 255 {
        [HIGHLIGHT_TINT[0], HIGHLIGHT_TINT[1], HIGHLIGHT_TINT[2], a]
    } else if r == 0 || g == 0 || b == 0 {
        [SHADOW_TINT[0], SHADOW_TINT[1], SHADOW_TINT[2], a]
    } else {
        [r, g, b, a]
    }
}

/// Applies [`clip_overlay_pixel`] over every texel of a 4-byte-per-pixel
/// `rgba8unorm` [`PixelBuf`] (the CPU reference `ClipOverlayPass::run`'s GPU
/// kernel is tested against, see the module doc).
pub fn clip_overlay_cpu(pixels: &PixelBuf) -> PixelBuf {
    let bpp = pixels.format.bytes_per_pixel() as usize;
    debug_assert_eq!(bpp, 4, "clip_overlay_cpu expects a 4-byte-per-pixel format");
    let mut out = pixels.clone();
    for y in 0..pixels.extent.h {
        let row_start = y as usize * pixels.stride as usize;
        for x in 0..pixels.extent.w {
            let o = row_start + x as usize * bpp;
            let [r, g, b, a] = [
                pixels.bytes[o],
                pixels.bytes[o + 1],
                pixels.bytes[o + 2],
                pixels.bytes[o + 3],
            ];
            let tinted = clip_overlay_pixel(r, g, b, a);
            out.bytes[o..o + 4].copy_from_slice(&tinted);
        }
    }
    out
}

const OVERLAY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const OVERLAY_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::STORAGE_BINDING
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC);

/// D13's GPU clip-overlay pass (see the module doc).
pub struct ClipOverlayPass {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    kernels: KernelBuilder,
    /// The owned output texture, (re)built to match the last-requested
    /// extent (a canvas-sized scratch, not pooled, one instance per canvas).
    output: Option<(wgpu::Texture, Extent)>,
}

impl ClipOverlayPass {
    /// A pass recording on `device`/`queue` (the shell's shared device).
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> ClipOverlayPass {
        let kernels = KernelBuilder::new(&DeviceCtx::new(Arc::clone(&device), Arc::clone(&queue)));
        ClipOverlayPass {
            device,
            queue,
            kernels,
            output: None,
        }
    }

    /// The owned output texture from the last [`ClipOverlayPass::run`] (test/
    /// diagnostic accessor, production callers only need the view `run`
    /// already returns; a readback needs the `Texture` itself as the
    /// `copy_texture_to_buffer` source, which a bare `TextureView` can't
    /// provide).
    pub fn output_texture(&self) -> Option<&wgpu::Texture> {
        self.output.as_ref().map(|(t, _)| t)
    }

    /// Runs the overlay over `src` (a `rgba8unorm` texture view at `extent`
    /// the published canvas frame) and returns the tinted result's view,
    /// synchronously (no CPU readback, see the module doc). `None` only on
    /// a shader/pipeline build failure (surfaced once via `tracing`, never a
    /// panic, the caller falls back to displaying `src` untouched).
    pub fn run(&mut self, src: &wgpu::TextureView, extent: Extent) -> Option<wgpu::TextureView> {
        let pipeline = self
            .kernels
            .compute_pipeline(CLIP_OVERLAY_WGSL, "main")
            .map_err(|err| {
                tracing::warn!(target: "lightbox_render", %err, "clip overlay pipeline build failed");
            })
            .ok()?;

        let needs_rebuild = !matches!(&self.output, Some((_, e)) if *e == extent);
        if needs_rebuild {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lightbox clip overlay"),
                size: wgpu::Extent3d {
                    width: extent.w.max(1),
                    height: extent.h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OVERLAY_FORMAT,
                usage: OVERLAY_USAGE,
                view_formats: &[],
            });
            self.output = Some((tex, extent));
        }
        let (texture, _) = self.output.as_ref().expect("just (re)built above");
        let dst_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let group0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox clip overlay src"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            }],
        });
        let group1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox clip overlay dst"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&dst_view),
            }],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lightbox clip overlay"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("lightbox clip overlay"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group0, &[]);
            pass.set_bind_group(1, &group1, &[]);
            pass.dispatch_workgroups(
                extent.w.max(1).div_ceil(16),
                extent.h.max(1).div_ceil(16),
                1,
            );
        }
        self.queue.submit([encoder.finish()]);

        Some(dst_view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::tile::PixelFormat;

    /// A 4-texel synthetic ramp with known-by-construction clip states:
    /// black (shadow), white (highlight), mid-gray (neither), and pure red
    /// (both, the highlight tie-break).
    fn synthetic_ramp() -> PixelBuf {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Unorm, Extent { w: 4, h: 1 });
        px.set_rgba_f32(0, 0, [0.0, 0.0, 0.0, 1.0]);
        px.set_rgba_f32(1, 0, [1.0, 1.0, 1.0, 1.0]);
        px.set_rgba_f32(2, 0, [0.5, 0.5, 0.5, 1.0]);
        px.set_rgba_f32(3, 0, [1.0, 0.0, 0.0, 1.0]);
        px
    }

    #[test]
    fn cpu_reference_tints_clipped_pixels_and_passes_through_the_rest() {
        let out = clip_overlay_cpu(&synthetic_ramp());
        let px = |x: u32| -> [u8; 4] {
            let o = x as usize * 4;
            [
                out.bytes[o],
                out.bytes[o + 1],
                out.bytes[o + 2],
                out.bytes[o + 3],
            ]
        };
        assert_eq!(px(0), [0, 0, 255, 255], "black -> shadow tint (blue)");
        assert_eq!(px(1), [255, 0, 0, 255], "white -> highlight tint (red)");
        assert_eq!(px(2), [128, 128, 128, 255], "mid gray passes through");
        assert_eq!(
            px(3),
            [255, 0, 0, 255],
            "pure red: highlight wins the tie-break"
        );
    }

    #[test]
    fn clip_overlay_pixel_matches_the_documented_tie_break() {
        // Simultaneously highlight AND shadow clipped on different channels
        // (r=255, g=0): highlight wins.
        assert_eq!(clip_overlay_pixel(255, 0, 128, 255), [255, 0, 0, 255]);
    }
}
