// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Scale-aware evaluation: [`RenderScale`] → working resolution (spec §3.1/§3.5;
//! task **C2**, the §10.1 E05.3 gate).
//!
//! Owner: **C**. The engine derives a *working extent* from the requested
//! [`RenderScale`] and the source's full extent so a fit-view render never
//! materializes or evaluates a full-resolution working tile, a 45 MP source at
//! `Fit(4K)` derives a ≤ ~8 MP working extent (the C2 gate). The derived ratio
//! is quantized to 1/64ths (`scale_q`, spec §3.5) so micro zoom jitter lands on
//! the same cache quantum and `OneToOne`/`Fit` ladders occupy distinct quanta.

use crate::ng::types::{Extent, RenderScale};

/// The number of quantization steps a decimation ratio is snapped to (spec §3.5
/// 1/64ths). `OneToOne` maps to the top quantum ([`SCALE_QUANTA`]).
pub const SCALE_QUANTA: u16 = 64;

/// A resolved render scale: the decimation `ratio` in `(0, 1]`, the working
/// `extent` the graph evaluates at, and the quantized `scale_q` (spec §3.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenderResolution {
    /// The working extent the pipeline evaluates at (`round(full * ratio)`,
    /// clamped to ≥ 1×1). Every node output at this scale is ≤ `extent.area()`.
    pub extent: Extent,
    /// The quantized decimation ratio (1/64ths; keys the tile cache, §3.5).
    pub scale_q: u16,
}

impl RenderResolution {
    /// The decimation ratio this resolution was quantized to, in `(0, 1]`.
    #[inline]
    pub fn ratio(&self) -> f32 {
        self.scale_q as f32 / SCALE_QUANTA as f32
    }

    /// The pixel count the graph evaluates at (`extent.area()`).
    #[inline]
    pub fn working_pixels(&self) -> u64 {
        self.extent.w as u64 * self.extent.h as u64
    }
}

/// Snap a decimation ratio in `(0, 1]` to the nearest 1/64th, never below the
/// smallest quantum (`1`) so a huge downscale still lands on a valid, cacheable
/// scale (spec §3.5).
#[inline]
pub fn quantize_ratio(ratio: f32) -> u16 {
    let r = ratio.clamp(0.0, 1.0);
    let q = (r * SCALE_QUANTA as f32).round() as i64;
    q.clamp(1, SCALE_QUANTA as i64) as u16
}

/// The raw (un-quantized) decimation ratio a [`RenderScale`] implies over a
/// `full`-extent source. `Fit` never upscales (clamped to ≤ 1); `Ratio` is taken
/// verbatim in `(0, 1]`; `OneToOne` is `1.0`.
pub fn raw_ratio(scale: RenderScale, full: Extent) -> f32 {
    let fw = full.w.max(1) as f32;
    let fh = full.h.max(1) as f32;
    match scale {
        RenderScale::OneToOne => 1.0,
        RenderScale::Ratio(f) => f.clamp(f32::MIN_POSITIVE, 1.0),
        RenderScale::Fit(vp) => {
            let rw = vp.w.max(1) as f32 / fw;
            let rh = vp.h.max(1) as f32 / fh;
            rw.min(rh).clamp(f32::MIN_POSITIVE, 1.0)
        }
    }
}

/// Derive the working [`RenderResolution`] for `scale` over a `full`-extent
/// source (spec §3.1 `RenderScale`; task C2). The working extent is the *raw*
/// ratio applied to the full extent (so the fit is honored exactly), while
/// `scale_q` is the quantized ratio the cache keys on.
///
/// Deriving off the raw ratio (not the quantized one) keeps `Fit` honest, a
/// 45 MP source at `Fit(3840×2160)` yields a working extent whose longer edge
/// matches the viewport, ≤ ~8 MP, regardless of the 1/64 cache grid.
pub fn derive(scale: RenderScale, full: Extent) -> RenderResolution {
    let full = Extent {
        w: full.w.max(1),
        h: full.h.max(1),
    };
    let ratio = raw_ratio(scale, full);
    let w = ((full.w as f32 * ratio).round() as u32).clamp(1, full.w);
    let h = ((full.h as f32 * ratio).round() as u32).clamp(1, full.h);
    RenderResolution {
        extent: Extent { w, h },
        scale_q: quantize_ratio(ratio),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(w: u32, h: u32) -> Extent {
        Extent { w, h }
    }

    #[test]
    fn one_to_one_is_full_res_top_quantum() {
        let r = derive(RenderScale::OneToOne, ext(8192, 5504));
        assert_eq!(r.extent, ext(8192, 5504));
        assert_eq!(r.scale_q, SCALE_QUANTA);
    }

    /// **The §10.1 E05.3 gate math (task C2):** a 45 MP source at `Fit(4K)`
    /// derives a working extent of ≤ ~8 MP. 8192×5504 = 45.1 MP; the 4K fit
    /// (3840×2160) is height-bound → ratio ≈ 0.3925 → 3215×2160 ≈ 6.9 MP.
    #[test]
    fn fit_4k_of_45mp_evaluates_under_8mp() {
        let full = ext(8192, 5504); // 45.1 MP
        let r = derive(RenderScale::Fit(ext(3840, 2160)), full);
        assert!(
            r.working_pixels() <= 8_400_000,
            "working {} px ({}×{}) must be ≤ ~8 MP",
            r.working_pixels(),
            r.extent.w,
            r.extent.h
        );
        // The fit is exact on the binding axis (height): output height == viewport.
        assert_eq!(r.extent.h, 2160);
        // And the derived resolution is a real decimation, not full-res.
        assert!(r.working_pixels() * 5 < full.w as u64 * full.h as u64);
    }

    #[test]
    fn fit_never_upscales_a_small_source() {
        // A 640×480 source into a 4K viewport stays 640×480 (ratio clamps to 1).
        let r = derive(RenderScale::Fit(ext(3840, 2160)), ext(640, 480));
        assert_eq!(r.extent, ext(640, 480));
        assert_eq!(r.scale_q, SCALE_QUANTA);
    }

    #[test]
    fn ratio_half_halves_each_axis() {
        let r = derive(RenderScale::Ratio(0.5), ext(4000, 3000));
        assert_eq!(r.extent, ext(2000, 1500));
        assert_eq!(r.scale_q, 32);
    }

    #[test]
    fn scale_q_quantizes_and_never_hits_zero() {
        assert_eq!(quantize_ratio(1.0), 64);
        assert_eq!(quantize_ratio(0.5), 32);
        assert_eq!(quantize_ratio(0.0), 1); // clamps up to the smallest quantum
        assert_eq!(quantize_ratio(0.0001), 1);
        // A tiny fit ratio still yields a distinct, valid working extent.
        let r = derive(RenderScale::Ratio(0.01), ext(10000, 10000));
        assert_eq!(r.extent, ext(100, 100));
        assert_eq!(r.scale_q, 1);
    }

    #[test]
    fn distinct_quanta_for_one_to_one_vs_fit_ladder() {
        let full = ext(6000, 4000);
        let one = derive(RenderScale::OneToOne, full);
        let fit = derive(RenderScale::Fit(ext(1500, 1000)), full);
        assert_ne!(one.scale_q, fit.scale_q);
        assert_eq!(fit.scale_q, 16); // 0.25 → 16/64
    }
}
