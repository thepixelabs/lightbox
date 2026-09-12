// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `geom.lens`: radial lens-distortion correction and radial lens-vignetting
//! correction, one resampling pass (`shaders/geom_lens.wgsl`).
//!
//! # There is no lens-profile database in this build
//!
//! Lightroom keys its Optics panel off a shipped database of thousands of
//! measured per-lens profiles, matched on the EXIF lens model.
//! [`lightbox_edit::leaves::LensCorrection::profile_id`] is the field that
//! would name such a profile, and **nothing in this repository populates it
//! and nothing resolves it**: [`resolve_lens_profile`] returns `None` for
//! every id, because inventing coefficients per lens would be fabrication,
//! not correction. The maths below is complete and correct; what is missing
//! is measured input for it, and the honest source of that input in the
//! meantime is the user's own hand on a slider, which is exactly what
//! Lightroom's own Manual tab is for.
//!
//! # How the recipe's fields reach the coefficients
//!
//! Nothing here reinterprets a field. Each of the four inputs means exactly
//! one thing, and [`lens_coeffs`] adds them:
//!
//! | Recipe field | Meaning | Reaches |
//! |---|---|---|
//! | `LensCorrection::distortion` | profile amount, `0..=200`, unity `100` | scales a resolved profile's `k1/k2/k3` |
//! | `LensCorrection::vignetting` | profile amount, `0..=200`, unity `100` | scales a resolved profile's gain terms |
//! | `LensCorrection::manual_distortion` | manual dial, `-100..=100`, neutral `0` | [`MANUAL_DISTORTION_K1_FULL`] |
//! | `Optics::vignette_corr` | manual dial, `-100..=100`, neutral `0` | [`MANUAL_VIGNETTE_FULL`] |
//!
//! The two profile amounts are amounts: with no profile to scale (every id,
//! today, see [`resolve_lens_profile`]) they contribute nothing, exactly as
//! `0..=200`-unity-`100` says they should. The two manual dials are
//! corrections in their own right and always apply. Manual stacks on top of
//! profile, the way Lightroom's Manual tab stacks on its Profile tab.
//!
//! This split is what keeps a written sidecar truthful:
//! `docs/interop/crs-mapping.md` maps `manual_distortion` onto Adobe's
//! `crs:LensManualDistortionAmount` and the profile amount onto
//! `crs:LensProfileDistortionScale`, so a file this build writes says in
//! Lightroom what the user actually asked for.
//!
//! # Placement
//!
//! Before `geom.warp` and before `geom.crop` (see
//! [`super::build_geometry_segment`]). Distortion is radial about the
//! *captured frame's* centre; correcting it after a crop would measure the
//! radius from the crop's centre instead and come out lopsided.
//!
//! # Resampling
//!
//! The same Lanczos-3 + 2x2 anti-ringing gather as `geom.warp`
//! ([`super::warp::sample_lanczos3`], shared, not re-derived) over the same
//! coordinate convention as [`crate::ng::warp::WarpField`] (pixel `(x, y)`
//! at exact continuous `(x, y)`, centre `((w-1)/2, (h-1)/2)`), so composing
//! this pass with the warp pass introduces no half-pixel drift.

use std::sync::Arc;

use lightbox_edit::leaves::Optics;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, InputRois, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock,
    ParamValue, PortDecl, RenderNode,
};
use crate::ng::nodes::geometry::warp::sample_lanczos3;
use crate::ng::nodes::geometry::OpticsNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType, Roi};
use crate::ng::warp::Vec2;

/// The lens-correction kernel, naga-validated at build (`build.rs`).
const LENS_WGSL: &str = include_str!("../../../../shaders/geom_lens.wgsl");

/// The Lanczos-3 window radius, the apron [`GeomLensNode::plan`] grows the
/// back-mapped ROI by. Must equal `warp.rs`'s own `LANCZOS_A` (private
/// there) because both modules gather with the same kernel.
const LANCZOS_A: f64 = 3.0;

