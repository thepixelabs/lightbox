# E11 — Detail, Optics, Geometry & Clean-Room Demosaic — Implementation Spec

_Epic planner: staff engineer, pixel-engine stream. Inputs: `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (approved; §4.1 pipeline, §10.1 E11 decomposition, §8 test strategy, Risks 2/6), `docs/research/03-develop-module-detail-sharpening-noise-optics-lens.md`, `docs/research/05-raw-processing-pipeline-internals-and-color-manage.md`._

| | |
|---|---|
| **Milestone** | M2 |
| **Effort** | XL, ~12–18 pw (≈60–90 engineer-days), requires a GPU/imaging specialist |
| **Depends on** | E02 (decode & color foundation), E05 (render node-graph engine) |
| **Blocks** | E12 (masks apply post-geometry; coordinate-space contract), E15 (export renders through these nodes), M2 exit |
| **Risk register** | Carries **Risk 2** (clean-room demosaic — the single hardest, most quality-defining item in the product) |
| **Staffing note** | Internally parallelizable into two lanes: Lane 1 = demosaic (Phases B), Lane 2 = detail/optics/geometry (Phases C–E). With one specialist the lanes serialize (per §10 staffing commitment); with two they overlap after Phase A. |

**Epic-level first failing test** (per §12 staff-engineer note): `demosaic_quality_gate` — the remosaic harness (task A5) asserting that the clean-room RCD output beats the committed LibRaw AHD/DCB baseline (task A6) on the reference corpus for CPSNR, false-color, and zipper metrics. It fails on day one because no clean-room implementation exists; the epic's core is making it pass.

---

## 1. Scope

### 1.1 In scope

Everything between the linearized mosaic (E02's output) and the LOCAL/masking stage (E12's input), per the §4.1 pipeline, plus the effects stage:

1. **E11.1 Clean-room Bayer demosaic** (~6–10 pw of the epic; Risk 2). RCD-class Bayer demosaic reimplemented **from published algorithm descriptions only** (GPL sources — RawTherapee/darktable — are behavior-study-only under the protocol in §6.1, never copied), in WGSL + CPU, with an algorithm spike preceding implementation. Includes: a fast **draft demosaic** (superpixel/half) for progressive first paint; routing for X-Trans/monochrome/linear-DNG inputs (X-Trans stays on the interim LibRaw path — see non-goals); the pre-demosaic classical raw-denoise slot from §4.1 filled minimally (hot-pixel/impulse suppression + green equilibration); raw-cache integration so demosaic runs once and sliders never re-trigger it.
2. **E11.2 Detail stage.** Capture sharpening (Amount/Radius/Detail/Masking, with diagnostic debug taps for the shell's Alt-key views); luminance NR (Luminance/Detail/Contrast); chroma NR (Color/Detail/Smoothness); moiré reduction node; ISO-adaptive defaults hook.
3. **E11.3 Optics.** DNG opcode parser + executor (OpcodeList1/2/3: WarpRectilinear, WarpFisheye, FixVignetteRadial, GainMap, FixBadPixels*, MapTable/MapPolynomial, Delta/Scale per row/col, TrimBounds, unknown-skip semantics); lensfun integration (DB match + model evaluation on GPU) with distortion/vignetting 0–200 % override sliders; LCP parsing for **user-supplied** profiles (no Adobe-authored LCP is ever bundled — constraint 4, same logic as §1.7 for DCP); one-click auto lateral CA removal; manual Defringe (purple/green amount + hue ranges + eyedropper sampling API); manual distortion + manual vignette sliders; correction-source precedence and per-lens defaults.
4. **E11.4 Geometry & effects.** Crop (normalized rect + angle, aspect ratios, constrain-to-warp), straighten (angle, level-tool math, auto-level), lossless rotate-90/flip; Upright automatic modes (Level/Vertical/Auto/Full) via own line detection + homography solve, with the solved homography **baked into the recipe** (§4.5 philosophy: deterministic replay, explicit recompute); manual Transform sliders (Vertical/Horizontal/Rotate/Aspect/Scale/X/Y); the **single-pass composed geometry warp** (lens geom ∘ TCA ∘ upright ∘ transform ∘ angle — one resample, per-channel for CA); post-crop vignette (Amount/Midpoint/Roundness/Feather/Highlights, three blend styles, crop-re-centering); film grain (Amount/Size/Roughness, deterministic seed, resolution-consistent).
5. **Cross-cutting.** Every node ships `eval_gpu` (WGSL) + `eval_cpu` (rayon/SIMD) within the §4.4 parity tolerance; ROI back-mapping through warps (trait extension negotiated with E05); recipe param structs + `crs:`/`lb:` mapping entries (with E09); golden corpora, quality gates, perf benches; license-surface entries (lensfun attribution, dynamic-link assertions).

### 1.2 Explicit non-goals

| Excluded | Where it lives / why |
|---|---|
| Tone/color nodes (exposure, curves, HSL, clarity/texture/dehaze…) | E10. E11 nodes sit around them per §4.1 order. |
| Halo-free highlight/shadow recovery | E10.2 (own sub-project). We *reuse* its guided-filter kernel infra where shared. |
| Output sharpening (export-time, screen/matte/glossy) | E15 export engine. Capture sharpen (this epic) ≠ output sharpen. |
| Local adjustments, mask evaluation, retouch | E12. E11 delivers the coordinate-space contract E12 consumes (§13). |
| Neural denoise / Super Resolution / Raw Details (AI demosaic) | v1.x per Risk 8 / §10.1 deferred list. The §4.1 raw-denoise node *slot* ships with the classical minimal fill only. |
| **X-Trans clean-room Markesteijn** | Should-tier, explicitly phased *after* Bayer ships (§10.1). v1 X-Trans renders via the interim LibRaw path (LGPL dyn-link), flagged in the UI as interim quality. No clean-room X-Trans work in this epic. |
| Guided Upright (user-drawn guides) | Could-tier; deferred. The homography solver (E5 task) is built so guides can be added later without rework. |
| Crop/transform on-canvas gizmos, Alt-key diagnostic *UI*, panel widgets | E08 shell. E11 exposes the math (level-tool angle-from-segment, constrain-crop rect, debug taps); E08 draws. |
| Calibration panel (camera-primary hue/sat sliders) | Input-transform territory — E02/E10 seam, not detail/optics. |
| HDR pipeline, soft proofing, DNG writing | E15/E16/v1.x per architecture. |
| Lens-profile *creation* tooling (target-shot LCP/lensfun authoring) | Could-tier, out of v1. User *import* of profiles is in scope. |
| Demosaic algorithm picker in the UI | Not a v1 feature (matches LR). Algorithm choice is automatic; a debug pref may exist but no recipe field. |

---

## 2. Crates & modules touched (per §2 decomposition)

| Crate | E11 additions | Notes |
|---|---|---|
| `lightbox-render` | `src/nodes/raw/{opcodes_exec.rs, rawnr.rs}`; `src/nodes/demosaic/{mod.rs, rcd.rs, draft.rs, xtrans_interim.rs, harness.rs}`; `src/nodes/detail/{sharpen.rs, nr_luma.rs, nr_chroma.rs, moire.rs}`; `src/nodes/optics/{vignette_corr.rs, ca_auto.rs, defringe.rs}`; `src/nodes/geometry/{warp_node.rs, crop.rs}`; `src/nodes/effects/{postcrop_vignette.rs, grain.rs}`; `src/warp/{field.rs, stages.rs, inscribe.rs}`; `src/upright/{lines.rs, solve.rs}`; `src/kernels/**.wgsl` | The bulk of the epic. Node impls registered under `(node_id, process_version)` per §4.5. |
| `lightbox-decode` | `src/opcodes.rs` (DNG opcode **parser**), `src/lcp.rs` (LCP XML parser), `src/lens/{meta.rs, match.rs, models.rs, resolve.rs}` (lens EXIF normalization, lensfun DB matching, correction-model resolution — CPU only, per crate charter "No GPU") | Mirrors the existing DCP-parser placement. |
| `lightbox-lensfun-sys` | **New leaf FFI crate**: bindgen bindings to liblensfun, **dynamic-link only** (LGPL-3) | Implementation detail beneath `lightbox-decode`; enters the surface-2 SBOM. |
| `lightbox-edit` | Param structs `DetailParams`, `OpticsParams`, `GeometryParams`, `EffectsParams` + defaults/ranges/validation + `crs:`/`lb:` mapping-table entries | Additive to §3.2 recipe schema; serialization + mapping machinery is E09's — we contribute fields (§13 seam). |
| `lightbox-catalog` | Migration: lens columns on `metadata_cache`; new tables `lens_default_profile`, `user_lens_profile` (§4) | Small; via the standard migration guard. |
| `lightbox-preview` (E03-owned) | Raw-cache key extension: demosaic revision + opcode digest folded into the `<params>` component of `rawcache/<hh>/<hash>.<params>.zst` | Coordinated PR into E03's crate — seam, not ownership transfer. |
| `lightbox-ingest` (E04-owned) | Probe writes the new lens metadata columns | One coordinated task (A8). |
| CI / manifests | Surface-2 SBOM entries (liblensfun dyn-link; LibRaw dyn-link already present from E02); surface-3 data manifest entry (lensfun DB, CC-BY-SA + attribution); corpus fetch scripts; demosaic quality gate job | §8 gates. |

No other crate is modified. `lightbox-shell` consumes debug taps and math helpers through the existing core query API (E08's epic).

---

## 3. Interface definitions

### 3.1 Consumed contracts (owned by neighbors; E11 assumes, does not define)

From **E02** (`lightbox-decode`) — the linearized mosaic. If E02's spec differs in field names, E11 adapts; the semantic content below is the requirement:

```rust
/// E02 output contract consumed by demosaic. Linearized (black-subtracted,
/// white-normalized to [0,1] f32 or scaled u16), bad pixels from the camera map
/// already patched (E02 owns linearize+bad-px; opcode-driven fixes are E11's).
pub struct MosaicImage {
    pub data: PlaneBuf,             // single-plane CFA samples
    pub width: u32, pub height: u32,
    pub active_area: RectU32,       // crop of valid photosites
    pub cfa: CfaDescriptor,
    pub wb_as_shot: [f32; 4],       // camera multipliers (WB node is E02/E10 territory)
    pub opcodes: RawOpcodeBlobs,    // undecoded OpcodeList1/2/3 byte blobs (E11 parses)
    pub lens_meta: LensMeta,        // normalized lens EXIF (task A8 adds extraction)
}

pub enum CfaDescriptor {
    Bayer { pattern: BayerPattern },            // RGGB | BGGR | GRBG | GBRG
    XTrans { pattern: [[CfaColor; 6]; 6] },
    Linear { channels: u8 },                    // linear DNG / already-demosaiced
    Mono,
}
```

From **E05** (`lightbox-render`) — `Engine`, `RenderNode`, `Tile`/`TileCpu`, `Roi`, `RenderScale`, node cache keyed by `hash(node_id, pv, input_hashes, params)`, 256² tiling, `Interactive` job class. E11 additionally **requires two trait extensions** (defaulted, so no other node changes) — negotiated with E05 owners in task A3:

```rust
pub trait RenderNode {
    // ... existing id / eval_gpu / eval_cpu / invalidates ...

    /// Map an output-space ROI to the input-space ROI needed to compute it.
    /// Identity for point-wise nodes; kernel-support dilation for convolutions;
    /// inverse-warp conservative bound for geometry. E05's ROI planner calls
    /// this when walking the DAG upstream (§4.3).
    fn input_roi(&self, output_roi: Roi, p: &Params) -> Roi { output_roi }

    /// Output canvas extent given input extent (geometry nodes change it: crop,
    /// constrain-to-warp, rotate). Identity default.
    fn output_extent(&self, input_extent: Extent, p: &Params) -> Extent { input_extent }

    /// Named auxiliary outputs for diagnostics (edge mask, NR delta, warp grid).
    /// The shell's Alt-key views (E08) request these via RenderRequest.
    fn debug_taps(&self) -> &'static [DebugTapId] { &[] }
}
```

From **E03** (`lightbox-preview`) — raw-cache put/get keyed by `content_hash` + params string; E11 supplies the params component (§4.3).

From **E09** (`lightbox-edit`) — recipe (de)serialization, XMP mapping registry, history integration. E11 registers field mappings; E09 owns the machinery.

### 3.2 Types E11 defines

**Recipe param structs** (live in `lightbox-edit`, consumed by nodes; all fields additive to §3.2 schema; ranges are validation bounds and UI hints):

```rust
/// §3.2 `global.detail`
pub struct DetailParams {
    pub sharpen: SharpenParams,
    pub nr: NoiseReductionParams,
    pub moire: f32,                     // 0..100, default 0 (global; LR-only-local — maps to lb:)
}
pub struct SharpenParams {
    pub amount: f32,                    // 0..150, default 40 (raw) / 0 (non-raw)
    pub radius: f32,                    // 0.5..3.0, default 1.0
    pub detail: f32,                    // 0..100, default 25 — USM↔deconvolution blend
    pub masking: f32,                   // 0..100, default 0 — edge-confinement threshold
}
pub struct NoiseReductionParams {
    pub luma: f32,                      // 0..100, default 0
    pub luma_detail: f32,               // 0..100, default 50
    pub luma_contrast: f32,             // 0..100, default 0
    pub chroma: f32,                    // 0..100, default 25 (raw) / 0 (non-raw)
    pub chroma_detail: f32,             // 0..100, default 50
    pub chroma_smoothness: f32,         // 0..100, default 50
}

/// §3.2 `global.optics`
pub struct OpticsParams {
    pub lens_profile: LensProfileChoice,
    pub distortion_scale: f32,          // 0.0..2.0 (0–200 %), default 1.0
    pub vignette_scale: f32,            // 0.0..2.0, default 1.0
    pub auto_ca: bool,                  // one-click lateral CA removal
    pub defringe: DefringeParams,
    pub manual_distortion: f32,         // -100..100 (barrel↔pincushion), default 0
    pub manual_vignette: ManualVignette // amount -100..100, midpoint 0..100
}
pub enum LensProfileChoice {
    Auto,                               // precedence: embedded opcodes > user LCP > lensfun (§3.2.3)
    None,                               // disable optional corrections (embedded mandatory opcodes still apply)
    Lensfun { lens_id: String, cam_id: String },
    Lcp { profile_hash: ContentHash },  // row in user_lens_profile
}
pub struct DefringeParams {
    pub purple_amount: f32,             // 0..20
    pub purple_hue: (f32, f32),         // hue range, default (30,70) on LR's 0..100 hue scale
    pub green_amount: f32,              // 0..20
    pub green_hue: (f32, f32),          // default (40,60)
}

/// §3.2 `geometry`
pub struct GeometryParams {
    pub crop: CropRect,                 // normalized [0,1] L/T/R/B in *post-warp* canvas
    pub angle: f32,                     // -45..45°, straighten
    pub orientation: Orientation,       // 90° steps + flips — lossless, no resample
    pub upright: UprightState,
    pub transform: TransformSliders,
    pub constrain_crop: bool,
}
pub enum UprightState {
    Off,
    /// Baked result of an explicit analysis run (§4.5 philosophy: replayed, never
    /// silently re-solved). `mode` records which button produced it.
    Applied { mode: UprightMode, h: [f64; 9], confidence: f32 },
}
pub enum UprightMode { Level, Vertical, Auto, Full }
pub struct TransformSliders {
    pub vertical: f32,    // -100..100 keystone
    pub horizontal: f32,  // -100..100
    pub rotate: f32,      // -10..10°
    pub aspect: f32,      // -100..100
    pub scale: f32,       // 50..150 %
    pub offset_x: f32,    // -100..100
    pub offset_y: f32,    // -100..100
}

/// §3.2 `effects`
pub struct EffectsParams {
    pub postcrop_vignette: PostCropVignette,
    pub grain: GrainParams,
}
pub struct PostCropVignette {
    pub style: VignetteStyle,           // HighlightPriority | ColorPriority | PaintOverlay
    pub amount: f32,                    // -100..100
    pub midpoint: f32,                  // 0..100
    pub roundness: f32,                 // -100..100
    pub feather: f32,                   // 0..100
    pub highlights: f32,                // 0..100 (HighlightPriority/ColorPriority only)
}
pub struct GrainParams { pub amount: f32, pub size: f32, pub roughness: f32 } // each 0..100
```

**DNG opcodes** (`lightbox-decode::opcodes`, executed by `lightbox-render::nodes::raw::opcodes_exec` and the warp):

```rust
pub struct OpcodeLists { pub list1: Vec<Opcode>, pub list2: Vec<Opcode>, pub list3: Vec<Opcode> }

pub enum Opcode {                       // DNG 1.4–1.7 ids
    WarpRectilinear(WarpRectilinear),   // 1: per-plane radial/tangential warp
    WarpFisheye(WarpFisheye),           // 2
    FixVignetteRadial(FixVignetteRadial), // 3
    FixBadPixelsConstant { constant: u32, bayer_phase: u32 },       // 4
    FixBadPixelsList { points: Vec<(u32,u32)>, rects: Vec<RectU32> }, // 5
    TrimBounds(RectU32),                // 6
    MapTable(MapTable),                 // 7
    MapPolynomial(MapPolynomial),       // 8
    GainMap(GainMap),                   // 9: sampled gain raster w/ plane spec
    DeltaPerRow(DeltaMap), DeltaPerColumn(DeltaMap),   // 10, 11
    ScalePerRow(ScaleMap), ScalePerColumn(ScaleMap),   // 12, 13
    Unknown { id: u32, flags: u32, data: Vec<u8> },    // honored per skip-if-unknown flag
}

pub fn parse_opcode_list(blob: &[u8]) -> Result<Vec<Opcode>, OpcodeError>;
```

Pipeline placement follows the DNG spec: **List1** applies to raw data as read (pre-linearization — coordinated with E02: E02 hands us the blobs; List1 raster ops execute in a raw-domain node E02's linearize output feeds), **List2** post-linearization pre-demosaic (mosaic domain — GainMap/vignette typically here), **List3** post-demosaic (WarpRectilinear typically here — folded into the composed geometry warp, not executed as a separate resample).

**Correction model** (`lightbox-decode::lens`):

```rust
pub struct CorrectionModel {
    pub geometry: Option<GeomDistortion>,   // evaluated inside WarpField
    pub tca: Option<TcaModel>,              // per-channel radial polynomials
    pub vignette: Option<VignetteModel>,    // radial gain fn or GainMap raster
    pub source: CorrSource,                 // provenance for UI ("built-in profile applied")
}
pub enum GeomDistortion {
    Poly3 { k1: f64 },                                  // lensfun poly3
    Poly5 { k1: f64, k2: f64 },                         // lensfun poly5
    Ptlens { a: f64, b: f64, c: f64 },                  // lensfun ptlens
    AdobeRectilinear { params: [f64; 5], center: [f64; 2] }, // LCP / WarpRectilinear model
    Fisheye { params: [f64; 2] },
}
pub enum CorrSource { EmbeddedOpcodes, UserLcp(ContentHash), Lensfun { lens: String }, Manual }

pub struct LensMeta {           // normalized from EXIF/MakerNotes by E02 probe (task A8)
    pub make: Option<String>, pub model: Option<String>,
    pub focal_mm: Option<f32>, pub aperture: Option<f32>, pub focus_dist_m: Option<f32>,
    pub crop_factor: Option<f32>,
}

/// Resolution precedence (Auto): embedded opcodes → user LCP (matched by lens) →
/// lensfun DB match (score-thresholded; ambiguous ⇒ no auto-apply, surfaced in UI)
/// → none. Explicit user choice overrides. Per-lens saved defaults consulted last.
pub fn resolve_lens_correction(
    meta: &LensMeta, choice: &LensProfileChoice, opcodes: &OpcodeLists,
    db: &LensDb, catalog_defaults: &LensDefaults,
) -> Result<CorrectionModel>;

/// lensfun DB handle: FFI (dyn-link, LGPL-3) used for *database load + matching only*;
/// model math is evaluated by our own code (GPU/CPU) from the documented model formulas.
pub struct LensDb { /* immutable snapshot, thread-safe by construction */ }
impl LensDb {
    pub fn load(paths: &[PathBuf]) -> Result<LensDb>;
    pub fn match_lens(&self, meta: &LensMeta) -> Vec<LensMatch>;   // scored candidates
    pub fn interpolate(&self, m: &LensMatch, focal: f32, ap: f32, dist: f32)
        -> Option<CorrectionModel>;
}
```

**Unified warp** (`lightbox-render::warp`) — the central quality decision: all geometric operations compose into **one** backward-mapped resample (Lanczos-3, per-channel offsets for TCA, anti-ringing clamp). Cascaded resampling is forbidden.

```rust
pub struct WarpField { stages: SmallVec<[WarpStage; 6]>, dst_extent: Extent, src_extent: Extent }
pub enum WarpStage {
    OpcodeWarp(WarpRectilinear),        // or fisheye
    LensGeom(GeomDistortion),           // scaled by distortion_scale
    Tca(TcaModel),                      // per-channel
    Homography([f64; 9]),               // upright ∘ transform sliders ∘ straighten angle
    ManualDistortion { k1: f64 },
}
impl WarpField {
    pub fn build(g: &GeometryParams, o: &OpticsParams, cm: &CorrectionModel,
                 src: Extent) -> WarpField;
    pub fn map_dst_to_src(&self, p: DVec2, ch: Channel) -> DVec2;   // exact, f64 on CPU
    pub fn map_roi_dst_to_src(&self, roi: Roi) -> Roi;              // conservative bound
    pub fn warped_src_polygon(&self) -> [DVec2; 4];                 // for constrain-crop
    pub fn is_identity(&self) -> bool;                              // node elides itself
    /// GPU form: per-tile grid of sample coordinates + per-channel deltas,
    /// interpolated in-kernel (dense analytic eval for demanding stages).
    pub fn to_gpu_uniform(&self, tile: Roi, scale: RenderScale) -> WarpGpuData;
}

