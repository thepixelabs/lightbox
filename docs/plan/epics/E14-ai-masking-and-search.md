> **SUPERSEDED (v2.0, 2026-07-05)** — replaced by [E14-ai-masking.md](E14-ai-masking.md); retained as historical record.

# E14 — AI Masking & Library Intelligence

| | |
|---|---|
| **Epic id** | E14 (`ai-masking-and-search`) |
| **Milestone** | M3 (Phase A: AI masking) with Phase B (search/faces/auto-tag) landing in M4 per §9 of the architecture |
| **Effort** | L, ~6–9 pw (task breakdown below sums to ~42 dev-days ≈ 8.5 pw; parallelizable across 2 engineers on the ML stream) |
| **Depends on** | E12 (mask engine, `Component::AiSeg` slot, mask tables), E13 (`lightbox-inferd`, IPC, model-pack manager, VRAM gating, supervision), E07 (catalog DAM: filter bar, FTS, smart-collection rule AST) |
| **Depended on by** | E16 (LR mask import, hardening, packaging of model packs) |
| **Architecture refs** | §1.5, §1.6 (model rows), §2.2 (`lightbox-mask`, `lightbox-ml`/`lightbox-inferd`), §3.1 (`mask_component`, `embedding`, `face/person/face_person`, `model_pack`), §3.3 (`masks/` store), §4.5 (baked-raster policy — Risk 7), §5.2/§5.3 (job classes, IPC boundary), §6 (inferd crash, VRAM, model-download failure rows), §8 (test gates), §9 (M3/M4 exits) |

---

## 1. Summary

E14 delivers the two halves of the "local AI matches the cloud tier" bet that sit on top of the E13 inference platform:

- **AI masking** — Select Subject (BiRefNet), Select Sky (SegFormer/ADE20K), Select Background (complement), and Select Objects (SAM2/MobileSAM point/box prompts) as `AiSeg` components inside E12's unified mask model, with the **baked-raster reproducibility policy of §4.5** implemented end-to-end: inference runs once, the raster is baked content-addressed into the recipe, and every subsequent render **replays the raster, never the model**.
- **Library intelligence** — CLIP semantic search ("dog on beach", zero keywords) over sqlite-vec, zero-shot auto-tagging feeding the FTS filter bar, and face detection + clustering (YuNet/SFace, Apache-licensed; InsightFace banned) with a minimal People surface — all as **pausable Background analyzers** surfaced in the activity center.

**Milestone split (matches §9 exactly):** Phase A (masking) is an M3 exit criterion ("AI-masked local adjustments render interactively; masks serialize to recipe + XMP; inference crash is isolated and recovered"). Phase B (semantic search + auto-tag + faces) is an M4 exit item. Both phases are specced here because they share the model-adapter, analyzer, and catalog machinery; Phase A tasks are strictly ordered before Phase B.

**First failing test of the epic (per §12 staff-engineer bar):** *AiSeg replay determinism* — create a Select Subject component on a fixture raw, kill `lightbox-inferd`, reopen the catalog, render the image: the mask weight raster is **byte-identical** to the post-bake render and **zero IPC calls** are made to inferd. This test encodes Risk 7's resolution and is written before any adapter code.

---

## 2. Scope

### 2.1 In scope

