# Lightbox — v1 System Architecture

_Author: system-architect. Input: `00-mandate.md`, `00-feature-catalog.md`, and the 10 domain research reports. This document is decision-complete: epic planners spec implementation from it without re-litigating stack, seams, data model, or budgets. Where a choice is reversible, the reversal trigger is named; where irreversible, it is flagged for CTO/CEO sign-off._

---

## 0. Architectural thesis (the one-paragraph version)

Lightbox is a **headless Rust core** (catalog + decode + render + edit-state + jobs) wrapped in a **thin, replaceable UI shell** that shares one GPU device with the render engine. The core's spine is a **GPU compute node-graph** (the vkdt lesson) that renders a **versioned, serializable edit recipe** against an untouched original. The **SQLite catalog is the single source of truth**; XMP sidecars are an opt-in projection, never a second owner. Everything expensive is a **cancellable job**; the UI thread never blocks and a `kill -9` never corrupts the catalog. AI runs on a **crash-isolated out-of-process ONNX inference host**. The whole dependency graph is **permissively licensed by construction** — GPL never links in-process, enforced in CI across **three** surfaces: the Rust crate graph (cargo-deny), the native/FFI binary surface (a vendored-binary SBOM with per-artifact license + build-flag audit), **and the bundled content/data manifest** (camera profiles, look family, LUT packs, lensfun data — §1.7/§8), because the LGPL/GPL exposure concentrates in the dynamically-linked C libraries and their transitive codecs (invisible to the crate graph) **and** the no-Adobe-assets exposure concentrates in bundled color/profile *data* whose open format hides proprietary content (invisible to both binary surfaces).

The three bets, in priority order: (1) the catalog is crash-proof and interactive at 100k+; (2) the node-graph render engine hits <100 ms slider-to-screen; (3) local AI matches the cloud tier offline. Everything else is assembly of mature building blocks.

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
   └──────────────────┘