/// `k1` at a manual Distortion of +/-100, the full-scale correction the
/// slider's ends reach. At `k1 = -0.25` a destination corner pixel
/// (normalized radius 1) resamples from source radius `0.75`, so the frame's
/// outer quarter-diagonal is pushed out of view: a strong barrel correction,
/// which is what a slider end should be. This is a UI range choice, the
/// only kind of constant this module is entitled to invent.
pub const MANUAL_DISTORTION_K1_FULL: f64 = 0.25;

/// Corner gain change at a manual Vignetting of +/-100. At `0.75` the corner
/// reaches gain `1.75` (about +0.8 stops, brightening a vignetted corner) or
/// `0.25` (about -2 stops, adding vignetting). Also a UI range choice.
pub const MANUAL_VIGNETTE_FULL: f64 = 0.75;

/// How the manual vignetting amount splits between the `r^2` and `r^4`
/// terms. A pure `r^2` correction starts falling off immediately from the
/// centre; real lens falloff is flat across the middle of the frame and
/// steepens outward, so the fourth-order term carries most of the weight.
/// The two fractions sum to one, which is what makes the corner gain change
/// exactly [`MANUAL_VIGNETTE_FULL`] at full slider deflection.
const MANUAL_VIGNETTE_R2_SHARE: f64 = 0.35;
/// The `r^4` share; see [`MANUAL_VIGNETTE_R2_SHARE`].
const MANUAL_VIGNETTE_R4_SHARE: f64 = 0.65;

/// A lens profile's measured correction coefficients, the shape a real
/// profile database would hand back.
///
/// `k` are the radial distortion polynomial's `k1`/`k2`/`k3` in the
/// destination-radius-to-source-radius direction (see the module docs);
/// `vignette` are the radial gain polynomial's `a`/`b`/`c`; `ca_scale_r` and
/// `ca_scale_b` are the per-channel lateral-CA radial scale deltas
/// [`super::defringe::GeomDefringeNode`] consumes.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LensProfileCoeffs {
    /// Radial distortion `k1`, `k2`, `k3`.
    pub k: [f64; 3],
    /// Radial vignetting gain `a`, `b`, `c`.
    pub vignette: [f64; 3],
    /// Red channel lateral-CA radial scale delta.
    pub ca_scale_r: f64,
    /// Blue channel lateral-CA radial scale delta.
    pub ca_scale_b: f64,
}

/// Looks `profile_id` up in the shipped lens-profile database.
///
/// **Lightbox ships no lens-profile database, so this returns `None` for
/// every id, always.** It is not a stub that will quietly start returning
/// plausible-looking numbers: a lens profile is a measurement of one
/// specific lens, and there is nothing here that has measured one. This
/// function is the single seam a real database would be wired into, and
/// until then the honest answer is "no profile", which is what the callers
/// act on (see [`lens_coeffs`] and
/// [`super::defringe::defringe_coeffs`]).
pub fn resolve_lens_profile(profile_id: &str) -> Option<LensProfileCoeffs> {
    let _ = profile_id;
    None
}

/// The radial distortion and vignetting coefficients an [`Optics`] recipe
/// slice resolves to. See the module docs for the profile-versus-manual
/// branch and why the two amounts are read differently.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LensCoeffs {
    /// Radial distortion `k1`, `k2`, `k3`.
    pub k: [f64; 3],
    /// Radial vignetting gain `a`, `b`, `c`.
    pub vignette: [f64; 3],
}

impl LensCoeffs {
    /// True iff neither polynomial does anything, the elision predicate.
    pub fn is_neutral(&self) -> bool {
        self.k.iter().all(|v| *v == 0.0) && self.vignette.iter().all(|v| *v == 0.0)
    }
}

/// The `r^2`/`r^4`/`r^6` vignetting gain terms for a signed manual amount in
/// `-1..=1`, shaped by [`MANUAL_VIGNETTE_R2_SHARE`] /
/// [`MANUAL_VIGNETTE_R4_SHARE`].
fn manual_vignette_terms(amount: f64) -> [f64; 3] {
    let a = amount * MANUAL_VIGNETTE_FULL;
    [
        a * MANUAL_VIGNETTE_R2_SHARE,
        a * MANUAL_VIGNETTE_R4_SHARE,
        0.0,
    ]
}