**Phase A — AI masking (M3):**
1. `lightbox-inferd` model-family adapters (running inside E13's adapter registry): BiRefNet subject matting, SegFormer-ADE20K sky segmentation, SAM2/MobileSAM promptable segmentation (encoder-session + interactive decoder).
2. The **bake pipeline**: render the current develop rendition at preview resolution → inference → inverse-map the raster into mask-anchor space → content-addressed raster in `masks/` → single-transaction catalog write (`mask_component.ai_recipe` + `cached_raster_ref` + history step).
3. `AiSeg` component **evaluation** in `lightbox-mask` (GPU + CPU): baked-raster load/upload, anchor→ROI transform, live-editable post-processing (threshold/soft, feather, expand/contract, invert) applied over the baked raw alpha.
4. Explicit **recompute** ("update AI mask") as a history-visible edit; derived (non-persisted) geometry-drift advisory; **no auto-recompute anywhere** (§4.5).
5. **Rebake-for-target-image primitive** consumed by E12's mask copy/sync and E09's adaptive presets (they own the UX/preset format; we own the "re-run this SegRecipe on that image" call).
6. Masking-panel AI buttons, Select Objects on-canvas prompt gizmo (points/box + live decode overlay), progress/error/CTA states (model pack missing, VRAM-gated, inferd down).
7. XMP projection of `AiSeg` components in the `lb:` namespace, including the baked raster (base64), so a sidecar round-trip preserves replay fidelity (mandate constraint 2).
8. Mask-raster store lifecycle: immutable, content-addressed, **excluded from cache eviction** (it is edit state, not cache), conservative GC of orphans.

**Phase B — Library intelligence (M4):**
9. CLIP (OpenCLIP ViT-B/32) image + text adapters, incl. a validated BPE tokenizer; `embedding` table + sqlite-vec ANN index; semantic search and similar-images queries integrated into the filter bar (E07/E08 seam); FTS fallback when the pack is absent.
10. Zero-shot **auto-tagging** against a Lightbox-authored tag vocabulary (~600 terms), written to `auto_tag` and indexed into FTS as hidden text.
11. **Faces**: YuNet detection + SFace embeddings, incremental + batch clustering with user confirm/reject pinning, person naming/merge/hide, `person`/`auto_tag`/`has_ai_mask` smart-collection rule atoms, minimal People panel + loupe face overlay.
12. **AnalyzerService**: resumable, pausable Background job pump over `analysis_state` with post-import triggers, backfill, model-change re-index, activity-center integration.

**Cross-cutting:** model-pack definitions (hashes, licenses, VRAM requirements) for all seven models; perf budgets; the §8 test gates for everything above.

### 2.2 Explicit non-goals

| Not in E14 | Where it lives / why |
|---|---|
| Mask engine core: weight buffers, brush/linear/radial/range components, boolean compositing, per-mask sub-recipes, mask panel/gizmo infrastructure | E12 (we plug `AiSeg` into its `Component` enum and gizmo layer) |
| `lightbox-inferd` process, IPC transport, supervision/restart, EP selection, model-pack download/verify/install, VRAM gating mechanism | E13 (we register adapters and declare per-pack VRAM requirements) |
| Filter bar UI, FTS infrastructure, smart-collection AST framework | E07 (we add rule atoms, an FTS text column, and a search mode via its extension points) |
| Content-aware Remove (LaMa), heal/clone/PatchMatch | E12.4 (classical) / E16 (LaMa) per §10 |
| Select People **body-part parsing**, Select Landscape multi-class parsing, depth masks | Could-tier, deferred (v1.x design headroom only; feature catalog §2.6) |
| Assisted culling, Best Photos, Lens Blur, adaptive *profiles*, ML Auto settings, preset recommendations, generative remove, super-resolution, neural raw denoise | Could/Won't or v1.x per feature catalog §2.7 and architecture deferred list |
| RAM++/GroundingDINO-class tagging/detection models | v1 auto-tag is zero-shot CLIP (reuses the search embeddings for near-zero marginal cost); RAM++ is a named upgrade path |
| Full People *module* (LR-style confirm-queue browsing at scale) | v1 ships the minimal panel below; module-grade UX is post-v1 polish |
| LR `.lrcat`/XMP **import** of Adobe AI masks | E16 (migration importer); we hand E16 the `SegRecipe`/rebake primitive it needs |
| Person-keyword export filtering ("strip person names on export") | E15 export metadata filtering; we expose person names in the metadata it filters |
| Duplicate detection (pHash) | E07/E16; not ML-platform work |

---

## 3. Crates & modules touched

Per the §2.1/§2.2 decomposition:

| Crate | E14 additions |
|---|---|
| `lightbox-ml` (client) | `seg` (SegRecipe/SegPrompt/bake orchestration `MaskBaker`), `embed` (CLIP client calls), `faces` (detect/embed calls, `FaceClusterer`), `analyzer` (`AnalyzerService`), `InferenceClientExt` IPC ops |
| `lightbox-inferd` (separate binary) | model-family **adapters** registered in E13's adapter registry: `birefnet`, `segformer_ade20k`, `sam2` (encoder-session cache + decoder), `openclip` (image+text+tokenizer), `yunet`, `sface`; shared tensor pre/post utils |
| `lightbox-mask` | `AiSegEvaluator` (GPU/CPU) for `Component::AiSeg`; raster texture cache; post-param kernels (threshold/feather/morphology/invert); anchor-space sampling |
| `lightbox-catalog` | migrations `0014_01..04`; DAOs: `EmbeddingDao`, `FaceDao`, `PersonDao`, `AutoTagDao`, `AnalysisStateDao`; smart-collection rule atoms `person`, `auto_tag`, `has_ai_mask` (AST extension via E07's registry) |
| `lightbox-core` | commands (`AddAiComponent`, `RecomputeAiComponent`, people commands, analyzer toggles) and queries (`semantic_search`, `similar_images`, `people`, `faces_in_image`, `analyzer_status`, geometry-drift advisory in mask views) |
| `lightbox-preview` | `MaskRasterStore` under `<catalog>.lbdata/masks/` (content-addressed, atomic, **non-evictable**); bake-input rendition file handoff |
| `lightbox-jobs` | job definitions only: one-click bake = **Foreground**, interactive seg decode = **Foreground (latency-sensitive)**, analyzers = **Background, pausable** (§5.3) |
| `lightbox-shell` | masking-panel AI buttons + states; Select Objects prompt gizmo + live overlay; semantic search mode in the filter bar; minimal People panel; loupe face overlay; analyzer rows in the activity center |
| `lightbox-edit` | none structurally — `Recipe.masks` stays an ordered `MaskId` list (§3.1 ownership note); E14 only defines the `lb:` XMP fields for AiSeg via E09's mapping layer |

---

## 4. Model roster (packs, licenses, VRAM)

All weights ship as E13 model packs (hash-pinned, `model_pack` rows, weights-license manifest — §8 surface 1). **Apache/MIT/BSD only; InsightFace banned (§1.5).**

| Pack id (v1 pin) | Model | Role | License | Input | Output | Min VRAM (GPU EP) |
|---|---|---|---|---|---|---|
| `seg-subject-birefnet-1` | BiRefNet (general, fp16 ONNX) | Select Subject | MIT | 1024², RGB | alpha 1024² | 2 GB |
| `seg-sky-segformer-b0-1` | SegFormer-B0 ADE20K | Select Sky | Apache-2.0 (NVIDIA SegFormer code is non-commercial — **use the HF transformers Apache export path; verify per-weight license at pin time, CTO sign-off**) | 512² | class logits → sky alpha | 1 GB |
| `seg-object-mobilesam-1` | MobileSAM (encoder+decoder ONNX) | Select Objects (default tier) | Apache-2.0 | 1024² | logits 256² → alpha | 1 GB |
| `seg-object-sam2-small-1` | SAM2.1-hiera-small (image-only export) | Select Objects (quality tier, ≥8 GB) | Apache-2.0 | 1024² | logits → alpha | 3 GB |
| `embed-openclip-vitb32-1` | OpenCLIP ViT-B/32 (image + text towers, BPE vocab) | semantic search, auto-tag, similar | MIT | 224² / 77 tokens | 512-d f32, L2-normed | 1 GB |
| `face-detect-yunet-1` | YuNet (OpenCV zoo) | face detection | MIT | ~640 long edge | bboxes + 5 landmarks + score | CPU-class |
| `face-embed-sface-1` | SFace (OpenCV zoo) | face embedding | Apache-2.0 | 112² aligned | 128-d f32 | CPU-class |

Notes:
- Every pack declares `min_vram_gpu`, `cpu_ok: bool`, and expected runtime class in its manifest; E13's VRAM gate consumes these. All seven are `cpu_ok = true` (subject/sky on CPU run seconds-scale with progress — acceptable for one-click ops; analyzers just run slower).
- The SegFormer license nuance above is a **named pin-time gate**: if no cleanly Apache-licensed sky-capable weight clears review, fallback is U2-Net-based sky (§1.6 names U2-Net as the fallback family). This is a pack swap behind the same adapter interface, not a design change.
- `model_pack` rows and the weights manifest are written in T03 and re-audited in T42.

---

## 5. Design

### 5.1 Coordinate spaces and the bake input

Two spaces matter:

- **View space** — the current develop rendition the user sees (post optics/geometry/crop), at preview resolution. All prompts (clicks, boxes) arrive in normalized view coordinates. Inference always runs on a view-space rendition — "select the subject I see" must respect the crop.
- **Mask-anchor space** — E12's component anchor space (content-anchored, pre-crop/rotate/perspective, post-orientation), in which all mask components are stored so masks stay glued to image content when the crop changes. E12 exposes the `anchor↔view` mapping in `MaskEvalCtx`; E14 consumes it, never defines it (seam #1, §7).

**Bake data flow:**

```
user action (button / prompt gizmo, view space)
  → RenderFacade::render_rendition(image, recipe, ≤1536px, sRGB8)   [Foreground job]
  → RenditionRef { path, content_hash, geometry_hash, px }
  → inferd: adapter runs model → MaskRaster (raw alpha8, model res, view space)
  → inverse geometry map: view → anchor space (out-of-crop = 0)      [client side]
  → MaskRasterStore::put(raster)  → ContentHash                      [atomic temp+rename]
  → catalog txn: mask_component { kind=AiSeg, ai_recipe=CBOR(SegRecipe),
       cached_raster_ref=hash, stale=0 } + edit_index.has_ai_mask + history_step
```

The **baked artifact is the raw model alpha** (pre-threshold, 8-bit, anchor space). Post-processing (threshold/soft, feather, expand, invert) is stored in the component's ordinary `params` blob and applied **at evaluation time** — deterministic, cheap, and re-editable without re-inference. This keeps §4.5's guarantee (replay is bit-deterministic) while making mask refinement a normal slider edit.

### 5.2 Staleness & recompute (§4.5, verbatim compliance)

- The persisted `mask_component.stale` flag is **only** set by the explicit `RecomputeAiComponent` command; nothing auto-invalidates a baked raster. Recompute produces a **new** raster (old raster retained — history references it), a new `SegRecipe` (new `baked_at`, `ep`, `geometry_hash`), and a history step. Undo restores the old `cached_raster_ref`.
- The UX need from research 04 ("panel flags stale AI masks with an update action") is met with a **derived, non-persisted advisory**: mask queries compare `SegRecipe.input.geometry_hash` against the image's current geometry hash and return `geometry_drift: bool`. The shell renders an "Update available" affordance; clicking it issues the explicit command. No DB write, no inference, until the user acts.
- A CI test asserts the invariant mechanically: mutate crop/geometry on an image with AI masks → **zero** IPC calls to inferd, raster ref unchanged.

### 5.3 Select Objects: interactive session

SAM2-family models split into a heavy image **encoder** (run once per rendition) and a light **decoder** (run per prompt). inferd holds an LRU of encoder outputs keyed by rendition hash (`SegSessionId`), so the interactive loop is: open session (encode, ≤1.5 s p95 GPU) → N × decode (≤150 ms p95) with the live mask overlay → accept (bake, §5.1) or cancel (close session). Session memory in inferd is bounded (LRU cap 3 sessions, TTL 5 min); eviction mid-loop transparently re-encodes.

### 5.4 Select Background

Baked as its **own raster** = 1 − subject alpha computed at bake time (one inference if no subject component exists yet, zero if derived from an existing one). Provenance records the source; the raster is independent afterward, so deleting the subject component later does not invalidate the background mask.

### 5.5 Library analyzers

One framework, three analyzers (`ClipEmbed`, `AutoTag`, `Faces`):

- **Background, pausable** jobs (§5.3) with per-analyzer rows in the activity center (the mandate's pausable-AI differentiator).
- Cursor = `analysis_state (image_id, analyzer, model_id, status)`; resumable across restarts and `kill -9` (each image's result + state row commit in one txn).
- Triggers: post-import hook (E04 emits imported `ImageId`s through core's event bus), launch backfill scan, explicit re-index on model change. Model change keeps serving the old index until the new one is complete, then swaps (search never goes dark).
- Inputs are T1 previews (E03); originals are never touched, offline assets analyze from previews.
- Throttle: bounded in-flight window (default 2) to inferd; analyzers yield entirely while Interactive/Foreground GPU work is active (E13 exposes the backpressure signal).

**Semantic search path:** query text → tokenizer+text tower (warm inferd session, memoized per query string) → 512-d vector → sqlite-vec ANN (k, oversampled ×4 when a scope filter applies, joined against the scope, min-cosine cutoff 0.2) → ranked `ImageId`s into the existing results grid. When the CLIP pack is missing/disabled the search bar stays plain FTS with a CTA (§6 model-download failure row: core features never block).

**Auto-tag:** vocabulary text embeddings are computed once per (vocab, model) and cached in the catalog; each image's tags = top-k (k=8) vocab terms above cosine 0.22 → `auto_tag` rows → FTS `ai_text` column. Tags are hidden metadata (search facets), never written to user keywords or XMP.

**Faces:** YuNet at ~640px long edge → per-face 5-point alignment → SFace 112² → 128-d embedding → `face` rows. Clustering is two-mode: **incremental** (new face joins nearest person centroid above cosine 0.6, as `source='auto'`) and **batch recluster** (agglomerative, average-linkage, cosine threshold 0.55, own ~200-LOC implementation — no GPL clustering deps) that **never moves `source='user'` assignments** (confirm/reject pinning). Persons are named/merged/hidden by user command; names flow into FTS and the `person` rule atom.

### 5.6 Degradation ladder (ties to §6 failure taxonomy)

| Condition | Behavior |
|---|---|
| Model pack not installed | Feature buttons disabled with install CTA; search falls back to FTS; analyzers idle |
| VRAM below pack gate | E13 gate → offer CPU EP ("slower, run anyway?") for one-click ops; analyzers default to CPU EP silently |
| inferd crash mid-op | E13 supervisor restarts; in-flight `JobFuture` fails → one automatic retry (re-encode for sessions) → user-visible error; **editor session untouched**; baked masks keep rendering (no inference on replay) |
| inferd down entirely | All AI entry points disabled with status notice; everything already baked/indexed keeps working |
| Original offline | Bake + analyzers run from previews (T1); no behavior change |

---

## 6. Interface definitions

Contracts, not implementations (illustrative per §2.2 convention). Types live in `lightbox-ml` unless noted.

### 6.1 Segmentation types & recipe (provenance — stored in `mask_component.ai_recipe` as CBOR)

```rust
pub enum SegKind { Subject, Sky, Background, Object }

#[derive(Serialize, Deserialize)]                 // CBOR; unknown-field-preserving
pub enum SegPrompt {
    Subject,
    Sky,
    Background { complement_of: Option<MaskComponentId> }, // None → fresh subject inference, inverted
    Object { points: Vec<PromptPoint>, bbox: Option<NormRect> },
}
pub struct PromptPoint { pub x: f32, pub y: f32, pub label: PointLabel } // normalized view space
pub enum PointLabel { Foreground, Background }

#[derive(Serialize, Deserialize)]
pub struct SegRecipe {                            // provenance + replay identity — never auto re-run (§4.5)
    pub schema: u16,                              // = 1
    pub kind: SegKind,
    pub model: ModelPin,                          // pack id + version + weights hash (E13 type)
    pub prompt: SegPrompt,
    pub input: RenditionProvenance,               // what inference saw
    pub ep: String,                               // execution provider used (provenance only)
    pub baked_at: i64,
}
pub struct RenditionProvenance {
    pub rendition_hash: ContentHash,              // hash of the rendered input
    pub geometry_hash: u64,                       // image geometry at bake time → drift advisory
    pub px: u32,                                  // rendition long edge
}

pub struct MaskRaster { pub w: u32, pub h: u32, pub alpha: Vec<u8>, pub space: RasterSpace }
pub enum RasterSpace { View, MaskAnchor }

// Post-processing — lives in mask_component.params (ordinary component params, live-editable):
pub struct MaskPostParams {
    pub mode: PostMode,                           // Soft (raw alpha) | Threshold { t: f32, softness: f32 }
    pub feather_px: f32,                          // separable gaussian on the weight buffer
    pub expand_px: f32,                           // signed morphological dilate/erode
    pub invert: bool,
}
```

### 6.2 Inference client extension (over E13's `InferenceClient` / IPC)

```rust
pub trait InferenceClientExt: InferenceClient {   // E13 owns transport, futures, cancellation, retry
    // one-shot segmentation (Subject / Sky):
    fn segment(&self, input: &RenditionRef, prompt: &SegPrompt, model: &ModelPin)
        -> JobFuture<MaskRaster>;
    // interactive promptable session (Select Objects):
    fn open_seg_session(&self, input: &RenditionRef, model: &ModelPin) -> JobFuture<SegSessionId>;
    fn seg_decode(&self, s: SegSessionId, prompt: &SegPrompt) -> JobFuture<MaskRaster>;
    fn close_seg_session(&self, s: SegSessionId);
    // embeddings:
    fn embed_image(&self, img: &PreviewRef, model: &ModelPin) -> JobFuture<EmbedVec>; // = §2.2 embed_clip, finalized
    fn embed_text(&self, model: &ModelPin, text: &str) -> JobFuture<EmbedVec>;
    // faces:
    fn detect_faces(&self, img: &PreviewRef, model: &ModelPin) -> JobFuture<Vec<FaceDet>>;
    fn embed_faces(&self, img: &PreviewRef, faces: &[FaceDet], model: &ModelPin)
        -> JobFuture<Vec<FaceEmbedding>>;
}
pub struct RenditionRef { pub path: PathBuf, pub hash: ContentHash, pub px: u32 } // rides E13's PreviewRef transport
pub struct FaceDet { pub bbox: NormRect, pub landmarks: [PointF; 5], pub score: f32 }
pub struct FaceEmbedding(pub [f32; 128]);         // SFace
pub struct EmbedVec(pub Vec<f32>);                // 512-d for embed-openclip-vitb32-1, L2-normalized
```

inferd-side, each family implements E13's adapter contract (final trait shape is E13's; E14's requirement is stated here):

```rust
// lightbox-inferd — E13 owns the registry/session machinery; E14 implements instances
pub trait ModelAdapter: Send + Sync {
    fn pack(&self) -> &ModelPackId;
    fn ops(&self) -> &[OpKind];                   // Segment | SegSession | EmbedImage | EmbedText | DetectFaces | EmbedFaces
    fn run(&self, sess: &mut OrtSessionHandle, req: OpRequest) -> Result<OpResponse, InferError>;
}
```

### 6.3 Bake orchestration & raster store

```rust
// lightbox-ml
pub struct MaskBaker<'a> { /* render façade + InferenceClientExt + MaskRasterStore + catalog writer */ }
impl MaskBaker<'_> {
    /// Full pipeline of §5.1. Foreground job; cancellable at every stage.
    pub async fn bake(&self, image: ImageId, prompt: SegPrompt, cancel: CancelToken)
        -> Result<BakedSeg, BakeError>;
    /// Re-run an existing SegRecipe: explicit recompute, mask copy/sync, adaptive presets (seam #3/#4).
    pub async fn rebake(&self, target: ImageId, recipe: &SegRecipe, cancel: CancelToken)
        -> Result<BakedSeg, BakeError>;
}
pub struct BakedSeg { pub raster_ref: ContentHash, pub recipe: SegRecipe }
pub enum BakeError { PackMissing(ModelPackId), VramGated { pack: ModelPackId, offer_cpu: bool },
                     InferdUnavailable, Cancelled, Inference(String), Io(io::Error) }

// lightbox-preview
pub struct MaskRasterStore { /* root = <catalog>.lbdata/masks/ */ }
impl MaskRasterStore {
    pub fn put(&self, r: &MaskRaster) -> io::Result<ContentHash>;   // gray8 PNG, atomic, idempotent
    pub fn get(&self, h: &ContentHash) -> io::Result<MaskRaster>;
    pub fn contains(&self, h: &ContentHash) -> bool;
    /// Conservative GC: `live` = every hash referenced by mask_component ∪ snapshot docs ∪ history steps.
    pub fn gc(&self, live: &HashSet<ContentHash>) -> io::Result<GcReport>;
}
// INVARIANT: masks/ is authoritative edit state — immutable files, never LRU-evicted,
// deleted only via gc() with the catalog-derived live set.
```

### 6.4 `AiSeg` evaluation (`lightbox-mask`, plugging into E12's evaluator registry)

```rust
pub struct AiSegState { pub raster_ref: ContentHash, pub post: MaskPostParams }

impl ComponentEvaluator for AiSegEvaluator {
    /// Load baked raster (texture cache keyed by raster hash) → sample anchor→ROI via
    /// ctx's geometry map (bilinear + edge-aware refine) → apply MaskPostParams on GPU.
    fn eval_gpu(&self, ctx: &MaskEvalCtx, s: &AiSegState, out: &mut WeightBuffer);
    fn eval_cpu(&self, ctx: &MaskEvalCtxCpu, s: &AiSegState, out: &mut WeightBufferCpu); // §4.4-tolerance parity
}
```

### 6.5 Analyzer service

```rust
pub enum AnalyzerKind { ClipEmbed, AutoTag, Faces }
pub struct AnalyzerService { /* jobs runtime + AnalysisStateDao + InferenceClientExt */ }
impl AnalyzerService {
    pub fn set_enabled(&self, k: AnalyzerKind, on: bool);           // persisted preference
    pub fn pause(&self, k: AnalyzerKind);
    pub fn resume(&self, k: AnalyzerKind);
    pub fn notify_imported(&self, images: &[ImageId]);              // E04 post-import hook
    pub fn reindex(&self, k: AnalyzerKind, model: &ModelPin);       // model change; old index serves until swap
    pub fn status(&self) -> Vec<AnalyzerProgress>;                  // activity-center rows
}
pub struct AnalyzerProgress { pub kind: AnalyzerKind, pub done: u64, pub total: u64,
                              pub failed: u64, pub state: RunState /* Running|Paused|Idle|Disabled */ }
```

### 6.6 Catalog DAOs

```rust
impl EmbeddingDao {
    /// Writes embedding row + vec0 index row in the SAME transaction (crash-safe by construction).
    pub fn upsert(&self, w: &mut WriterTxn, image: ImageId, model: &ModelPin, v: &EmbedVec) -> Result<()>;
    pub fn ann(&self, q: &EmbedVec, k: usize, scope: Option<&ScopeFilter>) -> Result<Vec<(ImageId, f32)>>;
    pub fn missing_for(&self, model: &ModelPin, limit: usize) -> Result<Vec<ImageId>>;
    pub fn swap_index(&self, w: &mut WriterTxn, from: &ModelPin, to: &ModelPin) -> Result<()>;
}
impl FaceDao {
    pub fn insert_faces(&self, w: &mut WriterTxn, image: ImageId, faces: &[(FaceDet, FaceEmbedding)]) -> Result<Vec<FaceId>>;
    pub fn unassigned(&self, limit: usize) -> Result<Vec<FaceRow>>;
    pub fn assign(&self, w: &mut WriterTxn, face: FaceId, person: Option<PersonId>, src: AssignSource) -> Result<()>;
    pub fn person_centroids(&self) -> Result<Vec<PersonCentroid>>;
}
impl PersonDao { pub fn create(..) -> Result<PersonId>; pub fn rename(..); pub fn merge(..); pub fn set_hidden(..); }
pub enum AssignSource { Auto, User }
```

### 6.7 Core commands & queries (headless boundary — no UI types)

```rust
pub enum AiCommand {
    AddAiComponent     { image: ImageId, mask: Option<MaskId>, prompt: SegPrompt, bool_op: BoolOp },
    RecomputeAiComponent { image: ImageId, component: MaskComponentId },      // the ONLY stale-setter (§4.5)
    RenamePerson { person: PersonId, name: String },
    MergePersons { into: PersonId, from: Vec<PersonId> },
    SetPersonHidden { person: PersonId, hidden: bool },
    AssignFace   { face: FaceId, person: Option<PersonId> },                  // source=User; None = reject/unassign
    ReclusterFaces,
    SetAnalyzer  { kind: AnalyzerKind, enabled: bool },
}
impl QueryFacade {
    pub fn semantic_search(&self, text: &str, k: usize, scope: Option<SourceScope>) -> Result<Vec<SemanticHit>>;
    pub fn similar_images(&self, image: ImageId, k: usize) -> Result<Vec<SemanticHit>>;
    pub fn people(&self) -> Result<Vec<PersonSummary>>;                       // incl. unnamed clusters + counts
    pub fn faces_in_image(&self, image: ImageId) -> Result<Vec<FaceView>>;
    pub fn analyzer_status(&self) -> Result<Vec<AnalyzerProgress>>;
    // mask views gain: pub geometry_drift: bool  (derived advisory, §5.2)
}
pub struct SemanticHit { pub image: ImageId, pub score: f32 }
```

---

## 7. Seams to neighboring epics (named, not designed)

1. **E12 → E14 (mask engine):** E12 defines `Component::AiSeg`, the `mask`/`mask_component` tables, `MaskEvalCtx` with the **anchor↔view geometry map**, the component-evaluator registry, boolean compositing, and the mask panel/gizmo framework. E14 registers the `AiSegEvaluator`, fills `ai_recipe`/`cached_raster_ref`/`stale`, and adds prompt gizmos on E12's gizmo layer.
2. **E13 → E14 (inference platform):** E13 owns inferd, IPC transport + `JobFuture` + cancellation, adapter registry + ORT session lifecycle, model-pack download/verify/install, VRAM gate, supervision/restart, CPU-EP fallback policy, and the Interactive/Foreground GPU backpressure signal. E14 registers adapters, declares pack manifests, and consumes `InferenceClientExt`. If E13's adapter trait diverges from §6.2's sketch, E14 conforms — the semantic op set (Segment/SegSession/EmbedImage/EmbedText/DetectFaces/EmbedFaces) is E14's requirement.
3. **E14 → E12 (mask copy/sync):** E12's copy/sync UX calls `MaskBaker::rebake(target, recipe)` for AI components (merge-or-replace semantics are E12's).
4. **E14 → E09 (presets & XMP):** adaptive presets store `SegPrompt`+`ModelPin` (not rasters) and call `rebake` on apply — preset format is E09's; the `lb:` field mapping for AiSeg (T23) goes through E09's mapping layer.
5. **E07 ↔ E14 (DAM):** E07 owns the filter bar, FTS, and rule-AST framework; E14 contributes the `ai_text` FTS column content (auto-tags + person names), the `person`/`auto_tag`/`has_ai_mask` rule atoms via E07's AST extension registry, and the semantic search query the shell's search bar invokes. The `0014_04` FTS rebuild migration is written by E14 and **reviewed by the E07 owner** (it recreates their index).
6. **E04 → E14:** post-import event (`ImageId` batch) on core's event bus drives `notify_imported`.
7. **E03/E01 → E14:** T1 previews are analyzer/bake inputs; `masks/` lives in the `.lbdata` layout (§3.3) but is **exempt from E03's eviction**; whether `masks/` joins E01's exit-time verified backup is Open Question Q3 (coordination item, not designed here).
8. **E14 → E16:** hands over `SegRecipe`/`rebake` + the `lb:` XMP schema for the LR mask importer, and the model packs for packaging/first-run download UX.
9. **E14 → E15:** person names appear in the metadata model; export-time "strip person keywords" filtering is E15's.

---

## 8. Data model & migrations

E14 assumes E12's migration already created `mask`/`mask_component` (with `ai_recipe BLOB`, `cached_raster_ref TEXT`, `stale INTEGER`) and E13's created `model_pack`. E14 adds four migrations (copy-on-write upgrade per §6; `schema_version` bumped once per release train per E01 policy):

```sql
-- 0014_01_embeddings.sql
CREATE TABLE embedding (
  image_id  INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  model_id  TEXT    NOT NULL,                 -- e.g. 'embed-openclip-vitb32-1'
  dim       INTEGER NOT NULL,
  vec       BLOB    NOT NULL,                 -- f32[dim], L2-normalized; source of truth
  built_at  INTEGER NOT NULL,
  PRIMARY KEY (image_id, model_id)
) WITHOUT ROWID;

-- ANN index: derived, rebuildable from `embedding`; one active model at a time.
-- vec0 rows are inserted/deleted inside the same txn as `embedding` rows (crash-safe).
CREATE VIRTUAL TABLE embedding_idx USING vec0(
  image_id  INTEGER PRIMARY KEY,
  v         FLOAT[512] distance_metric=cosine
);

-- 0014_02_faces.sql
CREATE TABLE face (
  id          INTEGER PRIMARY KEY,
  image_id    INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  det_model   TEXT NOT NULL,
  embed_model TEXT NOT NULL,
  bbox_x REAL NOT NULL, bbox_y REAL NOT NULL,  -- normalized [0,1], anchor-oriented image space
  bbox_w REAL NOT NULL, bbox_h REAL NOT NULL,
  landmarks   BLOB NOT NULL,                   -- 5 × (f32,f32), normalized
  det_score   REAL NOT NULL,
  embedding   BLOB NOT NULL,                   -- f32[128] (SFace)
  built_at    INTEGER NOT NULL
);
CREATE INDEX idx_face_image ON face(image_id);

CREATE TABLE person (
  id            INTEGER PRIMARY KEY,
  name          TEXT,                          -- NULL = unnamed cluster
  hidden        INTEGER NOT NULL DEFAULT 0,
  cover_face_id INTEGER REFERENCES face(id) ON DELETE SET NULL,
  created_at    INTEGER NOT NULL
);

CREATE TABLE face_person (
  face_id   INTEGER PRIMARY KEY REFERENCES face(id) ON DELETE CASCADE,
  person_id INTEGER NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  source    TEXT NOT NULL CHECK (source IN ('auto','user')),   -- 'user' pins survive recluster
  score     REAL                                               -- cosine to centroid at assign time
);
CREATE INDEX idx_face_person_person ON face_person(person_id);

-- 0014_03_analysis.sql
CREATE TABLE auto_tag (
  image_id   INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  tag        TEXT NOT NULL,
  confidence REAL NOT NULL,
  model_id   TEXT NOT NULL,
  PRIMARY KEY (image_id, tag, model_id)
) WITHOUT ROWID;
CREATE INDEX idx_auto_tag_tag ON auto_tag(tag);

CREATE TABLE analysis_state (
  image_id   INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  analyzer   TEXT NOT NULL CHECK (analyzer IN ('clip_embed','auto_tag','faces')),
  model_id   TEXT NOT NULL,
  status     TEXT NOT NULL CHECK (status IN ('pending','done','failed','skipped')),
  attempts   INTEGER NOT NULL DEFAULT 0,
  error      TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (image_id, analyzer)
) WITHOUT ROWID;

-- vocabulary text-embedding cache (computed once per (vocab_hash, model))
CREATE TABLE vocab_embedding (
  vocab_hash TEXT NOT NULL, model_id TEXT NOT NULL, term TEXT NOT NULL,
  vec BLOB NOT NULL,
  PRIMARY KEY (vocab_hash, model_id, term)
) WITHOUT ROWID;

-- 0014_04_fts_ai_text.sql  (coordinated with E07 — rebuilds their index)
-- Recreate assets_fts with an `ai_text` column (FTS5 cannot ALTER ADD COLUMN);
-- repopulate from metadata_cache/keywords (E07 projector) + auto_tag + person names (E14 projector).
DROP TABLE assets_fts;
CREATE VIRTUAL TABLE assets_fts USING fts5(
  filename, keywords, caption, camera,
  ai_text,                    -- auto-tags + confirmed person names (hidden search text)
  content=''                  -- external-content pattern per E07
);
-- rebuild driven by the migration runner via the FTS projector hooks
```

Storage notes:
- `masks/<hh>/<hash>.png` (§3.3): gray8 PNG, typically 60–300 KB at 1024² — **edit state, not cache** (never evicted; GC per §6.3).
- vec0 at 100k × 512-d f32 ≈ 200 MB scan worst-case; brute-force is within the ≤100 ms ANN budget on desktop NVMe/RAM. The §7 scale-check's IVF/partitioning mitigation is pre-named for 1M; not built in v1.
- `embedding.vec` is the source of truth; `embedding_idx` is derived and rebuildable (`swap_index` on model change, integrity re-check on catalog `.recover`).

---

## 9. XMP mapping (through E09's mapping layer)

No `crs:` equivalent exists for our AI components; everything is `lb:` namespace (§3.2 rule: Lightbox-only fields → `lb:`). Adobe's mask XMP is **not** imitated (constraint 4); E16's importer maps LR masks → our model separately.

| Field | Content |
|---|---|
| `lb:MaskComponentKind` | `AiSeg` |
| `lb:SegKind` | `Subject \| Sky \| Background \| Object` |
| `lb:SegModel` / `lb:SegModelVersion` / `lb:SegModelHash` | `ModelPin` |
| `lb:SegPrompt` | CBOR→base64 of `SegPrompt` (normalized coords) |
| `lb:SegInputGeometryHash`, `lb:SegInputPx`, `lb:SegEp`, `lb:SegBakedAt` | `RenditionProvenance` + provenance |
| `lb:SegRasterW/H` + `lb:SegRaster` | baked raw-alpha raster, PNG→base64 — **included by default** so a sidecar alone replays the mask identically (the raster *is* the edit per §4.5); pref `xmp.embed_ai_rasters=false` drops it to provenance-only for size-sensitive users (documented trade-off: replay then requires rebake on foreign machines) |
| `lb:MaskPost*` | `MaskPostParams` fields |

Round-trip contract (property-tested): write → read restores a replayable component whose raster bytes and recipe CBOR are identical. Unknown `lb:` fields from newer builds ride `xmp_passthrough` (§3.2).

Auto-tags and face/person data are **catalog-only** (not projected to XMP in v1; person-keyword projection is an E15/E16 decision).

---

## 10. Task breakdown (ordered; each ≤1 day)

Effort key: each task ≈ 0.5–1 dev-day; **AC** = acceptance criteria. Dependencies flow top-to-bottom within a phase; Phase B does not start before T05/T10 (shared plumbing) but may overlap Phase A's UI tasks across two engineers.

### Phase 0 — Foundations (5 tasks)

| # | Task | AC |
|---|---|---|
| **T01** | Catalog migrations `0014_01..04` + DAO skeletons (`EmbeddingDao`, `FaceDao`, `PersonDao`, `AutoTagDao`, `AnalysisStateDao`) | Migrations apply on fresh + previous-version catalogs; copy-on-write upgrade preserves the original; `kill -9` mid-migration leaves the old catalog intact and openable; DAO unit tests green on in-memory catalog |
| **T02** | `SegRecipe`/`SegPrompt`/`MaskPostParams`/`MaskRaster` types + CBOR ser/de | `proptest` round-trip identity; unknown-field preservation on decode of a future-schema doc; recipe schema documented in-crate |
| **T03** | Model-pack definitions for all 7 packs (manifest: hashes, licenses, `min_vram_gpu`, `cpu_ok`) + weights-license manifest entries | Surface-1 CI gate passes; SegFormer license path resolved or U2-Net fallback pack swapped in; CTO sign-off items filed |
| **T04** | inferd adapter plumbing: register E14 families in E13's registry; shared pre/post utils (resize/pad/normalize NCHW, sigmoid→alpha8, argmax→class mask, NMS) | Tensor-fixture unit tests; adapters enumerate under `--list-ops` in headless inferd |
| **T05** | `MaskRasterStore`: content-addressed gray8 PNG, atomic temp+rename, idempotent put, non-evictable flag in preview-store accounting | Crash-injection during put leaves no partial file visible; store size excluded from LRU cap; get(put(x)) == x |

### Phase A — AI masking (19 tasks, M3)

| # | Task | AC |
|---|---|---|
| **T06** | **First failing test**: AiSeg replay-determinism harness (fixture catalog, mock adapter) — written red before adapters exist | Test exists and fails for the right reason; goes green at T14; asserts byte-identical weight raster + zero IPC on replay with inferd killed |
| **T07** | BiRefNet subject adapter | IoU ≥ 0.90 vs reference masks on 20-image fixture corpus (CPU EP); raw alpha8 output at model res; runs under fp16 on GPU EPs |
| **T08** | SegFormer sky adapter | Sky IoU ≥ 0.85 on sky fixtures (CPU EP); no-sky fixtures produce < 2% false-positive area |
| **T09** | SAM2/MobileSAM encoder-session management in inferd (LRU cap 3, TTL 5 min, keyed by rendition hash) | Encode-once/decode-N verified; bounded memory under session churn; eviction mid-loop re-encodes transparently |
| **T10** | SAM2/MobileSAM decoder + prompt encoding (points/box) | Prompt fixtures IoU ≥ 0.85; decode p95 ≤ 150 ms on baseline GPU |
| **T11** | `InferenceClientExt` IPC ops (segment, session ops) + cancellation | Cancel mid-encode aborts without inferd restart; inferd crash mid-op → error + supervisor restart + one auto-retry (E13 policy) verified end-to-end |
| **T12** | Bake-input rendition: `RenderFacade::render_rendition` (current recipe, ≤1536 px, sRGB8, view space) → `RenditionRef` with `geometry_hash` | Rendition matches canvas render within golden tolerance; identical recipe → identical `rendition_hash` |
| **T13** | View→anchor inverse mapping + raster resample into `MaskAnchor` space (out-of-crop = 0) using E12's geometry map | Synthetic crop/rotate/perspective round-trip error ≤ 0.5 px at preview res; property test over random geometries |
| **T14** | Bake transaction: raster put + `mask_component` write (`ai_recipe`, `cached_raster_ref`, `stale=0`) + `edit_index.has_ai_mask` + history step, single WAL txn | T06 goes green; `kill -9` mid-bake → catalog clean, orphan raster swept by GC; history step present |
| **T15** | `AiSegEvaluator` GPU path: raster texture cache (keyed by hash), anchor→ROI sampling, post params (threshold/soft, feather, expand, invert) in WGSL | Golden weight-buffer tests; eval adds ≤ 10 ms p95 per AI component at fit-view; texture cache hit on repeated eval |
| **T16** | `AiSegEvaluator` CPU path (rayon) | CPU/GPU parity within §4.4 tolerance on the golden set |
| **T17** | Select Background derived bake (1 − subject; reuse existing subject raster when present) | Complement golden; zero extra inference when derived; independent raster survives source-component deletion |
| **T18** | Core command `AddAiComponent` (Foreground job: rendition → infer → bake → txn; progress; failure CTAs) | End-to-end on fixture; pack-missing → disabled + install CTA; VRAM-gated → CPU-EP offer; UI thread never blocks |
| **T19** | Core command `RecomputeAiComponent` (explicit; new raster; old retained; history step; stale lifecycle) | Undo restores previous `cached_raster_ref`; **geometry change triggers zero inference calls** (mechanical §4.5 test); recompute visible in history panel |
| **T20** | Geometry-drift advisory (derived, non-persisted) in mask query views | Crop change → `geometry_drift=true` in query result; no DB write; no inference; clears after recompute |
| **T21** | `MaskBaker::rebake` for copy/sync + adaptive presets (seams #3/#4) | Syncing a subject mask to a second fixture image produces a target-specific raster with fresh provenance; E12's merge/replace tests consume it |
| **T22** | Shell: masking-panel AI buttons (Subject/Sky/Background) + progress/error states | Click→mask visible ≤ 2.5 s p95 on baseline GPU fixture; states for running/failed/gated/pack-missing all reachable in a UI test |
| **T23** | Shell: Select Objects gizmo — +/− points, box drag, live decode overlay via session; accept/cancel | Interactive loop p95 ≤ 150 ms per decode; accept bakes exactly the previewed raster; cancel closes the session |
| **T24** | XMP projection of AiSeg (`lb:` fields incl. base64 raster; read-back) through E09's mapping layer | Sidecar round-trip → byte-identical raster + recipe CBOR; `embed_ai_rasters=false` path emits provenance-only; property tests in E09's harness |

### Phase A hardening (1 task)

| # | Task | AC |
|---|---|---|
| **T25** | Raster GC command (live set = `mask_component` ∪ snapshot docs ∪ history steps) + orphan sweep on launch | Property test over random edit/undo/snapshot sequences: a referenced raster is never deleted; orphans from crashed bakes are removed; GC report surfaced in activity center |

### Phase B — Library intelligence (13 tasks, M4)

| # | Task | AC |
|---|---|---|
| **T26** | OpenCLIP image adapter (224² preproc, L2-norm) + text adapter + own BPE tokenizer (vocab ships in pack) | Tokenizer exact-match vs OpenCLIP reference token vectors on a 200-string suite; dog-image scores "a photo of a dog" > "a photo of a car" by documented margin on fixtures |
| **T27** | `EmbeddingDao`: same-txn upsert (row + vec0), ANN with scope filter (oversample ×4 + join), `swap_index` | `kill -9` mid-upsert → row and index consistent; ANN k=200 @ synthetic 100k ≤ 100 ms; scope-filtered results correct |
| **T28** | `AnalyzerService` framework: pausable Background jobs, `analysis_state` cursor, bounded in-flight window, retry-with-cap, activity-center rows | Pause/resume mid-run; app restart resumes from cursor; failures retried ≤ 3 then `failed` with error string; GPU yield under Interactive load verified |
| **T29** | Analyzer triggers: post-import hook (E04 event), launch backfill, model-change reindex with index swap | Import 100 fixtures → embeddings appear without user action; model swap reindexes with zero search downtime |
| **T30** | Semantic search query path: warm text-embed (memoized) → ANN → hydrate, min-score cutoff; FTS fallback when pack missing | E2E ≤ 300 ms p95 warm @ 100k synthetic; "red car" fixture ranks the red-car image top-3; pack-missing → plain FTS + CTA |
| **T31** | Similar-images query + shell context-menu hook | Near-duplicate fixture ranks its sibling #1; query ≤ 150 ms @ 100k |
| **T32** | Shell: semantic mode in the filter bar (mode toggle, ranked grid results, "n of m indexed" state) | Keyboard-reachable; partial-index state communicated; ESC returns to FTS mode; no UI stall while embedding text |
| **T33** | Auto-tag: Lightbox-authored vocabulary asset (~600 terms, surface-3 manifest entry) + zero-shot scoring (cached vocab embeddings) → `auto_tag` rows | Top-5 tag relevance ≥ 60% on a 50-image labelled fixture; idempotent per (image, model); vocab hash change triggers re-tag |
| **T34** | FTS `ai_text` wiring (0014_04 rebuild, E07-reviewed) — auto-tags + person names searchable | Free-text "beach" matches an auto-tagged image with zero user keywords; person rename updates FTS; E07 owner sign-off recorded |
| **T35** | YuNet detect adapter + SFace align (5-pt affine) + embed adapter | Detection recall ≥ 0.9 for ≥ 40 px faces on group-photo fixtures; embedding verification spot-check on a labelled pair set ≥ agreed threshold; both run CPU-class |
| **T36** | Faces analyzer (detect+embed → `face` rows) on `AnalyzerService` | ≥ 2 img/s on baseline GPU, ≥ 0.5 img/s CPU; resume-safe; bboxes stored in anchor-oriented normalized space |
| **T37** | Clustering: incremental centroid assignment + batch recluster (agglomerative, own impl) respecting `source='user'` pins | Deterministic for fixed input order; ≥ 0.9 cluster purity on labelled fixture; user-confirmed assignment never moved by recluster/incremental pass |
| **T38** | People commands/queries + smart-collection rule atoms `person`/`auto_tag`/`has_ai_mask` (E07 AST registry) | Rule→SQL unit tests; merge/hide semantics tested (hidden persons excluded from suggestions and search); atoms compose with existing any/all/none groups |
| **T39** | Shell: minimal People panel (cluster grid, cover crops from preview+bbox, rename/merge/hide, unnamed pool) + loupe face-box hover overlay | Panel interactive at 3k faces (virtualized); end-to-end naming flow: name cluster → person searchable in filter bar |

### Phase C — Hardening & gates (3 tasks)

| # | Task | AC |
|---|---|---|
| **T40** | Failure-mode integration suite: inferd crash mid-bake & mid-analyzer (restart/retry/session re-encode), corrupt pack (disabled + CTA), VRAM-gate paths, offline originals, disk-full during raster put | All §5.6 rows exercised on the CI matrix (macOS/Windows/Linux); editor session survives every case |
| **T41** | Golden & determinism gates into §8 harness: baked-raster replay byte-identity (PR-blocking), AiSeg weight-buffer goldens per PV, CPU/GPU eval parity, per-EP IoU sanity floors (nightly, per model) | Gates wired PR-blocking (replay, goldens, parity) and nightly (EP IoU, perf); baselines committed |
| **T42** | Perf-harness runs against §11 budgets (nightly, baseline hw) + docs (recipe schema, pack authoring, seam handoff notes to E15/E16) + final license/manifest audit | Nightly job green with recorded baselines; weights + vocab manifest entries re-audited; E16 handoff doc reviewed; DoD checklist complete |

**Total: 42 tasks** (~42 dev-days ≈ 8.5 pw with review/integration slack inside the L 6–9 pw band; Phases A and B parallelize across the two ML-stream engineers after T05).

---

## 11. Performance budgets (asserted by T42's nightly harness; baseline = §7 hardware)

| Operation | Budget (GPU EP, baseline) | CPU-EP contract |
|---|---|---|
| One-click Subject/Sky (click → mask on canvas, incl. rendition render + bake) | ≤ 2.5 s p95 | ≤ 30 s with progress, cancellable |
| Select Objects: session open (encode) | ≤ 1.5 s p95 | ≤ 15 s with progress |
| Select Objects: per-prompt decode → overlay | ≤ 150 ms p95 | ≤ 2 s |
| AiSeg replay eval (per component, fit-view ROI) | ≤ 10 ms p95 (never calls inferd) | §4.4 preview-res CPU contract |
| CLIP indexing throughput (Background) | ≥ 4 img/s | ≥ 0.5 img/s |
| Faces throughput (Background) | ≥ 2 img/s | ≥ 0.5 img/s |
| Semantic query e2e (warm text tower, 100k assets) | ≤ 300 ms p95 | same (ANN is CPU-side anyway) |
| ANN k=200 @ 100k × 512-d | ≤ 100 ms | same |
| Analyzer impact on interactive canvas | zero starvation (§5.3 preemption; measured slider p95 unchanged while indexing) | same |

---

## 12. Test plan (per §8 strategy)

**Unit (PR-blocking):**
- SegRecipe/SegPrompt/MaskPostParams CBOR round-trip + unknown-field preservation (`proptest`).
- Tokenizer vs OpenCLIP reference vectors; pre/post tensor utils; prompt normalization; NMS.
- Clustering determinism, purity on labelled fixture, user-pin invariance.
- DAO logic incl. same-txn vec0 consistency; rule-atom AST→SQL; GC live-set computation.

**Integration (PR-blocking fast subset; full nightly, via `lightbox-cli` headless):**
- **Replay determinism (the epic's first failing test, T06):** bake → kill inferd → reopen → render: byte-identical weight raster, zero IPC.
- **No-auto-recompute:** geometry edits on AI-masked images provoke zero inference calls (§4.5 mechanical guard).
- Recompute-as-edit: history step, undo restores old raster, old raster survives GC while referenced.
- Analyzer resume across pause/restart/`kill -9`; post-import trigger; model-swap with zero search downtime.
- Failure-mode suite (T40): inferd crash/restart/retry, pack corrupt/missing, VRAM gate, disk-full, offline originals.
- XMP sidecar round-trip of AiSeg (with and without embedded raster) through E09's harness.
- `kill -9` fault injection on every new write path (bake txn, embedding upsert, face insert, FTS rebuild) → `integrity_check` clean.

**Golden-image (PR-blocking, per process version — §8):**
- AiSeg weight-buffer goldens: fixed baked rasters × post-param sets → committed goldens; CPU/GPU parity within §4.4 tolerance.
- Full-pipeline goldens: recipe with AI-masked local adjustments renders within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB per PV. **Inference outputs are deliberately NOT golden-tested across EPs** (§4.5 non-determinism); determinism is asserted on *replay*, quality on *IoU floors*.

**Model-quality (nightly, per EP class):**
- IoU sanity floors per model on fixture corpora (subject ≥ 0.90, sky ≥ 0.85, object-prompt ≥ 0.85, face recall ≥ 0.9); retrieval sanity (fixture query→image ranks); auto-tag relevance floor. Regression files an issue (non-blocking, tracked) per §8 perf-lane policy.

**Perf (nightly):** §11 budget harness; slider-latency-under-indexing scenario; ANN scaling probe at 10k/100k/250k synthetic embeddings.

**License gates (PR-blocking):** weights manifest (surface 1) for all seven packs; vocab asset in the surface-3 data manifest; no InsightFace anywhere (explicit denylist entry).

---

## 13. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **SAM2 ONNX export complexity** (encoder/decoder split, image-only path) is fiddlier than BiRefNet/SegFormer | MobileSAM (proven ONNX exports) is the *default* tier; SAM2-small is the quality tier and can slip to a pack update without code change — adapter interface is identical |
| R2 | **SegFormer weight licensing** (NVIDIA research license vs HF Apache exports) | Named pin-time gate in T03 with U2-Net sky fallback pre-approved (§1.6); CTO sign-off required either way |
| R3 | **Cross-EP segmentation quality variance** (CoreML fp16 vs DirectML vs CPU) degrades masks on some platforms | Per-EP IoU floors in nightly CI; provenance records `ep`; §4.5 means users never silently lose an existing mask to EP drift |
| R4 | **`masks/` loss = edit loss** — baked rasters are authoritative but live outside `catalog.sqlite`, and E01's verified backup covers only the DB | XMP-embedded rasters (T24 default-on) give per-file redundancy; Open Question Q3 escalates adding `masks/` to the verified backup; GC is conservative-by-construction |
| R5 | **XMP sidecar bloat** (100–300 KB per AI component) annoys sidecar-heavy workflows | Documented; pref to strip rasters (provenance-only) with the replay trade-off stated; typical wedding edit has ≤ 3 AI components/image |
| R6 | **SFace clustering quality** below ArcFace-class on hard libraries (profile faces, children) | Thresholds tuned on fixtures; user confirm/reject pinning bounds the damage; embed model is pack-swappable (only Apache/MIT candidates) with reindex path already built (T29) |
| R7 | **Anchor-space inverse mapping fidelity** at heavy perspective/crop boundaries could shift mask edges | ≤ 0.5 px round-trip AC in T13; edge cases in the golden set; drift advisory + explicit recompute is the designed escape hatch |
| R8 | **vec0 brute-force scan** at ≥ 250k assets approaches the 100 ms ANN budget | §7 scale-check already names IVF/partitioned sqlite-vec as the mitigation; scaling probe in nightly perf warns before users hit it |
| R9 | **Analyzer battery/thermal footprint** on laptops (hours of background inference after big imports) | Pausable + activity-center visibility (mandate differentiator); default in-flight window of 2; follow-up pref (on-AC-power-only) noted for E08's prefs panel — not built here |

### Open questions

| # | Question | Owner / forum | Default if unresolved |
|---|---|---|---|
| Q1 | OpenCLIP ViT-B/32 vs SigLIP-base as the v1 retrieval pin (SigLIP retrieves better; needs sentencepiece tokenizer instead of BPE) | E14 eng + fixture eval before T26 | OpenCLIP ViT-B/32 (simpler tokenizer, MIT, proven in Immich-class deployments) |
| Q2 | Auto-tag vocabulary curation (~600 terms): who authors/reviews the list (it is a shipped, surface-3-manifested content asset) | E14 eng drafts; product review | Ship the draft list, iterate in point releases; vocab hash versioning (T33) makes swaps cheap |
| Q3 | Does `masks/` join E01's exit-time verified backup (it is authoritative edit state, unlike everything else in `.lbdata`)? | E01 owner + E14; escalate at M3 planning | XMP-embedded rasters are the interim redundancy (R4) |
| Q4 | Per-person XMP keyword projection (writing person names as keywords on export) — privacy-sensitive | E15/E16 with product | Catalog-only in v1; E15 filters person names out of exports by default |
| Q5 | Exact E13 adapter-trait shape and whether tokenization lives inferd-side (this spec's assumption) or client-side | E13 owner; resolve before T04 | Conform to E13; the op set of §6.2 is the requirement, placement is negotiable |
| Q6 | People panel depth in v1 (minimal panel here vs LR-style confirm queue) | Product at M4 planning | Ship T39's minimal panel; confirm-queue UX is post-v1 |

---

## 14. Definition of done

E14 is done when all of the following hold on the CI matrix (macOS/Metal+CoreML, Windows/DX12+DirectML, Linux/Vulkan+CPU):

1. **M3 exit criteria (§9) pass:** AI-masked local adjustments render interactively (§11 budgets); masks serialize to recipe + XMP and round-trip; an injected inferd crash mid-session is isolated, recovered, and never disturbs the editor.
2. **§4.5 is mechanically enforced:** the replay-determinism test (T06) and the no-auto-recompute test (T19) are PR-blocking and green; recompute is a history-visible edit with working undo.
3. Select Subject / Sky / Background / Objects work end-to-end on all three platforms, on GPU and CPU EPs, with every degradation row of §5.6 exercised by T40.
4. **M4 library-intelligence items:** semantic search, similar images, auto-tag→FTS, and face clustering with naming/merge/hide run as pausable Background analyzers, resumable across restart and `kill -9`, with zero interactive-canvas starvation measured.
5. All §11 perf budgets met on baseline hardware in the nightly harness, with baselines recorded.
6. All seven model packs (plus the tag vocabulary) have hash-pinned manifest entries passing license surfaces 1 and 3; SegFormer's license question (R2) is resolved with CTO sign-off on record; InsightFace appears only on the denylist.
7. Catalog migrations upgrade a populated pre-E14 catalog copy-on-write; fault-injection (`kill -9`) on every new write path leaves `integrity_check` clean.
8. Golden gates registered: AiSeg weight-buffer goldens per process version, CPU/GPU parity, and per-EP IoU floors wired into §8's harness.
9. Seam handoffs delivered: `rebake` consumed by E12 copy/sync tests, `lb:` XMP schema + `SegRecipe` doc delivered to E16, `ai_text`/rule-atom integration signed off by the E07 owner, person-name metadata visible to E15's filtering.
10. No file outside E14's declared crate surface is owned or forked; all cross-epic changes (0014_04 FTS rebuild, E04 import hook) are reviewed by their owning epic's owner.
