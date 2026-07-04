# E15 — Export & Output Engine

_Implementation spec. Milestone **M2**; effort **M–L ~5–7 pw**; depends on **E05** (render engine), **E06** (jobs), **E09** (edit state / XMP). Author: staff-engineer (epic planner). Inputs: `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (approved, decision-complete), `docs/research/06-import-export-and-output-modules-lightroom-classic.md`, `docs/research/00-feature-catalog.md` §2.8._

This spec does not re-litigate stack or boundary decisions. Where E15 touches a neighboring epic it **names the seam and pins the contract**; it does not design the neighbor.

---

## 1. Scope

E15 delivers the complete output side of the M2 exit criteria: *"Export engine (JPEG/PNG/TIFF/DNG/JXL/AVIF, sizing, output sharpening, watermark, metadata filtering, presets, external-editor round-trip)"* and the M2 exit test *"export a 3k-image batch without starving the canvas."*

In scope (feature-catalog §2.8 Must + the Should items the milestone plan names):

1. **Export pipeline engine** (`lightbox-export`): per-image pipeline *render → resize → output color transform → output sharpen → watermark → quantize → encode → embed metadata → atomic write*, run as `Class::Foreground` jobs (§5.3 of the architecture) with cancellation, progress, and per-item error isolation.
2. **File formats:** JPEG (quality + target-file-size limit), PNG (8/16), TIFF (8/16, None/LZW/Deflate), JPEG XL, AVIF, **DNG** (raw→DNG conversion with embedded recipe XMP), **Original passthrough** (any asset incl. video, optional recipe sidecar).
3. **Output color management:** sRGB / Display-P3 / Adobe-RGB-compatible / ProPhoto + arbitrary user ICC, rendering intent, 8/16-bit, ICC embedding — consuming `lightbox-color` (E02.3 seam).
4. **Sizing:** long edge / short edge / width×height / megapixels / percent, don't-enlarge guard, PPI resolution stamp (metadata only).
5. **Output sharpening:** screen/matte/glossy × low/standard/high, post-resize, PhotoKit-style parameter table.
6. **Watermarking:** text (font/color/opacity/shadow) + PNG graphic; 9-anchor layout, proportional size, insets, rotation; watermark presets; live-preview raster API for the editor dialog.
7. **Metadata filtering & privacy:** tiers (copyright-only / copyright+contact / all-except-camera-raw / all), strip-GPS, strip-person-keywords, keyword hierarchy vs flat, honor keyword export flags — via `lightbox-meta`.
8. **Destination & naming:** destination folder (+ optional subfolder / export-to-source), token-based filename templates (shared engine, seam to E04), collision policy (ask / overwrite / rename-unique / skip), cross-platform filename sanitization.
9. **Presets & batch conveniences:** export presets (CRUD, catalog-stored), multi-preset batch (render once per image, fan out post-render), Export-with-Previous, post-export actions (reveal in file manager / open with app).
10. **Re-add to catalog:** exported files optionally re-imported add-in-place with provenance link to the source image.
11. **External-editor round-trip:** render 8/16-bit TIFF handoff with adjustments applied (or original copy), spawn configured editor, watch for the returned file, re-catalog next to the source with provenance.
12. **Thin shell UI** for the above: export dialog (panel stack), watermark editor with live preview, collision/error resolution, Edit-In menu + editor presets prefs. (No other epic owns export UI; E08 owns the *library* surfaces only. The UI here is deliberately thin — panels binding to core commands, no new widget infrastructure.)
13. **Headless coverage:** `lightbox-cli export` used by the §8 E2E gate.

Everything runs local-first; no network paths exist in this epic.

## 2. Explicit non-goals

| Excluded | Why / where it lives |
|---|---|
| Publish services framework (stateful new/modified/published sync), hard-drive publish target | Deferred to v1.x by the architecture (§10.1 deferred list). The `export_session`/`export_item` tables are designed so a publish layer can be built on top later without migration surgery. |
| Print / Book / Slideshow / Web modules | Mandate non-goals. |
| Video transcoding (H.264 export, quality tiers) | Mandate limits video to playback/metadata. Video assets export via **Original passthrough** only. |
| Email share, cloud share links, online services (Flickr/SmugMug) | Could-tier; out of v1. |
| Export/post-process plugin SDK, droplet/export-actions folder | Plugin SDK is v1.x; we ship reveal/open-with only. The `Encoder` trait and `PostAction` enum are the future extension points — designed for, not shipped. |
| Soft proofing | E10/deferred; export color transforms here are the non-proofed output path. |
| HDR gain-map export (libultrahdr) | v1.x design headroom per architecture. |
| C2PA content credentials | Could-tier (catalog §2.7); the `Encoder` metadata hook leaves room. |
| PSD output | Not in the architecture's format list; TIFF is the interchange format. |
| Smart-preview generation | E03 territory. |
| Import-side naming UI, batch rename | E04/E07; E15 only **shares** the token engine (seam, §5.5). |
| Grid "stack with original" UI | E15 records provenance (`export_item.readded_asset_id`, `external_edit.returned_asset_id`); stack presentation is an E07/E08 decision (open question #7). |

## 3. Crates / modules touched

Per the architecture's decomposition (§2.1/§2.2):

| Crate | Role in E15 | Change type |
|---|---|---|
| **`lightbox-export`** | The epic's home: settings model, planner, pipeline orchestration, resize/sharpen/watermark, encoders, external-editor round-trip. | **New (owner)** |
| `lightbox-templates` (new small shared module/crate) | Token filename-template engine shared with E04's rename templates. First epic to land creates it; the interface is pinned here (§5.5). | New (shared, coordinate with E04) |
| `lightbox-catalog` | Migration for `export_preset`, `watermark_preset`, `export_session`, `export_item`, `external_editor`, `external_edit` + DAOs. | Additive migration |
| `lightbox-core` | Command/query façade additions: plan/run/cancel export, preset CRUD, export-with-previous, edit-in. | Additive |
| `lightbox-render` | **Consumed.** `RenderRequest { target: RenderTarget::Export }` full-res render + CPU readback. Contract pinned in §5.8; readback is an E05 deliverable E15 integrates against. | Consume (seam) |
| `lightbox-jobs` | **Consumed.** `Class::Foreground` jobs, `CancelToken`, activity-model progress. | Consume |
| `lightbox-edit` (E09) | **Consumed.** `Recipe` materialization per image, `Recipe::to_xmp()` for sidecars/DNG embed. | Consume (seam) |
| `lightbox-color` | **Consumed.** Working→output ICC transforms, standard output profile set, user-ICC loading. Required API pinned in §5.7 (E02.3 owns implementation). | Consume (seam; small additive requests if gaps) |
| `lightbox-meta` | Metadata policy engine + per-container embedding for export. | Additive module (`meta::export`) |
| `lightbox-ingest` | **Consumed.** Add-in-place single-file ingest for re-add-to-catalog and returned external edits. | Consume (seam) |
| `lightbox-shell` | Export dialog, watermark editor, Edit-In menu, prefs pane for external editors, conflict/error dialogs. | Additive panels |
| `lightbox-cli` | `export` subcommand for headless E2E. | Additive |

New third-party dependencies (all pass surface-1/2 gates): `mozjpeg` (IJG/BSD-style, encode), `libjxl` (BSD-3, static), `libavif` (BSD-2, static), `fast_image_resize` (MIT, already selected §1.6), `cosmic-text` + `fontdb` (MIT, watermark text raster), one bundled OFL-1.1 fallback font (surface-3 manifest entry), `dnglab`/rawler DNG writer (MIT). `png`/`tiff` crates already in-tree via E02/E03.

---

## 4. Design overview

### 4.1 The per-image pipeline (fixed stage order)

```
                       ┌────────────── lightbox-render (E05) ──────────────┐
  ImageId + Recipe ───►│ Engine::submit(target=Export)  → full-res render  │
                       │ (all §4.1 stages EXCEPT the view/output transform)│
                       └───────────────┬───────────────────────────────────┘
                                       ▼  readback (seam §5.8)
                    ExportImage: linear ProPhoto-primaries f32 RGB, backend tag
                                       ▼
   [1] RESIZE        fast_image_resize Lanczos3, in linear f32 (all Sizing modes)
                                       ▼
   [2] OUTPUT COLOR  lightbox-color: ProPhoto-linear → output ICC (intent),
                     gamma-encoded output-referred, f32 → keep f32
                                       ▼
   [3] SHARPEN       output sharpening (medium × strength), luma-weighted USM,
                     on output-referred data at final resolution
                                       ▼
   [4] WATERMARK     raster (text or graphic) laid out for final W×H,
                     alpha-composited in output space
                                       ▼
   [5] QUANTIZE      f32 → u8/u16 per format bit depth, error-diffusion dither on 16→8
                                       ▼
   [6] ENCODE        Encoder trait: JPEG/PNG/TIFF/JXL/AVIF (+ ICC bytes embedded)
                                       ▼
   [7] METADATA      lightbox-meta: EXIF/IPTC/XMP per MetadataPolicy; Orientation=1
                                       ▼
   [8] ATOMIC WRITE  temp-then-rename in destination dir; fsync; register in session
