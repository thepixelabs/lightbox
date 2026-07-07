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
