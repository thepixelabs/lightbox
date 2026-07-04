# E12 — Masking, local adjustments & retouch

_Implementation spec. Milestone **M3**. Effort **XL ~10–14 pw** (VHigh — the biggest pixel-engine lift after demosaic; Risk 4). Depends on **E05** (render node-graph engine), **E10** (global develop toolset). Planned per architecture §10.1 (phases E12.1–E12.4), §4.1 (LOCAL + retouch stages), §3.1/§3.2 (edit-state ownership), §4.5 (baked-raster replay), §7 (budgets + the 30-mask rewrite trigger)._

_Author: staff engineer, pixel-engine stream. This spec stays inside E12; every neighboring dependency is a **named seam** (§9 below), not a design._

---

## 1. Scope

E12 delivers the complete **local-editing system**: the unified mask model, all non-AI mask component evaluators on GPU (+CPU parity), boolean compositing, the per-mask adjustment pipeline composited into the §4.1 `LOCAL` stage, mask management UI (panel, overlays, pins, gizmos), and the non-destructive retouch system (clone, heal, content-aware remove via own PatchMatch, red-eye, visualize spots) as editable, persisted objects.

**In scope (feature-catalog §2.6 mapping):**

| Deliverable | Catalog priority | Phase |
|---|---|---|
| Unified mask model: named masks of composable components; add/subtract/intersect | Must | E12.1 |
| Brush component (size/feather/flow/density, erase, tablet pressure) | Must | E12.1 |
| Linear + radial gradient components | Must | E12.1 |
| Luminance range + color range components (samplers, falloff/refine) | Must | E12.1 |
| `AiSeg` component **slot**: baked-raster replay, staleness flag, warp-to-frame (§4.5) — inference itself is E14 | Must (slot) | E12.1 |
| Full per-mask adjustment set (Tone / Color / Presence / Detail) blended into the global pipeline | Must | E12.2 |
| Sequential mask-order compositing; 30-mask stress case + §7 fuse rewrite trigger | Must | E12.2 |
| Mask management (rename/duplicate/duplicate-inverted/invert/hide/delete/reorder/convert-op) | Must | E12.3 |
| Overlay modes (color overlay, color-on-B&W, image-on-B/W, white-on-black) + component hover preview | Must | E12.3 |
| On-canvas pins + gizmos (linear 3-line, radial handles, brush cursor, range samplers) | Must | E12.3 |
| Per-mask Amount slider | Should (named in M3 exit) | E12.3 |
| Non-destructive heal/clone spot system (editable objects, movable source, auto-source search) | Must | E12.4 |
| Heal mode (OpenCV `seamlessClone`, mixed-gradient Poisson) | Must | E12.4 |
| Clone mode (feathered alpha composite) | Must | E12.4 |
| Content-aware Remove — **own PatchMatch** backend, baked + replayed (LaMa upgrade = E16 seam) | Should | E12.4 |
| Red-eye correction (editable object, pupil-size/darken) | Should | E12.4 |
| Visualize Spots (thresholded edge-map view mode) | Should | E12.4 |
| Mask + retouch serialization: catalog tables, recipe refs, `lb:` XMP, `crs:` gradient/retouch export mapping | Must (M3 exit) | E12.5 |
| Copy/paste + sync payload semantics for masks/retouch (merge-or-replace) | Should | E12.5 |

## 2. Explicit non-goals

- **AI segmentation execution** — Select Subject/Sky/Background/Objects, prompting UI, SAM2/BiRefNet/SegFormer integration, recompute execution: **E14**. E12 ships the `AiSeg` component kind, its baked-raster replay evaluator, the `stale` flag semantics, and the disabled-when-absent UI affordance only.
- **`lightbox-inferd` / model packs / VRAM gating**: **E13**.
- **LaMa neural fill**: **E16** (slots into `RetouchMode::Remove` behind the `RemoveBackend` seam defined here).
- **LR `crs:` mask *import*** (`from_lr_crs` for `MaskGroupBasedCorrections` etc.): **E16** migration importer. E12 defines the target model it imports into.
- **Auto Mask (edge-aware brush snapping)** — Should-tier, not in the §10.1 decomposition or the M3 exit. Deferred to v1.x; the brush evaluator keeps a per-dab weighting hook so an edge-confinement term can slot in without a model change (§9 seam note).
- **Deferred per §10.1**: depth masks, Select People body-part parsing, Select Landscape, per-mask tone curve, per-mask Point Color, Generative (diffusion) Remove, pet-eye.
- **Adaptive presets** (masks inside presets with per-image AI re-run): preset plumbing is E09, AI re-run is E14. E12 only guarantees masks serialize losslessly into the copy/preset payload.
- **Geometry/optics correctness itself** (upright, lens warp): E11. E12 consumes its transform via the `MaskFrame` contract with identity fallback.

---

## 3. Crates & modules touched (per §2 decomposition)

