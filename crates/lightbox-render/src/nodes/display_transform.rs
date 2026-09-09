// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `display.transform`, the one real M0 render node (E01 spec §3.4,
//! T23/T24).
//!
//! Operation: sRGB EOTF decode → EXIF orientation → bilinear resample to the
//! output size **in linear light** → sRGB OETF encode → RGBA8. The GPU
//! (WGSL compute, 16×16 workgroups, `display_transform.wgsl`) and CPU
//! (rayon over output rows, [`DisplayTransformNode::eval_cpu`]) paths
//! implement the **same written algorithm spec**, committed next to the
//! shader as `display_transform.md`; identical filter coordinates are what
//! make the ΔE2000 parity gate meaningful (§4.4).
//!
//! Registered `(NodeId("display.transform"), PV_M0)`. The math is
//! deliberately *almost* trivial, the node proves the trait, the registry,
//! the ticket lifecycle, the golden harness and the shared-device composite.
//! E02.3 replaces/extends the color math (real display ICC); the node's
//! *shape* is the deliverable.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use lightbox_jobs::CancelToken;
use lightbox_types::Orientation;
use rayon::prelude::*;
use wgpu::util::DeviceExt;

use crate::engine::{RenderRequest, RenderScale};
use crate::error::{NodeError, RenderError};
use crate::gpu::GpuContext;
use crate::node::{
    CpuCtx, GpuCtx, ImageBufU8, NodeId, ParamDelta, Params, RenderNode, Tile, TileCpu,
};
use crate::planner::{PlannedEval, RenderPlanner};
use crate::source::SourceResolver;

/// Params of [`DisplayTransformNode`] (spec §3.4 verbatim):
/// `{ orientation: u8, out_w: u32, out_h: u32 }`, CBOR-encoded via
/// [`Params::from_serialize`]. `orientation` is the EXIF value `1..=8`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayTransformParams {
    /// EXIF orientation `1..=8` of the *input* pixels (output is upright).
    pub orientation: u8,
    /// Output width in pixels (≥ 1).
    pub out_w: u32,
    /// Output height in pixels (≥ 1).
    pub out_h: u32,
}

/// GPU pipeline cached per device (a node instance may serve engines on
/// different devices across its lifetime; resources are device-bound).
struct GpuPipeline {
    device: Weak<wgpu::Device>,
    pipeline: wgpu::ComputePipeline,
    bind_layout: wgpu::BindGroupLayout,
}

/// The `display.transform` node (spec §3.4). See the module docs and
/// `display_transform.md` for the algorithm contract.
#[derive(Default)]
pub struct DisplayTransformNode {
    gpu: Mutex<Option<GpuPipeline>>,
}

impl DisplayTransformNode {
    /// The node's registry identity (spec §3.4).
    pub const ID: NodeId = NodeId("display.transform");

    /// A fresh node (no GPU resources until first `eval_gpu`).
    pub fn new() -> DisplayTransformNode {
        DisplayTransformNode::default()
    }

    /// Decodes + validates params and the single input tile's shape.
    fn checked_params(
        p: &Params,
        input_extent: [u32; 2],
    ) -> Result<(DisplayTransformParams, Orientation), NodeError> {
        let params: DisplayTransformParams = p.decode()?;
        let orientation =
            Orientation::from_exif(u16::from(params.orientation)).ok_or_else(|| {
                NodeError::BadParams(format!(
                    "EXIF orientation {} not in 1..=8",
                    params.orientation
                ))
            })?;
        if params.out_w == 0 || params.out_h == 0 {
            return Err(NodeError::BadParams(format!(
                "zero output size {}x{}",
                params.out_w, params.out_h
            )));
        }
        if input_extent[0] == 0 || input_extent[1] == 0 {
            return Err(NodeError::BadParams("zero-sized input tile".into()));
        }
        Ok((params, orientation))
    }

    /// The per-device compute pipeline, built on first use.
    fn pipeline_for(
        &self,
        gpu: &GpuContext,
    ) -> Result<(wgpu::ComputePipeline, wgpu::BindGroupLayout), NodeError> {
        let mut cache = self
            .gpu
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.as_ref() {
            if let Some(dev) = cached.device.upgrade() {
                if Arc::ptr_eq(&dev, &gpu.device) {
                    return Ok((cached.pipeline.clone(), cached.bind_layout.clone()));
                }
            }
        }

        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("display.transform"),
                source: wgpu::ShaderSource::Wgsl(include_str!("display_transform.wgsl").into()),
            });
        let bind_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("display.transform"),
                entries: &[
                    // @binding(0), the unoriented sRGB-encoded input.
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // @binding(1), the output (pool texture, storage write).
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: crate::pool::OUTPUT_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    // @binding(2), Params uniform (8 × u32).
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(UNIFORM_BYTES as u64),
                        },
                        count: None,
                    },
                ],
            });
        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("display.transform"),
                bind_group_layouts: &[Some(&bind_layout)],
                ..Default::default()
            });
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("display.transform"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });

        *cache = Some(GpuPipeline {
            device: Arc::downgrade(&gpu.device),
            pipeline: pipeline.clone(),
            bind_layout: bind_layout.clone(),
        });
        Ok((pipeline, bind_layout))
    }
}