```

**Ordering rationale (decisions, not defaults):**
- **Resize in linear light** (stage 1 before 2): resampling gamma-encoded data darkens high-contrast edges; we have the linear buffer anyway, so we resize there. This is the quality-correct order and costs nothing.
- **Sharpen after the output transform** (stage 3 after 2): output sharpening is perceptual, tuned for the medium's viewing condition; PhotoKit-style tables assume output-referred (gamma-encoded) data at final resolution. Sharpening in linear would over-drive shadows.
- **Watermark after sharpening**: the watermark must not be sharpened or resampled — it is rasterized directly at final resolution. Compositing happens in the output space (matches user expectation of "opacity 50% looks like 50%"; see open question #3 for the gamma note).
- **Output transform on CPU via LCMS2, not the GPU output node.** Architecture §4.1 places "working → output ICC (export)" as the logically-last stage; E15 executes it CPU-side in `lightbox-export` because (a) arbitrary user ICC profiles require LCMS2 exactness, (b) the resize already forced a CPU readback, and (c) it keeps encode-input generation deterministic and golden-testable independent of GPU backend. The GPU display-transform node remains the *view* path only. (A future GPU resize+transform fast path is noted in §10 as a perf option, not v1.)
- **DNG and Original bypass stages entirely** — they are container/copy operations, not rendered pixels (§5.4).

### 4.2 Concurrency shape

One export session = one `Class::Foreground` job group (E06). Internally a three-stage bounded pipeline:

```
 render lane (width 1 per GPU, serialized through Engine; CPU-fallback lane on device-lost)
   │  bounded(2)
   ▼
 postprocess pool (rayon: resize/color/sharpen/watermark/quantize; width = min(physical cores, 4))
   │  bounded(4)
   ▼
 encode+write pool (width = min(physical cores, 4); mozjpeg/libjxl/libavif are CPU-bound)
```

- The render lane submits to the shared Engine with **Foreground** priority; the Engine's scheduler time-slices against **Interactive** (E05/E06 contract) so the develop canvas is never starved — this is the mechanism behind the M2 exit test and §7's "zero interactive-canvas starvation."
- Bounded channels give backpressure: a 3k-image batch holds at most ~7 decoded full-res frames in memory (~7 × 45 MP × 12 B ≈ 3.8 GB worst-case at 100 MP → channel widths are computed from a memory budget: `max_inflight = clamp(ram_budget / frame_bytes, 1, 4)`).
- **Cancellation** (cooperative `CancelToken`): checked between stages and inside encoders' row loops where the codec API allows; on cancel, temp files are deleted, `export_item` rows flip to `cancelled`, the session closes cleanly.
- **Multi-preset batch:** the planner groups work by image; the render lane produces one `ExportImage` per image, and the postprocess stage fans out one task per (image, preset) — N presets cost N post-render pipelines but **one render**. Presets whose format is DNG/Original skip the render lane.

### 4.3 Failure isolation

Per-item errors (encode failure, ENOSPC, unreadable original, decode error) fail **that item only**: the item records `status='failed'` + `error`, the session continues, and the completion report lists failures. Pre-flight (free space, missing originals, destination writability, raw-only DNG validation) runs at plan time so predictable failures surface before any pixels move (§6 of the architecture: disk-full row). A GPU device-lost during export follows §4.4/§6.2: the render lane retries on the rebuilt device once, then degrades to the CPU path (full-res CPU render is minutes-scale — progress UI shows it honestly; `render_backend` records which path produced each file, per §4.4 "export records which backend produced the file").

---

## 5. Interface definitions

Signatures are the contract; bodies are implementation. All types `serde`-serializable where they persist (CBOR, schema-versioned like the recipe).

### 5.1 Settings model (`lightbox-export::settings`)

```rust
/// Versioned root document. Persisted as CBOR in export_preset.doc and
/// export_session.settings_doc. schema bumps follow the recipe-migration pattern (§3.2).
pub struct ExportSettings {
    pub schema: u16,                       // = 1
    pub dest: Destination,
    pub naming: NamingSpec,
    pub collision: CollisionPolicy,
    pub format: FileFormat,
    pub color: OutputColor,
    pub sizing: Sizing,
    pub sharpen: Option<OutputSharpen>,
    pub watermark: Option<WatermarkSpec>,  // inline doc or preset ref, resolved at plan time
    pub metadata: MetadataPolicy,          // defined in lightbox-meta (§5.6)
    pub readd: Option<ReaddPolicy>,
    pub post_action: PostAction,
}

pub enum Destination {
    Folder { path: PathBuf, subfolder: Option<String> },
    SameAsSource { subfolder: Option<String> },
}

pub enum CollisionPolicy { Ask, Overwrite, RenameUnique, Skip }

pub enum FileFormat {
    Jpeg  { quality: u8 /*0..=100*/, size_limit_kb: Option<u32>, chroma: ChromaMode },
    Png   { depth: BitDepth8or16 },
    Tiff  { depth: BitDepth8or16, compression: TiffCompression /*None|Lzw|Deflate*/ },
    Jxl   { lossless: bool, distance: f32 /*0.0..=15.0*/, effort: u8 /*1..=9*/ },
    Avif  { quality: u8 /*0..=100*/, depth: AvifDepth /*Eight|Ten*/, speed: u8 /*0..=10*/ },
    Dng   { compression: DngCompression /*Lossless|Uncompressed*/, embed_original: bool,
            preview: DngPreview /*None|Medium|Full*/ },
    Original { write_sidecar_xmp: bool },
}
pub enum ChromaMode { Auto /*4:4:4 if q>=80 else 4:2:0*/, Force444, Force420 }

