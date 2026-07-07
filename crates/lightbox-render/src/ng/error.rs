// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 error taxonomy — `EngineInitError` / `NodeError` / `CompileError` /
//! `RenderError` (spec §5 task A1), plus the seam errors `RegistryError`,
//! `DeviceError`, `SourceError`.
//!
//! Owner: **A-core** (task **A1**). Extended in place by later tasks (new
//! variants are additive — every enum is `#[non_exhaustive]`).

use crate::ng::types::{NodeId, ProcessVersion};

/// Failure constructing an [`crate::ng::Engine`] (spec §3.6 `Engine::new`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineInitError {
    /// The [`crate::ng::DeviceProvider`] yielded no usable device.
    #[error("no usable GPU device: {0}")]
    NoDevice(String),
    /// The render worker thread could not be spawned.
    #[error("failed to spawn render worker: {0}")]
    WorkerSpawn(String),
    /// GPU resource / pipeline warm-up failed during construction.
    #[error("engine warm-up failed: {0}")]
    Warmup(String),
}

/// Failure of a single [`crate::ng::RenderNode`] evaluation (spec §3.2).
/// `Clone` so it can be wrapped into a poll-snapshot [`RenderError`].
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
    /// The [`crate::ng::ParamBlock`] did not match the node's schema.
    #[error("bad node params: {0}")]
    BadParams(String),
    /// The node observed cancellation at a checkpoint and stopped.
    #[error("node evaluation cancelled")]
    Cancelled,
    /// A GPU-side failure (shader, pass, dispatch, readback).
    #[error("gpu: {0}")]
    Gpu(String),
    /// A CPU-side failure (the rayon parity path).
    #[error("cpu: {0}")]
    Cpu(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Failure compiling a `Recipe` into a [`crate::ng::RenderGraph`] (spec §3.4).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum CompileError {
    /// The requested process version has no registered template. **Never
    /// silently falls back to latest** (§4.5).
    #[error("unsupported process version {0:?}")]
    UnsupportedPv(ProcessVersion),
    /// A recipe field references a stage with no node in this PV's template.
    #[error("unknown stage {0:?} for this process version")]
    UnknownStage(String),
    /// Two edges disagree on [`crate::ng::PortType`] at a connection.
    #[error("port-type mismatch at {at}: {detail}")]
    TypeMismatch {
        /// Where in the graph the mismatch occurred.
        at: String,
        /// Human-readable detail.
        detail: String,
    },
    /// A required node id is not registered under `(id, pv)`.
    #[error("no node registered for {id:?} under {pv:?}")]
    NodeNotRegistered {
        /// The missing node.
        id: NodeId,
        /// The process version.
        pv: ProcessVersion,
    },
    /// The constructed graph is cyclic (topology is a DAG — spec §3.1/A8).
    #[error("render graph is cyclic")]
    Cyclic,
}

/// Terminal failure of a render ticket (spec §3.6 `RenderState::Failed`).
/// `Clone` because ticket states are snapshot-polled every frame.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// The recipe failed to compile.
    #[error("compile: {0}")]
    Compile(#[from] CompileError),
    /// A node evaluation failed.
    #[error("node {node:?}: {source}")]
    Node {
        /// The node that failed.
        node: NodeId,
        /// The underlying node error.
        source: NodeError,
    },
    /// The wgpu device was lost (recovery is E05.5).
    #[error("device lost: {0}")]
    DeviceLost(String),
    /// A submission was refused because the device is currently lost
    /// (export-safety, task E8).
    #[error("device unavailable: {0}")]
    DeviceUnavailable(String),
    /// The pixels-in seam failed.
    #[error("source: {0}")]
    Source(#[from] SourceError),
    /// GPU→CPU readback for a `Buffer` target failed.
    #[error("readback: {0}")]
    Readback(String),
    /// The engine is shutting down; the ticket will never run.
    #[error("render engine is shutting down")]
    ShuttingDown,
    /// Cancelled via [`crate::ng::Engine::cancel`] or latest-wins supersession.
    #[error("render cancelled")]
    Cancelled,
}

/// Failure registering a node factory (spec §3.3 `NodeRegistry::register`).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Registering an overlapping `(id, pv)` range — old PVs are never
    /// replaced (§4.5).
    #[error("overlapping (id, pv) registration for {id:?}: {detail}")]
    OverlappingPv {
        /// The node whose ranges overlap.
        id: NodeId,
        /// Human-readable detail of the overlap.
        detail: String,
    },
}

/// Failure from the [`crate::ng::DeviceProvider`] rebuild seam (spec §3.8).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceError {
    /// The shell could not recreate a device (adapter gone, etc.).
    #[error("device rebuild failed: {0}")]
    Rebuild(String),
}

/// Failure from the [`crate::ng::SourceProvider`] fetch seam (spec §3.8).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// No resolvable source pixels for this image at the requested want.
    #[error("no source pixels for image")]
    NotFound,
    /// The fetch observed cancellation and stopped.
    #[error("source fetch cancelled")]
    Cancelled,
    /// Decode / I/O failure upstream (E02/E03).
    #[error("source upstream: {0}")]
    Upstream(String),
}