/// Largest axis-aligned rectangle (optional locked aspect) inscribed in the
/// warped valid-pixel polygon. Used by constrain_crop and the crop UI (E08).
pub fn largest_inscribed_rect(poly: &[DVec2; 4], aspect: Option<f64>) -> RectF64;
```

**Upright** (`lightbox-render::upright`) — analysis runs as a Background job, **on CPU at a fixed analysis resolution (long edge 2048)** so the result is deterministic; the solved homography is baked into `UprightState::Applied` and replayed (never silently re-solved):

```rust
pub struct LineSegments { pub segs: Vec<Segment>, pub image_size: (u32, u32) }
/// Own implementation: Sobel gradient magnitude + progressive probabilistic Hough.
/// Deliberately NOT LSD (original LSD code is AGPL) and NOT opencv_contrib.
pub fn detect_segments(gray: &PlaneF32, opts: &DetectOpts) -> LineSegments;

pub struct UprightSolution { pub h: [f64; 9], pub confidence: f32, pub lines_used: u32 }
/// Vanishing-point clustering (RANSAC) → per-mode constrained homography
/// (Level: rotation only; Vertical: rotation+vertical keystone; Full: both+shear;
/// Auto: damped blend toward natural). Focal prior from EXIF when present.
pub fn solve_upright(lines: &LineSegments, mode: UprightMode,
                     focal_prior: Option<f64>) -> Option<UprightSolution>;