pub struct Sizing {
    pub rule: SizeRule,
    pub dont_enlarge: bool,
    pub ppi: Option<Ppi>,                  // metadata stamp only, never resamples
}
pub enum SizeRule {
    None,
    LongEdge  { px: u32 },
    ShortEdge { px: u32 },
    Fit       { w: u32, h: u32 },          // fit within box, aspect preserved
    Megapixels{ mp: f32 },
    Percent   { pct: f32 },                // 1.0..=1000.0
}
pub struct Ppi { pub value: u16, pub unit: PpiUnit /*Inch|Cm*/ }

pub struct OutputColor {
    pub space: OutputSpace,
    pub intent: RenderingIntent,           // Perceptual | RelativeColorimetric (BPC on)
    pub depth: BitDepth,                   // validated against format at plan time
}
pub enum OutputSpace { Srgb, DisplayP3, AdobeRgbCompatible, ProPhoto, UserIcc { path: PathBuf } }

pub struct OutputSharpen { pub medium: SharpenMedium, pub amount: SharpenAmount }
pub enum SharpenMedium { Screen, Matte, Glossy }
pub enum SharpenAmount { Low, Standard, High }

pub enum ReaddPolicy { Add, AddStackedWithOriginal }  // stacking presentation = E07/E08 seam
pub enum PostAction { None, RevealInFileManager, OpenWith { app: PathBuf } }
```

### 5.2 Planner, sessions, execution (`lightbox-export::run`)

```rust
pub struct Exporter { /* holds: Arc<Engine>, jobs handle, catalog handles, color ctx, meta ctx */ }

impl Exporter {
    /// Resolves everything decidable before pixels move: output names per template,
    /// collision scan (materializes Ask conflicts), duplicate-target detection within
    /// the batch, free-space preflight (§6), source validation (missing originals,
    /// DNG-requires-raw), watermark/ICC/font resource resolution.
    /// Pure + side-effect free; safe to call repeatedly as the dialog changes.
    pub fn plan(&self, images: &[ImageId], settings: &[ExportSettings]) -> Result<ExportPlan, PlanError>;

    /// Spawns the session as Foreground jobs (§4.2 pipeline). One session per settings
    /// entry (multi-preset = N sessions sharing one render lane via the batch group).
    pub fn run(&self, plan: ExportPlan) -> Result<ExportRun, ExportError>;
}

pub struct ExportPlan {
    pub items: Vec<PlannedItem>,           // (image_id, settings_idx, resolved output path)
    pub conflicts: Vec<Conflict>,          // non-empty + policy==Ask → UI must resolve before run
    pub warnings: Vec<PlanWarning>,        // e.g. video item under a rendered format → passthrough
    pub est_bytes: u64,                    // free-space preflight input
}
pub struct PlannedItem {
    pub image: ImageId,
    pub settings_idx: usize,
    pub out_path: PathBuf,                 // final, sanitized, collision-resolved (unless Ask)
    pub needs_render: bool,                // false for Dng/Original
}
pub enum Conflict { Exists { item: usize, path: PathBuf },
                    DuplicateTarget { items: Vec<usize>, path: PathBuf } }

pub struct ExportRun {
    pub session_ids: Vec<ExportSessionId>,
    pub progress: tokio::sync::watch::Receiver<ExportProgress>,
    pub cancel: CancelToken,
    pub done: JobHandle<ExportReport>,
}
pub struct ExportProgress { pub total: u32, pub completed: u32, pub failed: u32,
                            pub current: Option<(ImageId, ExportStage)>, pub bytes_written: u64 }
pub enum ExportStage { Rendering, PostProcessing, Encoding, Writing }
pub struct ExportReport { pub session_ids: Vec<ExportSessionId>,
                          pub ok: u32, pub failed: Vec<(ImageId, ExportError)>, pub skipped: u32,
                          pub outputs: Vec<PathBuf> }

pub enum ExportError {
    SourceMissing, SourceDecode(String), RenderFailed(String),
    IccLoad(PathBuf, String), WatermarkResource(String), FontResource(String),
    Encode { format: &'static str, msg: String },
    Io(std::io::Error), DiskFull { needed: u64, available: u64 },
    Cancelled, DngFromNonRaw,
}
```

### 5.3 Pixel post-processing (`lightbox-export::pixel`)

```rust
/// Full-res render output, CPU-side. Linear light, ProPhoto primaries (the §4.1 working
/// space), geometry/crop/retouch/local already applied by the Engine. Interleaved RGB f32.
pub struct ExportImage {
    pub w: u32, pub h: u32,
    pub rgb: Box<[f32]>,                   // len = w*h*3
    pub backend: RenderBackend,            // Gpu | Cpu — provenance (§4.4)
}

/// Output-referred buffer between stages 2–5 and the encoder input after quantize.
pub struct OutputImage {
    pub w: u32, pub h: u32,
    pub pixels: PixelBuf,                  // F32(Box<[f32]>) | U16(Box<[u16]>) | U8(Box<[u8]>), RGB
    pub icc: Arc<IccBytes>,                // profile to embed
    pub ppi: Option<Ppi>,
}

pub fn resize(src: &ExportImage, rule: &Sizing, orientation_applied: (u32, u32))
    -> Result<ExportImage, ExportError>;   // Lanczos3 via fast_image_resize, linear f32; no-op passthrough for SizeRule::None

pub fn sharpen_output(img: &mut OutputImage, s: OutputSharpen);
    // luma-weighted unsharp mask; (radius, amount, threshold) from the medium×strength
    // table in pixel/sharpen_params.rs — radius scales with output pixel density.

pub fn quantize(img: OutputImage, depth: BitDepth, dither: bool) -> OutputImage;
    // f32→u16 round-to-nearest; f32→u8 with Floyd–Steinberg when dither=true (default on).
```

### 5.4 Encoders (`lightbox-export::encode`)

```rust
pub trait Encoder: Send + Sync {
    fn id(&self) -> &'static str;                       // "jpeg", "png", ...
    fn accepts(&self, depth: BitDepthOf<PixelBuf>) -> bool;
    /// Encodes pixels + ICC into `out`. Metadata is embedded by lightbox-meta afterwards
    /// (or inline where the container requires it — the encoder exposes reserved segments).
    fn encode(&self, img: &OutputImage, opts: &FileFormat, out: &mut dyn std::io::Write)
        -> Result<(), ExportError>;
}

pub struct EncoderRegistry { /* format discriminant → Box<dyn Encoder> */ }

/// JPEG size-limit search: bisect quality in [min_q, q0] re-encoding until
/// bytes <= limit or floor reached; ≤ 7 iterations (1-unit quality resolution),
/// returns best-fitting encode or SizeLimitUnreachable error carrying smallest size.
pub fn encode_jpeg_size_limited(img: &OutputImage, base: &FileFormat, limit_kb: u32,
                                out: &mut dyn std::io::Write) -> Result<u8 /*used q*/, ExportError>;

/// DNG + Original are NOT Encoders (no rendered pixels):
pub fn export_dng(asset: &AssetRef, recipe_xmp: &XmpDoc, opts: &FileFormat /*Dng*/,
                  out_path: &Path) -> Result<(), ExportError>;   // dnglab conversion; raw sources only
pub fn export_original(asset: &AssetRef, sidecar: Option<&XmpDoc>, out_path: &Path)
    -> Result<(), ExportError>;            // byte copy + xxh3 verify; sidecar written as <name>.xmp
```

### 5.5 Filename token engine (`lightbox-templates` — SHARED seam with E04)

One implementation for import rename, in-catalog rename (E07), and export naming. **First epic to land creates the crate; this interface is the pinned contract** (E04's planner has the same text — divergence is a planning bug).

```rust
pub struct NameTemplate { pub tokens: Vec<Token> }     // parsed from "{date:YYYYMMDD}-{seq:4}"
pub enum Token {
    Literal(String),
    OriginalName, OriginalNumberSuffix,
    Sequence { start: u32, pad: u8 },
    Date(DateFmt),                                     // capture date; YYYY, MM, DD, HH, mm, ss combos
    Meta(MetaField),                                   // CameraModel | Iso | Rating | Title | ...
    CustomText,                                        // dialog-supplied per run
}
impl NameTemplate {
    pub fn parse(s: &str) -> Result<Self, TemplateError>;
    pub fn render(&self, ctx: &NameCtx) -> String;     // extension NOT included; caller appends
}
pub struct NameCtx<'a> { pub original_stem: &'a str, pub capture_time: Option<DateTime>,
                         pub seq: u32, pub meta: &'a dyn MetaLookup, pub custom_text: &'a str }
