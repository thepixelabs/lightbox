// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The E10.2 **B2 halo metric**, a test-support fn shared by every
//! recovery/local-contrast node's halo gate (spec §4.5: `tone_recovery`, task
//! D3's `clarity`). Operates on rendered sRGB8 texels (the same
//! `Vec<[u8;4]>` shape `tests/e10_*.rs` already produces via its own
//! `texels()` helper), so it composes directly with the existing golden/
//! parity harness in [`crate::corpus`]/[`crate::compare`].
//!
//! # The metric (spec §4.5)
//!
//! > along detected strong edges (Sobel magnitude > τ on the input), measure
//! > luminance gradient-reversal energy in a band of ±r px orthogonal to the
//! > edge between input and output, output must not introduce sign-flipped
//! > luma gradients above amplitude ε along edges where the input was
//! > monotonic, plus banded ΔE2000 vs a curve-only reference away from
//! > edges (the tool must actually recover, not just avoid halos).
//!
//! Concretely, per pixel at/above the Sobel-magnitude threshold `τ`
//! ([`HaloConfig::edge_threshold`]):
//! 1. Walk the **input** luma along the local gradient direction (the
//!    direction orthogonal to the edge tangent) from `-r` to `+r` px
//!    (bilinear-sampled); the sign of `input(+r) - input(-r)` is the edge's
//!    "expected direction" (skipped if it's exactly flat, not really an
//!    edge in this band).
//! 2. Walk the **output** luma along the same path; any step that moves
//!    *against* the expected direction is a local dip/overshoot, a
//!    gradient-reversal (halo). Its magnitude accumulates into that edge
//!    pixel's reversal energy.
//! 3. Off the detected edges (Sobel magnitude below `τ`), compare `output`
//!    against a caller-supplied `reference` (a curve-only / non-spatial
//!    render at the same recipe) via ΔE2000 ([`crate::compare::ciede2000`])
//!    this is the "actually recovers, not just avoids halos" half of the
//!    gate: a degenerate all-identity "recovery" would pass the reversal
//!    check trivially but fail this one.
//!
//! [`evaluate_halo`] aggregates both into one [`HaloReport`]; [`HaloConfig`]'s
//! thresholds gate `passed`. This is the **basic M1 version** named in
//! `docs/plan/epics/E10-deviations.md`: real threshold *calibration* from a
//! visual-board review (spec §4.5's "thresholds are fixed during the spike
//! from the visual boards") is B3-B6/B16 work, deferred to M2, the
//! thresholds here are chosen to cleanly separate a known-bad naive
//! unsharp-mask fixture from the shipped guided-filter node on synthetic
//! scenes (see `tests/e10_tone_recovery.rs`), not from a perceptual study.

use crate::compare::{ciede2000, srgb8_to_lab};

/// Config for [`evaluate_halo`], the thresholds a specific node/scene pairing
/// tunes (spec §4.5: "thresholds are fixed during the spike... then frozen
/// into CI", B6/B17, both M2-deferred here; this M1 metric ships with
/// caller-supplied thresholds rather than a single frozen constant).
#[derive(Clone, Copy, Debug)]
pub struct HaloConfig {
    /// Sobel gradient-magnitude threshold `τ` (on the input luma, `0..~1`
    /// scale) above which a pixel counts as a "strong edge".
    pub edge_threshold: f32,
    /// Half-width `r` (px) of the orthogonal walk on each side of an edge
    /// pixel.
    pub band_radius: i32,
    /// `mean_reversal_energy` must be `<=` this to pass.
    pub reversal_threshold: f64,
    /// `off_edge_delta_e_mean` must be `<=` this to pass.
    pub delta_e_threshold: f64,
}

impl HaloConfig {
    /// A reasonable default for a hard-edged synthetic step-scene at the
    /// `tests/e10_tone_recovery.rs` corpus scale: `band_radius = 6` covers
    /// the naive fixture's box-blur radius (see that test module), and the
    /// reversal/ΔE thresholds are picked (documented in that module) to
    /// cleanly separate the naive USM fixture (red) from the guided-filter
    /// node (green), not perceptually calibrated (M2 work, B6/B16).
    pub const SYNTHETIC_DEFAULT: HaloConfig = HaloConfig {
        edge_threshold: 0.08,
        band_radius: 6,
        reversal_threshold: 0.01,
        delta_e_threshold: 12.0,
    };
}

