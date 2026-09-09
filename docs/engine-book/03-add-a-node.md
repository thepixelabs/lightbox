# Walkthrough: adding a node

This walks through `crates/lightbox-render/tests/engine_book_walkthrough.rs`
section by section. That file **compiles and passes**, run it yourself:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p lightbox-render --test engine_book_walkthrough
```

Three tests, three angles on the same node:

- `walkthrough_node_renders_on_the_cpu_path`, headless, always runs, checks
  the schema default.
- `walkthrough_node_honors_a_non_default_param`, drives the graph directly
  with a real (non-default) param, proving the param ABI actually reaches
  the kernel.
- `walkthrough_node_cpu_gpu_parity_on_real_device`, the CPU/GPU parity gate,
  on whatever adapter this machine has (skips honestly if none, never fakes
  a GPU number).

We build `tone.exposure`: one input, one param (`exposure_ev`, an EV stop
count, default `0.0`), output = input × 2^exposure_ev. A real point-op node,
end to end, the same shape E10's actual `tone.exposure` will eventually take
(this file is test-only, it doesn't ship).

## Step 1, the WGSL kernel

```wgsl
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
```

Follows the §3.2 convention exactly: input at group 0, write-only output
storage at group 1, params UBO at group 2. `exp2` is a WGSL builtin so the
GPU kernel and the CPU twin below do bit-for-bit-equivalent math (`2^x` vs
`exp2(x)`).

## Step 2, the CPU parity twin

```rust
fn apply_exposure_cpu(gain: f32, p: [f32; 4]) -> [f32; 4] {
    [p[0] * gain, p[1] * gain, p[2] * gain, p[3]]
}
```

Deliberately the simplest possible function, the GPU kernel computes
`exp2(stops)` once per texel (cheap, no precomputation worth doing); the CPU
side precomputes `2f32.powf(stops)` once per eval and reuses it. Same
algorithm, different but equivalent phrasing, that's normal and is exactly
what the parity gate (not a source-diff) verifies.

## Step 3, the param schema

```rust
static EXPOSURE_SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "exposure_ev",
    kind: ParamKind::Float,
}]);
```

A real (non-`EMPTY`) schema: constructing a `ParamBlock` with an unknown
field name or wrong type for this node now fails validation instead of
silently accepting garbage.

## Step 4, the descriptor

```rust
static EXPOSURE_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("tone.exposure"),
    inputs: &[PortDecl { name: "in", ty: PortType::LinearRgbaF16 }],
    output: PortDecl { name: "out", ty: PortType::LinearRgbaF16 },
    params_schema: ParamsSchemaRef(&EXPOSURE_SCHEMA),
};
```

One input port, one output port, both the working format
(`LinearRgbaF16`), a point-op never needs `LinearRgbaF32` or `WeightR16`.

## Step 5, the node

```rust
struct ExposureNode {}