pub fn sanitize_filename(s: &str) -> String;           // cross-platform reserved chars/names/length
```

### 5.6 Metadata policy (`lightbox-meta::export` — additive module in E15)

```rust
pub struct MetadataPolicy {
    pub tier: MetaTier,
    pub strip_gps: bool,
    pub strip_person_keywords: bool,       // person keywords come from the keyword table's person flag (E07/E14 seam)
    pub keywords: KeywordStyle,            // Hierarchy | Flat | None; honors per-keyword export flags
    pub include_develop_xmp: bool,         // crs:/lb: projection of the recipe; forced false below tier All
}
pub enum MetaTier { CopyrightOnly, CopyrightAndContact, AllExceptCameraRawInfo, All }

/// Gathers from the catalog (reader handle) + original file, applies the policy,
/// returns a container-agnostic payload. Pure; unit/property-testable.
pub fn build_export_metadata(cat: &ReaderHandle, image: ImageId, policy: &MetadataPolicy,
                             recipe_xmp: Option<&XmpDoc>) -> Result<MetadataPayload>;

pub struct MetadataPayload { pub exif: ExifSet, pub iptc: IptcSet, pub xmp: XmpDoc }

/// Embeds into the finished file per container (JPEG APP1/APP2, TIFF tags,
/// PNG eXIf + iTXt-XMP, JXL Exif/xml boxes, AVIF Exif/XMP items).
/// Always writes Orientation = 1 (pixels are exported upright).
pub fn embed_metadata(path: &Path, container: Container, payload: &MetadataPayload) -> Result<()>;
```

Invariants the implementation must satisfy (tested in §8): `strip_gps` removes **every** location-bearing tag across EXIF GPS IFD, XMP (`exif:GPS*`), and IPTC location fields; `strip_person_keywords` removes person-flagged keywords from `dc:subject`, `lr:hierarchicalSubject`, and IPTC keywords; `CopyrightOnly` yields exactly the copyright/rights fields and nothing else camera- or location-derived.

### 5.7 Color seam (`lightbox-color` — consumed; contract E02.3 must satisfy)

```rust
/// Standard output profiles are SYNTHESIZED at build/first-run via LCMS2 from
/// published primaries (sRGB builtin; P3-D65; "compatible with Adobe RGB (1998)"
/// from published 1998 primaries; ROMM/ProPhoto per ISO 22028-2). No Adobe-authored
/// ICC file is ever bundled (mandate constraint 4; §8 surface-3 manifest entries).
pub fn output_profile(space: OutputSpace) -> Result<Arc<IccProfile>>;
pub fn load_user_icc(path: &Path) -> Result<Arc<IccProfile>>;   // validates: RGB display/output class

/// Prebuilt LCMS2 transform, working(ProPhoto-linear f32) → output profile, BPC on
/// for RelativeColorimetric. Thread-safe, reusable across a batch.
pub struct OutputTransform { /* cmsHTRANSFORM + profile bytes */ }
pub fn build_output_transform(p: &IccProfile, intent: RenderingIntent) -> Result<OutputTransform>;
impl OutputTransform {
    pub fn apply_f32(&self, src_rgb: &[f32], dst_rgb: &mut [f32]);  // row-chunked, rayon-friendly
    pub fn icc_bytes(&self) -> Arc<IccBytes>;                        // for embedding
}
```

If any part is missing when E15 starts, it is raised with the E02 owner as a seam gap — E15 does not fork its own color math.

### 5.8 Render seam (`lightbox-render` — consumed; contract pinned with E05)

```rust
// Existing per §2.2: Engine::submit(RenderRequest { image, recipe, pv, roi, scale, target })
// E15 pins the Export target semantics:
//  - target = RenderTarget::Export, roi = full frame, scale = 1:1
//  - graph evaluates ALL §4.1 stages EXCEPT the final view/output transform
//    (output stays linear working space)
//  - completion yields a CPU readback (tiles assembled, RGBA16F → f32 RGB):
pub enum RenderState { Queued, Preview(TextureRef), Full(TextureRef),
                       ExportReady(ExportImage), Failed(RenderError) }
//  - Foreground priority: time-sliced against Interactive (§5.3) — never starves the canvas
//  - device-lost during an export render: one retry on rebuilt device, then CPU path;
//    ExportImage.backend records which path produced the pixels (§4.4)
```

The readback (staging buffer, tile assembly, f16→f32) is **E05.3/E05.5 machinery**; E15 integrates and tests against it. If `ExportReady` doesn't exist when E15 starts, it is the first cross-epic work item, implemented in `lightbox-render` by the E05 owner with E15 as first consumer.

### 5.9 External-editor round-trip (`lightbox-export::edit_in`)

```rust
pub struct ExternalEditor {                             // persisted in external_editor table
    pub id: ExternalEditorId, pub name: String, pub app: PathBuf,
    pub handoff: HandoffFormat,
}
pub struct HandoffFormat { pub format: TiffOrPng /*Tiff{8|16}|Png{8|16}*/,
                           pub space: OutputSpace, pub ppi: Option<Ppi> }

pub enum EditInVariant {
    CopyWithAdjustments,     // render via §4.1 pipeline (no watermark/sharpen), raw + non-raw
    CopyWithoutAdjustments,  // non-raw only: byte-copy converted container, no recipe
    Original,                // non-raw only: hand the original file itself to the editor
}

impl Exporter {
    /// Renders "<stem>-Edit.<ext>" next to the original (collision → -Edit-2…),
    /// inserts external_edit row (status='waiting'), spawns the editor detached,
    /// registers a debounced fs-watch on the handoff file.
    pub fn edit_in(&self, image: ImageId, editor: &ExternalEditor, v: EditInVariant)
        -> Result<ExternalEditId, ExportError>;
}

/// Watch policy: on modification, wait for write-quiescence (no size/mtime change for
/// 2 s, file openable exclusively where the platform allows), then re-ingest add-in-place
/// via lightbox-ingest, set returned_asset_id, status='returned'. Rows still 'waiting'
/// at app relaunch are re-armed; the file is re-checked (mtime > created_at → treat as
/// returned). An explicit user "forget" flips to 'cancelled'; a missing handoff file
/// flips to 'orphaned'.
```

### 5.10 Core façade additions (`lightbox-core`)

```rust
// Commands (mutating, through the command bus; export runs are jobs, not history steps —
// exporting is not an edit and never touches history_step):
Cmd::ExportPlan     { images: Vec<ImageId>, settings: Vec<ExportSettings> } -> ExportPlan
Cmd::ExportRun      { plan: ExportPlan }                                    -> ExportRun
Cmd::ExportCancel   { session: ExportSessionId }
Cmd::ExportWithPrevious { images: Vec<ImageId> }        // re-runs latest session's settings_doc
Cmd::ExportPresetSave { name: String, settings: ExportSettings } / Delete / Rename
Cmd::WatermarkPresetSave { name: String, wm: WatermarkSpec } / Delete / Rename
Cmd::ExternalEditorSave { editor: ExternalEditor } / Delete
Cmd::EditIn         { image: ImageId, editor: ExternalEditorId, variant: EditInVariant }

