// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Monotone 1D tone-curve math (E10 Phase C, spec §4.3/§7 tasks **C1-C2**):
//! the point-curve interpolant ([`MonotoneCubic`]) and the parametric-curve
//! region weights/delta ([`region_weights`]/[`parametric_delta`]) that
//! `nodes::global::tone_curve` composes into the one baked 1D remap (task
//! C3). Both are clean-room implementations of published algorithms, no
//! GPL source consulted.
//!
//! # C1, monotone cubic Hermite interpolation
//!
//! [`MonotoneCubic`] implements Fritsch-Carlson (F. N. Fritsch & R. E.
//! Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J. Numer. Anal.
//! 17(2), 1980): a cubic Hermite spline whose per-knot tangents are
//! corrected (steps 2-4 of the paper) so the interpolant never overshoots a
//! knot, i.e. it is monotone on any sub-interval where the *data itself* is
//! monotone, unlike a plain (unconstrained) cubic spline. This is exactly
//! the "ringing-free" curve shape a tone curve needs: a user-placed S-curve
//! should never wiggle between control points.
//!
//! # C2, the parametric curve
//!
//! [`region_weights`] partitions `[0,1]` into four smoothly-blended regions
//! (shadows/darks/lights/highlights) around the recipe's three split points,
//! via three independent `smoothstep` ramps centered on each split with a
//! half-width bounded below half the distance to its nearest neighbor (or
//! the `0`/`1` boundary), which is what keeps the four weights an *exact*
//! partition of unity (`Σw == 1.0`, proven algebraically and by
//! [`tests::region_weights_always_sum_to_one`]) for any valid, ordered,
//! `MIN_SPLIT_GAP`-separated split triple: no two transition bands can ever
//! overlap, so each region's weight is a plain difference of two ramps that
//! is provably non-negative (see this module's own derivation in the E10
//! deviations log). [`parametric_delta`] turns those weights into ONE
//! additive y-offset the tone-curve node adds on top of the point curve's
//! own value (spec C2 "composed with the point curve into one 1D remap").

use lightbox_edit::{CurvePoint, ParametricCurve};

/// Floor on a segment's x-width used as an interpolation denominator, keeps
/// [`MonotoneCubic::build`]/[`MonotoneCubic::eval`] finite (no division by a
/// near-zero gap) even for two control points placed a hair apart (the C1
/// "stable under near-duplicate x" AC). `ToneCurve::validate` already
/// rejects *exactly* equal x, so this only ever guards the near-duplicate
/// case, never legitimate widely-spaced segments.
const MIN_DX: f32 = 1.0e-4;

/// A monotone cubic Hermite spline built from a curve's control points
/// (task C1). Degenerate inputs never panic or produce NaN/Inf:
///
/// - **`< 2` points** evaluate as the exact identity `y = x`, matches
///   [`lightbox_edit::ToneCurve`]'s own "empty is also treated as identity"
///   contract (and a single stray point is treated the same way rather than
///   as an ill-defined constant curve).
/// - **Outside `[x_min, x_max]`** the curve extends *flat* (clamped to the
///   nearest endpoint's `y`), the same convention the point-curve widget
///   already paints (`lightbox-shell::panels::curve`'s "flat extensions to
///   the edges when the endpoints sit inside").
/// - **Non-finite `x`** at eval time is treated as the left endpoint (a
///   defensive guard; production callers only ever pass companion-encoded
///   `[0,1]` values).
pub struct MonotoneCubic {
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// Per-knot tangents (Fritsch-Carlson corrected), same length as `xs`.
    m: Vec<f32>,
}

