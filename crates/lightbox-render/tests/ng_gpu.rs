// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 Wave A-gpu integration tests (real Metal adapter on this build box):
//! A6 KernelBuilder / pipeline cache, A7 TilePool budget+reuse+stress, A9 GPU
//! backend + readback, A10 source upload round-trip, A13 canvas double-buffer,
//! and CPU/GPU parity for the engine-owned nodes (`src.decoded`, `util.resize`,
//! `xform.display`).
//!
//! Every test acquires a headless wgpu adapter; if none is available (a
//! momentarily-contended worktree), it **reports and skips** rather than
//! faking a GPU result — the authoritative run is the single-process
//! merge/verify pass on `main`.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::cache::{Bytes, CacheKey};
use lightbox_render::ng::exec::backend::{Backend, BackendEvalRequest};
use lightbox_render::ng::exec::gpu::{readback_tile, GpuBackend};
use lightbox_render::ng::gpu::{dispatch_compute, DeviceCtx, KernelBuilder, TilePool};
use lightbox_render::ng::node::{GpuEvalCtx, ParamBlock, RenderNode};
use lightbox_render::ng::nodes::decoded::SrcDecodedNode;
use lightbox_render::ng::nodes::display::{apply_display_cpu, XformDisplayNode};
use lightbox_render::ng::nodes::resize::{decimate_box_cpu, UtilResizeNode};
use lightbox_render::ng::nodes::support;
use lightbox_render::ng::sched::canvas::CanvasPublisher;
use lightbox_render::ng::source::{SourceImage, Uploader};
use lightbox_render::ng::tile::{PixelBuf, PixelFormat, TileHandle, TileView};
use lightbox_render::ng::types::{Extent, Roi, TilePrecision};
use lightbox_render::ng::OutputQuality;
use lightbox_render::ng::{SourceColorimetry, SourceQuality};

/// Acquire a headless device/queue, or `None` when no adapter is available.
fn device() -> Option<DeviceCtx> {
    let ctx = lightbox_render::GpuContext::headless()?;
    Some(DeviceCtx::new(ctx.device.clone(), ctx.queue.clone()))
}

macro_rules! gpu_or_skip {
    ($name:literal) => {
        match device() {
            Some(d) => d,
            None => {
                eprintln!(
                    "[{}] no wgpu adapter available — SKIPPED (authoritative run is on main)",
                    $name
                );
                return;
            }
        }
    };
}

// ── source builders ──────────────────────────────────────────────────────────

fn source_u8_gradient(w: u32, h: u32) -> PixelBuf {
    let mut bytes = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            bytes[i] = (x * 255 / w.max(1)) as u8;
            bytes[i + 1] = (y * 255 / h.max(1)) as u8;
            bytes[i + 2] = 128;
            bytes[i + 3] = 255;
        }
    }
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba8Unorm,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

fn source_f32_gradient(w: u32, h: u32) -> PixelBuf {
    let mut bytes = vec![0u8; (w * h * 16) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 16) as usize;
            let r = x as f32 / w.max(1) as f32;
            let g = y as f32 / h.max(1) as f32;
            let vals = [r, g, 0.5, 1.0];
            for (c, v) in vals.iter().enumerate() {
                bytes[i + c * 4..i + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba32F,
        extent: Extent { w, h },
        stride: w * 16,
    }
}

fn source_f16_gradient(w: u32, h: u32) -> PixelBuf {
    let mut bytes = vec![0u8; (w * h * 8) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 8) as usize;
            let r = half::f16::from_f32(x as f32 / w.max(1) as f32);
            let g = half::f16::from_f32(y as f32 / h.max(1) as f32);
            let vals = [r, g, half::f16::from_f32(0.25), half::f16::from_f32(1.0)];
            for (c, v) in vals.iter().enumerate() {
                bytes[i + c * 2..i + c * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba16F,
        extent: Extent { w, h },
        stride: w * 8,
    }
}

fn as_source_image(pixels: PixelBuf) -> SourceImage {
    let full_extent = pixels.extent;
    SourceImage {
        pixels,
        colorimetry: SourceColorimetry::default(),
        full_extent,
        quality: SourceQuality::Full,
    }
}

// ── A6: KernelBuilder + pipeline cache + naga validation ─────────────────────

const CONST_KERNEL: &str = r#"
@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let d = textureDimensions(dst);
    if gid.x >= d.x || gid.y >= d.y { return; }
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(0.25, 0.5, 0.75, 1.0));
}
"#;