// Queries (reader handle):
Query::ExportPresets, Query::WatermarkPresets, Query::ExternalEditors,
Query::LastExportSettings,                       // for Export-with-Previous enablement
Query::ExportSessions { limit }, Query::ExportSession { id }  // history/report UI
```

### 5.11 Watermark model (`lightbox-export::watermark`)

```rust
pub struct WatermarkSpec { pub schema: u16, pub kind: WatermarkKind, pub layout: WmLayout }
pub enum WatermarkKind {
    Text { text: String, font_family: String, weight: FontWeight, style: FontStyle,
           color: Rgba8, shadow: Option<WmShadow> },
    Graphic { path: PathBuf },                          // PNG with alpha; resolved+validated at plan time
}
pub struct WmShadow { pub opacity: f32, pub offset_px: f32, pub radius: f32, pub angle_deg: f32 }
pub struct WmLayout {
    pub anchor: Anchor9,                                // TL TC TR | CL C CR | BL BC BR
    pub inset_pct: (f32, f32),                          // horizontal/vertical, % of image dims
    pub size: WmSize,                                   // Proportional { pct_of_long_edge } | Native
    pub rotation: WmRotation,                           // None | Cw90 | Ccw90
    pub opacity: f32,                                   // 0.0..=1.0
}

/// Rasterizes at target resolution. Text via cosmic-text + fontdb (system fonts,
/// bundled OFL fallback). Same call feeds the export pipeline and the dialog's
/// live preview (rendered over a T1 preview) — one code path, no drift.
pub fn rasterize(spec: &WatermarkSpec, target_w: u32, target_h: u32) -> Result<WmRaster, ExportError>;
pub fn composite(dst: &mut OutputImage, wm: &WmRaster, layout: &WmLayout);  // premultiplied src-over
```

---

## 6. Data model & migrations

One additive migration (no changes to existing tables). Timestamps are unix epoch seconds; `doc` blobs are schema-versioned CBOR per §5.1.

```sql
-- migration NNN_export_output.sql (next free number at land time)

CREATE TABLE export_preset (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    doc         BLOB NOT NULL,              -- CBOR ExportSettings (schema-versioned)
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE watermark_preset (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    doc         BLOB NOT NULL,              -- CBOR WatermarkSpec
    updated_at  INTEGER NOT NULL
);

CREATE TABLE export_session (
    id            INTEGER PRIMARY KEY,
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER,
    settings_doc  BLOB NOT NULL,            -- resolved ExportSettings snapshot (presets inlined)
    dest_path     TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('running','done','cancelled','failed')),
    total         INTEGER NOT NULL,
    completed     INTEGER NOT NULL DEFAULT 0,
    failed        INTEGER NOT NULL DEFAULT 0
);
-- Export-with-Previous = settings_doc of MAX(started_at) session; no extra state row.

CREATE TABLE export_item (
    id               INTEGER PRIMARY KEY,
    session_id       INTEGER NOT NULL REFERENCES export_session(id) ON DELETE CASCADE,
    image_id         INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    output_path      TEXT,
    status           TEXT NOT NULL CHECK (status IN
                       ('pending','running','done','skipped','failed','cancelled')),
    error            TEXT,
    render_backend   TEXT CHECK (render_backend IN ('gpu','cpu')),   -- §4.4 provenance
    readded_asset_id INTEGER REFERENCES asset(id) ON DELETE SET NULL -- re-add provenance
);
CREATE INDEX idx_export_item_session ON export_item(session_id, status);
CREATE INDEX idx_export_item_image   ON export_item(image_id);

CREATE TABLE external_editor (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL UNIQUE,
    app_path TEXT NOT NULL,
    doc      BLOB NOT NULL                  -- CBOR HandoffFormat
);

