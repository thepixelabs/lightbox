# E05 — Render node-graph engine

| | |
|---|---|
| **Epic id / slug** | E05 / `render-node-graph-engine` |
| **Milestone** | M1 |
| **Effort** | XL, ~10–16 pw, **2 engineers (≥1 GPU-depth)** — per §10/§10.1 staffing commitment |
| **Depends on** | E01 (workspace, catalog, Engine seed + shared-device handoff), E02 (decode & color foundation — source pixels + color transforms) |
| **Consumed by** | E10 (global toolset nodes), E11 (detail/optics/demosaic nodes), E12 (mask engine), E15 (export renders), E08 (canvas), E09 (recipe, at render time) |
| **Architecture anchors** | §0 bet #2 · §1.3 · §2.2 `lightbox-render` · §2.3 seams 2/5 · §4 (all) · §5.1/§5.2 · §6 device-lost row · §7 budgets · §8 golden/perf gates · §10.1 E05.1–E05.5 · Risk 4, Risk 6 |
| **Status** | Spec — ready for implementation |

This spec implements the architecture as approved. It refines the illustrative §2.2 signatures (fallible returns, ROI planning, explicit contexts) without changing the contract; anywhere this document and `01-architecture.md` appear to disagree, the architecture wins and the spec has a bug.

---

## 1. Scope

E05 delivers the **engine substrate** — the thing every pixel of Develop, export, and 1:1 preview flows through for the life of the product, and the mechanism behind the <100 ms slider-to-screen budget:

1. **Graph & trait spine (E05.1).** Generalize the one-node `Engine` seed E01 stood up at M0 into the full spine: petgraph DAG with typed image ports, the `RenderNode` trait (GPU + CPU eval, ROI planning, cache policy), the `NodeRegistry`, the `Engine::submit`/`poll`/`cancel` async lifecycle on the shell's shared `Arc<wgpu::Device>`, and the **recipe→graph compiler** framework with the PV1 pipeline template (§4.1 stage order).
2. **Content-keyed cache & tail invalidation (E05.2).** Node-output cache keyed by `hash(node_id, pv, input_hashes, params, tile, scale)`; one param change recomputes only that node and its downstream tail. Latest-wins coalescing of slider events in the render scheduler.
3. **ROI / tile / progressive evaluation (E05.3).** Fit-view evaluation at preview resolution over the visible ROI; 256² tiling for 1:1 with apron-correct neighborhood handling; the progressive ladder (existing preview tier → preview-res render → full-res on idle); visible-first tile scheduling; VRAM budgeting so a 100 MP render stays bounded.
4. **Process-version registry (E05.4).** `(node_id, process_version)` keying, append-only forever; per-PV graph templates; the per-PV golden-image immutability guard wired as a PR-blocking CI gate.
5. **Device-lost recovery + CPU fallback (E05.5).** wgpu device-lost detection → rebuild/re-upload → on recurrence degrade to **preview-resolution CPU editing** (the §4.4 degraded-but-usable contract); `eval_cpu` executor (rayon over tiles); CPU/GPU parity within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; backend provenance on every output.

E05 also ships the **shared test infrastructure** the whole pixel stream reuses: the golden-image harness (corpus manifest, ΔE2000/PSNR comparator, per-PV matrix), the recompute-count probe, and the slider-latency scenario harness — plus the **node-author guide** that E10/E11/E12 build against.

