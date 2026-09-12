// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `geom.defringe`: lateral chromatic-aberration correction and defringe
//! (`shaders/geom_defringe.wgsl`). Two different operations sharing one pass
//! because both are per-channel and neither may run after a resample.
//!
//! # They are not the same thing
//!
//! **Lateral chromatic aberration** is geometric. A lens focuses red and
//! blue at slightly different magnifications from green, so the three
//! channels are the same picture at three very slightly different scales.
//! Correcting it is a per-channel radial rescale about the frame centre:
//! red and blue move, green stays, no colour is invented.
//!
//! **Defringe** is chromatic. It removes the purple/violet and green
//! fringes that survive alignment (longitudinal CA, sensor crosstalk,
//! demosaic ringing) by pulling the green-magenta opponent channel back
//! toward its local low-frequency level at high-contrast edges. Colour
//! moves, pixels do not.
//!
//! Lightroom keeps these on separate controls and so does this node.
//!
//! # Placement
//!
//! First in the geometry segment, ahead of `geom.lens`, `geom.warp` and
//! `geom.crop` (see [`super::build_geometry_segment`]). A resample smears a
//! colour fringe across neighbouring pixels, which is exactly the signal
//! both halves need to isolate, so both must see the unresampled frame. The
//! radial models are also measured from the captured frame's centre, which
//! a crop would move.
//!
//! # What the CA half does today, honestly
//!
//! The correction maths is complete and runs on both backends, but its two
//! coefficients are per-lens measurements. They come from
//! [`super::lens::resolve_lens_profile`], and **Lightbox ships no
//! lens-profile database**, so today they are zero and
//! [`lightbox_edit::leaves::Optics::ca`] on its own moves nothing. The
//! remaining piece is an automatic estimate of the two scales from the image
//! itself. That is a whole-frame statistic, so it belongs either in a
//! `plan` that demands the entire frame (which would disable ROI tiling for
//! the whole upstream chain whenever the toggle is on) or in a separate
//! analysis pass alongside `sched::histogram`'s. Choosing between those is
//! an engine-seam decision, not a node-local one, and it is not in this
//! change. `Optics::defringe` is unaffected by any of that and works.

use std::sync::Arc;

use lightbox_edit::leaves::Optics;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::geometry::lens::resolve_lens_profile;
use crate::ng::nodes::geometry::warp::sample_lanczos3;
use crate::ng::nodes::geometry::OpticsNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, Roi};
use crate::ng::warp::Vec2;

/// The CA/defringe kernel, naga-validated at build (`build.rs`).
const DEFRINGE_WGSL: &str = include_str!("../../../../shaders/geom_defringe.wgsl");

/// The Lanczos-3 window radius the CA rescale gathers over; must equal
/// `warp.rs`'s own `LANCZOS_A` (private there) because it is that module's
/// [`sample_lanczos3`] doing the gathering.
const LANCZOS_A: f64 = 3.0;

/// Defringe reference-window radius in pixels. Three is the smallest radius
/// whose window still spans a one to three pixel fringe plus clean pixels on
/// both sides of it, which is what makes the window mean a fringe-free
/// reference level. Must equal `geom_defringe.wgsl`'s `DEFRINGE_RADIUS`.
pub const DEFRINGE_RADIUS: i32 = 3;

/// Local-contrast gate: below this, no defringe at all. A UI-shaping
/// choice, not a measured constant. Must equal the shader's `EDGE_LO`.
const EDGE_LO: f32 = 0.15;
/// Local contrast at which the full amount applies. Must equal the shader's
/// `EDGE_HI`.
const EDGE_HI: f32 = 0.50;

/// Resolved lateral-CA scales and defringe amount for an [`Optics`] slice.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct DefringeCoeffs {
    /// Red channel radial scale delta (`0` == no rescale).
    pub ca_scale_r: f64,
    /// Blue channel radial scale delta.
    pub ca_scale_b: f64,
    /// Defringe strength, `0..=1`.
    pub amount: f64,
}

