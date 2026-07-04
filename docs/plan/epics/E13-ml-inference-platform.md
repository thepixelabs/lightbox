# E13 — ML inference platform (`lightbox-inferd` + `lightbox-ml`)

_Epic spec. Author: staff engineer (E13 planner). Inputs: `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (approved; §1.5, §1.6, §2.2, §3.1, §3.3, §4.5, §5, §6, §8, §10, §12), `docs/research/08-*.md`, `docs/research/10-*.md`. Milestone **M3**. Effort **L ~6–9 pw**. Depends on **E01** (workspace, catalog + migrations, core façade), **E06** (jobs & background task system)._

This epic builds the **platform** every AI feature rides on: the out-of-process ONNX inference host, its IPC contract, the model-pack lifecycle, VRAM admission control, and crash supervision. It ships **zero user-visible AI features** — E14 builds Select Subject/Sky/Background/Objects, CLIP search, and faces *on top of* the interfaces defined here. The M3 exit criterion this epic owns: **"inference crash is isolated and recovered."**

---

## 1. Scope & non-goals

### 1.1 In scope

1. **`lightbox-inferd`** — a separate, supervised binary hosting ONNX Runtime (via `ort`, dynamically loaded), with the **shipped EP set fixed by §1.5: DirectML (Windows) / CoreML (macOS) / CPU (all)**. CUDA/TensorRT are **detected if user-installed, never bundled, never downloaded by us**.
2. **`lightbox-ml`** — the in-process client crate: supervisor (spawn/heartbeat/restart/backoff/crash-loop breaker), connection + request correlation, typed task adapters (segment, embed), retry/fallback policy, and the public `InferenceClient` trait per the §2.2 sketch.
3. **IPC protocol v1** — versioned, framed, cancellable, back-pressured request/response protocol over a local socket (UDS on macOS/Linux, named pipe on Windows). Copy-based tensor transfer (the §5.2 decision: inference output is cheap to copy; no zero-copy machinery).
4. **Model-pack manager** — registry, resumable hash-verified downloads with mirror fallback, atomic install into `<catalog>.lbdata/models/<name>-<version>/` (§3.3), verify/repair/remove, local import of user packs, license-manifest enforcement (Apache/MIT/BSD weights only; InsightFace banned, §1.5), and the **ORT runtime component** itself distributed as a pack (`kind = "runtime"`, per §1.5 "shipped as a downloadable component").
5. **VRAM gating & admission control** — device probe (dedicated/unified memory, EP availability), the §6 thresholds (8 GB dedicated / 16 GB unified), per-model requirements from pack manifests, tiled-inference framework, "insufficient VRAM — run on CPU?" decision surface, VRAM budget semaphore, idle unload/shutdown.
6. **Supervision & failure handling** — everything in the §6 failure rows "inferd process crash", "model download failure", "low/exhausted VRAM" that belongs to the platform.
7. **Job-system integration (E06)** — every ML call is a cancellable job; batch work is Background-class and pausable; user-invoked single inferences are Foreground-class; progress events map to the activity model.
8. **Provenance** — every result carries the exact `ModelRef` (pack id + version + file sha256), EP, ORT version, and inferd build, so E14 can satisfy the §4.5 baked-raster provenance requirement without asking us anything.
9. **CI/license plumbing owned by this epic** — model-weights manifest emitter (§8 surface 1 gate), SBOM entries for ORT + EP binaries (§8 surface 2), CPU-EP integration tests on all three CI platforms.

### 1.2 Explicit non-goals (named seams, not designs)

| Not in E13 | Owner | The seam |
|---|---|---|
| Model choice, prompting UX, mask post-processing (feather/threshold/expand), baked-raster storage & staleness (§4.5), tokenizers, embedding writes to `sqlite-vec`, face clustering | **E14** | E14 consumes `InferenceClient` typed tasks + `Provenance`; E13 returns rasters/vectors/tensors and never touches `mask_component`, `embedding`, or `face` tables |
| `AiSeg` mask component semantics, mask compositing | **E12** (via E14) | E12 replays baked rasters; E13 is invisible to it |
| Job classes, CancelToken, activity-center data model | **E06** | E13 spawns through `lightbox-jobs::spawn` and threads E06's `CancelToken` into the protocol `Cancel` frame |
| Prefs panel UI, activity-center UI, "run on CPU?" dialog rendering | **E08** | E13 exposes `MlStatus`, `MlEvent`, prefs keys, and a typed `GateDecision`; the shell renders them |
| Catalog schema v1, migration framework, core command/query façade plumbing | **E01** | E13 ships one migration extending `model_pack` and registers commands/queries on the existing façade |
| Installer/bundle packaging, code-signing of `lightbox-inferd`, release-runner SBOM inspection | **E16** | E13 provides the SBOM/manifest emitters and a signed-binary-friendly layout |
| Neural raw denoise, super-resolution, LaMa remove, depth models | **v1.x / E16 (LaMa)** | The generic `run_model` + tiling framework is the headroom; `denoise_tile` is a feature-flagged reserved API (Risk 8) — no v1 model shipped by E13 |
| Model *hosting* infrastructure (CDN, mirrors ops) | **operator/ops** | E13 defines the registry/mirror format and consumes URLs; who runs the servers is an ops decision (open question §9) |

### 1.3 Decisions this epic makes (within its remit; architecture decisions are cited, not re-opened)

1. **inferd is codec- and tokenizer-free.** The daemon consumes and produces **raw tensors only**. All image decode/resize/normalize and all text tokenization happen client-side (in `lightbox-ml` task adapters or in E14), where the app's codec crates already live. This keeps inferd's attack/dependency surface minimal (ORT + protocol + platform probe, nothing else) and keeps the wire protocol generic.
2. **Transport: tokio UDS (macOS/Linux) + tokio named pipe (Windows)** behind a ~100-line `Transport` trait. Rejected: gRPC/tonic (protobuf + HTTP/2 weight for a single-hop local link), shared-memory rings (zero-copy machinery §5.2 explicitly says we don't need). Frames are length-prefixed CBOR envelopes followed by raw little-endian tensor payload segments — CBOR for evolvability, raw segments so multi-MB tensors are never re-encoded.
3. **ORT is dynamically loaded** (`ort` crate `load-dynamic` feature) from the installed runtime component directory — mandated by the §1.6 dynamic-link policy row for ONNX Runtime's C++ core, and it is what makes the downloadable-runtime-component model (§1.5) work.
4. **The ORT runtime component (ORT dylibs + EP dylibs per platform) is distributed through the pack manager** as `kind = "runtime"`, hash-pinned and version-locked to the app build. The `lightbox-inferd` **binary itself ships in the app bundle** (it is small; only the ORT natives are downloaded). Keeps the installer lean (darktable-5.6-style optional AI, per research 10) while the §8 surface-2 SBOM still audits the runtime component on release runners because we build and publish it.
5. **Packs are plain directories of individually-downloaded files** (each listed with size + sha256 in the manifest). No archive extraction → no zip-slip/tar-slip surface, and per-file HTTP range resume falls out for free.
6. **Two-priority queue inside inferd** (`Interactive` ahead of `Batch`), bounded depth, explicit `Busy` backpressure, credit-windowed batch streaming on the client. This is the streaming/back-pressure contract §12 routes to **nexus review before the protocol freezes** (task A7).
7. **Version lockstep, not compatibility matrix:** the client refuses to talk to an inferd whose `(protocol, build_hash)` handshake doesn't match its own build. inferd ships inside the app bundle, so cross-version compatibility is a non-problem we decline to solve; the protocol version field exists for the *runtime component* (ORT ABI) checks and for future relaxation.
8. **Same-user threat model for IPC:** socket/pipe created in a per-user runtime dir with `0700`/owner-only DACL, plus a 32-byte session token passed to inferd over **stdin** at spawn (not argv, not env) and required in `Hello`. Remote access is impossible by construction (no TCP listener, ever — mandate constraint 6).

---

## 2. Crates & modules

Per the §2.1 decomposition. New crates in **bold**.

| Crate / module | Contents |
|---|---|
| **`lightbox-ml`** (lib) | `types` (tensors, models, errors, provenance) · `protocol` (wire types + framing; also consumed by inferd) · `transport` (UDS/named-pipe client) · `supervisor` (spawn/heartbeat/restart) · `service` (`MlService`, request correlation, retry/fallback policy) · `tasks` (segment/embed adapters, pre/post-processing, tiler) · `packs` (registry, downloader, installer, verifier, license gate) · `gate` (device profile, admission control) |
| **`lightbox-inferd`** (bin) | `main` (handshake, parent-watch, idle-exit) · `server` (accept loop, dispatch, priority queue) · `engine` (ort init, EP chains, session manager, run executor, cancel) · `device` (VRAM/EP probe) — depends on `lightbox-ml/protocol` (feature-gated so the daemon pulls no catalog/jobs code) |
| `lightbox-catalog` (touched) | migration `e13_ml_platform` (extends `model_pack`, adds `model_file`); `ModelPackDao` |
| `lightbox-core` (touched) | command/query façade registration: `MlCommand::*`, `MlQuery::*`, `MlEvent` on the event bus |
| `lightbox-jobs` (consumed) | `spawn(Class::Foreground | Background, …)`, `CancelToken`, pause hooks, progress reporting — no changes to E06 code expected; if a hook is missing we file against E06, not fork it |

Binary/asset layout (per §3.3 plus platform app-support dir):

```
<app bundle>/…/lightbox-inferd(.exe)          # ships with the app, code-signed (E16)
<catalog>.lbdata/models/<pack-id>-<version>/  # model packs: pack.toml + *.onnx + LICENSE…
<app-support>/Lightbox/ml/runtime/<ort-ver>/  # runtime component (ORT + EP dylibs) — machine-local,
                                              # shared across catalogs (it is code, not catalog data)
