// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Math core + working/standard color spaces (spec §3.3, module `math`/`spaces`).
//! **Owner: Phase B** (B1). [`Mat3`]/[`Vec3`] with their exact ops, the 3×3
//! inverse with singular rejection, the Bradford chromatic-adaptation transform,
//! chromaticity ⇄ tristimulus helpers, the primaries→matrix derivation, and the
//! fixed ProPhoto-linear-D50 working-space constants (all derived from primaries
//! at run time and tested against published references to `1e-4`).
//!
//! Everything here is `f64` throughout the solver; only the resolved GPU-upload
//! forms downgrade to `f32`.

use lightbox_decode::Mat3Array;

/// Row-major 3×3 matrix, `f64` throughout the solver (spec §3.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mat3(pub [[f64; 3]; 3]);

/// A 3-vector (`f64`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Vec3(pub [f64; 3]);

impl From<Mat3Array> for Mat3 {
    fn from(a: Mat3Array) -> Self {
        Mat3(a)
    }
}

impl Mat3 {
    /// The identity matrix.
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    /// A diagonal matrix from a 3-vector.
    pub fn from_diag(d: [f64; 3]) -> Mat3 {
        Mat3([[d[0], 0.0, 0.0], [0.0, d[1], 0.0], [0.0, 0.0, d[2]]])
    }

    /// Matrix product `self · rhs`.
    pub fn mul(&self, rhs: &Mat3) -> Mat3 {
        let a = &self.0;
        let b = &rhs.0;
        let mut out = [[0.0f64; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
            }
        }
        Mat3(out)
    }

    /// Matrix–vector product `self · v`.
    pub fn mul_vec(&self, v: Vec3) -> Vec3 {
        let a = &self.0;
        Vec3([
            a[0][0] * v.0[0] + a[0][1] * v.0[1] + a[0][2] * v.0[2],
            a[1][0] * v.0[0] + a[1][1] * v.0[1] + a[1][2] * v.0[2],
            a[2][0] * v.0[0] + a[2][1] * v.0[1] + a[2][2] * v.0[2],
        ])
    }

    /// Scales every entry by `s`.
    pub fn scale(&self, s: f64) -> Mat3 {
        let a = &self.0;
        let mut out = [[0.0f64; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = a[i][j] * s;
            }
        }
        Mat3(out)
    }

    /// Entry-wise `self + rhs`.
    pub fn add(&self, rhs: &Mat3) -> Mat3 {
        let a = &self.0;
        let b = &rhs.0;
        let mut out = [[0.0f64; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = a[i][j] + b[i][j];
            }
        }
        Mat3(out)
    }

    /// Convex blend `self·(1-t) + rhs·t`, entry-wise.
    pub fn lerp(&self, rhs: &Mat3, t: f64) -> Mat3 {
        self.scale(1.0 - t).add(&rhs.scale(t))
    }

    /// Determinant.
    pub fn det(&self) -> f64 {
        let a = &self.0;
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    }

    /// Transpose.
    pub fn transpose(&self) -> Mat3 {
        let a = &self.0;
        Mat3([
            [a[0][0], a[1][0], a[2][0]],
            [a[0][1], a[1][1], a[2][1]],
            [a[0][2], a[1][2], a[2][2]],
        ])
    }