```

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

### M1 — Library complete + develop foundation
Full DAM (folders/sync, collections/sets, smart collections, hierarchical keywords, ratings/flags/labels, filter bar + FTS, virtual copies, relink); culling grammar + auto-advance; preview pyramid + demand-driven generation + raw cache; job system + activity center; remappable keymap. Render node-graph MVP with the **basic panel** (WB, exposure/contrast/highlights/shadows/whites/blacks, tone curve) at **process version 1**; **license-clean camera-matrix base color + the Lightbox-authored default look (§1.7, E02.1/E02.4) so every camera renders correct color with no bundled profile**; edit-state model + history + snapshots (buildable against the §3.2 schema ahead of the engine, E09); XMP read/write; two-module (Library/Develop) workspace.
**Exit:** import 10 k raws non-blocking (< 60 s browsable); cull at keyboard speed; basic-panel slider < 100 ms at fit-view; catalog interactive at 100 k.

### M2 — Full develop + color + export
curated in-house **camera-matching DCP profiles** (E02.5 content line) layered over the M1 matrix base + default look; **clean-room RCD demosaic** replaces the interim path; full global toolset (HSL, color grading, curves, B&W, vibrance/sat, presence: clarity/texture/dehaze); detail (sharpen, luma/chroma NR); optics/geometry (lensfun + LCP + opcodes, CA/defringe, crop/rotate, upright/transform, post-crop vignette/grain); display + export color management; presets (incl. read LR `.xmp`); copy/paste/sync; before/after. Export engine (JPEG/PNG/TIFF/DNG/JXL/AVIF, sizing, output sharpening, watermark, metadata filtering, presets, external-editor round-trip).
**Exit:** global develop feature-complete for the Must set; export a 3 k-image batch without starving the canvas; XMP develop round-trips.

### M3 — Masking + local + AI
Unified mask model (brush, linear/radial gradients, luminance + color range, boolean add/subtract/intersect); full per-mask slider set; mask management/overlays/pins/amount; non-destructive heal/clone + editable retouch objects (+ red-eye, visualize spots); **`lightbox-inferd`** ONNX host + model-pack manager + VRAM gating; **Select Subject / Sky / Background / Objects** (SAM2/BiRefNet/SegFormer) with AI-mask recompute/staleness.
**Exit:** AI-masked local adjustments render interactively; masks serialize to recipe + XMP; inference crash is isolated and recovered.

### M4 — v1.0 hardening + interop + core Should tier
Lightroom `.lrcat` + `.xmp`/`crs:` **migration importer**; semantic search (OpenCLIP + sqlite-vec) + auto-tag; face detection + clustering (YuNet/SFace); content-aware remove (LaMa); smart previews; compare/survey; secondary display; corruption repair/restore flow; performance hardening to budget; cross-platform packaging; CI license/golden gates green. (Neural raw denoise ships as a **v1.x** flag per Risk 8.)
**Exit — the mandate success scenario:** shoot → ingest & cull 3 000 raws → develop with global + AI-masked local adjustments → export delivered JPEGs, **entirely offline on a mid-range laptop**. All Must + core Should features shipped.

---

## 10. Epic breakdown (v1 Must + core Should)

16 epics, each individually spec'able by a planner. Dependencies reference epic ids. **Effort is stated honestly, not uniformly** — the earlier "everything is 1–4 weeks" framing was false and directly contradicted this document's own risk analysis (Risks 2 and 4, §4.1). Four epics are **VHigh multi-week, multi-person sub-projects** and are flagged as such with internal phases in §10.1; the rest range from S (~2 pw) to L (~6–10 pw). Sizes are **person-weeks (pw)** of focused engineering, excluding integration/hardening slack.

**Team-size assumption the milestone plan depends on (without which M0–M4 convergence is unassessable).** The dependency graph is deliberately shaped into **parallel streams** — a catalog/DAM stream (E01/E03/E04/E07/E08), a pixel-engine stream (E05/E10/E11/E12), an edit/interop stream (E09/E15/E16), and an ML stream (E13/E14) — and the plan assumes **~4–5 engineers working those streams concurrently**. Total v1 effort sums to roughly **130–155 pw (~2.5–3.0 engineer-years)** (up from the earlier figure with E02's camera-color content line now budgeted honestly, §10.1); at 4–5 parallel engineers that is a **~9–12 month wall-clock** program including hardening.

**The pixel-engine stream requires ≥2 GPU/imaging-capable engineers — this is a hard resourcing commitment, not a preference.** The stream serializes ~42–62 pw of imaging-hard, dependency-chained work (E05 → E10.2 highlight recovery → E11.1 demosaic → E12.1 mask engine). Staffing it with a **single** specialist makes that engineer a **hero-critical path** — a single-point liability the architecture explicitly refuses to bless — and it breaks the 9–12-month wall-clock, because those four sub-projects cannot be parallelized within one person. **The plan therefore commits to ≥2 GPU/imaging-capable engineers on the pixel-engine stream.** If the org can staff only one such specialist, that is an accepted decision but the timeline changes with it: the four sub-projects **serialize**, adding roughly **+4–6 months** wall-clock (a **~15–18-month** program), and the milestone plan must be re-baselined to that longer clock. **What is not permitted is letting the 9–12-month claim ride on an unstated one-hero assumption** — the two-specialist requirement is stated here so the CTO accepts the headcount or the longer timeline explicitly, never by default. **On a single generalist engineer with no GPU/imaging depth this is not a bounded plan at all** — the pixel-engine sub-projects gate M1–M3 and are not general-backend work. The convergence claim in §9 is credible *only* under the ≥2-specialist parallel-staffing assumption above.

| id | slug | title | effort | milestone | depends_on |
|---|---|---|---|---|---|
| E01 | foundation-workspace-catalog | Foundation: workspace, catalog, headless core, shell skeleton (incl. one real RenderNode through the Engine, §9 M0) | **M** ~4–6 pw | M0 | — |
| E02 | decode-color-foundation | Decode & color foundation (rawler + LibRaw sandbox; camera-matrix base + DCP parse/evaluate; **Lightbox-authored default look**; LCMS2 + working/display/output color; **curated camera-profile content line** — decomposed in §10.1) | **XL** ~9–14 pw | M1 | E01 |
| E03 | preview-cache-pyramid | Preview pyramid & raw cache | **M** ~3–5 pw | M0 | E01 |
| E04 | ingest-import-pipeline | Ingest & import pipeline | **M** ~3–5 pw | M1 | E01, E03 |
| E05 | render-node-graph-engine | Render node-graph engine — **VHigh, decomposed in §10.1** | **XL** ~10–16 pw / 2 eng | M1 | E01, E02 |
| E06 | jobs-background-system | Jobs & background task system | **S–M** ~3–4 pw | M1 | E01 |
| E07 | catalog-dam | Catalog DAM core (folders/sync, collections, smart-collection AST→SQL, keywords, filter+FTS, relink; **EXIF/IPTC metadata viewer + editor, metadata presets, sync-metadata across a selection** — reads the catalog `metadata_cache`/keyword tables, writes out through `lightbox-meta` / E09's write-metadata command, seam 3 §2.3) | **L** ~5–8 pw | M1 | E01 (metadata write-out consumes E09's write-metadata command — same-milestone seam, not a scheduling edge) |
| E08 | library-ui-culling | Library UI & culling (virtualized grid/loupe/filmstrip, compare/survey, on-canvas gizmos, culling grammar, keymap; **performance preferences panel** — binds cache/raw-cache caps + relocation + T2 retention (E03/§3.3), GPU enable + VRAM thresholds (§6), and job-concurrency knobs (E06) to the prefs store the owning subsystems read) | **L–XL** ~7–11 pw | M1 | E01, E03, E06, E07 |
| E09 | edit-state-history-presets | Edit state, history, presets, XMP (CBOR recipe, **ISO 16684 XMP Toolkit RDF engine + own `crs:`/`lb:` mapping — §1.6**, `crs:` import, presets) | **M** ~4–6 pw | M1 | E01 |
| E10 | develop-global-toolset | Develop — global toolset incl. **halo-free highlight/shadow recovery** — **VHigh, decomposed in §10.1** | **XL** ~10–14 pw | M2 | E02, E05, E09 |
| E11 | detail-optics-geometry | Detail, optics, geometry & **clean-room demosaic** — **VHigh, the single hardest item, decomposed in §10.1** | **XL** ~12–18 pw / GPU-imaging eng | M2 | E02, E05 |
| E12 | masking-local-and-retouch | Masking, local adjustments & retouch — **VHigh, biggest pixel-engine lift after demosaic, decomposed in §10.1** | **XL** ~10–14 pw | M3 | E05, E10 |
| E13 | ml-inference-platform | ML inference platform (`lightbox-inferd`, ONNX + **DirectML/CoreML/CPU EPs — CUDA/TensorRT optional user-installed, not bundled, §1.5**, IPC, model-pack manager, VRAM gating, supervision) | **L** ~6–9 pw | M3 | E01, E06 |
| E14 | ai-masking-and-search | AI masking & library intelligence (SAM2/BiRefNet/SegFormer; baked-raster masks; CLIP search; faces) | **L** ~6–9 pw | M3 | E12, E13, E07 |
| E15 | export-output | Export & output engine | **M–L** ~5–7 pw | M2 | E05, E06, E09 |
| E16 | interop-migration-hardening | Interop, migration & v1 hardening (LR `.lrcat`/`crs:` importer, LaMa remove, packaging, CI gates, perf) | **L** ~6–10 pw | M4 | E07, E09, E14, E15 |

**Two sizing/dependency corrections worth stating explicitly:**
- **E08 is re-baselined up (was ~5–8 pw → ~7–11 pw).** A from-scratch immediate-mode egui surface — virtualized 100k grid + loupe + filmstrip + compare/survey + on-canvas gizmos + remappable keymap — is a large build against a thin widget ecosystem (we own the docking/virtualization/gizmo widgets, §1.2). The estimate now carries that buffer. **Backstop:** the headless-core boundary (§1.2/§2.3 seam 1) means the reversal trigger is real — if egui's widget ceiling is hit, `lightbox-shell` swaps for a Qt/Slint shell against the *same* core API, a bounded single-crate rewrite, not an architecture change. Keep that trigger explicit.
- **E09 depends on the frozen recipe schema (§3.2), not on the render engine (E05).** The CBOR recipe, the XMP `crs:`/`lb:` mapping, and `from_lr_crs()` import can be built and property-tested (§8) against the §3.2 schema *before* the Engine exists — the schema is decision-complete in this document. Decoupling E09 from E05 lets the edit/interop stream start at M1 alongside the pixel stream instead of serializing behind it. (E09 still integrates with E05 at render time, but that is consumption, not a build dependency.)

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

**Deferred to v1.x (design headroom preserved, no v1 engineering):** neural raw denoise, super-resolution, HDR edit/merge + gain-map export, soft proofing, publish-services framework, tethered capture, video trim/grade, map/geotag module, People body-part parsing, generative (diffusion) remove, plugin SDK, depth masks.

---

## 11. Risk register

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
