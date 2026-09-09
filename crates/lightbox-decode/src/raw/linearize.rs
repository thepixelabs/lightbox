// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! CPU reference linearization (A6, spec §4.1 stage 2 / §3.1 `linearize`).
//!
//! # Kernel spec (E05 ports this to WGSL)
//!
//! For each output sensel `(ox, oy)` in the active-area crop, reading the
//! source sensel `(sx, sy) = (ox + active.x, oy + active.y)` with tile-position
//! index `i = cfa.level_index(sx, sy)`:
//!
//! ```text
//! s   = data[sy * width + sx]                       // raw u16
//! s   = linearization[s]        (if a table is present; identity otherwise)
//! b   = black_levels.levels[i]                      // per CFA position
//! w   = white_levels[i]                             // per CFA position
//! out = clamp((s - b) / (w - b), 0.0, 1.0)          // f32 in [0, 1]
//! ```
//!
//! The linearization table is a raw-value LUT (DNG `LinearizationTable`
//! semantics): raw sample values index it and it maps them to linearized
//! sample values in the same integer domain; black/white subtraction and
//! normalization happen **after** the table. Out-of-range table indices clamp
//! to the last entry. A degenerate `w <= b` yields `0.0` (never a NaN/inf), so
//! a garbage level table can never poison the pipeline.
//!
//! Bit depth is implicit in `white_levels` (e.g. 4095 for 12-bit, 16383 for
//! 14-bit, 65535 for 16-bit): the same kernel covers all depths. Monochrome
//! and linear-DNG sources use position-0 levels and pass straight through the
//! same math.

use crate::raw::types::{LinearMosaic, MosaicImage};

/// Applies the linearization table to a raw sample, if present. Indices past
/// the table clamp to the final entry.
#[inline]
fn apply_table(sample: u16, table: Option<&Vec<u16>>) -> u16 {
    match table {
        Some(t) if !t.is_empty() => {
            let idx = (sample as usize).min(t.len() - 1);
            t[idx]
        }
        _ => sample,
    }
}

/// Linearizes one raw sensel to `f32` in `[0, 1]` (see the module kernel spec).
#[inline]
fn linearize_sensel(raw: u16, table: Option<&Vec<u16>>, black: u32, white: u32) -> f32 {
    let s = u32::from(apply_table(raw, table));
    if white <= black {
        return 0.0;
    }
    let num = f64::from(s.saturating_sub(black));
    let den = f64::from(white - black);
    (num / den).clamp(0.0, 1.0) as f32
}