    /// Matrix inverse via the adjugate / determinant. **Phase B (B1)** — rejects
    /// a singular or near-singular matrix with [`crate::ColorError::SingularMatrix`].
    pub fn inverse(&self) -> Result<Mat3, crate::ColorError> {
        let m = &self.0;
        // Cofactors (the adjugate is their transpose).
        let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
        let c01 = m[1][2] * m[2][0] - m[1][0] * m[2][2];
        let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
        let c10 = m[0][2] * m[2][1] - m[0][1] * m[2][2];
        let c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0];
        let c12 = m[0][1] * m[2][0] - m[0][0] * m[2][1];
        let c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1];
        let c21 = m[0][2] * m[1][0] - m[0][0] * m[1][2];
        let c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0];

        let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
        // Reject a singular matrix relative to the entry magnitude so a
        // legitimately small-but-well-conditioned matrix is not rejected while a
        // genuinely degenerate one is.
        let scale = m
            .iter()
            .flatten()
            .fold(0.0f64, |acc, &v| acc.max(v.abs()))
            .max(1.0);
        if !det.is_finite() || det.abs() < 1e-12 * scale * scale * scale {
            return Err(crate::ColorError::SingularMatrix(
                "3×3 matrix is singular or near-degenerate",
            ));
        }
        let inv_det = 1.0 / det;
        // adjugate = transpose(cofactor); divide by det.
        Ok(Mat3([
            [c00 * inv_det, c10 * inv_det, c20 * inv_det],
            [c01 * inv_det, c11 * inv_det, c21 * inv_det],
            [c02 * inv_det, c12 * inv_det, c22 * inv_det],
        ]))
    }
}

/// Chromaticity `[x, y]` → tristimulus `[X, Y, Z]`, normalized to `Y = 1`.
pub fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    let (x, y) = (xy[0], xy[1]);
    debug_assert!(y > 0.0, "chromaticity y must be positive");
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Tristimulus `[X, Y, Z]` → chromaticity `[x, y]`.
pub fn xyz_to_xy(xyz: [f64; 3]) -> [f64; 2] {
    let sum = xyz[0] + xyz[1] + xyz[2];
    if sum.abs() < f64::MIN_POSITIVE {
        return [0.0, 0.0];
    }
    [xyz[0] / sum, xyz[1] / sum]
}

/// The Bradford spectral-sharpening cone-response matrix (Lindbloom).
const BRADFORD: Mat3 = Mat3([
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
]);

/// The Bradford chromatic-adaptation transform mapping tristimulus values under
/// source white `src_white` to their appearance under destination white
/// `dst_white` (spec §3.3, B1). Both whites are absolute tristimulus (`Y = 1`).
pub fn bradford_adaptation(src_white: [f64; 3], dst_white: [f64; 3]) -> Mat3 {
    let m_a = BRADFORD;
    // The cone-response matrix is a fixed, well-conditioned constant.
    let m_a_inv = m_a.inverse().expect("Bradford matrix is invertible");
    let s = m_a.mul_vec(Vec3(src_white)).0;
    let d = m_a.mul_vec(Vec3(dst_white)).0;
    let gain = Mat3::from_diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]]);
    m_a_inv.mul(&gain).mul(&m_a)
}

/// Derives the linear `RGB → XYZ` matrix for an RGB space from its primary
/// chromaticities and white point (spec §3.3, B1). `primaries` are the R, G, B
/// chromaticities; `white_xy` is the reference white chromaticity.
pub fn rgb_to_xyz_matrix(primaries: [[f64; 2]; 3], white_xy: [f64; 2]) -> Mat3 {
    // Column tristimulus of each primary at unit luminance.
    let mut cols = [[0.0f64; 3]; 3];
    for (c, p) in primaries.iter().enumerate() {
        let xyz = xy_to_xyz(*p);
        cols[0][c] = xyz[0];
        cols[1][c] = xyz[1];
        cols[2][c] = xyz[2];
    }
    let primary_mat = Mat3(cols);
    let white = xy_to_xyz(white_xy);
    // Per-primary scale so the primaries sum to the white point.
    let s = primary_mat
        .inverse()
        .expect("primary matrix is invertible")
        .mul_vec(Vec3(white))
        .0;
    Mat3([
        [cols[0][0] * s[0], cols[0][1] * s[1], cols[0][2] * s[2]],
        [cols[1][0] * s[0], cols[1][1] * s[1], cols[1][2] * s[2]],
        [cols[2][0] * s[0], cols[2][1] * s[1], cols[2][2] * s[2]],
    ])
}

/// CIE D50 reference white tristimulus (`Y = 1`, Lindbloom).
pub const D50_WHITE_XYZ: [f64; 3] = [0.96422, 1.0, 0.82521];
/// CIE D65 reference white tristimulus (`Y = 1`, Lindbloom).
pub const D65_WHITE_XYZ: [f64; 3] = [0.95047, 1.0, 1.08883];