CREATE TABLE external_edit (
    id                INTEGER PRIMARY KEY,
    image_id          INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    editor_name       TEXT NOT NULL,        -- denormalized; editor preset may be deleted later
    handoff_path      TEXT NOT NULL,
    variant           TEXT NOT NULL CHECK (variant IN
                        ('copy_with_adjustments','copy_without_adjustments','original')),
    status            TEXT NOT NULL CHECK (status IN ('waiting','returned','cancelled','orphaned')),
    created_at        INTEGER NOT NULL,
    returned_asset_id INTEGER REFERENCES asset(id) ON DELETE SET NULL
);
CREATE INDEX idx_external_edit_status ON external_edit(status);
```

Notes:
- **Session rows are progress accounting, not a job queue** — the E06 job system owns scheduling; the rows exist for the report UI, Export-with-Previous, crash forensics, and as the future substrate for a v1.x publish layer (deliberate column shape: `image_id + settings snapshot + output_path + status`).
- Writes go through the single catalog writer (§5.1 of the architecture) in batched transactions: item status updates are coalesced (≤1 txn per 250 ms per session) so a 3k-item export doesn't serialize 3k tiny write txns against interactive culling writes.
- Crash recovery: on launch, any `export_session.status='running'` flips to `'failed'` and its `pending/running` items to `'cancelled'`; orphaned `*.lbtmp` temp files in recorded dest dirs are swept.

## 7. Ordered task breakdown

Each task ≤1 day, with acceptance criteria (AC). Order respects dependencies; tasks in the same phase with no arrow between them can parallelize across 2 engineers. Total: **32 tasks ≈ 6.4 pw**, inside the M–L 5–7 pw envelope.

### Phase E15.0 — contracts, schema, planning spine

- **T01 — Settings model + serialization.** Implement §5.1 types, CBOR (de)serialize, schema-version guard, JSON debug mirror.
  *AC:* `proptest` round-trip identity over generated `ExportSettings`; unknown-future-field document with higher schema is rejected with a typed error; format/depth validation matrix (e.g. `Jpeg` + `Sixteen` → plan-time error) unit-tested.
- **T02 — Catalog migration + DAOs.** §6 migration; DAO CRUD for the six tables; launch-time crash-recovery sweep (running→failed).
  *AC:* migration applies on fresh and on an M1-era catalog; `kill -9` during migration leaves catalog `integrity_check`-clean (copy-on-write upgrade path per §6 of the architecture, exercised by the existing fault-injection rig); DAO round-trips all row shapes.
- **T03 — Token template engine (`lightbox-templates`).** Parse + render + `sanitize_filename` per §5.5. Coordinate with E04 owner (single implementation).
  *AC:* golden table of ≥25 template→name cases (dates, sequences w/ padding, metadata tokens, missing-metadata fallback, unicode, Windows-reserved names `CON`/`NUL`, >240-char truncation); fuzz `parse` never panics.
- **T04 — Output-name resolution + collision policies.** Batch name rendering with stable sequence numbering (selection order), duplicate-target detection, per-policy resolution (Ask materializes `Conflict`s; RenameUnique appends `-2`, `-3`…; Skip marks items).
  *AC:* unit tests per policy incl. two images rendering to the same name inside one batch; case-insensitive collision detection on case-insensitive filesystems.
- **T05 — Export planner.** `Exporter::plan` per §5.2: destination resolution (incl. SameAsSource + subfolder creation plan), free-space preflight vs `est_bytes`, missing-original and DNG-from-non-raw validation, watermark/ICC/font resource resolution, video-under-rendered-format → passthrough warning.
  *AC:* each preflight failure class yields the right `PlanError`/warning without touching disk; plan is deterministic and side-effect free (called twice → identical output).
- **T06 — Session orchestration.** §4.2 pipeline skeleton on E06: Foreground job group, bounded channels with memory-budgeted widths, cancel propagation, temp-file cleanup, coalesced status writes, `watch` progress, `ExportReport`.
  *AC:* integration test with a stub render lane + stub encoder: cancel mid-batch leaves zero `*.lbtmp` files, session/item rows consistent (`cancelled`), progress monotonic; per-item induced failure fails only that item.

### Phase E15.1 — pixel path

- **T07 — Render seam integration.** Adapter over `Engine::submit(target=Export)` → `ExportImage` per §5.8; backend provenance capture; single-retry-then-CPU device-lost behavior.
  *AC:* golden — a corpus raw + committed recipe renders through the real Engine to an `ExportImage` matching the E05 golden within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; injected device-lost produces a `cpu`-tagged result and the item still succeeds.
- **T08 — Resize engine.** All `SizeRule`s + `dont_enlarge` + PPI stamp math in linear f32 via `fast_image_resize` (Lanczos3).
  *AC:* dimension-math unit tests for every rule × portrait/landscape × upscale/downscale/dont_enlarge (≥30 cases, exact expected w×h); downscale golden vs reference resampler ≥ 0.99 SSIM; `SizeRule::None` is byte-identical passthrough.
- **T09 — Output color transform + quantization.** Wire `lightbox-color` per §5.7; row-parallel `apply_f32`; user-ICC load/validate; `quantize` with Floyd–Steinberg on 16→8.
  *AC:* synthetic color-patch grid through each standard space matches LCMS2 reference values within 1 LSB @16-bit; invalid/CMYK user ICC rejected at plan time; dither on/off changes only ≤1 LSB noise distribution (χ² sanity test); profile bytes surface for embedding.
- **T10 — Output sharpening.** Luma-weighted USM post-transform; 3×3 parameter table (medium × strength) with radius scaled by output size; `None` bypass.
  *AC:* goldens per table cell on a fixed resized image (ΔE tolerance); disabled == byte-identical; parameters documented in-code with the PhotoKit-heuristic derivation note.
- **T11 — Watermark: text rasterization.** cosmic-text + fontdb; weight/style/color/opacity/shadow; bundled OFL fallback font (surface-3 manifest entry lands with this task).
  *AC:* goldens for plain / styled / shadowed / CJK+emoji-fallback text rasters; missing font family falls back (warning, not failure); raster is premultiplied RGBA.
- **T12 — Watermark: graphic, layout, composite, presets.** PNG-with-alpha load; §5.11 layout (9 anchors, insets, proportional size, rotation); src-over composite into `OutputImage`; watermark-preset DAO wiring; the shared `rasterize` path consumed by both pipeline and dialog preview.
  *AC:* 9-anchor × rotation golden grid on a fixed frame; proportional size scales with long edge across three output sizes; opacity 0/50/100% goldens; preset CRUD round-trips.

### Phase E15.2 — encoders & containers

- **T13 — Encoder trait + fixture rig.** §5.4 trait, registry, and a shared test rig: fixed `OutputImage` fixtures → encode → decode-back (where a decoder exists in-tree) → PSNR/pixel-compare + ICC-presence assert. All later encoder tasks reuse this rig.
  *AC:* rig runs a dummy encoder end-to-end; registry dispatch covered.
- **T14 — JPEG encoder (mozjpeg).** Quality mapping, `ChromaMode`, progressive on, ICC APP2 embed; size-limit bisection per §5.4.
  *AC:* rig passes (decode-back via zune-jpeg, PSNR ≥ 40 dB @ q90); q90 vs q50 size ordering sane; `size_limit_kb` lands ≤ limit within ≤7 encodes or returns `SizeLimitUnreachable`; ICC present.
- **T15 — PNG + TIFF encoders.** `png` crate 8/16 + iCCP; `tiff` crate 8/16, None/LZW/Deflate, ICC tag 34675, PPI resolution tags.
  *AC:* rig passes lossless bit-exact round-trip at both depths; compression modes decode identically; ICC + resolution readable by our own readers.
- **T16 — JXL encoder (libjxl FFI).** Lossless + lossy distance, effort, ICC; 16-bit input path.
  *AC:* rig passes (lossless bit-exact; lossy distance=1.0 PSNR ≥ 40 dB); build wired into the CI matrix on all three OSes; SBOM surface-2 entry lands.
- **T17 — AVIF encoder (libavif).** 8/10-bit, quality/speed, ICC.
  *AC:* rig passes (q≥90 PSNR ≥ 38 dB); 10-bit output verified 10-bit; SBOM entry lands.
- **T18 — Original passthrough.** Byte copy + xxh3 verify against `asset.content_hash`; optional recipe sidecar `<name>.xmp` via `Recipe::to_xmp()` (E09); works for video assets.
  *AC:* hash equality post-copy; sidecar content equals E09's XMP projection byte-for-byte; a video asset under a rendered-format batch exports as passthrough with the planner warning from T05.
- **T19 — DNG export.** Raw→DNG via dnglab: compression option, optional embed-original, preview embed (medium/full from the render), current recipe embedded as XMP inside the DNG.
  *AC:* round-trip integration test — exported DNG re-imports through `lightbox-decode`/`lightbox-ingest`, decodes, and the embedded recipe reads back equal to the source recipe; non-raw source rejected at plan time (`DngFromNonRaw`).

### Phase E15.3 — metadata, write path, batch conveniences

- **T20 — Metadata policy engine.** `lightbox-meta::export::build_export_metadata` per §5.6 with the stated invariants; person-keyword and export-flag sourcing from the catalog keyword tables (E07 seam — read-only).
  *AC:* property tests: for any generated metadata set, `strip_gps` ⇒ zero location-bearing tags across EXIF/XMP/IPTC; `strip_person_keywords` ⇒ no person-flagged keyword anywhere; tier matrices (4 tiers × representative tag set) golden-tested; keyword flatten/hierarchy/none respected incl. export-flag exclusions.
- **T21 — Metadata embedding per container.** `embed_metadata` for JPEG/TIFF/PNG/JXL/AVIF; Orientation=1 always; PPI stamps where the container carries them.
  *AC:* each container's output re-read by our own readers reproduces the payload; orientation of a rotated source image is upright-pixels + Orientation=1; nightly cross-check against `exiftool` (subprocess, dev-dependency only — never shipped) on the fixture set.
- **T22 — Atomic write + disk-space failure path.** `<name>.<ext>.lbtmp` in the destination dir → fsync → rename; ENOSPC mid-write → per-item `DiskFull` failure with temp cleanup; launch-time temp sweep (T02 hook).
  *AC:* fault injection — process kill mid-encode leaves no partial final file; simulated ENOSPC (quota'd tmpfs on Linux CI / injected io error elsewhere) fails the item, session continues, no litter.
- **T23 — Re-add to catalog.** `ReaddPolicy` → `lightbox-ingest` add-in-place of the finished file; set `readded_asset_id`; idempotent on overwrite re-export (same path re-added ⇒ update, not duplicate asset).
  *AC:* integration — exported JPEG appears as a catalogued asset in the destination folder with provenance link; re-export with Overwrite doesn't duplicate the asset row.
- **T24 — Multi-preset batch + Export-with-Previous + post actions.** Planner grouping (one render per image, fan-out per preset — T06 pipeline already shaped for it); `Cmd::ExportWithPrevious` from latest `settings_doc`; `PostAction` reveal/open-with (per-platform: `open -R` / Explorer `/select` / `xdg-open` parent).
  *AC:* 2 presets × 10 images ⇒ 20 outputs with a render-count probe reading 10; DNG+JPEG preset pair ⇒ render count still 10 (DNG lane skips render); Export-with-Previous disabled state (no prior session) surfaced via `Query::LastExportSettings`; post-action command lines unit-tested per platform (spawn mocked).

### Phase E15.4 — external-editor round-trip

- **T25 — Handoff render.** `edit_in` per §5.9: `-Edit` naming + collision suffix, TIFF/PNG handoff through the T07–T09 pipeline (no sharpen/watermark), `external_edit` row, all three variants with raw/non-raw validation.
  *AC:* handoff file renders next to the original in the chosen space/depth; `-Edit-2` on collision; `Original`/`CopyWithoutAdjustments` on a raw source rejected with a typed error.
- **T26 — Editor launch + return watch + re-ingest.** Detached spawn; debounced quiescence watch; re-ingest add-in-place; `returned_asset_id` set; relaunch re-arm and mtime-based catch-up; orphan/forget transitions.
  *AC:* integration with a scripted fake editor (touches/rewrites the file twice, atomic-rename save) — exactly one re-ingest after quiescence; app-restart-while-waiting recovers and still ingests; deleting the handoff flips the row to `orphaned`.
- **T27 — Editor presets + prefs surface.** `external_editor` DAO + core commands; shell prefs pane listing editors (name/app/format); "Edit In →" menu on the selected image wired to `Cmd::EditIn`.
  *AC:* CRUD round-trip; menu action produces T25's behavior end-to-end on one platform in CI (macOS runner), manual checklist for the others.

### Phase E15.5 — shell UI, CLI, hardening

- **T28 — Export dialog: scaffold + destination/naming/format panels.** Panel-stack dialog in `lightbox-shell` bound to `ExportPlan` (live re-plan on change, debounced); destination picker, subfolder, template editor with live example name, format + per-format params with depth/format validation from T01.
  *AC:* dialog drives `Cmd::ExportPlan`/`ExportRun` end-to-end for a JPEG export; invalid combos disabled with reason; no direct SQL/engine types in the shell (headless-boundary lint per §2.3 seam 1).
- **T29 — Export dialog: sizing/sharpen/metadata/watermark panels + presets.** Remaining panels; preset save/load/delete; multi-preset checkbox list; Export-with-Previous menu + shortcut (keymap registry seam to E08).
  *AC:* preset round-trip through the dialog reproduces identical `ExportSettings` (CBOR-equal); multi-preset run from the dialog produces per-preset outputs.
- **T30 — Watermark editor dialog.** Live preview over the selected image's T1 preview using the shared `rasterize` path (§5.11); text and graphic modes; anchor grid, sliders; saves `watermark_preset`.
  *AC:* preview raster equals export-pipeline raster for identical spec (probe compares hashes at preview resolution); presets created here selectable in T29's panel.
- **T31 — Progress, conflicts, errors + `lightbox-cli export`.** Activity-center wiring (E06 model): per-session progress, cancel button, completion report with failed-item list; `Ask` conflict dialog (apply-to-all); CLI subcommand `lightbox-cli export --preset <name> --out <dir> <selection>` mapping the same commands for headless E2E.
  *AC:* CLI drives import → edit → export headless and exits nonzero on any failed item (the §8 E2E gate consumes this); conflict dialog resolves an Exists conflict into each of the four policies correctly.
- **T32 — Performance gates + golden wiring.** `criterion` micro-benches (resize, transform, sharpen, each encoder @ 24 MP); scenario harness additions: (a) throughput — 100-raw batch to sized JPEG asserting ≥ 20 raws/min on the CPU reference config and ~2× on GPU (§7), (b) **canvas starvation** — 3k-image export while the develop-loupe synthetic drag runs, asserting slider p95 < 100 ms throughout (M2 exit test); full-pipeline export goldens (raw corpus × settings matrix incl. every format × sRGB/ProPhoto × sharpen on/off × watermark on/off) registered in the PR-blocking golden suite, GPU-vs-CPU-backend within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB.
  *AC:* both scenario benches run nightly with budget assertions and issue-on-regression per §8; golden suite green on the 3-OS matrix.

**Milestone ordering note:** T01–T06 have no E05 runtime dependency (stub render lane) and can start the moment E09's `Recipe` types are stable; T07 is the first task requiring a working E05 export readback. If the E05 readback slips, phases E15.2–E15.3 proceed against synthetic `ExportImage` fixtures — only T07, T19 (preview embed), T25–T26 and the perf/golden halves of T32 hard-block on E05.

## 8. Test plan

Per the architecture's §8 strategy; every layer named below states its gate.

| Layer | Coverage (tasks) | Gate |
|---|---|---|
| **Unit** | Sizing math (T08), token grammar + sanitization (T03), collision policies (T04), quantize/dither (T09), sharpen params (T10), watermark layout (T12), size-limit bisection (T14), post-action command lines (T24), edit-in state machine (T25/26) | PR-blocking |
| **Property (`proptest`)** | `ExportSettings` CBOR round-trip identity (T01); metadata-policy invariants — GPS/person-keyword strip totality, tier monotonicity (higher tier ⊇ lower tier's fields) (T20); template parse never panics (T03) | PR-blocking |
| **Golden-image** | Full-pipeline exports: raw corpus × settings matrix, committed goldens, ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB; **GPU vs CPU backend parity** on identical settings; watermark/sharpen/anchor grids; per-process-version (recipes in the corpus span PVs) (T07, T10–T12, T32) | PR-blocking (fast subset), full nightly |
| **Integration** | Session lifecycle: cancel/cleanup/per-item isolation (T06, T22); DNG export→re-import→recipe-readback round-trip (T19); re-add-to-catalog provenance + overwrite idempotence (T23); external-editor fake-editor round-trip incl. relaunch recovery (T26); metadata embed re-read per container + nightly exiftool cross-check (T21) | PR-blocking (except nightly cross-check) |
| **Fault injection** | Kill mid-encode → no partial final files, catalog `integrity_check`-clean; ENOSPC per-item failure; crash-recovery sweep of `running` sessions (T02, T22) | PR-blocking (rides the existing kill-9 rig) |
| **E2E (headless)** | `lightbox-cli` import → edit → export; visual regression on the exported JPEG (§8 row) (T31) | PR-blocking (fast subset), full nightly |
| **Performance** | Encoder/resize criterion benches; ≥20 raws/min CPU / ~2× GPU throughput scenario; **3k-batch canvas-starvation scenario asserting interactive p95 < 100 ms** (T32) | Nightly; regression → tracked issue (§8) |
| **License surfaces** | Surface 1: mozjpeg/jxl/avif/cosmic-text crates in cargo-deny allowlist. Surface 2: libjxl/libavif/mozjpeg native artifacts in the binary SBOM. Surface 3: bundled OFL fallback font + synthesized output ICC profiles registered in the data manifest (no Adobe-authored ICC — see risk #1) | PR-blocking |
| **Cross-platform** | Build + unit + golden subset on macOS/Windows/Linux; post-action + editor-spawn platform variants unit-mocked, macOS exercised in CI | PR-blocking |

## 9. Performance budgets (inherited from §7, owned here)

| Budget | Target | Mechanism |
|---|---|---|
| Export throughput | ≥ 20 raws/min to sized JPEG single-thread CPU reference; ~2× on GPU | pipelined stages (§4.2); encoders parallel across items |
| Canvas starvation during export | **zero** — interactive p95 < 100 ms during a 3k-batch export | Foreground vs Interactive GPU time-slicing (E05/E06 contract), verified by the T32 scenario |
| Memory ceiling | bounded in-flight frames: `clamp(budget/frame_bytes, 1, 4)` per stage; a 100 MP batch never exceeds the configured export RAM budget | bounded channels (§4.2) |
| Catalog write pressure | ≤ 1 status txn / 250 ms / session | coalesced item-status writes (§6) |

## 10. Risks & open questions

Risks (with mitigations E15 commits to):

1. **Output ICC provenance & naming.** Bundling Adobe's "Adobe RGB (1998)" ICC file would violate constraint 4; we synthesize all standard profiles from published primaries via LCMS2 (§5.7) and register them in the surface-3 manifest. *Residual:* the profile **description string** ("compatible with Adobe RGB (1998)") has trademark optics — flagged for the routine CTO license review alongside the E02 dependency reviews. Mitigation exists either way (neutral name + docs).
2. **E05 export-readback seam slips.** T07/T19/T25/T32 hard-block on `RenderState::ExportReady`. Mitigation: the phase ordering in §7 keeps ~3.5 pw of E15 unblocked behind stubs; the seam is flagged to the E05 owner at epic kickoff as their M2 deliverable with E15 as first consumer.
3. **Gamma-space watermark/sharpen compositing.** Compositing and USM in output-referred space matches user expectation and LR behavior but is not physically linear; risk is perceptual (fringing on high-contrast watermark edges). Mitigation: golden + perceptual review pass in T12; if edges fringe, switch composite (only) to linear with a documented opacity-perception tradeoff — node-local change.
4. **libjxl/libavif build weight on the 3-OS CI matrix.** Both are C/C++ static builds with their own toolchain quirks. Mitigation: T16/T17 land the CI wiring as part of the task's AC (not a follow-up); prebuilt vendored artifacts go through the surface-2 SBOM like every native lib.
5. **External-editor return detection is inherently heuristic.** Editors save via atomic rename, temp files, or in-place rewrites. Mitigation: quiescence debounce + rename-aware watching + relaunch mtime catch-up (T26); worst case is a manual "re-scan" affordance, never data loss (the handoff file is on disk regardless).
6. **JPEG size-limit unreachable targets** (tiny limit + large dimensions). Bounded bisection returns a typed error carrying the smallest achievable size; the dialog surfaces it at plan time when predictable (heuristic estimate), at item level otherwise.
7. **Full-res CPU-fallback exports are minutes-scale** (§4.4 degraded contract). Not a defect — but progress UI must set expectations. Mitigation: per-item stage progress + `render_backend` surfaced in the report; no latency promise made on the CPU path.

Open questions (owner, needed-by):

1. **Stacking presentation for re-added exports / returned external edits** — provenance columns exist (§6); does v1 grid render them as stacks or adjacent items? *(E07/E08 owners; before T23/T26 UI polish — does not block the engine.)*
2. **Standard-profile description strings** (risk #1 naming) — *(CTO license review; before T09 lands.)*
3. **Default export RAM budget and whether it joins the E08 performance-preferences panel** — *(E08 owner; cosmetic, default = min(25% RAM, 6 GB) until then.)*
4. **DNG export of non-raw sources** (LR wraps them; we reject in v1) — confirm product stance. *(product/CTO; before M2 feature-freeze; current spec: reject with typed error.)*
5. **`lightbox-templates` crate home** (standalone crate vs module under a shared util crate) — *(coordinate with E04 owner at kickoff; interface is pinned either way.)*

## 11. Seams to neighboring epics (named, not designed)

| Epic | Seam | Direction |
|---|---|---|
| **E05 render** | `RenderTarget::Export` + `ExportReady(ExportImage)` readback; Foreground/Interactive time-slicing (§5.8) | E05 provides, E15 consumes; contract pinned here |
| **E06 jobs** | `Class::Foreground` job group, `CancelToken`, activity-center progress model | consume |
| **E09 edit state** | `Recipe` materialization per image; `Recipe::to_xmp()` for sidecar/DNG | consume |
| **E02 color** | `output_profile` / `OutputTransform` / user-ICC (§5.7); synthesized standard profiles | consume; gaps raised to E02 owner |
| **E04 ingest** | shared `lightbox-templates` token engine (§5.5); add-in-place re-ingest for re-add + returned edits | shared / consume |
| **E07 DAM** | keyword export-flags + person flags read for metadata policy; stacking presentation (open q. #1) | read-only consume / handoff |
| **E08 library UI** | keymap registry entries (export shortcuts); performance-prefs slot for export RAM budget; stack display | register into / handoff |
| **E14 AI** | person-keyword flag semantics for `strip_person_keywords` once face→keyword mapping lands (M3) — policy engine keys off the keyword-table flag, so no E15 change needed | forward-compatible |
| **E16 hardening** | `lightbox-cli export` consumed by E16's E2E/packaging gates; SBOM/data-manifest entries feed the release gates | provide |
| **v1.x publish services** | `export_session`/`export_item` column shape is the substrate; no v1 code | design headroom only |

## 12. Definition of done

E15 is done when **all** of the following hold on the 3-OS CI matrix:

1. Every format in scope (JPEG w/ size-limit, PNG 8/16, TIFF 8/16×3 compressions, JXL lossless+lossy, AVIF 8/10, DNG, Original+sidecar) exports through the full pipeline with ICC embedded and metadata policy applied, verified by the golden + integration suites (PR-blocking, green).
2. The **M2 exit test passes**: a 3k-image batch exports while the develop canvas sustains p95 < 100 ms slider-to-screen, and throughput meets ≥20 raws/min CPU / ~2× GPU (nightly scenario green at epic close).
3. GPU-vs-CPU backend export parity holds within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on the golden matrix; every exported file's `render_backend` is recorded.
4. Cancel, kill-mid-encode, and ENOSPC fault-injection tests pass: no partial final files, no temp litter, catalog `integrity_check`-clean, per-item failure isolation.
5. Export presets, multi-preset batch (single render per image, probe-verified), Export-with-Previous, watermark presets, and metadata privacy switches work end-to-end from the dialog and from `lightbox-cli`; the CLI path is wired into the §8 E2E gate.
6. External-editor round-trip: handoff → fake-editor save → auto re-catalog with provenance passes in CI, including app-relaunch recovery.
7. DNG export round-trips: re-import decodes and the embedded recipe reads back equal.
8. License surfaces: new crates pass cargo-deny; libjxl/libavif/mozjpeg in the binary SBOM; bundled fallback font + synthesized ICC profiles in the surface-3 data manifest; **zero Adobe-authored assets** introduced.
9. Open questions #2 and #4 resolved (CTO/product) or explicitly deferred with the spec updated; seam contracts (§5.7, §5.8) acknowledged by the E02/E05 owners.
10. No modifications outside the crates/modules listed in §3; the shell talks to export only through the §5.10 façade (headless boundary preserved).