#[test]
fn a6_kernel_writes_constant_and_caches_pipeline() {
    let dev = gpu_or_skip!("a6");
    let kernels = KernelBuilder::new(&dev);
    let pool = TilePool::new(Arc::clone(&dev.device), Bytes(64 << 20));

    let out = pool.acquire(Extent { w: 40, h: 24 }, TilePrecision::F16);
    let pipeline = kernels
        .compute_pipeline(CONST_KERNEL, "main")
        .expect("pipeline");
    let bg = dev.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("const out"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(out.texture_view().unwrap()),
        }],
    });
    dispatch_compute(
        &dev.device,
        &dev.queue,
        &pipeline,
        &[&bg],
        Extent { w: 40, h: 24 },
        "const",
    );

    let px = readback_tile(&dev.device, &dev.queue, &out).expect("readback");
    for y in 0..24usize {
        for x in 0..40usize {
            let c = support::read_rgba_f32(&px, x, y);
            assert!((c[0] - 0.25).abs() < 1e-2, "r={} at {x},{y}", c[0]);
            assert!((c[1] - 0.5).abs() < 1e-2, "g={}", c[1]);
            assert!((c[2] - 0.75).abs() < 1e-2, "b={}", c[2]);
            assert!((c[3] - 1.0).abs() < 1e-2, "a={}", c[3]);
        }
    }

    // Same source+entry ⇒ cache hit (one resident pipeline).
    let _again = kernels
        .compute_pipeline(CONST_KERNEL, "main")
        .expect("cached");
    assert_eq!(
        kernels.cached_len(),
        1,
        "pipeline cache should dedupe by blake3(wgsl‖entry)"
    );
}

#[test]
fn a6_invalid_wgsl_is_an_error_not_a_panic() {
    let dev = gpu_or_skip!("a6-invalid");
    let kernels = KernelBuilder::new(&dev);
    let bad = "@compute @workgroup_size(16,16,1) fn main() { this is not wgsl }";
    assert!(kernels.compute_pipeline(bad, "main").is_err());
}

// ── A7: TilePool budget accounting, reuse, LRU, stress ───────────────────────

#[test]
fn a7_tilepool_reuses_and_respects_budget() {
    let dev = gpu_or_skip!("a7");
    let small = 256u64 * 256 * 8; // 512 KiB
    let big = 512u64 * 512 * 8; // 2 MiB
                                // Budget fits exactly one 512² tile — enough to force eviction of an idle
                                // 256² when the 512² is acquired, but not to hold both at once.
    let pool = TilePool::new(Arc::clone(&dev.device), Bytes(big));

    let a = pool.acquire(Extent { w: 256, h: 256 }, TilePrecision::F16);
    assert_eq!(pool.used().0, small);
    assert_eq!(pool.texture_count(), 1);
    drop(a);
    // Re-acquire same shape ⇒ reuse the idle texture, not a new one.
    let b = pool.acquire(Extent { w: 256, h: 256 }, TilePrecision::F16);
    assert_eq!(
        pool.texture_count(),
        1,
        "same-shape acquire must reuse the idle texture"
    );
    assert_eq!(pool.used().0, small);
    drop(b);

    // Acquiring a differently-shaped tile under budget pressure LRU-evicts the
    // idle 256², keeping residency ≤ budget.
    let c = pool.acquire(Extent { w: 512, h: 512 }, TilePrecision::F16);
    assert!(
        pool.used().0 <= pool.budget().0,
        "used {} must stay ≤ budget {}",
        pool.used().0,
        pool.budget().0
    );
    assert_eq!(
        pool.texture_count(),
        1,
        "idle 256² should have been evicted for the 512²"
    );
    drop(c);
}

#[test]
fn a7_tilepool_10k_cycle_stress_within_budget() {
    let dev = gpu_or_skip!("a7-stress");
    let one = 256u64 * 256 * 8;
    let pool = TilePool::new(Arc::clone(&dev.device), Bytes(one * 4));
    for i in 0..10_000u32 {
        let e = 128 + (i % 4) * 64; // 128,192,256,320 — a few live shapes
        let t = pool.acquire(Extent { w: e, h: e }, TilePrecision::F16);
        assert!(
            pool.used().0 <= pool.budget().0,
            "iteration {i}: used {} exceeded budget {}",
            pool.used().0,
            pool.budget().0
        );
        drop(t);
    }
    assert!(pool.peak().0 <= pool.budget().0);
}

// ── A10: source upload round-trips 8/16/f32 losslessly (within f16) ──────────