<app-support>/Lightbox/ml/device_profile.json # probe + EP-validation cache — machine-local
<runtime-dir>/lightbox/inferd-<uid>-<nonce>.sock | \\.\pipe\lightbox-inferd-<sid>-<nonce>
```

---

## 3. Interface definitions

Signatures are the contract; field lists may grow additively. All types `Send + Sync` unless noted.

### 3.1 Core types (`lightbox_ml::types`)

```rust
/// Stable pack identity, e.g. "seg-sam2-small", "embed-siglip-base", "runtime-ort".
pub struct PackId(pub String);

/// Fully pinned model identity — what §4.5 provenance stores. Never ambiguous.
pub struct ModelRef {
    pub pack: PackId,
    pub model: String,             // model name within the pack, e.g. "encoder"
    pub version: semver::Version,  // pack version
    pub file_sha256: [u8; 32],     // hash of the .onnx actually loaded
}

/// How callers *ask* for a model (resolved to a ModelRef by PackManager::resolve).
pub enum ModelSel {
    Pinned(ModelRef),                              // replay/provenance path
    Latest { pack: PackId, model: String },        // normal path: newest installed
}

pub enum ExecProvider { Cpu, CoreMl, DirectMl, Cuda, TensorRt }  // Cuda/TensorRt: detected only (§1.5)

pub enum DType { F32, F16, U8, I32, I64, Bool }

/// Row-major, little-endian, contiguous. Bytes is ref-counted; no copies inside the client.
pub struct Tensor { pub name: String, pub dtype: DType, pub shape: Vec<i64>, pub data: bytes::Bytes }

/// Client-side decoded image handed to task adapters. inferd never sees encoded images.
pub struct ImageInput {
    pub width: u32, pub height: u32,
    pub pixels: PixelBuf,                    // Rgb8 | Rgba8 | PlanarF32{c}
    pub color: ColorTag,                     // Srgb | LinearRec709 | LinearProPhoto
}

pub struct MaskRaster { pub width: u32, pub height: u32, pub weights: Vec<u8> }   // 0..=255

pub struct Provenance {
    pub model: ModelRef,
    pub ep: ExecProvider,
    pub ep_partition: EpPartition,           // FullyOnEp | Mixed{ ep_nodes: u32, cpu_nodes: u32 }
    pub ort_version: String,
    pub inferd_build: String,
    pub duration_ms: u32,
    pub tiled: Option<TileInfo>,
}

pub struct SegResult   { pub raster: MaskRaster, pub score: f32, pub provenance: Provenance }
pub struct Embedding   { pub vec: Vec<f32>, pub provenance: Provenance }

pub enum SegPrompt {
    Auto,                                        // salient subject (BiRefNet-class)
    Points(Vec<PromptPoint>),                    // SAM-class point prompts (pos/neg), normalized coords
    Box(RectNorm),                               // SAM-class box prompt
    SemanticClass(u16),                          // SegFormer/ADE20K class id (e.g. sky)
}
pub struct PromptPoint { pub x: f32, pub y: f32, pub positive: bool }

pub struct RunOpts {
    pub priority: Priority,                      // Interactive | Batch
    pub cancel: lightbox_jobs::CancelToken,
    pub timeout: Duration,                       // default 120 s
    pub ep_override: Option<ExecProvider>,       // prefs/debug only
    pub fallback: FallbackPolicy,                // GpuThenCpu | GpuOnly | CpuOnly | AskUser
    pub allow_tiling: bool,                      // default true
}
```

### 3.2 Client service & `InferenceClient` (implements the §2.2 sketch)

```rust
pub struct MlConfig {
    pub models_dir: PathBuf,          // <catalog>.lbdata/models
    pub runtime_dir: PathBuf,         // app-support ml/runtime
    pub inferd_path: PathBuf,         // bundled binary
    pub prefs: MlPrefs,               // see §4.3 prefs keys
    pub registries: Vec<RegistrySource>,   // bundled registry + user-added
}

