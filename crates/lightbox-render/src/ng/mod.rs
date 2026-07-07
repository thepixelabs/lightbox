// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `ng` — the E05 render node-graph engine (the full §2 module map + the
//! §3 interfaces, frozen).
//!
//! # What this is
//!
//! This module is the E05 generalization of E01's one-node Engine seed: a
//! petgraph DAG of typed image ports, the [`node::RenderNode`] trait, a
//! content-keyed [`cache`], ROI/tile/progressive [`exec`]ution, a per-process
//! [`compile`]r, a device-lost [`recover`]y state machine, and the coalescing
//! [`sched`]uler. Every pixel of Develop / export / 1:1 preview flows through
//! it (spec §1).
//!
//! # Scaffold status (E05 Phase A)
//!
//! Every interface below is transcribed **as written in the spec's §3** and
//! **frozen** so the parallel phase agents touch strictly-disjoint files. Method
//! bodies are `unimplemented!("<task> (<owner>): …")` stubs naming the owning
//! phase-task; the crate compiles, but calling a stub panics. The E01 seed at
//! the crate root stays the *working* path until task **F5** promotes `ng` to
//! the root and deletes the seed.
//!
//! # File-ownership map (keep waves disjoint — see `E05-deviations.md`)
//!
//! - **A-core** — [`node`] (trait/descriptor/param/registry/schema), [`graph`],
//!   [`compile`], [`exec`] mod (topo walk + ticket store), [`exec::cpu`],
//!   [`cache`] type surface, [`stats`], [`engine`] orchestration, [`types`],
//!   [`error`], [`config`].
//! - **A-gpu** — [`gpu`] (DeviceCtx/KernelBuilder/TilePool/pipeline cache),
//!   [`exec::gpu`], [`source`] (upload path), [`sched`] canvas double-buffer,
//!   `shaders/`, [`nodes`] (engine-owned node bodies).
//! - **B** — [`cache`] real impl (VRAM+RAM tiers), CacheKey derivation,
//!   scheduler latest-wins coalescing.
//! - **C** — [`exec`] tiling (ROI plan, resize, 256² tiles, ladders, budget).
//! - **D** — [`compile`] per-PV templates + PV plumbing + PV manifest.
//! - **E** — [`recover`] (detection, rebuild, re-warm, degradation), CPU parity.
//! - **F** — `lightbox-core` wiring + `lightbox-cli render` + engine book +
//!   fuzz + perf scenario harness (mostly outside this crate).
//!
//! - **Scaffold-frozen seams** (do not re-home without a deviation entry):
//!   [`exec::backend`] (`trait Backend` — the R8 exec↔backend seam) and
//!   [`tile`] (the `TileHandle`/`TileView`/`PixelBuf` currency).

// Shared frozen vocabulary (scaffold-owned).
pub mod colorimetry;
pub mod config;
pub mod error;
pub mod tile;
pub mod types;

// The §2 module map.
pub mod cache;
pub mod compile;
pub mod engine;
pub mod exec;
pub mod gpu;
pub mod graph;
pub mod node;
pub mod nodes;
pub mod recover;
pub mod sched;
pub mod source;
pub mod stats;

// ── Curated public surface (the names §3 makes normative for E05) ────────────
pub use colorimetry::{OutputColorimetry, SourceColorimetry, SourceKind, SourceQuality};
pub use config::{BackendPref, DegradePolicy, EngineConfig, VramBudget};
pub use error::{
    CompileError, DeviceError, EngineInitError, NodeError, RegistryError, RenderError, SourceError,
};
pub use tile::{CpuTileView, PixelBuf, PixelFormat, TileHandle, TileView};
pub use types::{
    Extent, NodeId, PortType, ProcessVersion, RenderScale, Roi, TileCoord, TilePrecision,
};

pub use cache::{Bytes, CacheKey, CacheKeyInputs, CacheStats, CachedTile, NodeCache, PinLabel};
pub use compile::manifest::{PvManifest, StageDigest};
pub use compile::{
    shipping_compiler, shipping_registry, GraphTemplate, RecipeCompiler, SourceDesc, PV_TEST_999,
};
pub use engine::{
    ActiveBackend, BackendId, Engine, EngineEvent, OutFormat, OutputPayload, OutputQuality,
    RenderOutput, RenderPriority, RenderRequest, RenderState, RenderTarget, RenderTicket,
};
pub use exec::backend::{Backend, BackendEvalRequest};
pub use exec::cpu::CpuBackend;
pub use exec::{Executor, SourceInject};
pub use gpu::{DeviceCtx, KernelBuilder, TilePool};
pub use graph::RenderGraph;
pub use node::{
    AuxRequirements, CachePolicy, CpuEvalCtx, FieldDecl, GpuEvalCtx, InputRois, KernelSalt,
    NodeDescriptor, NodeFactory, NodeRegistry, ParamBlock, ParamDelta, ParamHash, ParamKind,
    ParamValue, PortDecl, PvRange, RenderNode,
};
pub use recover::{DegradeState, RecoverStateMachine};
pub use sched::{CanvasFrame, Coalescer, JobsHandle, RenderScheduler, ViewState, Zoom};
pub use source::{DeviceProvider, SourceImage, SourceProvider, SourceWant, TileSink};
pub use stats::{EngineStats, RecomputeProbe};

/// `Pin<Box<dyn Future>>` used by the async seam traits (`DeviceProvider`,
/// `SourceProvider`) — spec §3.8. Local alias so the engine takes no `futures`
/// dependency.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