/// D50 white chromaticity.
pub const D50_WHITE_XY: [f64; 2] = [0.345704, 0.358540];
/// D65 white chromaticity.
pub const D65_WHITE_XY: [f64; 2] = [0.312727, 0.329023];

/// ProPhoto / ROMM primaries (Lindbloom): R, G, B chromaticities.
pub const PROPHOTO_PRIMARIES: [[f64; 2]; 3] = [
    [0.734699, 0.265301],
    [0.159597, 0.840403],
    [0.036598, 0.000105],
];

/// sRGB primaries: R, G, B chromaticities.
pub const SRGB_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];

/// A monotone cubic spline over control points (profile / look tone curve,
/// spec §3.4). Fritsch–Carlson monotone Hermite interpolation.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Spline1D {
    /// `(x, y)` control points in `[0, 1]`, ascending in `x`.
    pub control_points: Vec<[f32; 2]>,
}

impl Spline1D {
    /// The identity curve `y = x`.
    pub fn identity() -> Spline1D {
        Spline1D {
            control_points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }

    /// Evaluates the spline at `x`.
    ///
    /// Monotone cubic (Fritsch–Carlson) so control points never induce
    /// overshoot; clamps to the endpoints outside the control range. Nominally
    /// spec §3.4 F4, implemented in Phase B because [`Spline1D`] lives in this
    /// (B-owned) module and B8's `resolve_input_transform` samples profile/look
    /// tone curves through it (recorded in `E02-deviations.md`).
    pub fn eval(&self, x: f32) -> f32 {
        let pts = &self.control_points;
        match pts.len() {
            0 => return x,
            1 => return pts[0][1],
            _ => {}
        }
        let x = x as f64;
        if x <= pts[0][0] as f64 {
            return pts[0][1];
        }
        let last = pts.len() - 1;
        if x >= pts[last][0] as f64 {
            return pts[last][1];
        }
        // Locate the interval.
        let mut i = 0usize;
        while i + 1 < pts.len() && (pts[i + 1][0] as f64) < x {
            i += 1;
        }
        let x0 = pts[i][0] as f64;
        let x1 = pts[i + 1][0] as f64;
        let y0 = pts[i][1] as f64;
        let y1 = pts[i + 1][1] as f64;
        let h = x1 - x0;
        if h <= 0.0 {
            return y1 as f32;
        }
        // Secant slope of this interval.
        let delta = (y1 - y0) / h;
        // Tangents at the two interval endpoints via Fritsch–Carlson limiting.
        let m0 = self.tangent(i, delta);
        let m1 = self.tangent(i + 1, delta);
        // Cubic Hermite basis on the normalized parameter.
        let t = (x - x0) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1) as f32
    }

    /// Monotone tangent at control point `i`, given the local secant slope
    /// `delta` of the interval currently being evaluated.
    fn tangent(&self, i: usize, delta: f64) -> f64 {
        let pts = &self.control_points;
        let last = pts.len() - 1;
        let slope = |a: usize, b: usize| {
            let dx = pts[b][0] as f64 - pts[a][0] as f64;
            if dx.abs() < f64::MIN_POSITIVE {
                0.0
            } else {
                (pts[b][1] as f64 - pts[a][1] as f64) / dx
            }
        };
        // Neighbouring secant slopes (endpoints reuse the incident secant).
        let (d_prev, d_next) = if i == 0 {
            (delta, delta)
        } else if i == last {
            (slope(last - 1, last), slope(last - 1, last))
        } else {
            (slope(i - 1, i), slope(i, i + 1))
        };
        // Fritsch–Carlson: zero the tangent at local extrema, else average and
        // clamp to 3× the smaller-magnitude neighbouring slope.
        if d_prev * d_next <= 0.0 {
            0.0
        } else {
            let m = 0.5 * (d_prev + d_next);
            let lim = 3.0 * d_prev.abs().min(d_next.abs());
            m.clamp(-lim, lim)
        }
    }
}