/// Uniform buffer size: 5 used u32 + 3 padding (see `Params` in the WGSL).
const UNIFORM_BYTES: usize = 32;

fn uniform_bytes(params: &DisplayTransformParams, in_w: u32, in_h: u32) -> [u8; UNIFORM_BYTES] {
    let words: [u32; 8] = [
        u32::from(params.orientation),
        params.out_w,
        params.out_h,
        in_w,
        in_h,
        0,
        0,
        0,
    ];
    let mut bytes = [0u8; UNIFORM_BYTES];
    for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    bytes
}

impl RenderNode for DisplayTransformNode {
    fn id(&self) -> NodeId {
        DisplayTransformNode::ID
    }

    fn eval_gpu(&self, ctx: &GpuCtx<'_>, inputs: &[Tile], p: &Params) -> Result<Tile, NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = single_input(inputs)?;
        let (params, _) = Self::checked_params(p, input.extent)?;
        let (pipeline, bind_layout) = self.pipeline_for(ctx.gpu)?;

        let out_tex = ctx.pool.acquire([params.out_w, params.out_h]);
        let out_view = Arc::new(out_tex.create_view(&wgpu::TextureViewDescriptor::default()));
        let uniforms = ctx
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("display.transform params"),
                contents: &uniform_bytes(&params, input.extent[0], input.extent[1]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind_group = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("display.transform"),
                layout: &bind_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&input.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniforms.as_entire_binding(),
                    },
                ],
            });

        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let mut encoder = ctx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("display.transform"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("display.transform"),
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(params.out_w.div_ceil(16), params.out_h.div_ceil(16), 1);
        }
        ctx.gpu.queue.submit([encoder.finish()]);

        Ok(Tile {
            extent: [params.out_w, params.out_h],
            texture: out_tex,
            view: out_view,
            offset: [0, 0],
        })
    }

    fn eval_cpu(
        &self,
        ctx: &CpuCtx<'_>,
        inputs: &[TileCpu],
        p: &Params,
    ) -> Result<TileCpu, NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::BadParams("display.transform takes one input tile".into()))?;
        if inputs.len() != 1 {
            return Err(NodeError::BadParams(format!(
                "display.transform takes one input tile, got {}",
                inputs.len()
            )));
        }
        let (in_w, in_h) = (input.buf.width, input.buf.height);
        let (params, orientation) = Self::checked_params(p, [in_w, in_h])?;
        let expect = 4 * (in_w as usize) * (in_h as usize);
        if input.buf.px.len() != expect {
            return Err(NodeError::BadParams(format!(
                "input buffer is {} bytes, {in_w}x{in_h} RGBA needs {expect}",
                input.buf.px.len()
            )));
        }

        let [ow, oh] = oriented_dims(orientation, in_w, in_h);
        let (out_w, out_h) = (params.out_w, params.out_h);
        let src = input.buf.px.as_slice();
        let row_bytes = 4 * out_w as usize;
        let mut out = vec![0u8; row_bytes * out_h as usize];

        // Each pixel is a pure function of the inputs, rayon over rows is
        // byte-stable regardless of thread count (display_transform.md,
        // determinism note; the CLI's --cpu byte-stability AC rides on it).
        let cancelled = AtomicBool::new(false);
        out.par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(y, row)| {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                if ctx.cancel.is_cancelled() {
                    cancelled.store(true, Ordering::Relaxed);
                    return;
                }
                render_row(
                    row,
                    y as u32,
                    src,
                    in_w,
                    orientation,
                    [ow, oh],
                    [out_w, out_h],
                );
            });
        if cancelled.load(Ordering::Relaxed) || ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }

        Ok(TileCpu {
            buf: ImageBufU8 {
                px: out,
                width: out_w,
                height: out_h,
            },
            offset: [0, 0],
        })
    }

    fn invalidates(&self, _changed: &ParamDelta) -> bool {
        true // M0: conservative (spec §3.4)
    }
}