impl RenderNode for ExposureNode {
    fn descriptor(&self) -> &NodeDescriptor { &EXPOSURE_DESC }
    fn eval_gpu(&self, ctx: &mut GpuEvalCtx<'_>, inputs: &[TileView<'_>], params: &ParamBlock)
        -> Result<(), NodeError> { /* build UBO, bind groups, dispatch_compute */ }
    fn eval_cpu(&self, ctx: &mut CpuEvalCtx<'_>, inputs: &[CpuTileView<'_>], params: &ParamBlock)
        -> Result<(), NodeError> { /* apply_exposure_cpu per pixel */ }
}
```

Everything else, `plan`, `output_extent`, `cache_policy`, `affected_by`,
`precision`, `aux_requirements`, takes the trait default: identity ROI
(exposure is a point-op, no neighborhood read), identity output shape,
`Cache`, conservative `true`, `F16`, no scratch/feedback. A neighborhood node
would override `plan`; see `BlurRProbe` in the testkit for that shape.

The `eval_gpu` body follows the same three-bind-group pattern every
engine-owned node uses (`src.decoded`, `util.resize`, `xform.display`, read
any of them in `crates/lightbox-render/src/ng/nodes/`): acquire the output
view, build a UBO from the params, get the cached pipeline via
`ctx.kernels.compute_pipeline`, build three bind groups, call
`dispatch_compute`.

## Step 6, the factory

```rust
struct ExposureFactory {}
impl NodeFactory for ExposureFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> { Arc::new(ExposureNode::default()) }
    fn kernel_salt(&self) -> KernelSalt { KernelSalt(blake3::hash(EXPOSURE_WGSL.as_bytes())) }
}
```

The salt is a hash of the shader source, change the WGSL (or bump a version
marker if you also change `eval_cpu` without touching WGSL text), and every
cached tile for this node invalidates automatically, everywhere, because the
salt is part of the cache key.

## Step 7, register it and drive it through the real `Engine`

```rust
let mut reg = NodeRegistry::new();
reg.register(SrcDecodedNode::ID, PvRange::from_open(PV_M0), Arc::new(SrcDecodedFactory::default()))?;
reg.register(NodeId("tone.exposure"), PvRange::from_open(PV_M0), Arc::new(ExposureFactory::default()))?;

let template = GraphTemplate::linear(vec![SrcDecodedNode::ID, NodeId("tone.exposure")]);
let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
compiler.register_template(PV_M0, template)?;

let engine = Engine::with_compiler(device_provider, source_provider, compiler, cfg)?;
let ticket = engine.submit(RenderRequest { image, recipe: Recipe::identity(PV_M0), pv: PV_M0, .. });
// poll(&ticket) until Complete(RenderOutput { payload: OutputPayload::Pixels(px), .. })
```

This is the **exact same shape** `lightbox-core`'s `Session::open` uses to
wire the real PV1 template (`crates/lightbox-core/src/session.rs`), three
factories registered, one linear template, one `Engine::new`/`with_compiler`
call. Adding a node to a slot really is data, not an engine change.

## Step 8, proving the param actually does something

`Recipe::identity(pv)` carries no per-node params at M1 (E09 owns the real
recipe schema), so driving the graph through `Engine::submit` with the
identity recipe only ever exercises `tone.exposure`'s **default**
(`exposure_ev = 0.0`, gain 1.0, output equals input). To prove the param
wiring itself works, drive the graph one layer down, the same technique
`lightbox-render-testkit`'s scenario harness uses:

```rust
let mut graph = RenderGraph::new();
let decoded = graph.add_node(Arc::new(SrcDecodedNode::new()));
let exposure = graph.add_node_with_params(
    Arc::new(ExposureNode::default()),
    ParamBlock::from_fields([("exposure_ev", ParamValue::Float(1.0))])?,
);
graph.connect(decoded, exposure, "in")?;
let tile = Executor::new(Arc::new(CpuBackend::new(None)))
    .evaluate(&graph, PV_M0, roi, RenderScale::OneToOne, &cache, &cancel, Some(source_inject))?;
```

+1 EV must exactly double every channel, and it does (the test asserts it).
This is also the pattern E10 will actually use once real recipe params
exist: the compiler will build exactly this kind of `ParamBlock` from the
recipe fragment for your node's stage.

## What this walkthrough does *not* cover

- **A neighborhood node's apron.** `plan()` override + the C3 tiled-equals-
  untiled gate, read `BlurRProbe` in the testkit
  (`crates/lightbox-render-testkit/src/probes.rs`) instead; it's a complete,
  gated, worked example.
- **A generator node** (zero inputs, like `src.decoded`/`test.checker`)
  see `nodes::decoded::SrcDecodedNode` or `probes::CheckerProbe`.
- **Committing a real golden.** This walkthrough asserts pixel values
  directly rather than checking in a PNG, to keep it self-contained; see
  `02-testing-and-gates.md` for the real golden workflow your actual node
  needs.
