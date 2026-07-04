// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Error taxonomy of the render engine seed (spec §3.4).

use lightbox_types::ProcessVersion;

use crate::node::NodeId;

/// Errors constructing an [`crate::Engine`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The dedicated render worker thread could not be spawned.
    #[error("failed to spawn render worker thread: {0}")]
    WorkerSpawn(String),
}

/// Terminal failure of a render ticket. `Clone` because ticket states are
/// snapshot-polled by the UI every frame (spec §3.4 `Engine::poll`).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// No node registered under `(node, pv)` (spec §3.4 `NodeRegistry`).
    #[error("no render node registered for {node:?} under process version {pv:?}")]
    NodeNotRegistered {
        /// The node that was requested.
        node: NodeId,
        /// The process version it was requested under.
        pv: ProcessVersion,
    },
    /// `RenderTarget::Texture` was requested but the engine runs CPU-only.
    #[error("render engine has no GPU device; RenderTarget::Texture is unavailable")]
    NoGpu,
    /// No [`crate::RenderPlanner`] configured (M0 scaffolding; E05.1 replaces
    /// the planner with recipe-driven DAG construction).
    #[error("no render planner configured (Engine::set_planner)")]
    NoPlanner,
    /// The pixels-in seam failed (spec §3.5 `SourceResolver`).
    #[error("source resolve failed: {0}")]
    Source(String),
    /// A node evaluation failed (spec §3.4 `RenderNode`).
    #[error("node evaluation failed: {0}")]
    Node(String),
    /// GPU→CPU readback for `RenderTarget::CpuBuffer` failed.
    #[error("GPU readback failed: {0}")]
    Readback(String),
    /// The engine is shutting down; the ticket will never run.
    #[error("render engine is shutting down")]
    ShuttingDown,
    /// The wgpu device was lost. M0 behavior is degenerate by design (spec
    /// §3.4): in-flight tickets fail and the `on_device_lost` callbacks fire;
    /// the rebuild/CPU-degrade harness is E05.5.
    #[error("GPU device lost: {0}")]
    DeviceLost(String),
}

/// Why the GPU device was lost (spec §3.4 `on_device_lost`). Mirrors
/// `wgpu::DeviceLostReason` plus the driver's message, without leaking wgpu
/// callback types across the seam.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DeviceLostReason {
    /// Lost for an unspecified reason (driver error, TDR, …).
    Unknown(String),
    /// `wgpu::Device::destroy` was called.
    Destroyed(String),
}

impl DeviceLostReason {
    /// The driver/backend message accompanying the loss.
    pub fn message(&self) -> &str {
        match self {
            DeviceLostReason::Unknown(m) | DeviceLostReason::Destroyed(m) => m,
        }
    }
}

/// Errors from a [`crate::RenderNode`] evaluation (spec §3.4 — the `Result`
/// return is one of the two deliberate hardening deviations from §2.2).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
    /// The CBOR params did not decode to what the node expects.
    #[error("bad node params: {0}")]
    BadParams(String),
    /// The node observed cancellation at a checkpoint and stopped.
    #[error("node evaluation cancelled")]
    Cancelled,
    /// A GPU-side failure (shader, pass, submit).
    #[error("gpu: {0}")]
    Gpu(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Errors from the pixels-in seam (spec §3.5 `SourceResolver`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The image has no resolvable source at this tier.
    #[error("no source pixels for this image")]
    NotFound,
    /// The resolve observed cancellation and stopped.
    #[error("source resolve cancelled")]
    Cancelled,
    /// I/O reading the original file.
    #[error("io: {0}")]
    Io(String),
    /// Decode failure in the source pipeline.
    #[error("decode: {0}")]
    Decode(String),
}