fn single_input(inputs: &[Tile]) -> Result<&Tile, NodeError> {
    match inputs {
        [one] => Ok(one),
        other => Err(NodeError::BadParams(format!(
            "display.transform takes one input tile, got {}",
            other.len()
        ))),
    }
}

/// Oriented (upright) dimensions of an `w × h` input under `o`.
pub fn oriented_dims(o: Orientation, w: u32, h: u32) -> [u32; 2] {
    if o.transposes() {
        [h, w]
    } else {
        [w, h]
    }
}

/// Oriented integer coordinates → stored coordinates (the table in
/// `display_transform.md`; mirror of the WGSL `orient_map`).
fn orient_map(o: Orientation, x: u32, y: u32, sw: u32, sh: u32) -> (u32, u32) {
    match o {
        Orientation::O1 => (x, y),
        Orientation::O2 => (sw - 1 - x, y),
        Orientation::O3 => (sw - 1 - x, sh - 1 - y),
        Orientation::O4 => (x, sh - 1 - y),
        Orientation::O5 => (y, x),
        Orientation::O6 => (y, sh - 1 - x),
        Orientation::O7 => (sw - 1 - y, sh - 1 - x),
        Orientation::O8 => (sw - 1 - y, x),
    }
}

/// sRGB EOTF (decode) in f32, the same formula as the WGSL `srgb_decode`.
fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF (encode) in f32, the same formula as the WGSL `srgb_encode`.
fn srgb_encode(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// `a*(1-t) + b*t`, spelled exactly like the WGSL `lerp4`.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

/// One output row of the display_transform.md per-pixel algorithm.
#[allow(clippy::many_single_char_names)] // u/v/fu/fv are the .md's names
fn render_row(
    row: &mut [u8],
    y: u32,
    src: &[u8],
    in_w: u32,
    o: Orientation,
    oriented: [u32; 2],
    out: [u32; 2],
) {
    let [ow, oh] = oriented;
    let [out_w, out_h] = out;
    debug_assert_eq!(row.len(), 4 * out_w as usize);

    // Step 3 helper: one tap, clamp (oriented), orient, fetch, decode.
    let fetch = |ou: i64, ov: i64| -> [f32; 4] {
        let cx = ou.clamp(0, i64::from(ow) - 1) as u32;
        let cy = ov.clamp(0, i64::from(oh) - 1) as u32;
        let (sx, sy) = orient_map(o, cx, cy, in_w, oh_to_sh(o, oriented));
        let i = 4 * (sy as usize * in_w as usize + sx as usize);
        [
            srgb_decode(f32::from(src[i]) / 255.0),
            srgb_decode(f32::from(src[i + 1]) / 255.0),
            srgb_decode(f32::from(src[i + 2]) / 255.0),
            f32::from(src[i + 3]) / 255.0,
        ]
    };

    let v = (y as f32 + 0.5) * oh as f32 / out_h as f32 - 0.5;
    let v0 = v.floor();
    let fv = v - v0;
    let iv = v0 as i64;
    for (x, texel) in row.chunks_exact_mut(4).enumerate() {
        let u = (x as f32 + 0.5) * ow as f32 / out_w as f32 - 0.5;
        let u0 = u.floor();
        let fu = u - u0;
        let iu = u0 as i64;

        let c00 = fetch(iu, iv);
        let c10 = fetch(iu + 1, iv);
        let c01 = fetch(iu, iv + 1);
        let c11 = fetch(iu + 1, iv + 1);

        for ch in 0..4 {
            let lin = lerp(lerp(c00[ch], c10[ch], fu), lerp(c01[ch], c11[ch], fu), fv);
            let enc = if ch == 3 { lin } else { srgb_encode(lin) };
            // Round-half-up quantization (display_transform.md step 5).
            texel[ch] = (enc.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
        }
    }
}

/// Stored height (`sh`) from the oriented dims (inverse of
/// [`oriented_dims`]), keeps `render_row`'s tap closure argument list flat.
fn oh_to_sh(o: Orientation, oriented: [u32; 2]) -> u32 {
    if o.transposes() {
        oriented[0]
    } else {
        oriented[1]
    }
}

/// Output size for an oriented source under a [`RenderScale`], the planner
/// policy: `FitWithin` preserves aspect ratio and **never upscales** (the M0
/// loupe is honest about embedded-preview resolution, spec T26); `Native` is
/// the oriented source size. Always ≥ 1×1.
pub fn output_size(oriented: [u32; 2], scale: RenderScale) -> [u32; 2] {
    let [ow, oh] = [oriented[0].max(1), oriented[1].max(1)];
    match scale {
        RenderScale::Native => [ow, oh],
        RenderScale::FitWithin { w, h } => {
            let s = (f64::from(w) / f64::from(ow))
                .min(f64::from(h) / f64::from(oh))
                .min(1.0);
            [
                ((f64::from(ow) * s).round() as u32).max(1),
                ((f64::from(oh) * s).round() as u32).max(1),
            ]
        }
    }
}

/// The M0 planner (scaffolding, replaced by E05.1's recipe→DAG builder):
/// every request is one `display.transform` eval over the pixels the
/// [`SourceResolver`] produces for `(image, scale)`, resolved here, on the
/// render worker, honoring `cancel`.
#[derive(Default, Debug, Clone, Copy)]
pub struct DisplayTransformPlanner;

impl RenderPlanner for DisplayTransformPlanner {
    fn plan(
        &self,
        req: &RenderRequest,
        sources: &dyn SourceResolver,
        cancel: &CancelToken,
    ) -> Result<PlannedEval, RenderError> {
        let src = sources
            .resolve(req.image, req.scale, cancel)
            .map_err(|e| RenderError::Source(e.to_string()))?;
        let oriented = oriented_dims(src.orientation, src.width, src.height);
        let [out_w, out_h] = output_size(oriented, req.scale);
        let params = Params::from_serialize(&DisplayTransformParams {
            orientation: src.orientation.exif_value() as u8,
            out_w,
            out_h,
        })
        .map_err(|e| RenderError::Node(e.to_string()))?;
        Ok(PlannedEval {
            node: DisplayTransformNode::ID,
            params,
            source: Some(src),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2×3 source with one distinct red value per pixel (row*10+col), the
    /// same table as lightbox-preview's CPU bake test, proving both
    /// implementations share EXIF semantics.
    fn src_2x3() -> Vec<u8> {
        let mut px = Vec::new();
        for r in [0u8, 1, 10, 11, 20, 21] {
            px.extend_from_slice(&[r, 0, 0, 255]);
        }
        px
    }

    #[test]
    fn orient_map_matches_exif_semantics() {
        let px = src_2x3();
        let cases: &[(Orientation, [u32; 2], &[u8])] = &[
            (Orientation::O1, [2, 3], &[0, 1, 10, 11, 20, 21]),
            (Orientation::O2, [2, 3], &[1, 0, 11, 10, 21, 20]),
            (Orientation::O3, [2, 3], &[21, 20, 11, 10, 1, 0]),
            (Orientation::O4, [2, 3], &[20, 21, 10, 11, 0, 1]),
            (Orientation::O5, [3, 2], &[0, 10, 20, 1, 11, 21]),
            (Orientation::O6, [3, 2], &[20, 10, 0, 21, 11, 1]),
            (Orientation::O7, [3, 2], &[21, 11, 1, 20, 10, 0]),
            (Orientation::O8, [3, 2], &[1, 11, 21, 0, 10, 20]),
        ];
        for (o, dims, expect) in cases {
            let [ow, oh] = oriented_dims(*o, 2, 3);
            assert_eq!([ow, oh], *dims, "{o:?} oriented dims");
            let mut got = Vec::new();
            for y in 0..oh {
                for x in 0..ow {
                    let (sx, sy) = orient_map(*o, x, y, 2, 3);
                    got.push(px[4 * (sy as usize * 2 + sx as usize)]);
                }
            }
            assert_eq!(got, *expect, "{o:?} mapping");
        }
    }

    #[test]
    fn output_size_fits_without_upscaling() {
        // Downscale to fit, aspect preserved.
        assert_eq!(
            output_size([3408, 2272], RenderScale::FitWithin { w: 240, h: 160 }),
            [240, 160]
        );
        assert_eq!(
            output_size([6000, 4000], RenderScale::FitWithin { w: 512, h: 512 }),
            [512, 341]
        );
        // Never upscales (M0 honesty about source resolution).
        assert_eq!(
            output_size([64, 40], RenderScale::FitWithin { w: 4096, h: 4096 }),
            [64, 40]
        );
        // Native is the oriented size.
        assert_eq!(output_size([40, 64], RenderScale::Native), [40, 64]);
        // Degenerate boxes clamp to 1.
        assert_eq!(
            output_size([100, 1], RenderScale::FitWithin { w: 10, h: 10 }),
            [10, 1]
        );
        assert_eq!(
            output_size([100, 100], RenderScale::FitWithin { w: 0, h: 0 }),
            [1, 1]
        );
    }

    #[test]
    fn srgb_transfer_round_trips_within_one_lsb() {
        for v in 0..=255u32 {
            let c = v as f32 / 255.0;
            let round_tripped = srgb_encode(srgb_decode(c));
            let q = (round_tripped.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u32;
            assert!(
                q.abs_diff(v) <= 1,
                "byte {v} round-tripped to {q} (Δ > 1 LSB)"
            );
        }
        // Exact anchors.
        assert_eq!(srgb_decode(0.0), 0.0);
        assert!((srgb_decode(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(srgb_encode(0.0), 0.0);
        assert!((srgb_encode(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cpu_eval_identity_preserves_pixels_within_one_lsb() {
        // O1 at native size: fu = fv = 0 everywhere, so the only possible
        // drift is the decode→encode round trip (≤ 1 LSB per channel).
        let (w, h) = (5u32, 4u32);
        let mut px = Vec::new();
        for i in 0..(w * h) {
            px.extend_from_slice(&[(i * 7 % 256) as u8, (i * 13 % 256) as u8, 200, 255]);
        }
        let node = DisplayTransformNode::new();
        let cancel = CancelToken::new();
        let ctx = CpuCtx { cancel: &cancel };
        let input = TileCpu {
            buf: ImageBufU8 {
                px: px.clone(),
                width: w,
                height: h,
            },
            offset: [0, 0],
        };
        let params = Params::from_serialize(&DisplayTransformParams {
            orientation: 1,
            out_w: w,
            out_h: h,
        })
        .unwrap();
        let out = node.eval_cpu(&ctx, &[input], &params).unwrap();
        assert_eq!((out.buf.width, out.buf.height), (w, h));
        for (a, b) in out.buf.px.iter().zip(&px) {
            assert!(a.abs_diff(*b) <= 1, "identity drifted {a} vs {b}");
        }
    }

    #[test]
    fn cpu_eval_rejects_bad_params_and_inputs() {
        let node = DisplayTransformNode::new();
        let cancel = CancelToken::new();
        let ctx = CpuCtx { cancel: &cancel };
        let input = TileCpu {
            buf: ImageBufU8 {
                px: vec![0; 16],
                width: 2,
                height: 2,
            },
            offset: [0, 0],
        };

        let bad_orientation = Params::from_serialize(&DisplayTransformParams {
            orientation: 9,
            out_w: 2,
            out_h: 2,
        })
        .unwrap();
        assert!(matches!(
            node.eval_cpu(&ctx, std::slice::from_ref(&input), &bad_orientation),
            Err(NodeError::BadParams(_))
        ));

        let zero_out = Params::from_serialize(&DisplayTransformParams {
            orientation: 1,
            out_w: 0,
            out_h: 2,
        })
        .unwrap();
        assert!(matches!(
            node.eval_cpu(&ctx, std::slice::from_ref(&input), &zero_out),
            Err(NodeError::BadParams(_))
        ));

        let good = Params::from_serialize(&DisplayTransformParams {
            orientation: 1,
            out_w: 2,
            out_h: 2,
        })
        .unwrap();
        assert!(matches!(
            node.eval_cpu(&ctx, &[], &good),
            Err(NodeError::BadParams(_))
        ));

        let cancelled = CancelToken::new();
        cancelled.cancel();
        let ctx = CpuCtx { cancel: &cancelled };
        assert!(matches!(
            node.eval_cpu(&ctx, &[input], &good),
            Err(NodeError::Cancelled)
        ));
    }

    #[test]
    fn cpu_eval_orientation_six_rotates_and_transposes() {
        // 2×3 stored, O6 (stored is 90° CW of upright) → upright is 3×2.
        let node = DisplayTransformNode::new();
        let cancel = CancelToken::new();
        let ctx = CpuCtx { cancel: &cancel };
        let input = TileCpu {
            buf: ImageBufU8 {
                px: src_2x3(),
                width: 2,
                height: 3,
            },
            offset: [0, 0],
        };
        let params = Params::from_serialize(&DisplayTransformParams {
            orientation: 6,
            out_w: 3,
            out_h: 2,
        })
        .unwrap();
        let out = node.eval_cpu(&ctx, &[input], &params).unwrap();
        assert_eq!((out.buf.width, out.buf.height), (3, 2));
        let reds: Vec<u8> = out.buf.px.chunks_exact(4).map(|p| p[0]).collect();
        // Native-size O6 = pure remap; the sRGB round trip may drift ≤ 1 LSB.
        let expect = [20u8, 10, 0, 21, 11, 1];
        for (got, want) in reds.iter().zip(expect) {
            assert!(got.abs_diff(want) <= 1, "O6 remap: {reds:?} vs {expect:?}");
        }
    }
}
