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

### D-A-core-5 (verification pass, 2026-07-07): `ParamBlock::hash` reconciled with D-A-core-2

The A-core commit left `ParamBlock::hash` as an `unimplemented!("B1")` stub even though D-A-core-2 states
it "is implemented here (blake3 over canonical bytes)". Nothing on the live CPU path called it (the exec
walk uses its own `placeholder_key`), so the exit bar was green regardless — but the stub contradicted the
log and the method docstring. Reconciled by implementing the one-liner in `ng/node/param.rs`
(`ParamHash(blake3::hash(&self.canonical))`) plus a focused test
(`param_hash_is_canonical_and_order_independent`). B1 (Phase B) is now the no-op verify D-A-core-2 always
described. Blast radius: `ng/node/param.rs` only; signatures unchanged; all five gates re-run green.

## Wave A-gpu — 2026-07-07 (A6/A7/A9/A10/A13 + engine-owned node bodies)

Fills the A-gpu stubs (`ng/gpu/`, `ng/exec/gpu/`, `ng/source/`, `ng/sched/canvas.rs`, `shaders/`,
`ng/nodes/{decoded,resize,display}.rs`). Full five-green exit bar verified in the worktree on the
real Metal adapter (build / test / clippy -D warnings / fmt --check / deny check). New GPU tests in
`crates/lightbox-render/tests/ng_gpu.rs` (11 tests: A6 kernel+cache+invalid-WGSL, A7 pool
reuse/budget/10k-stress, A9 backend `src.decoded`, A10 upload 8/16/f32 round-trip, A13 tear-free
canvas, resize + display CPU/GPU parity) — all pass; each guards with `gpu_or_skip!` and reports
honestly if an adapter is momentarily unavailable rather than faking a readback.

### D-A6: `naga` added as a **build-dependency** (build-time WGSL validation)

`build.rs` parses + validates every `shaders/*.wgsl` with naga at build time, so **invalid WGSL
fails the build, not runtime** (the A6 acceptance criterion). `cargo tree -i naga` resolves to the
**single** version `29.0.4` — wgpu's own vendored shader front-end at the pinned wgpu 29 — so this
adds **no new external surface** (cargo-deny bans/licenses/sources all green). Blast radius:
`crates/lightbox-render/{Cargo.toml,build.rs}` + a `naga` line in `Cargo.lock` (already present
transitively). Rollback: delete `build.rs` + the `[build-dependencies]` block.

### D-tile: `TileHandle` internals filled (scaffold-frozen `ng/tile.rs`)

Per the scaffold note ("Field internals of the opaque `TileHandle` are filled by **A-gpu**"), the
frozen seam is now inhabited: a `Gpu(Arc<GpuTile>)` variant (pooled `Arc<wgpu::Texture>` + view +
extent + precision + format; reclaimed by `TilePool` via `Arc::strong_count == 1`) or a
`Cpu(Arc<PixelBuf>)` variant for the rayon path, plus a hand-written `Debug` (a
`wgpu::TextureView` is not `Debug`) and the accessor surface (`extent/precision/texture_view/
texture/format/cpu_pixels/is_empty`). **The seam SHAPE is unchanged** — still opaque,
`#[non_exhaustive]`, `Clone + Default`, still "CacheKey in / TileHandle out" — only the reserved
internals were added. Blast radius: `ng/tile.rs` only.

### D-gpuctx: `GpuEvalCtx` completed in A-core's `ng/node/mod.rs` (agreed cross-file touch)

