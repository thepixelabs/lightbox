// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `fx.grain`, film grain ([`lightbox_edit::leaves::Grain`]: `amount`,
//! `size`, `roughness`).
//!
//! # The noise field never moves
//!
//! **The grain value at a pixel is a pure function of that pixel's absolute
//! canvas position** and the two fixed seeds [`SEED_FINE`]/[`SEED_COARSE`].
//! Nothing in this module or in `shaders/global_grain.wgsl` reads a frame
//! counter, a clock, a dispatch id, or any other per-render source of
//! entropy. That is deliberate and it is the difference between grain and a
//! defect: a field reseeded per frame crawls across the image while you drag
//! a slider, which reads as broken rendering rather than as film. Re-render
//! the same pixel a thousand times and it gets the same grain value every
//! time.
//!
//! The hash is integer-only, wrapping `u32` multiplies plus logical shifts,
//! which WGSL and Rust agree on bit-for-bit, so [`lattice_value`] and the
//! kernel's `lattice_value` return the *same* numbers rather than merely
//! similar-looking noise. (A float hash of the `sin(dot(p, k)) * big`
//! variety would not survive that: it disagrees across drivers, so CPU and
//! GPU renders of the same photograph would carry visibly different grain.)
//!
//! # The field
//!
//! Two octaves of smooth (Hermite-interpolated) value noise:
//!
//! * a **fine** octave whose lattice spacing is [`grain_cell_px`] pixels,
//!   which is what `size` drives ([`GRAIN_CELL_MIN`] px at `size = 0`,
//!   [`GRAIN_CELL_MAX`] px at `size = 100`);
//! * a **coarse** octave at [`GRAIN_ROUGH_CELL_SCALE`] times that spacing,
//!   which modulates the fine octave's amplitude in proportion to
//!   `roughness`. That is what turns even grain into clumpy, uneven grain:
//!   at `roughness = 100` the local amplitude swings between `0` and `2`.
//!
//! # Applying it
//!
//! Grain is **monochromatic**, a luminance perturbation added identically to
//! R/G/B, which is what film grain is and what Lightroom's grain is. The
//! delta is computed in the companion-encoded domain (so its visual weight
//! is even across the tone range) and converted to a scene-linear delta
//! through the first derivative of the sRGB EOTF, the same reapply
//! `clarity::apply_clarity` documents at length: it keeps `amount == 0` an
//! exact pixel identity even for out-of-gamut pixels, where a full
//! `companion_encode -> adjust -> companion_decode` round trip would clamp.
//! [`grain_weight`] tapers the delta to zero at both ends of the encoded
//! range, where there is no headroom left to perturb.
//!
//! # Known limit: grain size is in output-canvas pixels
//!
//! [`grain_cell_px`] is measured in the pixels of the canvas this node is
//! handed, so if the engine ever renders the develop preview at a decimated
//! scale the on-screen grain will be coarser relative to the image than the
//! exported full-resolution grain. It does not bite today, `util.resize` is
//! extent-identity in this engine (`docs/plan/epics/E11-deviations.md`), so
//! preview and export see the same canvas; it is named here rather than
//! silently inherited.
//!
//! GPU (`shaders/global_grain.wgsl`) and CPU (this module) implement the
//! identical formulas from the identical constants, so CPU/GPU parity (§4.4)
//! holds to numerical rounding (`tests/e12_effects.rs`).
//!
//! # Node id vs module path
//!
//! The id is `fx.grain`, spec §4.4's own name for this stage: the prefix
//! follows the **pipeline segment** ([`super::build_effects_segment`]), not
//! the module path or the trait. See `vignette.rs`'s matching section for
//! the full reasoning and for why the file still lives under
//! `nodes/global/`.

use std::sync::Arc;