/// Linearizes a mosaic image (spec §3.1): per-CFA-position black subtract,
/// linearization-table application, white-level normalize to `[0, 1]` `f32`,
/// active-area crop. Never panics (clamps degenerate levels/indices).
pub fn linearize(m: &MosaicImage) -> LinearMosaic {
    // Clamp the active area into the real plane so a malformed rectangle can
    // never index out of bounds (A9 spirit: no panic on bad metadata).
    let ax = m.active_area.x.min(m.width);
    let ay = m.active_area.y.min(m.height);
    let aw = m.active_area.width.min(m.width - ax);
    let ah = m.active_area.height.min(m.height - ay);

    let table = m.linearization.as_ref();
    let mut out = vec![0.0f32; (aw as usize) * (ah as usize)];

    for oy in 0..ah {
        let sy = ay + oy;
        let src_row = (sy as usize) * (m.width as usize);
        let dst_row = (oy as usize) * (aw as usize);
        for ox in 0..aw {
            let sx = ax + ox;
            let i = m.cfa.level_index(sx, sy);
            let black = m.black_levels.levels[i];
            let white = m.white_levels[i];
            let raw = m.data.samples[src_row + sx as usize];
            out[dst_row + ox as usize] = linearize_sensel(raw, table, black, white);
        }
    }

    LinearMosaic {
        data: out,
        width: aw,
        height: ah,
        cfa: m.cfa.clone(),
        colorimetry: m.colorimetry.clone(),
        orientation: m.orientation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::types::{
        BlackLevels, CfaColor, CfaPattern, MosaicBuffer, RawColorimetry, Rect,
    };
    use lightbox_types::Orientation;

    fn rggb() -> CfaPattern {
        CfaPattern::Bayer([CfaColor::R, CfaColor::G, CfaColor::G, CfaColor::B])
    }

    #[allow(clippy::too_many_arguments)] // test fixture builder
    fn mosaic(
        w: u32,
        h: u32,
        samples: Vec<u16>,
        black: [u32; 4],
        white: [u32; 4],
        cfa: CfaPattern,
        active: Rect,
        table: Option<Vec<u16>>,
    ) -> MosaicImage {
        MosaicImage {
            data: MosaicBuffer { samples },
            width: w,
            height: h,
            cfa,
            active_area: active,
            default_crop: active,
            black_levels: BlackLevels { levels: black },
            white_levels: white,
            linearization: table,
            colorimetry: RawColorimetry::default(),
            orientation: Orientation::O1,
        }
    }

    #[test]
    fn per_position_black_and_white_normalize_exactly() {
        // 2×2 RGGB. black = [16,20,20,24], white = [4095,...] (12-bit).
        // samples chosen so each channel lands on an exact fraction.
        // R(0,0)=16+? , use midpoints.
        let black = [16, 20, 20, 24];
        let white = [4111, 4116, 4116, 4120]; // white-black = 4095/4096/4096/4096
        let samples = vec![
            16 + 4095, // (0,0) R -> 1.0
            20 + 1024, // (1,0) G -> 1024/4096 = 0.25
            20 + 2048, // (0,1) G -> 0.5
            24,        // (1,1) B -> 0.0
        ];
        let m = mosaic(2, 2, samples, black, white, rggb(), Rect::full(2, 2), None);
        let out = linearize(&m);
        assert_eq!((out.width, out.height), (2, 2));
        assert!((out.data[0] - 1.0).abs() < 1e-6);
        assert!((out.data[1] - 0.25).abs() < 1e-6);
        assert!((out.data[2] - 0.5).abs() < 1e-6);
        assert!((out.data[3] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn linearization_table_is_applied_before_normalize() {
        // A table that maps raw 0->0, 1->100, 2->200, 3->400 (nonlinear).
        // black=0 white=400 uniform => 400 maps to 1.0.
        let table = vec![0u16, 100, 200, 400];
        let samples = vec![3u16, 1, 2, 0];
        let m = mosaic(
            2,
            2,
            samples,
            [0; 4],
            [400; 4],
            CfaPattern::Mono, // uniform levels
            Rect::full(2, 2),
            Some(table),
        );
        let out = linearize(&m);
        assert!((out.data[0] - 1.0).abs() < 1e-6); // table[3]=400 -> 1.0
        assert!((out.data[1] - 0.25).abs() < 1e-6); // table[1]=100 -> 0.25
        assert!((out.data[2] - 0.5).abs() < 1e-6); // table[2]=200 -> 0.5
        assert!((out.data[3] - 0.0).abs() < 1e-6); // table[0]=0 -> 0.0
    }

    #[test]
    fn active_area_crop_shifts_the_origin() {
        // 4×4 plane, active area is the inner 2×2 at (1,1).
        let mut samples = vec![0u16; 16];
        // Put a marker at (1,1),(2,1),(1,2),(2,2).
        for (x, y, v) in [(1, 1, 4095u16), (2, 1, 0), (1, 2, 0), (2, 2, 4095)] {
            samples[y * 4 + x] = v;
        }
        let m = mosaic(
            4,
            4,
            samples,
            [0; 4],
            [4095; 4],
            rggb(),
            Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            None,
        );
        let out = linearize(&m);
        assert_eq!((out.width, out.height), (2, 2));
        assert!((out.data[0] - 1.0).abs() < 1e-6);
        assert!((out.data[1] - 0.0).abs() < 1e-6);
        assert!((out.data[2] - 0.0).abs() < 1e-6);
        assert!((out.data[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bit_depths_12_14_16_all_normalize_to_full_scale() {
        for white in [4095u32, 16383, 65535] {
            let samples = vec![white as u16, 0, 0, white as u16];
            let m = mosaic(
                2,
                2,
                samples,
                [0; 4],
                [white; 4],
                rggb(),
                Rect::full(2, 2),
                None,
            );
            let out = linearize(&m);
            assert!((out.data[0] - 1.0).abs() < 1e-6, "white={white}");
            assert!((out.data[3] - 1.0).abs() < 1e-6, "white={white}");
        }
    }

    #[test]
    fn values_below_black_clamp_to_zero_not_negative() {
        let samples = vec![5u16, 0, 0, 0];
        let m = mosaic(
            2,
            2,
            samples,
            [100; 4],
            [4095; 4],
            rggb(),
            Rect::full(2, 2),
            None,
        );
        let out = linearize(&m);
        assert_eq!(out.data[0], 0.0);
        assert!(out.data.iter().all(|v| *v >= 0.0));
    }

    #[test]
    fn degenerate_levels_never_produce_nan() {
        let samples = vec![100u16, 100, 100, 100];
        // white == black everywhere -> defined as 0.0.
        let m = mosaic(
            2,
            2,
            samples,
            [100; 4],
            [100; 4],
            rggb(),
            Rect::full(2, 2),
            None,
        );
        let out = linearize(&m);
        assert!(out.data.iter().all(|v| v.is_finite()));
        assert!(out.data.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn malformed_active_area_is_clamped_not_panicked() {
        let samples = vec![10u16; 4];
        let m = mosaic(
            2,
            2,
            samples,
            [0; 4],
            [4095; 4],
            rggb(),
            // Active area far outside the plane.
            Rect {
                x: 100,
                y: 100,
                width: 500,
                height: 500,
            },
            None,
        );
        let out = linearize(&m); // must not panic
        assert_eq!((out.width, out.height), (0, 0));
        assert!(out.data.is_empty());
    }

    #[test]
    fn mono_passthrough() {
        let samples = vec![2048u16, 4095, 0, 1024];
        let m = mosaic(
            2,
            2,
            samples,
            [0; 4],
            [4095; 4],
            CfaPattern::Mono,
            Rect::full(2, 2),
            None,
        );
        let out = linearize(&m);
        assert!((out.data[0] - 2048.0 / 4095.0).abs() < 1e-4);
        assert!((out.data[1] - 1.0).abs() < 1e-6);
        assert_eq!(out.data[2], 0.0);
    }
}
