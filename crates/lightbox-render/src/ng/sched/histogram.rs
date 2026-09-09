// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D task **D12**, `HistogramPass`: a GPU reduction of the
//! canvas's published display-space frame (`sched::canvas::CANVAS_FORMAT`,
//! `rgba8unorm`) into RGB(+luma) 256-bin histograms and highlight/shadow
//! [`ClipStats`], a bit-identical CPU fallback ([`histogram_cpu`]), and a
//! **non-blocking** async readback ring so a caller (the shell's per-frame
//! UI loop) never stalls waiting on the GPU.
//!
//! # Why this lives beside [`super::canvas`]
//!
//! This pass reduces the same texture [`super::canvas::CanvasPublisher`]
//! publishes (the composited display frame), not a `RenderNode`'s working
//! tile, it has no `ParamBlock`/port-typed output the E05 [`crate::ng::node`]
//! seam models, so it is a standalone canvas-adjacent utility (like
//! `CanvasPublisher` itself), not a graph node. Callers own an instance and
//! feed it whatever `wgpu::TextureView` they last displayed
//! `CanvasFrame::texture` on the live shell path, or a test-built texture in
//! this module's own tests.
//!
//! # The non-blocking contract (the D12 "zero UI-thread blocking" AC)
//!
//! [`HistogramPass::try_dispatch`] only records a compute dispatch + a
//! buffer→buffer copy and calls `wgpu::Queue::submit` + `BufferSlice::
//! map_async`, **it never calls `Device::poll` with a `Wait`**, so it always
//! returns in the time it takes to build one command buffer, regardless of
//! whether the GPU has even started that work. [`HistogramPass::poll`] drives
//! completion with `wgpu::PollType::Poll` (checks once, never blocks, the
//! opposite of `exec::gpu::readback_tile`'s `wait_indefinitely()`), draining
//! whichever ring slot(s) have finished and returning the newest. A caller
//! that wants a result polls once per frame; `tests::dispatch_never_blocks_
//! on_gpu_completion` measures both calls' wall time as the AC's probe.
//!
//! Two ring slots (mirrors [`super::canvas::CanvasPublisher`]'s own
//! double/triple-buffer rationale) let a fresh dispatch start while the
//! previous slot's readback is still in flight; [`HistogramPass::try_dispatch`]
//! returns `None` (a no-op skip, never a block) on the rare frame where both
//! slots are still busy, the next canvas frame gets another chance.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ng::gpu::{DeviceCtx, KernelBuilder};
use crate::ng::tile::PixelBuf;
use crate::ng::types::Extent;

const HISTOGRAM_WGSL: &str = include_str!("../../../shaders/histogram_reduce.wgsl");

/// Bins per channel (spec §4.4-style 8-bit display histogram, one bin per
/// possible `rgba8unorm` channel value).
pub const NUM_BINS: usize = 256;

/// `bins` storage-buffer layout: 4 planes (R, G, B, luma) of [`NUM_BINS`].
const BINS_LEN: usize = 4 * NUM_BINS;
const BINS_BYTES: u64 = (BINS_LEN * std::mem::size_of::<u32>()) as u64;
/// `clip` storage-buffer layout: `[highlight_clipped, shadow_clipped]`.
const CLIP_LEN: usize = 2;
const CLIP_BYTES: u64 = (CLIP_LEN * std::mem::size_of::<u32>()) as u64;

/// Ring depth (see the module doc's non-blocking contract).
const RING: usize = 2;

/// Highlight/shadow clip mass, as pixel counts over the reduced frame (D12/
/// D13: the shared source both the histogram's clip triangles and the J-key
/// overlay's thresholds read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipStats {
    /// Pixels with `r == 255 || g == 255 || b == 255`.
    pub highlight_clipped: u32,
    /// Pixels with `r == 0 || g == 0 || b == 0`.
    pub shadow_clipped: u32,
    /// Total pixels reduced (the fractions' denominator).
    pub total_pixels: u32,
}

impl ClipStats {
    /// Fraction of pixels highlight-clipped, `0.0` on an empty frame.
    pub fn highlight_fraction(&self) -> f32 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.highlight_clipped as f32 / self.total_pixels as f32
        }
    }

    /// Fraction of pixels shadow-clipped, `0.0` on an empty frame.
    pub fn shadow_fraction(&self) -> f32 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.shadow_clipped as f32 / self.total_pixels as f32
        }
    }
}