The scaffold left `GpuEvalCtx` with a `// A-gpu adds: output tile handle, TilePool, …` comment and
an `unimplemented!("A-gpu")` `output()`. Completed **additively**: a `kernels: &KernelBuilder`
field, a private `output: TileHandle` (pre-acquired by the backend from the `TilePool` at the
node's output precision/format), and `new()` / `output() -> &mut TileHandle` / `into_output()`.
This is the one A-gpu touch inside an A-core-owned file; it is **additive only** (no `RenderNode`
trait signature change, no change to any A-core method body) and was pre-agreed as the A-gpu
completion of the GPU eval context. The context is built and consumed entirely within A-gpu
territory (the GPU backend constructs it; engine-owned node bodies read it), so it carries no
cross-wave coupling. Blast radius: the `GpuEvalCtx` struct + its `impl`.

### D-nodes: engine-owned node bodies land against the future `CpuEvalCtx` (A14)

`src.decoded`, `util.resize`, `xform.display` have **complete, GPU-tested** `eval_gpu` bodies
(bind-group conventions §3.2: `@group(0)` input / `@group(1)` write-only storage output /
`@group(2)` params UBO / `@group(3)` LUT-aux; dispatched via `gpu::dispatch_compute`). Their
`eval_cpu` bodies call `CpuEvalCtx::output()`, which is still `unimplemented!("A14 (A-core)")` — so
the CPU **algorithm** ships and is parity-tested **now** as a standalone free function
(`decoded::copy_cpu`, `resize::decimate_box_cpu`, `display::apply_display_cpu`) against the GPU
readback, while the trait's `eval_cpu` wiring goes live unchanged when A14 lands the output buffer.
Nothing in-gate calls `eval_cpu` through the A14 stub, so no panic is reachable. The `resize` and
`display` CPU/GPU parity tests pass (byte-exact resize within 2/1000; display within 2 LSB on
`rgba8unorm`).

### D-resize-box: `util.resize` ships an area-average **box** filter (real Lanczos = C2)

Phase A ships box decimation (`resize.wgsl` / `decimate_box_cpu`) sharing **identical half-open
integer region math** GPU↔CPU so the §4.4 parity gate holds. Task **C2** replaces the kernel with
Lanczos; because the kernel change bumps `kernel_salt`, C2's replacement trips a fresh golden by
design (no trait change, no seam change).

### D-display-srgb: `xform.display` targets the **built-in sRGB** display (LCMS2-baked)

100% of the color math is `lightbox_color::display::build_display_transform` (a 1-D shaper + 65³
LUT, LCMS2); the engine only **applies** it (`out = trilinear(lut, shaper(rgb))`) — GPU
(`display.wgsl`) and CPU (`DisplayTransform::apply`) evaluate the identical baked data, so parity is
exact by construction and **zero color science lives in the engine** (E02 guardrail). Wiring a live
monitor profile through the params ABI is a later task, mechanism unchanged. This **resolves the
D-2 `use lightbox_color as _;` stub note** — `lightbox-color` is now genuinely consumed. The
`kernel_salt` spans both the WGSL and the baked color-data revision (`dt.key`).

### D-a13-readback: A13 shell-sim samples via **readback**, not a live egui-wgpu pass

The A13 mechanism — `sched::canvas::CanvasPublisher`: a 3-slot display-texture ring + atomic
generation counter + `tokio::sync::watch` publisher — is **standalone and independently tested**
(the `a13_canvas_publishes_tear_free_generations` test publishes 12 solid frames whose generation
is encoded in the red channel and, after each publish, samples the watch value **and** reads the
published ring slot back, asserting the whole texture is one consistent generation — no tear — in
lock-step with the watch generation). The publish-after-submit ordering + distinct ring slots are
what make it tear-free. Integration into a **live** egui-wgpu compositor pass rides on **F5**
(`RenderScheduler` → `lightbox-core`), where the scheduler owns the publisher;
`RenderScheduler::canvas()` stays `unimplemented!` until **B6** gives the scheduler its publisher
field (the scheduler struct body is B6-owned). The double-buffer/tear-free **mechanism is proven
now** on the real device; only the shell-side live compositing wiring is deferred to F5.

## Wave A — MERGE integration (e05-core + e05-gpu → main) — 2026-07-07

Merge agent, single-process on `main` (real Metal adapter). Both phase branches merged `--no-ff`
in order (`e05-core`, then `e05-gpu`). Full five-gate exit bar green on `main` after the merge +
the integration wiring below. `rawler`/`dnglab` confirmed absent from `Cargo.lock`.

### Conflict resolutions (merge commit `merge E05 e05-gpu`)

- **`ng/tile.rs` `TileHandle` (frozen seam, D-A-core-1 × D-tile).** Both waves inhabited the frozen
  handle with **incompatible** representations: A-core added a `cpu: Option<Arc<PixelBuf>>` field;
  A-gpu made it a `storage: Option<TileStorage{Gpu|Cpu}>` variant. The A-gpu representation is the
  **superset** (it carries the GPU pooled-texture payload the CPU-only shape cannot), so it wins;
  A-gpu's `cpu_pixels()` accessor was **renamed `cpu()`** to satisfy A-core's four callsites
  (`engine.rs`, `exec/cpu`, testkit `corpus.rs`). A-core's unused-by-callers `from_cpu_arc`/`cpu_arc`
  were dropped (no test referenced them; the `Cpu(Arc<PixelBuf>)` variant subsumes them). `half::f16`
  import kept (the merged A-core `PixelBuf` body uses it). Seam SHAPE unchanged (opaque,
  `#[non_exhaustive]`, `Clone + Default`, CacheKey-in/TileHandle-out).
- **`crates/lightbox-render/Cargo.toml` dev-deps.** Union: `serde_json` + `proptest` (A-core) and
  `half` (A-gpu integration tests).
- **`E05-deviations.md`.** Append-only union of both waves' sections.
- `Cargo.lock` / `ng/node/mod.rs` auto-merged (disjoint `GpuEvalCtx` (A-gpu) vs `CpuEvalCtx`
  (A-core) fills, exactly as the D-gpuctx note predicted).

### Integration wiring (commit `E05 Wave A integration: wire GPU/CPU backend seam + prove A9/A16`)

The A9 **engine-level** gate and full GPU render path span both agents and could not be tested in
either worktree alone (A-core deferred A9 GPU readback + backend selection to "the A-gpu merge";
A-gpu's `GainProbe::eval_gpu` / testkit gain kernel is deferred to "the A-gpu executor merge"). The
smallest-correct wiring, all in the exec↔backend seam owned jointly at merge (Risk R8):

- **`engine.rs` — backend selection.** `Engine::with_compiler` now selects the backend per
  `BackendPref`: `ForceCpu` → `CpuBackend`; `Auto` → build a `DeviceCtx` from `dp.current()` and a
  `GpuBackend` on the shell's shared device, retaining the `DeviceCtx` on `Shared` for source upload
  + terminal readback. This replaces the A-core `let _ = cfg.backend == Auto; CpuBackend` seam stub
  (D-A-core-3). Backend provenance (`RenderOutput.backend`) reflects the actual backend.
- **`engine.rs` — source injection.** `Shared::process` locates the source stage (`src.decoded`) via
  the new `RenderGraph::node_index`, fetches decoded pixels through the `SourceProvider` seam
  (`pollster::block_on`, `SourceWant::DecodedFull`), and lifts them to a working tile — a pooled GPU
  texture (`Uploader::upload`, A10) on the GPU path or an **identical-bytes** host buffer
  (`source::to_working_tile_cpu`, new) on the CPU path. Graphs without a source stage (probe
  generators like `test.const`/`test.checker`) render with no fetch, so all A-core `NullSource`
  tests are unaffected.
- **`exec/mod.rs` — `Executor::evaluate` gains `source: Option<(NodeIndex, TileHandle)>`.** The
  named source node receives the injected tile as its sole input (it declares zero graph inputs;
  `nodes::decoded` documents the executor pre-populating the source stage). Every other node draws
  inputs from upstream — the topo walk is otherwise unchanged. Two callers updated (`engine.rs`,
  testkit `corpus.rs` → `None`).
- **`engine.rs` — GPU readback.** The `Buffer`-target `readback` now copies a GPU-resident terminal
  tile back via `exec::gpu::readback_tile` (was: CPU-only, erroring "GPU readback is wired by
  A-gpu"). The CPU path is unchanged.
- **A-core engine test builder** switched to `BackendPref::ForceCpu` (its `NullDevice`'s `current()`
  is `unimplemented!`; `Auto` would now ask it for handles). These are CPU-path probe tests, so
  ForceCpu is faithful.

### A9 (engine-level) — PROVEN on `main`

`tests/ng_gpu.rs::a9_engine_gpu_two_node_graph_equals_cpu_reference_exactly` (real Metal): a 2-node
`src.decoded → test.gain` graph is driven through `Engine::submit` twice — `Auto` (→ `GpuBackend`,
provenance `BackendId::Gpu`) and `ForceCpu` (→ `CpuBackend`) — and the two `Buffer(Rgba16)`
readbacks are asserted **byte-for-byte identical**. Gain is ×2 on an `f16` gradient: `2·v` is exactly
representable in `f16` for these values, so the equality is exact by construction (not merely within
ΔE2000 ≤ 1.0). The test carries a self-contained GPU+CPU `test.gain` node — the testkit `GainProbe`
GPU kernel remains the deferred E6 parity work; this proves the engine's GPU render path end-to-end
without pre-empting it. This is the A9 acceptance criterion realized at the `Engine` level (the
worktree `a9_gpu_backend_src_decoded_copies_source` proved only the backend in isolation).

### A16 (first single-node golden) — GREEN on `main`

`lightbox-render-testkit corpus::tests::first_single_node_golden_within_tolerance` passes against
the committed `goldens/test.gain/pv1/checker-gain.png` within ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB (CPU
reference path, landed by A-core; unaffected by the merge). PR-blocking golden gate live.

## Phase B — E05.2 content-keyed cache & tail invalidation — 2026-07-07 (branch `e05-b`)

Phase B (B1–B8) implements the content-keyed cache, tail invalidation, the source-stage RAM pin,
latest-wins scheduling coalescing, and cache integrity under cancellation. It owns `cache/` (real
impl), the `CacheKey` derivation + propagation, and the scheduler's coalescing; it necessarily
touched a few adjacent files to wire those through (each noted below; all coordinated with the R8
`exec ↔ cache/compile` seam the spec §7 flags as jointly owned).

### Files touched beyond `cache/` + `sched/`

- **`graph/mod.rs` (A-core file, extended by B).** `NodeEntry` gains a per-node `KernelSalt`;
  `add_node_full(node, params, salt)` + a `salt(idx)` accessor added; `add_node`/`add_node_with_params`
  default the salt to a stable hash of the node id. The content key (spec §3.5) needs a per-node salt
  and the graph is its natural carrier. Structural-equality + existing tests are unaffected (they
  ignore the salt).
- **`compile/mod.rs`.** The compiler now stamps the registered kernel salt (`registry.kernel_salt(id,
  pv)`, already present) onto each graph node via `add_node_full`. Without this the salt ingredient
  would be dead; wiring it also readies D4's salt-discipline gate. This is a cache-derivation concern
  that lives in D-owned `compile/`; it is additive (2 lines) and does not touch per-PV template
  selection.
- **`exec/mod.rs` (B2/B5/B7).** Replaced the placeholder key with the real §3.5 `CacheKey::derive` +
  forward propagation through the topo walk; the walk now consults the cache (hit ⇒ serve + skip
  eval; miss ⇒ eval + `put`). `Executor::evaluate` gains a `pv: ProcessVersion` param and the source
  argument becomes `Option<SourceInject { idx, tile, key }>` (extends the earlier A-gpu-merge source
  tuple). The flat 8-arg signature carries `#[allow(clippy::too_many_arguments)]` — it is the frozen
  R8 seam and clearer positional than wrapped. **`cache.put` happens only after a successful backend
  eval**, so a cancelled/failed node never inserts a partial tile (B7).
- **`engine.rs`.** The `NodeCache` is built with a byte budget from `EngineConfig::vram_budget`
  (`Bytes(n)` verbatim; `Auto` ⇒ the cache's default 512 MiB cap — adapter probing lands with the GPU
  device-lost work) and shares the executor's `RecomputeProbe` (VRAM gauge). The source stage is now
  **pre-checked against the cache** (`peek`) and, on a miss, fetched once and RAM-pinned
  (`pin_tile(source_key, …, SourceStage)`) — a warm pin means **zero** `SourceProvider::fetch` (B4).
  `Engine::stats()` merges executor hits/misses/nodes_evaluated with cache evictions/bytes.
- **testkit `corpus.rs`.** The single `evaluate(..)` call updated for the new `pv` argument (passes
  `ProcessVersion(1)`); the A16 golden is byte-unchanged (the walk still evaluates every node cold).
- **`Cargo.toml`.** Dev-deps `blake3` + `criterion` added and a `[[bench]] name = "cache"` entry
  (B8). No new workspace deps — both are already in the workspace graph (MIT/Apache; `cargo deny`
  clean).

### Decisions / interpretations (nothing faked)

- **Source-stage key is scale-independent + image-discriminated.** `SourceProvider::DecodedFull`
  returns the full source regardless of preview scale, so `source_key` folds `(image, pv, kernel_salt,
  params)` but **not** `scale_q` (RAM pin per image+pv, per open-Q5's "pin at the active scale"
  spirit). Downstream nodes fold `scale_q` into their own keys, so a scale change still invalidates the
  tail while the pinned source is reused with zero re-fetch. The B4 engine gate exercises exactly this
  (render at `Ratio(1.0)` then `Ratio(0.5)`: fetch count stays 1, downstream recomputes) — the M1
  `Recipe` carries no develop params yet (E09), so the literal per-node **param** change is proven at
  the executor level (B2/B5), and the engine-level B4 proof uses a downstream-invalidating render.
- **In-flight pinning** is realized by `TileHandle`'s `Arc` backing: a returned `CachedTile` keeps its
  tile alive even if the map entry is later evicted, so a render in flight never loses a tile it is
  consuming; `get` also LRU-touches. The explicit RAM pin tier is the non-evictable set (its bytes are
  unbudgeted). An entry whose cost alone exceeds the budget is **never admitted**, keeping the
  "budget never exceeded" invariant strict (proven by a 5 000-op randomized fuzz).
- **B6 coalescing** is proven in two layers: the pure `Coalescer` state machine deterministically
  bounds a 1 000-update burst to ≤ 2 dispatches with final-wins (unit test), and an engine-level
  integration test drives a 200-call `set_recipe` burst against a ~12 ms render ⇒ **≤ 2 engine
  submissions** and the **final** request's sequence wins (`tests/ng_cache.rs::b6_…`). The
  `RenderScheduler` drives `Buffer`-target renders on a background thread; observable via
  `submissions()` / `last_output()`.
- **`RenderScheduler::canvas()` stays A13/A-gpu + F5.** The GPU canvas double-buffer publisher needs
  the shell's shared device (not available at `RenderScheduler::new(engine, jobs)`); the B6 coalescing
  mechanism is fully wired and headless-provable without it. `canvas()` remains the A13/F5 stub.
- **B8 benches** compile and run reduced locally (`cargo bench -p lightbox-render`: `full_graph_cold`
  vs `tail_only_warm` vs `slider_churn_last_node`). Per §8 the p95/regression baseline is the nightly
  job (perf regressions tracked, not PR-blocking); no perf number is recorded here as a gate.

### Gate status (worktree, single-process on this Metal box)

- **B5 tail-invalidation gate (PR-blocking, §10.1 E05.2): GREEN** —
  `tests/ng_cache.rs::b5_tail_invalidation_recompute_probe_chain`: on a 6-node chain, changing node k
  evaluates exactly the tail and the upstream all cache-hit (plus the source-node edge case: changing
  the source re-keys the whole chain). B2 (chains **and** diamonds), B4 (zero re-fetch), B6 (≤ 2
  submissions + final-wins), B7 (poisoning fuzz == never-cancelled reference) all green.
- Exit bar in the worktree: `build`, `clippy -D warnings`, `fmt --check`, `deny check` all green;
  `cargo test --workspace` green **except** the pre-existing `lightbox-cli/tests/e02_e2e.rs` cases,
  which require the downloaded fixture corpus (`cargo xtask fixtures`) — an environmental precondition
  unrelated to Phase B (they pass on `main` where fixtures are present; B touches no CLI/decode code).

## Wave 2 — Phase C (E05.3 ROI / tile / progressive) — 2026-07-07, branch `e05-c`

Phase C landed **entirely additively**: it adds new modules under `ng/exec/`
(`scale`, `roi`, `tiling`, `progressive`, `priority`, `budget`), fills the testkit probe/harness
stubs (`test.blur_r`, `test.accum`, synthetic corpus, `run_soak`), adds a Lanczos-3 CPU resampler to
`util.resize`, and adds integration tests. **No edits to `engine.rs`, `stats.rs`, `cache/`,
`recover/`, or `compile/`** — zero cross-wave friction; nothing in B/D/E territory was touched, and
the **frozen `exec::backend` seam and `tile` currency were NOT reshaped** (the tile executor drives
`RenderNode::eval_cpu` directly with ROI-aware `CpuTileView`s beside the whole-tile `Backend` seam,
so no additive field to any shared exec type was needed — the merge agent has nothing to reconcile
here).

### Design decision — the tile executor is CPU-reference (not GPU), by intent

C3's gate is "tiled render **exactly equals** untiled render (**same backend**)". The CPU path is the
deterministic, bit-exact reference the whole consistency contract is defined against (§4.4/R3:
"goldens rendered from the CPU reference path"), so the tiled evaluator
(`ng::exec::tiling::TileRender`) is implemented on the CPU path and the C3 gate is proven **byte-for-
byte** there (`tests/tiling.rs::tiled_equals_untiled_for_gain_and_blur`, both point-wise gain and
radius-3 blur). GPU per-tile dispatch is already supported by `GpuBackend` per-node (it dispatches
over the output extent); wiring GPU tiling end-to-end + its own tiled==untiled check is **deferred**
to the E6 CPU/GPU-parity work / merge pass — the apron contract (`plan()` + edge-replicate) is
backend-agnostic and the nodes are apron-correct by construction. **Not faked**: the exact-equality
gate runs for real, on the reference backend, in-gate.

### Gate disposition (all green in-gate on this Metal box)

| Task | What ships / proof | Gate |
|---|---|---|
| **C1** ROI planning | `ng::exec::roi::plan_rois` back-props `plan()` through the compiled graph; `test.blur_r` has a radius-dependent apron `out.expand(ceil(r·scale))`. `composed_aprons_sum_and_are_minimal` proves composed aprons == analytic sum **and minimal** (clamped to image). | unit + property |
| **C2** scale-aware eval (**§10.1 E05.3 gate, PR-blocking**) | `ng::exec::scale::derive`: `RenderScale`→working extent, `scale_q` 1/64ths. `tests/scale.rs::fit_4k_over_45mp_evaluates_at_most_8mp`: a **45.1 MP** source (8192×5504) at `Fit(3840×2160)` derives **3215×2160 ≈ 6.94 MP ≤ ~8 MP**, and the tile executor evaluates every node-tile at ≤256²·apron, terminal total == working extent (probe-counted). `util.resize` decimates a real large source to the fit tier (box CPU + WGSL). | **PR-blocking** |
| **C3** 256² tile executor | per-tile apron reads + seam-free stitch; `tiled_equals_untiled_for_gain_and_blur` (byte-exact), `tiled_partial_roi_matches_window_of_whole`, `blur_smooths_without_a_seam_at_the_tile_boundary`. | PR-blocking |
| **C4** visible-first order + off-screen cancel | `TileOrder::CenterOut` (focus-nearest first); `render_with(still_wanted)` skips off-screen tiles on `set_view`. `center_out_ordering_and_offscreen_cancellation`. | unit + integration |
| **C5** 1:1 demand-driven + `TileSink` | `OneToOneSession` keyed by `TileCoord`; a pan evaluates only newly-visible tiles; `TileHandoff` offers completed tiles to a mock `TileSink` (E03 seam, fire-and-forget). `one_to_one_demand_driven_pan_and_tilesink`. | integration |
| **C6** progressive ladder | `ng::exec::progressive::Ladder` sequences BestAvailable→`PreviewReady`(display-only)→preview-res→full-res, emitting the **3-stage `RenderState` sequence** (`Rendering`→`PreviewReady(PreviewTier)`→`PreviewReady(PreviewRes)`→`Complete(FullRes)`). Proven by unit + `tests/progressive.rs` (real renders through the ladder). First-`PreviewReady` wall-clock is recorded in `LadderReport` (indicative locally; ≤100 ms warm-store gate is on the reference machine — see F1). **Engine-level emission** (`Engine::poll` returning these) integrates at F5; the mechanism + sequence are complete. | integration (mechanism) |
| **C7** VRAM budget + degradation | `ng::exec::budget`: `VramBudget::Auto` → `min(60% adapter mem, cap)` (with an honest fallback — wgpu has **no portable VRAM query**, so `Auto` uses a conservative default when the adapter doesn't surface memory; recorded here). `degrade_to_budget` drops render scale so a 100 MP source (deterministic byte math, no 800 MB alloc) fits a 512 MB budget within headroom; the `TilePool` LRU-evicts to budget (A7) bounds resident tiles. | unit (reduced-scale, see below) |
| **C8** F32 precision path | `precision()` honored end-to-end — the tile executor allocates `rgba32float` vs `rgba16float` per node. `test.accum` at **F32 = 154.3 dB** vs the f64 reference (≥60 ✓); the same node at **F16 = 77.3 dB** (demonstrably worse — documents why the hatch exists). `tests/precision.rs`. | PR-blocking |
| **C9** priority lanes | `ng::exec::priority::PriorityLanes` (two-lane, Interactive-first pop at tile granularity). `interactive_preempts_batch_at_tile_boundary`: with a 24-tile Batch in flight, an Interactive submit's first tile starts within ~1 tile-duration and the Batch still completes (no starvation). This is the **mechanism** (§1.1: E15 sets export policy). | unit (mechanism) |
| **C10** soak | `scenario::run_soak`: seeded xorshift, randomized param/zoom/pan over the tile executor; zero errors, bounded working set, final frame == fresh render. | integration (reduced-scale, see below) |

### Reduced-scale actuals (nothing faked; blockers named)

- **C2 (source materialization).** The **≤ ~8 MP** result for a literal 45 MP source is proven by
  `scale::derive` on the real 45 MP dimensions (no allocation) **plus** the executor evaluating at
  exactly the derived extent (probe-counted). The end-to-end **decimation-reading-a-real-large-
  source** half runs on a **3.8 MP** source (2400×1600) to keep the PR gate fast — materializing a
  literal 45 MP working buffer (~360 MB f16) is a nightly/reduced-scale item. The derivation and the
  executor path are byte-identical at any size; only the input-source size is reduced. Blocker:
  CI wall-clock/RAM, not capability.
- **C7 (100 MP under 512 MB).** Proven as **deterministic byte math** (`hundred_mp_degrades_under_512mb`)
  — no 100 MP/800 MB working tile is ever allocated (that is the *point* of the degradation). The
  real GPU-pool-peak integration (`TilePool::peak()` ≤ budget on a rebuilt device) rides the merge /
  `ng_gpu` GPU tests; the CPU tiling executor has no VRAM to peak. `Auto` budget probing is
  fallback-only until a portable adapter-memory query exists (wgpu limitation, named above).
- **C10 (10 k iterations).** In-gate runs **250 iterations** at modest working extents (≤288×192) in
  **5.8 s**, peak per-eval working set **49 536 px** (bounded ≈ one 256² tile + apron, ≤ 264²).
  The 10 k count at full corpus resolutions is the **nightly** soak (§8). Blocker: PR-gate wall-clock
  (debug-build blur is O(n·r²) per-pixel), not correctness.
- **C6 first-`PreviewReady` ≤ 100 ms** and **F1 p95** remain **reference-machine** gates (this dev
  box is not the reference-perf runner — see the top-level DEFERRED F1 row); the local wall-clock is
  recorded via `LadderReport::first_preview` as indicative only.

### `util.resize` — box kept as the node kernel, Lanczos-3 added alongside

The shipped `util.resize` kernel stays **area-average box** decimation (WGSL + CPU, the correct
anti-aliasing filter for large downscales) so the merged A-gpu parity test
(`resize_gpu_matches_cpu_box_decimation`) is untouched. C2's "Lanczos/box" is satisfied by adding a
**separable Lanczos-3 CPU resampler** (`nodes::resize::decimate_lanczos_cpu`, unit-tested: flat-field
preservation, monotone ramp, near-identity at unit scale) as the higher-quality option; a Lanczos
**WGSL** kernel is deferred (box is the shipped GPU kernel; parity holds). No golden regressions.

### New deps: none

Phase C added **no crate dependencies** (the soak RNG is a 6-line inline xorshift; parallelism reuses
`PixelBuf::par_fill_rows`). cargo-deny surface unchanged.

## Phase E — E05.5 device-lost recovery + CPU fallback (E1–E8) — 2026-07-07

Branch `e05-e` (worktree). Tasks landed: **E1 E2 E3 E4 E5 E6 E7 E8**. Full five-gate exit bar
green in the worktree on the real Metal adapter (`build` · `test` · `clippy --all-targets -D
warnings` · `fmt --check` · `deny check`; `cargo xtask fixtures` fetched for the unrelated E02 CLI
e2e preconditions). Owned files: `ng/recover/mod.rs` (rewritten from stub), `ng/engine.rs`
(recovery/events/active_backend wiring), an additive method on `ng/source/mod.rs`'s
`DeviceProvider`, and two new integration test files. **No edits** to `ng/cache/` (B),
`ng/exec/` tiling (C), `ng/compile/` (D), `ng/sched/` (B), or the testkit probe bodies (A-core/C) —
strictly disjoint from the concurrent B/C/D waves.

### The four §10.1 / DoD gates this phase owns (PR-blocking, GREEN on the Metal box)

- **E05.5 gate (E5):** `tests/ng_recover.rs::e5_gate_injected_loss_keeps_editing_at_preview_resolution`
  — an injected device-lost mid-session (`max_losses = 1`) degrades to CPU preview
  (`EngineEvent::DegradedToCpu`, `active_backend() == CpuPreviewOnly`) and successive interactive
  submits keep producing frames at preview resolution on the CPU.
- **CPU/GPU parity gate (E6):** `tests/ng_parity.rs::e6_cpu_gpu_parity_over_corpus_and_graphs` —
  every engine-owned node (`src.decoded`, `util.resize`, `xform.display`) + a point-op probe over
  6 synthetic corpus patterns, GPU vs CPU within **ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB** (the testkit's
  validated comparators); provenance asserted on every `RenderOutput`.
- **Determinism gate (E6):** `e6_each_backend_is_bit_deterministic_across_three_runs` — each backend
  bit-identical across 3 repeat renders of the most kernel-diverse chain.

E1 (detection: event + in-flight tickets resolve `Failed(DeviceLost)`, never hang), E2 (rebuild on a
**genuinely fresh** wgpu device via the seam), E3 (re-warm with zero `SourceProvider::fetch`), E4
(policy boundary + `active_backend` + explicit re-enable), E7 (degraded contract), and E8
(export-safety) each have a dedicated green test in `tests/ng_recover.rs`.

### D-E-1 (STRUCTURAL): the engine backend is now runtime-swappable (`Shared.live: RwLock<LiveBackend>`)

**What.** Device-lost recovery + degrade require swapping the pixel backend (GPU → fresh GPU on
rebuild; GPU → CPU on degrade) at runtime. A-core's `Shared` held one immutable `Executor`
(`backend: Arc<dyn Backend>` baked in). E replaces that with a swappable
`live: RwLock<LiveBackend { backend, gpu, lost }>`; `Engine::stats` reads a `Shared`-owned
`probe: Arc<RecomputeProbe>`, and each render builds a fresh `Executor::with_probe(current_backend,
tile_size, probe)` from the snapshot of `live`. **The frozen `exec`↔backend seam is untouched** — no
change to `trait Backend`, `Executor::evaluate`'s signature, or any executor internal (C's tiling
layers on `evaluate` unchanged). **Why here.** `ng/engine.rs` is E-owned for the recovery wiring and
is not touched by the concurrent B/C/D waves, so restructuring `Shared` is conflict-free.
**Behaviour preserved.** All pre-existing engine unit tests (`submit_is_fast…`, `cancel_mid_render…`,
`unsupported_pv…`, `poll_of_unknown…`) and the A9 engine GPU test pass unchanged. **Rollback.**
revert `ng/engine.rs`.

### D-E-2: device-generation guard against stale-device callbacks

A rebuilt-past device may fire its `device_lost` callback when it is deliberately destroyed during a
rebuild. A monotonic `device_gen: AtomicU64` — captured in each callback closure at install time and
checked in `signal_device_lost_gen` — makes a stale device's loss a no-op, so destroying the old
device on rebuild never spuriously re-triggers loss on the new one. `set_state` is now
**sticky-terminal** (never overwrites Complete/Failed/Cancelled) so an in-flight ticket's
`Failed(DeviceLost)` wins over a late worker `Cancelled`/`Complete` — the E1 "resolve, never hang"
guarantee.

### D-E-3: `DeviceProvider::adapter_info()` — additive `None`-default seam method

`active_backend() -> Gpu(AdapterInfo)` needs the adapter identity, but the frozen §3.8 seam yields
only `(device, queue)` and `wgpu::AdapterInfo` has **no `Default`**. Added an **additive**
`fn adapter_info(&self) -> Option<wgpu::AdapterInfo> { None }` to `DeviceProvider` (backward-
compatible — existing impls compile unchanged). The shell (F5) and the GPU test providers, which own
the adapter, override it with the real info; when absent the engine reports a clearly-labelled
"unknown adapter" placeholder (honest metadata — never a fabricated identity or gate number).
Touches `ng/source/mod.rs` (A-gpu-owned, already merged); no B/C wave touches `DeviceProvider`, so
the union is conflict-free.

### D-E-4: E-owned RAM source pin for re-warm (distinct from B's NodeCache RAM tier)

Re-warm (E3) requires the decoded source to survive device loss and re-upload with zero
`SourceProvider::fetch`. B owns the general two-tier `NodeCache` RAM pin (`PinLabel::SourceStage`),
which is **still stubbed in this worktree** (B runs in a parallel wave). Rather than reach into B's
file, E holds its own `source_pin: Mutex<HashMap<ImageId, Arc<SourceImage>>>` on `Shared`: the first
fetch pins the decoded pixels; every later render (including post-rebuild and post-degrade) re-uploads
from the pin. This is host memory, so it survives GPU cache eviction and device loss. When B lands,
its NodeCache RAM tier is the general home; F5/integration can reconcile the two (both are pure
caches — correctness never depends on either). Blast radius: `ng/engine.rs` only.

### D-E-5: `Engine::inject_device_lost` — a public test-only fault injector

The E05.5 gates run headlessly and deterministically without depending on the Metal driver to
actually drop the device. `Engine::inject_device_lost(reason)` drives `signal_device_lost_gen`
exactly as the real wgpu `device_lost` callback would (the real callback is also installed, via
`set_device_lost_callback`, and routes to the same code). It is a plain `pub fn` documented as
"not part of the shipping API surface" (kept public so integration tests in a separate crate can call
it; a `#[cfg]` feature would exclude it from the default `cargo test` gate). F5/promotion can gate it
behind a `fault-injection` feature if desired.

### D-E-6: E7 degraded preview clamp uses the engine-owned box decimator (C2 stand-in)

The degraded interactive contract ("full-res never leaves the interactive path") is enforced by
badging the output `PreviewRes` **and** shrinking the delivered frame. True fit-viewport decimation
inside the pipeline is C2's (`util.resize` by `RenderScale`), which is not landed in this worktree.
E's clamp therefore box-downscales the (CPU-resident, since degraded) **terminal tile** via the
engine-owned `decimate_box_cpu` to a preview footprint (currently a 0.5 stand-in for the fit factor).
`Batch` requests are never clamped (full-res allowed, badged `FullRes`). When C2 lands, the in-pipeline
resize supersedes this terminal downscale; the *contract* (marking + no silent full-res interactive +
Batch full-res) is unchanged. Fine-grained tile progress on the Batch path likewise rides on C's
tiling; today the coarse `Rendering{…}` states are the progress surface.

### D-E-7: E6 parity gate carries a self-contained point-op probe (does not touch `probes.rs`)

The testkit `GainProbe`'s GPU kernel is deferred and owned by C's territory in
`lightbox-render-testkit/src/probes.rs` (C fills `BlurRProbe`/`AccumProbe` there in a parallel wave).
To keep Phase E strictly file-disjoint from C, the parity gate (`tests/ng_parity.rs`) carries its own
point-op gain node (fixed ×0.75, WGSL + CPU) rather than editing `probes.rs`. The gate proves E6 on
the three **real** engine-owned deliverable nodes plus this representative point-op — full cross-
backend numerical parity on the real device. Completing the testkit `GainProbe::eval_gpu` for D's
GPU golden matrix remains available to C/D; it is not required for the E6 parity gate.

### D-E-8: dev-dependency edge `lightbox-render` → `lightbox-render-testkit`

`tests/ng_parity.rs` reuses the testkit's **validated** ΔE2000 (Sharma-Wu-Dalal) / PSNR comparators
as the single ground truth, so `lightbox-render` gains a **dev-dependency** on
`lightbox-render-testkit`. This forms a dev-only cycle (`render` →[dev] `testkit` →[normal] `render`)
which Cargo permits (the render *library* never depends on the dev/test testkit; only its test target
does — `cargo build --workspace` never traverses it). Blast radius: one line in
`crates/lightbox-render/Cargo.toml` `[dev-dependencies]`.

### Nothing deferred inside Phase E's own scope

All eight tasks E1–E8 build and gate for real on this Metal box. The DEFERRED items in the §0
disposition (F1 reference-perf runner, F2 non-macOS GPU legs, F4 24 h soak, D3 full nightly matrix,
C7/C10 scale) are other phases' and are unchanged by Phase E. Phase E adds **no** fabricated perf
numbers, cross-platform runs, or goldens.

## Wave BCE — MERGE integration (e05-b, e05-c, e05-e → main) — 2026-07-07

Merge agent, single-process on `main` (real Metal adapter). The three phase branches merged
`--no-ff` **in order** — `e05-b`, then `e05-c`, then `e05-e`. Full five-gate exit bar re-run green
on `main` after the resolutions below. `rawler`/`dnglab` confirmed absent from `Cargo.lock`.

### Conflicts resolved

- **`E05-deviations.md`** (all three merges) — append-only union of each phase's section, markers
  dropped, ordering B → C → E preserved.
- **`crates/lightbox-render/Cargo.toml` `[dev-dependencies]`** (e05-b × e05-e) — union: e05-b's
  `blake3` + `criterion` + the `[[bench]] name = "cache"` entry (B8) and e05-e's
  `lightbox-render-testkit` path dev-dep (E6 parity comparators). Both retained.
- **`Cargo.lock`** — auto-merged by git (both branches added workspace-internal `[[bench]]`/dev-dep
  edges already present in the graph); no regeneration needed. Verified `cargo build` re-locks clean
  and no `rawler`/`dnglab` entry exists.
- **`crates/lightbox-render/src/ng/engine.rs`** (e05-b × e05-e, the one substantive conflict — both
  phases legitimately extend this A-core-owned file per the FILE-OWNERSHIP MAP: B wires the
  content-keyed cache through the render path; E rewrites `Shared`/`process`/`finish` for
  device-lost recovery). Six conflict hunks reconciled by **keeping BOTH sides' additions** on top of
  E's structural rewrite (E is the superset shape — it removed the single `Shared.executor` field in
  favour of a per-render `Executor` built around the swappable `live` backend):
  1. **imports** — union (E's `RwLock`/`Weak`, `recover::{DegradeState,RecoverStateMachine}`,
     `nodes::resize::decimate_box_cpu`, `source::SourceImage` + B's `RecomputeProbe`; B's
     `cache::{CacheKey,CacheKeyInputs,PinLabel}`, `graph::{NodeIndex,RenderGraph}`,
     `types::{TileCoord,TilePrecision}`, `config::VramBudget` were already auto-merged outside the hunk).
  2. **source-stage injection in `process`** — kept B's content-keyed block (`source_key` →
     `cache.peek` → on-miss `cache.pin_tile(…, PinLabel::SourceStage)` → `SourceInject{idx,tile,key}`)
     but calling **E's** `fetch_source_tile(&req, &cancel, gpu.as_ref())` (which adds the snapshot-`gpu`
     arg and the host `source_pin` re-warm). The per-render `executor.evaluate(&graph, req.pv, …)` call
     auto-merged (E's per-render executor + B's `pv` arg + the merged `exec` signature).
  3. **`source_key` method (B) vs `fetch_source_tile` doc line (E)** — kept B's `source_key` method
     verbatim; used E's "Obtain the decoded source …" doc line on the merged `fetch_source_tile`.
  4. **`with_compiler` probe/cache construction** — kept B's shared `RecomputeProbe` +
     `NodeCache::with_budget_and_probe(vram_budget_bytes(&cfg), …)`; dropped B's now-unused local
     `executor` (E builds the executor per render).
  5. **`Shared` literal** — dropped the removed `executor` field; used B's budgeted `cache` local; wired
     the single shared `probe` into `Shared.probe` (so the per-render executor and the cache share one
     recompute probe — B's "one shared probe" intent).
  6. **`stats()` + `vram_budget_bytes`** — merged body reads `self.shared.probe.snapshot()` (E; the
     `Shared.executor` field is gone) **and** folds `cache.stats()` evictions/bytes (B); kept B's
     `vram_budget_bytes` free function (needed by hunk 4).

### Integration fix (folded into the merge commit to keep `main` green at every commit)

The B-cache × E-recovery interaction is a **runtime** conflict the compiler can't catch: B pins the
uploaded source tile in the cache's **RAM/pinned** tier (`pin_tile`), and on the GPU path that pinned
tile holds a pooled texture on the live device. E's recovery swaps the device out from under it
(rebuild GPU→GPU, or degrade GPU→CPU) but — coming from a branch where the cache was never populated
(pre-B `Executor` did not consult the cache) — never dropped it. Result before the fix: after an
injected device-lost + degrade, the next CPU render's `cache.peek(source_key)` served the **stale
GPU-resident** source tile, which the CPU path cannot read back →
`e5_gate_injected_loss_keeps_editing_at_preview_resolution` **FAILED** with
`Readback("terminal tile is GPU-resident but no device context is available for readback")`.

**Smallest-correct fix (2 lines):** call `self.cache.clear()` in `Shared::try_recover` (after the
rebuild swap) and in the degrade branch of `Shared::signal_device_lost_gen` (after swapping to
`CpuBackend`). This realizes exactly what E's own code comment already promised ("The VRAM cache tier
is dropped with the old backend") using the cache method **B** shipped for it. `clear()` (not
`invalidate_vram()`) is required because B's source pin lives in the pinned tier, which
`invalidate_vram()` deliberately preserves; on device loss every resident tile is stale, so both
tiers must go. E's **separate** host-memory `source_pin: Mutex<HashMap<ImageId, Arc<SourceImage>>>`
(decoded pixels, not GPU textures) is untouched by `clear()`, so the re-warm still performs **zero**
`SourceProvider::fetch` (E3) — `e2_e3_single_loss_rebuilds_on_fresh_device_without_refetch` stays
green. B's `b4_source_pin_means_zero_refetch_on_downstream_change` is unaffected (a param change
never triggers device loss, so `clear()` never fires on that path). Blast radius: two 1-line calls in
`ng/engine.rs`; no signature, seam, or golden change.

