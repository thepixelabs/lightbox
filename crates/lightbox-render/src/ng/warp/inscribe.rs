// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `largest_inscribed_rect`, the constrain-crop helper (spec §3.2; E11 task
//! **E9**). A library helper for the future crop-tool UI (E08); this epic
//! slice does not build UI.
//!
//! # Scope
//!
//! The full spec signature takes an arbitrary convex quadrilateral (the
//! polygon any fully-composed homography warps the source rectangle to).
//! This engine slice's [`super::field::WarpField`] composes only a rotation,
//! so `poly`, [`super::field::WarpField::warped_src_polygon`]'s output, is
//! always the image of an axis-aligned rectangle under a pure rotation about
//! its own center: a **closed-form** solvable case (the well-known "largest
//! axis-aligned rectangle inscribed in a rotated rectangle" construction),
//! not the general iterative/LP solve a fully composed (sheared/perspective)
//! homography would need. See `docs/plan/epics/E11-deviations.md`.
//!
//! Both halfplane formulas below use `|sin(angle)|`/`|cos(angle)|` directly
//! (no angle-reduction case analysis): the axis-aligned rectangle's support
//! in a given outward-normal direction is `hw*|nx| + hh*|ny|` regardless of
//! quadrant, so the absolute values already fold in every rotation symmetry.

use super::field::Vec2;

/// An axis-aligned rectangle, `f64` (spec §3.2 `RectF64`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RectF64 {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl RectF64 {
    /// The 4 corners, TL/TR/BR/BL.
    pub fn corners(&self) -> [Vec2; 4] {
        [
            Vec2::new(self.x, self.y),
            Vec2::new(self.x + self.w, self.y),
            Vec2::new(self.x + self.w, self.y + self.h),
            Vec2::new(self.x, self.y + self.h),
        ]
    }

    /// The rectangle's area (`w * h`; `0` for a degenerate/negative rect).
    pub fn area(&self) -> f64 {
        (self.w.max(0.0)) * (self.h.max(0.0))
    }
}

/// The largest axis-aligned rectangle, centered at `poly`'s centroid,
/// inscribed in the rotated rectangle `poly` describes (spec §3.2
/// `largest_inscribed_rect`; task E9). `aspect` (width/height), when given,
/// is preserved **exactly** in the returned rect (E9 AC); `None` returns the
/// unconstrained maximal-area rect.
pub fn largest_inscribed_rect(poly: &[Vec2; 4], aspect: Option<f64>) -> RectF64 {
    let center = centroid(poly);
    // Recover the rotated rectangle's own (angle, w0, h0) from its edges:
    // poly[0]->poly[1] is one side (length w0, direction = the rotation
    // angle), poly[0]->poly[3] the other (length h0), valid for any `poly`
    // that is truly the affine image of an axis-aligned rectangle (this
    // slice's only producer, `WarpField::warped_src_polygon`).
    let e01 = Vec2::new(poly[1].x - poly[0].x, poly[1].y - poly[0].y);
    let e03 = Vec2::new(poly[3].x - poly[0].x, poly[3].y - poly[0].y);
    let w0 = (e01.x * e01.x + e01.y * e01.y).sqrt();
    let h0 = (e03.x * e03.x + e03.y * e03.y).sqrt();
    let angle = e01.y.atan2(e01.x);

    let (hw, hh) = match aspect {
        None => free_half_extents(w0, h0, angle),
        Some(ar) => locked_half_extents(w0, h0, angle, ar),
    };
    RectF64 {
        x: center.x - hw,
        y: center.y - hh,
        w: 2.0 * hw,
        h: 2.0 * hh,
    }
}

fn centroid(poly: &[Vec2; 4]) -> Vec2 {
    let sx: f64 = poly.iter().map(|p| p.x).sum();
    let sy: f64 = poly.iter().map(|p| p.y).sum();
    Vec2::new(sx / 4.0, sy / 4.0)
}

