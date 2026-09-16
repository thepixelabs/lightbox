// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Self-contained f64 Lab / ΔE2000 / PSNR comparators (spec §4.4/§6; task
//! **A16**).
//!
//! Owner: **A-core**. [`ciede2000`] is validated against the published
//! Sharma-Wu-Dalal ΔE2000 test vectors (see the unit tests); the golden
//! tolerance is **ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB** (the §4.4 consistency
//! contract). This is a *reference* implementation (own math, no external color
//! crate) so goldens and cross-backend parity share one ground truth.

/// The consistency-contract ΔE2000 tolerance (§4.4).
pub const TOLERANCE_DELTA_E: f64 = 1.0;

/// The consistency-contract PSNR floor, dB (§4.4).
pub const TOLERANCE_PSNR_DB: f64 = 45.0;

/// A CIE L*a*b* triple, f64 (spec §4.4 reference space). Computed under the
/// sRGB native white (D65) by [`srgb8_to_lab`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab {
    /// Lightness L*.
    pub l: f64,
    /// a* (green-red).
    pub a: f64,
    /// b* (blue-yellow).
    pub b: f64,
}

impl Lab {
    /// A `Lab` triple.
    pub fn new(l: f64, a: f64, b: f64) -> Lab {
        Lab { l, a, b }
    }
}

/// Aggregate ΔE2000 statistics over an image pair (spec §6 golden row).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeltaEStats {
    /// Mean ΔE2000.
    pub mean: f64,
    /// 99th-percentile ΔE2000.
    pub p99: f64,
    /// Maximum ΔE2000.
    pub max: f64,
}

impl DeltaEStats {
    /// Whether these stats pass the §4.4 tolerance (max ≤ 1.0).
    pub fn within_tolerance(&self) -> bool {
        self.max <= TOLERANCE_DELTA_E
    }
}

#[inline]
fn deg_to_rad(d: f64) -> f64 {
    d * std::f64::consts::PI / 180.0
}

/// `atan2(b, a)` in degrees, normalized to `[0, 360)`.
#[inline]
fn atan2_deg(y: f64, x: f64) -> f64 {
    let mut d = y.atan2(x).to_degrees();
    if d < 0.0 {
        d += 360.0;
    }
    d
}

/// CIEDE2000 ΔE between two Lab colors (spec §4.4; task A16), using the
/// Sharma-Wu-Dalal formulation with `kL = kC = kH = 1`.
pub fn ciede2000(reference: Lab, sample: Lab) -> f64 {
    let (l1, a1, b1) = (reference.l, reference.a, reference.b);
    let (l2, a2, b2) = (sample.l, sample.a, sample.b);

    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let c_bar = (c1 + c2) / 2.0;

    let c_bar7 = c_bar.powi(7);
    let pow25_7 = 25.0f64.powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + pow25_7)).sqrt());

    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();

    let h1p = if b1 == 0.0 && a1p == 0.0 {
        0.0
    } else {
        atan2_deg(b1, a1p)
    };
    let h2p = if b2 == 0.0 && a2p == 0.0 {
        0.0
    } else {
        atan2_deg(b2, a2p)
    };

    let d_lp = l2 - l1;
    let d_cp = c2p - c1p;

    let d_hp = if c1p * c2p == 0.0 {
        0.0
    } else {
        let diff = h2p - h1p;
        if diff.abs() <= 180.0 {
            diff
        } else if diff > 180.0 {
            diff - 360.0
        } else {
            diff + 360.0
        }
    };
    let d_hp_big = 2.0 * (c1p * c2p).sqrt() * deg_to_rad(d_hp / 2.0).sin();

    let l_bar_p = (l1 + l2) / 2.0;
    let c_bar_p = (c1p + c2p) / 2.0;

    let h_bar_p = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };

    let t = 1.0 - 0.17 * deg_to_rad(h_bar_p - 30.0).cos()
        + 0.24 * deg_to_rad(2.0 * h_bar_p).cos()
        + 0.32 * deg_to_rad(3.0 * h_bar_p + 6.0).cos()
        - 0.20 * deg_to_rad(4.0 * h_bar_p - 63.0).cos();

    let d_theta = 30.0 * (-(((h_bar_p - 275.0) / 25.0).powi(2))).exp();
    let c_bar_p7 = c_bar_p.powi(7);
    let r_c = 2.0 * (c_bar_p7 / (c_bar_p7 + pow25_7)).sqrt();

    let l_off = (l_bar_p - 50.0).powi(2);
    let s_l = 1.0 + (0.015 * l_off) / (20.0 + l_off).sqrt();
    let s_c = 1.0 + 0.045 * c_bar_p;
    let s_h = 1.0 + 0.015 * c_bar_p * t;
    let r_t = -deg_to_rad(2.0 * d_theta).sin() * r_c;

    let term_l = d_lp / s_l;
    let term_c = d_cp / s_c;
    let term_h = d_hp_big / s_h;

    (term_l * term_l + term_c * term_c + term_h * term_h + r_t * term_c * term_h).sqrt()
}

