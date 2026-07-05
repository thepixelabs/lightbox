# E02 — Decode & Color Foundation

| | |
|---|---|
| **Epic id** | E02 `decode-color-foundation` |
| **Milestone** | M1 (code complete; the E02.5 curated-profile content batch surfaces in the product at M2 per §9) |
| **Effort** | XL ~9–14 pw (per §10 / §10.1 — includes the previously-unbudgeted content-production line E02.5) |
| **Depends on** | E01 (workspace, catalog + migrations runner, `lightbox-cli` skeleton, CI base incl. cargo-deny + SBOM scaffolding) |
| **Architecture refs** | §1.6 (library selections), **§1.7 (three-tier camera color — the governing decision)**, §2.2 (`lightbox-decode`, `lightbox-color`), §4.1 (pipeline stages), §8 (test strategy, three license surfaces), §10.1 (E02.1–E02.5 phase decomposition), Risk 10 |
| **Research refs** | `docs/research/05-…` (raw pipeline internals & color mgmt), `docs/research/02-…` (profiles/WB), `docs/research/10-…` (build-vs-buy rows: rawler/LibRaw/LCMS2/dcamprof) |
| **Status** | Spec — ready for implementation |

**One-paragraph summary.** E02 gives Lightbox its eyes: every supported file decodes to normalized linear data with correct colorimetric metadata, and every camera renders **correct color with zero bundled profile assets** via the license-clean camera-matrix base (§1.7 tier 1), shaped by a **Lightbox-authored original default look** (tier 2), optionally upgraded by **curated in-house DCP camera-matching profiles** (tier 3, an ongoing content line). It also owns ICC color management end to end: the fixed ProPhoto-linear working space, per-monitor display transforms, and the output-transform primitives export will consume. E02 produces **math, data, specs, and CPU reference evaluators**; it does not build render nodes, WGSL kernels, or UI — those are E05/E10 consumers of the transform specs this epic defines.

**First failing test (per §10.1 E02.1):** `renders_correct_color_with_zero_bundled_profiles` — a known corpus raw is decoded, white-balanced as-shot, and rendered through `camera_matrix_base()` → working space → sRGB by the CPU reference pipeline **with the bundled-assets directory absent**, and the result matches a committed golden within ΔE2000 ≤ 1.0. This test is written before the matrix-base implementation and fails first.

---

## 0. Execution disposition (2026-07-05)

Reconciles the §6 phase order with parallel execution and with the build machine's license + tooling reality. The §6 task table stays authoritative for acceptance criteria; this section governs *sequencing* and *what ships now vs. later*.

### Parallel wave plan

Phases do not map 1:1 to the ideal parallel schedule; run them as waves.

- **Wave 0 — A serial.** Scaffold every crate and module first: `lightbox-decode`, `lightbox-color`, the `lightbox-rawproxy` skeleton, the `tools/lightbox-profgen` skeleton, all module stubs + error taxonomies + workspace wiring. Nothing else starts until the scaffolds compile green (exit bar: `cargo build --workspace` + `cargo deny check`).
- **Wave 1 — {C, B, D} in parallel.** C (LibRaw proxy + interim demosaic — the **primary mosaic path**), B (matrix-base + WB solver, anchored on in-crate-decodable linear-DNG + synthetic mosaics so it does not block on C), D (color management: LCMS2 / display / output). Full-corpus **mosaic** render-ref (B9) integrates once C2 lands.
- **Wave 2 — {E, F} in parallel.** E (Lightbox default look family), F (DCP parser + evaluator). F5/F6 depend on B6 (HueSat LUT evaluator).
- **Wave 3 — {G, H} in parallel.** G (curated content line — **tooling only** now, see DEFERRED), H (integration, hardening, seams).
- **Wave 4 — verify.** Full exit bar on the 3-OS matrix: `cargo build --workspace` · `cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo fmt --all --check` · `cargo deny check`; golden gates green.

Hard serializations preserved: A→{B, C, D}; B6→{E2, F5}; **C2→B9** (mosaic render-ref); F→G.

### BUILDABLE-NOW vs DEFERRED

| Task(s) | Status | Reason |
|---|---|---|
| A1–A7, A9 | BUILDABLE | in-crate permissive metadata/colorimetry, non-raw codecs, linear/mono-DNG decode, panic containment |
| **A8** (HEIC via libheif) | **DEFERRED** | libheif absent on the build machine — HEIC lands behind an **off-by-default** feature; default build stays green |
| B1–B9 | BUILDABLE | pure math + CPU reference evaluators; **ship ZERO bundled profiles** — B7 matrix-base renders the corpus correctly without them (tier-1 proof) |
| C1–C7 | BUILDABLE | LibRaw via `brew install libraw`; if brew fails, **feature-gate the proxy** and keep the default build green |
| D1–D7 | BUILDABLE | `lcms2` crate vendors little-cms2 (static); **fast_float GPL plugin asserted absent** (D1) |
| E1–E4, E6, E7 | BUILDABLE | `.lblook` format, authoring harness, look goldens |
| **E5** (≥2-human-reviewer perceptual sign-off) | **DEFERRED** (partial) | no human reviewers available — author **self-reviews** against the structured checklist and records the no-Adobe-data affidavit; the ≥2-reviewer perceptual gate + sign-off record is deferred |
| F1–F5, F7 | BUILDABLE | DCP parser, evaluator, fuzz, resolve/fallback |
| **F6** (dcamprof reference harness) | **DEFERRED** | dcamprof absent — §5.2 stage order pinned in the interim by hand-derived DNG-SDK-model fixtures; the dcamprof binding arbiter is wired when the tool is available. **No reference renders fabricated.** |
| G1, G3, G4, G5, G7 | BUILDABLE | profgen skeleton, validation harness, packaging/provenance, auto-select policy, content-line runbook — all buildable without physical shots |
| **G2, G6** (target-shot protocol content + first curated batch) | **DEFERRED** | require physical ColorChecker/IT8 capture sessions + dcamprof. **No profiles or reference renders faked.** |
| H1–H7 | BUILDABLE | migration, CLI, benches, ASan/LSan, golden gate, seam docs, threat model |

**Ship ZERO bundled profiles.** `assets/color/profiles/` is empty at ship. Tier-1 (matrix base) + tier-2 (Lightbox look) render every corpus body correctly with that tree absent — **B7** (`renders_correct_color_with_zero_bundled_profiles`) is the executable proof. Tier-3 curated DCP content (G2/G6) is deferred to physical capture sessions; nothing is fabricated to fill the gap.

---

## 1. Scope

### 1.1 In scope (mapped to the §10.1 phase decomposition)