### Post-merge gate status on `main`

`cargo build --workspace` · `cargo test --workspace` · `cargo clippy --workspace --all-targets -D
warnings` · `cargo fmt --all --check` · `cargo deny check` — all green (test actuals recorded in the
merge report). The four §10.1 phase gates (A16 golden, B5 tail-invalidation, C2 fit-view ≤~8 MP, E5
degrade-keeps-editing) and the E6 CPU/GPU parity + determinism gates all pass on the real Metal
adapter post-merge.

## Phase D completion — DoD #2 (per-PV golden immutability guard) — 2026-07-07

**Context.** The agent originally assigned Phase D (D1–D5) died mid-task (API failure) before
wiring the gates or writing this section. Its artifacts (`ng/compile/manifest.rs`, `pv-manifests/
pv1.json`, the per-PV template logic in `ng/compile/mod.rs`, `corpus.rs`'s golden-matrix runner,
the 12 committed matrix goldens, `tests/ng_pv.rs`) were salvaged onto `main` as-is at `35a66d4` /
merged at `d28195b` ("salvaged PV registry — D2/D3/D4 gates to be completed"). An independent
verify pass flagged DoD #2 as unsatisfied: `run_matrix` allegedly never called by a test, PV999's
divergent topology unconfirmed, and no kernel-salt trip-then-revert test. This entry (branch
`e05-d2`, worktree off `d28195b`) closes the gap and records what was actually found.

