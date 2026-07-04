# E01 — Implementation deviations from spec

Deviations from `E01-foundation-workspace-catalog.md`, recorded per phase.
Each entry names the spec point, the deviation, and why.

## Phase 1 — Workspace & guardrails

- **Project license selected: Apache-2.0.** Neither the mandate nor the epic
  spec pins the project's own license (mandate constraint 5 requires
  "permissive or weak-copyleft"). Apache-2.0 chosen (ecosystem default,
  patent grant); LICENSE, `LICENSES/`, SPDX headers, and `REUSE.toml` all use
  it. Flag for CTO review if dual MIT/Apache-2.0 is preferred.
- **Crate versions (July 2026) newer than spec-era assumptions:** wgpu 30.0.0
  (`Instance::new` takes `InstanceDescriptor` by value, no `Default` —
  `new_without_display_handle()` used; `request_adapter` returns
  `Result<Adapter, RequestAdapterError>`), thiserror 2.x, serde 1.0.228,
  tracing-subscriber 0.3.23. Spec interfaces unaffected. Edition **2021 kept**
  as the spec/architecture §1.1 explicitly pins it, despite edition 2024
  being current. Toolchain pinned 1.96.1; MSRV recorded as 1.96.
- **xxh3-128 via `twox-hash` (MIT), not `xxhash-rust`.** The commonly-used
  `xxhash-rust` crate is BSL-1.0, which is not on the spec §5 T3 license
  allowlist; `twox-hash` 2.x provides xxh3-128 under MIT. Pins use the
  canonical (big-endian) hex rendering; `lightbox-decode::hash_file`
  (Phase 5) must adopt the same canonicalization.
- **Fixture corpus: 9 CC0 raws instead of "≈12", plus generated non-raws.**
  All seven spec'd mounts are covered (CR2, CR3, NEF ×2, ARW, RAF ×2 — one
  Bayer, one X-Trans — ORF, DNG), ~116 MB total, chosen to keep the CI cache
  small; the manifest schema makes adding more a one-entry change.
  raw.pixls.us hosts raws only, so the JPEG/TIFF/PNG legs and the two corrupt
  fixtures are **deterministically generated** by `cargo xtask fixtures`
  (spec implied downloads for the whole corpus); their manifest entries use a
  `generator` spec instead of a `url`, still hash-pinned and licensed
  (CC0, self-made). The tiny JPEG is a committed 826-byte self-made asset
  (`tools/xtask/assets/`) so its pin can never drift with an encoder upgrade.
- **`docs/plan/licensing.md` and this file created under `docs/`** — the
  epic-execution ground rules say "do not modify docs/", read as no *edits*
  to approved plan documents; both files are new, spec-mandated artifacts
  (T3, and the deviation-recording rule itself).
