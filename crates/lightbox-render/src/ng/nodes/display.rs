// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `xform.display`, working→display transform (spec §1 engine-owned nodes),
//! generalized from E01's M0 `display.transform`.
//!
//! Owner: **A-gpu**. The **color math is supplied by `lightbox-color`**: the
//! working→display transform is *baked* by
//! [`lightbox_color::display::build_display_transform`] into a 1-D shaper + 65³
//! LUT, and this node only **applies** it (`out = trilinear(lut, shaper(rgb))`).
//! The engine holds **zero color science** (E02 guardrail). The GPU kernel
//! (`display.wgsl`) and the CPU path ([`lightbox_color::display::DisplayTransform::apply`])
//! evaluate the identical baked data, so CPU/GPU ΔE2000 parity (§4.4) is exact
//! by construction. This node produces the `DisplayRgba8` the canvas composites.
//!
//! Phase A targets the **built-in sRGB** display (the documented fallback when
//! no monitor profile is available); wiring a live monitor profile through the
//! params ABI is a later task, mechanism unchanged.

use std::sync::{Arc, OnceLock};

use lightbox_color::cms::IccProfile;
use lightbox_color::display::{build_display_transform, DisplayTransform, Intent};

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

/// The working→display application kernel, naga-validated at build (`build.rs`).
const DISPLAY_WGSL: &str = include_str!("../../../shaders/display.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("xform.display"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::DisplayRgba8,
    },
    params_schema: crate::ng::node::ParamsSchemaRef(&SCHEMA),
};

/// The baked sRGB working→display transform + its GPU-ready byte layouts, built
/// once via `lightbox-color` (LCMS2) and shared across evals/devices.
struct Baked {
    dt: DisplayTransform,
    shaper_bytes: Vec<u8>,
    lut_bytes: Vec<u8>,
    shaper_n: u32,
    lut_n: u32,
}

static BAKED: OnceLock<Baked> = OnceLock::new();

fn baked() -> &'static Baked {
    BAKED.get_or_init(|| {
        let dt = build_display_transform(&IccProfile::srgb(), Intent::RelColorimetric)
            .expect("bake built-in sRGB working→display transform");
        let mut shaper_bytes = Vec::with_capacity(dt.shaper.samples.len() * 4);
        for &s in &dt.shaper.samples {
            shaper_bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut lut_bytes = Vec::with_capacity(dt.lut.data.len() * 16);
        for node in &dt.lut.data {
            lut_bytes.extend_from_slice(&node[0].to_le_bytes());
            lut_bytes.extend_from_slice(&node[1].to_le_bytes());
            lut_bytes.extend_from_slice(&node[2].to_le_bytes());
            lut_bytes.extend_from_slice(&0.0f32.to_le_bytes());
        }
        let shaper_n = dt.shaper.samples.len() as u32;
        let lut_n = dt.lut.size;
        Baked {
            dt,
            shaper_bytes,
            lut_bytes,
            shaper_n,
            lut_n,
        }
    })
}

/// The `xform.display` working→display node.
#[derive(Default)]
pub struct XformDisplayNode {}

impl XformDisplayNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("xform.display");

    /// A fresh node.
    pub fn new() -> XformDisplayNode {
        XformDisplayNode::default()
    }
}

impl RenderNode for XformDisplayNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
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
            .ok_or_else(|| NodeError::Other("xform.display: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("xform.display: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });
        let b = baked();

        // Params UBO: shaper_n, lut_n + 2 words padding (std140).
        let mut params = [0u8; 16];
        params[0..4].copy_from_slice(&b.shaper_n.to_le_bytes());
        params[4..8].copy_from_slice(&b.lut_n.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("xform.display params"),
            size: params.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &params);

        let shaper = storage_buffer(
            ctx.device,
            ctx.queue,
            "xform.display shaper",
            &b.shaper_bytes,
        );
        let lut = storage_buffer(ctx.device, ctx.queue, "xform.display lut", &b.lut_bytes);

        let pipeline = ctx.kernels.compute_pipeline(DISPLAY_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xform.display in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xform.display out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xform.display params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let bg_aux = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xform.display aux"),
            layout: &pipeline.get_bind_group_layout(3),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shaper.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: lut.as_entire_binding(),
                },
            ],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params, &bg_aux],
            extent,
            "xform.display",
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
            .ok_or_else(|| NodeError::Other("xform.display: missing input tile".into()))?;
        *ctx.output() = apply_display_cpu(input.pixels);
        Ok(())
    }
}

/// The CPU working→display application: `lightbox-color`'s baked
/// [`DisplayTransform::apply`] per pixel (RGB), alpha passthrough → `Rgba8Unorm`.
/// The parity anchor for `eval_gpu`'s `display.wgsl`.
pub fn apply_display_cpu(src: &PixelBuf) -> PixelBuf {
    let dt = &baked().dt;
    let w = src.extent.w.max(1);
    let h = src.extent.h.max(1);
    let mut out = support::new_rgba8(Extent { w, h });
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = support::read_rgba_f32(src, x, y);
            let disp = dt.apply([p[0], p[1], p[2]]);
            support::write_rgba8(&mut out, x, y, [disp[0], disp[1], disp[2], p[3]]);
        }
    }
    out
}

fn storage_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    bytes: &[u8],
) -> wgpu::Buffer {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, bytes);
    buf
}

/// Factory registering [`XformDisplayNode`].
#[derive(Default)]
pub struct XformDisplayFactory {}

impl NodeFactory for XformDisplayFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(XformDisplayNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        // Salt spans the WGSL kernel and the *baked color data*, the shaper +
        // 65³ LUT float layouts `lightbox-color` produces for the built-in sRGB
        // transform. These are the exact bytes both backends sample, so the salt
        // moves iff the color algorithm/profile moves (the D4 remit).
        //
        // We deliberately do NOT fold in `dt.key`: that is an xxh3 of the
        // *serialized ICC profile*, whose header carries a creation timestamp, so
        // it is non-deterministic across processes, which would violate spec
        // §3.5 ("equal ingredients ⇒ equal key across processes and builds"),
        // shred the content cache across runs, and make the PV-manifest
        // immutability gate (task D1/D4) unpinnable. See E05-deviations.md
        // (Phase D, D-D-fix-1).
        let b = baked();
        let mut h = blake3::Hasher::new();
        h.update(DISPLAY_WGSL.as_bytes());
        h.update(&b.shaper_n.to_le_bytes());
        h.update(&b.lut_n.to_le_bytes());
        h.update(&b.shaper_bytes);
        h.update(&b.lut_bytes);
        KernelSalt(h.finalize())
    }
}
