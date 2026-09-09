# Node-author guide

Normative source: `docs/plan/epics/E05-render-node-graph-engine.md` §3.2
(the trait contract) and §3.2's "kernel conventions" box. This document
explains it; the spec is the contract of record if they ever disagree.

## The `RenderNode` trait, method by method

```rust
pub trait RenderNode: Send + Sync {
    fn descriptor(&self) -> &NodeDescriptor;
    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois { /* identity */ }
    fn output_extent(&self, inputs: &[Extent], params: &ParamBlock) -> Extent { /* identity */ }
    fn eval_gpu(&self, ctx: &mut GpuEvalCtx<'_>, inputs: &[TileView<'_>], params: &ParamBlock) -> Result<(), NodeError>;
    fn eval_cpu(&self, ctx: &mut CpuEvalCtx<'_>, inputs: &[CpuTileView<'_>], params: &ParamBlock) -> Result<(), NodeError>;
    fn cache_policy(&self) -> CachePolicy { CachePolicy::Cache }
    fn affected_by(&self, delta: &ParamDelta) -> bool { true }
    fn precision(&self) -> TilePrecision { TilePrecision::F16 }
    fn aux_requirements(&self, params: &ParamBlock) -> AuxRequirements { AuxRequirements::NONE }
}
```

- **`descriptor`**, a `&'static NodeDescriptor`. Almost always a `static`
  const value; see the walkthrough (`03-add-a-node.md`).
- **`plan`**, ROI back-propagation: given the output region you're asked to
  produce, what input region(s) do you need? **Default: identity** (a
  point-op reads exactly the region it writes). Override only for
  neighborhood ops, a radius-`r` blur needs `out.expand(ceil(r * scale))`.
  See `lightbox-render-testkit`'s `BlurRProbe` (`crates/lightbox-render-testkit/src/probes.rs`)
  for the worked apron example the C1 gate is built on. Get this wrong and
  your node produces seams at tile boundaries, that's exactly what the C3
  tiled-equals-untiled gate catches (§2 below).
- **`output_extent`**, output shape given input shapes. **Default:
  identity** (same shape as your first input). Override for crop/rotate
  (E11.4 geometry) or explicit-target-size nodes (`util.resize`).
- **`eval_gpu` / `eval_cpu`**, the actual work. Both are **mandatory**, no
  default. They must implement the **same algorithm**, the CPU/GPU parity
  gate (§4.4, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB) is not a suggestion, it is
  PR-blocking (`02-testing-and-gates.md`).
- **`cache_policy`**, `Cache` (default) or `Never` for pure-reorder
  passthrough nodes. You will almost never override this.
- **`affected_by`**, a *fast-path hint* only: "does this param delta change
  my output?" Getting it wrong (returning `false` when the answer is `true`)
  is a correctness bug (stale output survives a param change); getting it
  right just skips unnecessary recomputation. Default `true` (always
  conservative) is always safe, only add a real implementation if profiling
  shows it matters.
- **`precision`**, `F16` (default, the working format) or `F32` for
  accumulation-heavy stages (guided filter, deep local-contrast). See
  `test.accum` in the testkit for why the F32 escape hatch exists (C8).
- **`aux_requirements`**, multi-pass nodes (guided filter, feedback-driven
  local adjustments) request scratch tiles / a feedback slot here. Leave at
  `NONE` unless you actually need ping-pong intermediates or the previous
  invocation's output.

## Kernel conventions (normative)

- **One WGSL entry point per pass**, `@compute @workgroup_size(16, 16, 1)`.
- **Bind groups, contiguous from 0:**
  `@group(0)` input texture(s) (`texture_2d<f32>`, read-only) ·
  `@group(1)` output storage texture (`texture_storage_2d<rgba16float, write>`,
  **write-only**, read-write storage textures aren't portable across
  backends; ping-pong via `aux_requirements().scratch_tiles` if you need to
  read your own previous output) · `@group(2)` params UBO
  (std140-compatible layout hand-matched to your `ParamBlock` schema) ·
  `@group(3)` LUT/aux bindings (curves, HueSat tables, feedback slot).
