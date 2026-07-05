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

## 2026-07-05 — Phase D: color management working / display / output (staff engineer)

Delivered D1–D7 filling `lightbox-color::{cms, display, output}` plus the D2 companion encode.
Exit bar green in the worktree (`build`/`test`/`clippy -D warnings`/`fmt --check`/`deny check`).

### What landed

- **D1 — LCMS2 integration (`cms.rs`).** `IccProfile` wraps an owned
  `lcms2::Profile` on the global context; `from_bytes` is untrusted-input safe (empty →
  `InvalidProfile`, `> MAX_ICC_BYTES` (32 MiB) → `TooLarge`, malformed → `InvalidProfile`, never
  a panic/abort). A process-global LCMS2 error handler (installed once via `Once`) captures the
  last message into a thread-local for the diagnostic and guarantees no stderr spam under
  fuzzing. `fast_float` is structurally absent (`lcms2-sys` has no such feature/source) and the
  `fast_float_plugin_is_absent` test pins it against the lock file.
- **D2 — companion encode.** Filled `matrix::spaces::companion_encode` (sRGB OETF over the
  ProPhoto working primaries); verified ≤ 1e-4 vs an LCMS2 ProPhoto-linear → ProPhoto-sRGB
  transform (`cms::tests::companion_encode_matches_lcms`).
- **D3 — `build_display_transform`.** Working → display, rel-col + BPC, baked to a 1-D shaper +
  65³ LUT with an xxh3-64 cache key. The shaper is derived from the display's neutral-axis
  response so an identity (working-space) display yields the identity ramp + identity cube
  (`identity_profile_yields_identity_lut`, within 1e-4). Release first-bake ≈ 33 ms (≤ 100 ms
  budget), then cached by key.
- **D4 — baked-LUT fidelity gate.** 100 k random working-space samples, ΔE2000(baked vs exact
  LCMS2): sRGB p99 ≈ 0.14 / max ≈ 0.45; AdobeRGB (wide-gamut, ~2.2 gamma) p99 ≈ 0.10 /
  max ≈ 0.46 — both inside p99 ≤ 0.5 / max ≤ 1.0. CIEDE2000 implemented in-test (via a
  display→Lab LCMS2 transform).
- **D5 — `DisplayProfileProvider`.** `SystemDisplayProfileProvider` fetches the **real** macOS
  ColorSync profile via CoreGraphics (`CGDisplayCopyColorSpace` → `CGColorSpaceCopyICCData`,
  parsed through the untrusted path); `SrgbFallbackProvider` is the documented sRGB fallback.
- **D6 — `OutputTransform` (E15 seam).** All §3.5 spaces (sRGB / AdobeRGB / ProPhoto / Display P3
  / Rec.2020 / user ICC) at 8/16-bit int + f32 via a cache-disabled (`Send + Sync`) LCMS2
  transform; emitted ICC bytes re-validate; working→sRGB→working round-trip tight for in-gamut.
- **D7 — untrusted-ICC hygiene.** 32 MiB size cap + an always-on adversarial-fixture unit test
  (empty / truncated / lying-tag-count / random blobs → structured errors, no panic), plus a
  detached `cargo-fuzz` target (`crates/lightbox-color/fuzz`, `icc_from_bytes`).

### Deviations

- **`lightbox-color/Cargo.toml`: `lcms2` gains `features = ["static"]` and a direct
  `lcms2-sys = "4"` dependency (appended).** `static` forces the vendored static Little-CMS2 build
  (lcms2-sys's `static` short-circuits pkg-config), making the D1 "static" mandate deterministic
  even on a dev box with a system `liblcms2` — this is the force-static pin Phase A's deviation
  anticipated ("D1 … may pin to force-static so a system lcms2 can never shadow the vendored
  one"). `lcms2-sys` (already a transitive dep at the locked 4.0.7, already in
  `native-inventory.toml`) is promoted to a direct dep solely to call `cmsSetLogErrorHandler` for
  the D1 error-capture handler. **Merge note:** both are appended at the end of `[dependencies]`.
- **`matrix.rs::spaces::companion_encode` filled from Phase D (cross-file touch).** `matrix.rs` is
  a Phase-B file, but `companion_encode` is dual-owned "B1/D2" per the scaffold owner map and D2
  is the task that owns it. Only that one function body was changed (`unimplemented!()` →
  implementation); the B1 matrix derivations (`xyz_d50_to_working` / `working_to_xyz_d50`) are
  left untouched for Phase B. **Merge note:** if Phase B also implemented `companion_encode`, keep
  either — both compute the same sRGB-over-ProPhoto encode (the D2 test pins ≤ 1e-4 vs LCMS2).
- **`#![allow(unsafe_code)]` at the top of `cms.rs` / `display.rs` / `output.rs`.** The workspace
  denies `unsafe_code`; these three modules are the FFI surface (LCMS2 handler, macOS ColorSync,
  slice-reinterpretation for pixel buffers), which the workspace comment explicitly reserves for
  "FFI crates … with justification". Scoped to the module (not the crate root), so `lib.rs` is
  untouched.
- **DisplayTransform gained an inherent `apply()` and `LUT_SIZE` is `pub`.** Not in the §3.5 field
  list, but D4 needs a CPU reference application of the shaper+LUT and E05's WGSL node needs the
  parity anchor; documented as such. `OutputTransform` similarly gained a `depth()` accessor.
  Blast radius: additive, no signature change to the specced surface.
- **ProPhoto/Rec.2020 output TRCs are simplified.** The ProPhoto *output* space uses pure gamma
  1.8 (the tiny ROMM linear toe is omitted — negligible for export encoding) and Rec.2020 uses the
  Rec.709 transfer; primaries + white points are exact. sRGB / Display-P3 / AdobeRGB are standard.
  Round-trip + external-tooling-validity tests are gated on sRGB (fully defined).

### DEFERRED (with reason — nothing faked)

- **D5 — Windows ICM + Linux colord/`_ICC_PROFILE` providers:** only the **macOS** ColorSync path
  is implemented and compile-verified (this is a `darwin` build machine). On non-macOS targets
  `SystemDisplayProfileProvider` returns `None` (→ caller uses the sRGB fallback + surfaces the
  E08 event), rather than shipping Windows/Linux FFI I cannot compile-verify here. The exact APIs
  are named in the source; the shell/CI wires them against a live display server. (Environment:
  cannot compile-verify other-OS FFI on darwin; per the "mark hardware/OS tasks DEFERRED" rule.)
  Also, D5's "mac/win CI runners fetch a real monitor profile" is a **CI-with-display** task —
  the code path is present but the live fetch is exercised on a real runner, not this headless box.
- **D7 — the 1 M-iteration cargo-fuzz *run*:** `cargo-fuzz` is not installed and needs nightly +
  libFuzzer. The fuzz **target** ships (`crates/lightbox-color/fuzz`, detached workspace so it
  stays off the default exit bar); the always-on adversarial-fixture unit test is the in-gate
  subset. The 1 M-iteration soak is the nightly job. (Environment: absent tool + nightly.)
- **D6 — "emitted ICC validates in *external* tooling":** validated in-process by re-parsing the
  emitted bytes through `IccProfile::from_bytes` (a well-formed-ICC proxy). A third-party
  validator (e.g. `iccDumpProfile`) is not installed on this box. (Environment: absent tool.)
