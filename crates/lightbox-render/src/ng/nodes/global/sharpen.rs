// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.sharpen`, the Detail panel's sharpening half: unsharp masking on
//! companion-encoded luma with Lightroom's four controls, `amount`, `radius`,
//! `detail` and `masking` ([`lightbox_edit::Sharpen`], whose ranges and
//! neutral this node is built to and does not change).
//!
//! # What this implementation actually is
//!
//! **It is a Gaussian unsharp mask with two shaping terms. It is not Adobe's
//! algorithm and it does not claim parity with it.** Concretely:
//!
//! 1. **Encoded luma.** The same scalar
//!    `srgb_oetf(clamp(working_luma(rgb), 0, 1))`
//!    ([`crate::ng::nodes::global::clarity::encoded_luma`]) every other
//!    spatial node in this directory works on. Sharpening only luma is what
//!    keeps an unsharp mask from turning edges into colour fringes.
//! 2. **`radius`, the mask.** A separable Gaussian blur of the encoded-luma
//!    plane with `sigma = radius` in pixels, so the recipe's `0.5..=3.0`
//!    radius is a real Gaussian sigma, the same reading Photoshop's own
//!    Unsharp Mask radius has. `high = luma_e - blur_e`.
//! 3. **`detail`, halo suppression.** `high` is passed through a `tanh` soft
//!    clip at [`HALO_KNEE`] and then blended back toward the raw `high` as
//!    detail rises: `high_eff = mix(clip(high), high, detail/100)`. `tanh` is
//!    linear to first order for small amplitudes, so fine texture passes
//!    through untouched at any detail setting, while the large-amplitude
//!    overshoot a hard edge produces (which is what a halo IS) is compressed
//!    at low detail and allowed through at high detail.
//!    **Adobe's `detail` is described as doing something else**: their own
//!    documentation and the standard references on it describe a cross-fade
//!    toward a deconvolution-flavoured kernel rather than a soft clip on an
//!    unsharp mask. Their actual implementation is closed, so the honest
//!    statement is: the direction of the control matches (low = fewer
//!    halos, high = more raw high-frequency), the math is this file's own
//!    and is not theirs.
//! 4. **`masking`, the edge gate.** [`edge_gradient`] is the mean of nine
//!    Sobel magnitudes over the 3x3 neighborhood (a 5x5 luma window), which
//!    is the mask input; the gate is
//!    `mask = mix(1, smoothstep(0, masking/100 * MASK_FULL_GRADIENT, grad),
//!    masking/100)`. At `masking = 0` the mask is exactly `1` everywhere, at
//!    `masking = 100` only gradients at or above [`MASK_FULL_GRADIENT`]
//!    sharpen at full strength and flat sky is left alone.
//!    **Lightroom's mask is described as a blurred edge map**; averaging
//!    nine Sobels is a cheaper stand-in that still keeps a single noisy
//!    pixel from reading as an edge, but this mask's own edges are harder
//!    than a properly blurred one's would be.
//! 5. **`amount`.** A plain scale: `delta_encoded = amount/100 * mask *
//!    high_eff`, so the schema's `0..=150` reaches 1.5x the high-pass.
//!
//! Reapply is [`crate::ng::nodes::global::clarity`]'s local-derivative
//! additive delta, unchanged and for the same reason (`amount == 0` is an
//! exact pixel identity even for out-of-gamut scene-linear pixels). The only
//! guard on the boost is an encoded-domain `[0, 1]` clamp; unlike clarity and
//! texture this node deliberately does **not** apply the "no new local
//! extremum" clamp, because overshoot at an edge is exactly what sharpening
//! is for. Halo control here is `detail` and `masking`.
//!
//! # The preview is not the export
//!
//! This node runs at the **request** extent, not the source's. `util.resize`
//! (`nodes/resize.rs`, `output_extent`) takes the request extent verbatim
//! and the whole tone and colour chain, this node included, is spliced after
//! it. The shell only ever requests a fit-to-viewport render
//! (`lightbox-shell/src/canvas/view.rs`, `RenderScale::Fit`), so `radius` is
//! a sigma in **viewport** pixels in the preview and in source pixels at
//! export: on a 24 megapixel frame in a 1500 pixel viewport, radius 1.0 is
//! one viewport pixel on screen and about four source pixels in the file.
//! Lightroom's Detail panel has the same property and shows a "zoom to 100%
//! for an accurate preview" note; the panel here carries the same warning.
//!
//! # Not to be confused with export output sharpening
//!
//! `lightbox-export`'s `pixel::sharpen_in_place` is a separate, later,
//! output-referred stage (a fixed 3x3 box unsharp mask applied after the
//! output transform, see its own doc comment). This node is the develop-time,
//! scene-referred, user-driven one.
//!
//! GPU (`shaders/global_sharpen.wgsl`) and CPU ([`apply_sharpen`] over
//! [`gaussian_blur_encoded`] + [`edge_gradient`]) implement the identical
//! algorithm from the identical formulas, the established per-node
//! convention, so CPU/GPU parity holds to numerical rounding
//! (`tests/e10_detail.rs`).

