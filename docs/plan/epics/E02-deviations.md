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

## 2026-07-05 — Phase A: decode spine + full crate/module scaffold (staff engineer)

Delivered A1, A3, A4, A6, A7, A9 in full; scaffolded `lightbox-color` (all §3 module surface),
`lightbox-rawproxy` (bin), `tools/lightbox-profgen` (bin); wired both new members into the root
manifest; `deny.toml` now bans rawler + dnglab graph-wide. Exit bar green.

### Deviations

- **`RawColorimetry` matrices are `[[f64; 3]; 3]` arrays, not `Mat3` (§3.1).** `lightbox-color`
  depends on `lightbox-decode` for `RawColorimetry`/`CameraId`; storing `lightbox_color::Mat3` in
  `RawColorimetry` would invert that and create a crate **cycle**. `Mat3` implements
  `From<[[f64; 3]; 3]>` (= `lightbox_decode::Mat3Array`) so the B-phase solver reads them with
  zero friction. Blast radius: none (type-internal). Rollback: move `Mat3` to a lower crate.
- **`parse_dcp` lives in `lightbox-color::dcp`, not `lightbox-decode` (§3.1/§2 module map).** Its
  output `CameraProfile` is a `lightbox-color` type; hosting the parser in `lightbox-decode` would
  make decode depend on color (cycle). `.dcp` is still untrusted input — F3 fuzz/caps apply
  wherever it lives. Owner map unchanged (F owns `dcp`, `profile`).
- **`probe()`/`AssetProbe` keep their E01 shape; the §3.1 field enrichment is deferred.** The
  richer §3.1 `AssetProbe` (`kind`, `decode_support`, `camera: CameraId`, `cfa`) was **not**
  retrofitted onto the existing struct, because `lightbox-preview` consumes E01's `AssetProbe` and
  changing it would break that build. A3 is satisfied by **reusing the E01 permissive walkers**
  (no rawler in the graph) and exposing `normalize_camera`/`CameraId` (A4) for callers that need
  the normalized identity. Adding the classification fields (`DecodeSupport`/`AssetKind`/CFA) to
  the probe output is a small later reconciliation (E04 seam), tracked here. Blast radius: E04
  ingest reads classification via a helper rather than a struct field for now.
- **`lcms2` default features** (`dynamic` + `static-fallback` + `parallel`) are used; on this bare
  machine (no system lcms2) it built the **vendored static** lib via `static-fallback`. **No
  `fast_float` feature is enabled** (verified via `cargo tree -e features`) — the GPL-3 plugin is
  absent (R2). D1 owns the CI symbol/grep assert and may pin to force-static so a system lcms2 can
  never shadow the vendored one. `lcms2-sys` is recorded in `native-inventory.toml` (surface-2).
- **Superset dependencies declared now, unused in Phase A** (per the parallel-scaffold mandate, so
  Wave-1..3 phases need not edit manifests): `lcms2` in `lightbox-color`; `libc` in
  `lightbox-rawproxy`; `lightbox-color`/`lightbox-decode`/`anyhow` in `lightbox-profgen`. These
  compile clean (no default unused-dep lint) and are consumed when the owning phase lands.

### DEFERRED (with reason — nothing faked)

