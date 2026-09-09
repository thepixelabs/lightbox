// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.contrast` (E10 task **A7**). Companion-domain sigmoid-family
//! pivot curve (spec §4.2 "Contrast | companion-encoded (ProPhoto primaries +
//! sRGB-like curve), sigmoid pivoted at encoded 18% gray (~0.46)").
//!
//! # The curve
//!
//! Each of R/G/B is independently encoded via
//! [`lightbox_color::matrix::spaces::companion_encode`], remapped by a
//! two-branch power curve pinned at the pivot (encoded 18% gray) **and at
//! both endpoints** for any `gamma`, then decoded back via
//! [`lightbox_color::matrix::spaces::companion_decode`]. `gamma =
//! 2^(amount/100)`: `amount ∈ [-100,100] ⇒ gamma ∈ [0.5, 2.0]`, `amount > 0`
//! steepens the pivot (more contrast), `amount < 0` flattens it (less
//! contrast), `amount = 0 ⇒ gamma = 1` is the exact identity. Because the
//! curve is pinned at 0 and 1 by construction (not by clamping), the
//! ±100 endpoints move **exactly 0%**, comfortably inside the A7 "< 1%"
//! contract. WGSL (`shaders/global_contrast.wgsl`) and CPU
//! ([`contrast_curve`]) evaluate the identical formula from the identical
//! `(gamma, pivot)` pair (computed once, on the CPU side, and handed to the
//! GPU kernel via its params UBO, the single-source-of-truth pattern also
//! used by `global.whites_blacks`), so CPU/GPU parity (§4.4) holds to float
//! rounding.

use std::sync::Arc;

use lightbox_color::matrix::spaces::{companion_decode, companion_encode};
use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The contrast pivot-curve kernel, naga-validated at build (`build.rs`).
const CONTRAST_WGSL: &str = include_str!("../../../../shaders/global_contrast.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
    name: "amount",
    kind: ParamKind::Float,
}]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.contrast"),
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

/// `gamma = 2^(amount/100)` (spec §4.2), `amount = 0 ⇒ gamma = 1` (identity).
#[inline]
pub fn contrast_gamma(amount: f32) -> f32 {
    (amount / 100.0).exp2()
}

/// The companion-encoded pivot: encoded 18% gray (spec §4.2 "~0.46"),
/// derived from [`companion_encode`] rather than a hand-copied literal so it
/// can never drift from the encode function it is paired with.
#[inline]
pub fn contrast_pivot() -> f32 {
    companion_encode([0.18, 0.18, 0.18])[0]
}

/// The pivot-anchored two-branch power curve (spec §4.2), evaluated in the
/// companion-encoded domain. Continuous and exactly pinned at `(0,0)`,
/// `(pivot,pivot)`, `(1,1)` for any `gamma > 0`.
#[inline]
pub fn contrast_curve(e: f32, pivot: f32, gamma: f32) -> f32 {
    let e = e.clamp(0.0, 1.0);
    if e <= pivot {
        pivot * (e / pivot).powf(gamma)
    } else {
        1.0 - (1.0 - pivot) * ((1.0 - e) / (1.0 - pivot)).powf(gamma)
    }
}

/// Applies the full companion-domain contrast op to one working-space RGBA
/// pixel (alpha untouched), the CPU parity anchor for `global_contrast.wgsl`.
#[inline]
pub fn apply_contrast(rgba: [f32; 4], gamma: f32, pivot: f32) -> [f32; 4] {
    let enc = companion_encode([rgba[0], rgba[1], rgba[2]]);
    let curved = [
        contrast_curve(enc[0], pivot, gamma),
        contrast_curve(enc[1], pivot, gamma),
        contrast_curve(enc[2], pivot, gamma),
    ];
    let dec = companion_decode(curved);
    [dec[0], dec[1], dec[2], rgba[3]]
}

/// The `global.contrast` develop node.
#[derive(Default)]
pub struct ContrastNode {}