use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::global::clarity::srgb_eotf_deriv;
use crate::ng::nodes::global::tone_recovery::{working_luma, working_luma_weights};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The grain kernel, naga-validated at build (`build.rs`).
const GRAIN_WGSL: &str = include_str!("../../../../shaders/global_grain.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "amount",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "size",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "roughness",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("fx.grain"),
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
//    header; the CPU/GPU parity test catches drift) ─────────────────────────

/// Lattice spacing in pixels at `size = 0`: one pixel, the finest grain the
/// output grid can carry.
pub const GRAIN_CELL_MIN: f32 = 1.0;
/// Lattice spacing in pixels at `size = 100`.
pub const GRAIN_CELL_MAX: f32 = 12.0;
/// The roughness octave's lattice spacing, as a multiple of the fine
/// octave's.
pub const GRAIN_ROUGH_CELL_SCALE: f32 = 3.0;
/// Encoded-domain amplitude at `amount = 100` and a full-swing noise value.
///
/// Calibrated against a flat scene-linear 0.25 patch rendered through the
/// real engine: `amount = 25, size = 25` gives a display-luma standard
/// deviation of about 8 codes out of 255 (a fine, tasteful film grain),
/// `amount = 70` about 24, and `amount = 100` about 35 (a deliberately
/// extreme endpoint, like Lightroom's). Move this and those move with it.
pub const GRAIN_RANGE: f32 = 0.30;
/// Grain tapers in from zero over this much of the encoded range at the
/// black end.
pub const GRAIN_LOW: f32 = 0.05;
/// Grain tapers back out to zero above this encoded value.
pub const GRAIN_HIGH: f32 = 0.95;
/// The fine octave's fixed seed. Fixed, not generated: see this module's
/// "the noise field never moves".
pub const SEED_FINE: u32 = 0x5f37_59df;
/// The roughness octave's fixed seed.
pub const SEED_COARSE: u32 = 0x9e37_79b9;

/// Smoothstep (Hermite), duplicated from `global_grain.wgsl`'s
/// `smoothstep_01` so the two read line for line.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The fine octave's lattice spacing in pixels for a `0..=100` `size`.
#[inline]
pub fn grain_cell_px(size: f32) -> f32 {
    GRAIN_CELL_MIN + (GRAIN_CELL_MAX - GRAIN_CELL_MIN) * (size / 100.0).clamp(0.0, 1.0)
}

/// Zero-taper at both ends of the encoded range: no headroom to perturb at
/// clipped black or clipped white.
#[inline]
pub fn grain_weight(e: f32) -> f32 {
    let lo = smoothstep(0.0, GRAIN_LOW, e);
    let hi = 1.0 - smoothstep(GRAIN_HIGH, 1.0, e);
    (lo * hi).clamp(0.0, 1.0)
}

/// The "lowbias32" integer finalizer. Bit-exact twin of
/// `global_grain.wgsl`'s `hash_u32`: WGSL `u32` multiplication wraps and
/// `>>` on `u32` is a logical shift, which is what `wrapping_mul` and Rust's
/// `u32 >>` do.
#[inline]
pub fn hash_u32(x: u32) -> u32 {
    let mut h = x;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    h
}

/// One lattice sample in `[0, 1)`. The 24-bit mantissa slice keeps the
/// integer-to-float conversion exact on both backends.
#[inline]
pub fn lattice_value(ix: i32, iy: i32, seed: u32) -> f32 {
    let a = (ix as u32).wrapping_mul(0x9e37_79b1);
    let b = (iy as u32).wrapping_mul(0x85eb_ca6b);
    // `(b << 16) | (b >> 16)`, which is what the kernel writes because WGSL
    // has no rotate intrinsic; on `u32` a 16-bit rotate is its own inverse,
    // so this is the identical value.
    let bb = b.rotate_right(16);
    let h = hash_u32(a ^ bb ^ seed);
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Smooth value noise on the unit lattice, centred on zero: `[-0.5, 0.5)`.
///
/// The lerps are written as `a * (1 - t) + b * t`, WGSL's own `mix`
/// expansion, so the kernel matches this term for term.
#[inline]
pub fn value_noise(px: f32, py: f32, seed: u32) -> f32 {
    let fx = px.floor();
    let fy = py.floor();
    let ix = fx as i32;
    let iy = fy as i32;
    let tx = px - fx;
    let ty = py - fy;
    let ux = tx * tx * (3.0 - 2.0 * tx);
    let uy = ty * ty * (3.0 - 2.0 * ty);
    let n00 = lattice_value(ix, iy, seed);
    let n10 = lattice_value(ix.wrapping_add(1), iy, seed);
    let n01 = lattice_value(ix, iy.wrapping_add(1), seed);
    let n11 = lattice_value(ix.wrapping_add(1), iy.wrapping_add(1), seed);
    let a = n00 * (1.0 - ux) + n10 * ux;
    let b = n01 * (1.0 - ux) + n11 * ux;
    a * (1.0 - uy) + b * uy - 0.5
}

/// The two-octave grain field at absolute canvas position `(ax, ay)`, in
/// `[-1, 1]`. `inv_cell_fine`/`inv_cell_coarse` are the reciprocals of the
/// two lattice spacings (precomputed per eval, not per pixel).
#[inline]
pub fn grain_field(
    ax: f32,
    ay: f32,
    inv_cell_fine: f32,
    inv_cell_coarse: f32,
    roughness: f32,
) -> f32 {
    let n1 = value_noise(ax * inv_cell_fine, ay * inv_cell_fine, SEED_FINE);
    let n2 = value_noise(ax * inv_cell_coarse, ay * inv_cell_coarse, SEED_COARSE);
    let r = (roughness / 100.0).clamp(0.0, 1.0);
    let amp = 1.0 + r * (2.0 * n2);
    (2.0 * n1 * amp).clamp(-1.0, 1.0)
}

/// The full per-pixel op and the numeric parity anchor for
/// `global_grain.wgsl`'s `main`: given a working RGBA pixel, its encoded
/// luma and the grain field value there, returns the grained RGBA (alpha
/// untouched).
///
/// `delta_encoded` is exactly `0.0` at `amount == 0` for any finite `n` and
/// weight, so the identity contract holds bit-for-bit rather than to a
/// tolerance.
#[inline]
pub fn apply_grain(rgba: [f32; 4], luma_e: f32, n: f32, amount: f32) -> [f32; 4] {
    let delta_encoded = GRAIN_RANGE * (amount / 100.0) * n * grain_weight(luma_e);
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

/// Companion-encoded scalar luma, the domain the grain delta lives in (same
/// definition `clarity::encoded_luma` and `global_grain.wgsl`'s
/// `encoded_luma_of` use).
#[inline]
fn encoded_luma(rgba: [f32; 4], weights: [f32; 3]) -> f32 {
    let l = working_luma([rgba[0], rgba[1], rgba[2]], weights);
    lightbox_color::matrix::spaces::companion_encode([l, l, l])[0]
}

/// The `fx.grain` develop node.
#[derive(Default)]
pub struct GrainNode {}

impl GrainNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("fx.grain");

    /// A fresh node.
    pub fn new() -> GrainNode {
        GrainNode::default()
    }
}

impl RenderNode for GrainNode {
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
            .ok_or_else(|| NodeError::Other("fx.grain: missing input tile".into()))?;
        let in_roi = input.roi;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("fx.grain: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: in_roi.w,
            h: in_roi.h,
        });

        let amount = params.get_f64_or("amount", 0.0) as f32;
        let size = params.get_f64_or("size", 0.0) as f32;
        let roughness = params.get_f64_or("roughness", 0.0) as f32;
        let cell = grain_cell_px(size);
        let weights = working_luma_weights();

        let mut ubo_bytes = [0u8; 48];
        let mut put_f32 = |slot: usize, v: f32| {
            ubo_bytes[slot * 4..slot * 4 + 4].copy_from_slice(&v.to_le_bytes());
        };
        put_f32(0, weights[0]);
        put_f32(1, weights[1]);
        put_f32(2, weights[2]);
        put_f32(3, amount);
        put_f32(4, 1.0 / cell);
        put_f32(5, 1.0 / (cell * GRAIN_ROUGH_CELL_SCALE));
        put_f32(6, roughness);
        // The GPU backend hands every input tile a zero-origin ROI
        // (`ng/exec/gpu/mod.rs:109-119`), so this is zero today; threading
        // it keeps the kernel correct the day that backend reports real
        // ROIs, the same shape `geom.crop`'s `in_off_x`/`in_off_y` uses.
        ubo_bytes[32..36].copy_from_slice(&in_roi.x.to_le_bytes());
        ubo_bytes[36..40].copy_from_slice(&in_roi.y.to_le_bytes());

        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx.grain params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(GRAIN_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.grain in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.grain out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx.grain params"),
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
            "fx.grain",
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
            .ok_or_else(|| NodeError::Cpu("fx.grain: missing input tile".into()))?;
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;

        let amount = params.get_f64_or("amount", 0.0) as f32;
        let size = params.get_f64_or("size", 0.0) as f32;
        let roughness = params.get_f64_or("roughness", 0.0) as f32;
        let cell = grain_cell_px(size);
        let inv_fine = 1.0 / cell;
        let inv_coarse = 1.0 / (cell * GRAIN_ROUGH_CELL_SCALE);
        let weights = working_luma_weights();

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        // Same clamped local-coordinate mapping `clarity::eval_cpu` uses;
        // zero offsets for every pointwise splice (`in_roi == out_roi`).
        let ox = out_roi.x as i64 - in_roi.x as i64;
        let oy = out_roi.y as i64 - in_roi.y as i64;
        let ix_max = input.extent.w as i64 - 1;
        let iy_max = input.extent.h as i64 - 1;
        out.par_fill_rows(|ly, row| {
            let iy = (oy + ly as i64).clamp(0, iy_max.max(0)) as u32;
            // The noise coordinate is the ABSOLUTE canvas position, which is
            // what makes a tiled CPU render and a whole-image GPU render
            // agree on the field (and what makes it stand still).
            let ay = (out_roi.y as i64 + ly as i64) as f32;
            for lx in 0..w {
                let ix = (ox + lx as i64).clamp(0, ix_max.max(0)) as u32;
                let ax = (out_roi.x as i64 + lx as i64) as f32;
                let p = input.get_rgba_f32(ix, iy);
                let luma_e = encoded_luma(p, weights);
                let n = grain_field(ax, ay, inv_fine, inv_coarse, roughness);
                let o = apply_grain(p, luma_e, n, amount);
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for GrainNode {
    fn is_identity(p: &GlobalStages) -> bool {
        p.effects.grain.amount == 0.0
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let g = &p.effects.grain;
        ParamBlock::from_fields([
            ("amount", ParamValue::Float(g.amount as f64)),
            ("size", ParamValue::Float(g.size as f64)),
            ("roughness", ParamValue::Float(g.roughness as f64)),
        ])
        .expect("grain fields are always finite (clamped on ingest, spec A2)")
    }
}

/// Factory registering [`GrainNode`].
#[derive(Default)]
pub struct GrainFactory {}

impl NodeFactory for GrainFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GrainNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(GRAIN_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::leaves::Grain;

    fn stages_with(g: Grain) -> GlobalStages {
        let mut s = GlobalStages::default();
        s.effects.grain = g;
        s
    }

    fn field(x: f32, y: f32, size: f32, roughness: f32) -> f32 {
        let cell = grain_cell_px(size);
        grain_field(
            x,
            y,
            1.0 / cell,
            1.0 / (cell * GRAIN_ROUGH_CELL_SCALE),
            roughness,
        )
    }

    // ── trait wiring ──────────────────────────────────────────────────────

    #[test]
    fn is_identity_tracks_only_the_amount_field() {
        let mut s = GlobalStages::default();
        assert!(GrainNode::is_identity(&s));
        s.effects.grain.size = 60.0;
        s.effects.grain.roughness = 80.0;
        assert!(
            GrainNode::is_identity(&s),
            "size/roughness alone render nothing"
        );
        s.effects.grain.amount = 25.0;
        assert!(!GrainNode::is_identity(&s));
    }

    #[test]
    fn param_block_carries_every_leaf_field() {
        let s = stages_with(Grain {
            amount: 40.0,
            size: 25.0,
            roughness: 60.0,
        });
        let pb = GrainNode::param_block(&s);
        assert_eq!(pb.get_f64("amount"), Some(40.0));
        assert_eq!(pb.get_f64("size"), Some(25.0));
        assert_eq!(pb.get_f64("roughness"), Some(60.0));
    }

    // ── the headline contract: the field is deterministic ─────────────────

    /// The anti-shimmer property, stated as a test: the same absolute pixel
    /// position always yields the same grain value, no matter how many
    /// times it is evaluated or in what order.
    #[test]
    fn the_grain_field_is_a_pure_function_of_position() {
        let samples: Vec<f32> = (0..64).map(|i| field(i as f32, 7.0, 25.0, 40.0)).collect();
        for _ in 0..8 {
            for (i, want) in samples.iter().enumerate() {
                assert_eq!(field(i as f32, 7.0, 25.0, 40.0), *want, "x={i}");
            }
        }
        // And evaluating a neighbour in between changes nothing.
        let a = field(10.0, 10.0, 25.0, 40.0);
        let _ = field(11.0, 10.0, 25.0, 40.0);
        assert_eq!(field(10.0, 10.0, 25.0, 40.0), a);
    }

    /// A tiled CPU render evaluates a pixel with `lx` local but `ax`
    /// absolute; the field must key on the absolute coordinate only, or the
    /// grain would visibly seam at every tile boundary.
    #[test]
    fn the_field_keys_on_absolute_position_not_tile_local_position() {
        // Same absolute pixel (300, 300) reached from two different tile
        // origins yields the same value because only the sum is used.
        let from_tile_a = field(256.0 + 44.0, 256.0 + 44.0, 25.0, 0.0);
        let from_tile_b = field(300.0, 300.0, 25.0, 0.0);
        assert_eq!(from_tile_a, from_tile_b);
    }

    #[test]
    fn the_field_actually_varies_across_the_canvas() {
        let vals: Vec<f32> = (0..200).map(|i| field(i as f32, 3.0, 0.0, 0.0)).collect();
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / vals.len() as f32;
        assert!(var > 1e-3, "a constant field is not grain: variance {var}");
        assert!(
            mean.abs() < 0.2,
            "the field should be roughly centred: {mean}"
        );
    }

    #[test]
    fn the_field_stays_inside_minus_one_to_one() {
        for y in 0..40 {
            for x in 0..40 {
                let v = field(x as f32 * 0.37, y as f32 * 0.91, 50.0, 100.0);
                assert!((-1.0..=1.0).contains(&v), "({x},{y}) -> {v}");
            }
        }
    }

    // ── size and roughness ────────────────────────────────────────────────

    #[test]
    fn size_maps_to_a_growing_lattice_spacing() {
        assert_eq!(grain_cell_px(0.0), GRAIN_CELL_MIN);
        assert_eq!(grain_cell_px(100.0), GRAIN_CELL_MAX);
        assert!(grain_cell_px(50.0) > grain_cell_px(10.0));
    }

    /// Bigger `size` means a coarser field: neighbouring pixels are more
    /// alike, so the mean absolute difference between adjacent samples
    /// drops.
    #[test]
    fn a_bigger_size_produces_a_coarser_field() {
        let roughness = 0.0;
        let mad = |size: f32| {
            let n = 400;
            let mut acc = 0.0f32;
            for i in 0..n {
                let a = field(i as f32, 5.0, size, roughness);
                let b = field(i as f32 + 1.0, 5.0, size, roughness);
                acc += (a - b).abs();
            }
            acc / n as f32
        };
        let fine = mad(0.0);
        let coarse = mad(100.0);
        assert!(
            coarse < fine * 0.5,
            "coarse grain should change far more slowly: fine={fine} coarse={coarse}"
        );
    }

    /// Roughness makes the grain uneven: the amplitude of the field varies
    /// more from region to region.
    #[test]
    fn roughness_makes_the_local_amplitude_uneven() {
        let spread = |roughness: f32| {
            // Local RMS in 16-pixel blocks; the spread of those block RMS
            // values is what "uneven" means.
            let blocks: Vec<f32> = (0..24)
                .map(|b| {
                    let mut acc = 0.0f32;
                    for i in 0..16 {
                        let v = field((b * 16 + i) as f32, 11.0, 10.0, roughness);
                        acc += v * v;
                    }
                    (acc / 16.0).sqrt()
                })
                .collect();
            let mean = blocks.iter().sum::<f32>() / blocks.len() as f32;
            (blocks.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / blocks.len() as f32)
                .sqrt()
        };
        let even = spread(0.0);
        let rough = spread(100.0);
        assert!(
            rough > even,
            "roughness must widen the local-amplitude spread: even={even} rough={rough}"
        );
    }

    // ── the hash's cross-backend contract ─────────────────────────────────

    /// Pinned values. The whole cross-backend story rests on this hash
    /// being reproducible, so it is pinned rather than merely exercised: if
    /// these change, `global_grain.wgsl`'s `hash_u32`/`lattice_value` must
    /// change with them or CPU and GPU will render different grain from the
    /// same recipe.
    #[test]
    fn the_hash_is_pinned_to_exact_values() {
        assert_eq!(hash_u32(0), 0x0000_0000);
        assert_eq!(hash_u32(1), 0x6889_90c0);
        assert_eq!(hash_u32(2), 0xd113_2181);
        assert_eq!(hash_u32(0xdead_beef), 0xe628_c683);
    }

    /// The same pin one level up, at the lattice sampler the kernel calls.
    #[test]
    fn lattice_values_are_pinned_to_exact_values() {
        assert_eq!(lattice_value(0, 0, SEED_FINE), 0.359_352_65);
        assert_eq!(lattice_value(1, 0, SEED_FINE), 0.956_447_66);
        assert_eq!(lattice_value(0, 1, SEED_FINE), 0.163_003_8);
        assert_eq!(lattice_value(-7, -13, SEED_FINE), 0.043_395_28);
    }

    #[test]
    fn lattice_values_are_in_unit_range_and_well_spread() {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0f64;
        let n = 4096;
        for i in 0..n {
            let v = lattice_value(i % 64, i / 64, SEED_FINE);
            assert!((0.0..1.0).contains(&v), "i={i} v={v}");
            min = min.min(v);
            max = max.max(v);
            sum += v as f64;
        }
        assert!(min < 0.02 && max > 0.98, "min={min} max={max}");
        let mean = sum / n as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean={mean}");
    }

    #[test]
    fn the_two_octave_seeds_produce_different_fields() {
        let a = lattice_value(3, 5, SEED_FINE);
        let b = lattice_value(3, 5, SEED_COARSE);
        assert!((a - b).abs() > 1e-6, "a={a} b={b}");
    }

    #[test]
    fn negative_lattice_coordinates_are_handled() {
        // A crop can put the canvas origin anywhere; the hash must not
        // panic or degenerate on negative indices.
        let v = lattice_value(-7, -13, SEED_FINE);
        assert!((0.0..1.0).contains(&v), "v={v}");
    }

    // ── applying it ───────────────────────────────────────────────────────

    #[test]
    fn zero_amount_is_an_exact_pixel_identity() {
        for rgba in [
            [0.3f32, 0.6, 0.9, 1.0],
            [2.5f32, 2.5, 2.5, 0.5], // out of gamut, > 1
            [-0.02f32, 0.01, 0.03, 1.0],
        ] {
            let luma_e = encoded_luma(rgba, working_luma_weights());
            for n in [-1.0f32, -0.3, 0.0, 0.42, 1.0] {
                assert_eq!(apply_grain(rgba, luma_e, n, 0.0), rgba, "n={n}");
            }
        }
    }

    #[test]
    fn grain_is_monochromatic_the_same_delta_on_every_channel() {
        let rgba = [0.2f32, 0.5, 0.8, 1.0];
        let o = apply_grain(rgba, 0.5, 0.7, 100.0);
        let d0 = o[0] - rgba[0];
        let d1 = o[1] - rgba[1];
        let d2 = o[2] - rgba[2];
        assert!((d0 - d1).abs() < 1e-7 && (d1 - d2).abs() < 1e-7, "{o:?}");
        assert!(d0 > 0.0, "a positive field value must brighten: {o:?}");
        assert_eq!(o[3], 1.0);
    }

    #[test]
    fn grain_fades_out_at_clipped_black_and_clipped_white() {
        assert_eq!(grain_weight(0.0), 0.0);
        assert_eq!(grain_weight(1.0), 0.0);
        assert!(grain_weight(0.5) > 0.99);
        let black = [0.0f32, 0.0, 0.0, 1.0];
        assert_eq!(apply_grain(black, 0.0, 1.0, 100.0), black);
    }

    #[test]
    fn a_bigger_amount_makes_a_bigger_delta() {
        let rgba = [0.4f32, 0.4, 0.4, 1.0];
        let small = apply_grain(rgba, 0.5, 0.8, 20.0)[0] - rgba[0];
        let big = apply_grain(rgba, 0.5, 0.8, 100.0)[0] - rgba[0];
        assert!(big > small && small > 0.0, "small={small} big={big}");
    }

    #[test]
    fn alpha_is_never_touched() {
        let rgba = [0.5f32, 0.5, 0.5, 0.25];
        assert_eq!(apply_grain(rgba, 0.5, 1.0, 100.0)[3], 0.25);
    }
}