pub struct MlService { /* supervisor, conn, packs, gate, event bus */ }

impl MlService {
    /// Non-blocking: does NOT spawn inferd. The daemon starts lazily on first request
    /// (or eagerly via warm_up()). Core editing never waits on ML (§6 model-download row).
    pub fn start(cfg: MlConfig, jobs: JobsHandle, catalog: CatalogHandle) -> anyhow::Result<Arc<MlService>>;

    pub fn client(&self) -> Arc<dyn InferenceClient>;
    pub fn packs(&self) -> Arc<PackManager>;
    pub fn status(&self) -> MlStatus;                                  // core query façade
    pub fn device_profile(&self) -> Option<DeviceProfile>;
    pub fn subscribe(&self) -> broadcast::Receiver<MlEvent>;           // E08 activity/prefs UI
    pub fn warm_up(&self) -> JobHandle<()>;                            // spawn + probe + validate EPs
    pub async fn shutdown(&self);                                      // graceful: drain, stop inferd
}

/// §2.2 contract, concretized. `embed_clip` generalized to embed_image/embed_text
/// (same shape, model-agnostic); `denoise` kept as reserved v1.x headroom per Risk 8.
pub trait InferenceClient: Send + Sync {
    fn segment(&self, img: ImageInput, model: ModelSel, prompt: SegPrompt, opts: RunOpts)
        -> JobFuture<SegResult>;

    fn embed_image(&self, img: ImageInput, model: ModelSel, opts: RunOpts)
        -> JobFuture<Embedding>;

    /// tokens are pre-tokenized ids — tokenization is E14's (model-pack-specific) job.
    fn embed_text(&self, tokens: Vec<i64>, model: ModelSel, opts: RunOpts)
        -> JobFuture<Embedding>;

    /// Generic escape hatch: E14's exotic pipelines (SAM2 two-stage encode/decode,
    /// face embedding) run through this without protocol changes.
    fn run_model(&self, model: ModelSel, inputs: Vec<Tensor>, opts: RunOpts)
        -> JobFuture<Vec<Tensor>>;

    /// v1.x reserved (Risk 8): compiled only under feature "neural-denoise".
    #[cfg(feature = "neural-denoise")]
    fn denoise_tile(&self, tile: Tensor, model: ModelSel, opts: RunOpts) -> JobFuture<Tensor>;
}

pub struct MlStatus {
    pub host: HostState,
    pub enabled: bool,                        // ml.enabled pref && runtime installed
    pub runtime_installed: Option<semver::Version>,
    pub active_eps: Vec<ExecProvider>,
    pub loaded_sessions: Vec<(ModelRef, ExecProvider)>,
    pub queue: QueueStats,                    // depths per priority — activity center
    pub vram: Option<VramSnapshot>,
}

pub enum MlEvent {
    HostStateChanged(HostState),
    PackProgress { pack: PackId, phase: PackPhase, done_bytes: u64, total_bytes: u64 },
    PackInstalled(PackId), PackFailed { pack: PackId, error: String },
    GatePrompt { request: RequestId, decision: GateDecision },   // “run on CPU?” — E08 renders
    RunProgress { request: RequestId, done: u32, total: u32 },
}
```

### 3.3 Supervisor (`lightbox_ml::supervisor`)

```rust
pub enum HostState {
    NotStarted,
    Starting,
    Ready { pid: u32, eps: Vec<ExecProvider> },
    Restarting { attempt: u32, backoff: Duration },
    Degraded { reason: DegradeReason },       // e.g. GPU EP quarantined → CPU-only
    Disabled { reason: DisableReason },       // crash-loop breaker tripped; user CTA to re-enable
}

pub struct Supervisor { /* child handle, heartbeat task, policy */ }

impl Supervisor {
    /// Spawns if needed, completes handshake, returns a live connection.
    /// Errors are typed (InferError::HostUnavailable / ProtocolMismatch / …).
    pub async fn ensure_running(&self) -> Result<ConnHandle, InferError>;
    pub fn state(&self) -> HostState;
    pub fn kill_and_restart(&self);           // debug/prefs "restart AI engine"
}

pub struct SupervisionPolicy {
    pub heartbeat_interval: Duration,         // 2 s
    pub heartbeat_timeout: Duration,          // 10 s (pings answered by I/O thread, never blocked by a run)
    pub restart_backoff: ExpBackoff,          // 500 ms → 8 s, ×2
    pub model_quarantine: CrashWindow,        // ≥3 crashes in 5 min while loading/running same ModelRef
                                              //   → quarantine (ModelRef, ep) pair, retry on CPU EP
    pub session_breaker: CrashWindow,         // ≥5 crashes per app session → Disabled + non-modal notice
    pub idle_unload: Duration,                // 60 s no requests → inferd unloads sessions (frees VRAM)
    pub idle_exit: Duration,                  // 300 s → inferd exits; supervisor respawns on demand
}
```

Failure propagation contract (implements §6 "inferd process crash" row): in-flight requests fail with `InferError::HostCrashed`; `MlService`'s policy engine retries **once** on the same EP after restart, then falls to the next EP in the chain (ultimately CPU), then fails the job gracefully with a user-visible activity-center error. **The editor session is never touched.** inferd watches its parent PID and exits if orphaned; the supervisor removes stale sockets on spawn.

### 3.4 Wire protocol v1 (`lightbox_ml::protocol`)

Framing: `magic "LBIF" · u32 frame_len · CBOR envelope · [raw payload segments]`. Tensor data travels as raw segments referenced by `TensorHdr { name, dtype, shape, seg_len }` in the envelope — never CBOR-encoded. Max frame 256 MiB (hard reject). Protocol is **frozen at the end of Phase A after nexus review** (§12 escalation item); the freeze artifact is `docs/plan/epics/E13-protocol-v1.md` generated from doc-comments.

```rust
pub const PROTOCOL_VERSION: u32 = 1;

pub enum C2S {
    Hello { protocol: u32, client_build: String, token: [u8; 32] },
    QueryDevice,
    LoadModel { model: ModelRef, path: PathBuf, ep_chain: Vec<ExecProvider>, opts: SessionOpts },
    Unload { session: SessionId },
    Run { request: RequestId, session: SessionId, priority: Priority,
          inputs: Vec<TensorHdr>, deadline_ms: Option<u32> },
    Cancel { request: RequestId },            // cooperative: ORT RunOptions::terminate + queue removal
    Ping { nonce: u64 },
    Shutdown,                                  // graceful drain
}

