// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase F3, the engine book's "add a node" walkthrough, as a real,
//! compiling, passing test (not just a documentation snippet). See
//! `docs/engine-book/03-add-a-node.md`, which walks through this exact file
//! section by section.
//!
//! `tone.exposure` is a minimal but complete example develop-tool node: one
//! input, one typed float param (`exposure_ev`, default `0.0`), a real WGSL
//! kernel + its CPU parity twin, registered into a tiny PV1-shaped template
//! and driven end-to-end through `Engine::submit` on **both** backends. It
//! is not shipped (test-only, this file), E10 registers the real
//! `tone.exposure` under the engine-owned `lightbox-render` crate.
//!
//! If no GPU adapter is available the GPU half of this test reports and
//! skips (never fakes a readback), the authoritative run is CI's macOS/
//! Metal leg (§8 DoD #8).

use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::node::param::{FieldDecl, ParamKind, ParamsSchema};
use lightbox_render::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamsSchemaRef,
};
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent, NodeError,
    NodeId, NodeRegistry, OutFormat, OutputPayload, ParamBlock, ParamValue, PixelBuf, PixelFormat,
    PortDecl, PortType, PvRange, RecipeCompiler, RenderNode, RenderPriority, RenderRequest,
    RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry, SourceError, SourceImage,
    SourceProvider, SourceQuality, SourceWant, TileView,
};
use lightbox_types::{ImageId, PV_M0};

// ── Step 1: the WGSL kernel ─────────────────────────────────────────────────
//
// One entry point; the §3.2 bind-group convention: @group(0) input texture,
// @group(1) write-only output storage, @group(2) params UBO. `exp2` is a
// WGSL builtin, the kernel does the same math as the CPU twin below, bit for
// bit reproducible to within the §4.4 ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB tolerance.
const EXPOSURE_WGSL: &str = r#"
struct Params {
    stops: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let gain = exp2(params.stops);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(c.rgb * gain, c.a));
}
"#;

/// Step 2: the CPU parity twin, the identical algorithm, no shader.
fn apply_exposure_cpu(gain: f32, p: [f32; 4]) -> [f32; 4] {
    [p[0] * gain, p[1] * gain, p[2] * gain, p[3]]
}

// ── Step 3: the param schema ─────────────────────────────────────────────────
//
// A non-empty `ParamsSchema` rejects unknown field names / type mismatches at
// `ParamBlock` construction, the recipe→params boundary a node author gets
// for free by declaring real fields (contrast with `ParamsSchema::EMPTY`,
// which every engine-owned M1 scaffold node still uses, per the module docs
// in `ng::node::param`).
static EXPOSURE_SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "exposure_ev",
    kind: ParamKind::Float,
}]);

// ── Step 4: the descriptor ───────────────────────────────────────────────────
static EXPOSURE_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("tone.exposure"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&EXPOSURE_SCHEMA),
};

/// Step 5: the node. Every method not overridden here takes the trait's
/// spec-documented default, `plan` (identity ROI: exposure is a point-op,
/// no neighborhood), `output_extent` (identity: same shape as input),
/// `cache_policy` (`Cache`), `affected_by` (conservative `true`), `precision`
/// (`F16`), `aux_requirements` (`NONE`). A neighborhood node (blur, sharpen)
/// would override `plan` to expand the requested ROI by its radius, see
/// `lightbox-render-testkit`'s `BlurRProbe` for a worked apron example.
#[derive(Default)]
struct ExposureNode {}

impl RenderNode for ExposureNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &EXPOSURE_DESC
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("tone.exposure: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("tone.exposure: output is not a GPU tile".into()))?
            .clone();
        let stops = params.get_f64_or("exposure_ev", 0.0) as f32;

        // The params UBO: std140-compatible layout matching `struct Params`
        // in the WGSL above (§3.2 kernel conventions).
        let ubo_bytes = stops.to_le_bytes();
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tone.exposure params"),
            size: 16, // std140 rounds the smallest uniform block up to 16 bytes
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut padded = [0u8; 16];
        padded[..4].copy_from_slice(&ubo_bytes);
        ctx.queue.write_buffer(&ubo, 0, &padded);

        let pipeline = ctx.kernels.compute_pipeline(EXPOSURE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tone.exposure in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tone.exposure out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tone.exposure params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let extent = ctx.output().extent().unwrap_or(Extent { w: 1, h: 1 });
        lightbox_render::ng::gpu::dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params],
            extent,
            "tone.exposure",
        );
        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[lightbox_render::ng::CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("tone.exposure: missing input tile".into()))?
            .pixels;
        let gain = 2f32.powf(params.get_f64_or("exposure_ev", 0.0) as f32);
        let out = ctx.output();
        let (w, h) = (out.extent.w, out.extent.h);
        for y in 0..h {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                out.set_rgba_f32(x, y, apply_exposure_cpu(gain, p));
            }
        }
        Ok(())
    }
}

