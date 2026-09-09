// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `solid.color`, the Phase 2 tracer/test node (spec T6): fills a texture
//! with one color. Deliberately trivial; its job is to prove the `RenderNode`
//! trait, the registry, the ticket lifecycle, the texture pool and the
//! shared-device composite, NOT to be part of the product pipeline.
//! `display.transform` (Phase 6) is the one real M0 node.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lightbox_jobs::CancelToken;

use crate::engine::{RenderRequest, RenderScale};
use crate::error::{NodeError, RenderError};
use crate::node::{
    CpuCtx, GpuCtx, ImageBufU8, NodeId, ParamDelta, Params, RenderNode, Tile, TileCpu,
};
use crate::planner::{PlannedEval, RenderPlanner};
use crate::source::SourceResolver;

/// Params of [`SolidColorNode`]: an sRGB-encoded straight-alpha color and the
/// output size (CBOR via [`Params::from_serialize`]).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct SolidColorParams {
    /// sRGB-encoded RGBA, each channel in `0.0..=1.0`.
    pub rgba: [f32; 4],
    /// Output width in pixels.
    pub out_w: u32,
    /// Output height in pixels.
    pub out_h: u32,
}

/// The `solid.color` node. Counts its evaluations so tests can assert
/// "exactly one eval ran" / "no eval ran" (spec T6 acceptance criteria).
#[derive(Default)]
pub struct SolidColorNode {
    gpu_evals: AtomicU64,
    cpu_evals: AtomicU64,
}

impl SolidColorNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("solid.color");

    /// A fresh node with zeroed eval counters.
    pub fn new() -> SolidColorNode {
        SolidColorNode::default()
    }

    /// How many GPU evaluations ran (test probe, spec T6).
    pub fn gpu_evals(&self) -> u64 {
        self.gpu_evals.load(Ordering::Acquire)
    }

    /// How many CPU evaluations ran (test probe, spec T6).
    pub fn cpu_evals(&self) -> u64 {
        self.cpu_evals.load(Ordering::Acquire)
    }

    /// Total evaluations on either path.
    pub fn total_evals(&self) -> u64 {
        self.gpu_evals() + self.cpu_evals()
    }
}

impl RenderNode for SolidColorNode {
    fn id(&self) -> NodeId {
        SolidColorNode::ID
    }

    fn eval_gpu(&self, ctx: &GpuCtx<'_>, _inputs: &[Tile], p: &Params) -> Result<Tile, NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let params: SolidColorParams = p.decode()?;
        self.gpu_evals.fetch_add(1, Ordering::AcqRel);

        let texture = ctx.pool.acquire([params.out_w, params.out_h]);
        let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));

        // A clear render pass writes the color verbatim (the output format is
        // non-sRGB-aware, so these are the exact bytes egui later samples).
        let mut encoder = ctx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("solid.color"),
            });
        {
            let [r, g, b, a] = params.rgba;
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("solid.color clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(r),
                            g: f64::from(g),
                            b: f64::from(b),
                            a: f64::from(a),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        ctx.gpu.queue.submit([encoder.finish()]);

        Ok(Tile {
            extent: [texture.width(), texture.height()],
            texture,
            view,
            offset: [0, 0],
        })
    }

    fn eval_cpu(
        &self,
        ctx: &CpuCtx<'_>,
        _inputs: &[TileCpu],
        p: &Params,
    ) -> Result<TileCpu, NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let params: SolidColorParams = p.decode()?;
        self.cpu_evals.fetch_add(1, Ordering::AcqRel);

        let [w, h] = [params.out_w.max(1), params.out_h.max(1)];
        let texel: [u8; 4] = params
            .rgba
            .map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let mut px = Vec::with_capacity(4 * (w as usize) * (h as usize));
        for _ in 0..(w as usize) * (h as usize) {
            px.extend_from_slice(&texel);
        }
        Ok(TileCpu {
            buf: ImageBufU8 {
                px,
                width: w,
                height: h,
            },
            offset: [0, 0],
        })
    }

    fn invalidates(&self, _changed: &ParamDelta) -> bool {
        true // M0: conservative (spec §3.4)
    }
}

/// Planner pairing with [`SolidColorNode`]: plans every request as one
/// source-less `solid.color` eval sized by the request's scale. The color is
/// mutable so the Phase 2 shell spike can animate it (proving latest-wins
/// coalescing under live updates). M0 scaffolding, replaced by E05.1.
pub struct SolidColorPlanner {
    rgba: Mutex<[f32; 4]>,
}

impl SolidColorPlanner {
    /// A planner painting `rgba` (sRGB-encoded, straight alpha).
    pub fn new(rgba: [f32; 4]) -> SolidColorPlanner {
        SolidColorPlanner {
            rgba: Mutex::new(rgba),
        }
    }

    /// Changes the color for subsequently planned requests.
    pub fn set_color(&self, rgba: [f32; 4]) {
        *self.rgba.lock().expect("color lock poisoned") = rgba;
    }
}

impl RenderPlanner for SolidColorPlanner {
    fn plan(
        &self,
        req: &RenderRequest,
        _sources: &dyn SourceResolver,
        _cancel: &CancelToken,
    ) -> Result<PlannedEval, RenderError> {
        let [out_w, out_h] = match req.scale {
            RenderScale::FitWithin { w, h } => [w.max(1), h.max(1)],
            // A solid color has no native size; pick something visible.
            RenderScale::Native => [256, 256],
        };
        let params = Params::from_serialize(&SolidColorParams {
            rgba: *self.rgba.lock().expect("color lock poisoned"),
            out_w,
            out_h,
        })
        .map_err(|e| RenderError::Node(e.to_string()))?;
        Ok(PlannedEval {
            node: SolidColorNode::ID,
            params,
            source: None,
        })
    }
}