use std::sync::Arc;

use lightbox_edit::GlobalStages;
use rayon::prelude::*;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::global::clarity::{encoded_luma, srgb_eotf_deriv};
use crate::ng::nodes::global::tone_recovery::working_luma_weights;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, RenderScale, Roi};

/// The two-pass separable-Gaussian unsharp kernel, naga-validated at build
/// (`build.rs`).
const SHARPEN_WGSL: &str = include_str!("../../../../shaders/global_sharpen.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "amount",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "radius",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "detail",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "masking",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.sharpen"),
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

// ── algorithm constants (duplicated literally in the WGSL, see that file's
//    header for why; the CPU/GPU parity test catches drift) ────────────────

/// Worst-case Gaussian half-window in pixels, `ceil(3 * sigma)` at the
/// schema's maximum radius (`3.0`, `lightbox-edit`'s `Sharpen::clamp`). Both
/// backends always walk the full window and normalize by the weight sum, so a
/// smaller radius gives the same answer a truncated window would.
pub const MAX_BLUR_RADIUS: u32 = 9;

/// Half-window of the Sobel mask input (a 3x3 mean of 3x3 Sobels).
pub const MASK_RADIUS: u32 = 2;

/// The apron this node reads outside its output ROI: the blur window plus
/// the mask window. Constant rather than radius-derived because
/// [`GlobalNode::roi_in`] is handed no [`ParamBlock`], so ROI planning must
/// not depend on the current radius.
pub const APRON: u32 = MAX_BLUR_RADIUS + MASK_RADIUS;

/// Encoded-luma amplitude at which the `detail = 0` soft clip is at roughly
/// 76% of its input (`tanh(1) = 0.7616`). Fine texture sits well below this,
/// a hard edge's high-pass overshoot well above it.
pub const HALO_KNEE: f32 = 0.06;

/// [`edge_gradient`] value that counts as a fully "real" edge at
/// `masking = 100`.
///
/// Calibration, in terms an image can be held against: `0.20` is exactly what
/// [`edge_gradient`] returns for a hard vertical step `0.6` tall in
/// companion-encoded luma (a 3x3 Sobel spreads the step over two pixels, so
/// it reads `0.3` at each of the two flanking columns and `0` at the third,
/// and the nine-Sobel mean is `0.2`). Pinned by
/// [`tests::edge_gradient_is_zero_on_a_flat_plane_and_large_at_a_step`], so
/// this sentence cannot quietly stop being true.
pub const MASK_FULL_GRADIENT: f32 = 0.20;

/// The three controls [`apply_sharpen`] consumes per pixel. `radius` is not
/// here: it is spent producing the blurred plane, before this point.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct SharpenAmounts {
    /// Strength, `0..=150`, neutral `0`.
    pub amount: f32,
    /// Halo suppression, `0..=100`.
    pub detail: f32,
    /// Edge masking, `0..=100`.
    pub masking: f32,
}

/// Smoothstep (Hermite), guarded against a zero-width edge pair, the same
/// helper shape every other node in this directory carries.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The unnormalized 1D Gaussian taps over `-MAX_BLUR_RADIUS ..=
/// MAX_BLUR_RADIUS`, plus their sum. Returned unnormalized (with the sum
/// alongside) so the CPU path divides exactly where
/// `global_sharpen.wgsl`'s two passes divide.
pub fn gaussian_kernel(sigma: f32) -> (Vec<f32>, f32) {
    let sigma = sigma.max(1.0e-3);
    let inv = 1.0 / (2.0 * sigma * sigma);
    let r = MAX_BLUR_RADIUS as i32;
    let mut taps = Vec::with_capacity((2 * r + 1) as usize);
    let mut sum = 0.0f32;
    for d in -r..=r {
        let wt = (-((d * d) as f32) * inv).exp();
        taps.push(wt);
        sum += wt;
    }
    (taps, sum)
}

