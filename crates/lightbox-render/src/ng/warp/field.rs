// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `WarpField`, the composed geometric transform the `geom.warp`/`geom.crop`
//! nodes consume (spec §3.2 `WarpField`; E11 Phase-A seam + Phase-E tasks
//! E1-E4).
//!
//! # Scope of this engine slice
//!
//! The full architecture composes `WarpField` from lens geometry, TCA, an
//! upright homography, manual transform sliders, *and* the straighten angle
//! (`WarpStage::{OpcodeWarp, LensGeom, Tca, Homography, ManualDistortion}`,
//! spec §3.2). This slice builds only the **geometry ENGINE seams** (tasks
//! E1-E4/E9), optics (Phase D), upright/transform sliders (E5-E8), and
//! effects (E10-E12) are separate, later passes (see
//! `docs/plan/epics/E11-deviations.md`). `WarpField` here therefore composes
//! **one stage: the recipe's straighten `angle`**, a pure rotation about the
//! source image's center. `geom.crop` folds the normalized crop rect on top
//! as a *separate* canvas-extent change (spec §5: "`geom.crop` changes canvas
//! extent only"), not a further warp stage.
//!
//! # Coordinate convention
//!
//! Pixel `(x, y)` sits at the *exact* continuous coordinate `(x, y)` (not
//! `x+0.5`), and the rotation center is the image's exact geometric center
//! `((w-1)/2, (h-1)/2)`. This differs from `util.resize`'s `+0.5`
//! pixel-center decimation convention **on purpose**: it makes a 90° rotation
//! of a square image an **exact** pixel permutation (every mapped coordinate
//! lands on another integer pixel index, no residue beyond `sin`/`cos`
//! rounding), the property [`tests::four_quarter_turns_compose_to_identity`]
//! (task E1's AC) needs.

use crate::ng::types::{Extent, Roi};

/// A 2-D point/vector, `f64`. A local minimal type so this module (and
/// [`super::inscribe`]) need no new crate dependency for vector math.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vec2 {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
}

impl Vec2 {
    /// A point at `(x, y)`.
    pub const fn new(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }
}

/// The composed geometric warp (reduced scope: straighten-angle rotation
/// only, see module docs). Maps between the source canvas and the
/// post-warp (destination) canvas, which share the same extent in this
/// slice (a pure rotation never changes canvas size; `geom.crop` is the
/// separate, later extent change).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WarpField {
    /// Rotation angle, radians. Content rotates by `+angle_rad` (src → dst);
    /// [`WarpField::map_dst_to_src`] applies the inverse.
    angle_rad: f64,
    /// Rotation center (source-pixel coordinates, this module's convention).
    center: Vec2,
    /// Source (== destination, in this slice) extent.
    extent: Extent,
}

impl WarpField {
    /// Builds the warp for `angle_deg` degrees of straighten over `extent`
    /// (spec §3.2 `WarpField::build`, reduced to this slice's one stage).
    pub fn from_angle_deg(angle_deg: f64, extent: Extent) -> WarpField {
        let center = Vec2::new(
            (extent.w.max(1) - 1) as f64 / 2.0,
            (extent.h.max(1) - 1) as f64 / 2.0,
        );
        WarpField {
            angle_rad: angle_deg.to_radians(),
            center,
            extent,
        }
    }

    /// True iff this warp is the identity (no rotation), the `geom.warp`
    /// node elides itself on this (task E1 AC: "identity warp is ...
    /// elided").
    pub fn is_identity(&self) -> bool {
        self.angle_rad == 0.0
    }

    /// The (unchanged, in this slice) destination extent.
    pub fn dst_extent(&self) -> Extent {
        self.extent
    }

    /// The rotation center (source-pixel coordinates).
    pub fn center(&self) -> Vec2 {
        self.center
    }

    /// Maps a destination-canvas point to the source point to resample
    /// (spec §3.2 `map_dst_to_src`; exact, `f64`).
    pub fn map_dst_to_src(&self, p: Vec2) -> Vec2 {
        rotate_about(p, self.center, -self.angle_rad)
    }

    /// Maps a source-canvas point to where it lands in the destination
    /// canvas (the forward direction; used by
    /// [`WarpField::warped_src_polygon`]).
    pub fn map_src_to_dst(&self, p: Vec2) -> Vec2 {
        rotate_about(p, self.center, self.angle_rad)
    }