/// One [`evaluate_halo`] run's outcome.
#[derive(Clone, Copy, Debug)]
pub struct HaloReport {
    /// How many pixels cleared the Sobel edge threshold.
    pub edge_pixel_count: usize,
    /// Mean gradient-reversal energy over edge pixels (0 = no reversals
    /// detected anywhere).
    pub mean_reversal_energy: f64,
    /// The single worst edge pixel's reversal energy.
    pub max_reversal_energy: f64,
    /// How many pixels were compared against `reference` (below the edge
    /// threshold).
    pub off_edge_pixel_count: usize,
    /// Mean ΔE2000 vs `reference` over the off-edge pixels.
    pub off_edge_delta_e_mean: f64,
    /// Whether both the reversal and off-edge-ΔE checks passed
    /// [`HaloConfig`]'s thresholds.
    pub passed: bool,
}

/// Perceptual (Rec.709-weighted) luma from an sRGB8 texel, normalized to
/// `[0,1]`, the metric's own working plane (independent of the render
/// engine's ProPhoto working-space luma; this operates on already-rendered
/// display-referred output, per the metric's "along detected strong edges...
/// between input and output" contract, which is stated over rendered pixels).
fn srgb8_luma(p: [u8; 4]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Sobel gradient `(gx, gy)` of `luma` at `(x, y)`, clamp-to-edge border.
fn sobel_at(luma: &[f32], w: u32, h: u32, x: i32, y: i32) -> (f32, f32) {
    let get = |xx: i32, yy: i32| -> f32 {
        let cx = xx.clamp(0, w as i32 - 1);
        let cy = yy.clamp(0, h as i32 - 1);
        luma[(cy as usize) * (w as usize) + (cx as usize)]
    };
    let gx = (get(x + 1, y - 1) + 2.0 * get(x + 1, y) + get(x + 1, y + 1))
        - (get(x - 1, y - 1) + 2.0 * get(x - 1, y) + get(x - 1, y + 1));
    let gy = (get(x - 1, y + 1) + 2.0 * get(x, y + 1) + get(x + 1, y + 1))
        - (get(x - 1, y - 1) + 2.0 * get(x, y - 1) + get(x + 1, y - 1));
    (gx, gy)
}

/// Bilinear-sampled `plane` at `(x, y)`, clamped to the image bounds.
fn bilinear(plane: &[f32], w: u32, h: u32, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, w as f32 - 1.001);
    let y = y.clamp(0.0, h as f32 - 1.001);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x0 + 1).min(w as i32 - 1);
    let y1 = (y0 + 1).min(h as i32 - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let get = |xx: i32, yy: i32| plane[(yy as usize) * (w as usize) + (xx as usize)];
    let top = get(x0, y0) * (1.0 - fx) + get(x1, y0) * fx;
    let bot = get(x0, y1) * (1.0 - fx) + get(x1, y1) * fx;
    top * (1.0 - fy) + bot * fy
}