**Re-verification finding: two of the three flagged gaps were already closed on `main` at
`d28195b`.** Building + testing the salvaged worktree before touching anything showed:

- **D3 (`run_matrix` wiring) was already real.** `corpus::tests::per_pv_matrix_subset_within_tolerance`
  calls `run_matrix(&matrix_cases(MatrixScope::Subset))` (12 cases: 3 corpus kinds × 2 gains × 2
  PVs) and asserts every case's `GoldenReport::passed`. It is **not** `#[ignore]`d, runs inside
  `cargo test --workspace`, and CI's PR gate (`.github/workflows/ci.yml`, `test:` step) is
  literally `cargo test --workspace` on all three OSes with no filtering — so this gate was already
  PR-blocking. All 12 committed goldens under `crates/lightbox-render-testkit/goldens/matrix/`
  matched current render output within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; **no goldens needed reblessing.**
- **D2 (PV999 divergent topology) was already real, at two levels.** At the compiler/template
  level, `GraphTemplate::pv_test_999()` (`ng/compile/mod.rs`) drops the `util.resize` stage PV1
  carries (3 nodes/2 edges vs 2 nodes/1 edge — `per_pv_template_selection_diverges_topology`). At
  the golden-matrix level, `corpus::build_matrix_graph` adds a `test.blur_r` stage for PV999 that
  PV1 does not have (`source → gain` vs `source → gain → blur`), producing **distinct committed
  goldens** (`pv1_and_pv999_produce_distinct_goldens`). `CacheKey::derive` (`ng/cache/mod.rs`)
  folds `pv` directly into the hash (independent of `kernel_salt`), and
  `corpus::tests::no_cross_pv_cache_pollution` proves this behaviorally — rendering PV1 then PV999
  through one shared cache re-evaluates every PV999 node (no stale PV1 tile served).

