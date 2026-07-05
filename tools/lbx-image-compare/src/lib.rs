// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lbx-image-compare` — perceptual golden-image comparison.
//!
//! Owned by **E01** (spec §5 T22); implemented in E01 Phase 6: CIEDE2000
//! (through Lab from sRGB) + PSNR comparison, failure reports with diff
//! heatmaps, and the `LIGHTBOX_BLESS=1` golden-regeneration workflow over the
//! committed layout `crates/lightbox-render/goldens/<node>/<pv>/<case>.png`.
//! Every pixel epic after E01 (E02, E05, E09, E10, …) extends this harness.
//!
//! # Tolerances (architecture §4.4)
//!
//! The PR gate for pixel output is **max ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB**
//! ([`GOLDEN_TOLERANCE`]) — perceptual, not bit-exact, so CPU/GPU and
//! cross-backend float differences (±1 LSB) pass while real color changes
//! (a 2° hue rotation) fail. Both directions are pinned by tests here.
//!
//! # What is measured
//!
//! * **ΔE2000** per pixel over the RGB channels (sRGB → linear → XYZ(D65) →
//!   Lab → CIEDE2000), reported as mean / p99 / max.
//! * **PSNR** over all four RGBA channels (alpha differences are a real
//!   defect even though Lab has no room for them). `+∞` for identical
//!   buffers.

mod color;
mod golden;
mod image;

pub use color::{ciede2000, srgb8_to_lab};
pub use golden::{
    bless_requested, check_golden, GoldenConfig, GoldenError, GoldenMismatch, GoldenOutcome,
    GoldenSpec,
};
pub use image::Rgba8Image;

/// Comparison errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompareError {
    /// The images have different sizes — never tolerated, whatever the ΔE.
    #[error("dimension mismatch: {a_width}x{a_height} vs {b_width}x{b_height}")]
    DimensionMismatch {
        /// First image width.
        a_width: u32,
        /// First image height.
        a_height: u32,
        /// Second image width.
        b_width: u32,
        /// Second image height.
        b_height: u32,
    },
    /// A pixel buffer that does not describe what it claims.
    #[error("bad image: {0}")]
    BadImage(String),
    /// PNG encode/decode failure.
    #[error("png: {0}")]
    Png(String),
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(String),
}

/// A pass bar for [`CompareReport::passes`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tolerance {
    /// Maximum allowed per-pixel ΔE2000.
    pub max_de: f64,
    /// Minimum required PSNR in dB.
    pub min_psnr_db: f64,
}

impl std::fmt::Display for Tolerance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(max ΔE2000 ≤ {}, PSNR ≥ {} dB)",
            self.max_de, self.min_psnr_db
        )
    }
}

/// The §4.4 golden/parity gate: max ΔE2000 ≤ 1.0 **and** PSNR ≥ 45 dB.
pub const GOLDEN_TOLERANCE: Tolerance = Tolerance {
    max_de: 1.0,
    min_psnr_db: 45.0,
};

/// The measured difference between two same-sized images.
#[derive(Clone, Debug)]
pub struct CompareReport {
    /// Image width (both images).
    pub width: u32,
    /// Image height (both images).
    pub height: u32,
    /// Mean per-pixel ΔE2000 (RGB).
    pub mean_de: f64,
    /// 99th-percentile per-pixel ΔE2000 (nearest-rank).
    pub p99_de: f64,
    /// Maximum per-pixel ΔE2000.
    pub max_de: f64,
    /// PSNR over all RGBA channels, dB; `+∞` when identical.
    pub psnr_db: f64,
    /// Pixels differing in any channel (incl. alpha).
    pub differing_px: u64,
}

impl CompareReport {
    /// True when this difference is within `tol`.
    pub fn passes(&self, tol: Tolerance) -> bool {
        self.max_de <= tol.max_de && self.psnr_db >= tol.min_psnr_db
    }
}

impl std::fmt::Display for CompareReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ΔE2000 mean {:.4} / p99 {:.4} / max {:.4}, PSNR {:.2} dB, {} of {} px differ",
            self.mean_de,
            self.p99_de,
            self.max_de,
            self.psnr_db,
            self.differing_px,
            u64::from(self.width) * u64::from(self.height),
        )
    }
}