/// Level-tool + auto-straighten helper for the shell (E08 draws, we compute).
pub fn angle_from_segment(a: DVec2, b: DVec2) -> f32;
pub fn auto_level_angle(lines: &LineSegments) -> Option<f32>;
```

**Node inventory** (all implement `RenderNode`; ids stable, registered under PV1):

| NodeId | Stage (§4.1) | Params | Notes |
|---|---|---|---|
| `raw.opcodes1` | post-decode, pre-linearize seam w/ E02 | — (opcode-driven) | FixBadPixels*, MapTable/Poly on raw values |
| `raw.opcodes2` | post-linearize, pre-demosaic | vignette_scale | GainMap / FixVignetteRadial on mosaic |
| `raw.nr` | pre-demosaic | (internal, auto) | hot-pixel/impulse + green equilibration; the §4.1 classical raw-denoise slot |
| `raw.demosaic` | demosaic | quality: Draft\|Full (engine-chosen, not recipe) | RCD (Bayer), LibRaw interim (X-Trans), passthrough (Linear/Mono) |
| `detail.sharpen` | detail | `SharpenParams` | luminance-domain USM + RL-deconv blend; taps: edge_mask, effect_gray |
| `detail.nr_luma` | detail | NR luma triplet | à-trous wavelet shrinkage |
| `detail.nr_chroma` | detail | NR chroma triplet | opponent-space guided/bilateral + median speckle |
| `detail.moire` | detail | moire | chroma pattern suppression |
| `optics.vignette` | optics (gain, pre-warp) | vignette_scale, manual_vignette | lensfun model post-demosaic; GainMap handled by `raw.opcodes2` |
| `optics.defringe` | optics | `DefringeParams` | hue-range desaturation near high-contrast edges |
| `geom.warp` | optics/geometry | Optics+Geometry (via `WarpField`) | THE single resample; includes auto-CA per-channel offsets; elides when identity |
| `geom.crop` | geometry | crop, orientation | logical: canvas extent + orientation metadata, no resample |
| `fx.vignette` | effects | `PostCropVignette` | superellipse falloff, 3 blend styles, crop-centered |
| `fx.grain` | effects | `GrainParams` | seeded `xxh3(image_id ‖ "grain")`; band-limited octave noise; resolution-consistent |

Auto-CA analysis (for `auto_ca: true`) runs at render time as a cached deterministic CPU analysis (fixed analysis resolution, tile-based per-channel radial misalignment fit → low-order polynomial `TcaModel`), cache-keyed by upstream content — unlike Upright it need not be recipe-baked because it is deterministic and cheap; the fitted model feeds the same `Tca` warp stage.

---

## 4. Data model & migrations

### 4.1 Catalog migration (SQL)

One migration (number assigned by the migration sequence at merge time), additive only:

```sql
-- E11: lens metadata for optics auto-match + filter facets
ALTER TABLE metadata_cache ADD COLUMN lens_make TEXT;
ALTER TABLE metadata_cache ADD COLUMN lens_model TEXT;
ALTER TABLE metadata_cache ADD COLUMN lens_key TEXT;          -- normalized "make|model" match key
ALTER TABLE metadata_cache ADD COLUMN focal_length_mm REAL;
ALTER TABLE metadata_cache ADD COLUMN aperture_f REAL;
ALTER TABLE metadata_cache ADD COLUMN focus_distance_m REAL;  -- NULL when EXIF lacks it
CREATE INDEX idx_metadata_cache_lens_key ON metadata_cache(lens_key);

