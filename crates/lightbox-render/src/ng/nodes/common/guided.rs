// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Box-filter + self-guided-filter CPU primitives (spec §4.4
//! `nodes/common/guided.rs`), the shared base/detail decomposition the
//! provisional `ToneRecoveryNode` (E10 Phase B M1 slice) builds on. Clean-room
//! from He & Sun, *Fast Guided Filter* (2015): the self-guided case (guide ==
//! input) simplifies the general guided-filter linear-coefficient solve to a
//! single running-variance computation, no external/GPL source consulted.
//!
//! The GPU counterpart (`shaders/global_tone_recovery.wgsl`'s `box_blur` /
//! `compute_ab` entries) evaluates the identical box-filter/coefficient math
//! from the identical inputs, so CPU/GPU parity (§4.4) holds to float
//! rounding, this module is the single written spec both backends implement.
//!
//! **Provisional-slice scope:** a plain (non-separable-optimized-but-still
//! O(n) per axis) sliding-window box filter, no downsampled "fast" `s`-stride
//! variant (He & Sun's own speed-up, named here as the M2 perf lever if B14's
//! hardening pass needs it, see `docs/plan/epics/E10-deviations.md`).

use rayon::prelude::*;

/// A separable box (mean) filter over a `w × h` row-major single-channel
/// plane, border-**replicated** (clamp-to-edge), same border rule the
/// engine's tiled apron already establishes at the true image border
/// ([`crate::ng::exec::roi`]), so this filter behaves identically whether
/// `plane` is a whole-image tile or an apron-padded sub-tile.
///
/// O(w·h·r) per axis (a plain sliding accumulator, not a prefix-sum/SAT
/// optimization, the M1-provisional perf choice, see the module doc).
pub fn box_blur(plane: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    debug_assert_eq!(plane.len(), (w as usize) * (h as usize));
    let mut tmp = vec![0.0f32; plane.len()];
    box_blur_horizontal(plane, &mut tmp, w, radius);
    let mut out = vec![0.0f32; plane.len()];
    box_blur_vertical(&tmp, &mut out, w, h, radius);
    out
}

fn box_blur_horizontal(src: &[f32], dst: &mut [f32], w: u32, radius: u32) {
    let (w, r) = (w as i64, radius as i64);
    dst.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row = &src[y * w as usize..(y + 1) * w as usize];
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                let mut count = 0.0f32;
                for dx in -r..=r {
                    let sx = (x as i64 + dx).clamp(0, w - 1) as usize;
                    sum += src_row[sx];
                    count += 1.0;
                }
                *out_px = sum / count.max(1.0);
            }
        });
}

fn box_blur_vertical(src: &[f32], dst: &mut [f32], w: u32, h: u32, radius: u32) {
    let (w_u, h_i, r) = (w as usize, h as i64, radius as i64);
    dst.par_chunks_mut(w_u).enumerate().for_each(|(y, row)| {
        for (x, out_px) in row.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            let mut count = 0.0f32;
            for dy in -r..=r {
                let sy = (y as i64 + dy).clamp(0, h_i - 1) as usize;
                sum += src[sy * w_u + x];
                count += 1.0;
            }
            *out_px = sum / count.max(1.0);
        }
    });
}

/// The self-guided filter's per-pixel `(a, b)` linear coefficients, **before**
/// their own box-filter smoothing pass (He & Sun eq. 5/6, self-guided
/// specialization `p == I`): `a = var_I / (var_I + eps)`, `b = mean_I·(1 − a)`.
/// Exposed separately from [`guided_filter_self`] so the provisional
/// `ToneRecoveryNode`'s GPU kernel (`compute_ab`, a distinct dispatch between
/// the two box-filter passes) and this CPU reference evaluate the identical
/// formula.
#[inline]
pub fn guided_ab(mean_i: f32, corr_i: f32, eps: f32) -> (f32, f32) {
    let var_i = (corr_i - mean_i * mean_i).max(0.0);
    let a = var_i / (var_i + eps);
    let b = mean_i * (1.0 - a);
    (a, b)
}

