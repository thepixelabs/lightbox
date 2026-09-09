// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `util.resize`, decimation for the scale ladders (spec §1 engine-owned nodes;
//! task **C2**).
//!
//! Owner: **A-gpu** (WGSL + CPU resampler). Phase A ships an **area-average box
//! filter** decimation (`resize.wgsl` / [`decimate_box_cpu`]); real Lanczos is
//! task C2. The output extent is whatever the backend sized the output tile to
//! (identity when unscaled, smaller for the fit/preview ladders); the kernel
//! area-averages the input region each output texel covers. GPU and CPU use the
//! exact same integer region math so the ΔE2000 parity gate (§4.4) holds.

use std::sync::Arc;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::ParamsSchema;
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, PortDecl,
    RenderNode,
};
use crate::ng::nodes::support;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The box-decimation kernel, naga-validated at build (`build.rs`).
const RESIZE_WGSL: &str = include_str!("../../../shaders/resize.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("util.resize"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: crate::ng::node::ParamsSchemaRef(&SCHEMA),
};

/// The `util.resize` decimation node.
#[derive(Default)]
pub struct UtilResizeNode {}

impl UtilResizeNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("util.resize");

    /// A fresh node.
    pub fn new() -> UtilResizeNode {
        UtilResizeNode::default()
    }
}

impl RenderNode for UtilResizeNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    /// **The scale ladder's landing point.** This is the one node in the PV1
    /// chain that genuinely resamples, so it, and only it, takes the render
    /// request's target extent rather than passing its input's through. Every
    /// stage upstream of it therefore runs at the source's own extent and
    /// every stage downstream at the request's, with the resample that bridges
    /// them happening here, in the kernel that actually area-averages
    /// (`resize.wgsl` / [`decimate_box_cpu`]).
    ///
    /// The request extent is taken verbatim, upscales included: a viewport
    /// larger than the source still yields a viewport-sized frame, which is
    /// what the shell's compositor expects to map onto the image rect.
    fn output_extent(&self, _inputs: &[Extent], _params: &ParamBlock, request: Extent) -> Extent {
        request
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("util.resize: missing input tile".into()))?;
        let in_extent = Extent {
            w: input.roi.w,
            h: input.roi.h,
        };
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("util.resize: output is not a GPU tile".into()))?
            .clone();
        let out_extent = ctx.output().extent().unwrap_or(in_extent);

        let params = [in_extent.w, in_extent.h, out_extent.w, out_extent.h];
        let mut ubo_bytes = [0u8; 16];
        for (i, word) in params.iter().enumerate() {
            ubo_bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("util.resize params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(RESIZE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("util.resize params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params],
            out_extent,
            "util.resize",
        );
        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("util.resize: missing input tile".into()))?;
        // The output extent is carried on the pre-sized output buffer.
        let out_extent = ctx.output().extent;
        *ctx.output() = decimate_box_cpu(input.pixels, out_extent);
        Ok(())
    }
}

/// Area-average box decimation of `src` down to `out_extent` (the CPU parity
/// twin of `resize.wgsl`; identical half-open integer region math).
pub fn decimate_box_cpu(src: &PixelBuf, out_extent: Extent) -> PixelBuf {
    let in_w = src.extent.w.max(1);
    let in_h = src.extent.h.max(1);
    let out_w = out_extent.w.max(1);
    let out_h = out_extent.h.max(1);
    let mut out = support::new_rgba16f(Extent { w: out_w, h: out_h });

    for oy in 0..out_h {
        let y0 = (oy * in_h) / out_h;
        let y1 = (((oy + 1) * in_h) / out_h).max(y0 + 1);
        for ox in 0..out_w {
            let x0 = (ox * in_w) / out_w;
            let x1 = (((ox + 1) * in_w) / out_w).max(x0 + 1);
            let mut acc = [0.0f32; 4];
            let mut count = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = support::read_rgba_f32(src, xx as usize, yy as usize);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                    count += 1.0;
                }
            }
            let inv = 1.0 / count.max(1.0);
            support::write_rgba16f(
                &mut out,
                ox as usize,
                oy as usize,
                [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv],
            );
        }
    }
    out
}

/// The Lanczos window radius (a = 3): the quality resampler's support.
const LANCZOS_A: f64 = 3.0;

/// The normalized-sinc Lanczos-3 kernel weight at offset `x`.
#[inline]
fn lanczos3(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else if x.abs() < LANCZOS_A {
        let px = std::f64::consts::PI * x;
        LANCZOS_A * px.sin() * (px / LANCZOS_A).sin() / (px * px)
    } else {
        0.0
    }
}