- **`lightbox-edit`'s frozen `Recipe { schema, pv }` placeholder implemented
  in Phase 1** as part of crate stubbing (spec §2 defines the crate's entire
  E01 content as exactly this placeholder; no later E01 phase claims it, and
  Phase 2's `RenderRequest` needs it).
- **`deny.toml` additions beyond the spec'd allowlist mechanics:**
  `allow-wildcard-paths = true` (intra-workspace path deps carry no version),
  `multiple-versions = "warn"`, yanked-crate denial, and an explicit ban on
  `openssl`. The license allowlist itself is exactly the spec's list.
- **CI toolchain install uses `rustup show active-toolchain`** (rustup
  auto-installs the toolchain pinned in `rust-toolchain.toml`) instead of a
  third-party toolchain action — fewer moving parts, same pin.
- **T4 observability lives in `lightbox-core::observability`** (spec doesn't
  name a crate). File logging is synchronous write-through (daily rotation via
  `tracing-appender`) rather than a buffered background writer, which is what
  makes "panic hook that logs + flushes" trivially true at M0 log volumes.
- **nightly.yml not created in Phase 1.** The workspace layout in spec §2
  lists it, but both of its jobs (long fault-injection, perf harness) are
  Phase 3/8 deliverables (T13, T28); an empty workflow would be noise.

## Phase 2 — Zero-copy seam tracer bullet

- **wgpu downgraded 30 → 29 (29.0.4) workspace-wide.** Current stable
  egui/eframe/egui-wgpu (0.35.0) link against wgpu ^29, and the zero-copy
  seam (§2.3 seam 2) requires the shell and the engine to share ONE
  `wgpu::Device` — i.e. one wgpu crate version. Phase 1's wgpu 30 pin was
  provisional; the APIs Phase 1 used are identical in 29. Revisit when egui
  moves majors.
- **`CancelToken` (a T14 item) pulled forward into Phase 2** and implemented
  in `lightbox-jobs` (tokio `sync` feature only). The frozen §3.4/§3.5
  surfaces (`GpuCtx`, `SourceResolver::resolve`) carry it, so the Engine seed
  could not compile without it. The rest of T14 (`JobSystem`, `Class`,
  `spawn`/`spawn_blocking`, `JobHandle`) remains Phase 4.
- **`RenderPlanner` scaffolding added (non-frozen).** §3.4 defines the ticket
  lifecycle but not how the seed engine maps a `RenderRequest` to its single
  node + params. `Engine::set_planner(Arc<dyn RenderPlanner>)` fills that
  hole, documented as M0 scaffolding that E05.1's recipe→DAG builder
  replaces. The frozen `Engine::new(gpu, registry, sources)` signature is
  unchanged. Phase 6 plans `display.transform`; Phase 2's tracer plans
  `solid.color`.
- **Engine output textures are `Rgba8Unorm`, not `Rgba8UnormSrgb`.** egui
  0.35's user-texture path samples registered textures as "normal"
  non-sRGB-aware gamma data (and its docs require `Rgba8Unorm`), and the M0
  node contract already has nodes writing sRGB-*encoded* values explicitly
  (the display-transform algorithm ends with an OETF encode). Same bytes,
  correct compositing, and the format stays storage-capable for Phase 6
  compute output.
- **egui 0.35 API drift from the spec's sketch:** `eframe::App::update` is now
  `App::ui(&mut self, ui: &mut egui::Ui, …)`, and panels are `egui::Panel::top`
  etc. shown inside a parent `Ui`. `egui_wgpu::Renderer::register_native_texture`
  / `free_texture` / `ui.image` exist as the spec assumed; T7 implemented
  verbatim with re-registration on texture swap.
- **T7's "device pointer equality" assertion** is `Arc::ptr_eq` on the
  `GpuContext.device` the shell built from eframe's `RenderState` vs the one
  the engine reports (wgpu 29 `Device` has no public id/PartialEq). The
  stronger runtime proof is structural: wgpu validation would reject
  registering a texture created on any *other* device, so every composited
  frame re-proves the shared-device seam.
- **Shell seam smoke added to CI as `continue-on-error`** (`lightbox --smoke
  N` runs N frames headless-windowed — Xvfb on Linux — and exits nonzero if
  no `Engine::submit` texture was composited). T5's AC wants the eframe
  window proven on all 3 CI OSes; windowed apps on hosted runners are the one
  piece not verifiable from this machine, so the step is observational until
  seen green, then promoted to blocking (tracked for T29). Verified locally
  on macOS/Metal: `seam_proven=true`, exit 0.
- **Scoped cargo-deny exceptions added (deny.toml), global allowlist
  untouched:** `clipboard-win` + `error-code` (BSL-1.0 — Boost, OSI-approved
  permissive; Windows clipboard via egui-winit→arboard) and
  `epaint_default_fonts` (OFL-1.1 AND Ubuntu-font-1.0 — egui's embedded UI
  fonts; font-data licenses, i.e. the first real entry for license surface 3
  (bundled content, E16)). `docs/plan/licensing.md` was NOT updated alongside
  (deny.toml's own rule) because this epic's ground rules forbid editing
  docs/ beyond this file — needs a follow-up licensing-review commit.
- **`solid.color` node + `SolidColorPlanner` are public in
  `lightbox-render::nodes`** (spec T6 calls it a "test node"): the T7 shell
  spike and the T6 integration tests both need it, and it remains useful as
  the minimal example node. Documented as tracer/test-only, not a product
  pipeline node.
- **Ticket-table retention policy (unspecified by the spec):** terminal
  tickets are dropped on the next submit to the same viewport and capped at
  256 globally; polling an evicted ticket reads `Superseded`. This is what
  lets the texture pool actually recycle (a retained `Ready` state would pin
  its texture's refcount forever).
