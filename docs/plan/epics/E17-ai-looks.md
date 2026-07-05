# E17 — AI Looks: image-adaptive cinematic grading (`lightbox-looks`)

_Epic spec. Author: staff engineer (E17 planner). Inputs: `docs/plan/00-mandate.md` (**v2.1** — the AI Looks Core bullet), `docs/plan/01-architecture.md` (approved incl. v2.1: §0, §2.1/§2.2 `lightbox-looks` sketch, §9 M3, §10.1 E17.1–E17.5 + deployment note), epic specs E09 (recipe/history/preset seams), E10 (`GlobalStages` param model), E03 (preview service), E05 (engine), E06 (jobs), E08 (panel framework), E13 (`InferenceClient`), research reports 02 (develop-global conventions), 08 (AI/ML landscape: Adaptive Presets/Profiles, Auto settings), 10 (competitive: nothing FOSS ships image-fitted editable grades offline). Milestone **M3**. Effort **L ~6–8 pw**. Depends on **E09, E10** (hard); **E13 optional** (bias only, never gating); consumes E03 (preview pixels), E05 (proposal thumbnails), E06 (jobs), E08 (panel chrome)._

This epic builds the v2.1 headline differentiator: a **locally-run looks engine** that analyzes the opened image's palette and tonal distribution and proposes varied cinematic grades — teal-orange, film-stock families, bleach bypass, matte fade, … — **fitted to the image's actual colors**, with a **seeded shuffle/variations** action. Every proposal is a **partial develop recipe** (color-grade wheels + tone-curve points + HSL deltas) merged onto the user's current edit through E09 as **one undoable history step**, fully editable afterward in the ordinary E10 panels — **never a baked filter, never an opaque layer, never a LUT**. Classical parameterized synthesis is the primary and complete path; an optional ML palette/mood classifier rides E13's `InferenceClient` to *bias family selection only* and is not a dependency of any kind (architecture §10.1 E17.5: "no feature loss").

**What makes this different from a preset pack (the thesis, restated as an engineering contract):** a static preset applies the *same* wheel angles to every image. E17's `fit()` reads the image — where its skin tones sit, where its dominant hues cluster, where its tonal mass lies — and *places* the grade's poles, curve anchors, and band shifts relative to those measurements. A teal-orange look anchors its warm pole near the image's detected skin/highlight hue and its cool pole in the complementary shadow region; a matte fade lifts the toe to just above the image's measured black point. That is what "adaptive" means, and it is classical arithmetic over classical statistics — no model required (mandate: "classical palette analysis first, models only where they earn their footprint").

**The epic's first failing test (written before any implementation):** `same_seed_same_image_same_look` — a headless test decodes a fixture JPEG through E03, runs `analyze()`, then `propose(&stats, &opts)` **twice** with `seed = 42, count = 8`. The two proposal lists must be identical (same families, same order, byte-identical patch CBOR). Applying proposal 0 via `EditCommand::ApplyLook` must produce **exactly one** history step; `Undo` must restore the prior recipe struct-equal; after apply, `Recipe::get(GradeShadows)` must return the wheel value the patch set (i.e. the look is ordinary recipe state). It fails until the analyzer, family registry, proposer, and E09 apply path all exist — the epic's spine, exercised end to end.

---

## 1. Scope & non-goals

### 1.1 In scope

1. **New leaf crate `lightbox-looks`** (architecture §2.1/§2.2): pure-CPU, no GPU device, no catalog writes, no async in the core API. Contents: image-stat analyzer, look-family registry + fitting, patch validation, seeded proposal engine, classical bias heuristics, optional ML-bias adapter.
2. **Image-stat analyzer (E17.1)** — CPU-side, over **decoded E03 preview pixels** (never a render-DAG node — the §10.1 placement decision, restated in §4.1 below): OkLab k-means palette, tone histogram + percentiles, hue field, temp/tint estimate, skin detection. Deterministic and versioned (`ANALYZER_REV`).
3. **Look-family model + recipe-delta synthesis (E17.2)** — ≥ 9 parameterized cinematic families, each a `fit(stats, base, variation) → LookPatch` producing **only** color-grade wheel values, point-tone-curve control points, and HSL band / vibrance / saturation deltas (the pinned `LOOK_PARAM_SURFACE`, §3.3). Fitting is anchored to `ImageStats` and composed over the current recipe's values.
4. **Seeded shuffle / variations (E17.3)** — `LookSeed(u64)` drives a SplitMix64-keyed deterministic stream; `propose()` yields *n* varied-but-coherent proposals; re-shuffle derives a new seed; any proposal is reproducible from `(image pixels, analyzer_rev, looks_engine_rev, seed, slot)`.
5. **Proposal preview + apply (E17.4)** — a `LooksHub` session service in `lightbox-core` (accessor pattern of `previews()`/`engine()`/`edits()`): analysis as an E06 job, proposal thumbnails rendered through the normal **E05 `Engine`** on the *merged* recipe (`RenderTarget::Buffer`, thumbnail `RenderScale::Fit`), apply via a new **additive** `EditCommand::ApplyLook` → one WAL txn → one `history_step` (`StepLabel::Look`) → one undo removes the whole look.
6. **AI Looks develop panel** in `lightbox-shell` on **E08's `PanelDef` framework**, behind a **feature flag** (cargo feature `ai-looks` + runtime pref `looks.enabled`): proposal thumbnail grid, shuffle button, seed display, apply-on-click. `SourceReq::Any` — looks work identically on raw and non-raw sources (they touch only universal stages, §2.4 surface table rows "full/full").
7. **Optional ML palette/mood classifier (E17.5)** — a small ONNX scene/mood classifier as an *optional* E13 model pack; its output only **reweights family selection** (a `FamilyBias`). Classical bias heuristics are the always-available fallback; the invariant *bias never changes patch content* is tested (§7.5).
8. **Determinism + golden-recipe CI** — committed golden proposal sets (params, not pixels) over a fixture corpus × seeds, gated exactly like golden images: drift fails CI unless `LOOKS_ENGINE_REV` is bumped in the same PR (§7.2).
9. **CLI surface** — `lightbox-cli looks analyze|propose|apply` for headless testing and scripting (the E01 CLI harness pattern).

### 1.2 Explicit non-goals (named seams, not designs)

