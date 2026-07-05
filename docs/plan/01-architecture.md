# Lightbox — v2 System Architecture

_Author: system-architect. Input: `00-mandate.md` (**v2.1**), `00-feature-catalog.md`, and the 10 domain research reports. This document is decision-complete: epic planners spec implementation from it without re-litigating stack, seams, data model, or budgets. Where a choice is reversible, the reversal trigger is named; where irreversible, it is flagged for CTO/CEO sign-off._

---

> ## v2.0 SCOPE REVISION (2026-07-05, system-architect, tracking mandate v2.0)
>
> **This is a SCOPE revision, not a technology revision.** The stack (§1), engine design (§4), data-model *mechanics* (§3), license policy (§1.6–§1.7/§8), and testing strategy (§8) all **stand** — every ADR in §1 survives unchanged. What changed is the **product**: Lightbox is now an **editing-first raw developer**, and the entire DAM/library ambition (managed catalog, grid library view, folder panel, collections, keywords, ratings/culling, faces, semantic search, watched folders) is **cut**. The concrete deltas:
>
> - **Product thesis** reframed to editing-first (§0).
> - **Entry model** is drag-and-drop + OS open dialog → editor with a **session filmstrip**; no library. New **editor-shell contract** in **§2.4**.
> - **Data model** rescoped: `lightbox-catalog` is now an **internal edit store + cache index**; the DAM tables go **keep-dormant** (retained in schema, never populated) — **§3.1**.
> - **E01 disposition audit** (what built code is retained / repurposed / retired) in **§10.0**.
> - **Epic set** re-sequenced **develop-first**: milestones and the epic table are revised in **§9** and **§10**; retired/rescoped epics are marked there.
> - **Effort** recomputed down: v1.x was ~130–155 pw; v2.0 is **~105–120 pw** (§10) — the pixel-engine stream is unchanged; the savings are entirely the cut DAM scope.
> - **DAM-scale budgets and risks** (import 10k, filter 100k, cull-at-keyboard, catalog-interactive-at-100k, ANN index) are **superseded** — see the v2.0 notes in §7 and §11. The **crash-safety** bar is retained, rescoped to the edit store.
>
> Sections not touched by this banner retain their v1.x text as the enduring technical record; where a v1.x paragraph asserts a DAM-scale claim, the v2.0 note in its section governs.

---

> ## v2.1 SCOPE STRENGTHENING (2026-07-05, system-architect, tracking mandate v2.1)
>
> **Two Core-scope additions, no technology or seam change.** The stack, engine, data model, and CI gates all **stand**. (1) **Complete raw parameter surface** — opening a raw exposes *everything the pipeline can vary*, not a curated subset; the raw-only parameter groups are enumerated and bound to their delivering epics in the **§2.4** develop-surface contract (a panel-population guarantee, not new pipeline work — the node-graph already varies these parameters). (2) **AI Looks — image-adaptive cinematic grading** — a new leaf epic **E17** (`lightbox-looks`, §2.1/§2.2/§10) that analyzes the opened image's palette/tone and proposes varied cinematic grades *fitted to its actual colors*, each materializing as an ordinary fully-editable develop recipe merged via E09 — never a baked filter. Deltas: differentiators (§0), crate map (§2.1/§2.2), §2.4 raw-surface binding, epic table + effort + streams (§10.1-preamble), E17 decomposition (§10.1), milestone M3 (§9). E17 rides the E09 recipe + E10 color foundation and the optional E13 inference host — it adds **~6–8 pw off the pixel-engine critical path**, so the wall-clock driver is unchanged.

---

## 0. Architectural thesis (the one-paragraph version)

Lightbox is the **best local-first, AI-assisted, GPU-fast raw editor** — the FOSS editor that pairs reference-grade raw color with local AI enhancement and a modern interactive engine, a combination no existing open-source editor offers (darktable/RawTherapee have the color but no local AI and a slower fixed-order pipeline; nothing FOSS pairs all three). Two v2.1-mandated capabilities sharpen the differentiation further: a **complete raw develop parameter surface** (every stage the pipeline can vary is a live control on a raw, not a curated subset — §2.4) and **AI Looks — image-adaptive cinematic grading** (E17): a locally-run engine that analyzes the *opened image's own* palette and tonal distribution and proposes varied cinematic grades fitted to its actual colors, every look materializing as an ordinary, fully-editable develop recipe layered on the current edit — **a headline feature no competitor ships locally** (cloud tools bake opaque filters; darktable/RawTherapee ship static preset LUTs that ignore the image's content; nothing FOSS proposes image-fitted, fully-editable cinematic grades offline). Architecturally it is a **headless Rust core** (decode + render + edit-state + jobs + edit-store) wrapped in a **thin, replaceable editor shell** that shares one GPU device with the render engine. The core's spine is a **GPU compute node-graph** (the vkdt lesson) that renders a **versioned, serializable edit recipe** against an untouched original. Files enter by **drag-and-drop or the OS open dialog** and open directly in the editor with a **session filmstrip** — there is no library. The **edit store (`lightbox-catalog`) is the single source of truth** for a file's recipe, keyed by content-hash; XMP sidecars are an opt-in projection, never a second owner. Everything expensive is a **cancellable job**; the UI thread never blocks and a `kill -9` never corrupts the edit store. AI runs on a **crash-isolated out-of-process ONNX inference host**. The whole dependency graph is **permissively licensed by construction** — GPL never links in-process, enforced in CI across **three** surfaces: the Rust crate graph (cargo-deny), the native/FFI binary surface (a vendored-binary SBOM with per-artifact license + build-flag audit), **and the bundled content/data manifest** (camera profiles, look family, LUT packs, lensfun data — §1.7/§8), because the LGPL/GPL exposure concentrates in the dynamically-linked C libraries and their transitive codecs (invisible to the crate graph) **and** the no-Adobe-assets exposure concentrates in bundled color/profile *data* whose open format hides proprietary content (invisible to both binary surfaces).

The three bets, in priority order: (1) the edit store is crash-proof (`kill -9`-safe, transactional); (2) the node-graph render engine hits <100 ms slider-to-screen; (3) local AI matches the cloud tier offline. Everything else is assembly of mature building blocks. **(v2.0: the former bet "interactive at 100k+ assets" is retired with the DAM; crash-safety is the surviving invariant, rescoped to the edit store.)**

---

## 1. Tech stack selection (ADR-style)

Each decision names the failure mode it prevents, weighs ≥2 alternatives, states the license + linkage mode, and gives the reversal trigger.

### 1.1 Core language — **Rust** (edition 2021, MSRV pinned)

**Problem it solves:** the app decodes hundreds of adversarial binary formats (proprietary raws) and must never corrupt a 100k-asset catalog on crash. Memory-safety bugs in C/C++ raw decoders are the classic RCE/corruption surface. Rust removes that class outright and gives fearless concurrency for the job system.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **Rust** | Memory-safe; `rawler`, `wgpu`, `ort`, `rusqlite`, `petgraph`, `tokio` are all first-class and MIT/Apache; one language end-to-end | Weaker native-widget UI story; some C libs need FFI (LibRaw, LCMS2, ffmpeg) | **Chosen** |
| C++ + Qt | Mature imaging ecosystem (Krita/digiKam/Capture One); direct LibRaw/OpenCV/LCMS2 | Memory-unsafe decode surface; manual concurrency; larger corruption blast radius | Lost: safety on the untrusted-decode path is load-bearing for the crash-safe mandate |
| Rust core + C++ Qt UI (CXX-Qt) | Best-of-both | Two languages, two build systems, two GPU contexts, brittle FFI boundary at the hottest seam | Lost: doubles cognitive load; the zero-copy canvas seam becomes the hardest part of the app |

**License:** Rust toolchain (MIT/Apache-2.0). **Reversal trigger:** none realistic — this is a foundational, effectively one-way door. Flagged for CTO sign-off as an irreversible commitment.

### 1.2 UI shell — **egui / eframe**, with the live editor canvas as a native wgpu render pass

**Problem it solves:** the mandate calls a color-managed live canvas "non-negotiable." The hardest UI problem in a raw editor is getting the GPU-rendered pipeline output onto screen **zero-copy, at 60 fps, composited with the UI chrome**. Any design with two GPU contexts (one for the engine, one for the toolkit) pays a per-frame texture-handoff tax and a cross-API synchronization bug budget.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **egui/eframe** (immediate-mode, Rust, MIT/Apache) | Shares **one wgpu device** with the engine — the canvas is just another render pass in the same frame, zero-copy by construction; pure Rust; AccessKit a11y; multi-viewport (secondary display); trivial custom widgets | Immediate-mode means we build docking/virtualized-grid/gizmo widgets ourselves; less "native" feel | **Chosen** |
| Qt 6 (LGPL-3, dyn-link) | Deepest pro-imaging widget set, native docking, proven | Two-language FFI; second GPU context; embedding a wgpu surface under QRhi is the fiddliest cross-platform code in the app | Lost: the canvas seam cost outweighs widget savings |
| Tauri (web UI, MIT) | Fast UI iteration, web ecosystem | Live wgpu canvas must be an overlaid native child surface under the webview — platform-specific, fights the compositor; JS↔Rust boundary on hot paths | Lost: same two-context tax, plus a JS runtime |

**Design guardrail that makes this reversible:** the **core is headless** (`lightbox-core` exposes a command/query API with no UI types). The shell is a thin consumer. **Reversal trigger:** if accessibility, native platform integration, or third-party-widget needs exceed egui's ceiling, we swap `lightbox-shell` for a Qt or Slint shell against the same core API — a bounded, single-crate rewrite, not an architecture change. This keeps a bold bet on a reversible footing.

**License note:** egui/eframe MIT/Apache-2.0, linked statically — clean.

### 1.3 GPU strategy — **wgpu** compute (WGSL), node-graph DAG

