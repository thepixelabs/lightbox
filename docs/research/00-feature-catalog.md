# Lightbox Feature Catalog
**Product synthesis from 10 domain research tracks — v1.0, July 2026**

---

## 1. Executive Summary

Adobe Lightroom is two products sharing one idea: a **non-destructive, parametric photo workflow**. Originals never change; every rating, keyword, and slider move is a database record (mirrored to XMP), replayed against the raw file on each render. On that foundation sit four load-bearing pillars:

1. **A catalog-centric DAM** — a single SQLite database indexing photos by reference, with folders (physical), collections/smart collections (logical), ratings/flags/labels, hierarchical keywords, a faceted filter bar, and a multi-tier preview cache that makes 100k+ photo libraries browsable.
2. **A raw develop engine** — 32-bit float, scene-linear, wide-gamut pipeline: decode → demosaic → camera profile (DCP) → global tone/color tools (basic panel, curves, HSL, color grading) → detail (sharpen/NR) → optics/geometry → effects, all versioned so old edits render identically forever.
3. **Local adjustments and retouching** — a component-based masking system (brush, gradients, luminance/color range, AI segmentations) where each mask drives a near-full copy of the develop sliders, plus non-destructive heal/clone/remove.
4. **Output** — a panelized export dialog (formats, sizing, sharpening, metadata filtering, watermarking, presets), external-editor round-trip, and publish/print surfaces.

Since ~2021 Adobe's differentiation has shifted to **ML**: one-click subject/sky/people masks, AI denoise, semantic search, face clustering, assisted culling, and generative remove. The critical finding across all research tracks: **nearly every on-device AI feature has a credible open-source equivalent** (SAM2, BiRefNet, NAFNet, OpenCLIP, LaMa, Depth Anything), and the open raw editors (darktable, RawTherapee, vkdt) already match or exceed Adobe's raw rendering quality. What no FOSS product has ever combined is *all four pillars plus the ML layer in one polished, fast application* — that is Lightbox's thesis.

**A credible v1 must ship:** catalog + import + culling + organization; a quality raw pipeline with the full global toolset; the masking/retouch core including AI subject/sky selection; a complete export system; XMP interop (including one-way Lightroom migration); and performance architecture (GPU node graph, preview tiers, background job system) built in from day one — it cannot be retrofitted.

---

## 2. Prioritized Feature Matrix