/// A separable Gaussian blur over a `w`x`h` row-major single-channel plane,
/// border-replicated (clamp to edge), the same border rule
/// [`crate::ng::nodes::common::guided::box_blur`] and the engine's tiled
/// apron already establish.
pub fn gaussian_blur_encoded(plane: &[f32], w: u32, h: u32, sigma: f32) -> Vec<f32> {
    debug_assert_eq!(plane.len(), (w as usize) * (h as usize));
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (taps, wsum) = gaussian_kernel(sigma);
    let r = MAX_BLUR_RADIUS as i64;

    let (wi, hi) = (w as i64, h as i64);
    let mut tmp = vec![0.0f32; plane.len()];
    tmp.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row = &plane[y * w as usize..(y + 1) * w as usize];
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for d in -r..=r {
                    let sx = (x as i64 + d).clamp(0, wi - 1) as usize;
                    sum += taps[(d + r) as usize] * src_row[sx];
                }
                *out_px = sum / wsum;
            }
        });

    let mut out = vec![0.0f32; plane.len()];
    let w_us = w as usize;
    out.par_chunks_mut(w_us).enumerate().for_each(|(y, row)| {
        for (x, out_px) in row.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for d in -r..=r {
                let sy = (y as i64 + d).clamp(0, hi - 1) as usize;
                sum += taps[(d + r) as usize] * tmp[sy * w_us + x];
            }
            *out_px = sum / wsum;
        }
    });
    out
}

/// The `masking` control's edge map at one pixel: the mean of the nine 3x3
/// Sobel magnitudes centered on the 3x3 neighborhood of `(x, y)`, which needs
/// a 5x5 clamped luma window. Divided by `8` so the result reads as a slope
/// in encoded-luma units per pixel.
///
/// Averaging nine Sobels rather than taking the one at the center is what
/// keeps a single noisy pixel from reading as an edge and punching a hole in
/// the mask. The GPU kernel (`global_sharpen.wgsl`'s `pass_v`) walks the
/// identical window in the identical order, including the border clamping, so
/// the two agree at the tile border as well as inside it.
pub fn edge_gradient(luma: &[f32], w: u32, h: u32, x: u32, y: u32) -> f32 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    let (wi, hi) = (w as i64, h as i64);
    let mut lw = [0.0f32; 25];
    for j in -2i64..=2 {
        let sy = (y as i64 + j).clamp(0, hi - 1);
        for i in -2i64..=2 {
            let sx = (x as i64 + i).clamp(0, wi - 1);
            lw[((j + 2) * 5 + (i + 2)) as usize] = luma[(sy * wi + sx) as usize];
        }
    }
    let mut gsum = 0.0f32;
    for j in -1i64..=1 {
        for i in -1i64..=1 {
            let b = ((j + 2) * 5 + (i + 2)) as usize;
            let gx = (lw[b - 6] + 2.0 * lw[b - 1] + lw[b + 4])
                - (lw[b - 4] + 2.0 * lw[b + 1] + lw[b + 6]);
            let gy = (lw[b - 6] + 2.0 * lw[b - 5] + lw[b - 4])
                - (lw[b + 4] + 2.0 * lw[b + 5] + lw[b + 6]);
            gsum += (gx * gx + gy * gy).sqrt() / 8.0;
        }
    }
    gsum / 9.0
}

