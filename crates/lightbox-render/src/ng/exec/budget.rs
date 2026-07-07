// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! VRAM budgeting + tiling working-set degradation (spec §3.6 `VramBudget`;
//! task **C7**).
//!
//! Owner: **C**. Two mechanisms keep a large render bounded:
//! 1. **The tile pool's LRU eviction to budget** (task A7) caps *resident*
//!    working tiles — a big render streams tiles through a fixed-size pool.
//! 2. **Scale degradation** (here) handles the case the pool cannot: when the
//!    RAM/VRAM-pinned *source* tile at the requested render scale would itself
//!    blow the budget (a 100 MP source at f16 = 800 MB vs a 512 MB budget), the
//!    engine drops the render scale so the working resolution fits.
//!
//! `VramBudget::Auto` resolves to `min(60% adapter mem, cap)`. wgpu exposes no
//! portable adapter-memory query, so [`resolve_budget`] takes the probed total
//! when the platform surfaces one and falls back to a conservative default
//! otherwise (recorded in `E05-deviations.md`, C7).

use crate::ng::cache::Bytes;
use crate::ng::config::VramBudget;
use crate::ng::types::{Extent, TilePrecision};

use super::scale::{derive, RenderResolution};
use crate::ng::types::RenderScale;

/// Fraction of adapter memory `Auto` targets (spec §3.6).
pub const AUTO_FRACTION: f64 = 0.60;

/// Hard cap on the `Auto` budget regardless of adapter memory (6 GiB).
pub const AUTO_CAP: u64 = 6 << 30;

/// Conservative fallback budget when the adapter does not report its memory
/// (wgpu has no portable VRAM query — see the module note). 1 GiB.
pub const AUTO_FALLBACK: u64 = 1 << 30;

/// The fraction of the budget the pinned source tile is allowed to occupy
/// before scale degradation kicks in (leaving headroom for tile intermediates,
/// the cache, and the display double-buffer).
pub const SOURCE_HEADROOM: f64 = 0.5;

/// Resolve a [`VramBudget`] to a concrete byte cap (task C7). `probed_total` is
/// the adapter's memory in bytes when the platform surfaces it, else `None`.
pub fn resolve_budget(budget: VramBudget, probed_total: Option<u64>) -> Bytes {
    match budget {
        VramBudget::Bytes(n) => Bytes(n.max(1)),
        VramBudget::Auto => match probed_total {
            Some(total) => Bytes((((total as f64) * AUTO_FRACTION) as u64).clamp(1, AUTO_CAP)),
            None => Bytes(AUTO_FALLBACK),
        },
    }
}

/// Bytes a working tile of `extent` at `precision` occupies (RGBA, 8 B/px at
/// f16, 16 B/px at f32).
pub fn tile_bytes(extent: Extent, precision: TilePrecision) -> u64 {
    let bpp = match precision {
        TilePrecision::F16 => 8u64,
        TilePrecision::F32 => 16u64,
    };
    extent.w as u64 * extent.h as u64 * bpp
}

/// The resident working-set estimate for a tiled render: the pinned source tile
/// (whole working image) plus a small ring of in-flight tile intermediates
/// (task C7). This is what degradation must fit under the budget.
pub fn working_set_bytes(
    working: Extent,
    precision: TilePrecision,
    tile_size: u32,
    live_tiles: u32,
) -> u64 {
    let source = tile_bytes(working, precision);
    let tile = tile_bytes(
        Extent {
            w: tile_size,
            h: tile_size,
        },
        precision,
    );
    source + tile * live_tiles as u64
}

