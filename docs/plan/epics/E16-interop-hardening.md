# E16 — Interop & v1 hardening (v2.0)

_Epic spec, **v2.0** (2026-07-05). Milestone: **M4**. Effort: **M–L, ~4–7 pw** (this plan lands at ~35 task-days ≈ 7 pw, the top of the re-baselined band; parallelizes across 2 engineers ≈ 3.5–4 calendar weeks). Depends on: **E09** (recipe/XMP: `XmpDoc`, sidecar I/O, `from_lr_crs`, `xmp_passthrough`, `xmp_sync` divergence), **E14** (AI masking shipped — transitively E12 mask/retouch engine + `RemoveBackend` seam and E13 `lightbox-inferd`/`run_model`/pack manager), **E15** (export engine, needed by the mandate scenario and release validation)._

_Planned from `docs/plan/00-mandate.md` **v2.1** and `docs/plan/01-architecture.md` **v2.0** (decision-complete; stack/boundaries not re-litigated). Research inputs: reports 04 (remove), 06 (import/export interop), 07/09 (smart previews, secondary display, performance/backup), 08 (AI/ML), 10 (landscape). Built-code inputs: `crates/lightbox-{catalog,core,preview,shell,edit,meta,ml}`, `tools/{xtask,lbx-perf}`, `.github/workflows/{ci,nightly}.yml`, `deny.toml`, `native-inventory.toml`, and `docs/plan/epics/E01-deviations.md`._

> **SUPERSEDES `E16-interop-migration-hardening.md` (v1.x) ENTIRELY.** The mandate v2.0 rescope cuts the DAM, and with it the epic's largest slice: **the Lightroom `.lrcat` catalog-migration importer is retired** (there is no catalog to migrate into — no folders/collections/keywords/ratings entities are live, §3.1.1 keep-dormant). The `lightbox-migrate` crate, `LrcatReader`, the data-only Lua parser, smart-rule mapping, `migration_session`/`migration_map` schema, and the migration wizard are all **not built**. Interop narrows to **XMP `.xmp`/`crs:` READ** on open. The epic also **absorbs two items v1.x parked elsewhere for M4**: the **secondary display window** (E08 explicitly built nothing and kept the door open) and **smart previews / offline proxy** (E03 reserved the `smartpreview/` namespace and schema headroom, S9 → E16). Effort re-baselines ~8 pw → ~4–7 pw.

---

## 0. What this epic is

E16 is the M4 "ship v1.0" epic. Eight workstreams turn the M3 feature-complete build into the shippable product:

| WS | Deliverable | One-line contract |
|---|---|---|
| **A** | **XMP `.xmp`/`crs:` read interop** | Opening a file with no edit-store recipe but an existing sidecar (or embedded) XMP recipe **honors it**: `lb:` restores exactly, `crs:` imports approximately with the expectation set per Risk 9 |
| **B** | **Content-aware Remove (LaMa)** | `big-lama` ONNX registered as a second `RemoveBackend` behind E12's trait via E13's generic `run_model` — baked + replayed per §4.5, PatchMatch stays the floor |
| **C** | **Secondary display** | A second OS window (egui multi-viewport, same wgpu device) showing a live-rendered loupe of the active/locked image, per-monitor color-managed |
| **D** | **Smart previews / offline proxy** | ~2560 px linear scene-referred JXL proxies enabling full develop editing while originals are offline, per §3.3 (`smartpreview/`) |
| **E** | **Edit-store repair/restore** | Guided corruption flow: quick_check fail → non-destructive salvage attempt → verified-backup restore picker (§6 corruption row, finished) |
| **F** | **Packaging & distribution** | Signed/notarized macOS `.dmg`, signed Windows installer, best-effort Linux AppImage; LGPL-compliant dylib layout; ORT as a downloadable runtime pack; manual update check; local-only crash logs |
| **G** | **CI license/golden gates** | The three-surface license gate (§8) implemented and release-blocking; golden per-PV + fault-injection suites wired to the 3-OS release matrix; `xtask release` |
| **H** | **Performance hardening** | The **v2.0** §7 budget set (editing budgets only — DAM budgets superseded) asserted by the existing `tools/lbx-perf` harness, driven to green |

**Exit = the mandate v2.0 success scenario, measured from a packaged, signed build, offline:** open a 500-raw shoot folder (drop or ⌘O) → color-correct and enhance keepers with global + AI-masked local adjustments and retouch (incl. a LaMa remove) → apply a look across the working set → batch-export delivery JPEGs, on a mid-range laptop, network disabled throughout.

**First failing test (staff-engineer contract, architecture §12):** `open_honors_lr_sidecar_approximately` — opening a fixture raw that has **no edit-store recipe** but an adjacent LR-Classic-written `.xmp` yields a persisted recipe whose mapped fields match E09's documented `crs:` mapping table, with provenance `lb_extra.migrated_from.kind == "crs"`, a `"Imported settings"` snapshot, and a `CrsImportReport` event. It fails until tasks A1–A2 land.

---

## 1. Scope

### 1.1 In scope