/// The fixed working space (§4.1): ProPhoto/ROMM primaries, linear TRC, D50
/// white. **Owner: Phase B (B1).**
pub mod spaces {
    use super::{
        bradford_adaptation, rgb_to_xyz_matrix, xyz_to_xy, Mat3, D50_WHITE_XYZ, D65_WHITE_XYZ,
        PROPHOTO_PRIMARIES, SRGB_PRIMARIES,
    };

    /// Working (ProPhoto-linear, D50) → XYZ(D50). Derived from the ProPhoto
    /// primaries at run time; tested against the published reference (B1). The
    /// white is taken from the D50 reference *tristimulus* (Lindbloom's ASTM
    /// value) so the primaries derivation, the Bradford CAT, and the published
    /// matrix all agree on the same D50 to `1e-4`.
    pub fn working_to_xyz_d50() -> Mat3 {
        rgb_to_xyz_matrix(PROPHOTO_PRIMARIES, xyz_to_xy(D50_WHITE_XYZ))
    }

    /// XYZ(D50) → working (ProPhoto-linear).
    pub fn xyz_d50_to_working() -> Mat3 {
        working_to_xyz_d50()
            .inverse()
            .expect("ProPhoto primaries are non-degenerate")
    }

    /// Working (ProPhoto-linear, D50) → linear sRGB (D65), with Bradford
    /// adaptation D50→D65. Used by the CPU reference render (B9) and output.
    pub fn working_to_linear_srgb() -> Mat3 {
        let srgb_to_xyz_d65 = rgb_to_xyz_matrix(SRGB_PRIMARIES, xyz_to_xy(D65_WHITE_XYZ));
        let xyz_d65_to_srgb = srgb_to_xyz_d65
            .inverse()
            .expect("sRGB primaries are non-degenerate");
        let cat = bradford_adaptation(D50_WHITE_XYZ, D65_WHITE_XYZ);
        xyz_d65_to_srgb.mul(&cat).mul(&working_to_xyz_d50())
    }

    /// The sRGB opto-electronic transfer function (linear → encoded).
    pub fn srgb_oetf(u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        if u <= 0.003_130_8 {
            12.92 * u
        } else {
            1.055 * u.powf(1.0 / 2.4) - 0.055
        }
    }

