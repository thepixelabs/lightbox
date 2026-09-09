// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Wave A-gpu integration tests (real Metal adapter on this build box):
//! A6 KernelBuilder / pipeline cache, A7 TilePool budget+reuse+stress, A9 GPU
//! backend + readback, A10 source upload round-trip, A13 canvas double-buffer,
//! and CPU/GPU parity for the engine-owned nodes (`src.decoded`, `util.resize`,
//! `xform.display`).
//!
//! Every test acquires a headless wgpu adapter; if none is available (a
//! momentarily-contended worktree), it **reports and skips** rather than
//! faking a GPU result, the authoritative run is the single-process
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
// A9 engine-level integration: drive a 2-node graph through `Engine::submit` on
// both backends and compare readbacks (single-backend determinism).
use lightbox_edit::Recipe;
use lightbox_render::ng::node::{ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::nodes::decoded::SrcDecodedFactory;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::BoxFuture;
use lightbox_render::ng::{
    BackendId, BackendPref, CpuEvalCtx, CpuTileView, DeviceError, DeviceProvider, Engine,
    EngineConfig, GraphTemplate, KernelSalt, NodeDescriptor, NodeError, NodeFactory, NodeId,
    NodeRegistry, OutFormat, OutputPayload, PortDecl, PortType, PvRange, RecipeCompiler,
    RenderPriority, RenderRequest, RenderScale, RenderState, RenderTarget, SourceError,
    SourceProvider, SourceWant,
};
use lightbox_types::{ImageId, PV_M0};

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
                                // Budget fits exactly one 512² tile, enough to force eviction of an idle
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
        let e = 128 + (i % 4) * 64; // 128,192,256,320, a few live shapes
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
            target_extent: Extent { w: 32, h: 20 },
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
            target_extent: Extent { w: 48, h: 32 },
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

// ── A9 (engine-level): Engine selects the GpuBackend via the frozen seam ──────
//
// The worktree A9 test above proves the *backend* drives `src.decoded` in
// isolation. This spans both agents: the A-core `Engine` (backend selection,
// source injection, terminal readback) driving the A-gpu `GpuBackend` end to
// end. A 2-node graph `src.decoded → test.gain` is rendered through
// `Engine::submit` on the GPU backend and on the CPU backend, and the two
// readbacks must be **byte-for-byte identical**, the single-backend
// determinism claim (spec A9). Gain is ×2 on an `f16` gradient: `2·v` is exactly
// representable in `f16` for these values, so the match is exact by
// construction, not merely within the ΔE tolerance.

/// A GPU+CPU `test.gain` (×2) node, the A-core testkit `GainProbe` defers its
/// GPU kernel to this merge (parity gate E6), so the engine-level A9 proof
/// carries a self-contained gain node with both kernels.
const GAIN2X_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(c.rgb * 2.0, c.a));
}
"#;

static GAIN_SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
static GAIN_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.gain"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&GAIN_SCHEMA),
};

struct Gain2x;

impl RenderNode for Gain2x {
    fn descriptor(&self) -> &NodeDescriptor {
        &GAIN_DESC
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("test.gain: no input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("test.gain: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent { w: 1, h: 1 });
        let pipeline = ctx.kernels.compute_pipeline(GAIN2X_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test.gain in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test.gain out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out],
            extent,
            "test.gain",
        );
        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("test.gain: no input tile".into()))?
            .pixels;
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                PixelBuf::encode_pixel(
                    fmt,
                    &mut row[x as usize * bpp..],
                    [p[0] * 2.0, p[1] * 2.0, p[2] * 2.0, p[3]],
                );
            }
        });
        Ok(())
    }
}

struct Gain2xFactory;
impl NodeFactory for Gain2xFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(Gain2x)
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.gain2x@v1"))
    }
}

/// A `DeviceProvider` over an already-acquired shared device (the shell owns the
/// device; the engine renders on it, §2.3 seam 2 / §3.8).
struct SharedDevice {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}
impl DeviceProvider for SharedDevice {
    fn current(&self) -> DeviceHandles {
        (Arc::clone(&self.device), Arc::clone(&self.queue))
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        let handles = (Arc::clone(&self.device), Arc::clone(&self.queue));
        Box::pin(async move { Ok(handles) })
    }
}

/// A `SourceProvider` returning a fixed decoded gradient (E02 seam; the engine
/// never decodes).
struct SynthSource {
    pixels: PixelBuf,
}
impl SourceProvider for SynthSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let image = as_source_image(self.pixels.clone());
        Box::pin(async move { Ok(image) })
    }
}

fn build_engine(backend: BackendPref, device: Arc<dyn DeviceProvider>, w: u32, h: u32) -> Engine {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .expect("register src.decoded");
    reg.register(
        NodeId("test.gain"),
        PvRange::from_open(PV_M0),
        Arc::new(Gain2xFactory),
    )
    .expect("register test.gain");
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(
            PV_M0,
            GraphTemplate::linear(vec![SrcDecodedNode::ID, NodeId("test.gain")]),
        )
        .expect("register template");
    let source: Arc<dyn SourceProvider> = Arc::new(SynthSource {
        pixels: source_f16_gradient(w, h),
    });
    Engine::with_compiler(
        device,
        source,
        compiler,
        EngineConfig {
            backend,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds")
}

/// Submit a full-ROI `Buffer(Rgba16)` render, poll to `Complete`, and return the
/// terminal pixels, asserting the recorded backend provenance.
fn render_pixels(engine: &Engine, w: u32, h: u32, expect: BackendId) -> PixelBuf {
    let req = RenderRequest {
        image: ImageId(1),
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi { x: 0, y: 0, w, h },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba16,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    };
    let ticket = engine.submit(req);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(out) => {
                assert_eq!(out.backend, expect, "backend provenance mismatch");
                match out.payload {
                    OutputPayload::Pixels(px) => return px,
                    other => panic!("expected Pixels payload, got {other:?}"),
                }
            }
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled unexpectedly"),
            _ => {
                if std::time::Instant::now() > deadline {
                    panic!("render did not complete within 10s");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

#[test]
fn a9_engine_gpu_two_node_graph_equals_cpu_reference_exactly() {
    let dev = gpu_or_skip!("a9-engine");
    let (w, h) = (24u32, 16u32);
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&dev.device),
        queue: Arc::clone(&dev.queue),
    });

    // Auto selects the GpuBackend on the shared device; ForceCpu is the CPU
    // reference. Both drive the identical `src.decoded → test.gain` graph.
    // Auto builds a GpuBackend on the shared device (proven by the Gpu backend
    // provenance stamped on the completed output below); ForceCpu is the CPU
    // reference.
    let gpu_engine = build_engine(BackendPref::Auto, Arc::clone(&dp), w, h);
    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::clone(&dp), w, h);

    let gpu_px = render_pixels(&gpu_engine, w, h, BackendId::Gpu);
    let cpu_px = render_pixels(&cpu_engine, w, h, BackendId::Cpu);

    assert_eq!(gpu_px.extent, cpu_px.extent, "extent mismatch");
    assert_eq!(gpu_px.format, cpu_px.format, "format mismatch");
    assert_eq!(
        gpu_px.bytes, cpu_px.bytes,
        "GPU 2-node render must equal the CPU reference byte-for-byte (single-backend determinism)"
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
