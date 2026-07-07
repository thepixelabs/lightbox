# The Lightbox engine book

**Audience:** anyone registering a node into `lightbox-render` — E10 (global
develop tools), E11 (detail/optics/demosaic), E12 (masking/local adjustments),
E15 (export). **Task F3** of E05 (spec `docs/plan/epics/E05-render-node-graph-engine.md`).

This is the on-ramp: what the engine gives you, what you must supply, and the
gates your node must pass before it ships. It does not re-derive the E05
spec or architecture — it tells you where to look and what to build against.

## 1. What already exists (read this first, don't rebuild it)

E05 is **done and green on `main`**: the full node-graph engine — trait,
registry, compiler, content-keyed cache, ROI/tile/progressive evaluation,
per-PV registry, device-lost recovery + CPU fallback — lives at
`lightbox_render::ng::*` in `crates/lightbox-render/src/ng/`. As of E05 Phase
F5, this is **the live engine**: `lightbox-core`'s session façade,
`lightbox-shell`'s loupe/canvas, and `lightbox-cli`'s `render` subcommand all
render through it. (Historical note: the E01 one-node seed still sits at the
crate root, unused by the live app — see
`docs/plan/epics/E05-deviations.md` §F5 if you're wondering why both exist.)

You are **registering a node**, not building engine infrastructure. If you
find yourself wanting to add a module under `ng/`, touch `exec/mod.rs`, or
change a trait signature — stop and read §7 (seams you must not cross) below.

## 2. The five things a node is

1. **A [`NodeDescriptor`](../../crates/lightbox-render/src/ng/node/mod.rs)** —
   static id, typed input/output ports, a params schema. `const`-constructible.
2. **A [`RenderNode`](../../crates/lightbox-render/src/ng/node/mod.rs) impl** —
   `eval_gpu` (WGSL dispatch) + `eval_cpu` (the parity twin). Everything else
   (`plan`, `output_extent`, `cache_policy`, `affected_by`, `precision`,
   `aux_requirements`) has a spec-documented default; override only what your
   node actually needs to differ.
3. **A WGSL kernel** — one entry point, the §3.2 bind-group convention
   (`@group(0)` input, `@group(1)` output, `@group(2)` params UBO,
   `@group(3)` LUT/aux). Embedded via `include_str!`, naga-validated at
   **build** time (invalid WGSL fails `cargo build`, not a test).
4. **A [`NodeFactory`](../../crates/lightbox-render/src/ng/node/mod.rs)** —
   instantiates the node, supplies its `KernelSalt` (a hash of the WGSL
   source + algorithm revision — part of the cache key, §5 below).
5. **A registration call** — `registry.register(id, PvRange::from_open(pv), factory)`
   into a `NodeRegistry`, plus a slot in the relevant `GraphTemplate` stage
   (tone/detail/optics/LOCAL/retouch/effects — see the spec §4.1 stage order).
   **Registering a node is data, not an engine change** — you never edit
   `ng/compile/mod.rs`'s topology logic to add a develop tool.

## 3. Book contents

| File | What it covers |
|---|---|
| [`01-node-author-guide.md`](01-node-author-guide.md) | The `RenderNode` trait method-by-method, kernel conventions, the param ABI, `plan()`/apron rules for neighborhood nodes, cache policy, PV discipline |
| [`02-testing-and-gates.md`](02-testing-and-gates.md) | Per-node golden + CPU/GPU parity requirements, the testkit, what's PR-blocking vs nightly |
| [`03-add-a-node.md`](03-add-a-node.md) | A **compiling, passing** walkthrough — `tone.exposure`, a real point-op node, end to end |

## 4. The one-paragraph mental model

A `Recipe` (owned by E09) compiles to a `RenderGraph` (topologically-sorted,
type-checked DAG of your nodes) under a `ProcessVersion`. The `Engine`
evaluates it on the GPU or CPU backend, caching every node's output under a
content key `hash(node_id, pv, kernel_salt, param_hash, input_keys, tile,
scale, precision)`. Change one slider → one node's param_hash changes → that
node and everything downstream re-keys and recomputes; everything upstream is
an untouched cache hit. That's the whole system. Your job is to be a pure
function of `(inputs, params)` on both backends, agreeing within
ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (§4.4) — the engine does the rest.