/// The unconstrained largest-area axis-aligned half-extents `(hw, hh)`
/// centered in a `w0`×`h0` rectangle rotated by `angle_rad`, maximizing
/// `hw*hh` subject to the two halfplane constraints (`hw*cos_a + hh*sin_a <=
/// w0/2`, `hw*sin_a + hh*cos_a <= h0/2`).
///
/// Maximizing a *product* subject to one linear constraint alone (AM-GM) puts
/// the optimum at `hw = w0/(4cos_a), hh = w0/(4sin_a)` (touching only the
/// `w0`-direction pair of sides), feasible w.r.t. the *other* constraint
/// exactly when `w0 <= 2*h0*sin_a*cos_a`. Symmetrically for the `h0`
/// constraint alone. These two feasibility conditions are close to (but not
/// exactly) complementary, for a band of `w0/h0` ratios near 1 (away from a
/// thin rectangle) **neither** single-constraint optimum satisfies the other
/// constraint, and the true optimum is where **both** bind simultaneously
/// (the 2×2 linear-system solve, the classical construction's other branch).
fn free_half_extents(w0: f64, h0: f64, angle_rad: f64) -> (f64, f64) {
    let sin_a = angle_rad.sin().abs();
    let cos_a = angle_rad.cos().abs();
    if sin_a < 1e-9 {
        return (w0 / 2.0, h0 / 2.0);
    }
    let sc = sin_a * cos_a;
    if w0 <= 2.0 * h0 * sc {
        return (w0 / (4.0 * cos_a), w0 / (4.0 * sin_a));
    }
    if h0 <= 2.0 * w0 * sc {
        return (h0 / (4.0 * sin_a), h0 / (4.0 * cos_a));
    }
    let cos_2a = cos_a * cos_a - sin_a * sin_a;
    let wr = (w0 * cos_a - h0 * sin_a) / cos_2a;
    let hr = (h0 * cos_a - w0 * sin_a) / cos_2a;
    (wr.max(0.0) / 2.0, hr.max(0.0) / 2.0)
}

/// The locked-aspect (`ar = width/height`) largest half-extents `(hw, hh)`
/// centered in a `w0`×`h0` rectangle rotated by `angle_rad`. Parametrizes the
/// candidate rect as `(hw, hh) = (t*ar, t)` and solves for the largest `t`
/// satisfying both halfplane constraints, exact by construction (`hw/hh ==
/// ar` always), the E9 AC.
fn locked_half_extents(w0: f64, h0: f64, angle_rad: f64, aspect: f64) -> (f64, f64) {
    let sin_a = angle_rad.sin().abs();
    let cos_a = angle_rad.cos().abs();
    let ar = aspect.max(1e-12);
    let denom1 = ar * cos_a + sin_a;
    let denom2 = ar * sin_a + cos_a;
    let t1 = if denom1 > 1e-12 {
        (w0 / 2.0) / denom1
    } else {
        f64::INFINITY
    };
    let t2 = if denom2 > 1e-12 {
        (h0 / 2.0) / denom2
    } else {
        f64::INFINITY
    };
    let t = t1.min(t2).max(0.0);
    (t * ar, t)
}

