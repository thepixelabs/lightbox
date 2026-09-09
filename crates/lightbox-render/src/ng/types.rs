// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Core identity & geometry types, spec §3.1, transcribed verbatim.
//!
//! Owner: **A-core** (task **A2**, Roi/Extent/TileCoord algebra, RenderScale,
//! PortType, TilePrecision; unit tests for expand/intersect/tiles + serde
//! round-trip where derived).

use serde::{Deserialize, Serialize};

// ── identity ─────────────────────────────────────────────────────────────────

/// Stable node identity, a dotted namespace, e.g. `"xform.display"`,
/// `"tone.exposure"` (spec §3.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub &'static str);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Pipeline process version, `PV1 == ProcessVersion(1)`; stored per image
/// (`edit_recipe.pv`, E01 schema). Append-only forever (§4.5). Re-exported from
/// `lightbox-types` so the whole workspace shares one definition (spec §3.1).
pub use lightbox_types::ProcessVersion;

// ── geometry (source-pixel coordinate frame unless stated) ───────────────────

/// A pixel extent (width × height).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Extent {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// A region of interest, in source-pixel coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Roi {
    /// Left edge (may be negative for apron growth past the image border).
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Roi {
    /// The right edge (exclusive), in `i64` to avoid `i32`/`u32` overflow.
    #[inline]
    fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    /// The bottom edge (exclusive).
    #[inline]
    fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    /// The pixel area (`w * h`).
    #[inline]
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// Apron growth for neighborhood nodes: expand on every side by `margin`
    /// (spec §3.1). The origin moves up-and-left (possibly negative, past the
    /// image border) and the extent grows by `2 * margin`; saturating so a huge
    /// margin can never wrap.
    pub fn expand(&self, margin: u32) -> Roi {
        let m = margin as i64;
        let nx = (self.x as i64 - m).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let ny = (self.y as i64 - m).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        Roi {
            x: nx,
            y: ny,
            w: self.w.saturating_add(margin.saturating_mul(2)),
            h: self.h.saturating_add(margin.saturating_mul(2)),
        }
    }

    /// Intersection with `other`, or `None` when the two are disjoint (spec
    /// §3.1). Empty inputs (`w == 0` or `h == 0`) never intersect.
    pub fn intersect(&self, other: &Roi) -> Option<Roi> {
        if self.w == 0 || self.h == 0 || other.w == 0 || other.h == 0 {
            return None;
        }
        let x0 = self.x.max(other.x) as i64;
        let y0 = self.y.max(other.y) as i64;
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 > x0 && y1 > y0 {
            Some(Roi {
                x: x0 as i32,
                y: y0 as i32,
                w: (x1 - x0) as u32,
                h: (y1 - y0) as u32,
            })
        } else {
            None
        }
    }

    /// The `size`²-tile grid covering this ROI, anchored to the global
    /// source-pixel grid at the origin so tile coordinates are stable across
    /// ROIs (256² default; spec §3.1/§4.3). Apron regions past the top/left
    /// border are clamped to the grid origin. `scale_q` is left `0`, the
    /// executor stamps the render's quantized scale (spec §3.5).
    pub fn tiles(&self, size: u32) -> impl Iterator<Item = TileCoord> {
        let size = size.max(1) as i64;
        let mut coords = Vec::new();
        if self.w != 0 && self.h != 0 {
            let left = self.x.max(0) as i64;
            let top = self.y.max(0) as i64;
            let right = self.right().max(0);
            let bottom = self.bottom().max(0);
            if right > left && bottom > top {
                let tx0 = left / size;
                let tx1 = (right - 1) / size;
                let ty0 = top / size;
                let ty1 = (bottom - 1) / size;
                for ty in ty0..=ty1 {
                    for tx in tx0..=tx1 {
                        coords.push(TileCoord {
                            tx: tx as u32,
                            ty: ty as u32,
                            scale_q: 0,
                        });
                    }
                }
            }
        }
        coords.into_iter()
    }
}

/// Coordinates of one tile: grid position plus the quantized scale it was
/// evaluated at (spec §3.1; `scale_q` per §3.5, 1/64ths).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TileCoord {
    /// Tile column.
    pub tx: u32,
    /// Tile row.
    pub ty: u32,
    /// Quantized render scale (see §3.5, RenderScale → 1/64ths).
    pub scale_q: u16,
}

/// How much of the source resolution a render evaluates at (spec §3.1).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum RenderScale {
    /// Engine derives decimation so output ≤ the given viewport extent
    /// (≤ ~8 MP for a 45 MP source at a 4K fit).
    Fit(Extent),
    /// An explicit fraction of source resolution, in `(0, 1]`.
    Ratio(f32),
    /// 1:1 zoom, tiled, demand-driven.
    OneToOne,
}