pub enum S2C {
    HelloAck { protocol: u32, inferd_build: String, ort_version: String },
    Device { info: DeviceInfo },               // adapters, dedicated/unified MiB, EP availability+versions
    Loaded { session: SessionId, ep: ExecProvider, partition: EpPartition, load_ms: u32,
             weights_mb: u32 },
    Unloaded { session: SessionId },
    RunDone { request: RequestId, outputs: Vec<TensorHdr>, timing: RunTiming },
    Progress { request: RequestId, done: u32, total: u32 },   // tiled runs / long sessions
    Busy { request: RequestId, queue_depth: u32 },            // backpressure: bounded queue full
    Failed { request: RequestId, error: WireError },
    Pong { nonce: u64, busy_runs: u32 },
    Bye,
}

pub struct SessionOpts { pub intra_threads: u16, pub coreml_compute_units: CoreMlUnits /* All|CpuAndGpu|CpuOnly */,
                         pub allow_mixed_partition: bool }
```

**Backpressure contract (the nexus-review surface):** inferd holds two bounded FIFO queues — `Interactive` (depth 2) served strictly before `Batch` (depth 8). A `Run` arriving at a full queue gets an immediate `Busy` (never silent buffering). The client enforces a **credit window** per priority (Interactive 2, Batch 8 in flight) so `Busy` is a bug-detector, not a steady-state mechanism; batch producers (E14 indexing) block on credits, which composes with E06 pause (pausing a Background job stops credit consumption; in-flight requests drain). One GPU run executes at a time (VRAM semaphore, §3.6); CPU-EP runs may execute concurrently up to `min(cores/2, 4)`.

### 3.5 Model packs (`lightbox_ml::packs`)

Manifest — `pack.toml` at the pack root (canonical JSON copy stored in `model_pack.manifest_json`):

```toml
[pack]
id            = "seg-sam2-small"
version       = "1.2.0"
kind          = "model"                  # "model" | "runtime"
title         = "Segmentation — SAM2 Small"
license       = "Apache-2.0"             # SPDX; allowlist-enforced at install AND load
source        = "https://github.com/facebookresearch/sam2"   # provenance (§8 surface 1 manifest)
min_app       = "0.9.0"
min_protocol  = 1

[[file]]                                  # every file individually hashed; no archives
path   = "encoder.onnx"
sha256 = "3f6a…"
bytes  = 182451200

[[model]]
name        = "encoder"
file        = "encoder.onnx"
task        = "seg-encoder"               # adapter dispatch key
vram_weights_mb = 180
vram_peak_mb    = 1400                    # conservative activation estimate at max input
cpu_viable      = true
tile_capable    = false
ep_denylist     = []                      # e.g. ["coreml"] for dynamic-shape-hostile graphs
[model.io]                                # drives client-side pre/post-processing
inputs  = [{ name="image", dtype="f32", layout="NCHW", shape=[1,3,1024,1024], norm="imagenet" }]
outputs = [{ name="embeddings", dtype="f32" }]

[[mirror]]
url = "https://models.lightbox.example/seg-sam2-small/1.2.0/"
[[mirror]]
url = "https://huggingface.co/lightbox/seg-sam2-small/resolve/v1.2.0/"
```

```rust
pub struct PackManager { /* registry cache, dao, downloader, jobs handle */ }

impl PackManager {
    pub fn refresh_registry(&self) -> JobFuture<Vec<PackListing>>;   // offline-safe: cached copy on failure
    pub fn available(&self) -> Vec<PackListing>;
    pub fn installed(&self) -> Vec<InstalledPack>;

    /// Resumable (HTTP Range), per-file sha256-verified, mirror-fallback, free-space-preflighted,
    /// atomic (tmp dir → fsync → rename). Background-class job; progress via MlEvent::PackProgress.
    pub fn install(&self, id: &PackId, req: semver::VersionReq) -> JobFuture<InstalledPack>;

    pub fn verify(&self, id: &PackId) -> JobFuture<VerifyReport>;    // re-hash all files
    pub fn repair(&self, id: &PackId) -> JobFuture<InstalledPack>;   // re-download failed files only
    pub fn remove(&self, id: &PackId) -> anyhow::Result<()>;         // refuses while a session holds it
    pub fn import_local(&self, dir: &Path) -> anyhow::Result<InstalledPack>;  // user pack; same gates

    /// name(+version-req) → exact pinned ModelRef; verifies file hash before first load per app run.
    pub fn resolve(&self, sel: &ModelSel) -> Result<(ModelRef, PathBuf, ModelMeta), InferError>;

    /// Emits the model-weights license manifest consumed by the §8 surface-1/-3 CI gates.
    pub fn export_license_manifest(&self) -> WeightsManifest;
}
```

License gate (mandate + §1.5): `install`/`import_local` **hard-fail** any pack whose `license` is not in the allowlist `{Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause}` or that appears on the denylist (`InsightFace`-derived weights, non-commercial riders). The same check re-runs at `resolve` so a hand-copied directory can't bypass it.

Runtime component: `PackId("runtime-ort")`, `kind="runtime"`, one pack per platform-arch, containing the ORT + DirectML/CoreML-EP dylibs, version-locked to the app build (`min_app` == exact). Installed under app-support (machine-local, shared across catalogs — it is code, not catalog data); inferd receives its path at spawn and refuses a version mismatch at handshake.

### 3.6 VRAM gate (`lightbox_ml::gate`)

```rust
pub struct DeviceProfile {
    pub adapters: Vec<AdapterInfo>,          // name, dedicated_mb, shared_mb
    pub unified_memory_mb: Option<u32>,      // Apple Silicon / iGPU
    pub eps: Vec<EpStatus>,                  // per EP: Available{version} | Missing | FailedValidation{detail}
    pub probed_at: SystemTime,
    pub validations: Vec<EpValidation>,      // sanity-model golden check results, cached
}

pub enum GateVerdict {
    RunGpu { ep: ExecProvider },
    RunTiled { ep: ExecProvider, grid: (u32, u32), overlap_px: u32 },
    RunCpu,
    AskUser { recommended: Box<GateVerdict> },   // below floor → E08 renders “run on CPU?” (§6)
    Deny { reason: GateDenyReason },              // e.g. GpuOnly policy + no viable EP
}

pub struct GateDecision { pub verdict: GateVerdict, pub required_mb: u32, pub budget_mb: u32,
                          pub model: ModelRef }

/// Pure function — unit-testable against a fixture matrix (task D2).
pub fn admit(meta: &ModelMeta, dev: &DeviceProfile, prefs: &MlPrefs,
             input_px: (u32, u32), policy: FallbackPolicy) -> GateDecision;
