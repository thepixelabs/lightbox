# Lightbox — Session Handoff & Resume Guide

_Last updated 2026-07-05. This document lets any fresh session (any model) resume the program without prior context. Read this first, then only what it points to._

## 1. What this project is

**Lightbox**: open-source, local-first, **editing-first raw photo editor** (color correction, tone, local adjustments, retouch, AI enhancement). **There is no library/DAM** — entry is drag-and-drop (file/multi-file/folder) or the OS open dialog, landing straight in an editor with a filmstrip of the opened set. Raw files expose the complete raw parameter surface; JPEG/TIFF/PNG/HEIC run the same pipeline minus raw-only stages. All AI runs offline. See `00-mandate.md` (v2.1 — the binding contract).

**Stack (approved, do not re-litigate):** Rust workspace · egui/eframe shell sharing ONE wgpu device with the render engine (zero-copy) · wgpu compute node-graph DAG (vkdt pattern) · SQLite edit store (WAL, content-hash keyed) · out-of-process ONNX inference (`lightbox-inferd`) · ISO 16684 XMP Toolkit behind an own `crs:`/`lb:` mapping layer. License policy: MIT/BSD/Apache free; LGPL dynamic-link only; GPL never in-process; enforced by cargo-deny + REUSE in CI.

## 2. Current state (2026-07-05)

| Artifact | State |
|---|---|
| Research (352 features, 10 domains) | ✅ `../research/` |
| Architecture v2.0 (editing-first) | ✅ CTO-approved; `01-architecture.md` |
| Governance record | ✅ `02-approval.md` (v1.x trail; v2.0 approved CTO round 1) |
| **v2.1 addendum** (AI Looks epic + complete-raw-surface contract) | ✅ **LANDED & CTO-approved** (2026-07-05). E17 in `01-architecture.md` + §2.4 raw-surface binding; `epics/E17-ai-looks.md` (28 tasks). Recorded in `02-approval.md` §7. **One follow-up gates E10/E11 planning** (not E02): extend §3.2 recipe schema to persist raw-only params — see `02-approval.md` §7 follow-up #1. |
| **E01 Foundation** | ✅ **BUILT & independently verified** (~23k LOC, commits `4ff768f…57039a0`) |
| E02–E17 | 📋 Specced, not built (see §3) |

**E01 delivered:** 16-crate workspace; 3-OS CI + license gate (GPL-canary tested); crash-safe SQLite store (kill -9 fault-injection verified); jobs system; headless `lightbox-core` façade; decode probe (CR2/CR3/NEF/ARW/RAF/ORF/DNG, permissive in-crate walkers — rawler was dropped from probe for LGPL); embedded-preview pipeline; render Engine seed with one real GPU node + CIEDE2000 golden harness; virtualized grid + Engine-rendered loupe; CLI; perf harness; `cargo xtask exit-drill`. Deviations log: `epics/E01-deviations.md`. Handoff: `epics/E01-handoff.md`. Known pending: Win/Linux real-hardware exit-drill legs; shell-smoke CI legs are continue-on-error; `licensing.md` needs the egui-font/BSL scoped-exception entries documented.

## 3. Epic board (v2.0/v2.1)

Authoritative table lives in `01-architecture.md` §10. Summary — spec file per epic in `epics/`:

| Epic | Title | Milestone | Status | Depends on |
|---|---|---|---|---|
| E01 | Foundation (edit store, engine seed, shell skeleton) | M0 | **BUILT** | — |
| E02 | Decode & color foundation | M1 | spec ready | E01 |
| E03 | Preview pyramid & raw cache | M1 | spec ready (v1.x spec valid) | E01 |
| E04 | Working-set loader & drag-drop intake | M1 | **v2 spec** `E04-working-set-loader.md` | E01, E03 |
| E05 | Render node-graph engine | M1 | spec ready | E01, E02 |
| E06 | Jobs & background system | M1 | spec ready (mostly seeded in E01) | E01 |
| E07 | ~~Catalog DAM~~ | — | **RETIRED** | — |
| E08 | Editor shell, drag-drop entry & develop UI | M1 | **v2 spec** `E08-editor-shell.md` | E01,03,04,05,09 |
| E09 | Edit state, history, presets, XMP | M1 | **v2 spec** (edit store = primary data model) | E01 |
| E10 | Develop — global toolset | M2 | spec ready | E02,05,09 |
| E11 | Detail, optics, geometry, clean-room demosaic | M2 | spec ready | E02,05 |
| E12 | Masking, local adjustments & retouch | M3 | spec ready | E05,10 |
| E13 | ML inference platform (`lightbox-inferd`) | M3 | spec ready | E01,06 |
| E14 | AI masking | M3 | **v2 spec** `E14-ai-masking.md` | E12,13 |
| E15 | Export & output engine | M2 | spec ready | E05,06,09 |
| E16 | Interop & v1 hardening | M4 | **v2 spec** `E16-interop-hardening.md` | E09,14,15 |
| E17 | AI Looks — image-adaptive cinematic grading | M2/M3 | ⏳ verify exists (v2.1 addendum) | E09,10 (E13 optional) |

