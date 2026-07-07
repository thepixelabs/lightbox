// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! CPU backend — the rayon tile executor (spec §2/§4.4; tasks **A14**, E5).
//!
//! Owner: **A-core**. Implements the frozen [`Backend`] seam at
//! preview-resolution (the §4.4 degraded-but-usable contract). Same scheduler,
//! same ladder as the GPU path (task E5); parity within ΔE2000 ≤ 1.0 /
//! PSNR ≥ 45 dB (task E6).

use crate::ng::engine::BackendId;
use crate::ng::error::RenderError;
use crate::ng::exec::backend::{Backend, BackendEvalRequest};
use crate::ng::tile::TileHandle;

/// The CPU pixel backend: evaluates each node with rayon over tiles.
/// **A14 fills the internals.**
#[derive(Default)]
pub struct CpuBackend {}

impl CpuBackend {
    /// A CPU backend using `threads` rayon workers (`None` = rayon default).
    pub fn new(threads: Option<usize>) -> CpuBackend {
        let _ = threads;
        unimplemented!("A14 (A-core): CpuBackend::new — rayon tile executor")
    }
}

impl Backend for CpuBackend {
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError> {
        let _ = req;
        unimplemented!("A14 (A-core): CpuBackend::eval_node — rayon tile map, §4.4 parity")
    }

    fn kind(&self) -> BackendId {
        unimplemented!("A14 (A-core): CpuBackend::kind")
    }
}