impl DefringeCoeffs {
    /// True iff neither half does anything, the elision predicate.
    pub fn is_neutral(&self) -> bool {
        self.ca_scale_r == 0.0 && self.ca_scale_b == 0.0 && self.amount == 0.0
    }

    /// True iff the CA rescale is active (and so the pass must resample).
    pub fn ca_on(&self) -> bool {
        self.ca_scale_r != 0.0 || self.ca_scale_b != 0.0
    }

    /// True iff the defringe half is active.
    pub fn defringe_on(&self) -> bool {
        self.amount != 0.0
    }
}

/// Resolves an [`Optics`] recipe slice to this node's coefficients.
///
/// The CA scales are per-lens measurements and come only from a resolved
/// lens profile; with no database (see [`resolve_lens_profile`]) they are
/// zero and the `ca` toggle is inert. The defringe amount is the user's own
/// `0..=100` slider and always applies.
pub fn defringe_coeffs(o: &Optics) -> DefringeCoeffs {
    let mut out = DefringeCoeffs {
        amount: (o.defringe as f64 / 100.0).clamp(0.0, 1.0),
        ..DefringeCoeffs::default()
    };
    if o.ca {
        if let Some(profile) = o
            .lens_profile
            .as_ref()
            .and_then(|lp| resolve_lens_profile(&lp.profile_id))
        {
            out.ca_scale_r = profile.ca_scale_r;
            out.ca_scale_b = profile.ca_scale_b;
        }
    }
    out
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "ca_scale_r",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "ca_scale_b",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "amount",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "src_w",
        kind: ParamKind::Int,
    },
    FieldDecl {
        name: "src_h",
        kind: ParamKind::Int,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("geom.defringe"),
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

/// The resolved per-pixel geometry `plan`/`eval_cpu`/`eval_gpu` share.
#[derive(Clone, Copy, PartialEq, Debug)]
struct DefringeGeometry {
    coeffs: DefringeCoeffs,
    /// Frame centre, `((w-1)/2, (h-1)/2)`, the
    /// [`crate::ng::warp::WarpField`] convention `geom.warp` and `geom.lens`
    /// also use.
    center: Vec2,
}

impl DefringeGeometry {
    fn new(coeffs: DefringeCoeffs, extent: Extent) -> DefringeGeometry {
        let w = extent.w.max(1) as f64;
        let h = extent.h.max(1) as f64;
        DefringeGeometry {
            coeffs,
            center: Vec2::new((w - 1.0) / 2.0, (h - 1.0) / 2.0),
        }
    }

    fn from_params(params: &ParamBlock) -> DefringeGeometry {
        let coeffs = DefringeCoeffs {
            ca_scale_r: params.get_f64_or("ca_scale_r", 0.0),
            ca_scale_b: params.get_f64_or("ca_scale_b", 0.0),
            amount: params.get_f64_or("amount", 0.0),
        };
        let extent = Extent {
            w: params.get_i64("src_w").unwrap_or(1).max(1) as u32,
            h: params.get_i64("src_h").unwrap_or(1).max(1) as u32,
        };
        DefringeGeometry::new(coeffs, extent)
    }

    /// The apron `plan` grows the requested ROI by: whichever of the two
    /// halves reaches further. The CA rescale displaces a pixel by
    /// `|s| * |p - centre|`, largest at the ROI corner farthest from the
    /// centre, plus the Lanczos-3 support; defringe reads a fixed
    /// [`DEFRINGE_RADIUS`] box.
    fn apron(&self, out: Roi) -> u32 {
        let mut apron = if self.coeffs.defringe_on() {
            DEFRINGE_RADIUS.max(0) as u32
        } else {
            0
        };
        if self.coeffs.ca_on() && out.w > 0 && out.h > 0 {
            let x0 = out.x as f64;
            let y0 = out.y as f64;
            let x1 = x0 + out.w as f64 - 1.0;
            let y1 = y0 + out.h as f64 - 1.0;
            let dx = (self.center.x - x0).abs().max((x1 - self.center.x).abs());
            let dy = (self.center.y - y0).abs().max((y1 - self.center.y).abs());
            let s = self
                .coeffs
                .ca_scale_r
                .abs()
                .max(self.coeffs.ca_scale_b.abs());
            let shift = (dx * dx + dy * dy).sqrt() * s;
            apron = apron.max((shift + LANCZOS_A).ceil().max(0.0) as u32);
        }
        apron
    }
}

/// `smoothstep(lo, hi, x)`, WGSL's own definition, so the CPU twin gates the
/// defringe amount on exactly the curve the shader does.
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rec. 709 luminance over the scene-linear working RGB this node runs in.
fn luma(p: &[f32; 4]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// The green-magenta opponent channel. Purple/violet fringes drive it
/// negative, green fringes positive, which is the pair of hues defringe
/// targets.
///
/// It is **not** orthogonal to blue-yellow: a pure blue shift lands on the
/// magenta side of this axis and a pure yellow shift on the green side. What
/// keeps a blue sky or a yellow wall safe is not the axis, it is that only
/// the part of this channel that deviates from its own local low-frequency
/// level, at a high-contrast edge, is ever touched. An object's interior has
/// no such deviation.
fn opponent(p: &[f32; 4]) -> f32 {
    p[1] - 0.5 * (p[0] + p[2])
}

/// The `geom.defringe` develop node.
#[derive(Default)]
pub struct GeomDefringeNode {}

impl GeomDefringeNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("geom.defringe");

    /// A fresh node.
    pub fn new() -> GeomDefringeNode {
        GeomDefringeNode::default()
    }
}

impl RenderNode for GeomDefringeNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn plan(&self, out: Roi, _scale: f32, params: &ParamBlock) -> InputRois {
        let g = DefringeGeometry::from_params(params);
        let apron = g.apron(out);
        if apron == 0 {
            return InputRois::identity(out, 1);
        }
        InputRois(vec![out.expand(apron)])
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
            .ok_or_else(|| NodeError::Other("geom.defringe: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("geom.defringe: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let g = DefringeGeometry::from_params(params);
        let mut ubo = [0u8; 48];
        let f32s: [f32; 5] = [
            g.coeffs.ca_scale_r as f32,
            g.coeffs.ca_scale_b as f32,
            g.coeffs.amount as f32,
            g.center.x as f32,
            g.center.y as f32,
        ];
        for (i, v) in f32s.iter().enumerate() {
            ubo[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        ubo[20..24].copy_from_slice(&input.roi.w.to_le_bytes());
        ubo[24..28].copy_from_slice(&input.roi.h.to_le_bytes());
        ubo[28..32].copy_from_slice(&input.roi.x.to_le_bytes());
        ubo[32..36].copy_from_slice(&input.roi.y.to_le_bytes());
        ubo[36..40].copy_from_slice(&u32::from(g.coeffs.ca_on()).to_le_bytes());
        ubo[40..44].copy_from_slice(&u32::from(g.coeffs.defringe_on()).to_le_bytes());

        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom.defringe params"),
            size: ubo.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&buf, 0, &ubo);

        let pipeline = ctx.kernels.compute_pipeline(DEFRINGE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.defringe in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.defringe out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.defringe params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params],
            extent,
            "geom.defringe",
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
            .ok_or_else(|| NodeError::Cpu("geom.defringe: missing input tile".into()))?;
        let g = DefringeGeometry::from_params(params);
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let ca_on = g.coeffs.ca_on();
        let defringe_on = g.coeffs.defringe_on();
        let amount = g.coeffs.amount as f32;
        let max_x = input.extent.w as i64 - 1;
        let max_y = input.extent.h as i64 - 1;

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|ly, row| {
            let ay = out_roi.y + ly as i32;
            let iy = (ay - in_roi.y) as i64;
            for lx in 0..w {
                let ax = out_roi.x + lx as i32;
                let ix = (ax - in_roi.x) as i64;
                let mut p = input.get_rgba_f32(
                    ix.clamp(0, max_x.max(0)) as u32,
                    iy.clamp(0, max_y.max(0)) as u32,
                );

                if ca_on {
                    let dx = ax as f64 - g.center.x;
                    let dy = ay as f64 - g.center.y;
                    let base_x = g.center.x - in_roi.x as f64;
                    let base_y = g.center.y - in_roi.y as f64;
                    let sr = 1.0 + g.coeffs.ca_scale_r;
                    let sb = 1.0 + g.coeffs.ca_scale_b;
                    let red = sample_lanczos3(input, base_x + dx * sr, base_y + dy * sr);
                    let blue = sample_lanczos3(input, base_x + dx * sb, base_y + dy * sb);
                    p[0] = red[0];
                    p[2] = blue[2];
                }

                if defringe_on {
                    let mut cd_sum = 0f32;
                    let mut n = 0f32;
                    let mut y_min = f32::INFINITY;
                    let mut y_max = f32::NEG_INFINITY;
                    for j in -DEFRINGE_RADIUS..=DEFRINGE_RADIUS {
                        let sy = (iy + j as i64).clamp(0, max_y.max(0)) as u32;
                        for i in -DEFRINGE_RADIUS..=DEFRINGE_RADIUS {
                            let sx = (ix + i as i64).clamp(0, max_x.max(0)) as u32;
                            let q = input.get_rgba_f32(sx, sy);
                            cd_sum += opponent(&q);
                            n += 1.0;
                            let y = luma(&q);
                            y_min = y_min.min(y);
                            y_max = y_max.max(y);
                        }
                    }
                    let cd_ref = cd_sum / n.max(1.0);
                    let cd = opponent(&p);
                    let contrast = (y_max - y_min) / (y_max + y_min).max(1e-6);
                    let edge = smoothstep(EDGE_LO, EDGE_HI, contrast);
                    p[1] = (p[1] + amount * edge * (cd_ref - cd)).max(0.0);
                }

                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], p);
            }
        });
        Ok(())
    }
}

