# E10 — Develop: Global Toolset

_Implementation spec. Epic id **E10** (`develop-global-toolset`). Milestone **M2** (with an M1 slice — see §5). Effort **XL ~10–14 pw**. Depends on **E02** (decode & color foundation), **E05** (render node-graph engine), **E09** (edit state, history, presets, XMP)._

_Authority: `docs/plan/01-architecture.md` (decision-complete; §4.1 pipeline order, §10.1 E10.1–E10.4 decomposition, §7 budgets, §8 testing, Risk 3). This spec does not re-litigate stack, seams, or the pipeline order — it implements them._

---

## 1. Summary

E10 delivers the complete **global develop toolset** as `RenderNode` implementations on the E05 engine, plus the develop-panel UI content that drives them:

- **E10.1** Basic panel: white balance (as-shot/presets/Kelvin+tint/eyedropper), exposure, contrast, whites/blacks — as nodes on the E05 engine. *(M1 slice)*
- **E10.2** **Halo-free highlight/shadow recovery** — the quality-defining sub-project (~4–6 pw on its own; architecture Risk 3). Local-Laplacian / guided-filter local tone mapping with its own algorithm spike, a dedicated clipped-highlight / deep-shadow golden corpus, a quantitative halo gate, and a perceptual review pass. CPU+GPU parity within the §4.4 tolerance is the exit gate.
- **E10.3** Tone curves (point + per-RGB-channel + parametric), 8-band HSL mixer, vibrance/saturation, three-way color grading.
- **E10.4** B&W conversion with channel mix, presence (clarity / texture / dehaze), creative LUT profiles with amount, live histogram with clipping indicators.

Everything is parameterized through `GlobalStages` in the E09-owned `Recipe` (§3.2 of the architecture), so presets, copy/paste, sync, snapshots, and history come "almost for free" from the edit-state model — E10 supplies the **parameter semantics, the pixels, and the panels**, not the serialization machinery.

**First failing test of the epic (staff-engineer contract, architecture §12):** a golden-image test — fixed raw + `exposure_ev = +1.0` recipe rendered through `Engine::submit` — compared against a committed golden within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, on both GPU and CPU backends. It fails until `ExposureNode` exists (task A6).

**First failing test of E10.2:** the halo metric (task B2) run over the clipped/shadow corpus with a naive unsharp-mask-style shadows lift **must fail** (proving the metric detects halos), and must pass with the accepted algorithm (task B17).

---

## 2. Scope & non-goals

### 2.1 In scope

| Area | Deliverable |
|---|---|
| Param model semantics | Field-level definition of `GlobalStages` (types, ranges, identity defaults, `crs:` mapping rows) inside E09's `Recipe` container |
| Render nodes | WB, exposure, contrast, highlight/shadow recovery, whites/blacks, tone curve, HSL, vibrance/saturation, color grading, B&W mix, clarity, texture, dehaze, creative LUT — each with `eval_gpu` (WGSL) **and** `eval_cpu` (rayon + SIMD) per the §4.4 contract |
| Shared node infra | Gaussian/Laplacian pyramid, guided filter, 1D/3D LUT bake & sampling, working↔Oklab/OkLCh and working↔companion-encoding conversions (WGSL + CPU, shared spec) |
| Analysis | GPU histogram reduction pass + clipping statistics; clip-preview diagnostic render target (Alt-drag whites/blacks, J-toggle overlay) |
| Develop UI (panel content) | Basic panel, curve editor widget, HSL mixer panel, color-grading wheels widget, B&W panel, presence sliders, creative-look browser with amount + hover preview, histogram widget with draggable regions and clip triangles, WB eyedropper canvas tool, before/after view modes |
| Commands | `DevelopCommand` set on the `lightbox-core` command bus (set-param with gesture coalescing, WB-from-sample, auto-WB, set-look; heuristic auto-tone as a de-scopeable stretch) |
| Catalog | One migration: `installed_look` registry for creative LUT packs (user-installed; bundled content gated by CI surface 3) |
| Quality gates | Per-node and combined-recipe goldens, CPU/GPU parity, halo gate + perceptual review for E10.2, per-slider latency scenarios against the §7 <100 ms p95 budget |

### 2.2 Explicit non-goals (named seam instead)

| Not in E10 | Where it lives |
|---|---|
| Camera profiles, DCP parse/evaluate, default look family, working/display/output color management, per-monitor ICC | **E02** (§1.7, E02.1–E02.5). E10's chain starts after the input transform; the WB node sits before it |
| Raw-domain **highlight reconstruction** (rebuilding clipped-channel color from surviving channels) | **E02/E11** raw path. E10.2 does *tone-mapping recovery* of whatever linear data arrives; see Risk R6 |
| Demosaic, capture sharpen, noise reduction, moiré, lens corrections, CA/defringe, crop/rotate, upright/transform, post-crop vignette, grain | **E11** (E11.1–E11.4) |
| Masks, per-mask sub-recipes, local adjustments, retouch | **E12** — but every E10 node is written so E12 can re-run its math under a mask weight (pure functions over params + pixels; no global state) |
| Recipe container, CBOR serde, history persistence, snapshots, presets system (incl. reading LR `.xmp` presets), copy/paste/sync/Auto-Sync commands & dialogs, XMP RDF engine and `crs:` serializer | **E09**. E10 hands E09: per-field `crs:` mapping rows, the `SettingsGroup` taxonomy for checklist dialogs, and identity-default semantics |
| Panel docking framework, keymap registry, workspace chrome, filmstrip/grid/loupe | **E08**. E10 contributes panel *content* into E08's framework and registers key bindings through its registry |
| Soft proofing, export color management | **E02/E15** (M4-tier for proofing) |
| Auto tone (ML), adaptive/AI profiles, Point Color, TAT, preset amount, ISO-adaptive presets, HDR edit mode | Could/Won't tier (feature catalog §2.4); heuristic auto-tone only, as a stretch task |
| Engine internals: DAG, cache, ROI negotiation, tiling, process-version registry, device-lost/CPU-fallback harness | **E05**. E10 consumes `RenderNode`, `NodeRegistry`, `GraphBuilder`, ROI-padding declaration (§9 seams) |

---

## 3. Crates & modules touched (per architecture §2)

| Crate | Modules added/changed | Notes |
|---|---|---|
| `lightbox-edit` | `src/params/global.rs`, `src/params/ranges.rs`, `src/params/groups.rs`, `src/xmp/crs_map_global.rs` (table rows only) | E09 owns the crate and the `Recipe` container; E10 owns the **semantic content** of `GlobalStages` (this split is per §3.2 — the schema sketch is the contract, E10 fills in field semantics) |
| `lightbox-render` | `src/nodes/global/{wb,exposure,contrast,tone_recovery,whites_blacks,tone_curve,hsl,vibrance,color_grade,bw_mix,clarity,texture,dehaze,creative_lut}.rs`; `src/nodes/common/{pyramid,guided,lut,colorspace}.rs`; matching WGSL in `shaders/global/`; `src/analysis/histogram.rs`; `src/graph/global_segment.rs` | Node library + the global-segment graph builder. Engine spine itself is E05's |
| `lightbox-color` | consumes `cct` (CCT/tint ↔ xy, CAT02/Bradford), companion display encoding constants | Seam to E02; if a utility is missing E10 adds it here **in coordination with the E02 owner** (task A10) |
| `lightbox-core` | `src/commands/develop.rs`, `src/queries/histogram.rs` | Commands route through E09's transaction/history manager; no UI types |
| `lightbox-catalog` | `migrations/NNNN_installed_look.sql`, `src/dao/looks.rs` | NNNN = next free migration number at land time |
| `lightbox-shell` | `src/develop/panels/{basic,curve,hsl,grading,bw,presence,looks,histogram}.rs`; `src/develop/tools/{wb_eyedropper,clip_overlay,before_after}.rs`; `src/widgets/{curve_editor,color_wheel}.rs` | Inside E08's panel framework; egui immediate-mode widgets we own |