/// Per-pixel ΔE2000 (RGB through Lab). Errors on dimension mismatch.
fn per_pixel_de(a: &Rgba8Image, b: &Rgba8Image) -> Result<Vec<f64>, CompareError> {
    if (a.width, a.height) != (b.width, b.height) {
        return Err(CompareError::DimensionMismatch {
            a_width: a.width,
            a_height: a.height,
            b_width: b.width,
            b_height: b.height,
        });
    }
    Ok(a.px
        .chunks_exact(4)
        .zip(b.px.chunks_exact(4))
        .map(|(pa, pb)| {
            if pa[..3] == pb[..3] {
                0.0 // identical sRGB bytes — skip the Lab trip
            } else {
                ciede2000(
                    srgb8_to_lab([pa[0], pa[1], pa[2]]),
                    srgb8_to_lab([pb[0], pb[1], pb[2]]),
                )
            }
        })
        .collect())
}

/// Compares two images: per-pixel ΔE2000 statistics + RGBA PSNR.
pub fn compare(a: &Rgba8Image, b: &Rgba8Image) -> Result<CompareReport, CompareError> {
    let mut des = per_pixel_de(a, b)?;
    let n = des.len().max(1);

    let mean_de = des.iter().sum::<f64>() / n as f64;
    let max_de = des.iter().copied().fold(0.0, f64::max);
    // Nearest-rank p99.
    des.sort_unstable_by(|x, y| x.partial_cmp(y).expect("ΔE is never NaN"));
    let p99_idx = ((0.99 * n as f64).ceil() as usize).clamp(1, n) - 1;
    let p99_de = des[p99_idx];

    let mut sq_err = 0.0f64;
    let mut differing_px = 0u64;
    for (pa, pb) in a.px.chunks_exact(4).zip(b.px.chunks_exact(4)) {
        if pa != pb {
            differing_px += 1;
        }
        for (&ca, &cb) in pa.iter().zip(pb) {
            let d = f64::from(ca) - f64::from(cb);
            sq_err += d * d;
        }
    }
    let mse = sq_err / (a.px.len().max(1)) as f64;
    let psnr_db = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * ((255.0 * 255.0) / mse).log10()
    };

    Ok(CompareReport {
        width: a.width,
        height: a.height,
        mean_de,
        p99_de,
        max_de,
        psnr_db,
        differing_px,
    })
}

/// ΔE saturating the heatmap ramp (everything at or above renders white-hot).
const HEATMAP_MAX_DE: f64 = 4.0;