impl OpticsNode for GeomDefringeNode {
    fn is_identity(p: &Optics) -> bool {
        defringe_coeffs(p).is_neutral()
    }

    fn param_block(p: &Optics, src_extent: Extent) -> ParamBlock {
        let c = defringe_coeffs(p);
        ParamBlock::from_fields([
            ("ca_scale_r", ParamValue::Float(c.ca_scale_r)),
            ("ca_scale_b", ParamValue::Float(c.ca_scale_b)),
            ("amount", ParamValue::Float(c.amount)),
            ("src_w", ParamValue::Int(src_extent.w as i64)),
            ("src_h", ParamValue::Int(src_extent.h as i64)),
        ])
        .expect("geom.defringe params are always finite (optics amounts clamped on ingest)")
    }
}

/// Factory registering [`GeomDefringeNode`].
#[derive(Default)]
pub struct GeomDefringeFactory {}

impl NodeFactory for GeomDefringeFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GeomDefringeNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(DEFRINGE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use lightbox_edit::leaves::LensCorrection;
    use lightbox_jobs::CancelToken;

    use super::*;
    use crate::ng::tile::PixelFormat;
    use crate::ng::types::TilePrecision;

    fn optics_defringe(amount: f32) -> Optics {
        Optics {
            defringe: amount,
            ..Optics::default()
        }
    }

    /// The `ca` toggle alone resolves to nothing, because the scales are
    /// per-lens measurements and there is no database to measure from. This
    /// is the honest state of the CA half, pinned so it cannot quietly turn
    /// into invented numbers.
    #[test]
    fn ca_toggle_alone_resolves_to_no_scales() {
        let o = Optics {
            ca: true,
            ..Optics::default()
        };
        let c = defringe_coeffs(&o);
        assert_eq!((c.ca_scale_r, c.ca_scale_b), (0.0, 0.0));
        assert!(!c.ca_on());
        assert!(GeomDefringeNode::is_identity(&o), "so the node elides");

        // Even with a lens profile present: the id does not resolve.
        let with_profile = Optics {
            ca: true,
            lens_profile: Some(LensCorrection {
                profile_id: "Sony FE 24mm F1.4 GM".to_owned(),
                ..LensCorrection::default()
            }),
            ..Optics::default()
        };
        assert!(!defringe_coeffs(&with_profile).ca_on());
    }

    #[test]
    fn defringe_amount_maps_to_unit_range_and_gates_elision() {
        assert!(GeomDefringeNode::is_identity(&Optics::default()));
        assert_eq!(defringe_coeffs(&optics_defringe(50.0)).amount, 0.5);
        assert!(!GeomDefringeNode::is_identity(&optics_defringe(1.0)));
    }

    #[test]
    fn plan_grows_by_the_defringe_window_and_nothing_more() {
        let pb = GeomDefringeNode::param_block(&optics_defringe(100.0), Extent { w: 400, h: 300 });
        let out = Roi {
            x: 40,
            y: 40,
            w: 16,
            h: 16,
        };
        let got = GeomDefringeNode::new().plan(out, 1.0, &pb).0[0];
        assert_eq!(got, out.expand(DEFRINGE_RADIUS as u32));
    }

    /// The opponent channel's sign convention, which is what makes one
    /// amount cover both of Lightroom's fringe hues: purple negative, green
    /// positive, neutral exactly zero.
    #[test]
    fn opponent_channel_separates_purple_from_green() {
        let neutral = [0.4f32, 0.4, 0.4, 1.0];
        let purple = [0.6f32, 0.4, 0.6, 1.0];
        let green = [0.4f32, 0.6, 0.4, 1.0];
        assert_eq!(opponent(&neutral), 0.0);
        assert!(opponent(&purple) < 0.0);
        assert!(opponent(&green) > 0.0);
        // The axis is NOT orthogonal to blue-yellow (see `opponent`'s doc):
        // blue lands on the magenta side, yellow on the green side. Pinned
        // so the doc comment and the maths cannot drift apart.
        assert!(opponent(&[0.4, 0.4, 0.8, 1.0]) < 0.0, "blue reads magenta");
        assert!(opponent(&[0.8, 0.8, 0.4, 1.0]) > 0.0, "yellow reads green");
    }

    fn run_cpu(input: PixelBuf, o: &Optics) -> PixelBuf {
        let extent = input.extent;
        let pb = GeomDefringeNode::param_block(o, extent);
        let mut out = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
        let cancel = CancelToken::new();
        let roi = Roi {
            x: 0,
            y: 0,
            w: extent.w,
            h: extent.h,
        };
        let mut ctx = CpuEvalCtx::new(1.0, &cancel, roi, &mut out);
        let view = CpuTileView {
            pixels: &input,
            roi,
            precision: TilePrecision::F32,
        };
        GeomDefringeNode::new()
            .eval_cpu(&mut ctx, &[view], &pb)
            .expect("cpu eval");
        out
    }

    /// **The acceptance criterion for the defringe half**: a synthetic
    /// purple fringe on a high-contrast edge loses most of its purple, and a
    /// large flat purple patch (real object colour, no edge) keeps all of
    /// its.
    #[test]
    fn defringe_kills_an_edge_fringe_and_spares_a_flat_purple_patch() {
        let extent = Extent { w: 64, h: 16 };
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
        for y in 0..extent.h {
            for x in 0..extent.w {
                let p = if x < 32 {
                    // Left half: a hard black-to-white edge at x == 16 with a
                    // two pixel purple fringe sitting on the bright side.
                    if x < 16 {
                        [0.02, 0.02, 0.02, 1.0]
                    } else if x < 18 {
                        [0.9, 0.35, 0.9, 1.0]
                    } else {
                        [0.9, 0.9, 0.9, 1.0]
                    }
                } else {
                    // Right half: a flat purple object, no luminance edge.
                    [0.5, 0.2, 0.5, 1.0]
                };
                px.set_rgba_f32(x, y, p);
            }
        }
        let before_fringe = px.get_rgba_f32(16, 8);
        let before_patch = px.get_rgba_f32(50, 8);

        let out = run_cpu(px, &optics_defringe(100.0));
        let after_fringe = out.get_rgba_f32(16, 8);
        let after_patch = out.get_rgba_f32(50, 8);

        let cast = |p: &[f32; 4]| -opponent(p); // positive == purple
        assert!(
            cast(&after_fringe) < 0.35 * cast(&before_fringe),
            "edge fringe must lose most of its purple: {} -> {}",
            cast(&before_fringe),
            cast(&after_fringe)
        );
        assert!(
            (cast(&after_patch) - cast(&before_patch)).abs() < 1e-4,
            "a flat purple object must be untouched: {} -> {}",
            cast(&before_patch),
            cast(&after_patch)
        );
        // Purple and green are the same axis, so the same run must leave the
        // luminance edge itself in place (defringe moves colour, not pixels).
        assert!((out.get_rgba_f32(10, 8)[0] - 0.02).abs() < 1e-4);
        assert!((out.get_rgba_f32(28, 8)[0] - 0.9).abs() < 1e-4);
    }

    /// A green fringe is the same axis with the opposite sign, so it must be
    /// removed by the same pass with no extra control.
    #[test]
    fn defringe_removes_a_green_fringe_too() {
        let extent = Extent { w: 32, h: 16 };
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
        for y in 0..extent.h {
            for x in 0..extent.w {
                let p = if x < 16 {
                    [0.02, 0.02, 0.02, 1.0]
                } else if x < 18 {
                    [0.5, 0.95, 0.5, 1.0]
                } else {
                    [0.9, 0.9, 0.9, 1.0]
                };
                px.set_rgba_f32(x, y, p);
            }
        }
        let before = opponent(&px.get_rgba_f32(16, 8));
        let out = run_cpu(px, &optics_defringe(100.0));
        let after = opponent(&out.get_rgba_f32(16, 8));
        assert!(before > 0.0);
        assert!(
            after < 0.35 * before,
            "green fringe must lose most of its green: {before} -> {after}"
        );
    }

    /// The claim `opponent`'s doc comment makes: a flat blue field, which
    /// DOES sit on the magenta side of the opponent axis, survives a full
    /// defringe pass untouched, because the protection is the low-frequency
    /// reference and the edge gate rather than the axis.
    #[test]
    fn a_flat_blue_field_survives_full_defringe() {
        let extent = Extent { w: 24, h: 24 };
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
        for y in 0..extent.h {
            for x in 0..extent.w {
                px.set_rgba_f32(x, y, [0.20, 0.35, 0.85, 1.0]);
            }
        }
        let out = run_cpu(px, &optics_defringe(100.0));
        for y in 0..extent.h {
            for x in 0..extent.w {
                let p = out.get_rgba_f32(x, y);
                assert!(
                    (p[1] - 0.35).abs() < 1e-5,
                    "({x},{y}) blue field was desaturated: {p:?}"
                );
            }
        }
    }

    /// Amount zero is elided upstream, but the node must also be a
    /// passthrough if it is ever handed a zero amount directly.
    #[test]
    fn zero_amount_is_a_passthrough() {
        let extent = Extent { w: 8, h: 8 };
        let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
        for y in 0..extent.h {
            for x in 0..extent.w {
                px.set_rgba_f32(x, y, [x as f32 / 8.0, y as f32 / 8.0, 0.25, 1.0]);
            }
        }
        let want = px.clone();
        let out = run_cpu(px, &Optics::default());
        for y in 0..extent.h {
            for x in 0..extent.w {
                assert_eq!(out.get_rgba_f32(x, y), want.get_rgba_f32(x, y));
            }
        }
    }
}