No other crate is modified.

---

## 4. Design

### 4.1 Pipeline placement (fixed by architecture §4.1)

E10 owns two segments of the DAG, assembled by the pipeline assembler (E05) via E10's segment builders:

```
[E02: decode ► linearize ► raw-denoise ► demosaic]
    ──► E10: WhiteBalanceNode                        (raw: channel gains; non-raw: CAT in working space)
    ──► [E02: input transform (matrix base | DCP) ► Lightbox default look ► working space]
    ──► E10 GLOBAL TONE/COLOR CHAIN:
         ExposureNode ► ContrastNode ► ToneRecoveryNode(highlights+shadows)
         ► WhitesBlacksNode ► ToneCurveNode(parametric∘point, luma+R/G/B)
         ► HslNode(8-band) ► VibranceSatNode ► ColorGradeNode ► BwMixNode
         ► ClarityNode ► TextureNode ► DehazeNode ► CreativeLutNode
    ──► [E11: detail ► optics/geometry]  ──► [E12: LOCAL]  ──► [E11: effects]
    ──► [E02: output transform (view | export)]
```

Notes pinned here so no implementer guesses:

- **Vibrance/saturation** are not listed in the §3.2 illustrative sketch but are Must features assigned to this epic (M2 milestone text; catalog §2.4). They are added to `GlobalStages` as their own node between HSL and color grading. The sketch is explicitly "illustrative"; this is a field addition with identity defaults, not a schema re-litigation.
- **Node granularity = cache granularity.** Each stage above is one node so E05's tail invalidation works per stage (§4.2). Pointwise-stage fusion (baking exposure→curve→HSL→grade into one 3D LUT dispatch) is a **named optimization lever**, only pulled if the E6 perf scenario fails budget — it changes dispatch, not semantics or cache keys.
- **Identity elision:** every E10 node reports `is_identity(&params) -> bool`; the global-segment builder omits identity nodes from the graph, so an untouched image's chain is near-zero cost and cache keys don't churn.
- **Working data:** RGBA16F tiles (§4.2); `ToneRecoveryNode` pyramid accumulation and the histogram reduction run in f32 (architecture calls out highlight-recovery accumulation explicitly).

### 4.2 Value domains (pinned, per-node)