**E02.1 — Decode + camera-matrix base color**
- `probe()` for all supported formats: format identification, dimensions, normalized camera make/model, capture metadata, embedded-preview descriptor, CFA layout, decode-support classification (in-crate-linear / libraw-proxy-mosaic / non-raw / unsupported).
- Raw **MOSAIC** decode, primary path: **out-of-process LibRaw proxy** (`lightbox-rawproxy`, §5.1 + Phase C) → `MosaicImage` (mosaic data + black/white levels + linearization table + CFA + as-shot WB + `ColorMatrix1/2` / `ForwardMatrix1/2` / calibration illuminants). LibRaw (LGPL-2.1) is **dynamic-linked and isolated in the sandbox subprocess** — it never links into the app. The proxy also produces the **interim demosaic floor** (LibRaw AHD, §1.6) that M1 develop renders ride on until E11.1's clean-room demosaic replaces it at M2. **rawler (LGPL-2.1) is BANNED from the crate graph (deny.toml) — never a crate dependency.**
- Raw **metadata + colorimetry** and linear/monochrome decode, in-crate: **E01's permissive TIFF-IFD / EXIF walkers** (memory-safe Rust, MIT-family) extract `RawColorimetry` (levels, CFA, matrices, illuminants, as-shot neutral) for `probe()` and the tier-1 base, and decode linear-DNG / monochrome-DNG directly (no mosaic demosaic needed). Only the CFA-mosaic pixel decode is delegated to the proxy.
- Linearization (CPU reference implementation + kernel spec for E05): per-channel black subtract, linearization-table application, white-level normalize to `[0,1]` f32, active-area crop, monochrome and linear-DNG passthrough.
- Non-raw decode into the same pipeline: JPEG (zune-jpeg), PNG/TIFF (`png`/`tiff` crates), HEIC (libheif+libde265, dyn-link) → display-referred `LinearImage` with extracted ICC, converted to working space by `lightbox-color`.
- The **license-clean colorimetric base** (§1.7 tier 1): dual-illuminant matrix interpolation, white-point iteration, ForwardMatrix/ColorMatrix paths, Bradford adaptation to D50, camera→ProPhoto-linear.
- The **white-balance model**: as-shot neutral ↔ (Kelvin, tint) solver + WB preset table (consumed by E10's WB node and eyedropper).
- Decode failure containment: no panic crosses the API boundary; failures land as `asset.decode_error` taxonomy codes; a proxy crash never touches the parent.

**E02.2 — DCP parser + evaluator (format engine — own code, §1.6)**
- Own TIFF-IFD parser for `.dcp` containers (all DNG-spec camera-profile tags: matrices, illuminants, `ProfileHueSatMapDims/Data1/Data2` + encoding, `ProfileLookTableDims/Data` + encoding, `ProfileToneCurve`, `BaselineExposureOffset`, `DefaultBlackRender`, embed policy, copyright).
- Evaluator: dual-illuminant interpolation ⊕ HueSatMap ⊕ LookTable ⊕ profile tone curve, per the DNG SDK reference model, validated against **dcamprof-rendered references** (the binding arbiter for stage order/encodings — §7.2).
- Parser hardening: `.dcp` is **untrusted user input** (user-installable profiles) — fuzzed, capped, panic-free.

**E02.3 — Color management**
- LCMS2 (MIT core **only** — the `fast_float` plugin is GPL-3 and **forbidden**, see Risks) via the `lcms2` crate, statically linked.
- Fixed working space: **ProPhoto/ROMM primaries, linear, D50** (§4.1), plus the "Melissa-style" companion encoding (ProPhoto primaries + sRGB curve) for histogram/readout consumers (E10).
- Display transforms: working → per-monitor ICC, baked to shaper + 3D LUT (`DisplayTransform`) for GPU application, exact-LCMS2 path retained for verification; `DisplayProfileProvider` trait + macOS ColorSync / Windows ICM implementations (Linux best-effort behind a feature flag).
- Output transforms: working → sRGB / AdobeRGB / ProPhoto / Display P3 / Rec.2020 / user ICC at 8/16-bit int + f32, with embeddable profile bytes — the primitive E15 consumes.

**E02.4 — Lightbox default look family (authoring + code)**
- `.lblook` file format (versioned CBOR): tone curve + optional hue/sat shaping + provenance metadata; loader, validator, evaluator with 0–200 % amount semantics.
- **Authoring of "Lightbox Color v1"** (the Adobe-Color-*analogue in role*, original in content — §1.7 tier 2) and "Lightbox Neutral" (identity), iterated against a neutral scene corpus with a structured perceptual review gate.
- Surface-3 data-manifest entries (provenance: Lightbox-authored, project license) and the manifest policy-checker wiring for color assets.

**E02.5 — Curated camera-matching profile pipeline (CONTENT LINE — ongoing)**
- `tools/lightbox-profgen`: internal CLI orchestrating **dcamprof as a GPL-3 subprocess (never linked, never shipped)** from our own ColorChecker/IT8 target-shot sessions → validated `.dcp` output under the project license.
- Target-shot capture protocol document + session metadata schema; profile validation harness (parse with our engine, patch-render ΔE gates vs dcamprof reference and measured chart values).
- Packaging + provenance: bundled profiles land in `assets/color/profiles/`, each with a surface-3 manifest entry; catalog registration and auto-selection policy (curated DCP if present for the body, else matrix base + default look, gap flagged to the user per Risk 10).
- **First content batch** for the initial curated bodies (see Open Questions for the N and owner) — explicitly a scale-with-investment line whose per-body cost is documented in a runbook, not a bounded code task.

**Cross-cutting**
- Catalog migration: `camera_profile` registry table + `asset.decode_backend` column.
- `DecodedRawState` serialization format + params-hash (the contract E03's raw cache stores).
- `lightbox-cli` subcommands (`probe`, `decode`, `render-ref`, `profile`, `look`) for headless testing and the golden-image harness.
- CI: golden gates for PV1 color, fuzz targets, license surfaces 2 & 3 entries for everything this epic links or bundles.

### 1.2 Explicit non-goals (named seams, not designs)

| Not in E02 | Where it lives |
|---|---|
| Render nodes, WGSL kernels, DAG/cache, tiles, ROI | **E05** consumes `ResolvedInputTransform`/`DisplayTransform` as node params; E02 ships CPU reference evaluators + kernel-porting notes only |
| Clean-room RCD/AMaZE demosaic; X-Trans Markesteijn; highlight reconstruction | **E11.1** (E02 ships only the LibRaw interim demosaic floor and the seam it plugs into) |
| WB UI, eyedropper widget, profile browser, histogram, tone/color sliders | **E10** (consumes the WB solver, companion-space encode, and profile registry) |
| Raw-cache storage, eviction, relocation | **E03** (E02 defines the `DecodedRawState` byte format + cache key inputs) |
| Import pipeline, checksum copy, preview builds | **E04/E03** (consume `probe()` and decode) |
| Export encoders, sizing, watermark, soft proofing | **E15** (consumes `OutputTransform`); soft-proof transform primitive is a Should, deferred with LCMS2 headroom noted |
| Recipe schema, history, XMP mapping, `crs:` import | **E09** (E02 defines `ProfileRef` resolution semantics only) |
| DNG *writing*/conversion | E04/E16 Should-tier; profgen uses **dnglab as a subprocess** (never linked — rawler/dnglab are LGPL-2.1 and out of the crate graph) for fixtures only |
| Creative LUT profiles (.cube/HaldCLUT), LUT amount UI | **E10.4** |
| PSD decode | Not in §1.6's codec table — v1.x candidate, out of E02 |
| Video probe/decode | FFmpeg territory, out of E02 (E04 seam) |
| HDR editing / gain-map output / float-DNG merge | v1.x (§10.1 deferred list) |
| OS-level sandbox tightening (seatbelt/AppContainer profiles) beyond process isolation + rlimits | security-engineer design review (§12); E02 ships process isolation, resource caps, and the threat-model notes it needs |

---

## 2. Crates & modules touched (per §2 decomposition)

```
crates/
  lightbox-decode/            NEW — owns E02.1/E02.2 parse side
    src/probe.rs              format id, metadata, normalized make/model, embedded-preview descriptor
    src/raw/metadata.rs       in-crate permissive TIFF-IFD/EXIF walker (reuses E01) → RawColorimetry;
                              linear-DNG + monochrome-DNG in-crate decode. NO rawler dependency.
    src/raw/linearize.rs      CPU reference linearization + kernel spec doc-comments
    src/raw/proxy_client.rs   lightbox-rawproxy supervisor client (spawn/pool/timeout/restart) —
                              PRIMARY CFA-mosaic decode path (LibRaw, out-of-process)
    src/raw/state.rs          DecodedRawState (de)serialization + params hash  [E03 contract]
    src/image/                zune-jpeg / png / tiff / libheif → LinearImage (+ICC bytes)
    src/dcp/parser.rs         TIFF-IFD reader for .dcp  (untrusted input; fuzzed)
    src/dcp/types.rs          CameraProfile, DualIlluminant, HueSatLut, …
    src/error.rs              DecodeError taxonomy ↔ asset.decode_error codes
  lightbox-color/             NEW — owns E02.1 matrix math, E02.2 eval, E02.3, E02.4 eval
    src/math.rs               Mat3/Vec3 f64, Bradford CAT, xy↔XYZ, primaries→matrix derivation
    src/spaces.rs             working space (ProPhoto-linear D50), companion encode, std spaces
    src/cct.rs                planckian/daylight locus, xy↔(CCT,tint)
    src/wb.rs                 WhiteBalance model, neutral↔temp/tint solver, presets
    src/profile.rs            camera_matrix_base(), dual-illuminant interpolation, cam→XYZ(D50)
    src/dcp_eval.rs           HueSatMap/LookTable/tone-curve evaluation stages
    src/look.rs               .lblook load/validate/evaluate, amount semantics
    src/resolve.rs            resolve_input_transform() → ResolvedInputTransform (+cache key)
    src/icc.rs                LCMS2 wrapper (contexts, error capture, untrusted-profile hygiene)
    src/display.rs            DisplayTransform bake (shaper + 65³ LUT), DisplayProfileProvider
    src/output.rs             OutputTransform (exact LCMS2 path)  [E15 contract]
  lightbox-rawproxy/          NEW — bin. LibRaw (LGPL, dyn-link) decode sandbox. CBOR-over-stdio
                              protocol + shm payload handoff. Never links into the app.
tools/
  lightbox-profgen/           NEW — internal CLI (not shipped). dcamprof GPL subprocess
                              orchestration, validation harness, manifest emit.
assets/
  color/looks/                lightbox-color-v1.lblook, lightbox-neutral.lblook
  color/profiles/<make>/<model>.dcp        curated in-house profiles (E02.5)
  MANIFEST.toml               surface-3 data-provenance manifest (E02 establishes color entries)
crates/lightbox-catalog/      TOUCHED — one migration (camera_profile table, asset.decode_backend)
crates/lightbox-cli/          TOUCHED — probe/decode/render-ref/profile/look subcommands
```

`lightbox-decode` has **no GPU code and no wgpu dependency** (§2.2). `lightbox-color` is pure math + LCMS2 FFI; its outputs are plain-data transform specs any backend can consume.

---

## 3. Interface definitions

Contracts, not implementations; field lists are normative, exact derive/attr choices are not.

### 3.1 `lightbox-decode` — probe & decode

```rust
/// Format + metadata probe. Cheap (metadata-only, no pixel decode). Used by E04 ingest.
pub fn probe(path: &Path) -> Result<AssetProbe, DecodeError>;

pub struct AssetProbe {
    pub format: FileFormat,                  // Cr2|Cr3|Nef|Arw|Raf|Orf|Rw2|Pef|Dng|Jpeg|Tiff|Png|Heic|…
    pub kind: AssetKind,                     // RawMosaic | RawLinear | RawMono | Image
    pub decode_support: DecodeSupport,       // InCrateLinear | LibrawProxyMosaic | NonRaw | Unsupported
    pub width: u32, pub height: u32,
    pub camera: CameraId,                    // normalized make/model + raw strings (see 3.6)
    pub capture_time: Option<OffsetDateTime>,
    pub embedded_previews: Vec<PreviewDescriptor>,   // offset/len/dims — E03 consumes
    pub cfa: Option<CfaPattern>,             // RGGB variants, X-Trans, mono
    pub orientation: Orientation,
}

pub struct DecodeOpts {
    pub backend: BackendPolicy,              // Auto (mosaic→proxy, linear/mono→in-crate) | ForceProxy | ForceInCrate
    pub interim_demosaic: bool,              // true ⇒ proxy returns demosaiced camera-RGB (M1 path)
    pub timeout: Duration,                   // proxy budget; default 30 s
}

/// Raw decode. CFA-mosaic → LibRaw proxy (primary); linear/mono → in-crate. Never panics.
pub fn decode_raw(path: &Path, opts: &DecodeOpts) -> Result<RawDecode, DecodeError>;

pub enum RawDecode {
    Mosaic(MosaicImage),                     // LibRaw proxy mosaic mode (primary CFA path)
    /// Interim M1 develop path: LibRaw AHD output, linear camera-native RGB,
    /// no WB / no output color / no gamma applied. Replaced by E11.1 at M2.
    DemosaicedInterim(SourceImage),
    Linear(SourceImage),                     // linear DNG / monochrome
}

pub struct MosaicImage {
    pub data: MosaicBuffer,                  // u16 plane (packed formats unpacked by decode)
    pub width: u32, pub height: u32,
    pub cfa: CfaPattern,
    pub active_area: Rect, pub default_crop: Rect,
    pub black_levels: BlackLevels,           // per-CFA-position, per-area where the format has it
    pub white_levels: [u32; 4],
    pub linearization: Option<Vec<u16>>,     // DNG LinearizationTable semantics
    pub colorimetry: RawColorimetry,
    pub orientation: Orientation,
}

/// Everything the §1.7 tier-1 matrix base needs — factual calibration data from the file (in-crate
/// permissive walker, DNG tags) or the LibRaw proxy's calibration tables (proprietary mosaics).
pub struct RawColorimetry {
    pub as_shot_neutral: Option<[f64; 3]>,   // or derived from cam_mul
    pub illuminant1: Illuminant, pub illuminant2: Option<Illuminant>,
    pub color_matrix1: Mat3, pub color_matrix2: Option<Mat3>,
    pub forward_matrix1: Option<Mat3>, pub forward_matrix2: Option<Mat3>,
    pub analog_balance: Option<[f64; 3]>,
    pub baseline_exposure: f32,
}

/// Linearize per §4.1 stage 2 — CPU reference impl; E05 ports the documented kernel spec to WGSL.
pub fn linearize(m: &MosaicImage) -> LinearMosaic;    // f32 [0,1], black-subtracted, active-area cropped

/// One develop-ready linear image in *camera-native* RGB (pre input-transform) or display-referred
/// source color (non-raw). This is what the E05 source node + E03 raw cache traffic in.
pub struct SourceImage {
    pub data: Vec<f32>,                      // interleaved RGB(A), linear
    pub width: u32, pub height: u32,
    pub color: SourceColor,                  // CameraNative(RawColorimetry) | Tagged(IccBytes) | AssumedSrgb
    pub provenance: SourceProvenance,        // backend, interim_demosaic flag, decode params hash
}

/// Non-raw decode (JPEG/PNG/TIFF/HEIC). ICC extracted, EXIF orientation applied.
pub fn decode_image(path: &Path) -> Result<SourceImage, DecodeError>;

/// DCP parse (curated/user profiles — untrusted input, fuzzed, capped).
pub fn parse_dcp(bytes: &[u8]) -> Result<CameraProfile, DcpParseError>;
```

### 3.2 `lightbox-decode` — error taxonomy & proxy protocol

```rust
#[non_exhaustive]
pub enum DecodeError {
    UnsupportedFormat { format: String },
    CorruptFile { detail: String },
    ProxyCrashed { signal: Option<i32> },     // child died — parent unaffected, retriable once
    ProxyTimeout,
    ResourceCap { cap: CapKind },             // dims / payload / memory caps exceeded
    Io(std::io::Error),
}
impl DecodeError { pub fn catalog_code(&self) -> &'static str; }  // → asset.decode_error

/// lightbox-rawproxy wire protocol (v1): u32-LE length-prefixed CBOR frames over stdin/stdout;
/// pixel payloads returned via a temp shm file (memfd/CreateFileMapping) named in the response.
pub enum ProxyRequest {
    Hello { proto: u16 },
    Probe { path: PathBuf },
    DecodeMosaic { path: PathBuf },
    DecodeDemosaiced { path: PathBuf, params: LibrawParams }, // AHD, no WB, raw color, 16-bit linear
    Shutdown,
}
pub enum ProxyResponse {
    Hello { proto: u16, libraw_version: String },
    Ok { meta: ProxyMeta, payload: Option<ShmRef> },
    Err { code: String, detail: String },
}
```

Supervisor contract: one warm pooled child, restarted on crash/hang; caller sees `DecodeError`, never a broken pipe; kill-on-timeout; caps enforced child-side (rlimit/JobObject) **and** parent-side (payload size).

### 3.3 `lightbox-color` — camera color & white balance

```rust
pub struct Mat3(pub [[f64; 3]; 3]);          // + mul/inv/transpose; f64 throughout the solver

/// Fixed working space (§4.1): ProPhoto/ROMM primaries, linear TRC, D50 white. Not configurable.
pub mod spaces {
    pub fn xyz_d50_to_working() -> Mat3;     // derived from primaries at build time, tested vs refs
    pub fn working_to_xyz_d50() -> Mat3;
    /// "Melissa-style" companion encode for histograms/readouts (E10 consumes): sRGB curve over
    /// ProPhoto primaries. Encode only — never a processing space.
    pub fn companion_encode(rgb_linear: [f32; 3]) -> [f32; 3];
}

pub mod cct {
    pub fn xy_to_cct_tint(xy: [f64; 2]) -> (f64, f64);   // DNG-compatible locus + orthogonal tint
    pub fn cct_tint_to_xy(cct: f64, tint: f64) -> [f64; 2];
}

pub enum WbMode { AsShot, TempTint { kelvin: f64, tint: f64 }, Neutral([f64; 3]) }

/// §1.7 tier 1: build the license-clean colorimetric base from file metadata alone.
pub fn camera_matrix_base(c: &RawColorimetry, camera: &CameraId) -> Result<CameraProfile, ColorError>;

/// Per-profile colorimetric solver (DNG spec ch. 6 semantics).
pub struct ColorimetricSolver<'p> { /* profile + precomputed state */ }
impl<'p> ColorimetricSolver<'p> {
    pub fn new(profile: &'p CameraProfile) -> Result<Self, ColorError>;
    /// White-point self-consistent iteration for AsShot neutral; direct for TempTint.
    pub fn white_point(&self, wb: &WbMode, as_shot: Option<[f64; 3]>) -> Result<WhitePoint, ColorError>;
    pub fn neutral_from_temp_tint(&self, kelvin: f64, tint: f64) -> [f64; 3];
    pub fn temp_tint_from_neutral(&self, neutral: [f64; 3]) -> (f64, f64);
    /// Interpolated (by inverse CCT) camera→XYZ(D50): ForwardMatrix path when present,
    /// else inverse-ColorMatrix + Bradford to D50.
    pub fn cam_to_xyz_d50(&self, wp: &WhitePoint) -> Mat3;
}

/// WB presets (Daylight/Cloudy/Shade/Tungsten/Fluorescent/Flash → CCT/tint) — E10's preset menu.
pub fn wb_presets() -> &'static [(WbPreset, f64, f64)];
```

### 3.4 `lightbox-color` — profiles, look, resolved transform

```rust
pub struct ProfileId(pub [u8; 16]);          // xxh3-128 of canonical serialization
pub enum ProfileSource { MatrixBase, CuratedDcp, UserDcp }

pub struct CameraProfile {
    pub id: ProfileId,
    pub name: String,
    pub source: ProfileSource,
    pub calibration: DualIlluminant,          // illuminants + CM1/2, FM1/2, analog balance
    pub hue_sat_map: Option<HueSatLut>,       // + HueSatEncoding (Linear | Srgb)
    pub look_table: Option<HueSatLut>,
    pub tone_curve: Option<Spline1D>,         // ProfileToneCurve control points
    pub baseline_exposure_offset: f32,
    pub default_black_render: DefaultBlackRender,
    pub copyright: Option<String>,            // surfaced; Adobe-authored content is banned upstream
}

pub struct HueSatLut { pub dims: [u32; 3], pub deltas: Vec<[f32; 3]>, pub encoding: HueSatEncoding }
impl HueSatLut { pub fn eval(&self, hsv: [f32; 3]) -> [f32; 3]; }  // trilinear, hue-wrapped

/// Lightbox look (§1.7 tier 2). Loaded from versioned .lblook CBOR.
pub struct Look {
    pub id: ProfileId, pub name: String, pub version: u16,
    pub tone_curve: Spline1D,                 // scene-referred; identity for "Lightbox Neutral"
    pub hue_sat: Option<HueSatLut>,
    pub provenance: LookProvenance,           // author, license, review record — manifest-checked
}
pub fn load_look(bytes: &[u8]) -> Result<Look, LookError>;

/// The E05/E10 seam: a backend-agnostic, GPU-ready description of the §4.1 input-transform stage.
/// Plain data (matrix + sampled tables) — a WGSL node uploads these; eval_cpu is the reference
/// semantics golden tests and the §4.4 CPU path share.
pub struct ResolvedInputTransform {
    pub cam_to_working: [[f32; 3]; 3],        // WB ⊕ interpolated matrices ⊕ adaptation, baked
    pub baseline_exposure: f32,
    pub hue_sat_map: Option<HueSatTable>,     // resolved (encoding applied), upload-ready
    pub look_table: Option<HueSatTable>,
    pub profile_tone_curve: Option<Curve1D>,  // sampled N=4096
    pub look: Option<ResolvedLook>,           // default-look curve/shaping ⊕ amount pre-applied
    pub key: u64,                             // content hash → E05 node-cache key component
}
impl ResolvedInputTransform {
    /// Reference evaluator: camera-native linear RGB → working-space linear RGB.
    pub fn eval_cpu(&self, rgb_cam: [f32; 3]) -> [f32; 3];
}

pub fn resolve_input_transform(
    profile: &CameraProfile, wb: &WbMode, as_shot: Option<[f64; 3]>,
    look: Option<&Look>, look_amount: f32,     // 0.0..=2.0; 1.0 = authored strength
) -> Result<ResolvedInputTransform, ColorError>;

/// Recipe seam (E09): resolve a Recipe.base_profile reference with the Risk-10 fallback chain:
/// requested profile → (missing) → matrix base + default look, with a user-visible flag.
pub struct ProfileRef { pub kind: ProfileKind, pub id: Option<ProfileId>,
                        pub look_ref: Option<ProfileId>, pub look_amount: f32 }
pub struct ResolvedProfile { pub profile: CameraProfile, pub look: Option<Look>,
                             pub fallback: Option<FallbackReason> }
pub fn resolve_profile_ref(reg: &dyn ProfileRegistry, colorimetry: &RawColorimetry,
                           camera: &CameraId, r: &ProfileRef) -> Result<ResolvedProfile, ColorError>;
```

### 3.5 `lightbox-color` — ICC / display / output

```rust
/// LCMS2 wrapper. MIT core only — fast_float plugin (GPL-3) is FORBIDDEN and CI-asserted absent.
pub struct IccProfile { /* parsed, size-capped, error-callback captured */ }
impl IccProfile {
    pub fn from_bytes(bytes: &[u8]) -> Result<IccProfile, IccError>;   // untrusted-input hygiene
    pub fn srgb() -> IccProfile;  pub fn display_p3() -> IccProfile;  // built-ins
}

pub trait DisplayProfileProvider: Send + Sync {
    fn profile_for_monitor(&self, monitor: MonitorId) -> Option<IccProfile>;  // shell implements per-OS
}

/// Working → display, baked for GPU: 1D shaper + 65³ LUT. Exact LCMS2 path kept for tolerance tests.
pub struct DisplayTransform { pub shaper: Curve1D, pub lut: Lut3D, pub key: u64 }
pub fn build_display_transform(display: &IccProfile, intent: Intent /* default RelColorimetric+BPC */)
    -> Result<DisplayTransform, IccError>;

/// Working → output space for export (E15 seam). Exact LCMS2 path — never the baked LUT.
pub enum OutputSpace { Srgb, AdobeRgb, ProPhoto, DisplayP3, Rec2020, UserIcc(IccProfile) }
pub struct OutputTransform { /* … */ }
impl OutputTransform {
    pub fn new(dest: OutputSpace, depth: BitDepth, intent: Intent) -> Result<Self, IccError>;
    pub fn apply(&self, working_linear: &[f32], out: &mut OutputBuffer);
    pub fn profile_bytes(&self) -> &[u8];      // for embedding in exported files
}
```

### 3.6 Camera identity normalization

```rust
/// Normalized (make, model) key used to join: probe metadata ↔ curated-profile lookup ↔
/// lensfun (E11 seam) ↔ catalog camera_model column. Alias table handles vendor spelling drift
/// ("NIKON CORPORATION NIKON Z 6" → ("Nikon", "Z6")).
pub struct CameraId { pub make: String, pub model: String,
                      pub raw_make: String, pub raw_model: String }
pub fn normalize_camera(raw_make: &str, raw_model: &str) -> CameraId;
```

---

## 4. Data model & migrations

### 4.1 Catalog migration (next free slot after E01's baseline, e.g. `0002_e02_color.sql`)

```sql
-- Registry of installed color assets (curated bundled + user-installed). Bundled entries are
-- re-synced idempotently on catalog open (like model_pack); files themselves live in the app
-- resources / user profile dir — the catalog stores identity + provenance, not blobs.
CREATE TABLE camera_profile (
    id            TEXT PRIMARY KEY,          -- ProfileId hex (xxh3-128 of canonical bytes)
    kind          TEXT NOT NULL CHECK (kind IN ('dcp', 'look')),
    name          TEXT NOT NULL,
    camera_make   TEXT,                      -- NULL for looks
    camera_model  TEXT,                      -- normalized (CameraId)
    source        TEXT NOT NULL CHECK (source IN ('bundled', 'user')),
    license       TEXT NOT NULL,             -- mirrors the surface-3 manifest entry
    file_path     TEXT NOT NULL,
    file_hash     TEXT NOT NULL,
    installed_at  INTEGER NOT NULL,          -- unixepoch
    UNIQUE (kind, name, camera_make, camera_model)
);
CREATE INDEX idx_camera_profile_camera ON camera_profile (camera_make, camera_model);

-- Diagnostics: which backend produced the accepted decode (libraw_proxy | in_crate | image codec id).
ALTER TABLE asset ADD COLUMN decode_backend TEXT;
```

Notes: `edit_recipe.doc.base_profile` (owned by E09, schema frozen in §3.2) references `camera_profile.id`; resolution + fallback semantics are E02's `resolve_profile_ref` (§3.4). No changes to `preview`, `edit_recipe`, or mask tables. Migration is copy-on-write per §6 and fault-injection tested.

### 4.2 `DecodedRawState` — the E03 raw-cache payload (binary format v1)

```
[u32 magic 'LBRS'][u16 version=1][u16 flags]
[CBOR header: dims, layout (mosaic|rgb), cfa, colorimetry (RawColorimetry), provenance
  { backend, libraw_version?, interim_demosaic: bool, decode_params_hash: u64 }]
[zstd frame: pixel plane(s), row-major]
```

Cache key inputs (E03 stores under `content_hash` + params): `decode_params_hash = xxh3(backend policy, interim_demosaic, proxy libraw version, linearize impl version)`. The `interim_demosaic` bit segregates M1 LibRaw-AHD states from post-E11 mosaic states so the M2 demosaic swap is a cache miss, never a stale hit.

### 4.3 Data-provenance manifest — surface 3 (§8), color entries

`assets/MANIFEST.toml` (E02 establishes the format for color assets; E16 hardens release-runner asset-tree inspection):

```toml
[[asset]]
path       = "color/looks/lightbox-color-v1.lblook"
kind       = "look"
provenance = "lightbox-authored"           # + review record id
license    = "project"
bundled    = true

[[asset]]
path       = "color/profiles/Sony/ILCE-7M4.dcp"
kind       = "camera-profile"
provenance = "profgen:session-2026-07-xx"  # target-shot session id, dcamprof version
license    = "project"
bundled    = true
```

Policy checker (CI, PR-blocking): every file under `assets/color/` has a manifest entry; provenance ∈ {lightbox-authored, profgen:…, cleared community pack (CC0/CC-BY + attribution)}; **any entry whose provenance or copyright indicates Adobe authorship fails the build** (constraint 4). The checker also cross-reads `ProfileCopyright` from bundled `.dcp` bytes.

---

## 5. Pipeline semantics pinned by this epic

### 5.1 Decode path selection

1. `probe()` classifies `decode_support` using E01's permissive metadata walkers (metadata-only, no pixel decode). 2. `Auto` policy: **CFA-mosaic raw (Bayer/X-Trans) decodes via `lightbox-rawproxy` (LibRaw, Phase C) — the PRIMARY and only mosaic path**; linear-DNG and monochrome decode **in-crate** via the permissive walker + `linearize`; non-raw via the image codecs. Proxy unavailable / crashes / times out on a mosaic file → `asset.decode_error` + structured code, asset stays catalogued (§6) — there is no in-process mosaic fallback (single mosaic backend by license necessity; see R1). 3. M1 develop renders request `interim_demosaic = true` → LibRaw AHD via proxy for mosaic; in-crate linear path for linear-DNG/mono. 4. A per-format quirk list (config, versioned in-repo) pins proxy decode params (`ForceProxy` variants) for bodies where LibRaw output is known-wrong; every quirk entry links an upstream issue. **rawler is never in the crate graph (LGPL-2.1, denied in `deny.toml`).**

### 5.2 Input-transform stage order (PV1)

Camera-native linear RGB → `cam_to_working` matrix (WB ⊕ dual-illuminant interpolation ⊕ ForwardMatrix-or-inverse-CM ⊕ Bradford→D50 ⊕ XYZ→ProPhoto) → `BaselineExposureOffset` → HueSatMap (encoding per tag) → LookTable (encoding per tag) → ProfileToneCurve → Lightbox look (tone curve + shaping ⊕ amount). **Binding arbiter:** where the DNG SDK's documented model and dcamprof's rendering disagree with this ordering or the encoding handling, the dcamprof patch-render harness (task F6) wins, and this section is amended before PV1 freezes. After M1 ships, this stage order is frozen under PV1 — any change is a new process version (§4.5).

### 5.3 Non-raw images

Decoded display-referred, ICC-tagged (untagged ⇒ assumed sRGB) → LCMS2 to working space (linearized) → same downstream pipeline. `base_profile` for non-raw images is `MatrixBase`-kind with an identity camera stage; the default look **does not** apply to non-raw sources (they already carry a rendered look) — pinned here so E10's profile UI doesn't have to guess.

### 5.4 White-balance contract with E10

E10's WB node applies per-channel gains **inside** the resolved matrix (the solver bakes WB into `cam_to_working`), so a WB slider change re-runs `resolve_input_transform` (<1 ms, CPU) and invalidates the input-transform node's cache key — never a decode or demosaic. The eyedropper solve is `temp_tint_from_neutral` on the sampled camera-native RGB (E10 supplies the sample; the sample must be taken pre-input-transform, which the E05 source node exposes).

---

## 6. Ordered task breakdown

Every task ≤ 1 engineer-day. AC = acceptance criteria. Phases are ordered; tasks within a phase are ordered unless marked ∥ (parallelizable). Milestone tags: [M1] must land for M1 exit; [M2] may trail into early M2 per §9.

### Phase A — decode spine (in-crate metadata + non-raw + linear; mosaic decode is Phase C) [M1]

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | Scaffold `lightbox-decode` + `lightbox-color` crates: error taxonomies, feature flags, workspace wiring | Builds on macOS/Windows/Linux CI; cargo-deny green; `DecodeError::catalog_code()` unit-tested |
| A2 | Test-corpus bootstrap: pinned CC0 raw set from raw.pixls.us (≥12 bodies: CR2, CR3, NEF, ARW, RAF/X-Trans, ORF, RW2, PEF, DNG, linear DNG, mono DNG, float DNG-reject case) + fetch script + hash manifest | `just fetch-corpus` reproducible offline-cached; every file's CC0 status recorded; corpus lives outside git |
| A3 | `probe()` v1: E01 permissive TIFF-IFD walkers + kamadak-exif; format, dims, capture time, embedded-preview descriptors, CFA, `decode_support` classification | Correct fields for all corpus files (fixture-asserted); truncated/garbage file → structured error, no panic; no rawler in the crate graph |
| A4 | Camera identity normalization: `normalize_camera` + alias table (initial ~30 aliases across the corpus makers) | Corpus bodies normalize to expected keys; alias table is data (TOML), unit-tested; doc for E11/lensfun reuse |
| A5 | In-crate raw metadata + colorimetry via E01's permissive TIFF-IFD/EXIF walker → `RawColorimetry` fully populated (levels, CFA, matrices, illuminants, as-shot neutral); linear-DNG + monochrome-DNG in-crate decode → `RawDecode::Linear`. **CFA-mosaic pixel decode itself is Phase C (C2, LibRaw proxy).** | Colorimetry fields for all corpus raws spot-checked vs exiftool/dcraw reference dumps; linear-DNG + mono-DNG decode in-crate; cargo-deny asserts no rawler in the crate graph |
| A6 | `linearize()` CPU reference: black subtract (per-CFA-position), linearization table, white normalize, active-area crop; kernel-spec doc-comment for E05 | Synthetic mosaic fixtures (known levels/table → exact expected f32); 12/14/16-bit inputs covered; mono + linear-DNG passthrough |
| A7 ∥ | Non-raw decode: JPEG (zune-jpeg), PNG, TIFF (8/16-bit) → `SourceImage` with ICC bytes + EXIF orientation | Fixtures with embedded ICC round-trip the profile bytes; 16-bit TIFF precision preserved; orientation matrix applied |
| A8 ∥ | HEIC decode via libheif (dyn-link) behind feature flag; surface-2 SBOM rows (libheif LGPL-3 + libde265, x265 ban assert) | HEIC fixture decodes; SBOM policy check green; main-binary symbol audit shows dyn-link only |
| A9 | Panic containment + decode-error write path: `catch_unwind` at the API boundary; taxonomy → `asset.decode_error` | 1 000 mutated/truncated corpus variants: zero panics, zero UB (ASan job), 100 % structured errors |

### Phase B — camera-matrix base + WB solver (E02.1 color) [M1]

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `lightbox-color` math core: Mat3/Vec3, Bradford CAT, xy↔XYZ, primaries→matrix derivation; working-space + standard-space constants | Derived XYZ(D50)↔ProPhoto matrix matches published references to 1e-4; CAT round-trip property test |
| B2 | CCT module: DNG-compatible locus, `xy_to_cct_tint` + inverse | A/D50/D65 recovered within ±15 K; round-trip property test over a locus grid; tint orthogonality test |
| B3 | Dual-illuminant interpolation + white-point self-consistent iteration (AsShot neutral) | Converges ≤ 8 iterations on all corpus profiles; matches known reference values for 3 bodies within tolerance; non-convergence → structured error |
| B4 | `cam_to_xyz_d50`: ForwardMatrix path (analog balance, reference-neutral diag) + no-FM path (inverse CM + Bradford) | Fixture tests for both paths; singular/degenerate matrix rejected cleanly |
| B5 | WB solver: `neutral↔temp/tint` both directions + preset table | Round-trip property test within ±3 % CCT / ±2 tint over corpus profiles; as-shot neutrals produce plausible CCT (committed sanity table) |
| B6 | HueSat LUT evaluator (shared by DCP + look): trilinear with hue wrap, Linear/Srgb encodings, val-dims=1 fast path | Hand-computed synthetic-LUT fixtures pass; hue-wrap continuity property test (no seam at 0°/360°) |
| B7 | **The first failing test** + `camera_matrix_base()`: tier-1 profile from decode metadata alone | `renders_correct_color_with_zero_bundled_profiles` green: corpus raw → matrix base → golden within ΔE2000 ≤ 1.0, with assets dir absent |
| B8 | `resolve_input_transform` + `eval_cpu` reference evaluator + content-hash key | Resolve < 1 ms (criterion); identical inputs ⇒ identical key; WB/look/amount change ⇒ key change; eval_cpu property: identity profile ⇒ identity transform |
| B9 | CPU reference pipeline harness: `lightbox-cli render-ref` (decode → linearize → interim demosaic → WB → matrix base → look → display-sRGB PNG) | Renders full corpus headless in CI; per-image ΔE report; goldens committed with `--bless` workflow |

### Phase C — LibRaw sandbox + interim demosaic [M1]

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | `lightbox-rawproxy` binary skeleton: CBOR-frame protocol, version handshake, shm payload handoff | Probe round-trip integration test; protocol doc committed; payload cap enforced both sides |
| C2 | LibRaw FFI inside the proxy: `DecodeMosaic` (primary CFA path) + `DecodeDemosaiced` (AHD, linear 16-bit camera-RGB, no WB/gamma/output-color) | Corpus mosaic raws decode via proxy; metadata parity with the in-crate permissive walker on shared colorimetry fields; LibRaw version reported in provenance |
| C3 | Supervisor client: warm pooled child, timeout, kill+restart, `Auto` routing (mosaic → proxy; proxy crash/timeout → `decode_error`, no in-process retry path exists) | Injected SIGKILL and hang in child → parent returns `ProxyCrashed`/`ProxyTimeout` within budget; next request succeeds on a fresh child |
| C4 | Resource limits & hygiene: rlimits (POSIX) / JobObject (Windows), temp cleanup, dims/memory caps, no-network assertion | Crafted memory-bomb fixture rejected by cap; parent RSS unaffected; lsof/handle audit shows no leaked temp files after 100 cycles |
| C5 | `decode_for_develop` wiring: interim-demosaic `SourceImage` via proxy (Bayer/X-Trans) or in-crate decode (linear-DNG/mono); provenance flags set | `render-ref` end-to-end uses it; X-Trans corpus file renders; `interim_demosaic` bit present in provenance + cache hash |
| C6 | `DecodedRawState` v1 (de)serialization + `decode_params_hash` (E03 contract) | Round-trip identity test; version byte honored; hash stable across platforms (cross-CI assertion) |
| C7 | License wiring: LibRaw dyn-linked to the proxy **only**; surface-2 SBOM rows; main-app symbol audit; **`deny.toml` bans the rawler crate graph-wide** | CI asserts no LibRaw dependency in app binaries; `cargo deny check` fails if rawler (or any LGPL crate) enters the graph; SBOM row carries LGPL-2.1 + relink note |

### Phase D — color management: working / display / output (E02.3) [M1]

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | LCMS2 integration (`lcms2` crate, static): thread-safe contexts, error-callback capture; **assert fast_float plugin absent** (GPL-3) | cargo-deny + SBOM entries green; a CI grep/symbol check proves no fast_float; malformed-profile error surfaces as `IccError`, no abort |
| D2 | Working-space module + companion encode (Melissa-style) for E10 histogram/readouts | Companion encode matches an LCMS2-built equivalent transform ≤ 1e-4; rustdoc pins semantics (encode-only, never processing) |
| D3 | `build_display_transform`: working → display ICC, rel-col+BPC, baked shaper + 65³ LUT + key | Identity profile ⇒ identity LUT within 1e-4; sRGB + wide-gamut fixture profiles; bake ≤ 100 ms then cached by key |
| D4 | Baked-LUT fidelity gate: LUT-applied vs exact LCMS2 over 100 k random working-space samples | ΔE2000 ≤ 0.5 p99, ≤ 1.0 max; failure path documented (LUT-size escalation) |
| D5 | `DisplayProfileProvider`: macOS ColorSync + Windows ICM implementations; Linux `_ICC_PROFILE`/colord best-effort behind feature | mac/win CI runners fetch a real monitor profile; missing/invalid profile → sRGB fallback + surfaced event (shell listens, E08 seam) |
| D6 | `OutputTransform` for E15: all §3.5 spaces, 8/16-bit int + f32, embeddable profile bytes, exact LCMS2 path | Round-trip working→sRGB→working error bound test; emitted ICC bytes validate in external tooling; 16-bit TIFF + 8-bit JPEG buffer paths covered |
| D7 | Untrusted-ICC hygiene: size caps, fuzz target over `IccProfile::from_bytes` | 1 M+ fuzz iterations zero crash/OOM; adversarial fixture set (truncated, huge tag tables) all structured errors |

### Phase E — Lightbox default look family (E02.4) [M1]

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | `.lblook` format v1 (CBOR, versioned) + loader/validator; `Look` type + provenance block | Round-trip property test; unknown-version rejected with structured error; schema documented in rustdoc |
| E2 | Look evaluator with amount 0–200 %: identity-lerp below 100 %, bounded extrapolation above, clamped | amount=0 ⇒ bit-exact identity; continuity property test across amount range; eval folded into `ResolvedInputTransform.key` |
| E3 | Authoring harness: `lightbox-cli look-dev` renders a neutral scene corpus (≥30 CC0 scenes: skin, foliage, sky, low-light, high-DR, clipped highlights) to an HTML contact sheet, base vs look | Contact sheet generated on demand in CI; scene corpus manifest committed with licenses |
| E4 | Author **Lightbox Color v1** iteration 1: scene-referred S-curve + highlight-desaturation guard + mild hue/sat shaping | Renders across the scene corpus without hue skews on skin/sky (harness ΔH report); no clipping artifacts vs base |
| E5 | Perceptual review gate + sign-off: structured checklist (skin, sky, foliage, neutrals, gradients, clipped highlights), ≥2 reviewers; iterate once on findings | Review record committed (reviewers, date, findings, resolution); provenance affidavit: no Adobe-derived data used at any authoring step |
| E6 | Ship looks + manifest + defaults: `lightbox-color-v1` + `lightbox-neutral` in `assets/color/looks/`, surface-3 entries, `ProfileRef` default (`look_ref = lightbox-color-v1`, `look_amount = 1.0`) documented for E09 | Manifest policy checker green; recipe default pinned in rustdoc; raw vs non-raw look-application rule (§5.3) asserted in a test |
| E7 | Look golden corpus: corpus × default look through the CPU reference path, PR-blocking gate | Intentional 1-LSB curve perturbation fails the gate (verified once, reverted); `--bless` regeneration requires review |

### Phase F — DCP parser + evaluator (E02.2) [M1 code; consumed at M2]

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | TIFF-IFD reader for `.dcp` containers (own minimal reader; reuses E01's permissive TIFF-IFD infra) | Parses dcamprof- and dcptool-produced profiles; unknown tags skipped with warning; IFD-loop/overlap guards |
| F2 | `CameraProfile` from DCP: all §3.4 fields incl. HueSatMap dims/data/encoding, LookTable, tone-curve spline | Field-level equality vs `dcptool -d` XML dumps for 3 reference profiles |
| F3 | Parser hardening: cargo-fuzz target + adversarial fixtures (truncated, dims overflow, NaN floats, giant tables) | 1 M+ iterations zero panics/OOM; dims×size caps enforced; fuzz job wired into CI (nightly) |
| F4 | Profile tone-curve evaluation: monotone cubic spline → `Curve1D[4096]` | Matches dcamprof's rendering of the same curve within 1e-3; endpoint/degenerate-control-point tests |
| F5 | Full DCP path in `resolve_input_transform`: interpolation ⊕ HueSatMap ⊕ LookTable ⊕ tone curve per §5.2 order | Synthetic ColorChecker patch set through our evaluator vs dcamprof reference render: mean ΔE2000 ≤ 0.5, max ≤ 1.5 |
| F6 | dcamprof reference harness (`tools/`, GPL subprocess, CI-cached): generate reference DCP + patch renders from a public target shot; **pins §5.2 stage order/encodings as executable tests** | Harness reproducible; dcamprof exec'd only (never linked, never shipped); any §5.2 amendment lands before PV1 freeze |
| F7 | `resolve_profile_ref` + fallback chain (Risk 10): requested → missing → matrix base + default look with `FallbackReason` | Unit tests over all branches; recipe naming an uninstalled profile renders via matrix base and carries the user-visible flag |

### Phase G — curated camera-profile content line (E02.5) [M2]

| # | Task | Acceptance criteria |
|---|---|---|
| G1 | `tools/lightbox-profgen` skeleton: session dir (raws + YAML metadata: body, chart, illuminants) → dcamprof subprocess → `.dcp` | End-to-end run on a public sample target shot emits a parseable profile; GPL isolation asserted (subprocess exec, tool excluded from app packaging) |
| G2 | Target-shot capture protocol doc: chart (ColorChecker 24/SG), dual-illuminant procedure (StdA + daylight), exposure/flat-field rules, session schema | Reviewed by the content-line owner (or escalation recorded, see Open Questions); printable checklist |
| G3 | Validation harness in profgen: parse with our F-phase engine, patch ΔE vs dcamprof reference + measured chart values, reject gates | Sample profile passes; deliberately corrupted profile and an out-of-tolerance profile are rejected with reports |
| G4 | Packaging + provenance: profiles → `assets/color/profiles/<make>/<model>.dcp`, manifest entries (session id, dcamprof version, project license), catalog sync-on-open | Manifest checker fails on missing provenance; app bundles only manifest-cleared profiles; `camera_profile` rows appear after open |
| G5 | Auto-selection policy: normalized `CameraId` → curated DCP default-if-present else matrix base; per-image override honored | Lookup unit tests incl. alias table; policy documented for E10's profile browser |
| G6 | First content batch: generate + validate profiles for the initial curated bodies (target ≥5 at epic close, from bodies/target-shots available — see Open Questions) | Each passes G3 gates + E5-style review; manifest + per-body golden committed |
| G7 | Content-line runbook: per-body cost (capture + generation + verification), cadence, target-shot raw retention, ownership; restates the §12 escalation | Runbook merged; named owner recorded **or** the unstaffed state explicitly escalated to CTO/operator |

### Phase H — integration, hardening, seams [M1 except H5-golden-per-PV which spans]

| # | Task | Acceptance criteria |
|---|---|---|
| H1 | Catalog migration `0002_e02_color` (camera_profile + decode_backend) + bundled-asset sync-on-open | Up/down migration tested; `kill -9` mid-migration fault injection leaves catalog `integrity_check`-clean (copy-on-write per §6) |
| H2 | `lightbox-cli` subcommands: `probe`, `decode`, `render-ref`, `profile inspect/install`, `look inspect` | Headless E2E: fetch-corpus → probe+decode all → render-ref → report; used by the CI golden job |
| H3 | Perf bench suite (criterion + scenario): decode per format, resolve transform, display bake, proxy overhead, linearize throughput | Nightly baselines recorded; §7-derived budget assertions (see Test Plan) wired as regression alarms |
| H4 | ASan/LSan CI job over decode + FFI surfaces (LCMS2 wrapper, proxy client, libheif path) | Green on the full corpus + adversarial fixtures |
| H5 | Golden gate per process version: E02 CPU-reference goldens keyed `(pv=1)`, PR-blocking, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB | Gate wired; PV-immutability guard documented (post-M1, any §5.2 change requires a new PV entry, §4.5) |
| H6 | Seam handoff docs (rustdoc-level): `ResolvedInputTransform`/`DisplayTransform` node-param contract + WGSL porting notes (E05/E10), `DecodedRawState` (E03), `probe` (E04), `OutputTransform` (E15), `ProfileRef` (E09) | E05 planner sign-off recorded on the transform-spec contract; each consumer doc names invariants + failure modes |
| H7 | Threat-model notes for the §12 security review: decode surface (in-crate permissive-walker robustness, proxy caps + sandbox), DCP/ICC untrusted parse, fuzz coverage summary | Notes committed alongside the crates; open risks enumerated for the security-engineer phase |

**Task count: 60** (A:9, B:9, C:7, D:7, E:7, F:7, G:7, H:7), each ≤1 day ⇒ ~60 engineer-days ≈ 12 pw — inside the XL 9–14 pw band with the content batch (G6) explicitly elastic.

**Suggested parallelization (2 engineers):** Eng-1: A → B → F → G; Eng-2: C ∥ D → E → H. The hard serializations are A→{B,C,D}, B6→{E2,F5}, **C2→B9** (full-corpus mosaic render-ref needs the proxy; B's tier-1 math anchors on in-crate-decodable linear-DNG + synthetic mosaics until C2 lands), F→G. See §0 for the wave schedule.

---

## 7. Test plan (per §8 strategy)

### 7.1 Unit (PR-blocking)
- Matrix/CAT/CCT math vs published reference values (1e-4); primaries→matrix derivation.
- Linearization on synthetic mosaics (exact expected output); bit-depth variants.
- HueSat LUT evaluation vs hand-computed fixtures; hue-wrap continuity.
- DCP field parity vs `dcptool -d` dumps; `.lblook` round-trip; error-taxonomy mapping.
- WB solver round-trip (`neutral↔temp/tint`) — property test (`proptest`) over corpus profiles.
- `resolve_input_transform` key stability/sensitivity; identity-profile ⇒ identity-transform property.

### 7.2 Golden-image (PR-blocking; the PV1 color contract)
- **Matrix-base goldens:** corpus × as-shot WB × {no look, default look} through the CPU reference pipeline → committed sRGB goldens, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB. Includes the zero-bundled-assets first failing test.
- **DCP evaluator goldens:** ColorChecker patch renders vs **dcamprof reference** (mean ΔE2000 ≤ 0.5 / max ≤ 1.5) — the binding arbiter for §5.2 stage semantics.
- **Display/output fidelity:** baked display LUT vs exact LCMS2 (ΔE2000 ≤ 0.5 p99); output round-trip bounds.
- Golden regeneration only via `--bless` + human review; per-PV keying wired for the §4.5 immutability guard (E05 extends to GPU parity when nodes exist — E02's CPU reference is the parity anchor).

### 7.3 Integration (PR-blocking fast subset, full nightly)
- Headless `lightbox-cli` E2E: corpus probe → decode (both backends) → render-ref → report.
- Proxy crash/hang injection: SIGKILL, infinite loop, memory bomb → structured errors, warm-pool recovery, parent unharmed.
- Decode-failure path: corrupt file → `asset.decode_error` set, asset browsable (with E01 catalog).
- Per-monitor ICC fetch on mac/win runners; missing-profile → sRGB fallback event.
- Migration fault injection (`kill -9` mid-`0002`).

### 7.4 Fuzz & sanitizers (nightly)
- cargo-fuzz targets: `parse_dcp`, `IccProfile::from_bytes`, `.lblook` loader, proxy-frame decoder, raw-container mutation corpus. Zero panics/OOM/UB; ASan/LSan job over FFI surfaces.

### 7.5 Performance (nightly, §7-derived budgets)

| Metric | Budget | Ties to |
|---|---|---|
| `probe()` p50 | ≤ 5 ms | E04 import throughput |
| proxy mosaic decode + in-crate linearize, 24 MP / 45 MP (single core, M-series base, incl. warm-proxy overhead) | ≤ 500 ms / ≤ 900 ms | §7 develop-open (background, parallel per-core) |
| Proxy overhead (warm child, excl. LibRaw decode) | ≤ 30 ms | interim path viability |
| Proxy cold spawn + handshake | ≤ 150 ms | first-use latency |
| `resolve_input_transform` | ≤ 1 ms | WB slider inside the <100 ms §7 budget |
| Display-transform bake (per monitor, first time) | ≤ 100 ms, then cached | develop-open |
| `parse_dcp` | ≤ 10 ms | profile browser (E10) |
| `DecodedRawState` serialize 45 MP | ≤ 250 ms (zstd level tuned) | E03 raw-cache write |

### 7.6 License gates (PR- and release-blocking)
- Surface 1: cargo-deny (lcms2 MIT vendored-static, zune-jpeg MIT, png/tiff MIT/Apache, …); **rawler (LGPL-2.1) explicitly denied — never in the crate graph**; the only LGPL code (LibRaw) lives out-of-process in the proxy, dynamic-linked.
- Surface 2: SBOM rows for LibRaw (dyn, proxy-only, relink note), libheif+libde265 (x265 ban), LCMS2 (static MIT, **fast_float absent**); main-binary symbol audits.
- Surface 3: `assets/MANIFEST.toml` policy checker — every color asset has cleared provenance; **Adobe-authored content check fails the build**; dcamprof appears nowhere in shipped artifacts.

---

## 8. Risks & open questions

### Risks

| # | Risk | Mitigation in this epic |
|---|---|---|
| R1 | **LibRaw coverage gaps / wrong output for specific bodies** (Risk 3 of the catalog: camera treadmill) — and, because rawler is license-banned, the proxy is the **single** mosaic backend with no in-process second try | LibRaw's body coverage is broad and updates with the vendored version; per-format quirk list (§5.1) pins proxy decode params per body; `decode_backend` + `decode_error` telemetry quantifies failure share so coverage work is data-driven; loss of dual-backend redundancy is accepted as a license consequence (blast radius = per-file decode failure, asset stays catalogued) |
| R2 | **LCMS2 fast_float trap:** the research report suggests the fast-float plugin, but it is **GPL-3** — linking it would violate the license mandate | Explicitly forbidden (D1 assert + SBOM). Perf need is met by baking display LUTs once and applying them on GPU; exact LCMS2 runs only at bake/export time |
| R3 | **DCP stage-order/encoding subtleties** (HueSatMap sRGB encoding, LookTable order, hue-wrap): getting these silently wrong ships wrong color for curated profiles | The dcamprof patch-render harness (F6) is the binding arbiter and an executable spec; §5.2 is amendable **only before** PV1 freezes |
| R4 | **Content line unstaffed** (§12 escalation): without an owner + target-shot budget, tier 3 doesn't exist | Degradation is graceful *by design* (§1.7): every body still gets correct color via tier 1 + tier 2; G7 forces the ownership decision to be explicit, never a silent default |
| R5 | **Interim demosaic quality** sets first impressions at M1 (LibRaw AHD is below the E11.1 clean-room bar) | Accepted per §1.6 ("interim floor"); cache-key segregation (§4.2) guarantees a clean swap at M2; expectation noted in M1 demo script |
| R6 | **Default-look taste risk:** an original look that reads "off" hurts credibility more than a neutral one | Two-reviewer perceptual gate (E5), neutral fallback ships alongside; look is amount-scalable and swappable content, not code |
| R7 | **Untrusted-input surfaces** (raw containers, user DCP, user ICC) | in-crate container parsing uses memory-safe Rust permissive walkers; the memory-unsafe LibRaw mosaic decoder is sandboxed out-of-process with rlimit/JobObject caps; DCP/ICC parsers fuzzed + capped; threat-model notes handed to the §12 security review |
| R8 | **Per-monitor ICC on Linux/Wayland is immature** | Best-effort behind a feature flag with sRGB fallback + surfaced event; mandate requires mac/win at v1, Linux not architecturally excluded — this honors exactly that |

### Open questions (owners named; none block Phase A start)

1. **Curated body list + N for the first batch (G6)** — needs the CTO/operator content-line decision (§12): which bodies we own or can borrow for target shots, and the initial N. Spec assumes ≥5 at epic close. *Owner: product/ops with CTO.*
2. **Content-line owner** — the §12 escalation restated: G7 records an owner or an explicit unstaffed status. *Owner: CTO/operator.*
3. **Fluorescent/flash WB preset CCT+tint values** — pick published standard-illuminant values and commit them as data (B5); trivial but must be sourced, not invented. *Owner: implementing engineer, reviewed in B5.*
4. **`look_amount` > 100 % extrapolation semantics** (clamp curve slope? limit to 150 %?) — E2 proposes bounded extrapolation; E10's UI may choose to cap the slider. *Owner: E02 proposes, E10 disposes.*
5. **Does the proxy also serve E03 preview builds for LibRaw-only formats at M0-M1 scale?** (10k-import burst → proxy pool size >1?) — H3 benchmarks decide; the protocol already permits a small pool. *Owner: E02+E03 planners at integration.*
6. **`tiff` crate sufficiency for 32-bit float TIFF sources** — if lacking, scope-fence to 8/16-bit at M1 and log v1.x follow-up. *Owner: A7 implementer.*

---

## 9. Seams to neighboring epics (consume/provide contract table)

| Epic | E02 provides | E02 consumes | Contract artifact |
|---|---|---|---|
| E01 | — | workspace, catalog + migration runner, `lightbox-cli` base, CI scaffolding | existing |
| E03 | `DecodedRawState` format + `decode_params_hash`; embedded-preview descriptors from `probe()` | raw-cache storage/eviction | §4.2 + H6 rustdoc |
| E04 | `probe()`, decode-error taxonomy → `asset.decode_error`/`decode_backend` | import pipeline calls | §3.1/§3.2 |
| E05 | `ResolvedInputTransform`, `DisplayTransform` as plain-data node params + CPU reference evaluators + WGSL porting notes; linearize kernel spec | node registry, DAG, tiles, GPU execution | H6 sign-off doc |
| E09 | `ProfileRef` resolution semantics + default look_ref/amount; `camera_profile` registry | recipe `base_profile` field (schema frozen in §3.2 of the architecture) | §3.4 `resolve_profile_ref` |
| E10 | WB solver + presets, eyedropper solve, companion-space encode, profile auto-selection policy, look/profile registry for the browser | WB node, profile UI, histogram | §3.3/§5.4 |
| E11 | the demosaic seam: `MosaicImage`/`LinearMosaic` in, `SourceImage` out; cache-key segregation for the interim→clean-room swap; `CameraId` normalization for lensfun matching | clean-room demosaic (M2) replaces `DemosaicedInterim` | §4.2 + A4 |
| E15 | `OutputTransform` (exact LCMS2 path) + embeddable profile bytes | encoders/sizing/metadata | §3.5 |
| E16 | surface-3 manifest format + policy checker for color assets | release-runner asset-tree inspection hardening | §4.3 |

---

## 10. Definition of done

1. **All 60 tasks** merged with their acceptance criteria green on the 3-OS CI matrix.
2. **Tier-1 guarantee proven:** every corpus body renders colorimetrically-correct color through `camera_matrix_base()` with the bundled-assets directory absent (the first failing test, now permanently green as a PR gate).
3. **Tier-2 shipped:** `lightbox-color-v1` + `lightbox-neutral` bundled with perceptual-review records and surface-3 manifest entries; default recipe wiring documented for E09.
4. **Tier-3 operational:** profgen pipeline runs end-to-end (dcamprof strictly subprocess), validation gates reject bad profiles, first content batch bundled per the G6 target (or the shortfall + unstaffed-owner status explicitly escalated), runbook merged.
5. **DCP engine validated:** parse→evaluate matches dcamprof references within the F5 tolerances; parser fuzz-clean; §5.2 stage order pinned by executable tests and frozen as PV1.
6. **Color management live:** per-monitor display transforms on macOS + Windows (Linux best-effort), baked-LUT fidelity gate green, `OutputTransform` ready for E15.
7. **Sandbox holds:** LibRaw runs only in `lightbox-rawproxy`; crash/hang/memory-bomb injection tests green; no LibRaw symbols in app binaries.
8. **Contracts honored:** golden gates (matrix base, look, DCP, display) PR-blocking and keyed to PV1; all three license surfaces green including the new color-asset manifest entries; zero Adobe-authored bytes anywhere in `assets/`.
9. **Seam sign-offs:** E05 planner has accepted the transform-spec contract (H6); E03/E04/E09/E10/E15 contract docs merged.
10. **Budgets met** on the H3 nightly bench (or regressions filed with owners) for every row of the §7.5 table.
11. **No scope leakage:** no WGSL, no render nodes, no UI, no demosaic algorithms beyond the LibRaw interim call — verified at epic review against §1.2 non-goals.