/// Resolves an [`Optics`] recipe slice to this node's coefficients.
pub fn lens_coeffs(o: &Optics) -> LensCoeffs {
    let mut out = LensCoeffs::default();

    if let Some(lp) = &o.lens_profile {
        // The profile amounts are amounts: they scale a resolved profile's
        // measured coefficients and contribute nothing when none resolves.
        if let Some(profile) = resolve_lens_profile(&lp.profile_id) {
            let d = lp.distortion as f64 / 100.0;
            let v = lp.vignetting as f64 / 100.0;
            for i in 0..3 {
                out.k[i] = profile.k[i] * d;
                out.vignette[i] = profile.vignette[i] * v;
            }
        }
        // The manual dial is a correction in its own right and stacks on
        // top. Positive removes barrel, which means sampling from inside
        // the destination radius, which means a negative k1.
        if lp.manual_distortion != 0.0 {
            out.k[0] -= (lp.manual_distortion as f64 / 100.0) * MANUAL_DISTORTION_K1_FULL;
        }
    }

    // Manual vignetting correction adds on top of whatever a profile
    // contributed, the same way Lightroom's Manual tab stacks on its Profile
    // tab.
    if o.vignette_corr != 0.0 {
        let manual = manual_vignette_terms(o.vignette_corr as f64 / 100.0);
        for (slot, term) in out.vignette.iter_mut().zip(manual) {
            *slot += term;
        }
    }
    out
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "k1",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "k2",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "k3",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "vig_a",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "vig_b",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "vig_c",
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
    id: NodeId("geom.lens"),
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

/// The resolved radial geometry `plan`/`eval_cpu`/`eval_gpu` all derive from,
/// so they can never disagree about the frame centre or the normalization
/// radius.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LensGeometry {
    /// Distortion polynomial coefficients.
    pub k: [f64; 3],
    /// Vignetting gain polynomial coefficients.
    pub vignette: [f64; 3],
    /// Frame centre, `((w-1)/2, (h-1)/2)` (the [`crate::ng::warp::WarpField`]
    /// convention).
    pub center: Vec2,
    /// Reciprocal of half the frame diagonal, so the corner pixels sit at
    /// normalized radius exactly `1`.
    pub inv_r_norm: f64,
}

impl LensGeometry {
    /// Builds the geometry for `coeffs` over a `extent`-sized source frame.
    pub fn new(coeffs: LensCoeffs, extent: Extent) -> LensGeometry {
        let w = extent.w.max(1) as f64;
        let h = extent.h.max(1) as f64;
        let half_diag = 0.5 * ((w - 1.0).powi(2) + (h - 1.0).powi(2)).sqrt();
        LensGeometry {
            k: coeffs.k,
            vignette: coeffs.vignette,
            center: Vec2::new((w - 1.0) / 2.0, (h - 1.0) / 2.0),
            inv_r_norm: if half_diag > 0.0 {
                1.0 / half_diag
            } else {
                0.0
            },
        }
    }

    fn from_params(params: &ParamBlock) -> LensGeometry {
        let coeffs = LensCoeffs {
            k: [
                params.get_f64_or("k1", 0.0),
                params.get_f64_or("k2", 0.0),
                params.get_f64_or("k3", 0.0),
            ],
            vignette: [
                params.get_f64_or("vig_a", 0.0),
                params.get_f64_or("vig_b", 0.0),
                params.get_f64_or("vig_c", 0.0),
            ],
        };
        let extent = Extent {
            w: params.get_i64("src_w").unwrap_or(1).max(1) as u32,
            h: params.get_i64("src_h").unwrap_or(1).max(1) as u32,
        };
        LensGeometry::new(coeffs, extent)
    }

