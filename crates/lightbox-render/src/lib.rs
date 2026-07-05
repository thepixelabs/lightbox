// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-render` — the GPU compute render engine.
//!
//! Owned by **E01 for the Engine seed** (spec §3.4); E01 Phase 2 (T5–T6)
//! implemented: [`GpuContext`] (shared with the shell — seam 2), [`Engine`]
//! submit/poll/cancel with per-viewport latest-wins coalescing,
//! [`NodeRegistry`] keyed `(NodeId, ProcessVersion)`, the [`RenderNode`]
//! trait, the output [`TexturePool`], and the `solid.color` tracer node.
//! Phase 6 (T23–T24) added [`nodes::display_transform`] — the one real M0
//! node (WGSL compute + rayon CPU from one written algorithm spec,
//! `display_transform.md`), golden-image tested with CPU/GPU parity per
//! §4.4. **E05** generalizes the seed to the full DAG (cache, tiling,
//! progressive refinement, device-lost recovery) starting from this exact
//! trait surface — no double-build (architecture §10.1).
//!
//! # Frozen vs scaffolding
//!
//! Frozen for E02/E05 (spec §3.4/§3.5): [`GpuContext`], [`Engine`]'s
//! submit/poll/cancel/on_device_lost/backend_kind, [`RenderRequest`],
//! [`RenderState`]/[`RenderOutput`], [`RenderNode`]/[`NodeId`]/[`Params`]/
//! [`NodeRegistry`], and [`SourceResolver`]/[`SourceImage`].
//! M0 scaffolding E05.1 replaces: [`RenderPlanner`] (single-node planning)
//! and the worker-loop internals.
//!
//! This is the one workspace crate where `unsafe` may be locally allowed for
//! GPU interop (workspace lints deny it everywhere else — E01 spec §5 T1);
//! the seed needs none yet, so the workspace-level `deny` still applies here.

mod engine;
mod error;
mod gpu;
mod node;
pub mod nodes;
mod planner;
mod pool;
mod source;

pub use engine::{
    BackendKind, Engine, RenderOutput, RenderRequest, RenderScale, RenderState, RenderTarget,
    RenderTicket, Roi, ViewportId,
};
pub use error::{DeviceLostReason, EngineError, NodeError, RenderError, SourceError};
pub use gpu::GpuContext;
pub use node::{
    CpuCtx, GpuCtx, ImageBufU8, NodeId, NodeRegistry, ParamDelta, Params, RenderNode, Tile, TileCpu,
};
pub use planner::{PlannedEval, RenderPlanner};
pub use pool::{TexturePool, OUTPUT_FORMAT, OUTPUT_USAGE};
pub use source::{NullSourceResolver, SourceImage, SourcePixelFormat, SourceResolver, SourceTier};