/// Degrade the requested render resolution so the pinned source tile fits within
/// `SOURCE_HEADROOM` of the `budget` (task C7). Returns the (possibly reduced)
/// [`RenderResolution`] and whether degradation occurred. The reduction is
/// area-proportional (halving each axis quarters the bytes), so the smallest
/// scale that fits is found in one closed-form step.
pub fn degrade_to_budget(
    full: Extent,
    requested: RenderResolution,
    budget: Bytes,
    precision: TilePrecision,
) -> (RenderResolution, bool) {
    let target = (budget.0 as f64 * SOURCE_HEADROOM).max(1.0);
    let current = tile_bytes(requested.extent, precision) as f64;
    if current <= target {
        return (requested, false);
    }
    // bytes ∝ ratio² ⇒ scale the *linear* ratio by sqrt(target/current).
    let shrink = (target / current).sqrt();
    let new_ratio = (requested.ratio() as f64 * shrink).clamp(f64::MIN_POSITIVE, 1.0) as f32;
    let degraded = derive(RenderScale::Ratio(new_ratio), full);
    (degraded, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(w: u32, h: u32) -> Extent {
        Extent { w, h }
    }

    #[test]
    fn auto_budget_is_sixty_percent_capped() {
        // 16 GiB adapter → 60% = 9.6 GiB, capped at 6 GiB.
        assert_eq!(resolve_budget(VramBudget::Auto, Some(16 << 30)).0, AUTO_CAP);
        // 2 GiB adapter → 60% = 1.2 GiB (under the cap).
        let b = resolve_budget(VramBudget::Auto, Some(2 << 30)).0;
        assert_eq!(b, (2u64 << 30) * 3 / 5);
        // Unprobeable → conservative fallback.
        assert_eq!(resolve_budget(VramBudget::Auto, None).0, AUTO_FALLBACK);
        // Explicit bytes pass through.
        assert_eq!(resolve_budget(VramBudget::Bytes(777), None).0, 777);
    }

    /// **C7 gate (reduced-scale, deterministic):** a 100 MP source under a
    /// 512 MB budget degrades its render scale so the pinned source working tile
    /// fits within headroom — no 800 MB working tile is ever requested.
    #[test]
    fn hundred_mp_degrades_under_512mb() {
        let full = ext(12000, 8334); // ≈ 100 MP
        let budget = Bytes(512 << 20); // 512 MB
                                       // Requested: 1:1 (full res) — 100 MP × 8 B = 800 MB, way over budget.
        let requested = derive(RenderScale::OneToOne, full);
        assert!(tile_bytes(requested.extent, TilePrecision::F16) > budget.0);

        let (degraded, did) = degrade_to_budget(full, requested, budget, TilePrecision::F16);
        assert!(did, "must degrade");
        let bytes = tile_bytes(degraded.extent, TilePrecision::F16);
        let target = (budget.0 as f64 * SOURCE_HEADROOM) as u64;
        assert!(
            bytes <= target,
            "degraded source {bytes} B must be ≤ headroom {target} B",
        );
        // And the working set (source + a few live tiles) stays under budget.
        let ws = working_set_bytes(degraded.extent, TilePrecision::F16, 256, 8);
        assert!(
            ws <= budget.0,
            "working set {ws} B over budget {} B",
            budget.0
        );
    }

    #[test]
    fn fitting_render_is_not_degraded() {
        let full = ext(4000, 3000); // 12 MP
        let budget = Bytes(1 << 30); // 1 GiB — plenty
        let requested = derive(RenderScale::OneToOne, full);
        let (out, did) = degrade_to_budget(full, requested, budget, TilePrecision::F16);
        assert!(!did);
        assert_eq!(out, requested);
    }

    #[test]
    fn f32_needs_more_degradation_than_f16() {
        let full = ext(8000, 6000); // 48 MP
        let budget = Bytes(256 << 20);
        let req = derive(RenderScale::OneToOne, full);
        let (d16, _) = degrade_to_budget(full, req, budget, TilePrecision::F16);
        let (d32, _) = degrade_to_budget(full, req, budget, TilePrecision::F32);
        // f32 is 2× the bytes, so it must degrade to a smaller working extent.
        assert!(d32.working_pixels() < d16.working_pixels());
    }
}