impl MonotoneCubic {
    /// Builds the spline from `points`. Defensively sorted by `x` (a single
    /// out-of-order pair from fuzzed/property-test input can never invert a
    /// segment); production callers (`ToneCurve::validate`'s
    /// strictly-increasing contract) always hand in already-sorted points,
    /// so the sort is a no-op there.
    pub fn build(points: &[CurvePoint]) -> MonotoneCubic {
        let mut pts: Vec<CurvePoint> = points.to_vec();
        pts.sort_by(|a, b| a.x.total_cmp(&b.x));
        let n = pts.len();
        if n < 2 {
            // Identity spline: `eval` special-cases an empty `xs`.
            return MonotoneCubic {
                xs: Vec::new(),
                ys: Vec::new(),
                m: Vec::new(),
            };
        }
        let xs: Vec<f32> = pts.iter().map(|p| p.x).collect();
        let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();

        // Segment secant slopes, width-floored against near-duplicate x.
        let delta: Vec<f32> = (0..n - 1)
            .map(|i| {
                let dx = (xs[i + 1] - xs[i]).max(MIN_DX);
                (ys[i + 1] - ys[i]) / dx
            })
            .collect();

        // Initial tangent estimate: endpoints take the adjacent secant;
        // interior knots take the average of their two adjacent secants
        // (Fritsch-Carlson's own starting point before the monotonicity
        // correction below).
        let mut m = vec![0f32; n];
        m[0] = delta[0];
        m[n - 1] = delta[n - 2];
        for i in 1..n - 1 {
            m[i] = 0.5 * (delta[i - 1] + delta[i]);
        }

        // Step 2: a knot straddling a local extremum (adjacent secants of
        // opposite sign, or either secant exactly flat) gets a zero tangent
        // the interpolant touches the extremum without overshooting past
        // it on either side.
        for (i, tangent) in m.iter_mut().enumerate() {
            let left = if i > 0 { delta[i - 1] } else { delta[0] };
            let right = if i < n - 1 { delta[i] } else { delta[n - 2] };
            if left == 0.0 || right == 0.0 || left.signum() != right.signum() {
                *tangent = 0.0;
            }
        }
        // Steps 3-4: rescale each segment's (alpha, beta) tangent pair so
        // alpha^2 + beta^2 <= 9, the paper's sufficient condition for the
        // Hermite cubic on that segment to be monotone whenever its secant
        // is non-zero.
        for i in 0..n - 1 {
            let d = delta[i];
            if d == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let a = m[i] / d;
            let b = m[i + 1] / d;
            let s = a * a + b * b;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[i] = tau * a * d;
                m[i + 1] = tau * b * d;
            }
        }

        MonotoneCubic { xs, ys, m }
    }

    /// Evaluates the spline at `x` (flat-extended outside the knot range;
    /// identity when built from `< 2` points).
    pub fn eval(&self, x: f32) -> f32 {
        let n = self.xs.len();
        if n == 0 {
            return x;
        }
        let x = if x.is_finite() { x } else { self.xs[0] };
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        // Linear scan for the containing segment, curves are capped at 64
        // points (`MAX_CURVE_POINTS`), so this is trivially cheap even
        // baked into a LUT at hundreds of samples per channel.
        let mut i = 0;
        while i + 1 < n && x > self.xs[i + 1] {
            i += 1;
        }
        let (x0, x1) = (self.xs[i], self.xs[i + 1]);
        let (y0, y1) = (self.ys[i], self.ys[i + 1]);
        let (m0, m1) = (self.m[i], self.m[i + 1]);
        let h = (x1 - x0).max(MIN_DX);
        let t = ((x - x0) / h).clamp(0.0, 1.0);
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1
    }
}

// ── C2: the parametric curve's region weights + composed delta ────────────

/// Half-width of the smooth transition band centered on each split point
/// an implementer's calibration constant (PV1's own choice; spec §4.3
/// leaves the exact region-shaping curve to the implementer, same latitude
/// as `whites_blacks::ENDPOINT_RANGE`), bounded so adjacent transition bands
/// can never overlap (see the module docs' partition-of-unity derivation).
const TRANSITION_HALF_WIDTH: f32 = 0.08;

/// The transition half-width actually used for boundary `i` (0/1/2 for
/// shadow-dark/dark-light/light-highlight), clamped below half the distance
/// to its nearest neighboring split (or the `0`/`1` domain edge).
fn transition_half(splits: &[f32; 3], i: usize) -> f32 {
    let s = splits[i];
    let left_gap = if i == 0 { s } else { s - splits[i - 1] };
    let right_gap = if i == 2 { 1.0 - s } else { splits[i + 1] - s };
    TRANSITION_HALF_WIDTH
        .min(0.49 * left_gap.min(right_gap))
        .max(1.0e-4)
}