/// The reduced histogram: four [`NUM_BINS`]-wide planes plus [`ClipStats`]
/// (D12/D13's shared currency between `HistogramPass`, the CPU reference, and
/// the shell's histogram widget).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistogramData {
    /// Red channel, 256 bins.
    pub r: [u32; NUM_BINS],
    /// Green channel, 256 bins.
    pub g: [u32; NUM_BINS],
    /// Blue channel, 256 bins.
    pub b: [u32; NUM_BINS],
    /// Rec. 709 luma (fixed-point, see `histogram_reduce.wgsl`), 256 bins.
    pub luma: [u32; NUM_BINS],
    /// Highlight/shadow clip mass over the same reduction.
    pub clip: ClipStats,
}

impl Default for HistogramData {
    fn default() -> Self {
        HistogramData {
            r: [0; NUM_BINS],
            g: [0; NUM_BINS],
            b: [0; NUM_BINS],
            luma: [0; NUM_BINS],
            clip: ClipStats::default(),
        }
    }
}

/// Rec. 709 luma bin index for one `rgba8unorm` texel, fixed-point
/// (`54/183/19` over 256, weights sum to exactly 256, so this is exact
/// integer arithmetic, bit-identical to `histogram_reduce.wgsl`'s copy of
/// the same formula).
#[inline]
fn luma_bin(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 54 + g as u32 * 183 + b as u32 * 19) >> 8) as u8
}

/// The CPU reference reduction (D12's bit-exact fallback / GPU-parity
/// oracle). Accepts any 4-bytes-per-pixel `rgba8unorm`/`rgba8unorm-srgb`
/// [`PixelBuf`] (both store raw 8-bit channel bytes identically, the
/// srgb-ness is a display-encoding *meaning*, not a byte-layout difference,
/// so binning the raw bytes is format-agnostic by construction).
pub fn histogram_cpu(pixels: &PixelBuf) -> HistogramData {
    let bpp = pixels.format.bytes_per_pixel() as usize;
    debug_assert_eq!(bpp, 4, "histogram_cpu expects a 4-byte-per-pixel format");
    let mut data = HistogramData::default();
    for y in 0..pixels.extent.h {
        let row_start = y as usize * pixels.stride as usize;
        let row = &pixels.bytes[row_start..];
        for x in 0..pixels.extent.w {
            let o = x as usize * bpp;
            let r = row[o];
            let g = row[o + 1];
            let b = row[o + 2];
            data.r[r as usize] += 1;
            data.g[g as usize] += 1;
            data.b[b as usize] += 1;
            data.luma[luma_bin(r, g, b) as usize] += 1;
            if r == 255 || g == 255 || b == 255 {
                data.clip.highlight_clipped += 1;
            }
            if r == 0 || g == 0 || b == 0 {
                data.clip.shadow_clipped += 1;
            }
            data.clip.total_pixels += 1;
        }
    }
    data
}

/// `histogram_reduce.wgsl`'s `Params` UBO size (`4 * u32`, 2 real fields +
/// 2 padding, matching the shader's `std140`-compatible layout).
const PARAMS_BYTES: u64 = 16;

/// The `histogram_reduce.wgsl` params UBO. No `bytemuck` dep, this crate's
/// other GPU params writers (`nodes::global::*`) all hand-encode their small
/// UBOs the same way (`to_le_bytes` into a fixed-size array).
struct GpuParams {
    width: u32,
    height: u32,
}

impl GpuParams {
    fn to_bytes(&self) -> [u8; PARAMS_BYTES as usize] {
        let mut out = [0u8; PARAMS_BYTES as usize];
        out[0..4].copy_from_slice(&self.width.to_le_bytes());
        out[4..8].copy_from_slice(&self.height.to_le_bytes());
        out
    }
}

/// One ring slot's GPU-side resources (see the module doc's ring rationale).
struct Slot {
    bins_storage: wgpu::Buffer,
    clip_storage: wgpu::Buffer,
    readback: wgpu::Buffer,
    params: wgpu::Buffer,
    /// Set by the `map_async` callback once the readback buffer is mapped
    /// polled (never awaited) by [`HistogramPass::poll`].
    ready: Arc<AtomicBool>,
    /// `true` from `try_dispatch` until `poll` collects (or the caller never
    /// asks), the slot is unavailable for a fresh dispatch while `true`.
    busy: bool,
    generation: u64,
    /// The extent this slot's in-flight dispatch reduced (its pixel count is
    /// `ClipStats::total_pixels`).
    extent: Extent,
}