| Crate | E12 additions |
|---|---|
| **`lightbox-mask`** (primary; E12 is its founding epic) | mask/component model, `LocalRecipe`, component evaluators (WGSL + CPU), boolean compositor, `WeightBuffer`, raster store client, brush stroke engine |
| **`lightbox-render`** | `LocalAdjustNode`, `RetouchNode`, `MaskOverlayNode` (view-mode), `VisualizeSpotsNode` (view-mode); multi-input edges from mask weight buffers; cache keys for weight buffers |
| **`lightbox-edit`** | `Recipe.masks: Vec<MaskId>` / `Recipe.retouch: Vec<RetouchOpId>` materialization; `LocalRecipe` (de)serialization; `lb:` XMP mask/retouch mapping; copy/sync payloads |
| **`lightbox-catalog`** | migration: `mask`, `mask_component`, `retouch_op` tables; DAO; `edit_index.has_retouch`; same-txn `edit_index` derivation |
| **`lightbox-core`** | `MaskCommand`/`RetouchCommand` on the command bus; history coalescing policy for strokes/drags; queries (`masks_for_image`, `retouch_for_image`) |
| **`lightbox-shell`** | masking panel, per-mask sliders, overlay modes, pins/gizmos (on E08's gizmo framework), retouch tool UI |
| **`lightbox-jobs`** | Remove-bake job (Background class, cancellable); retouch full-res bake for export |
| New native dep | **OpenCV** (Apache-2.0) — `photo` module for `seamlessClone` (§1.6 row "Heal/seamless clone"). Registered in the surface-2 SBOM. First OpenCV consumer in the workspace (E14 reuses it for YuNet/SFace) |

Storage: baked AI rasters already live at `masks/<hh>/<hash>.png` (§3.3). E12 adds **`retouch/<hh>/<hash>.png`** for baked Remove patches — an additive extension of the `.lbdata` layout, same content-addressed, LRU-exempt-while-referenced rules as `masks/`.

---

## 4. Design decisions internal to E12

These are within-epic decisions the architecture leaves to the planner; each is final for E12 unless its named trigger fires.

### 4.1 Coordinate space: components live in normalized source space

Mask component geometry (brush dabs, gradient lines, ellipse params, sampler points) is stored in **normalized oriented-source space**: `x,y ∈ [0,1]` over the full decoded, orientation-applied source image, independent of crop, rotation, upright, or lens warp. At evaluation, the render graph supplies a `MaskFrame` carrying the composed forward transform *source-norm → LOCAL-stage buffer pixels* (accumulated from the optics/geometry nodes; **identity when no geometry stage is present** — the pre-E11-integration fallback). Analytic components (linear/radial/range) evaluate per-target-pixel via the inverse transform; raster components (brush bake, `AiSeg`, Remove patches) are warped forward with bilinear filtering.

**Why:** masks stay anchored to image content when the user changes crop/rotation later (the alternative — crop-relative coordinates — silently slides every mask on recrop). `AiSeg` and Remove rasters additionally stamp the `RasterFrame` they were baked in; a geometry change that alters that frame sets `stale=1` per §4.5 (badge + explicit recompute, never silent).

### 4.2 Compositing algebra (definitive semantics)

Weights are `[0,1]` scalars. With running mask value `a` and component value `b` (after per-component invert `b ← 1−b` if set):

```
add:        a ⊕ b = 1 − (1−a)(1−b)        (probabilistic union — smooth, commutative, bounded)
subtract:   a ⊖ b = a · (1−b)
intersect:  a ⊗ b = a · b                  (≡ subtract-of-inverse, matching Adobe's construction)
```

Mask-level invert applies last: `w ← 1−w`. **The first component of a mask is always `add`** (UI enforces; DAO rejects otherwise). Property-tested: identities/annihilators, `intersect(a,b) == subtract(a, invert(b))` exactly, closure in `[0,1]`, no NaN propagation (all component evaluators clamp + NaN-scrub on write).

### 4.3 Weight buffers

`WeightBuffer` = single-channel **R16Float** tiles, 256², matching the engine's tiling (§4.2), evaluated at the request's `RenderScale`/ROI only (§4.3). Each *component* raster and each *mask's* composited buffer is cached in the E05 node cache under `hash(component_id_content, mask_frame, scale, roi)` — moving a per-mask **slider** never re-evaluates weight buffers; editing a **component** invalidates only that mask's buffer and the LOCAL tail.

### 4.4 Per-mask application & mask-order compositing

For input `P₀` (the LOCAL-stage input) and masks `m₁…mₙ` in list order:

```
Pₖ = lerp(Pₖ₋₁, apply(Pₖ₋₁, adjustₖ·amountₖ), wₖ)      — sequential; mask k+1 sees mask k's output
```

- `apply` = the per-mask sub-recipe. **Pointwise group** (tone: exposure/contrast/highlights/shadows/whites/blacks; color: temp/tint/hue/sat/tint-color) is **one fused WGSL kernel** reusing E10's math as shared WGSL/Rust functions — identical parameter semantics to the global panel, weighted per-pixel. **Neighborhood group** (presence: texture/clarity/dehaze; detail: sharpness/NR/moiré/defringe) runs as **conditional passes emitted only when the group's params are non-default** (the common case is zero → a 30-mask stack of pointwise edits stays cheap). Presence/detail kernels are reused from E10.4/E11.2 (seam §9).
- **Amount** (`0..2`, default `1`) scales the mask's *parameter vector* (each slider × amount, clamped to that slider's valid range) — not the weight buffer — so `amount=2` extrapolates the look, per the research note.
- Range components (`LumaRange`/`ColorRange`) compute their weight from **`P₀`** (the LOCAL-stage input), not from intermediate `Pₖ` — otherwise editing mask 1 would silently re-select mask 2's pixels. Documented and golden-tested.
- **Rewrite trigger (§7, restated as an executable gate):** if the M3 perf bench shows a *typical wedding edit* (defined: 8 masks, mixed brush+radial+luma-range, pointwise-only adjustments, 24 MP raw, fit-view, reference GPU) breaching 100 ms p95 slider-to-screen, task C9 (fuse per-mask compositing into a single multi-mask pass over a weight-buffer array) activates. It is a `lightbox-mask`-internal change; the trigger, bench, and fallback are all inside this epic.

### 4.5 Retouch determinism policy (extends §4.5 of the architecture)

- **Clone / Heal / Red-eye are deterministic algorithms over pipeline pixels** → evaluated live inside `RetouchNode`, cached under the standard content key, and **auto-re-rendered when upstream edits change** (matches LR behavior; matches §8's determinism-per-backend guarantee).
- **Remove (PatchMatch) is baked + replayed**, exactly mirroring the `AiSeg` policy (§4.5): on creation a seeded, multi-scale PatchMatch fill runs as a Background job **at full resolution**, writes a content-addressed patch raster to `retouch/`, and stores `patch_ref + seed + patch_frame` on the op. All renders (preview and export) **replay** the baked patch (downsampled as needed). Upstream recipe changes that alter pixels under the op's src/dst footprint set `stale=1` → badge + refresh CTA; **never silent re-roll**. "Refresh" bumps the seed and re-bakes (a history-visible edit). Missing patch raster (cache purged) renders the op as a no-op with the stale badge + rebake CTA — never blocks the pipeline.
- Heal runs `seamlessClone` on a **CPU readback of the op's src/dst bounding regions at the current render scale** (regions are small; readback+solve+upload is ms-scale), uploaded and composited on GPU. This is the one deliberate GPU→CPU→GPU round-trip in the graph; it is per-op-region, not per-frame, and cached.

### 4.6 Pipeline placement & process version

`LocalAdjustNode` and `RetouchNode` register at the **current process version** in the exact §4.1 order (`…geometry → LOCAL → retouch → effects…`). Adding them does **not** bump PV: both are identity for empty mask/retouch lists, so every existing image renders bit-identically (guarded by the per-PV goldens). `MaskOverlayNode`/`VisualizeSpotsNode` are **view-target-only** nodes (never in the export graph).

### 4.7 Brush semantics

Strokes are stored as **vector data** (resampled input path + per-point pressure), resolution-independent; rasterized on demand into a cached component raster pyramid. Dab model: Gaussian-falloff stamp of radius `size/2`, hardness from `feather`; within a stroke, dabs accumulate `α ← α + flow·shape·(1−α)`; the stroke is capped at `density`; strokes compose into the component raster with `add` algebra; erase strokes apply `subtract`. Pressure (when a tablet reports it) modulates flow (default) or size (preference). Live painting draws the in-progress stroke as an immediate GPU stamp pass on top of the cached raster so dab-to-screen stays under the interactive budget; the raster bake + history commit happen on stroke end (pointer-up).

### 4.8 Range component math

- **LumaRange:** window `[lo,hi]` over `P₀` luma (working-space Y), with two smoothness parameters widening each edge into a smoothstep ramp. Eyedropper/drag-rect sets `[lo,hi]` from sampled min/max. "Show luminance map" is an overlay mode.
- **ColorRange:** up to **5 point samples** or one rect sample (mean color); weight `w = 1 − smoothstep(0, r(refine), min_i d_Oklab(p, sample_i))` with chroma-weighted Oklab distance (hue+chroma dominant, lightness down-weighted ×0.25 so shading doesn't break the selection). Refine slider maps to the falloff radius.

---

## 5. Interface definitions

Contracts, not implementations. Types live in `lightbox-mask` unless prefixed.

### 5.1 Identifiers, model

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)] pub struct MaskId(pub i64);        // catalog rowid
#[derive(Copy, Clone, PartialEq, Eq, Hash)] pub struct ComponentId(pub i64);
#[derive(Copy, Clone, PartialEq, Eq, Hash)] pub struct RetouchOpId(pub i64);