-- "Save as default for this lens": per-lens correction preference
CREATE TABLE lens_default_profile (
  lens_key         TEXT PRIMARY KEY,
  source           TEXT NOT NULL CHECK (source IN ('auto','none','lensfun','lcp')),
  profile_ref      TEXT,               -- lensfun lens id or user_lens_profile.hash
  distortion_scale REAL NOT NULL DEFAULT 1.0,
  vignette_scale   REAL NOT NULL DEFAULT 1.0,
  updated_at       INTEGER NOT NULL
) STRICT;

-- User-imported lens profiles (LCP / lensfun XML), stored as files in the app
-- data dir, indexed here; content-hash keyed like other stores
CREATE TABLE user_lens_profile (
  hash        TEXT PRIMARY KEY,        -- xxh3-128 of file content
  kind        TEXT NOT NULL CHECK (kind IN ('lcp','lensfun-xml')),
  path        TEXT NOT NULL,
  lens_key    TEXT,                    -- extracted match key (nullable if unparsed)
  imported_at INTEGER NOT NULL
) STRICT;
CREATE INDEX idx_user_lens_profile_lens_key ON user_lens_profile(lens_key);
```

Backfill of the new `metadata_cache` columns happens lazily (on next metadata read / background maintenance job), not as a blocking migration step — 100k-asset catalogs must migrate in O(schema) time.

### 4.2 Recipe schema additions (CBOR, via E09)

Additive fields under the existing `schema: 2` envelope (E09 owns whether this is a minor bump; fields unknown to older builds ride `xmp_passthrough`/ignore-unknown per §3.2 forward-compat contract): the four param structs of §3.2 above, serialized under `global.detail`, `global.optics`, `geometry`, `effects` exactly as the architecture's recipe sketch names them. `UprightState::Applied.h` stores 9×f64 — trivial size. No mask/retouch/table content changes.

**`crs:` mapping entries registered with E09** (representative; full table is deliverable of task A2): `Sharpness`, `SharpenRadius`, `SharpenDetail`, `SharpenEdgeMasking`; `LuminanceSmoothing`, `LuminanceNoiseReductionDetail`, `LuminanceNoiseReductionContrast`; `ColorNoiseReduction`, `ColorNoiseReductionDetail`, `ColorNoiseReductionSmoothness`; `LensProfileEnable`, `LensProfileName`, `LensProfileDistortionScale`, `LensProfileVignettingScale`; `AutoLateralCA`; `DefringePurpleAmount/PurpleHueLo/PurpleHueHi/GreenAmount/GreenHueLo/GreenHueHi`; `LensManualDistortionAmount`, `VignetteAmount`, `VignetteMidpoint`; `CropLeft/Top/Bottom/Right/Angle`, `CropConstrainToWarp`, `tiff:Orientation`; `PerspectiveUpright`, `PerspectiveVertical/Horizontal/Rotate/Aspect/Scale/X/Y`; `PostCropVignetteAmount/Midpoint/Roundness/Feather/Style/HighlightContrast`; `GrainAmount/GrainSize/GrainFrequency`. Import from LR is **approximate** (different engines; Risk 9 expectation-setting); our own fields round-trip losslessly. Global moiré and the baked Upright homography have no `crs:` equivalent → `lb:` namespace.

### 4.3 Raw-cache key (E03 seam)

The `<params>` component of `rawcache/<hh>/<hash>.<params>.zst` becomes:

```
pv{process_version}-dm{demosaic_algo_id}.{demosaic_impl_rev}-op{xxh3(opcode_list1‖list2 bytes)}-rnr{raw_nr_rev}
```

Cached state = post-`raw.nr`, post-demosaic working-space-input planes (pre-WB, per §4.1 — WB and everything downstream are cheap re-runnable nodes). Any demosaic algorithm revision bumps `demosaic_impl_rev` and naturally invalidates (dev-time only; post-v1.0 a revision means a new PV per §4.5).

### 4.4 Shipped data & manifests (§8 surfaces)

| Item | Surface | Entry |
|---|---|---|
| liblensfun (dyn-lib, LGPL-3) | 2 (binary SBOM) | link mode = dynamic, relink capability documented |
| lensfun database | 3 (data manifest) | CC-BY-SA, bundled **with attribution**, share-alike on data honored; update channel via data packs (E16 packaging seam) |
| LibRaw (dyn-lib, LGPL-2.1) | 2 | already present from E02; E11 adds the in-process X-Trans interim demosaic usage note |
| Blue-noise texture tiles (grain) | 3 | generated in-house (own tool, seeded) — project license |
| Demosaic/optics test corpora | not shipped | repo/LFS + fetch scripts; Kodak/McMaster sets used in CI only, not redistributed in the app |
| **Adobe LCP library** | 3 | **NEVER bundled** (constraint 4 — Adobe-authored asset; user-supplied only). Gate asserts zero `.lcp` files in the shipped asset tree. |

---

## 5. Pipeline placement (mapping to §4.1 — no reordering)

```
E02: decode ─► linearize ─► [E11 raw.opcodes1*] ─► [E11 raw.nr] ─► [E11 raw.opcodes2]
  ─► [E11 raw.demosaic] ─► E02/E10: WB ─► input transform ─► E10: global tone/color
  ─► [E11 detail.sharpen ► detail.nr_luma ► detail.nr_chroma ► detail.moire]
  ─► [E11 optics.vignette ► geom.warp (lens geom ∘ TCA(auto-CA) ∘ upright ∘ transform ∘ angle)
       ► optics.defringe ► geom.crop]
  ─► E12: LOCAL masks ─► E12: retouch ─► [E11 fx.vignette ► fx.grain] ─► E02: output transform
