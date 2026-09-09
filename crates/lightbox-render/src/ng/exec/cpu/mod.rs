// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! CPU backend, the rayon tile executor (spec §2/§4.4; tasks **A14**, E5).
//!
//! Owner: **A-core**. Implements the frozen [`Backend`] seam at
//! preview-resolution (the §4.4 degraded-but-usable contract). Same scheduler,
//! same ladder as the GPU path (task E5); parity within ΔE2000 ≤ 1.0 /
//! PSNR ≥ 45 dB (task E6, the GPU side lands with A-gpu, checked at merge).
//!
//! For each node the backend allocates the output working tile (sized from the
//! eval ROI, formatted per [`RenderNode::precision`]), builds read-only input
//! views over the upstream CPU tiles, and drives [`RenderNode::eval_cpu`] on a
//! rayon pool sized by [`CpuBackend::new`]. The per-node pixel loop parallelizes
//! over output rows (`PixelBuf::par_fill_rows`).

use crate::ng::engine::BackendId;
use crate::ng::error::{NodeError, RenderError};
use crate::ng::exec::backend::{Backend, BackendEvalRequest};
use crate::ng::node::CpuEvalCtx;
use crate::ng::tile::{CpuTileView, PixelBuf, PixelFormat, TileHandle};
use crate::ng::types::{Roi, TilePrecision};

/// The CPU pixel backend: evaluates each node with rayon over tile rows.
#[derive(Default)]
pub struct CpuBackend {
    /// Dedicated rayon pool (honors `cpu_threads`); `None` = rayon's global
    /// pool.
    pool: Option<rayon::ThreadPool>,
}

impl CpuBackend {
    /// A CPU backend using `threads` rayon workers (`None` = rayon default).
    pub fn new(threads: Option<usize>) -> CpuBackend {
        let pool = threads.and_then(|n| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .thread_name(|i| format!("lbx-cpu-{i}"))
                .build()
                .map_err(|e| {
                    tracing::warn!("CPU backend rayon pool build failed ({e}); using global pool");
                })
                .ok()
        });
        CpuBackend { pool }
    }

    /// The working-tile pixel format for a given output precision.
    fn working_format(precision: TilePrecision) -> PixelFormat {
        match precision {
            TilePrecision::F16 => PixelFormat::Rgba16F,
            TilePrecision::F32 => PixelFormat::Rgba32F,
        }
    }

    /// Run a node eval on the configured pool (or the global pool).
    fn run<R, F>(&self, op: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        match &self.pool {
            Some(p) => p.install(op),
            None => op(),
        }
    }
}

impl Backend for CpuBackend {
    fn eval_node(&self, req: BackendEvalRequest<'_>) -> Result<TileHandle, RenderError> {
        let node_id = req.node.descriptor().id;
        if req.cancel.is_cancelled() {
            return Err(RenderError::Cancelled);
        }

        // Build read-only input views over upstream CPU tiles.
        let mut views: Vec<CpuTileView<'_>> = Vec::with_capacity(req.inputs.len());
        for (i, handle) in req.inputs.iter().enumerate() {
            let px = handle.cpu().ok_or_else(|| RenderError::Node {
                node: node_id,
                source: NodeError::Cpu(format!(
                    "input {i} to {node_id} has no CPU tile (GPU-only handle on the CPU path)"
                )),
            })?;
            let precision = match px.format {
                PixelFormat::Rgba32F => TilePrecision::F32,
                _ => TilePrecision::F16,
            };
            views.push(CpuTileView {
                pixels: px,
                roi: Roi {
                    x: 0,
                    y: 0,
                    w: px.extent.w,
                    h: px.extent.h,
                },
                precision,
            });
        }

        // Allocate the output working tile at the executor's propagated
        // target extent (task E11, see `ng::exec::Executor::evaluate`'s doc
        // comment and `docs/plan/epics/E11-deviations.md` E-scope-6/9): equal
        // to the request ROI for every node that doesn't itself change
        // resolution (the pre-E11 behavior, still relied on by `util.resize`'s
        // Fit-scale decimation), and to a `geom.crop`-style node's own
        // computed size from that node onward.
        let out_format = Self::working_format(req.precision);
        let out_extent = req.target_extent;
        let mut out = PixelBuf::new_zeroed(out_format, out_extent);
        let node_out_roi = Roi {
            x: req.roi.x,
            y: req.roi.y,
            w: out_extent.w,
            h: out_extent.h,
        };

        // Drive the node's CPU kernel on the pool.
        let result = self.run(|| {
            let mut ctx = CpuEvalCtx::new(req.scale, req.cancel, node_out_roi, &mut out);
            req.node.eval_cpu(&mut ctx, &views, req.params)
        });
        result.map_err(|e| match e {
            NodeError::Cancelled => RenderError::Cancelled,
            source => RenderError::Node {
                node: node_id,
                source,
            },
        })?;

        Ok(TileHandle::from_cpu(out))
    }

