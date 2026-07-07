# E05 — Deviations & Deferred Tasks

Append-only log of spec deviations and DEFERRED tasks against `E05-render-node-graph-engine.md`.
Each entry records: what changed, why, and the blast radius / rollback.

## §0 Execution disposition — 2026-07-07 (RECONCILE phase, system-architect)

E05 executes under the handoff §5 **parallel-wave** method (owner asked for max parallelism):
a serial **scaffold** phase pre-creates the full §2 module map + workspace members + a superset
of deps so downstream phases touch **strictly disjoint files**; then independent phases run
concurrently in `isolation: 'worktree'` (each on its own branch `e05-<x>`); a **merge agent**
integrates each wave onto `main` (conflicts limited to `Cargo.lock` → regenerate, and a few
`Cargo.toml`/`lib.rs` union lines) and runs the verify/exit-bar pass on `main` single-process.
E05 **generalizes E01's ~3200-line one-node Engine seed IN PLACE** (`engine.rs`, `node.rs`,
`planner.rs`, `gpu.rs`, `pool.rs`, `source.rs`, `nodes/{solid_color,display_transform}.rs`) —
the scaffold refactors the seed into `graph/ compile/ node/ exec/ cache/ gpu/ sched/ recover/
source/ nodes/`; it does **not** clobber the working seed.

### Wave plan

```
Scaffold (serial, on main / scaffold branch)
   crate layout per §2 module map (graph/ compile/ node/ exec{,/gpu,/cpu}/ cache/ gpu/ sched/
   recover/ source/ nodes/ stats.rs); lightbox-render-testkit new dev-crate; error taxonomy
   (thiserror: EngineInitError/NodeError/CompileError/RenderError); superset deps declared once
   (blake3, half, rayon, wide + testkit); module stubs so waves are file-disjoint.   [A1]
        │
        ▼
Wave A  { core ∥ gpu }                         (the two-engineer split, §5)
   core (Eng-A: graph/trait/registry/compiler): A2 A3 A4 A5 A8 A10 A11 A12 A13 A14 A15 A16
   gpu  (Eng-B: GPU-depth):                     A6 A7 A9
   — freeze the exec ↔ cache/compile interface (CacheKey in, TileHandle out) at end of Wave A
     (R8 mitigation: A9/B2 boundary).
        │
        ▼
{ B ∥ C ∥ E }
   B  content-keyed cache & tail invalidation  (Eng-A; starts after A11) B1..B8
   C  ROI / tile / progressive                 (Eng-B; C1–C2 after A11)  C1..C10
   E  device-lost recovery + CPU fallback       (Eng-B; E1 after A13)     E1..E8
        │
        ▼
{ D ∥ F }
   D  process-version registry                 (Eng-A; after B2)         D1..D5
   F  cross-cutting & M1 integration            (shared)                  F1..F5
        │
        ▼
verify (merge agent, on main, single-process)
   full exit bar green (build/test/clippy -D warnings/fmt --check/deny check); the four §10.1
   phase gates (A16, B5, C2, E5) run PR-blocking; parity (E6) + determinism + per-PV subset (D3
   fast) run PR-blocking; record actuals for the reduced-scale items below.
```

Dependency notes carried from §5: B starts after A11 (compiler + template); C1–C2 after A11; D
after B2 (cache-key propagation); E1 after A13 (canvas double-buffer). F5 (M1 integration into
`lightbox-core` + `lightbox-cli render`) lands last. Within a phase, table order is dependency
order.

### GPU truth on this build machine

This is a **macOS / Metal** box; wgpu gets a **real Metal adapter headlessly** (E01 already ships
GPU tests here), so all GPU-backed tasks — GPU executor (A9), tile executor (C3), device-lost
inject/rebuild (E1–E3), canvas double-buffer (A13), and the Metal leg of every golden/parity gate
— are **BUILDABLE-NOW and run for real**. No GPU result, golden, or parity number is faked. If a
parallel worktree momentarily can't acquire an adapter, that task leans on the single-process
verify pass on `main` and is reported honestly — never faked. What is **not** available here is
**non-macOS GPU hardware** and a **self-hosted reference-perf runner** (see DEFERRED F1/F2).