    /// Conservative axis-aligned **source** ROI covering everything needed to
    /// resample `dst_roi` with a resampler of half-support `support` pixels
    /// (spec §3.2 `map_roi_dst_to_src`; the E3 `plan`/ROI back-mapping seam)
    /// back-maps `dst_roi`'s 4 corner pixels, bounds them, then grows by
    /// `support` (the Lanczos-3 kernel radius). *Not* clamped to the image
    /// here, the engine's ROI planner (`ng::exec::roi::plan_rois`) clamps
    /// every producer's requirement to that producer's own extent.
    pub fn map_roi_dst_to_src(&self, dst_roi: Roi, support: f64) -> Roi {
        if dst_roi.w == 0 || dst_roi.h == 0 {
            return dst_roi;
        }
        let x0 = dst_roi.x as f64;
        let y0 = dst_roi.y as f64;
        let x1 = x0 + dst_roi.w as f64 - 1.0;
        let y1 = y0 + dst_roi.h as f64 - 1.0;
        let corners = [
            Vec2::new(x0, y0),
            Vec2::new(x1, y0),
            Vec2::new(x0, y1),
            Vec2::new(x1, y1),
        ];
        let mapped: Vec<Vec2> = corners.iter().map(|&p| self.map_dst_to_src(p)).collect();
        bounding_roi(&mapped).expand(support.ceil().max(0.0) as u32)
    }

    /// The forward-warped image of the source rectangle's 4 corners, in
    /// destination-canvas coordinates (spec §3.2 `warped_src_polygon`), the
    /// "valid pixel" polygon [`super::inscribe::largest_inscribed_rect`]
    /// inscribes into (task E9's constrain-crop seam). Order: TL, TR, BR, BL.
    pub fn warped_src_polygon(&self) -> [Vec2; 4] {
        let (w, h) = (self.extent.w as f64, self.extent.h as f64);
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(w - 1.0, 0.0),
            Vec2::new(w - 1.0, h - 1.0),
            Vec2::new(0.0, h - 1.0),
        ];
        let mut out = [Vec2::default(); 4];
        for (i, &c) in corners.iter().enumerate() {
            out[i] = self.map_src_to_dst(c);
        }
        out
    }

    /// The thin analytic GPU-uniform payload (spec §3.2 `to_gpu_uniform`,
    /// adapted to this slice, see the module docs and
    /// `docs/plan/epics/E11-deviations.md`): `sin`/`cos` of the rotation and
    /// the rotation center. Because this slice's warp is a single closed-form
    /// rotation (no lens distortion needing a per-tile sampled grid), the
    /// WGSL kernel evaluates `map_dst_to_src` exactly, per-pixel, from these
    /// four floats rather than interpolating a coordinate grid.
    pub fn to_gpu_uniform(&self) -> WarpGpuUniform {
        let (sin_a, cos_a) = self.angle_rad.sin_cos();
        WarpGpuUniform {
            sin_a: sin_a as f32,
            cos_a: cos_a as f32,
            center_x: self.center.x as f32,
            center_y: self.center.y as f32,
        }
    }
}

/// The GPU-side warp uniform ([`WarpField::to_gpu_uniform`]).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WarpGpuUniform {
    /// `sin(angle_rad)`.
    pub sin_a: f32,
    /// `cos(angle_rad)`.
    pub cos_a: f32,
    /// Rotation center X.
    pub center_x: f32,
    /// Rotation center Y.
    pub center_y: f32,
}

/// Rotates `p` about `c` by `theta` radians. The sign convention is internal
/// to this module and consistent between `map_dst_to_src`/`map_src_to_dst`
/// and `geom_warp.wgsl`'s mirrored formula.
fn rotate_about(p: Vec2, c: Vec2, theta: f64) -> Vec2 {
    let (s, co) = theta.sin_cos();
    let dx = p.x - c.x;
    let dy = p.y - c.y;
    Vec2::new(c.x + dx * co - dy * s, c.y + dx * s + dy * co)
}