impl ContrastNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.contrast");

    /// A fresh node.
    pub fn new() -> ContrastNode {
        ContrastNode::default()
    }
}

impl RenderNode for ContrastNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
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
            .ok_or_else(|| NodeError::Other("global.contrast: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.contrast: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let amount = params.get_f64_or("amount", 0.0) as f32;
        let gamma = contrast_gamma(amount);
        let pivot = contrast_pivot();
        let mut ubo_bytes = [0u8; 16];
        ubo_bytes[0..4].copy_from_slice(&gamma.to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&pivot.to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.contrast params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(CONTRAST_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.contrast in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.contrast out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.contrast params"),
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
            extent,
            "global.contrast",
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
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.contrast: missing input tile".into()))?
            .pixels;
        let amount = params.get_f64_or("amount", 0.0) as f32;
        let gamma = contrast_gamma(amount);
        let pivot = contrast_pivot();
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_contrast(p, gamma, pivot);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for ContrastNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.contrast == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        ParamBlock::from_fields([("amount", ParamValue::Float(p.contrast as f64))])
            .expect("contrast amount is always finite (clamped on ingest — spec A2)")
    }
}

/// Factory registering [`ContrastNode`].
#[derive(Default)]
pub struct ContrastFactory {}

impl NodeFactory for ContrastFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(ContrastNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(CONTRAST_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_amount_is_exact_identity() {
        assert_eq!(contrast_gamma(0.0), 1.0);
        let pivot = contrast_pivot();
        for e in [0.0f32, 0.1, pivot, 0.5, 0.9, 1.0] {
            assert!((contrast_curve(e, pivot, 1.0) - e).abs() < 1e-5, "e={e}");
        }
    }

    /// A7 acceptance: endpoints move < 1% at ±100 (the curve is pinned exactly
    /// 0% movement, by construction).
    #[test]
    fn endpoints_are_pinned_at_extreme_amounts() {
        let pivot = contrast_pivot();
        for amount in [-100.0f32, 100.0] {
            let gamma = contrast_gamma(amount);
            assert!(
                contrast_curve(0.0, pivot, gamma).abs() < 1e-6,
                "amount={amount}"
            );
            assert!(
                (contrast_curve(1.0, pivot, gamma) - 1.0).abs() < 1e-6,
                "amount={amount}"
            );
        }
    }

    #[test]
    fn positive_contrast_steepens_the_pivot_negative_flattens_it() {
        let pivot = contrast_pivot();
        let eps = 0.02;
        let base =
            contrast_curve(pivot + eps, pivot, 1.0) - contrast_curve(pivot - eps, pivot, 1.0);
        let steep = contrast_curve(pivot + eps, pivot, contrast_gamma(100.0))
            - contrast_curve(pivot - eps, pivot, contrast_gamma(100.0));
        let flat = contrast_curve(pivot + eps, pivot, contrast_gamma(-100.0))
            - contrast_curve(pivot - eps, pivot, contrast_gamma(-100.0));
        assert!(steep > base, "steep={steep} base={base}");
        assert!(flat < base, "flat={flat} base={base}");
    }

    /// Monotonicity property test: the curve never reverses tone order across
    /// a dense sweep, at both extreme settings.
    #[test]
    fn curve_is_monotone_across_the_full_range() {
        let pivot = contrast_pivot();
        for amount in [-100.0f32, -50.0, 0.0, 50.0, 100.0] {
            let gamma = contrast_gamma(amount);
            let mut prev = -1.0f32;
            for i in 0..=200 {
                let e = i as f32 / 200.0;
                let v = contrast_curve(e, pivot, gamma);
                assert!(v >= prev - 1e-6, "amount={amount} non-monotone at e={e}");
                prev = v;
            }
        }
    }

    #[test]
    fn is_identity_tracks_the_contrast_field() {
        let mut g = GlobalStages::default();
        assert!(ContrastNode::is_identity(&g));
        g.contrast = 10.0;
        assert!(!ContrastNode::is_identity(&g));
    }
}