    /// True iff the distortion polynomial is the identity (no resample).
    pub fn distort_on(&self) -> bool {
        self.k.iter().any(|v| *v != 0.0)
    }

    /// True iff the vignetting gain polynomial is the identity (gain 1).
    pub fn vignette_on(&self) -> bool {
        self.vignette.iter().any(|v| *v != 0.0)
    }

    /// `1 + k1*rn^2 + k2*rn^4 + k3*rn^6`, the destination-to-source radial
    /// scale at normalized radius `rn`.
    pub fn radial_scale(&self, rn: f64) -> f64 {
        let u = rn * rn;
        1.0 + u * (self.k[0] + u * (self.k[1] + u * self.k[2]))
    }

    /// The source point a destination point resamples from.
    pub fn map_dst_to_src(&self, p: Vec2) -> Vec2 {
        let dx = p.x - self.center.x;
        let dy = p.y - self.center.y;
        let s = self.radial_scale((dx * dx + dy * dy).sqrt() * self.inv_r_norm);
        Vec2::new(self.center.x + dx * s, self.center.y + dy * s)
    }

    /// The interval `radial_scale` spans over normalized radii `u = rn^2` in
    /// `[u_lo, u_hi]`. `radial_scale` is a cubic in `u`, so its extremes on a
    /// closed interval are attained at the endpoints or at a stationary point
    /// inside it; both are checked, which makes this an exact bound rather
    /// than a sampled guess.
    fn scale_range(&self, u_lo: f64, u_hi: f64) -> (f64, f64) {
        let eval = |u: f64| 1.0 + u * (self.k[0] + u * (self.k[1] + u * self.k[2]));
        let mut lo = eval(u_lo).min(eval(u_hi));
        let mut hi = eval(u_lo).max(eval(u_hi));
        // d/du = k1 + 2*k2*u + 3*k3*u^2.
        let (a, b, c) = (3.0 * self.k[2], 2.0 * self.k[1], self.k[0]);
        let mut consider = |u: f64| {
            if u > u_lo && u < u_hi {
                let v = eval(u);
                lo = lo.min(v);
                hi = hi.max(v);
            }
        };
        if a.abs() > f64::EPSILON {
            let disc = b * b - 4.0 * a * c;
            if disc >= 0.0 {
                let sq = disc.sqrt();
                consider((-b + sq) / (2.0 * a));
                consider((-b - sq) / (2.0 * a));
            }
        } else if b.abs() > f64::EPSILON {
            consider(-c / b);
        }
        (lo, hi)
    }

    /// Conservative source ROI covering everything needed to resample
    /// `dst_roi` with a resampler of half-support `support` pixels.
    ///
    /// The image of the rectangle under `p -> c + (p-c)*f(|p-c|)` lies inside
    /// `{c + v*t : v in rect-c, t in [f_lo, f_hi]}`, and each axis of that
    /// set is the product of two intervals, whose extremes are attained at
    /// endpoint pairs. Taking the four corners against both ends of the
    /// scale range is therefore an exact axis-aligned bound, not an estimate.
    pub fn map_roi_dst_to_src(&self, dst_roi: Roi, support: f64) -> Roi {
        if dst_roi.w == 0 || dst_roi.h == 0 {
            return dst_roi;
        }
        let x0 = dst_roi.x as f64;
        let y0 = dst_roi.y as f64;
        let x1 = x0 + dst_roi.w as f64 - 1.0;
        let y1 = y0 + dst_roi.h as f64 - 1.0;

        // Radius extremes over the rectangle: the nearest point to the centre
        // (zero when the centre is inside) and the farthest corner.
        let dx_near = (x0 - self.center.x).max(0.0).max(self.center.x - x1);
        let dy_near = (y0 - self.center.y).max(0.0).max(self.center.y - y1);
        let r_near = (dx_near * dx_near + dy_near * dy_near).sqrt() * self.inv_r_norm;
        let dx_far = (self.center.x - x0).abs().max((x1 - self.center.x).abs());
        let dy_far = (self.center.y - y0).abs().max((y1 - self.center.y).abs());
        let r_far = (dx_far * dx_far + dy_far * dy_far).sqrt() * self.inv_r_norm;
        let (f_lo, f_hi) = self.scale_range(r_near * r_near, r_far * r_far);

        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for (px, py) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            for t in [f_lo, f_hi] {
                let mx = self.center.x + (px - self.center.x) * t;
                let my = self.center.y + (py - self.center.y) * t;
                min_x = min_x.min(mx);
                max_x = max_x.max(mx);
                min_y = min_y.min(my);
                max_y = max_y.max(my);
            }
        }
        let ix = min_x.floor();
        let iy = min_y.floor();
        Roi {
            x: ix as i32,
            y: iy as i32,
            w: ((max_x.ceil() - ix) as i64 + 1).max(1) as u32,
            h: ((max_y.ceil() - iy) as i64 + 1).max(1) as u32,
        }
        .expand(support.ceil().max(0.0) as u32)
    }

    /// The vignetting gain at source normalized radius `rs`, floored at zero
    /// so a wild negative coefficient can never produce negative light.
    pub fn vignette_gain(&self, rs: f64) -> f64 {
        let u = rs * rs;
        (1.0 + u * (self.vignette[0] + u * (self.vignette[1] + u * self.vignette[2]))).max(0.0)
    }
}