/// Runs the B2 halo metric: `input` is the pre-recovery render, `output` is
/// the candidate node's render, `reference` is a curve-only (non-spatial)
/// render at the same recipe, all `w`×`h` sRGB8 texel buffers (same layout
/// `tests/e10_*.rs`'s `texels()` helper produces). See the module docs for
/// the algorithm.
pub fn evaluate_halo(
    input: &[[u8; 4]],
    output: &[[u8; 4]],
    reference: &[[u8; 4]],
    w: u32,
    h: u32,
    cfg: HaloConfig,
) -> HaloReport {
    assert_eq!(input.len(), (w * h) as usize, "input size mismatch");
    assert_eq!(output.len(), (w * h) as usize, "output size mismatch");
    assert_eq!(reference.len(), (w * h) as usize, "reference size mismatch");

    let input_luma: Vec<f32> = input.iter().map(|&p| srgb8_luma(p)).collect();
    let output_luma: Vec<f32> = output.iter().map(|&p| srgb8_luma(p)).collect();

    let mut edge_pixel_count = 0usize;
    let mut sum_reversal = 0f64;
    let mut max_reversal = 0f64;
    let mut off_edge_deltas: Vec<f64> = Vec::new();

    let r = cfg.band_radius.max(1) as f32;
    let steps = cfg.band_radius.max(1) * 2;

    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (gx, gy) = sobel_at(&input_luma, w, h, x, y);
            let mag = (gx * gx + gy * gy).sqrt();
            let i = (y as usize) * (w as usize) + (x as usize);

            if mag >= cfg.edge_threshold {
                edge_pixel_count += 1;
                let len = mag.max(1e-6);
                let (dx, dy) = (gx / len, gy / len);

                let sample_input = |t: f32| -> f32 {
                    bilinear(&input_luma, w, h, x as f32 + dx * t, y as f32 + dy * t)
                };
                let sample_output = |t: f32| -> f32 {
                    bilinear(&output_luma, w, h, x as f32 + dx * t, y as f32 + dy * t)
                };

                let in_lo = sample_input(-r);
                let in_hi = sample_input(r);
                let expected_dir = (in_hi - in_lo).signum();
                if expected_dir == 0.0 {
                    // Not actually monotonic across the band (texture, not a
                    // clean edge), the spec explicitly scopes the check to
                    // "edges where the input was monotonic".
                    continue;
                }

                let mut reversal_here = 0f64;
                let mut prev = sample_output(-r);
                for s in 1..=steps {
                    let t = -r + s as f32 * (2.0 * r / steps as f32);
                    let cur = sample_output(t);
                    let step_delta = cur - prev;
                    let against = -expected_dir * step_delta;
                    if against > 0.0 {
                        reversal_here += against as f64;
                    }
                    prev = cur;
                }
                sum_reversal += reversal_here;
                if reversal_here > max_reversal {
                    max_reversal = reversal_here;
                }
            } else {
                let d = ciede2000(srgb8_to_lab(reference[i]), srgb8_to_lab(output[i]));
                off_edge_deltas.push(d);
            }
        }
    }

    let mean_reversal_energy = if edge_pixel_count > 0 {
        sum_reversal / edge_pixel_count as f64
    } else {
        0.0
    };
    let off_edge_pixel_count = off_edge_deltas.len();
    let off_edge_delta_e_mean = if off_edge_pixel_count > 0 {
        off_edge_deltas.iter().sum::<f64>() / off_edge_pixel_count as f64
    } else {
        0.0
    };
    let passed = mean_reversal_energy <= cfg.reversal_threshold
        && off_edge_delta_e_mean <= cfg.delta_e_threshold;

    HaloReport {
        edge_pixel_count,
        mean_reversal_energy,
        max_reversal_energy: max_reversal,
        off_edge_pixel_count,
        off_edge_delta_e_mean,
        passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<[u8; 4]> {
        vec![[rgb[0], rgb[1], rgb[2], 255]; (w * h) as usize]
    }

    #[test]
    fn identical_images_have_zero_reversal_and_zero_delta_e() {
        let (w, h) = (16u32, 16u32);
        let img = solid(w, h, [128, 128, 128]);
        let report = evaluate_halo(&img, &img, &img, w, h, HaloConfig::SYNTHETIC_DEFAULT);
        assert_eq!(report.mean_reversal_energy, 0.0);
        assert_eq!(report.off_edge_delta_e_mean, 0.0);
        assert!(report.passed);
    }

    #[test]
    fn a_flat_image_has_no_edge_pixels() {
        let (w, h) = (16u32, 16u32);
        let img = solid(w, h, [64, 64, 64]);
        let report = evaluate_halo(&img, &img, &img, w, h, HaloConfig::SYNTHETIC_DEFAULT);
        assert_eq!(report.edge_pixel_count, 0);
        assert_eq!(report.off_edge_pixel_count, (w * h) as usize);
    }

    /// A synthetic step edge whose OUTPUT overshoots right at the boundary
    /// (a textbook halo: a dark undershoot just before the edge, a bright
    /// overshoot just after) must be flagged with positive reversal energy.
    #[test]
    fn a_deliberate_overshoot_at_a_step_edge_is_flagged() {
        let (w, h) = (32u32, 16u32);
        let mut input = vec![[20u8, 20, 20, 255]; (w * h) as usize];
        let mut output = input.clone();
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if x >= w / 2 {
                    input[i] = [220, 220, 220, 255];
                    output[i] = [220, 220, 220, 255];
                }
            }
        }
        // Overshoot: two columns before the edge go darker than the dark
        // side, two columns after go brighter than the bright side.
        for y in 0..h {
            let dark_edge = (y * w + (w / 2 - 1)) as usize;
            let bright_edge = (y * w + (w / 2)) as usize;
            output[dark_edge] = [0, 0, 0, 255];
            output[bright_edge] = [255, 255, 255, 255];
        }
        let reference = input.clone();
        let report = evaluate_halo(
            &input,
            &output,
            &reference,
            w,
            h,
            HaloConfig::SYNTHETIC_DEFAULT,
        );
        assert!(report.edge_pixel_count > 0);
        assert!(
            report.mean_reversal_energy > 0.0,
            "deliberate overshoot must register nonzero reversal energy"
        );
    }
}