**Engine-owned scaffold nodes** (infrastructure, not develop tools): `src.decoded` (source injection), `util.resize` (decimation/Lanczos for scale ladders), `xform.display` (working→display, generalized from E01's M0 node, color math supplied by `lightbox-color`), and test-only probe nodes (`test.gain`, `test.blur_r`, `test.accum`) used by the cache/ROI/parity gates.

### 1.1 Explicit non-goals

| Not in E05 | Owner | E05's obligation |
|---|---|---|
| Basic-panel / tone / HSL / curve **user-facing nodes** and their algorithms | E10 | trait, kernel conventions, param ABI, per-node golden requirements documented (F3) |
| Halo-free highlight/shadow recovery | E10.2 | multi-pass-within-node + f32 precision hint + `FeedbackSlot` mechanisms exist and are tested |
| Demosaic (clean-room or interim), sharpen/NR, lens/geometry nodes | E11 | `MosaicU16` port type reserved; `output_extent` supports coordinate-frame changes (crop/rotate) |
| Mask component evaluation, boolean compositing, per-mask sub-recipes | E12 | `WeightR16` port type + multi-input edges + the LOCAL stage slot in the PV template |
| Raw/image **decode**, camera-matrix/DCP color math, LCMS2 transforms | E02 | consumed via `SourceProvider` and node params; engine contains **zero color science** |
| Preview pyramid, raw cache, T2 persistence **storage** | E03 | consumed via `SourceProvider`; produced tiles offered via `TileSink` |
| Job scheduler, cancel tokens, activity-center model | E06 | engine threads `CancelToken` through everything; registers Interactive-class work |
| Recipe schema, history, XMP | E09 | engine is a **read-only consumer** of `Recipe`; never persists edit state |
| Export encoders, sizing, watermark | E15 | `RenderTarget::Buffer` + Batch priority + backend provenance + GPU time-slicing mechanism |
| Canvas widgets, zoom/pan UX, develop UI | E08 | `CanvasFrame` subscription + double-buffered texture handoff |
| ML inference, `lightbox-inferd` | E13/E14 | none — AI masks arrive later as baked rasters (§4.5), i.e. plain `WeightR16` inputs |
| GPU-accelerated export *scheduling policy* | E15 | E05 provides the preemption **mechanism** (tile-granularity Interactive-over-Batch); E15 sets policy |
| HDR/EDR output, soft proofing | v1.x / E15 | none |

**No re-litigation:** wgpu/WGSL, petgraph, in-process render on the shared device, RGBA16F working tiles, and the ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB consistency contract are decided (§1.3, §4.2, §4.4, §5.2). This spec implements them.

---

## 2. Crates & modules touched

Per the §2.1 decomposition:

```
lightbox-render/                     ← E05 owns this crate (E01's seed generalizes in place)
  src/
    lib.rs            Engine, EngineConfig, public API, EngineEvent
    graph/            RenderGraph (petgraph), typed ports, topology validation
    compile/          RecipeCompiler, per-PV GraphTemplates (PV1), SourceDesc
    node/             RenderNode trait, NodeDescriptor, ParamBlock/ParamHash, NodeRegistry, PvRange
    exec/             Executor: topo walk, tile scheduler (visible-first), priority lanes
    exec/gpu/         GPU backend: encoder mgmt, dispatch, readback
    exec/cpu/         CPU backend: rayon tile executor (preview-res contract)
    cache/            CacheKey, VRAM tier (LRU texture cache), RAM pin tier
    gpu/              DeviceCtx wrapper, KernelBuilder, pipeline cache, TilePool (RGBA16F/RGBA32F)
    sched/            RenderScheduler: latest-wins coalescing, ViewState, CanvasFrame publisher
    recover/          device-lost state machine, rebuild, degradation policy
    source/           SourceProvider trait, upload path, progressive ladder
    nodes/            engine-owned scaffold nodes (src.decoded, util.resize, xform.display)
    stats.rs          EngineStats, recompute-count probe, tracing spans
  shaders/            WGSL kernels (include_str!-embedded; naga-validated at build)

lightbox-render-testkit/             ← new dev-only crate (workspace member, not shipped)
    golden harness (corpus manifest, GoldenCase runner, per-PV matrix)
    comparators (ΔE2000 mean/p99/max, PSNR) — self-contained f64 Lab reference impl
    probe nodes (test.gain, test.blur_r, test.accum, test.checker)
    scenario harness (slider-latency p95, soak)
```

**Small, named touches outside the crate** (each ≤ a handful of lines, PR'd with the owning team):
- `lightbox-core`: wire `RenderScheduler` into the session façade replacing E01's single-node canvas path (task F5); expose `EngineEvent` to the shell notification surface.
- `lightbox-cli`: `render` subcommand (image + recipe JSON + pv → PNG) for the headless E2E harness (§8).
- `lightbox-jobs` (E06): consume `Class::Interactive` registration + `CancelToken` — no API additions expected; if the token type needs a `is_cancelled_relaxed()` fast path for per-tile checks, that lands in E06's crate by agreement.
- `Cargo.toml` workspace: add `blake3`, `half`, `rayon`, `wide` (all MIT/Apache — cargo-deny surface-1 clean; no native/FFI additions, so surfaces 2–3 are untouched by E05).

**E05 does not touch:** `lightbox-catalog` schema, `lightbox-decode`, `lightbox-color` internals, `lightbox-edit`, `lightbox-mask`, `lightbox-meta`, `lightbox-export`, `lightbox-inferd`.

---

## 3. Interface definitions

The contract, in compile-shaped Rust. Names below are normative for E05 and for node-authoring epics.

### 3.1 Core types

```rust
// ── identity ────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub &'static str);            // dotted namespace: "xform.display", "tone.exposure"

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ProcessVersion(pub u16);             // PV1 == ProcessVersion(1); stored per image (edit_recipe.pv, E01 schema)

pub use lightbox_catalog::ImageId;              // i64 newtype from E01

// ── geometry (source-pixel coordinate frame unless stated) ─────────────────
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent { pub w: u32, pub h: u32 }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Roi { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }
impl Roi {
    pub fn expand(&self, margin: u32) -> Roi;                 // apron growth for neighborhood nodes
    pub fn intersect(&self, other: &Roi) -> Option<Roi>;
    pub fn tiles(&self, size: u32) -> impl Iterator<Item = TileCoord>;  // 256² default
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileCoord { pub tx: u32, pub ty: u32, pub scale_q: u16 }     // scale quantized (see 3.5)

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RenderScale {
    Fit(Extent),        // engine derives decimation so output ≤ viewport (≤ ~8 MP for 45 MP @ 4K fit)
    Ratio(f32),         // explicit fraction of source resolution, (0,1]
    OneToOne,           // 1:1 zoom — tiled, demand-driven
}

// ── port typing (edges type-checked at graph build) ─────────────────────────
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortType {
    MosaicU16,      // reserved for E11 (GPU demosaic); no v1 engine node produces it
    LinearRgbaF16,  // the working format: scene-linear, ProPhoto primaries, RGBA16F tiles (§4.2)
    LinearRgbaF32,  // precision escape hatch (accumulation-heavy stages, §4.2)
    WeightR16,      // grayscale mask weight buffers (E12 / baked AI rasters §4.5)
    DisplayRgba8,   // post-display-transform, what the canvas composites
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TilePrecision { F16, F32 }
```

### 3.2 `RenderNode` — the node-author contract

Refines the §2.2 sketch: fallible eval, explicit contexts, ROI back-propagation (`plan`), and coordinate-frame support (`output_extent`) so E11.4 geometry nodes fit without a trait break.

```rust
pub struct NodeDescriptor {
    pub id: NodeId,
    pub inputs: &'static [PortDecl],       // name + PortType; multi-input supported (mask compositing)
    pub output: PortDecl,                  // exactly one output port in v1 (multi-output: see open Q5)
    pub params_schema: ParamsSchemaRef,    // typed schema; drives ParamBlock validation + hashing
}

pub trait RenderNode: Send + Sync {
    fn descriptor(&self) -> &NodeDescriptor;

    /// ROI back-propagation: input regions required to produce `out` at `scale`.
    /// Default: identity. A radius-r neighborhood node returns out.expand(ceil(r * scale)).
    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois;

    /// Output extent given input extents — identity by default; crop/rotate (E11.4) override.
    fn output_extent(&self, inputs: &[Extent], params: &ParamBlock) -> Extent;

    /// GPU evaluation: record compute dispatches into ctx; write ctx.output().
    /// MUST be apron-correct: (tiled eval == whole-image eval) exactly, same backend.
    fn eval_gpu(&self, ctx: &mut GpuEvalCtx<'_>, inputs: &[TileView<'_>], params: &ParamBlock)
        -> Result<(), NodeError>;

    /// CPU evaluation: same algorithm spec as the WGSL kernel (§4.4). rayon-safe, allocation-light.
    /// Parity gate: agrees with eval_gpu within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB.
    fn eval_cpu(&self, ctx: &mut CpuEvalCtx<'_>, inputs: &[CpuTileView<'_>], params: &ParamBlock)
        -> Result<(), NodeError>;

    /// Cache behavior. Default Cache. Passthrough nodes (pure reorder) may return Never.
    fn cache_policy(&self) -> CachePolicy { CachePolicy::Cache }

    /// Fast-path invalidation hint (§2.2 `invalidates`): does this delta change output?
    /// Default true (conservative). Content keys make correctness independent of this hint.
    fn affected_by(&self, delta: &ParamDelta) -> bool { let _ = delta; true }

    /// Output tile precision. Default F16; F32 for accumulation-sensitive stages.
    fn precision(&self) -> TilePrecision { TilePrecision::F16 }

    /// Multi-pass nodes (guided filter, E10.2) request scratch/persistent slots here.
    fn aux_requirements(&self, params: &ParamBlock) -> AuxRequirements { AuxRequirements::NONE }
}

pub struct AuxRequirements {
    pub scratch_tiles: u8,                  // ping-pong intermediates within one eval
    pub feedback_slot: Option<PortType>,    // previous-invocation output (vkdt feedback pattern)
    pub lut_slots: u8,                      // small read-only textures/UBOs (curves, HueSat LUTs)
}

pub trait NodeFactory: Send + Sync {
    fn instantiate(&self) -> Arc<dyn RenderNode>;
    fn kernel_salt(&self) -> KernelSalt;    // hash of WGSL source + algorithm rev; part of CacheKey
}
```

**Kernel conventions (normative for node authors, documented in the engine book — task F3):**
- One WGSL entry point per pass, workgroup size 16×16×1; `@group(0)` input texture bindings, `@group(1)` output storage texture (write-only; read-write storage is not portable across backends — ping-pong via `scratch_tiles`), `@group(2)` params UBO (std140-compatible layout emitted from the `ParamBlock` schema), `@group(3)` LUT/aux bindings.
- Shaders embedded via `include_str!`, validated by naga at build time (task A6); no runtime shader downloads, no `f16` arithmetic in WGSL v1 (storage is `rgba16float`, compute in f32 — see Risk R2).

### 3.3 Registry & process versions

```rust
#[derive(Clone, Copy, Debug)]
pub struct PvRange { pub from: ProcessVersion, pub to_inclusive: Option<ProcessVersion> } // None = open

pub struct NodeRegistry { /* append-only map: (NodeId, pv) → factory */ }
impl NodeRegistry {
    /// Append-only. Registering an overlapping (id, pv) range is an error — old PVs are never
    /// replaced (§4.5). Improving an algorithm = new NodeId impl registered for a NEW pv range.
    pub fn register(&mut self, id: NodeId, pvs: PvRange, f: Arc<dyn NodeFactory>)
        -> Result<(), RegistryError>;
    pub fn resolve(&self, id: NodeId, pv: ProcessVersion) -> Option<Arc<dyn RenderNode>>;
    pub fn supported_pvs(&self) -> Vec<ProcessVersion>;
}
```

### 3.4 Recipe compilation

```rust
/// What the compiler needs to know about the source, independent of decode (E02 seam).
pub struct SourceDesc {
    pub image: ImageId,
    pub full_extent: Extent,
    pub source_kind: SourceKind,      // Raw { cfa } | Rgb — v1 templates take Rgb (decode is upstream)
    pub colorimetry: SourceColorimetry,
}

pub struct GraphTemplate { /* per-PV stage layout: §4.1 order, with LOCAL/retouch slots */ }

pub struct RecipeCompiler { /* holds templates keyed by ProcessVersion */ }
impl RecipeCompiler {
    /// Recipe (read-only, from lightbox-edit) → executable DAG.
    /// Unknown pv → CompileError::UnsupportedPv (NEVER silently falls back to latest — §4.5).
    /// Recipe fields with no node in this pv's template → CompileError::UnknownStage.
    /// Stages at identity/default params still compile (cache keys make them ~free) — v1 keeps
    /// topology stable per (recipe schema, pv) so param drags never restructure the graph.
    pub fn compile(&self, recipe: &Recipe, pv: ProcessVersion, src: &SourceDesc)
        -> Result<RenderGraph, CompileError>;
}
```

**PV1 template (M1, this epic):** `src.decoded → util.resize(scale) → [E10 tone-stage slots, empty at E05-close] → xform.display`. The template declares *slots* for every §4.1 stage (tone, detail, optics, LOCAL, retouch, effects) in order; E10/E11/E12 fill slots by registering nodes — **adding a node to a slot is data, not an engine change.**

### 3.5 Cache

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(pub blake3::Hash);
// key = H(node_id ‖ pv ‖ kernel_salt ‖ param_hash ‖ input_keys[] ‖ tile_coord ‖ scale_q ‖ precision)
//
// - param_hash: blake3 over canonical-CBOR ParamBlock (field-order independent; -0.0 → 0.0; NaN rejected
//   at ParamBlock construction). Stable across builds.
// - kernel_salt: changes when the WGSL/CPU algorithm changes → old cached outputs can never be
//   served for a new kernel. Under §4.5 discipline a salt change for a *released* pv trips the
//   per-PV golden gate (task D4).
// - scale_q: RenderScale quantized to 1/64ths so micro zoom jitter doesn't shred the cache;
//   OneToOne and Fit ladders land on distinct quanta.
// - input_keys: upstream CacheKeys — a param change on node k gives new keys for k..n (the tail)
//   while 1..k-1 stay hittable. Tail invalidation IS key propagation; there is no dirty-bit walk.

pub struct NodeCache { /* VRAM tier + RAM pin tier */ }
impl NodeCache {
    pub fn get(&self, k: &CacheKey) -> Option<CachedTile>;          // pins while in-flight
    pub fn put(&self, k: CacheKey, t: TileHandle, cost: Bytes);     // LRU-evicts to budget
    pub fn pin_ram(&self, k: CacheKey, label: PinLabel);            // e.g. post-source, survives device loss
    pub fn stats(&self) -> CacheStats;                               // hits/misses/evictions/bytes
}

pub enum CachePolicy { Cache, Never }
```

### 3.6 Engine lifecycle

```rust
pub struct EngineConfig {
    pub tile_size: u32,                 // default 256 (§4.3)
    pub vram_budget: VramBudget,        // Auto (probe: min(60% adapter mem, cap)) | Bytes(u64)
    pub cpu_threads: Option<usize>,     // rayon pool size; None = rayon default
    pub backend: BackendPref,           // Auto | ForceCpu (prefs/debug) — bound by E08 prefs panel
    pub device_lost_degrade: DegradePolicy { pub max_losses: u8 /*2*/, pub window: Duration /*60s*/ },
}

pub struct Engine { /* device ctx, registry, compiler, cache, executor, recover sm */ }
impl Engine {
    pub fn new(dp: Arc<dyn DeviceProvider>, sp: Arc<dyn SourceProvider>,
               registry: NodeRegistry, cfg: EngineConfig) -> Result<Engine, EngineInitError>;

    pub fn submit(&self, req: RenderRequest) -> RenderTicket;      // never blocks; validation errors
                                                                   // surface via poll → Failed
    pub fn poll(&self, t: &RenderTicket) -> RenderState;
    pub fn cancel(&self, t: &RenderTicket);                        // cooperative; tile-boundary latency
    pub fn events(&self) -> broadcast::Receiver<EngineEvent>;      // device-lost / degraded / vram
    pub fn active_backend(&self) -> ActiveBackend;                 // Gpu(AdapterInfo) | CpuPreviewOnly
    pub fn supported_pvs(&self) -> Vec<ProcessVersion>;
    pub fn stats(&self) -> EngineStats;                            // incl. recompute-count probe
}

pub struct RenderRequest {
    pub image: ImageId,
    pub recipe: Recipe,                  // materialized view (lightbox-edit, §3.1 ownership note)
    pub pv: ProcessVersion,
    pub roi: Roi,
    pub scale: RenderScale,
    pub target: RenderTarget,
    pub priority: RenderPriority,        // Interactive | Batch (maps to E06 Class semantics)
    pub cancel: CancelToken,             // from lightbox-jobs (E06)
}

pub enum RenderTarget {
    Canvas,                              // engine-owned double-buffered texture pair, same device
    Buffer { format: OutFormat },        // CPU readback: export (E15), preview build (E03), tests
}

pub enum RenderState {
    Queued,
    Rendering { tiles_done: u32, tiles_total: u32 },
    PreviewReady(RenderOutput),          // progressive intermediate (§4.3 ladder)
    Complete(RenderOutput),
    Failed(RenderError),
    Cancelled,
}

pub struct RenderOutput {
    pub payload: OutputPayload,          // CanvasGeneration(u64) | Pixels(PixelBuf)
    pub colorimetry: OutputColorimetry,
    pub backend: BackendId,              // recorded by export per §4.4 ("export records which backend")
    pub pv: ProcessVersion,
    pub quality: OutputQuality,          // PreviewTier | PreviewRes | FullRes — badge-able by shell
}
```

### 3.7 Render scheduler (the coalescing front-end, §4.3/§5.1)

```rust
pub struct ViewState { pub viewport: Extent, pub zoom: Zoom, pub pan: Roi }

pub struct RenderScheduler { /* owns latest-wins slots per image; one in-flight + one pending */ }
impl RenderScheduler {
    pub fn new(engine: Arc<Engine>, jobs: JobsHandle) -> RenderScheduler;
    pub fn set_recipe(&self, image: ImageId, recipe: Recipe, pv: ProcessVersion); // latest-wins debounce
    pub fn set_view(&self, image: ImageId, view: ViewState);                      // roi/zoom churn
    pub fn canvas(&self) -> tokio::sync::watch::Receiver<CanvasFrame>;            // shell subscribes
}

pub struct CanvasFrame {
    pub generation: u64,                 // shell samples only completed generations (double-buffer)
    pub texture: wgpu::TextureView,      // same-device, zero-copy (§2.3 seam 2)
    pub quality: OutputQuality,          // drives the "Loading"/preview badge (§4.3)
    pub extent: Extent,
}
```

### 3.8 Seam traits (implemented by neighbors, consumed by E05)

```rust
/// E01/E08 (shell owns the device): current handles + coordinated rebuild on device-lost.
pub trait DeviceProvider: Send + Sync {
    fn current(&self) -> (Arc<wgpu::Device>, Arc<wgpu::Queue>);
    fn rebuild(&self) -> BoxFuture<'static, Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), DeviceError>>;
}

/// E02/E03: the engine never decodes and never reads the preview store directly.
pub enum SourceWant { BestAvailable { max_px: u32 }, DecodedFull, CachedRawState }
pub struct SourceImage { pub pixels: PixelBuf, pub colorimetry: SourceColorimetry,
                         pub full_extent: Extent, pub quality: SourceQuality }
pub trait SourceProvider: Send + Sync {
    fn fetch(&self, image: ImageId, want: SourceWant, cancel: &CancelToken)
        -> BoxFuture<'static, Result<SourceImage, SourceError>>;
}

/// E03: completed 1:1 tiles offered for T2 persistence (fire-and-forget; E03 owns store + eviction).
pub trait TileSink: Send + Sync {
    fn offer_t2(&self, image: ImageId, recipe_hash: blake3::Hash, tile: TileCoord, px: PixelBuf);
}
```

**Interim reality at M1 (stated so no planner guesses):** `SourceProvider::DecodedFull` returns **already-demosaiced RGB** from E02 (rawler / interim LibRaw path, CPU) — the v1 graph's source port is `LinearRgbaF16`, and `MosaicU16` stays dormant until E11 moves demosaic onto the graph. The engine's <100 ms budget is slider-to-screen with a warm source (raw cache, E03); first-open cost is decode-bound and covered by the §4.3 progressive ladder, not by this epic.

---

## 4. Data model — additions & migrations

**None. E05 adds no catalog tables, columns, or migrations.** Stated explicitly, per spec requirements:

- `edit_recipe.pv` (the per-image process version, §3.1/§4.5) already exists in E01's schema v1; E05 only reads it via the `RenderRequest`.
- The node cache is **in-VRAM/in-RAM only** — deliberately non-persistent; correctness never depends on it and a crash loses nothing.
- Persistent pixel state is owned elsewhere: partially-decoded raw state → `rawcache/` (E03, §3.3); rendered 1:1 tiles → `previews/<hash>.t2/` via `TileSink` (E03); baked AI rasters → `masks/` (E12/E14, §4.5).
- Backend provenance on exports is recorded by E15 in its export log from `RenderOutput.backend`; E05 defines the value, not the storage.
- The golden corpus + committed goldens live in the repo (Git LFS) under `lightbox-render-testkit/corpus/` — developer data, not user data model.

Any future need for a persistent node-output cache (e.g. disk-spilled intermediates) is a named non-goal here and would be an E03-owned store behind `TileSink`-style traits.

---

## 5. Ordered task breakdown

Each task ≤ 1 engineer-day. ~52 tasks ≈ 10–11 pw core work; the 10–16 pw envelope carries integration slack, platform debugging (WGSL/driver variance), and review. **Two-engineer split:** Eng-A (graph/cache/PV/compiler: phases A-core, B, D), Eng-B (GPU-depth: GPU ctx, tiling, backends, recovery: A6–A9, C, E); F shared. Order within a phase is the dependency order; phases overlap where noted.

### Phase A — E05.1 Graph & trait spine (16 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | Crate layout per §2 module map; error taxonomy (`EngineInitError`, `NodeError`, `CompileError`, `RenderError` w/ `thiserror`); feature flags (`gpu` default-on) | workspace builds + clippy-clean on macOS/Windows/Linux CI |
| A2 | Core types: `NodeId`, `ProcessVersion`, `Roi`/`Extent`/`TileCoord` algebra, `RenderScale`, `PortType`, `TilePrecision` | unit tests: roi expand/intersect/tiles; serde round-trip where derived |
| A3 | `ParamBlock` + schema: typed construction from recipe fragments, canonical-CBOR encoding, NaN rejection, `-0.0` normalization | property test (proptest): construction from permuted field order ⇒ identical canonical bytes |
| A4 | `RenderNode` trait + `NodeDescriptor` + `AuxRequirements` (as §3.2) with default impls; `test.gain` probe node (CPU only for now) | probe node compiles against trait; descriptor validation rejects port-type mismatch |
| A5 | `NodeRegistry` + `PvRange`: append-only registration, overlap detection, `resolve`, `supported_pvs` | unit: overlapping (id,pv) registration errors; resolve picks correct impl at range edges |
| A6 | `DeviceCtx` + `KernelBuilder`: bind-group layout conventions (§3.2), pipeline cache keyed by shader blake3, naga validation of all `shaders/` at build | trivial WGSL kernel writes constant; readback matches; invalid WGSL fails the build, not runtime |
| A7 | `TilePool`: rgba16float/rgba32float texture pool, alloc/free, budget accounting, LRU reclaim of free-list | stress test: 10k alloc/free cycles never exceed budget; zero wgpu validation errors |
| A8 | `RenderGraph` on petgraph: typed edges, topo sort, cycle rejection, multi-input support; structural equality helper for tests | building an ill-typed or cyclic graph returns `CompileError`, never panics |
| A9 | GPU executor v1: whole-ROI-as-one-tile topo walk, encoder per render, submit on shared queue | 2-node graph (`src.decoded → test.gain`) renders; readback equals CPU reference exactly (single backend determinism) |
| A10 | `SourceProvider` trait + upload path: 8-bit/16-bit/f32 `PixelBuf` → working-format texture (colorimetry tag passthrough; conversion math deferred to `xform` nodes) | uploads of all three depths round-trip a synthetic gradient losslessly (within format quantization) |
| A11 | `RecipeCompiler` + PV1 `GraphTemplate` with §4.1 slot layout; unknown-pv / unknown-stage typed errors; stable topology per (schema, pv) | fixed recipe compiles to expected topology (structural assert); pv=99 ⇒ `UnsupportedPv` |
| A12 | `Engine::new/submit/poll/cancel` + ticket store + render thread; `CancelToken` threaded to tile boundaries | submit returns <1 ms; poll walks Queued→Rendering→Complete; cancel mid-render yields `Cancelled` within one tile's work |
| A13 | Canvas double-buffer: texture pair + generation counter, `CanvasFrame` publisher; integration with E01's headless egui-wgpu compositor test rig | soak: shell-sim samples during continuous renders — zero validation errors, zero torn generations |
| A14 | CPU executor v1: `CpuTile` (f32 planes), rayon tile map, `test.gain` parity | gain node CPU vs GPU within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on corpus image |
| A15 | `EngineStats` + tracing: per-node eval spans, nodes_evaluated / cache_hits counters (the recompute probe) | counters assert-able from tests; `tracing` spans visible under `RUST_LOG` |
| A16 | **Testkit v1 + first golden (the §10.1 E05.1 gate):** corpus manifest format, self-contained f64 Lab/ΔE2000 + PSNR comparators, `GoldenCase` runner; commit first golden: single-node graph over a known input | **PR-blocking CI job: single-node golden within ΔE2000 ≤ 1.0**; comparator validated against published ΔE2000 test vectors |

### Phase B — E05.2 Content-keyed cache & tail invalidation (8 tasks; starts after A11)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `ParamHash`: blake3 over canonical `ParamBlock`; `KernelSalt` from `NodeFactory` | property: equal params ⇒ equal hash across processes/builds; salt change ⇒ key change |
| B2 | `CacheKey` derivation + propagation through the graph walk (as §3.5) | unit: param change at node k changes keys k..n and leaves 1..k−1 unchanged, for chains and diamonds |
| B3 | `NodeCache` VRAM tier: keyed map over `TilePool` handles, byte-cost LRU, in-flight pinning | budget never exceeded under randomized put/get fuzz; stats (hit/miss/evict) exact |
| B4 | RAM pin tier: pin post-`src.decoded` output (`PinLabel::SourceStage`); survives GPU cache eviction and device loss | after cold render, a param change performs **zero** `SourceProvider::fetch` calls (probe-asserted) |
| B5 | **Tail-invalidation gate (the §10.1 E05.2 test):** recompute-count probe test on an n-node chain | **PR-blocking: changing node k's params evaluates exactly (n−k+1) nodes; upstream all cache-hit** |
| B6 | `RenderScheduler` latest-wins coalescing: one in-flight + one pending slot per image; drop intermediate states | probe: 1 000 rapid `set_recipe` calls ⇒ ≤ 2 engine submissions after the in-flight completes; final frame reflects final params |
| B7 | Cache integrity under cancellation: cancelled evals never insert partial tiles; poisoning fuzz | fuzz with random cancels then a clean render ⇒ output equals never-cancelled reference exactly |
| B8 | Criterion micro-benches: tail-only recompute latency vs full-graph, cache hit rate under slider churn | benches run nightly; baseline recorded; regression opens tracked issue (§8 non-blocking) |

### Phase C — E05.3 ROI / tile / progressive (10 tasks; C1–C2 can start after A11)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | ROI planning: `plan()` back-propagation through the compiled graph; `test.blur_r` probe node with radius-dependent apron | property: composed aprons through a chain match analytic expectation; requested source ROI is minimal |
| C2 | Scale-aware eval: `RenderScale` → decimation at `util.resize` (Lanczos/box WGSL + CPU); scale plumbed into `plan`/eval ctxs | **§10.1 E05.3 gate: 45 MP source at `Fit(4K)` evaluates ≤ ~8 MP** (probe-counted pixels); resize node passes its own golden |
| C3 | 256² tile executor: ROI split, per-tile eval with apron reads from neighbor regions, seam-free stitch | tiled render **exactly equals** untiled render (same backend) for gain + blur probe graphs |
| C4 | Visible-first tile ordering (center-out) + off-screen tile cancellation on `set_view` | probe: tile completion order is center-out; a pan cancels not-yet-started off-screen tiles |
| C5 | 1:1 path: demand-driven tile eval keyed by `TileCoord`; completed tiles offered via `TileSink` (E03 seam, fire-and-forget) | pan at 1:1 evaluates only newly-visible tiles (probe); `TileSink` mock receives correct coords/hashes |
| C6 | Progressive ladder: `BestAvailable` tier → `PreviewReady` (display-transform only) → preview-res render → full-res on idle | headless test asserts the 3-stage `RenderState` sequence; with a warm preview store, first `PreviewReady` ≤ 100 ms on the reference machine |
| C7 | VRAM probe + `VramBudget::Auto`; tiling working-set degradation under low budget | synthetic 100 MP render under a 512 MB simulated budget completes within cap (peak tracked by pool accounting) |
| C8 | F32 precision path: `precision()` honored end-to-end (`rgba32float` tiles, cache-key inclusion); `test.accum` probe | accumulator probe at F32 matches f64 CPU reference ≥ 60 dB PSNR; same probe at F16 demonstrably worse (test documents why the hatch exists) |
| C9 | Priority lanes: Interactive preempts Batch at tile granularity (the E15 no-starvation mechanism) | with a Batch render in flight, an Interactive submit's first tile starts within one tile-duration; Batch still completes; both asserted headless |
| C10 | Soak: 10k randomized param/zoom/pan iterations over the corpus | zero validation errors, zero deadlocks, VRAM within budget throughout, final frame equals fresh render |

### Phase D — E05.4 Process-version registry (5 tasks; after B2)

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | PV plumbing end-to-end: `RenderRequest.pv` → compiler template + registry resolution; PV1 manifest (node ids + kernel salts) committed | rendering the same recipe under an unregistered pv fails typed; PV1 manifest file exists and is diff-reviewed |
| D2 | Per-PV `GraphTemplate` selection + a `pv-test` second template (test-only PV 999) exercising divergent topology | same recipe renders under PV1 and PV999 producing distinct committed goldens; no cross-PV cache pollution (key includes pv) |
| D3 | **Per-PV golden matrix:** corpus × recipe set × registered PVs; fast subset PR-blocking, full matrix nightly (§8) | **PR-blocking: any drift beyond ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on any registered PV fails the build** |
| D4 | Kernel-salt discipline + CI wiring: changing a shipped PV's kernel trips D3; contributor workflow documented (new algorithm ⇒ new PV) | verified once by intentional kernel tweak on PV999 (gate trips), then reverted; workflow doc merged |
| D5 | `Engine::supported_pvs()` + compile-under-different-pv support (the migrate-preview primitive for E09/E10 UX) | one recipe renders under two PVs in one session; outputs independently cached and golden-checked |

### Phase E — E05.5 Device-lost recovery + CPU fallback (8 tasks; E1 can start after A13)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | Detection: `device_lost` callback + uncaptured-error hook → `recover` state machine + `EngineEvent::DeviceLost`; test-only fault injector (forced device drop) | injected loss emits event; in-flight tickets resolve `Failed(DeviceLost)` (not hang) |
| E2 | Rebuild path via `DeviceProvider::rebuild()`: recreate `DeviceCtx`, pipeline cache, `TilePool`; invalidate VRAM cache tier cleanly | after injected loss, next submit renders successfully on the rebuilt device; no stale-handle validation errors |
| E3 | Re-warm: RAM-pinned source stage re-uploads without `SourceProvider` re-fetch; shaders recompile from embedded source | post-rebuild render performs zero fetches when pin present (probe); rebuild-to-first-frame < 2 s on reference machine |
| E4 | Degradation policy state machine: `max_losses` within `window` ⇒ `CpuPreviewOnly` + `EngineEvent::DegradedToCpu`; explicit re-enable API (prefs, E08) | policy unit-tested at boundaries (K−1 losses stays GPU; Kth degrades); re-enable restores GPU path |
| E5 | CPU backend completeness for engine-owned nodes at preview res: same scheduler, same ladder, rayon executor | **§10.1 E05.5 gate: injected device-lost mid-session ⇒ session keeps editing at preview resolution** (headless integration: param changes keep producing frames) |
| E6 | **CPU/GPU parity gate** for all engine-owned + probe nodes over the corpus | PR-blocking: parity within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (§4.4/§8); each backend individually deterministic across 3 repeat runs |
| E7 | Degraded-contract enforcement: on `CpuPreviewOnly`, Interactive full-res requests are clamped to preview-res with `OutputQuality` marking; full-res only as `Batch` with progress | contract tests; progress callbacks fire; no silent full-res CPU on the interactive path (§4.4 honesty clause) |
| E8 | Export-safety: submissions refused on a lost device (typed error, E15 retries after rebuild); `BackendId` provenance correct in all terminal states | test: Batch submit during lost-device window fails typed; provenance matches executing backend in mixed sessions |

### Phase F — Cross-cutting & M1 integration (5 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | Perf scenario harness: scripted slider churn at fit-view over corpus; p95 slider-to-screen measured end-to-end (scheduler in) | nightly on reference GPU runner: **p95 < 100 ms** with scaffold-node graphs; results time-series tracked; regression ⇒ tracked issue |
| F2 | Cross-platform CI: golden subset on macOS/Metal, Windows/DX12, Linux/Vulkan (lavapipe fallback where no HW GPU; see R4) | all three PR-blocking legs green; per-platform tolerances all within the single §4.4 tolerance (no platform carve-outs) |
| F3 | Engine book: node-author guide (trait, kernel conventions, param ABI, cache policy, apron rules, per-node golden + parity requirements, PV discipline) — the E10/E11/E12 on-ramp | reviewed by one prospective node author (E10 planner); a sample "add a node" walkthrough compiles |
| F4 | Fuzz/property hardening: compiler fuzz (arbitrary recipes ⇒ typed error or valid graph, never panic); concurrency stress on ticket store + cache | 24 h fuzz run clean; stress suite in CI (bounded) |
| F5 | M1 integration: `RenderScheduler` wired into `lightbox-core` session façade (replacing E01's single-node path); `lightbox-cli render` headless command | E2E: cli renders image+recipe+pv → PNG matching golden; shell canvas shows engine output with progressive badge states |

---

## 6. Test plan

Per the §8 strategy; E05 both consumes it and builds the shared pieces (testkit).

| Layer | What E05 tests | Gate |
|---|---|---|
| **Unit** | Roi/Extent algebra; ParamBlock canonicalization; registry PvRange edges; cache-key propagation (chains + diamonds); compiler error taxonomy; degradation state machine | PR-blocking |
| **Property (proptest)** | param-order-independent hashing; plan() apron composition; graph compile never panics on arbitrary recipes; tiled==untiled for apron-correct probes | PR-blocking |
| **Golden-image** | (a) first single-node golden (A16); (b) engine-owned node goldens (resize, display transform); (c) **per-PV matrix** (D3) — corpus × recipes × PVs, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; (d) **CPU-vs-GPU parity** (E6) same tolerance. Corpus: ≥ 6 sources — synthetic gradients/checker, low-key, high-key, high-frequency detail, wide-gamut saturated, 100 MP synthetic — committed via LFS with provenance (own-shot/CC0 only, §8 surface 3 applies to test data too) | fast subset PR-blocking; full matrix nightly |
| **Determinism** | 3× repeat renders per backend bit-identical for fixed (pv, engine build) — the §4.4 per-backend determinism claim | PR-blocking |
| **Fault injection** | forced device-lost (single + recurrent) → rebuild → degrade-to-CPU-preview; cancellation storms; cache-poisoning fuzz; lost-device export refusal | PR-blocking |
| **Perf** | criterion micro (tail recompute, cache hit, tile stitch overhead) + scenario harness (F1: p95 slider-to-screen < 100 ms fit-view; C6: first PreviewReady ≤ 100 ms warm; C9 no-starvation; C7 100 MP within VRAM budget) on the reference-GPU runner (§7 hardware class: RTX 3060 / M-series base) | nightly; regressions tracked, not blocking (§8) |
| **Integration / E2E** | `lightbox-cli render` headless (F5); scheduler+canvas soak with shell-sim (A13, C10) | PR-blocking (fast), full nightly |
| **Cross-platform** | golden subset + unit on macOS/Metal, Windows/DX12, Linux/Vulkan (F2) | PR-blocking on all three |
| **License** | cargo-deny over new deps (blake3, half, rayon, wide — all MIT/Apache); no new native artifacts ⇒ surfaces 2–3 unchanged | PR-blocking (existing gate) |

**What E05 explicitly does not test:** color-science correctness (E02 owns matrix/DCP/ICC goldens), decode fidelity (E02), preview-store persistence (E03), develop-tool visual quality (E10/E11 own their per-node goldens using this harness).

---

## 7. Risks & open questions

### Risks

| # | Risk | Likelihood/Impact | Mitigation in this spec |
|---|---|---|---|
| R1 | **Bet-#2 complexity underestimate** (Risk 4): cache/ROI/tiling interactions breed subtle correctness bugs (seams, stale tiles) | M / VH | tiled==untiled exact-equality gate (C3); cancellation-poisoning fuzz (B7); soak (C10); recompute probe makes cache behavior observable, not inferred |
| R2 | **WGSL/wgpu maturity**: no portable f16 arithmetic; read-write storage textures limited to r32 formats; subgroup support uneven | H / M | committed conventions: rgba16float *storage* + f32 *compute*, write-only outputs + ping-pong scratch (§3.2); no subgroup ops in engine kernels; per-node native-API escape hatch is the §1.3 named reversal, node-local |
| R3 | **Driver variance breaks the single tolerance** — a platform/driver combo exceeds ΔE2000 ≤ 1.0 vs goldens | M / H | goldens rendered from the CPU reference path; per-backend determinism tested separately from cross-backend tolerance; if a specific driver exceeds tolerance, that node drops to native per §1.3 — we do **not** widen the tolerance (it's the §4.4 contract) |
| R4 | **CI GPU availability**: hosted runners have no reliable Metal/DX12/Vulkan hardware; lavapipe (software Vulkan) differs numerically from HW | H / M | PR gates run lavapipe/WARP where HW is absent + the CPU path everywhere; **HW-GPU truth runs on a self-hosted reference runner nightly** (F1/F2); tolerance must hold on both — if lavapipe can't, HW-runner results are the gate of record (decision recorded in CI docs) |
| R5 | **Cache-key discipline erosion**: a param that changes output but escapes `ParamBlock` (hidden state) silently serves stale tiles | L / VH | rule: nodes are pure functions of (inputs, params, kernel_salt) — enforced by golden runs with cold vs warm cache compared (added to D3 runner); `aux` state (FeedbackSlot) is keyed explicitly |
| R6 | **Scheduler/shell frame-pacing interaction**: engine queue submissions contend with egui's render pass; canvas jitter | M / M | same-queue submission ordering + double-buffered generations (A13); soak test with shell-sim; Interactive tile granularity keeps encoder chunks small |
| R7 | **Preview-res ≠ full-res drift** for scale-dependent future nodes (sharpen/NR) makes fit-view lie about export | M / M | engine plumbs true scale into every eval (C2) and documents the resolution-independence requirement in the node-author guide (F3); scale-dependent quality is E11's per-node problem, but the *mechanism* ships here |
| R8 | **Two-engineer coupling**: Eng-A's compiler/cache and Eng-B's executor/tiling meet at the graph-walk interface; a seam mismatch stalls both | M / M | the `exec` ↔ `cache`/`compile` interface (CacheKey in, TileHandle out) is frozen end of week 2 (after A9/B2); integration tests (B5, C3) owned jointly |

### Open questions (with default answers so nothing blocks)

1. **Feedback-edge depth (E10.2/E12 needs).** v1 ships `AuxRequirements::feedback_slot` (previous-invocation output) + multi-pass scratch; full arbitrary feedback subgraphs (vkdt's general form) are **not** built until a concrete E10.2 kernel demands them. *Default: mechanism-only, revisit at E10.2 spike.*
2. **Multi-output nodes.** v1 descriptor allows exactly one output port; histogram/statistics taps (E10.4) would want a second. *Default: single output + a dedicated `stats.tap` sink-node pattern; widen the descriptor only if E10.4's histogram can't live with a tap node.*
3. **Reference CI hardware.** Which physical boxes back the nightly reference runner (one Windows/RTX 3060-class + one Apple-Silicon Mac is the assumed minimum)? *Owner: operator/CTO — needed before F1 lands; not a code blocker.*
4. **`Fit` scale quantization step** (1/64 assumed) vs cache hit rate under continuous zoom — tune with C10 soak data. *Default: 1/64, revisit with data.*
5. **RAM pin budget** for the source-stage pin on 100 MP sources (~800 MB at f16 full-res). *Default: pin at the active render scale, not full-res; full-res pins only during 1:1 sessions, LRU-capped at 2 GB.*
6. **T2 tile handoff format** (`PixelBuf` encoding before E03's JXL encode). *Default: engine hands raw RGBA8 post-display-transform; E03 owns encoding — confirm with E03 planner before C5.*

---

## 8. Definition of done

E05 is done when **all** of the following hold:

1. **The four §10.1 phase gates are green and PR-blocking in CI:**
   - E05.1 — single-node graph renders a known input to a committed golden within ΔE2000 ≤ 1.0 (A16).
   - E05.2 — one param change recomputes exactly that node + its downstream tail, asserted by the recompute-count probe (B5).
   - E05.3 — a 45 MP source at fit-view evaluates ≤ ~8 MP (C2).
   - E05.5 — an injected device-lost event keeps the session editing at preview resolution (E5).
2. **Per-PV golden immutability guard live** (D3/D4): the corpus × recipe × PV matrix runs (subset PR-blocking, full nightly) and a kernel change to a registered PV demonstrably fails the build.
3. **CPU/GPU parity + determinism gates green** (E6): every engine-owned and probe node within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB across backends; each backend bit-deterministic across repeat runs; backend provenance present on every `RenderOutput`.
4. **Perf evidence on reference hardware** (F1): p95 slider-to-screen < 100 ms at fit-view with scaffold graphs; first `PreviewReady` ≤ 100 ms warm-store; 100 MP render within the VRAM budget; Batch never starves Interactive (C9). Nightly time-series established.
5. **M1 integration shipped** (F5): the shell canvas renders through `RenderScheduler`/`Engine` (E01's single-node path deleted), progressive badges driven by `OutputQuality`, and `lightbox-cli render` produces golden-matching output headless.
6. **Seams honored:** zero develop-tool algorithms, color science, decode, or persistence in `lightbox-render`; all neighbor contact through `SourceProvider`/`TileSink`/`DeviceProvider`/`Recipe`/`CancelToken`; no UI types below the core boundary; **no catalog schema changes** (§4 of this spec).
7. **Node-author guide merged** (F3) and validated by an E10 planner walkthrough — E10.1 can start with no engine changes, only registrations.
8. **Cross-platform CI green** on macOS/Metal, Windows/DX12, Linux/Vulkan (F2); cargo-deny green with the new deps; no new native/FFI or bundled-data artifacts (surfaces 2–3 unchanged).
9. **Soak/fuzz clean** (C10, F4): 10k-iteration interactive soak and 24 h compiler fuzz with zero panics, validation errors, deadlocks, or budget breaches.