**The one gap that was real: D4's "trip-then-revert" tests existed but were not honest about their
own provenance, and neither exercised the actual registered-kernel-salt mechanism.** Both
`manifest.rs::a_salt_change_on_a_shipped_stage_is_detected` and
`corpus.rs::pv999_kernel_tweak_trips_the_golden_gate` carried a doc comment claiming "the real
trip-then-revert … was performed once and recorded in `E05-deviations.md`" — **no such record
existed anywhere in this file.** That claim was false. Separately, neither test actually drove a
change through `NodeRegistry`/`NodeFactory::kernel_salt()` (the literal "registered PV's kernel
salt" the task names): `manifest.rs`'s test mutated a `PvManifest` struct post-hoc, and
`corpus.rs`'s test mutated a `MatrixCase.blur_radius` field directly — both bypass the registry
entirely (the golden-matrix's probe nodes are wired with `graph.add_node`/`add_node_with_params`,
which stamp an id-derived `default_salt`, not a registry-resolved one — see the doc comment on
`ng::graph::default_salt`).

**Fix — `crates/lightbox-render-testkit/src/corpus.rs`:**

1. Added `d4_registered_pv999_kernel_salt_change_trips_then_reverts_the_matrix_gate`: a scratch
   `NodeRegistry` registers `test.gain`/`test.blur_r` **only** for `PV_TEST_999`
   (`PvRange::single`, never `PV1`), resolved via `registry.resolve` + `registry.kernel_salt` and
   stamped with `graph.add_node_full` — the exact mechanism `RecipeCompiler::compile` uses (task
   B2). The test renders three times against the **existing committed**
   `goldens/matrix/highfreq/gain200/pv999.png` (no new golden files added):
   - **Baseline (green):** the shipped `BlurRFactory` salt (`test.blur_r@v1`) + shipped default
     radius (`MATRIX_BLUR_RADIUS` = 2.0), resolved through the registry, reproduces the committed
     golden exactly — also a cross-check that the registry-driven path renders the same pixels as
     `build_matrix_graph`'s raw-`Arc` path.
   - **Tweak (trips):** a second registry registers the same node id under PV999 with a
     **different** `KernelSalt` (`blake3(b"test.blur_r@pv999-tweaked-v2")`, distinct from the
     shipped `test.blur_r@v1`) *and* a different radius (5.0) — standing in for "a contributor
     touched `test.blur_r`'s CPU body and, per discipline, bumped its `KernelSalt`". This render
     drifts beyond ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB from the committed golden — the D3-style gate trips,
     for real, inside the test run.
   - **Revert (green again):** a third registry, identical to the baseline (same salt, same
     radius), renders byte-identical pixels and passes the gate again.
   - Comparison goes through a new **read-only** `compare_committed_readonly` helper (not the
     existing `compare_srgb8_to_golden`), which never writes the golden file even under
     `LIGHTBOX_BLESS=1` — the existing blessable comparator would have let a stray
     `LIGHTBOX_BLESS=1` local run overwrite the committed PV999 golden with the *tweaked* (wrong)
     pixels partway through this test. This is a real footgun the salvaged code did not guard
     against for any multi-render-per-golden test; `compare_committed_readonly` closes it for this
     one.
2. Strengthened `pv999_kernel_tweak_trips_the_golden_gate` (the pre-existing `MatrixCase`-level
   test) with an explicit third step — re-running the untouched `committed` case after the tweak
   and asserting it passes again — so the tweak → trip → revert → green cycle is fully live in one
   test run, not just implied by the untouched baseline having been checked first.
3. Removed the false "recorded in `E05-deviations.md`" claims from both tests' doc comments;
   pointed them at this entry and at the new registry-mechanism test.

**Fix — `crates/lightbox-render/src/ng/compile/manifest.rs`:** corrected
`a_salt_change_on_a_shipped_stage_is_detected`'s doc comment the same way — it remains a valid
*manifest-layer* mechanism proof (a salt bump flips manifest equality), but no longer claims a
prior live trip-then-revert that never happened; it now points at the real one in `corpus.rs`.

**Formatting.** `cargo fmt --all --check` was **not** clean on the salvaged `d28195b` commit inside
Phase D's own files (`ng/compile/manifest.rs`, `ng/compile/mod.rs`,
`lightbox-render-testkit/src/corpus.rs` — all pre-existing drift, not introduced by this pass; no
other crate had drift). Ran `cargo fmt --all` (formatting only, no logic changes) so the exit bar's
fmt gate is genuinely green.