/// The `geom.lens` develop node: radial distortion plus radial lens
/// vignetting correction.
#[derive(Default)]
pub struct GeomLensNode {}

impl GeomLensNode {
    /// The node's registry identity.
    pub const ID: NodeId = NodeId("geom.lens");

    /// A fresh node.
    pub fn new() -> GeomLensNode {
        GeomLensNode::default()
    }
}

impl RenderNode for GeomLensNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    /// ROI back-propagation through the radial distortion, grown by the
    /// Lanczos-3 support. Identity when only the vignetting half is active:
    /// a radial gain is pointwise and needs no apron at all.
    fn plan(&self, out: Roi, _scale: f32, params: &ParamBlock) -> InputRois {
        let g = LensGeometry::from_params(params);
        if !g.distort_on() {
            return InputRois::identity(out, 1);
        }
        InputRois(vec![g.map_roi_dst_to_src(out, LANCZOS_A)])
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
            .ok_or_else(|| NodeError::Other("geom.lens: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("geom.lens: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let g = LensGeometry::from_params(params);
        let mut ubo = [0u8; 64];
        let f32s: [f32; 9] = [
            g.k[0] as f32,
            g.k[1] as f32,
            g.k[2] as f32,
            g.vignette[0] as f32,
            g.vignette[1] as f32,
            g.vignette[2] as f32,
            g.center.x as f32,
            g.center.y as f32,
            g.inv_r_norm as f32,
        ];
        for (i, v) in f32s.iter().enumerate() {
            ubo[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        ubo[36..40].copy_from_slice(&input.roi.w.to_le_bytes());
        ubo[40..44].copy_from_slice(&input.roi.h.to_le_bytes());
        // The absolute-to-local offset between the output tile's frame and
        // the (possibly apron-grown) input tile's frame, mirroring
        // `geom.warp`'s own `in_off_*` convention.
        ubo[44..48].copy_from_slice(&input.roi.x.to_le_bytes());
        ubo[48..52].copy_from_slice(&input.roi.y.to_le_bytes());
        ubo[52..56].copy_from_slice(&u32::from(g.distort_on()).to_le_bytes());
        ubo[56..60].copy_from_slice(&u32::from(g.vignette_on()).to_le_bytes());

        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom.lens params"),
            size: ubo.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&buf, 0, &ubo);

        let pipeline = ctx.kernels.compute_pipeline(LENS_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.lens in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.lens out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom.lens params"),
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
            "geom.lens",
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
            .ok_or_else(|| NodeError::Cpu("geom.lens: missing input tile".into()))?;
        let g = LensGeometry::from_params(params);
        let input = input_view.pixels;
        let in_roi = input_view.roi;
        let out_roi = ctx.out_roi;
        let distort = g.distort_on();
        let vignette = g.vignette_on();

        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|ly, row| {
            let ay = out_roi.y + ly as i32;
            for lx in 0..w {
                let ax = out_roi.x + lx as i32;
                let dx = ax as f64 - g.center.x;
                let dy = ay as f64 - g.center.y;
                let rn = (dx * dx + dy * dy).sqrt() * g.inv_r_norm;
                let scale = if distort { g.radial_scale(rn) } else { 1.0 };
                let fx = g.center.x + dx * scale - in_roi.x as f64;
                let fy = g.center.y + dy * scale - in_roi.y as f64;
                let mut p = if distort {
                    sample_lanczos3(input, fx, fy)
                } else {
                    // Distortion off: `fx`/`fy` are exactly the input-local
                    // integer coordinates, so the clamped fetch is the
                    // bit-exact answer (and the GPU twin takes the same
                    // branch).
                    let ix = (fx as i64).clamp(0, input.extent.w as i64 - 1) as u32;
                    let iy = (fy as i64).clamp(0, input.extent.h as i64 - 1) as u32;
                    input.get_rgba_f32(ix, iy)
                };
                if vignette {
                    let gain = g.vignette_gain(rn * scale) as f32;
                    p[0] *= gain;
                    p[1] *= gain;
                    p[2] *= gain;
                }
                PixelBuf::encode_pixel(fmt, &mut row[lx as usize * bpp..], p);
            }
        });
        Ok(())
    }
}

impl OpticsNode for GeomLensNode {
    fn is_identity(p: &Optics) -> bool {
        lens_coeffs(p).is_neutral()
    }

    fn param_block(p: &Optics, src_extent: Extent) -> ParamBlock {
        let c = lens_coeffs(p);
        ParamBlock::from_fields([
            ("k1", ParamValue::Float(c.k[0])),
            ("k2", ParamValue::Float(c.k[1])),
            ("k3", ParamValue::Float(c.k[2])),
            ("vig_a", ParamValue::Float(c.vignette[0])),
            ("vig_b", ParamValue::Float(c.vignette[1])),
            ("vig_c", ParamValue::Float(c.vignette[2])),
            ("src_w", ParamValue::Int(src_extent.w as i64)),
            ("src_h", ParamValue::Int(src_extent.h as i64)),
        ])
        .expect("geom.lens params are always finite (optics amounts clamped on ingest)")
    }
}

/// Factory registering [`GeomLensNode`].
#[derive(Default)]
pub struct GeomLensFactory {}

impl NodeFactory for GeomLensFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(GeomLensNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(LENS_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use lightbox_edit::leaves::LensCorrection;

    use super::*;

    /// `manual_distortion` on the recipe's own signed `-100..=100` scale.
    fn optics_with_distortion(manual_distortion: f32) -> Optics {
        Optics {
            lens_profile: Some(LensCorrection {
                manual_distortion,
                ..LensCorrection::default()
            }),
            ..Optics::default()
        }
    }

    /// The database is empty and says so. This is the test that fails the
    /// day somebody wires in coefficients that were not measured.
    #[test]
    fn no_lens_profile_resolves_because_there_is_no_database() {
        for id in ["", "Canon RF 24-70mm F2.8 L IS USM", "anything at all"] {
            assert_eq!(
                resolve_lens_profile(id),
                None,
                "{id} must not resolve: Lightbox ships no lens-profile database"
            );
        }
    }

    /// A freshly enabled lens correction is `LensCorrection::default()`:
    /// profile amounts at unity `100`, manual dial at neutral `0`. Nothing
    /// happens until the user moves something.
    #[test]
    fn a_default_lens_correction_is_neutral_and_elides_the_node() {
        let o = optics_with_distortion(0.0);
        assert!(lens_coeffs(&o).is_neutral());
        assert!(GeomLensNode::is_identity(&o));
        assert!(GeomLensNode::is_identity(&Optics::default()));
    }

    /// **The reason the schema grew a field.** The profile amounts are
    /// amounts: with no profile to scale they contribute nothing, at ANY
    /// value including the extremes. Only the manual dial corrects. If this
    /// test ever fails, the two meanings have been folded back together and
    /// the sidecar this build writes has started lying.
    #[test]
    fn profile_amounts_alone_never_produce_a_correction() {
        for amount in [0.0f32, 50.0, 100.0, 200.0] {
            let o = Optics {
                lens_profile: Some(LensCorrection {
                    profile_id: "Canon RF 15-35mm F2.8 L IS USM".to_owned(),
                    distortion: amount,
                    vignetting: amount,
                    manual_distortion: 0.0,
                }),
                ..Optics::default()
            };
            assert!(
                lens_coeffs(&o).is_neutral(),
                "profile amount {amount} has no profile to scale, so it must \
                 correct nothing"
            );
        }
    }

    /// Positive corrects barrel (pushes content outward, so the destination
    /// samples from a SMALLER source radius, `k1 < 0`); negative corrects
    /// pincushion. Signs are the load-bearing half of a distortion model,
    /// so they are pinned here rather than assumed.
    #[test]
    fn distortion_sign_matches_the_barrel_versus_pincushion_convention() {
        let barrel = lens_coeffs(&optics_with_distortion(100.0));
        assert_eq!(barrel.k[0], -MANUAL_DISTORTION_K1_FULL);
        let pincushion = lens_coeffs(&optics_with_distortion(-100.0));
        assert_eq!(pincushion.k[0], MANUAL_DISTORTION_K1_FULL);

        let g = LensGeometry::new(barrel, Extent { w: 101, h: 101 });
        // The corner pixel of a 101x101 frame sits at normalized radius 1.
        assert!((g.radial_scale(1.0) - 0.75).abs() < 1e-12);
        let corner = g.map_dst_to_src(Vec2::new(0.0, 0.0));
        assert!(
            corner.x > 0.0 && corner.y > 0.0,
            "correcting barrel must resample the destination corner from \
             inside the source frame, got {corner:?}"
        );
    }

    /// `LensCorrection::vignetting` is a profile amount and stays inert with
    /// no profile to scale; `Optics::vignette_corr` is the manual control
    /// and is the one that moves the gain.
    #[test]
    fn manual_vignetting_comes_from_vignette_corr_not_the_profile_amount() {
        let profile_only = Optics {
            lens_profile: Some(LensCorrection {
                vignetting: 200.0,
                ..LensCorrection::default()
            }),
            ..Optics::default()
        };
        assert_eq!(lens_coeffs(&profile_only).vignette, [0.0; 3]);

        let manual = Optics {
            vignette_corr: 100.0,
            ..Optics::default()
        };
        let c = lens_coeffs(&manual);
        assert!(!c.is_neutral());
        let g = LensGeometry::new(c, Extent { w: 101, h: 101 });
        // Full positive deflection brightens the corner by exactly
        // MANUAL_VIGNETTE_FULL and leaves the centre untouched.
        assert!((g.vignette_gain(1.0) - (1.0 + MANUAL_VIGNETTE_FULL)).abs() < 1e-12);
        assert!((g.vignette_gain(0.0) - 1.0).abs() < 1e-12);
        // Flat across the middle of the frame: less than a fifth of the
        // correction is spent by half radius.
        assert!(g.vignette_gain(0.5) - 1.0 < 0.2 * MANUAL_VIGNETTE_FULL);
    }

    #[test]
    fn negative_vignette_gain_is_floored_at_zero() {
        let g = LensGeometry::new(
            LensCoeffs {
                k: [0.0; 3],
                vignette: [-10.0, 0.0, 0.0],
            },
            Extent { w: 101, h: 101 },
        );
        assert_eq!(g.vignette_gain(1.0), 0.0);
    }

    /// The frame centre and the normalization radius follow
    /// `WarpField::from_angle_deg`'s convention exactly (`(w-1)/2`, corner at
    /// normalized radius 1). A mismatch here is the half-pixel drift that
    /// only ever shows up as softness once this pass composes with
    /// `geom.warp`.
    #[test]
    fn frame_centre_matches_the_warp_field_convention() {
        let extent = Extent { w: 640, h: 480 };
        let g = LensGeometry::new(LensCoeffs::default(), extent);
        let w = crate::ng::warp::WarpField::from_angle_deg(0.0, extent);
        assert_eq!(g.center, w.center());
        // Corner at normalized radius exactly 1.
        let d = Vec2::new(0.0 - g.center.x, 0.0 - g.center.y);
        let rn = (d.x * d.x + d.y * d.y).sqrt() * g.inv_r_norm;
        assert!((rn - 1.0).abs() < 1e-12, "corner normalized radius {rn}");
    }

    /// An identity distortion resamples nothing, so `plan` must not grow the
    /// ROI at all when only the vignetting half is on.
    #[test]
    fn plan_is_identity_when_only_vignetting_is_active() {
        let o = Optics {
            vignette_corr: 50.0,
            ..Optics::default()
        };
        let pb = GeomLensNode::param_block(&o, Extent { w: 400, h: 300 });
        let out = Roi {
            x: 10,
            y: 20,
            w: 32,
            h: 32,
        };
        assert_eq!(GeomLensNode::new().plan(out, 1.0, &pb).0[0], out);
    }

    /// `plan`'s bound must actually contain every source point the eval loop
    /// reaches, apron included. Checked by brute force against the real
    /// mapping over every pixel of a requested ROI.
    #[test]
    fn plan_bound_contains_every_source_point_the_eval_touches() {
        let extent = Extent { w: 400, h: 300 };
        for amount in [-100.0f32, -40.0, 0.0, 60.0, 100.0] {
            let o = optics_with_distortion(amount);
            let pb = GeomLensNode::param_block(&o, extent);
            let g = LensGeometry::from_params(&pb);
            if !g.distort_on() {
                continue;
            }
            for out in [
                Roi {
                    x: 0,
                    y: 0,
                    w: 16,
                    h: 16,
                },
                Roi {
                    x: 190,
                    y: 140,
                    w: 20,
                    h: 20,
                },
                Roi {
                    x: 380,
                    y: 280,
                    w: 20,
                    h: 20,
                },
                Roi {
                    x: 0,
                    y: 0,
                    w: 400,
                    h: 300,
                },
            ] {
                let got = GeomLensNode::new().plan(out, 1.0, &pb).0[0];
                let (gx1, gy1) = (got.x + got.w as i32 - 1, got.y + got.h as i32 - 1);
                for dy in 0..out.h {
                    for dx in 0..out.w {
                        let p = Vec2::new((out.x + dx as i32) as f64, (out.y + dy as i32) as f64);
                        let s = g.map_dst_to_src(p);
                        // The gather reaches floor(f)-2 .. floor(f)+3.
                        let lo_x = s.x.floor() as i32 - 2;
                        let hi_x = s.x.floor() as i32 + 3;
                        let lo_y = s.y.floor() as i32 - 2;
                        let hi_y = s.y.floor() as i32 + 3;
                        assert!(
                            lo_x >= got.x && hi_x <= gx1 && lo_y >= got.y && hi_y <= gy1,
                            "amount {amount}: taps [{lo_x}..{hi_x}]x[{lo_y}..{hi_y}] \
                             escape planned ROI {got:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn param_block_round_trips_the_resolved_coefficients() {
        let o = optics_with_distortion(50.0);
        let pb = GeomLensNode::param_block(&o, Extent { w: 640, h: 480 });
        assert_eq!(pb.get_f64("k1"), Some(-0.5 * MANUAL_DISTORTION_K1_FULL));
        assert_eq!(pb.get_f64("k2"), Some(0.0));
        assert_eq!(pb.get_i64("src_w"), Some(640));
        assert_eq!(pb.get_i64("src_h"), Some(480));
    }
}