### BUILDABLE-NOW vs DEFERRED

**BUILDABLE-NOW (the bulk of E05 — builds and gates for real on this Metal box):**

| Area | Tasks | Note |
|---|---|---|
| Graph & trait spine (E05.1) | A1–A16 | incl. the first single-node golden gate (A16) |
| Content-keyed cache & tail invalidation (E05.2) | B1–B8 | B5 tail-invalidation recompute-probe gate is PR-blocking |
| ROI / tile / progressive (E05.3) | C1–C6, C8, C9 | C2 fit-view ≤~8 MP gate; C3 tiled==untiled exact-equality; C7/C10 see below |
| Process-version registry (E05.4) | D1, D2, D4, D5; D3 **fast subset** | per-PV keying, PV999 divergent-topology golden, kernel-salt trip-then-revert |
| Device-lost recovery + CPU fallback (E05.5) | E1–E8 | fault-injected loss → rebuild → degrade-to-CPU-preview all exercised on the real Metal device |
| Engine-owned scaffold nodes | `src.decoded`, `util.resize`, `xform.display` (generalized from E01), probe nodes | color math consumed from `lightbox-color`; zero color science in-engine |
| Testkit + goldens | `lightbox-render-testkit` corpus/comparators/probe nodes/scenario harness | **goldens rendered from the CPU reference path** (§4.4 / R3); synthetic corpus (§1 note) — no raw corpus needed |
| M1 integration | F5 | `RenderScheduler` into `lightbox-core`; `lightbox-cli render` headless E2E |

The whole A/B/C/D/E **core** + engine-owned nodes + testkit + CPU-reference goldens + M1
integration is provable with **synthetic sources** (the testkit corpus: gradients / checker /
low-key / high-key / high-frequency / wide-gamut / 100 MP-synthetic). The raw corpus is **not**
required to build or gate E05 (E02-as-built: `SourceProvider::DecodedFull` returns
already-demosaiced RGB via the out-of-process LibRaw proxy; rawler is crate-banned — see E02 and
§3.8).

**DEFERRED (each needs absent hardware, absent tooling, or a long standalone run — nothing faked):**

| # | What is deferred | Reason | What DOES ship / run now |
|---|---|---|---|
| **F1** | p95 slider-to-screen perf measured on a **self-hosted reference GPU runner** (RTX 3060 / M-series-base class, §7) | **No such runner on this mac.** A perf number on this dev box is not the reference-hardware gate of record; recording it as the gate would be fabrication. | The **scenario harness itself is built** (scripted slider churn at fit-view over the corpus, end-to-end through the scheduler) and runnable locally for regression signal; the p95<100 ms **gate of record is nightly on the reference runner** (§8 non-blocking perf). Local wall-clock actuals may be noted here as indicative, labelled non-authoritative. |
| **F2** | Cross-platform HW legs: **Windows/DX12 + Linux/Vulkan** golden+unit subset | **Single-OS build box** — macOS/Metal is the only real backend here; **lavapipe/WARP not installed.** Compiling/verifying other-OS GPU paths on darwin would be a fabricated cross-platform run. | The **macOS/Metal leg runs for real and is PR-blocking**; the CPU path runs everywhere; the golden subset + per-backend tolerance harness is written so the Windows/Linux legs light up unchanged on a real 3-OS matrix (per-platform tolerances all within the single §4.4 ΔE2000≤1.0/PSNR≥45 dB — no platform carve-outs). |
| **F4** | **24 h compiler fuzz + concurrency soak** as a standalone long run | A multi-hour soak is a nightly/long-run job, not a PR-gate step; claiming a 24 h clean run inside this pass would be fabrication. | The **in-gate proptest/fuzz-target SUBSET ships and runs**: compiler property test (arbitrary recipes ⇒ typed error or valid graph, never panic) + bounded concurrency stress on the ticket store + cache in `cargo test`. The 24 h soak is the nightly job. |
| **D3** | **FULL nightly per-PV golden matrix** (corpus × recipe set × all registered PVs) | The full matrix is by design the nightly job (§8); it is wall-clock-heavy and is not the PR-blocking surface. | The **fast PR-blocking subset ships** (D3 fast: any drift beyond ΔE2000≤1.0/PSNR≥45 dB on any registered PV fails the build); the immutability trip-then-revert proof (D4) runs; the full matrix is wired for nightly. |
| **C7** | 100 MP render **under a 512 MB** simulated VRAM budget | May be **wall-clock / VRAM-bound** on this box. | Runs, possibly **reduced-scale locally** (smaller synthetic MP and/or budget) with the tiling working-set degradation exercised and **peak-VRAM tracked by pool accounting**; the reduced factors + observed peak will be **noted here as actuals** when the phase lands. Not faked to full scale. |
| **C10** | **10 k-iteration** randomized param/zoom/pan soak over the corpus | May be **wall-clock-bound** on this box. | Runs, possibly at a **reduced iteration count locally** (asserting zero validation errors / zero deadlocks / VRAM within budget / final frame == fresh render); the actual iteration count run will be **noted here**. The 10 k count is the nightly soak. |