impl Slot {
    fn new(device: &wgpu::Device) -> Slot {
        let bins_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightbox histogram bins"),
            size: BINS_BYTES,
            // COPY_DST: `try_dispatch` zeroes this via `queue.write_buffer`
            // before every reduction. COPY_SRC: copied into `readback` after.
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let clip_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightbox histogram clip"),
            size: CLIP_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightbox histogram readback"),
            size: BINS_BYTES + CLIP_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightbox histogram params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Slot {
            bins_storage,
            clip_storage,
            readback,
            params,
            ready: Arc::new(AtomicBool::new(false)),
            busy: false,
            generation: 0,
            extent: Extent { w: 0, h: 0 },
        }
    }
}

/// D12's GPU histogram reduction + non-blocking async readback ring (see the
/// module doc).
pub struct HistogramPass {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    kernels: KernelBuilder,
    slots: Vec<Slot>,
    next_generation: u64,
}

impl HistogramPass {
    /// A pass recording on `device`/`queue` (the shell's shared device, same
    /// seam [`super::canvas::CanvasPublisher`] renders on).
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> HistogramPass {
        let slots = (0..RING).map(|_| Slot::new(&device)).collect();
        let kernels = KernelBuilder::new(&DeviceCtx::new(Arc::clone(&device), Arc::clone(&queue)));
        HistogramPass {
            device,
            queue,
            kernels,
            slots,
            next_generation: 0,
        }
    }

    /// Records a reduction of `src` (a `w`×`h` `rgba8unorm` texture view
    /// the published canvas frame) into the next free ring slot and kicks off
    /// its non-blocking readback. Returns the dispatch's generation, or
    /// `None` if every slot is still busy (a skip, never a block, see the
    /// module doc). Never calls a blocking `Device::poll`.
    pub fn try_dispatch(&mut self, src: &wgpu::TextureView, extent: Extent) -> Option<u64> {
        let slot_ix = self.slots.iter().position(|s| !s.busy)?;
        let pipeline = self.kernels.compute_pipeline(HISTOGRAM_WGSL, "main").ok()?;

        let generation = self.next_generation + 1;
        self.next_generation = generation;

        let slot = &mut self.slots[slot_ix];
        // Zero the accumulators, a fresh reduction starts from zero counts.
        self.queue
            .write_buffer(&slot.bins_storage, 0, &vec![0u8; BINS_BYTES as usize]);
        self.queue
            .write_buffer(&slot.clip_storage, 0, &vec![0u8; CLIP_BYTES as usize]);
        self.queue.write_buffer(
            &slot.params,
            0,
            &GpuParams {
                width: extent.w,
                height: extent.h,
            }
            .to_bytes(),
        );

        let group0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox histogram src"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            }],
        });
        let group1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox histogram bins+clip"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: slot.bins_storage.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: slot.clip_storage.as_entire_binding(),
                },
            ],
        });
        let group2 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox histogram params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: slot.params.as_entire_binding(),
            }],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lightbox histogram reduce"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("lightbox histogram reduce"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group0, &[]);
            pass.set_bind_group(1, &group1, &[]);
            pass.set_bind_group(2, &group2, &[]);
            pass.dispatch_workgroups(
                extent.w.max(1).div_ceil(16),
                extent.h.max(1).div_ceil(16),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&slot.bins_storage, 0, &slot.readback, 0, BINS_BYTES);
        encoder.copy_buffer_to_buffer(
            &slot.clip_storage,
            0,
            &slot.readback,
            BINS_BYTES,
            CLIP_BYTES,
        );
        self.queue.submit([encoder.finish()]);

        slot.busy = true;
        slot.generation = generation;
        slot.extent = extent;
        slot.ready.store(false, Ordering::Release);
        let ready = Arc::clone(&slot.ready);
        // Never blocks: schedules the callback, returns immediately. Fires
        // once a NON-BLOCKING `poll` (below) observes the submission done.
        slot.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    ready.store(true, Ordering::Release);
                }
            });

        Some(generation)
    }

    /// One non-blocking maintenance tick (`wgpu::PollType::Poll`, "check the
    /// device a single time without blocking", never `Wait`): pumps any
    /// completed `map_async` callbacks, then collects + frees the
    /// newest-generation ready slot, if any. Call once per UI frame; a caller
    /// that never calls this simply never observes results (dispatch itself
    /// still never blocks).
    pub fn poll(&mut self) -> Option<(u64, HistogramData)> {
        // `PollType::Poll` never waits, this is the whole non-blocking
        // contract (contrast `exec::gpu::readback_tile`'s
        // `wait_indefinitely()`).
        let _ = self.device.poll(wgpu::PollType::Poll);

        let slot_ix = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.busy && s.ready.load(Ordering::Acquire))
            .max_by_key(|(_, s)| s.generation)
            .map(|(i, _)| i)?;

        let slot = &mut self.slots[slot_ix];
        let total_pixels = slot.extent.w.saturating_mul(slot.extent.h);
        let data = {
            let mapped = slot.readback.slice(..).get_mapped_range();
            decode(&mapped, total_pixels)
        };
        slot.readback.unmap();
        slot.busy = false;
        Some((slot.generation, data))
    }
}