- **XMP read interop (WS-A).** The **open-time recipe-resolution ladder** (edit store → sidecar `.xmp` → embedded XMP → default), the `crs:` approximate-import policy on first open (persist + provenance + snapshot + history seed, fidelity surfaced), working-set-level aggregation of import notices, divergence-badge behavior in the editor, and the headless CLI surface. All parsing/mapping machinery is **consumed from E09** — E16 owns only the policy, orchestration, and surfacing.
- **Content-aware Remove upgrade (WS-B).** `big-lama` model pack (Apache-2.0, hash-pinned, license-manifested); a `lama` task adapter in `lightbox-ml` (window prep, color conversion, seed-derived re-roll jitter) running through E13's **generic `run_model`** — **no inferd protocol change** (E13's R8 hard rule: model-specific logic lives in client adapters); `LamaRemoveBackend` implementing **E12's `RemoveBackend` trait**; backend auto-selection + fallback ladder; bake/replay riding E12's existing `retouch/` store + `patch_ref` (no schema change).
- **Secondary display (WS-C).** One additional OS window via egui multi-viewport on the **same** `wgpu::Device`: loupe-only (Normal = follows the active filmstrip image / Locked = pinned), own fit/1:1 zoom, monitor picker + fullscreen, per-monitor ICC via E02.3's display transform, its own `Engine` viewport ticket. Preference-persisted.
- **Smart previews / offline proxy (WS-D).** Build pipeline (Background job + command + CLI), the `smart_preview` catalog table (one migration), storage in E03's reserved `BlobStore` namespace, **source substitution** in the render source resolver when the original is offline (or the prefer-proxy performance mode is on), the honest control-degradation surface (demosaic-stage options + highlight-reconstruction mode require the original), export-from-proxy warning, reconcile-on-reappear, user-managed retention.
- **Edit-store repair/restore (WS-E).** `lightbox-catalog` salvage + restore primitives over the built backup/integrity machinery; `Core`-level (pre-session) repair API; CLI `repair`/`restore`; the guided shell flow; the scripted CI drill. Also the small hygiene item E01 left: stale `.tmp-*` scratch dirs under `backups/` are cleaned by the next successful backup.
- **Packaging (WS-F).** `cargo xtask dist` per-OS staging from a bundle manifest; every LGPL native artifact (LibRaw sandbox binary's lib, lensfun, libheif+libde265) as a separate, import-linked, user-replaceable dynamic library (relink capability, §1.6); THIRD-PARTY-NOTICES + lensfun CC-BY-SA attribution generated from the three license surfaces; macOS universal2 codesign/notarize/staple `.dmg`; Windows signed installer; Linux AppImage CI job (non-release-blocking); ORT + EP dylibs staged as E13's `kind="runtime"` downloadable pack (DirectML/CoreML/CPU only — CUDA/TensorRT never staged); manual "Check for updates" (signed static JSON, explicit user action only); local-only crash/panic capture.
- **CI gates (WS-G).** Surface 1 finalization (deny.toml exceptions documented incl. the ISO XMP Toolkit CTO-signed entry; LGPL-must-be-dynamic assertion cross-checked against the surface-2 SBOM; model-weights manifest gate). Surface 2: `xtask sbom emit` (CycloneDX from the actual staging tree + a `cargo metadata` build-script/`links` audit that **replaces** the `lint-native-deps` `-sys`-naming heuristic) and `xtask sbom check` with the three hard gates (libheif→libde265 never x265; FFmpeg — not shipped in v1 — asserted absent or LGPL-only if ever staged; ORT EP allowlist), default-deny. Surface 3: `data/MANIFEST.toml` + checker (zero Adobe-authored assets; attribution presence; `model_pack` cross-check) — first real entries already exist (epaint fonts OFL-1.1/Ubuntu-font, committed golden PNGs, lensfun data, E02.4 look family, E02.5 curated DCPs, E13/E14/E16 model packs). Gate wiring: per-PV golden fast-subset PR / full nightly / full release-blocking ×3 OS; fault-injection + restore drill promoted release-blocking; the two `continue-on-error` shell-smoke/grid-scroll jobs promoted to blocking once observed green (E01 handoff debt); `xtask release` all-gates readiness runner that the tag pipeline requires.
- **Performance hardening (WS-H).** Extend the **existing** `tools/lbx-perf` harness with the v2.0 budget scenarios (`working_set_open_500`, `develop_open`, `slider_latency`, `export_throughput` + starvation probe, `memory_ceiling`, `remove_op`); retire the superseded DAM scenarios to informational; chrome-trace profiling toggle + triage runbook; a bounded fix-to-budget buffer; 5 consecutive green nightlies before the release cut.

### 1.2 Explicit non-goals

- **No `.lrcat` catalog-migration importer** — retired with the DAM (there is no live folder/collection/keyword/rating model to migrate into). No `lightbox-migrate` crate, no Lua parsing, no smart-rule mapping, no migration wizard. If a future version revives organization entities, the v1.x spec is the historical starting point.
- **No XMP writing beyond E09's.** Sidecar write, opt-in auto-write, divergence resolution commands, and the never-write-into-originals rule are E09's shipped surface; E16 adds no writer.
- **No two-way sync with Lightroom, no Apple Photos/Capture One importers, no import of LR develop history or LR snapshots.** Current develop state only, read once, honestly labeled.
- **No pixel-parity promise for `crs:` import** — develop settings import **approximately**; the expectation is set explicitly in UI, report, and docs (Risk 9). `crs:` mask groups (`crs:MaskGroupBasedCorrections`) are **skipped + preserved in `xmp_passthrough`** (E09's mapping decision), counted per file in the report.
- **No generative/diffusion Remove** (Could-tier, v1.x+); LaMa is the non-generative ceiling, PatchMatch the floor. **No object-aware selection expansion** (SAM scribble→object) in v1 — named v1.x hook on E14's segmentation API.
- **Secondary display is loupe-only.** No grid/compare/survey second window (those views don't exist in v2.0), and no LR "Live" hover-follows-mouse mode in v1 (cheap to add later behind the same window; cut for scope honesty).
- **No lossy-DNG smart-preview format.** The proxy is the §3.3 JXL container, not a DNG; Adobe's format choice is not a compatibility target (proxies are internal plumbing, invisible interop-wise).
- **No smart-preview auto-build on open by default** and **no proxy sync/upload anything** (local-first).
- **No auto-update mechanism, no telemetry, no crash upload.** Manual update check + local logs only.
- **Not designing neighbors' internals.** E09's mapping fidelity, E12's retouch object model/coordinate space, E13's IPC framing, E03's store internals, E15's encoders are consumed through their published interfaces; gaps are filed against those epics (§10).

---

## 2. Crates & modules touched (against the real tree)

| Crate/module | Change | Notes |
|---|---|---|
| `lightbox-core` | modified | `interop::resolve_recipe_on_open` (WS-A ladder, called from the E04 open path); new `Command` variants (`BuildSmartPreviews`, `DiscardSmartPreviews`, `RefreshRemove` passthrough already E12's); pre-session `repair` module (`diagnose`/`salvage`/`list_backups`/`restore` — no `Session` exists when the store is corrupt); events (`CrsImported`, `SmartPreviewProgress`, `ProxyActive`) |
| `lightbox-edit` | modified | `lb_extra.migrated_from` + `lb_extra.crs_import` conventions (open-map fields — **no recipe-schema change**); consumed: `from_lr_crs`, `CrsImportReport`, snapshot/history APIs (all E09) |
| `lightbox-meta` | consumed | `XmpDoc`, `sidecar::{sidecar_path, read, read_embedded}`, `xmp_sync` divergence DAO — E09's shipped surface; E16 adds **no** code here beyond re-exports if needed |
| `lightbox-ml` | modified | `tasks::lama` adapter (window prep, linear↔sRGB, seed jitter, `run_model` invocation, paste-back); `LamaRemoveBackend` (implements E12's `RemoveBackend`); pack descriptor for `big-lama` registered with E13's `PackManager` |
| `lightbox-inferd` | **untouched** | LaMa rides the generic tensor protocol (`run_model`); E13's R8 rule forbids model-specific ops in the daemon |
| `lightbox-mask` / retouch (E12 code) | modified (one seam) | backend registry gains the `"lama"` registration + auto-selection pref; everything else (bake job, `retouch/` store, `patch_ref`, staleness) is E12's shipped machinery |
| `lightbox-catalog` | modified | one migration: `smart_preview` table (§5); `repair.rs` (salvage/restore primitives over `backup.rs`/`integrity_check_file`); backup scratch-dir hygiene |
| `lightbox-preview` | modified | `SmartPreviewService` (build/lookup/discard) over E03's `BlobStore(BlobNamespace::SmartPreview)`; cache-stats extension (`smartpreview` bytes in `CacheStats`) |
| `lightbox-render` / `lightbox-core::render_source` | modified | `SourceResolver` proxy substitution: offline/prefer-proxy resolution returning a `Proxy` source at the post-demosaic seam (O2 confirms the injection point with E05) |
| `lightbox-shell` | modified | secondary-display viewport (`secondary.rs`); open-time import notice + filmstrip badge + info-panel fidelity drill-down; proxy badge + "requires original" control states; repair/restore guided dialog; "Check for updates"; crash-log reveal |
| `lightbox-cli` | modified | `repair`, `restore`, `smartpreview build|discard|stat` subcommands; `open` honors WS-A ladder headlessly and can emit the import report as JSON |
| `tools/xtask` | modified | `dist`, `sbom emit|check`, `data-manifest check`, `release`; `lint-native-deps` superseded by `sbom check --lock` (kept as alias until removal) |
| `tools/lbx-perf` | modified | v2.0 scenario set + budgets (§4.8); superseded scenarios flagged informational |
| `.github/workflows/` | modified | release workflow (3-OS matrix, gate tiers); promotions of the two observational jobs; nightly gains the new scenarios |
| `deny.toml`, `native-inventory.toml`, `data/MANIFEST.toml`, `policy/native-sbom.toml` | finalized / created | the three license surfaces' policy inputs (`native-inventory.toml` is the E01 placeholder E16 replaces; `data/MANIFEST.toml` + `policy/native-sbom.toml` are new) |

Render-pipeline note: **E16 adds no render nodes.** The retouch composite node is E12's; the proxy substitutes a *source*, not a node; the secondary display composites the same engine output texture. The only pipeline-adjacent change is the `SourceResolver` resolution enum (§4.4.3).

---

## 3. Interop position (stated up front)

Constraint 4 bars Adobe product code, binary reverse-engineering, and Adobe-authored assets; interop rides **open/industry formats only**. WS-A complies by construction: the read channel is **XMP** (ISO 16684), parsed by the ISO XMP Toolkit (BSD-3, mandate v1.1 ruling + CTO sign-off recorded in `02-approval.md`), mapped by E09's own `crs:`/`lb:` layer. Sidecar `.xmp` files and embedded XMP packets are the **user's own data** in a standard format. Nothing Adobe is linked beyond the ISO reference implementation already cleared; nothing is ever written into an original (E09's rule). Sidecar/embedded XMP at open time is **untrusted input**: it enters exclusively through E09's fuzz-hardened, capped parser — E16 adds orchestration, not parsing. (The v1.x `.lrcat` compliance position is moot: retired.)

---

## 4. Design & interfaces

Interfaces are binding in shape; field lists may grow.

### 4.1 WS-A — XMP `.xmp`/`crs:` read interop

#### 4.1.1 The open-time recipe-resolution ladder

Runs once per file as the E04 working-set loader lands it in the session (and headlessly in `lightbox-cli open`). Owned by `lightbox-core::interop`; every step consumes E09 primitives.

```rust
pub struct OpenRecipePolicy {
    pub honor_sidecar_on_open: bool,   // pref `interop.honor_xmp_on_open`, default true
    pub read_embedded_fallback: bool,  // pref, default true (JPEG/TIFF/DNG/HEIC embedded packets)
}

pub enum OpenRecipeResolution {
    /// Edit store had a recipe for this content_hash — the normal reopen path.
    Store { divergence: Option<SidecarDivergence> },     // Some(_) ⇒ badge, never silent (seam 3)
    /// No store recipe; our own sidecar restored at full fidelity (lb: primary).
    SidecarLb,
    /// No store recipe; foreign crs:-only settings imported approximately.
    CrsImported { source: CrsSource, report: CrsImportReport },   // report type is E09's
    /// Nothing found — default recipe for the source kind.
    Default,
}
pub enum CrsSource { Sidecar, Embedded }

/// Called by the E04 open path per file, after probe + content-hash + store lookup.
pub fn resolve_recipe_on_open(
    store: &EditStore,               // E09
    probe: &AssetProbe,              // E01/E02 (SourceKind drives raw-only field handling)
    path: &Path,
    policy: &OpenRecipePolicy,
) -> Result<OpenRecipeResolution, InteropError>;
```

Ladder, in order — **the edit store always wins** (seam 3: catalog owns edit-state, XMP is a projection):

1. **Store hit** (`edit_recipe` by `content_hash`): use it. If a sidecar also exists and its stamp diverges (`xmp_sync`, E09): surface `SidecarDivergence` — filmstrip/panel badge + explicit `ReadMetadata`/`WriteMetadata` resolution (E09's commands). Never auto-read.
2. **No store recipe, sidecar `.xmp` exists** (`sidecar_path`, E09's LR-compatible `<stem>.xmp` policy): parse via `XmpDoc`. If it carries `lb:` properties (our own emit) → E09 `read_back` restores **exactly** (this is the reopen-my-own-edits-on-a-new-machine path). Else if it carries `crs:` → `from_lr_crs(doc, probe)` → approximate import (§4.1.2).
3. **No sidecar, embedded XMP present** and `read_embedded_fallback`: `read_embedded` (read-only; JPEG/TIFF/DNG/HEIC) → same `crs:` handling. (LR writes `crs:` into DNG/JPEG embedded packets when "write to XMP" is on — this is the common no-sidecar LR interop case.)
4. **Nothing** → `Recipe::default_for(probe.source_kind)`.

#### 4.1.2 The `crs:` import commit (one transaction)

On `CrsImported`, one writer txn (the ordinary E09 edit-commit path — **no new write machinery**): persist the mapped recipe to `edit_recipe` (+ same-txn `edit_index`); set provenance `lb_extra.migrated_from = { kind: "crs", source: "sidecar"|"embedded", imported_at }` and the compact fidelity summary `lb_extra.crs_import = { mapped: u32, approximate: u32, skipped: u32 }`; create snapshot **"Imported settings"**; seed one `history_step` (`op = "xmp.import"`). The full `CrsImportReport` is **not** persisted per-image (the summary is); it is emitted as a core event for the shell/CLI:

```rust
pub enum Event { /* existing … */
    CrsImported { image: ImageId, source: CrsSource, summary: CrsSummary }, // per file
}
```

**Working-set surfacing (shell):** a single non-modal notice per open gesture — *"N files opened with imported Lightroom settings (approximate — colors may differ)"* — plus a per-file filmstrip badge and an info-panel drill-down listing the report partitions (mapped / approximate / skipped, incl. the mask-group skip). Copy reviewed against research 00 §"Interop fidelity expectations" (Risk 9). The import is an ordinary history state: **undo/reset never resurrects it silently** (reset-to-default is reset, not re-import; re-import is the explicit `ReadMetadata` command).

**Idempotency:** the ladder only fires when the store has no recipe for the content-hash, so reopening never re-imports; a *changed* sidecar after import surfaces as divergence (step 1), resolved explicitly.

### 4.2 WS-B — Content-aware Remove (LaMa)

#### 4.2.1 Placement in the E12/E13 machinery (what already exists by M3)

E12 ships: editable `retouch_op` objects with `RetouchParams::Remove { seed, patch, patch_frame, stale }`, the full-res **Background bake job**, the content-addressed `retouch/<hh>/<hash>.png` store (LRU-exempt while referenced), downsample **replay** at all scales, staleness + refresh-as-history-edit (§4.5 policy), and the trait seam:

```rust
// E12 (exists): the seam E16 plugs into.
pub trait RemoveBackend: Send + Sync {
    fn id(&self) -> &'static str;                        // "patchmatch" (E12) | "lama" (E16)
    fn fill(&self, img: &TileCpu, mask: &WeightTileCpu, seed: u64, cancel: &CancelToken)
        -> Result<RasterImage, RemoveError>;             // full-res bake, Background job
}
```

E13 ships: `lightbox-inferd` (tensor-only wire protocol), `InferenceClient::run_model(model, inputs, opts) -> JobFuture<Vec<Tensor>>` as the model-generic escape hatch, the pack manager, VRAM gating, supervision. **E16 therefore adds zero protocol ops and zero daemon code** — the v1.x plan's `inpaint` protocol extension is obsolete against E13's actual design (its R8 rule: model-specific logic lives client-side).

#### 4.2.2 The LaMa backend (`lightbox-ml::tasks::lama`)

```rust
pub struct LamaRemoveBackend {
    client: Arc<dyn InferenceClient>,      // E13
    packs:  Arc<PackManager>,              // E13 — big-lama availability
}
impl RemoveBackend for LamaRemoveBackend {
    fn id(&self) -> &'static str { "lama" }
    fn fill(&self, img: &TileCpu, mask: &WeightTileCpu, seed: u64, cancel: &CancelToken)
        -> Result<RasterImage, RemoveError>;
}

/// Internal, unit-tested window preparation (pure):
pub struct WindowPrep { pub window: Rect, pub scale: f32, pub model_dims: (u32, u32) } // multiple-of-8
pub fn prepare_window(mask_bbox: Rect, full: (u32, u32), seed: u64) -> WindowPrep;
```

**Window/color handling (E16-owned, inside the adapter):** context window = mask bbox dilated ×2 (min 512², cap 2048², dims multiple-of-8; larger regions are inferred at reduced resolution and upsampled on paste-back — a stated v1 quality bound). Working-space linear → display-referred sRGB 8-bit tensors for the model (LaMa is sRGB-trained); inverse on return; feathered alpha at the mask boundary; optional `seamlessClone` mixed-gradient harmonization at the seam (OpenCV already linked, §1.6). **Seed semantics:** LaMa itself is deterministic per input; re-roll derives a deterministic mask-dilation delta + window jitter from `seed`, so "Refresh" (seed+1, E12's existing command) produces a visibly different, reproducible-from-bake result.

**Reproducibility (§4.5, restated for this backend):** cross-EP inference is not bit-deterministic, so — exactly like `AiSeg` rasters and PatchMatch patches — the authoritative artifact is the **baked patch** in `retouch/`, replayed on every render. One nuance vs E12's determinism gate: PatchMatch promises *byte-identity per (input, seed) across platforms*; LaMa does **not** (EP-dependent). What is promised: replay from the bake is bit-identical everywhere and forever; a fresh bake on different hardware may differ within perceptual tolerance. `patch_ref` provenance records `backend_id` + `model_pin` (inside E12's CBOR params — additive fields in an open map, no migration).

**Selection & fallback ladder:** pref `retouch.remove_backend = auto | patchmatch | lama` (default `auto`). `auto`: LaMa when `MlStatus` is healthy + pack installed (VRAM gate may route to CPU EP — acceptable: one-shot, seconds-scale); pack absent → PatchMatch now + non-modal CTA "Better results with the Remove model — download (~200 MB)" (E13 pack-manager CTA); inferd crash mid-bake → E13 supervisor restarts, one retry, then PatchMatch. Core editing never blocks on ML (§6). Existing ops **never** silently re-bake on backend availability changes (§4.5: refresh is explicit).

**Model pack:** `big-lama` ONNX export, Apache-2.0, hash-pinned, registered through E13's `PackManager` manifest; entries land in the surface-1 model-weights gate and the surface-3 data manifest (G4 cross-check against `model_pack`).

### 4.3 WS-C — Secondary display

**Shape:** one additional OS window via **egui multi-viewport** (the §1.2 capability note; shell is on eframe/egui 0.35 + wgpu 29 — same `Arc<wgpu::Device>`, so the engine texture composites zero-copy in the second viewport exactly as in the first). Implemented as an **immediate viewport** first (state lives in the existing `ShellApp`; simplest correct thing); if profiling shows the second window's repaint coupling hurts the <100 ms slider budget, switch to a deferred viewport — an internal change behind the same module (recorded as the reversal note, task C3).

```rust
// lightbox-shell::secondary
pub struct SecondaryDisplay {
    pub open: bool,
    pub mode: SecondaryMode,            // Normal (follows active image) | Locked(ImageId)
    pub zoom: SecondaryZoom,            // Fit | OneToOne
    pub monitor: Option<MonitorSelector>, // None = OS default placement
    pub fullscreen: bool,
}
pub enum SecondaryMode { Normal, Locked(ImageId) }
```

- **Content:** loupe only — the active (or locked) image, live-rendered. The secondary view issues its **own** `Engine::submit` per state change with its own viewport id, ROI, and scale sized to the second display (the E01 ticket table already retires superseded tickets per viewport). No panels, no filmstrip; an optional minimal HUD (filename, zoom). Hidden/closed secondary issues **no** renders (C3 AC guards double-render cost).
- **Color:** per-monitor ICC is E02.3's display-transform contract; the secondary viewport resolves **its own** monitor profile and passes it to the display-transform stage — the seam is *named to E02.3* (the view transform must already be parameterized per-target; E16 wires the second target, files a gap if it is not).
- **Modes:** `Normal` follows filmstrip navigation (swap budget: the T26 <50 ms nav-swap bar applies); `Locked` pins one image (reference while editing another — the v2.0 replacement for LR's compare use-case). Toggle + mode + monitor persisted in shell prefs; keyboard shortcut registered in E08's keymap registry.
- **Failure modes:** device-lost recovery (§6.2) rebuilds both viewports from the same recovery path — the secondary must degrade with the primary (preview-res CPU) and never wedge the session; monitor unplug falls back to windowed on the primary display.

### 4.4 WS-D — Smart previews / offline proxy

#### 4.4.1 Format & capture point (the load-bearing decisions)

- **Capture point: post-linearize, post-highlight-reconstruction, post-demosaic, pre-white-balance — linear camera-native RGB.** Everything downstream (WB with true Kelvin/tint, input transform incl. profile choice, the entire global/local/retouch pipeline) re-runs identically on the proxy, preserving the **full develop surface** offline except: demosaic-stage options and the highlight-reconstruction *mode* (both baked at build time — controls shown but marked "requires original", mirroring the §2.4 hidden/disabled discipline). Float values are stored **unclipped** (headroom above 1.0 preserved) so tone-recovery latitude survives.
- **Container: JXL, lossy float** (per §3.3 `smartpreview/<hh>/<hash>.jxl`), default 2560 px long edge (pref 2048/2560/4096), distance ≈ 1.0. Two blobs per proxy in the `BlobStore`: `<hash>.jxl` (pixels, written first) and `<hash>.sp.cbor` (decode-metadata envelope, written last — **its presence is the commit marker**; a crash between the two leaves an invisible orphan the E03 reconcile scan deletes).
- **Decode metadata carried (the offline contract — the original cannot be re-probed):** `content_hash`, source dims + orientation, `SourceKind`, camera make/model, `ColorMatrix1/2`/`ForwardMatrix` (or profile ref), as-shot WB coefficients, black/white levels (already applied), highlight-reconstruction mode used, demosaic algorithm id, builder `pv` + engine build. Authoritative copy in the catalog row (§5); the store blob is self-describing for relocation/reconcile.
- **Non-raw sources:** proxy = resized linear decode of the original through the same envelope (`source_kind='rendered'`); cheap, keeps offline behavior uniform.

#### 4.4.2 Service surface (`lightbox-preview::smartpreview`)

```rust
pub struct SmartPreviewService { /* BlobStore(SmartPreview) + catalog DAO + jobs handle */ }
impl SmartPreviewService {
    pub fn build(&self, assets: Vec<AssetId>, opts: SpBuildOpts) -> JobHandle<SpBuildReport>; // Background
    pub fn lookup(&self, asset: AssetId) -> Option<SmartPreviewDesc>;                          // sync, index
    pub fn discard(&self, assets: Vec<AssetId>) -> Result<u64 /*bytes freed*/, PreviewError>;
    pub fn stats(&self) -> SmartPreviewStats;              // count + bytes → cache panel / CachePressure
}
pub struct SpBuildOpts { pub long_edge: u32 /*2048|2560|4096*/, pub overwrite_stale: bool }
```

Commands `BuildSmartPreviews { images }` / `DiscardSmartPreviews { images }` on the core bus; activity-center progress via E06; CLI `lightbox-cli smartpreview build|discard|stat`. Pref `smartpreview.build_on_open` (default **off**). **Retention: user-managed, never auto-LRU** — a proxy silently evicted would break the offline-editing promise; the store is size-reported (cache panel) with a `CachePressure` advisory, and `discard` is the only deletion path. Rebuild is only needed if the user wants a different size or a newer demosaic (`overwrite_stale`); recipes never invalidate proxies (they are pre-recipe sources).

#### 4.4.3 Source substitution (the render seam)

```rust
// lightbox-core::render_source — extends the M0 resolver (render_source.rs)
pub enum ResolvedSource {
    Original(SourceImage),                                // existing path
    Proxy { img: SourceImage, native_long_edge: u32, meta: SmartPreviewMeta },
}
```

Resolution order per render request: original present → `Original` (unless pref `performance.prefer_smart_previews` is on and a proxy exists — the deliberate performance mode from research 09); original missing/unreadable (`asset.missing`, offline volume) → `Proxy` if one exists, else the existing best-preview fallback with the read-only "offline, no smart preview" state. The proxy enters the graph at the post-demosaic seam (its pixels *are* that stage's output; upstream raw nodes are skipped — **O2** confirms the injection point against E05's graph builder). The loupe badges **"Editing offline proxy (2560 px)"**; 1:1 zoom is 1:1 *of the proxy*. **Export from proxy** is allowed with an explicit per-item warning + report note (`proxy: true`, output capped at proxy resolution) — matching LR's behavior and the batch-unattended mandate (a missing original must not wedge a 500-file export; it degrades honestly). When the original reappears (fs event / next open), resolution reverts automatically; edits made against the proxy apply unchanged (same recipe, same parameter space).

### 4.5 WS-E — Edit-store repair/restore

Built primitives (E01, `lightbox-catalog`): WAL + `synchronous=NORMAL`, `Catalog::integrity()` (quick_check at open), `integrity_check_file`, `backup_verified` (online-backup → integrity-check-on-copy → zstd → temp+rename dated dir → prune), `newest_backup_time`, copy-on-write pre-upgrade backups. E16 adds the recovery half of the §6 corruption row:

```rust
// lightbox-catalog::repair
pub struct Diagnosis { pub status: IntegrityStatus, pub details: Vec<String>, pub backups: Vec<BackupEntry> }
pub struct BackupEntry { pub dir: PathBuf, pub stamp: SystemTime, pub bytes: u64, pub verified: bool }

pub fn diagnose(lbdata: &Path) -> Result<Diagnosis>;
/// Stage 1: SQLite online-backup copy of the damaged db + integrity check (fixes index-level rot).
/// Stage 2: per-table best-effort row salvage into a fresh schema-current catalog, FK-ordered,
///          edit-store tables first (asset, image, edit_recipe, mask, mask_component, retouch_op,
///          history_step, snapshot, preview, smart_preview, model_pack), per-row errors skipped+counted.
/// The damaged original is NEVER modified; output goes to a new file, swapped in only on success
/// with the original preserved as catalog.sqlite.damaged-<stamp>.
pub fn salvage(lbdata: &Path) -> Result<SalvageReport>;
pub struct SalvageReport { pub stage: SalvageStage /*CopyVerified|TableSalvage*/,
                           pub rows_recovered: BTreeMap<String, u64>, pub rows_lost: BTreeMap<String, u64> }

pub fn list_backups(lbdata: &Path) -> Result<Vec<BackupEntry>>;
/// unzstd → integrity_check → swap-in (original preserved as above).
pub fn restore_from_backup(lbdata: &Path, backup: &BackupEntry) -> Result<RestoreReport>;
```

Exposed pre-session from `lightbox-core::repair` (a corrupt store means `open_catalog` fails — repair cannot be a session command); `check_catalog` (exists) is the entry diagnostic. **Guided shell flow:** failed open → "Edit store damaged" dialog → \[Attempt repair] (runs `salvage`, shows the per-table report) → on failure \[Restore from backup] picker over `list_backups` (date, size, verified badge) → reopened session. **Synergy stated in the dialog:** if sidecar auto-write was on, `ReadMetadata` after restore reconciles edits newer than the backup. CLI: `lightbox-cli repair <lbdata>` / `restore <lbdata> [--list | --from <dir>]` (exit codes follow the E01 convention: 0/1/2/3). Backup hygiene: a successful `backup_verified` removes stale `.tmp-*` scratch dirs (E01 T15 leftover, documented there as E16's).

### 4.6 WS-F — Packaging & distribution

- **Staging (`xtask dist`)**: per-OS bundle layout from a committed manifest: app binary, `lightbox-inferd` binary (small; ships in-bundle per E13 — only ORT natives are downloaded), the LibRaw **sandbox subprocess** binary, dynamic libs — every LGPL artifact (LibRaw, lensfun, libheif+libde265) as a separate, import-linked, user-replaceable dylib (relink capability, §1.6) — bundled data (E02.4 default look family, E02.5 curated profiles, lensfun data + attribution), THIRD-PARTY-NOTICES generated from all three license surfaces, license texts, and `data/MANIFEST.toml` itself. **FFmpeg is not staged in v1** (video is a mandate non-goal); the surface-2 policy still carries the LGPL-only build-flag gate so it cannot land un-audited later.
- **macOS**: universal2 `.app`, codesign (hardened runtime, minimal entitlements), notarize + staple, `.dmg`. Acceptance: Gatekeeper-clean on a vanilla VM.
- **Windows**: x64 installer (NSIS vs WiX decided in F3 by signing ergonomics), Authenticode-signed binaries + installer, per-user default install, `.lbdata`/image "Open with" association (no catalog-file association — there is no user-facing catalog).
- **Linux**: AppImage CI job proving no-architectural-exclusion; explicitly non-release-blocking.
- **ORT runtime component**: ONNX Runtime + EP dylibs (DirectML/CoreML/CPU **only**, §1.5) packaged as E13's signed, hash-pinned `kind="runtime"` pack, fetched on first AI use via the pack manager; base installer contains no ORT. CUDA/TensorRT never staged (gate G3 asserts); CUDA remains a detected user-installed bonus.
- **Updates & crash logs**: "Check for updates" fetches a signed static JSON manifest over HTTPS on explicit user action only; compares versions; links to downloads. No auto-update, no phone-home, no telemetry. Panics write a local report (the E01 panic hook already logs+flushes; F6 adds the crash-report file + "reveal in file manager"); nothing uploads.

### 4.7 WS-G — CI license/golden gates (the three §8 surfaces)

- **Surface 1 (crate graph, PR-blocking — mostly green today):** finalize `deny.toml` (existing exceptions — BSL-1.0 `clipboard-win`/`error-code`, OFL/Ubuntu `epaint_default_fonts` — get their promised `docs/plan/licensing.md` entries, closing the E01 deviation debt; the ISO XMP Toolkit entry references the CTO sign-off record); REUSE/SPDX lint (exists); **new**: an assertion that every LGPL-wrapped native dep resolves dynamically (cross-checked against the surface-2 SBOM, not hand-maintained) and the model-weights manifest gate (`model_pack` ⊆ manifest, Apache/MIT/BSD only, InsightFace banned).
- **Surface 2 (native/FFI SBOM, PR-blocking policy + release-blocking inspection):** `xtask sbom emit` produces CycloneDX per platform from (a) a `cargo metadata` build-script/`links`-key audit of the crate graph — **replacing** `lint-native-deps`'s `-sys`-naming heuristic (its documented limitation) — and (b) the *actual* `xtask dist` staging tree on release runners. `xtask sbom check --policy policy/native-sbom.toml` enforces per-artifact `{license, link_mode, build_flags}`; hard gates: libheif's HEVC decoder is libde265 (dep-walk via `otool -L`/`ldd`/`dumpbin` **and** a deny-symbol scan — no `x264_`/`x265_` exports anywhere), FFmpeg absent-or-LGPL-only, staged ORT EP set ⊆ {CPU, CoreML, DirectML}; any unlisted native artifact fails (default-deny).
- **Surface 3 (content/data manifest, PR-blocking policy + release-blocking tree inspection):** `data/MANIFEST.toml` — one entry per shipped non-code asset `{id, path_glob, source, provenance, content_license, bundled|user-installable, attribution}`. Checker fails on: unlisted staged asset (default-deny); Adobe-authored provenance anywhere (constraint 4); lensfun data without staged attribution; camera-matching profiles whose provenance isn't `dcamprof-from-own-targets` or cleared CC0/CC-BY; mismatch vs `model_pack`. Known first entries: `epaint_default_fonts` font data, committed golden PNGs (REUSE-annotated, self-generated), lensfun DB, E02.4 look family, E02.5 curated DCPs, model packs (segmentation set + `big-lama`).
- **Correctness/durability wiring:** per-PV golden suite — fast subset PR-blocking on 3 OS (exists for pv1; E16 owns subset selection as pixel epics add corpora), full nightly, full release-blocking; `kill -9` fault-injection + the scripted restore drill (WS-E) promoted release-blocking on 3 OS; the two `continue-on-error` jobs (shell seam smoke, grid-scroll capture) promoted to blocking after being observed green on the hosted fleet (E01 handoff item).
- **`xtask release`:** one command running surfaces 1–3 + goldens + fault-injection + the perf release subset, emitting a release-readiness report; the tag pipeline refuses to publish without it green.

### 4.8 WS-H — Performance hardening (v2.0 budget set)

Extend the **existing** `tools/lbx-perf` (scenario style, JSON summary, `baselines.json`, nightly wiring — all built by E01). The v2.0 §7 note governs: DAM budgets are superseded; `import_1k`/`page_query_100k` stay as **informational** regression canaries (they still exercise the ingest walker + store), never release gates.

| Scenario | Budget (mid-range GPU) | Notes |
|---|---|---|
| `working_set_open_500` | first image visible < 100 ms (best preview tier); filmstrip scrollable throughout; all T1 within the E03 background budget | the mandate open-a-shoot bar, replaces `import_10k` |
| `develop_open` | preview < 100 ms; full render < 1 s from raw cache | §7 row, unchanged |
| `slider_latency` | < 100 ms p95 at fit-view, measured `Engine::submit→Ready` + the documented compositing allowance (the E01 in-app probe stays the composite-inclusive check) | §7 headline |
| `nav_swap` | < 50 ms p95 | exists; kept |
| `export_throughput` + starvation probe | ≥ 20 raws/min CPU baseline; interactive render p95 unchanged during a working-set batch export | §7 row + §5.3 |
| `memory_ceiling` | single 100 MP render within bounded VRAM (tiling proof) | §7 row |
| `remove_op` | brush→filled preview ≤ 2 s p95 GPU EP / ≤ 12 s CPU EP at ≤ 1024² window; replay-from-bake ≤ 5 ms render overhead | WS-B |
| `smartpreview_build` | informational: ≥ 2 proxies/s/core on the fixture corpus | WS-D sizing sanity |

Plus: `tracing` chrome-trace export toggle on hot paths + `docs/perf-triage.md` runbook; fix-to-budget buffer bounded at 3 task-days (regressions triaged with traces and filed to owning crates; E16 owns the green gate); release requires **5 consecutive green nightlies** on reference hardware. CPU-path budgets are **not** asserted beyond the §4.4 degraded contract (preview-res interactivity smoke only).

---

## 5. Data model changes (SQL, `lightbox-catalog`)

**One migration** (numbered at implementation time via `docs/plan/migrations.md` + `cargo xtask lint-migrations`; copy-on-write upgrade semantics apply). Timestamps follow the E01 convention (RFC3339 UTC, fixed 6-digit subseconds, TEXT).

```sql
-- mNN_e16_smart_preview
CREATE TABLE smart_preview (
  asset_id      INTEGER PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
  store_key     TEXT    NOT NULL,              -- BlobStore key (smartpreview/<hh>/<key>.jxl + .sp.cbor)
  long_edge_px  INTEGER NOT NULL,              -- 2048 | 2560 | 4096
  source_kind   TEXT    NOT NULL CHECK (source_kind IN ('raw','rendered')),
  pv            INTEGER NOT NULL,              -- builder process version (provenance)
  built_at      TEXT    NOT NULL,
  bytes         INTEGER NOT NULL,
  meta_cbor     BLOB    NOT NULL               -- §4.4.1 decode-metadata envelope (authoritative copy)
);
```

A **separate table**, not `preview` rows: the `preview` table is a *disposable cache index* (`source IN ('embedded','rendered')` CHECK, LRU columns) while a smart preview is a **user-managed durable proxy** with decode metadata and no eviction — overloading `tier` would fight both the CHECK constraints and the cache-contract semantics (E03 owner ack = O3).

**Explicitly no other schema changes:** WS-A writes only `lb_extra` map fields inside `edit_recipe.doc` (open map — no recipe-schema bump) plus E09's existing `xmp_sync`/snapshot/history tables; WS-B rides E12's `retouch_op.patch_ref` + CBOR params (backend id/model pin are additive map fields); WS-E reads/writes files, not schema. The v1.x `migration_session`/`migration_map` tables are **not created**.

Preview-store layout delta (§3.3): the reserved `smartpreview/<hh>/` namespace becomes live (two-blob commit protocol per §4.4.1); `retouch/` and `masks/` are E12/E14's, untouched.

---

## 6. Ordered task breakdown (each ≤ 1 day)

Workstreams parallelize across 2 engineers; within a workstream, order is binding. ~35 task-days ≈ 7 pw.

### WS-A — XMP read interop (4 d)

| # | Task | Acceptance criteria |
|---|---|---|
| A1 | `interop::resolve_recipe_on_open` ladder + `OpenRecipePolicy` prefs; unit tests over a fake store/sidecar matrix (store-hit, lb:, crs:, embedded, none, divergent) | Ladder order proven; store always wins; policy-off short-circuits to store/default; no file is ever written by resolution alone |
| A2 | `crs:` import commit txn (recipe + `lb_extra.migrated_from`/`crs_import` + "Imported settings" snapshot + `xmp.import` history step + same-txn `edit_index`) + `CrsImported` event | **First failing test** `open_honors_lr_sidecar_approximately` passes on the E09 LR-sidecar fixture corpus; reopen does not re-import; `kill -9` mid-commit leaves the store `integrity_check`-clean with no half-import |
| A3 | Embedded-XMP fallback (JPEG/TIFF/DNG/HEIC via E09 `read_embedded`) + divergence badge on open (consume `xmp_sync`); working-set notice + filmstrip badge + info-panel fidelity drill-down | LR-written DNG with embedded `crs:` imports; divergent sidecar on a store-hit file badges and is never silently read; notice aggregates N files per open gesture |
| A4 | Headless surface: `lightbox-cli open --report-json`; proptest junk-`crs:` never panics the open path (rides E09's fuzz-hardened parser); fidelity copy review (Risk 9) | CLI emits per-file report JSON; 500-file mixed corpus opens with correct per-file resolutions; copy reviewed against research 00 §"Interop fidelity expectations" |

### WS-B — LaMa Remove (5 d)

| # | Task | Acceptance criteria |
|---|---|---|
| B1 | `big-lama` pack descriptor (hash pin, Apache-2.0 manifest entries, surfaces 1+3) registered with E13 `PackManager`; `run_model` round-trip integration on CPU EP | Pack installs/verifies via the pack manager; a masked test image round-trips inference headlessly; gates G1/G4 pass with the new entries |
| B2 | `tasks::lama` adapter: `prepare_window` (dilate/min/cap/multiple-of-8, seed jitter), linear↔sRGB conversion, tensor marshalling, paste-back + feather | Pure-fn unit tests: window geometry invariants across bbox extremes; color round-trip ΔE within tolerance on synthetic patches; oversized region downscales + upsamples correctly |
| B3 | `LamaRemoveBackend` (`RemoveBackend` impl) + registry entry + `retouch.remove_backend` pref/auto-selection; provenance fields in op params | Fixture remove op bakes via LaMa and renders; **re-render replays bit-identically from `retouch/` with inferd killed**; provenance records backend + model pin |
| B4 | Fallback ladder + availability CTA + refresh/re-roll wiring (E12's `RefreshRemove`, seed+1) | Pack absent → PatchMatch result + download CTA; inferd crash mid-bake → retry→PatchMatch, session unaffected; refresh = new bake + history step; no silent re-bake on availability change |
| B5 | Goldens + perf: remove corpus × {LaMa, PatchMatch} composite goldens (§4.4 tolerance at the seam, CPU/GPU); `remove_op` scenario in lbx-perf | Budgets: ≤ 2 s p95 GPU EP / ≤ 12 s CPU EP @ ≤ 1024²; replay ≤ 5 ms overhead; seam artifacts pass perceptual check |

### WS-C — Secondary display (3 d)

| # | Task | Acceptance criteria |
|---|---|---|
| C1 | Multi-viewport window plumbing: open/close command + shortcut (E08 keymap), monitor picker, fullscreen, pref persistence | Second window opens on chosen display, survives restart via prefs, closes cleanly; primary loop unaffected when closed |
| C2 | Secondary loupe rendering: own Engine viewport ticket sized to the display, Fit/1:1 zoom, Normal/Locked modes, per-monitor ICC wiring (E02.3 seam) | Nav in Normal mode swaps < 50 ms p95 (probe); Locked pins while primary navigates; the two windows render through their respective monitor profiles |
| C3 | Hardening: no renders while hidden; device-lost degrades both viewports to the §4.4 contract; monitor-unplug fallback; `--smoke-secondary` headless-windowed smoke; immediate-vs-deferred decision recorded | Slider p95 budget unchanged with the secondary open (measured); injected device-lost keeps both windows editing at preview res; smoke green on 3 OS (observational until fleet-green, then blocking — G5) |

### WS-D — Smart previews (5 d)

| # | Task | Acceptance criteria |
|---|---|---|
| D1 | Migration `smart_preview` + DAO; `BlobStore(SmartPreview)` two-blob commit protocol; `SmartPreviewMeta` CBOR (serialize/deserialize + property tests) | Migration up on a fixture store passes `integrity_check` (copy-on-write harness); torn two-blob write is invisible + reconciled; meta round-trips |
| D2 | Build pipeline (Background job): decode→linearize→reconstruct→demosaic→resize→JXL-float encode; non-raw path; commands + activity progress + CLI `smartpreview build|discard|stat` | Fixture corpus builds proxies with unclipped >1.0 headroom preserved (asserted on a clipped-highlight fixture); cancel + `kill -9` mid-build leave store + catalog clean; CLI stat reports count/bytes |
| D3 | `ResolvedSource::Proxy` substitution in the source resolver (offline detection via `asset.missing`); post-demosaic graph injection (O2); "requires original" control states + proxy badge | With the original renamed away: file opens from proxy, full develop toolset live except demosaic options + reconstruction mode (marked); badge shows; restoring the file reverts resolution automatically |
| D4 | Prefer-proxy performance mode pref; export-from-proxy warning + capped-res report note; retention/cache-panel stats + `CachePressure` advisory | Batch export with 10% offline originals completes unattended, flags proxy items, never wedges; prefer-proxy mode renders from proxy with originals online |
| D5 | Fidelity + durability proof: proxy-render goldens (deterministic replay; vs downscaled original render within the documented proxy tolerance **ΔE2000 ≤ 2.0**, a stated looser bound than §4.4's 1.0); WS-D rows in the fault-injection suite | Goldens committed + green CPU/GPU; the proxy-fidelity bound is documented user-facing; fault rows green |

### WS-E — Repair/restore (3 d)

| # | Task | Acceptance criteria |
|---|---|---|
| E1 | `repair::{diagnose, salvage}` (copy-verify stage + FK-ordered per-table salvage) + `SalvageReport`; corrupted fixture set (bit-flipped page, truncated WAL, garbage header) | Index-rot fixture recovers fully via stage 1; page-corrupt fixture salvages edit-store tables with correct recovered/lost counts; the damaged original's bytes are untouched in every path |
| E2 | `list_backups`/`restore_from_backup` + `lightbox-core::repair` pre-session API + CLI `repair`/`restore` (exit codes per E01 convention); backup `.tmp-*` hygiene | Restore from a dated backup yields an opening, `integrity_check`-clean store with the damaged file preserved as `.damaged-<stamp>`; CLI distinguishes recovered/restored/failed; stale scratch dirs removed by next backup |
| E3 | Guided shell flow (damaged → repair → report → restore picker) + scripted CI drill wired into nightly + release matrix (G5) | Manual script: both fixture classes walked through the dialog end-to-end; drill green in CI on 3 OS |

### WS-F — Packaging (6 d)

| # | Task | Acceptance criteria |
|---|---|---|
| F1 | `xtask dist` staging from a committed bundle manifest; LGPL dylib layout (import-linked, replaceable); THIRD-PARTY-NOTICES + lensfun attribution generation from the three surfaces | Staged tree on 3 OS contains every runtime dep incl. inferd + LibRaw sandbox binary; a hand-swapped LibRaw dylib still loads (relink proven); default-deny: an artifact absent from the manifest fails staging |
| F2 | macOS: universal2 build, codesign (hardened runtime + entitlements), notarize + staple, `.dmg` | Clean-VM install passes Gatekeeper; app launches, opens a folder, edits, exports |
| F3 | Windows: installer (NSIS/WiX decision recorded), Authenticode signing, per-user install, "Open with" association | Clean-VM install, no SmartScreen hard-block (signed); uninstall removes app, preserves `.lbdata` |
| F4 | Linux AppImage CI job (best-effort, non-blocking) | AppImage launches + opens a folder on stock Ubuntu LTS; failure does not block release |
| F5 | ORT `kind="runtime"` pack staging (DirectML/CoreML/CPU only) + first-run download CTA E2E; installer ships without ORT | Fresh install: AI features show CTA; post-download, segmentation + LaMa work; no CUDA/TensorRT artifact anywhere in staging (G3 cross-checks) |
| F6 | Manual update check (signed static JSON, explicit action, wrong-signature rejected) + local crash/panic report + "reveal log"; release build profile (LTO, symbols archived) | Airplane-mode session makes zero network calls; injected panic produces a readable local report; nothing leaves the machine |

### WS-G — CI gates (5 d)

| # | Task | Acceptance criteria |
|---|---|---|
| G1 | Surface 1 finalize: deny.toml exception docs → `docs/plan/licensing.md` (E01 debt); LGPL-dynamic assertion (vs SBOM); model-weights manifest gate | A test crate with a GPL dep fails PR CI; a statically-linked LGPL artifact seeded into the SBOM fails; XMP Toolkit entry references the sign-off record |
| G2 | `xtask sbom emit`: CycloneDX from `cargo metadata` links-audit + dist staging tree per platform; retires the `-sys` heuristic (alias kept) | Emitted SBOM enumerates 100% of staged native artifacts incl. non-`-sys` bundlers; unlisted artifact → emit fails |
| G3 | `xtask sbom check` + `policy/native-sbom.toml`: license/link-mode/build-flag policy + hard gates (libde265-not-x265 dep-walk + deny-symbol scan; FFmpeg absent-or-LGPL-only; ORT EP allowlist) | Seeded violations (x265-linked libheif, `--enable-gpl` ffmpeg artifact, staged CUDA EP) each fail with a pointed message; honest tree passes |
| G4 | Surface 3: `data/MANIFEST.toml` schema + checker (default-deny, Adobe-provenance ban, attribution presence, `model_pack` cross-check) + first real entries | Seeded Adobe-named `.dcp` fails; missing lensfun attribution fails; current tree (fonts, goldens, look family, profiles, model packs) passes |
| G5 | Gate wiring: per-PV golden tiers (PR fast / nightly full / release full ×3 OS); fault-injection + restore drill release-blocking; smoke/scroll job promotion; `xtask release` readiness runner + tag-pipeline requirement | Deliberate 1-node perturbation fails PR CI on 3 OS; `xtask release` runs everything and emits the report; tag refuses without it |

### WS-H — Perf hardening (4 d)

| # | Task | Acceptance criteria |
|---|---|---|
| H1 | lbx-perf v2.0 scenarios: `working_set_open_500`, `develop_open`, `slider_latency` (engine boundary + documented compositing allowance); DAM scenarios flagged informational | Budgets asserted per §4.8 on reference hardware; nightly JSON archives the new set |
| H2 | `export_throughput` + canvas-starvation probe; `memory_ceiling` (100 MP tiled render VRAM bound); `smartpreview_build` informational | Starvation probe shows interactive p95 unchanged during batch export; VRAM bound asserted via the engine's pool stats |
| H3 | Chrome-trace toggle on hot paths + `docs/perf-triage.md` runbook | A seeded slider-latency regression is traceable to a node/stage in one capture |
| H4 | Fix-to-budget buffer (bounded; extends to 3 d total across the epic if needed): triage nightly failures, land integration-level fixes, file crate-owner issues | All §4.8 budgets green for 5 consecutive nightlies before the release cut |

---

## 7. Test plan (per architecture §8)

### 7.1 Unit / property / fuzz (PR-blocking)

- Resolution-ladder truth table (A1): every (store, sidecar-kind, embedded, policy) combination → expected `OpenRecipeResolution`; divergence never auto-resolves.
- `crs:` open path over fuzzed/junk property soup — must never panic or error the open (rides E09's fuzz-hardened `XmpDoc`; E16 asserts the *orchestration* is total).
- `prepare_window` geometry invariants + linear↔sRGB round-trip tolerance (B2); seed-jitter determinism (same seed → same window/dilation).
- `SmartPreviewMeta` CBOR round-trip property tests; two-blob commit-marker protocol (torn-write simulation).
- Salvage report accounting (recovered + lost = source rows per table) on synthetic corrupt DBs.
- SBOM/data-manifest checkers: table-driven — every hard gate has a seeded-violation test that must fail and a clean case that must pass.
- Migration up + `smart_preview` DAO CRUD under the E01 copy-on-write harness.

### 7.2 Integration (PR-blocking fast subset; full nightly)

- **Headless interop E2E** (A4): mixed 500-file corpus (store-known, lb:-sidecar, crs:-sidecar, embedded-crs, bare) → per-file resolutions + reports asserted; reopen idempotence.
- **`kill -9` fault injection** extended with: mid-crs-import, mid-smart-preview-build, mid-remove-bake, mid-salvage → `integrity_check` clean, resumable (extends the E01 suite; nightly ×1000 job inherits).
- **Remove through inferd**: brush → LaMa bake → kill inferd → re-render replays bit-identically from `patch_ref`; ladder (pack absent / inferd down / VRAM-gated) produces PatchMatch without session disruption.
- **Offline proxy E2E**: build proxies → rename originals away → open, edit (WB/tone/mask/remove), export with warning → restore originals → same recipe renders against originals.
- **Repair/restore drill**: all corrupted-fixture classes through CLI + guided flow; original bytes untouched asserted.
- **Secondary display smoke**: `--smoke-secondary` on 3 OS (Xvfb on Linux); device-lost injection with both windows open.
- **Packaging smoke (release matrix)**: clean-VM install → launch → open fixture folder → develop edit → LaMa remove → export JPEG, per OS; **airplane-mode network-silence assertion** over the whole scenario.

### 7.3 Golden-image (PR fast subset; full nightly; release-blocking)

- Remove corpus × {LaMa bake, PatchMatch} composites within §4.4 tolerance (ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB) CPU vs GPU; **replay-from-bake bit-identical** (stronger than tolerance — it composites a stored raster).
- Imported-`crs:` recipes rendered → committed goldens (guards the E09 mapping *as integrated*; drift fails here even if E09's unit mapping passes).
- Proxy renders: deterministic replay goldens + the documented ΔE2000 ≤ 2.0 proxy-fidelity bound vs downscaled original renders.
- E16 wires (does not author) the per-PV immutability gate into the 3-OS release matrix (G5); pixel-epic corpora are owned by E05/E10/E11/E12.

### 7.4 Performance (nightly; release-blocking subset)

All §4.8 scenarios and budgets; regression policy per §8: nightly regression → tracked issue (the existing `gh issue` step); release branch → blocking; 5 consecutive green nightlies gate the cut.

---

## 8. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **`crs:` fidelity disappointment** — "opens my Lightroom edits" read as "renders identically" (Risk 9) | Approximate-import expectation set at every surface (open notice, badge, drill-down, docs); per-file mapped/approximate/skipped counts; integrated goldens catch *our* drift; mask groups explicitly reported as skipped-and-preserved |
| R2 | **LaMa quality on large/high-res fills** (2048² window cap → soft large fills) | Stated UI bound; harmonization pass; PatchMatch comparison goldens keep the floor honest; refinement tiling named v1.x follow-up |
| R3 | **egui multi-viewport maturity** (0.35): repaint coupling or platform quirks on the secondary window | Loupe-only scope (lowest-risk content); immediate→deferred fallback recorded (C3); worst case the feature ships macOS/Windows-first with the Linux leg observational — it gates nothing else |
| R4 | **Proxy fidelity expectations** — proxy renders are not pixel-equal to original renders (resolution + baked reconstruction) | Dedicated looser tolerance stated (ΔE ≤ 2.0) and documented user-facing; badge + capped-export warning; controls that can't act on the proxy are marked, not silently wrong |
| R5 | **Signing/notarization operational lead time** (Apple Developer ID, Windows Authenticode) | Certs requested at epic start (O4/O5 ops dependency); CI dummy-signs until real certs land; F2/F3 acceptance requires real certs |
| R6 | **SBOM false confidence** (dep-walk/symbol heuristics miss exotic linkage) | Default-deny posture; release runners inspect *actual* binaries; per-release manual audit checklist attached to the `xtask release` report |
| R7 | **Late cross-crate perf regressions** after owners roll off | Nightly harness runs the new scenarios from H1 onward; bounded fix-to-budget buffer with escalation; 5-green-nightly release rule |
| R8 | **Seam drift against M3 reality** — `RemoveBackend` shape (E12), proxy injection point (E05), per-target display transform (E02.3) | O1/O2 resolved before B3/D3 start; adapter/window-prep and build pipeline are seam-shape-independent by design |
| R9 | **Salvage overpromise** — no salvage can guarantee recovery of arbitrary corruption | The flow's floor is the **verified backup** (E01's exit-time backup makes one exist); salvage is best-effort and reported honestly; the drill tests the unrecoverable class ending in restore |

### Open questions

- **O1** (before B3): confirm E12's final `RemoveBackend` trait + registry shape and the `retouch_op` params CBOR field names for backend/model-pin provenance — §4.2 is spec'd against E12's published seam.
- **O2** (before D3): confirm with the E05 owner the graph-builder injection point for a post-demosaic proxy source (and that upstream raw nodes are skippable per-request); §4.4.3 is the proposal.
- **O3** (before D1): E03 owner ack for the separate `smart_preview` table + live `smartpreview/` namespace semantics (user-managed, no LRU) — E03's S9 reserved exactly this.
- **O4**: update-manifest hosting + signing-key custody (static JSON endpoint) — ops decision, needed by F6.
- **O5**: Windows cert class (EV vs OV; SmartScreen reputation) — ops decision, needed by F3.
- **O6** (before C2): E02.3's display transform per-target parameterization — confirm the second monitor's profile can be supplied per viewport; file the gap to E02 if not.
- **O7** (before D2): JXL float lossy encode via E03's `PreviewCodec` thin libjxl binding — confirm float/HDR support; fallback is a direct libjxl call inside `SmartPreviewService` behind the same API.
- **O8**: whether `working_set_open_500`'s budget holds on spinning-disk sources — measure in H1; budgets are SSD-referenced, HDD expectation documented if materially worse.

---

## 9. Definition of done

- [ ] All WS-A–WS-H tasks complete with acceptance criteria met; the first failing test and every §7 suite green.
- [ ] **Mandate v2.0 success scenario executed end-to-end on reference hardware, offline, from a packaged signed build**: open a 500-raw folder → global + AI-masked local edits + a LaMa remove → look applied across the set → batch-export delivery JPEGs — network disabled throughout, canvas never starved.
- [ ] XMP read interop: lb: sidecars restore exactly; LR `crs:` sidecars/embedded import approximately with per-file fidelity surfaced; divergence never silently resolved; reopen never re-imports.
- [ ] Remove: LaMa via inferd with baked bit-identical replay, PatchMatch fallback ladder, refresh-as-edit; budgets met.
- [ ] Secondary display ships (Normal/Locked, fit/1:1, per-monitor ICC); slider budget unaffected with it open.
- [ ] Smart previews: build/discard/stat shipped; full offline develop editing proven; export-from-proxy warns; fidelity bound documented.
- [ ] Repair/restore: guided flow + CLI shipped; drill green on 3 OS; damaged originals never modified.
- [ ] Installers: Gatekeeper-clean notarized `.dmg`; signed Windows installer; AppImage job exists (non-blocking); LGPL relink demonstrated; THIRD-PARTY-NOTICES + attributions complete; ORT runtime pack (no CUDA) verified.
- [ ] **All three license surfaces green and release-blocking** with seeded-violation proofs; zero Adobe-authored assets in the staged tree; `native-inventory.toml` heuristic retired for the real SBOM.
- [ ] Golden (incl. per-PV immutability), fault-injection + restore drill, and the perf release subset wired into `xtask release`; tag pipeline refuses without it; the two E01 observational CI jobs promoted to blocking.
- [ ] All §4.8 budgets green for 5 consecutive nightlies before the release cut.
- [ ] User-facing interop note written (what imports exactly vs approximately, the mask-group limitation, the proxy fidelity bound).
- [ ] O1–O8 resolved or explicitly deferred with owner + date.

---

## 10. Seams to neighboring epics (named, not designed here)

| Epic | Seam | Direction |
|---|---|---|
| **E09** | `XmpDoc`, `sidecar::{sidecar_path, read, read_embedded}`, `read_back`, `from_lr_crs` + `CrsImportReport`, `xmp_sync` divergence, `ReadMetadata`/`WriteMetadata`, snapshot/history APIs, `lb_extra` open map — the entire parsing/mapping fidelity lives behind these; E16 owns open-time policy + surfacing; fidelity gaps filed to E09 | E16 consumes |
| **E12** | `RemoveBackend` trait + registry, retouch object model/coordinate space, bake job, `retouch/` store + `patch_ref`, staleness/refresh commands — E16 registers `"lama"` (O1) | E16 implements E12's trait |
| **E13** | `InferenceClient::run_model` (tensor-only — **no protocol change**), `PackManager` (big-lama + the ORT `kind="runtime"` component), VRAM gating, supervision | E16 consumes/extends via published mechanism |
| **E14** | The baked-artifact reproducibility pattern (`cached_raster_ref`/`stale`, §4.5) — mirrored for fill bakes; SAM scribble→object remove-selection is a named v1.x hook on E14's segmentation API | pattern reuse |
| **E15** | Export engine — mandate scenario, `export_throughput`, export-from-proxy warning + report note, backend-provenance on exports | E16 consumes |
| **E03** | `BlobStore(BlobNamespace::SmartPreview)` (reserved for E16 by name), preview-store relocation/reconcile/stats, `PreviewCodec` JXL (O7); `smart_preview` table ack (O3) | E16 builds on reserved seam |
| **E04** | The working-set open path calls `resolve_recipe_on_open` per file (the only WS-A entry point) | E16 plugs into E04's open flow |
| **E05 / E02** | `SourceResolver` proxy substitution + post-demosaic injection point (O2); per-target display transform for the secondary monitor (E02.3, O6); decode/linearize/demosaic stages consumed by the proxy builder | E16 consumes; gaps filed |
| **E06** | Job classes/cancel/pause + activity center for proxy builds, LaMa bakes, repair | E16 consumes |
| **E08** | Shell widget kit, keymap registry, prefs panel (proxy/remove/update knobs), filmstrip badge slots; the secondary-display door E08 explicitly left open | E16 consumes/extends |
| **E01** | WAL/backup/integrity primitives, migration framework, fault-injection harness, `xtask`/CI skeleton, `native-inventory.toml` placeholder (retired by G2), backup scratch-dir hygiene debt, observational-job promotions | E16 consumes/hardens |
| **security review (§12)** | Untrusted inputs at open time (sidecar/embedded XMP) ride E09's fuzz-hardened parser; model-pack download integrity is E13's; E16 brings the corrupted-DB salvage path (bounded by SQLite's own parser) to the design-time review | E16 participates |
