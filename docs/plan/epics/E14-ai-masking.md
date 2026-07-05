# E14 — AI masking

_Implementation spec, **v2.0**. Author: staff engineer (E14 planner). Inputs: `docs/plan/00-mandate.md` (v2.1), `docs/plan/01-architecture.md` (v2.0, decision-complete; §1.5, §1.6, §2.2, §3.1/§3.1.1, §3.3, §4.5, §5.2/§5.3, §6, §7, §8, §10), `docs/research/04-*.md`, `docs/research/08-*.md`, the E12 and E13 epic specs (frozen seams), and the built E01 code (`crates/`)._

> **Supersedes `E14-ai-masking-and-search.md` (v1.x) entirely.** The v1.x epic was half masking, half library intelligence. The v2.0 mandate cuts the DAM, and with it **CLIP semantic search, similar-images, zero-shot auto-tag, faces/people detection+clustering, the AnalyzerService, and every FTS/smart-collection/filter-bar integration** — all retired (see non-goals). The E07 dependency is dropped. What survives is the AI-masking half — Select Subject / Sky / Background / Objects with the §4.5 baked-raster policy — re-spec'd here as a standalone epic and re-baselined to **~4–6 pw**.

| | |
|---|---|
| **Epic id** | E14 (`ai-masking`) |
| **Milestone** | M3 |
| **Effort** | **M–L, ~4–6 pw** (task breakdown sums to 26 tasks ≈ 26 dev-days ≈ 5.2 pw incl. integration slack) |
| **Depends on** | **E12** (mask engine: `AiSeg` component slot, replay evaluator, `mask`/`mask_component` tables, `MaskFrame`/`RasterFrame`, stale semantics, panel/gizmo framework), **E13** (`lightbox-inferd`, `InferenceClient`, model-pack manager, VRAM gate, supervision, provenance) |
| **Soft seams** | E03 (`BlobStore` `masks/` namespace), E09 (history steps, XMP mapping layer, preset payloads), E08 (shell chrome/keymap), E05 (render engine for the bake-input rendition), E15 (export replays baked rasters), E16 (consumes the `SegRecipe`/`lb:` handoff) |
| **Depended on by** | E16 (XMP `crs:` read interop for AI-masked sidecars, packaging of model packs, LaMa — none of which E14 builds) |
| **Architecture refs** | §1.5/§1.6 (model rows, EP posture), §2.2 (`lightbox-mask`, `lightbox-ml`), §3.1 (`mask_component` ownership note), §3.3 (`masks/` store), **§4.5 (baked-raster policy — Risk 7, the epic's governing invariant)**, §5.2/§5.3 (job classes, IPC boundary), §6 (inferd crash / VRAM / model-download rows), §8 (test gates), §9 (M3 exit), §10 (epic table, E14 row) |

---

## 1. Summary

E14 puts the four AI mask sources — **Select Subject** (BiRefNet), **Select Sky** (SegFormer/ADE20K), **Select Background** (complement), **Select Objects** (MobileSAM default / SAM2-small quality tier, point+box prompts) — into E12's unified mask model, on top of E13's inference platform, with the **§4.5 baked-raster reproducibility policy implemented end-to-end**: inference runs once, the resulting weight raster is baked content-addressed into the edit store, and every subsequent render — preview, export, this year or in 2031 — **replays the baked raster and never re-runs the model**. Recompute is an explicit, history-visible user act.

The division of labor is sharper than in v1.x, because E12 and E13 have since spec'd their halves:

- **E12 owns** the `AiSeg` component kind, its **replay evaluator** (raster load → frame check → warp-to-frame, task B12 of E12), the `stale` flag semantics, the mask tables, and the stale badge + "Update mask" affordance (E12 D12).
- **E13 owns** inferd, the IPC/typed-task surface (`segment`, `run_model`), pack download/verify/install, VRAM admission, crash supervision, and `Provenance`.
- **E14 owns** everything between them: model curation (packs, io-specs, licenses), the **bake pipeline** (rendition → inference → view→source mapping → content-addressed raster → single edit-store transaction), SAM's two-stage interactive session, prompt UX, explicit recompute + the rebake primitive, AI-raster post-refinement (threshold/feather/expand), raster lifecycle (pinning + GC), the `lb:` raster embedding in XMP, and the quality/determinism gates.

**First failing test of the epic (staff-engineer bar, §12):** *AiSeg replay determinism* — create a Select Subject component on a fixture raw, **kill `lightbox-inferd`**, reopen the edit store, render the image: the mask weight buffer is **byte-identical** to the post-bake render and **zero IPC calls** reach inferd. Written red before any model integration (T03); green at T08. This test mechanically encodes Risk 7's resolution.

**M3 exit criterion this epic co-owns (§9):** "AI-masked local adjustments render interactively; masks serialize to recipe + XMP; inference crash is isolated and recovered." E14 delivers the AI leg; E12 the mask/serialize leg; E13 the crash-isolation leg; T24 proves them together.

---

## 2. Scope

### 2.1 In scope

1. **Model packs** (E13 manifest format, hash-pinned, license-manifested): `seg-subject-birefnet`, `seg-sky-segformer` (with a named U2-Net fallback if the license gate fails), `seg-object-mobilesam` (default tier), `seg-object-sam2-small` (quality tier). Pack authoring includes the io-specs that drive E13's generic segmentation task adapter and the VRAM/`cpu_viable`/`ep_denylist` fields that drive E13's gate.
2. **The bake pipeline**: render the image's *current* recipe at ≤1536 px sRGB8 via the engine (view space — "select the subject I see" respects crop/geometry) → E13 inference → inverse-map the alpha into E12's **normalized oriented-source space** (out-of-crop = 0) → content-addressed gray8 PNG in the E03 `masks/` BlobStore namespace → **one edit-store transaction** writing `mask_component.{ai_recipe, cached_raster_ref, raster_frame, stale=0}` through E12's DAO + `edit_index.has_ai_mask` + an E09 history step.
3. **One-click Select Subject / Sky / Background** as core commands with progress, cancellation, and the full degradation ladder (pack missing → CTA; VRAM-gated → run-on-CPU offer; inferd down → disabled; crash → E13 retry policy, editor untouched).
4. **Select Objects interactive session**: client-side SAM orchestration over E13's `run_model` (encoder output cached per rendition; per-prompt decoder ≤150 ms), on-canvas prompt gizmo (+/− points, box), live overlay, accept-bakes-exactly-the-previewed-alpha, cancel.
5. **Explicit recompute** (`RecomputeAiComponent`): the *only* path that re-runs inference on an existing component; new raster, old ref retained for undo/history; consumes E12's D12 recompute-request event and E12 F3's paste-arrives-stale flow. Plus the **`rebake(target, recipe)` primitive** for E12 copy/sync and E09 adaptive-preset application.
6. **AI-raster post-refinement**: threshold/soft, feather, expand/contract applied at evaluation time over the baked raw alpha (live-editable without re-inference) — a small additive extension to E12's `AiSeg` component params + a post kernel in `lightbox-mask`, landed under E12-owner review (E13 explicitly assigns mask post-processing to E14).
7. **Raster lifecycle**: `masks/` is authoritative edit state, never LRU-evicted (E03 blob-namespace contract); conservative GC of orphans (live set = component refs ∪ snapshots ∪ history).
8. **XMP**: `lb:` raster embedding (base64 gray8 PNG, default on, pref to strip) layered on E12's mask `lb:` mapping through E09's layer, so a sidecar alone replays the mask identically on another machine.
9. **Gates**: replay byte-identity + no-auto-recompute tests (PR-blocking), AiSeg-in-pipeline goldens per process version, per-EP IoU sanity floors + perf budgets (nightly).

### 2.2 Explicit non-goals

| Not in E14 | Disposition / owner |
|---|---|
| **CLIP semantic search, similar images, text/image embedding, sqlite-vec ANN** | **Retired with the DAM (v2.0).** `embedding` table stays keep-dormant (§3.1.1); the sqlite-vec extension is not loaded; E13's `embed_image`/`embed_text` remain unused headroom |
| **Faces / people detection, clustering, People UI** | **Retired.** `face`/`person`/`face_person` keep-dormant; YuNet/SFace packs not authored; OpenCV stays an E12-only consumer (`seamlessClone`) |
| **Zero-shot auto-tag, tag vocabulary asset, FTS `ai_text`, filter-bar/smart-collection rule atoms** | **Retired.** `assets_fts` keep-dormant; no FTS migration; no E07 seam (E07 itself is retired) |
| **AnalyzerService / background library indexing** | **Retired.** Nothing in E14 scans the working set; every inference is user-initiated on the active image |
| Mask engine core: weight buffers, brush/linear/radial/range evaluators, boolean compositing, per-mask sub-recipes, **the AiSeg replay evaluator itself**, stale-badge UI, mask panel/gizmo framework | **E12** (built; E14 plugs into it) |
| inferd process, IPC, supervision, EP selection, pack download/verify/install mechanics, VRAM gate mechanism, tiling framework | **E13** (built; E14 registers packs and consumes the client) |
| Content-aware Remove (LaMa), heal/clone/PatchMatch | E12.4 (classical) / **E16** (LaMa backend swap) |
| LR `.xmp` **import** of Adobe AI masks (`crs:MaskGroupBasedCorrections`) | **E16** (read-interop; consumes this epic's `rebake` + `SegRecipe` handoff) |
| Select People body-part parsing, Select Landscape multi-class, depth masks, Lens Blur | v1.x design headroom only (feature catalog §2.6; architecture deferred list) |
| Generative remove, super-resolution, neural raw denoise, AI Looks (mandate v2.1 — a develop/looks concern, not a masking one; if it ever needs models it rides E13) | Other epics / v1.x |
| Auto Mask (edge-aware brush snapping), ViTMatte-class edge refinement | v1.x; the post-refinement kernel (item 6) keeps a slot for an edge-aware refine pass without a model change |

---

## 3. Crates & modules touched (against the real code)

Reality check (read from `crates/` at spec time): `lightbox-mask` and `lightbox-ml` are **E01 reserved stubs with zero logic** — E12 and E13 are their founding epics and E14 lands *after* both, so "the real code" for this epic is E01's built surfaces plus E12/E13's frozen interfaces. Schema v1 (`lightbox-catalog/migrations/0001_spine.sql`) contains **no mask tables** — E12's migration creates them; `edit_index.has_ai_mask` is reserved by E09's migration ("E14 populates"). `lightbox-jobs` (`Class`, `CancelToken`, `spawn`) is built and frozen (E01 Phase 4). Content hashes are xxh3-128 via `twox-hash`, canonical big-endian hex (E01 deviation log) — raster content addresses follow the same canonicalization.

| Crate | E14 additions | Notes vs reality |
|---|---|---|
| **`lightbox-ml`** | new module **`masking`**: `SegRecipe`/`MaskPrompt` types + CBOR schema, `MaskBaker` (bake/rebake orchestration), `ObjectSession` (SAM encoder-cache + decoder loop), model io-spec/pack curation data, sky/subject post-config | Extends the crate E13 founds; E14 code stays in this module — E13's `service`/`tasks`/`packs`/`gate` modules are consumed, never modified. If a needed hook is missing we file against E13, not fork |
| **`lightbox-inferd`** | **nothing** | E13's daemon is codec-free and generic-tensor-only (E13 decision 1); all E14 model-specific logic is client-side |
| **`lightbox-mask`** | `AiSegPost` params + post-refinement kernel (WGSL + CPU) invoked by E12's AiSeg evaluator after warp; additive field on `AiSegComponent` | **Cross-epic touch, E12-owner-reviewed** (T15). E13's non-goals table assigns "mask post-processing (feather/threshold/expand)" to E14; E12's evaluator replays raw alpha — this is the one seam where E14 code lands in E12's crate |
| **`lightbox-core`** | commands `AddAiComponent`, `RecomputeAiComponent`; query `ai_mask_availability`; handler for E12's D12 recompute-request event; bake-input rendition provider over the session engine | Command bus / façade per E01's built `Command`/`Event`/`Queries` machinery |
| **`lightbox-shell`** | masking-panel AI buttons (Subject/Sky/Background) + progress/error/CTA states; Select Objects prompt gizmo + live overlay on E12's gizmo layer; run-on-CPU prompt rendering (E13 `GatePrompt`) | |
| **`lightbox-edit`** | `lb:SegRaster*` embedding extension on E12's mask `lb:` mapping, through E09's `xmp_map` layer; read-back materialization | Coordinated with E12 (owns `masks_to_lb_xmp`) and E09 (owns the layer) |
| **`lightbox-catalog`** | **no migration, no DDL** | See §7 — E14 writes existing E12/E09 columns through E12's DAO only |
| **`lightbox-preview`** | **nothing new** | E03's `BlobStore::namespace("masks")` is the reserved mechanism (E03 T02/S8); E14 owns content + lifecycle policy, not the store |
| **`lightbox-jobs`** | job-class usage only: one-click bake = **Foreground**, interactive decode = **Foreground** (E13 `Priority::Interactive` inside inferd), GC sweep = **Background** | No code changes |
| **`lightbox-cli`** | `mask ai` subcommands (bake/recompute/gc/status) for headless E2E per the headless-core principle | |

---

## 4. Model roster (packs, licenses, VRAM)

All weights ship as E13 model packs (`pack.toml`, per-file sha256, `model_pack` rows, §8 surface-1/-3 manifests). **Apache/MIT/BSD only; InsightFace-derived weights remain on the denylist (§1.5) even though faces are cut — the denylist entry stays as a guard.**

| Pack id (v1 pin) | Model | Role | License | Input | Output | `vram_peak_mb` (indicative) | `cpu_viable` |
|---|---|---|---|---|---|---|---|
| `seg-subject-birefnet` | BiRefNet general, fp16 ONNX | Select Subject | MIT | 1024² RGB | alpha 1024² | ~2000 | yes |
| `seg-sky-segformer` | SegFormer-B0 ADE20K | Select Sky | **pin-time license gate** — use an Apache-licensed weight lineage (HF transformers export path); NVIDIA research-license weights are inadmissible. CTO sign-off recorded at T02 | 512² | class logits → sky alpha | ~1000 | yes |
| `seg-object-mobilesam` | MobileSAM encoder+decoder ONNX | Select Objects (default) | Apache-2.0 | 1024² | low-res logits → alpha | ~1000 | yes |
| `seg-object-sam2-small` | SAM2.1-hiera-small, image-only export | Select Objects (quality, gate-admitted) | Apache-2.0 | 1024² | logits → alpha | ~3000 | yes |

Notes:
- **Named fallback (pre-approved by §1.6):** if no cleanly-licensed sky-capable SegFormer weight clears review at T02, the sky pack swaps to a **U2-Net sky variant** behind the identical `segment(…, SemanticClass)`-or-`Auto` adapter surface — a pack swap, not a design change.
- The ADE20K **sky class id is pinned in the pack manifest** (`[model.meta] sky_class = N`), not hardcoded in Rust, so a model refresh with a different palette is data-only.
- All four are `cpu_viable = true`: one-click ops on CPU EP run seconds-scale with progress and cancellation (E13's honest-floor budget); that is acceptable for explicit user actions and there are **no background analyzers left** to care about throughput.
- `ep_denylist` is E14's curation duty per model (E13 Risk R2: CoreML dynamic-shape hostility — the SAM decoder is the likely candidate; measured at T18/T19, encoded in the manifest).
- Every result's `Provenance` (E13) is persisted in `SegRecipe` — §4.5's provenance requirement costs E14 nothing beyond storage.

---

## 5. Design

### 5.1 Coordinate spaces & the bake data flow

Two spaces matter, both defined by neighbors:

- **View space** — the rendition the user sees: the image's current recipe rendered post optics/geometry/crop at preview resolution. All prompts (button, points, box) are in normalized view coordinates. Inference always runs on a view-space rendition.
- **Normalized oriented-source space** — E12's component space (`[0,1]²` over the full decoded, orientation-applied source, independent of crop/rotate/upright/warp; E12 §4.1). Baked rasters are stored here, stamped with E12's `RasterFrame { src_dims, geom_hash }`, and E12's replay evaluator warps them forward through `MaskFrame` at render time.

```
user action (panel button / prompt gizmo, view space)
  → bake-input rendition: Engine::submit(current recipe, ≤1536 px, sRGB8, RenderTarget::CpuBuffer)
        [Foreground job, cancellable]                                  → BakeInput{ pixels, frame, rendition_hash }
  → E13: segment(...) or ObjectSession decode                          → view-space alpha8 + Provenance
  → inverse map view → oriented-source (MaskFrame.local_to_src at bake scale; out-of-crop = 0)
  → BlobStore("masks").put(gray8 PNG)                                  → RasterRef  [atomic, idempotent — E03]
  → edit-store txn (E12 DAO): mask_component{ kind=ai_seg, ai_recipe=CBOR(SegRecipe),
        cached_raster_ref, raster_frame, stale=0 } + edit_index.has_ai_mask + history step (E09)
```

**The baked artifact is the raw model alpha** (pre-threshold, 8-bit, oriented-source space). Post-refinement (threshold/soft, feather, expand, plus E12's component-level invert) lives in ordinary component params and is applied **at evaluation time** — deterministic, cheap, re-editable without re-inference. This preserves §4.5 (replay is bit-deterministic) while keeping mask refinement a normal slider edit.

Because inference sees only the cropped view, pixels outside the bake-time crop bake to 0. If the user later widens the crop, the raster's `RasterFrame` no longer matches → E12's evaluator **badges stale and keeps rendering the last raster warped** (E12 B12); recovery is the explicit recompute. This is designed, documented behavior, not a bug.

### 5.2 Staleness & recompute (§4.5 compliance, reconciled with E12's built semantics)

- **Inference never runs implicitly.** The only inference triggers in the product are: `AddAiComponent`, `RecomputeAiComponent`, `ObjectSession` decodes during the interactive loop, and `rebake` invoked by an explicit user act (paste/sync "update", preset apply). A mechanical CI guard (T14) asserts that geometry edits and renders on AI-masked images produce **zero IPC frames** to inferd.
- **The `stale` flag** follows E12's built semantics: set by explicit user action (`MarkAiStale`, paste via E12 F3) **or by a `RasterFrame` mismatch detected at evaluation** (E12 B12). A stale flag is only ever a *badge + affordance*; it never triggers inference. This is the one refinement over the architecture's §4.5 wording ("set only by explicit user recompute") that E12 already committed to — the governing invariant, *no silent recomputation*, is identical, and E14 conforms to the dependency's letter.
- **Recompute produces a new raster and is an edit**: new `RasterRef` + new `SegRecipe` (fresh provenance, `baked_at`, frame), one history step; **the old raster ref is retained** by the history step and by any snapshot that captured it, so undo restores the previous `cached_raster_ref` byte-for-byte and GC (T17) cannot collect it while referenced.

### 5.3 Select Objects: the interactive session (client-side SAM orchestration)

E13's protocol is generic tensors; inferd holds no model-family logic. SAM's two-stage split therefore lives **client-side** in `lightbox_ml::masking`:

- **Encode once per rendition**: `run_model(encoder, [image tensor])` → embedding tensors (~4 MB), cached in the client keyed by `rendition_hash` (LRU cap 3, TTL 5 min). E13's IPC budget (4 MB round-trip ≤ 10 ms) makes re-sending embeddings per decode negligible.
- **Decode per prompt**: `run_model(decoder, [embeddings, point/box tensors, mask hint])` at `Priority::Interactive` → low-res logits → upsampled view-space alpha for the live overlay. Budget ≤ 150 ms p95 GPU EP.
- **Accept bakes exactly the previewed alpha** — no re-inference at accept time (predictability beats a marginal resolution win; recorded as a decision, revisit is OQ4). Cancel drops the session; encoder-cache eviction mid-loop transparently re-encodes (one progress-visible hiccup).
- inferd crash mid-session: E13 restarts the host; the session re-encodes on next decode (embeddings are client-cached but ORT sessions are gone — the decode request itself transparently reloads via `ModelSel`).

### 5.4 Select Background

Baked as its **own independent raster** = `255 − subject_alpha`: derived with **zero inference** when a fresh (non-stale, same-frame) subject component exists on the image; otherwise one subject inference then inverted at bake. `SegRecipe.prompt` records the derivation (`Background { complement_of }`) as provenance only — the raster is self-sufficient afterward, so deleting the source subject component later changes nothing.

### 5.5 Degradation ladder (ties to §6 failure taxonomy; all rows exercised by T24)

| Condition | Behavior |
|---|---|
| runtime-ort / model pack not installed | AI buttons disabled with install CTA (E13 `RuntimeMissing`/`ModelNotInstalled` → E08-rendered); everything already baked keeps rendering |
| VRAM below pack gate | E13 `GateVerdict::AskUser{RunCpu}` → "slower, run anyway?" for one-click ops; Objects quality tier falls back to MobileSAM |
| inferd crash mid-bake / mid-decode | E13 policy: restart + 1 retry same EP → next EP → graceful job failure with activity-center error; **editor session untouched**; baked masks unaffected (replay does no IPC) |
| inferd disabled / crash-loop breaker tripped | All AI entry points disabled with status notice + re-enable CTA; replay unaffected |
| `masks/` raster file missing (user deleted, store damage) | E12 B12: component skipped + stale badge + rebake CTA — **never a render failure** |
| Disk full during raster put | E03 atomic-write contract: no torn file; bake job fails typed, catalog untouched |
| Original file offline | Bake runs from the preview-backed rendition (E03 tiers); no behavior change |

---

## 6. Interface definitions

Contracts, not implementations (§2.2 convention). E14 types live in `lightbox_ml::masking` unless noted. E13 types (`ImageInput`, `ModelSel`, `ModelRef`, `SegPrompt`, `SegResult`, `MaskRaster`, `RunOpts`, `Provenance`, `InferenceClient`) and E12 types (`MaskId`, `ComponentId`, `BoolOp`, `AiSegComponent`, `RasterRef`, `RasterFrame`, `MaskFrame`) are consumed exactly as their specs define them; any divergence found at integration is filed against the owner, not forked.

### 6.1 Persisted recipe & prompt types (E14-owned schema; stored opaque in `mask_component.ai_recipe`)

```rust
/// User-level prompt, persisted. Distinct from E13's wire-level `SegPrompt`
/// (Auto/Points/Box/SemanticClass), which it lowers onto per model.
#[derive(Serialize, Deserialize)]                    // CBOR; unknown-field-preserving
pub enum MaskPrompt {
    Subject,
    Sky,
    Background { complement_of: Option<ComponentId> },   // provenance of the derivation (§5.4)
    Object { points: Vec<ObjPoint>, bbox: Option<RectNorm> },  // normalized view space
}
pub struct ObjPoint { pub x: f32, pub y: f32, pub positive: bool }

#[derive(Serialize, Deserialize)]
pub struct SegRecipe {                               // provenance + replay identity; NEVER auto re-run (§4.5)
    pub schema: u16,                                 // = 1
    pub prompt: MaskPrompt,
    pub model: ModelRef,                             // E13 pinned identity (pack, model, version, file sha256)
    pub input: BakeInputStamp,                       // what inference saw
    pub provenance: Provenance,                      // E13: ep, ep_partition, ort_version, inferd_build, timing
    pub baked_at: i64,                               // unixepoch
}
pub struct BakeInputStamp {
    pub rendition_hash: ContentHash,                 // xxh3-128 of the rendered bake input (E01 canonicalization)
    pub long_edge_px: u32,                           // ≤ 1536
    pub frame: RasterFrame,                          // E12 frame stamp: src_dims + geom_hash at bake time
    pub recipe_fingerprint: u64,                     // hash of the recipe rendered into the bake input
}
```

### 6.2 Bake orchestration

```rust
pub struct MaskBaker { /* rendition provider + Arc<dyn InferenceClient> + BlobStore("masks")
                          + core command sink (E12 DAO writes ride the command bus) */ }
impl MaskBaker {
    /// Full §5.1 pipeline. Foreground job; cancellable at every stage (render, infer, put, txn).
    pub async fn bake(&self, image: ImageId, prompt: MaskPrompt, tier: ObjectTier,
                      cancel: CancelToken) -> Result<BakedSeg, BakeError>;
    /// Re-run an existing SegRecipe against a (possibly different) image: explicit recompute,
    /// paste/sync update, adaptive-preset apply. Always fresh provenance.
    pub async fn rebake(&self, target: ImageId, recipe: &SegRecipe,
                        cancel: CancelToken) -> Result<BakedSeg, BakeError>;
}
pub struct BakedSeg { pub raster: RasterRef, pub frame: RasterFrame, pub recipe: SegRecipe }
pub enum ObjectTier { Auto /* gate-admitted best */, MobileSam, Sam2Small }

pub enum BakeError {
    Unavailable(InferError),            // pack/runtime missing, gated-deny, host down — CTA payloads inside
    GatePrompt(GateDecision),           // AskUser surfaced to the shell (E13 type)
    RenderFailed(RenderError),          // bake-input rendition failed
    Cancelled,
    Store(BlobStoreError),              // put failed (disk full etc.)
    Inference(InferError),
}

/// Bake-input rendition provider (lightbox-core, over the session engine — E05 seam).
pub trait RenditionProvider: Send + Sync {
    /// Renders the image's CURRENT recipe, view target, sRGB8, long edge ≤ max_px,
    /// via Engine::submit with RenderTarget::CpuBuffer. Returns pixels + frame stamp + hash.
    fn render_bake_input(&self, image: ImageId, max_px: u32, cancel: &CancelToken)
        -> JobFuture<BakeInput>;
}
pub struct BakeInput { pub image: ImageInput /* E13 type */, pub stamp: BakeInputStamp,
                       pub view_to_src: Mat3x3 /* from E12's MaskFrame at bake scale */ }
```

### 6.3 Select Objects session

```rust
pub struct ObjectSession { /* BakeInput + cached encoder output tensors + client + tier */ }
impl ObjectSession {
    /// Renders the bake input (or reuses a fresh one) and runs the encoder once.
    pub async fn open(baker: &MaskBaker, image: ImageId, tier: ObjectTier,
                      cancel: CancelToken) -> Result<ObjectSession, BakeError>;
    /// Per-prompt decode → view-space alpha for the live overlay. Interactive priority.
    pub async fn decode(&mut self, points: &[ObjPoint], bbox: Option<RectNorm>,
                        cancel: CancelToken) -> Result<ViewAlpha, BakeError>;
    /// Bakes EXACTLY the last previewed alpha (no re-inference — §5.3) into a component.
    pub async fn accept(self, target: AddTarget) -> Result<BakedSeg, BakeError>;
    pub fn cancel(self);
}
pub struct ViewAlpha { pub w: u32, pub h: u32, pub alpha: Vec<u8> }   // view space, overlay + accept source
pub enum AddTarget { NewMask { image: ImageId, name: Option<String> },
                     Component { mask: MaskId, op: BoolOp } }
```

### 6.4 Post-refinement (lands in `lightbox-mask`, E12-owner-reviewed — T15)

```rust
/// Additive field on E12's AiSegComponent (`params` CBOR grows a `post` map; absent = identity —
/// pre-E14 components deserialize unchanged, forward-compat by E12's unknown-field tolerance).
#[derive(Serialize, Deserialize, Default)]
pub struct AiSegPost {
    pub mode: PostMode,                 // Soft (raw alpha, default) | Threshold { t: f32, softness: f32 }
    pub feather_px: f32,                // separable gaussian on the warped weight tile
    pub expand_px: f32,                 // signed morphological dilate(+)/erode(−)
}
/// Invoked by E12's AiSeg evaluator after warp-to-frame, before compositing.
/// WGSL + CPU (rayon) with §4.4-tolerance parity; identity when `post` is default.
pub fn apply_ai_post(tile: &mut WeightTile, post: &AiSegPost, ctx: &MaskEvalCtx);
```

Component-level **invert** already exists on E12's `MaskComponent` — not duplicated here.

### 6.5 Core commands, queries, events (headless boundary — no UI types)

```rust
pub enum AiMaskCommand {
    /// One-click Subject/Sky/Background or accepted Objects prompt. Spawns the Foreground
    /// bake job; commits via E12's DAO + E09 history in ONE writer txn on success.
    AddAiComponent   { target: AddTarget, prompt: MaskPrompt, tier: ObjectTier },
    /// THE ONLY inference path for an existing component (§4.5/§5.2). New raster; old ref
    /// retained by history; undo restores previous cached_raster_ref byte-for-byte.
    RecomputeAiComponent { image: ImageId, comp: ComponentId },
    /// Post-refinement edits route through E12's MaskCommand::UpdateComponent (patch carries
    /// AiSegPost) — no E14 command needed; listed for completeness.
    RunMaskGc,                                          // Background sweep; also on idle timer
}

impl Queries {
    /// Drives button/CTA states: per-feature availability from E13's MlStatus + PackManager.
    pub fn ai_mask_availability(&self) -> AiMaskAvailability;
}
pub struct AiMaskAvailability {
    pub subject: FeatureState, pub sky: FeatureState, pub objects: FeatureState,
    pub object_tier_active: ObjectTier,
}
pub enum FeatureState { Ready, PackMissing { pack: PackId }, RuntimeMissing,
                        Gated { decision: GateDecision }, HostDisabled { reason: String } }

// Consumed events: E12's D12 recompute-request (→ dispatch RecomputeAiComponent),
// E13's MlEvent::{GatePrompt, PackProgress, HostStateChanged} (→ shell states).
// Emitted: standard job progress on the E06/E01 activity model; no new event kinds.
```

### 6.6 Raster lifecycle (over E03's BlobStore — mechanism theirs, policy ours)

```rust
/// masks/ namespace policy (documented invariant, enforced by T07/T17 tests):
/// - content-addressed gray8 PNG, immutable, idempotent put (E03 T02 semantics)
/// - AUTHORITATIVE EDIT STATE: exempt from all LRU/eviction (E03 blob-namespace contract, E03 S8)
/// - deleted ONLY by gc() with a catalog-derived live set
pub struct MaskRasterGc { /* reader pool + BlobStore("masks") */ }
impl MaskRasterGc {
    /// live = cached_raster_ref ∪ raster refs inside snapshot recipe docs (E09 materialized
    /// snapshots) ∪ raster refs inside history_step params (undo targets). Conservative:
    /// unparseable docs pin everything they might reference.
    pub fn sweep(&self, cancel: &CancelToken) -> Result<GcReport, GcError>;
}
```

---

## 7. Edit-store / data-model changes

**E14 ships zero migrations and zero DDL.** Everything it persists already has a home created by its dependencies; this is a deliberate consequence of the v2.0 rescope (the four v1.x E14 migrations — embeddings, faces, analysis, FTS — are all retired with the DAM, and the corresponding §3.1 tables stay keep-dormant per §3.1.1).

Write-surface inventory (all writes go through E12's DAO on the single-writer command path — E14 never issues raw SQL):

| Existing column (owner) | E14's use |
|---|---|
| `mask_component.ai_recipe BLOB` (E12 DDL) | CBOR `SegRecipe` — opaque to E12, schema'd in §6.1 |
| `mask_component.cached_raster_ref TEXT` (E12) | content address into the `masks/` BlobStore namespace |
| `mask_component.raster_frame BLOB` (E12) | CBOR `RasterFrame` stamped at bake time |
| `mask_component.stale INTEGER` (E12) | written `0` at bake/recompute; set per §5.2 semantics (E12 owns the setters) |
| `mask_component.params BLOB` (E12) | grows the additive `post: AiSegPost` map (T15, E12-reviewed; absent = identity) |
| `edit_index.has_ai_mask` (reserved by E09: "E14 populates") | derived in the same writer txn — mechanically, by E12's DAO facet rebuild, which E14's writes flow through |
| `model_pack` + `model_file` (E13) | rows created by E13's PackManager when E14's packs install; E14 authors manifests only |
| `history_step` / `snapshot` (E09) | ordinary history steps for bake/recompute/post edits; snapshots capture raster refs by value (E09 `SnapshotMaterializer` via E12's materializer) |

On-disk (non-SQL) surface: `masks/<hh>/<hash>.png` inside `.lbdata` (E03 layout, reserved namespace) — gray8 PNG, typically 60–300 KB at 1024²-class resolutions; pinned (never evicted), GC'd per §6.6. Prefs keys registered (E01 prefs store): `xmp.embed_ai_rasters` (bool, default **true**), `ml.object_tier` (`auto|mobilesam|sam2`, default `auto`).

---

## 8. XMP mapping (through E09's layer, on E12's mask mapping)

E12's F1 already round-trips the full mask model to `lb:` — including `AiSegComponent` with `SegRecipe` as opaque CBOR and the raster *reference*. E14 adds the **raster payload** so a sidecar alone is replayable on a machine that has never run inference (mandate constraint 2: edits are exportable as sidecars; per §4.5 the raster *is* the edit):

| Field (`lb:` namespace) | Content |
|---|---|
| `lb:SegRecipe` | CBOR→base64 of `SegRecipe` (already emitted by E12's mapping as the opaque blob; E14 documents the schema for E16) |
| `lb:SegRasterW` / `lb:SegRasterH` / `lb:SegRaster` | baked raw-alpha raster, gray8 PNG → base64 — **included by default** (`xmp.embed_ai_rasters=true`); the pref strips it to provenance-only for size-sensitive sidecar workflows (documented trade-off: foreign-machine replay then requires an explicit rebake) |
| `lb:SegPost*` | `AiSegPost` fields (ride the component `params`, mapped by E12; named here because E16's reader must know them) |

Read-back: if `lb:SegRaster` is present and the referenced content-hash is absent from the local `masks/` store, the blob is **materialized into the store on sidecar read** (hash-verified against `cached_raster_ref`; mismatch → stale badge + rebake CTA, never silent adoption). Round-trip contract (property-tested in E09's harness): write → read restores a replayable component whose raster bytes and `SegRecipe` CBOR are identical. No `crs:` mapping exists or is imitated for AI masks (constraint 4); E16 owns any LR-mask read interop, consuming `rebake`.

Auto-tags/person data no longer exist (retired), so this section is the epic's entire XMP surface.

---

## 9. Seams to neighboring epics (named, not designed)

1. **E12 → E14 (mask engine):** E12 provides `Component::AiSeg` + tables, the replay evaluator (B12: load → frame check → warp), `MaskFrame`/`RasterFrame`, stale semantics + `MarkAiStale`, the D12 badge/recompute-request event, the panel/gizmo framework, and treats `SegRecipe` as opaque CBOR. **E14 → E12:** fills `ai_recipe`/`cached_raster_ref`/`raster_frame`, handles D12's event, supplies `rebake` for F3 copy/sync ("update AI masks on paste" is user-initiated per image), and lands the T15 post-kernel in `lightbox-mask` under E12 review.
2. **E13 → E14 (inference platform):** E13 owns transport, supervision, retries, packs, gating, provenance, and the generic `segment`/`run_model` ops. E14 registers pack manifests (io-specs drive E13's E1 adapter), curates `ep_denylist`/VRAM fields, and builds SAM orchestration client-side over `run_model` — **no protocol or daemon change is ever requested** (E13 Risk R8's hard rule; if the frozen protocol truly cannot express something, that is an E13 version-bump negotiation, not an E14 workaround).
3. **E03 → E14:** `BlobStore::namespace("masks")` (E03 T02, seam S8) is the storage mechanism; blob namespaces are exempt from E03's LRU by contract. E14 owns content, pinning policy, and GC. **Whether `masks/` joins the exit-time verified backup is OQ1** (E01/E03 owners; `masks/` is authoritative edit state unlike everything else in `.lbdata`).
4. **E09 → E14:** history steps for bake/recompute (undo restores the old raster ref); snapshot self-containment via the materializer registry (snapshots capture raster refs by value); the `xmp_map` layer under §8; **adaptive presets** (E09's format) store `MaskPrompt` + `ModelRef` — never rasters — and call `rebake` on apply.
5. **E05 → E14:** the bake-input rendition is an ordinary `Engine::submit` with `RenderTarget::CpuBuffer` (the readback path E01 built for CLI/tests); Interactive/Foreground GPU time-slicing already arbitrates canvas vs bake.
6. **E08 → E14:** shell chrome (buttons, progress, CTA rendering incl. E13's `GatePrompt`), keymap entries; the gizmo framework hosts the Objects prompt tool.
7. **E15 → E14:** export renders replay baked rasters through E12's evaluator — zero inference at export time (asserted in T24); export metadata records the render backend per §4.4, nothing AI-specific.
8. **E14 → E16:** handoff = `SegRecipe` schema doc + `lb:SegRaster*` fields + `rebake` primitive (for LR AI-mask read interop), plus the four pack definitions for packaging/first-run UX.

---

## 10. Ordered task breakdown (26 tasks, each ≤ 1 day)

Dependencies flow top-to-bottom within a phase; Phase 3 (Objects) can overlap Phase 2 across two engineers after T08. **AC** = acceptance criteria.

### Phase 0 — contracts, packs, first failing test (5 tasks)

| # | Task | AC |
|---|---|---|
| **T01** | `lightbox_ml::masking` types: `SegRecipe`/`MaskPrompt`/`BakeInputStamp` + CBOR ser/de + schema doc | `proptest` round-trip identity; unknown-field preservation on a future-schema doc; opacity fixture: bytes stored through E12's `AiSegComponent.recipe` come back unchanged |
| **T02** | Author the 4 pack manifests (io-specs, vram fields, `cpu_viable`, mirrors, per-file sha256) + weights-manifest entries (surfaces 1/3) + **SegFormer license gate**: Apache-lineage weights verified or U2-Net fallback swapped in; CTO sign-off filed | E13's manifest validator (C1) passes all 4; `lightbox-cli ml install/verify` round-trips each pack headlessly; license gate green; sign-off recorded |
| **T03** | **First failing test:** replay-determinism harness (fixture catalog, mock adapter behind `InferenceClient`) — bake → kill inferd → reopen → render → assert byte-identical weight buffer + **zero IPC frames** | Test exists, runs in CI, and fails for the right reason (no bake txn yet); wired to go green at T08 |
| **T04** | Model configs driving E13's generic segment adapter: BiRefNet (`Auto`, sigmoid→alpha8) + SegFormer (`SemanticClass(sky)`, argmax→class alpha, class id from manifest meta) | Tensor-fixture unit tests for both pre/post paths; a synthetic logits fixture produces the expected alpha; sky class id read from the manifest, not code |
| **T05** | Bake-input `RenditionProvider` in `lightbox-core`: current recipe → `Engine::submit` (CpuBuffer, ≤1536 px, sRGB8) → `BakeInput` with `RasterFrame` stamp + xxh3 rendition hash | Identical recipe ⇒ identical `rendition_hash` (byte-stable, per E01's CPU determinism property); rendition matches the canvas render within golden tolerance; cancellable |

### Phase 1 — bake pipeline & one-click masks (7 tasks)

| # | Task | AC |
|---|---|---|
| **T06** | View→source inverse resample: view-space alpha → normalized oriented-source raster via `MaskFrame.local_to_src` (identity fallback pre-geometry); out-of-crop = 0 | Property test over mocked crop/rotate geometries: round-trip error ≤ 0.5 px at bake res; out-of-crop pixels exactly 0; degenerate transforms → typed error |
| **T07** | `masks/` write path over `BlobStore::namespace("masks")`: gray8 PNG encode, idempotent put, pinning policy doc + accounting assertion | `get(put(x)) == x`; put idempotent; store size excluded from E03's LRU caps (asserted against E03's accounting); crash-injection during put leaves no visible partial (rides E03's T02 guarantee, re-asserted here) |
| **T08** | Bake transaction: raster put → one writer txn via E12 DAO (`ai_recipe`, `cached_raster_ref`, `raster_frame`, `stale=0`) + `edit_index.has_ai_mask` facet + E09 history step | **T03 goes green**; `kill -9` mid-bake → `integrity_check` clean, no component row without raster (orphan raster is T17's job); history step present; undo removes the component and facet |
| **T09** | Select Subject end-to-end (BiRefNet through `MaskBaker::bake`) | IoU ≥ 0.90 vs reference masks on a 20-image fixture corpus, CPU EP (PR-blocking floor); result renders through E12's evaluator as a working local adjustment |
| **T10** | Select Sky end-to-end (SegFormer or fallback pack) | Sky IoU ≥ 0.85 on sky fixtures (CPU EP); no-sky fixtures < 2 % false-positive area; same-pack swap test proves the U2-Net fallback path compiles the same code |
| **T11** | Select Background: derive-or-infer complement (§5.4) | Zero inference when a fresh subject component exists (IPC-count probe); complement golden; raster independent — deleting the source subject changes nothing |
| **T12** | Core command `AddAiComponent` + `ai_mask_availability` query + `lightbox-cli mask ai add` | Headless CLI: open fixture → add subject mask → render → committed golden; every `FeatureState` variant reachable in tests (pack missing / runtime missing / gated / host disabled); UI thread never blocks (command is a job) |

### Phase 2 — recompute, refinement, lifecycle (5 tasks)

| # | Task | AC |
|---|---|---|
| **T13** | `RecomputeAiComponent`: explicit re-inference, new raster + recipe, old ref retained; consumes E12 D12's recompute-request event and the F3 paste-stale flow | Undo restores the previous `cached_raster_ref` byte-for-byte; recompute is a visible history step; D12's "Update mask" affordance drives it end-to-end; paste → stale badge → per-image update works on a 2-image fixture |
| **T14** | **No-auto-recompute mechanical guard** (PR-blocking): crop/geometry/slider mutations + renders on an AI-masked fixture image | Zero IPC frames to inferd across the whole scenario (transport-level counter); raster ref unchanged; frame-mismatch path badges stale and still renders the warped raster (E12 B12 integration) |
| **T15** | `AiSegPost` + post-refinement kernel in `lightbox-mask` (WGSL + CPU), invoked by E12's evaluator after warp; **E12-owner review recorded** | Default post = bit-identical to pre-T15 output (identity proof — existing E12 goldens unchanged); threshold/feather/expand goldens; CPU/GPU parity within §4.4 tolerance; post edits are ordinary coalesced component edits (1 history step per gesture) |
| **T16** | `MaskBaker::rebake` for copy/sync + adaptive presets (seams §9.1/§9.4) | Syncing a subject mask to a second fixture image produces a target-specific raster with fresh provenance; E12's F3 merge/replace tests consume it; a preset storing `MaskPrompt`+`ModelRef` applies via rebake |
| **T17** | Raster GC (`MaskRasterGc::sweep`): live set = component refs ∪ snapshot docs ∪ history steps; idle Background job + `lightbox-cli mask ai gc` | Property test over random bake/undo/snapshot/delete sequences: a referenced raster is never deleted; orphans from killed bakes are collected; report surfaced on the activity model |

### Phase 3 — Select Objects (4 tasks)

| # | Task | AC |
|---|---|---|
| **T18** | SAM encoder session: `run_model(encoder)` over E13, client-side embedding cache (key = rendition hash, LRU 3, TTL 5 min) | Encode-once/decode-N proven by IPC counts; session open ≤ 1.5 s p95 GPU on the fixture bench; bounded memory under churn; eviction mid-loop re-encodes transparently |
| **T19** | SAM decoder loop: prompt tensor encoding (points/box), `run_model(decoder)` at Interactive priority, logits → `ViewAlpha`; tier selection (MobileSAM default, SAM2-small when gate-admitted; `ml.object_tier` pref); measure + encode `ep_denylist` per platform | Prompt-fixture IoU ≥ 0.85 (CPU EP); decode p95 ≤ 150 ms GPU; cancel mid-decode ≤ 500 ms (E13 B3 contract); tier switch is pack-data only (no code branch on model name) |
| **T20** | Shell: Objects prompt gizmo on E12's gizmo layer — +/− points, box drag, live overlay, accept/cancel | Accept bakes exactly the previewed alpha (byte-compare `ViewAlpha` → baked raster modulo the T06 resample, golden-pinned); cancel leaves no component/raster; keymap registered; overlay never appears in export graphs (E12's view-node assertion reused) |
| **T21** | Object-session failure modes: inferd crash mid-session (restart → transparent re-encode on next decode), gate-denied quality tier (→ MobileSAM), cache-evicted mid-loop | Each path scripted against E13's failure-injection mocks; the editor session survives all; user sees progress/notice, never a stall |

### Phase 4 — shell states, XMP, gates (5 tasks)

| # | Task | AC |
|---|---|---|
| **T22** | Masking-panel AI buttons (Subject/Sky/Background + "Select Objects" tool entry) with progress/error/CTA states bound to `ai_mask_availability` + E13 events (`PackProgress`, `GatePrompt`, `HostStateChanged`) | Click → mask on canvas ≤ 2.5 s p95 (GPU fixture); every §5.5 ladder row reachable in a headless UI test; install-CTA drives E13's pack install and re-enables live |
| **T23** | XMP raster embedding: `lb:SegRaster*` emit (pref-gated) + read-back materialization into `masks/` (hash-verified) — coordinated with E12's `masks_to_lb_xmp` + E09's harness | Sidecar round-trip → byte-identical raster + `SegRecipe` CBOR (property test in E09's harness); `embed_ai_rasters=false` emits provenance-only and read-back badges stale with rebake CTA; hash-mismatch payload rejected |
| **T24** | Failure-mode integration suite on the CI matrix: crash mid-bake (retry policy), disk-full during put, missing raster render, VRAM-gate AskUser both answers, runtime/pack-missing CTAs, offline original, **export-does-zero-inference** assertion | All §5.5 rows exercised on macOS/Windows/Linux; editor session survives every case; export of an AI-masked image performs zero IPC |
| **T25** | Gates wiring: replay byte-identity (T03) + no-auto-recompute (T14) as PR-blocking; AiSeg-in-pipeline golden per process version (fixed baked raster × post params × local adjustment → ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, CPU+GPU); per-EP IoU sanity floors + §11 perf budgets nightly | Gates green and PR-blocking/nightly as designated; baselines committed; **inference outputs are deliberately NOT golden-tested cross-EP** (§4.5 non-determinism) — determinism is asserted on replay, quality on IoU floors |
| **T26** | Docs + handoff + final audit: `SegRecipe` schema + `lb:` field doc to E16; pack-curation notes (ep_denylist rationale per platform); weights/data manifests re-audited; DoD checklist walked | E16 handoff reviewed by its owner; license surfaces 1/3 green with all four packs; DoD items all checked or explicitly flagged |

**Total: 26 tasks ≈ 26 dev-days ≈ 5.2 pw** — inside the M–L ~4–6 pw band, parallelizable to ~2 engineers after T08 (Phase 3 ∥ Phase 2/4).

---

## 11. Performance budgets (asserted by T25's nightly harness; reference hw per §7; E13 platform budgets stack underneath)

| Operation | Budget (GPU EP) | CPU-EP contract |
|---|---|---|
| One-click Subject/Sky: click → mask on canvas (rendition + inference + bake + eval) | ≤ 2.5 s p95 | ≤ 30 s, progress + cancel |
| Select Objects: session open (rendition + encode) | ≤ 1.5 s p95 | ≤ 15 s, progress |
| Select Objects: per-prompt decode → overlay | ≤ 150 ms p95 | ≤ 2 s |
| AiSeg replay eval per component at fit-view (E12 B12 warp + T15 post) | ≤ 10 ms p95, **zero IPC** (post kernel's share ≤ 2 ms) | §4.4 preview-res CPU contract |
| Bake raster put + txn commit | ≤ 50 ms | same |
| Interactive canvas while a bake runs | slider p95 unchanged (separate process + Foreground slicing; measured) | same |
| GC sweep (1k rasters) | Background, cancellable, no canvas impact | same |

---

## 12. Test plan (per §8 strategy)

**Unit (PR-blocking):** `SegRecipe`/`MaskPrompt`/`AiSegPost` CBOR round-trip + unknown-field preservation (proptest); prompt→tensor encoding (SAM points/box, ADE20K class); pre/post tensor configs (T04); view→source resample math incl. out-of-crop and degenerate transforms; GC live-set computation; availability-state derivation from `MlStatus`.

**Integration (PR-blocking fast subset via `lightbox-cli`; full nightly):**
- **Replay determinism (T03):** bake → kill inferd → reopen → render: byte-identical weight buffer, zero IPC.
- **No-auto-recompute (T14):** geometry/slider edits + renders provoke zero inference; frame drift badges only.
- Recompute-as-edit: history step, undo restores the old raster, old raster survives GC while referenced.
- Rebake for paste/sync/preset on a 2-image fixture; Background derive-vs-infer IPC counts.
- Failure suite (T24) incl. export-does-zero-inference; `kill -9` fault injection on the bake txn path → `integrity_check` clean (extends E01's harness).
- XMP sidecar round-trip with and without embedded raster (E09's harness).

**Golden-image (PR-blocking, per process version):** AiSeg-in-pipeline goldens with **fixed committed rasters** (deterministic by construction) × post-param sets × a local adjustment, CPU+GPU within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; T15 identity proof (default post changes zero pre-existing E12 goldens). Cross-EP inference outputs are **not** goldened — §4.5.

**Model quality (nightly, per EP class):** IoU floors per model on fixture corpora (subject ≥ 0.90, sky ≥ 0.85, object-prompt ≥ 0.85); regressions file tracked issues per §8 perf-lane policy (driver/EP drift detection, not a product promise — the product artifact is the baked raster).

**Perf (nightly):** §11 harness incl. slider-latency-during-bake scenario.

**License (PR-blocking):** all four packs in the weights manifest (surface 1) with provenance (surface 3 where data assets apply); InsightFace denylist entry present; synthetic GPL-weights pack fails (E13's gate, re-asserted with E14's packs).

---

## 13. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **SAM ONNX export fiddliness** (encoder/decoder split, dynamic prompt counts) — the classic time sink | MobileSAM (proven exports) is the *default* tier; SAM2-small is a quality tier that can slip to a pack update with zero code change (tier is data); client-side orchestration means no protocol/daemon coupling |
| R2 | **SegFormer weight licensing** (NVIDIA research license vs Apache export lineages) | Named pin-time gate at T02 with the §1.6-pre-approved U2-Net sky fallback; CTO sign-off either way; pack-swap not design-change |
| R3 | **Cross-EP segmentation variance** (CoreML fp16 vs DirectML vs CPU) yields platform-different masks | §4.5 makes this a *creation-time-only* variance: existing masks never drift (baked + replayed); nightly per-EP IoU floors bound creation quality; `ep` recorded in provenance; `ep_denylist` curation at T19 |
| R4 | **`masks/` loss = edit loss** — rasters are authoritative edit state outside `catalog.sqlite`, and E01's verified backup covers only the DB | Default-on XMP raster embedding (T23) gives per-file redundancy; OQ1 escalates backup inclusion; GC is conservative-by-construction; missing-raster renders degrade to badge + CTA, never data-destroying |
| R5 | **XMP sidecar bloat** (60–300 KB per AI component) | Pref to strip with the replay trade-off documented; typical edit has ≤ 3 AI components; raster payload is per-component, not per-recipe |
| R6 | **View→source mapping fidelity** at heavy perspective/warp shifts mask edges | ≤ 0.5 px round-trip AC (T06) with property tests; real-geometry integration test once E11 lands (E12 R6's pattern); drift advisory + explicit recompute is the designed escape hatch |
| R7 | **E12/E13 interface drift at integration time** (both specs frozen but unbuilt at authoring) | E14 conforms — the semantic requirements (opaque `SegRecipe`, replay-not-recompute, generic-tensor SAM path) are stable even if signatures shift; divergences are filed against owners (E13 A7 protocol freeze + E12 §5 contracts are the anchors), never forked |
| R8 | **Post-kernel in E12's crate creates ownership friction** | Single small, additive, review-gated task (T15) with an identity-proof AC (zero existing goldens change); if E12's owner prefers, the kernel can live in `lightbox-ml::masking` behind an evaluator hook — a file-location decision, not a design one |

### Open questions

| # | Question | Owner / forum | Default if unresolved |
|---|---|---|---|
| OQ1 | Does `masks/` join the exit-time verified backup? (It is authoritative edit state — the only `.lbdata` content that is.) | E01/E03 owners + E14; escalate at M3 planning | XMP-embedded rasters (T23, default on) are the interim redundancy |
| OQ2 | Final sky pack pin: SegFormer Apache lineage vs U2-Net fallback | T02, with CTO license sign-off | U2-Net fallback (pre-approved §1.6) |
| OQ3 | Post-refinement UI depth in v1: full threshold/feather/expand sliders vs feather-only | Product at M3 planning; kernel supports all regardless | Ship all three (they exist for LR parity per research 04); UI cost is three sliders on E12's panel |
| OQ4 | Accept-time resolution for Objects: bake the previewed decode alpha (current decision) vs one final higher-res decode at accept | E14 eng, revisit after T19 quality review on fixtures | Bake the previewed alpha (predictability; §5.3 decision stands) |
| OQ5 | Rendition size cap (1536 px) vs model-native 1024²: is the extra resample worth it, or should the bake input match model input exactly? | T05/T09 fixture evaluation | ≤ 1536 px rendition, model-side resize per io-spec (keeps one rendition per bake for all models incl. Background derivation) |

---

## 14. Definition of done

E14 is done when all of the following hold on the CI matrix (macOS/Metal+CoreML, Windows/DX12+DirectML, Linux/Vulkan+CPU):

1. **The M3 exit criterion's AI leg passes:** Select Subject / Sky / Background / Objects work end-to-end on all three platforms on GPU and CPU EPs; AI-masked local adjustments render interactively within §11 budgets; an injected inferd crash at any stage is isolated, recovered, and never disturbs the editor or the edit store.
2. **§4.5 is mechanically enforced:** replay byte-identity (T03) and no-auto-recompute (T14) are PR-blocking and green; recompute is a history-visible edit with working undo restoring the prior raster byte-for-byte; export performs zero inference.
3. Every §5.5 degradation row is exercised by T24, including missing-raster, disk-full, VRAM-gate both answers, and pack/runtime-missing CTAs.
4. AiSeg-in-pipeline goldens (fixed rasters, CPU+GPU, per process version) are wired into §8's harness; adding T15's post support changed **zero** pre-existing E12 goldens (identity proof).
5. All four model packs have hash-pinned, license-cleared manifest entries passing surfaces 1/3; the SegFormer license question is resolved with CTO sign-off on record; the InsightFace denylist entry stands.
6. **Zero migrations shipped**; every persistent write flows through E12's DAO on the single-writer path; `kill -9` fault injection on the bake path leaves `integrity_check` clean; `edit_index.has_ai_mask` is populated and correct.
7. Raster lifecycle proven: `masks/` exempt from eviction (accounting assertion), GC property tests green (referenced rasters never collected), orphans from killed bakes swept.
8. XMP: sidecar round-trip restores a byte-identical replayable component (with raster embedded); pref-stripped sidecars degrade to the documented rebake path.
9. Nightly gates live: per-EP IoU floors, §11 perf budgets with committed baselines, slider-latency-during-bake scenario.
10. Seam handoffs delivered and acknowledged: T15 reviewed by the E12 owner; `rebake` consumed by E12's F3 tests; `SegRecipe` schema + `lb:` field doc delivered to E16; no file outside §3's declared surface is owned or forked, and the retired v1.x scope (search/faces/auto-tag) has left **no** code, tables-in-use, or UI behind.