fn assert_upload_roundtrip(dev: &DeviceCtx, src: PixelBuf) {
    let reference = src.clone();
    let handle = Uploader::new().upload(dev, &as_source_image(src));
    let back = readback_tile(&dev.device, &dev.queue, &handle).expect("readback");
    assert_eq!(back.extent, reference.extent);
    for y in 0..reference.extent.h as usize {
        for x in 0..reference.extent.w as usize {
            let want = support::read_rgba_f32(&reference, x, y);
            let got = support::read_rgba_f32(&back, x, y);
            for c in 0..4 {
                // f16 has ~11 bits mantissa; 1/1000 covers quantization on [0,1].
                assert!(
                    (want[c] - got[c]).abs() <= 1.0 / 1000.0,
                    "channel {c} at ({x},{y}): want {} got {}",
                    want[c],
                    got[c]
                );
            }
        }
    }
}

#[test]
fn a10_upload_roundtrip_u8() {
    let dev = gpu_or_skip!("a10-u8");
    assert_upload_roundtrip(&dev, source_u8_gradient(37, 21));
}

#[test]
fn a10_upload_roundtrip_f16() {
    let dev = gpu_or_skip!("a10-f16");
    assert_upload_roundtrip(&dev, source_f16_gradient(37, 21));
}

#[test]
fn a10_upload_roundtrip_f32() {
    let dev = gpu_or_skip!("a10-f32");
    assert_upload_roundtrip(&dev, source_f32_gradient(37, 21));
}

// ── A9: GPU backend drives src.decoded end-to-end ────────────────────────────

#[test]
fn a9_gpu_backend_src_decoded_copies_source() {
    let dev = gpu_or_skip!("a9");
    let backend = GpuBackend::new(&dev);
    let source = Uploader::new().upload(&dev, &as_source_image(source_f16_gradient(32, 20)));
    let node = SrcDecodedNode::new();
    let params = ParamBlock::default();
    let cancel = CancelToken::new();
    let inputs = [source.clone()];

    let out = backend
        .eval_node(BackendEvalRequest {
            key: CacheKey(blake3::hash(b"src.decoded")),
            node: &node,
            params: &params,
            inputs: &inputs,
            roi: Roi {
                x: 0,
                y: 0,
                w: 32,
                h: 20,
            },
            scale: 1.0,
            precision: TilePrecision::F16,
            cancel: &cancel,
        })
        .expect("eval src.decoded");

    let want = readback_tile(&dev.device, &dev.queue, &source).expect("src readback");
    let got = readback_tile(&dev.device, &dev.queue, &out).expect("out readback");
    assert_eq!(want.bytes, got.bytes, "src.decoded must be an exact copy");
    assert_eq!(backend.kind(), lightbox_render::ng::BackendId::Gpu);
}

// ── Node CPU/GPU parity: util.resize box decimation ──────────────────────────

#[test]
fn resize_gpu_matches_cpu_box_decimation() {
    let dev = gpu_or_skip!("resize");
    let uploaded = Uploader::new().upload(&dev, &as_source_image(source_f32_gradient(64, 48)));
    let working = readback_tile(&dev.device, &dev.queue, &uploaded).expect("working readback");
    let out_extent = Extent { w: 16, h: 12 };

    // CPU reference.
    let cpu = decimate_box_cpu(&working, out_extent);

    // GPU via the node, into a pool-acquired smaller output tile.
    let kernels = KernelBuilder::new(&dev);
    let pool = TilePool::new(Arc::clone(&dev.device), Bytes(16 << 20));
    let out_tile = pool.acquire(out_extent, TilePrecision::F16);
    let node = UtilResizeNode::new();
    let params = ParamBlock::default();
    let cancel = CancelToken::new();
    let mut ctx = GpuEvalCtx::new(&dev.device, &dev.queue, &kernels, 1.0, &cancel, out_tile);
    let view = TileView {
        view: uploaded.texture_view().unwrap(),
        precision: TilePrecision::F16,
        roi: Roi {
            x: 0,
            y: 0,
            w: 64,
            h: 48,
        },
    };
    node.eval_gpu(&mut ctx, &[view], &params)
        .expect("resize gpu");
    let gpu = readback_tile(&dev.device, &dev.queue, ctx.output()).expect("gpu readback");

    for y in 0..out_extent.h as usize {
        for x in 0..out_extent.w as usize {
            let a = support::read_rgba_f32(&cpu, x, y);
            let b = support::read_rgba_f32(&gpu, x, y);
            for c in 0..4 {
                assert!(
                    (a[c] - b[c]).abs() <= 2.0 / 1000.0,
                    "resize parity ch {c} at ({x},{y}): cpu {} gpu {}",
                    a[c],
                    b[c]
                );
            }
        }
    }
}

