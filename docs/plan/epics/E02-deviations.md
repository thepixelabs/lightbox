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

## 2026-07-05 — Phase C: LibRaw out-of-process proxy (staff engineer)

Delivered C1–C7. `lightbox-rawproxy` now: applies a resource sandbox at startup, speaks the
v1 CBOR protocol with an out-of-band payload handoff, and — under the `libraw` feature — links
LibRaw via a C shim and decodes mosaic + interim-AHD-demosaiced. `lightbox-decode` gained the
supervisor client (`ProxySupervisor`/`ProxyClient`), `decode_for_develop`, and the full
`DecodedRawState` (de)serialize. Default + `--features libraw` builds, clippy, fmt, deny all green.

### Deviations

- **LibRaw is feature-gated OFF by default (`lightbox-rawproxy` `libraw` feature).** The default
  `cargo build --workspace` links no LibRaw and the proxy answers decode requests with a structured
  `no_libraw` `Err` (client → `DecodeError::Unimplemented`). This is the spec §0 sanctioned fallback
  ("if brew fails, feature-gate the proxy and keep the default build green"), applied unconditionally
  so CI / the merge machine / the 3-OS matrix stay green without LibRaw. **Consequence:** production
  packaging must build the proxy with `--features libraw` (LibRaw present); the build script emits an
  actionable error if the feature is on but LibRaw is absent. On this build machine both builds are
  proven green (LibRaw 0.22.1 via Homebrew; the FFI links and the handshake reports the real version).
- **Payload handoff is a temp file, not `memfd`/`CreateFileMapping` (§3.2).** `lightbox-decode` denies
  `unsafe` workspace-wide, so it cannot `mmap` a shared segment; a plain temp file (path in
  `ShmRef.name`) is portable across the 3 OSes with the identical hygiene contract (client deletes
  after read; proxy cleans up on error). Proven leak-free over 100 cycles + client always-delete
  (even on cap rejection). Blast radius: one extra file write+read per decode vs. zero-copy mmap;
  E11/E16 may revisit for the zero-copy path.
- **Proxy wire protocol enriched (additive).** `ProxyMeta` gained `payload: ProxyPayloadKind`
  (`None`/`Mosaic(MosaicMeta)`/`Demosaiced(DemosaicedMeta)`) so the client reconstructs
  `MosaicImage`/`SourceImage` from out-of-band `u16` bytes + metadata. `#[derive(Serialize,
  Deserialize)]` added to `CfaColor`/`CfaPattern`/`BlackLevels`/`Rect`/`Illuminant`/`RawColorimetry`/
  `DecodeBackend` (and `PartialEq` to `RawColorimetry`) — additive, no field/shape change to the
  Phase-A cross-phase contract; needed so calibration travels the wire AND the `DecodedRawState`
  CBOR header.
- **`DecodedRawState` v1 adds a `u32 header_len`** between the fixed prefix and the CBOR header so the
  reader finds the zstd frame boundary without a streaming CBOR probe; the rest matches §4.2. `zstd`
  appended to `lightbox-decode`'s `[dependencies]` (Phase A omitted it; already deny-approved via
  `lightbox-catalog`).
- **LibRaw FFI via a hand-written C shim (`src/libraw_shim.c`), not a Rust mirror of `libraw_data_t`.**
  The shim exposes a small flat `lbx_lr_*` C API we control; the Rust `extern "C"` block declares only
  those, so the FFI is stable across LibRaw releases (the giant `libraw_data_t` struct never appears in
  Rust). Compiled by `cc` (appended as a `[build-dependencies]` of `lightbox-rawproxy`; MIT/Apache,
  already in the deny-approved graph via `zstd-sys`/`lcms2-sys`; inert unless the feature is on).
- **rlimits: `RLIMIT_AS` is unsupported on macOS** (setrlimit → EINVAL) → use `RLIMIT_DATA` there,
  `RLIMIT_AS` on Linux; plus `RLIMIT_CPU`/`RLIMIT_CORE`/`RLIMIT_FSIZE` everywhere POSIX.
- **Surface-2 SBOM row for LibRaw is a documented comment in `native-inventory.toml`, not a
  `[[package]]` entry.** The placeholder checker (`cargo xtask lint-native-deps`) only models `-sys`
  CRATES and flags any inventoried name absent from `Cargo.lock` as stale; LibRaw is a dynamically
  linked *library* (no crate), so a real entry would break the check. The comment block records the
  full row (library, version, LGPL-2.1, dynamic/out-of-process, feature, transitive deps, audit test).
  E16 formalizes it when the inventory model grows a "dynamic-external" kind.
- **`raw/proxy_client.rs` (supervisor) is new alongside `raw/proxy.rs` (wire types).** The §2 module
  map named the supervisor `proxy_client.rs`; Phase A had created `proxy.rs` for the protocol types.
  Both coexist (disjoint). `raw/mod.rs` gained `pub mod proxy_client;` + `decode_for_develop_impl`
  (a module file, not a crate `lib.rs` — within Phase C's surface).

### DEFERRED (with reason — nothing faked)

- **C2 full-corpus decode PARITY + X-Trans render:** requires the **A2 CC0 raw corpus**, which is
  deferred (A2 is not in any executed phase) and could not be fetched on this bare machine (the
  `cargo xtask fixtures` download timed out — network-bound). The FFI decode path is proven to
  **compile, link, and run** (the `Hello` handshake reports the real linked LibRaw 0.22.1, proving
  LibRaw is callable). A real end-to-end pixel decode is exercised by the env-gated integration test
  `real_raw_decodes_via_the_proxy_when_a_sample_is_provided` (`LIGHTBOX_TEST_RAW=<raw file>`), which
  **skips honestly** when no sample is supplied. **No corpus, metadata parity, or decode output is
  fabricated.** When A2 lands, that test + a metadata-parity assertion against the in-crate walker
  become PR-gating.
- **C4 Windows `JobObject` sandbox:** the build machine is macOS. POSIX rlimits are implemented; the
  Windows Job Object (parent-applied `JOB_OBJECT_LIMIT_PROCESS_MEMORY`/`JOB_OBJECT_LIMIT_JOB_TIME`) is
  a documented follow-up. In the interim the parent-side payload cap + kill-on-timeout supervisor bound
  the child there. **No fake sandbox claimed.**
- **C4 hard no-network sandbox (seccomp / seatbelt / AppContainer):** scoped to the §12
  security-engineer review (threat-model note, task H7). The proxy opens no sockets; egress is not
  hard-blocked at the kernel level yet. Stated honestly, not oversold.
- **`decode_for_develop` in-crate linear/mono-DNG fast path:** depends on **A5** (not in Phase C's
  task set). In the interim `decode_for_develop` routes every raw through the proxy's AHD path; once A5
  lands, linear-DNG/mono decode in-crate and only CFA-mosaic goes to the proxy.