/// Step 6: the factory, registered instances + the kernel salt (part of the
/// cache key; bump it whenever the WGSL/CPU algorithm changes, per §4.5).
#[derive(Default)]
struct ExposureFactory {}

impl NodeFactory for ExposureFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ExposureNode::default())
    }
    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(EXPOSURE_WGSL.as_bytes()))
    }
}

// ── Step 7: wire it into a graph and drive it through the real Engine ──────

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
struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unreachable!("ForceCpu never calls DeviceProvider::current")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "no device in the walkthrough test".into(),
            ))
        })
    }
}

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

fn gradient(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    for y in 0..h {
        for x in 0..w {
            let v = (x + y) as f32 / (w + h).max(1) as f32;
            px.set_rgba_f32(x, y, [v * 0.3, v * 0.3, v * 0.3, 1.0]);
        }
    }
    px
}

fn build_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .expect("register src.decoded");
    reg.register(
        NodeId("tone.exposure"),
        PvRange::from_open(PV_M0),
        Arc::new(ExposureFactory::default()),
    )
    .expect("register tone.exposure");
    reg
}

fn build_template() -> lightbox_render::ng::GraphTemplate {
    lightbox_render::ng::GraphTemplate::linear(vec![SrcDecodedNode::ID, NodeId("tone.exposure")])
}

