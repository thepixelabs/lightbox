// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! sRGB → CIELAB conversion and the CIEDE2000 color difference.
//!
//! ΔE2000 is computed "through Lab from sRGB" (spec §5 T22): 8-bit sRGB is
//! EOTF-decoded to linear light, converted to CIE XYZ (D65), then to CIELAB
//! (D65 reference white), and differenced with the Sharma et al. (2005)
//! CIEDE2000 formulation, validated below against the paper's published
//! test pairs.

/// D65 reference white (2° observer), the sRGB white point.
const D65: [f64; 3] = [0.950_47, 1.0, 1.088_83];

/// 25⁷, CIEDE2000's chroma normalization constant.
const POW25_7: f64 = 6_103_515_625.0;

/// sRGB EOTF: one encoded channel in `0.0..=1.0` to linear light.
fn srgb_eotf(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// One 8-bit sRGB pixel to CIELAB `[L*, a*, b*]` (D65).
pub fn srgb8_to_lab(rgb: [u8; 3]) -> [f64; 3] {
    let r = srgb_eotf(f64::from(rgb[0]) / 255.0);
    let g = srgb_eotf(f64::from(rgb[1]) / 255.0);
    let b = srgb_eotf(f64::from(rgb[2]) / 255.0);

    // Linear sRGB → XYZ, D65 (IEC 61966-2-1 primaries).
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;

    // XYZ → Lab (CIE 1976), D65 reference white.
    let f = |t: f64| -> f64 {
        const DELTA3: f64 = 216.0 / 24_389.0; // (6/29)³
        const KAPPA: f64 = 24_389.0 / 27.0; // (29/3)³
        if t > DELTA3 {
            t.cbrt()
        } else {
            (KAPPA * t + 16.0) / 116.0
        }
    };
    let fx = f(x / D65[0]);
    let fy = f(y / D65[1]);
    let fz = f(z / D65[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Hue angle in degrees `0.0..360.0`; 0 for the (0, 0) singularity.
fn hue_deg(a: f64, b: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let h = b.atan2(a).to_degrees();
    if h < 0.0 {
        h + 360.0
    } else {
        h
    }
}

/// CIEDE2000 color difference between two CIELAB values (kL = kC = kH = 1),
/// per Sharma, Wu & Dalal, "The CIEDE2000 Color-Difference Formula:
/// Implementation Notes, …" (2005).
pub fn ciede2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;

    // Step 1: C', h'.
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_bar = 0.5 * (c1 + c2);
    let c_bar7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + POW25_7)).sqrt());
    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = a1p.hypot(b1);
    let c2p = a2p.hypot(b2);
    let h1p = hue_deg(a1p, b1);
    let h2p = hue_deg(a2p, b2);

    // Step 2: ΔL', ΔC', ΔH'.
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh_deg = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > 180.0 {
            d - 360.0
        } else if d < -180.0 {
            d + 360.0
        } else {
            d
        }
    };
    let dh = 2.0 * (c1p * c2p).sqrt() * (dh_deg.to_radians() / 2.0).sin();

    // Step 3: weighting functions around the means.
    let l_bar = 0.5 * (l1 + l2);
    let c_bar_p = 0.5 * (c1p + c2p);
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else {
        let sum = h1p + h2p;
        if (h1p - h2p).abs() <= 180.0 {
            0.5 * sum
        } else if sum < 360.0 {
            0.5 * (sum + 360.0)
        } else {
            0.5 * (sum - 360.0)
        }
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let c_bar_p7 = c_bar_p.powi(7);
    let rc = 2.0 * (c_bar_p7 / (c_bar_p7 + POW25_7)).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar_p;
    let sh = 1.0 + 0.015 * c_bar_p * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;

    let dl_ = dl / sl;
    let dc_ = dc / sc;
    let dh_ = dh / sh;
    (dl_ * dl_ + dc_ * dc_ + dh_ * dh_ + rt * dc_ * dh_).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test pairs from Sharma, Wu & Dalal (2005), Table 1, the canonical
    /// CIEDE2000 verification data (subset spanning every branch: the hue
    /// discontinuities, the RT rotation region, chroma singularities).
    #[test]
    fn ciede2000_sharma_test_pairs() {
        #[rustfmt::skip]
        let cases: &[([f64; 3], [f64; 3], f64)] = &[
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, -1.3802, -84.2814], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, -1.1848, -84.8006], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, -0.9009, -85.5211], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, 0.0, 0.0],          [50.0, -1.0, 2.0],     2.3669),
            ([50.0, -1.0, 2.0],         [50.0, 0.0, 0.0],      2.3669),
            ([50.0, 2.4900, -0.0010],   [50.0, -2.4900, 0.0009], 7.1792),
            ([50.0, 2.4900, -0.0010],   [50.0, -2.4900, 0.0011], 7.2195),
            ([50.0, 2.5000, 0.0],       [50.0, 0.0, -2.5000],  4.3065),
            ([50.0, 2.5000, 0.0],       [73.0, 25.0, -18.0],   27.1492),
            ([50.0, 2.5000, 0.0],       [61.0, -5.0, 29.0],    22.8977),
            ([50.0, 2.5000, 0.0],       [56.0, -27.0, -3.0],   31.9030),
            ([50.0, 2.5000, 0.0],       [58.0, 24.0, 15.0],    19.4535),
            ([50.0, 2.5000, 0.0],       [50.0, 3.2592, 0.3350], 1.0000),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([90.9257, -0.5406, -0.9208],  [88.6381, -0.8985, -0.7239],  1.5381),
            ([35.0831, -44.1164, 3.7933],  [35.0232, -40.0716, 1.5901],  1.8645),
        ];
        for (lab1, lab2, expected) in cases {
            let de = ciede2000(*lab1, *lab2);
            assert!(
                (de - expected).abs() < 1e-4,
                "ΔE00({lab1:?}, {lab2:?}) = {de:.5}, expected {expected:.4}"
            );
        }
    }

    #[test]
    fn ciede2000_is_symmetric_and_zero_on_identity() {
        let a = [43.2, 12.4, -33.1];
        let b = [44.1, 10.0, -30.0];
        assert_eq!(ciede2000(a, a), 0.0);
        assert!((ciede2000(a, b) - ciede2000(b, a)).abs() < 1e-12);
    }

    #[test]
    fn srgb_to_lab_anchors() {
        let white = srgb8_to_lab([255, 255, 255]);
        assert!((white[0] - 100.0).abs() < 1e-3, "white L*: {}", white[0]);
        assert!(white[1].abs() < 1e-2 && white[2].abs() < 1e-2);

        let black = srgb8_to_lab([0, 0, 0]);
        assert!(black[0].abs() < 1e-9);
        assert!(black[1].abs() < 1e-9 && black[2].abs() < 1e-9);

        // Neutral grays stay neutral (a*/b* at f64 matrix-rounding noise,
        // orders of magnitude below perception) and L* is monotonic.
        let mut prev_l = -1.0;
        for v in [1u8, 32, 64, 118, 160, 200, 254] {
            let lab = srgb8_to_lab([v, v, v]);
            assert!(lab[1].abs() < 1e-3 && lab[2].abs() < 1e-3, "gray {v}");
            assert!(lab[0] > prev_l, "L* monotonic at {v}");
            prev_l = lab[0];
        }

        // sRGB red, published Lab (D65): ~ (53.24, 80.09, 67.20).
        let red = srgb8_to_lab([255, 0, 0]);
        assert!((red[0] - 53.24).abs() < 0.05, "red L* {}", red[0]);
        assert!((red[1] - 80.09).abs() < 0.05, "red a* {}", red[1]);
        assert!((red[2] - 67.20).abs() < 0.05, "red b* {}", red[2]);
    }
}