**M1 exit criterion (next milestone):** a dropped raw file on screen with working WB/exposure/tone editing that auto-persists and survives kill -9.

**Recommended execution order:** E02 → E05 (these unblock everything) → E09 + E04 in parallel → E08 → *M1 done* → E10 + E15 → E11 → E17 → E12 → E13 → E14 → E16.

## 4. How to build & verify

```sh
export PATH="$HOME/.cargo/bin:$PATH"   # rustup-installed 1.96.1, pinned in rust-toolchain.toml
cargo xtask fixtures                    # one-time: hash-pinned CC0 raw corpus
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
cargo deny check                        # license gate
cargo run -p lightbox-shell             # the app (stays open)
cargo run -p lightbox-shell -- --smoke 60  # 1-second self-test, EXITS BY DESIGN (not a crash)
cargo xtask exit-drill                  # kill -9 crash-safety drill
```

## 5. The working method (reproduce it)

The pipeline that produced everything so far — reuse it per epic:

1. **Execute an epic** — two patterns:
   - **Sequential** (E01): one phase agent at a time on `main`, commit per phase. Simple, robust, slower wall-clock.
   - **Parallel waves** (E02, owner asked for max parallelism): a serial **scaffold phase** first that pre-creates ALL crates/module-stubs/workspace-members + a superset of deps, so downstream phases touch **strictly disjoint files**. Then run independent phases concurrently with `isolation: 'worktree'` (each on its own branch `e02-<x>`), and a **merge agent** integrates each wave onto `main` (conflicts limited to Cargo.lock → regenerate, and a few Cargo.toml/lib.rs lines → union). Respect the dependency DAG between waves. Tradeoff: parallelism cuts wall-clock but costs MORE tokens (isolated rebuilds + merge agents) — it saves time, not tokens.
   - Either way, each phase agent: reads spec + architecture + existing code; adapts to current crate versions (record every deviation in `epics/<EPIC>-deviations.md`); exit bar = build+test+clippy+fmt+deny all green; never commit broken; return status done/blocked.
2. **Verify** with an independent agent: full gate + walk the spec's definition-of-done with file:line evidence + exercise the feature end-to-end; fix smallest-correct; commit `"<EPIC> verify: <what>"`.
3. **Plan changes** (scope/feature changes) go through governance: amend `00-mandate.md` (versioned note) → architect agent revises `01-architecture.md` → CTO agent reviews (loop on blocking issues, ≤3 rounds) → spec agents write/rewrite affected `epics/*.md` → record in `02-approval.md`. Mind the race: **commit mandate edits before launching the review**, and verify the reviewer saw the latest mandate.
4. Agent personas that worked: `system-architect` (design/revision), `cto` (approval), general staff-engineer prompts (specs + implementation). Structured-output schemas on every agent keep results machine-usable.

## 6. Product guardrails (owner's expressed intent — do not drift)

- Editing/color is the product: color tuning, raw develop, image improvement. No library features, ever, unless the owner reverses.
- Drag-and-drop must feel like the front door, not a bolt-on.
- AI = local only; AI Looks must output *editable recipes* (wheels/curves/HSL deltas), never baked filters; shuffle/variations must be seeded-deterministic (same seed + image → same look).
- Raw files: expose **all** pipeline-variable parameters; non-raw: same UI, raw-only stages hidden.
- The owner cares about honest reporting: never claim a gate passed that didn't; log deviations.

## 7. Open items beyond the epic board

- ✅ v2.1 addendum verified & committed (2026-07-05).
- **Before E10/E11 phase planning**: close CTO follow-up #1 (`02-approval.md` §7) — extend §3.2 CBOR recipe schema + XMP mapping for raw-only params. Editorial (architect), bounded. Does NOT block E02.
- Smoke-mode UX: window closing after N frames reads as a crash to users; proposed fix (verdict banner or stay-open) offered, not yet approved/implemented.
- `docs/plan/licensing.md`: add the scoped cargo-deny exceptions from E01 Phase 2 (BSL-1.0 clipboard-win/error-code via arboard; OFL/Ubuntu-font epaint fonts).
- Promote shell-smoke + grid-scroll CI legs to blocking once observed green on hosted runners.
- Win/Linux real-hardware exit-drill runs.
- `02-approval.md` records v1.x; append v2.0/v2.1 approval entries (v2.0: CTO approved round 1, 2026-07-05; v2.1: pending the addendum result).
