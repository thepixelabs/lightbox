// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 Phase E — **CPU/GPU parity + determinism gate** (E05.5, task E6;
//! PR-blocking per §10.1 / §8), on the real Metal adapter of this build box.
//!
//! For every engine-owned node (`src.decoded`, `util.resize`, `xform.display`)
//! plus a point-op probe (`test.gain`), across a synthetic corpus (gradient /
//! checker / low-key / high-key / high-frequency / wide-gamut), the same graph is
//! rendered through `Engine::submit` on the GPU backend (`Auto`) and the CPU
//! backend (`ForceCpu`) and asserted **within ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB**
//! (§4.4) using the testkit's validated comparators. Each backend is also checked
//! **bit-deterministic across 3 repeat renders**, and every `RenderOutput`
//! carries correct backend provenance.
//!
//! The gate rules out non-representable outputs by comparing the display-encoded
//! terminal (`DisplayRgba8`) read back as sRGB8 — the space the ΔE2000 reference
//! operates in. If no adapter is available the test **reports and skips** rather
//! than faking a GPU number; the authoritative run is the single-process merge on
//! `main`.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::node::{ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::XformDisplayFactory;
use lightbox_render::ng::nodes::resize::UtilResizeFactory;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, CpuEvalCtx, CpuTileView, DeviceError, DeviceProvider,
    Engine, EngineConfig, Extent, GpuEvalCtx, GraphTemplate, KernelSalt, NodeDescriptor, NodeError,
    NodeFactory, NodeId, NodeRegistry, OutFormat, OutputPayload, PixelBuf, PixelFormat, PortDecl,
    PortType, PvRange, RecipeCompiler, RenderNode, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, RenderTicket, SourceColorimetry, SourceError, SourceImage,
    SourceProvider, SourceQuality, SourceWant, TileView,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_types::{ImageId, PV_M0};

// ── seams ─────────────────────────────────────────────────────────────────────

/// A `DeviceProvider` over an already-acquired shared device (§2.3 seam 2 / §3.8).
struct SharedDevice {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}
impl DeviceProvider for SharedDevice {
    fn current(&self) -> DeviceHandles {
        (Arc::clone(&self.device), Arc::clone(&self.queue))
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        let h = (Arc::clone(&self.device), Arc::clone(&self.queue));
        Box::pin(async move { Ok(h) })
    }
}

/// A `SourceProvider` returning a fixed decoded pattern (E02 seam).
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
        let pixels = self.pixels.clone();
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry: SourceColorimetry::default(),
                full_extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

// ── a self-contained point-op probe (test.gain, ×0.75) with GPU + CPU kernels ──
//
// The testkit `GainProbe` GPU kernel is deferred (owned by C in probes.rs); to
// keep this parity gate in strictly-disjoint files (E owns no testkit probe
// bodies) the gate carries its own point-op probe with both kernels. The fixed
// 0.75 factor keeps values in-range so the display LUT maps a full gradient
// (never a degenerate clamp).

const GAIN_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(c.rgb * 0.75, c.a));
}
"#;

const GAIN: f32 = 0.75;

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

struct GainProbe;
impl RenderNode for GainProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &GAIN_DESC
    }
    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        _: &lightbox_render::ng::ParamBlock,
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
        let pipeline = ctx.kernels.compute_pipeline(GAIN_WGSL, "main")?;
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
        lightbox_render::ng::gpu::dispatch_compute(
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
        _: &lightbox_render::ng::ParamBlock,
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
                    [p[0] * GAIN, p[1] * GAIN, p[2] * GAIN, p[3]],
                );
            }
        });
        Ok(())
    }
}

struct GainFactory;
impl NodeFactory for GainFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GainProbe)
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(b"test.gain075@v1"))
    }
}

// ── corpus (synthetic, 32×32 u8 decoded RGB) ──────────────────────────────────

fn buf(bytes: Vec<u8>, w: u32, h: u32) -> PixelBuf {
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba8Unorm,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

fn corpus(name: &str, w: u32, h: u32) -> PixelBuf {
    let mut b = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, bl) = match name {
                "gradient" => (
                    (x * 255 / w.max(1)) as u8,
                    (y * 255 / h.max(1)) as u8,
                    128u8,
                ),
                "checker" => {
                    if (x / 4 + y / 4) % 2 == 0 {
                        (30, 60, 90)
                    } else {
                        (200, 170, 140)
                    }
                }
                "low_key" => ((x * 40 / w.max(1)) as u8, (y * 30 / h.max(1)) as u8, 12u8),
                "high_key" => (
                    (215 + x * 40 / w.max(1)) as u8,
                    (220 + y * 30 / h.max(1)) as u8,
                    240u8,
                ),
                "high_frequency" => {
                    if (x + y) % 2 == 0 {
                        (10, 10, 10)
                    } else {
                        (245, 245, 245)
                    }
                }
                "wide_gamut" => {
                    // fully-saturated primaries/secondaries cycling per column.
                    match x % 6 {
                        0 => (255, 0, 0),
                        1 => (0, 255, 0),
                        2 => (0, 0, 255),
                        3 => (255, 255, 0),
                        4 => (0, 255, 255),
                        _ => (255, 0, 255),
                    }
                }
                _ => (128, 128, 128),
            };
            b[i] = r;
            b[i + 1] = g;
            b[i + 2] = bl;
            b[i + 3] = 255;
        }
    }
    buf(b, w, h)
}