```

Thresholds (encode §6 verbatim, all pref-overridable): full-GPU path recommended at **≥ 8 GB dedicated or ≥ 16 GB unified**; below that, models declaring `tile_capable` run tiled and others fall to smaller pack variants or CPU; below the per-model `vram_peak_mb` floor with no tiling path, the verdict is `AskUser{RunCpu}` — the §6 "insufficient VRAM — run on CPU?" flow. inferd enforces a **VRAM budget semaphore**: default budget `min(50% of dedicated, dedicated − 2 GB)` (unified: `25%` of total), one GPU run at a time in v1; `OutOfMemory` at run time is caught and re-admitted as `RunTiled`→`RunCpu` (never a crash). Session-manager eviction (LRU) triggers when loading a model would exceed the weights budget.

### 3.7 Error taxonomy (`lightbox_ml::types::InferError`)

```rust
pub enum InferError {
    HostUnavailable { state: HostState },
    HostCrashed { will_retry: bool },
    ProtocolMismatch { host_build: String },
    RuntimeMissing,                                  // runtime-ort pack not installed → CTA
    ModelNotInstalled { pack: PackId },
    PackIntegrity { pack: PackId, file: String },    // hash mismatch at resolve/load
    LicenseBlocked { pack: PackId, license: String },
    EpInitFailed { ep: ExecProvider, detail: String },
    Gated(GateDecision),                             // caller surfaces AskUser/Deny
    OutOfMemory { ep: ExecProvider },
    Busy, Cancelled, Timeout,
    RunFailed { detail: String },                    // ORT run error (mapped, message sanitized)
    Io(std::io::Error),
}
```

Every variant maps to a stable user-facing message id (E08 renders); `RunFailed`/`EpInitFailed` details go to logs, not the UI.

---

## 4. Data model & migrations

### 4.1 Catalog migration `mNN_e13_ml_platform` (number assigned by E01's migration sequence)

Schema v1 (§3.1) already declares `model_pack (id, name, version, hash, license, installed)`. This migration extends it and adds per-file integrity. All statements run in one transaction under the E01 migration framework (copy-on-write upgrade rules apply).

```sql
-- Pack lifecycle + provenance (extends §3.1 model_pack)
ALTER TABLE model_pack ADD COLUMN kind TEXT NOT NULL DEFAULT 'model'
    CHECK (kind IN ('model','runtime'));
ALTER TABLE model_pack ADD COLUMN status TEXT NOT NULL DEFAULT 'installed'
    CHECK (status IN ('downloading','installed','failed','disabled','quarantined'));
ALTER TABLE model_pack ADD COLUMN manifest_json TEXT;        -- canonical JSON of pack.toml
ALTER TABLE model_pack ADD COLUMN source_url TEXT;
ALTER TABLE model_pack ADD COLUMN total_bytes INTEGER;
ALTER TABLE model_pack ADD COLUMN installed_at INTEGER;      -- unixepoch
ALTER TABLE model_pack ADD COLUMN verified_at INTEGER;

CREATE UNIQUE INDEX IF NOT EXISTS idx_model_pack_name_ver ON model_pack(name, version);