| Not in E17 | Owner | The seam |
|---|---|---|
| Recipe schema, `ParamDelta`/`ParamId` vocabulary, history/undo mechanics, gesture model, preset store | **E09** | E17 emits a `ParamDelta` restricted to `LOOK_PARAM_SURFACE`; apply rides E09's dispatcher exactly like `ApplyPreset` (one txn, one step). E17 adds two *additive* variants (`EditCommand::ApplyLook`, `StepLabel::Look`) under E09-owner joint review |
| Color-grading / curve / HSL **rendering** (nodes, WGSL, value domains) | **E10** | E17 sets E10's params; it renders nothing itself. Param ranges come from E10's `range_of()`; every emitted value is in-range by construction and re-validated by `Recipe::apply` |
| Static creative LUT looks (`.cube`/HaldCLUT browser, `installed_look`, `LookRef`) | **E10.4** | Disjoint feature. A creative LUT is a fixed pixel mapping; an AI Look is a param patch. The AI Looks panel never emits `CreativeLookRef` |
| Engine spine, cache, ROI, progressive ladder, device-lost | **E05** | Thumbnails are ordinary `RenderRequest`s (`Buffer` target, `Batch` priority). E17 adds **zero render nodes** and holds **no device** |
| Preview pyramid, decode, raw cache | **E03/E02** | Analyzer input is `PreviewService::best_available` + `open_pixels` (≤ 512 px long edge) |
| Panel chrome, `param_slider`, keymap, prefs store plumbing | **E08** | E17 registers one `PanelDef` and binds prefs keys; the panel uses E08's grid/thumbnail idioms |
| Inference host, model packs, VRAM gating, supervision | **E13** | E17.5 calls `InferenceClient::run_model` with client-side pre/post-processing (E13's task-adapter pattern); pack install/licensing rides `PackManager` unchanged |
| Auto **correction** (Sensei-style auto exposure/contrast — research 08 "Auto settings") | E10 E4 stretch / future | Looks are creative grades layered *over* correction; E17 never touches `BasicTone`/WB (§3.3 rationale) |
| Mask-scoped ("adaptive-preset"-style, research 08) looks; B&W/monochrome film stocks (`Treatment`/`BwMix`) | **v1.x headroom** | The patch surface can grow additively later; v1 is global-only, color-only (mandate names "color grading + curves + HSL deltas") |
| Batch "apply this look across the set" | **E09** (`sync_to`) / E10 M2 UI | Apply targets the active image; cross-set application is Copy/Sync of the resulting settings — already E09's machinery |
| Any catalog migration | — | **E17 ships zero migrations** (§5); this is the rollback guarantee, asserted in CI (§7.6) |

### 1.3 Decisions this epic makes (within its remit; architecture decisions are cited, not re-opened)

1. **Analyzer input = decoded preview at ≤ 512 px long edge, subsampled to ≤ 150k samples.** Sub-frame CPU cost, stable statistics. (Architecture §10.1 E17.1 fixes *placement* — CPU-side over E03 tiles, not a DAG node; this fixes the *operating point*.)
2. **Analysis basis = the current edit, not the original.** Stats are computed from the best available preview whose `recipe_rev` matches the working recipe (stale previews are used with a `StatsBasis::stale` marker and re-analysis on next shuffle). Rationale: looks layer on the user's correction; fitting to the corrected image is what "fitted to the image's actual colors" means after the user warms the WB.
3. **The patch surface is pinned and enumerable** (`LOOK_PARAM_SURFACE`, §3.3): point tone curves (luma + R/G/B), the six color-grade params, the 8 HSL bands, vibrance, saturation. **Excluded:** exposure/contrast/highlights/shadows/whites/blacks, WB, parametric curve, presence, treatment, creative LUT, geometry, detail. Rationale: (a) the mandate names exactly "color grading + curves + HSL deltas"; (b) a look must never fight the user's tonal *correction* — applying a look then fixing exposure must not interact; (c) one curve representation (point) keeps composition well-defined (§4.3).
4. **Applied looks store *values*, not (family, seed).** The recipe after apply contains ordinary param values; replay/render never re-runs the synthesizer (the recipe-side mirror of §4.5's baked-raster philosophy — the reproducible artifact is the materialized patch). Family + seed live in the history-step label and panel session as *provenance/display only*, never in `lb_extra` — the recipe stays indistinguishable from hand-set values.
5. **Fitting composes over the current values** of the surface params (curves compose functionally; wheels and HSL deltas compose additively with clamping) so a look layers on an existing grade instead of clobbering it (§4.3). The thumbnail always renders the merged result, so WYSIWYG holds regardless.
6. **Bias reweights selection only.** Classical or ML, a `FamilyBias` changes which families fill the *n* slots and in what order — it never alters what `fit()` emits for a given `(family, stats, seed, slot)`. This keeps the classical and ML paths *identical in output vocabulary* and makes "no feature loss" a testable invariant (§7.5).
7. **Feature flag = cargo feature `ai-looks` (panel + core wiring) + runtime pref `looks.enabled`.** Compiled out → the panel does not exist and `lightbox-looks` is not linked into the shell; compiled in → pref-gated. Default: feature **on** in dev builds, release default decided at the M3 exit review (OQ1).
8. **RNG = SplitMix64** (own 20-line implementation, no dependency), per-slot substreams keyed `seed ⊕ xxh3_64(family_id) ⊕ slot`. Deterministic, platform-independent, serializable.
9. **Golden-proposal comparisons use tolerance, not bit-equality, across platforms** (|Δparam| ≤ 1e-4; curve points ≤ 1e-4 both axes): OkLab conversion uses `cbrt` and the fitter uses transcendentals whose libm results may differ across platforms. Within one platform, byte-identical CBOR is asserted (§7.1).

---

## 2. Crates & modules

Per architecture §2.1 (v2.1 crate-map annotation). New crate in **bold**.

| Crate / module | Contents |
|---|---|
| **`lightbox-looks`** (lib, new leaf) | `stats` (OkLab, sampler, k-means, tone/hue/skin analyzers, `ImageStats`, `analyze()`) · `patch` (`LookPatch`, `LOOK_PARAM_SURFACE`, validation, curve-composition util) · `family` (`LookFamily`, `Variation`, the family registry + per-family `fit` fns, `LOOKS_ENGINE_REV`) · `propose` (`LookSeed`, SplitMix64, weighted deterministic selection, `propose()`) · `bias` (`FamilyBias`, `classical_bias`; `ml` submodule behind cargo feature `ml-bias`: mood-classifier adapter over `InferenceClient`) |
| `lightbox-core` (touched) | `looks::LooksHub` (session service: stats cache, proposal state machine, thumbnail tickets, shuffle) · `Session::looks()` accessor · dispatcher arm for `EditCommand::ApplyLook` · additive `Event::{LooksProposalsReady, LooksThumbReady}` · additive `Queries::looks_state` |
| `lightbox-edit` (touched, E09-owned — joint review) | additive `EditCommand::ApplyLook` variant + additive `StepLabel::Look { family, seed }` variant (both enums are `#[non_exhaustive]`) |
| `lightbox-shell` (touched) | `develop/panels/ai_looks.rs` — one `PanelDef` (`"develop.ai-looks"`, `SourceReq::Any`) behind `#[cfg(feature = "ai-looks")]`: thumbnail grid, shuffle, seed field, apply, empty/analyzing/error states |
| `lightbox-cli` (touched) | `looks analyze <path>` / `looks propose <path> --seed N --count K --json` / `looks apply <path> --seed N --slot I` subcommands |
| `lightbox-color` (consumed) | sRGB → linear → OkLab conversion helpers if already present; else `lightbox-looks::stats::oklab` owns the 30 lines of matrix math (own code, no dependency) — resolved at A1 |

**Dependencies of `lightbox-looks`:** `lightbox-types`, `lightbox-edit` (for `ParamDelta`/`ParamId`/`Recipe` types), `lightbox-preview` (for `DecodedPreview` — type only), `serde`, `thiserror`, `twox-hash` (already in workspace). Optional (`ml-bias` feature): `lightbox-ml` (for `InferenceClient` types only). **No** `wgpu`, **no** `rusqlite`, **no** `tokio` — the crate is synchronous and pure; concurrency lives in `lightbox-core`'s `LooksHub`. New third-party deps: **none** (license surface unchanged).

---

## 3. Interface definitions

Signatures are the contract; field lists may grow additively. All types `Send + Sync + 'static` unless noted; all public types `Clone + Debug`, stats/patch/proposal types `Serialize + Deserialize` (CBOR/JSON for goldens and CLI).

### 3.1 Image statistics (`lightbox_looks::stats`) — the E17.1 model

```rust
/// Bumped whenever analyze() semantics change. Part of every ImageStats and every
/// golden-proposal key: goldens fail CI unless this (or LOOKS_ENGINE_REV) is bumped
/// in the same PR (§7.2).
pub const ANALYZER_REV: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OkLab { pub l: f32, pub a: f32, pub b: f32 }
impl OkLab {
    pub fn from_srgb8(rgb: [u8; 3]) -> OkLab;        // sRGB EOTF → linear → LMS → OkLab (own math)
    pub fn hue_deg(&self) -> f32;                     // atan2(b, a), [0, 360)
    pub fn chroma(&self) -> f32;                      // hypot(a, b)
}

/// One palette cluster. Weight = fraction of samples in the cluster (Σ = 1).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Swatch { pub color: OkLab, pub weight: f32 }

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Percentiles { pub p01: f32, pub p05: f32, pub p25: f32, pub p50: f32,
                         pub p75: f32, pub p95: f32, pub p99: f32 }   // of OkLab L, 0..=1

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToneStats {
    pub hist: [f32; 64],            // normalized luma histogram (OkLab L)
    pub pct: Percentiles,
    pub black_point: f32,           // ≈ p01
    pub white_point: f32,           // ≈ p99
    pub mean_l: f32,
    pub contrast_index: f32,        // (p95 − p05), 0..=1 — flat scenes score low
    pub clipped_lo: f32,            // fraction of samples with L ≤ δ
    pub clipped_hi: f32,            // fraction with L ≥ 1−δ
}

/// Chroma-weighted hue mass in 36 × 10° bins — "where the image's color actually is".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HueField { pub bins: [f32; 36] }
impl HueField {
    pub fn dominant_hue_deg(&self) -> Option<f32>;            // None if avg chroma ≈ 0 (near-mono image)
    pub fn mass_near(&self, hue_deg: f32, width_deg: f32) -> f32;
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SkinStats { pub fraction: f32, pub mean_hue_deg: f32, pub mean_chroma: f32 }

/// What the stats were computed FROM — recorded for cache-keying and honesty, never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatsBasis {
    pub image: ImageId,
    pub recipe_rev: u64,            // working-recipe rev the preview reflected
    pub stale: bool,                // preview lagged the recipe (proposals still valid — they are proposals)
    pub tier: Tier,                 // E03 tier the pixels came from
    pub long_edge: u32,             // analysis resolution actually used (≤ 512)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageStats {
    pub analyzer_rev: u32,
    pub basis: StatsBasis,
    pub palette: Vec<Swatch>,       // k ≤ 8, sorted weight-desc then hue-asc (deterministic order)
    pub tone: ToneStats,
    pub hue: HueField,
    pub avg_chroma: f32,
    pub temp_tint: (f32, f32),      // mean (b, a) of upper-luma samples — a *cast* estimate, not sensor Kelvin
    pub skin: Option<SkinStats>,    // None below detection threshold
}

/// Pure, synchronous, CPU-only. Deterministic: identical input pixels + same ANALYZER_REV
/// → bit-identical ImageStats (fixed k-means++ seeding from a content-derived seed, fixed
/// iteration cap = 20, fixed subsample stride). Never acquires a GPU device (asserted in test).
/// TaggedIcc previews are converted to sRGB via lightbox-color (LCMS2, CPU) before sampling.
pub fn analyze(px: &DecodedPreview, basis: StatsBasis) -> Result<ImageStats, AnalyzeError>;

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("preview too small: {0}px")]         TooSmall(u32),       // < 32 px long edge
    #[error("unsupported colorspace")]           Colorspace,
    #[error("degenerate image (single color)")]  Degenerate,          // propose() still works: tonal families only
}
```

**Analyzer internals (pinned so determinism is checkable, not vibes):** subsample on a fixed stride grid to ≤ 150k samples → convert to OkLab → (a) k-means, k = 8, k-means++ init seeded by `xxh3_64(pixel bytes)` with deterministic tie-breaking, ≤ 20 iterations, empty clusters dropped then merged below 1% weight; (b) 64-bin L histogram + exact percentiles over the sample set; (c) 36-bin chroma-weighted hue histogram; (d) skin detector = fraction of samples inside a fixed OkLab hue/chroma/luma ellipsoid (hue 15°–45°, chroma 0.03–0.15, L 0.35–0.85 — constants tuned at A4 against portrait fixtures), threshold `fraction ≥ 0.02` → `Some(SkinStats)`.

### 3.2 Look families (`lightbox_looks::family`) — the E17.2 model

```rust
/// Bumped whenever any family's fit() output changes for identical inputs.
/// The golden-proposal gate keys on (ANALYZER_REV, LOOKS_ENGINE_REV).
pub const LOOKS_ENGINE_REV: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct LookFamilyId(pub &'static str);   // "teal-orange", "bleach-bypass", …

/// The per-proposal variation vector: every axis in [-1, 1], derived deterministically
/// from the seeded stream (§3.4). Families interpret axes; unused axes are ignored.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Variation {
    pub intensity: f32,   // overall grade strength (mapped to ~[0.4, 1.0] internally)
    pub warmth: f32,      // warm/cool bias of the primary pole
    pub split: f32,       // shadow/highlight emphasis balance
    pub fade: f32,        // toe lift / matte amount
    pub drift: [f32; 4],  // family-specific spare axes (pole hue drift, band emphasis, …)
}

/// The values a look composes OVER — the current recipe's LOOK_PARAM_SURFACE state.
/// Extracted via Recipe::extract(&look_subset()) by the caller; lightbox-looks never
/// touches a Recipe directly (it needs only these values).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LookBasis {
    pub point_curves: [ChannelCurve; 4],     // E10 types re-exported via lightbox-edit
    pub color_grade: ColorGrade,
    pub hsl: [HslBand; 8],
    pub vibrance: f32,
    pub saturation: f32,
}

pub struct LookFamily {
    pub id: LookFamilyId,
    pub name: &'static str,                  // panel display: "Teal & Orange"
    pub blurb: &'static str,                 // one-line tooltip
    /// Prior in [0, 1] given the image: how plausible is this family HERE.
    /// (teal-orange → 0 on a near-monochrome image; golden-hour → high when the
    /// hue field masses warm; matte-fade → high on low-contrast scenes; …)
    pub applicability: fn(&ImageStats) -> f32,
    /// THE fitting function: image stats + current values + variation → a patch.
    /// Pure, deterministic, total (never fails; degenerate stats produce a tonal-only grade).
    /// Every emitted value is inside E10's range_of() domain by construction.
    pub fit: fn(&ImageStats, &LookBasis, &Variation) -> LookPatch,
}

/// The v1 registry — static, ordered, ≥ 9 families:
///   teal-orange · film-warm ("golden negative") · film-cool ("northern chrome")
///   bleach-bypass · matte-fade · vivid-chrome · cool-noir · golden-hour · cross-process
/// Adding a family is additive (goldens extended, LOOKS_ENGINE_REV bumped).
pub fn registry() -> &'static [LookFamily];
```

**One worked fitting example (normative for the pattern, not the constants — teal-orange):**
warm pole hue = `skin.mean_hue_deg` clamped to [25°, 55°] when `skin` is present, else the dominant warm swatch hue, else 40°; cool pole = warm pole + 180°, pulled toward [200°, 230°] proportional to `hue.mass_near(210°, 60°)` (use the teal the image *has*, don't invent one); `highlights` wheel ← warm pole at `sat = 12·intensity·(1 − clipped_hi)`; `shadows` wheel ← cool pole at `sat = 16·intensity`; `balance` ← recentered from `tone.pct.p50` so the zone split matches the image's tonal mass; luma curve = gentle S through anchors placed at `black_point`/`p25`/`p75`/`white_point` with toe lift `0.02 + 0.05·fade`; HSL: aqua/blue bands hue-shift toward the cool pole (≤ ±18), orange band **guarded** — when `skin` is present its hue/sat deltas are capped at ±6/±8 (the skin-protection invariant, tested in §7.3). All constants are family-module `const`s reviewed at the perceptual gate (B-phase), never magic numbers inline.

### 3.3 The patch (`lightbox_looks::patch`) — recipe-delta synthesis API

```rust
/// The pinned, enumerable parameter surface a look may touch. E09 ParamIds; E10 semantics.
/// Everything else in the recipe is out of bounds BY TYPE (validation rejects, tests enforce).
pub fn look_param_surface() -> &'static [ParamId];
///   = ToneCurveLuma, ToneCurveR, ToneCurveG, ToneCurveB,
///     HslBand(0..=7), Vibrance, Saturation,
///     GradeShadows, GradeMidtones, GradeHighlights, GradeGlobal, GradeBlending, GradeBalance
pub fn look_subset() -> ParamSubset;         // the same surface as an E09 ParamSubset
                                             // (groups ToneCurve, ColorMixer, ColorGrading)

/// A validated partial recipe: the architecture's `RecipePatch` (§2.2), realized as a
/// newtype over E09's ParamDelta restricted to look_param_surface() with all values
/// inside E10's ranges. THIS is what "partial develop recipe" means concretely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookPatch(ParamDelta);            // constructor-validated; inner delta is read-only

impl LookPatch {
    /// Total validation: rejects any ParamId outside the surface, any out-of-range value,
    /// any non-monotonic curve. fit() output is constructed through this — a family bug
    /// becomes a test failure, never a corrupt recipe.
    pub fn validate(delta: ParamDelta) -> Result<LookPatch, PatchError>;
    pub fn delta(&self) -> &ParamDelta;      // hand to Recipe::apply / EditCommand::ApplyLook
    pub fn is_empty(&self) -> bool;
    /// Perceptual-ish distance between two patches (wheel-angle + curve-area + band L2) —
    /// drives the proposal-distinctness guarantee (§3.4) and its test.
    pub fn distance(&self, other: &LookPatch) -> f32;
    pub fn to_cbor(&self) -> Vec<u8>;        // canonical (sorted keys) — golden + determinism tests
}

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("param {0:?} outside the look surface")] OutOfSurface(ParamId),
    #[error("value out of range for {0:?}")]         OutOfRange(ParamId),
    #[error("curve not strictly increasing in x")]   BadCurve,
}

/// Curve composition utility (§1.3 decision 5): result(x) = look(base(x)), re-fit to
/// ≤ 16 control points, strictly-increasing x preserved, endpoints pinned. Max abs
/// deviation of the refit from the exact composition ≤ 1/512 (tested).
pub fn compose_curves(base: &ChannelCurve, look: &ChannelCurve) -> ChannelCurve;
```

**Why absolute values + composition, not "offsets":** E09's `ParamDelta` carries absolute values (it is the history/preset currency — E09 §3.2). A `LookPatch` therefore stores the *post-composition* values: `fit()` receives the `LookBasis` and bakes the layering in. Undo restores the pre-apply values via the history step's `inverse` delta — E09's normal machinery, nothing new.

### 3.4 Proposals & shuffle seeding (`lightbox_looks::propose`) — the E17.3 model

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LookSeed(pub u64);
impl LookSeed {
    pub fn random() -> LookSeed;             // entropy from std; the ONLY nondeterminism, at the UI edge
    pub fn next(self) -> LookSeed;           // splitmix64(self.0) — the shuffle button
}

/// Own SplitMix64 (~20 lines, no dependency). Committed test vectors (A2).
pub struct Stream(/* u64 state */);
impl Stream {
    /// Per-slot substream: seed ⊕ xxh3_64(family_id.0) ⊕ slot — proposals are independent
    /// of each other and of count (dropping count from 8 to 4 yields a prefix).
    pub fn for_slot(seed: LookSeed, family: LookFamilyId, slot: u32) -> Stream;
    pub fn unit(&mut self) -> f32;           // [0,1)
    pub fn signed(&mut self) -> f32;         // [-1,1]
    pub fn variation(&mut self) -> Variation;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LookProposal {
    pub family: LookFamilyId,
    pub title: String,                       // "Teal & Orange II" (family name + variant ordinal)
    pub seed: LookSeed,                      // the SET's seed (reproducibility handle)
    pub slot: u32,                           // position within the set (with seed → full replay key)
    pub patch: LookPatch,
    pub score: f32,                          // applicability × bias — panel sort order
}

#[derive(Clone, Debug)]
pub struct ProposeOpts {
    pub seed: LookSeed,
    pub count: usize,                        // default 8 (pref looks.count, 4..=16)
    pub families: Option<Vec<LookFamilyId>>, // None = full registry (panel family filter, Should)
    pub bias: Option<FamilyBias>,            // §3.5; None = classical_bias(stats) applied internally
}

/// THE synthesis entry point. Pure + deterministic: same (stats, basis, opts) → identical
/// Vec<LookProposal> including order (the first-failing-test contract). Always returns
/// exactly `count` proposals (degenerate/low-chroma images fill with tonal families —
/// matte-fade / bleach-bypass / cool-noir have nonzero applicability everywhere).
/// Selection: weight(f) = applicability(f)(stats) · bias[f]; families ranked by weight
/// with deterministic tie-break (registry order); slots filled round-robin over the top
/// families (a family may appear ≤ ceil(count / max(3, distinct)) times, each occurrence
/// a different substream variation). Distinctness: pairwise patch.distance ≥ MIN_DISTINCT
/// within a set — enforced by re-rolling a slot's variation (bounded to 4 re-rolls,
/// deterministic because the re-roll consumes the same substream).
pub fn propose(stats: &ImageStats, basis: &LookBasis, opts: &ProposeOpts) -> Vec<LookProposal>;
```

### 3.5 Family bias — classical + optional ML (`lightbox_looks::bias`) — the E17.5 model

```rust
/// Multiplicative per-family selection weights, all in [0, 4]; missing key = 1.0.
/// INVARIANT (tested §7.5): bias influences WHICH families fill slots and their order —
/// never the patch content of a given (family, stats, basis, seed, slot).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FamilyBias(pub BTreeMap<LookFamilyId, f32>);

/// Always available, pure. Heuristics over stats: skin_present → film-warm/teal-orange up,
/// cross-process down; low contrast_index → matte-fade up; dominant warm hue mass →
/// golden-hour up; low avg_chroma → bleach-bypass/cool-noir up; etc.
pub fn classical_bias(stats: &ImageStats) -> FamilyBias;

// ---- cargo feature "ml-bias" (compiled into the app build; runtime-optional) ----

/// Small scene/mood classifier (optional E13 model pack "looks-mood", Apache/MIT weights
/// only — pack choice gated on the E13 license review, task E2). Client-side pre/post
/// (resize/normalize to the model's input; softmax → class-probability → FamilyBias table)
/// per E13's task-adapter pattern, via InferenceClient::run_model. One call per analysis,
/// Foreground priority, 2 s timeout, CancelToken threaded.
///
/// EVERY failure path — inferd absent/crashed/starting, pack not installed, VRAM-gated,
/// timeout, any InferError — resolves to Ok(classical_bias(stats)): the classifier can
/// only improve ordering, never gate the feature (architecture §10.1 E17.5).
pub async fn mood_bias(client: &dyn InferenceClient, px: &DecodedPreview,
                       stats: &ImageStats, cancel: CancelToken) -> FamilyBias;
```

### 3.6 Core surface (`lightbox-core`) — session service, command, events

```rust
// Session accessor, the previews()/engine()/edits() pattern:
impl Session {
    #[cfg(feature = "ai-looks")]
    pub fn looks(&self) -> Arc<LooksHub>;
}

pub struct LooksHub { /* per-image LooksState, stats cache, engine + previews + edits handles */ }

#[derive(Clone, Debug)]
pub enum LooksPanelState {
    Idle,                                        // panel closed / never opened for this image
    Analyzing,                                   // E06 job in flight (Foreground class)
    Ready { seed: LookSeed, proposals: Vec<ProposalCard> },
    Unavailable { reason: LooksUnavailable },    // NoPreviewYet | AnalyzeFailed(String)
}

#[derive(Clone, Debug)]
pub struct ProposalCard {
    pub family: LookFamilyId, pub title: String, pub seed: LookSeed, pub slot: u32,
    pub thumb: ThumbState,                       // Pending | Ready(ThumbPixels) | Failed
    pub applied: bool,                           // this exact (seed, slot) was the last ApplyLook
}

impl LooksHub {
    /// Idempotent. Kicks (or reuses) the analysis job for the image's CURRENT recipe_rev,
    /// then proposes with the session seed (initial = LookSeed::random()) and submits
    /// thumbnail renders. Never blocks; state transitions surface via events.
    pub fn open(&self, image: ImageId);
    /// Cancels in-flight thumbnail renders and the analysis job. Frees thumb buffers.
    pub fn close(&self, image: ImageId);
    pub fn state(&self, image: ImageId) -> LooksPanelState;
    /// seed = seed.next(); re-proposes (re-analyzes first iff recipe_rev moved); re-renders thumbs.
    pub fn shuffle(&self, image: ImageId);
    pub fn set_seed(&self, image: ImageId, seed: LookSeed);   // reproduce a known set (CLI/QA)
    /// Submits Command::Edit(ApplyLook{..}) for the slot's patch. Returns the pending label
    /// so the panel can mark the card; the durable truth arrives via EditCommitted.
    pub fn apply(&self, image: ImageId, slot: u32) -> Result<(), LooksError>;
}

// lightbox-edit (E09-owned enums; additive variants, joint review):
pub enum EditCommand {
    /* … E09 variants unchanged … */
    /// Dispatcher arm mirrors ApplyPreset exactly: validate LookPatch → Recipe::apply →
    /// ONE WAL txn → ONE history_step (delta + inverse) → EditCommitted. Single undo
    /// removes the whole look (the mandate's hard requirement).
    ApplyLook { image: ImageId, patch: ParamDelta /* re-validated as LookPatch in the arm */,
                family: String, seed: u64, slot: u32 },
}
pub enum StepLabel {
    /* … E09 variants unchanged … */
    Look { family: String, seed: u64, slot: u32 },   // provenance/display ONLY (§1.3 decision 4)
}

// lightbox-core events (additive on the #[non_exhaustive] Event):
pub enum Event {
    /* … existing … */
    LooksProposalsReady { image: ImageId },                  // state() now Ready (thumbs pending)
    LooksThumbReady     { image: ImageId, slot: u32 },
    LooksStateChanged   { image: ImageId },                  // Analyzing/Unavailable transitions
}
```

**Thumbnail pipeline (E17.4, inside `LooksHub`):** for each proposal, clone the working recipe (`EditHub::working_recipe`), `recipe.apply(patch.delta())` (E09 validates/clamps — the *same* merge the apply path runs, so the preview is exactly what applying produces), then `Engine::submit(RenderRequest { recipe: merged, roi: full frame, scale: RenderScale::Fit(thumb_extent /* pref looks.thumb_px, default 192 */), target: RenderTarget::Buffer { format: srgb8 }, priority: Batch, cancel })`. One in-flight render per slot, latest-wins on shuffle; all tickets cancelled on `close()`/image switch/set replacement. **Cache economics (why 8 thumbs are cheap):** the proposals differ only in `tone_curve`/`hsl`/`color_grade` params, so E05's content-keyed cache reuses every upstream node (decode → WB → exposure → recovery) across all 8 renders — only the pointwise tail re-executes per thumb, at 192 px. Asserted by the E05 recompute-count probe in C3's AC.

### 3.7 Shell panel (`lightbox-shell`, feature `ai-looks`)

```rust
// develop/panels/ai_looks.rs — registered iff cfg(feature = "ai-looks") && prefs.looks.enabled
PanelDef {
    id: PanelId("develop.ai-looks"),
    title: "AI Looks",
    source_req: SourceReq::Any,          // universal stages only — identical on raw & JPEG (§2.4)
    order: /* after E10's creative-looks panel slot */,
    build: ai_looks_panel,
}
```

Panel behavior (E08 idioms, no new widget frameworks): 2-column thumbnail grid of `ProposalCard`s (placeholder shimmer while `Pending`, family title caption, applied checkmark); **Shuffle** button (and `looks.shuffle` keymap action) → `LooksHub::shuffle`; small seed readout with click-to-edit (power users / bug reports: "seed 0x1B3… slot 4" reproduces exactly); click a card → `LooksHub::apply`; states: `Analyzing` spinner row, `Unavailable` inline notice with retry, engine-busy = cards stay `Pending` (never blocks the canvas). Opening the panel calls `open`; collapsing it calls `close` (frees GPU-side thumb work). **The panel never renders to the main canvas** — v1 has no hover-preview-on-canvas (OQ3 names it as a Should for later; it would ride E09's `preview_recipe` + the render scheduler, no new seam).

### 3.8 CLI (`lightbox-cli`)

```
lightbox-cli looks analyze <file> [--json]                       # ImageStats dump
lightbox-cli looks propose <file> --seed <u64> [--count N] [--json]   # proposals incl. patch params
lightbox-cli looks apply   <file> --seed <u64> --slot <i>        # apply → edit store; prints step seq
```

Headless (no GPU required — analyze/propose are pure; apply writes the store; no thumbnails in CLI). This is the determinism-test and golden-generation harness (§7).

---

## 4. Design decisions & data flow

### 4.1 Analyzer placement — CPU-side over preview pixels, never a DAG node (restating §10.1 E17.1, made concrete)

The architecture already made this call; the concrete flow: `LooksHub::open` → `PreviewService::best_available(image, 384)` → if none, request a T1 build (`BuildPriority::Visible`) and sit in `Analyzing` until `PreviewEvent::Ready` (the §6 "core editing never blocks" posture: no preview yet just delays *proposals*, nothing else) → `open_pixels(desc, Some(512))` → `analyze()` on an E06 **Foreground** job (sub-second, user-initiated, cancellable) → stats cached in the hub keyed `(image, recipe_rev, ANALYZER_REV, desc.variant)`. Where E10's histogram already exists for the develop UI it is *not* reused: it is display-companion-encoded, 256-bin luma only — the analyzer needs OkLab palette/hue data anyway, and duplicating a 64-bin luma pass over 150k samples is microseconds. (The §2.2 sketch's "reads engine-computed stats where available" is satisfied vacuously at M3; if E10's histogram later grows OkLab moments, swap the source behind `analyze()` — nothing upstream changes.)

### 4.2 Determinism & provenance — the reproducibility contract, stated once

- **Session determinism (tested):** `propose(stats, basis, opts)` is a pure function. Same decoded pixels + same `ANALYZER_REV` → bit-identical `ImageStats`; same stats + seed + count + bias → identical proposals, byte-identical canonical CBOR (within a platform; cross-platform within 1e-4 tolerance, §1.3 decision 9).
- **What determinism is NOT promised across:** preview *tier/variant* changes (an embedded-preview analysis vs a rendered-preview analysis may differ slightly — `StatsBasis` records which), `ANALYZER_REV`/`LOOKS_ENGINE_REV` bumps (goldens re-blessed by the same PR), and edits to the image between shuffles (recipe_rev moved → re-analysis is *correct*, not drift).
- **The applied artifact is self-contained.** `ApplyLook` persists concrete param values through E09's normal path. Rendering, XMP projection (`crs:ColorGrade*`, `crs:ToneCurvePV2012*`, HSL fields — E10's existing mapping rows; **zero new XMP work**), export, history replay — none of them know E17 exists. Deleting `lightbox-looks` from the build tomorrow leaves every applied look rendering identically forever. This is the recipe-side mirror of §4.5's baked-raster rule and the reason rollback is trivial.

### 4.3 Composition semantics (layering on an existing grade)

`fit()` composes over `LookBasis`: curves via `compose_curves` (functional composition, refit ≤ 16 points); grade wheels via vector addition in (a,b)-like wheel space (hue/sat → cartesian, add, clamp sat to range, back to hue/sat); HSL band and vibrance/saturation deltas additive with clamping to E10 ranges. Consequences, stated honestly: applying look B after look A compounds (B was *fitted to* the A-graded preview — coherent, but two history steps deep); the panel marks the last-applied card and the release-notes/UX copy say "one look at a time; undo before trying another" while OQ2 tracks a possible "replace last look" affordance. Repeated apply of the *same* card is idempotent-ish only for pure-curve families and is not special-cased — undo is the contract.

### 4.4 Failure modes (per architecture §6 discipline)

| Failure | Detection | Behavior |
|---|---|---|
| No preview yet (fresh drop, cold cache) | `best_available` = None | `Analyzing` state; T1 requested Visible-priority; proposals appear when Ready. Canvas/editing unaffected |
| Analyze fails (tiny/degenerate image) | `AnalyzeError` | `Unavailable { AnalyzeFailed }` chip + retry; `Degenerate` downgrades to tonal-family-only proposals instead of failing where possible |
| Engine busy / device-lost during thumbs | `RenderState::Failed` / E05 events | Cards stay `Pending`/`Failed` individually; **apply never needs a render** — a user can apply an un-thumbed proposal; CPU-fallback engine renders thumbs at its own pace (§4.4 degraded contract) |
| inferd absent / crash / VRAM-gated / timeout | `mood_bias` error paths | Silent classical bias (a one-line tracing event, no UI error — ML here is an *enhancer*, §10.1 E17.5) |
| Shuffle spam | ticket latest-wins | In-flight thumb renders cancelled per slot; analysis reused (stats keyed by recipe_rev) |
| `ApplyLook` on a stale slot (post-shuffle race) | hub validates (seed, slot) against current set | Typed no-op error; panel refreshes |
| Working set replaced / image closed | `close()` on session events | All jobs + tickets cancelled; hub state dropped (session-only, nothing persisted) |

### 4.5 Feature flag & rollback (restating the architecture's deployment note as mechanics)

Cargo feature `ai-looks` on `lightbox-shell`/`lightbox-core` (default per OQ1); runtime pref `looks.enabled` (E08 prefs panel checkbox under "Labs"). Rollback = flip the feature off (or revert the crate + wiring PRs): **no migrations to unwind** (E17 ships none — §5), no recipe-schema change (patches use existing E10 fields), applied looks remain valid ordinary recipes. Blast radius of a total E17 failure: one develop panel.

---

## 5. Data-model additions

**Catalog migrations: NONE.** This is a deliberate, CI-asserted property (T-F3): E17 must leave `schema_version` and every table untouched. The additions are:

| Addition | Where | Nature |
|---|---|---|
| `EditCommand::ApplyLook { image, patch, family, seed, slot }` | `lightbox-edit`/`lightbox-core` (E09-owned enums) | additive variant on `#[non_exhaustive]` enum; dispatcher arm mirrors `ApplyPreset` (one txn, one step) |
| `StepLabel::Look { family: String, seed: u64, slot: u32 }` | `lightbox-edit` | additive variant; serializes into `history_step.op` CBOR like every label; provenance/display only |
| `Event::{LooksProposalsReady, LooksThumbReady, LooksStateChanged}` | `lightbox-core` | additive variants on `#[non_exhaustive] Event` |
| Prefs keys: `looks.enabled: bool` (default = feature default), `looks.count: u8 = 8` (4..=16), `looks.thumb_px: u16 = 192`, `looks.ml_bias: enum { Auto, Off } = Auto` | E08 prefs store (`prefs.toml`) | registered keys; E08 binds UI |
| Optional model pack `looks-mood` (classifier ONNX, `kind = "model"`) | E13 `model_pack` registry + weights manifest | **no schema change** — rides E13's existing tables/gates; ships only if weights clear the license review (E2); its absence costs nothing |
| §8 surface-3 data-manifest entry for the family parameter tables | `docs/…/data-manifest` | family constants are Lightbox-authored original content compiled into the binary; the entry documents provenance (satisfies the audit even though it is code-embedded data) |
| History/undo storage | — | **unchanged**: an applied look is `history_step { op: Look…, delta, inverse }` — E09's existing shape |

Nothing E17-related persists outside an applied look's ordinary history step: stats, proposals, seeds, and thumbnails are session-transient by design (architecture: "look proposals are transient until the user applies one").

---

## 6. Ordered task breakdown

Every task ≤ 1 engineer-day; AC = acceptance criteria. Order within a phase is dependency order. Phase A/B are pure-crate work and can start the moment E09's param vocabulary (E09 T1) and E10's param types exist (both are M1/M2 deliverables — satisfied well before M3). Phases C/D/E in order; F closes. One generalist engineer (per §10.1-preamble: E17 adds no GPU-engineer requirement); ~26 dev-days ≈ 6–8 pw with review/perceptual-iteration slack.

### Phase A — crate scaffold + analyzer (E17.1)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | `lightbox-looks` crate scaffold: workspace member, deps per §2 (assert **no** wgpu/rusqlite/tokio in `cargo tree`), `OkLab` conversion (own matrix math or `lightbox-color` re-export — resolve which at PR), error types | `cargo deny` green; OkLab golden vectors (12 committed sRGB↔OkLab pairs incl. primaries/greys) pass to 1e-5; `cargo tree -p lightbox-looks` contains no GPU/db/async crates |
| A2 | Deterministic sampler + tone stats: fixed-stride subsample (≤ 150k), 64-bin L histogram, exact percentiles, black/white point, contrast index, clip fractions. Plus `Stream` (SplitMix64) with committed test vectors | same buffer twice → bit-identical `ToneStats`; SplitMix64 matches 8 published reference vectors; stride independence documented (chosen stride is part of ANALYZER_REV) |
| A3 | Seeded k-means palette: k-means++ init from content-derived seed, deterministic tie-breaks, ≤ 20 iters, ≤ 1%-weight merge; swatch ordering (weight-desc, hue-asc) | determinism: 100 runs on one fixture → identical swatches; sanity: 3-color synthetic image → 3 swatches within 1e-3 of truth; perf: ≤ 60 ms on 150k samples, 1 core (criterion) |
| A4 | Hue field + temp/tint estimate + skin detector (fixed OkLab ellipsoid, constants tuned on fixture portraits) | portrait fixtures → `skin = Some` with mean hue in [20°, 50°]; landscape/no-people fixtures → `None`; hue-field mass sums to 1 ± 1e-4 |
| A5 | `analyze()` façade: `StatsBasis`, TaggedIcc→sRGB via LCMS2, `ANALYZER_REV`, error paths; no-GPU assertion; serde (CBOR/JSON) for `ImageStats` | first-failing-test *analyzer half* green: stable stats on the fixture set; a test wgpu-device counter proves zero device acquisition; total analyze ≤ 100 ms @ 512 px on 1 core (nightly bench); `ImageStats` JSON snapshot committed per fixture |

### Phase B — patch, families, proposals (E17.2 + E17.3)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `LookPatch` + `look_param_surface()`/`look_subset()` + `validate` + canonical CBOR + `distance` | proptest: any `ParamDelta` with an out-of-surface id or out-of-range value is rejected; valid patches round-trip CBOR byte-identically; surface list is an exhaustive-match doctest against E09's `ParamId` registry (drift fails the build) |
| B2 | `Variation` derivation from `Stream::for_slot`; `LookSeed::next`; slot independence | same (seed, family, slot) → identical `Variation` across 1k runs; slots 0..16 pairwise-differ; `count` prefix property holds (n=4 set == first 4 of n=8 set) |
| B3 | `compose_curves`: functional composition + ≤ 16-point refit, monotonic-x preserved, endpoints pinned | goldens for 6 canned compositions; refit max abs deviation ≤ 1/512 (property-tested over random monotone curves); identity∘x == x |
| B4 | Families 1–3: **teal-orange**, **film-warm**, **bleach-bypass** — fit fns per the §3.2 worked pattern, constants as reviewed module consts | anchoring asserts: teal-orange warm pole within 10° of `skin.mean_hue_deg` on portrait fixtures; skin-guard cap holds (orange-band hue delta ≤ 6 when skin present); every fixture × 16 variations → `LookPatch::validate` green |
| B5 | Families 4–6: **matte-fade**, **vivid-chrome**, **cool-noir** | matte-fade toe anchors to measured `black_point` ± 0.02; all-fixture × variation validation green; applicability fns return sane priors on the fixture matrix (snapshot test) |
| B6 | Families 7–9: **film-cool**, **golden-hour**, **cross-process** + registry assembly + `LOOKS_ENGINE_REV` | registry length ≥ 9; ids unique; every family total (never panics) on the degenerate/mono fixture — emits tonal-only or near-identity patch |
| B7 | `propose()`: weighting, deterministic selection, round-robin slot fill, distinctness re-roll, `classical_bias` | **the first failing test goes green** (propose half): identical lists on repeat; different seeds → ≥ 90% of slots differ (fixture matrix); pairwise `distance ≥ MIN_DISTINCT` within every proposed set; low-chroma fixture yields no teal-orange/vivid slots |
| B8 | Golden-proposal harness + corpus: ≥ 6 fixtures (portrait/skin, blue-sky landscape, sunset, night urban, overcast low-contrast, high-key; reuse E02/E05 golden corpus files where suitable) × 3 seeds → committed golden JSON (params) | CI job compares within 1e-4 tolerance; any drift fails unless `ANALYZER_REV`/`LOOKS_ENGINE_REV` bumped in the same PR (mirrors the §8 golden-image policy); regeneration is one `cargo xtask bless-looks` away |

### Phase C — core integration: apply path + thumbnails (E17.4)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | E09 additive variants (`EditCommand::ApplyLook`, `StepLabel::Look`) + dispatcher arm (validate as `LookPatch` → `Recipe::apply` → one txn → one step) — **joint-review PR with the E09 owner** (frozen-surface rule) | apply → exactly one `history_step` with correct label CBOR; `Undo` restores struct-equal recipe; `Redo` re-applies; `kill -9` mid-apply leaves store `integrity_check`-clean (E01 fault harness); after apply, editing `GradeShadows` in the E10 panel behaves identically to hand-set values (integration test drives both paths, compares recipes) |
| C2 | `LooksHub` skeleton: state machine, stats cache keyed `(image, recipe_rev, ANALYZER_REV, variant)`, analysis as Foreground E06 job, preview acquisition incl. the no-preview wait path, events | headless: open → `Analyzing` → `Ready` event sequence; second open is a cache hit (no second job — instrumented); cancel-on-close verified; `Unavailable` on a corrupt fixture |
| C3 | Thumbnail pipeline: merged-recipe `RenderRequest`s (Buffer target, Batch, `Fit(192)`), per-slot latest-wins, cancellation on shuffle/close | 8 thumbs complete on the reference machine ≤ 600 ms warm (nightly perf); E05 recompute-count probe shows upstream nodes evaluated **once** across the set (the cache-sharing claim, asserted); device-lost injection → thumbs `Failed`/retried, session editing unaffected |
| C4 | Shuffle/seed management: `shuffle` (seed.next + re-propose + re-render), `set_seed`, re-analysis iff recipe_rev moved, stale-slot apply guard | shuffle twice → distinct sets; set_seed reproduces a prior set exactly (thumbs byte-equal on same backend); edit-then-shuffle re-analyzes (instrumented); stale (seed,slot) apply → typed error |
| C5 | CLI subcommands `looks analyze/propose/apply` + e2e script | headless script: open fixture → `propose --seed 42` json matches golden → `apply --slot 0` → `edit history` shows one Look step → `undo` → recipe neutral again; malformed args exit 2 (E01 CLI conventions) |

### Phase D — the panel (shell, feature-flagged)

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | Panel scaffold behind `cfg(feature = "ai-looks")` + pref gate: `PanelDef` registration, card grid with shimmer placeholders, state rendering (Analyzing/Unavailable/Ready) | egui_kittest: flag off → panel id absent from `PanelHost`; flag on + pref off → absent; on+on → present for raw AND jpeg fixtures (`SourceReq::Any`); state transitions render without panic on event replay |
| D2 | Interactions: card click → apply (with applied-checkmark from `EditCommitted`), Shuffle button + `looks.shuffle` keymap action, seed readout with click-to-edit | kittest: click card → exactly one `Command::Edit(ApplyLook)` submitted; shuffle → hub called once per click (no double-fire); seed entry of a recorded value reproduces titles/family order |
| D3 | Thumb texture upload + lifecycle (upload on `LooksThumbReady`, free on close/collapse), panel open/close ↔ `LooksHub::open/close` wiring | no texture leak across 100 shuffle cycles (egui texture-count probe); collapsing the panel cancels in-flight renders (engine ticket probe); reopening restores the same seed's set from hub state without re-analysis |
| D4 | Prefs UI rows (`looks.enabled`, `looks.count`, `looks.ml_bias`) in E08's prefs panel + non-modal error chips + empty-state copy | prefs round-trip `prefs.toml`; count change applies on next shuffle; all copy strings centralized (l10n-ready, house rule) |

### Phase E — optional ML bias (E17.5)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | `classical_bias` heuristic table hardening + snapshot tests over the fixture matrix (this also finalizes each family's `applicability`) | bias snapshots committed per fixture; perceptual-review sign-off recorded for the default ordering on the 6 fixtures (§7.4 board) |
| E2 | `mood_bias` adapter: pre/post-processing over `InferenceClient::run_model`, class→bias mapping table; **model-pack selection + license review request filed** (Apache/MIT weights only, E13 gate) — shipping the pack is a separate go/no-go, not this task | with a **mock** InferenceClient returning canned logits: bias reflects the mapping table; timeout/`InferError`/absent-client paths all return `classical_bias` (each path unit-tested); no panic on malformed tensor shape |
| E3 | Runtime wiring: `looks.ml_bias = Auto` probes `MlService::status()` once per session (never spawns inferd eagerly — E13's lazy-start discipline), threads `mood_bias` into `LooksHub` analysis flow | **feature-loss test:** inferd absent → full `count` proposals, classical order (integration); **invariance test:** for fixed (seed, family, slot), patch CBOR is byte-identical with bias on/off (the §1.3-6 invariant); ML failure emits one tracing event, zero UI errors |

### Phase F — hardening, CI, docs, rollback drill

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | Cross-platform determinism: golden-proposal job wired into the 3-OS CI matrix (tolerance compare); nightly perf benches (analyze/propose/thumb budgets §8) registered | goldens green on macOS/Windows/Linux from one committed set; per-platform byte-identity job green (same-platform reruns); budget regressions file nightly issues (non-blocking, §8 policy) |
| F2 | Docs: `docs/develop/look-families.md` (family authoring guide: axes, anchoring patterns, the skin guard, how to bless goldens), param-surface reference, §8 surface-3 manifest entry for family tables | docs build; manifest policy checker green; a new-family checklist exists (the content line for future families) |
| F3 | Rollback/flag drill + zero-migration assertion: build & test matrix with `ai-looks` off; schema-hash assertion in CI (catalog schema before/after E17 merge is identical); release-notes copy for the flag | flag-off build: workspace green, panel absent, `lightbox-looks` absent from the shell binary (symbol check); schema hash unchanged; drill documented in the PR |

---

## 7. Test plan

Per architecture §8 layers; everything PR-blocking except the marked nightly benches.

### 7.1 Determinism (the epic's named test axis)

- **Same seed + same image → same look** (the M3 exit phrasing): `propose` twice on identical stats/opts → identical proposal vectors, byte-identical canonical CBOR per patch (A5/B7; the first failing test).
- Analyzer determinism: repeated `analyze()` on one buffer → bit-identical `ImageStats` (100-run loop, A3/A5).
- Seed sensitivity: different seeds → ≥ 90% differing slots; `LookSeed::next` chain of 100 seeds → 100 distinct sets (B7).
- Slot/count independence: n=4 is a prefix of n=8 (B2).
- Cross-platform: one committed golden set compared at 1e-4 tolerance on all three CI OSes; byte-identity asserted only within-platform (F1; decision §1.3-9).

### 7.2 Golden recipes over fixture images (the anti-drift gate)

Corpus (B8): ≥ 6 fixtures spanning skin/landscape/sunset/night/low-contrast/high-key × 3 seeds × full registry → committed JSON of every proposal's params. CI recomputes and compares (1e-4). Drift fails unless `ANALYZER_REV` or `LOOKS_ENGINE_REV` is bumped in the same PR with a blessed regeneration — the exact policy shape of the §8 golden-image/process-version guard, applied to *synthesis* instead of pixels. (Pixel rendering of applied looks needs no new goldens: an applied look is ordinary E10 params, already covered by E10's per-node and per-PV goldens.)

### 7.3 Property & unit

- proptest: `LookPatch::validate` total over arbitrary `ParamDelta`s; every registry family × arbitrary valid `ImageStats` (generated) × arbitrary `Variation` → `fit` never panics, always validates (B4–B6).
- Apply/undo round-trip over random valid base recipes: apply patch → undo → struct-equal base (C1, riding E09's harness).
- `compose_curves` monotonicity + deviation bound (B3); skin-guard cap under skin-present stats (B4); wheel/band clamping at range edges.
- Patch-surface freeze: exhaustive-match doctest ties `look_param_surface()` to E09's registry — adding a recipe param can never silently enter the look surface (B1).

### 7.4 Perceptual gate (quality, the honest one)

Like E10's boards: a review pass over the fixture corpus × default-seed sets, recorded sign-off before M3 exit (E1). Criteria: no proposal produces clipped skin, banding, or out-of-gamut garishness at default intensity; every set contains ≥ 3 visually distinct directions. This is a human gate — the risk register (§10 R1) owns its iteration budget.

### 7.5 ML-path invariants

- **Feature-loss:** inferd absent/mocked-dead → full proposal set, classical order (E3) — the architecture's E17.5 acceptance line verbatim.
- **Bias-only:** patch bytes for fixed (family, stats, seed, slot) identical with bias on/off (E3).
- Adapter robustness: timeout, crash-mid-call (supervisor restart), malformed output tensor, VRAM-gate refusal → all resolve to classical bias, no user-facing error (E2).

### 7.6 Integration / E2E / CI

- CLI e2e (C5): propose → apply → history → undo, headless, in the PR-blocking fast subset.
- Shell kittest flows (D1–D3): flag gating, click-to-apply single-command discipline, texture lifecycle.
- `kill -9` mid-`ApplyLook` → store `integrity_check` clean (C1, E01 fault-injection harness).
- Zero-migration assertion + flag-off build in CI (F3).
- Thumbnail cache-sharing probe and budgets (C3, nightly).

---

## 8. Performance budgets (E17-owned; nightly-asserted via F1)

| Operation | Budget | Notes |
|---|---|---|
| `analyze()` @ 512 px | ≤ 100 ms, 1 core | A5 bench; k-means dominates (≤ 60 ms, A3) |
| `propose()` n=8 | ≤ 10 ms | pure arithmetic |
| First thumbnail visible | ≤ 250 ms after `Ready` (warm engine) | Batch priority — must not touch the < 100 ms interactive canvas budget (§7 architecture) |
| Full 8-thumb set | ≤ 600 ms warm, reference GPU | upstream nodes shared via E05 cache (C3 probe) |
| Panel closed / flag off | **zero** E17 CPU/GPU work | instrumented in D3/F3 |
| Memory | stats ≤ 50 KB/image; thumbs ≤ 8 × 192² RGBA8 ≈ 1.2 MB, freed on close | session-transient |

Slider interactivity is untouched by construction: E17 renders only Batch-priority buffers and runs analysis on a Foreground job — it is never in the Interactive path.

---

## 9. Seams to neighboring epics (named, not designed here)

| Epic | E17 consumes | E17 provides / touches |
|---|---|---|
| **E09** | `ParamDelta`/`ParamSubset`/`ParamId`, `Recipe::{apply, extract, get}`, `EditHub::working_recipe`, dispatcher/txn/history machinery, `Event::EditCommitted` | two additive enum variants (joint-review PR C1); the `preview_recipe` note at E09 §3.7 ("a proposed look IS a DevelopPreset-shaped delta") is honored: a `LookPatch` is exactly that shape, restricted |
| **E10** | `GlobalStages` field semantics + `range_of()` domains for curves/HSL/grade; the rendering of every param a look sets; existing `crs:` XMP mapping rows | nothing — E17 adds no nodes, no params, no panels to E10 |
| **E03** | `best_available` / `open_pixels` / `PreviewRequest` (Visible) / `PreviewEvent::Ready` | nothing persistent; read-only consumer |
| **E05** | `Engine::submit/poll/cancel`, `RenderTarget::Buffer`, `RenderScale::Fit`, Batch priority, content-keyed cache behavior | thumbnail load (bounded, cancellable); the C3 recompute probe is a consumer of E05's stats API |
| **E06** | `spawn(Class::Foreground, …)`, `CancelToken` | analysis/thumb jobs visible in the activity model |
| **E08** | `PanelDef`/`PanelHost`, prefs store, keymap registry, kittest harness | one panel, three prefs keys, one keymap action |
| **E13** | `InferenceClient::run_model`, `ModelSel::Latest`, `RunOpts`, `MlService::status`, `PackManager` + license gates | one optional model-pack listing (`looks-mood`); zero protocol changes |
| **E15/E16** | — | nothing: applied looks are ordinary recipes; export/XMP interop see plain E10 params |

---

## 10. Risks & open questions

### Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | **Aesthetic quality** — fitted grades look mediocre or samey on real photos (the actual product risk; determinism and plumbing are easy, taste is not) | High | The perceptual gate (§7.4) with an explicit iteration budget inside E17.2's ~2 pw; anchoring assertions keep fits image-relative; distinctness floor keeps sets varied; family constants are data-like consts revisable post-M3 with a `LOOKS_ENGINE_REV` bump and golden re-bless — **no process-version implications ever** (E17 changes proposals, never the rendering of applied edits) |
| R2 | Cross-platform float drift breaks golden determinism | Medium | Tolerance goldens across platforms + byte-identity only within-platform (§1.3-9); no `fast-math`; SplitMix64 is integer-exact |
| R3 | E09 frozen-surface coordination (two enum variants + dispatcher arm on E09-owned code) | Medium | Additive-only on `#[non_exhaustive]` enums; joint-review PR (C1) per the E01 frozen-surface rule; fallback if E09 owner rejects the command variant: route through `ApplyPreset` with a synthesized transient preset — same txn/step shape, worse label fidelity (named, not preferred) |
| R4 | Skin damage from hue-shifting families (the classic teal-orange failure) | Medium | Skin detector + per-family orange-band guard caps (B4 AC); portrait fixtures in the golden corpus; perceptual gate criterion |
| R5 | Compounding looks confuse users (apply-on-apply, §4.3) | Low–Med | Applied-card marking, single-undo contract, UX copy; OQ2 tracks a replace-last-look affordance |
| R6 | `looks-mood` weights with acceptable license/quality may not exist at M3 | Low | The classifier is optional by architecture; ship classical-only (zero feature loss, tested); pack can land in any later release without code changes (E2's adapter is model-pack-driven) |
| R7 | Thumbnail load starves interactive renders on weak GPUs | Low | Batch priority + E05 time-slicing (§5.3); 192 px renders with shared upstream; budgets nightly-asserted (§8) |

### Open questions (named, non-blocking to start)

- **OQ1:** Release default for the `ai-looks` flag at M3 (dev builds: on). Owner: product/CTO at M3 exit review. Spec assumption: ships flag-on-by-default only after the §7.4 perceptual gate passes.
- **OQ2:** "Replace last look" affordance (undo-then-apply as one action) vs plain undo. UX call; mechanics trivial (hub knows the last Look step seq). Default: not in v1.
- **OQ3:** Hover-preview on the main canvas (render merged recipe full-size on card hover). Rides existing seams (`preview_recipe` + scheduler); deferred as a Should — thumbnail fidelity is already exact-by-construction.
- **OQ4:** Family filter chips in the panel (`ProposeOpts::families` is already plumbed). Default: not in v1.
- **OQ5:** Should `StatsBasis`/seed be recorded into `lb_extra` on apply for cross-session provenance display? Current decision (§1.3-4) is no — history label only; revisit if users ask "which look was this?" across machines (XMP carries history labels never, `lb_extra` would).

---

## 11. Definition of done

1. **M3 exit line (architecture §9), demonstrably:** on a mid-range laptop, offline, no inference host installed — open a raw, open the AI Looks panel: **≥ 8 image-fitted cinematic proposals** appear with thumbnails; **Shuffle** produces a new coherent set; **applying one produces an ordinary, fully-editable recipe undoable in a single step**; every wheel/curve/HSL value the look set is live and adjustable in the E10 panels, indistinguishable from hand-set values.
2. **Determinism:** same seed + same image → same look, CI-enforced (first failing test green; §7.1 suite green on the 3-OS matrix).
3. **Golden recipes:** the fixture-corpus golden-proposal gate is PR-blocking with the rev-bump bless policy (§7.2).
4. **Never a baked filter:** the acceptance test of §10.1 E17.4 passes — post-apply panel editing behaves identically to hand-set values; one undo removes the whole look; export/XMP/e2e see only ordinary E10 params.
5. **Classical-first honored:** with `lightbox-inferd` absent, the full feature works with no loss (§7.5 feature-loss test green); ML bias, when present, is selection-only (invariance test green).
6. **Additive & rollback-clean:** zero catalog migrations (CI schema-hash assertion); `ai-looks` off → panel absent, workspace green, no `lightbox-looks` symbols in the shell binary; all E09/core changes are additive variants under joint review.
7. **Budgets:** §8 rows green on the nightly reference machine; the interactive canvas budget is unaffected with the panel open (probe in C3).
8. **License surfaces:** no new third-party code deps (surface 1 unchanged); no bundled model (surface 2 unchanged unless `looks-mood` clears review through E13's existing gate); family-table provenance entry in surface 3.
9. Docs: family authoring guide + param-surface reference merged; CLI subcommands documented in the CLI help; release-notes copy for the flag drafted.