/// The full per-pixel op: given a working RGBA pixel, the encoded luma there,
/// the Gaussian-blurred encoded luma there, and the [`edge_gradient`] there,
/// returns the sharpened RGBA (alpha untouched). The CPU parity anchor for
/// `global_sharpen.wgsl`'s `pass_v`.
///
/// **Why an additive local-derivative delta.** Identical to
/// [`crate::ng::nodes::global::clarity::apply_clarity`]'s, see that function's
/// doc comment for the full argument: `amount == 0` gives
/// `raw_delta_encoded == 0` exactly, `luma_e` is already inside `[0, 1]` by
/// construction so the encoded clamp below is then an exact no-op, and
/// `0 * finite == 0` in IEEE-754 makes the linear delta exactly zero, so the
/// neutral node is a bit-exact identity even for out-of-gamut scene-linear
/// pixels ([`tests::identity_at_zero_amount_holds_for_out_of_gamut_pixels`]).
///
/// **Why no local-extremum clamp.** Clarity and texture clamp the boosted
/// value into `[local_min, local_max]` so they can never create a new local
/// extremum. Sharpening must be allowed to: the bright/dark rim either side
/// of an edge is the effect, not an artifact of it. The `[0, 1]` encoded
/// clamp is the only bound, and `detail`/`masking` are the controls that keep
/// that rim from becoming a visible halo.
#[inline]
pub fn apply_sharpen(
    rgba: [f32; 4],
    luma_e: f32,
    blur_e: f32,
    edge_grad: f32,
    s: SharpenAmounts,
) -> [f32; 4] {
    let high = luma_e - blur_e;
    // `detail`: soft-clip the high-pass amplitude, then blend back toward the
    // raw value as detail rises.
    let clipped = HALO_KNEE * (high / HALO_KNEE).tanh();
    let d01 = (s.detail / 100.0).clamp(0.0, 1.0);
    let high_eff = clipped + (high - clipped) * d01;
    // `masking`: gate by the edge map. `mix(1, edge, 0) == 1` exactly, so a
    // zero masking slider costs nothing.
    let m01 = (s.masking / 100.0).clamp(0.0, 1.0);
    let thresh = (m01 * MASK_FULL_GRADIENT).max(1.0e-6);
    let edge = smoothstep(0.0, thresh, edge_grad);
    let mask = 1.0 + (edge - 1.0) * m01;

    let raw_delta_encoded = (s.amount / 100.0) * mask * high_eff;
    let boosted = (luma_e + raw_delta_encoded).clamp(0.0, 1.0);
    let delta_encoded = boosted - luma_e;
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);
    let out = [
        rgba[0] + delta_linear,
        rgba[1] + delta_linear,
        rgba[2] + delta_linear,
        rgba[3],
    ];
    if !out[0].is_finite() || !out[1].is_finite() || !out[2].is_finite() {
        return rgba;
    }
    out
}

/// The `global.sharpen` develop node.
#[derive(Default)]
pub struct SharpenNode {}

impl SharpenNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("global.sharpen");

    /// A fresh node.
    pub fn new() -> SharpenNode {
        SharpenNode::default()
    }
}

fn amounts_from(params: &ParamBlock) -> SharpenAmounts {
    SharpenAmounts {
        amount: params.get_f64_or("amount", 0.0) as f32,
        detail: params.get_f64_or("detail", 0.0) as f32,
        masking: params.get_f64_or("masking", 0.0) as f32,
    }
}