- **No runtime shader downloads.** Shaders are `include_str!`-embedded
  strings, validated by naga **at build time**, `A6`'s acceptance criterion
  ("invalid WGSL fails the build, not runtime") is enforced by
  `crates/lightbox-render/build.rs` for every file under `shaders/`. If you
  add a shader file there, a typo is a compile error before you ever run a
  test. (The walkthrough embeds its WGSL as an inline `const &str` instead,
  since it's test-only and not meant to ship, see `03-add-a-node.md`.)
- **No `f16` arithmetic in WGSL.** Storage is `rgba16float`; compute in
  `f32` (Risk R2, wgpu/WGSL portability). No subgroup ops.
- Use `crate::ng::gpu::dispatch_compute`, the shared dispatch primitive
  every engine-owned node uses (binds groups 0..N, dispatches
  `ceil(out/16)` workgroups, submits). Don't hand-roll encoder/pass setup.

## The param ABI

`ParamBlock` is a validated, canonically-CBOR-encoded map (field-order
independent, `-0.0` normalized, `NaN` rejected at construction, spec §3.5).
Two ways to declare params:

- **`ParamsSchema::EMPTY`**, accept any well-typed fields, no validation.
  This is what every current engine-owned scaffold node uses (`src.decoded`,
  `util.resize`, `xform.display`) because they take no develop params.
- **A real schema**, `ParamsSchema::new(&[FieldDecl { name, kind }, ...])`.
  Construction then rejects unknown field names / type mismatches. **Use
  this for develop-tool nodes**, it's the contract your params UI (E10/E12)
  and the compiler agree on. See `03-add-a-node.md`'s `EXPOSURE_SCHEMA`.

Read a param with a default: `params.get_f64_or("exposure_ev", 0.0)` (also
`get_i64_or`/`get_bool_or`/`get_str_or`, grep `ng::node::param` for the
full accessor list). **Always supply a sane default**, at M1 the recipe
schema (E09) is still `{schema, pv}` with no per-node params, so every
compiled graph runs your node at its schema default until E09/E10 wire real
recipe fields through.

## Cache policy & the content key

You don't compute cache keys, the executor does, from
`(node_id, pv, kernel_salt, param_hash, input_keys, tile, scale, precision)`
(spec §3.5). What you control:

- **`KernelSalt`** (in your `NodeFactory`), hash your WGSL source (and any
  CPU-algorithm revision marker) into it. **Bump the salt whenever the
  algorithm changes**, this is what makes a kernel change invalidate every
  cached tile for that node, even at identical params. Under the §4.5
  process-version discipline, a salt change on an **already-shipped** PV
  trips the per-PV golden-immutability gate, that's the intended trip-wire
  telling you the change needs a new PV, not a silent kernel swap.
- **Purity.** Your node must be a pure function of `(inputs, params,
  kernel_salt)`. Any hidden state (a global, a file read, wall-clock time)
  silently serves stale cached tiles to a caller who has no way to know
  Risk R5 in the spec, and the reason `aux` (feedback slots) are keyed
  explicitly rather than implicitly.

## PV discipline (append-only forever)

`NodeRegistry::register(id, PvRange, factory)` is **append-only**: you
cannot replace what's registered for an already-shipped `(id, pv)`. Improving
an algorithm means registering a **new** `NodeId` (or the same id under a
**new**, non-overlapping `PvRange`) for a **new** process version, never
overwriting. This is what makes "render this image under last year's PV"
possible forever, and it's why every registration call takes an explicit
`PvRange` rather than "just the current version." Use `PvRange::from_open(pv)`
for "this and every future PV" (what every engine-owned node does today) or
`PvRange::single(pv)`/an explicit closed range when you know a future PV will
supersede it.

## Seams you must not cross

The engine holds **zero color science, decode, persistence, or UI types**
(§2.3 architecture seams). Concretely, from inside a node:

- Pixels in: only through the `SourceProvider` seam (you never call decode
  code). Color transforms: call into `lightbox-color` (like `xform.display`
  does), never hand-roll a matrix/LUT inline in the engine crate.
- Tiles out to persistence: only through `TileSink` (E03).
- No `lightbox-catalog`, `lightbox-edit`, or UI (egui/eframe) types in your
  node's signature beyond what the trait already threads through
  (`ParamBlock`, `Roi`, `Extent`, all engine-owned).
- If your node needs a new port type, precision, or trait method the current
  contract doesn't support, that's a spec change, not a local workaround
  raise it through the epic's governance loop (`03-handoff.md` §5.3), don't
  quietly widen the trait.