```

\* `raw.opcodes1` placement straddles E02's linearize; execution order per DNG spec is settled with E02 in task D2 (List1 ops that the camera intends pre-linearization run inside E02's linearize node as a delegated call, or E02 hands us pre-linearized data and we run List1 first — decided by whichever E02 shipped; the parser and raster-op kernels are E11's either way).

Defringe runs post-warp (channel-aligned edges); the warp is elided entirely (`is_identity`) when no optics/geometry corrections apply — zero-cost for untouched images. `geom.crop` changes canvas extent only. Masks (E12) are evaluated in **post-crop canvas coordinates**; `geom.warp`+`geom.crop` publish the composed dst→src transform through the engine so E12 gizmos and E15 can map coordinates (§13).

---

## 6. Algorithm commitments & clean-room protocol

### 6.1 Clean-room protocol (Risk 2 / mandate constraint 4 hygiene)

The state-of-the-art demosaicers (AMaZE, RCD-as-implemented, Markesteijn) exist as GPL-3 source in RawTherapee/darktable. The architecture's rule: **reimplement from published algorithm descriptions; GPL source is study-only, never copied.** This epic tightens that into a written protocol (task B1, CTO-acknowledged):

1. **Primary sources are publications:** RCD — Luis Sanz Rodríguez's published algorithm description ("Ratio Corrected Demosaicking"); LMMSE — Zhang & Wu 2005; AHD — Hirakawa & Parks 2005; plus standard texts for USM/RL-deconvolution, à-trous wavelets (Starck et al.), NLM (Buades 2005), guided filter (He et al. 2010), Hough transform, Brown–Conrady. Implementers work from these + an internal algorithm spec document.
2. **GPL source consultation is firewalled:** if behavior study of RawTherapee/darktable is needed (e.g., to understand a quality difference), it is done by a designated reader who produces **prose behavior notes (no code, no pseudocode transcription)**; implementers never have GPL demosaic/NR source open while writing ours. Notes are archived in the provenance dossier.
3. **AMaZE has no formal paper.** Therefore "AMaZE-class" is a **quality target, not an algorithm to port**: the committed plan is RCD as primary (published description exists), with LMMSE (published) as the high-ISO/quality-tier candidate if the spike shows RCD alone can't beat the floor everywhere. Porting AMaZE is not an option under the protocol.
4. **Provenance dossier** (deliverable): per-algorithm source list, spike notes, reader notes, and a contributor attestation. This is the artifact the CTO/license review audits.

The same protocol covers detail/optics algorithms (all have permissive-safe published sources; the demosaic is the only genuinely contested territory). lensfun's *model formulas* are taken from its published documentation; its *code* is used only through the dyn-linked FFI for DB parse/matching.

### 6.2 Committed algorithm choices (reversal = node-local swap per §1.3)

| Node | Algorithm | Source basis |
|---|---|---|
| `raw.demosaic` Full | **RCD** (directional gradients → green interpolation → ratio-corrected chroma → refinement); optional LMMSE tier per spike outcome (B5 gate) | published descriptions (§6.1) |
| `raw.demosaic` Draft | superpixel/half (2×2 → 1 RGB px) for progressive first paint only | trivial/own |
| `raw.nr` | median-based impulse/hot-pixel detect + green channel equilibration; conservative, auto, no UI slider in v1 | own / standard |
| `detail.sharpen` | luminance-domain unsharp mask blended with N-iteration Richardson–Lucy deconvolution (Detail slider = blend + RL weight); Sobel-magnitude edge mask, smoothed, thresholded by Masking | textbook |
| `detail.nr_luma` | à-trous (stationary) wavelet decomposition, per-band adaptive shrinkage; Detail = threshold curve, Contrast = low-band preservation | Starck et al.; own tuning |
| `detail.nr_chroma` | opponent-space (Y/Cb/Cr-like in working space) chroma planes: 3×3/5×5 median speckle pre-pass + guided-filter smoothing at reduced resolution; Smoothness = guide radius/blend | He et al. 2010 (shared kernel infra with E10.2) |
| `detail.moire` | local chroma high-frequency pattern detection → directional chroma desaturation/averaging | own, standard practice |
| `optics` geometry/TCA/vignette | model evaluation per §3.2 `GeomDistortion`/`TcaModel`/`VignetteModel`; auto-CA = tile-wise per-channel radial displacement estimation + robust polynomial fit | lensfun docs, DNG spec, published CA-correction literature |
| `geom.warp` resample | single-pass inverse mapping, Lanczos-3, per-channel coords, anti-ringing (clamp to local min/max of 2×2 neighborhood blend) | standard |
| upright | Sobel + progressive probabilistic Hough (own; **no LSD — AGPL**; no opencv_contrib) → RANSAC vanishing points → constrained homography via small Gauss–Newton (own; no ceres) | published methods |
| `fx.vignette` | superellipse falloff mask; HighlightPriority = gain in scene-linear pre-output (allows recovery), ColorPriority = luminance-only application preserving hue, PaintOverlay = flat blend toward black/white in output space | own, matching documented LR semantics |
| `fx.grain` | precomputed blue-noise octave tiles, band-limited mix by Size/Roughness, applied in full-res image coordinate space with scale-aware band-limiting so preview ≈ export | own |

Precision rule: mosaic-domain and demosaic interior compute in **f32** (RGBA16F tiles are the §4.2 default but demosaic gradients/ratios need f32 — the tile format supports per-node f32 scratch; output tiles may downcast to f16 after the input transform). This is within §4.2's "f32 only where a stage needs it".

---

## 7. Ordered task breakdown (each ≤ 1 day)

Phases: **A** seams/scaffolding → **B** demosaic (E11.1) → **C** detail (E11.2) → **D** optics (E11.3) → **E** geometry/effects (E11.4) → **F** integration/hardening. B and C–E can run as parallel lanes after A when two engineers are staffed. Within a phase, order is dependency order.

### Phase A — Seams & scaffolding (8 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | Define `DetailParams`/`OpticsParams`/`GeometryParams`/`EffectsParams` (+ defaults, ranges, validation) in `lightbox-edit`; CBOR round-trip | Serde/CBOR round-trip property test green; out-of-range values clamp+warn; defaults match §3.2 tables |
| A2 | Register `crs:`/`lb:` mapping entries for all E11 fields with E09's mapping layer; document full mapping table | E09 property tests (serialize→XMP→parse identity for our fields) green; approximate-import semantics documented per field |
| A3 | Upstream `input_roi`/`output_extent`/`debug_taps` defaulted extensions to `RenderNode` (PR into E05 core with E05 owners) | Merged; all existing E05/E10 nodes compile unchanged; engine ROI planner calls `input_roi` (probe test proves a 5×5-kernel stub dilates its ROI) |
| A4 | `WarpField`: stage composition, `map_dst_to_src`, `map_roi_dst_to_src`, `warped_src_polygon`, `is_identity` (CPU, f64) | Unit: homography round-trip < 1e-9 px; property: mapped ROI of any dst point's source always inside `map_roi_dst_to_src` bound |
| A5 | Remosaic quality harness: reference-RGB → simulated Bayer → demosaic → metrics (CPSNR, false-color = chroma error in smooth-hue regions, zipper = along-edge alternation energy); corpus fetch script (Kodak + McMaster + own raw shots list) | Harness runs end-to-end with a bilinear stub; metrics reproducible bit-exact across runs; corpus pinned by hash |
| A6 | Interim-floor baseline: run LibRaw AHD/DCB/PPG through harness; commit baseline metrics JSON; wire `demosaic_quality_gate` (fails until clean-room lands) — **the epic's first failing test** | Baseline JSON committed; gate job red in CI with a clear "clean-room not yet implemented" message |
| A7 | Debug-tap plumbing: `RenderRequest` gains optional tap selection; `Engine::poll` returns tap textures alongside main output (E05 seam) | A stub node's tap renders to a texture the CLI can dump to PNG; zero overhead when no tap requested (bench diff < 1 %) |
| A8 | Catalog migration §4.1 + lens EXIF normalization in probe (`LensMeta`; coordinated with E02/E04 owners); lazy backfill job | Migration runs on a 100k-asset fixture < 1 s; probe fills `lens_key/focal/aperture` for corpus raws; `kill -9` mid-migration leaves catalog `integrity_check`-clean |

### Phase B — E11.1 clean-room demosaic (19 tasks; ~6–10 pw with spike slack)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | Clean-room protocol doc + provenance dossier skeleton; collect primary sources (§6.1); CTO acknowledgment requested | Protocol merged to `docs/plan/epics/`; dossier template in repo; source PDFs/links archived |
| B2 | Internal RCD algorithm spec (math from the published description, our notation, tile/border strategy) — reviewed by second engineer | Spec doc reviewed; every step traceable to a cited publication, none to GPL source |
| B3 | Spike: CPU RCD step 1 — directional gradient/discrimination + green interpolation (scalar f32, no perf work) | On Kodak subset, green-channel PSNR ≥ bilinear +3 dB; harness plots committed |
| B4 | Spike: CPU RCD step 2 — ratio-corrected chroma interpolation + refinement pass | Full-RGB harness run completes; visual dump shows no gross artifacts on test chart |
| B5 | **Spike gate:** full-corpus metrics vs A6 floor; decide RCD-only vs RCD+LMMSE tier; record decision + evidence in dossier | Written gate decision; if RCD < floor on any metric class, LMMSE tier tasks (B5a/B5b, mirror of B3/B4) are activated before proceeding |
| B6 | Border handling: 2-px CFA margins, non-square `active_area`, all four Bayer phases | Unit tests per phase; no NaN/garbage in a 1-px-inset diff test; odd-dimension images correct |
| B7 | Production CPU impl: tiled (rayon), `std::simd`/`wide` inner loops; bit-exact vs spike prototype | Prototype-parity test exact; ≥ 4× scalar throughput on 8-core reference |
| B8 | WGSL kernel pass 1 (gradients + green), 256² tiles + apron | GPU/CPU green-plane diff within f32 tolerance (≤ 1e-5 rel) on corpus tiles |
| B9 | WGSL kernel pass 2 (chroma ratio correction) | Full GPU output vs CPU within ΔE2000 ≤ 0.3 on corpus (intra-node tolerance tighter than pipeline gate) |
| B10 | WGSL pass 3 (refinement) + tile-apron correctness | Seam test: tiled render == untiled render bit-comparable per §8 parity method; no visible tile seams in gradient sweep image |
| B11 | Demosaic node integration: `raw.demosaic` `RenderNode` registered under PV1; f32 scratch tiles; cache key wiring | Recompute-count probe: any downstream slider change re-runs **zero** demosaic work; engine golden test with demosaic+display transform passes |
| B12 | Draft demosaic (superpixel/half) + progressive policy: draft for first paint at fit-view only, never 1:1/export | E2E: develop-open shows draft ≤ 100 ms (from decoded mosaic), full RCD replaces it; export path provably never selects Draft (assert in export test) |
| B13 | Raw-cache integration (E03 seam): params key per §4.3; put/get of post-demosaic state | Second develop-open renders full-res from cache < 1 s (§7 budget); key changes on `demosaic_impl_rev` bump invalidate |
| B14 | Input routing: X-Trans → interim LibRaw path (dyn-link, in-process, marked interim in UI string resource); Linear DNG / Mono → passthrough | Corpus of RAF/linear-DNG/mono renders end-to-end; routing unit tests; SBOM note updated |
| B15 | `raw.nr` node: hot-pixel/impulse suppression (median-deviation detect) + green equilibration; auto-on for raws, conservative thresholds | Synthetic hot-pixel fixture fully cleaned with < 0.1 % false positives on flat-field corpus; metrics harness shows no CPSNR loss on clean images |
| B16 | Wire `demosaic_quality_gate` green: full corpus, beat-floor assertions (mean CPSNR ≥ floor + 0.3 dB; no per-image regression > 0.5 dB; false-color and zipper ≤ floor) | A6's red gate is green in CI; gate promoted to nightly + release-blocking |
| B17 | Perf: 45 MP Bayer full RCD ≤ 60 ms on RTX 3060-class / M-series base (subgroup ops where available, guarded) | criterion + GPU-timestamp bench committed; budget met on both reference machines; CPU path ≤ 2 s for same frame (background-tier per §4.4) |
| B18 | Perceptual review #1 with imaging lead: real-raw corpus (fabric, foliage, fine text, high-ISO night) vs interim floor; punch list filed | Review notes in dossier; punch list triaged into B19 |
| B19 | Punch-list fixes + perceptual review #2 → demosaic sign-off | All punch-list items closed or explicitly waived by imaging lead; sign-off recorded in dossier |

### Phase C — E11.2 detail stage (13 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | Sharpen algorithm spec (USM+RL blend, edge-mask semantics, luminance domain definition) + defaults table (raw vs non-raw) | Spec reviewed; Alt-view tap outputs defined (edge_mask, effect_gray) |
| C2 | Separable Gaussian blur kernel (CPU+WGSL), radius-parameterized — shared kernel util (check reuse from E05/E10 first) | Blur vs analytic Gaussian reference ≤ 1e-4 rel error; reused by ≥ 2 later nodes |
| C3 | `detail.sharpen` CPU: USM path + Masking edge mask | Golden vs committed reference; masking=100 leaves flat-synthetic regions unchanged (Δ < 1e-5); amount=0 is exact identity (node elides) |
| C4 | `detail.sharpen` RL-deconvolution path + Detail blend; WGSL both paths | GPU/CPU parity ΔE2000 ≤ 1.0; RL 10-iter on preview ROI ≤ 8 ms GPU |
| C5 | Debug taps for sharpen (edge mask, grayscale effect) via A7 plumbing | CLI dumps match expected diagnostics on test chart; E08 consumption documented |
| C6 | À-trous wavelet decomposition/reconstruction (CPU+WGSL, 5 levels) | Perfect-reconstruction property test (decompose→reconstruct ≤ 1e-5); level responses match reference filters |
| C7 | `detail.nr_luma`: per-band shrinkage + Detail/Contrast semantics | High-ISO fixture: noise σ reduction ≥ 50 % at luma=50 with edge-preservation metric (gradient correlation ≥ 0.9); golden committed; parity ≤ ΔE 1.0 |
| C8 | `detail.nr_chroma`: median speckle pre-pass + reduced-res guided smoothing + Smoothness control | Chroma-noise fixture: color speckle removed (chroma σ ≤ 25 % of input) with no edge bleed > 1 px on chart; parity green |
| C9 | `detail.moire` node | Synthetic moiré chart: colored banding suppressed at moire=50, no desaturation of legitimate color detail beyond ΔE 2 outside pattern regions |
| C10 | ISO-adaptive defaults resolver (table-driven; neutral v1 table: chroma NR 25 raw baseline, luma 0; hook reads ISO from `metadata_cache`) | New raw at ISO 6400 opens with table defaults; defaults application is a recipe initialization, never a hidden render-time modifier |
| C11 | Detail-stage combined perf: sharpen+NR at fit-view preview ROI ≤ 25 ms GPU on reference | Scenario bench committed; §7 slider p95 budget still met with detail stage active |
| C12 | High-ISO golden set + perceptual NR review with imaging lead; punch list | Review recorded; punch list triaged |
| C13 | NR/sharpen punch-list fixes → detail sign-off | Punch list closed/waived; goldens re-committed with review approval |

### Phase D — E11.3 optics (14 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | DNG opcode binary parser (all §3.2 opcodes; unknown-skip per flag semantics) | Unit tests on hand-crafted blobs (every opcode + unknown-optional + unknown-mandatory error); parses opcode lists from real corpus DNGs |
| D2 | Raw-domain opcode raster ops (`raw.opcodes1`/`raw.opcodes2` nodes): GainMap, FixVignetteRadial, FixBadPixels*, MapTable/MapPolynomial, Delta/Scale rows/cols; List1 placement settled with E02 | Spec-derived fixtures render correctly (synthetic DNG with known GainMap → flat field); parity CPU/GPU; a GainMap-bearing real DNG shows corners lifted per map |
| D3 | WarpRectilinear/WarpFisheye → `WarpStage::OpcodeWarp` conversion + coefficient math | Round-trip unit test vs spec formulas; synthetic grid DNG straightens (line straightness residual < 0.5 px) |
| D4 | `lightbox-lensfun-sys`: bindgen bindings, dyn-link build on macOS/Windows/Linux CI; SBOM surface-2 entry | 3-OS CI green; `ldd`/`otool` assertion: dynamic link; cargo-deny green |
| D5 | `LensDb` safe wrapper: DB load → immutable snapshot; `match_lens` scoring + ambiguity threshold; interpolation across focal/aperture/distance | Pinned mini-DB tests: exact match, ambiguous match (returns no auto-apply), crop-factor mismatch rejection; thread-safety test (concurrent reads) |
| D6 | Own evaluators for lensfun models (poly3/poly5/ptlens geometry; linear/poly3 TCA; `pa` vignette) from documented formulas | For 3 sample lenses, our correction displacement field matches liblensfun's modifier output within 0.1 px / gain within 0.5 % |
| D7 | `optics.vignette` node (lensfun model, post-demosaic gain) + 0–200 % `vignette_scale` + manual vignette sliders | Flat-field fixture with known falloff corrects to ≤ 1 % residual at scale=1.0; scale=0 exact identity; parity green |
| D8 | LCP XML parser (rectilinear, fisheye, vignette, TCA models; XMP-based format) for **user-supplied** files; `user_lens_profile` import flow | Hand-built spec-conformant LCP fixtures parse; malformed files rejected with actionable error; import registers row + file copy; **no `.lcp` in shipped assets** gate added to surface-3 checker |
| D9 | `resolve_lens_correction` precedence + `lens_default_profile` persistence + "built-in profile applied" provenance surfacing | Precedence unit-tested (opcodes > user LCP > lensfun > manual; explicit choice wins; embedded mandatory corrections survive `None`); saved default round-trips through catalog |
| D10 | Auto lateral CA analysis: tile-wise per-channel radial misalignment estimation + robust poly fit → `TcaModel` (deterministic CPU at fixed analysis res, node-cache keyed) | On CA-heavy corpus, fitted model reduces mean edge R/B misalignment ≥ 70 %; determinism test: identical model across 10 runs and across platforms |
| D11 | TCA into `geom.warp` per-channel sampling; golden on CA corpus | Fringe-width metric (chroma edge width) ≤ 40 % of uncorrected on corpus; no luminance resolution loss > 0.2 dB CPSNR on neutral chart |
| D12 | `optics.defringe` node + hue-range semantics + eyedropper sampling API (`sample_fringe(roi) -> DefringeParams` for E08) | Purple-fringe fixture: fringe ΔE to background ≤ 3 at amount=10; protected skin-tone patch shifts < ΔE 1; parity green |
| D13 | Manual distortion slider (k1 radial) into warp; identity elision verified | slider=0 ⇒ `is_identity` (recompute probe shows warp node elided); ±100 produces documented curvature on grid chart |
| D14 | Optics integration goldens (3 real lenses with known profiles: wide barrel, tele pincushion, fisheye) + review | Corrected outputs match committed goldens (ΔE gate); straight-line residual < 1 px at 1:1 on architecture shots; review sign-off |

### Phase E — E11.4 geometry & effects (12 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | `geom.warp` CPU: single-pass inverse-map Lanczos-3 resample over composed `WarpField`, per-channel coords, anti-ringing clamp | Identity warp is bit-exact passthrough (and elided); 4×90° rotation compose ≈ identity within 1e-4; resample vs reference Lanczos ≤ 1e-4 |
| E2 | `geom.warp` WGSL: grid-interpolated warp data per tile (§3.2 `to_gpu_uniform`), Lanczos-3 in-kernel | GPU/CPU parity ΔE2000 ≤ 1.0 on warp corpus; preview-ROI warp ≤ 10 ms on reference GPU |
| E3 | ROI back-mapping through warp integrated with engine tiling (uses A3 `input_roi`) | 1:1 zoom into corner of heavily-warped image evaluates only needed source tiles (recompute probe ≤ 1.5× ideal tile count); no seams |
| E4 | `geom.crop` node: normalized crop+angle into warp homography; orientation fast path (90°/flip = metadata, no resample); `edit_index.crop_ratio` update | Crop of un-warped image = pure canvas change (no resample in trace); orientation change re-renders < 50 ms at fit view; edit_index updated in same txn (E09 write path) |
| E5 | Line-segment detection: Sobel + progressive probabilistic Hough (own), fixed 2048-long-edge analysis res | Synthetic scenes: ≥ 95 % of ground-truth segments ≥ 40 px recovered within 1°; deterministic across runs/platforms; ≤ 150 ms CPU |
| E6 | Vanishing-point RANSAC + constrained homography solve per mode (Level/Vertical/Full/Auto damping); focal prior from EXIF | Synthetic keystoned renders (known H): recovered verticals within 0.3° for Vertical/Full; Level matches known horizon within 0.2°; Auto ≤ Full correction (damping property test) |
| E7 | Upright command flow: Background analysis job → `UprightState::Applied{h, …}` baked into recipe as an undoable edit; explicit re-run command; auto-level + level-tool helpers exposed | Upright apply is one history step; re-render replays baked H with **zero** re-analysis (probe); catalog/XMP round-trips the matrix via `lb:`; auto-level API returns angle on horizon fixture |
| E8 | Manual `TransformSliders` → homography compose (+ interaction with baked upright) | All-zeros ⇒ identity (property test); each slider matches documented direction on grid fixture; combined with Upright composes (not replaces) |
| E9 | Constrain-crop: `largest_inscribed_rect` (free + locked aspect) + auto-apply on warp change when enabled | Property test: rect always inside warped polygon (10k random homographies); area within 1 % of brute-force reference; locked-aspect variant preserves ratio exactly |
| E10 | `fx.vignette` node: superellipse falloff + 3 styles + crop re-centering | Style goldens committed (each style visually distinct per documented semantics); changing crop re-centers (golden pair); amount=0 elides; parity green |
| E11 | `fx.grain` node: blue-noise octaves, deterministic seed, resolution-consistent application | Same recipe renders identical grain across runs/backends (hash-equal CPU, ΔE-tolerance GPU); preview vs export at matched scale within ΔE2000 ≤ 1.0; Size/Roughness monotonic effect on power spectrum |
| E12 | Geometry/effects perf pass: full E11 pipeline active, slider p95 < 100 ms at fit view (§7) on reference | Scenario bench: WB→grain full graph, worst-case slider (sharpen amount) p95 < 100 ms; warp+crop+vignette+grain combined ≤ 20 ms preview ROI |

### Phase F — Integration & hardening (7 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | Full-pipeline golden suite: raw corpus × recipe set exercising every E11 node, committed per PV1 (§8 immutability guard) | Goldens committed; CI compares ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; suite runs in PR-blocking fast subset (3 images) + nightly full |
| F2 | CPU-fallback E2E: injected device-lost mid-develop with E11 nodes active → preview-res CPU editing continues (§4.4 contract) | E05's device-lost harness passes with demosaic/NR/warp in graph; full-res CPU render completes as background job with progress |
| F3 | Export-path integration (with E15/CLI): 45 MP full-res render through all E11 nodes within bounded VRAM (tiling); backend recorded | Headless CLI export golden green; VRAM peak ≤ budget on 8 GB reference; export uses Full demosaic (never Draft) — asserted |
| F4 | XMP interop E2E with E09: LR-authored `crs:` detail/optics/geometry sidecars import approximately (documented tolerances); our fields round-trip losslessly | Fixture set of LR sidecars imports without error; round-trip property tests green; approximate-import doc reviewed (Risk 9 expectation) |
| F5 | License/CI closeout: surface-2 SBOM (lensfun dyn, LibRaw usage note), surface-3 manifest (lensfun DB attribution, blue-noise tiles, zero-`.lcp` gate), cargo-deny, AGPL-LSD absence assertion | All three §8 surfaces green including new entries; release-runner binary inspection passes |
| F6 | Docs & handoff: node spec sheets (params, taps, budgets), coordinate-space contract note to E12/E15, clean-room dossier finalized | Docs merged; E12/E15 planners sign off on the seam note; dossier complete with attestations |
| F7 | M2 exit drill for E11 scope: import → develop with detail/optics/geometry edits → export, on all 3 CI platforms | Scripted scenario green on macOS/Windows/Linux CI; perf budgets from B17/C11/E12 re-asserted on reference hardware |

**Total: 73 tasks.** At ≤ 1 day each plus spike/review slack this lands inside the 12–18 pw envelope (Lane 1: A+B ≈ 27 tasks + spike slack ≈ 6–8 pw; Lane 2: C+D+E ≈ 39 tasks ≈ 8–9 pw; F shared).

---

## 8. Test plan (per §8 strategy)

| Layer | Tests (owner tasks) | Gate |
|---|---|---|
| **Unit** | Opcode parser (crafted blobs, unknown-flag semantics — D1); LCP parser (D8); lensfun matching/ambiguity (D5); model evaluators vs liblensfun reference (D6); homography/warp math (A4, E6, E8); inscribed-rect (E9); CFA phase/border handling (B6); wavelet perfect reconstruction (C6) | PR-blocking |
| **Property** | Param CBOR round-trip (A1); XMP mapping identity for `lb:`/our `crs:` fields (A2/F4); warp ROI-bound soundness (A4); slider-zero ⇒ identity/elision for every node (C3, D7, D13, E8, E10); inscribed-rect containment (E9) | PR-blocking |
| **Golden-image** | Per-node goldens (sharpen, NR, moiré, defringe, vignette styles, grain) and full-pipeline corpus × recipe set, per process version, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (F1) | PR-blocking (fast subset) + nightly full; per-PV immutability guard from v1.0 tag |
| **Demosaic quality gate** (epic-specific, Risk 2) | Remosaic harness on Kodak+McMaster+own-raw corpus: mean CPSNR ≥ LibRaw-floor + 0.3 dB, no per-image regression > 0.5 dB, false-color and zipper metrics ≤ floor (A5/A6/B16); perceptual review sign-offs (B18/B19) recorded | Nightly + **release-blocking**; the epic's first failing test |
| **CPU/GPU parity** | Every node `eval_cpu` vs `eval_gpu` within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (§4.4 surrendered-bit-identity contract); tiled == untiled seam tests (B10, E3) | PR-blocking |
| **Determinism** | Grain seed stability across runs/backends (E11); upright analysis + auto-CA fit determinism across platforms (E5/E6, D10); baked-H replay with zero re-analysis (E7) | PR-blocking |
| **Integration/E2E** | `lightbox-cli` headless: import → apply E11 recipe → export golden (F3); device-lost continuity (F2); LR sidecar approximate import (F4); M2 exit drill (F7) | PR-blocking fast subset; full nightly |
| **Performance** | criterion + GPU-timestamp scenario benches: demosaic 45 MP ≤ 60 ms GPU (B17); detail stage ≤ 25 ms preview ROI (C11); warp ≤ 10 ms preview ROI (E2); full-graph slider p95 < 100 ms fit-view (E12); develop-open from raw cache < 1 s (B13) | Nightly; regression → tracked issue (§8) |
| **Fault/crash** | Migration `kill -9` (A8); LibRaw X-Trans interim path failure isolation (decode-error marking, no crash — B14); malformed opcode/LCP inputs never panic (fuzz seed corpus on parsers, D1/D8) | PR-blocking |
| **License** | cargo-deny; surface-2 dyn-link assertions (lensfun, LibRaw); surface-3: lensfun-DB attribution entry, zero-`.lcp`/zero-Adobe-asset tree gate, AGPL-LSD absence (F5) | PR-blocking + release-blocking |

Reference hardware for budget assertion: RTX 3060-class Windows + M-series-base macOS (per §7); CPU-path checks on 8-core reference.

---

## 9. Performance budgets (E11-internal, derived from §7)

| Operation | Budget | Where asserted |
|---|---|---|
| Full RCD demosaic, 45 MP Bayer, GPU | ≤ 60 ms | B17 |
| Draft demosaic + first paint at fit-view | ≤ 100 ms from decoded mosaic | B12 |
| Develop-open full preview from raw cache | < 1 s (§7) | B13 |
| Detail stage (sharpen+NR+moiré) at fit-view preview ROI | ≤ 25 ms | C11 |
| `geom.warp` at fit-view preview ROI | ≤ 10 ms | E2 |
| Effects (vignette+grain) preview ROI | ≤ 5 ms | E12 |
| Full E11-active graph, worst slider, fit-view | p95 < 100 ms (§7 headline) | E12, F7 |
| Upright analysis job (CPU, 2048 px) | ≤ 500 ms end-to-end (Background class) | E5/E6 |
| Demosaic CPU fallback, 45 MP | ≤ 2 s (background tier; preview-res interactive per §4.4) | B17 |
| VRAM for 100 MP export render | within §7 bounded-tiling ceiling | F3 |

These are GPU-path budgets per §7's rule; the CPU path asserts only preview-res interactivity (§4.4 degraded contract).

---

## 10. Risks

| # | Risk | Severity | Mitigation / trigger |
|---|---|---|---|
| R1 | **Clean-room demosaic fails to beat the LibRaw floor** (architecture Risk 2). | Very high | Spike-first (B3–B5) with a hard gate before production work; LMMSE second tier pre-identified; metrics-based gate, not vibes. **Named fallback if the gate still fails at M2 freeze:** ship v1.0 PV1 on LibRaw DCB (LGPL dyn-link — license-clean, quality floor) and re-run E11.1 as a v1.x PV2 — an explicit CTO escalation, never a silent slip. |
| R2 | **AMaZE has no published paper** — the "AMaZE-class" label cannot be met by porting. | High | Reframed in §6.1: quality *target* via RCD/LMMSE from publications; the quality gate (relative to floor) is the acceptance bar, not an algorithm name. Recorded in the dossier so nobody "helpfully" ports GPL code under schedule pressure. |
| R3 | **GPL contamination via study** of RawTherapee/darktable source. | Existential (Risk 1 class) | Written clean-room protocol (B1): firewalled reader, prose-only notes, implementer attestation, dossier audit. |
| R4 | **AGPL/contrib traps in line detection** (original LSD is AGPL; FastLineDetector lives in opencv_contrib). | Medium | Own Sobel+Hough implementation committed (E5); CI assertion that no LSD/AGPL symbol enters the tree (F5). |
| R5 | **lensfun mismatch applies wrong corrections** (wrong lens auto-matched). | Medium | Score threshold + ambiguity ⇒ no auto-apply (D5); provenance shown in UI; per-lens saved defaults; 0–200 % override sliders always available. |
| R6 | **Lens-profile coverage gap without Adobe's LCP library** (mirror of Risk 10 for optics). | Medium | Constraint 4 forbids bundling Adobe LCPs — accepted. Coverage = embedded opcodes (modern mirrorless are self-describing) + lensfun DB (bundled, attributed) + user-supplied LCP/lensfun profiles. Expectation stated in UI; hot-gap fix is lensfun DB updates (data packs), never Adobe assets. |
| R7 | **Cascaded-resample quality loss or warp seams.** | Medium | Single-pass composed warp is a design invariant (§5); tiled==untiled seam tests (E3); anti-ringing clamp; f64 CPU reference for parity. |
| R8 | **NR/sharpen quality disappoints vs LR** (subjective bar). | Medium | Perceptual review loops with imaging lead built into the plan (C12/C13); algorithm choices are node-local and swappable (§1.3 reversal) without graph changes. |
| R9 | **f16 precision artifacts in raw-domain math.** | Medium | §6.2 precision rule: f32 scratch tiles for mosaic/demosaic interior; parity tests catch drift; VRAM impact bounded by tiling. |
| R10 | **liblensfun FFI instability/thread-safety** across 3 platforms. | Low-med | Immutable DB snapshot wrapper (D5); FFI confined to DB parse+match (model math is ours); dyn-lib pinned per-platform in packaging; reversal: reimplement the (documented, XML) DB parser in Rust — bounded, since model evaluators are already ours. |
| R11 | **Schedule: single-specialist serialization** (12–18 pw is the two-lane figure). | High | Lanes named (§7 preamble); Lane 2 (C–E) is deliberately buildable by a second GPU-capable engineer against A-phase contracts while Lane 1 runs the demosaic spike. Per §10 architecture staffing commitment, one-specialist staffing re-baselines the milestone — flag early, don't absorb. |
| R12 | **Interim-vs-final rendering drift during development** (M1 recipes rendered with interim demosaic look different once RCD lands). | Low | Pre-release only: PV1 is not frozen until the v1.0 tag (per-PV golden guard activates then); the interim LibRaw Bayer path never ships in a released PV (except the R1 fallback, which *becomes* PV1 by explicit decision). X-Trans interim path *does* ship and is documented as such. |

---

## 11. Open questions (named, non-blocking, with owners)

1. **NR placement vs tone mapping.** §4.1 fixes the detail stage *after* global tone/color; shadow-lifted images amplify noise before NR sees it. Not a re-order request: proposal is gain-awareness *inside* the NR nodes (threshold scaling by upstream exposure gain, passed as a param; the DAG's multi-input edges permit an aux tap if needed). Decide with E10 lead during C7. Default if undecided: static thresholds (LR-comparable behavior).
2. **LMMSE second tier** — activated only by the B5 spike gate. If activated, does it ship as automatic high-ISO routing or stay a hidden pref? Default: automatic by ISO threshold, no recipe field.
3. **Draft-vs-full progressive swap visibility** at fit view (draft demosaic first paint may visibly "pop" when RCD lands). UX acceptance with E08 during B12; fallback is skipping draft when the raw cache is warm.
4. **Classical raw-NR exposure in UI** — v1 proposal: fully automatic (hot-pixel + green-equilibration only), no slider. Confirm with product before C10 locks defaults.
5. **lensfun DB update channel** — bundled snapshot at release + data-pack updates through the model/data-pack mechanism. Packaging mechanics land in E16; E11 only needs `LensDb::load(paths)` to accept an override directory (already specced).
6. **Baked Upright homography in XMP** — `lb:` namespace round-trips it; LR imports obviously can't carry it (their `PerspectiveUpright` is a mode flag → we re-run analysis on import? Default: import maps LR Upright mode to `Off` + a badge suggesting re-run; approximate-import doc covers it). Confirm with E09 during F4.
7. **wgpu f16/subgroup availability spread** — kernels guard on feature detection (B17); confirm minimum-spec behavior on Intel iGPU during F7 drill.
8. **`raw.opcodes1` execution home** (inside E02's linearize vs first E11 node) — settled with E02 owners in D2; parser and kernels are E11's either way.

---

## 12. Definition of done

E11 is done when **all** of the following hold:

1. **Demosaic:** the clean-room Bayer demosaic quality gate is green (beats the committed LibRaw AHD/DCB floor on CPSNR/false-color/zipper per B16 thresholds) and carries two recorded perceptual-review sign-offs; 45 MP ≤ 60 ms GPU; the interim in-process Bayer path is deleted from the release build (X-Trans interim path remains, documented); raw-cache integration proves sliders never re-run demosaic.
2. **Feature completeness (Must set):** capture sharpening (4 params + debug taps), luma+chroma NR, one-click auto CA, profile lens corrections (opcodes + lensfun + user LCP with precedence and overrides), crop/straighten/rotate-90/flip, manual transform + Upright auto modes, post-crop vignette + grain — all rendering on GPU **and** CPU within the §4.4 parity tolerance, all parameters in the recipe with registered `crs:`/`lb:` mappings that round-trip.
3. **Gates:** per-node and full-pipeline goldens committed and green (PR-blocking subset + nightly full); CPU/GPU parity green; determinism tests (grain, upright, auto-CA) green; fault tests (migration kill -9, parser fuzz-never-panics) green; the three §8 license surfaces green including lensfun attribution, dyn-link assertions, zero-Adobe-`.lcp`, and AGPL-LSD absence.
4. **Performance:** §9 budget table asserted on both reference machines; full-graph slider p95 < 100 ms at fit view with every E11 node active; develop-open < 1 s from raw cache; device-lost continuity at preview res demonstrated with E11 nodes in the graph.
5. **Data model:** catalog migration shipped (lazy backfill, crash-safe); `lens_default_profile`/`user_lens_profile` flows exercised by tests.
6. **Provenance:** the clean-room dossier is complete (sources, spike evidence, reader notes, attestations) and CTO-acknowledged.
7. **Seams honored:** E12/E15 planners have signed off on the coordinate-space/transform contract note; E03 raw-cache key extension and E02 opcode-placement decisions are merged in their crates with their owners' review; no file outside the §2 crate list was modified.
8. **M2 exit contribution:** the F7 drill (import → detail/optics/geometry develop → export, 3 platforms) passes.

---

## 13. Seams to neighboring epics (named, not designed)

| Neighbor | Seam (what crosses it) | Direction |
|---|---|---|
| **E02** decode/color | `MosaicImage` contract (CFA, active area, opcode blobs, `LensMeta`); `raw.opcodes1` placement vs linearize; input transform sits between our demosaic and E10's tone stages | E02 → E11 |
| **E05** engine | `RenderNode` trait extensions (`input_roi`/`output_extent`/`debug_taps`, task A3); f32 scratch-tile support; node registration under PV keys; recompute-count probe test API | negotiated, merged into E05's crate |
| **E03** preview/raw cache | Raw-cache `<params>` key extension (§4.3); cached post-demosaic state format | E11 → E03 crate, E03-reviewed |
| **E04** ingest | Probe writes new `metadata_cache` lens columns (A8) | coordinated task |
| **E09** edit state/XMP | Param structs + defaults live in `lightbox-edit`; `crs:`/`lb:` mapping registration; Upright-apply as a history step; approximate-import semantics doc | E11 contributes fields; E09 owns machinery |
| **E10** global toolset | Pipeline neighbors (detail follows E10's tone stages); shared Gaussian/guided-filter kernel infra (C2 reuse); NR gain-awareness open question #1 | shared kernels, joint decision |
| **E08** shell/UI | Consumes debug taps (Alt-key views), crop/transform/level-tool math helpers (`angle_from_segment`, `auto_level_angle`, `largest_inscribed_rect`), defringe eyedropper API, "built-in profile applied"/interim-X-Trans UI strings | E11 exposes core APIs; E08 draws |
| **E12** masking/retouch | **Coordinate-space contract:** masks/retouch operate in post-crop canvas space; `geom.warp`+`geom.crop` publish the composed dst→src transform through the engine; geometry edits set no automatic AI-mask staleness (§4.5 — explicit recompute only) | contract note, F6 deliverable |
| **E15** export | Full-res render path through E11 nodes (tiling, Full-demosaic-only assertion, backend recording); output sharpening is E15's, explicitly not ours | E15 consumes engine; F3 joint test |
| **E13/E14** ML | None in v1 (neural denoise/SR/raw-details are v1.x; the §4.1 raw-NR slot is where a neural node would later mount) | design headroom only |
| **E16** interop/packaging | lensfun DB data-pack update channel; LR `crs:` migration fidelity for our fields (F4 fixtures feed E16's importer tests); release-runner license inspection | named, not designed |