/// One-pass separable **Lanczos-3** decimation of `src` down to `out_extent`
/// (the higher-quality resampler for the scale ladders; spec C2 "Lanczos/box").
///
/// The filter support widens by the decimation factor so downscaling is properly
/// anti-aliased (a box/area-average is the degenerate 0th-order case). Weights
/// are normalized per output texel, so a flat field is preserved exactly and no
/// energy is gained or lost. Deterministic (`f64` accumulation, straight
/// quantization to the `rgba16float` output), a stable golden pin.
pub fn decimate_lanczos_cpu(src: &PixelBuf, out_extent: Extent) -> PixelBuf {
    let in_w = src.extent.w.max(1);
    let in_h = src.extent.h.max(1);
    let out_w = out_extent.w.max(1);
    let out_h = out_extent.h.max(1);
    let mut out = support::new_rgba16f(Extent { w: out_w, h: out_h });

    let ratio_x = in_w as f64 / out_w as f64;
    let ratio_y = in_h as f64 / out_h as f64;
    // Filter scale: widen for downscale (≥1), stay 1 for upscale/identity.
    let fs_x = ratio_x.max(1.0);
    let fs_y = ratio_y.max(1.0);

    // Precompute vertical taps per output row (separable).
    for oy in 0..out_h {
        let cy = (oy as f64 + 0.5) * ratio_y - 0.5;
        let (yw, y0) = taps(cy, fs_y, in_h);
        for ox in 0..out_w {
            let cx = (ox as f64 + 0.5) * ratio_x - 0.5;
            let (xw, x0) = taps(cx, fs_x, in_w);

            let mut acc = [0.0f64; 4];
            let mut wsum = 0.0f64;
            for (j, &wy) in yw.iter().enumerate() {
                let sy = y0 + j as u32;
                for (i, &wx) in xw.iter().enumerate() {
                    let sx = x0 + i as u32;
                    let w = wx * wy;
                    let p = support::read_rgba_f32(src, sx as usize, sy as usize);
                    for c in 0..4 {
                        acc[c] += w * p[c] as f64;
                    }
                    wsum += w;
                }
            }
            let inv = if wsum.abs() > 1e-12 { 1.0 / wsum } else { 0.0 };
            support::write_rgba16f(
                &mut out,
                ox as usize,
                oy as usize,
                [
                    (acc[0] * inv) as f32,
                    (acc[1] * inv) as f32,
                    (acc[2] * inv) as f32,
                    (acc[3] * inv) as f32,
                ],
            );
        }
    }
    out
}

/// The clamped tap weights + first tap index for a 1-D Lanczos sample centered
/// at `center` with filter scale `fs`, over `[0, len)` (edge-clamped).
fn taps(center: f64, fs: f64, len: u32) -> (Vec<f64>, u32) {
    let lo = (center - LANCZOS_A * fs).floor() as i64;
    let hi = (center + LANCZOS_A * fs).ceil() as i64;
    let first = lo.clamp(0, (len - 1) as i64) as u32;
    let last = hi.clamp(0, (len - 1) as i64) as u32;
    let mut w = Vec::with_capacity((last - first + 1) as usize);
    for s in first..=last {
        w.push(lanczos3((s as f64 - center) / fs));
    }
    (w, first)
}

/// Factory registering [`UtilResizeNode`].
#[derive(Default)]
pub struct UtilResizeFactory {}

impl NodeFactory for UtilResizeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(UtilResizeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(RESIZE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::tile::PixelFormat;

    fn fill(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> PixelBuf {
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
        for y in 0..h {
            for x in 0..w {
                px.set_rgba_f32(x, y, f(x, y));
            }
        }
        px
    }

    #[test]
    fn lanczos_preserves_a_flat_field() {
        // A constant image must decimate to the same constant (normalized taps).
        let src = fill(64, 64, |_, _| [0.4, 0.6, 0.25, 1.0]);
        let out = decimate_lanczos_cpu(&src, Extent { w: 16, h: 16 });
        assert_eq!(out.extent, Extent { w: 16, h: 16 });
        for y in 0..16 {
            for x in 0..16 {
                let p = out.get_rgba_f32(x, y);
                assert!((p[0] - 0.4).abs() < 2e-3, "r={} at {x},{y}", p[0]);
                assert!((p[1] - 0.6).abs() < 2e-3);
                assert!((p[2] - 0.25).abs() < 2e-3);
                assert!((p[3] - 1.0).abs() < 2e-3);
            }
        }
    }

    #[test]
    fn lanczos_keeps_a_ramp_monotone_and_within_range() {
        // A horizontal ramp decimates to a still-monotone, in-[0,1] ramp.
        let src = fill(64, 8, |x, _| {
            let v = x as f32 / 63.0;
            [v, v, v, 1.0]
        });
        let out = decimate_lanczos_cpu(&src, Extent { w: 16, h: 8 });
        let mut prev = -1.0f32;
        for x in 0..16 {
            let v = out.get_rgba_f32(x, 0)[0];
            assert!((-0.02..=1.02).contains(&v), "ramp out of range: {v}");
            assert!(v >= prev - 1e-3, "ramp not monotone at {x}: {v} < {prev}");
            prev = v;
        }
    }

    #[test]
    fn lanczos_identity_extent_is_near_identity() {
        let src = fill(8, 8, |x, y| [x as f32 / 8.0, y as f32 / 8.0, 0.5, 1.0]);
        let out = decimate_lanczos_cpu(&src, Extent { w: 8, h: 8 });
        for y in 0..8 {
            for x in 0..8 {
                let a = src.get_rgba_f32(x, y);
                let b = out.get_rgba_f32(x, y);
                for c in 0..4 {
                    assert!(
                        (a[c] - b[c]).abs() < 2e-3,
                        "ch {c} at {x},{y}: {} vs {}",
                        a[c],
                        b[c]
                    );
                }
            }
        }
    }
}
