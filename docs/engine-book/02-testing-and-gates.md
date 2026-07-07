# Testing and gates

What your node must pass before it ships, and what infrastructure
`lightbox-render-testkit` gives you for free.

## The testkit (`crates/lightbox-render-testkit`)

Dev/test-only, not shipped. It gives you:

- **`compare`** — self-contained f64 Lab / ΔE2000 (validated against the 34
  published Sharma-Wu-Dalal test vectors) and PSNR comparators. This is the
  single ground truth every golden and parity gate in the codebase compares
  against — use it, don't reimplement color-difference math.
- **`corpus`** — `synth_source(CorpusKind, w, h)`: seven synthetic patterns
  (gradient, checker, low-key, high-key, high-frequency, wide-gamut,
  100 MP-synthetic), deterministic and license-clean (own-math, no external
  data — §8 surface-3 applies to test data too). `GoldenCase`/`run_golden`/
  `run_matrix` for the golden-image workflow.
- **`probes`** — `test.gain`, `test.blur_r`, `test.accum`, `test.checker`:
  worked examples of a point-op, a neighborhood op with a real apron, an
  F32-precision accumulator, and a generator. Read these before writing your
  own node of the same shape.
- **`scenario`** — the F1 slider-latency harness and the C10 interactive
  soak. You won't usually call these directly; they exercise the engine as a
  whole, not individual nodes.

## Per-node requirements (what you must ship with a new node)

1. **A golden.** Render your node (or a small graph containing it) against a
   testkit corpus pattern on the **CPU reference path** (§4.4/R3 — goldens
   are rendered from CPU, not GPU, so driver variance never becomes the
   reference), commit the PNG under `<goldens_root>/<node>/pv<N>/<case>.png`,
   and assert `check_golden` passes within the standard tolerance
   (ΔE2000 ≤ 1.0 mean, PSNR ≥ 45 dB — `crate::GOLDEN_TOLERANCE`). Regenerate
   with `LIGHTBOX_BLESS=1 cargo test ...` **locally** — bless mode
   deliberately fails in CI (blessing is a reviewed, local act; a PR can
   never silently rewrite a golden).
2. **CPU/GPU parity.** The same graph, same params, rendered on both
   backends, compared within the same ΔE2000/PSNR tolerance. See
   `crates/lightbox-render/tests/ng_parity.rs` (the E6 gate) and
   `engine_book_walkthrough.rs`'s `walkthrough_node_cpu_gpu_parity_on_real_device`
   for the pattern: build two `Engine`s (`BackendPref::Auto` vs `ForceCpu`)
   over the same registry/template, submit the same request, compare
   readbacks.
3. **Determinism.** Each backend must be **bit-identical** across repeat
   renders of the same request (§4.4). If your algorithm has any
   nondeterminism (parallel-reduction order, uninitialized padding), fix it
   — don't widen the tolerance.
4. **Apron correctness, if you override `plan`.** `tiled render == untiled
   render`, exactly, same backend (spec C3). See
   `crates/lightbox-render-testkit/tests/tiling.rs`'s
   `blur_smooths_without_a_seam_at_the_tile_boundary` for the pattern:
   render your node once over the whole extent and once tiled at 256², and
   diff.
5. **No panics on adversarial params.** If your node has a schema with
   numeric params, a property test (`proptest`) driving edge values (0,
   negative, huge, NaN-adjacent) through your `eval_cpu` is good practice —
   see `crates/lightbox-render/tests/fuzz_compile.rs` for the compiler-level
   version of this discipline (F4).

## What's PR-blocking vs nightly (the pattern to follow)

| Gate | PR-blocking | Nightly-only |
|---|---|---|
| Your node's golden (small case, CPU) | ✅ | — |
| CPU/GPU parity (small case) | ✅ | — |
| Determinism (3 repeats) | ✅ | — |
| Full per-PV × full-corpus golden matrix | fast subset only | full matrix (D3) |
| 10k-iteration interactive soak | reduced count (250 in-gate) | full 10k (`#[ignore]`d test, run explicitly in `nightly.yml`) |
| Compiler fuzz | 2000 bounded proptest cases | `PROPTEST_CASES=200000` override in `nightly.yml` (still not real coverage-guided fuzzing — see `fuzz_compile.rs` module docs) |
| Slider-latency p95 < 100 ms | — (needs the §7 reference GPU runner, not provisioned) | indicative-only numbers published to the nightly job summary |

The rule of thumb: **fast + deterministic + no external hardware ⇒
PR-blocking; anything wall-clock-heavy or hardware-gated ⇒ nightly**, and the
nightly job must still actually run something real (not a `--ignored` filter
that silently matches zero tests — see the C10 note in
`docs/plan/epics/E05-deviations.md` for exactly this mistake caught and fixed
during F5).

## Where to look for a worked example of each gate

Every gate above has a real, currently-green example in the codebase:

- Golden: `crates/lightbox-render-testkit/src/corpus.rs` (`first_single_node_golden_within_tolerance`)
- Parity: `crates/lightbox-render/tests/ng_parity.rs`
- Apron/tiling: `crates/lightbox-render-testkit/tests/tiling.rs`
- Fuzz: `crates/lightbox-render/tests/fuzz_compile.rs`
- Concurrency stress: `crates/lightbox-render/tests/concurrency_stress.rs`
- Full node walkthrough (all of the above, minus the full matrix): `crates/lightbox-render/tests/engine_book_walkthrough.rs`
