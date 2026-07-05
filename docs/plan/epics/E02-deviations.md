# E02 — Deviations & Deferred Tasks

Append-only log of spec deviations and DEFERRED tasks against `E02-decode-color-foundation.md`.
Each entry records: what changed, why, and the blast radius / rollback.

## 2026-07-05 — Phase 0: spec reconciliation + parallel disposition (system-architect)

### Deviations

- **Decode spine flipped: LibRaw-proxy-primary, rawler crate-BANNED.** §5.1 and Phase A task
  A5 originally specced **rawler (LGPL-2.1)** as the in-process **primary** mosaic decoder with
  LibRaw as a fallback. rawler is LGPL-2.1 and is **BANNED from the crate graph** (`deny.toml`).
  Rewrote so that: raw **CFA-mosaic** decode (Bayer / X-Trans) = the **out-of-process LibRaw
  proxy** (`lightbox-rawproxy`, Phase C) as the **PRIMARY and only** mosaic path; in-crate
  `lightbox-decode` reuses **E01's permissive TIFF-IFD / EXIF walkers** for `probe()` +
  `RawColorimetry` extraction and decodes **linear-DNG / monochrome-DNG in-crate**; **rawler is
  never a crate dependency**. §5.2 input-transform stage order left **unchanged** as required.
  - Touched: §1.1 (E02.1), §2 module map (`raw/metadata.rs` replaces `raw/rawler_path.rs`),
    §3.1 (`DecodeSupport`, `BackendPolicy`, `decode_raw` doc, `RawDecode::Mosaic`,
    `RawColorimetry` source), §4.1 (`decode_backend` values), §5.1, Phase A (header, A3, A5),
    Phase C (C2, C3, C5, C7), §6 parallelization note, §7.5 perf row, §7.6 Surface-1 gate,
    R1, R7, F1, H7, §1.2 non-goal (dnglab-as-subprocess).
  - **Consequence surrendered:** the dual-backend redundancy for mosaic decode is gone. A proxy
    crash / timeout on a mosaic file yields `asset.decode_error` with **no in-process second
    try** (single mosaic backend by license necessity — recorded in R1 and §5.1). Accepted:
    blast radius is a per-file decode failure; the asset stays catalogued.
  - **Rollback:** none — this is the license-correct baseline. Reverting would reintroduce a
    banned crate.
- **profgen fixtures use dnglab as a subprocess, not a crate.** §1.2 non-goal amended: dnglab
  (LGPL-2.1) is **exec'd as a subprocess (never linked)** for DNG fixtures — the same isolation
  pattern as the dcamprof GPL subprocess. rawler/dnglab stay out of the crate graph.

### DEFERRED (with reason — nothing faked)

- **A8 — HEIC decode (libheif):** libheif absent on the build machine. HEIC lands behind an
  **off-by-default feature**; the default build stays green. (Environment: absent tool.)
- **F6 — dcamprof reference harness:** dcamprof absent. §5.2 stage order is pinned in the
  interim by **hand-derived DNG-SDK-model fixtures**; the dcamprof binding arbiter is wired when
  the tool is available. **No reference renders fabricated.** (Environment: absent tool.)
- **G2 + G6 — target-shot capture protocol content + first curated profile batch:** require
  **physical** ColorChecker / IT8 capture sessions plus dcamprof. **No profiles or reference
  renders faked.** The G1/G3/G4/G5/G7 tooling (profgen skeleton, validation harness, packaging,
  auto-select policy, runbook) is buildable now. (Environment: hardware + absent tool.)
- **E5 — ≥2-human perceptual-review sign-off:** no human reviewers available. The author
  **self-reviews** against the structured checklist and records the no-Adobe-data affidavit; the
  **≥2-reviewer perceptual gate + sign-off record is deferred**. (Human task.)
- **Ship ZERO bundled profiles:** `assets/color/profiles/` is empty at ship. Tier-1 matrix base
  + tier-2 Lightbox look render the corpus correctly without them — **B7**
  (`renders_correct_color_with_zero_bundled_profiles`) is the executable proof.

### Environment notes (build machine)

- **LibRaw:** install via `brew install libraw`; if it fails, **feature-gate the proxy** and keep
  the default build green.
- **LCMS2:** the `lcms2` crate vendors little-cms2 and builds **static**; the **fast_float GPL
  plugin is asserted absent** (D1).
- **Exit bar (all green):** `cargo build --workspace` · `cargo test --workspace` ·
  `cargo clippy --workspace --all-targets -- -D warnings` · `cargo fmt --all --check` ·
  `cargo deny check`.