pub struct Mask {
    pub id: MaskId,
    pub name: String,
    pub visible: bool,
    pub inverted: bool,                 // mask-level invert, applied after compositing
    pub components: Vec<MaskComponent>, // ordered; components[0].op must be Add
    pub adjust: LocalRecipe,
}

#[derive(Copy, Clone, PartialEq, Eq)] pub enum BoolOp { Add, Subtract, Intersect }

pub struct MaskComponent {
    pub id: ComponentId,
    pub op: BoolOp,
    pub inverted: bool,
    pub visible: bool,
    pub kind: ComponentKind,
}

pub enum ComponentKind {
    Brush(BrushParams),
    Linear(LinearParams),
    Radial(RadialParams),
    LumaRange(LumaRangeParams),
    ColorRange(ColorRangeParams),
    AiSeg(AiSegComponent),              // slot only; produced by E14, replayed here (§4.5)
}
```

### 5.2 Component parameter types (all geometry in normalized source space, §4.1)

```rust
pub struct SrcPoint { pub x: f32, pub y: f32 }             // [0,1]² over oriented source

pub struct BrushParams {
    pub strokes: Vec<Stroke>,
}
pub struct Stroke {
    pub points: Vec<StrokePoint>,       // resampled path
    pub size: f32,                      // dab diameter, fraction of source long edge
    pub feather: f32,                   // 0 hard .. 1 soft
    pub flow: f32,                      // 0..1 buildup per dab
    pub density: f32,                   // 0..1 stroke opacity cap
    pub erase: bool,
    pub pressure_target: PressureTarget, // Flow | Size | None
}
pub struct StrokePoint { pub p: SrcPoint, pub pressure: f32 }

pub struct LinearParams { pub start: SrcPoint, pub end: SrcPoint }
    // weight 1 before `start` line, 0 after `end` line, smooth ramp between;
    // the three on-canvas lines are start / midpoint (derived) / end

pub struct RadialParams {
    pub center: SrcPoint, pub radius_x: f32, pub radius_y: f32,
    pub rotation_rad: f32, pub feather: f32,   // inside=1 → 0 at feathered edge
}

pub struct LumaRangeParams {
    pub lo: f32, pub hi: f32,                  // [0,1] working-space luma window
    pub smooth_lo: f32, pub smooth_hi: f32,    // edge ramp widths
}

pub struct ColorRangeParams {
    pub samples: ColorSamples,                 // Points(Vec<OklabColor> /*≤5*/) | Rect{ mean, spread }
    pub refine: f32,                           // 0..1 → falloff radius
}

pub struct AiSegComponent {
    pub recipe: SegRecipe,                     // provenance: model_id, version, prompt, params (E14-owned schema)
    pub raster: Option<RasterRef>,             // content hash into masks/ store; None until first bake
    pub raster_frame: RasterFrame,             // frame stamp of the bake (§4.1)
    pub stale: bool,                           // set ONLY by explicit user action / frame change (§4.5)
}
pub struct RasterRef(pub String);              // content-address in the .lbdata raster stores
pub struct RasterFrame { pub src_dims: (u32,u32), pub geom_hash: u64 }
```

### 5.3 Per-mask adjustment recipe

```rust
pub struct LocalRecipe {
    pub amount: f32,            // 0..2, default 1 — scales the parameter vector (§4.4)
    pub tone: LocalTone,        // exposure_ev, contrast, highlights, shadows, whites, blacks   (all -100..100 except exposure_ev -4..4; identical semantics to E10 global)
    pub color: LocalColor,      // temp, tint, hue_shift, saturation, tint_color: Option<Rgb>
    pub presence: LocalPresence,// texture, clarity, dehaze
    pub detail: LocalDetail,    // sharpness, noise, moire, defringe
}
impl LocalRecipe {
    pub fn is_identity(&self) -> bool;
    pub fn scaled(&self, amount: f32) -> LocalRecipe;    // per-param clamped multiply
    pub fn pointwise_uniforms(&self) -> LocalPointwiseUniforms; // fused-kernel packing
}
```

### 5.4 Evaluation

```rust
pub struct MaskFrame {
    pub src_to_local: Mat3x3,           // source-norm → LOCAL buffer px (identity fallback pre-E11)
    pub local_to_src: Mat3x3,
    pub buffer_px: (u32, u32),
    pub roi: Roi,
    pub scale: RenderScale,
}

pub struct WeightBuffer { /* R16Float tiles covering roi */ }

pub struct MaskEvalCtx<'a> {
    pub gpu: &'a GpuCtx,
    pub frame: &'a MaskFrame,
    pub basis: &'a Tile,                    // P₀ — LOCAL-stage input, for range components (§4.4)
    pub rasters: &'a dyn RasterStore,
}

/// Composite a whole mask (components in order, §4.2 algebra) into one weight buffer. Cached (§4.3).
pub fn evaluate(mask: &Mask, ctx: &MaskEvalCtx) -> Result<WeightBuffer, MaskError>;

/// Per-kind evaluator; registered like RenderNodes so kinds are individually testable + PV-stable.
pub trait ComponentEvaluator: Send + Sync {
    fn kind(&self) -> &'static str;                                            // "brush" | "linear" | ...
    fn eval_gpu(&self, ctx: &MaskEvalCtx, c: &MaskComponent) -> Result<WeightTile, MaskError>;
    fn eval_cpu(&self, ctx: &MaskEvalCtxCpu, c: &MaskComponent) -> Result<WeightTileCpu, MaskError>;
    fn content_hash(&self, c: &MaskComponent) -> u64;                          // cache key input (§4.3)
}

pub trait RasterStore: Send + Sync {
    fn get(&self, r: &RasterRef) -> Result<RasterImage, RasterError>;          // masks/ + retouch/
    fn put(&self, img: &RasterImage) -> Result<RasterRef, RasterError>;        // content-addressed
}

pub enum MaskError { RasterMissing(RasterRef), DegenerateGeometry, Gpu(GpuError) }
    // RasterMissing → component skipped + stale badge, never a render failure (§4.5)
