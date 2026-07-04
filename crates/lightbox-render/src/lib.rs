// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-render` — the GPU compute render engine.
//!
//! Owned by **E01 for the Engine seed** (spec §3.4); implemented in E01
//! Phases 2 and 6 (T5–T6, T23–T24): `GpuContext` (shared with the shell —
//! seam 2), `Engine` submit/poll/cancel with per-viewport latest-wins
//! coalescing, `NodeRegistry` keyed `(NodeId, ProcessVersion)`, the
//! `RenderNode` trait, and `DisplayTransformNode` in WGSL + CPU with golden
//! parity tests. **E05** generalizes the seed to the full DAG (cache, tiling,
//! progressive refinement, device-lost recovery) starting from this exact
//! trait surface — no double-build (architecture §10.1).
//!
//! This is the one workspace crate where `unsafe` may be locally allowed for
//! GPU interop (workspace lints deny it everywhere else — E01 spec §5 T1);
//! the seed needs none yet, so the workspace-level `deny` still applies here.
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1). The wgpu adapter
//! smoke test in `tests/adapter_smoke.rs` is Phase 1's T2 acceptance
//! criterion (every CI OS must produce an adapter, software ones count).
