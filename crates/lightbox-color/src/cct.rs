// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Correlated-color-temperature module (spec §3.3). **Owner: Phase B (B2).**
//! DNG-compatible Planckian/daylight locus + orthogonal tint, using the
//! Robertson 31-point isotemperature table in the CIE 1960 UCS `(u, v)` space —
//! the exact construction the DNG SDK (`dng_temperature`) uses, so as-shot
//! neutrals land on the same locus Adobe/dcamprof-derived data assumes.

/// One Robertson isotemperature line: reciprocal megakelvin `r`, the locus point
/// `(u, v)` in CIE 1960 UCS, and the isotherm slope `t`.
struct IsoTemp {
    r: f64,
    u: f64,
    v: f64,
    t: f64,
}

/// The Robertson 31-point table (Wyszecki & Stiles; the values the DNG SDK
/// ships). `r` is reciprocal megakelvin (`1e6 / CCT`).
#[rustfmt::skip]
const ROBERTSON: [IsoTemp; 31] = [
    IsoTemp { r:   0.0, u: 0.18006, v: 0.26352, t: -0.24341 },
    IsoTemp { r:  10.0, u: 0.18066, v: 0.26589, t: -0.25479 },
    IsoTemp { r:  20.0, u: 0.18133, v: 0.26846, t: -0.26876 },
    IsoTemp { r:  30.0, u: 0.18208, v: 0.27119, t: -0.28539 },
    IsoTemp { r:  40.0, u: 0.18293, v: 0.27407, t: -0.30470 },
    IsoTemp { r:  50.0, u: 0.18388, v: 0.27709, t: -0.32675 },
    IsoTemp { r:  60.0, u: 0.18494, v: 0.28021, t: -0.35156 },
    IsoTemp { r:  70.0, u: 0.18611, v: 0.28342, t: -0.37915 },
    IsoTemp { r:  80.0, u: 0.18740, v: 0.28668, t: -0.40955 },
    IsoTemp { r:  90.0, u: 0.18880, v: 0.28997, t: -0.44278 },
    IsoTemp { r: 100.0, u: 0.19032, v: 0.29326, t: -0.47888 },
    IsoTemp { r: 125.0, u: 0.19462, v: 0.30141, t: -0.58204 },
    IsoTemp { r: 150.0, u: 0.19962, v: 0.30921, t: -0.70471 },
    IsoTemp { r: 175.0, u: 0.20525, v: 0.31647, t: -0.84901 },
    IsoTemp { r: 200.0, u: 0.21142, v: 0.32312, t: -1.0182 },
    IsoTemp { r: 225.0, u: 0.21807, v: 0.32909, t: -1.2168 },
    IsoTemp { r: 250.0, u: 0.22511, v: 0.33439, t: -1.4512 },
    IsoTemp { r: 275.0, u: 0.23247, v: 0.33904, t: -1.7298 },
    IsoTemp { r: 300.0, u: 0.24010, v: 0.34308, t: -2.0637 },
    IsoTemp { r: 325.0, u: 0.24792, v: 0.34655, t: -2.4681 },
    IsoTemp { r: 350.0, u: 0.25591, v: 0.34951, t: -2.9641 },
    IsoTemp { r: 375.0, u: 0.26400, v: 0.35200, t: -3.5814 },
    IsoTemp { r: 400.0, u: 0.27218, v: 0.35407, t: -4.3633 },
    IsoTemp { r: 425.0, u: 0.28039, v: 0.35577, t: -5.3762 },
    IsoTemp { r: 450.0, u: 0.28863, v: 0.35714, t: -6.7262 },
    IsoTemp { r: 475.0, u: 0.29685, v: 0.35823, t: -8.5955 },
    IsoTemp { r: 500.0, u: 0.30505, v: 0.35907, t: -11.324 },
    IsoTemp { r: 525.0, u: 0.31320, v: 0.35968, t: -15.628 },
    IsoTemp { r: 550.0, u: 0.32129, v: 0.36011, t: -23.325 },
    IsoTemp { r: 575.0, u: 0.32931, v: 0.36038, t: -40.770 },
    IsoTemp { r: 600.0, u: 0.33724, v: 0.36051, t: -116.45 },
];

/// The DNG tint scale: tint is the signed UCS distance off the locus, scaled by
/// this factor (`dng_temperature::kTintScale`).
const TINT_SCALE: f64 = -3000.0;

/// `xy` → CIE 1960 UCS `(u, v)`.
fn xy_to_uv(xy: [f64; 2]) -> (f64, f64) {
    let (x, y) = (xy[0], xy[1]);
    let denom = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / denom, 6.0 * y / denom)
}

/// CIE 1960 UCS `(u, v)` → `xy`.
fn uv_to_xy(u: f64, v: f64) -> [f64; 2] {
    let denom = 2.0 * u - 8.0 * v + 4.0;
    [3.0 * u / denom, 2.0 * v / denom]
}