impl RenderNode for SharpenNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois(vec![out.expand(APRON); 1])
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("global.sharpen: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.sharpen: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let s = amounts_from(params);
        let radius = params.get_f64_or("radius", 1.0) as f32;
        let weights = working_luma_weights();

        let mut ubo_bytes = [0u8; 32];
        for (i, v) in [
            weights[0], weights[1], weights[2], s.amount, radius, s.detail, s.masking, 0.0,
        ]
        .iter()
        .enumerate()
        {
            ubo_bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.sharpen params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let scratch = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("global.sharpen scratch blur_h"),
            size: wgpu::Extent3d {
                width: extent.w.max(1),
                height: extent.h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());

        let pipe_h = ctx.kernels.compute_pipeline(SHARPEN_WGSL, "pass_h")?;
        let bg0_h = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_h in"),
            layout: &pipe_h.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg1_h = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_h out"),
            layout: &pipe_h.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&scratch_view),
            }],
        });
        let bg2_h = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_h params"),
            layout: &pipe_h.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_h,
            &[&bg0_h, &bg1_h, &bg2_h],
            extent,
            "global.sharpen.pass_h",
        );

        let pipe_v = ctx.kernels.compute_pipeline(SHARPEN_WGSL, "pass_v")?;
        let bg0_v = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_v in"),
            layout: &pipe_v.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scratch_view),
                },
            ],
        });
        let bg1_v = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_v out"),
            layout: &pipe_v.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg2_v = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.sharpen pass_v params"),
            layout: &pipe_v.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipe_v,
            &[&bg0_v, &bg1_v, &bg2_v],
            extent,
            "global.sharpen.pass_v",
        );

        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input_view = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.sharpen: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let s = amounts_from(params);
        let radius = params.get_f64_or("radius", 1.0) as f32;
        let weights = working_luma_weights();

        let (iw, ih) = (input.extent.w, input.extent.h);
        let mut luma_plane = vec![0f32; (iw as usize) * (ih as usize)];
        for y in 0..ih {
            for x in 0..iw {
                let p = input.get_rgba_f32(x, y);
                luma_plane[(y * iw + x) as usize] = encoded_luma([p[0], p[1], p[2]], weights);
            }
        }
        let blur_plane = gaussian_blur_encoded(&luma_plane, iw, ih, radius);

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        let ox = out_roi.x as i64 - in_roi.x as i64;
        let oy = out_roi.y as i64 - in_roi.y as i64;
        let ix_max = iw as i64 - 1;
        let iy_max = ih as i64 - 1;
        out.par_fill_rows(|ly, row| {
            let iy = (oy + ly as i64).clamp(0, iy_max) as u32;
            for lx in 0..w {
                let ix = (ox + lx as i64).clamp(0, ix_max) as u32;
                let p = input.get_rgba_f32(ix, iy);
                let i = (iy * iw + ix) as usize;
                let grad = edge_gradient(&luma_plane, iw, ih, ix, iy);
                let o = apply_sharpen(p, luma_plane[i], blur_plane[i], grad, s);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for SharpenNode {
    /// Neutral is `amount == 0` and nothing else: with no amount, `radius`,
    /// `detail` and `masking` have nothing to scale, so the node is a pixel
    /// identity whatever they hold. This matches `lightbox-edit`'s own
    /// statement of the leaf's neutral ("Neutral = amount zero (renders
    /// unmodified)", `leaves.rs`'s `Sharpen` doc comment), and it means a
    /// recipe carrying a non-default radius with zero amount still elides the
    /// node entirely rather than paying for a no-op blur.
    fn is_identity(p: &GlobalStages) -> bool {
        p.detail.sharpen.amount == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let s = p.detail.sharpen;
        ParamBlock::from_fields([
            ("amount", ParamValue::Float(s.amount as f64)),
            ("radius", ParamValue::Float(s.radius as f64)),
            ("detail", ParamValue::Float(s.detail as f64)),
            ("masking", ParamValue::Float(s.masking as f64)),
        ])
        .expect("sharpen fields are always finite (clamped on ingest)")
    }

    fn roi_in(&self, out: Roi, _scale: RenderScale) -> Roi {
        out.expand(APRON)
    }
}

/// Factory registering [`SharpenNode`].
#[derive(Default)]
pub struct SharpenFactory {}

impl NodeFactory for SharpenFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(SharpenNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(SHARPEN_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::Sharpen;

    fn stages(sharpen: Sharpen) -> GlobalStages {
        GlobalStages {
            detail: lightbox_edit::Detail {
                sharpen,
                ..Default::default()
            },
            ..GlobalStages::default()
        }
    }

    // ── is_identity / param_block ────────────────────────────────────────

    #[test]
    fn is_identity_tracks_amount_only() {
        let g = GlobalStages::default();
        assert!(
            SharpenNode::is_identity(&g),
            "the default recipe is neutral"
        );

        let mut amount = g;
        amount.detail.sharpen.amount = 40.0;
        assert!(!SharpenNode::is_identity(&amount));

        // radius/detail/masking alone cannot change a pixel: with amount 0
        // the delta is exactly 0, so the node must still elide.
        for tweak in [
            Sharpen {
                radius: 3.0,
                ..Sharpen::default()
            },
            Sharpen {
                detail: 100.0,
                ..Sharpen::default()
            },
            Sharpen {
                masking: 100.0,
                ..Sharpen::default()
            },
        ] {
            assert!(
                SharpenNode::is_identity(&stages(tweak)),
                "amount == 0 is neutral whatever the shaping controls hold: {tweak:?}"
            );
        }
    }

    #[test]
    fn param_block_carries_all_four_controls() {
        let pb = SharpenNode::param_block(&stages(Sharpen {
            amount: 85.0,
            radius: 2.5,
            detail: 30.0,
            masking: 60.0,
        }));
        assert_eq!(pb.get_f64("amount"), Some(85.0));
        assert_eq!(pb.get_f64("radius"), Some(2.5));
        assert_eq!(pb.get_f64("detail"), Some(30.0));
        assert_eq!(pb.get_f64("masking"), Some(60.0));
    }

    /// `param_block` and the node's declared [`SCHEMA`] must agree: a field
    /// name or type this node emits but does not declare would be rejected
    /// the moment the block is decoded through the schema, which is what
    /// `from_canonical_cbor` does here. A neutral block also has to carry the
    /// leaf's real default radius (`1.0`), not a zero.
    #[test]
    fn param_block_round_trips_through_the_declared_schema() {
        let pb = SharpenNode::param_block(&stages(Sharpen::default()));
        assert_eq!(pb.get_f64("radius"), Some(1.0), "default radius is 1.0");
        let decoded = ParamBlock::from_canonical_cbor(pb.canonical_bytes(), &SCHEMA)
            .expect("every field this node emits is declared in its schema");
        assert_eq!(decoded, pb);

        let bogus = ParamBlock::from_fields([("sharpness", ParamValue::Float(1.0))]).unwrap();
        assert!(
            ParamBlock::from_canonical_cbor(bogus.canonical_bytes(), &SCHEMA).is_err(),
            "the schema must be real, not empty-accept-anything"
        );
    }

    #[test]
    fn plan_pads_by_the_blur_plus_mask_apron() {
        let node = SharpenNode::new();
        let out = Roi {
            x: 5,
            y: 5,
            w: 40,
            h: 40,
        };
        let params = ParamBlock::from_fields::<&str, _>([]).unwrap();
        assert_eq!(node.plan(out, 1.0, &params).0[0], out.expand(APRON));
        assert_eq!(node.roi_in(out, RenderScale::OneToOne), out.expand(APRON));
    }

    // ── identity ─────────────────────────────────────────────────────────

    #[test]
    fn zero_amount_is_exact_identity_at_any_blur_and_gradient() {
        let rgba = [0.3f32, 0.6, 0.9, 1.0];
        let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
        for blur_e in [0.0f32, 0.2, luma_e, 0.8, 1.0] {
            for grad in [0.0f32, 0.01, 0.5] {
                for (detail, masking) in [(0.0f32, 0.0f32), (100.0, 100.0), (50.0, 25.0)] {
                    let o = apply_sharpen(
                        rgba,
                        luma_e,
                        blur_e,
                        grad,
                        SharpenAmounts {
                            amount: 0.0,
                            detail,
                            masking,
                        },
                    );
                    assert_eq!(o, rgba, "blur_e={blur_e} grad={grad}");
                }
            }
        }
    }

    /// The identity contract must hold for OUT-OF-GAMUT pixels too, the
    /// property [`apply_sharpen`]'s doc comment claims (and the reason the
    /// reapply is a local-derivative delta rather than an encode/decode round
    /// trip).
    #[test]
    fn identity_at_zero_amount_holds_for_out_of_gamut_pixels() {
        for rgba in [
            [1.4f32, 1.1, 0.9, 1.0],
            [-0.02f32, 0.01, 0.03, 1.0],
            [3.0f32, 3.0, 3.0, 1.0],
        ] {
            let luma_e = encoded_luma([rgba[0], rgba[1], rgba[2]], working_luma_weights());
            let o = apply_sharpen(rgba, luma_e, 0.25, 0.3, SharpenAmounts::default());
            assert_eq!(o, rgba, "rgba={rgba:?}");
        }
    }

    // ── the four controls ────────────────────────────────────────────────

    #[test]
    fn amount_scales_the_high_pass_and_its_sign_follows_it() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.55f32;
        let above = apply_sharpen(
            rgba,
            luma_e,
            0.50,
            0.0,
            SharpenAmounts {
                amount: 100.0,
                detail: 100.0,
                masking: 0.0,
            },
        );
        let below = apply_sharpen(
            rgba,
            luma_e,
            0.60,
            0.0,
            SharpenAmounts {
                amount: 100.0,
                detail: 100.0,
                masking: 0.0,
            },
        );
        assert!(
            above[0] > rgba[0],
            "a pixel brighter than its blur must brighten: {above:?}"
        );
        assert!(
            below[0] < rgba[0],
            "a pixel darker than its blur must darken: {below:?}"
        );

        let half = apply_sharpen(
            rgba,
            luma_e,
            0.50,
            0.0,
            SharpenAmounts {
                amount: 50.0,
                detail: 100.0,
                masking: 0.0,
            },
        );
        let full_delta = above[0] - rgba[0];
        let half_delta = half[0] - rgba[0];
        assert!(
            (half_delta * 2.0 - full_delta).abs() < 1e-6,
            "amount is a linear scale: half={half_delta} full={full_delta}"
        );
    }

    /// `detail` is halo suppression: at a LARGE high-pass amplitude (a hard
    /// edge's overshoot) low detail must move the pixel less than high detail
    /// does. At a SMALL amplitude (fine texture) the two must agree closely,
    /// because that is the whole point of a soft knee rather than a threshold.
    #[test]
    fn detail_suppresses_big_overshoot_but_leaves_fine_texture_alone() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.5f32;
        let amounts = |detail| SharpenAmounts {
            amount: 100.0,
            detail,
            masking: 0.0,
        };

        // Hard edge: high == 0.30, far above HALO_KNEE.
        let lo = apply_sharpen(rgba, luma_e, 0.20, 0.0, amounts(0.0));
        let hi = apply_sharpen(rgba, luma_e, 0.20, 0.0, amounts(100.0));
        let (lo_d, hi_d) = (lo[0] - rgba[0], hi[0] - rgba[0]);
        assert!(
            lo_d < hi_d * 0.5,
            "detail=0 must suppress a big overshoot hard: lo={lo_d} hi={hi_d}"
        );

        // Fine texture: high == 0.005, well below HALO_KNEE.
        let lo = apply_sharpen(rgba, luma_e, 0.495, 0.0, amounts(0.0));
        let hi = apply_sharpen(rgba, luma_e, 0.495, 0.0, amounts(100.0));
        let (lo_d, hi_d) = (lo[0] - rgba[0], hi[0] - rgba[0]);
        assert!(
            lo_d > hi_d * 0.98,
            "fine texture must survive detail=0 nearly intact: lo={lo_d} hi={hi_d}"
        );
    }

    /// `masking` is the control people rely on to keep sky clean. At
    /// `masking = 100` a flat region (gradient ~0) must be left essentially
    /// untouched while a strong edge still sharpens; at `masking = 0` both
    /// sharpen identically.
    #[test]
    fn masking_protects_flat_areas_and_spares_edges() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let luma_e = 0.5f32;
        let blur_e = 0.47f32;
        let masked = |masking, grad| {
            apply_sharpen(
                rgba,
                luma_e,
                blur_e,
                grad,
                SharpenAmounts {
                    amount: 100.0,
                    detail: 100.0,
                    masking,
                },
            )[0] - rgba[0]
        };

        let flat_unmasked = masked(0.0, 0.001);
        let edge_unmasked = masked(0.0, 0.40);
        assert!(
            (flat_unmasked - edge_unmasked).abs() < 1e-6,
            "masking=0 must ignore the edge map entirely: {flat_unmasked} vs {edge_unmasked}"
        );

        let flat_masked = masked(100.0, 0.001);
        let edge_masked = masked(100.0, 0.40);
        assert!(
            flat_masked.abs() < flat_unmasked.abs() * 0.02,
            "masking=100 must all but eliminate the flat-area boost: {flat_masked}"
        );
        assert!(
            edge_masked > edge_unmasked * 0.95,
            "masking=100 must still sharpen a real edge: {edge_masked} vs {edge_unmasked}"
        );
    }

    #[test]
    fn masking_is_monotonic_in_the_edge_gradient() {
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let mut prev = f32::NEG_INFINITY;
        for grad in [0.0f32, 0.02, 0.05, 0.1, 0.15, 0.2, 0.4] {
            let d = apply_sharpen(
                rgba,
                0.5,
                0.47,
                grad,
                SharpenAmounts {
                    amount: 100.0,
                    detail: 100.0,
                    masking: 100.0,
                },
            )[0] - rgba[0];
            assert!(
                d >= prev - 1e-7,
                "mask must not fall as the edge strengthens: grad={grad} d={d} prev={prev}"
            );
            prev = d;
        }
    }

    // ── the blur and the edge map ────────────────────────────────────────

    #[test]
    fn gaussian_kernel_is_symmetric_and_peaks_at_the_center() {
        let (taps, sum) = gaussian_kernel(1.0);
        let r = MAX_BLUR_RADIUS as usize;
        assert_eq!(taps.len(), 2 * r + 1);
        assert!((taps[r] - 1.0).abs() < 1e-6, "center tap is exp(0)");
        for d in 1..=r {
            assert!((taps[r - d] - taps[r + d]).abs() < 1e-9, "symmetric at {d}");
            assert!(taps[r - d] < taps[r - d + 1], "monotonically falling");
        }
        assert!(sum > 1.0);
    }

    #[test]
    fn gaussian_blur_leaves_a_flat_plane_untouched() {
        let (w, h) = (16u32, 16u32);
        let plane = vec![0.37f32; (w * h) as usize];
        for sigma in [0.5f32, 1.0, 3.0] {
            let blurred = gaussian_blur_encoded(&plane, w, h, sigma);
            for (i, v) in blurred.iter().enumerate() {
                assert!(
                    (v - 0.37).abs() < 1e-5,
                    "sigma={sigma} idx={i} v={v}: a constant plane must survive a normalized blur"
                );
            }
        }
    }

    /// A bigger radius must blur more, measured as the residual amplitude of
    /// a one-pixel-period stripe pattern.
    #[test]
    fn a_bigger_radius_blurs_more() {
        let (w, h) = (32u32, 32u32);
        let plane: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) % 2 == 0 { 0.2 } else { 0.8 })
            .collect();
        let amp = |sigma: f32| {
            let b = gaussian_blur_encoded(&plane, w, h, sigma);
            let mid = (h / 2 * w) as usize;
            (b[mid + 10] - b[mid + 11]).abs()
        };
        let (a_small, a_big) = (amp(0.5), amp(3.0));
        assert!(
            a_big < a_small,
            "sigma 3.0 must flatten the stripes more than sigma 0.5: {a_big} vs {a_small}"
        );
    }

    #[test]
    fn edge_gradient_is_zero_on_a_flat_plane_and_large_at_a_step() {
        let (w, h) = (16u32, 16u32);
        let flat = vec![0.5f32; (w * h) as usize];
        assert!(edge_gradient(&flat, w, h, 8, 8) < 1e-6);

        // A hard vertical step at x == 8: 0.2 on the left, 0.8 on the right.
        //
        // The exact value is worth pinning rather than bounding, because it
        // is what calibrates `MASK_FULL_GRADIENT`. A 3x3 Sobel reads the
        // 0.6-tall step across TWO pixels, so it is `0.3` at the two columns
        // flanking the step (7 and 8) and `0` at column 9; the step is
        // uniform in y, so the nine-Sobel mean at x == 8 is
        // `(3*0.3 + 3*0.3 + 3*0) / 9 == 0.2` exactly.
        let step: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) < 8 { 0.2 } else { 0.8 })
            .collect();
        let at_edge = edge_gradient(&step, w, h, 8, 8);
        let far = edge_gradient(&step, w, h, 2, 8);
        assert!(
            (at_edge - 0.2).abs() < 1e-5,
            "a 0.6-tall hard step must read as exactly MASK_FULL_GRADIENT: {at_edge}"
        );
        assert!(
            (at_edge - MASK_FULL_GRADIENT).abs() < 1e-5,
            "and that is the calibration MASK_FULL_GRADIENT claims: {at_edge}"
        );
        assert!(
            far < 1e-6,
            "the flat side of the step must read as no edge: {far}"
        );
    }

    /// The nine-Sobel mean is what keeps a single hot pixel from reading as
    /// an edge. Compare it against the single centered Sobel it replaces.
    #[test]
    fn a_lone_hot_pixel_reads_weaker_than_a_real_edge() {
        let (w, h) = (16u32, 16u32);
        let mut speck = vec![0.5f32; (w * h) as usize];
        speck[(8 * w + 8) as usize] = 0.8;
        let step: Vec<f32> = (0..(w * h))
            .map(|i| if (i % w) < 8 { 0.2 } else { 0.8 })
            .collect();
        let speck_g = edge_gradient(&speck, w, h, 8, 8);
        let edge_g = edge_gradient(&step, w, h, 8, 8);
        assert!(
            speck_g < edge_g * 0.6,
            "one 0.3-tall speck must read well below a 0.6-tall edge: {speck_g} vs {edge_g}"
        );
    }
}