/// Strict-enough point-in-convex-quadrilateral test (consistent cross-product
/// sign across all 4 edges, with an `eps` boundary tolerance), an
/// independent verification path from the construction above, used by the
/// property tests.
pub fn point_in_convex_poly(poly: &[Vec2; 4], p: Vec2, eps: f64) -> bool {
    let mut sign = 0i32;
    for i in 0..4 {
        let a = poly[i];
        let b = poly[(i + 1) % 4];
        let edge = Vec2::new(b.x - a.x, b.y - a.y);
        let to_p = Vec2::new(p.x - a.x, p.y - a.y);
        let cross = edge.x * to_p.y - edge.y * to_p.x;
        if cross.abs() < eps {
            continue;
        }
        let s = if cross > 0.0 { 1 } else { -1 };
        if sign == 0 {
            sign = s;
        } else if sign != s {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::types::Extent;
    use crate::ng::warp::field::WarpField;

    fn poly_for(angle_deg: f64, w: u32, h: u32) -> [Vec2; 4] {
        WarpField::from_angle_deg(angle_deg, Extent { w, h }).warped_src_polygon()
    }

    #[test]
    fn zero_angle_gives_the_full_rectangle() {
        // `warped_src_polygon`'s corners sit at pixel-INDEX extremes (0 and
        // w-1/h-1, see `WarpField`'s module docs), so the recovered edge
        // length is `w-1`/`h-1`, not the pixel-count `w`/`h`; a 1px fencepost
        // difference, negligible at any real image size.
        let poly = poly_for(0.0, 100, 50);
        let r = largest_inscribed_rect(&poly, None);
        assert!((r.w - 99.0).abs() < 1e-6, "w={}", r.w);
        assert!((r.h - 49.0).abs() < 1e-6, "h={}", r.h);
    }

    #[test]
    fn square_at_45_degrees_gives_the_known_half_diagonal_square() {
        let side = 100.0f64;
        let poly = poly_for(45.0, side as u32 + 1, side as u32 + 1);
        let r = largest_inscribed_rect(&poly, None);
        let expected = (side + 1.0) / std::f64::consts::SQRT_2;
        assert!(
            (r.w - expected).abs() / expected < 0.02,
            "w={} expected~{}",
            r.w,
            expected
        );
        assert!(
            (r.h - expected).abs() / expected < 0.02,
            "h={} expected~{}",
            r.h,
            expected
        );
    }

    /// **Task E9 AC**: the rect is always inside the warped polygon, over
    /// many random rotations (this slice's only warp shape).
    #[test]
    fn free_rect_is_always_inside_the_warped_polygon() {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut rand = move || {
            // xorshift64*, deterministic and dependency-free.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / ((1u64 << 53) as f64)
        };
        for _ in 0..10_000 {
            let angle = (rand() - 0.5) * 90.0; // -45..45 deg (recipe's own range)
            let w = 30 + (rand() * 900.0) as u32;
            let h = 30 + (rand() * 900.0) as u32;
            let poly = poly_for(angle, w, h);
            let r = largest_inscribed_rect(&poly, None);
            assert!(
                r.w >= -1e-6 && r.h >= -1e-6,
                "degenerate rect at angle={angle} w={w} h={h}: {r:?}"
            );
            for c in r.corners() {
                assert!(
                    point_in_convex_poly(&poly, c, 1e-3),
                    "corner {c:?} outside polygon {poly:?} at angle={angle} w={w} h={h}"
                );
            }
        }
    }

    /// **Task E9 AC**: the locked-aspect variant preserves the ratio exactly.
    #[test]
    fn locked_aspect_rect_preserves_ratio_exactly_and_stays_inside() {
        let mut state: u64 = 0xD1B54A32D192ED03;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / ((1u64 << 53) as f64)
        };
        for _ in 0..2_000 {
            let angle = (rand() - 0.5) * 90.0;
            let w = 40 + (rand() * 800.0) as u32;
            let h = 40 + (rand() * 800.0) as u32;
            let ar = 0.3 + rand() * 3.0; // wide range of target aspect ratios
            let poly = poly_for(angle, w, h);
            let r = largest_inscribed_rect(&poly, Some(ar));
            assert!(r.h > 0.0, "degenerate: {r:?}");
            let got_ar = r.w / r.h;
            assert!(
                (got_ar - ar).abs() < 1e-9,
                "aspect not exact: got {got_ar} want {ar}"
            );
            for c in r.corners() {
                assert!(
                    point_in_convex_poly(&poly, c, 1e-3),
                    "locked-aspect corner {c:?} outside polygon at angle={angle} ar={ar}"
                );
            }
        }
    }

    #[test]
    fn locked_aspect_matches_the_free_rect_area_at_zero_angle_when_aspect_matches() {
        let poly = poly_for(0.0, 200, 100);
        let free = largest_inscribed_rect(&poly, None);
        // Derive the aspect from the free rect itself (not the nominal
        // 200/100 = 2.0 the polygon's *pixel-count* dims would suggest, its
        // recovered edge lengths are off by the same 1px fencepost
        // `zero_angle_gives_the_full_rectangle` documents) so this test
        // isolates "locked aspect == free rect when the locked aspect
        // already equals the free rect's own aspect", independent of that
        // fencepost.
        let ar = free.w / free.h;
        let locked = largest_inscribed_rect(&poly, Some(ar));
        assert!((free.area() - locked.area()).abs() < 1e-6);
    }
}