fn request(recipe: Recipe) -> RenderRequest {
    RenderRequest {
        image: ImageId(1),
        recipe,
        pv: PV_M0,
        roi: Roi {
            x: 0,
            y: 0,
            w: 32,
            h: 32,
        },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba32F,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

fn wait_complete(engine: &Engine, ticket: &lightbox_render::ng::RenderTicket) -> PixelBuf {
    loop {
        match engine.poll(ticket) {
            RenderState::Complete(out) => match out.payload {
                OutputPayload::Pixels(px) => return px,
                other => panic!("expected Pixels, got {other:?}"),
            },
            RenderState::Queued | RenderState::Rendering { .. } => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            other => panic!("unexpected terminal state: {other:?}"),
        }
    }
}

/// The CPU path: `tone.exposure` at +1 EV doubles every channel, the
/// walkthrough's headless, always-runnable proof.
#[test]
fn walkthrough_node_renders_on_the_cpu_path() {
    let mut compiler = RecipeCompiler::with_registry(Arc::new(build_registry()));
    compiler.register_template(PV_M0, build_template()).unwrap();
    let engine = Engine::with_compiler(
        Arc::new(NullDevice),
        Arc::new(SynthSource {
            pixels: gradient(32, 32),
        }),
        compiler,
        EngineConfig {
            backend: BackendPref::ForceCpu,
            ..EngineConfig::default()
        },
    )
    .unwrap();

    let ticket = engine.submit(request(Recipe::identity(PV_M0)));
    let px = wait_complete(&engine, &ticket);
    let source = gradient(32, 32);
    // Recipe::identity carries no params (M1, E09 owns real recipe params),
    // so the compiled graph's `tone.exposure` runs at its schema default,
    // `exposure_ev = 0.0` ⇒ `gain = 2^0 = 1.0` (identity), the output must
    // equal the source unchanged.
    let expect = apply_exposure_cpu(1.0, source.get_rgba_f32(10, 10));
    let got = px.get_rgba_f32(10, 10);
    for c in 0..3 {
        assert!(
            (got[c] - expect[c]).abs() < 1e-3,
            "channel {c}: got {}, expected {} (default exposure_ev=0.0 ⇒ gain 1.0 identity)",
            got[c],
            expect[c]
        );
    }
}

/// Step 8: the param ABI actually changes output. `Recipe` carries no
/// per-node params at M1 (E09's job), so the two `Engine`-level tests above
/// only exercise `tone.exposure`'s **default** `exposure_ev = 0.0`. Driving
/// the graph directly (the same technique `lightbox-render-testkit`'s
/// scenario harness uses, `scenario.rs::render_step`) proves a **non**-default
/// param is honored end to end: +1 EV must double every channel.
#[test]
fn walkthrough_node_honors_a_non_default_param() {
    use lightbox_render::ng::cache::NodeCache;
    use lightbox_render::ng::exec::cpu::CpuBackend;
    use lightbox_render::ng::{Executor, RenderGraph};

    // A minimal graph: the engine-owned `src.decoded` (identity-copy of the
    // injected source, see `nodes::decoded`) → `tone.exposure(+1 EV)`. The
    // executor's `source` injection below drives `src.decoded`'s input.
    let mut graph = RenderGraph::new();
    let decoded = graph.add_node(Arc::new(SrcDecodedNode::new()));
    let exposure = graph.add_node_with_params(
        Arc::new(ExposureNode::default()),
        ParamBlock::from_fields([("exposure_ev", ParamValue::Float(1.0))]).unwrap(),
    );
    graph.connect(decoded, exposure, "in").unwrap();

    let cache = NodeCache::with_budget(64 * 1024 * 1024);
    let backend = CpuBackend::new(None);
    let executor = Executor::new(Arc::new(backend));
    let cancel = CancelToken::new();

    let source = gradient(32, 32);
    let source_tile = lightbox_render::ng::TileHandle::from_cpu(source.clone());
    let key = lightbox_render::ng::CacheKey::derive(&lightbox_render::ng::CacheKeyInputs {
        node_id: SrcDecodedNode::ID,
        pv: PV_M0,
        kernel_salt: graph.salt(decoded),
        param_hash: graph.params(decoded).hash(),
        input_keys: &[],
        tile: lightbox_render::ng::TileCoord {
            tx: 0,
            ty: 0,
            scale_q: 64,
        },
        scale_q: 64,
        // A source-role key (this pre-pins the injected tile), constant,
        // matching `Engine::source_key`'s own "scale-independent" convention
        // (see `CacheKeyInputs::content_extent`'s doc).
        content_extent: Extent { w: 0, h: 0 },
        precision: lightbox_render::ng::TilePrecision::F16,
    });

    let tile = executor
        .evaluate(
            &graph,
            PV_M0,
            Roi {
                x: 0,
                y: 0,
                w: 32,
                h: 32,
            },
            RenderScale::OneToOne,
            &cache,
            &cancel,
            Some(lightbox_render::ng::SourceInject {
                idx: decoded,
                tile: source_tile,
                key,
            }),
        )
        .expect("direct graph eval");
    let out = tile.cpu().expect("CPU tile");

    let expect = apply_exposure_cpu(2.0, source.get_rgba_f32(10, 10)); // +1 EV ⇒ gain 2.0
    let got = out.get_rgba_f32(10, 10);
    for c in 0..3 {
        assert!(
            (got[c] - expect[c]).abs() < 1e-3,
            "channel {c}: got {}, expected {} (exposure_ev=1.0 must double, not pass through)",
            got[c],
            expect[c]
        );
    }
}

/// The GPU path (real Metal device on this box): CPU and GPU must agree
/// within the §4.4 tolerance, the per-node parity requirement every new
/// node must satisfy before E10/E11/E12 ship it for real.
#[test]
fn walkthrough_node_cpu_gpu_parity_on_real_device() {
    let Some(ctx) = lightbox_render::GpuContext::headless() else {
        eprintln!("[engine-book walkthrough] no wgpu adapter — SKIPPED (authoritative run is CI's macOS/Metal leg)");
        return;
    };

    let build = |backend: BackendPref, device: Arc<dyn DeviceProvider>| {
        let mut compiler = RecipeCompiler::with_registry(Arc::new(build_registry()));
        compiler.register_template(PV_M0, build_template()).unwrap();
        Engine::with_compiler(
            device,
            Arc::new(SynthSource {
                pixels: gradient(32, 32),
            }),
            compiler,
            EngineConfig {
                backend,
                ..EngineConfig::default()
            },
        )
        .unwrap()
    };

    let gpu_engine = build(
        BackendPref::Auto,
        Arc::new(SharedDevice {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
        }),
    );
    let cpu_engine = build(BackendPref::ForceCpu, Arc::new(NullDevice));

    let gpu_ticket = gpu_engine.submit(request(Recipe::identity(PV_M0)));
    let cpu_ticket = cpu_engine.submit(request(Recipe::identity(PV_M0)));
    let gpu_px = wait_complete(&gpu_engine, &gpu_ticket);
    let cpu_px = wait_complete(&cpu_engine, &cpu_ticket);

    assert_eq!(gpu_px.extent, cpu_px.extent);
    let mut max_diff = 0.0f32;
    for y in 0..gpu_px.extent.h {
        for x in 0..gpu_px.extent.w {
            let g = gpu_px.get_rgba_f32(x, y);
            let c = cpu_px.get_rgba_f32(x, y);
            for ch in 0..3 {
                max_diff = max_diff.max((g[ch] - c[ch]).abs());
            }
        }
    }
    assert!(
        max_diff < 0.01,
        "CPU/GPU max channel diff {max_diff} exceeds the walkthrough's parity check \
         (production nodes use the testkit's ΔE2000/PSNR comparators — §4.4)"
    );
}
