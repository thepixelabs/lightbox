// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! GPU device context, kernel builder, pipeline cache, tile pool (spec §2;
//! tasks **A6**/**A7**).
//!
//! Owner: **A-gpu**. Kernel conventions (spec §3.2): one WGSL entry per pass,
//! 16×16×1 workgroups, `@group(0)` inputs / `@group(1)` write-only output /
//! `@group(2)` params UBO / `@group(3)` LUT-aux; shaders embedded via
//! `include_str!`, naga-validated at build (task A6). No `f16` arithmetic in
//! WGSL v1 (storage `rgba16float`, compute f32 — Risk R2).

use std::sync::Arc;

use crate::ng::cache::Bytes;
use crate::ng::error::NodeError;
use crate::ng::tile::TileHandle;
use crate::ng::types::{Extent, TilePrecision};

/// A wrapper over the shared wgpu device/queue plus engine-owned GPU state
/// (limits, pipeline cache). The engine renders on the **shell's** device so
/// the output texture composites zero-copy (§2.3 seam 2). **A6 adds limits +
/// pipeline cache.**
pub struct DeviceCtx {
    /// The shared device (from the [`crate::ng::DeviceProvider`] seam).
    pub device: Arc<wgpu::Device>,
    /// The submission queue paired with `device`.
    pub queue: Arc<wgpu::Queue>,
}

impl DeviceCtx {
    /// Wraps a shared device/queue pair.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> DeviceCtx {
        DeviceCtx { device, queue }
    }
}

/// Builds compute pipelines under the §3.2 bind-group conventions, caching by
/// `blake3(wgsl)` (spec task **A6**). **A6 fills the internals.**
#[derive(Default)]
pub struct KernelBuilder {}

impl KernelBuilder {
    /// A kernel builder for `device`.
    pub fn new(device: &DeviceCtx) -> KernelBuilder {
        let _ = device;
        unimplemented!("A6 (A-gpu): KernelBuilder::new — pipeline cache keyed by shader blake3")
    }

    /// A compute pipeline for `wgsl`'s `entry` point, cached by `blake3(wgsl)`.
    /// Invalid WGSL fails the build (naga), never at runtime (spec A6).
    pub fn compute_pipeline(
        &self,
        wgsl: &str,
        entry: &str,
    ) -> Result<wgpu::ComputePipeline, NodeError> {
        let _ = (wgsl, entry);
        unimplemented!("A6 (A-gpu): KernelBuilder::compute_pipeline")
    }
}

/// A pool of `rgba16float` / `rgba32float` working tiles with budget accounting
/// and LRU reclaim of the free-list (spec task **A7**). **A7 fills the
/// internals.**
#[derive(Default)]
pub struct TilePool {}

impl TilePool {
    /// A pool creating tiles on `device`, capped at `budget`.
    pub fn new(device: Arc<wgpu::Device>, budget: Bytes) -> TilePool {
        let _ = (device, budget);
        unimplemented!("A7 (A-gpu): TilePool::new — rgba16float/rgba32float pool + budget")
    }

    /// Acquires a tile of `extent` at `precision` (reused from the free-list
    /// when possible). Holding the returned handle keeps it out of reuse.
    pub fn acquire(&self, extent: Extent, precision: TilePrecision) -> TileHandle {
        let _ = (extent, precision);
        unimplemented!("A7 (A-gpu): TilePool::acquire")
    }

    /// The configured byte budget.
    pub fn budget(&self) -> Bytes {
        unimplemented!("A7 (A-gpu): TilePool::budget")
    }

    /// Bytes currently resident (peak-trackable for the §C7 VRAM gate).
    pub fn used(&self) -> Bytes {
        unimplemented!("A7 (A-gpu): TilePool::used")
    }
}