// ── Node CPU/GPU parity: xform.display via lightbox-color bake ────────────────

#[test]
fn display_gpu_matches_cpu_lightbox_color() {
    let dev = gpu_or_skip!("display");
    let backend = GpuBackend::new(&dev);
    let uploaded = Uploader::new().upload(&dev, &as_source_image(source_f32_gradient(48, 32)));
    let working = readback_tile(&dev.device, &dev.queue, &uploaded).expect("working readback");

    // CPU reference (lightbox-color baked apply).
    let cpu = apply_display_cpu(&working);

    // GPU via the backend (display node → rgba8 output tile).
    let node = XformDisplayNode::new();
    let params = ParamBlock::default();
    let cancel = CancelToken::new();
    let inputs = [uploaded.clone()];
    let out = backend
        .eval_node(BackendEvalRequest {
            key: CacheKey(blake3::hash(b"xform.display")),
            node: &node,
            params: &params,
            inputs: &inputs,
            roi: Roi {
                x: 0,
                y: 0,
                w: 48,
                h: 32,
            },
            scale: 1.0,
            precision: TilePrecision::F16,
            cancel: &cancel,
        })
        .expect("eval xform.display");
    let gpu = readback_tile(&dev.device, &dev.queue, &out).expect("display readback");

    assert_eq!(gpu.format, PixelFormat::Rgba8Unorm);
    let mut max_diff = 0u8;
    for y in 0..32usize {
        for x in 0..48usize {
            let a = &cpu.bytes[(y * cpu.stride as usize + x * 4)..][..4];
            let b = &gpu.bytes[(y * gpu.stride as usize + x * 4)..][..4];
            for c in 0..4 {
                max_diff = max_diff.max(a[c].abs_diff(b[c]));
            }
        }
    }
    assert!(
        max_diff <= 2,
        "display CPU/GPU parity: max byte diff {max_diff} > 2"
    );
}

// ── A13: canvas double-buffer publishes tear-free generations ────────────────

fn solid_rgba8_tile(dev: &DeviceCtx, extent: Extent, rgba: [u8; 4]) -> TileHandle {
    let pool = TilePool::new(Arc::clone(&dev.device), Bytes(16 << 20));
    let tile = pool.acquire_format(extent, wgpu::TextureFormat::Rgba8Unorm, TilePrecision::F16);
    let w = extent.w as usize;
    let h = extent.h as usize;
    let mut data = vec![0u8; w * h * 4];
    for px in data.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    dev.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tile.texture().unwrap(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(extent.w * 4),
            rows_per_image: Some(extent.h),
        },
        wgpu::Extent3d {
            width: extent.w,
            height: extent.h,
            depth_or_array_layers: 1,
        },
    );
    dev.queue.submit(std::iter::empty());
    // The local pool drops here, but the tile's own `Arc<Texture>` keeps the
    // texture resident for the returned handle's lifetime.
    tile
}

#[test]
fn a13_canvas_publishes_tear_free_generations() {
    let dev = gpu_or_skip!("a13");
    let extent = Extent { w: 32, h: 32 };
    let (publisher, rx) =
        CanvasPublisher::new(Arc::clone(&dev.device), Arc::clone(&dev.queue), extent);

    // Shell-sim: publish a run of solid frames, each encoding its generation in
    // the red channel; after each publish the shell samples the watch value and
    // reads the published slot back, asserting the whole texture is a single
    // consistent generation (no tear) and the frame generation is in lock-step.
    for gen in 1u8..=12 {
        let frame_src = solid_rgba8_tile(&dev, extent, [gen, 0, 0, 255]);
        let published = publisher
            .publish(&frame_src, OutputQuality::FullRes)
            .expect("publish");
        assert_eq!(published, u64::from(gen));

        // The watch channel reflects the just-published generation synchronously.
        let frame = rx.borrow().clone();
        assert_eq!(frame.generation, u64::from(gen));
        assert_eq!(frame.extent, extent);

        let (read_gen, px) = publisher.readback_current().expect("readback");
        assert_eq!(read_gen, u64::from(gen));
        for chunk in px.bytes.chunks_exact(4) {
            assert_eq!(chunk[0], gen, "torn canvas frame at generation {gen}");
            assert_eq!(chunk[3], 255);
        }
    }
    assert_eq!(publisher.generation(), 12);
}