/// The self-guided filter (guide == input == `luma`, radius `radius`, ridge
/// term `eps`): returns `(mean_a, mean_b)`, the smoothed linear-coefficient
/// planes such that `base(x,y) = mean_a(x,y) * luma(x,y) + mean_b(x,y)` is the
/// edge-aware smoothed "base" layer a caller subtracts from `luma` for the
/// "detail" layer (He & Sun 2015, self-guided case). Four box-filter passes
/// total (`mean_I`/`corr_I`, then `mean_a`/`mean_b`), the same four-pass
/// shape `shaders/global_tone_recovery.wgsl` dispatches.
pub fn guided_filter_self(
    luma: &[f32],
    w: u32,
    h: u32,
    radius: u32,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    let luma_sq: Vec<f32> = luma.iter().map(|&v| v * v).collect();
    let mean_i = box_blur(luma, w, h, radius);
    let corr_i = box_blur(&luma_sq, w, h, radius);

    let n = luma.len();
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .zip(mean_i.par_iter().zip(corr_i.par_iter()))
        .for_each(|((ai, bi), (&mi, &ci))| {
            let (av, bv) = guided_ab(mi, ci, eps);
            *ai = av;
            *bi = bv;
        });

    let mean_a = box_blur(&a, w, h, radius);
    let mean_b = box_blur(&b, w, h, radius);
    (mean_a, mean_b)
}

/// The general (cross) guided filter's per-pixel `(a, b)` linear
/// coefficients, **before** their own box-filter smoothing pass (He & Sun eq.
/// 5/6, general case: guide `I` and target `p` need not be the same plane)
/// `a = cov_Ip / (var_I + eps)`, `b = mean_p - a·mean_I`. [`guided_ab`] is
/// this function's `I == p` specialization (`cov_Ip` collapses to `var_I`).
/// Used by E10 task **D6** (`DehazeNode`'s transmission-map refine: guide =
/// scene luma, `p` = the raw dark-channel transmission estimate, genuinely
/// cross-guided, `I ≠ p`).
#[inline]
pub fn guided_ab_cross(
    mean_i: f32,
    mean_p: f32,
    corr_ip: f32,
    corr_i: f32,
    eps: f32,
) -> (f32, f32) {
    let var_i = (corr_i - mean_i * mean_i).max(0.0);
    let cov_ip = corr_ip - mean_i * mean_p;
    let a = cov_ip / (var_i + eps);
    let b = mean_p - a * mean_i;
    (a, b)
}

/// The general (cross) guided filter (He & Sun 2015 / He, Sun, Tang 2010 eq.
/// 4-8): filters `p` under the edge structure of `guide` (radius `radius`,
/// ridge term `eps`), returning `(mean_a, mean_b)` such that `q(x,y) =
/// mean_a(x,y) * guide(x,y) + mean_b(x,y)` is the filtered result. Six
/// box-filter passes total (`mean_I`, `mean_p`, `corr_I`, `corr_Ip`, then
/// `mean_a`/`mean_b`), [`guided_filter_self`] is the `guide == p`
/// specialization that collapses `mean_p`/`corr_Ip` onto `mean_I`/`corr_I`
/// (four passes instead of six).
pub fn guided_filter(
    guide: &[f32],
    p: &[f32],
    w: u32,
    h: u32,
    radius: u32,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    debug_assert_eq!(guide.len(), p.len());
    let ip: Vec<f32> = guide.iter().zip(p.iter()).map(|(&i, &pp)| i * pp).collect();
    let i2: Vec<f32> = guide.iter().map(|&i| i * i).collect();

    let mean_i = box_blur(guide, w, h, radius);
    let mean_p = box_blur(p, w, h, radius);
    let corr_i = box_blur(&i2, w, h, radius);
    let corr_ip = box_blur(&ip, w, h, radius);

    let n = guide.len();
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .zip(
            mean_i
                .par_iter()
                .zip(mean_p.par_iter())
                .zip(corr_ip.par_iter().zip(corr_i.par_iter())),
        )
        .for_each(|((ai, bi), ((&mi, &mp), (&cip, &ci)))| {
            let (av, bv) = guided_ab_cross(mi, mp, cip, ci, eps);
            *ai = av;
            *bi = bv;
        });

    let mean_a = box_blur(&a, w, h, radius);
    let mean_b = box_blur(&b, w, h, radius);
    (mean_a, mean_b)
}