Priorities are MoSCoW (**M**ust / **S**hould / **C**ould / **W**on't-for-v1). Complexity: Low / Med / High / VHigh. Conflicts between researchers resolved per judgment noted inline.

### 2.1 Catalog & Library (DAM core)

| Feature | Priority | Complexity | Notes / building blocks |
|---|---|---|---|
| SQLite catalog (single-file, WAL, non-destructive writes, 100k+ scale) | M | High | Lightroom itself uses SQLite; FTS5 + sqlite-vec ready for search/AI |
| Folders panel mirroring filesystem, sync-folder reconciliation | M | Med | fs watchers + atomic transactions |
| Collections & collection sets (virtual albums, custom order) | M | Low | Join tables |
| Smart collections (rule-tree saved queries) | M | Med | Rule-AST → SQL |
| Star ratings, pick/reject flags, color labels (XMP round-trip) | M | Low | One-key shortcuts, auto-advance |
| Hierarchical keywords (synonyms, export flags, autocomplete) | M | Med | dc:subject + lr:hierarchicalSubject |
| EXIF/IPTC viewer + editor, metadata presets, sync metadata | M | Med | BSD XMP Toolkit / Rust EXIF crates (see licensing risk §5) |
| XMP sidecar read/write incl. Lightroom `crs:` compat | M | Med | The migration lever — treat as first-class |
| Filter bar: free text + attribute + faceted metadata columns | M | High | FTS5; saved filter presets |
| Grid view (virtualized, badges, sorts) + Loupe (zoom/pan/info) | M | Med | Demand-driven preview loading, never blocks |
| Multi-tier preview pyramid (embedded / standard / 1:1) + retention policy | M | High | libvips; embedded-JPEG culling **on by default** (see §4) |
| Missing-file relink (single, folder-tree, find-all-missing) | M | Med | Hash + heuristic matching |
| Virtual copies | M | Med | Falls out of parametric data model |
| Skip-duplicates at import; remove-vs-delete semantics; catalog backup/optimize/integrity | M | Low–Med | SQLite backup API, OS trash |
| Compare & Survey views; Quick/target collection; filmstrip | S | Low–Med | Culling muscle-memory features |
| Stacking (manual + auto by capture gap) | S | Med | |
| Capture-time edit; batch rename templates | S | Low | Shared token-template engine with import/export |
| Smart previews (offline proxy editing, ~2560px) | S | High | Lossy DNG via DNG SDK, or JXL proxies |
| Perceptual-hash duplicate detection view | S | Med | pHash + BK-tree |
| Video asset management (import, scrub, poster, rough trim) | S→C | High | FFmpeg; catalog-only in v1, trim can slip |
| Secondary display window | S | High | |
| Quick Develop (relative batch adjustments), Painter tool, People view, GPS/Map, multi-catalog merge | C | Med–VHigh | Defer; People view rides on face clustering (§2.7) |

### 2.2 Import & Ingest

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Import dialog: copy / move / add-in-place / convert-to-DNG | M | High | Thumbnail grid, per-photo checkboxes |
| Broad format ingest (raw ×hundreds, JPEG, TIFF, PNG, HEIC, PSD, video) | M | High | LibRaw + rawler; libheif; FFmpeg probe |
| Date-based destination templates, token file renaming | M | Med | |
| Apply-on-import: develop preset, metadata preset, keywords | M | Low | |
| Preview-build choice at import (embedded → 1:1) | M(embedded) / S(rest) | Med | Embedded-first is a headline speed win |
| Parallel import pipeline + second-copy backup | S | High / Low | Checksummed copy |
| Import presets, card-insert auto-launch, watched auto-import folder | S | Low | Watched folder also bridges third-party tethering |
| Import from Lightroom catalog (.lrcat is SQLite) | S | High | Major adoption lever; one-way, best-effort edit translation |
| Tethered capture | C | VHigh | libgphoto2; vendor SDKs as optional plugins later |

### 2.3 Raw Pipeline & Color Management (engine)

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Raw decode (LibRaw/rawler dual path), linearization, black/white levels | M | High | rawler (MIT) primary in a permissive core; LibRaw (LGPL, dyn-link) for coverage |
| High-quality Bayer demosaic | M | VHigh | Best algorithms (AMaZE/RCD/LMMSE) are GPL — **clean-room from papers**, GPU ports |
| 32-bit float scene-linear pipeline, fixed wide-gamut working space | M | High | ProPhoto-class primaries; "Melissa RGB"-style display companion |
| DCP camera profile engine (dual-illuminant matrices, HSV LUTs, tone curve) | M | High | Adobe DNG SDK; dcamprof as external tool |
| DNG read (full spec incl. opcodes: WarpRectilinear, GainMap, bad pixels) | M | High | Embedded mirrorless corrections are non-optional |
| White balance model (as-shot, presets, Kelvin+tint, eyedropper) | M | Med | |
| Non-raw editing through same pipeline (JPEG/TIFF/HEIC/PNG) | M | Med | |
| Versioned rendering engine (process versions) from day one | M | High | Architecture requirement even with one version shipped |
| Display color management (per-monitor ICC) + export color management | M | Med / Low | LCMS2 (MIT) |
| **GPU node-graph engine (vkdt pattern): DAG of compute nodes, cached intermediates, ROI evaluation, CPU fallback** | M | VHigh | The core engineering bet; wgpu (MIT). Resolves the "GPU: must vs could" conflict — the *architecture* is Must, full acceleration tiers phase in |
| X-Trans (Markesteijn-class) demosaic, highlight reconstruction | S | High / Med | X-Trans can slip to v1.x if schedule forces it |
| DNG writing/conversion; camera-matching profiles; OCIO integration; catalog↔XMP sync controls | S | Med–High | dnglab (MIT) |
| Soft proofing (ICC proof transforms, gamut warnings, proof copies) | S | High | |
| HDR merge to float DNG | S→C | High | HDRMerge prior art |
| HDR editing mode + gain-map export (libultrahdr/AVIF/JXL) | C | VHigh / High | Fast-moving area; design pipeline headroom now, ship later |

### 2.4 Develop — Global Adjustments

One uniform "edit state = serializable, versioned parameter set" buys presets, copy/paste, sync, snapshots, and history nearly for free.

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Histogram w/ clipping indicators & draggable regions | M | Med | |
| Basic panel: exposure, contrast, highlights/shadows (halo-free local tone mapping), whites/blacks w/ clip preview | M | Low–High | Highlight/shadow recovery is the quality-defining hard part (guided filter / local Laplacian) |
| Vibrance (skin-protected) + saturation | M | Med / Low | |
| Tone curve: point + per-RGB-channel | M | Med | Parametric-region curve: S |
| HSL / Color Mixer (8 bands, smooth overlap weighting) | M | High | |
| Camera profiles as base rendering (Adobe-Color-analogue default family) | M | High | |
| Presets system (partial presets, groups, import/export, hover preview, **reads Lightroom .xmp presets**) | M | Med | |
| History panel (persistent) + Before/After views | M | Med | |
| Copy/paste settings (group checklist), Sync + Auto Sync | M | Med | |
| Snapshots (named versions) | M* | Low | *Two tracks rated Should; promoted to Must — trivial cost on the parametric model, high user value |
| Presence: clarity (Med), texture (High), dehaze (High) | S | Med–High | Wavelet/guided-filter work; dark-channel prior for dehaze |
| Color grading 3-way wheels (+ legacy split-toning XMP mapping) | S | High | |
| B&W conversion w/ channel mix; parametric curve; creative LUT profiles w/ amount; batch/Quick Develop; Auto tone (heuristic first, ML later) | S | Med–High | HaldCLUT packs = near-free "looks" |
| TAT, Point Color, preset amount, ISO-adaptive presets, reference view, process-version migration UI | C | Med–High | |

### 2.5 Detail, Optics, Geometry, Effects

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Capture sharpening (Amount/Radius) | M | Med | Detail + edge-Masking sliders: S |
| Luminance + chroma noise reduction (classical, edge-preserving) | M | High / Med | Baseline before any AI denoise |
| Profile-based lens corrections (lensfun + LCP parsing + EXIF auto-match) | M | Med | Plus embedded-opcode handling (§2.3) |
| One-click chromatic aberration removal | M | Med | |
| Crop tool (ratios, angle, level tool, auto-straighten), rotate/flip | M | Med / Low | Composition overlays: C |
| Upright auto perspective (Level/Vertical/Auto/Full) | S | High | LSD lines + homography solve; Guided Upright: C |
| Manual transform sliders, distortion, defringe, vignette correction, constrain crop | S | Low–Med | |
| Post-crop vignette (3 blend styles), film grain | S | Med | |
| Calibration panel (camera primaries) | S | Med | |
| Alt-key diagnostic previews | C | Low | |

### 2.6 Masking, Local Adjustments, Retouching

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Unified mask model: named masks of composable components, add/subtract/intersect | M | High | The architectural centerpiece of local editing |
| Brush (size/feather/flow/density, erase, tablet pressure) | M | Med | Auto Mask edge-snapping: S |
| Linear + radial gradients w/ on-canvas gizmos & pins | M | Low–Med | |
| Luminance range + color range masks (samplers, falloff) | M | Med | |
| Full per-mask adjustment set (tone, color, presence, detail) | M | VHigh | Blends into global pipeline; the biggest pixel-engine lift after demosaic |
| Mask management (rename/duplicate/invert/hide) + overlay modes | M | Med | |
| **Select Subject / Select Sky (AI)** | M | High | SAM2/BiRefNet, SegFormer — headline v1 feature (see §4) |
| Select Background (complement) | S* | Low | *Near-free once subject/sky exist; one track said Must — Should, ships trivially with them |
| Select Objects (box/scribble-prompted SAM) | S | High | |
| Non-destructive heal/clone spot system (editable objects, movable source) | M | High | |
| Heal mode (color-matched blend, Poisson/seamless clone) | M | High | Clone mode: M / Med |
| Content-aware Remove (local, PatchMatch/LaMa) | S | High | Conflict resolved: clone/heal are the Must; synthesized fill is Should — it's the expected 2026 baseline but v1 can ship weeks behind core heal |
| Red-eye correction; Visualize Spots; per-mask Amount; AI mask recompute/staleness; mask copy/sync & adaptive presets | S | Low–Med | |
| Select People w/ body-part parsing; Select Landscape; depth masks; per-mask curves/Point Color; Generative Remove (diffusion); pet-eye | C | Med–VHigh | Body-part parsing & diffusion fill are the two most expensive items in this domain |

### 2.7 AI/ML Platform & Library Intelligence

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| **ONNX-based on-device inference layer** (CoreML/DirectML/CUDA/ROCm EPs, CPU fallback, downloadable model packs, VRAM gating) | M | Med | The single most leveraged investment — every AI feature rides on it |
| Masking model stack (subject/sky/objects) | M | High | Counted in §2.6; listed here as the models |
| AI Denoise (raw-domain neural NR) | S | VHigh | Conflict resolved: one track Must, two Could → **Should**. Ship classical NR in v1.0; neural denoise is the flagship v1.x feature. Raw-domain (pre-demosaic) weights are scarce — may launch RGB-domain first |
| Semantic search (OpenCLIP/SigLIP + sqlite-vec, fully local) | S | Med | Proven on consumer hardware by Immich/PhotoPrism |
| Auto-tagging (RAM++/zero-shot CLIP) | S | Med | Hidden tags powering search facets |
| Face detection + People clustering (YuNet/SFace — Apache-licensed; **avoid InsightFace weights**, non-commercial) | S | High | Pausable background service |
| Super Resolution (Real-ESRGAN/SwinIR) | S→C | Med | Mature, cheap win when the runtime exists |
| Auto settings (ML slider prediction), adaptive presets (mask-aware) | S→C | Med–High | Heuristic auto first |
| Assisted culling, Best Photos, Lens Blur (depth bokeh), adaptive profiles, quick actions, C2PA Content Credentials (c2pa-rs), AI-edit tracking, third-party model plugin API | C | Med–High | C2PA is unusually cheap (Adobe's own impl is open source) |
| Raw Details (ML demosaic), Reflection Removal | W | VHigh | Open weights scarce / research-grade |

### 2.8 Export & Output

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Export dialog: destination, collision handling, re-import to catalog | M | Med | Job queue w/ progress & cancel |
| File settings: JPEG (quality/size-limit), PNG, TIFF 8/16, DNG, original passthrough; sRGB/AdobeRGB/ProPhoto/P3 + any ICC; bit depth | M | High | JXL/AVIF: S (libjxl/libavif, cheap adds) |
| Sizing (long edge/dimensions/MP/%, don't-enlarge, PPI) | M | Med | Lanczos via libvips |
| Metadata filtering & privacy (strip GPS, person keywords, tiers) | M | Low | |
| Watermark editor (text + graphic, anchors, opacity, presets) | M | Med | |
| Export presets, multi-preset batch, Export-with-Previous | M | Low | |
| Output sharpening (screen/matte/glossy × strength) | S | Med | |
| External-editor round-trip (16-bit TIFF handoff → GIMP/Krita/Photoshop, auto re-catalog stacked) | S* | Med | *One track said Must; Should-ship-in-v1 — small, high-leverage |
| Post-export actions; video export/passthrough; hard-drive publish target | S | Low–Med | |
| Publish services framework (stateful new/modified/published sync) | S→C | High | Framework Should, online services (Flickr etc.) Could/plugin |
| Print: contact sheets, single-image, ICC-managed, print-to-JPEG | C | High | Conspicuous for print shooters but deferrable |
| Map/geotag module, GPX matching, email, cloud share links | C | Med–VHigh | |

### 2.9 Performance Architecture & UX Conventions

| Feature | Priority | Complexity | Notes |
|---|---|---|---|
| Raw decode cache (relocatable, size-capped, content-hash keyed) | M | Med | ACR-cache analogue |
| Demand-driven preview generation, virtualized grid, progressive Develop loading | M | High / Med | UI never blanks, never blocks |
| Unified background-task activity center (cancel/pause per task; AI/analysis pausable) | M | Med | Prevents Lightroom's "background work steals my session" complaint |
| One-key culling grammar (P/X/U, 1–5, 6–9, auto-advance) | M | Low | De facto industry standard |
| **Remappable shortcuts** (declarative keymap registry) + cheat-sheet overlays | M | Med | Lightroom has never shipped remapping — cheap differentiator |
| Two-module workspace (Library + Develop) with LR-style mode switching | M | Med | |
| Collapsible panels w/ Solo mode, Tab chrome toggles, filmstrip | M | Med | |
| Performance preferences panel (GPU mode + hardware readout, cache knobs) | M | Low | |
| Exit-time catalog backup w/ integrity test; corruption repair & restore flow; versioned schema migrations | M / S | Med | Catalog corruption is the #1 trust-killer among competitors |
| Catalog optimize command; preview retention caps | S | Low | |
| Secondary display; screen modes / Lights Out; identity plate | S / C | High / Low | |
| UI shell: Qt 6 (LGPL, dyn-link) or Tauri chrome around a wgpu-rendered canvas | M | High | Color-managed canvas is non-negotiable |
| Lua/WASM plugin SDK (export destinations, metadata, menu commands) | S→C | VHigh | Design the internal API surface for it in v1; ship SDK v1.x |

---

## 3. Out of Scope for v1

Explicitly deferred — the data model should not preclude them, but no v1 engineering:

- **Book, Slideshow, and Web modules** — legacy output surfaces; static-gallery and PDF tools cover the need externally.
- **Cloud sync / hosted originals / multi-device parity** — no sync service, no quota storage, no Classic-style sync bridge. *However:* store edits as ordered, serializable operations from day one so a future self-hosted sync (Immich-style) is possible.
- **Mobile apps, web editor, TV client, in-app camera, camera-roll backup.**
- **Social/community layer** — edit replays, remixes, follows, preset recommendations, notifications.
- **Generative AI at Firefly quality** — diffusion Generative Remove is Could-at-best; Reflection Removal and ML demosaic (Raw Details) are Won't.
- **Adobe Lua plugin binary compatibility** — running existing Lightroom plugins unmodified would mean reimplementing Adobe's `Lr*` namespaces; Lightbox ships its own plugin API instead.
- **Invite-based collaboration, comments/favorites, connected print labs, pooled-storage identity.**
- **Full video grading**; v1 video is catalog + playback (+ trim if schedule allows).
- **Adaptive AI profiles, AI-assisted culling as a shipped feature** (models can be prototyped behind flags).
- **Picture package / custom package printing; Photoshop smart-object/Photomerge handoffs** (map to Hugin/GIMP scripts later).

---

## 4. Signature Differentiators

Where open building blocks let Lightbox match or beat Lightroom, not just chase it:

1. **No subscription, no login, local-first forever.** Every feature — including all AI — runs on-device with zero network dependency. Adobe's own "Local mode" signals demand for exactly this; Lightbox makes it the whole product.
2. **Local AI that matches the cloud tier.** SAM2/BiRefNet masking, OpenCLIP semantic search ("dog on beach" with zero keywords), face clustering, auto-tagging, and neural denoise/upscale all run on a mid-range GPU or Apple Silicon via one ONNX runtime. Immich and PhotoPrism have proven the pattern; no desktop raw editor has shipped it. **This is the headline.**
3. **A faster engine than Adobe's.** vkdt demonstrates that a GPU-resident node-graph DAG delivers order-of-magnitude interactivity over fixed-order CPU pipelines — directly attacking Lightroom's most-cited complaints (mask-heavy Develop lag, export regressions, slow module switching). Ship embedded-preview culling *by default* to beat Lightroom's import-to-culling latency out of the box.
4. **The DAM + raw-engine combination no competitor has.** darktable/RawTherapee win on pixels but lose on library UX; digiKam/Immich win on DAM/ML but have no real editor. Uniting them is the product.
5. **Migration as a feature.** .lrcat is plain SQLite and Lightroom edits are documented `crs:` XMP — a guided importer for photos, collections, ratings, keywords, and best-effort develop settings gives switchers a path no commercial rival offers.
6. **User-respecting fixes Adobe never shipped:** remappable keyboard shortcuts, transparent cache/performance controls, crash-proof GPU fallback, catalog that tolerates relocation, pausable background AI.
7. **Open ecosystem economics:** free HaldCLUT film-simulation packs, community presets, a documented model-plugin API (generalizing Adobe's Topaz integration), and optional C2PA provenance via Adobe's own open-source c2pa-rs.

---

## 5. Key Risks

1. **GPL contamination is the existential licensing risk.** The best demosaicers (AMaZE, RCD, LMMSE, Markesteijn), darktable/RawTherapee code, exiv2, ExifTool-as-library, and dcamprof are GPL/AGPL. Mitigation: standing policy — MIT/BSD/Apache linked freely; LGPL (LibRaw, lensfun, libvips, Qt, libgphoto2) dynamic-link only; GPL never in-process (subprocess or clean-room from papers); per-model weight license review (InsightFace weights are non-commercial). Enforce with cargo-deny/REUSE in CI.
2. **Clean-room demosaic quality.** Reimplementing AMaZE/RCD-class algorithms from papers, on GPU, is the single hardest and most quality-defining engineering item. A weak demosaic sinks the product's credibility with exactly the pixel-peeping audience it courts. Budget it as a project unto itself; LibRaw's LGPL-tier algorithms are the interim floor.
3. **Camera coverage treadmill.** New bodies, embedded correction opcodes, X-Trans, and per-camera color (DCP-equivalent profiles) require permanent ongoing effort. rawler's coverage is narrower than LibRaw's; per-camera color without Adobe's profile library will be a visible gap for some bodies.
4. **The GPU node-graph bet.** A vkdt-style engine is the performance moat but also VHigh complexity across three GPU APIs (via wgpu) with a bit-faithful CPU fallback. Driver-related crashes are Lightroom's own recurring failure; getting graceful degradation right is hard and must be designed, not patched.
5. **Scope gravity.** The Must set alone is a multi-year product. The catalog says no to Book/Web/cloud/social, but the Should column (smart previews, publish services, soft proofing, tethering pull) will pressure v1. Discipline: ship Library + Develop + Export excellently before anything else.
6. **Catalog trust.** Corruption and data loss destroyed user loyalty for ON1/Luminar and haunts digiKam at scale. WAL mode, exit-time verified backups, integrity checks, and a tested restore path are Must-tier, not polish.
7. **AI model lifecycle.** Model packs need versioning, download management, hardware gating (8 GB VRAM-class recommendations), and reproducibility (an AI mask must re-render identically years later — tie model versions into the process-version system).
8. **Raw-domain neural denoise gap.** Open pretrained weights for pre-demosaic denoising barely exist; matching DeepPRIME/Adobe Denoise may require training on paired raw data — real cost, uncertain timeline. Position classical NR as solid and neural NR as a v1.x flagship rather than promising it for launch.
9. **Interop fidelity expectations.** "Imports your Lightroom catalog" will be read as "renders my edits identically," which is impossible (proprietary pipeline). Set expectations explicitly: organization migrates losslessly, develop settings migrate approximately.