/// The integer-pixel bounding [`Roi`] covering every point in `pts` (floor of
/// the min, ceil of the max, expressed as a half-open pixel-count `Roi`).
fn bounding_roi(pts: &[Vec2]) -> Roi {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in pts {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let x0 = min_x.floor() as i64;
    let y0 = min_y.floor() as i64;
    let x1 = max_x.ceil() as i64 + 1; // +1: inclusive max corner -> exclusive bound
    let y1 = max_y.ceil() as i64 + 1;
    Roi {
        x: x0.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        y: y0.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(w: u32, h: u32) -> Extent {
        Extent { w, h }
    }

    #[test]
    fn zero_angle_is_identity() {
        let w = WarpField::from_angle_deg(0.0, ext(64, 64));
        assert!(w.is_identity());
        let nz = WarpField::from_angle_deg(1e-3, ext(64, 64));
        assert!(!nz.is_identity());
    }

    #[test]
    fn identity_warp_maps_every_point_to_itself() {
        let w = WarpField::from_angle_deg(0.0, ext(80, 40));
        for p in [
            Vec2::new(0.0, 0.0),
            Vec2::new(79.0, 39.0),
            Vec2::new(12.5, 3.25),
        ] {
            let s = w.map_dst_to_src(p);
            assert!(
                (s.x - p.x).abs() < 1e-12 && (s.y - p.y).abs() < 1e-12,
                "{p:?} -> {s:?}"
            );
        }
    }

    /// Task E1 AC: composing four 90° warps returns (very nearly) to the
    /// identity mapping, the resample-composition property, checked at the
    /// point-mapping level (the node-level pixel-resample composition is
    /// proven in `nodes::geometry::warp`'s own tests / `tests/e11_geom.rs`).
    #[test]
    fn four_quarter_turns_compose_to_identity() {
        let extent = ext(65, 65); // odd so the center is an exact integer pixel
        let quarter = WarpField::from_angle_deg(90.0, extent);
        let mut p = Vec2::new(17.0, 40.0);
        let start = p;
        for _ in 0..4 {
            p = quarter.map_src_to_dst(p);
        }
        assert!(
            (p.x - start.x).abs() < 1e-4 && (p.y - start.y).abs() < 1e-4,
            "4x90 deg composition drifted: {start:?} -> {p:?}"
        );
    }

    /// A 90° rotation of a *square* image maps every integer pixel index to
    /// another exact integer pixel index (the property that makes the
    /// `geom.warp` CPU resample of a 90° turn artifact-free / bit-exact
    /// per-tap, task E1's "4×90° composes to identity" precondition).
    #[test]
    fn quarter_turn_of_square_image_is_exact_pixel_permutation() {
        let w = WarpField::from_angle_deg(90.0, ext(32, 32));
        for y in 0..32u32 {
            for x in 0..32u32 {
                let d = w.map_src_to_dst(Vec2::new(x as f64, y as f64));
                assert!(
                    (d.x - d.x.round()).abs() < 1e-9 && (d.y - d.y.round()).abs() < 1e-9,
                    "({x},{y}) -> {d:?} is not an exact pixel"
                );
            }
        }
    }

    #[test]
    fn map_roi_dst_to_src_covers_the_backmapped_corners_plus_support() {
        let extent = ext(100, 100);
        let w = WarpField::from_angle_deg(15.0, extent);
        let dst_roi = Roi {
            x: 40,
            y: 40,
            w: 10,
            h: 10,
        };
        let src_roi = w.map_roi_dst_to_src(dst_roi, 3.0);
        // Sample many points inside dst_roi; every back-mapped (and
        // support-grown) point must fall inside src_roi.
        for i in 0..=10 {
            for j in 0..=10 {
                let p = Vec2::new(dst_roi.x as f64 + i as f64, dst_roi.y as f64 + j as f64);
                let s = w.map_dst_to_src(p);
                assert!(
                    s.x >= src_roi.x as f64 - 3.01
                        && s.x <= (src_roi.x + src_roi.w as i32) as f64 + 3.01
                        && s.y >= src_roi.y as f64 - 3.01
                        && s.y <= (src_roi.y + src_roi.h as i32) as f64 + 3.01,
                    "backmapped {s:?} not covered by src_roi {src_roi:?}"
                );
                // Actually assert the tight (non-slack) containment: the
                // support growth is already folded into src_roi, so the
                // mapped point itself (before any extra slack) must lie
                // within [src_roi.x, src_roi.x+src_roi.w) inclusive-ish.
                assert!(s.x >= (src_roi.x as f64) - 1e-6);
                assert!(s.x <= (src_roi.x + src_roi.w as i32) as f64 + 1e-6);
                assert!(s.y >= (src_roi.y as f64) - 1e-6);
                assert!(s.y <= (src_roi.y + src_roi.h as i32) as f64 + 1e-6);
            }
        }
    }

    #[test]
    fn warped_src_polygon_corners_match_forward_map() {
        let extent = ext(50, 30);
        let w = WarpField::from_angle_deg(10.0, extent);
        let poly = w.warped_src_polygon();
        let expected_tl = w.map_src_to_dst(Vec2::new(0.0, 0.0));
        assert!((poly[0].x - expected_tl.x).abs() < 1e-9);
        assert!((poly[0].y - expected_tl.y).abs() < 1e-9);
    }

    #[test]
    fn to_gpu_uniform_matches_the_cpu_rotation_formula() {
        let w = WarpField::from_angle_deg(23.0, ext(40, 40));
        let u = w.to_gpu_uniform();
        let expected = (23f64.to_radians()).sin_cos();
        assert!((u.sin_a as f64 - expected.0).abs() < 1e-6);
        assert!((u.cos_a as f64 - expected.1).abs() < 1e-6);
        let c = w.center();
        assert!((u.center_x as f64 - c.x).abs() < 1e-4);
        assert!((u.center_y as f64 - c.y).abs() < 1e-4);
    }
}