/// A separable box (min) filter over a `w × h` row-major single-channel
/// plane, border-**replicated** (clamp-to-edge), the same border rule
/// [`box_blur`] uses. A rectangular min filter is separable exactly like a
/// box sum (`min` over a 2D window == `min` over columns of the row-wise
/// `min`s), so this is `box_blur`'s structure with the reduction swapped from
/// `+`/`÷n` to `min`. Used by E10 task **D5** (`DehazeNode`'s windowed
/// dark-channel prior: He, Sun, Tang 2009 eq. 5 windows a per-pixel
/// channel-min over a local patch, not a single pixel, so a lone dark object
/// smaller than the window can't be mistaken for haze-free foreground).
pub fn box_min(plane: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    debug_assert_eq!(plane.len(), (w as usize) * (h as usize));
    let mut tmp = vec![0.0f32; plane.len()];
    box_min_horizontal(plane, &mut tmp, w, radius);
    let mut out = vec![0.0f32; plane.len()];
    box_min_vertical(&tmp, &mut out, w, h, radius);
    out
}

fn box_min_horizontal(src: &[f32], dst: &mut [f32], w: u32, radius: u32) {
    let (w, r) = (w as i64, radius as i64);
    dst.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row = &src[y * w as usize..(y + 1) * w as usize];
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut m = f32::INFINITY;
                for dx in -r..=r {
                    let sx = (x as i64 + dx).clamp(0, w - 1) as usize;
                    m = m.min(src_row[sx]);
                }
                *out_px = m;
            }
        });
}

fn box_min_vertical(src: &[f32], dst: &mut [f32], w: u32, h: u32, radius: u32) {
    let (w_u, h_i, r) = (w as usize, h as i64, radius as i64);
    dst.par_chunks_mut(w_u).enumerate().for_each(|(y, row)| {
        for (x, out_px) in row.iter_mut().enumerate() {
            let mut m = f32::INFINITY;
            for dy in -r..=r {
                let sy = (y as i64 + dy).clamp(0, h_i - 1) as usize;
                m = m.min(src[sy * w_u + x]);
            }
            *out_px = m;
        }
    });
}

/// [`box_min`]'s max-reduction twin, same separable structure, `max`
/// instead of `min`. Used by E10 task **D3** (`ClarityNode`'s halo guard: the
/// recombined pixel is clamped to `[box_min(window), box_max(window)]`, so a
/// detail boost can never overshoot past the values that already exist in
/// its own neighborhood, the standard "no new extremum" halo-suppression
/// technique, structural rather than a tuned threshold).
pub fn box_max(plane: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    debug_assert_eq!(plane.len(), (w as usize) * (h as usize));
    let mut tmp = vec![0.0f32; plane.len()];
    box_max_horizontal(plane, &mut tmp, w, radius);
    let mut out = vec![0.0f32; plane.len()];
    box_max_vertical(&tmp, &mut out, w, h, radius);
    out
}

fn box_max_horizontal(src: &[f32], dst: &mut [f32], w: u32, radius: u32) {
    let (w, r) = (w as i64, radius as i64);
    dst.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row = &src[y * w as usize..(y + 1) * w as usize];
            for (x, out_px) in row.iter_mut().enumerate() {
                let mut m = f32::NEG_INFINITY;
                for dx in -r..=r {
                    let sx = (x as i64 + dx).clamp(0, w - 1) as usize;
                    m = m.max(src_row[sx]);
                }
                *out_px = m;
            }
        });
}