**Problem it solves:** darktable's fixed-order CPU/OpenCL pixelpipe is the documented source of mask-heavy Develop lag; vkdt's GPU-resident DAG delivers order-of-magnitude interactivity. We need that engine, cross-platform, from one shader source.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **wgpu** (MIT/Apache, Rust) | Vulkan+Metal+DX12 from one WGSL source; native Rust; `Device.on_uncaptured_error` + device-lost recovery hooks | WGSL younger than GLSL (subgroups/f16 support uneven); compute debugging rough | **Chosen** |
| Native Vulkan + MoltenVK | Max control (vkdt's path); mature GLSL | 3× platform surface; MoltenVK translation layer on macOS; no DX12 path for Windows-native | Lost: triples platform work for control we don't need in v1 |
| Halide (MIT) | One algorithm → tuned CPU+GPU kernels | C++ build toolchain; gives kernels, **not** the DAG/cache/ROI framework we actually need; heavy learning curve | Lost as the substrate; **retained as an option inside individual heavy nodes** (e.g. demosaic) if hand-WGSL underperforms |

**License:** wgpu MIT/Apache-2.0, static. **Reversal trigger:** if a specific platform's WGSL compute perf is unacceptable, individual nodes can drop to native Metal/Vulkan behind the `RenderNode` trait without touching the graph — node-local, reversible.

### 1.4 Catalog database — **SQLite** (WAL) via `rusqlite`, FTS5 + sqlite-vec

**Problem it solves:** catalog corruption destroyed loyalty for ON1/Luminar and is the #1 trust-killer; the mandate demands `kill -9` safety. SQLite's WAL mode makes torn writes impossible and is exactly what Lightroom itself uses.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **SQLite** (public domain) + WAL + FTS5 + sqlite-vec (Apache) | Crash-safe by design; single-file; proven to 1M+ assets in LR; full-text + vector search in-process; zero server | Single-writer; ANN index needs tuning at scale | **Chosen** |
| DuckDB | Fast analytical scans | Columnar/OLAP, wrong shape for transactional edit mutation; weaker single-writer durability story for desktop | Lost as primary; candidate **read-only analytics sidecar** later |
| redb / sled (pure Rust KV) | Memory-safe, no C dep | No SQL, no FTS, no vec; we'd rebuild query engine | Lost: reinventing SQLite badly |

**License:** SQLite public domain (bundled amalgamation, no system dep); `rusqlite` MIT; sqlite-vec Apache-2.0. **Reversal trigger:** none intended — the on-disk catalog format is a **near-one-way door** (migration cost). Schema is versioned from day one; engine swap is out of scope. Flagged for CTO sign-off.

### 1.5 ML runtime — **ONNX Runtime** via `ort`, in an out-of-process inference host

**Problem it solves:** one model format must run accelerated on CoreML (macOS), DirectML (Windows), and CUDA/CPU, and a model/EP/driver crash must **not** take down the editor mid-session.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **ONNX Runtime** (`ort`, MIT) | One integration → CoreML/DirectML/CUDA/TensorRT/CPU EPs; SAM2/BiRefNet/SegFormer/CLIP export to ONNX cleanly | C++ runtime dep; large binary (shipped as a downloadable component) | **Chosen** |
| candle (pure Rust) | No C++ dep; Rust-native | EP/hardware-accel and op coverage far behind ORT; would bottleneck AI features | Lost for v1; **retained as future pure-Rust path for small models** |
| ncnn | Good mobile perf | Desktop CoreML/DirectML story weaker; more per-model integration | Lost: ORT's one-integration EP coverage wins |

**Crash-isolation decision:** ORT runs in a **separate supervised process** (`lightbox-inferd`). Rationale: (a) EP/GPU-driver segfaults are recoverable — the supervisor restarts inferd, the requesting job fails gracefully; (b) it firewalls any GPL-adjacent model tooling behind a process boundary; (c) inference output (mask rasters, embeddings) is cheap to copy over IPC, so we pay no zero-copy penalty for isolating it (unlike the render engine — see §5).

**License + redistribution (pre-decided so E13 has no open gate):** ORT MIT; `ort` bindings MIT/Apache. The **shipped EP set is DirectML / CoreML / CPU** — all cleanly redistributable under permissive terms. **CUDA / TensorRT EPs are not bundled**; they are an **optional user-installed accelerator** (NVIDIA's CUDA/TensorRT redistribution terms are incompatible with permissive bundling), detected and used if present. Model weights get **individual license review** and ship as versioned packs (Apache/MIT/BSD only; **InsightFace weights explicitly banned** — non-commercial).

### 1.6 Per-subsystem library selections (with license + linkage)

| Subsystem | Primary | License / linkage | Alternative considered → why it lost |
|---|---|---|---|
| Raw decode (primary) | **rawler** | MIT, static | LibRaw as sole path → LGPL + memory-unsafe C++ on the untrusted surface |
| Raw decode (coverage fallback) | **LibRaw** | LGPL-2.1, **dynamic-link, sandboxed subprocess for untrusted input** | rawspeed → LGPL too, narrower gain |
| Demosaic (quality tier) | **clean-room RCD + AMaZE-class**, WGSL/CPU | own code (from papers) | Copy RawTherapee/darktable → GPL, contaminates. LibRaw AHD/DCB = LGPL **interim floor** only |
| Demosaic (interim) | LibRaw built-in (AHD/DCB/PPG) | LGPL, dyn-link | — |
| ICC color mgmt | **Little CMS 2 (LCMS2)** | MIT, static | — (no viable alternative; universal) |
| Working/view transforms | hand-rolled matrix pipeline (fixed ProPhoto-linear space) | own code | OpenColorIO (BSD) → deferred; our working space is fixed, OCIO's flexibility is unused weight in v1 |
| DCP profile engine (format eval) | own DCP parser + evaluator (format is Adobe-permissive DNG SDK spec) | own code | RawTherapee `dcp.cc` → GPL, clean-room only. **This engine is the format evaluator only; where the profile *content* and the default *look* come from is a distinct decision — see §1.7** |
| Camera color base + default look | **camera matrices (ColorMatrix1/2 / ForwardMatrix, exposed by rawler/rawspeed/LibRaw) + Lightbox-authored default look** — see §1.7 | factual matrix data + own authored look | Ship Adobe's `.dcp` library → **mandate violation** (Adobe-authored asset). dcamprof-generated in-house profiles → GPL subprocess-only, real target-shot cost |
| DNG read/write | **rawler / dnglab** | MIT, static | Adobe DNG SDK → permissive but C++ SDK weight; rawler covers v1 |
| Lens correction | **lensfun** (code) + CC-BY-SA data + embedded DNG opcodes | LGPL-3 dyn-link; data CC-BY-SA (ship w/ attribution) | — |
| Metadata (EXIF/IPTC) | **kamadak-exif** (EXIF/IPTC) + XMP via the **ISO XMP Toolkit** (row below) | BSD-2 / BSD-3, static | **exiv2 banned** (GPL-2). ExifTool = optional user-invoked subprocess only |
| XMP RDF engine | **ISO 16684 XMP Toolkit** (the Adobe-published reference implementation of the ISO standard) as the RDF read/write substrate, behind **Lightbox's own `crs:`/`lb:` field-mapping layer** (the differentiated part we own regardless of substrate) | **BSD-3, static** — permitted under **mandate v1.1** (Adobe-published OSI-licensed libs allowed with per-dependency license review + explicit CTO sign-off); **CTO license sign-off GRANTED in the round-4 confirmation review, 2026-07-04** (recorded in `02-approval.md`) | Own quick-xml RDF → **named reversal fallback, not primary.** Engineering rationale now that license is unblocked: the *only* reason the doc previously hand-rolled RDF was constraint 4, and mandate v1.1 removed it. Hand-rolling lossless foreign-field passthrough + `lr:hierarchicalSubject` round-trip (Risk 9) reinvents the ISO reference impl's hardest, most *undifferentiated* work — boring plumbing belongs on the proven reference; the novelty (semantic `crs:`/`lb:` mapping, approximate develop-setting migration) stays in our mapping layer either way. Third-party pure-Rust XMP crate → none mature enough for lossless round-trip. **Reversal trigger:** if the C++ SDK build/integration weight proves unacceptable, swap the substrate for own quick-xml behind the *same* mapping layer — bounded to `lightbox-meta`, one crate, mapping layer untouched |
| Codecs — JPEG | **zune-jpeg** (decode) + **mozjpeg** (encode) | MIT / IJG-BSD, static/dyn | libjpeg-turbo → fine, mozjpeg gives better ratio |
| Codecs — PNG/TIFF | `png`, `tiff` crates | MIT, static | libpng/libtiff → C deps unneeded |
| Codecs — JXL / AVIF | **libjxl / libavif** | BSD-3 / BSD-2, static | — |
| Codecs — HEIC | **libheif** + **libde265** (HEVC decode) | libheif LGPL-3 + libde265 LGPL-3, **dynamic-link** | **Transitive-codec trap:** libheif is a container shim — the HEVC decoder underneath is the licensing surface. We ship **libde265 (LGPL-3)** as that decoder and **explicitly ban linking libheif against an x265/GPL HEVC backend**. Audited by the native-binary SBOM gate (§8), not cargo-deny. **Known, accepted non-license risk:** HEVC decode carries **patent** exposure (a separate axis from the copyright/license mandate); this is identical to every competitor that decodes HEIC, out of scope for the license mandate, and accepted as-is — named here so it is explicit, not implicit |
| Preview resize | **fast_image_resize** (Lanczos, SIMD) | MIT, static | libvips (LGPL) → deferred optional accelerated backend behind `PreviewEncoder` trait |
| GPU DAG | **petgraph** | MIT, static | — |
| Async / jobs | **tokio** | MIT, static | — |
| Vector search | **sqlite-vec** | Apache-2.0, loadable ext | usearch → sqlite-vec keeps it in the catalog txn boundary |
| Face detect/embed | **OpenCV YuNet + SFace** (via `opencv` crate or ONNX export) | Apache-2.0 | InsightFace → **banned** (non-commercial weights) |
| Semantic search | **OpenCLIP / SigLIP** (ONNX) | MIT | — |
| Masking segmentation | **SAM2 / MobileSAM, BiRefNet, SegFormer(ADE20K)** (ONNX) | Apache-2.0 / MIT | U2-Net fallback |
| Content-aware fill | **LaMa (big-lama, ONNX)** + own PatchMatch | Apache-2.0 / own | Generative/diffusion → Could, out of v1 |
| Heal/seamless clone | **OpenCV `seamlessClone`** (Poisson/mixed-gradient blend) | Apache-2.0, static/dyn | own Poisson/mean-value solver → **rejected**: the "avoid a heavy dep" rationale no longer holds — OpenCV is *already* linked for YuNet/SFace (face detect/embed), so hand-rolling a seamless-clone solver is reinventing an undifferentiated wheel we already ship. Own PatchMatch is still ours for structural fill |
| Video | **FFmpeg** (probe/decode/thumbs) | **LGPL-2.1 build only — `--enable-gpl` FORBIDDEN**, dynamic-link | GStreamer → heavier integration. **Build-flag trap:** a default `--enable-gpl` FFmpeg pulls in GPL x264/x265 and contaminates the whole app. We consume a stock LGPL-only FFmpeg (no `--enable-gpl`, no `--enable-nonfree`); the configured flag set is asserted per-artifact by the SBOM gate (§8) |
| Tethering (deferred) | libgphoto2 | LGPL, dyn-link | vendor SDKs → optional closed plugins later |

**Standing license policy (enforced in CI over THREE surfaces, §8):** MIT/BSD/Apache/Zlib/Unicode link freely and statically. LGPL (LibRaw, lensfun, libheif+libde265, ffmpeg, ONNX Runtime's C++ core, optionally libvips) **dynamic-link only, with relink capability**. GPL/AGPL **never in-process** — subprocess isolation (ExifTool, dcamprof, ArgyllCMS) or clean-room reimplementation (demosaic, DCP eval). **Shipped content/data is a first-class license surface, not a footnote (surface 3, §8):** camera profiles, the default look family, HaldCLUT/creative-LUT packs, and lensfun correction data each carry a provenance + license entry in a data manifest — because a `.dcp` file is an open *format* whose *content* can be a proprietary asset (see §1.7), and the crate-graph/binary SBOM surfaces are structurally blind to bundled data. CC-BY-SA data (lensfun DB) shipped with attribution, share-alike on data only; CC0/CC-BY packs (HaldCLUT) with attribution; **no Adobe-authored `.dcp`/profile assets, ever** (mandate constraint 4). Model weights individually reviewed against a manifest.

**The load-bearing subtlety — transitive native codecs and build flags.** cargo-deny sees only the Rust crate graph; the real GPL/LGPL exposure lives *underneath* the FFI boundary, in how the C libraries were **built**, not in their crate wrappers. The three concrete traps this policy commits against, each audited by the native-binary SBOM gate (§8), not by cargo-deny:
- **libheif → its HEVC decoder.** libheif is a container shim; the codec beneath decides the license. We link it against **libde265 (LGPL-3)** and forbid an x265/GPL backend.
- **FFmpeg build configuration.** Only an LGPL-only build (no `--enable-gpl`, which would pull GPL x264/x265) is admissible.
- **ONNX Runtime EP binaries + accelerator libs.** The `ort` crate is MIT, but the shipped native ORT build, its execution-provider binaries, and the CUDA/TensorRT/CoreML/DirectML libraries they load each carry their own license (and, for CUDA/TensorRT, redistribution terms) that the crate graph never surfaces. **Pre-decided (closes the §8 open gate):** the shipped accelerator set is **DirectML (Windows) / CoreML (macOS) / CPU** — all cleanly redistributable. **CUDA/TensorRT EPs are NOT bundled**; they are an **optional, user-installed** accelerator that Lightbox loads if present, because NVIDIA's CUDA/TensorRT redistribution terms are not compatible with permissive bundling. E13 ships against the DirectML/CoreML/CPU baseline with no ambiguity; CUDA is a detected bonus, never a dependency.

---

## 1.7 Camera color — base rendering, the default look, and profile-data provenance

**Problem it solves (and the mandate exposure it closes):** the feature catalog lists "camera profiles as base rendering (**Adobe-Color-analogue default family**)" as a **Must**, and Risk 3 flags that per-camera color "without Adobe's profile library will be a visible gap." The reflexive way to close that gap — bundling Adobe's `.dcp` profile library — is a **hard-constraint violation**: a `.dcp` file is an open DNG-spec *format*, but its *content* is an **Adobe-authored asset** (mandate constraint 4, "no Adobe assets"). §1.6 designs the DCP *engine*; this section fixes where the profile **content and the default look come from**, so no E02/E10 planner has to guess (the "planners spec without guessing" bar) and no Adobe asset can slip past the (previously binary/ONNX-only) CI surfaces.

**The decision, in three tiers — the base tier depends on NO bundled profile at all:**

1. **Colorimetric base (default, universal, license-clean, zero bundled assets).** The base sensor→working-space color transform is built from the **camera's own color matrices** — the dual-illuminant `ColorMatrix1/2` (and `ForwardMatrix1/2` where present) that the DNG spec defines and that **rawler / rawspeed's `cameras.xml` / LibRaw already expose** from raw metadata. These are factual per-body calibration matrices, not authored artwork; our matrix evaluator (§1.6) interpolates them by white point exactly as it evaluates a DCP's matrix block. **The pipeline is therefore not dependent on any bundled `.dcp` file for correct color** — every camera rawler decodes renders colorimetrically-correct color out of the box. This is the same license-clean baseline darktable ships, and it means a missing profile degrades to *correct color*, never *wrong color*.

2. **The default "look" family — Lightbox-authored, original content, our license.** "Adobe Color" is a *role* (an opinionated default tone curve + mild hue/sat shaping layered on the colorimetric base), not a format we may copy. Lightbox ships its **own** default look family — an original tone curve and hue/sat shaping **we author** (analogous in *role* to darktable's base-curve/sigmoid default, or to Adobe Color's role, but original content under Lightbox's own license, containing no reverse-engineered Adobe LUT). This original look is the "default rendering family" the Must feature requires, produced and owned by us. Providing an *original* default look — rather than cloning Adobe's — is precisely what mandate constraint 4 demands.

3. **Camera-matching profiles — a curated, in-house-generated set; explicitly NOT promised for the whole camera universe.** Manufacturer-JPEG-emulating profiles for a **curated top-N of popular bodies** are **generated in-house** from **our own ColorChecker/IT8 target shots** using **dcamprof (GPL-3 → subprocess-only build tool, never linked)**. The output is Lightbox-generated profile data under our own license. **This is a real, previously-unbudgeted content-production cost** (physical target shots per body + generation/verification runs) — budgeted explicitly as an ongoing data line in E02 (§10.1), *not* one-time code, and *not* claimed to cover every camera. Community CC0/CC-BY DCP or HaldCLUT collections may be **user-installable**, but nothing is **bundled** until its content license clears the data manifest (§8, surface 3). **No Adobe-authored `.dcp` asset is ever shipped, bundled, or reverse-engineered.**

**The credibility trade-off, named against Risk 3 (stated, not buried):** for any body *without* an in-house camera-matching profile, the shipped rendering is the **colorimetric matrix base + the Lightbox default look** — correct color, but *not* a pixel-match to that manufacturer's in-camera JPEG "look." That is a real, visible gap versus Adobe's per-camera library — identical to the gap darktable/RawTherapee live with — and we **set that expectation to the user** rather than implying parity. The tiering guarantees the gap degrades **gracefully** (correct color always; manufacturer-look match only where we invested target shots), and the fix for a hot-body gap is *more target shots*, a content scale-up, **never** an architecture change and **never** a reach for Adobe's library.

**Provenance & license manifest (what ships, from where, under what license — audited by §8 surface 3):**

| Shipped/consumed data | Source / provenance | License | Bundled? |
|---|---|---|---|
| Base color matrices | camera metadata + rawler/rawspeed `cameras.xml` / DNG spec | factual calibration data; rawspeed data LGPL (consumed, not embedded) | derived at runtime |
| Default look family | **Lightbox-authored (original)** | Lightbox project license | yes |
| Camera-matching profiles (curated set) | in-house **dcamprof (GPL subprocess)** from **our own** target shots | Lightbox-generated, project license | yes — curated top-N only |
| Community DCP / creative-LUT / HaldCLUT packs | third-party collections | CC0 / CC-BY (verified via §8 surface 3) | **no** — user-installable only |
| lensfun correction data | lensfun database | CC-BY-SA (attribution + share-alike on data) | yes, with attribution |
| **Adobe `.dcp` / Adobe profile assets** | — | Adobe-authored | **NEVER** (constraint 4) |

**License:** own default look + own evaluator = project license; dcamprof/ArgyllCMS = GPL/AGPL subprocess-only; lensfun data CC-BY-SA. **Reversal trigger:** none for the base tier (matrices are foundational and license-clean); the curated-profile tier scales with content investment, not code.

---

## 2. System decomposition

Rust cargo workspace. One crate per bounded responsibility. The seam that matters most: **the shell talks only to `lightbox-core`'s command/query API**, and **`lightbox-core` is UI-agnostic** (headless-testable via `lightbox-cli`).

### 2.1 Component / crate map

```
                        ┌───────────────────────────────────────────┐
                        │           lightbox-shell (egui)            │
                        │  grid · loupe · develop · panels · gizmos  │
                        │  keymap registry · secondary display        │
                        └───────────────┬───────────────────────────┘
                         command/query  │  (headless boundary — no UI types below)
                        ┌───────────────▼───────────────────────────┐
                        │              lightbox-core                  │
                        │  session · command bus · query façade       │
                        │  transaction/history manager                │
                        └─┬───────┬───────┬───────┬───────┬──────────┘
             ┌────────────┘       │       │       │       └────────────┐
   ┌─────────▼────────┐ ┌─────────▼──┐ ┌──▼──────────┐ ┌──▼─────────┐ ┌▼──────────────┐
   │ lightbox-catalog │ │lightbox-   │ │lightbox-    │ │lightbox-   │ │lightbox-jobs  │
   │ SQLite · WAL     │ │edit        │ │render       │ │mask        │ │priority sched │
   │ FTS5 · vec       │ │recipe·hist │ │NODE GRAPH   │ │components  │ │cancel/pause   │
   │ smart-coll AST   │ │·snapshot   │ │wgpu compute │ │boolean ops │ │activity model │
   │ backup/integrity │ │·XMP map    │ │CPU fallback │ │brush/range │ └───────┬───────┘
   └──────────────────┘ └────────────┘ └──┬──────────┘ └────────────┘         │
                                          │ shares wgpu device w/ shell        │ IPC
   ┌──────────────────┐ ┌────────────┐ ┌──▼──────────┐ ┌────────────┐ ┌───────▼───────┐
   │ lightbox-decode  │ │lightbox-   │ │lightbox-    │ │lightbox-   │ │ lightbox-inferd│
   │ rawler/LibRaw    │ │color       │ │export       │ │meta        │ │ (SEPARATE PROC)│
   │ linearize·DCP    │ │LCMS2·space │ │encoders·    │ │EXIF/XMP    │ │ ONNX Runtime   │
   │ non-raw codecs   │ │display/out │ │sizing·wm    │ │crs import  │ │ CoreML/DirectML│
   └──────────────────┘ └────────────┘ └────────────┘ └────────────┘ └────────────────┘
   ┌──────────────────┐
   │ lightbox-ingest  │  import pipeline · probe · checksum copy · 2nd-copy · preview build
   │ lightbox-preview │  pyramid (embed/std/1:1) · raw decode cache · demand-driven
   │ lightbox-looks   │  (v2.1) image-stat analysis · look families → recipe deltas · shuffle
   └──────────────────┘
```

> **v2.1 crate-map annotation (AI Looks).** A new leaf crate **`lightbox-looks`** (E17, §10) hosts the image-adaptive cinematic-grading engine: a CPU-side palette/tonal **analyzer** over preview tiles, the parameterized **look families** and the fitting logic that maps them onto **recipe deltas** (color-grade wheels + curves + HSL), and the seeded **shuffle/variations** action. It is a *leaf*: it depends on `lightbox-edit` (E09 — to emit a partial `Recipe` merged onto the current edit) and reads image statistics that the render engine (E05) already computes; it holds **no** GPU device of its own (previews of look proposals render through the existing `Engine`, §4.2) and it is **not** on any hot path. The optional ML hook (a small palette/mood classifier) calls `lightbox-inferd` through the same `InferenceClient` (§2.2) as every other model — no new process, no new IPC. Boring analysis in the data bus; novelty isolated in this leaf, exactly where the thesis puts it.

> **v2.0 crate-map annotations** (the diagram is the built v1.x map; roles reframe, crates keep their names — no churn): the **shell** row `grid · loupe · develop · panels · gizmos` reads, for v2.0, **`drop-zone · filmstrip · loupe · develop · panels · gizmos`** — the grid virtualization becomes the filmstrip (§2.4/§10.0). `lightbox-catalog`'s `smart-coll AST` / `collection` / `keyword` responsibilities are **keep-dormant** (§3.1.1); its live role is the **edit store + cache index**. `lightbox-ingest`'s `checksum copy · 2nd-copy · import pipeline` become the **working-set loader** (walk/probe/hash → session set; **no managed copy**, E04). Everything else in the map is unchanged.

### 2.2 Crate responsibilities & public interface sketches

Interfaces are illustrative Rust signatures — the contract, not the implementation.

**`lightbox-catalog`** — owns all persistent library state; the single source of truth.
```rust
pub struct Catalog { /* connection pool: 1 writer + N WAL readers */ }
impl Catalog {
    pub fn open(path: &Path) -> Result<Catalog>;      // opens WAL, runs migrations
    pub fn writer(&self) -> WriterHandle;              // serialized single writer
    pub fn query(&self) -> ReaderHandle;               // WAL snapshot reader
    pub fn backup_verified(&self, dest: &Path) -> Result<BackupReport>; // integrity_check + copy
    pub fn integrity(&self) -> IntegrityStatus;        // quick_check on launch
}
pub trait SmartCollection { fn to_sql(&self, rules: &RuleTree) -> (String, Vec<Value>); }
```

**`lightbox-decode`** — raw + non-raw → linear sensor/RGB; DCP parsing. No GPU.
```rust
pub fn probe(path: &Path) -> Result<AssetProbe>;                 // format, dims, camera, embedded preview offset
pub fn decode_raw(path: &Path, opts: DecodeOpts) -> Result<MosaicImage>; // rawler; LibRaw subprocess fallback
pub fn decode_image(path: &Path) -> Result<LinearImage>;        // JPEG/TIFF/HEIC/PNG → working input
pub fn parse_dcp(bytes: &[u8]) -> Result<CameraProfile>;         // curated/user profiles only
pub fn camera_matrix_base(probe: &AssetProbe) -> Result<CameraProfile>; // §1.7 license-clean default
```

**`lightbox-render`** — the node-graph engine (see §4). Shares the wgpu `Device` with the shell.
```rust
pub struct Engine { device: Arc<wgpu::Device>, cache: NodeCache, registry: NodeRegistry }
pub struct RenderRequest { pub image: ImageId, pub recipe: Recipe, pub pv: ProcessVersion,
                           pub roi: Roi, pub scale: RenderScale, pub target: RenderTarget }
impl Engine {
    pub fn submit(&self, req: RenderRequest) -> RenderTicket;    // async; cancellable
    pub fn poll(&self, t: RenderTicket) -> RenderState;          // Preview(tex) → Full(tex)
    pub fn on_device_lost(&self, cb: impl Fn(DeviceLostReason)); // → rebuild or CPU fallback
}
pub trait RenderNode {                                          // one per stage × process-version
    fn id(&self) -> NodeId;
    fn eval_gpu(&self, ctx: &GpuCtx, inputs: &[Tile], p: &Params) -> Tile;
    fn eval_cpu(&self, inputs: &[TileCpu], p: &Params) -> TileCpu; // bit-faithful within ΔE tol
    fn invalidates(&self, changed: &ParamDelta) -> bool;
}
```

**`lightbox-edit`** — the versioned recipe, history, snapshots, XMP mapping.
```rust
pub struct Recipe { pub schema: u16, pub pv: ProcessVersion, pub base_profile: ProfileRef,
                    pub global: GlobalStages, pub crop: Crop, pub masks: Vec<MaskId>,
                    pub retouch: Vec<RetouchOp>, pub extra: RawXmp /* preserved unknowns */ }
// In-memory materialized view: `masks`/`retouch` are hydrated from the authoritative
// mask/mask_component/retouch_op tables; the serialized recipe doc stores ID references only (§3.2).
impl Recipe { pub fn to_xmp(&self) -> XmpDoc; pub fn from_lr_crs(x: &XmpDoc) -> Recipe; }
pub struct History { /* persistent step log */ }
pub struct Snapshot { pub name: String, pub recipe: Recipe }
```

**`lightbox-mask`** — mask model + component evaluation → grayscale weight buffers.
```rust
pub struct Mask { pub id: MaskId, pub name: String, pub components: Vec<Component>, pub adjust: LocalRecipe }
pub enum Component { Brush(Strokes), Linear(Gizmo), Radial(Gizmo),
                     LumaRange(Range), ColorRange(Sample),
                     AiSeg(SegRecipe /* provenance; raster baked to cached_raster_ref, replayed not recomputed — §4.5 */) }
pub enum BoolOp { Add, Subtract, Intersect }
pub fn evaluate(mask: &Mask, geom: &Geometry, gpu: &GpuCtx) -> WeightBuffer;
```

**`lightbox-ml`** (client) + **`lightbox-inferd`** (separate binary) — see §5/§6.
```rust
pub trait InferenceClient {
    fn segment(&self, img: PreviewRef, prompt: SegPrompt) -> JobFuture<MaskRaster>;
    fn embed_clip(&self, img: PreviewRef) -> JobFuture<Vec<f32>>;
    fn denoise(&self, tile: RawTile, model: ModelId) -> JobFuture<RawTile>;
}   // all calls cross the process boundary; supervisor restarts inferd on crash
```

**`lightbox-looks`** (v2.1, E17) — image-adaptive cinematic grading. Analyzes the opened image and emits **partial recipes** (never baked pixels) for E09 to merge.
```rust
pub struct ImageStats {                        // classical, CPU-side, over preview tiles
    pub palette: Vec<Swatch>,                  // k-means clusters in a perceptual space (OkLab)
    pub tone: ToneHistogram,                   // shadow/mid/high mass, black/white points, contrast
    pub temp_tint: (f32, f32), pub skin_present: bool,
}
pub fn analyze(preview: PreviewRef) -> ImageStats;              // no GPU device; reads engine-computed stats where available

pub struct LookFamily { pub id: LookId, pub name: String }     // teal-orange, film-stock, bleach-bypass, …
pub struct LookProposal {
    pub family: LookId, pub seed: u64,
    pub delta: RecipePatch,                    // color-grade wheels + curves + HSL deltas ONLY — a partial Recipe
}
/// Fit each family's parameters to THIS image's stats; `seed` drives coherent shuffle/variations.
pub fn propose(stats: &ImageStats, families: &[LookFamily], seed: u64, n: usize) -> Vec<LookProposal>;

/// Optional ML hook (E13): a small palette/mood classifier that biases family selection.
/// Absent inferd → classical selection only; NEVER a hard dependency.
pub fn propose_ml(stats: &ImageStats, infer: &dyn InferenceClient, seed: u64, n: usize) -> Vec<LookProposal>;
```
`RecipePatch` is a **partial** `Recipe` (§3.2): only the color-grade / tone-curve / HSL fields it sets are `Some`. Applying a look is `Recipe::apply_patch(&RecipePatch)` in E09 — a single, undoable history step; the result is an **ordinary editable recipe**, not a filter. Look-proposal thumbnails render through the normal `Engine` (§4.2) by evaluating the merged recipe at thumbnail resolution.

**`lightbox-jobs`** — priority scheduler; the activity center's backing model.
```rust
pub enum Class { Interactive, Foreground, Background }        // render / import·export / previews·AI
pub struct Job { pub class: Class, pub cancel: CancelToken, pub pausable: bool }
pub fn spawn<T>(class: Class, f: impl Future<Output=T>) -> JobHandle<T>;
```

**`lightbox-export`**, **`lightbox-ingest`**, **`lightbox-preview`**, **`lightbox-color`**, **`lightbox-meta`** — encoders/sizing/watermark; import pipeline; preview pyramid + raw cache; LCMS2 + working space; EXIF/XMP read-write + LR interop, respectively.

### 2.3 The load-bearing seams

1. **Shell ↔ Core (headless boundary).** Commands mutate through the transaction/history manager; queries read WAL snapshots. No UI type crosses down; no SQL crosses up. This is what makes the UI reversible (§1.2).
2. **Core ↔ Render (shared wgpu device).** The render engine and the egui shell hold the **same `Arc<wgpu::Device>`**, so the output texture is composited zero-copy. This is the reason render stays in-process (§5).
3. **Catalog owns edit-state; XMP is a projection.** One source of truth. XMP sidecars are written on opt-in and read on import — they are never a concurrent writer of record. Divergence is surfaced as a badge, resolved by explicit read/write-metadata commands.
4. **Jobs ↔ inferd (IPC).** The only process boundary in the hot path is ML, chosen because its output is cheap to copy and its crash-likelihood is highest.
5. **RenderNode registry keyed by (node, process_version).** Old process versions keep their node implementations registered forever → old edits render identically (§4.5).

### 2.4 Editor-shell contract (v2.0 — the entry model that replaces the DAM)

**Problem it solves:** v1.x entered the product through a *managed library* (import → catalog → grid → pick → develop). v2.0 cuts that entirely; the product opens *files*, not a catalog. This section is the decision-complete contract for how a photographer gets pixels on screen and into the develop surface, so no E04/E08 planner guesses. **egui capability is proven:** `eframe` surfaces both `RawInput.hovered_files` and `dropped_files` (path + optional bytes), so the drop target and hover affordance are first-class, not a platform hack.

**Entry points — exactly these, no managed import:**

| Entry | Behavior | Owner |
|---|---|---|
| **Drag-drop a single file** onto the window or empty-state drop zone | opens it in the loupe; filmstrip holds the one file | E08 (drop handling) → E04 (load) |
| **Drag-drop a multi-file selection** | all supported files become the working set; first is active in the loupe; filmstrip shows the set in drop order | E08 → E04 |
| **Drag-drop a folder** | the folder's directly-contained supported files become the working set (sorted by capture-time then name) | E08 → E04 (walker, non-recursive) |
| **Drag-drop a folder, recursive** (modifier held, or a prefs default) | the folder tree is walked recursively; all supported files become the working set | E08 → E04 (walker, recursive) |
| **OS file-open dialog** (menu / Cmd-O) | native multi-select file+folder picker → same working-set construction | E08 (`rfd`) → E04 |
| **File-association / "Open With" / CLI arg** | OS hands Lightbox one or more paths at launch or via a running-instance IPC → same working-set construction | E08 (platform launch handler) → E04 |

**Empty-state drop zone.** With no working set, the window is a full-bleed **drop zone** ("Drop photos or a folder to start editing — or ⌘O") that also reflects `hovered_files` (highlight + count) before the drop lands. It is the *only* chrome in the empty state — no library, no recents grid required for v1 (a recents *list* is a permitted Should, still session-launched, never a managed catalog).

**The working set (session, not a library).** The working set is **in-memory session state** — an ordered list of opened file paths plus per-file probe results and edit-store handles. It is **not** persisted as a catalog: closing the app discards the set; reopening a file re-derives its recipe from the edit store by content-hash (§3.1). Dropping a new set **replaces** the current one (with an unsaved-nothing guarantee — edits auto-persist continuously, §3.1, so there is never a "save before closing the set?" prompt). The **filmstrip** is the working set's view: a horizontally virtualized strip of T0/T1 previews (E03) in set order, with the active file enlarged in the loupe; it reuses the virtualized-strip machinery E01 built for the grid (§10.0), now bounded to a session set (tens–hundreds of files), not a 100k catalog.

**Raw vs. non-raw develop surface (which controls appear per source type).** Every source runs the **same node-graph pipeline** (§4.1); the develop surface *adapts* to what the source can meaningfully expose. The rule: **raw sources expose the full develop toolset; non-raw sources expose the same toolset minus the raw-only stages**, which are *hidden* (not merely disabled) so the surface never shows a control that cannot act.

**Complete raw parameter surface (v2.1 — binding, not a curated subset).** The mandate (v2.1) makes this a Core guarantee: opening a raw exposes *everything the pipeline can vary* at the raw-decode and demosaic stages — a curated/simplified subset is a scope violation, not a design choice. This is the explicit, decision-complete list of the raw-only parameter groups and the epic that delivers each, so no E02/E10/E11 planner ships a partial surface. Every group here populates the **"full"** cells of the surface table below; on a non-raw source every one is *hidden* per the rule above (no mosaic, no raw latitude, no sensor-native color to reinterpret).

| Raw-only parameter group | Controls that MUST be exposed | Delivered by |
|---|---|---|
| **White balance (full sensor)** | as-shot; presets (daylight/cloudy/shade/tungsten/fluorescent/flash); **Kelvin + tint** sliders (true Kelvin from the sensor, not a working-space shift); gray-point **eyedropper** | **E02.1** (WB on the matrix base) → **E10.1** (panel/eyedropper UI) |
| **Camera profile selection** | matrix base (always available, §1.7 tier 1); curated in-house DCP; user-installed DCP; default-look family + amount | **E02.1/E02.2** (matrix base + DCP eval) → **E10.1** (profile picker) |
| **Demosaic-stage options** | algorithm select (interim AHD/DCB → clean-room RCD/AMaZE-class; X-Trans Markesteijn where applicable); false-color / maze suppression | **E11.1** (clean-room demosaic) |
| **Black / white point** | raw black-point offset; white-point / highlight-clip level (per-channel where the sensor exposes it) — operating on the pre-transform linear range | **E02.1** (linearize) → **E10.1** |
| **Highlight-reconstruction mode** | clip / blend / **rebuild-from-adjacent-channels** (uses raw latitude that a rendered source has already discarded) | **E10.2** (halo-free recovery, raw-fed) |
| **Full-latitude tone recovery** | highlights/shadows/whites/blacks acting on the pre-clip raw range, not the 8-bit display range | **E10.2** |
| **Per-channel camera calibration** | camera-calibration R/G/B primary hue + saturation shift and shadow tint (the `crs:` Calibration-block analogue) | **E02.2** (calibration evaluator) → **E10.3** (panel) |
| **Raw detail / raw geometry** | raw-domain (pre-demosaic) denoise; bad-pixel / hot-pixel removal; raw chromatic aberration | **E11.1/E11.2/E11.3** |

| Develop surface element | Raw (CR3/NEF/ARW/RAF/DNG/…) | Non-raw (JPEG/TIFF/PNG/HEIC) |
|---|---|---|
| White balance (Kelvin + tint, as-shot, presets, eyedropper) | **full** (true Kelvin from sensor) | present but **relative** (WB is a working-space shift, not sensor re-interpretation; labeled as such) |
| Camera profile / base look (matrix base + curated DCP + default look, §1.7) | **full** (matrix base always; DCP where curated) | **hidden** (no mosaic/camera-native color to profile; the embedded rendering is the base) |
| Black / white point (raw black offset, clip level) | **full** (acts on the pre-transform linear range) | **hidden** (no raw range; levels fold into exposure/tone) |
| Highlight **reconstruction** (rebuild clipped raw channels) | **full** (raw latitude present) | **hidden** (clipped JPEG data is unrecoverable — offer only highlight *compression*, not reconstruction) |
| Per-channel camera calibration (primary hue/sat, shadow tint) | **full** (E02.2) | **hidden** (no camera-native primaries to calibrate) |
| Demosaic / raw-denoise / raw geometry (X-Trans, bad-pixel) | **full** (E11) | **hidden** (already demosaiced) |
| Exposure/contrast/tone-curve/HSL/color-grade/B&W/presence | **full** | **full** |
| Detail (sharpen, luma/chroma NR), optics (lens/CA/defringe), geometry (crop/upright/transform) | **full** | **full** (lens profile applies if EXIF identifies the lens; else manual) |
| Masking, local adjustments, retouch, AI masks | **full** | **full** |
| Export | **full** | **full** |

The surface is driven by a single `SourceKind { Raw, Rendered }` flag on the probe (§2.2 `AssetProbe`); panels declare a `min_source_kind` and the shell hides those the active file cannot satisfy. **The v2.1 completeness bar is a panel-population contract, not new pipeline work:** the node-graph already varies every one of these parameters (§4.1); the guarantee is that the raw develop surface *surfaces every one of them as a control* rather than presenting a reduced panel — enforced as an E08/E10 acceptance check that every raw-only group above has a live, wired control on a raw source. This mirrors the Lightroom develop experience (raw gets the full basic panel; a JPEG opens in the same Develop module with the raw-only affordances inapplicable) — and it means **the pipeline, engine, edit store, and export path are identical across source types**; only the *visible controls* differ.

**Deployment/rollback story for the entry model:** the working-set loader (E04) and the shell drop-handling (E08) are additive over the built E01 shell. Ship Phase 1 (drop → filmstrip → loupe over the existing display-transform node) with no develop panels; roll back by reverting E04/E08 and dropping no migrations (the working set is not persisted, so there is no schema to unwind). The develop panels layer on top without touching the entry contract.

---

## 3. Core data model

### 3.1 Catalog schema (entities)

Ownership is explicit: **`asset`** = one physical file (immutable original); **`image`** = one develop-able instance (virtual copies are multiple `image` rows over one `asset`). Edit-state hangs off `image`.

| Table | Key columns | Notes |
|---|---|---|
| `library_root` | id, volume_uuid, path | supports relocation via volume UUID |
| `folder` | id, root_id, parent_id, rel_path | physical tree; fs-watcher reconciled |
| `asset` | id, folder_id, filename, content_hash(xxh3-128), format, camera_model, capture_time, width, height, bytes, missing(bool), decode_error | immutable original; `content_hash` keys caches + relink |
| `image` | id, asset_id, is_virtual, name, orientation | one editable instance; VC falls out for free |
| `edit_recipe` | image_id(pk), pv, schema, doc(CBOR blob **authoritative for the global recipe + ordered mask/retouch reference lists**), updated_at | one row per image; `doc` holds global stages, geometry, `lb_extra`, `xmp_passthrough` and the `MaskId[]` / `RetouchOpId[]` order lists — **not** mask/retouch content (see ownership note below); derived index rebuilt in same txn |
| `edit_index` | image_id, has_masks, has_ai_mask, is_edited, crop_ratio, … | denormalized from `doc` on every write — one write path, no divergence |
| `history_step` | id, image_id, seq, op, params, ts | persistent history |
| `snapshot` | id, image_id, name, recipe_doc, ts | named versions (promoted to Must) |
| `mask` | id, image_id, name, order, adjust(blob) | per-mask local recipe |
| `mask_component` | id, mask_id, kind, bool_op, params(blob), ai_recipe, cached_raster_ref, stale(bool) | AI comps: `cached_raster_ref` (the **baked, authoritative, replayed** segmentation raster) + `ai_recipe` (provenance/model-pin) + `stale` (set only by explicit user recompute, never auto — §4.5) |
| `retouch_op` | id, image_id, order, mode(clone/heal/remove), src, dst, params | editable objects |
| `preview` | asset_id/image_id, tier, store_path, px, hash, built_at | index into preview store |
| `collection`, `collection_set`, `collection_item` | — | virtual albums + custom order |
| `smart_collection` | id, name, rule_tree(json) | AST → SQL at query time |
| `keyword`, `keyword_hierarchy`, `keyword_asset` | — | hierarchical; `dc:subject`/`lr:hierarchicalSubject` |
| `label`, `flag`, `rating` | on `image` | one-key culling grammar |
| `metadata_cache` | asset_id, exif/iptc columns + json | fast filter facets |
| `embedding` | image_id, model_id, vec(sqlite-vec) | semantic search; ANN index |
| `face`, `person`, `face_person` | bbox, embedding(SFace), cluster | pausable background service |
| `import_session` | id, ts, source, options | audit + undo-import |
| `model_pack` | id, name, version, hash, license, installed | reproducibility + license manifest |
| `schema_version` | version | migration guard |
| `assets_fts` (FTS5) | filename, keywords, caption, camera | free-text filter bar |

**Edit-state ownership — one live owner per entity (resolves the `edit_recipe.doc`-vs-mask/retouch-table authority question):** `edit_recipe.doc` is authoritative for the **global** recipe (base profile, global stages, geometry, `lb_extra`, `xmp_passthrough`) and for the **ordered reference lists** (`MaskId[]`, `RetouchOpId[]`) that sequence local edits — it does **not** embed mask or retouch *content*. The **`mask` + `mask_component` tables are the sole authoritative store for mask content** (component geometry/params, per-mask `adjust`, and the AI `cached_raster_ref` / `stale` runtime state); the **`retouch_op` table is the sole authoritative store for retouch objects**. The split is deliberate, not incidental: mask-raster baking and AI-staleness updates (§4.5) mutate `mask_component` on their own cadence and must never rewrite the recipe blob or perturb `history_step` — dual-homing mask/retouch content in both the blob and the tables would be exactly the silent-divergence trap the single-source-of-truth rule exists to prevent. The in-memory `Recipe` (§2.2 / §3.2) is a **materialized view** assembled by joining `edit_recipe.doc`'s reference lists against these tables; `Recipe::to_xmp()` and `snapshot.recipe_doc` are **read-only materialized projections** that gather content from the authoritative tables into one self-contained document — a snapshot is an immutable point-in-time copy, explicitly **not** a second live owner. `edit_index` is derived from `edit_recipe.doc` + the mask tables inside the same write txn, so there is exactly one write path and no divergence.

**Crash-safety invariant:** every catalog mutation is a single WAL transaction; `PRAGMA synchronous = NORMAL` in WAL is proven safe against `kill -9` (torn writes impossible; at most the last uncommitted transaction is lost). Verified by fault-injection tests (§8).

#### 3.1.1 v2.0 rescope — the catalog becomes an internal **edit store + cache index** (retain vs keep-dormant)

The v2.0 mandate cuts the DAM but keeps the *crash-safe transactional store* — it is now the **edit store**: recipes, snapshots, history, mask/retouch content, and cache/preview/sidecar bookkeeping, **keyed by `content_hash` (path is a hint, not identity)**. The crate keeps its name (`lightbox-catalog`) — renaming 16 crates' worth of imports is unwarranted churn (mandate: no code deletion planned); its *role* is reframed, not rebuilt. **On open, a file gets an `asset` row (content-hash keyed) + an `image` row on demand**; there is no managed import, no `import_session`, no folder tree of record. The store is **plumbing, never a user-facing surface**.

**Disposition of the v1.x tables — decision: KEEP-DORMANT (not retire-drop).** One choice, justified: E01 already shipped schema v1 with these tables and is `kill -9`-verified against real catalogs; a destructive migration that *drops* columns/tables carries rollback risk (copy-on-write upgrade, restore drills) for **zero user benefit** — a dormant table that is never written and never queried costs nothing at runtime and nothing in VRAM/disk. Dropping them is a reversible future migration if ever wanted; keeping them dormant is the crash-safe, no-churn default and honors "no code deletion planned in this doc." So:

| Table | v2.0 disposition | Rationale |
|---|---|---|
| `asset`, `image` | **RETAIN** (reframed) | one row per opened file / editable instance; `content_hash`-keyed; `folder_id` nullable (path stored on asset), virtual copies still fall out for free |
| `edit_recipe`, `edit_index` | **RETAIN** — the core of the edit store | recipe authority (§3.2); `edit_index` badges the filmstrip |
| `history_step`, `snapshot` | **RETAIN** | history + named versions are editor features |
| `mask`, `mask_component`, `retouch_op` | **RETAIN** | authoritative local-edit content (§3.1 ownership note) |
| `preview` | **RETAIN** (rescoped) | indexes the preview store for filmstrip + develop-open, not a 100k grid |
| `model_pack` | **RETAIN** | AI model provenance/license manifest |
| `metadata_cache` | **RETAIN (read-only, minimal)** | the editor's info panel reads EXIF (camera/lens/exposure/ISO/date); it is a per-open cache, **not** a filter-facet index |
| `schema_version` | **RETAIN** | migration guard |
| `folder` | **KEEP-DORMANT** | no managed physical tree of record; the working set is session state (§2.4); asset carries its path directly |
| `collection`, `collection_set`, `collection_item`, `smart_collection` | **KEEP-DORMANT** | collections/albums/smart-collections cut |
| `keyword`, `keyword_hierarchy`, `keyword_asset` | **KEEP-DORMANT** | hierarchical keywords cut (XMP `dc:subject` still round-trips as passthrough via E09, not via these tables) |
| `label`, `flag`, `rating` | **KEEP-DORMANT** | culling grammar cut |
| `embedding` | **KEEP-DORMANT** | semantic search cut |
| `face`, `person`, `face_person` | **KEEP-DORMANT** | faces/people cut |
| `import_session` | **KEEP-DORMANT** | managed import cut; files open in place |
| `assets_fts` (FTS5) | **KEEP-DORMANT** | free-text filter bar cut |

**Sidecar strategy (per mandate):** the edit store is the **automatic, always-on internal persistence** — every recipe mutation commits transactionally so reopening a file by content-hash restores its recipe with no user action. **XMP sidecar export is opt-in** (a preference / explicit "write metadata"): the edit store is the single writer of record; XMP is a projection (§2.3 seam 3, unchanged), with `xmp_passthrough` preserving foreign fields. The `preview` table's `sidecar sync state` is tracked so a divergent sidecar surfaces a badge rather than silently overwriting — the same divergence contract as v1.x, now the *only* interop surface (no `.lrcat` catalog migration; see E16 rescope, §10).

**Crash-safety, rescoped:** the `kill -9`-safety invariant above **stands unchanged** and is the surviving half of the v1.x "three bets" — it now guards the *edit store* (a lost uncommitted transaction = at most one un-persisted slider change), not a 100k-asset catalog.

### 3.2 Edit-recipe serialization (versioned, XMP-mappable)

Authoritative form: **CBOR document** in `edit_recipe.doc`. Human-diffable JSON mirror available for debugging/export.

```
Recipe {
  schema: 2,                       // serialization schema (migrates independently of pv)
  process_version: 5,              // render-engine semantics (immutable per image)
  base_profile: { kind: "matrix" | "dcp", id, look_ref, look_amount },
                                   // kind "matrix" = license-clean camera-matrix base (§1.7),
                                   // the default when no curated/user DCP is selected;
                                   // look_ref names the Lightbox-authored default look family

  global: {                        // ordered stages — see §4.1
    white_balance, exposure, contrast, highlights, shadows, whites, blacks,
    tone_curve{rgb,r,g,b}, hsl[8], color_grade{shadow,mid,high,blend,balance},
    bw{mix[8]}, presence{clarity,texture,dehaze},
    detail{sharpen{amount,radius,detail,mask}, nr{luma,chroma,detail}},
    optics{lens_profile, ca, defringe, vignette}, effects{postcrop_vignette, grain}
  },
  geometry: { crop, angle, flip, upright, transform },
  masks: [ MaskId… ],              // ordered refs — authoritative content in mask/mask_component (§3.1)
  retouch: [ RetouchOpId… ],       // ordered refs — authoritative objects in retouch_op (§3.1)
  lb_extra: { … },                 // Lightbox-only fields (lb: namespace)
  xmp_passthrough: { … }           // unknown foreign fields, preserved verbatim on round-trip
}
```

**XMP mapping rule:** every field carries a documented mapping to Adobe `crs:` where one exists (exposure→`crs:Exposure2012`, etc.); Lightbox-only fields serialize to an `lb:` namespace. `from_lr_crs()` reads `crs:` for one-way migration (organization migrates losslessly; develop settings migrate approximately — expectation set explicitly per Risk 9). **Forward-compat contract:** unknown fields on read are preserved in `xmp_passthrough` and re-emitted on write, so a newer catalog's edits survive an older build's round-trip.

### 3.3 Preview & cache storage layout

Sibling directory to the catalog, **relocatable and size-capped**, keyed by `content_hash` so it survives catalog moves and file relinks:

```
<catalog-name>.lbdata/
  catalog.sqlite         (+ -wal, -shm)
  backups/               YYYY-MM-DD/catalog.sqlite.zst  (verified, dated, pruned)
  previews/<hh>/<hash>.t0.jpg      T0: embedded camera JPEG (instant culling, flagged)
                <hash>.t1.jxl      T1: standard, sized to display
                <hash>.t2/<tile>.jxl  T2: 1:1 tiled
  rawcache/<hh>/<hash>.<params>.zst   partially-demosaiced raw state (default 5 GB LRU)
  smartpreview/<hh>/<hash>.jxl        ~2560px proxy for offline editing (Should)
  masks/<hh>/<hash>.png               baked AI-segmentation rasters (mask_component.cached_raster_ref — §4.5)
  models/<name>-<version>/            ONNX packs (hash-pinned, license-manifested)
  thumbcache.sqlite                   hot thumbnail atlas for grid scroll
```

Retention: T2 1:1 auto-evicted after N days (configurable/never); total preview cache capped with LRU; raw cache capped, purgeable, relocatable to a fast drive — all surfaced in the performance preferences panel.

---

## 4. Render pipeline design

### 4.1 Staged pixel pipeline (scene-linear, 32-bit float, fixed ProPhoto-primaries working space)

Logical order (each stage = one node or subgraph in the DAG). Raw-only stages marked `[raw]`.

```
[raw] decode ──► [raw] linearize (black/white, lin-curve, bad px)
      ──► [raw] raw-denoise (classical v1.0; neural v1.x)
      ──► [raw] demosaic (clean-room RCD/AMaZE; Markesteijn X-Trans = Should)
      ──► white balance (as-shot / Kelvin+tint / eyedropper)
      ──► input transform: camera-matrix base (ColorMatrix1/2 ⊕ ForwardMatrix, always
           available, license-clean — §1.7) OR DCP (dual-illuminant matrix ⊕ HueSat LUT
           ⊕ profile tone curve) when a curated/user profile is selected;
           then the Lightbox-authored default look (§1.7)
           → working space (ProPhoto primaries, linear)
      ──► global tone/color:
           exposure ► contrast ► highlights/shadows (local Laplacian / guided filter)
           ► whites/blacks ► parametric+point tone curve ► HSL (8-band) ► color grading
           ► B&W mix ► presence (clarity/texture/dehaze) ► creative LUT profile
      ──► detail: capture sharpen ► luma/chroma NR ► moiré
      ──► optics/geometry: lens corr (lensfun+LCP+opcodes) ► CA/defringe
           ► upright/transform ► crop ► rotate/flip
      ──► LOCAL: for each mask { evaluate weight buffer ► apply per-mask sub-recipe }
           composited in mask order
      ──► retouch: heal / clone / content-aware remove (editable objects)
      ──► effects: post-crop vignette ► film grain
      ──► OUTPUT transform: working → display ICC (view)  |  working → output ICC (export)
```

The highlight/shadow recovery stage (local Laplacian / guided filter) is called out in research as the quality-defining hard part of the basic panel — budget it as its own sub-project within E10.

### 4.2 GPU node-graph approach

- **DAG** (`petgraph`): nodes are compute passes with typed image ports; edges carry tile handles. Feedback/multi-input edges supported (vkdt pattern) for e.g. guided-filter and mask compositing.
- **Cached intermediates:** each node's output is cached under a key = hash(node_id, process_version, input_hashes, params). Moving one slider changes one node's params → invalidates only that node and its downstream tail → **recompute the tail, not the pipeline**. This is the mechanism behind the <100 ms budget.
- **Working data:** RGBA16F tiles on GPU (half-float storage, float compute) to halve VRAM vs f32 while keeping precision; f32 only where a stage needs it (highlight recovery accumulation).

### 4.3 ROI / preview evaluation

- **Fit-view:** evaluate the graph at **preview resolution** over the **visible ROI** only. A 45 MP raw at fit-view on a 4K display evaluates ~8 MP, not 45.
- **1:1 zoom:** tile the visible ROI (256² tiles), evaluate on demand, cache tiles in T2.
- **Progressive:** on develop-open, show the best existing preview tier immediately (<100 ms, "Loading" badge), render preview-res from the raw cache (~sub-second), then full-res on idle. UI never blanks.
- **Coalescing:** the render scheduler debounces rapid slider changes to latest-wins so a slider drag issues one in-flight preview render, not one per mouse-move event.

### 4.4 CPU fallback

Every `RenderNode` implements `eval_cpu` (rayon + `std::simd`/`wide`), sharing the *algorithm spec* with its WGSL kernel. The CPU path is the fallback when: no compatible GPU, GPU disabled in prefs, or device-lost recovery has degraded (§6.2).

**Degraded-but-usable contract — stated so "crash-proof fallback" is not oversold as full-res parity.** The performance budgets in §7 are **GPU-path budgets**. The CPU path is a *correctness and continuity* fallback, **not** a performance-parity fallback: a full-resolution 100 MP render through the whole graph on CPU is a **minutes-scale** operation, not a <100 ms one. So the guaranteed CPU contract is **preview-resolution editing only** — on the CPU path the engine renders the visible ROI at fit-view/preview resolution so the user keeps *editing* interactively (target: interactive at preview res on an 8-core CPU), while **full-resolution CPU renders are relegated to export/background jobs with a visible progress indicator and no latency promise**. "Keep editing after device-lost" (§4.4/§6.2) means *keep editing at preview resolution*, which is honest and testable; it does not mean full-res slider parity with the GPU.

**Consistency property surrendered — stated explicitly:** we do **not** guarantee GPU/CPU bit-identity. Floating-point rounding, FMA contraction, and transcendental implementations differ across backends and drivers; bit-exactness is unachievable and Adobe doesn't promise it either. **What we guarantee instead:** for a fixed `(process_version, engine_build)`, each backend renders **deterministically**, and all backends agree within a **perceptual tolerance (ΔE2000 ≤ 1.0, PSNR ≥ 45 dB)** validated by golden-image tests (§8). Export records which backend produced the file. This is the one consistency guarantee we trade, and it is bounded and tested.

### 4.5 Process versioning

- `process_version` is stored per `image` and is **immutable** unless the user explicitly migrates.
- The `NodeRegistry` is keyed by `(node_id, process_version)`; old versions' node implementations stay registered forever. Improving an algorithm ships a **new** process version, never mutates an old one → a 2026 edit renders pixel-consistent (within §4.4 tolerance) in 2031.
- **AI-mask reproducibility policy (authoritative; reconciles §4.4).** Cross-EP ONNX inference is **not** bit-deterministic — CoreML vs DirectML vs CUDA vs CPU, and any driver/model-version change, can produce a different segmentation raster. That is a *stronger* non-determinism than the render engine itself surrenders in §4.4 (which only surrenders GPU/CPU bit-identity for continuous math, still bounded by ΔE2000 ≤ 1.0). We therefore do **not** rely on re-running inference for the "edits render forever" promise. Instead: **the segmentation raster is baked into the immutable recipe.** On first evaluation, `AiSeg` runs inference once and writes the resulting weight raster to `mask_component.cached_raster_ref` (content-addressed in the preview store); **that baked raster — not a fresh inference call — is what replays on every subsequent render, this year or in 2031.** The stored `model_id + version + params` and the `model_pack` hash-pin exist for *provenance and explicit user-initiated recompute*, never for silent recomputation. `mask_component`'s staleness flag is set only by an explicit "update AI mask" command (e.g. after the user edits the crop or picks a newer model); it never auto-invalidates the baked raster. **So the reproducible artifact is the baked raster (deterministic, replayed); a recompute is a deliberate, user-visible act that produces a new — possibly different — raster and is treated as an edit.** This is the single authoritative answer to Risk 7.
- Golden-image tests run **per process version** and fail the build if any historical PV's output drifts beyond tolerance — the immutability guard.

---

## 5. Concurrency & process architecture

### 5.1 Threads, tasks, processes

| Unit | Runs | Never does | Isolation |
|---|---|---|---|
| **UI thread** (egui event loop) | widget layout, input, compositing at 60 fps | I/O, decode, heavy compute | in-process |
| **Render scheduler** | coalesces slider events, submits to engine, polls GPU async | block the UI thread | in-process (shared wgpu device) |
| **Job runtime** (tokio multi-thread) | import, export, preview build, AI analysis, embeddings, faces | touch UI types | in-process, bounded queues |
| **Catalog writer** (dedicated task) | serializes all mutations → no `SQLITE_BUSY` | concurrent writes | in-process, single writer |
| **Catalog readers** (pool) | WAL snapshot reads | writes | in-process |
| **`lightbox-inferd`** | ONNX inference (CoreML/DirectML/CUDA) | anything the editor can't lose | **separate process, supervised** |
| **Decode sandbox** (LibRaw path) | exotic/untrusted C++ decode | corrupt the parent | **separate process** for the LGPL C++ fallback only |

### 5.2 Why render is in-process but ML is out-of-process

This is the central concurrency decision. The render engine must hand a texture to the shell **zero-copy every frame** — cross-process GPU texture sharing is platform-specific and fragile, so render shares the shell's wgpu device in-process, and GPU resilience is handled by **device-lost recovery + CPU fallback** (§6.2) rather than process isolation. ML inference, by contrast, (a) crashes more often (EP/driver bugs, OOM on large models), (b) produces cheap-to-copy output (mask rasters, embedding vectors), and (c) benefits from a GPL/weights firewall — so it pays no zero-copy penalty to live in a supervised subprocess. **The boundary is drawn by zero-copy need vs crash-likelihood, not by module tidiness.**

### 5.3 Job classes, backpressure, cancellation

- **Interactive** (render) preempts **Foreground** (import/export) preempts **Background** (preview builds, AI analysis, faces, geocoding). GPU time is sliced so export never starves the interactive canvas — the fix for Lightroom's documented "background work steals my session" and "export got 3× slower" complaints.
- **Cancellation** is a cooperative `CancelToken` threaded through every job and the render scheduler; scrolling the grid cancels off-screen preview renders (visible-first scheduling).
- **Pause** is available on all Background jobs (AI/analysis), surfaced per-task in the activity center — the mandate's pausable-background-AI differentiator.
- **Backpressure:** bounded channels between stages; import throttles preview-build enqueue so a 10k-raw card can't OOM the queue.

---

## 6. Failure-mode taxonomy & mitigations

| Failure | Detection | Designed mitigation |
|---|---|---|
| **Catalog corruption / `kill -9`** | `PRAGMA quick_check` on launch | WAL + `synchronous=NORMAL` makes torn writes impossible; **exit-time verified backup** (`integrity_check` → dated `.zst`); on corruption: auto `.recover` attempt → guided restore-from-backup; schema upgrades **copy-on-write** (original preserved for rollback). Fault-injection tested (§8). |
| **GPU driver crash / device lost** | wgpu device-lost callback / uncaptured-error | Rebuild the device and re-upload; if it recurs within a window, **drop to CPU backend at preview resolution** (the degraded-but-usable contract of §4.4 — interactive editing continues at preview res; full-res renders defer to background/export jobs with progress, not the <100 ms budget), show a non-modal notice, keep editing. Export never uses a crashed device. This is the "crash-proof GPU pipeline" differentiator — with the CPU tier honestly scoped as preview-res continuity, not full-res parity. |
| **Missing / offline originals** | fs-watcher + launch scan → `asset.missing` | Grid badge; edits + previews remain fully usable (T1/T2 previews and smart-preview cover offline editing); **relink** single / folder-tree / find-all-missing via `content_hash` + filename heuristic. |
| **Model download failure** | checksum mismatch / network error | Packs are **resumable, hash-verified, mirror-fallback**; on failure the feature is disabled with a clear CTA — **core editing never blocks**; models version+hash pinned for reproducibility. |
| **Corrupt / unsupported raw** | decode error in `lightbox-decode` | rawler (memory-safe) primary; LibRaw C++ fallback runs **sandboxed subprocess** so a decoder crash can't corrupt the parent; asset marked `decode_error`, still catalogued, app never crashes. |
| **Low / exhausted VRAM** | VRAM probe at startup + before inference | AI features gated at 8 GB dedicated / 16 GB unified thresholds; **tiled inference** below; graceful "insufficient VRAM — run on CPU?" fallback. Render tiles to fit bounded VRAM regardless. |
| **Disk full (import/export/cache)** | pre-flight free-space check | Atomic temp-then-rename writes; cache LRU eviction reclaims space; import second-copy verified by checksum; partial export cleaned up. |
| **inferd process crash** | supervisor heartbeat / exit code | Restart inferd; in-flight inference job fails gracefully with retry then CPU-EP fallback; editor session untouched. |
| **XMP / catalog divergence** | hash compare on fs-watch | Surface a badge; resolve via explicit read-metadata / write-metadata; catalog stays source of truth (no silent overwrite). |

---

## 7. Performance budgets (tied to the quality bar)

> **v2.0:** the DAM-scale rows below — **"import 10k raws"**, **"cull at keyboard speed"**, and **"catalog interactive at 100k+"** — are **superseded** (no library). The surviving budgets are the **editing** ones: develop slider **< 100 ms**, develop-open first-render, export throughput (now **working-set batch export**), the memory ceiling, and **crash safety** (rescoped to the edit store, §3.1.1). The filmstrip is a session set (tens–hundreds of files); it inherits the develop-open + preview budgets, not a 100k-grid budget. The 10× scale-check dimensions that were DAM-specific (1M assets, burst-import 100k, ANN index) are likewise moot; **fanout → masks/nodes per image** remains the live scale risk and its named rewrite trigger stands.

| Quality-bar requirement | Budget (mid-range GPU: ~RTX 3060 / M-series base) | How the architecture meets it |
|---|---|---|
| Import 10k raws without blocking UI | grid browsable **< 60 s**; UI input latency unaffected throughout | embedded-preview-first extraction (multi-core, no render); standard previews build as Background jobs; demand-driven grid never waits on them |
| Cull at keyboard speed | flag/rate keystroke → badge **< 16 ms**; next/prev preview swap **< 50 ms** | pre-rendered T1 previews from cache; virtualized grid/filmstrip; auto-advance keystroke coalesced; culling writes are tiny single-row txns |
| Develop renders interactively | slider-to-screen **< 100 ms p95** at fit-view | ROI + preview-res eval; node-cache invalidates only the downstream tail; render scheduler coalesces to latest-wins |
| Develop open (first render) | preview tier visible **< 100 ms**; full render **< 1 s** from raw cache | progressive loading; partially-demosaiced raw cache skips decode+demosaic |
| Catalog interactive at 100k+ | filter/search/smart-collection **< 100 ms p95** | indexed queries; FTS5; ANN vec index; WAL snapshot reads never block on the writer |
| Export throughput | ≥ **20 raws/min** to sized JPEG single-thread, ~2× on GPU; **zero** interactive-canvas starvation | separate Foreground job class; GPU time-sliced against Interactive |
| Memory ceiling | a single 100 MP render stays within bounded VRAM | 256² tiling; RGBA16F working tiles; raw cache 5 GB default, preview cache capped |
| Crash safety | **0** catalog corruptions under `kill -9` | WAL invariant, fault-injection tested |

**These are GPU-path budgets.** The CPU fallback (§4.4) carries a **separate, weaker contract**: interactive editing at **preview resolution only** (target: interactive on an 8-core CPU at fit-view), with **full-resolution CPU renders relegated to background/export jobs that show progress and make no latency promise** (a 100 MP full-graph CPU render is minutes-scale). The <100 ms slider budget is not asserted on the CPU path, by design — selling crash-proof fallback as full-res parity would be dishonest.

**10× scale check (which dimension breaks first):**
- **Users → 1 M assets:** SQLite holds (LR proven to 1 M+); the risk is the **ANN vector index** — mitigation is IVF/partitioned sqlite-vec, tunable. No rewrite.
- **Write throughput → burst import of 100 k:** single-writer serialization is the bottleneck; mitigation is batched transactions (1 txn / N assets). If insufficient, we shard the preview-index writes off the main catalog — named, bounded.
- **Data volume → previews:** capped + LRU; unbounded growth is impossible by construction.
- **Fanout → masks/nodes per image:** a 30-mask image is the stress case; live editing degrades to preview-res + full-on-commit. **Named rewrite trigger:** if a typical wedding-edit's mask count routinely blows the 100 ms budget, we fuse per-mask compositing into a single multi-mask pass — a `lightbox-mask` internal change, not an architecture change.

---

## 8. Testing & CI strategy

| Layer | Test type | Gate |
|---|---|---|
| Per-crate logic | Unit (catalog DAO, rule-AST→SQL, recipe (de)serialize, XMP mapping, color transforms) | PR-blocking |
| Recipe / XMP round-trip | Property tests (`proptest`): serialize→deserialize identity; unknown-field preservation; `crs:` import fidelity | PR-blocking |
| **Render correctness** | **Golden-image tests**: fixed raw corpus × recipe set → compare to committed goldens within **ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB**, **per process version**, **CPU vs GPU parity** within the same tolerance | PR-blocking; the process-version immutability guard |
| Catalog crash safety | Fault injection: `kill -9` mid-transaction → `integrity_check` must pass; restore-from-backup drill | PR-blocking (the `kill -9` acceptance test) |
| Performance | `criterion` micro-benchmarks + scenario harness (import 10k, filter 100k, slider latency) asserting §7 budgets | Nightly; regression → issue (non-blocking, tracked) |
| Integration / E2E | `lightbox-cli` drives import → edit → export headless; visual regression on exported JPEG | PR-blocking (fast subset), full nightly |
| **License enforcement — surface 1: Rust crate graph** | `cargo-deny` (license allowlist MIT/BSD/Apache/Zlib/Unicode; **deny GPL/AGPL in the graph**); REUSE/SPDX check; a linkage assertion that LGPL crate deps resolve **dynamically**; a **model-weights license manifest** gate | PR-blocking |
| **License enforcement — surface 2: native / FFI binary SBOM** | A **vendored/dynamically-linked-binary SBOM** enumerating every shipped native artifact (libheif, libde265, LibRaw, lensfun, FFmpeg, ONNX Runtime + its EP binaries, CoreML/DirectML libs, mozjpeg, libjxl, libavif, OpenCV, SQLite, the ISO XMP Toolkit, …) with, **per artifact**: resolved license, actual link mode (must be dynamic for anything LGPL), and — where the license depends on how the binary was **built** — the audited **configure/build flags**. Hard gates: **libheif's HEVC decoder is libde265, never x265/GPL**; **FFmpeg is configured LGPL-only (assert absence of `--enable-gpl`/`--enable-nonfree`)**; **the shipped ORT EP set is DirectML/CoreML/CPU (CUDA/TensorRT are NOT bundled, per §1.5) and each carries an approved license + redistribution terms**; model-weights manifest cross-checked against `model_pack`. Implemented as a build-time SBOM emitter (CycloneDX) + a policy checker over the emitted component list; on the release runners it inspects the *actual* linked/vendored binaries, not a hand-maintained list. | PR-blocking (policy check) + release-blocking (binary inspection) |
| **License enforcement — surface 3: bundled content/data manifest** | A **data-provenance manifest** (§1.7) enumerating every shipped *non-code* asset — the **default look family**, **curated camera-matching profiles**, **HaldCLUT/creative-LUT packs**, **lensfun correction data**, and any bundled DCP — with, **per asset**: source/provenance, resolved content license, and bundled-vs-user-installable status. Hard gates: **zero Adobe-authored profile/`.dcp`/look assets** (constraint 4); every camera-matching profile is Lightbox-generated (dcamprof-from-own-targets) or a cleared CC0/CC-BY community pack; lensfun data ships **with attribution + share-alike honored**; HaldCLUT packs are CC0/CC-BY with attribution. Surfaces 1 and 2 are **structurally blind to data** (a `.dcp` is an open format whose content can be a proprietary asset), so this surface is what actually enforces the "no Adobe assets" half of the mandate. Implemented as a manifest file checked into the repo + a policy checker that fails on any asset lacking a cleared provenance entry, cross-checked against `model_pack` and the shipped asset tree on release runners. | PR-blocking (policy check) + release-blocking (asset-tree inspection) |
| Cross-platform | CI matrix: macOS (Metal/CoreML), Windows (DX12/DirectML), Linux (Vulkan) — build + unit + golden subset | PR-blocking on all three |

The golden-image and license gates are non-negotiable: the first protects the "old edits render forever" promise, the rest protect the permissive-license and no-Adobe-assets mandate. **Why the three-surface split is load-bearing, not bureaucratic:** the licensing/asset risk lives in three places, each invisible to the surface above it. (1) cargo-deny inspects only the Rust crate graph and is *structurally blind* to where the LGPL/GPL *linkage* risk concentrates — a native libheif built against GPL x265, an FFmpeg configured `--enable-gpl`, or an ORT EP binary with restrictive redistribution terms — which live *under* the FFI boundary in how the C libraries were built. **Surface 2** enforces that. (2) Both binary surfaces are in turn *blind to bundled data*: a `.dcp` profile, a HaldCLUT pack, or a look LUT is an open *format* whose *content* can be a proprietary (e.g. Adobe-authored) asset that no crate-graph or binary SBOM tool would ever flag. **Surface 3** enforces the "no Adobe assets" mandate (constraint 4) and the CC-BY-SA/CC-BY attribution obligations on shipped color/profile data. Crediting cargo-deny alone with containing "the existential licensing risk" (Risk 1) would be an overclaim; the "enforced in CI" claim in §0/Risk 1 is defined as **all three surfaces green**.

---

## 9. Milestone plan M0–M4

Each milestone is a **shippable, demoable increment** with concrete exit criteria.

### M0 — Walking skeleton
Rust workspace; `lightbox-core` headless façade + egui shell that opens a catalog, imports files (add-in-place), extracts embedded previews, shows a **virtualized grid + loupe**; SQLite schema v1 + WAL + exit-time verified backup. **Crucially, the loupe canvas routes through one real `RenderNode` evaluated by the actual `lightbox-render` `Engine` on the shell's shared `Arc<wgpu::Device>`** — even if that graph is a single node (a display-transform node over the **decoded embedded-preview buffer** — E03; M0 carries **no raw decode/demosaic**, so E01 takes no dependency on E02, consistent with the epic-table dependencies). We do **not** hardcode a "decode → display transform" blit path that bypasses the engine.
**Why this shape for the skeleton (de-risks the #2 bet a full milestone early):** the two scariest integrations in the whole product are (a) egui custom-paint compositing a texture the render engine produced on the *same* wgpu device (the zero-copy seam, §2.3 seam 2) and (b) the `RenderNode`/DAG/cache trait boundary itself (§4.2). A blit-path skeleton exercises only (a) and defers (b) to M1/E05. Threading even a one-node graph through the real `Engine::submit`/`poll` at skeleton time proves the trait boundary and the shared-device handoff **together**, so E05 grows the DAG rather than discovering its seam is wrong.
**Exit:** import 1 k raws; browse grid + loupe with no UI stall; the loupe image is produced by `Engine::submit` returning a texture composited zero-copy in the egui frame; `kill -9` mid-import leaves the catalog `integrity_check`-clean.

> **v2.0 re-sequence (develop-first).** The v1.x milestones below M0 were *library-first* (M1 = "library complete", develop was a foundation slice). v2.0 inverts this: **the next milestone puts a draggable file on screen with working basic-tone editing.** M1 is now the **develop skeleton**; the DAM milestones are cut. M0 (built) stands unchanged — its walking-skeleton shape (one real `RenderNode` through the real `Engine` on the shared device) is exactly the substrate the develop-first M1 grows from. The four v2.0 milestones follow; the retired v1.x M1–M4 text is superseded by this block.

### M1 — Develop skeleton: a dropped file with basic-tone editing (v2.0 headline)
**Drag-drop / open-dialog entry (§2.4) → filmstrip → loupe → basic-panel editing that persists.** Working-set loader (E04, repurposed from the ingest walker) turns a dropped file/multi-file/folder/recursive-folder or an Open dialog selection into a session working set; the editor shell (E08, repurposed from the grid shell) renders the empty-state drop zone, the filmstrip, and the loupe. Real decode + **license-clean camera-matrix base color + the Lightbox-authored default look** (E02.1/E02.3/E02.4) so every camera renders correct color with no bundled profile; the **render node-graph engine** (E05) grows from the E01 seed; the **basic panel** (WB, exposure/contrast/highlights/shadows/whites/blacks, tone curve) at **process version 1** as the E10.1 slice; edit-state + history + snapshots (E09) auto-persist to the edit store so reopening the file by content-hash restores its recipe; preview pyramid + raw cache (E03) for fast develop-open; jobs (E06). Raw-vs-non-raw develop surface adapts per §2.4.
**Exit:** drop a raw (and a JPEG) → it appears in the filmstrip and loupe within the preview budget; basic-panel slider **< 100 ms at fit-view**; edits auto-persist and survive `kill -9` (edit-store `integrity_check` clean) and app restart; non-raw source hides the raw-only panels.

### M2 — Full develop + color + export
curated in-house **camera-matching DCP profiles** (E02.5 content line) layered over the M1 matrix base + default look; **clean-room RCD demosaic** replaces the interim path (E11.1); **halo-free highlight/shadow recovery** (E10.2) + full global toolset (HSL, color grading, curves, B&W, vibrance/sat, presence: clarity/texture/dehaze); detail (sharpen, luma/chroma NR); optics/geometry (lensfun + LCP + opcodes, CA/defringe, crop/rotate, upright/transform, post-crop vignette/grain); display + export color management; presets (incl. read LR `.xmp`); copy/paste/sync across the working set; before/after. Export engine (JPEG/PNG/TIFF/DNG/JXL/AVIF, sizing, output sharpening, watermark, metadata filtering, presets, external-editor round-trip).
**Exit:** global develop feature-complete for the Must set; **batch-export the working set** without starving the canvas; XMP develop round-trips.

### M3 — Masking + local + AI
Unified mask model (brush, linear/radial gradients, luminance + color range, boolean add/subtract/intersect); full per-mask slider set; mask management/overlays/pins/amount; non-destructive heal/clone + editable retouch objects (+ red-eye, visualize spots); **`lightbox-inferd`** ONNX host + model-pack manager + VRAM gating (E13); **Select Subject / Sky / Background / Objects** (SAM2/BiRefNet/SegFormer) with baked-raster AI masks (E14, AI-masking only — semantic search/faces cut with the DAM); **AI Looks — image-adaptive cinematic grading** (E17, `lightbox-looks`): the CPU palette/tonal analyzer, parameterized look families fitted to the image, seeded shuffle/variations, proposal thumbnails through the E05 engine, and one-step recipe-patch apply via E09 — classical analysis at M3, the optional E13 ML palette/mood classifier riding the inference host that lands this same milestone. (E17 depends only on E09+E10, both complete at M2; it is scheduled at M3 to land alongside the optional ML hook, but does not depend on the rest of M3's masking work.)
**Exit:** AI-masked local adjustments render interactively; masks serialize to recipe + XMP; inference crash is isolated and recovered; **AI Looks proposes ≥N image-fitted cinematic grades, shuffles coherently, and applying one produces an ordinary fully-editable recipe undoable in a single step** (offline, no inference host required for the classical path).

### M4 — v1.0 hardening + XMP interop + core Should tier
XMP `.xmp`/`crs:` **read interop** (open a file, honor an existing sidecar recipe approximately per Risk 9 — **no `.lrcat` catalog-migration importer**, cut with the DAM); content-aware remove (LaMa); secondary display; smart previews / offline-editing proxy; edit-store repair/restore flow; performance hardening to budget; cross-platform packaging; CI license/golden gates green. (Neural raw denoise ships as a **v1.x** flag per Risk 8.)
**Exit — the mandate success scenario (v2.0):** open a 500-raw shoot folder (drop or Open) → color-correct and enhance the keepers with global + AI-masked local adjustments and retouch → apply a look across the working set → **batch-export delivery JPEGs, entirely offline on a mid-range laptop.** All Must + core Should editing features shipped.

---

## 10. Epic breakdown (v2.0 editing-first + v2.1 — Must + core Should)

### 10.0 E01 disposition audit (what the built code becomes — no deletion planned)

E01 is fully implemented and `kill -9`-verified (~19k LOC across 16 crates). v2.0 changes scope, not the foundation. **No code is deleted by this document**; each built surface is dispositioned **retained-as-is**, **retained-repurposed**, or **retired-dormant** (feature off, code left in place). This audit is the authoritative map from the built M0 to the v2.0 epics.

| Built E01 surface | Disposition | v2.0 role / repurpose target |
|---|---|---|
| `lightbox-types` (id newtypes, `ContentHash`, `Orientation`, `ProcessVersion`, `SourceTier`) | **retained-as-is** | unchanged; `ContentHash` is now the edit-store primary key (§3.1.1) |
| `lightbox-catalog` (SQLite/WAL, migrations, verified backup, integrity, DAOs) | **retained-repurposed** | reframed as the **edit store + cache index** (§3.1.1); DAM tables keep-dormant; crate name kept (no churn) |
| `lightbox-jobs` (`Class`, `CancelToken`, `JobSystem::spawn`) | **retained-as-is** | the E06 seed; unchanged |
| `lightbox-render` (`GpuContext`, `Engine` submit/poll, `RenderNode`/`NodeRegistry`, one real GPU node + golden harness) | **retained-as-is** | the E05 seed; E05.1 generalizes the one-node graph to the full DAG |
| `lightbox-decode` (`probe`/`read_embedded`/`hash_file` frozen; `decode_*` declared) | **retained-as-is** | `probe` gains the `SourceKind` flag (§2.4); real decode is E02 |
| `lightbox-preview` (`PreviewProvider`, embedded-JPEG extraction) | **retained-repurposed** | feeds the **filmstrip** (session set), not a 100k grid; rendered producers plug in at M1 (E03) |
| `lightbox-shell` (**virtualized grid** + loupe, shared-device zero-copy composite) | **retained-repurposed** | the **grid virtualization machinery → the filmstrip**; loupe stays the editor canvas; **drop-zone + develop chrome are added** (E08) |
| `lightbox-ingest` (walk / probe / hash / batch-insert; add-in-place import) | **retained-repurposed** | walker/probe/hash → the **working-set loader** (E04); the managed-import / `import_session` path is **retired-dormant** |
| `lightbox-core` (`Core`/`Session`, `Command`/`Event` bus, `Queries`) | **retained-as-is** | headless boundary unchanged; commands gain "open working set" (E04/E08) |
| `lightbox-edit` (`Recipe { schema, pv }` seed) | **retained-as-is** | E09 grows the full recipe |
| `lightbox-cli` (create→import→list→render→check→backup e2e) | **retained-repurposed** | `import` becomes `open`-a-path; headless harness otherwise unchanged |
| `lightbox-color` / `lightbox-meta` / `lightbox-mask` / `lightbox-ml` / `lightbox-export` (declared stubs) | **retained-as-is** | grown by E02/E09/E12/E13/E15 respectively |
| **DAM schema tables** (folder/collection/keyword/label/flag/rating/embedding/face/import_session/FTS — §3.1.1) | **retired-dormant** | present in schema, never written; feature cut, code not deleted |
| **CI gates** (3-surface license, golden-image, fault-injection, cross-platform matrix) | **retained-as-is** | unchanged — the license/color/crash mandate is untouched by the scope cut |

### 10.1-preamble Epic set, sizing, and streams (v2.0)

**15 live epics + 1 retired.** Dependencies reference epic ids. Effort is stated honestly in **person-weeks (pw)**, excluding integration slack. Four epics remain **VHigh multi-week pixel-engine sub-projects** (E05, E10, E11, E12) with internal phases in §10.1 — **the hard imaging work is unchanged by the scope cut; all v2.0 savings come from cutting the DAM, not from making the engine cheaper.** **(v2.1 adds one epic — E17 AI Looks — riding the existing recipe/color foundation, not the pixel engine.)**

**Streams (v2.0, + v2.1).** The v1.x catalog/DAM stream collapses: E07 is retired; E04/E08 shrink to the entry model. The live streams are a **pixel-engine stream** (E05/E10/E11/E12, unchanged, the critical path), an **editor-shell stream** (E04/E08 entry model + UI), a **decode/color/edit stream** (E02/E03/E09/E15), and an **ML stream** (E13/E14). **v2.1 adds a small looks stream (E17) that hangs off the decode/color/edit stream** — it consumes E09 recipes + E10 color tools and (optionally) the E13 inference host; it touches **neither** the pixel-engine critical path nor the render engine's device. The **pixel-engine stream still requires ≥2 GPU/imaging-capable engineers** — this commitment is unchanged from v1.x (§10.1); it is the reason the timeline is engine-gated, not DAM-gated. E17 does **not** add to that GPU-engineer requirement (classical analysis + recipe synthesis, one generalist).

**Effort recompute (meaningfully smaller — then v2.1 adds one leaf epic).** v1.x summed to ~130–155 pw. v2.0 removes the DAM: **E07 retired (−~6.5 pw)**, E04 shrinks (ingest→working-set loader, ~5→~2.5 pw), E08 shrinks (library-UI→editor-shell, no 100k grid/culling/compare-survey, ~9→~7.5 pw), E14 shrinks (drop CLIP search + faces, ~7.5→~5 pw), E16 shrinks (drop `.lrcat` migration, ~8→~5.5 pw). **v2.0 total ≈ 105–120 pw**, of which E01 (~5 pw) is **already banked**. **v2.1 adds E17 (AI Looks, ~6–8 pw)** → **v2.1 total ≈ 111–128 pw; remaining future effort ≈ 106–123 pw**. The pixel-engine stream (E05/E10/E11/E12 ≈ 42–62 pw) is **untouched by v2.1** and remains the wall-clock driver — E17 rides the recipe/color foundation and parallelizes against the engine, so it does not extend the critical path.

| id | slug | title | effort | milestone | depends_on |
|---|---|---|---|---|---|
| E01 | foundation-workspace-catalog | Foundation: workspace, edit-store, headless core, shell skeleton (incl. one real RenderNode through the Engine, §9 M0) — **BUILT** | **M** ~4–6 pw *(done)* | M0 | — |
| E02 | decode-color-foundation | Decode & color foundation (rawler + LibRaw sandbox; camera-matrix base + DCP parse/evaluate; **Lightbox-authored default look**; LCMS2 + working/display/output color; **curated camera-profile content line** — decomposed in §10.1) | **XL** ~9–14 pw | M1 | E01 |
| E03 | preview-cache-pyramid | Preview pyramid & raw cache (rescoped: filmstrip + fast develop-open, not a 100k grid) | **M** ~3–5 pw | M1 | E01 |
| E04 | working-set-loader | **Working-set loader & drag-drop intake** (repurposed from the ingest walker): walk single/multi/folder/recursive-folder + open-dialog → session working set; probe/hash/order; **no managed import** | **S** ~2–3 pw | M1 | E01, E03 |
| E05 | render-node-graph-engine | Render node-graph engine — **VHigh, decomposed in §10.1** | **XL** ~10–16 pw / 2 eng | M1 | E01, E02 |
| E06 | jobs-background-system | Jobs & background task system | **S–M** ~3–4 pw | M1 | E01 |
| E07 | catalog-dam | **RETIRED (v2.0).** DAM core (folders/collections/smart-collections/keywords/filter+FTS/relink/metadata-editor) — the entire library concept is cut. The minimal EXIF info panel folds into E08; XMP field round-trip rides E09's passthrough. | *(retired)* | — | — |
| E08 | editor-shell | **Editor shell, drag-drop entry & develop UI** (repurposed from library-UI/culling): empty-state drop zone, filmstrip (from the grid machinery), loupe, develop panels + on-canvas gizmos, keymap, **performance preferences panel** — binds cache caps/GPU/VRAM/job knobs (§3.3/§6/E06). Grid/culling/compare-survey/smart-collection UI **retired**. | **L** ~7–9 pw | M1 | E01, E03, E04, E05, E09 |
| E09 | edit-state-history-presets | Edit state, history, presets, XMP (CBOR recipe, **ISO 16684 XMP Toolkit RDF engine + own `crs:`/`lb:` mapping — §1.6**, `crs:` read, presets; **content-hash keyed, auto-persist on open/edit** per §3.1.1; **recipe-patch merge `Recipe::apply_patch` as one undoable step** — the E17 look-layering contract, §2.2) | **M** ~4–6 pw | M1 | E01 |
| E10 | develop-global-toolset | Develop — global toolset incl. **halo-free highlight/shadow recovery** — **VHigh, decomposed in §10.1** (E10.1 basic-panel slice lands at M1, the develop-skeleton headline) | **XL** ~10–14 pw | M2 | E02, E05, E09 |
| E11 | detail-optics-geometry | Detail, optics, geometry & **clean-room demosaic** — **VHigh, the single hardest item, decomposed in §10.1** | **XL** ~12–18 pw / GPU-imaging eng | M2 | E02, E05 |
| E12 | masking-local-and-retouch | Masking, local adjustments & retouch — **VHigh, biggest pixel-engine lift after demosaic, decomposed in §10.1** | **XL** ~10–14 pw | M3 | E05, E10 |
| E13 | ml-inference-platform | ML inference platform (`lightbox-inferd`, ONNX + **DirectML/CoreML/CPU EPs — CUDA/TensorRT optional user-installed, not bundled, §1.5**, IPC, model-pack manager, VRAM gating, supervision) | **L** ~6–9 pw | M3 | E01, E06 |
| E14 | ai-masking | **AI masking** (rescoped from ai-masking-and-search): SAM2/BiRefNet/SegFormer Select Subject/Sky/Background/Objects; baked-raster masks (§4.5). **CLIP semantic search + faces/people + auto-tag retired** with the DAM. | **M–L** ~4–6 pw | M3 | E12, E13 |
| E15 | export-output | Export & output engine (incl. **batch-export the working set**) | **M–L** ~5–7 pw | M2 | E05, E06, E09 |
| E16 | interop-hardening | **Interop & v1 hardening** (rescoped from interop-migration-hardening): XMP `.xmp`/`crs:` **read** interop, LaMa content-aware remove, packaging, CI gates, perf. **LR `.lrcat` catalog-migration importer retired** (no catalog to migrate into). | **M–L** ~4–7 pw | M4 | E09, E14, E15 |
| E17 | ai-looks | **AI Looks — image-adaptive cinematic grading (v2.1)** (`lightbox-looks`): CPU-side palette/tonal analyzer over preview tiles; parameterized **look families** (teal-orange, film-stock, bleach-bypass, …) **fitted to the image's actual colors**; each proposal a **partial develop recipe** (color-grade wheels + curves + HSL deltas) merged onto the current edit via E09 as one undoable step, **fully editable after**, never a baked filter; seeded **shuffle/variations**; proposal thumbnails render through the E05 engine. Classical analysis first; **optional** ML palette/mood classifier via E13 (not a hard dependency). — decomposed in §10.1 | **L** ~6–8 pw | M3 | E09, E10 *(E13 optional)* |

**Sizing/dependency notes (v2.0):**
- **E08 (editor-shell) is re-baselined *down* from v1.x E08 (~7–11 → ~7–9 pw).** The 100k virtualized grid, culling grammar, and compare/survey are cut; the filmstrip reuses the built virtualization machinery (§10.0) bounded to a session set. What it *gains* — drop-zone, develop-panel chrome, raw-vs-non-raw surface (§2.4) — is net smaller than the DAM UI it sheds. The **reversal trigger stands** (§1.2/§2.3 seam 1): if egui's widget ceiling is hit, swap `lightbox-shell` for a Qt/Slint shell against the same headless core — a bounded single-crate rewrite.
- **E09 depends on the frozen recipe schema (§3.2), not on the render engine (E05).** Unchanged from v1.x. The CBOR recipe + XMP mapping build and property-test against §3.2 before the Engine exists, so the decode/color/edit stream starts at M1 alongside the pixel stream. v2.0 adds content-hash keying + auto-persist (§3.1.1) — additive, not a re-architecture.
- **E04 (working-set-loader) is the smallest new-scope epic.** It is the ingest walker minus managed copy/second-copy/import-sessions, plus drop-path/open-dialog/recursive-folder intake into a session set (§2.4). Deployment: additive over E01; rollback drops no migrations (the working set is not persisted).

### 10.1 VHigh sub-project decomposition (internal phases)

These epics are **not** 1–4-week units and must not be spec'd as such. The four pixel-engine epics (E05, E10, E11, E12) are each a multi-week, multi-person sub-project with its own internal phases, an explicit quality gate, and (for demosaic and highlight recovery) an algorithm spike that precedes implementation. **E02 is decomposed here for a different reason** — it is not algorithmically VHigh, but it carries the camera-color base, the Lightbox default look, and an ongoing camera-profile *content-production* line that a planner would otherwise under-budget or fill by guessing (the §1.7 / Risk 10 gap). Planners spec all of these at the **phase** granularity below.

**E05 — Render node-graph engine (XL, ~10–16 pw, 2 engineers incl. one GPU-depth). Bet #2 of the whole product (§0); Risk 4.**
- **E05.1 Graph & trait spine** — **generalizing the minimal one-node Engine seed E01 stands up at M0 (§9)** into the full spine: petgraph DAG, `RenderNode` trait (typed image ports, tile handles), `Engine::submit`/`poll` async lifecycle. (E01 owns the seed trait + shared-device handoff; E05.1 owns the DAG/multi-node generalization — no double-build.) First failing test: a single-node graph renders a known input to a committed golden within ΔE2000 ≤ 1.0.
- **E05.2 Content-keyed cache & tail invalidation** — cache key = hash(node_id, pv, input_hashes, params); one param change invalidates only that node + its downstream tail. Test: changing one node's params recomputes only that node and its tail, asserted by a recompute-count probe.
- **E05.3 ROI / tile / progressive eval** — preview-res + visible-ROI evaluation; 256² tiling for 1:1; progressive preview→full. Test: a 45 MP raw at fit-view evaluates ≤ ~8 MP.
- **E05.4 Process-version registry** — `(node_id, process_version)` keying; old PVs registered forever; wired into the per-PV golden immutability guard (§4.5/§8).
- **E05.5 Device-lost recovery + CPU-fallback harness** — rebuild/re-upload; degrade to **preview-res CPU** per §4.4. Test: an injected device-lost event keeps the session editing at preview resolution.

**E02 — Decode & color foundation (XL, ~9–14 pw). Not algorithmically VHigh, but decomposed here because it carries a distinct, previously-unbudgeted *content-production* line (camera profiles + default look) that must be visible to a planner — closes the §1.7 / Risk 10 gap.**
- **E02.1 Decode + camera-matrix base color** — rawler primary + LibRaw sandbox fallback; linearization/black-white; extract `ColorMatrix1/2` / `ForwardMatrix` and build the **license-clean colorimetric base** (§1.7 tier 1). First failing test: a known raw renders correct color through the matrix base with **zero bundled profile assets present**.
- **E02.2 DCP parser + evaluator** — own parser/evaluator for curated + user DCPs (dual-illuminant matrix ⊕ HueSat LUT ⊕ profile tone curve). Test: a Lightbox-generated DCP round-trips through parse→evaluate within ΔE tolerance against its dcamprof reference.
- **E02.3 Color management** — LCMS2 working/display/output transforms; fixed ProPhoto-linear working space; per-monitor ICC.
- **E02.4 Lightbox default look family (authoring + code)** — the **original** default tone curve + hue/sat shaping (§1.7 tier 2), the "Adobe-Color-analogue default" the Must feature requires, authored by us. Includes a perceptual review pass. **Deliverable is original content under the project license — no reverse-engineered Adobe LUT.** Gate: reviewed against a neutral scene corpus; provenance entry lands in the §8 surface-3 manifest.
- **E02.5 Curated camera-matching profile pipeline (CONTENT LINE — ongoing, not one-time code).** Stand up the **in-house dcamprof (GPL subprocess) generation pipeline** from **our own ColorChecker/IT8 target shots**, produce profiles for a **curated top-N of popular bodies**, and register each in the surface-3 data manifest. **This budgets the real, previously-unbudgeted cost** (target-shot capture + generation/verification per body) and is explicitly a *scale-with-investment content line*, not a bounded code task; bodies outside the curated set fall back to E02.1's matrix base + E02.4 look, with the gap stated to the user (Risk 10). No Adobe `.dcp` is ever bundled.

**E10 — Develop global toolset (XL, ~10–14 pw). Contains a quality-defining sub-project.**
- **E10.1 Basic-panel scaffolding** — exposure/contrast/whites/blacks/WB as nodes on the E05 engine.
- **E10.2 Halo-free highlight/shadow recovery — the quality-defining sub-project (~4–6 pw on its own; §4.1, Risk 3).** Local-Laplacian / guided-filter tone mapping where the *halo-free* requirement is the hard part: it needs its own algorithm spike, a dedicated golden corpus of clipped-highlight / deep-shadow scenes, and a perceptual review pass, budgeted separately from the rest of E10. CPU+GPU parity within the §4.4 tolerance is the exit gate.
- **E10.3 Curves (parametric + point), HSL 8-band, color grading.**
- **E10.4 B&W mix, presence (clarity/texture/dehaze), creative LUT profiles, histogram.**

**E11 — Detail, optics, geometry & clean-room demosaic (XL, ~12–18 pw, needs a GPU/imaging specialist). Contains the single hardest, most quality-defining engineering item in the product; Risk 2.**
- **E11.1 Clean-room demosaic — a project unto itself (~6–10 pw of the epic; Risk 2).** RCD + AMaZE-class Bayer demosaic reimplemented **from the published papers only** (RawTherapee/darktable source is GPL — study only, never copied), in WGSL + CPU. The acceptance bar is a **golden-image quality gate against a reference corpus** that the clean-room result must beat the interim LibRaw AHD/DCB floor on (false-color, zippering, maze artifacts). This phase carries its own algorithm spike before implementation. X-Trans Markesteijn is a **Should**, phased *after* Bayer ships.
- **E11.2 Capture sharpen + luma/chroma NR + moiré.**
- **E11.3 Lens corrections** — lensfun + LCP + DNG opcodes (WarpRectilinear, GainMap, FixBadPixels); CA/defringe.
- **E11.4 Geometry** — crop/rotate/flip, upright/transform, post-crop vignette/grain.

**E12 — Masking, local adjustments & retouch (XL, ~10–14 pw). The biggest pixel-engine lift after demosaic; Risk 4.**
- **E12.1 Mask engine core — the multi-week pixel-engine phase.** Unified grayscale-weight-buffer model; brush / linear / radial / luma-range / color-range component evaluators on GPU; boolean add/subtract/intersect compositing.
- **E12.2 Per-mask sub-recipe application & mask-order compositing** into the §4.1 LOCAL stage; the 30-mask stress case and the named fuse-to-single-multi-mask-pass rewrite trigger (§7).
- **E12.3 Mask management** — overlays/pins/amount UI + on-canvas gizmos.
- **E12.4 Non-destructive retouch** — heal/clone via **OpenCV `seamlessClone`** + own PatchMatch for structural fill; editable retouch objects; red-eye; visualize spots. (AI-driven `Select Subject/Sky/…` is E14; the baked-raster `AiSeg` component consumes this engine, §4.5.)

**E17 — AI Looks: image-adaptive cinematic grading (L, ~6–8 pw). v2.1 headline differentiator (§0). Not pixel-engine work — it synthesizes recipes, it does not add render nodes. Decomposed here because its design decisions are load-bearing and a planner must not guess them.**

- **E17.1 Image-stat analyzer — CPU-side, over preview tiles (the placement decision, made).** Palette + tonal analysis lives in **`lightbox-looks` as a CPU-side analyzer over the existing T1 preview tiles (E03)**, **not** as a new stats node in the render DAG. Rationale, named against the alternative: a DAG stats node would couple look analysis to the hot render path, force a device round-trip for a once-per-image computation, and re-run on every param change for no benefit — analysis is needed *once when a look palette is requested*, over a downsampled preview (a few hundred-K pixels), where classical k-means/histogram work is sub-frame on CPU. Where the engine *already* computes a histogram for the develop UI, the analyzer reads it rather than recomputing. Output: `ImageStats { palette (OkLab k-means swatches), tone histogram, temp/tint estimate, skin-present flag }` (§2.2). First failing test: `analyze()` on a known image returns a stable palette + tone signature within tolerance, with **no GPU device acquired**.
- **E17.2 Look families + fitting (the synthesis model, made).** Look families are **parameterized templates** (teal-orange, film-stock emulations, bleach-bypass, warm-fade, high-contrast neutral, …), each a function `fit(family_params, ImageStats) → RecipePatch` producing **color-grade wheel offsets + tone-curve control points + HSL band deltas** — never pixels, never a LUT bake. Fitting *anchors the grade to the image's actual palette/tone* (e.g. teal-orange places its warm pole near the image's detected skin/highlight hue and its cool pole in the complementary shadow region, rather than applying a fixed rotation), which is what "adaptive" means and what a static preset LUT cannot do. This is the novel work; it is classical parameterized synthesis, no model required. Gate: a curated scene corpus × families reviewed perceptually; every emitted patch is a valid partial `Recipe` and round-trips through E09.
- **E17.3 Seeded shuffle / variations.** A `seed: u64` drives coherent random exploration — `propose(stats, families, seed, n)` yields *n* varied-but-plausible grades; re-rolling the seed gives a new coherent set; a chosen proposal's seed is recorded so it is reproducible. Test: same `(stats, seed)` → identical proposals (determinism); different seeds → distinct, still in-gamut, grades.
- **E17.4 Proposal preview + apply (the E09 layering contract, made).** Proposals are previewed as **thumbnails rendered through the normal E05 `Engine`** — each thumbnail evaluates *the current recipe merged with the proposal's `RecipePatch`* at thumbnail resolution, so the preview is exactly what applying will produce (no separate look renderer). Applying is **`Recipe::apply_patch(&RecipePatch)` in E09 (E17.4 depends on the E09 contract, §2.2 / epic table)**: the patch merges onto the user's current edit as **one undoable history step**; afterward it is an ordinary recipe — the user can open the color-grade/curve/HSL panels and adjust every value the look set. **A look is never a baked filter and never an opaque layer** — this is the mandate's hard requirement and the acceptance test: after applying a look, editing any color-grade wheel it touched behaves identically to having set that wheel by hand, and one undo removes the whole look.
- **E17.5 Optional ML hook (via E13, NOT a hard dependency).** A small palette/mood classifier (ONNX, ships as an optional model pack) can *bias family selection and weighting* toward the image's content (e.g. bias film-stock families for portraits, teal-orange for landscapes) through `propose_ml(stats, infer, seed, n)` calling the standard `InferenceClient` (§2.2). **If `lightbox-inferd` is absent, the model pack is not installed, or VRAM-gated off, E17 falls back to classical family selection with no feature loss** — the ML hook only *reorders/weights* proposals, it never gates the feature. This keeps E17's hard dependencies at E09 + E10; E13 is an optional enhancer, honoring "models only where they earn their footprint." Test: with inferd unavailable, `propose()` still returns a full, coherent proposal set.

**Deployment/rollback story for E17.** `lightbox-looks` is a new leaf crate; E17 also adds the **"AI Looks" develop panel** (proposal grid + shuffle button) **using E08's existing develop-panel framework** (E08 shipped at M1 — E17 consumes that chrome; E08 does **not** depend on E17, so milestone order is preserved) behind a **feature flag**. Ship it additively over M2's completed E09+E10 (and the E05 engine already live for thumbnail renders); roll back by reverting the E17 crate + the panel wiring and **dropping no migrations** — look proposals are transient until the user applies one, and an *applied* look is just an ordinary recipe patch already persisted by E09's normal path, so there is no E17-owned schema to unwind. Blast radius: the develop UI's new panel only; the recipe format, engine, and edit store are untouched (E17 emits the *existing* `RecipePatch` shape).

**Deferred to v1.x (design headroom preserved, no v1 engineering):** neural raw denoise, super-resolution, HDR edit/merge + gain-map export, soft proofing, publish-services framework, tethered capture, video trim/grade, map/geotag module, People body-part parsing, generative (diffusion) remove, plugin SDK, depth masks.

---

## 11. Risk register

> **v2.0:** the pixel-engine and license risks (1, 2, 3, 4, 6, 7, 8, 9, 10) are **unchanged** — the scope cut does not touch the hard imaging/color/licensing work. **Risk 5 (catalog corruption on `kill -9`)** survives, **rescoped to the edit store** (§3.1.1) — it remains the surviving member of the "three bets". The DAM-scale worries folded into §7's superseded rows (1M-asset ANN index, burst-import) are **retired with the library**. **Risk 9 (LR migration fidelity)** narrows: `.lrcat` catalog migration is cut (E16), so only **XMP `crs:` read** interop carries the approximate-develop-settings caveat.

The risks the rest of this document references by number. Each names the failure mode, the epic/section that carries it, and the mitigation the design commits to. Two of these (Risk 2, Risk 4) are the reason four pixel-engine epics are sized as VHigh sub-projects in §10.1; two (Risk 1, Risk 10) are the reason the CI license gate is a **three-surface** gate in §8 — crate graph, native binaries, **and bundled content/data**.

| # | Risk | Where it lives | Severity | Mitigation (this document commits to) |
|---|---|---|---|---|
| **1** | **GPL/LGPL license contamination — the existential licensing risk.** A single GPL binary linked in-process (or an LGPL binary built against a GPL codec) revokes the permissive-distribution mandate. Concentrated *under the FFI boundary*, invisible to crate-graph tooling. | §1.6, §8 | Existential | **Three-surface CI gate (§8):** cargo-deny over the Rust crate graph **plus** a native/FFI binary SBOM with per-artifact license + build-flag audit (libheif→libde265 not x265; FFmpeg LGPL-only; ORT ships DirectML/CoreML/CPU, CUDA not bundled; model-weights manifest) **plus** the bundled content/data manifest (Risk 10). "Enforced in CI" = all three surfaces green. |
| **2** | **Clean-room demosaic quality — the single hardest, most quality-defining engineering item in the product, and a project unto itself.** The state-of-the-art demosaicers exist only as GPL-3 source (study-only); we must reimplement RCD/AMaZE-class from papers and *beat* the LibRaw floor. | E11.1 (§10.1) | Very high | Budgeted as a **~6–10 pw sub-project with its own algorithm spike** ahead of implementation; acceptance = a golden-image quality gate against a reference corpus (false-color/zippering/maze) beating the interim LibRaw AHD/DCB floor. Sized honestly in §10.1, never as a 1–4-week unit. |
| **3** | **Halo-free highlight/shadow recovery** — the quality-defining hard part of the *basic* panel; naïve local-Laplacian/guided-filter tone mapping halos on clipped-highlight and deep-shadow scenes. | E10.2 (§10.1) | High | Budgeted as its **own ~4–6 pw sub-project** inside E10 with a dedicated clipped/shadow golden corpus + perceptual review; CPU+GPU parity within §4.4 tolerance is the exit gate. |
| **4** | **Node-graph engine + mask engine complexity** — bet #2 (§0). The DAG/cache/ROI engine (E05) and the mask engine (E12, the biggest pixel-engine lift after demosaic) are each multi-week, multi-person builds; under-resourcing either sinks the <100 ms interactivity promise. | E05, E12 (§10.1) | Very high | Both decomposed into internal phases in §10.1 (E05.1–E05.5, E12.1–E12.4), sized XL, and assigned to the GPU/imaging-capable pixel-engine stream under the staffing assumption in §10. |
| **5** | **Catalog corruption on `kill -9`** — the #1 trust-killer that broke loyalty for ON1/Luminar. | §3.1, E01/E07 | High | WAL + `synchronous=NORMAL` (torn writes impossible) + exit-time verified backup + copy-on-write schema upgrades; fault-injection tested as a PR-blocking gate (§8). |
| **6** | **GPU driver crash / device-lost mid-session.** | §6, E05.5 | Medium | Rebuild/re-upload; on recurrence, drop to **preview-res CPU** (§4.4 degraded contract) and keep editing; export never uses a crashed device. |
| **7** | **AI-mask reproducibility across execution providers and years.** Cross-EP ONNX inference is not bit-deterministic, so re-running a segmentation model does not reproduce an old mask — a *stronger* non-determinism than the render engine surrenders. | §4.5, E14 | High | The segmentation raster is **baked into the immutable recipe** (`cached_raster_ref`) and **replayed**, never silently recomputed; `model_id`/pin are provenance only; recompute is an explicit, user-visible edit (§4.5). The baked raster is the authoritative artifact behind "edits render forever." |
| **8** | **Neural raw denoise scope creep.** A DeepPRIME-class neural denoiser is very-high complexity and would blow the v1 budget if treated as a Must. | §9 M4, deferred list | Medium | Ships as a **v1.x flag**, not v1; classical raw-denoise covers v1. Design headroom preserved (the raw-denoise node slot exists in §4.1) without v1 engineering. |
| **9** | **Lightroom develop-settings migration fidelity.** `crs:` develop settings do not map 1:1 onto our pipeline; and lossless foreign-field / `lr:hierarchicalSubject` round-trip is a classic under-estimate if RDF is hand-rolled. | §3.2, E09/E16 | Medium | Organization metadata migrates **losslessly**; develop settings migrate **approximately with the expectation set explicitly** to the user. RDF read/write uses the **ISO 16684 XMP Toolkit (BSD-3, permitted under mandate v1.1)** behind Lightbox's own `crs:`/`lb:` mapping layer, so lossless foreign-field + `lr:hierarchicalSubject` round-trip rides the ISO reference implementation instead of a hand-roll — converting the classic under-estimate into a **bounded integration cost** (own quick-xml RDF is the named reversal fallback, §1.6). Cost budgeted in E09; unknown fields preserved via `xmp_passthrough`. |
| **10** | **Camera-color coverage & profile-data provenance — a mandate exposure *and* a completeness gap.** Filling the "Adobe-Color-analogue default" Must by bundling Adobe's `.dcp` library would violate constraint 4 (Adobe-authored assets), and neither binary CI surface audits bundled profile/LUT data; without Adobe's per-camera library, manufacturer-look match is a visible gap for uncovered bodies. | §1.7, §4.1, E02.1/E02.5, §8 surface 3 | High | **Three-tier color model (§1.7):** license-clean **camera-matrix base** (ColorMatrix1/2 via rawler — no bundled asset, correct color for every camera) + **Lightbox-authored original default look** (the Must "default family", not Adobe's) + **curated in-house dcamprof-generated camera-matching profiles** (own target shots, GPL subprocess, ongoing content cost budgeted in E02.5). Camera-matching for the full universe is **explicitly downgraded to matrix-base fallback**, gap stated to the user. **No Adobe `.dcp` ever shipped; enforced by §8 surface 3 (content/data manifest).** |

## 12. Open items requiring escalation

- **Irreversible-decision gate — CLOSED (2026-07-04):** Rust as core language (§1.1) and the SQLite on-disk catalog format (§1.4) are near-one-way doors requiring sign-off above the CTO seat. The **mandate-owner ratified both** in the project approval record (`02-approval.md` §4) after the round-4 CTO confirmation. E01 is unblocked.
- **[RESOLVED — mandate-owner ruling, mandate v1.1, 2026-07-04] Adobe XMP Toolkit / XMP RDF engine.** §1.6 previously flagged the Adobe XMP Toolkit as a constraint-4 conflict and escalated it to the mandate-owner. **The ruling is in:** proprietary Adobe *product* code/SDKs stay barred, but **Adobe-published, OSI-licensed open-source libraries** (the ISO 16684 XMP Toolkit BSD-3, c2pa-rs) are **permitted** under the same per-dependency license review as any third-party dependency, each requiring explicit CTO sign-off. **Architecture decision of record (§1.6), made on engineering grounds now that license is unblocked:** the **ISO 16684 XMP Toolkit (BSD-3) is the primary RDF read/write engine**, behind Lightbox's own `crs:`/`lb:` field-mapping layer, with own quick-xml RDF as the named reversal fallback — the reference implementation carries the hardest, most undifferentiated work (lossless foreign-field + `lr:hierarchicalSubject` round-trip, Risk 9) rather than a hand-roll. **Residual action — CLOSED:** the per-dependency CTO license sign-off for the toolkit was **granted in the round-4 confirmation review (2026-07-04)**; see `02-approval.md`. **E09 re-baselined to ~4–6 pw** (§10): the toolkit converts an open-ended hand-roll *fidelity* risk into a bounded FFI-*integration* cost, the better risk profile at comparable effort.
- **security-engineer (design-time, before M3/M4):** the trust boundaries — untrusted raw decode (sandbox), model-pack download integrity, XMP/lrcat import parsing — need a threat-model review as a phase, not a footnote.
- **data-engineer:** the `lightbox-catalog` schema (§3.1) and the LR `.lrcat` migration query plan (§10 E16) are handed off with the entity-ownership map above and the consistency requirement (catalog = single source of truth, XMP = projection).
- **nexus:** the `lightbox-jobs ↔ lightbox-inferd` IPC protocol (§5.2) is a streaming/back-pressure contract — review fan-out and back-pressure before the protocol closes.
- **staff-engineer:** every epic ships to implementation with the seam, data flow, failure modes, and first failing test named in its planner spec.
- **cto / operator (resourcing, not architecture):** two commitments this document makes explicit rather than assumes. (1) The pixel-engine stream requires **≥2 GPU/imaging-capable engineers** (§10); accepting one instead is a valid call but re-baselines the program to ~15–18 months — decide, don't default. (2) **E02.5 is a content-production line, not one-time code** — an ongoing camera-profile pipeline needs an owner and a budget for physical ColorChecker/IT8 target shots per body; without that owner the curated-profile tier does not exist and every camera falls back to the matrix base + default look (Risk 10). Both are headcount/ops decisions outside the architect's seat.
```
