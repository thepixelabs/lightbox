// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D tasks **D12**/**D13**, `HistogramPass` GPU/CPU exact-bin
//! parity, the non-blocking-readback probe, and `ClipOverlayPass`
//! GPU/CPU pixel-exact parity on synthetic ramps.
//!
//! Mirrors `tests/e10_wb.rs`'s `device_or_skip` posture: every GPU
//! assertion here degrades to a printed skip (never a failure) on a machine
//! with no adapter, the authoritative run is CI/main, which `gpu_context.rs`
//! documents as always providing one (Metal / DX12 WARP / Vulkan lavapipe).

use std::time::{Duration, Instant};

use lightbox_render::ng::{
    clip_overlay_cpu, histogram_cpu, ClipOverlayPass, Extent, HistogramData, HistogramPass,
    PixelBuf, PixelFormat,
};
use lightbox_render::GpuContext;

fn device_or_skip(name: &str) -> Option<GpuContext> {
    match GpuContext::headless() {
        Some(c) => Some(c),
        None => {
            eprintln!(
                "[{name}] no wgpu adapter available — SKIPPED (authoritative run is on main)"
            );
            None
        }
    }
}

/// A synthetic `rgba8unorm` test image exercising every histogram/clip-
/// overlay branch: a horizontal ramp (0..=255 in steps, so every bin gets
/// SOME mass) whose first column is pure black (shadow-clipped), last
/// column pure white (highlight-clipped), plus one row of saturated-channel
/// pixels (highlight-clipped on exactly one channel each).
fn synthetic_ramp(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Unorm, Extent { w, h });
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let bpp = 4usize;
            let o = x as usize * bpp;
            let v = ((x * 255) / w.max(1).saturating_sub(1).max(1)) as u8;
            let rgba: [u8; 4] = if y == h.saturating_sub(1) && h > 1 {
                // A saturated-single-channel row (never fully white/black).
                match x % 3 {
                    0 => [255, 40, 40, 255],
                    1 => [40, 255, 40, 255],
                    _ => [40, 40, 255, 255],
                }
            } else {
                [v, v, v, 255]
            };
            row[o..o + 4].copy_from_slice(&rgba);
        }
    });
    px
}

