<!--
SPDX-FileCopyrightText: 2026 Lightbox contributors
SPDX-License-Identifier: Apache-2.0
-->

# E01 handoff — foundation: workspace, catalog, headless core, shell skeleton

The T29 handoff note for every epic that builds on E01 (E02–E09, E13).
Spec: `E01-foundation-workspace-catalog.md`; implementation deltas:
`E01-deviations.md` (normative — read it before consuming any surface).

## 1. M0 exit drill (spec §8 DoD 1)

Scripted drill: `cargo xtask exit-drill` (builds the release binaries,
stages a 1 000-file corpus — every intact fixture + pad-unique raw
variants + synthetic JPEGs — then create → import → list → render →
check → backup, a **kill -9 mid-import** leg, and the shell smoke).

Recorded runs on real hardware:

| platform | hardware | result |
|---|---|---|
| macOS (Metal) | Apple M5 Max | **PASS** 2026-07-05 — import 1k = 209 ms; kill -9 landed with 192/1000 rows committed → `check` clean → re-import completed (808 + 192 dup-skip); render via `Engine::submit` (GPU); shell smoke: grid → loupe, engine texture composited zero-copy on the shared device |
| Windows (DX12) | — | **PENDING** — run `cargo xtask exit-drill` on a real-GPU Windows box; CI (WARP) covers the automated equivalents |
| Linux (Vulkan) | — | **PENDING** — run `cargo xtask exit-drill` on a real-GPU Linux box; CI (lavapipe) covers the automated equivalents |

The interactive half (scroll the grid during an import, flip the loupe,
F1 overlay: frame p95 < 16 ms, nav p95 < 50 ms) was performed on the
macOS machine; the scripted `--perf-scroll` capture (below) records the
same numbers mechanically.

## 2. Definition-of-done walk (spec §8)

| # | item | status | evidence |
|---|---|---|---|
| 1 | M0 exit: 1k import, grid+loupe, loupe via `Engine::submit` zero-copy, kill -9 clean | macOS witnessed; Win/Linux pending real hardware | §1 above; `cargo xtask exit-drill`; smoke asserts the shared-device composite |
| 2 | PR gates green on 3-OS matrix | wired in `ci.yml`; full gate set verified locally (build/test/clippy/fmt/deny/lints) | 48 test targets green; `cargo deny check` ok; `lint-migrations`/`lint-native-deps` ok |
| 3 | Nightly gates running | workflows committed (`nightly.yml`: 1000-iter fault injection, T28 perf + grid-scroll + criterion); every command verified locally | baselines: `tools/lbx-perf/baselines.json` |
| 4 | Frozen surfaces documented + consumed headlessly | done | CLI e2e (`crates/lightbox-cli/tests/e2e.rs`) drives create→import→list→render→check→backup with zero shell deps; §4 below |
| 5 | Catalog invariants (single-txn mutations, verified backup, corrupt refusal naming newest backup, synthetic-0002 upgrade test) | done | `lightbox-catalog` tests: `synthetic_0002_upgrade_writes_pre_upgrade_copy`, backup/retention suite, fault injection |
| 6 | No native C deps beyond the two spec-mandated ones; SBOM placeholder in CI | done | `native-inventory.toml` + `cargo xtask lint-native-deps` (CI step); bundled-C set = {libsqlite3-sys, zstd-sys}, both spec-mandated |
| 7 | Effort honesty | recorded | §7 below |

## 3. Perf baselines (T28, recorded on the drill machine)

`cargo run --release -p lbx-perf` — committed to
`tools/lbx-perf/baselines.json`; nightly publishes the comparison table.

| number (§6/§7) | budget | measured (M5 Max) |
|---|---|---|
| page-query p95 @100 k | < 100 ms | **0.22 ms** |
| nav-swap p95 (warm, Metal) | < 50 ms | **7.5 ms** (CPU-only: 8.6 ms) |
| import-1k wall (scenario corpus) | browsable during import | 63 ms; first image queryable at 40 ms |
| grid-scroll frame p95 (`lightbox --perf-scroll`) | < 16 ms | **9.2 ms** (120 Hz vsync cadence) |
| kill -9 fault injection | 0 corruptions | 0 across PR subset (50) — nightly runs 1000 |

Semantics: exceeding a *budget* is a REGRESSION (nightly files a tracked
issue, non-blocking per §8); baseline ratios are advisory (`watch`)
because runners differ from the recording machine.

## 4. Frozen surfaces (consumers → owners)

Per spec §3 — changes after M0 need a joint review with the owning
epic's planner. All live where §3 says, with deviations noted in
`E01-deviations.md`:

- **`lightbox-types`**: id newtypes, `ContentHash`, `Orientation`,
  `ProcessVersion`/`PV_M0`, `Flag`, `SourceTier` (moved here — Phase 4).
- **`lightbox-catalog`** (E07/E09/E12/E14): `Catalog`
  create/open/writer/reader/integrity/backup_verified; `CatalogTxn`/
  `ReaderHandle` DAOs; keyset `ImageQuery`; migration runner + registry
  (`docs/plan/migrations.md`, `0001` spine reserved; lint:
  `cargo xtask lint-migrations`).