**Exit bar, this worktree (`e05-d2`, off `d28195b`), all green:**
`cargo xtask fixtures` (14 fixtures, cached after first run) →
`cargo build --workspace` ok →
`cargo test --workspace` ok, 0 failed (includes the 3 `lightbox-cli e02_e2e` tests, which need the
fixture corpus and pass once it's fetched) →
`cargo clippy --workspace --all-targets -- -D warnings` clean →
`cargo fmt --all --check` clean →
`cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok` (pre-existing duplicate
`windows_x86_64_*` warnings only, unrelated to E05, not new).

**DoD #2 status: genuinely satisfied and PR-blocking.** The corpus × recipe × registered-PV matrix
(`per_pv_matrix_subset_within_tolerance`) runs on every PR across all three CI OSes with no
filtering and fails the build on any drift beyond ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; PV999's divergent
topology is real at both the template and golden-matrix layers with no cross-PV cache pollution
(`pv` folded directly into `CacheKey`); a registered PV999 kernel-salt change demonstrably trips
that gate and reverting it is green again, proven live through the actual `NodeRegistry` mechanism
in `d4_registered_pv999_kernel_salt_change_trips_then_reverts_the_matrix_gate`. The full nightly
matrix (`corpus::tests::full_matrix_nightly`, `#[ignore]`d in the PR gate per the §0 disposition) is
unchanged by this pass. The "new algorithm ⇒ new PV" contributor workflow is documented in
`ng/compile/manifest.rs`'s module doc (§ "Contributor workflow when you need to change a shipped
node's algorithm").