#[inline]
fn srgb_eotf(u: f64) -> f64 {
    if u <= 0.040_448_236_277_105_097 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn xyz_to_lab_f(t: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    if t > DELTA * DELTA * DELTA {
        t.cbrt()
    } else {
        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

/// Convert an sRGB-encoded RGBA8 texel to Lab (spec §4.4; task A16). The alpha
/// channel is ignored (the difference metric is on color); the transfer/matrix
/// path is sRGB→linear→XYZ(D65)→Lab(D65). Self-consistent for golden and
/// cross-backend comparison.
pub fn srgb8_to_lab(rgba: [u8; 4]) -> Lab {
    let r = srgb_eotf(rgba[0] as f64 / 255.0);
    let g = srgb_eotf(rgba[1] as f64 / 255.0);
    let b = srgb_eotf(rgba[2] as f64 / 255.0);

    // sRGB (Rec.709 primaries) → XYZ, D65.
    let x =
        0.412_390_799_265_959_5 * r + 0.357_584_339_383_877_96 * g + 0.180_480_788_401_834_3 * b;
    let y = 0.212_639_005_871_510_36 * r + 0.715_168_678_767_756 * g + 0.072_192_315_360_733_7 * b;
    let z =
        0.019_330_818_715_591_82 * r + 0.119_194_779_794_625_88 * g + 0.950_532_152_249_660_6 * b;

    // D65 reference white.
    const XN: f64 = 0.950_489;
    const YN: f64 = 1.0;
    const ZN: f64 = 1.088_840;

    let fx = xyz_to_lab_f(x / XN);
    let fy = xyz_to_lab_f(y / YN);
    let fz = xyz_to_lab_f(z / ZN);

    Lab {
        l: 116.0 * fy - 16.0,
        a: 500.0 * (fx - fy),
        b: 200.0 * (fy - fz),
    }
}

/// ΔE2000 stats between two equally-sized RGBA8 images (spec §6; task A16).
///
/// Panics if the two slices differ in length (a caller bug: comparing
/// differently-sized images).
pub fn delta_e_stats(reference: &[[u8; 4]], sample: &[[u8; 4]]) -> DeltaEStats {
    assert_eq!(
        reference.len(),
        sample.len(),
        "delta_e_stats: image sizes differ ({} vs {})",
        reference.len(),
        sample.len()
    );
    if reference.is_empty() {
        return DeltaEStats {
            mean: 0.0,
            p99: 0.0,
            max: 0.0,
        };
    }
    let mut deltas: Vec<f64> = reference
        .iter()
        .zip(sample.iter())
        .map(|(&r, &s)| ciede2000(srgb8_to_lab(r), srgb8_to_lab(s)))
        .collect();
    let sum: f64 = deltas.iter().sum();
    let mean = sum / deltas.len() as f64;
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let max = *deltas.last().unwrap();
    // p99: nearest-rank on the sorted deltas.
    let idx = (((deltas.len() as f64) * 0.99).ceil() as usize).saturating_sub(1);
    let p99 = deltas[idx.min(deltas.len() - 1)];
    DeltaEStats { mean, p99, max }
}

/// Largest absolute difference in any channel of any pixel, alpha included,
/// between two equally-sized RGBA8 byte buffers. `0` means byte-identical.
pub fn max_channel_delta(reference: &[u8], sample: &[u8]) -> u8 {
    assert_eq!(
        reference.len(),
        sample.len(),
        "max_channel_delta: buffer sizes differ ({} vs {})",
        reference.len(),
        sample.len()
    );
    reference
        .iter()
        .zip(sample.iter())
        .map(|(&r, &s)| r.abs_diff(s))
        .max()
        .unwrap_or(0)
}

/// The **bit-adjacent** parity gate: every channel of every pixel, alpha
/// included, differs by at most one 8-bit code, and PSNR clears the §4.4
/// floor. Use it for CPU-versus-GPU parity on content where the §4.4 ΔE2000
/// gate is unsatisfiable, and nowhere else.
///
/// # Why a second gate exists
///
/// The house gate is `max ΔE2000 <= 1.0` ([`TOLERANCE_DELTA_E`]). Near
/// neutral, the CIEDE2000 chroma term is unattenuated (`S_C` is about 1), so a
/// one-code move in **opposite** directions on two channels scores well above
/// 1.0 even though the two pixels are as close as 8-bit output can be without
/// being identical. Measured with this crate's own [`ciede2000`]:
///
/// | grey | `(v+1, v-1, v)` | `(v+1, v, v)` |
/// |---|---|---|
/// | 32  | 1.754 | |
/// | 64  | 1.585 | |
/// | 96  | 1.485 | |
/// | 128 | 1.415 | 0.575 |
/// | 192 | 1.311 | |
/// | 224 | 1.273 | |
///
/// A gate that two bit-adjacent images cannot pass is measuring the metric,
/// not the renders. Kernels with a steep, data-dependent gradient under every
/// pixel (a bilateral filter's weights, a per-pixel noise field) leave many
/// pixels sitting exactly on a rounding boundary, and one ULP of `pow` or
/// fused-multiply-add difference between backends flips them. That is the
/// only situation this gate is for.
///
/// # What it does and does not bound
///
/// It has no ΔE arm on purpose. Under a one-code bound the worst attainable
/// ΔE2000 is about 2.35, at `(24,24,24)` against `(25,23,25)`, and that is a
/// pixel differing by one code in each channel, which is the definition of
/// bit-adjacent. Bounding ΔE here would reintroduce the problem the gate
/// exists to remove.
///
/// The PSNR arm is redundant and kept as a printed diagnostic: a one-code
/// bound implies MSE at most 1, so PSNR is at least `10 * log10(255^2)`, which
/// is 48.13 dB, above the 45 dB floor. Conversely 45 dB alone permits an RMS
/// error of 1.43 codes, looser than this gate, so the one-code bound is the
/// arm that does the work.
///
/// Neither gate subsumes the other. A two-code single-channel move can pass
/// the house gate at ΔE 0.8 and fail this one; a one-code opposite move can
/// pass this one at ΔE 1.75 and fail the house gate. Tests that use this gate
/// must print both numbers so the choice stays reviewable.
///
/// # Discipline
///
/// Do not apply this gate to a case the house gate passes. It was applied to
/// five cases when first introduced; measured, three of them passed the house
/// gate and were reverted. A relaxation earns its place with a number.
///
/// Goldens never use it. A golden is one backend against its own rendered
/// PNG, so there is no cross-backend ULP divergence to absorb.
pub fn bit_adjacent(reference: &[u8], sample: &[u8]) -> bool {
    max_channel_delta(reference, sample) <= 1 && psnr(reference, sample) >= TOLERANCE_PSNR_DB
}

/// The one failure a golden cannot catch on its own, made a gate.
///
/// A golden proves a render has not changed. It cannot prove the render was
/// ever right. If a golden is blessed from a render where the node was
/// elided, or a parameter never reached the kernel, or the corpus was one
/// the operator is mathematically inert on, the golden pins that no-op and
/// stays green forever. Two cases in this repository shipped that way: a
/// defringe golden over a corpus with no fringe in it, and a sharpening
/// golden over a linear gradient, where the Gaussian of a ramp is the ramp
/// and the sharpened output sat within the house gate of the unsharpened
/// one.
///
/// So every golden runner renders the same source under the identity recipe
/// first and calls this with both, requiring the edit to have moved the
/// picture by at least `min_de` ΔE2000 somewhere. `2.0` is the working
/// floor: twice the house gate, so a difference the gate could not tell
/// from noise cannot pass as evidence. Returns the stats so the runner can
/// print them beside the golden's.
///
/// Extent-changing edits (a crop) are self-evidently not no-ops and have
/// nothing to compare texel for texel; callers skip this for those and say
/// so in their log line.
pub fn assert_edit_is_not_a_no_op(
    identity: &[[u8; 4]],
    edited: &[[u8; 4]],
    label: &str,
    min_de: f64,
) -> DeltaEStats {
    let stats = delta_e_stats(identity, edited);
    assert!(
        stats.max > min_de,
        "[{label}] the edit changed nothing measurable against an identity render \
         (ΔE2000 max {:.4}, floor {min_de}); a golden of this render would pin a no-op",
        stats.max
    );
    stats
}

/// PSNR (dB) between two equally-sized RGBA8 byte buffers (spec §6; task A16).
/// Identical inputs return [`f64::INFINITY`] (which passes any dB floor).
pub fn psnr(reference: &[u8], sample: &[u8]) -> f64 {
    assert_eq!(
        reference.len(),
        sample.len(),
        "psnr: buffer sizes differ ({} vs {})",
        reference.len(),
        sample.len()
    );
    if reference.is_empty() {
        return f64::INFINITY;
    }
    let mut sse = 0.0f64;
    for (&r, &s) in reference.iter().zip(sample.iter()) {
        let d = r as f64 - s as f64;
        sse += d * d;
    }
    let mse = sse / reference.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    let max = 255.0f64;
    10.0 * (max * max / mse).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Sharma-Wu-Dalal (2005) CIEDE2000 test data: `(Lab1, Lab2, ΔE00)`.
    /// The canonical 34-pair validation set from the paper's supplement.
    #[rustfmt::skip]
    const SHARMA: &[([f64; 3], [f64; 3], f64)] = &[
        ([50.0000,  2.6772, -79.7751], [50.0000,  0.0000, -82.7485], 2.0425),
        ([50.0000,  3.1571, -77.2803], [50.0000,  0.0000, -82.7485], 2.8615),
        ([50.0000,  2.8361, -74.0200], [50.0000,  0.0000, -82.7485], 3.4412),
        ([50.0000, -1.3802, -84.2814], [50.0000,  0.0000, -82.7485], 1.0000),
        ([50.0000, -1.1848, -84.8006], [50.0000,  0.0000, -82.7485], 1.0000),
        ([50.0000, -0.9009, -85.5211], [50.0000,  0.0000, -82.7485], 1.0000),
        ([50.0000,  0.0000,   0.0000], [50.0000, -1.0000,   2.0000], 2.3669),
        ([50.0000, -1.0000,   2.0000], [50.0000,  0.0000,   0.0000], 2.3669),
        ([50.0000,  2.4900,  -0.0010], [50.0000, -2.4900,   0.0009], 7.1792),
        ([50.0000,  2.4900,  -0.0010], [50.0000, -2.4900,   0.0010], 7.1792),
        ([50.0000,  2.4900,  -0.0010], [50.0000, -2.4900,   0.0011], 7.2195),
        ([50.0000,  2.4900,  -0.0010], [50.0000, -2.4900,   0.0012], 7.2195),
        ([50.0000, -0.0010,   2.4900], [50.0000,  0.0009,  -2.4900], 4.8045),
        ([50.0000, -0.0010,   2.4900], [50.0000,  0.0010,  -2.4900], 4.8045),
        ([50.0000, -0.0010,   2.4900], [50.0000,  0.0011,  -2.4900], 4.7461),
        ([50.0000,  2.5000,   0.0000], [50.0000,  0.0000,  -2.5000], 4.3065),
        ([50.0000,  2.5000,   0.0000], [73.0000, 25.0000, -18.0000], 27.1492),
        ([50.0000,  2.5000,   0.0000], [61.0000, -5.0000,  29.0000], 22.8977),
        ([50.0000,  2.5000,   0.0000], [56.0000, -27.0000, -3.0000], 31.9030),
        ([50.0000,  2.5000,   0.0000], [58.0000, 24.0000,  15.0000], 19.4535),
        ([50.0000,  2.5000,   0.0000], [50.0000,  3.1736,   0.5854], 1.0000),
        ([50.0000,  2.5000,   0.0000], [50.0000,  3.2972,   0.0000], 1.0000),
        ([50.0000,  2.5000,   0.0000], [50.0000,  1.8634,   0.5757], 1.0000),
        ([50.0000,  2.5000,   0.0000], [50.0000,  3.2592,   0.3350], 1.0000),
        ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
        ([63.0109, -31.0961, -5.8663], [62.8187, -29.7946, -4.0864], 1.2630),
        ([61.2901,  3.7196,  -5.3901], [61.4292,  2.2480,  -4.9620], 1.8731),
        ([35.0831, -44.1164,  3.7933], [35.0232, -40.0716,  1.5901], 1.8645),
        ([22.7233, 20.0904, -46.6940], [23.0331, 14.9730, -42.5619], 2.0373),
        ([36.4612, 47.8580, 18.3852],  [36.2715, 50.5065, 21.2231], 1.4146),
        ([90.8027, -2.0831,  1.4410],  [91.1528, -1.6435,  0.0447], 1.4441),
        ([90.9257, -0.5406, -0.9208],  [88.6381, -0.8985, -0.7239], 1.5381),
        ([ 6.7747, -0.2908, -2.4247],  [ 5.8714, -0.0985, -2.2286], 0.6377),
        ([ 2.0776,  0.0795, -1.1350],  [ 0.9033, -0.0636, -0.5514], 0.9082),
    ];

    #[test]
    fn ciede2000_matches_sharma_vectors() {
        for (i, &(a, b, expected)) in SHARMA.iter().enumerate() {
            let got = ciede2000(Lab::new(a[0], a[1], a[2]), Lab::new(b[0], b[1], b[2]));
            assert!(
                (got - expected).abs() < 1e-4,
                "pair {i}: got {got:.5}, expected {expected:.5}"
            );
        }
    }

    #[test]
    fn ciede2000_is_zero_for_identical_colors() {
        let c = Lab::new(42.0, 12.0, -7.0);
        assert!(ciede2000(c, c).abs() < 1e-12);
    }

    #[test]
    fn psnr_infinite_for_identical_and_finite_for_differing() {
        let a = vec![10u8, 20, 30, 255];
        assert!(psnr(&a, &a).is_infinite());
        let b = vec![11u8, 20, 30, 255];
        let p = psnr(&a, &b);
        assert!(p.is_finite() && p > 40.0, "psnr={p}");
    }

    #[test]
    fn delta_e_stats_zero_for_identical_images() {
        let img = vec![[10u8, 128, 200, 255], [0, 0, 0, 255], [255, 255, 255, 255]];
        let stats = delta_e_stats(&img, &img);
        assert_eq!(stats.max, 0.0);
        assert!(stats.within_tolerance());
    }

    #[test]
    fn delta_e_stats_flags_a_visible_difference() {
        let a = vec![[128u8, 128, 128, 255]];
        let b = vec![[200u8, 60, 60, 255]];
        let stats = delta_e_stats(&a, &b);
        assert!(stats.max > TOLERANCE_DELTA_E, "max={}", stats.max);
        assert!(!stats.within_tolerance());
    }

    /// The table in `bit_adjacent`'s docs, pinned. If CIEDE2000 or the sRGB
    /// to Lab path ever changes, the numbers that justify the second gate
    /// change with them, and this is where that shows up.
    #[test]
    fn a_one_code_opposite_move_near_neutral_exceeds_the_house_gate() {
        let de = |a: [u8; 3], b: [u8; 3]| {
            ciede2000(
                srgb8_to_lab([a[0], a[1], a[2], 255]),
                srgb8_to_lab([b[0], b[1], b[2], 255]),
            )
        };
        let expected = [
            (32u8, 1.754),
            (64, 1.585),
            (96, 1.485),
            (128, 1.415),
            (192, 1.311),
            (224, 1.273),
        ];
        for (v, want) in expected {
            let got = de([v, v, v], [v + 1, v - 1, v]);
            assert!(
                (got - want).abs() < 0.005,
                "grey {v}: (v+1, v-1, v) scored {got:.3}, the docs say {want}"
            );
            assert!(
                got > TOLERANCE_DELTA_E,
                "grey {v}: a one-code opposite move must exceed the house gate, got {got:.3}"
            );
        }
        // A single-channel one-code move stays comfortably inside it.
        let single = de([128, 128, 128], [129, 128, 128]);
        assert!(
            (single - 0.575).abs() < 0.005,
            "single-channel: {single:.3}"
        );
        assert!(single < TOLERANCE_DELTA_E);
    }

    /// The gate itself: one code everywhere passes, two codes anywhere fails,
    /// and alpha counts.
    #[test]
    fn bit_adjacent_counts_every_channel_including_alpha() {
        let a = [100u8, 100, 100, 255, 50, 60, 70, 255];
        let one = [101u8, 99, 100, 255, 50, 61, 70, 254];
        assert_eq!(max_channel_delta(&a, &one), 1);
        assert!(bit_adjacent(&a, &one));
        let two_rgb = [102u8, 100, 100, 255, 50, 60, 70, 255];
        assert_eq!(max_channel_delta(&a, &two_rgb), 2);
        assert!(!bit_adjacent(&a, &two_rgb));
        let two_alpha = [100u8, 100, 100, 253, 50, 60, 70, 255];
        assert_eq!(max_channel_delta(&a, &two_alpha), 2);
        assert!(!bit_adjacent(&a, &two_alpha), "alpha must count");
        assert!(bit_adjacent(&a, &a), "identical is trivially adjacent");
    }
}
