// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

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

    /// Matrix-vector product `self · v`.
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

    /// Matrix inverse via the adjugate / determinant. **Phase B (B1)**, rejects
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

/// Display-P3 primaries: the DCI-P3 primaries on a D65 white (Apple's
/// "Display P3"). **This is what modern iPhones tag their captures with**, so
/// it is the single most common non-sRGB input space a photo editor sees.
pub const DISPLAY_P3_PRIMARIES: [[f64; 2]; 3] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]];

/// Adobe RGB (1998) primaries, the classic wide-gamut camera output space
/// (most DSLRs offer it as an in-camera JPEG setting).
pub const ADOBE_RGB_PRIMARIES: [[f64; 2]; 3] = [[0.640, 0.330], [0.210, 0.710], [0.150, 0.060]];

/// ITU-R BT.2020 primaries (Rec.2020 / the HDR stills container space).
pub const REC2020_PRIMARIES: [[f64; 2]; 3] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]];

/// A monotone cubic spline over control points (profile / look tone curve,
/// spec §3.4). Fritsch-Carlson monotone Hermite interpolation.
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
    /// Monotone cubic (Fritsch-Carlson) so control points never induce
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
        // Tangents at the two interval endpoints via Fritsch-Carlson limiting.
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
        // Fritsch-Carlson: zero the tangent at local extrema, else average and
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
        bradford_adaptation, rgb_to_xyz_matrix, xyz_to_xy, Mat3, ADOBE_RGB_PRIMARIES,
        D50_WHITE_XYZ, D65_WHITE_XYZ, DISPLAY_P3_PRIMARIES, PROPHOTO_PRIMARIES, REC2020_PRIMARIES,
        SRGB_PRIMARIES,
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

    /// Linear sRGB (D65) → working (ProPhoto-linear, D50), with Bradford
    /// adaptation D65→D50. The exact inverse of [`working_to_linear_srgb`], and
    /// the primaries half of the **input** transform for a display-referred
    /// (already-rendered, non-raw) source: a decoded sRGB JPEG/PNG is linearized
    /// with [`srgb_eotf`] and then brought into the working space with this
    /// matrix, before any develop stage runs. The raw path does not use this
    /// it reaches the working space through
    /// [`crate::resolve_input_transform`]'s camera matrices instead.
    ///
    /// sRGB's primaries sit strictly inside the ProPhoto/ROMM gamut, so every
    /// in-range sRGB colour maps to non-negative working RGB (no gamut
    /// clipping is needed on this direction).
    pub fn linear_srgb_to_working() -> Mat3 {
        working_to_linear_srgb()
            .inverse()
            .expect("working→linear-sRGB is non-degenerate")
    }

    /// Linear RGB in a **D65** space with `primaries` → working (ProPhoto-linear,
    /// D50), Bradford-adapted D65→D50.
    ///
    /// The generalization of [`linear_srgb_to_working`] to the other display-
    /// referred spaces a file can legitimately be tagged with. It is written as
    /// the forward composition (`RGB→XYZ(D65)`, adapt, `XYZ(D50)→working`) rather
    /// than by inverting a working→RGB matrix; [`tests::the_generic_d65_builder_reproduces_the_srgb_matrix`]
    /// pins that the two constructions agree, which is why [`linear_srgb_to_working`]
    /// is deliberately **left alone**, re-deriving it through this helper would
    /// perturb its last bits and could move a committed golden for no gain.
    fn linear_d65_rgb_to_working(primaries: [[f64; 2]; 3]) -> Mat3 {
        let rgb_to_xyz_d65 = rgb_to_xyz_matrix(primaries, xyz_to_xy(D65_WHITE_XYZ));
        let cat = bradford_adaptation(D65_WHITE_XYZ, D50_WHITE_XYZ);
        xyz_d50_to_working().mul(&cat).mul(&rgb_to_xyz_d65)
    }

    /// Linear Display-P3 (D65) → working. The primaries half of the input
    /// transform for a Display-P3-tagged file, the default capture space of
    /// modern iPhones, so the most common non-sRGB source in a real library.
    ///
    /// P3's primaries are **wider** than sRGB's, so reading P3 pixels as if they
    /// were sRGB under-transforms them and the image renders over-saturated;
    /// this matrix is what makes that not happen.
    pub fn linear_display_p3_to_working() -> Mat3 {
        linear_d65_rgb_to_working(DISPLAY_P3_PRIMARIES)
    }

    /// Linear Adobe RGB (1998) (D65) → working.
    pub fn linear_adobe_rgb_to_working() -> Mat3 {
        linear_d65_rgb_to_working(ADOBE_RGB_PRIMARIES)
    }

    /// Linear Rec.2020 (D65) → working.
    pub fn linear_rec2020_to_working() -> Mat3 {
        linear_d65_rgb_to_working(REC2020_PRIMARIES)
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

    /// The Adobe RGB (1998) electro-optical transfer function (encoded →
    /// linear): a pure power law with the spec's exact exponent 563/256
    /// (≈ 2.19921875), **not** 2.2, which is the approximation that shows up
    /// in casual references and is wrong by ~0.5 % in the midtones.
    pub fn adobe_rgb_eotf(v: f32) -> f32 {
        v.clamp(0.0, 1.0).powf(563.0 / 256.0)
    }

    /// The ITU-R BT.709 / BT.2020 electro-optical transfer function (encoded →
    /// linear). BT.2020 shares BT.709's curve at 8/10-bit precision, which is
    /// the only precision a tagged still arrives at here.
    pub fn rec709_eotf(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if v < 0.081 {
            v / 4.5
        } else {
            ((v + 0.099) / 1.099).powf(1.0 / 0.45)
        }
    }

    /// The ROMM RGB (ProPhoto) electro-optical transfer function (encoded →
    /// linear): gamma 1.8 above a short linear toe of slope 1/16 below
    /// `E' = 0.03125` (ISO 22028-2).
    ///
    /// The toe is included here because a real ProPhoto-tagged *file* carries
    /// it. Note the deliberate asymmetry with [`crate::cms`]'s `prophoto()`
    /// **output** profile, which omits the toe as negligible for export
    /// encoding, that simplification predates this function and is left as it
    /// is; an input transform reading someone else's file does not get to
    /// choose the curve the file was written with.
    pub fn prophoto_eotf(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        // The breakpoint is exact in binary: E = 2⁻⁹ encodes to
        // E' = 16·2⁻⁹ = 2⁻⁵ = 0.03125, and 2⁻⁹ = (2⁻⁵)^1.8 as well, so the two
        // segments meet with no discontinuity at all (not merely to rounding).
        if v < 0.031_25 {
            v / 16.0
        } else {
            v.powf(1.8)
        }
    }

    /// The "Melissa-style" companion encode for histograms/readouts (E10):
    /// sRGB curve over ProPhoto primaries. **Encode only, never a processing
    /// space** (spec §3.3). Owner: Phase B (B1)/D2, **filled by Phase D (D2)**.
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

    /// Inverse of [`companion_encode`] (E10 task A7/A8): the sRGB
    /// electro-optical transfer applied per channel, encoded → linear, same
    /// ProPhoto/ROMM primaries. Companion-domain nodes (contrast,
    /// whites/blacks, spec §4.2) encode into this space, apply their curve,
    /// then decode back through this function so their math stays in scene-
    /// linear working RGB on both sides. Input is clamped to `[0, 1]` (mirrors
    /// [`companion_encode`]'s readout-domain contract). This is exactly
    /// [`srgb_eotf`] applied per channel; kept as a named pair with
    /// [`companion_encode`] so call sites read as a matched encode/decode.
    #[must_use]
    pub fn companion_decode(rgb_encoded: [f32; 3]) -> [f32; 3] {
        [
            srgb_eotf(rgb_encoded[0]),
            srgb_eotf(rgb_encoded[1]),
            srgb_eotf(rgb_encoded[2]),
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

    /// The input-transform primaries matrix is the exact inverse of the output
    /// one, so a display-referred source that is linearized, taken to working
    /// space, and taken back out again lands where it started.
    #[test]
    fn linear_srgb_to_working_inverts_working_to_linear_srgb() {
        approx_mat(
            &spaces::linear_srgb_to_working().mul(&spaces::working_to_linear_srgb()),
            &Mat3::IDENTITY,
            1e-10,
        );
    }

    /// The generic D65 builder used by the Display-P3 / Adobe RGB / Rec.2020
    /// input matrices reproduces the hand-written sRGB one. This is what lets
    /// [`spaces::linear_srgb_to_working`] stay byte-for-byte as it was (its
    /// output feeds a committed golden) while the new spaces reuse one
    /// derivation.
    #[test]
    fn the_generic_d65_builder_reproduces_the_srgb_matrix() {
        let generic = {
            let rgb_to_xyz_d65 = rgb_to_xyz_matrix(SRGB_PRIMARIES, xyz_to_xy(D65_WHITE_XYZ));
            let cat = bradford_adaptation(D65_WHITE_XYZ, D50_WHITE_XYZ);
            spaces::xyz_d50_to_working().mul(&cat).mul(&rgb_to_xyz_d65)
        };
        approx_mat(&generic, &spaces::linear_srgb_to_working(), 1e-12);
    }

    /// The rounded D65 chromaticity the CSS Color 4 / BT.2020 reference
    /// matrices are published under. It is **not** the same D65 this crate
    /// works in: [`D65_WHITE_XY`] is derived from Lindbloom's tristimulus
    /// (`Z = 1.08883` ⇒ `y = 0.329023`), while these specs round to
    /// `(0.3127, 0.3290)` (`Z = 1.08906`). The 2·10⁻⁴ that separates them shows
    /// up in the blue-Z entry of any matrix derived from them, so a reference
    /// matrix is checked under the white it was published with, otherwise the
    /// test measures the two committees' rounding, not our primaries.
    const PUBLISHED_D65_XY: [f64; 2] = [0.3127, 0.3290];

    /// Display-P3 → XYZ(D65) against the published reference (CSS Color 4 /
    /// Apple "Display P3": DCI-P3 primaries on a D65 white). Guards the
    /// primaries constants themselves, independently of the working-space
    /// plumbing.
    #[test]
    fn display_p3_matrix_matches_published_reference() {
        let published = Mat3([
            [0.4865709, 0.2656677, 0.1982173],
            [0.2289746, 0.6917385, 0.0792869],
            [0.0000000, 0.0451134, 1.0439444],
        ]);
        let derived = rgb_to_xyz_matrix(DISPLAY_P3_PRIMARIES, PUBLISHED_D65_XY);
        approx_mat(&derived, &published, 1e-4);
    }

    /// Adobe RGB (1998) → XYZ(D65) against Lindbloom's published matrix.
    #[test]
    fn adobe_rgb_matrix_matches_published_reference() {
        let published = Mat3([
            [0.5767309, 0.1855540, 0.1881852],
            [0.2973769, 0.6273491, 0.0752741],
            [0.0270343, 0.0706872, 0.9911085],
        ]);
        let derived = rgb_to_xyz_matrix(ADOBE_RGB_PRIMARIES, D65_WHITE_XY);
        approx_mat(&derived, &published, 1e-4);
    }

    /// Rec.2020 → XYZ(D65) against the published ITU-R BT.2020 matrix.
    #[test]
    fn rec2020_matrix_matches_published_reference() {
        let published = Mat3([
            [0.6369580, 0.1446169, 0.1688810],
            [0.2627002, 0.6779981, 0.0593017],
            [0.0000000, 0.0280727, 1.0609851],
        ]);
        let derived = rgb_to_xyz_matrix(REC2020_PRIMARIES, PUBLISHED_D65_XY);
        approx_mat(&derived, &published, 1e-4);
    }

    /// Every source space we accept sits inside ProPhoto/ROMM **except for one
    /// edge case worth naming**, and the excursion there is negligible.
    ///
    /// Display-P3's red primary is `(0.680, 0.320)`, whose chromaticities sum to
    /// exactly 1.0, and ROMM's red→green edge is exactly the line `y = 1 − x`
    /// (its red sums to 1.0 and so does its green). P3 red therefore lies
    /// *precisely on* the ROMM boundary, not inside it. The Bradford adaptation
    /// D65→D50 then nudges it a hair across, so a fully saturated P3 red lifts
    /// to a working blue of about −0.0013, 0.2 % of the red channel's own
    /// magnitude.
    ///
    /// The lift deliberately **does not clamp** that: the working space is
    /// scene-linear f16 and every develop stage is happy with a slightly
    /// negative channel, whereas clamping would quietly desaturate the reddest
    /// pixels an iPhone can record. The display transform clips at the end,
    /// where clipping belongs. This test pins the size of the excursion so a
    /// future primaries or CAT change cannot grow it unnoticed.
    #[test]
    fn source_spaces_sit_inside_the_working_gamut_to_within_a_rounding_error() {
        let mut worst: (f64, &str, [f64; 3]) = (0.0, "", [0.0; 3]);
        for (name, m) in [
            ("display-p3", spaces::linear_display_p3_to_working()),
            ("adobe-rgb", spaces::linear_adobe_rgb_to_working()),
            ("rec2020", spaces::linear_rec2020_to_working()),
        ] {
            for corner in [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
            ] {
                let w = m.mul_vec(Vec3(corner)).0;
                for c in w {
                    if c < worst.0 {
                        worst = (c, name, w);
                    }
                }
            }
        }
        assert!(
            worst.0 >= -0.005,
            "worst working-gamut excursion {} ({} → {:?}) is larger than the \
             boundary-rounding effect this test allows for",
            worst.0,
            worst.1,
            worst.2,
        );
    }

    /// A neutral is a neutral in every source space: encoded white must land on
    /// the working space's own white (equal RGB, since the working space is
    /// D50-native and the adaptation is part of the matrix).
    #[test]
    fn source_space_white_maps_to_working_neutral() {
        for (name, m) in [
            ("srgb", spaces::linear_srgb_to_working()),
            ("display-p3", spaces::linear_display_p3_to_working()),
            ("adobe-rgb", spaces::linear_adobe_rgb_to_working()),
            ("rec2020", spaces::linear_rec2020_to_working()),
        ] {
            let w = m.mul_vec(Vec3([1.0, 1.0, 1.0])).0;
            assert!(
                (w[0] - w[1]).abs() < 1e-6 && (w[1] - w[2]).abs() < 1e-6,
                "{name}: white → {w:?} is not neutral in the working space",
            );
        }
    }

    /// The ROMM toe joins its gamma-1.8 segment exactly at the breakpoint (both
    /// sides are powers of two there), and the curve is monotone through it.
    #[test]
    fn prophoto_eotf_is_continuous_at_the_toe() {
        let below = spaces::prophoto_eotf(0.031_25 - f32::EPSILON);
        let at = spaces::prophoto_eotf(0.031_25);
        assert!(
            (at - 0.001_953_125).abs() < 1e-9,
            "breakpoint maps to 2⁻⁹, got {at}",
        );
        assert!(
            (at - below).abs() < 1e-6,
            "toe/gamma segments disagree at the breakpoint: {below} vs {at}",
        );
    }

    /// Each new transfer function is a monotone map of `[0,1]` onto `[0,1]`,
    /// anchored at both ends, the minimum contract the source lift's decode
    /// table relies on.
    #[test]
    fn source_transfer_functions_are_monotone_and_anchored() {
        for (name, f) in [
            ("adobe-rgb", spaces::adobe_rgb_eotf as fn(f32) -> f32),
            ("rec709", spaces::rec709_eotf),
            ("prophoto", spaces::prophoto_eotf),
            ("srgb", spaces::srgb_eotf),
        ] {
            assert!(f(0.0).abs() < 1e-9, "{name}: f(0) = {}", f(0.0));
            assert!((f(1.0) - 1.0).abs() < 1e-6, "{name}: f(1) = {}", f(1.0));
            let mut prev = -1.0;
            for i in 0..=255 {
                let y = f(i as f32 / 255.0);
                assert!(y >= prev - 1e-9, "{name}: non-monotone at code point {i}");
                assert!(
                    (-1e-9..=1.0 + 1e-6).contains(&y),
                    "{name}: {y} out of range"
                );
                prev = y;
            }
        }
    }

    /// sRGB's primaries lie inside ProPhoto/ROMM, so every sRGB corner maps to
    /// non-negative working RGB, the input transform never needs gamut
    /// clipping in this direction.
    #[test]
    fn srgb_primaries_map_inside_the_working_gamut() {
        let m = spaces::linear_srgb_to_working();
        for corner in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
        ] {
            let w = m.mul_vec(Vec3(corner)).0;
            for c in w {
                assert!(c >= -1e-6, "sRGB {corner:?} → working {w:?} went negative");
            }
        }
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

    /// E10 A7/A8: `companion_decode` is the exact inverse of `companion_encode`
    /// over the readout domain `[0,1]`, the round-trip the contrast /
    /// whites-blacks nodes rely on (encode → curve → decode).
    #[test]
    fn companion_decode_round_trips_companion_encode() {
        for x in [0.0f32, 0.02, 0.18, 0.46, 0.5, 0.75, 1.0] {
            let rgb = [x, x, x];
            let enc = spaces::companion_encode(rgb);
            let dec = spaces::companion_decode(enc);
            for d in dec {
                assert!((d - x).abs() < 1e-5, "round trip at x={x}: got {d}");
            }
        }
    }
}