| Node | Math domain | Rationale |
|---|---|---|
| WB (raw path) | pre-input-transform sensor RGB, per-channel gains derived from CCT/tint via camera matrices | DNG-spec CCT model; E02 supplies matrices |
| WB (non-raw) | working space, Bradford/CAT02 adaptation | display-referred input already in working space |
| Exposure | scene-linear working, gain = 2^EV | photographic stops contract |
| Contrast | companion-encoded (ProPhoto primaries + sRGB-like curve — E02's display-companion encoding), sigmoid pivoted at encoded 18% gray (~0.46) | matches user expectation of "curve around midtones" |
| Highlight/shadow recovery | scene-linear luma pyramid, f32 | local tone mapping needs linear light |
| Whites/blacks | companion-encoded endpoint remap | endpoint semantics are display-referred |
| Tone curve | companion-encoded; x-axis 0..1 encoded | LR-compatible curve shapes ("Melissa RGB" analogue) |
| HSL / vibrance / B&W mix | OkLCh derived from working RGB; raised-cosine hue-band weights | smooth overlap, no banding; own math, no license issue |
| Color grading | luma-zone weights over companion-encoded luma; hue/sat wheels applied as chroma offsets in OkLCh, per-zone lum as gain | ASC-CDL-style behavior with perceptual chroma |
| Clarity / texture | guided-filter (clarity) and band-pass (texture) on companion-encoded luma, detail recombined | frequency tools operate perceptually |
| Dehaze | scene-linear RGB, dark-channel prior + transmission map, guided-filter refined | physical haze model wants linear |
| Creative LUT | companion-encoded RGB in, tetrahedral 3D-LUT sample, amount by interpolation toward identity (extrapolation for >100%) | `.cube`/HaldCLUT packs are authored display-referred |

These domains are part of PV1's frozen semantics once golden-frozen (task E7); changing any of them afterward means a new process version (§4.5).

### 4.3 Parameter model — concrete types (in `lightbox-edit`, semantic owner E10)

```rust
// lightbox-edit/src/params/global.rs
// Serialization derive/config is E09's; field semantics, ranges, defaults are E10's.
// Every field: absent-in-CBOR == identity default (forward-compatible growth).

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalStages {
    pub white_balance: WhiteBalance,
    pub tone: BasicTone,
    pub tone_curve: ToneCurve,
    pub color: ColorMixer,               // HSL bands + vibrance + saturation
    pub color_grade: ColorGrade,
    pub treatment: Treatment,            // Color | Monochrome(BwMix)
    pub presence: Presence,
    pub creative_look: Option<CreativeLook>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WhiteBalance {
    AsShot,                                          // default; resolves per-image from raw metadata (E02 probe)
    Preset(WbPreset),                                // Daylight, Cloudy, Shade, Tungsten, Fluorescent, Flash, Auto
    Custom { temp_k: f32, tint: f32 },               // temp 2000..=50000 (mired-linear slider), tint -150..=150
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BasicTone {
    pub exposure_ev: f32,   // -5.0..=5.0, id 0.0
    pub contrast:    f32,   // -100..=100, id 0
    pub highlights:  f32,   // -100..=100, id 0   (ToneRecoveryNode)
    pub shadows:     f32,   // -100..=100, id 0   (ToneRecoveryNode)
    pub whites:      f32,   // -100..=100, id 0
    pub blacks:      f32,   // -100..=100, id 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToneCurve {
    pub parametric: ParametricCurve,                 // region sliders + split points
    pub point:      [ChannelCurve; 4],               // [Luma, R, G, B]; empty/2-point-linear = identity
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelCurve { pub points: Vec<CurvePoint> }  // sorted strictly-increasing x; 0..=1 both axes
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint { pub x: f32, pub y: f32 }
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParametricCurve {
    pub highlights: f32, pub lights: f32, pub darks: f32, pub shadows: f32, // -100..=100, id 0
    pub splits: [f32; 3],                            // shadow/dark, dark/light, light/highlight; id [0.25,0.50,0.75]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorMixer {
    pub bands: [HslBand; 8],   // R, Orange, Y, G, Aqua, B, Purple, Magenta — fixed center hues, raised-cosine weights
    pub vibrance:   f32,       // -100..=100, id 0 (chroma-weighted, skin-hue-protected)
    pub saturation: f32,       // -100..=100, id 0 (uniform chroma scale)
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HslBand { pub hue: f32, pub sat: f32, pub lum: f32 }   // each -100..=100, id 0

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorGrade {
    pub shadows: GradeWheel, pub midtones: GradeWheel, pub highlights: GradeWheel, pub global: GradeWheel,
    pub blending: f32,        // 0..=100, id 50 — zone overlap width
    pub balance:  f32,        // -100..=100, id 0 — zone boundary shift
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeWheel { pub hue_deg: f32, pub sat: f32, pub lum: f32 } // hue 0..360, sat 0..=100 (id 0), lum -100..=100 (id 0)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Treatment { Color, Monochrome(BwMix) }      // id: Color
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BwMix { pub mix: [f32; 8] }               // -100..=100 per band; auto-mix is a command, not a stored flag

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence { pub clarity: f32, pub texture: f32, pub dehaze: f32 } // each -100..=100, id 0

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreativeLook {
    pub look: LookRef,        // content-hash reference into installed_look (survives rename/move)
    pub amount: f32,          // 0.0..=2.0, id 1.0 (0–200%)
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LookRef { pub content_hash: String, pub display_name: String }
```

Supporting definitions:

```rust
// lightbox-edit/src/params/ranges.rs
pub struct ParamRange { pub min: f32, pub max: f32, pub identity: f32, pub step: f32 }
pub fn range_of(param: GlobalParamId) -> ParamRange;          // single source for UI + validation + fuzzing
impl GlobalStages {
    pub fn is_identity(&self) -> bool;
    pub fn clamped(self) -> Self;                              // range-enforced on ingest (XMP/preset import)
}

// lightbox-edit/src/params/groups.rs — consumed by E09's preset/copy-paste checklist dialogs
pub enum SettingsGroup { WhiteBalance, BasicTone, ToneCurve, ColorMixer, ColorGrading,
                         Treatment, Presence, CreativeLook /* E11/E12 add theirs */ }
pub fn fields_of(group: SettingsGroup) -> &'static [GlobalParamId];

// Param delta classification — drives E05 node invalidation (RenderNode::invalidates)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalParamId { ExposureEv, Contrast, Highlights, Shadows, Whites, Blacks,
                         WbMode, WbTempK, WbTint, ToneCurveLuma, ToneCurveR, ToneCurveG, ToneCurveB,
                         ToneCurveParametric, HslBand(u8), Vibrance, Saturation,
                         GradeShadows, GradeMidtones, GradeHighlights, GradeGlobal, GradeBlending, GradeBalance,
                         TreatmentToggle, BwBand(u8), Clarity, Texture, Dehaze, CreativeLookRef, CreativeLookAmount }
```

**`crs:` mapping rows (handed to E09's mapping layer):** `exposure_ev→crs:Exposure2012`, `contrast→crs:Contrast2012`, `highlights→crs:Highlights2012`, `shadows→crs:Shadows2012`, `whites→crs:Whites2012`, `blacks→crs:Blacks2012`, WB→`crs:WhiteBalance/Temperature/Tint`, point curves→`crs:ToneCurvePV2012{,Red,Green,Blue}`, parametric→`crs:ParametricShadows…ParametricHighlightSplit`, HSL→`crs:HueAdjustmentRed…LuminanceAdjustmentMagenta`, vibrance/sat→`crs:Vibrance/Saturation`, grading→`crs:ColorGrade*` (+ legacy `crs:SplitToning*` import-only mapping onto shadows/highlights wheels), B&W→`crs:ConvertToGrayscale` + `crs:GrayMixer*`, clarity→`crs:Clarity2012`, texture→`crs:Texture`, dehaze→`crs:Dehaze`. Import is approximate-by-design (Risk 9 in the architecture): values map 1:1 syntactically, rendering differs; the migration-fidelity expectation is set by E09/E16, not here.

### 4.4 Render nodes — trait usage and shared infra (in `lightbox-render`)

All nodes implement E05's `RenderNode` (§2.2). E10 additionally standardizes, within the node library:

```rust
// lightbox-render/src/nodes/global/mod.rs
pub trait GlobalNode: RenderNode {
    fn is_identity(params: &GlobalStages) -> bool where Self: Sized;
    /// ROI the node needs upstream to produce `out` — pointwise nodes return `out`;
    /// pyramid/guided nodes return out + context padding. Forwarded to E05's ROI negotiation.
    fn roi_in(&self, out: Roi, scale: RenderScale) -> Roi;
}

/// Builds the two E10-owned DAG segments from a recipe; called by E05's pipeline assembler.
pub fn build_wb_segment(g: &mut GraphBuilder, p: &GlobalStages, src: PortId) -> PortId;
pub fn build_tone_color_segment(g: &mut GraphBuilder, p: &GlobalStages, src: PortId) -> PortId;

pub fn register_global_nodes(reg: &mut NodeRegistry, pv: ProcessVersion);  // called once per PV at engine init
```

Node inventory with `NodeId` constants (stable strings — they enter cache keys and the per-PV registry):

| NodeId | Stage | GPU kernel(s) | Perf budget (8 MP preview, RTX 3060 / M-base class) |
|---|---|---|---|
| `global.wb` | white balance | pointwise gain / CAT matrix | ≤ 1.5 ms |
| `global.exposure` | exposure | pointwise | ≤ 1.0 ms |
| `global.contrast` | contrast | pointwise sigmoid | ≤ 1.0 ms |
| `global.tone_recovery` | highlights+shadows | pyramid build + remap + collapse (f32) | ≤ 20 ms |
| `global.whites_blacks` | endpoints | pointwise | ≤ 1.0 ms |
| `global.tone_curve` | curves | 4×1D LUT sample (LUTs baked CPU-side per param change) | ≤ 1.5 ms |
| `global.hsl` | 8-band mixer | pointwise OkLCh + band weights | ≤ 2.0 ms |
| `global.vibrance_sat` | vibrance/saturation | pointwise OkLCh | ≤ 1.5 ms |
| `global.color_grade` | 3-way wheels | pointwise, zone weights | ≤ 2.0 ms |
| `global.bw_mix` | B&W | pointwise, shares band-weight fn with hsl | ≤ 1.5 ms |
| `global.clarity` | clarity | guided filter (box-filter passes) + recombine | ≤ 8 ms |
| `global.texture` | texture | 2-level band-pass + boost | ≤ 8 ms |
| `global.dehaze` | dehaze | dark-channel (windowed min) + transmission + guided refine + recover | ≤ 12 ms |
| `global.creative_lut` | creative look | 3D texture tetrahedral sample | ≤ 2.0 ms |

Worst case with **everything** active ≈ 63 ms, leaving headroom inside the 100 ms p95 slider budget for upstream cache hits, mask compositing (E12) and compositor overhead; typical edits touch a suffix of the chain only (tail invalidation). Budgets are asserted per-node by criterion benches (task E6 asserts the end-to-end scenario).

Shared infra modules (each has WGSL + CPU implementations against one written algorithm spec, per §4.4's shared-spec rule):

```rust
// nodes/common/pyramid.rs   — gaussian_down, gaussian_up, laplacian_build, laplacian_collapse (f32)
// nodes/common/guided.rs    — guided_filter(I, p, radius, eps) via box-filter passes; fast variant (subsample s)
// nodes/common/lut.rs       — Lut1D::bake(&ChannelCurve|composed) -> texture; Lut3D::{from_cube, from_haldclut},
//                             tetrahedral sample; identity-mix for amount
// nodes/common/colorspace.rs / colorspace.wgsl
//    working_to_oklch / oklch_to_working          (own implementation of published Oklab math)
//    working_to_companion / companion_to_working  (encoding constants from lightbox-color, E02 seam)
//    band_weights(hue_deg) -> [f32; 8]            (raised-cosine, partition of unity)
```

### 4.5 E10.2 — halo-free highlight/shadow recovery (the sub-project)

**Problem.** `highlights`/`shadows` must compress/lift quarter-tones *spatially adaptively*: a plain curve flattens texture; naive local contrast (unsharp/bilateral base-detail) produces gradient-reversal halos along high-contrast edges (skyline, backlit rim, dress-on-dark). This is architecture Risk 3 and is budgeted ~4–6 pw with a decision spike.

**Candidates (both clean-room from published papers; GPL implementations study-only, never copied):**
1. **Fast guided-filter base/detail** (He & Sun 2015): luma → guided filter (edge-aware base) → asymmetric range compression of base (highlights pull down / shadows lift) → detail re-add with gain clamp. Cheap (O(N)), decent edge behavior, risk of residual gradient reversal at strong edges.
2. **Fast local Laplacian pyramids** (Paris/Hasinoff/Kautz 2011; Aubry et al. 2014 fast approximation): per-level remapping functions parameterized by the highlights/shadows sliders; halo-free by construction at the cost of K remap pyramids (K ≈ 8–12 discretization levels, shared across both sliders).

**Spike protocol (tasks B3–B6, timeboxed 5 working days):** implement both as CPU prototypes; evaluate on the corpus (below) with the quantitative halo metric + visual boards; measure projected GPU cost from arithmetic/bandwidth counts. **Decision gate:** an ADR-style memo picks one (or guided-as-fast-path/LL-as-quality is explicitly rejected — one algorithm, one PV semantics). Default expectation is fast-LL; guided filter remains the fallback if fast-LL blows the 20 ms budget on target hardware.

**Corpus (task B1):** 20–30 scenes committed to the golden repo (own shots / CC0-verified, entered in the §8 surface-3 data manifest): blown skies with cloud detail, white-dress-dark-suit wedding pairs, backlit portraits, specular highlights, deep-shadow interiors, night scenes, high-DR landscapes; each with 3 recipe points per slider (±50, ±100, mixed highlight −100 + shadow +100).

**Halo metric (task B2):** along detected strong edges (Sobel magnitude > τ on the input), measure luminance **gradient-reversal energy** in a band of ±r px orthogonal to the edge between input and output — output must not introduce sign-flipped luma gradients above amplitude ε along edges where the input was monotonic — plus banded ΔE2000 vs a curve-only reference away from edges (the tool must actually recover, not just avoid halos). Thresholds are fixed during the spike from the visual boards and then frozen into CI.

**Exit gate (architecture-mandated):** CPU+GPU parity within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on the corpus; halo metric green; perceptual review pass signed off (review session with boards, task B16); node ≤ 20 ms at 8 MP preview on reference GPUs; f32 accumulation; ROI evaluation correct at tile borders (pyramid context padding via `roi_in`).

### 4.6 Histogram & clipping feedback

- **Compute:** `analysis/histogram.rs` — a standalone compute reduction over the engine's output texture (not a recipe node; it never enters cache keys). Workgroup-local 256-bin histograms → atomic merge; bins over the **display-companion encoding** (E02's "Melissa-RGB analogue"), channels R, G, B, Luma; plus clip counters (any channel ≥ 1−δ / ≤ δ).

```rust
pub struct HistogramPass { /* pipelines, readback ring */ }
impl HistogramPass {
    pub fn submit(&self, ctx: &GpuCtx, tex: &wgpu::TextureView) -> HistogramTicket;
    pub fn poll(&self, t: HistogramTicket) -> Option<Histogram>;   // async readback, never blocks UI
}
pub struct Histogram { pub bins: [[u32; 256]; 4], pub clip: ClipStats }
pub struct ClipStats { pub shadow_pct: [f32; 3], pub highlight_pct: [f32; 3] }
```

CPU fallback path: rayon reduction over the CPU tile output (same binning spec).
- **Widget:** overlaid RGB histogram; corner clip triangles light up from `ClipStats`; clicking them toggles persistent clip overlays (blue = shadow clip, red = highlight clip) rendered as a canvas overlay pass in the shell from the same threshold spec; **draggable regions** map horizontal drag on [blacks | shadows | exposure | highlights | whites] bands to the corresponding `SetGlobalParam` gesture (same coalescing path as sliders).
- **Alt-drag clip preview** for whites/blacks: the develop canvas requests `RenderTarget::Diagnostic(DiagnosticKind::ClipVis { stage })` — a render-target variant added at the E05 seam (agreed extension, see §9): the graph renders normally through the requested stage, then a visualization kernel maps clipped channels to solid colors.

### 4.7 Commands, gestures, before/after

```rust
// lightbox-core/src/commands/develop.rs — routed through E09's transaction/history manager
pub enum DevelopCommand {
    BeginParamGesture { image: ImageId, param: GlobalParamId },   // opens a coalescing scope
    SetGlobalParam    { image: ImageId, delta: GlobalParamDelta },// live value during drag; render coalesced latest-wins (§4.3 arch)
    CommitParamGesture{ image: ImageId },                          // exactly one history_step per gesture
    SetWhiteBalanceFromSample { image: ImageId, sample: SamplePointWorking }, // eyedropper
    AutoWhiteBalance  { image: ImageId },                          // robust AWB (gray-world/gray-edge hybrid on preview)
    SetTreatment      { image: ImageId, t: Treatment },
    AutoBwMix         { image: ImageId },                          // WB-informed auto gray mix (one-shot param write)
    SetCreativeLook   { image: ImageId, look: Option<CreativeLook> },
    AutoTone          { image: ImageId },                          // STRETCH (task E4): percentile heuristic
}
```

Gesture semantics (seam agreed with E09): `Begin/Commit` bracket a slider drag; intermediate `SetGlobalParam`s update the live recipe and trigger coalesced renders but write **no** history steps; `Commit` writes one step (undo restores pre-gesture state). Preset hover-preview (E09's dialog) reuses the same non-committing live-recipe path.

**Before/After (shell, `develop/tools/before_after.rs`):** modes `Off | Toggle(\) | SideBySide | TopBottom | SplitSlider`. "Before" is a second full `Recipe` (default: import-time state; user-assignable from any history step via E09's history API). Implementation = a second `RenderRequest` with the before-recipe + a split compositor in the canvas pass. Both requests share the upstream cache automatically (same content hash through decode/demosaic), so the marginal cost is the global chain only.

### 4.8 Creative looks: file formats & registry

- Parsers: `.cube` (3D, sizes 17–65, `DOMAIN_MIN/MAX` honored) and HaldCLUT PNG (level-8/12). Parsed → normalized f32 3D table → GPU 3D texture. Malformed files are rejected with a user-facing error, never a panic (untrusted input).
- Application space: companion-encoded RGB (these packs are authored display-referred); documented in the param reference so pack authors know the contract.
- **Amount:** output = `lerp(identity, lut_out, amount)` for amount ≤ 1; linear extrapolation clamped to gamut for 1 < amount ≤ 2.
- Looks are referenced from recipes by **content hash** (`LookRef`), never path. A missing look renders as identity + a non-modal badge, and the parameter survives (recipe is not rewritten) — same philosophy as missing originals.
- v1 ships **no bundled LUT packs** unless/until specific packs clear the CI surface-3 data manifest (CC0/CC-BY); the browser is fully functional with user-installed packs. (Open question Q5.)

---

## 5. Data model & migrations

### 5.1 Recipe (CBOR, E09 container) — no SQL impact

`GlobalStages` fields land inside `edit_recipe.doc` per §3.2. All E10 additions (vibrance, saturation, treatment, creative_look, parametric splits) carry identity defaults; **absent field = identity** on read, so existing M1 docs deserialize unchanged. Schema-number bump (if E09's policy requires one for additive fields) is executed by E09; E10 supplies the field list. No data migration needed.

### 5.2 New table: `installed_look` (one migration, `lightbox-catalog`)

```sql
-- migrations/NNNN_installed_look.sql  (NNNN assigned at land time)
CREATE TABLE installed_look (
    id            INTEGER PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK (kind IN ('cube3d', 'haldclut')),
    name          TEXT    NOT NULL,
    family        TEXT,                         -- browser grouping (e.g. "B&W", "Film")
    rel_path      TEXT    NOT NULL,             -- under <catalog>.lbdata/looks/
    content_hash  TEXT    NOT NULL UNIQUE,      -- xxh3-128 hex of the file; LookRef target
    source        TEXT    NOT NULL CHECK (source IN ('user', 'bundled')),
    license       TEXT,                         -- REQUIRED when source='bundled' (surface-3 manifest id)
    installed_at  INTEGER NOT NULL              -- unix seconds
);
CREATE INDEX idx_installed_look_family ON installed_look(family, name);
```

Storage: look files copied into `<catalog>.lbdata/looks/<hh>/<hash>.<ext>` (extends the §3.3 layout; relocatable with the data dir). DAO:

```rust
// lightbox-catalog/src/dao/looks.rs
pub fn install_look(w: &WriterHandle, file: &Path) -> Result<LookRow>;   // hash, copy, insert (idempotent on hash)
pub fn remove_look(w: &WriterHandle, id: LookId) -> Result<()>;          // file kept if any recipe references hash? no —
                                                                         // removal allowed; recipes degrade to badge (§4.8)
pub fn list_looks(r: &ReaderHandle) -> Result<Vec<LookRow>>;
pub fn resolve_look(r: &ReaderHandle, content_hash: &str) -> Result<Option<LookRow>>;
```

`edit_index` is untouched (derived by E09's write path; no new denormalized columns needed for E10 filtering in v1).

---

## 6. Phasing & process-version policy

- **M1 slice (Phase A + first E10.2 iteration):** the architecture's M1 milestone includes the basic panel (WB, exposure, contrast, highlights/shadows, whites/blacks, tone curve) on the engine MVP. E10 Phase A + tasks C1–C3 (point-curve node) + a **provisional** `ToneRecoveryNode` (the spike's chosen algorithm at prototype quality) land for M1. The M1 exit test "basic-panel slider < 100 ms at fit-view" is asserted on this slice.
- **M2 completes the epic:** E10.2 hardened through its quality gate; E10.3/E10.4 complete; M2 exit "global develop feature-complete for the Must set" (for E10's share).
- **Process-version policy for the M1→M2 window (decision, flagged as Q1):** PV1 semantics are **frozen at the end of E10 (M2)**, when task E7 locks the per-PV goldens. Between M1 and M2, PV1 is *in development*: algorithm refinements (esp. E10.2 hardening) may change PV1 output with a reviewed golden update in the same PR. The §4.5 "old PVs render forever" guard **activates at the M2 freeze** — after that, any semantic change to an E10 node requires PV2. Rationale: M1 is an internal milestone; no user edits exist outside the team. If the CTO instead rules that M1 edits must render identically forever, the provisional recovery node ships as its own PV and the M2 algorithm lands as PV2 — a registry entry, not a redesign.

---

## 7. Ordered task breakdown

Every task ≤ 1 engineer-day; AC = acceptance criteria. Order within a phase is the dependency order; phases B and C/D can run on parallel tracks (2 engineers per the pixel-stream staffing commitment, architecture §10).

### Phase A — param model + basic panel (E10.1, M1 slice)

| # | Task | AC |
|---|---|---|
| A1 | `GlobalStages` + sub-types in `lightbox-edit/params/global.rs` (fields per §4.3, serde attrs per E09 conventions) | compiles in the E09 recipe container; CBOR round-trip test green; absent-field = identity test green |
| A2 | `ranges.rs`: `ParamRange` table, `is_identity()`, `clamped()`; property test that identity `GlobalStages` serializes to an empty/minimal map | fuzzed out-of-range values clamp on ingest; identity detection exact |
| A3 | `crs:` mapping rows for WB + basic tone handed to E09 (`crs_map_global.rs` table entries + doc) | E09 mapping-layer test imports a LR sidecar's Exposure2012/Temperature/Tint into `GlobalStages` correctly |
| A4 | `GlobalParamId` + delta classification + per-node `invalidates()` map | unit test: an `ExposureEv` delta invalidates `global.exposure` and nothing upstream |
| A5 | Node scaffolding: `nodes/global/mod.rs`, `GlobalNode` trait, `register_global_nodes`, `build_wb_segment`/`build_tone_color_segment` with identity elision | a recipe with identity globals builds a graph containing zero E10 nodes (probe test) |
| A6 | `ExposureNode` (WGSL + CPU) | **epic's first failing test goes green:** +1 EV golden within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, GPU & CPU |
| A7 | `ContrastNode` (companion-domain sigmoid, pinned pivot) | ±100 goldens; endpoints move < 1% (contract: endpoints roughly pinned); parity green |
| A8 | `WhitesBlacksNode` (endpoint remap) | goldens; monotonicity property test (no tone reversal); parity green |
| A9 | `WhiteBalanceNode` — raw path (gains from resolved temp/tint via E02 matrices) + non-raw path (Bradford CAT) | as-shot renders byte-identical to E02's M1 baseline; ±temp/tint goldens both paths |
| A10 | CCT/tint ↔ gains solve + WB preset table (Daylight…Flash CCT anchors) in/against `lightbox-color` (coordinate with E02 owner) | round-trip property test: gains→(temp,tint)→gains within 0.5%; presets resolve to expected CCT ±50 K |
| A11 | WB eyedropper: neutral-solve fn (sampled patch → temp/tint that makes it achromatic) + canvas tool with magnified sample loupe | clicking a shot gray card yields measured neutrality (a/b < 0.5 in Oklab) on 5 test raws |
| A12 | Basic panel UI (`panels/basic.rs`): sliders with scrubby drag, double-click reset, WB preset dropdown, eyedropper toggle; bound via `DevelopCommand` | manual QA script + shell smoke test; panel state always reflects recipe (no widget-local state) |
| A13 | Gesture coalescing: `Begin/Set/CommitParamGesture` through E09's history manager; render scheduler latest-wins verified | a 200-event drag produces exactly 1 history step and ≤ a handful of in-flight renders (probe asserts no per-event render) |
| A14 | Basic-panel golden suite (recipe grid over 5 corpus raws) + slider-latency perf scenario registered in the nightly harness | goldens committed; scenario reports p95 < 100 ms fit-view on reference GPU (M1 exit metric for this slice) |

### Phase B — E10.2 halo-free highlight/shadow recovery (the ~4–6 pw sub-project)

| # | Task | AC |
|---|---|---|
| B1 | Assemble + commit the clipped/shadow corpus (20–30 scenes, own/CC0, licensing recorded in the surface-3 manifest; recipes per §4.5) | corpus loads in the golden harness; manifest entries pass the surface-3 policy check |
| B2 | Halo metric (gradient-reversal energy + off-edge banded ΔE) as a test-support crate fn | **E10.2's first failing test:** metric flags a naive USM shadows-lift (red) and passes a plain curve (green) on the corpus |
| B3 | CPU spike: fast guided-filter base/detail prototype (luma, compress base, re-add detail) | runs on corpus; metric + visual board artifacts produced |
| B4 | CPU spike: fast-LL remap-function family (slider → per-level remap curves, K-level discretization) | unit tests: remap identity at sliders 0; monotone in slider value |
| B5 | CPU spike: fast-LL pyramid build/interp/collapse using B4 remaps | runs on corpus; boards produced |
| B6 | **Spike bake-off + ADR memo (decision gate)** — quality (metric + boards), projected GPU cost, tile/ROI implications | memo committed; one algorithm chosen; halo-metric thresholds frozen |
| B7 | Production param mapping: (highlights, shadows) ∈ [−100,100]² → algorithm params; asymmetry rules (highlights never lift blacks, etc.) | property tests: identity at (0,0); monotone response; channel-neutral (no hue shift > 0.5 ΔE on gray ramps) |
| B8 | GPU pyramid infra (`common/pyramid.rs` + WGSL): f32 gaussian down/up, level management within tile pool | up(down(x)) reconstruction error < 1e-3 on test tiles; VRAM within tile-pool bounds at 8 MP |
| B9 | GPU remap kernels (chosen algorithm) | kernel-level parity vs CPU reference < 1e-3 RMS per level |
| B10 | `ToneRecoveryNode::eval_gpu`: full integration (build→remap→collapse), f32 accumulation, RGB reapplied via luma-ratio with chroma guard | corpus goldens (GPU) committed provisionally |
| B11 | `ToneRecoveryNode::eval_cpu` production impl (rayon + SIMD, same written spec) | corpus renders complete; no NaN/Inf on fuzzed inputs |
| B12 | CPU/GPU parity suite for the node | ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB across corpus × recipe grid |
| B13 | ROI/tiling correctness: `roi_in` context padding; tile-border test (render tiled vs whole-frame) | tiled output == whole-frame within 1e-4 RMS; no seam artifacts on boards |
| B14 | Perf to budget: ≤ 20 ms at 8 MP preview on RTX 3060-class and M-series base (f16 where safe, workgroup tuning) | criterion bench green on both reference machines; no parity regression |
| B15 | Stage-interaction tuning: recovery ∘ whites/blacks ∘ tone curve on corpus; fix ordering artifacts (e.g. re-clipping recovered highlights) | boards reviewed; no re-clip of recovered detail at default curve; goldens updated |
| B16 | **Perceptual review session** (boards: corpus × slider grid vs curve-only + competitor references) + one tuning iteration | sign-off recorded from 2+ reviewers incl. epic owner; issues filed or fixed |
| B17 | Freeze goldens + wire halo gate into PR-blocking CI | halo metric + goldens + parity run in CI; **E10.2 exit gate green** |

### Phase C — curves, HSL, color grading (E10.3)

| # | Task | AC |
|---|---|---|
| C1 | Monotone curve interpolation (PCHIP-style) + `ChannelCurve` validation | property tests: interpolant monotone where control points are; passes through points; stable under near-duplicate x |
| C2 | Parametric curve: region weight functions + split points, composed with point curve into one 1D remap | identity at zeros; region isolation test (highlights slider moves only upper region beyond tolerance) |
| C3 | `ToneCurveNode`: CPU-side 4×1D LUT bake (luma + R/G/B) + GPU LUT-sample kernel + CPU eval | goldens for canned shapes (linear/medium/strong); parity green; LUT rebake only on curve-param delta (cache probe) |
| C4 | `CurveEditor` widget core (egui): render curve + histogram backdrop, add/drag/delete points, snap/reset | interaction unit tests via egui harness; no point-ordering violations possible through UI |
| C5 | `CurveEditor` channels (Luma/R/G/B tabs), parametric mode with region sliders + draggable split handles | manual QA script; switching channels preserves per-channel state; parametric↔point modes co-exist (composed) |
| C6 | Oklab/OkLCh conversions (`common/colorspace`, WGSL + CPU, own impl of published math) | round-trip working→OkLCh→working < 1e-4; known color-pair checks |
| C7 | Raised-cosine 8-band hue weighting | partition-of-unity property test (Σweights = 1 ∀hue); band centers pinned to documented hues |
| C8 | `HslNode` (per-band hue shift / sat / lum in OkLCh) | goldens incl. hue-sweep gradient image — **no banding**: second-derivative smoothness assertion on the sweep output; parity green |
| C9 | HSL panel UI (8 bands × 3 sliders, band color chips, per-band reset) | manual QA; panel drives recipe via gesture commands |
| C10 | `VibranceSatNode` (uniform chroma scale; vibrance chroma-weighted with skin-hue protection band) | goldens; skin-tone patch (documented hue range) moves < 30% of a non-skin patch at vibrance +100; parity |
| C11 | Color-grade math: luma-zone weights (blending/balance semantics pinned), wheel → OkLCh chroma offset + per-zone lum gain | unit tests: zones sum to 1; balance shifts boundaries monotonically; sat 0 = identity regardless of hue |
| C12 | `ColorGradeNode` GPU + CPU + goldens | wheel goldens (warm-highlights/cool-shadows reference looks); parity green |
| C13 | `ColorWheel` widget (hue/sat puck, lum slider, fine-drag modifier) + grading panel (shadows/mid/high/global tabs, blending/balance) | manual QA; keyboard-accessible (arrow nudge); resets per wheel |
| C14 | `crs:` mapping rows: curves, HSL, vibrance/sat, grading + legacy `SplitToning*` import-only mapping | E09 round-trip tests green; split-toning-era sidecar imports into equivalent grading wheels |

### Phase D — B&W, presence, creative looks, histogram (E10.4)

| # | Task | AC |
|---|---|---|
| D1 | `BwMixNode` (band-weighted luminance mix, shares C7 weights) + `AutoBwMix` command (WB-informed initial mix) | goldens; mix [0;8] ≡ neutral luma conversion; auto produces documented deterministic result per WB |
| D2 | Treatment toggle plumbing + B&W panel UI (toggle + 8 mix sliders; color panels visibly disabled in Monochrome) | toggling Color↔Monochrome is one history step and round-trips; downstream color nodes elided when mono (graph probe) |
| D3 | `ClarityNode`: guided-filter local contrast on companion luma, midtone-weighted, ± range | goldens; halo metric (B2) green on corpus at clarity +100; negative clarity board reviewed; parity |
| D4 | `TextureNode`: mid-frequency band-pass boost/suppress (2-level pyramid band) | goldens; noise-amp guard: σ of a flat noise patch grows < 1.2× at texture +100; skin-smoothing board (−) reviewed; parity |
| D5 | Dehaze 1/2: dark-channel prior (windowed min) + atmospheric-light estimate + transmission map (scene-linear) | unit tests on synthetic haze; transmission map boards on hazy corpus scenes |
| D6 | Dehaze 2/2: guided-filter transmission refine + recovery + negative-dehaze (haze add); `DehazeNode` GPU/CPU | goldens on hazy scenes; no sky posterization (smoothness assertion); parity; ≤ 12 ms budget |
| D7 | Presence golden + perf consolidation (clarity/texture/dehaze into nightly perf scenarios) | all presence budgets green on both reference machines |
| D8 | `.cube` + HaldCLUT parsers (`common/lut.rs`), defensive against malformed input | parser unit tests incl. fuzzed/truncated files (error, never panic); domain handling verified against a reference LUT |
| D9 | `CreativeLutNode`: 3D texture upload, tetrahedral sampling, amount interp/extrapolate (§4.8) | identity LUT ≡ passthrough (< 1e-4); amount 0 ≡ identity; amount 2.0 clamps in-gamut; parity green |
| D10 | `installed_look` migration + DAO + `InstallLook`/`RemoveLook` commands (hash, copy into `.lbdata/looks/`) | migration up/down tested; duplicate install is idempotent; removal leaves referencing recipes rendering identity + badge state queryable |
| D11 | Look browser panel: grid w/ family grouping, hover live-preview (non-committing recipe path from A13), amount slider, missing-look badge | manual QA; hover preview never writes history; missing hash shows badge and preserves param |
| D12 | `HistogramPass` GPU reduction + CPU fallback + async readback ring | bins match a CPU reference exactly on test images; zero UI-thread blocking (probe); ≤ 2 ms GPU at preview res |
| D13 | Histogram widget: RGB overlay render, clip triangles from `ClipStats`, J-key clip overlays (canvas pass) | overlays match ClipStats thresholds pixel-exactly on synthetic ramps; keymap registered via E08 registry |
| D14 | Histogram draggable regions → param gestures (blacks/shadows/exposure/highlights/whites bands) | dragging each band edits the mapped param through the A13 gesture path (1 history step per drag) |
| D15 | `RenderTarget::Diagnostic(ClipVis)` support: visualization kernel + Alt-drag wiring on whites/blacks sliders | Alt-drag shows per-channel clip colors matching ground truth on synthetic ramps; releasing Alt restores normal target |

### Phase E — integration, polish, freeze

| # | Task | AC |
|---|---|---|
| E1 | Before/After modes (Toggle/SideBySide/TopBottom/SplitSlider) via second `RenderRequest` + split compositor | before-render reuses upstream cache (cache-hit probe); `\` toggle < 100 ms swap; all four modes manual-QA'd |
| E2 | Before-state assignment (from history step via E09 API) + `SettingsGroup` taxonomy finalized for E09's copy/paste & preset checklists | E09's checklist dialog lists E10 groups; group→field mapping test green |
| E3 | `AutoWhiteBalance`: robust AWB (gray-world/gray-edge hybrid over preview tier) | deterministic per image; sane results on 10-scene board (review); one history step |
| E4 | **STRETCH (de-scopeable):** heuristic `AutoTone` — percentile-based exposure/whites/blacks + contrast from histogram | produces documented deterministic params; never clips > 0.5% pixels beyond target percentiles; board reviewed |
| E5 | Combined-recipe golden suite: full-chain recipes (all E10 stages active) × corpus, GPU + CPU | committed goldens; parity ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB |
| E6 | Worst-case perf scenario: everything-active recipe, exposure slider drag (invalidates the whole tail) at fit-view | p95 < 100 ms on both reference machines in nightly harness; per-node budget table (§4.4) asserted by criterion |
| E7 | **PV1 freeze:** lock per-PV goldens; register PV1 node set permanently in `NodeRegistry`; enable the §4.5 immutability guard for E10 nodes in CI | any subsequent E10-node output drift fails CI; freeze recorded in the PV ledger (per Q1 ruling) |
| E8 | Documentation: param semantics reference (ranges, domains §4.2, identity defaults), complete `crs:` mapping table, look-pack authoring contract | docs reviewed by E09 + E12 owners (they consume param semantics); linked from the architecture doc's epic index |

**Total: 68 tasks** (A:14, B:17, C:14, D:15, E:8) ≈ 12–14 pw with review/integration overhead — within the XL envelope; E4 is the named de-scope if the envelope tightens.

---

## 8. Test plan (per architecture §8)

| Layer | Tests | Gate |
|---|---|---|
| Unit | Param ranges/identity/clamping; curve interpolation; hue-band partition of unity; CCT round-trip; zone weights; LUT parsers (incl. malformed input); histogram binning vs CPU reference | PR-blocking |
| Property (`proptest`) | `GlobalStages` CBOR round-trip identity; absent-field=identity; curve monotonicity under random control points; WB solve round-trip; no-NaN/Inf on fuzzed params × fuzzed tiles for every node | PR-blocking |
| Golden-image | Per-node goldens (each node × param grid × 5-raw base corpus); E10.2 dedicated corpus (§4.5); combined-recipe suite (E5); **per process version** from the E7 freeze | PR-blocking |
| CPU/GPU parity | Every node, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (§4.4 contract), on the CI matrix (Metal/DX12/Vulkan) | PR-blocking |
| Halo gate | B2 metric over the E10.2 corpus for `tone_recovery` and `clarity` | PR-blocking from B17/D3 |
| Invalidation probes | Param delta → recompute-count assertions (only the touched node + tail re-evaluates); identity elision; LUT rebake only on curve deltas; before/after cache sharing | PR-blocking |
| Integration (headless) | `lightbox-cli`: import raw → apply scripted global recipe → export JPEG → visual regression; LR sidecar import → params land per mapping table (with E09) | PR-blocking (fast subset), full nightly |
| Perf | criterion per-node benches vs §4.4 budget table; nightly scenario harness: per-slider p95 latency, worst-case E6 scenario, histogram-pass cost | Nightly; regression → tracked issue |
| Perceptual review | E10.2 (B16, gate), clarity/texture boards (D3/D4), default-interaction boards (B15) — humans, recorded sign-off | Phase-gate, not CI |
| Fault/robustness | Malformed `.cube`/HaldCLUT never panics; missing look renders identity + badge; device-lost mid-drag continues at preview-res CPU (rides E05.5 harness, E10 nodes must pass through it) | PR-blocking |

Reference hardware for budget assertions: RTX 3060-class Windows (DX12) and M-series base macOS (Metal), per §7.

---

## 9. Seams to neighboring epics (named, not designed here)

| Seam | Direction | Contract |
|---|---|---|
| **E05** `RenderNode`/`NodeRegistry`/`GraphBuilder` | E10 consumes | E10 registers nodes per PV; needs from E05: (a) **ROI-padding negotiation** (`roi_in`-style) for pyramid/guided nodes — required by B13; (b) **`RenderTarget::Diagnostic`** variant for clip-vis (D15); (c) f32 tile format request for `tone_recovery`. All three flagged to the E05 owner at planning time; fallback for (a) is evaluating recovery on the full visible ROI + guard band |
| **E05** pipeline assembler | E05 calls E10 | `build_wb_segment` / `build_tone_color_segment` are the only entry points; E10 never mutates the DAG outside its segments |
| **E02** color foundation | E10 consumes | Camera matrices + as-shot neutral (probe) for WB; CCT/CAT utilities in `lightbox-color` (A10 coordinates); companion-encoding + Oklab constants; input-transform/default-look nodes sit *between* E10's two segments. **Raw highlight reconstruction** (unclipped-channel rebuild) is E02/E11 upstream work — see Risk R6 |
| **E09** edit state | both | E10 defines field semantics + `crs:` rows + `SettingsGroup`s; E09 owns container, serde, history, presets, copy/paste/sync, XMP engine. Gesture API (`Begin/Set/Commit`) agreed in A13. Preset hover-preview reuses E10's non-committing live-recipe path |
| **E08** shell framework | E10 consumes | Panels mount in E08's docking/panel framework; keys (`\`, J, eyedropper `W`, etc.) registered through the keymap registry; E10 ships panel content only |
| **E11** detail/optics/geometry | downstream neighbor | E11's segments attach after `creative_lut`'s port; shared `common/{pyramid,guided}` infra is E10-built, E11-reused (sharpen/NR) — treat as render-crate common code with joint ownership after E10 lands |
| **E12** masking/local | future consumer | Every E10 node's math is a pure function usable under a mask weight; the per-mask `LocalRecipe` reuses E10 param types and value domains (§4.2). No E10 node may hold cross-tile global state that would break masked re-evaluation (the histogram pass, which is global, is analysis — not a recipe node — for exactly this reason) |
| **E15** export | downstream | Export renders the same graph at full res with `RenderTarget::Export`; no E10-specific export logic. Output sharpening is E15/E11 |
| **CI surfaces (§8)** | E10 feeds | Corpus raws + any bundled look packs enter the surface-3 data manifest (B1, D10); no new native deps introduced by E10 (pure Rust + WGSL), so surfaces 1–2 are untouched |

---

## 10. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **E10.2 algorithm misses quality or perf** (Risk 3 of the architecture): fast-LL too slow at 8 MP preview, guided filter halos residually | Timeboxed dual spike with quantitative gate (B6); budgets asserted early (B14); guided-filter fallback named; scope firewall — E10.2 slips alone without blocking C/D tracks |
| R2 | **100 ms budget erosion**: exposure edits invalidate the entire tail; everything-active recipes stack ~63 ms of E10 nodes before engine overhead | Per-node budget table asserted by criterion (§4.4); identity elision keeps typical chains short; named lever: pointwise-stage fusion into a baked 3D LUT dispatch (semantics-preserving) if E6 fails |
| R3 | **Hue-band banding / hue twists** in HSL & B&W at extreme settings | Partition-of-unity + smoothness assertions on sweep images (C8); OkLCh domain chosen for hue linearity; perceptual boards |
| R4 | **PV churn between M1 and M2**: hardening E10.2 changes PV1 output after M1 edits exist internally | Q1 policy (§6): PV1 mutable-with-reviewed-golden-update until the E7 freeze; freeze activates the immutability guard |
| R5 | **Cross-backend parity failures** (transcendentals/FMA differ per driver — §4.4 surrenders bit-identity) | Tolerance-based parity (ΔE2000 ≤ 1.0) not bit-equality; shared algorithm spec per node; avoid backend-divergent intrinsics in WGSL where a polynomial is stable |
| R6 | **Recovery quality capped by missing raw highlight reconstruction**: without unclipped-channel rebuild upstream, highlights −100 recovers to flat white where all channels clipped | Named seam to E02/E11 (reconstruction is a Should-tier raw-path stage); E10.2 corpus includes fully-clipped patches so the gap is measured and visible; user-facing behavior (recover-to-white, never magenta) asserted in goldens |
| R7 | **LUT packs as untrusted input** (malformed/adversarial files) | Defensive parsers, fuzz tests (D8), size caps; parse errors are user-visible, never panics |
| R8 | **Curve editor / wheel widgets underestimate** (immediate-mode, we own the widgets — same class of risk that re-baselined E08) | Widgets are two tasks each with harness tests; if they balloon, panel polish (C5/C13 niceties) is the cut line, not node work |

### Open questions

| # | Question | Owner / deadline |
|---|---|---|
| Q1 | Ratify the §6 PV-freeze policy (PV1 freezes at M2; M1-internal edits not immutability-protected) | CTO, before B15 lands |
| Q2 | Histogram/companion encoding: confirm E02's display-companion curve (sRGB-like over ProPhoto primaries) is the pinned domain for curves/contrast/histogram (§4.2) — it is PV-frozen once E7 lands | E02 owner + E10 owner, before C3 |
| Q3 | Does E05's ROI negotiation already carry per-node input padding (`roi_in`)? If not, agree the API before B8 (fallback named in §9) | E05 owner, before B8 |
| Q4 | Vibrance skin-protection hue range + strength: fixed constants after perceptual review, or exposed as a calibration-panel-style tweak (leaning: fixed, PV-frozen) | E10 owner at C10 review |
| Q5 | Ship any bundled starter look pack at M2 (requires CC0/CC-BY clearance through surface 3) or user-install only for v1? | Product + license review, before D11 UI copy |
| Q6 | `AutoTone` heuristic (E4): in or out of the M2 cut? (Should-tier; catalog says heuristic-first is acceptable) | Epic owner at Phase E start |
| Q7 | Parametric curve is Should-tier per the catalog but cheap once C1–C3 exist — confirm it stays in M2 scope (this spec includes it; cutting it saves ~1.5 d) | Epic owner, Phase C start |

---

## 11. Definition of done

E10 is done when **all** of the following hold:

1. **Feature completeness:** every Must-tier global tool (histogram + clipping, WB incl. presets/eyedropper, exposure, contrast, halo-free highlights/shadows, whites/blacks w/ clip preview, point + per-RGB curves, HSL 8-band, vibrance/saturation, before/after) and the committed Should-tier set (parametric curve, color grading, B&W mix, clarity/texture/dehaze, creative LUTs w/ amount) render through the E05 engine on GPU **and** CPU, driven from panels in the shell, with all parameters living in `GlobalStages`.
2. **E10.2 exit gate (architecture-mandated):** halo metric green on the dedicated corpus, perceptual review signed off, CPU+GPU parity within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, node ≤ 20 ms at 8 MP preview on both reference machines.
3. **Performance:** worst-case everything-active exposure-drag scenario p95 < 100 ms at fit-view on both reference machines (nightly harness green at epic close); per-node budgets (§4.4 table) asserted by criterion.
4. **Correctness gates green in CI (PR-blocking):** per-node + combined goldens, parity suite, invalidation probes, property tests, halo gate, headless import→edit→export integration test.
5. **PV1 frozen:** E7 executed — per-PV goldens locked, immutability guard active for all E10 nodes, PV ledger entry recorded.
6. **Edit-state integration:** every `GlobalStages` field CBOR-round-trips with absent=identity semantics; documented `crs:` mapping rows delivered and exercised by E09's round-trip tests (incl. legacy split-toning import); `SettingsGroup` taxonomy consumed by E09's preset/copy-paste checklists; slider gestures produce exactly one history step.
7. **Catalog:** `installed_look` migration shipped with up/down tests; missing-look degradation (identity render + badge, param preserved) tested.
8. **License/data hygiene:** corpus raws and any bundled looks cleared through the surface-3 data manifest; no new native dependencies; clean-room provenance note (papers cited, no GPL source consulted beyond study-only policy) recorded in the E10.2 ADR memo.
9. **Docs:** param semantics reference (domains, ranges, identity defaults), `crs:` mapping table, and look-pack authoring contract published and reviewed by the E09 and E12 owners.
10. **M2 exit contribution:** the "global develop feature-complete for the Must set" clause of the M2 milestone is demonstrably satisfied for E10's scope via the headless CLI demo script.