- **`decode_raw` body is a scaffold.** Its mosaic path is the Phase-C LibRaw proxy and its
  in-crate linear/mono-DNG path is **A5** (not in this phase's task set). The frozen §3.1 signature
  + panic containment (A9) ship now; the body returns a structured `DecodeError::Unimplemented`
  (never a panic; E04 treats it like any decode error). No decode is faked.
- **A5 — in-crate raw metadata/colorimetry extraction + linear/mono-DNG pixel decode:** not in the
  Phase-A task list (A1/A3/A4/A6/A7/A9). The **types** it populates (`RawColorimetry`,
  `MosaicImage`, `LinearMosaic`, `RawDecode`) are defined in full so B/C build against them;
  populating them from real DNG tags is A5.
- **A2 — test-corpus bootstrap:** the pinned CC0 raw corpus fetch is out of this phase's task set;
  A6/A7 are proven with **synthetic fixtures and hermetic in-memory encode/decode** instead
  (linearize synthetic mosaics; PNG-ICC + 16-bit-TIFF round-trips), so no network corpus is needed
  to gate Phase A.
- **`lightbox-rawproxy` / `lightbox-profgen` bodies are skeletons** (Phase C / Phase G own them):
  the proxy ships a real CBOR frame loop + `Hello`/`Shutdown` handshake and answers decode
  requests with a structured `Err` frame; profgen is a CLI skeleton that names the deferred
  pipeline stages. LibRaw FFI, shm handoff, sandbox (C2–C4) and dcamprof orchestration (G) are not
  implemented — nothing is fabricated.

### Scaffold surface delivered (so Wave-1..3 touch disjoint files)

- `lightbox-decode` (extended): `error` (DecodeError taxonomy + `catalog_code`, A1/A9), `camera`
  (`CameraId` + `normalize_camera` + `camera_aliases.toml`, A4), `raw::types` (all §3.1 shared
  types), `raw::linearize` (A6, tested), `raw::proxy` (v1 protocol types), `raw::state`
  (`DecodedRawState` magic/version + `decode_params_hash`, E03 contract), `image` (A7: jpeg/png/tiff
  → `SourceImage` + orientation + ICC), `panic` (A9 `catch_unwind` boundary). `decode_raw`/
  `decode_image` are panic-guarded at the API boundary.
- `lightbox-color` (new full surface): modules `matrix`(B), `cct`(B), `wb`(B), `lut`(B),
  `transform`(B), `cms`(D), `display`(D), `output`(D), `look`(E), `dcp`(F), `profile`(F), `error`(A).
  Every §3.3–§3.6 public type is a real definition; phase-owned fn bodies are `unimplemented!("<task>")`.
- New members: `crates/lightbox-rawproxy` (bin, C), `tools/lightbox-profgen` (bin, G).

## 2026-07-05 — Phase B: camera-matrix base + WB solver + first failing test (staff engineer)

Delivered B1–B8 in full and the color core of B9. Exit bar green (`cargo build/test/clippy
--all-targets -D warnings/fmt --check/deny check`). No `Cargo.toml` or `lib.rs` edits — all deps
(`twox-hash`, `serde`, `ciborium`) were already declared by Phase A.

### Deviations

- **Edited `profile.rs` to fill the B-tagged bodies (nominally F in the scaffold owner map).** The
  Phase-A owner map lists `profile` under Phase F, but `camera_matrix_base` (B7) and
  `ColorimetricSolver::{white_point, neutral_from_temp_tint, temp_tint_from_neutral, cam_to_xyz_d50}`
  (B3/B4/B5) are **Phase B tasks** and are tagged **Phase B** in their own scaffold doc comments —
  and B7's first-failing test cannot exist without `camera_matrix_base`. B filled **only** those
  B-tagged function bodies + the private solver helpers; **`resolve_profile_ref` (F7) is left
  `unimplemented!()`** and `dcp.rs` untouched, so B and F still touch **disjoint** function bodies.
  Merge note for the integrator: B/F edits to `profile.rs` do not overlap. Blast radius: a textual
  merge on `profile.rs` (function-disjoint). Rollback: none needed.
- **Implemented `Spline1D::eval` (nominally F4) in `matrix.rs`.** `Spline1D` lives in the B-owned
  `matrix` module and B8's `resolve_input_transform` samples profile/look tone curves through it, so
  a stubbed `eval` would panic B8. Implemented as **monotone cubic (Fritsch–Carlson)** — the DNG-SDK
  reference model F4 also calls for. Serves E (look curve) and F (profile tone curve) unchanged.
- **Implemented `spaces::companion_encode` (nominally D2) in `matrix.rs`.** It physically lives in
  the B-owned `matrix` module and is a pure per-channel sRGB OETF over ProPhoto-linear values;
  implementing it here keeps `matrix.rs` self-contained and spares Phase D from having to edit a
  B-owned file (avoids a merge conflict). D2's rustdoc-pinned "encode-only, never a processing space"
  semantics are preserved.
- **WB preset tints are all `0` (sourced CCTs only).** `wb_presets()` commits **published CIE
  standard-illuminant CCTs** (Std A 2856 K, D55 5503 K, D65-class 6504 K, D75 7504 K, F2 4230 K,
  flash≈D55 5503 K) per Open Question 3, with **tint 0** for every row (the locus point at that CCT).
  Fluorescent F2's true off-locus green tint needs its spectral power distribution, which is out of
  Phase B scope; E10 may refine per body. **No tint values invented.**
- **CCT tint of daylight points is ~10, not ~0 — by design.** The `cct` module uses the DNG SDK's
  Robertson 31-point **Planckian** locus (the "DNG-compatible locus"). CIE D-series daylight
  chromaticities sit slightly above the Planckian locus, so a faithful solve yields a small non-zero
  tint (~10 for D50/D65). This matches `dng_temperature` behaviour; documented in the `cct`/`profile`
  tests (thresholds set accordingly). Not a defect.

### DEFERRED (with reason — nothing faked)

- **B9 — full `lightbox-cli render-ref` corpus harness is DEFERRED to merge/Phase H.** The color
  core of the CPU reference render is delivered and tested: `ResolvedInputTransform::eval_cpu`
  (§5.2 stage order) → `render_reference_srgb8` / `working_linear_to_srgb8` (working→sRGB with
  Bradford D50→D65 ⊕ sRGB OETF). The end-to-end CLI subcommand (decode → linearize → interim
  demosaic → this → PNG contact sheet, full corpus, `--bless` goldens) needs the **Phase C LibRaw
  proxy** and the **A2 CC0 corpus**, neither of which is on this branch, and lives in the
  E01/other-phase-owned `lightbox-cli/main.rs` (outside B's five modules). Handed to the integrator/H.
- **B7 golden uses a synthetic self-consistent decoded-raw fixture, not a real corpus raw + PNG
  golden.** Per the phase brief ("use a committed decoded-raw fixture or synthetic mosaic if the C
  proxy isn't on your branch"), `renders_correct_color_with_zero_bundled_profiles` renders an
  8-patch neutral→saturated set through a synthetic camera whose native space is linear sRGB (exact
  ColorMatrix/ForwardMatrix), gated at **ΔE2000 ≤ 1.0**. Because the camera is colorimetrically
  self-consistent, any error in the matrix base, WB solve, working space, or Bradford adaptation
  breaks the round-trip and fails the gate — it is a genuine end-to-end colorimetric proof with
  **zero bundled profile assets** (`camera_matrix_base` reads only in-memory `RawColorimetry`). The
  **real-corpus goldens + `--bless` workflow (§7.2) are deferred** to merge/H once the C proxy +
  corpus land. No reference renders fabricated.

### What Phase B delivered (files owned: `matrix`, `cct`, `wb`, `lut`, `transform`; + B-tagged
### bodies in `profile`)

- `matrix` (B1): `Mat3` inverse (singular-rejecting), Bradford CAT, xy⇄XYZ, primaries→matrix
  derivation, ProPhoto-linear-D50 working space + sRGB constants (matched to Lindbloom refs 1e-4),
  `Spline1D` monotone-cubic eval, `spaces::companion_encode`.
- `cct` (B2): DNG-compatible Robertson locus `xy⇄(CCT,tint)` (A/D50/D65 within ±15 K, round-trip).
- `wb` (B5): `WbMode`/`WhitePoint`/`WbPreset` vocab + sourced preset table.
- `lut` (B6): trilinear hue-wrapped HueSat evaluator, Linear/sRGB encodings, val-dims==1 fast path.
- `profile` (B3/B4/B7): `camera_matrix_base`, `ColorimetricSolver` (dual-illuminant interpolation,
  white-point iteration, `cam_to_xyz_d50` ForwardMatrix + inverse-CM/Bradford paths).
- `transform` (B8/B9-core): `resolve_input_transform` + `eval_cpu` (§5.2) + xxh3-64 content key +
  the working→sRGB reference-render helpers.
- 33 unit/property tests (incl. the first-failing test + a `proptest` WB round-trip) all green.