/// Typed image-port kind; graph edges are type-checked at build (spec §3.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PortType {
    /// Reserved for E11 (GPU demosaic); no v1 engine node produces it.
    MosaicU16,
    /// The working format: scene-linear, ProPhoto primaries, RGBA16F (§4.2).
    LinearRgbaF16,
    /// Precision escape hatch for accumulation-heavy stages (§4.2).
    LinearRgbaF32,
    /// Grayscale mask weight buffers (E12 / baked AI rasters §4.5).
    WeightR16,
    /// Post-display-transform output the canvas composites.
    DisplayRgba8,
}

/// Output tile precision (spec §3.1/§3.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TilePrecision {
    /// `rgba16float` storage, f32 compute (the default).
    F16,
    /// `rgba32float`, accumulation-sensitive stages (§4.2).
    F32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roi(x: i32, y: i32, w: u32, h: u32) -> Roi {
        Roi { x, y, w, h }
    }

    #[test]
    fn expand_grows_all_sides_and_may_go_negative() {
        let r = roi(10, 20, 100, 80).expand(4);
        assert_eq!(r, roi(6, 16, 108, 88));
        // Apron past the top-left border.
        let a = roi(2, 1, 8, 8).expand(4);
        assert_eq!(a, roi(-2, -3, 16, 16));
    }

    #[test]
    fn expand_by_zero_is_identity() {
        let r = roi(-5, 7, 3, 9);
        assert_eq!(r.expand(0), r);
    }

    #[test]
    fn intersect_overlap_disjoint_and_touching() {
        let a = roi(0, 0, 100, 100);
        assert_eq!(
            a.intersect(&roi(50, 50, 100, 100)),
            Some(roi(50, 50, 50, 50))
        );
        // Fully disjoint.
        assert_eq!(a.intersect(&roi(200, 0, 10, 10)), None);
        // Edge-touching (right edge == left edge) is NOT an overlap.
        assert_eq!(a.intersect(&roi(100, 0, 10, 100)), None);
        // Containment.
        assert_eq!(a.intersect(&roi(10, 10, 20, 20)), Some(roi(10, 10, 20, 20)));
        assert!(a.intersect(&a).is_some());
    }

    #[test]
    fn intersect_is_commutative_and_empty_never_hits() {
        let a = roi(3, 4, 30, 40);
        let b = roi(10, 10, 50, 5);
        assert_eq!(a.intersect(&b), b.intersect(&a));
        assert_eq!(a.intersect(&roi(3, 4, 0, 40)), None);
    }

    #[test]
    fn tiles_cover_and_align_to_global_grid() {
        // A ROI straddling three 256-tiles horizontally, two vertically.
        let t: Vec<_> = roi(200, 100, 400, 300).tiles(256).collect();
        // x: [200,600) → tx 0,1,2 ; y: [100,400) → ty 0,1
        let txs: Vec<u32> = t.iter().map(|c| c.tx).collect();
        let tys: Vec<u32> = t.iter().map(|c| c.ty).collect();
        assert_eq!(t.len(), 6);
        assert_eq!(*txs.iter().max().unwrap(), 2);
        assert_eq!(*txs.iter().min().unwrap(), 0);
        assert_eq!(*tys.iter().max().unwrap(), 1);
        assert!(t.iter().all(|c| c.scale_q == 0));
    }

    #[test]
    fn tiles_exact_tile_boundaries() {
        // Exactly one tile.
        assert_eq!(roi(0, 0, 256, 256).tiles(256).count(), 1);
        // 257 wide spills into a second column.
        assert_eq!(roi(0, 0, 257, 256).tiles(256).count(), 2);
        // Empty ROI yields nothing.
        assert_eq!(roi(0, 0, 0, 256).tiles(256).count(), 0);
    }

    #[test]
    fn serde_round_trip_of_derived_types() {
        let vals = [
            serde_json::to_string(&Extent { w: 640, h: 480 }).unwrap(),
            serde_json::to_string(&roi(-1, 2, 3, 4)).unwrap(),
            serde_json::to_string(&TileCoord {
                tx: 1,
                ty: 2,
                scale_q: 64,
            })
            .unwrap(),
            serde_json::to_string(&RenderScale::Fit(Extent { w: 3840, h: 2160 })).unwrap(),
            serde_json::to_string(&RenderScale::Ratio(0.5)).unwrap(),
            serde_json::to_string(&PortType::LinearRgbaF16).unwrap(),
            serde_json::to_string(&TilePrecision::F32).unwrap(),
        ];
        assert_eq!(
            serde_json::from_str::<Extent>(&vals[0]).unwrap(),
            Extent { w: 640, h: 480 }
        );
        assert_eq!(
            serde_json::from_str::<Roi>(&vals[1]).unwrap(),
            roi(-1, 2, 3, 4)
        );
        assert_eq!(
            serde_json::from_str::<TileCoord>(&vals[2]).unwrap(),
            TileCoord {
                tx: 1,
                ty: 2,
                scale_q: 64
            }
        );
        assert_eq!(
            serde_json::from_str::<PortType>(&vals[5]).unwrap(),
            PortType::LinearRgbaF16
        );
        assert_eq!(
            serde_json::from_str::<TilePrecision>(&vals[6]).unwrap(),
            TilePrecision::F32
        );
    }
}
