// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! GPU backend — encoder management, compute dispatch, readback (spec §2;
//! tasks **A9** GPU executor v1, C tiling, E15 readback).
//!
//! Owner: **A-gpu**. Implements the frozen [`Backend`] seam.

use crate::ng::engine::BackendId;
use crate::ng::error::RenderError;
use crate::ng::exec::backend::{Backend, BackendEvalRequest};
use crate::ng::gpu::DeviceCtx;
use crate::ng::tile::TileHandle;

/// The GPU pixel backend: records compute dispatches for each node onto the
/// shared queue and reads back on the `Buffer` path. **A9 fills the internals.**
#[derive(Default)]
pub struct GpuBackend {}

impl GpuBackend {
    /// A GPU backend on `device`.
    pub fn new(device: &DeviceCtx) -> GpuBackend {
        let _ = device;
        unimplemented!("A9 (A-gpu): GpuBackend::new — encoder mgmt + pipeline/pool wiring")
    }
}

impl Backend for GpuBackend {
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError> {
        let _ = req;
        unimplemented!("A9 (A-gpu): GpuBackend::eval_node — record dispatches, write output tile")
    }

    fn kind(&self) -> BackendId {
        unimplemented!("A9 (A-gpu): GpuBackend::kind")
    }
}