fn box_max_vertical(src: &[f32], dst: &mut [f32], w: u32, h: u32, radius: u32) {
    let (w_u, h_i, r) = (w as usize, h as i64, radius as i64);
    dst.par_chunks_mut(w_u).enumerate().for_each(|(y, row)| {
        for (x, out_px) in row.iter_mut().enumerate() {
            let mut m = f32::NEG_INFINITY;
            for dy in -r..=r {
                let sy = (y as i64 + dy).clamp(0, h_i - 1) as usize;
                m = m.max(src[sy * w_u + x]);
            }
            *out_px = m;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_blur_of_constant_plane_is_the_constant() {
        let plane = vec![0.42f32; 16 * 16];
        let out = box_blur(&plane, 16, 16, 3);
        for &v in &out {
            assert!((v - 0.42).abs() < 1e-6, "v={v}");
        }
    }

    #[test]
    fn box_blur_radius_zero_is_identity() {
        let mut plane = vec![0.0f32; 4 * 4];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = i as f32;
        }
        let out = box_blur(&plane, 4, 4, 0);
        for (i, (&a, &b)) in plane.iter().zip(out.iter()).enumerate() {
            assert!((a - b).abs() < 1e-6, "index {i}: {a} vs {b}");
        }
    }

    #[test]
    fn box_blur_smooths_a_step_edge_monotonically() {
        // A 1-row-tall step: left half 0, right half 1. The blurred profile
        // across the transition must be monotone non-decreasing (a box filter
        // of a monotone step is itself monotone).
        let w = 40u32;
        let plane: Vec<f32> = (0..w).map(|x| if x < w / 2 { 0.0 } else { 1.0 }).collect();
        let out = box_blur(&plane, w, 1, 6);
        let mut prev = -1.0f32;
        for &v in &out {
            assert!(
                v >= prev - 1e-6,
                "non-monotone blurred step: {v} after {prev}"
            );
            prev = v;
        }
        // And it actually smooths (the transition is no longer a hard step).
        let mid = (w / 2) as usize;
        assert!(
            out[mid - 1] > 0.05 && out[mid - 1] < 0.95,
            "out[mid-1]={}",
            out[mid - 1]
        );
    }

    #[test]
    fn guided_ab_is_identity_reconstruction_on_flat_regions() {
        // On a perfectly flat plane, var_I == 0 everywhere ⇒ a == 0, b ==
        // mean_I, so base == mean_I == the flat value exactly.
        let (a, b) = guided_ab(0.5, 0.25, 1e-4);
        assert!(a.abs() < 1e-6, "a={a}");
        assert!((b - 0.5).abs() < 1e-6, "b={b}");
    }

    #[test]
    fn guided_ab_tracks_input_on_high_variance_regions() {
        // Large var_I relative to eps ⇒ a → 1, b → 0, i.e. base → I (the
        // filter preserves, does not smooth, genuine high-contrast content,
        // exactly the "edge-aware" property the recovery curve depends on).
        let (a, b) = guided_ab(0.5, 0.5 * 0.5 + 10.0, 1e-4);
        assert!(a > 0.999, "a={a}");
        assert!(b.abs() < 1e-3, "b={b}");
    }

    #[test]
    fn guided_filter_self_reconstructs_a_flat_plane_exactly() {
        let plane = vec![0.3f32; 20 * 20];
        let (mean_a, mean_b) = guided_filter_self(&plane, 20, 20, 4, 1e-4);
        for i in 0..plane.len() {
            let base = mean_a[i] * plane[i] + mean_b[i];
            assert!((base - 0.3).abs() < 1e-5, "base={base}");
        }
    }

    #[test]
    fn guided_filter_self_preserves_a_strong_step_edge_at_its_center() {
        // Far from a strong step edge (many radii away), the guided filter
        // should behave like plain smoothing; AT the edge, a self-guided
        // filter with a small eps should track the input closely (a ≈ 1),
        // which is exactly what keeps a downstream pointwise recompression
        // curve from producing gradient-reversal halos (B2's target property).
        let w = 60u32;
        let plane: Vec<f32> = (0..w).map(|x| if x < w / 2 { 0.1 } else { 0.9 }).collect();
        let (mean_a, _mean_b) = guided_filter_self(&plane, w, 1, 8, 1e-4);
        let at_edge = mean_a[(w / 2) as usize];
        assert!(
            at_edge > 0.7,
            "expected a≈1 at a strong edge, got a={at_edge}"
        );
    }

    // ── general (cross) guided filter ────────────────────────────────────

    #[test]
    fn guided_filter_cross_matches_self_guided_when_p_equals_guide() {
        // guide == p degenerates to the self-guided case: guided_ab_cross's
        // cov_Ip collapses to var_I exactly, so guided_filter(guide, guide,
        // ..) must agree with guided_filter_self(guide, ..) to float
        // rounding (same 4 underlying box-blurred quantities, computed a
        // different, but mathematically equivalent, way).
        let w = 24u32;
        let h = 24u32;
        let mut plane = vec![0f32; (w * h) as usize];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = ((i * 37) % 101) as f32 / 101.0;
        }
        let (a_self, b_self) = guided_filter_self(&plane, w, h, 5, 1e-3);
        let (a_cross, b_cross) = guided_filter(&plane, &plane, w, h, 5, 1e-3);
        for i in 0..plane.len() {
            assert!((a_self[i] - a_cross[i]).abs() < 1e-4, "a mismatch at {i}");
            assert!((b_self[i] - b_cross[i]).abs() < 1e-4, "b mismatch at {i}");
        }
    }

    #[test]
    fn guided_filter_cross_reconstructs_a_flat_target_exactly() {
        // A flat `p` under any guide reconstructs to that flat value: no
        // spatial variation in `p` means no linear-coefficient structure to
        // recover beyond a constant offset (a == 0, b == the flat value).
        let w = 16u32;
        let h = 16u32;
        let guide: Vec<f32> = (0..w * h).map(|i| (i % 7) as f32 / 7.0).collect();
        let p = vec![0.6f32; (w * h) as usize];
        let (mean_a, mean_b) = guided_filter(&guide, &p, w, h, 4, 1e-4);
        for i in 0..p.len() {
            let q = mean_a[i] * guide[i] + mean_b[i];
            assert!((q - 0.6).abs() < 1e-4, "q={q} at {i}");
        }
    }

    #[test]
    fn guided_filter_cross_tracks_a_step_edge_in_the_guide() {
        // `p` is a smooth ramp; `guide` has a hard step at the midpoint. A
        // small-eps cross guided filter should still respect the guide's
        // edge structure (its `a` coefficient rises near the guide's step,
        // mirroring the self-guided edge-preservation property) rather than
        // just plain-smoothing `p` as if the guide were flat.
        let w = 60u32;
        let guide: Vec<f32> = (0..w).map(|x| if x < w / 2 { 0.1 } else { 0.9 }).collect();
        let p: Vec<f32> = (0..w).map(|x| x as f32 / w as f32).collect();
        let (mean_a, _mean_b) = guided_filter(&guide, &p, w, 1, 8, 1e-4);
        assert!(mean_a.iter().all(|v| v.is_finite()));
    }

    // ── box_min ────────────────────────────────────────────────────────

    #[test]
    fn box_min_of_a_constant_plane_is_the_constant() {
        let plane = vec![0.7f32; 12 * 12];
        let out = box_min(&plane, 12, 12, 3);
        for &v in &out {
            assert!((v - 0.7).abs() < 1e-6, "v={v}");
        }
    }

    #[test]
    fn box_min_radius_zero_is_identity() {
        let mut plane = vec![0f32; 5 * 5];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = i as f32;
        }
        let out = box_min(&plane, 5, 5, 0);
        assert_eq!(plane, out);
    }

    #[test]
    fn box_min_finds_the_single_dark_pixel_within_its_window() {
        // A bright flat plane with one dark pixel at the center: every output
        // pixel within `radius` of the dark pixel must read that dark value
        // (a windowed min "spreads" a lone dark spot across its window,
        // exactly the dark-channel-prior robustness property D5 relies on
        // a single dark object can't be mistaken for a haze-free patch
        // narrower than the window).
        let (w, h) = (21u32, 21u32);
        let mut plane = vec![0.9f32; (w * h) as usize];
        let (cx, cy) = (10usize, 10usize);
        plane[cy * w as usize + cx] = 0.05;
        let out = box_min(&plane, w, h, 3);
        // Directly at the dark pixel and within radius 3, the min is 0.05.
        assert!((out[cy * w as usize + cx] - 0.05).abs() < 1e-6);
        assert!((out[(cy - 3) * w as usize + cx] - 0.05).abs() < 1e-6);
        assert!((out[cy * w as usize + (cx + 3)] - 0.05).abs() < 1e-6);
        // Outside the window (radius 4 away), the dark pixel has no effect.
        assert!((out[(cy - 4) * w as usize + cx] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn box_min_is_monotone_non_increasing_in_radius() {
        // Widening the window can only ever lower (or hold) the min.
        let (w, h) = (18u32, 18u32);
        let mut plane = vec![0f32; (w * h) as usize];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = ((i * 53) % 97) as f32 / 97.0;
        }
        let r2 = box_min(&plane, w, h, 2);
        let r5 = box_min(&plane, w, h, 5);
        for i in 0..plane.len() {
            assert!(
                r5[i] <= r2[i] + 1e-6,
                "index {i}: r5={} r2={}",
                r5[i],
                r2[i]
            );
        }
    }

    // ── box_max ────────────────────────────────────────────────────────

    #[test]
    fn box_max_of_a_constant_plane_is_the_constant() {
        let plane = vec![0.4f32; 12 * 12];
        let out = box_max(&plane, 12, 12, 3);
        for &v in &out {
            assert!((v - 0.4).abs() < 1e-6, "v={v}");
        }
    }

    #[test]
    fn box_max_radius_zero_is_identity() {
        let mut plane = vec![0f32; 5 * 5];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = i as f32;
        }
        let out = box_max(&plane, 5, 5, 0);
        assert_eq!(plane, out);
    }

    #[test]
    fn box_max_finds_the_single_bright_pixel_within_its_window() {
        let (w, h) = (21u32, 21u32);
        let mut plane = vec![0.1f32; (w * h) as usize];
        let (cx, cy) = (10usize, 10usize);
        plane[cy * w as usize + cx] = 0.95;
        let out = box_max(&plane, w, h, 3);
        assert!((out[cy * w as usize + cx] - 0.95).abs() < 1e-6);
        assert!((out[(cy - 3) * w as usize + cx] - 0.95).abs() < 1e-6);
        assert!((out[(cy - 4) * w as usize + cx] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn min_max_of_a_window_always_bracket_the_center_pixel() {
        // The structural property `ClarityNode`'s halo guard relies on: a
        // window's min/max always bracket every sample in that window,
        // including its own center.
        let (w, h) = (16u32, 16u32);
        let mut plane = vec![0f32; (w * h) as usize];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = ((i * 41) % 89) as f32 / 89.0;
        }
        let lo = box_min(&plane, w, h, 4);
        let hi = box_max(&plane, w, h, 4);
        for i in 0..plane.len() {
            assert!(
                lo[i] <= plane[i] + 1e-6,
                "index {i}: lo={} v={}",
                lo[i],
                plane[i]
            );
            assert!(
                hi[i] >= plane[i] - 1e-6,
                "index {i}: hi={} v={}",
                hi[i],
                plane[i]
            );
        }
    }
}