/// `xy` chromaticity → `(CCT Kelvin, tint)` (spec §3.3). **Phase B (B2)**:
/// DNG-compatible Robertson locus; A/D50/D65 recovered within ±15 K.
pub fn xy_to_cct_tint(xy: [f64; 2]) -> (f64, f64) {
    let (u, v) = xy_to_uv(xy);

    let mut last_dt = 0.0;
    let mut last_du = 0.0;
    let mut last_dv = 0.0;

    for index in 1..ROBERTSON.len() {
        // Unit vector along the current isotherm, normalized.
        let mut du = 1.0;
        let mut dv = ROBERTSON[index].t;
        let len = (1.0 + dv * dv).sqrt();
        du /= len;
        dv /= len;

        // Signed distance from the sample to this isotherm.
        let uu = u - ROBERTSON[index].u;
        let vv = v - ROBERTSON[index].v;
        let mut dt = -uu * dv + vv * du;

        if dt <= 0.0 || index == ROBERTSON.len() - 1 {
            if dt > 0.0 {
                dt = 0.0;
            }
            dt = -dt;
            // Fraction between the bracketing isotherms.
            let f = if index == 1 { 0.0 } else { dt / (last_dt + dt) };
            let cct = 1.0e6 / (ROBERTSON[index - 1].r * f + ROBERTSON[index].r * (1.0 - f));

            // Tint: signed distance off the interpolated locus point along the
            // interpolated isotherm direction.
            let uu = u - (ROBERTSON[index - 1].u * f + ROBERTSON[index].u * (1.0 - f));
            let vv = v - (ROBERTSON[index - 1].v * f + ROBERTSON[index].v * (1.0 - f));
            let mut idu = du * (1.0 - f) + last_du * f;
            let mut idv = dv * (1.0 - f) + last_dv * f;
            let len = (idu * idu + idv * idv).sqrt();
            idu /= len;
            idv /= len;
            let tint = (uu * idu + vv * idv) * TINT_SCALE;
            return (cct, tint);
        }

        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }

    // Off the table's temperature range; clamp to the hottest entry.
    (1.0e6 / ROBERTSON[ROBERTSON.len() - 1].r, 0.0)
}

/// `(CCT Kelvin, tint)` → `xy` chromaticity (spec §3.3). **Phase B (B2)** —
/// inverse of [`xy_to_cct_tint`], round-trip within tolerance.
pub fn cct_tint_to_xy(cct: f64, tint: f64) -> [f64; 2] {
    let r = 1.0e6 / cct.clamp(1000.0, 40000.0);

    // Bracket the reciprocal temperature between two table rows.
    let mut index = 0usize;
    while index < ROBERTSON.len() - 2 && r >= ROBERTSON[index + 1].r {
        index += 1;
    }

    let hi = &ROBERTSON[index + 1];
    let lo = &ROBERTSON[index];
    // Fraction of the way from the hi row toward the lo row.
    let f = ((hi.r - r) / (hi.r - lo.r)).clamp(0.0, 1.0);

    let u = lo.u * f + hi.u * (1.0 - f);
    let v = lo.v * f + hi.v * (1.0 - f);

    // Interpolated, normalized isotherm direction at this temperature.
    let (mut uu1, mut vv1) = (1.0, lo.t);
    let len1 = (1.0 + vv1 * vv1).sqrt();
    uu1 /= len1;
    vv1 /= len1;
    let (mut uu2, mut vv2) = (1.0, hi.t);
    let len2 = (1.0 + vv2 * vv2).sqrt();
    uu2 /= len2;
    vv2 /= len2;
    let mut uu = uu1 * f + uu2 * (1.0 - f);
    let mut vv = vv1 * f + vv2 * (1.0 - f);
    let len = (uu * uu + vv * vv).sqrt();
    uu /= len;
    vv /= len;

    // Offset off the locus by the tint.
    let offset = tint / TINT_SCALE;
    let u = u + uu * offset;
    let v = v + vv * offset;

    uv_to_xy(u, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_illuminants_recovered_within_15k() {
        // (xy, expected CCT) for CIE illuminants A, D50, D65.
        let cases = [
            ([0.44757, 0.40745], 2856.0),
            ([0.34567, 0.35850], 5003.0),
            ([0.31272, 0.32903], 6504.0),
        ];
        for (xy, expected) in cases {
            let (cct, _tint) = xy_to_cct_tint(xy);
            assert!(
                (cct - expected).abs() <= 15.0,
                "xy={xy:?}: got {cct} K, expected {expected} K"
            );
        }
    }

    #[test]
    fn daylight_points_have_modest_tint_off_the_planckian_locus() {
        // The Robertson table is the *Planckian* locus; CIE D-series daylight
        // chromaticities sit slightly above it, so a faithful DNG-compatible
        // solve yields a small (non-zero) tint (~10), not zero. This is expected
        // and matches the DNG SDK's dng_temperature behaviour.
        for xy in [[0.34567, 0.35850], [0.31272, 0.32903]] {
            let (_cct, tint) = xy_to_cct_tint(xy);
            assert!(tint.abs() < 15.0, "xy={xy:?} tint={tint}");
        }
    }

    #[test]
    fn round_trip_over_locus_grid() {
        for step in 0..14 {
            let cct = 2500.0 + step as f64 * 500.0; // 2500..9000
            for &tint in &[-40.0, -10.0, 0.0, 10.0, 40.0] {
                let xy = cct_tint_to_xy(cct, tint);
                let (c2, t2) = xy_to_cct_tint(xy);
                assert!(
                    (c2 - cct).abs() / cct < 0.03,
                    "CCT round-trip {cct}->{c2} (tint {tint})"
                );
                assert!(
                    (t2 - tint).abs() < 2.0,
                    "tint round-trip {tint}->{t2} (cct {cct})"
                );
            }
        }
    }

    #[test]
    fn tint_is_orthogonal_and_signed() {
        // From a locus point, a +tint and a -tint offset land on opposite sides.
        let cct = 5500.0;
        let plus = cct_tint_to_xy(cct, 20.0);
        let minus = cct_tint_to_xy(cct, -20.0);
        let (_, tp) = xy_to_cct_tint(plus);
        let (_, tm) = xy_to_cct_tint(minus);
        assert!(tp > 10.0 && tm < -10.0, "tp={tp} tm={tm}");
        // The CCT is preserved across the tint offset.
        let (cp, _) = xy_to_cct_tint(plus);
        assert!((cp - cct).abs() / cct < 0.03);
    }
}
