// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Math core + working/standard color spaces (spec §3.3, module `math`/`spaces`).
//! **Owner: Phase B** (B1). Phase A ships the [`Mat3`]/[`Vec3`] types with their
//! unambiguous basic ops (so every downstream module compiles against them) and
//! leaves the numerically-load-bearing derivations (inverse, Bradford CAT,
//! primaries→matrix, the working-space constants) as `unimplemented!()` for B.

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

    /// Transpose.
    pub fn transpose(&self) -> Mat3 {
        let a = &self.0;
        Mat3([
            [a[0][0], a[1][0], a[2][0]],
            [a[0][1], a[1][1], a[2][1]],
            [a[0][2], a[1][2], a[2][2]],
        ])
    }

    /// Matrix inverse. **Phase B (B1)** — rejects a singular matrix with
    /// [`crate::ColorError::SingularMatrix`].
    pub fn inverse(&self) -> Result<Mat3, crate::ColorError> {
        unimplemented!("B1: 3×3 inverse with singular-matrix rejection")
    }
}

/// A monotone cubic spline over control points (profile / look tone curve,
/// spec §3.4). **Owner: Phase F (F4) / E**.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Spline1D {
    /// `(x, y)` control points in `[0, 1]`, ascending in `x`.
    pub control_points: Vec<[f32; 2]>,
}

impl Spline1D {
    /// Evaluates the spline at `x ∈ [0, 1]`. **Phase F (F4)** — monotone cubic
    /// per the DNG SDK reference model.
    pub fn eval(&self, _x: f32) -> f32 {
        unimplemented!("F4: monotone cubic spline evaluation")
    }
}

/// The fixed working space (§4.1): ProPhoto/ROMM primaries, linear TRC, D50
/// white. **Owner: Phase B (B1).**
pub mod spaces {
    use super::Mat3;

    /// XYZ(D50) → working (ProPhoto-linear). Derived from primaries at build
    /// time, tested vs published references (B1).
    pub fn xyz_d50_to_working() -> Mat3 {
        unimplemented!("B1: derive ProPhoto matrices from primaries")
    }

    /// Working → XYZ(D50).
    pub fn working_to_xyz_d50() -> Mat3 {
        unimplemented!("B1: derive ProPhoto matrices from primaries")
    }

    /// The "Melissa-style" companion encode for histograms/readouts (E10):
    /// sRGB curve over ProPhoto primaries. **Encode only — never a processing
    /// space** (spec §3.3). Owner: Phase B (B1)/D2.
    pub fn companion_encode(_rgb_linear: [f32; 3]) -> [f32; 3] {
        unimplemented!("D2: Melissa companion encode")
    }
}