/// Uploads `pixels` (`rgba8unorm`) as a `TEXTURE_BINDING | COPY_DST` GPU
/// texture and returns it + its view.
fn upload_texture(ctx: &GpuContext, pixels: &PixelBuf) -> (wgpu::Texture, wgpu::TextureView) {
    let extent = pixels.extent;
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("e10_histogram test source"),
        size: wgpu::Extent3d {
            width: extent.w,
            height: extent.h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels.bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(pixels.stride),
            rows_per_image: Some(extent.h),
        },
        wgpu::Extent3d {
            width: extent.w,
            height: extent.h,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Test-only BLOCKING readback of an `rgba8unorm` texture (never used by
/// product code, `HistogramPass`'s own readback is the non-blocking one
/// under test; this is just how the test observes `ClipOverlayPass`'s
/// GPU-resident output for comparison).
fn readback_rgba8(ctx: &GpuContext, texture: &wgpu::Texture, extent: Extent) -> PixelBuf {
    let unpadded = extent.w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("e10_histogram test readback"),
        size: u64::from(padded) * u64::from(extent.h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
                rows_per_image: Some(extent.h),
            },
        },
        wgpu::Extent3d {
            width: extent.w,
            height: extent.h,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv().expect("map channel").expect("map succeeded");
    let mapped = slice.get_mapped_range();
    let mut bytes = vec![0u8; (unpadded as usize) * (extent.h as usize)];
    for y in 0..extent.h as usize {
        let src = &mapped[y * padded as usize..y * padded as usize + unpadded as usize];
        bytes[y * unpadded as usize..(y + 1) * unpadded as usize].copy_from_slice(src);
    }
    drop(mapped);
    buffer.unmap();
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba8Unorm,
        extent,
        stride: unpadded,
    }
}

/// Polls `hist` (bounded, never an unbounded/blocking wait) until a result
/// is ready or a generous deadline elapses.
fn poll_until_ready(hist: &mut HistogramPass, deadline: Duration) -> Option<(u64, HistogramData)> {
    let start = Instant::now();
    loop {
        if let Some(result) = hist.poll() {
            return Some(result);
        }
        if start.elapsed() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ── D12: GPU bins == CPU reference, EXACTLY ─────────────────────────────────

#[test]
fn gpu_histogram_bins_match_the_cpu_reference_exactly_on_a_synthetic_ramp() {
    let Some(ctx) = device_or_skip("d12-gpu-cpu-exact") else {
        return;
    };
    let pixels = synthetic_ramp(64, 8);
    let cpu = histogram_cpu(&pixels);

    let (_tex, view) = upload_texture(&ctx, &pixels);
    let mut hist = HistogramPass::new(
        std::sync::Arc::clone(&ctx.device),
        std::sync::Arc::clone(&ctx.queue),
    );
    hist.try_dispatch(&view, pixels.extent)
        .expect("a fresh HistogramPass always has a free ring slot");
    let (_gen, gpu) =
        poll_until_ready(&mut hist, Duration::from_secs(5)).expect("GPU histogram completed");

    assert_eq!(gpu.r, cpu.r, "R bins must match exactly");
    assert_eq!(gpu.g, cpu.g, "G bins must match exactly");
    assert_eq!(gpu.b, cpu.b, "B bins must match exactly");
    assert_eq!(gpu.luma, cpu.luma, "luma bins must match exactly");
    assert_eq!(gpu.clip, cpu.clip, "ClipStats must match exactly");
}

/// Same exactness claim on an all-black / all-white pair, the degenerate
/// corners where every pixel is clipped one way or the other.
#[test]
fn gpu_histogram_matches_cpu_exactly_on_fully_clipped_frames() {
    let Some(ctx) = device_or_skip("d12-gpu-cpu-exact-clipped") else {
        return;
    };
    for (name, rgba) in [
        ("black", [0u8, 0, 0, 255]),
        ("white", [255u8, 255, 255, 255]),
    ] {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Unorm, Extent { w: 16, h: 16 });
        px.par_fill_rows(|_y, row| {
            for chunk in row.chunks_exact_mut(4) {
                chunk.copy_from_slice(&rgba);
            }
        });
        let cpu = histogram_cpu(&px);
        let (_tex, view) = upload_texture(&ctx, &px);
        let mut hist = HistogramPass::new(
            std::sync::Arc::clone(&ctx.device),
            std::sync::Arc::clone(&ctx.queue),
        );
        hist.try_dispatch(&view, px.extent).unwrap();
        let (_gen, gpu) = poll_until_ready(&mut hist, Duration::from_secs(5))
            .unwrap_or_else(|| panic!("[{name}] GPU histogram did not complete"));
        assert_eq!(gpu, cpu, "[{name}] GPU/CPU histogram mismatch");
    }
}

// ── D12: the non-blocking readback probe ────────────────────────────────────

/// **AC probe:** `try_dispatch` returns almost immediately (it only records
/// a command buffer + submits, never waits on the GPU), and each `poll()`
/// call, even the very first, right after dispatch, when the GPU has
/// almost certainly not finished, returns within a few milliseconds
/// (`PollType::Poll`, never `Wait`). This is the literal contrast with
/// `exec::gpu::readback_tile`'s `wait_indefinitely()`, which blocks for
/// however long the GPU submission takes.
#[test]
fn dispatch_and_poll_never_block_on_gpu_completion() {
    let Some(ctx) = device_or_skip("d12-non-blocking-probe") else {
        return;
    };
    let pixels = synthetic_ramp(512, 512); // a larger frame -> real GPU work
    let (_tex, view) = upload_texture(&ctx, &pixels);
    let mut hist = HistogramPass::new(
        std::sync::Arc::clone(&ctx.device),
        std::sync::Arc::clone(&ctx.queue),
    );

    // Warm the shader-compile + pipeline cache (a one-time, several-ms-to-
    // tens-of-ms cost `KernelBuilder` amortizes after the first build, see
    // its own doc) so the timed dispatch below measures steady-state
    // behavior, not first-run compilation.
    hist.try_dispatch(&view, pixels.extent)
        .expect("warm-up dispatch");
    poll_until_ready(&mut hist, Duration::from_secs(5)).expect("warm-up completes");

    let dispatch_started = Instant::now();
    hist.try_dispatch(&view, pixels.extent)
        .expect("free ring slot");
    let dispatch_elapsed = dispatch_started.elapsed();
    println!("[non-blocking probe] try_dispatch took {dispatch_elapsed:?}");
    assert!(
        dispatch_elapsed < Duration::from_millis(50),
        "try_dispatch must never block on GPU completion (took {dispatch_elapsed:?})"
    );

    // Every individual `poll()` call must itself return fast, whether or
    // not it found a ready result, because `PollType::Poll` never waits.
    let mut polls = 0u32;
    let probe_deadline = Instant::now() + Duration::from_secs(5);
    let mut result = None;
    while result.is_none() && Instant::now() < probe_deadline {
        let poll_started = Instant::now();
        result = hist.poll();
        let poll_elapsed = poll_started.elapsed();
        assert!(
            poll_elapsed < Duration::from_millis(50),
            "poll() call #{polls} took {poll_elapsed:?} — PollType::Poll must never block"
        );
        polls += 1;
        if result.is_none() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let (_gen, data) = result.expect("the dispatched histogram eventually completes");
    println!("[non-blocking probe] ready after {polls} non-blocking poll() calls");
    assert_eq!(data.clip.total_pixels, 512 * 512);
}

/// **AC probe (ring):** a second `try_dispatch` while the first slot is
/// still in flight succeeds (the ring's whole point), proving the pass
/// doesn't serialize dispatches behind a blocking wait either.
#[test]
fn a_second_dispatch_succeeds_while_the_first_is_still_in_flight() {
    let Some(ctx) = device_or_skip("d12-ring-depth") else {
        return;
    };
    let pixels = synthetic_ramp(32, 32);
    let (_tex, view) = upload_texture(&ctx, &pixels);
    let mut hist = HistogramPass::new(
        std::sync::Arc::clone(&ctx.device),
        std::sync::Arc::clone(&ctx.queue),
    );
    let first = hist.try_dispatch(&view, pixels.extent);
    let second = hist.try_dispatch(&view, pixels.extent);
    assert!(first.is_some() && second.is_some(), "ring depth >= 2");
    assert_ne!(first, second, "distinct generations");

    // Both eventually drain (order not guaranteed, `poll` returns newest-
    // ready-first); collect up to 2 results.
    let mut seen = std::collections::HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.len() < 2 && Instant::now() < deadline {
        if let Some((gen, _)) = hist.poll() {
            seen.insert(gen);
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(seen.len(), 2, "both dispatched generations were collected");
}

// ── D13: ClipOverlayPass GPU == CPU reference, pixel-exactly ────────────────

#[test]
fn gpu_clip_overlay_matches_the_cpu_reference_pixel_exactly_on_a_synthetic_ramp() {
    let Some(ctx) = device_or_skip("d13-clip-overlay-exact") else {
        return;
    };
    let pixels = synthetic_ramp(64, 8);
    let cpu = clip_overlay_cpu(&pixels);

    let (_tex, view) = upload_texture(&ctx, &pixels);
    let mut pass = ClipOverlayPass::new(
        std::sync::Arc::clone(&ctx.device),
        std::sync::Arc::clone(&ctx.queue),
    );
    pass.run(&view, pixels.extent)
        .expect("clip overlay pipeline builds");
    let out_tex = pass
        .output_texture()
        .expect("run() just (re)built the output texture");
    let gpu_bytes = readback_rgba8(&ctx, out_tex, pixels.extent);

    assert_eq!(
        gpu_bytes.bytes, cpu.bytes,
        "GPU clip overlay must match the CPU reference pixel-exactly"
    );
}

/// `ClipOverlayPass::run` needs no CPU readback of its own (D13's "canvas
/// pass" is texture→texture): this probe proves it returns fast even for a
/// large frame, no `map_async`/`Device::poll(Wait)` anywhere in its path.
#[test]
fn clip_overlay_run_never_blocks_on_a_cpu_readback() {
    let Some(ctx) = device_or_skip("d13-clip-overlay-non-blocking") else {
        return;
    };
    let pixels = synthetic_ramp(1024, 1024);
    let (_tex, view) = upload_texture(&ctx, &pixels);
    let mut pass = ClipOverlayPass::new(
        std::sync::Arc::clone(&ctx.device),
        std::sync::Arc::clone(&ctx.queue),
    );
    // Warm the shader-compile + pipeline cache, see the histogram probe's
    // identical rationale above.
    pass.run(&view, pixels.extent).expect("warm-up run");

    let started = Instant::now();
    pass.run(&view, pixels.extent).expect("pipeline builds");
    let elapsed = started.elapsed();
    println!("[clip overlay] run() on 1024x1024 took {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(50),
        "run() must never block on a CPU readback (took {elapsed:?})"
    );
}