/// Renders the per-pixel ΔE2000 as a heatmap (black → red → yellow → white,
/// saturating at ΔE [`HEATMAP_MAX_DE`]) — the failure artifact humans and CI
/// look at (spec §5 T22).
pub fn diff_heatmap(a: &Rgba8Image, b: &Rgba8Image) -> Result<Rgba8Image, CompareError> {
    let des = per_pixel_de(a, b)?;
    let mut px = Vec::with_capacity(des.len() * 4);
    for de in des {
        let t = (de / HEATMAP_MAX_DE).clamp(0.0, 1.0);
        let ramp = |lo: f64| ((t * 3.0 - lo).clamp(0.0, 1.0) * 255.0).round() as u8;
        px.extend_from_slice(&[ramp(0.0), ramp(1.0), ramp(2.0), 255]);
    }
    Rgba8Image::new(a.width, a.height, px)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colorful test card: smooth gradients plus fully saturated primary
    /// bars (saturated colors are where hue errors hurt the most — and where
    /// ΔE2000 must catch them).
    fn test_card() -> Rgba8Image {
        let (w, h) = (48u32, 32u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let p: [u8; 4] = match y * 4 / h {
                    0 => [255, 0, 0, 255],                                     // saturated red
                    1 => [0, 255, 0, 255],                                     // saturated green
                    2 => [0, 128, 255, 255],                                   // saturated azure
                    _ => [(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255], // gradient
                };
                px.extend_from_slice(&p);
            }
        }
        Rgba8Image::new(w, h, px).unwrap()
    }

    /// RGB → HSV → RGB hue rotation by `deg` (test injector for the T22 AC).
    fn rotate_hue(img: &Rgba8Image, deg: f64) -> Rgba8Image {
        let mut out = img.clone();
        for p in out.px.chunks_exact_mut(4) {
            let (r, g, b) = (
                f64::from(p[0]) / 255.0,
                f64::from(p[1]) / 255.0,
                f64::from(p[2]) / 255.0,
            );
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let c = max - min;
            let mut hue = if c == 0.0 {
                0.0
            } else if max == r {
                60.0 * (((g - b) / c) % 6.0)
            } else if max == g {
                60.0 * ((b - r) / c + 2.0)
            } else {
                60.0 * ((r - g) / c + 4.0)
            };
            hue = (hue + deg).rem_euclid(360.0);
            let (v, s) = (max, if max == 0.0 { 0.0 } else { c / max });
            let cc = v * s;
            let x = cc * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
            let m = v - cc;
            let (r1, g1, b1) = match (hue / 60.0) as u32 {
                0 => (cc, x, 0.0),
                1 => (x, cc, 0.0),
                2 => (0.0, cc, x),
                3 => (0.0, x, cc),
                4 => (x, 0.0, cc),
                _ => (cc, 0.0, x),
            };
            p[0] = ((r1 + m) * 255.0).round() as u8;
            p[1] = ((g1 + m) * 255.0).round() as u8;
            p[2] = ((b1 + m) * 255.0).round() as u8;
        }
        out
    }

    #[test]
    fn identical_images_are_perfect() {
        let img = test_card();
        let report = compare(&img, &img).unwrap();
        assert_eq!(report.max_de, 0.0);
        assert_eq!(report.mean_de, 0.0);
        assert!(report.psnr_db.is_infinite());
        assert_eq!(report.differing_px, 0);
        assert!(report.passes(GOLDEN_TOLERANCE));
    }

    /// T22 AC (must flag): an injected 2° hue rotation fails the §4.4 gate.
    #[test]
    fn two_degree_hue_rotation_is_flagged() {
        let img = test_card();
        let rotated = rotate_hue(&img, 2.0);
        let report = compare(&img, &rotated).unwrap();
        assert!(
            report.max_de > GOLDEN_TOLERANCE.max_de,
            "2° hue rotation must exceed max ΔE 1.0, got {report}"
        );
        assert!(
            !report.passes(GOLDEN_TOLERANCE),
            "2° hue rotation must fail the golden gate: {report}"
        );
    }

    /// T22 AC (must pass): a ±1 LSB dither — the size of legitimate CPU/GPU
    /// rounding differences — stays inside the gate.
    #[test]
    fn one_lsb_dither_passes() {
        let img = test_card();
        let mut dithered = img.clone();
        for (i, p) in dithered.px.chunks_exact_mut(4).enumerate() {
            for (c, ch) in p.iter_mut().take(3).enumerate() {
                // Deterministic alternating ±1 per pixel and channel.
                *ch = if (i + c) % 2 == 0 {
                    ch.saturating_add(1)
                } else {
                    ch.saturating_sub(1)
                };
            }
        }
        let report = compare(&img, &dithered).unwrap();
        assert!(report.max_de > 0.0, "dither did change pixels");
        assert!(
            report.passes(GOLDEN_TOLERANCE),
            "±1 LSB dither must pass the golden gate: {report}"
        );
    }

    #[test]
    fn psnr_alone_catches_alpha_changes() {
        let img = test_card();
        let mut alpha_broken = img.clone();
        for p in alpha_broken.px.chunks_exact_mut(4) {
            p[3] = 0;
        }
        let report = compare(&img, &alpha_broken).unwrap();
        assert_eq!(report.max_de, 0.0, "Lab is blind to alpha");
        assert!(
            !report.passes(GOLDEN_TOLERANCE),
            "PSNR leg must catch alpha destruction: {report}"
        );
    }

    #[test]
    fn dimension_mismatch_errors() {
        let a = test_card();
        let b = Rgba8Image::new(1, 1, vec![0, 0, 0, 255]).unwrap();
        assert!(matches!(
            compare(&a, &b),
            Err(CompareError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn heatmap_is_black_on_match_and_hot_on_change() {
        let img = test_card();
        let map = diff_heatmap(&img, &img).unwrap();
        assert!(map.px.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));

        let mut broken = img.clone();
        // Invert one pixel's green channel: a huge local ΔE.
        broken.px[1] = 255 - broken.px[1];
        let map = diff_heatmap(&img, &broken).unwrap();
        let first = &map.px[0..4];
        assert!(first[0] > 200, "hot pixel renders hot, got {first:?}");
        assert!(
            map.px[4..].chunks_exact(4).all(|p| p == [0, 0, 0, 255]),
            "untouched pixels stay black"
        );
    }

    #[test]
    fn p99_is_robust_to_a_single_outlier() {
        let img = test_card();
        let mut one_bad = img.clone();
        one_bad.px[0] = 255 - one_bad.px[0];
        let report = compare(&img, &one_bad).unwrap();
        assert!(report.max_de > 1.0, "the outlier registers in max");
        assert_eq!(report.p99_de, 0.0, "one pixel in 1536 is beyond p99");
        assert_eq!(report.differing_px, 1);
    }
}