```

### 5.5 Render nodes (`lightbox-render`)

```rust
/// §4.1 LOCAL stage. Multi-input: pipeline tile + N weight buffers (E05 multi-input edges).
pub struct LocalAdjustNode;   // node_id "local_adjust", registered at current PV
pub struct LocalStageParams { pub masks: Vec<(Mask, WeightBufferKey)> }  // resolved by graph build

/// §4.1 retouch stage, after LOCAL.
pub struct RetouchNode;       // node_id "retouch"
pub struct RetouchStageParams { pub ops: Vec<RetouchOp> }

/// View-target-only overlay + spots nodes (never in export graphs).
pub struct MaskOverlayNode;   // params: OverlayMode, overlay color/opacity, active mask/component
pub enum OverlayMode { ColorOverlay, ColorOnBw, ImageOnBlack, ImageOnWhite, WhiteOnBlack }
pub struct VisualizeSpotsNode; // params: threshold (DoG edge-map remap)
```

### 5.6 Retouch model

```rust
pub enum RetouchMode { Clone, Heal, Remove, RedEye }

pub struct RetouchOp {
    pub id: RetouchOpId,
    pub mode: RetouchMode,
    pub visible: bool,
    pub dst: RetouchRegion,             // Spot { center, radius } | Brushed { strokes: Vec<Stroke> }
    pub src: SrcPlacement,              // Auto | Manual(SrcPoint offset)  (Clone/Heal; Remove sample-area override)
    pub feather: f32, pub opacity: f32,
    pub params: RetouchParams,
}
pub enum RetouchParams {
    CloneHeal,
    Remove { seed: u64, patch: Option<RasterRef>, patch_frame: RasterFrame, stale: bool },
    RedEye { pupil_size: f32, darken: f32, detected: Option<EyeRegion> },
}

/// Deterministic auto-source: ring search around dst minimizing SSD over candidate patches (§4.5 policy).
pub fn auto_source(basis: &TileCpu, dst: &RetouchRegion) -> SrcPoint;

/// Backend seam for Remove — E12 ships PatchMatch; E16 registers LaMa behind the same trait.
pub trait RemoveBackend: Send + Sync {
    fn id(&self) -> &'static str;                        // "patchmatch" (E12) | "lama" (E16)
    fn fill(&self, img: &TileCpu, mask: &WeightTileCpu, seed: u64, cancel: &CancelToken)
        -> Result<RasterImage, RemoveError>;             // full-res bake, Background job
}
```

### 5.7 Core commands & queries (`lightbox-core`)

All mutations go through the command bus → catalog writer txn → `edit_index` rebuild in the same txn → history step (E09). High-frequency gestures (paint, gizmo drag, slider drag) use the standard **coalescing policy**: live preview via engine params only, one command committed on gesture end.

```rust
pub enum MaskCommand {
    Create { image: ImageId, first: ComponentDraft, name: Option<String> },
    Delete { mask: MaskId },
    Rename { mask: MaskId, name: String },
    Duplicate { mask: MaskId, invert: bool },
    SetVisible { mask: MaskId, visible: bool },
    SetInverted { mask: MaskId, inverted: bool },
    Reorder { image: ImageId, order: Vec<MaskId> },
    SetAdjust { mask: MaskId, patch: LocalRecipePatch },     // one committed step per gesture
    SetAmount { mask: MaskId, amount: f32 },
    AddComponent { mask: MaskId, draft: ComponentDraft, op: BoolOp },
    RemoveComponent { comp: ComponentId },
    SetComponentOp { comp: ComponentId, op: BoolOp },        // reject index 0 != Add
    SetComponentInverted { comp: ComponentId, inverted: bool },
    UpdateComponent { comp: ComponentId, patch: ComponentPatch },  // gizmo/range edits
    AppendStrokes { comp: ComponentId, strokes: Vec<Stroke> },     // one per pointer-up
    MarkAiStale { comp: ComponentId },                        // recompute dispatch = E14 seam
}

pub enum RetouchCommand {
    Add { image: ImageId, draft: RetouchDraft },
    Delete { op: RetouchOpId },
    SetVisible { op: RetouchOpId, visible: bool },
    MoveSource { op: RetouchOpId, src: SrcPoint },
    MoveDest { op: RetouchOpId, delta: (f32, f32) },
    Resize { op: RetouchOpId, radius: f32 },
    SetFeather { op: RetouchOpId, feather: f32 },
    SetOpacity { op: RetouchOpId, opacity: f32 },
    SetMode { op: RetouchOpId, mode: RetouchMode },          // clone↔heal toggle
    RefreshRemove { op: RetouchOpId },                       // seed+1, re-bake job (history-visible)
    SetRedEyeParams { op: RetouchOpId, pupil_size: f32, darken: f32 },
    Reorder { image: ImageId, order: Vec<RetouchOpId> },
}

