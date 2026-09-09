// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `Lut1D`, a baked, linearly-sampled 1D lookup table over `[0,1]` (E10
//! spec §4.4 `nodes/common/lut.rs`'s 1D half; task **C3**). CPU bakes it
//! once from an arbitrary `f32 -> f32` function (the composed monotone-cubic
//! and parametric remap from [`super::curve1d`]); both `eval_cpu`
//! ([`Lut1D::sample`]) and the GPU kernel (`shaders/global_tone_curve.wgsl`)
//! sample the **identical baked table** with the identical linear
//! interpolation formula, so CPU/GPU parity (§4.4) holds to float rounding
//! by construction, with no curve math duplicated on the GPU side at all
//! (mirrors `xform.display`'s `shaper`/`eval_curve` shaper-LUT pattern
//! exactly, `crate::ng::nodes::display`).

/// Sample count baked into every [`Lut1D`] (256 intervals + 1). Comfortably
/// finer than 8-bit output precision (a linear-interpolated 257-sample LUT
/// over `[0,1]` has a worst-case inter-sample error far below one sRGB8
/// step for any curve shape a 64-point tone curve can produce), so no
/// visible banding versus evaluating the spline directly per pixel.
pub const LUT_SAMPLES: usize = 257;

/// A baked 1D lookup table over `[0,1]`, linearly sampled.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1D {
    samples: Vec<f32>,
}

impl Lut1D {
    /// Bakes `f` at [`LUT_SAMPLES`] evenly-spaced points over `[0,1]`.
    pub fn bake(f: impl Fn(f32) -> f32) -> Lut1D {
        let n = LUT_SAMPLES;
        let samples = (0..n).map(|i| f(i as f32 / (n as f32 - 1.0))).collect();
        Lut1D { samples }
    }

    /// The identity LUT (`sample(x) == x` for every `x`), used as the
    /// no-op fallback when a channel has no baked data.
    pub fn identity() -> Lut1D {
        Lut1D::bake(|x| x)
    }

    /// Linearly-interpolated sample at `x`, clamped to `[0,1]`. The exact
    /// mirror of `global_tone_curve.wgsl`'s `sample_*` functions (and of
    /// `xform.display`'s `eval_curve` shaper).
    pub fn sample(&self, x: f32) -> f32 {
        let n = self.samples.len();
        if n == 0 {
            return x;
        }
        if n == 1 {
            return self.samples[0];
        }
        let x = if x.is_finite() { x } else { 0.0 };
        let p = x.clamp(0.0, 1.0) * (n as f32 - 1.0);
        let i0 = (p.floor() as usize).min(n - 2);
        let frac = p - i0 as f32;
        self.samples[i0] * (1.0 - frac) + self.samples[i0 + 1] * frac
    }

    /// The raw baked samples, in `[0,1]`-domain order, the exact bytes the
    /// GPU path uploads as a storage buffer (`eval_gpu`'s LUT upload).
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_lut_samples_as_x() {
        let lut = Lut1D::identity();
        for i in 0..=32 {
            let x = i as f32 / 32.0;
            assert!(
                (lut.sample(x) - x).abs() < 1e-4,
                "x={x} got {}",
                lut.sample(x)
            );
        }
    }

    #[test]
    fn baked_function_is_reproduced_within_lut_resolution() {
        let lut = Lut1D::bake(|x| x * x);
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            let want = x * x;
            let got = lut.sample(x);
            assert!((got - want).abs() < 1e-3, "x={x}: got {got} want {want}");
        }
    }

    #[test]
    fn sample_clamps_and_stays_finite_outside_0_1() {
        let lut = Lut1D::bake(|x| x);
        assert!((lut.sample(-1.0) - 0.0).abs() < 1e-4);
        assert!((lut.sample(2.0) - 1.0).abs() < 1e-4);
        assert!(lut.sample(f32::NAN).is_finite());
    }

    #[test]
    fn sample_count_is_the_documented_constant() {
        let lut = Lut1D::bake(|x| x);
        assert_eq!(lut.samples().len(), LUT_SAMPLES);
    }
}