- **`lightbox-jobs`** (E06): `Class`, `CancelToken`, `JobSystem::spawn`/
  `spawn_blocking`, `JobHandle`.
- **`lightbox-render`** (E02/E05): `GpuContext`, `Engine`
  new/submit/poll/cancel/on_device_lost/backend_kind, `RenderRequest`/
  `RenderState`/`RenderOutput`, `RenderNode`/`NodeRegistry` keyed
  `(NodeId, ProcessVersion)`, `SourceResolver`. Non-frozen scaffolding:
  `RenderPlanner` (E05.1 replaces).
- **`lightbox-preview`** (E03): `PreviewProvider`/`PreviewClass`/
  `PreviewState`/`DecodedImage`; `AssetLocator` is wiring, not frozen.
- **`lightbox-decode`** (E02): `probe`/`read_embedded`/`hash_file`
  frozen; `decode_raw`/`decode_image` declared (`Unimplemented`).
- **`lightbox-edit`** (E09): `Recipe { schema, pv }` + `identity()` only.
- **`lightbox-core`** (E08 + everyone): `Core`/`Session`, `Command`/
  `Event` (`#[non_exhaustive]`), `Queries`, `close(CloseOpts)`; additive:
  `check_catalog`, `Session::schema_version`.
- **`lightbox-ingest`** (E04): walk/probe/hash/batch-insert primitives,
  `ImportOptions` `#[non_exhaustive]`, `ImportReport`.

## 5. Migration registry state

`0001_spine` shipped (schema_version = 1). Registry:
`docs/plan/migrations.md`, linted in CI. The copy-on-write pre-upgrade
path is exercised by a synthetic-`0002` test; next epic to add a table
reserves `0002` via PR (E03 `preview` is the expected claimant).

## 6. Fixture corpus

`fixtures/manifest.toml` (pinned xxh3 + license per file): 9 CC0 raws
covering CR2/CR3/NEF×2/ARW/RAF×2/ORF/DNG, generated JPEG/PNG/TIFF, and
2 deliberately-corrupt files. `cargo xtask fixtures` fetches/generates,
verifies pins, idempotent + offline once cached. CR3 is the one mount
that rejects trailing-pad bytes (ISO-BMFF) — relevant when synthesizing
unique-content corpora (see `xtask exit_drill.rs`, `lbx-perf corpus.rs`).

## 7. Effort actuals (spec §8 DoD 7)

Plan: 29 tasks ≈ 27–29 engineer-days (§5 roll-up). Actual: E01 was
implemented in **8 single-session phase commits** (2026-07-04 → -05, one
engineer + agent tooling), roughly one commit per spec phase:

| phase | commit | scope actually landed |
|---|---|---|
| 1 workspace & guardrails | `4ff768f` | plan T1–T4 |
| 2 seam tracer bullet | `fa77162` | T5–T7 (+ `CancelToken` pulled forward) |
| 3 catalog | `f542a52` | T8–T13 |
| 4 jobs & core façade | `fbdd206` | T14–T18 (probe stub tolerated) |
| 5 probe & previews | `fbb8f20` | T19–T21 (rawler replaced in-crate) |
| 6 node & goldens | `b5c88c7` | T22–T24 |
| 7 shell & CLI | `42fb219` | T25–T27 |
| 8 proof & exit | this commit | T28–T29 + DoD walk |

Calibration note for E02–E09 planners: the per-task day estimates held
up as *relative* weights; absolute wall-clock was compressed by the
agent-assisted workflow. Treat the §5 estimates as review-effort units.

## 8. Known limitations (M0, by design)

- Loupe source = camera-embedded JPEG only; 100% zoom is honest about
  that (tier badge). Real decode = E02, pyramid = E03.
- sRGB-only display transform; no monitor ICC (E02.3).
- `probe()` uses in-crate container walkers (not rawler — LGPL); R4
  coverage risk transfers to our walkers, LibRaw sandbox fallback = E02.
- Import = add-in-place only; `volume_uuid` stored NULL (E04).
- Non-UTF-8 paths rejected per-file at import (OQ-3, E04 owns).
- Grid virtualizes textures/decodes over an in-memory summary list
  (~100 B/row); true windowed row fetch is OQ-5/E08 territory.
- Shell smoke + grid-scroll CI legs are `continue-on-error` until
  observed green across the hosted-runner fleet (windowed apps).
- Exit-backup policy default: on close if last backup > 24 h, retain 10
  (OQ-6 — confirm with product before M1).

## 9. Open items handed to owners

| item | owner |
|---|---|
| Witness the exit drill on real Windows + Linux hardware (§1) | maintainer w/ hardware, before M0 sign-off is declared for those platforms |
| Promote shell-smoke + grid-scroll CI legs to blocking once green on the fleet | E01 maintainers |
| `docs/plan/licensing.md` refresh for the Phase-2 deny.toml exceptions | licensing follow-up (recorded in E01-deviations.md) |
| Migration number `0002` | E03 (expected) via registry PR |
| Per-OS perf baselines once nightly history exists | E01 maintainers / E16 |