const CORPUS: &[&str] = &[
    "gradient",
    "checker",
    "low_key",
    "high_key",
    "high_frequency",
    "wide_gamut",
];

// ── engine over a named graph ─────────────────────────────────────────────────

fn engine_for(
    pref: BackendPref,
    dp: Arc<dyn DeviceProvider>,
    pixels: PixelBuf,
    stages: Vec<NodeId>,
) -> Engine {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("util.resize"),
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("xform.display"),
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("test.gain"),
        PvRange::from_open(PV_M0),
        Arc::new(GainFactory),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(PV_M0, GraphTemplate::linear(stages))
        .unwrap();
    Engine::with_compiler(
        dp,
        Arc::new(SynthSource { pixels }),
        compiler,
        EngineConfig {
            backend: pref,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds")
}

fn render(engine: &Engine, w: u32, h: u32, expect: BackendId) -> PixelBuf {
    let req = RenderRequest {
        image: ImageId(1),
        recipe: lightbox_edit::Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: lightbox_render::ng::Roi { x: 0, y: 0, w, h },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Batch,
        cancel: CancelToken::new(),
    };
    let ticket: RenderTicket = engine.submit(req);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(out) => {
                assert_eq!(out.backend, expect, "backend provenance");
                let OutputPayload::Pixels(px) = out.payload else {
                    panic!("expected pixels");
                };
                assert_eq!(px.format, PixelFormat::Rgba8Srgb);
                return px;
            }
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => {
                if std::time::Instant::now() > deadline {
                    panic!("render did not complete within 20s");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

fn texels(px: &PixelBuf) -> Vec<[u8; 4]> {
    px.bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

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

/// The three graphs the gate exercises (each ends in `xform.display`).
fn graphs() -> Vec<(&'static str, Vec<NodeId>)> {
    vec![
        ("display", vec![SrcDecodedNode::ID, NodeId("xform.display")]),
        (
            "resize",
            vec![
                SrcDecodedNode::ID,
                NodeId("util.resize"),
                NodeId("xform.display"),
            ],
        ),
        (
            "gain",
            vec![
                SrcDecodedNode::ID,
                NodeId("test.gain"),
                NodeId("xform.display"),
            ],
        ),
    ]
}

// ── E6: CPU/GPU parity within ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB ─────────────────────

#[test]
fn e6_cpu_gpu_parity_over_corpus_and_graphs() {
    let Some(ctx) = device_or_skip("e6-parity") else {
        return;
    };
    let (w, h) = (32u32, 32u32);

    for (gname, stages) in graphs() {
        for &cname in CORPUS {
            let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
                device: Arc::clone(&ctx.device),
                queue: Arc::clone(&ctx.queue),
            });
            let pattern = corpus(cname, w, h);

            let gpu_engine = engine_for(
                BackendPref::Auto,
                Arc::clone(&dp),
                pattern.clone(),
                stages.clone(),
            );
            let cpu_engine = engine_for(
                BackendPref::ForceCpu,
                Arc::clone(&dp),
                pattern.clone(),
                stages.clone(),
            );

            let gpu_px = render(&gpu_engine, w, h, BackendId::Gpu);
            let cpu_px = render(&cpu_engine, w, h, BackendId::Cpu);
            assert_eq!(gpu_px.extent, cpu_px.extent, "{gname}/{cname}: extent");

            let stats = delta_e_stats(&texels(&cpu_px), &texels(&gpu_px));
            let psnr_db = psnr(&cpu_px.bytes, &gpu_px.bytes);
            assert!(
                stats.within_tolerance(),
                "{gname}/{cname}: ΔE2000 max {:.4} exceeds 1.0 (mean {:.4}, p99 {:.4})",
                stats.max,
                stats.mean,
                stats.p99
            );
            assert!(
                psnr_db >= TOLERANCE_PSNR_DB,
                "{gname}/{cname}: PSNR {psnr_db:.2} dB below {TOLERANCE_PSNR_DB}"
            );
        }
    }
}

// ── E6: each backend bit-deterministic across 3 repeat renders ───────────────

#[test]
fn e6_each_backend_is_bit_deterministic_across_three_runs() {
    let Some(ctx) = device_or_skip("e6-determinism") else {
        return;
    };
    let (w, h) = (32u32, 32u32);
    // The most kernel-diverse graph: source → gain → resize? use the full PV1
    // chain plus the point-op, on the high-frequency pattern (worst case for any
    // nondeterministic rounding).
    let stages = vec![
        SrcDecodedNode::ID,
        NodeId("test.gain"),
        NodeId("util.resize"),
        NodeId("xform.display"),
    ];
    let pattern = corpus("high_frequency", w, h);

    for (pref, expect) in [
        (BackendPref::Auto, BackendId::Gpu),
        (BackendPref::ForceCpu, BackendId::Cpu),
    ] {
        let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
            device: Arc::clone(&ctx.device),
            queue: Arc::clone(&ctx.queue),
        });
        let engine = engine_for(pref, dp, pattern.clone(), stages.clone());
        let a = render(&engine, w, h, expect);
        let b = render(&engine, w, h, expect);
        let c = render(&engine, w, h, expect);
        assert_eq!(
            a.bytes, b.bytes,
            "{expect:?} run 1 vs 2 must be bit-identical"
        );
        assert_eq!(
            b.bytes, c.bytes,
            "{expect:?} run 2 vs 3 must be bit-identical"
        );
    }
}