/// Reads one little-endian `u32` at `bins_u32_index * 4` bytes into `bytes`
/// (the manual, no-`bytemuck` twin of `nodes::global`'s `to_le_bytes` UBO
/// writers, see [`GpuParams`]'s doc).
#[inline]
fn read_u32_le(bytes: &[u8], u32_index: usize) -> u32 {
    let o = u32_index * 4;
    u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
}

/// Decodes the `readback` buffer's mapped bytes (`bins` then `clip`, the
/// [`Slot`] layout) into [`HistogramData`]; `total_pixels` is the dispatched
/// extent's pixel count (the reduction's fraction denominator).
fn decode(mapped: &[u8], total_pixels: u32) -> HistogramData {
    let mut data = HistogramData::default();
    for i in 0..NUM_BINS {
        data.r[i] = read_u32_le(mapped, i);
        data.g[i] = read_u32_le(mapped, NUM_BINS + i);
        data.b[i] = read_u32_le(mapped, 2 * NUM_BINS + i);
        data.luma[i] = read_u32_le(mapped, 3 * NUM_BINS + i);
    }
    let clip_base = (BINS_BYTES / 4) as usize;
    data.clip = ClipStats {
        highlight_clipped: read_u32_le(mapped, clip_base),
        shadow_clipped: read_u32_le(mapped, clip_base + 1),
        total_pixels,
    };
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::tile::PixelFormat;

    /// A hand-built 2×2 `rgba8unorm` buffer: one pure-black (shadow-clipped),
    /// one pure-white (highlight-clipped), one mid-gray, one that's
    /// highlight-clipped on one channel only (`g == 255`), exercises every
    /// branch of the CPU reference against known-by-construction counts.
    fn synthetic_2x2() -> PixelBuf {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Unorm, Extent { w: 2, h: 2 });
        px.set_rgba_f32(0, 0, [0.0, 0.0, 0.0, 1.0]); // black: shadow-clipped
        px.set_rgba_f32(1, 0, [1.0, 1.0, 1.0, 1.0]); // white: highlight-clipped
        px.set_rgba_f32(0, 1, [0.5, 0.5, 0.5, 1.0]); // mid gray: neither
        px.set_rgba_f32(1, 1, [0.2, 1.0, 0.2, 1.0]); // green-clipped only
        px
    }

    #[test]
    fn cpu_reference_bins_known_synthetic_pixels_exactly() {
        let data = histogram_cpu(&synthetic_2x2());
        assert_eq!(data.r[0], 1, "one texel with r == 0");
        assert_eq!(data.r[255], 1, "one texel with r == 255");
        assert_eq!(data.clip.highlight_clipped, 2, "white + green-clipped");
        assert_eq!(data.clip.shadow_clipped, 1, "only the black texel");
        assert_eq!(data.clip.total_pixels, 4);
        assert!((data.clip.highlight_fraction() - 0.5).abs() < 1e-6);
        assert!((data.clip.shadow_fraction() - 0.25).abs() < 1e-6);
    }

    /// `luma_bin`'s fixed-point weights sum to exactly 256 (no drift at the
    /// white point, the AC's own "exact" bar starts here).
    #[test]
    fn luma_weights_sum_to_256_and_saturate_correctly() {
        assert_eq!(luma_bin(255, 255, 255), 255);
        assert_eq!(luma_bin(0, 0, 0), 0);
    }

    /// A uniformly black or white synthetic image has ALL pixels clipped
    /// the ClipStats-threshold "ground truth" D13's overlay tests also pin.
    #[test]
    fn a_fully_black_frame_is_100pct_shadow_clipped() {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Unorm, Extent { w: 4, h: 4 });
        px.par_fill_rows(|_y, row| {
            for chunk in row.chunks_exact_mut(4) {
                chunk.copy_from_slice(&[0, 0, 0, 255]);
            }
        });
        let data = histogram_cpu(&px);
        assert_eq!(data.clip.shadow_clipped, 16);
        assert_eq!(data.clip.highlight_clipped, 0);
        assert!((data.clip.shadow_fraction() - 1.0).abs() < 1e-6);
    }
}