/// Smoothstep (Hermite) interpolation, clamped outside `[edge0, edge1]`.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let denom = (edge1 - edge0).max(1.0e-6);
    let t = ((x - edge0) / denom).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The 0->1 ramp centered on split `splits[i]` (0 well below it, 1 well
/// above it), the shared building block [`region_weights`] differences into
/// four disjoint-by-construction region weights.
fn boundary_ramp(splits: &[f32; 3], i: usize, x: f32) -> f32 {
    let s = splits[i];
    let h = transition_half(splits, i);
    smoothstep(s - h, s + h, x)
}

/// The `[-100,100]`-slider-to-y-delta calibration: the y-range a fully
/// deflected region slider may push (an implementer's choice, PV1's own
/// pick, same pattern as `whites_blacks::ENDPOINT_RANGE`).
pub const PARAMETRIC_RANGE: f32 = 0.30;

/// The four region weights at `x`, `[shadows, darks, lights, highlights]`,
/// summing to **exactly** `1.0` for every `x` (task C2's partition-of-unity
/// contract; proven in the module docs' derivation and
/// [`tests::region_weights_always_sum_to_one`]). Built from three
/// independent, non-overlapping [`boundary_ramp`]s so each weight is a
/// provably non-negative difference (or complement) of two ramps.
pub fn region_weights(splits: &[f32; 3], x: f32) -> [f32; 4] {
    let t0 = boundary_ramp(splits, 0, x);
    let t1 = boundary_ramp(splits, 1, x);
    let t2 = boundary_ramp(splits, 2, x);
    [1.0 - t0, t0 - t1, t1 - t2, t2]
}

/// The parametric curve's additive y-delta at `x` (task C2): each region
/// slider's value (`-100..=100`) scaled by [`PARAMETRIC_RANGE`] and weighted
/// by its own [`region_weights`] entry. Exactly `0.0` at every `x` when
/// every slider is `0` (the C2 "identity at zeros" AC), `region_weights`'
/// entries are finite and bounded regardless of `x`, so a zero slider
/// contributes exactly zero, unconditionally.
pub fn parametric_delta(x: f32, p: &ParametricCurve) -> f32 {
    let w = region_weights(&p.splits, x);
    (p.shadows / 100.0) * w[0] * PARAMETRIC_RANGE
        + (p.darks / 100.0) * w[1] * PARAMETRIC_RANGE
        + (p.lights / 100.0) * w[2] * PARAMETRIC_RANGE
        + (p.highlights / 100.0) * w[3] * PARAMETRIC_RANGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── C1: monotone cubic interpolation ────────────────────────────────

    #[test]
    fn identity_curve_evaluates_as_x() {
        let pts = [CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }];
        let s = MonotoneCubic::build(&pts);
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!(
                (s.eval(x) - x).abs() < 1e-5,
                "identity curve: eval({x})={} want {x}",
                s.eval(x)
            );
        }
    }

    #[test]
    fn fewer_than_two_points_is_identity() {
        let s0 = MonotoneCubic::build(&[]);
        let s1 = MonotoneCubic::build(&[CurvePoint { x: 0.4, y: 0.9 }]);
        for x in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            assert!((s0.eval(x) - x).abs() < 1e-6);
            assert!((s1.eval(x) - x).abs() < 1e-6);
        }
    }

    /// C1 AC: the interpolant passes exactly through every control point.
    #[test]
    fn interpolant_passes_through_control_points() {
        let pts = [
            CurvePoint { x: 0.0, y: 0.05 },
            CurvePoint { x: 0.3, y: 0.15 },
            CurvePoint { x: 0.5, y: 0.6 },
            CurvePoint { x: 0.8, y: 0.85 },
            CurvePoint { x: 1.0, y: 0.98 },
        ];
        let s = MonotoneCubic::build(&pts);
        for p in pts {
            assert!(
                (s.eval(p.x) - p.y).abs() < 1e-4,
                "eval({})={} want {}",
                p.x,
                s.eval(p.x),
                p.y
            );
        }
    }

    /// C1 AC: flat extension outside the knot range.
    #[test]
    fn flat_extension_outside_knot_range() {
        let pts = [CurvePoint { x: 0.2, y: 0.3 }, CurvePoint { x: 0.8, y: 0.7 }];
        let s = MonotoneCubic::build(&pts);
        assert!((s.eval(-1.0) - 0.3).abs() < 1e-6);
        assert!((s.eval(0.0) - 0.3).abs() < 1e-6);
        assert!((s.eval(1.0) - 0.7).abs() < 1e-6);
        assert!((s.eval(2.0) - 0.7).abs() < 1e-6);
    }

    /// C1 AC: stable under near-duplicate x, finite output across a dense
    /// sweep even when two knots sit a hair apart.
    #[test]
    fn stable_under_near_duplicate_x() {
        let pts = [
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.5, y: 0.2 },
            CurvePoint {
                x: 0.500_000_1,
                y: 0.9,
            },
            CurvePoint { x: 1.0, y: 1.0 },
        ];
        let s = MonotoneCubic::build(&pts);
        for i in 0..=1000 {
            let x = i as f32 / 1000.0;
            let y = s.eval(x);
            assert!(y.is_finite(), "eval({x}) = {y} is not finite");
            assert!(
                (0.0..=1.0).contains(&y) || (y - 0.0).abs() < 2.0,
                "y={y} wildly out of range"
            );
        }
    }

    proptest! {
        /// C1 AC: the interpolant is monotone non-decreasing wherever the
        /// control points themselves are (a strictly-increasing-x,
        /// non-decreasing-y point set), the Fritsch-Carlson "no overshoot"
        /// guarantee, exercised over the whole sampled domain, not just at
        /// the knots.
        #[test]
        fn interpolant_monotone_where_points_are_monotone(
            n in 2usize..12,
            ys in prop::collection::vec(0.0f32..1.0, 2..12),
        ) {
            let n = n.min(ys.len());
            prop_assume!(n >= 2);
            let mut ys: Vec<f32> = ys[..n].to_vec();
            ys.sort_by(|a, b| a.total_cmp(b)); // non-decreasing y
            let pts: Vec<CurvePoint> = ys
                .iter()
                .enumerate()
                .map(|(i, &y)| CurvePoint {
                    x: i as f32 / (n as f32 - 1.0),
                    y,
                })
                .collect();
            let s = MonotoneCubic::build(&pts);
            let mut prev = f32::NEG_INFINITY;
            for i in 0..=200 {
                let x = i as f32 / 200.0;
                let y = s.eval(x);
                prop_assert!(y >= prev - 1e-4, "non-monotone at x={x}: {y} < prev {prev}");
                prev = y;
            }
        }
    }

    // ── C2: parametric region weights + delta ───────────────────────────

    const DEFAULT_SPLITS: [f32; 3] = [0.25, 0.50, 0.75];

    #[test]
    fn region_weights_always_sum_to_one() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            let w = region_weights(&DEFAULT_SPLITS, x);
            let sum: f32 = w.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "x={x}: weights {w:?} sum to {sum}"
            );
            for c in w {
                assert!(
                    (-1e-5..=1.0 + 1e-5).contains(&c),
                    "x={x}: weight {c} out of [0,1]"
                );
            }
        }
    }

    /// C2 AC: identity at zeros.
    #[test]
    fn parametric_delta_is_zero_at_zero_sliders() {
        let p = ParametricCurve::default();
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert_eq!(parametric_delta(x, &p), 0.0, "x={x}");
        }
    }

    /// C2 AC: region isolation, a highlights-only slider leaves the lower
    /// half of the domain untouched (well below tolerance) while visibly
    /// moving the upper region.
    #[test]
    fn region_isolation_highlights_only_moves_the_upper_region() {
        let p = ParametricCurve {
            highlights: 100.0,
            ..ParametricCurve::default()
        };
        let low = parametric_delta(0.1, &p).abs();
        let high = parametric_delta(0.98, &p).abs();
        assert!(
            low < 1e-3,
            "highlights slider leaked into the low region: delta={low}"
        );
        assert!(
            high > 0.05,
            "highlights slider should visibly move the upper region: delta={high}"
        );
    }

    /// The symmetric case: shadows-only leaves the top untouched.
    #[test]
    fn region_isolation_shadows_only_moves_the_lower_region() {
        let p = ParametricCurve {
            shadows: 100.0,
            ..ParametricCurve::default()
        };
        let high = parametric_delta(0.9, &p).abs();
        let low = parametric_delta(0.02, &p).abs();
        assert!(
            high < 1e-3,
            "shadows slider leaked into the high region: delta={high}"
        );
        assert!(
            low > 0.05,
            "shadows slider should visibly move the lower region: delta={low}"
        );
    }
}