    fn kind(&self) -> BackendId {
        BackendId::Cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::cache::CacheKey;
    use crate::ng::node::param::ParamsSchema;
    use crate::ng::node::{GpuEvalCtx, NodeDescriptor, ParamsSchemaRef, RenderNode};
    use crate::ng::tile::TileView;
    use crate::ng::types::Extent;
    use crate::ng::{NodeId, ParamBlock, ParamValue, PortDecl, PortType};
    use lightbox_jobs::CancelToken;

    static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
    static GAIN_DESC: NodeDescriptor = NodeDescriptor {
        id: NodeId("test.gain"),
        inputs: &[PortDecl {
            name: "in",
            ty: PortType::LinearRgbaF16,
        }],
        output: PortDecl {
            name: "out",
            ty: PortType::LinearRgbaF16,
        },
        params_schema: ParamsSchemaRef(&SCHEMA),
    };

    /// Local mirror of the testkit `GainProbe` CPU path, so the backend has a
    /// node to drive without a cross-crate dependency.
    struct Gain;
    impl RenderNode for Gain {
        fn descriptor(&self) -> &NodeDescriptor {
            &GAIN_DESC
        }
        fn eval_gpu(
            &self,
            _: &mut GpuEvalCtx<'_>,
            _: &[TileView<'_>],
            _: &ParamBlock,
        ) -> Result<(), NodeError> {
            Ok(())
        }
        fn eval_cpu(
            &self,
            ctx: &mut CpuEvalCtx<'_>,
            inputs: &[CpuTileView<'_>],
            params: &ParamBlock,
        ) -> Result<(), NodeError> {
            let gain = params.get_f64_or("gain", 2.0) as f32;
            let input = inputs[0].pixels;
            let out = ctx.output();
            let (w, fmt, bpp) = (
                out.extent.w,
                out.format,
                out.format.bytes_per_pixel() as usize,
            );
            out.par_fill_rows(|y, row| {
                for x in 0..w {
                    let p = input.get_rgba_f32(x, y);
                    let g = [p[0] * gain, p[1] * gain, p[2] * gain, p[3]];
                    PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], g);
                }
            });
            Ok(())
        }
    }

    // A placeholder key: the CPU backend does not read `req.key` (caching is
    // task B), so any well-formed key works. Constructed directly to avoid the
    // B2-owned `CacheKey::derive`.
    fn dummy_key() -> CacheKey {
        CacheKey(blake3::hash(b"test-key"))
    }

    #[test]
    fn gain_doubles_input() {
        // A 2x2 F16 input with a known ramp.
        let mut input = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w: 2, h: 2 });
        input.set_rgba_f32(0, 0, [0.1, 0.2, 0.3, 1.0]);
        input.set_rgba_f32(1, 0, [0.25, 0.0, 0.5, 1.0]);
        input.set_rgba_f32(0, 1, [0.4, 0.4, 0.4, 0.5]);
        input.set_rgba_f32(1, 1, [0.05, 0.15, 0.45, 1.0]);
        let input_handle = TileHandle::from_cpu(input.clone());

        let backend = CpuBackend::new(Some(2));
        let params = ParamBlock::from_fields([("gain", ParamValue::Float(2.0))]).unwrap();
        let cancel = CancelToken::new();
        let node = Gain;
        let out = backend
            .eval_node(BackendEvalRequest {
                key: dummy_key(),
                node: &node,
                params: &params,
                inputs: &[input_handle],
                roi: Roi {
                    x: 0,
                    y: 0,
                    w: 2,
                    h: 2,
                },
                target_extent: Extent { w: 2, h: 2 },
                scale: 1.0,
                precision: TilePrecision::F16,
                cancel: &cancel,
            })
            .unwrap();

        let px = out.cpu().unwrap();
        // f16 round-trips these exactly enough for a tight check.
        let got = px.get_rgba_f32(0, 0);
        assert!((got[0] - 0.2).abs() < 1e-3, "r doubled: {got:?}");
        assert!((got[1] - 0.4).abs() < 1e-3);
        assert!((got[2] - 0.6).abs() < 1e-3);
        assert!((got[3] - 1.0).abs() < 1e-3, "alpha unchanged");
        let got11 = px.get_rgba_f32(1, 1);
        assert!((got11[2] - 0.9).abs() < 1e-3);
        assert_eq!(backend.kind(), BackendId::Cpu);
    }

    #[test]
    fn cancelled_before_eval_returns_cancelled() {
        let backend = CpuBackend::new(None);
        let cancel = CancelToken::new();
        cancel.cancel();
        let node = Gain;
        let params = ParamBlock::default();
        let input = TileHandle::from_cpu(PixelBuf::new_zeroed(
            PixelFormat::Rgba16F,
            Extent { w: 1, h: 1 },
        ));
        let err = backend
            .eval_node(BackendEvalRequest {
                key: dummy_key(),
                node: &node,
                params: &params,
                inputs: &[input],
                roi: Roi {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
                target_extent: Extent { w: 1, h: 1 },
                scale: 1.0,
                precision: TilePrecision::F16,
                cancel: &cancel,
            })
            .unwrap_err();
        assert!(matches!(err, RenderError::Cancelled));
    }
}