-- Per-file integrity (packs are directories of hashed files; verify/repair granularity)
CREATE TABLE model_file (
    id       INTEGER PRIMARY KEY,
    pack_id  INTEGER NOT NULL REFERENCES model_pack(id) ON DELETE CASCADE,
    rel_path TEXT    NOT NULL,
    sha256   TEXT    NOT NULL,
    bytes    INTEGER NOT NULL,
    UNIQUE (pack_id, rel_path)
);
```

Not in the catalog (deliberately): the **device profile** and **EP validation cache** are machine-local JSON under app-support — a catalog moves between machines and must not carry hardware conclusions. The **quarantine list** (`(ModelRef, ep)` pairs from the crash-loop breaker) also lives machine-local. Provenance of *results* (which model produced which mask/embedding) is stored by the consumers in their tables (`mask_component.ai_recipe`, `embedding.model_id` — E14), keyed by the `ModelRef` we return; E13 adds no result tables.

### 4.2 Prefs keys (registered with E01's prefs store; E08 binds the UI)

| Key | Type | Default | Meaning |
|---|---|---|---|
| `ml.enabled` | bool | true | master switch; false → `MlService` answers `HostUnavailable` with CTA, never spawns |
| `ml.fallback` | enum | `GpuThenCpu` | global `FallbackPolicy` default |
| `ml.ep_override` | enum? | none | force a specific EP (debug/support) |
| `ml.cuda_enabled` | bool | false | opt-in use of a **user-installed** CUDA/TensorRT EP (§1.5) |
| `ml.vram_budget_mb` | u32? | auto | overrides the §3.6 budget formula |
| `ml.idle_unload_secs` / `ml.idle_exit_secs` | u32 | 60 / 300 | VRAM release cadence |
| `ml.batch_parallelism` | u32 | auto | CPU-EP concurrent batch runs cap |
| `ml.registry_urls` | list | bundled | extra registries (advanced users) |

---

## 5. Ordered task breakdown

35 tasks, each ≤ 1 engineer-day, in six phases. Phases A→B→C can interleave across two engineers after A5 (protocol) lands; D–F serialize behind B and C. Task ids are stable for tracking.

### Phase A — protocol, transport, process skeleton (7 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **A1** | Scaffold `lightbox-ml` (lib) + `lightbox-inferd` (bin) crates; workspace, feature flags (`neural-denoise` off), CI build on macOS/Windows/Linux | Both crates build in the CI matrix; `lightbox-inferd` pulls no catalog/jobs/UI crates (asserted by `cargo tree` check in CI) |
| **A2** | `protocol` module: `C2S`/`S2C`, `TensorHdr`, CBOR envelope + raw-segment framing, 256 MiB cap, `PROTOCOL_VERSION` | Round-trip proptest over all frame types incl. multi-tensor payloads; malformed/oversized frames rejected with typed errors, no panic (fuzz corpus seeded) |
| **A3** | `transport`: UDS (tokio) + Windows named pipe behind `Transport` trait; per-user path/DACL; stale-socket cleanup | Loopback echo test passes on all 3 OS in CI; a second user cannot connect (perm test, unix); 64 MB frame transfers < 100 ms locally |
| **A4** | inferd skeleton: stdin token read, `Hello`/`HelloAck` (protocol + build lockstep), `Ping`/`Pong` from I/O thread, parent-PID watch, idle-exit timer, structured logging to a rotating file | Handshake with wrong token/build/protocol → connection refused with typed error; killing the parent makes inferd exit ≤ 5 s (test); idle inferd exits after configured timeout |
| **A5** | Supervisor: spawn (binary discovery from bundle path), handshake, heartbeat, exit detection, restart with exp backoff, `HostState` machine + events | Integration test: kill inferd → state walks Ready→Restarting→Ready; heartbeat timeout triggers kill+restart; stale socket from a previous run doesn't block spawn |
| **A6** | `MlService` core: request-id correlation, per-priority credit windows, `Busy` handling, `Cancel` plumbing from `CancelToken`, timeout enforcement | Concurrency test with a mock inferd: 100 interleaved requests correlate correctly; cancelled request produces `Cancelled` and a `Cancel` frame; credit window never exceeds configured in-flight |
| **A7** | **Protocol freeze gate:** write `E13-protocol-v1.md` from doc-comments; walk nexus through the streaming/backpressure contract (§12 escalation item); apply review edits | Nexus sign-off recorded in the doc header; protocol version pinned; any post-freeze change requires a version bump + supervisor lockstep note |

### Phase B — ORT engine & execution providers (8 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **B1** | ort integration with `load-dynamic`: locate ORT dylibs from runtime dir passed at spawn; version handshake (`ort_version` in `HelloAck`); refuse mismatch | inferd with the pinned ORT loads a toy identity `.onnx` and returns correct output on CPU EP; wrong/missing dylib → clean `RuntimeMissing`-class startup error, not a crash |
| **B2** | Session manager: `LoadModel`/`Unload`, mmap model files, session LRU keyed by `ModelRef`+EP, weights-budget eviction, idle unload | Loading a 3rd model over budget evicts LRU; reload hits mmap cache; `Unloaded` emitted; unload frees process RSS (measured) |
| **B3** | Run executor: input binding, output extraction, `RunOptions` timeout + terminate-on-cancel, ORT error → `WireError` mapping | Toy model run round-trips f32/u8/i64 tensors bit-exactly; cancel mid-run (large synthetic model) returns `Cancelled` ≤ 500 ms after the frame; ORT exceptions never unwind across FFI |
| **B4** | Two-priority bounded queue in inferd: Interactive-before-Batch scheduling, `Busy` on overflow, `Progress` emission hooks | Queue test: batch flood + one interactive request → interactive dequeues next; overflow returns `Busy` immediately; drain order deterministic |
| **B5** | CoreML EP init (macOS): MLProgram format, compute-units option, model cache dir; fallback to CPU on init failure; `EpPartition` reporting (count of nodes placed on EP vs CPU) | On an M-series runner: toy CNN loads on CoreML, partition reported; a dynamic-shape model denylisted for CoreML falls back cleanly; init failure path unit-tested via bad option injection |
| **B6** | DirectML EP init (Windows): adapter selection (highest-VRAM non-software), device id in `DeviceInfo`; CPU fallback on failure | On the Windows runner: toy CNN loads on DirectML; forced-failure path (bogus adapter id) degrades to CPU with `EpInitFailed` surfaced in `Loaded.ep` |
| **B7** | CUDA/TensorRT detection (never bundled, §1.5): probe user-installed provider dylibs + driver, behind `ml.cuda_enabled`; self-test before first use; quarantine on failure | With no CUDA present: probe reports `Missing`, zero warnings; mock-present test exercises the enable→self-test→active path; SBOM emitter (F4) asserts no CUDA/TensorRT artifact in our shipped set |
| **B8** | EP validation harness: tiny bundled sanity model (~200 KB, committed) run on each available EP at first use; output vs committed reference within tolerance; result cached in `device_profile.json` | Corrupted-driver simulation (perturbed reference) marks EP `FailedValidation` and drops it from chains; cache prevents re-validation on every launch; re-probe on driver/OS version change |

### Phase C — model-pack manager (7 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **C1** | `pack.toml` parser + validator: schema, SPDX allowlist/denylist gate, io-spec, vram fields, `ep_denylist`, semver; canonical-JSON emitter | Fixture suite: valid packs parse; GPL/non-commercial license → `LicenseBlocked`; missing hash/field → typed validation error; canonical JSON stable across runs (hashable) |
| **C2** | Catalog migration `mNN_e13_ml_platform` + `ModelPackDao` (CRUD, status transitions, per-file rows) | Migration up on a v1 fixture catalog passes `integrity_check`; DAO round-trips a manifest; unique (name,version) enforced; `kill -9` during migration leaves old schema intact (E01 copy-on-write harness) |
| **C3** | Registry: bundled registry file + `refresh_registry` over HTTPS with cached-copy offline fallback; merge user registries | Offline start serves cached listings; malformed remote registry rejected, cache retained; listing → `PackListing` including platform/EP compatibility flags |
| **C4** | Downloader: per-file HTTP Range resume, streaming sha256, mirror failover, free-space preflight (§6 disk-full row), bandwidth-limited Background job with progress events | Kill/resume test resumes mid-file (byte-verified); first mirror 404/500 → second mirror; hash mismatch → file re-fetched once then `PackIntegrity`; preflight refuses when free space < 1.2× pack size |
| **C5** | Atomic install/remove/verify/repair: tmp-dir → fsync → rename; `model_file` rows written in the same catalog txn as `status='installed'`; remove refuses while sessions hold the pack | `kill -9` mid-install leaves no partial pack visible and catalog consistent (fault-injection); verify detects a flipped bit in any file; repair re-downloads only the bad file |
| **C6** | `import_local` (user packs) + `resolve` with load-time hash re-verification + quarantine list consultation | Importing a directory with a tampered file fails with the exact file named; resolve of `Latest` picks highest installed semver; pinned resolve of an uninstalled version → `ModelNotInstalled` with install CTA payload |
| **C7** | Runtime component (`runtime-ort`, `kind="runtime"`): per-platform pack definitions, install to app-support, first-enable flow (`RuntimeMissing` CTA → install job → spawn), exact-version lock to app build | Fresh machine simulation: first `segment()` call yields `RuntimeMissing`; after install job completes, the same call succeeds; version-mismatched runtime refused at handshake with a repair CTA |

### Phase D — VRAM gating & tiling (4 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **D1** | Device probe in inferd (`QueryDevice`): adapter enumeration + dedicated/unified memory (DXGI / Metal / sysfs), EP availability + versions; client-side `device_profile.json` cache with staleness rules | Probe returns sane values on all 3 CI platforms; cache invalidated on OS/driver/app version change; probe cost ≤ 200 ms warm |
| **D2** | `admit()` pure gate: §6 thresholds (8 GB dedicated / 16 GB unified), per-model `vram_peak_mb`/`cpu_viable`/`tile_capable`, prefs overrides, `FallbackPolicy` interaction → `GateVerdict` matrix | Table-driven unit tests over ≥ 20 device×model×prefs fixtures incl. Apple-unified, 4 GB dGPU, iGPU-only, CPU-only; every §6 sentence maps to at least one asserted case |
| **D3** | Tiled-inference framework in `tasks`: overlap-and-feather tiler at tensor level for image-to-image tasks, per-tile `Progress`, cancel at tile boundaries, deterministic blend | Synthetic blur model: tiled output vs untiled within PSNR ≥ 60 dB away from borders; cancel between tiles ≤ 1 tile latency; progress events monotonic (done/total exact) |
| **D4** | VRAM budget semaphore + OOM recovery: single GPU run in v1, budget formula + pref override; catch EP OOM → re-admit as `RunTiled` → `RunCpu`; wire `AskUser` verdict through `MlEvent::GatePrompt` | Forced-OOM test (oversized synthetic input on capped budget) completes via tiled retry without inferd restart; second forced failure lands on CPU; the decision chain appears in the job's activity log |

### Phase E — typed tasks, jobs, façade (5 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **E1** | Segmentation task adapter: manifest-io-spec-driven preprocess (resize/pad/normalize from `ImageInput`), prompt encoding (points/box/class), postprocess to `MaskRaster` (+ score), `Provenance` assembly | Golden test with a small real seg model (U2-Net-lite class) on CPU EP: known image → raster matches committed golden ≥ 99% pixel agreement at 0.5 threshold; provenance fields fully populated |
| **E2** | Embedding task adapters: `embed_image` (resize/center-crop/normalize per io-spec, L2-normalize output) + `embed_text` (pre-tokenized ids in, vector out); `run_model` passthrough | Small CLIP-class model on CPU: cosine(sim) of (image, matching text) > (image, mismatched text) on a 5-pair fixture; vectors L2-normalized; `run_model` round-trips arbitrary tensor sets |
| **E3** | E06 integration: every client call runs as a job (`Foreground` for user-invoked, `Background` pausable for batch), `CancelToken` → protocol `Cancel`, pause = stop issuing credits + drain in-flight, progress mapping to the activity model | Pausing a batch embed job stops new `Run` frames ≤ 1 request; cancel from the activity center kills an in-flight GPU run; job states/progress render correctly in the E06 activity model (headless assertion) |
| **E4** | Retry/fallback policy engine in `MlService`: `HostCrashed` → restart + 1 retry same EP → next EP → fail; `EpInitFailed`/quarantine → skip EP; `Gated(AskUser)` surfaced, answer resumes the same request | Scripted-failure mock matrix: each policy path asserted incl. "crash during CPU retry → graceful failure, editor untouched"; no retry storm (≤ 2 total attempts per EP) |
| **E5** | Core façade + events: `MlCommand::{InstallPack,RemovePack,VerifyPack,SetPrefs,RestartHost,AnswerGatePrompt}`, `MlQuery::{Status,Packs,DeviceProfile}`, `MlEvent` on the core event bus; `lightbox-cli ml` subcommands (status/install/verify/run-selftest) for headless E2E | CLI drives install→status→selftest against a temp catalog headlessly (this is the §8 E2E hook); commands round-trip through the E01 command bus with no UI types below the boundary |

### Phase F — hardening, tests, CI/license, docs (4 tasks)

| # | Task | Acceptance criteria |
|---|---|---|
| **F1** | Fault-injection suite: `kill -9` inferd mid-run (×100 loop), crash-loop breaker (model quarantine + session breaker), orphan test, stale-socket test, handshake-token negative tests | All pass deterministically in CI on 3 platforms; post-kill catalog `integrity_check` clean; breaker states reachable and recover via `RestartHost` |
| **F2** | Real-model integration tests wired into CI: seg + embed small models on CPU EP (all platforms), CoreML/DirectML paths on native runners, per-EP output tolerance vs committed reference (bounded, since §4.5 bakes rasters — tolerance is QA, not a product promise) | PR-blocking: CPU-EP subset (< 90 s). Nightly: GPU-EP runs on macOS/Windows runners with per-EP tolerance gates; failures file issues per §8 perf policy |
| **F3** | Perf harness (nightly, §8): budgets from §7 of this spec — cold spawn, model load, segment latency, embed throughput, IPC overhead micro-bench (criterion) | Harness emits trend JSON; regression > 20% vs baseline opens a tracked issue; budgets table in this spec is the assertion source |
| **F4** | License/SBOM emitters + threat-model handoff + docs: `export_license_manifest` wired to §8 surface-1/-3 gates; CycloneDX component entries for ORT + EP dylibs + inferd (surface 2, incl. "no CUDA/TensorRT shipped" assertion); pack-authoring guide; EP-troubleshooting runbook; security-engineer review packet (download integrity, IPC, pack parsing — §12) | CI license gates consume the emitted manifests and go green; a synthetic GPL-weights pack fails the gate; security review scheduled with the packet delivered (review itself is the §12 owner's) |

Total: 35 tasks ≈ 7 pw nominal — within the L (~6–9 pw) envelope with slack for EP-specific debugging (the known time sink, Risk R2 below).

---

## 6. Test plan

Per the §8 strategy; E13's rows:

| Layer | What | Gate |
|---|---|---|
| **Unit** | Protocol round-trip (proptest) + malformed-frame fuzz corpus; manifest parse/validate incl. license gate; `admit()` fixture matrix; downloader hash/resume state machine; DAO/migration; supervisor state machine (mock child) | PR-blocking |
| **Integration** | Real inferd spawn on all 3 platforms: handshake, toy-model run, cancel, Busy backpressure, credit windows, pause/resume via E06, install→resolve→load→run pipeline in a temp catalog via `lightbox-cli ml` | PR-blocking (CPU-EP subset < 90 s); full matrix nightly |
| **Fault injection** | `kill -9` inferd mid-run; kill parent (orphan); mid-install kill (atomicity + catalog `integrity_check`); crash-loop breaker; forced EP OOM → tiled → CPU chain | PR-blocking (extends the E01 kill-9 harness) |
| **Golden / tolerance** | Sanity model per-EP validation vs committed reference; real seg/embed models: CPU-EP goldens PR-blocking, CoreML/DirectML tolerance nightly on native runners. Note: cross-EP drift is *expected* (§4.5) — these gates bound it for QA and driver-regression detection; the product-level reproducibility artifact is E14's baked raster, not EP determinism | PR-blocking / nightly as noted |
| **Perf** | §7 budget harness: spawn, load, segment latency, embed throughput, IPC micro-benches | Nightly; regression → tracked issue (§8 policy) |
| **License/CI** | cargo-deny (surface 1) incl. weights-manifest gate fed by `export_license_manifest`; SBOM policy check (surface 2) over ORT + EP dylibs + the runtime component, asserting DirectML/CoreML/CPU-only shipped set and dynamic linkage; synthetic-violation fixtures (GPL pack, x-license dylib) must fail | PR-blocking; binary inspection release-blocking (E16 runners) |
| **Security-adjacent** | Handshake-token rejection, cross-user socket access denial, path-traversal fixtures against pack file paths (`rel_path` with `..`/absolute rejected), oversized-frame rejection | PR-blocking; feeds the §12 security-engineer review |

---

## 7. Performance budgets (E13-owned; nightly-asserted via F3)

Reference hardware per §7: RTX 3060-class / Apple M-series base. These are platform budgets — E14 feature latency budgets stack on top of them.

| Metric | Budget |
|---|---|
| inferd cold spawn → `Ready` (runtime installed, no model load) | ≤ 2 s |
| Model session load, SAM2-small-class encoder (~180 MB), warm cache | ≤ 5 s GPU EP / ≤ 3 s CPU |
| Segment (1024² input, SAM2-small-class), end-to-end incl. IPC + pre/post, GPU EP | ≤ 1.5 s p95 |
| Same, CPU EP (8-core) | ≤ 8 s p95 (honest floor, shown with progress) |
| Embed throughput, CLIP-base-class, batch, GPU EP | ≥ 8 img/s |
| Same, CPU EP | ≥ 1.5 img/s |
| IPC overhead: 4 MB raster round-trip (excl. inference) | ≤ 10 ms |
| Idle VRAM footprint after `idle_unload` | 0 (sessions freed; verified via probe) |
| Editor impact: interactive render p95 (§7 <100 ms) while a batch embed job runs | unchanged (inferd is a separate process; verified in the E06 starvation scenario test) |

---

## 8. Risks & open questions

### Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | **`ort` crate API churn / ORT version coupling** — `ort` 2.x evolves quickly; EP option surfaces change | Pin exact `ort` + ORT versions; runtime component is version-locked to the app build (lockstep, no matrix); upgrade is a deliberate task with the full F2 suite as the gate |
| R2 | **CoreML EP partial-graph placement** — SAM2-class decoders with dynamic shapes often fall partially or wholly to CPU under CoreML, silently costing 2–5× | `EpPartition` reported per load and logged; per-model `ep_denylist` in manifests (E14 curates); budgets asserted per-EP nightly on native runners so a silent placement regression is caught |
| R3 | **VRAM estimation is heuristic** — activation peaks vary with input size and EP; a wrong `vram_peak_mb` under-gates or OOMs | Conservative manifest values + runtime OOM catch → tiled → CPU re-admission (D4); gate is advisory-plus-recovery, never trust-the-estimate-only |
| R4 | **DirectML adapter selection on hybrid laptops** (iGPU+dGPU) picks the wrong device | Explicit highest-VRAM non-software default + `ml.ep_override`/adapter pref; adapter identity logged in provenance for support |
| R5 | **Crash-loop on a specific (model, driver) pair** burns the restart budget and disables AI broadly | Quarantine granularity is `(ModelRef, ep)`, not global; breaker degrades stepwise (EP → CPU → disabled) with user-visible state and one-click re-enable |
| R6 | **Model hosting/mirror availability** — packs are useless if the primary mirror dies | Mirror list per pack + registry refresh can add mirrors post-release; offline cache; local import path as the last resort |
| R7 | **Windows named-pipe + AV interference** (security products throttling/blocking local pipes or the spawned process) | Named pipe with explicit per-user DACL; code-signed inferd (E16); runbook entry + `lightbox-cli ml selftest` for support triage |
| R8 | **Scope suction from E14** — "just add this one model-specific op to the platform" | Hard rule: model-specific logic lives in adapters or E14; the wire protocol only ever carries generic tensors (protocol frozen at A7; changes need a version bump + review) |

### Open questions (named, non-blocking to start)

1. **Model dedup across catalogs.** §3.3 places packs under `<catalog>.lbdata/models/`, so two catalogs duplicate a 2 GB pack. Proposal for a later minor: machine-local content-addressed store with per-catalog hardlinks. Not v1; raised for the architect's backlog. (The runtime component already avoids this by living in app-support.)
2. **Who operates the pack mirrors** (primary CDN, HF org, bandwidth budget)? Ops/operator decision (§12 spirit); E13 ships the formats and consumes URLs. Needed before M3 beta, not before coding.
3. **Registry authenticity:** v1 ships hash-pinned manifests over HTTPS from a bundled registry URL. Do we additionally want a signing key (minisign/TUF-lite) for the registry file itself in v1, or v1.x? Recommended: v1.x, security-engineer review (F4 packet) to confirm HTTPS+pinning is acceptable for v1.
4. **Linux EP posture** (not architecturally excluded, per mandate): CPU EP is the v1 baseline; ROCm/CUDA as user-installed detections mirror the Windows CUDA path. Confirm nothing in E13 blocks it — believed true by construction (EP chain is data).
5. **Interactive priority class naming:** user-invoked inference is spec'd as E06 `Foreground`. If E06's final class semantics make `Foreground` import/export-only, we need a ruling on whether user-invoked ML joins `Foreground` or a new sub-priority — one-line change here, flagged to the E06 owner.

---

## 9. Definition of done

E13 is done when all of the following hold on the M3 branch:

1. **Crash isolation proven (the M3 exit criterion):** the F1 fault-injection suite is PR-blocking and green on macOS/Windows/Linux — `kill -9` of inferd mid-inference fails the job gracefully, the supervisor restarts the host, the editor session and catalog are untouched (`integrity_check` clean), and the crash-loop breaker degrades EP→CPU→disabled with user-visible state.
2. **The shipped EP posture matches §1.5 exactly:** DirectML/CoreML/CPU load and validate on their platforms; CUDA/TensorRT are detected-only behind `ml.cuda_enabled`, and the surface-2 SBOM asserts no NVIDIA artifact in any shipped/published component.
3. **Pack lifecycle complete:** install (resumable, hash-verified, mirror-fallback, atomic), verify, repair, remove, local import, and the runtime-component first-enable flow all work headlessly via `lightbox-cli ml`; a tampered or GPL-licensed pack cannot be installed *or* loaded.
4. **VRAM gating live:** the `admit()` matrix implements the §6 thresholds; forced-OOM recovers via tiled→CPU without a host restart; the "insufficient VRAM — run on CPU?" decision reaches the shell as a typed event.
5. **E14 can build without touching this epic:** `segment`, `embed_image`, `embed_text`, `run_model` are stable, documented, and demonstrated end-to-end with one real segmentation model and one real embedding model on CPU EP in CI (goldens committed); every result carries full `Provenance` sufficient for §4.5 `ai_recipe` storage.
6. **Jobs integration honest:** batch inference is Background-class, pausable and cancellable from the activity model; user-invoked inference preempts batch inside inferd; a running batch embed does not move the interactive-render p95.
7. **Protocol frozen and reviewed:** `E13-protocol-v1.md` exists with the nexus review recorded (§12); handshake enforces lockstep.
8. **CI/license gates green:** weights-manifest gate (surface 1/3) and ORT/EP SBOM entries (surface 2) are emitted by this epic's code and consumed by the PR-blocking gates; synthetic violations fail.
9. **Budgets baselined:** the §7 table is asserted by the nightly harness on reference hardware with trend tracking.
10. **Docs delivered:** protocol spec, pack-authoring guide, EP troubleshooting runbook, and the security-review packet handed to the §12 security-engineer review.