// Queries (WAL reader):
pub fn masks_for_image(img: ImageId) -> Vec<Mask>;
pub fn retouch_for_image(img: ImageId) -> Vec<RetouchOp>;
```

### 5.8 XMP mapping (`lightbox-edit`, on E09's mapping layer)

```rust
/// lb: namespace — authoritative, lossless for the full model (property-tested round-trip).
pub fn masks_to_lb_xmp(masks: &[Mask], out: &mut XmpDoc);
pub fn retouch_to_lb_xmp(ops: &[RetouchOp], out: &mut XmpDoc);
/// crs: export, best-effort interop subset: linear/radial gradients + their tone/color sliders
/// → crs:GradientBasedCorrections / crs:CircularGradientBasedCorrections;
/// spot clone/heal → crs:RetouchAreas. Brush/range/AiSeg masks: lb:-only (documented gap).
pub fn masks_to_crs_export(masks: &[Mask], ops: &[RetouchOp], out: &mut XmpDoc);
```

---

## 6. Data model & migration

Implements the §3.1 tables (this epic creates them — schema versions prior to E12 don't have them). Column `ord` replaces the architecture sketch's `order` (SQL keyword); semantics identical. **Ownership per §3.1:** these tables are the **sole authoritative store** for mask/retouch content; `edit_recipe.doc` holds only the ordered `MaskId[]`/`RetouchOpId[]` reference lists; raster-bake and staleness writes touch only these tables and never rewrite the recipe blob or emit history steps.

```sql
-- migration 00NN_masking_retouch.sql  (schema_version bump; copy-on-write upgrade per §6)
CREATE TABLE mask (
    id          INTEGER PRIMARY KEY,
    image_id    INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    ord         INTEGER NOT NULL,
    visible     INTEGER NOT NULL DEFAULT 1,
    inverted    INTEGER NOT NULL DEFAULT 0,
    adjust      BLOB    NOT NULL,          -- CBOR LocalRecipe (schema-versioned like Recipe)
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_mask_image ON mask(image_id, ord);

CREATE TABLE mask_component (
    id                INTEGER PRIMARY KEY,
    mask_id           INTEGER NOT NULL REFERENCES mask(id) ON DELETE CASCADE,
    ord               INTEGER NOT NULL,
    kind              TEXT    NOT NULL CHECK (kind IN
                        ('brush','linear','radial','luma_range','color_range','ai_seg')),
    bool_op           TEXT    NOT NULL CHECK (bool_op IN ('add','subtract','intersect')),
    inverted          INTEGER NOT NULL DEFAULT 0,
    visible           INTEGER NOT NULL DEFAULT 1,
    params            BLOB    NOT NULL,    -- CBOR per-kind params (§5.2)
    ai_recipe         BLOB,                -- CBOR SegRecipe, ai_seg only (provenance, §4.5)
    cached_raster_ref TEXT,                -- content hash → masks/<hh>/<hash>.png
    raster_frame      BLOB,                -- CBOR RasterFrame
    stale             INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_mask_component_mask ON mask_component(mask_id, ord);

CREATE TABLE retouch_op (
    id           INTEGER PRIMARY KEY,
    image_id     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    ord          INTEGER NOT NULL,
    mode         TEXT    NOT NULL CHECK (mode IN ('clone','heal','remove','red_eye')),
    visible      INTEGER NOT NULL DEFAULT 1,
    dst          BLOB    NOT NULL,         -- CBOR RetouchRegion
    src          BLOB,                     -- CBOR SrcPlacement
    feather      REAL    NOT NULL DEFAULT 0.5,
    opacity      REAL    NOT NULL DEFAULT 1.0,
    params       BLOB,                     -- CBOR RetouchParams (seed / patch_ref / red-eye)
    patch_ref    TEXT,                     -- content hash → retouch/<hh>/<hash>.png (remove only)
    patch_frame  BLOB,                     -- CBOR RasterFrame
    stale        INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_retouch_image ON retouch_op(image_id, ord);

-- edit_index (§3.1): has_masks / has_ai_mask exist since schema v1; add retouch facet.
ALTER TABLE edit_index ADD COLUMN has_retouch INTEGER NOT NULL DEFAULT 0;
```

**Invariants (DAO-enforced, unit-tested):** first component per mask is `add`; `ord` sequences are dense per parent; every mask/retouch row's id appears exactly once in its image's `edit_recipe.doc` reference list (repaired toward the tables on integrity check — the tables are authoritative for existence, the doc for order); `edit_index.{has_masks,has_ai_mask,has_retouch}` rebuilt in the same write txn as any mutation of these tables or the doc lists.

---

## 7. Ordered task breakdown

Each task ≤1 day. `[gate]` = blocks the phase from proceeding. Phases may overlap across the 2-engineer pixel stream (B/C engine work ∥ D shell work once B4 lands), but the listed order within a phase is dependency order. Total: 63 tasks + 1 contingency ≈ 12–13 pw with review/integration slack — inside the XL budget.

### Phase A — Data model, recipe plumbing (E12.0, 7 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | Migration `00NN_masking_retouch` + copy-on-write upgrade path | Migration applies on a v(N−1) catalog; original preserved; `quick_check` clean; downgrade-open refuses cleanly |
| A2 | `lightbox-mask` model types (§5.1–5.3) + CBOR serde | proptest: serialize→deserialize identity for all component kinds, `LocalRecipe`, `RetouchOp`; unknown-field tolerance for forward-compat |
| A3 | Catalog DAO: mask/mask_component CRUD + ordering + invariants | Unit tests: first-comp-must-be-Add rejected; dense `ord` maintained on insert/delete/reorder; cascade delete |
| A4 | Catalog DAO: retouch_op CRUD + `edit_index.has_retouch` same-txn derivation [gate] | Facet columns correct after every mutation; `kill -9` fault-injection mid-mask-write leaves catalog clean |
| A5 | `Recipe` integration: `masks[]`/`retouch[]` ref lists + materialized-view join (E09 seam) | `Recipe` assembled from doc+tables matches inserted state; snapshot copies content (immutable projection, §3.1) |
| A6 | `MaskCommand`/`RetouchCommand` bus handlers + history steps (E09) | Every command undoes/redoes correctly; undo of `Create` removes rows + doc ref atomically |
| A7 | Gesture coalescing policy: `AppendStrokes`/`SetAdjust`/`UpdateComponent` one-step-per-gesture | Simulated 200-event slider drag produces 1 history step; paint stroke produces 1 step on pointer-up |

### Phase B — Mask engine core (E12.1, 15 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `WeightBuffer` R16F tile alloc/clear/fill + NaN-scrub write helpers [gate] | Tiles allocate/free within engine VRAM budget; NaN input clamps to 0; unit tests on CPU mirror |
| B2 | `MaskFrame` plumbing: graph build composes geometry transform, identity fallback (E11 seam) | With no geometry nodes, `src_to_local` is the scale transform only; with a mocked rotate node, a fixed source point maps correctly |
| B3 | Linear gradient evaluator, WGSL + CPU | **First failing test of the epic:** gray-ramp raw + single linear mask (exposure −1 EV) matches committed golden ≤ ΔE2000 1.0, GPU and CPU |
| B4 | Radial evaluator (ellipse, rotation, feather, inside/outside), WGSL + CPU | Golden; feather 0 and 1 extremes correct; degenerate radius → `DegenerateGeometry`, renders as empty, no panic |
| B5 | Boolean compositor kernel (§4.2 algebra) + mask-level invert | proptest: closure in [0,1]; `intersect ≡ subtract∘invert` exact; commutativity of `add`; golden on a 3-component compound |
| B6 | Stroke capture/resampling (path → dab positions at spacing = size/4) | Deterministic dab list for a fixed input path; resample independent of input event rate |
| B7 | Dab stamp kernel: Gaussian falloff, flow accumulation, density cap (§4.7) | Overlapping-stroke goldens: flow buildup monotone, capped at density; hardness extremes correct |
| B8 | Erase strokes + brush component raster bake + raster pyramid cache | Erase subtracts per §4.2; bake cached under content hash; re-render after edit reuses pyramid where valid |
| B9 | Live-paint immediate stamp path (in-progress stroke on top of cached raster) | Dab-to-screen < 50 ms on reference GPU while a 500-dab stroke is in progress |
| B10 | LumaRange evaluator (window + dual smoothness), WGSL + CPU | Golden on a tone-ramp scene; smoothness 0 = hard window; sampler sets window from rect min/max |
| B11 | ColorRange evaluator (Oklab distance, ≤5 samples, refine falloff), WGSL + CPU | Golden on a color-chart scene: selecting one patch excludes neighbors at refine 0, includes family at refine 1 |
| B12 | `AiSeg` replay evaluator: raster load, frame check, warp-to-frame (§4.5) | A hand-authored raster in `masks/` replays across scales/ROIs; frame mismatch sets stale + renders last raster warped; `RasterMissing` → skip + badge, no render failure |
| B13 | Weight-buffer cache keys in the E05 node cache (§4.3) | Recompute-count probe: per-mask slider change re-evaluates 0 weight buffers; component edit re-evaluates exactly 1 mask's buffer |
| B14 | CPU parity pass for all evaluators (rayon impls behind `ComponentEvaluator::eval_cpu`) | CPU vs GPU ≤ ΔE2000 1.0 / PSNR ≥ 45 dB on the phase-B golden set |
| B15 | Phase-B golden corpus registration (per-PV harness hookup, §8) | All B goldens run per process version in CI; drift fails the build |

### Phase C — LOCAL stage & per-mask application (E12.2, 9 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | `LocalAdjustNode` skeleton in the DAG at §4.1 position; identity for empty mask list [gate] | Existing per-PV goldens unchanged with node inserted (no-PV-bump proof, §4.6) |
| C2 | Fused pointwise kernel: tone group, reusing E10 math as shared functions | Per-mask exposure/contrast/h/s/w/b at w=1 matches the E10 global equivalent ≤ ΔE 1.0 (semantic-parity golden) |
| C3 | Fused pointwise kernel: color group (temp/tint/hue/sat/tint-color) + amount scaling (§4.4) | Color-group goldens; `amount=0` is identity; `amount=2` equals doubled-clamped params |
| C4 | Weighted blend + sequential mask-order compositing (`Pₖ` chain) | Two overlapping masks composite in list order; reorder command changes output; goldens |
| C5 | Conditional neighborhood passes: presence (texture/clarity/dehaze) reusing E10.4 kernels | Pass emitted only when group non-default (recompute-count probe); weighted-application goldens; halo check on the E10.2 clipped corpus |
| C6 | Conditional neighborhood passes: detail (sharpness/NR/moiré/defringe) reusing E11.2/E11.3 kernels | Same probe + goldens; if E11.2 unavailable at integration time, group ships behind `local_detail` feature flag (Risk R2) |
| C7 | Range-basis correctness: `P₀` basis for LumaRange/ColorRange (§4.4) | Golden: editing mask 1's exposure does not change mask 2's luma-range selection |
| C8 | 30-mask stress bench + typical-wedding-edit bench (criterion + scenario harness, §7 budgets) | Bench runs in nightly CI; report asserts <100 ms p95 slider-to-screen for the 8-mask typical case at fit-view on reference GPU |
| C9 | **Contingency (trigger-gated, §4.4):** fuse per-mask compositing into one multi-mask pass | Only if C8's typical case fails: fused pass restores <100 ms p95; goldens unchanged |

### Phase D — Mask management UI (E12.3, 12 tasks)

Built on E08's gizmo/overlay framework and E10's slider widgets (seams §9). Shell-side; runs parallel to C after B4.

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | Masking panel: mask list, component sub-list, add-mask/add-component menus | Create/select/delete flows drive commands; list reflects catalog state via query subscription |
| D2 | Per-mask adjustment sliders (reuse E10 develop widgets bound to `LocalRecipePatch`) | Slider drag coalesces to one history step; live preview via engine params during drag |
| D3 | Rename / duplicate / duplicate-inverted / invert / hide / convert-op / reorder actions | Each action = one command, one history step; duplicate-inverted produces complementary render |
| D4 | Amount slider | `amount` behaves per §4.4 incl. preset-applied masks |
| D5 | `MaskOverlayNode` + overlay modes (5 modes, color/opacity prefs) | View-mode toggles render correctly at all zoom levels; never present in export graph (asserted in test) |
| D6 | Component hover preview + keyboard toggles (overlay on/off, pins show/hide) | Hovering a component shows only that component's weight; keymap entries registered in E08's registry |
| D7 | Pins: placement, hit-testing, drag-to-move per component | Pin drag updates component geometry via `UpdateComponent`, coalesced; pins track under zoom/pan |
| D8 | Linear gizmo: 3-line drag / rotate / stretch | Gizmo edits round-trip through normalized source space correctly under crop/rotation (mocked geometry) |
| D9 | Radial gizmo: center/radii/rotation/feather handles + invert affordance | Same round-trip criteria; feather handle maps monotonically |
| D10 | Brush tool UX: cursor size/feather rings, `[`/`]` size keys, erase modifier, tablet pressure (winit) | Pressure modulates flow on a tablet-report path (integration-tested with synthesized events); mouse fallback = pressure 1.0 |
| D11 | Range sampler UX: eyedropper + drag-rect, falloff/refine sliders, show-luma-map mode | Sampling sets params per §4.8; luma-map view mode renders |
| D12 | Stale-AI badge + "Update mask" affordance (dispatch stub → E14 seam; disabled w/o inferd) | `stale=1` shows badge; action emits the recompute request event; absent inferd → disabled tooltip, no crash |

### Phase E — Non-destructive retouch (E12.4, 13 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | `RetouchNode` skeleton at §4.1 position; identity for empty op list [gate] | Per-PV goldens unchanged; ops render in `ord` order |
| E2 | Dst region model: spot + brushed region, feather, region rasterizer (reuses B6–B7 stroke engine) | Region raster goldens; feather edge profile matches spec curve |
| E3 | Deterministic auto-source search (SSD ring search on `basis` at preview res) | Fixed image + spot → identical source across runs/platforms (CPU, integer SSD); picks visually plausible source on test corpus |
| E4 | Clone engine: feathered alpha composite of warped source region, WGSL + CPU | Clone goldens; opacity/feather sliders behave; source visibly verbatim |
| E5 | OpenCV `photo` module linkage (Apache-2.0) + SBOM surface-2 entry + build on all 3 CI platforms [gate] | Workspace builds macOS/Windows/Linux; SBOM emits opencv artifact with license + link mode; cargo-deny green |
| E6 | Heal engine: region readback → `seamlessClone` (mixed-gradient) → upload composite (§4.5) | Heal golden on gradient/sky/skin corpus: no visible seam at 1:1 (perceptual review + PSNR floor vs reference); solve ≤ 30 ms for a 512² region |
| E7 | Editable objects: drag src/dst, resize, feather/opacity live re-render; clone↔heal toggle | Dragging source re-renders within interactive budget at preview res; all edits are history steps; cache reuses unchanged ops |
| E8 | Own PatchMatch: seeded multi-scale fill, cancellable (`RemoveBackend` impl) | Fixed (image, mask, seed) → byte-identical fill across runs and platforms; cancel aborts ≤ 100 ms; quality review vs G'MIC/PatchMatch references on a 12-scene corpus |
| E9 | Remove bake/replay: full-res Background bake job → `retouch/` store; downsample replay; staleness + refresh (§4.5) | Preview shows baked patch at all scales; upstream slider change under footprint sets `stale=1` + badge, never re-rolls; `RefreshRemove` re-bakes with seed+1 as a history step; missing patch → no-op + rebake CTA |
| E10 | Red-eye: redness-ratio detection within drag region + desaturate/darken fix + sliders | Detection hits ≥ 90 % on a 20-image red-eye corpus; pupil-size/darken re-render live; miss → manual ellipse fallback |
| E11 | `VisualizeSpotsNode`: DoG edge-map + threshold slider view mode | Dust spots visible on test sensor-dust frames; view-only (asserted); threshold interactive |
| E12 | Retouch tool UI: tool modes, pins per op, panel list, delete/visibility | Full create/edit/delete flow drives commands; pins hit-test correctly among masks' pins |
| E13 | Retouch golden + determinism suite registration | Clone/heal/red-eye live-path goldens per PV; remove replay golden (baked raster fixed); CPU/GPU parity within §4.4 tolerance |

### Phase F — Serialization, XMP, integration & hardening (E12.5, 8 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | `lb:` XMP schema for masks + retouch, on E09's mapping layer (§5.8) | proptest round-trip: arbitrary mask/retouch state → XMP → identical state; unknown `lb:` sub-fields preserved |
| F2 | `crs:` export subset: gradient/radial corrections + spot `crs:RetouchAreas` | Exported XMP validates against the crs shape LR accepts (fixture-based); documented mapping table incl. named non-mapped kinds |
| F3 | Copy/paste + sync payload: merge-or-replace semantics; AiSeg comps flagged `stale` on paste (per-image recompute = E14 seam) | Paste onto a second image reproduces non-AI masks exactly; AI comps arrive stale-badged with recipe intact; merge vs replace both tested |
| F4 | Snapshot/history correctness across the full model | Snapshot → mutate → restore reproduces mask/retouch state exactly (content copied, not referenced, §3.1); 200-step undo/redo fuzz clean |
| F5 | `lightbox-cli` headless flows: apply mask recipe + retouch from JSON, render, export | CI integration test: import → scripted 5-mask + 3-op edit → export → visual-regression compare |
| F6 | Failure-mode sweep: raster missing, degenerate geometry, device-lost mid-paint, huge stroke lists, cancelled bake | Each renders degraded-but-correct per §5.4/§4.5; device-lost during paint recovers to CPU preview path (E05.5 harness) with stroke intact |
| F7 | Perf hardening to §7: profile + fix the slider p95, paint latency, weight-buffer VRAM ceiling | Nightly scenario bench green: typical-wedding-edit < 100 ms p95; 30-mask case documented with measured numbers vs trigger |
| F8 | Docs: mask model + algebra + coordinate-space + determinism policy write-up; golden corpus provenance entries | Doc reviewed; every committed golden asset has a provenance line (test-data hygiene, §8 surface-3 adjacent) |

---

## 8. Test plan (per §8 strategy)

| Layer | E12 tests | Gate |
|---|---|---|
| **Unit** | Compositing algebra (proptest, §4.2 identities); stroke resampling determinism; `LocalRecipe` scaling/clamping; DAO invariants (first-comp-Add, dense `ord`, cascade); range math edge cases; auto-source determinism | PR-blocking |
| **Property / round-trip** | CBOR model round-trip (A2); `lb:` XMP round-trip (F1); copy/sync payload round-trip (F3); undo/redo fuzz (F4) | PR-blocking |
| **Golden-image** | Per-component goldens (B3–B12); compound-mask goldens (B5); per-mask-group semantic-parity vs global (C2–C6); range-basis (C7); retouch clone/heal/red-eye (E13); remove *replay* golden (fixed baked raster); all **per process version**, GPU + CPU, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB (§4.4) | PR-blocking (fast subset), full nightly |
| **Determinism** | PatchMatch byte-identity per (input, seed) across platforms (E8); heal determinism per backend (§4.5); baked rasters replay identically across scales | PR-blocking |
| **Integration / E2E** | `lightbox-cli` headless mask+retouch edit → export → visual regression (F5); stale-badge flows; overlay/spots nodes excluded from export graphs (asserted) | PR-blocking fast subset |
| **Fault injection** | `kill -9` mid-mask-write (A4); missing raster / cancelled bake / device-lost mid-paint (F6) | PR-blocking |
| **Performance** | criterion micro (per-evaluator kernel times, compositor); scenario harness: typical-wedding-edit p95 < 100 ms, 30-mask stress tracked vs trigger, dab-to-screen < 50 ms, heal solve < 30 ms/512² | Nightly; regression → tracked issue |
| **Cross-platform** | Full unit + golden subset on macOS (Metal) / Windows (DX12) / Linux (Vulkan); OpenCV linkage on all three (E5) | PR-blocking |

**First failing test of the epic (staff-engineer requirement, §12):** `golden_local_linear_gradient` — a gray-ramp raw with one linear-gradient mask at exposure −1 EV rendered through `Engine::submit` matches a committed golden within ΔE2000 ≤ 1.0 on both GPU and CPU backends (task B3).

---

## 9. Seams to neighboring epics (named, not designed)

| Epic | Seam (what crosses, who owns which side) |
|---|---|
| **E05** | E12 consumes `RenderNode`, the node cache, tiling/ROI, and **multi-input edges** (weight buffers → `LocalAdjustNode`). Any DAG capability gap (e.g., N-ary input arity limits) is an E05 API request, not a fork. Device-lost/CPU-fallback behavior rides E05.5's harness. |
| **E10** | Tone/color math shared as WGSL/Rust functions with identical parameter semantics (C2/C3 parity goldens are the contract); presence kernels (texture/clarity/dehaze) reused from E10.4; per-mask highlight/shadow rides E10.2's halo-free operator. |
| **E11** | (a) `MaskFrame` transform: geometry nodes publish the composed source→buffer transform; E12 consumes with identity fallback. (b) Detail kernels (sharpen/NR/moiré/defringe) reused per-mask; E12 gates the group behind a flag if E11.2 slips (Risk R2). |
| **E13** | None direct — E12 never talks to inferd. VRAM gating and model packs are invisible here. |
| **E14** | Owns: segmentation execution, prompt UI, `SegRecipe` population, recompute dispatch, adaptive-preset re-run. E12 owns: the `AiSeg` component slot, `SegRecipe` storage as an opaque CBOR blob, baked-raster replay (B12), `stale` semantics, the badge/affordance + `MarkAiStale`/recompute-request event (D12). Contract type: `SegRecipe` schema is E14's; E12 treats it as opaque provenance. |
| **E09** | Recipe doc ref-lists + materialized view (A5); history manager + coalescing (A6/A7); XMP toolkit mapping layer under §5.8; presets carry the F3 payload format. |
| **E08** | Gizmo/overlay framework, keymap registry, canvas hit-testing infra — E12 implements mask/retouch-specific gizmos and keys on top. |
| **E15** | Export renders full-res through the same `LocalAdjustNode`/`RetouchNode`; Remove replays the full-res baked patch; view-only nodes excluded by target type; backend recorded in export metadata per §4.4. |
| **E16** | (a) LR migration importer maps `crs:` mask/retouch corrections *into* E12's model (E12 provides the target types + the F2 mapping doc). (b) LaMa registers as a second `RemoveBackend` behind §5.6's trait. |
| **v1.x** | Auto Mask: per-dab weighting hook in the dab kernel (§4.7) reserved; depth-range component = one new `ComponentKind` + evaluator, no model change. |

---

## 10. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Per-mask neighborhood ops blow the interactive budget** (texture/clarity/dehaze/sharpen/NR are per-mask convolution passes) | Conditional pass emission (zero-param groups cost nothing, the common case); guided-filter passes computed at reduced res + upsampled (matches E10.4's approach); worst case falls under the §4.4/C9 fuse trigger; budget asserted by C8 nightly bench |
| R2 | **E11.2 detail kernels not landed when C6 integrates** (E12 formally depends only on E05/E10; M2 ordering makes E11 available in practice) | Per-mask Detail group behind a feature flag; ships within M3 hardening once E11.2 merges; the rest of E12 is unaffected |
| R3 | **Brush feel** — flow/density/pressure behavior is craft, not just correctness; winit tablet-pressure support is uneven across platforms | §4.7 semantics fixed early + a dedicated feel-review pass with a stylus on macOS/Windows in D10; pressure degrades to 1.0 gracefully; dab pipeline keeps stamp params tweakable without model change |
| R4 | **OpenCV dependency weight** (first workspace consumer; build time, binary size, 3-platform CI) | Minimal module set (`photo` + core); E5 is an explicit early gate task; static-vs-dynamic per platform decided at E5 with SBOM entry; fallback documented: own mean-value-coordinates clone is the §1.6-rejected-but-known alternative if linkage proves unshippable |
| R5 | **PatchMatch quality vs 2026 expectations** (LaMa-class fill is the perceived baseline) | Scoped Should per catalog ("v1 can ship weeks behind core heal"); multi-scale + user source-override + refresh close most gaps; LaMa upgrade is a bounded E16 backend swap behind `RemoveBackend` |
| R6 | **Mask/geometry interaction correctness** (upright/lens warp vs normalized source space; AI rasters baked in a stale frame) | `MaskFrame` contract with frame stamps + staleness (§4.1); mocked-geometry round-trip tests in B2/D8/D9 before E11 integration; real-geometry integration test added in M3 hardening |
| R7 | **`crs:` mask export fidelity oversold** — full mask-model export to LR is not achievable (brush dab encoding, range/AI kinds) | Expectation set per Risk 9 pattern: `lb:` is authoritative + lossless; `crs:` export is a documented gradient/retouch subset (F2); import fidelity is E16's scope |
| R8 | **30-mask stress case breaches budget on low-end GPUs** | The §7 trigger + C9 contingency are inside this epic's budget; preview-res degradation (§4.3) is the floor; measured numbers published in F7, not hand-waved |

### Open questions (tracked; none block phase A/B start)

1. **Pressure API**: winit's tablet support may need a platform shim (macOS NSEvent pressure / Windows Ink) — resolve during D10; affects D10 only.
2. **Per-mask defringe scope**: does local defringe need E11.3's CA context, or is a standalone hue-targeted desaturation kernel acceptable for v1? Decide with the E11 owner at C6; default = standalone kernel.
3. **Color-range rect sampling statistics**: mean + fixed spread vs mean + covariance-scaled distance — pick during B11 based on the color-chart corpus results.
4. **Remove staleness footprint test**: exact definition of "upstream pixels under the footprint changed" (hash of the region at bake scale vs recipe-delta heuristic) — decide at E9; must be cheap enough to evaluate per recipe change.
5. **`retouch/` store retention**: LRU-exempt while referenced (proposed here) needs E03 owner's ack, since the preview-store cap accounting lives there.

---

## 11. Performance budgets (E12-specific, from §7)

| Metric | Budget | Where asserted |
|---|---|---|
| Slider-to-screen, typical wedding edit (8 mixed masks, pointwise adjustments, 24 MP, fit-view, reference GPU) | < 100 ms p95 | C8 / F7 nightly |
| 30-mask stress case | measured + tracked vs the §7 fuse trigger; preview-res floor guaranteed | C8 nightly |
| Brush dab-to-screen during live paint | < 50 ms | B9 bench |
| Per-mask slider change: weight buffers re-evaluated | 0 (cache key proof) | B13 probe test |
| Heal solve (512² region readback→solve→upload) | ≤ 30 ms | E6 bench |
| Remove full-res bake (24 MP, typical op) | Background job, cancellable ≤ 100 ms; no interactive impact | E8/E9 |
| Weight-buffer VRAM | within E05 tile budget; 30 masks at fit-view ≤ 256 MB additional | F7 |
| CPU fallback | preview-res interactive editing incl. masks per §4.4 degraded contract; no full-res promise | B14 / F6 |

---

## 12. Definition of done

E12 is done when all of the following hold on all three CI platforms:

1. **M3 exit criteria (§9 milestone plan), E12's share:** local adjustments with the full mask model render interactively; masks + retouch serialize to recipe **and** XMP; the baked-raster `AiSeg` slot replays correctly (demonstrated with a fixture raster; live inference = E14).
2. All Must-tier rows in §1's table are implemented, command-driven, undoable, and persisted through the §6 schema with `kill -9` safety (fault-injection green).
3. Golden-image suite: every component evaluator, compound compositing, per-mask group semantic parity with the global panel, and retouch modes — per process version, GPU + CPU within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; inserting the LOCAL/retouch nodes changed **zero** pre-existing goldens (no-PV-bump proof).
4. Determinism policy verified: clone/heal deterministic per backend; Remove and AiSeg replay baked rasters byte-stable across sessions and scales; staleness only ever set explicitly or by frame change, never silently recomputed.
5. Performance: §11 budgets green in the nightly scenario harness; the typical-wedding-edit case under 100 ms p95; the 30-mask number published with the trigger decision recorded (fused pass shipped or explicitly not needed).
6. `lb:` XMP round-trip property tests green; `crs:` export subset validated against fixtures; the non-mapped kinds documented (R7).
7. License surfaces green: OpenCV in the surface-2 SBOM with resolved license + link mode; no new crate violates cargo-deny; golden/test assets carry provenance entries.
8. Failure modes of §5.4/§4.5 (missing raster, degenerate geometry, cancelled bake, device-lost mid-paint) each degrade per spec, demonstrated by F6 tests.
9. Seam handoffs delivered: `SegRecipe` opacity + stale/recompute contract documented for E14; `RemoveBackend` trait + `MaskFrame` contract documented for E16/E11; F2 mapping doc for E16's importer.
10. Headless `lightbox-cli` mask/retouch flow exercisable end-to-end (F5) — the epic is demoable without the shell, per the headless-core principle.