    /// The sRGB electro-optical transfer function (encoded → linear).
    pub fn srgb_eotf(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.040_449_936 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// The "Melissa-style" companion encode for histograms/readouts (E10):
    /// sRGB curve over ProPhoto primaries. **Encode only — never a processing
    /// space** (spec §3.3). Owner: Phase B (B1)/D2 — **filled by Phase D (D2)**.
    ///
    /// The working space already carries ProPhoto/ROMM primaries, so the
    /// companion space (same primaries, sRGB transfer) differs from it only by
    /// the per-channel transfer function: this is exactly the sRGB opto-electronic
    /// transfer applied to each linear channel. Input is clamped to `[0, 1]`
    /// (readout domain). Verified to ≤ 1e-4 against an LCMS2-built
    /// ProPhoto-linear → ProPhoto-sRGB transform in
    /// `cms::tests::companion_encode_matches_lcms` (D2).
    #[must_use]
    pub fn companion_encode(rgb_linear: [f32; 3]) -> [f32; 3] {
        fn srgb_oetf(c: f32) -> f32 {
            let c = c.clamp(0.0, 1.0);
            if c <= 0.003_130_8 {
                12.92 * c
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            }
        }
        [
            srgb_oetf(rgb_linear[0]),
            srgb_oetf(rgb_linear[1]),
            srgb_oetf(rgb_linear[2]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_mat(a: &Mat3, b: &Mat3, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (a.0[i][j] - b.0[i][j]).abs() < tol,
                    "entry [{i}][{j}]: {} vs {} (tol {tol})",
                    a.0[i][j],
                    b.0[i][j]
                );
            }
        }
    }

    #[test]
    fn inverse_round_trips_to_identity() {
        let m = Mat3([[0.5, 0.1, 0.2], [0.3, 0.9, 0.1], [0.05, 0.2, 0.7]]);
        let inv = m.inverse().unwrap();
        approx_mat(&m.mul(&inv), &Mat3::IDENTITY, 1e-12);
        approx_mat(&inv.mul(&m), &Mat3::IDENTITY, 1e-12);
    }

    #[test]
    fn singular_matrix_is_rejected() {
        // Row 3 = row1 + row2 ⇒ rank-deficient.
        let m = Mat3([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [5.0, 7.0, 9.0]]);
        assert!(matches!(
            m.inverse(),
            Err(crate::ColorError::SingularMatrix(_))
        ));
    }

    #[test]
    fn prophoto_matrix_matches_published_reference() {
        // Bruce Lindbloom's ProPhoto RGB (D50) → XYZ.
        let published = Mat3([
            [0.7976749, 0.1351917, 0.0313534],
            [0.2880402, 0.7118741, 0.0000857],
            [0.0000000, 0.0000000, 0.8252100],
        ]);
        approx_mat(&spaces::working_to_xyz_d50(), &published, 1e-4);
    }

    #[test]
    fn srgb_matrix_matches_published_reference() {
        // Lindbloom sRGB (D65) → XYZ.
        let published = Mat3([
            [0.4124564, 0.3575761, 0.1804375],
            [0.2126729, 0.7151522, 0.0721750],
            [0.0193339, 0.1191920, 0.9503041],
        ]);
        let derived = rgb_to_xyz_matrix(SRGB_PRIMARIES, D65_WHITE_XY);
        approx_mat(&derived, &published, 1e-4);
    }

    #[test]
    fn working_space_round_trips() {
        approx_mat(
            &spaces::xyz_d50_to_working().mul(&spaces::working_to_xyz_d50()),
            &Mat3::IDENTITY,
            1e-10,
        );
    }

    #[test]
    fn bradford_round_trips() {
        let fwd = bradford_adaptation(D50_WHITE_XYZ, D65_WHITE_XYZ);
        let back = bradford_adaptation(D65_WHITE_XYZ, D50_WHITE_XYZ);
        approx_mat(&back.mul(&fwd), &Mat3::IDENTITY, 1e-10);
        // D50 white adapts to D65 white.
        let w = fwd.mul_vec(Vec3(D50_WHITE_XYZ)).0;
        for k in 0..3 {
            assert!((w[k] - D65_WHITE_XYZ[k]).abs() < 1e-6);
        }
    }

    #[test]
    fn xy_xyz_round_trip() {
        let xy = [0.3127, 0.3290];
        let back = xyz_to_xy(xy_to_xyz(xy));
        assert!((back[0] - xy[0]).abs() < 1e-12 && (back[1] - xy[1]).abs() < 1e-12);
    }

    #[test]
    fn spline_identity_is_exact() {
        let s = Spline1D::identity();
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            assert!((s.eval(x) - x).abs() < 1e-6, "x={x} -> {}", s.eval(x));
        }
    }

    #[test]
    fn spline_is_monotone_and_clamped() {
        let s = Spline1D {
            control_points: vec![[0.0, 0.0], [0.25, 0.1], [0.75, 0.9], [1.0, 1.0]],
        };
        // Endpoints clamp.
        assert!((s.eval(-1.0) - 0.0).abs() < 1e-6);
        assert!((s.eval(2.0) - 1.0).abs() < 1e-6);
        // Monotone non-decreasing, no overshoot beyond [0,1].
        let mut prev = -1.0;
        for i in 0..=200 {
            let y = s.eval(i as f32 / 200.0);
            assert!(y >= prev - 1e-6, "non-monotone at {i}");
            assert!((-1e-6..=1.0 + 1e-6).contains(&y));
            prev = y;
        }
    }

    #[test]
    fn companion_encode_matches_srgb_curve() {
        let out = spaces::companion_encode([0.0, 0.5, 1.0]);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - spaces::srgb_oetf(0.5)).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }
}