Nothing above is a scope cut: F1/F2 are **hardware**-gated (reference-perf runner / other-OS GPUs
absent), F4/D3 are **long-run/nightly** by design (in-gate subset ships), C7/C10 may run
**reduced-scale** locally with actuals recorded. Every deferral names its blocker; when the
hardware/runner is provided the gate lights up with **no code change**.

### Exit bar (all green, every wave, on main)

`cargo build --workspace` · `cargo test --workspace` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo fmt --all --check` ·
`cargo deny check`. New deps (blake3, half, rayon, wide — all MIT/Apache, cargo-deny surface-1
clean; no native/FFI, so surfaces 2–3 untouched) declared in the workspace + the owning crate.
Never commit broken.

### Spec edits made in this reconcile phase

- **§3.8 "Interim reality at M1" — stale source-seam wording corrected.** Replaced
  "`SourceProvider::DecodedFull` returns already-demosaiced RGB from E02 (rawler / interim LibRaw
  path, CPU)" with the **E02-as-built** truth: source arrives already-demosaiced **via E02's
  out-of-process LibRaw-proxy path** (CPU); **rawler is banned from the crate graph — see E02**.
  This is the only change to the spec body; the source port stays `LinearRgbaF16` and `MosaicU16`
  stays dormant until E11, exactly as specced. Rationale: E02 shipped LibRaw-proxy-primary with
  rawler crate-banned (`deny.toml`); a planner reading the old parenthetical would guess a banned
  in-process rawler path that does not exist. Blast radius: documentation only — no interface,
  gate, or task changes.

## Phase A — SCAFFOLD (full module map + frozen interfaces) — 2026-07-07

Serial scaffold on `main`. Exit bar re-run and **all five gates green**:
`cargo build --workspace` · `cargo test --workspace` (every workspace test passes, incl. the
four E01 render tests — `adapter_smoke`, `gpu_context`, `engine_lifecycle` (10), `display_transform`
(3) — which ran **on the real Metal adapter**, unchanged) · `cargo clippy --workspace --all-targets
-- -D warnings` · `cargo fmt --all --check` · `cargo deny check` (advisories/bans/licenses/sources
all ok). Commit: `E05 Phase A: full module scaffold + testkit crate + frozen interfaces`.

### D-1 (STRUCTURAL, the big one): E01 seed kept at crate root; the E05 §3 surface lives under `pub mod ng`

**What.** The full E05 §2 module map + §3 interfaces are created under
`crates/lightbox-render/src/ng/` (the "next-generation" engine), **not** at the crate root. The
working **E01 one-node Engine seed stays exactly where it is** at the crate root
(`engine.rs node.rs planner.rs gpu.rs pool.rs source.rs error.rs nodes/`), re-exported unchanged.

**Why.** `lightbox-render`'s E01 public API is consumed by **four external crates** —
`lightbox-core` (`session.rs`, `render_source.rs`, `error.rs`, tests), `lightbox-shell`
(`lib.rs`, `loupe.rs`), `lightbox-cli` (`main.rs`), `tools/lbx-perf` (`scenarios.rs`, `main.rs`) —
plus the four in-crate render tests. The E05 §3 surface **redefines the same names with different
shapes** (`Engine`, `RenderNode`, `RenderRequest`/`State`/`Target`/`Output`, `Roi`, `RenderScale`,
`NodeRegistry`, `SourceImage`, `NodeError`, `RenderError`). A single crate cannot expose two types
of the same name; promoting the E05 surface to the root now would break all four consumers and every
seed test — far beyond a scaffold's remit ("keep the build green; freeze disjoint interfaces"), and
the task also mandates every new method be an `unimplemented!()` stub, which is incompatible with
"the E01 tests must still pass" if the root `Engine` becomes a stub. Namespacing under `ng` resolves
the contradiction: the seed keeps proving E01's functionality; the `ng::*` stubs freeze the E05
interfaces for the parallel waves in strictly-disjoint files.

**Reconciliation with "generalize the seed IN PLACE."** The seed's *algorithms and patterns* (the
`display.transform` math, the worker-loop/ticket-store/latest-wins pattern, the texture pool, the
`GpuContext` shared-device seam) are the reference the wave agents port from; the seed is **retained,
not clobbered**. Promotion is **task F5's** job — exactly where the spec §2 "small named touches" and
DoD #5 already place the single-node-path deletion + `lightbox-core`/`-cli` rewiring. Until F5:
`lightbox_render::Engine` = working seed; `lightbox_render::ng::Engine` = E05 engine (stubbed).

**Blast radius.** Zero source changes outside `crates/lightbox-render/src/{lib.rs,ng/**}` and the two
manifests + the new testkit crate. No consumer edits, no seed edits, no test edits. Rollback: delete
`src/ng/` and the `pub mod ng;` line.

### D-2: dependencies added (superset, spec §2/deliverable D)

- `blake3`, `half`, `wide` (task-listed) + **`petgraph`** (NOT in the task's enumerated list, but the
  spec §2 module map + §1.3 make petgraph the **architecturally-DECIDED** DAG backend — declared so
  the A8 wave never touches the manifest). All MIT/Apache/CC0/Zlib — `cargo deny licenses` green.
- `tokio` gains the **`sync`** feature on `lightbox-render` — the §3.6/§3.7 signatures name
  `broadcast::Receiver<EngineEvent>` and `watch::Receiver<CanvasFrame>` verbatim.
- **`lightbox-color`** added as a `lightbox-render` dep (superset): the engine-owned `xform.display`
  node consumes color math from it (E02 guardrail — zero color science in-engine). Referenced via
  `use lightbox_color as _;` in `ng/nodes/display.rs` so the edge is explicit while the body is
  stubbed (A-gpu).
- `wide` is **unused in the scaffold** (declared per deliverable D's "unused-in-scaffold is fine").

### D-3: pre-existing advisory remediated by lockfile bump (not introduced by E05)

`cargo deny check` surfaced **RUSTSEC-2026-0204** (`crossbeam-epoch 0.9.18`, a `fmt::Pointer`
invalid-deref, transitive via `rayon → rayon-core → crossbeam-deque`). Verified against
`git show HEAD:Cargo.lock`: **`crossbeam-epoch 0.9.18` + `rayon 1.12.0` were already locked before
this scaffold** — this is a newly-published advisory going red on `main` independent of E05, not
something E05 pulled in. Fixed with the advisory's own recommended, **lockfile-only, semver-compatible**
bump `cargo update -p crossbeam-epoch` (0.9.18 → 0.9.20). No manifest change; no functional change.

### D-4: minor interface adaptations (spec's illustrative sigs → compiling Rust)

- `DeviceProvider::rebuild` return factored through a `pub type DeviceHandles = (Arc<Device>,
  Arc<Queue>)` alias to satisfy `clippy::type_complexity` (behavior identical to §3.8).
- `CacheKey::derive` takes a `CacheKeyInputs<'_>` struct rather than 8 positional args (avoids
  `clippy::too_many_arguments`; same §3.5 ingredients).
- `Roi::tiles` (spec `-> impl Iterator<Item = TileCoord>`) ships a **placeholder empty iterator**
  (an `unimplemented!()` body can't inhabit an opaque `impl Iterator` return); `Roi::expand` /
  `intersect` are `unimplemented!("A2")`. A2 fills the real 256² algebra.
- `SourceColorimetry` / `OutputColorimetry` / `SourceKind{Raw{cfa}}` are **opaque placeholder tags**
  in `ng/colorimetry.rs` (the engine holds zero color science). To be reconciled with
  `lightbox-color`/E02 descriptors when `xform.display` lands (A-gpu) — noted in-code.
- Frozen shared seams placed in dedicated files (reshaping needs a deviation): `ng/exec/backend.rs`
  (`trait Backend` — the R8 exec↔backend seam: `CacheKey` + graph node in, `TileHandle` out) and
  `ng/tile.rs` (`TileHandle`/`TileView`/`CpuTileView`/`PixelBuf` currency).

### FILE-OWNERSHIP MAP (keep the parallel waves strictly disjoint)

All E05 modules are under `crates/lightbox-render/src/ng/`. Every method is an `unimplemented!("<task>
(<owner>): …")` stub or a spec-default; the crate compiles, tests are green, nothing calls a stub.

| Wave / owner | Files it owns (fills the stubs) |
|---|---|
| **A-core** | `ng/types.rs` (A2) · `ng/node/{mod,param,registry}.rs` (A3/A4/A5) · `ng/graph/mod.rs` (A8) · `ng/compile/mod.rs` (A11; D extends) · `ng/exec/mod.rs` (A9/A12) · `ng/exec/cpu/mod.rs` (A14) · `ng/cache/mod.rs` *type surface* (A-core) · `ng/stats.rs` (A15) · `ng/engine.rs` + `ng/config.rs` + `ng/error.rs` (A1/A12) · testkit `compare.rs` + `corpus.rs` GoldenCase/first golden (A16) |
| **A-gpu** | `ng/gpu/mod.rs` (A6/A7) · `ng/exec/gpu/mod.rs` (A9) · `ng/source/mod.rs` upload path (A10) · `ng/sched/mod.rs` canvas double-buffer (A13) · `shaders/` (A6) · `ng/nodes/{decoded,resize,display}.rs` (engine-owned node bodies) |
| **B** | `ng/cache/mod.rs` *real impl* (VRAM+RAM tiers, LRU, pin, integrity; B1–B4,B7) · `CacheKey` derivation+propagation (B2) · `ng/sched/mod.rs` latest-wins coalescing (B6) · cache benches (B8) |
| **C** | `ng/exec/mod.rs` tiling layered on the topo walk (ROI plan, scale/resize, 256² tiles, visible-first, 1:1, progressive ladder, VRAM budget, F32 path, priority lanes, soak; C1–C10) · testkit `probes.rs` `BlurRProbe`/`AccumProbe` · `scenario.rs` soak (C10) |
| **D** | `ng/compile/mod.rs` per-PV templates + PV plumbing + PV manifest (D1–D5) · testkit per-PV golden matrix (`corpus::run_matrix`, D3) |
| **E** | `ng/recover/mod.rs` (detection, rebuild, re-warm, degradation; E1–E4) · CPU parity gate (E6) · degraded-contract enforcement (E7) · export-safety (E8); `ng/engine.rs` `events`/`active_backend` wiring |
| **F** | `lightbox-core` wiring + `lightbox-cli render` + engine book (docs) + fuzz targets + `scenario.rs` perf harness (F1); **F5 promotes `ng` → crate root, deletes the seed, rewires consumers** |
| **Scaffold-frozen** (deviation to touch) | `ng/exec/backend.rs` (`trait Backend`, R8 seam) · `ng/tile.rs` (tile currency) |
| **Retained seed** (no wave owner; F5 deletes) | crate-root `engine.rs node.rs planner.rs gpu.rs pool.rs source.rs error.rs nodes/**` — **do not modify** unless your task explicitly ports from it |

## Wave A — A-core (graph / trait / registry / compiler / engine / CPU / testkit) — 2026-07-07

Branch `e05-core` (worktree). Tasks landed: **A2 A3 A4 A5 A8 A11 A12 A14 A15 A16**. Full five-gate
exit bar green in the worktree (`build` · `test` · `clippy --all-targets -D warnings` · `fmt --check` ·
`deny check`); fixtures fetched via `cargo xtask fixtures` (E02 CLI e2e preconditions, unrelated to
E05). Commit: `E05 Phase A-core: graph/registry/compiler/engine/cpu/testkit + first golden`.

Test coverage added: `ng::types` (roi expand/intersect/tiles + serde), `ng::node::param` (canonical-CBOR
order-independence property, NaN reject, `-0.0` normalize, schema validation), `ng::node::registry`
(overlap/edge/open-range resolve, supported_pvs), `ng::graph` (topo/cycle/typed-edge/multi-input/structural-eq),
`ng::compile` (PV1 topology, unsupported-pv, missing-node, stable topology), `ng::exec::cpu` (gain parity,
cancel), `ng::engine` (submit<50ms / poll→Complete / cancel-mid-render / unsupported-pv-typed / recompute
count), `ng::stats`; testkit `compare` (34 Sharma-Wu-Dalal ΔE2000 vectors, PSNR), `corpus`
(**first single-node golden**, determinism, recipe-fragment parse).

### D-A-core-1 (STRUCTURAL): `TileHandle` gains a CPU payload field (touching the frozen `ng/tile.rs` seam)

**What.** `ng/tile.rs`'s `TileHandle` — a scaffold-frozen empty `#[non_exhaustive]` struct — gains one
additive private field `cpu: Option<Arc<PixelBuf>>` plus `from_cpu`/`from_cpu_arc`/`cpu`/`cpu_arc`
accessors, and `PixelBuf` gains real body (alloc, `get/set_rgba_f32`, `encode_pixel`, `par_fill_rows`).
**Why.** The frozen `Backend` seam threads `TileHandle` between nodes; the A-core CPU backend must carry
CPU-resident working tiles through it. `tile.rs` is listed as a **"Scaffold-frozen (deviation to touch)"**
file, so this is expected. **Merge shape.** A-gpu adds its GPU-texture payload as a **second additive
field** on the same struct (both waves fill the frozen seam disjointly, Risk R8). The merge is a struct-field
union — a handful of lines, same class as the planned `Cargo.toml`/`lib.rs` union. `Default`/`Clone`/`Debug`
all stay derivable. **Rollback.** Remove the `cpu` field + accessors; the CPU path is the only consumer.

### D-A-core-2: `ParamBlock` / `ParamsSchema` concretized (A3)

`ParamsSchema { fields: &'static [FieldDecl] }` is const-constructible for `NodeDescriptor`s (`EMPTY`
preserved); `ParamValue` is `Float(f64)|Int(i64)|Bool(bool)|Text(String)`; `ParamBlock::from_fields`
validates against the schema, **rejects NaN**, **normalizes `-0.0 → 0.0`**, and encodes **canonical CBOR
over a sorted (field-order-independent) map** (proptest-pinned). `ParamBlock::hash` (scaffold-tagged B1)
is implemented here (blake3 over canonical bytes) since the compiler/executor reference it; B1 is a no-op
verify on merge. Blast radius: `ng/node/param.rs` only; the scaffold's method signatures are unchanged.

### D-A-core-3: Engine renders the CPU path; GPU-backend selection + source upload are merge-wired

- `Engine::new`/`with_compiler` build a **`CpuBackend` for every `BackendPref`** in A-core (`ForceCpu`
  honored today). GPU-backend selection (build `DeviceCtx` from `dp.current()`, `GpuBackend::new`, fall
  back to CPU on failure) is a clearly-marked seam in `with_compiler` that **A-gpu wires at merge** — so
  the A-core worktree is constructible and rendering without a live adapter and never calls an A-gpu stub.
- `Engine::with_compiler(dp, sp, compiler, cfg)` added (additive to the spec `new`) so **D** can inject
  extra-PV templates and tests can drive probe graphs; `new` delegates to it after registering PV1.
- **Source injection**: the compiled graph's source node (`src.decoded`, 0 inputs) produces the working
  tile; a node with no inputs is evaluated as a generator on the CPU path. The **`SourceProvider` upload**
  path (`Uploader`, A10) and the **`Canvas`** double-buffer target (A13) are A-gpu — the A-core engine
  renders `Buffer` targets and returns a typed error for `Canvas`. A worker-thread panic (an A-gpu stub
  reached on the CPU-only path) is caught and surfaced typed, never aborting the render worker.
- `Executor::evaluate` threads the `NodeCache` but **does not consult it** (v1 always recomputes; **B**
  wires content-keyed get/put + tail invalidation). `Executor::with_probe` shares the recompute probe so
  `Engine::stats` observes the same counters. A v1 placeholder `CacheKey` (node id + graph index) is
  constructed directly (not via the B2-owned `CacheKey::derive`).
- `NodeRegistry::supported_pvs` returns the sorted distinct **lower bounds** of registered ranges (the
  discrete entry-point PVs); `Engine::supported_pvs` reflects the compiler's registered templates.

### D-A-core-4: First golden — `test.checker → test.gain(×2)` on the CPU reference path (A16, the §10.1 E05.1 gate)

The committed golden is `crates/lightbox-render-testkit/goldens/test.gain/pv1/checker-gain.png`
(64×64 RGBA8), rendered by `Executor` over `CpuBackend` (the CPU reference path, §4.4/R3) and read back
with a straight linear quantization (no display encoding — that is `xform.display`'s job; the harness holds
zero color science). PR-blocking test `first_single_node_golden_within_tolerance` compares within
**ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB** using the self-contained comparators (validated against the 34
Sharma-Wu-Dalal vectors). Regenerate with `LIGHTBOX_BLESS=1`; a missing golden **fails** (never
self-certifies). `corpus::run_matrix` (D3) is a thin `map(run_golden)` for D to extend; `run_golden`
currently renders the checker→gain reference graph (D wires the full case→graph mapping).

### DEFERRED at A-core — wired at the A-gpu merge (nothing faked)

| Item | Why deferred | Lights up when |
|---|---|---|
| **A9** 2-node **GPU** readback (`src.decoded → test.gain` on the real Metal device) | needs A-gpu's `GpuBackend`/`TilePool`/`KernelBuilder` (their files, stubbed in this worktree) | A-gpu lands; run single-process on `main` at the merge/verify pass on the real Metal adapter |
| **E6** CPU/**GPU** parity (ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB) for the probe + engine-owned nodes | same — no GPU backend in the A-core worktree; the comparators + CPU determinism ship and are green | A-gpu GPU backend present at merge; parity asserted on Metal |
| `Engine::active_backend` (E4), `events` emission (E) | E owns device-lost / degradation | Wave E |
| `Canvas` target (A13), `SourceProvider` upload (A10), GPU-pref backend select | A-gpu owns the device seam | A-gpu merge |

The CPU path, self-contained ΔE2000/PSNR comparators, and the first golden are **green in the worktree
now**; the GPU-dependent parity/readback is honestly deferred to the single-process merge on the real
Metal box, per the disposition above.
