// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The 256² tile executor — ROI split, per-tile apron reads, seam-free stitch,
//! visible-first ordering, 1:1 demand-driven eval (spec §4.3; tasks **C3**,
//! **C4**, **C5**).
//!
//! Owner: **C**. This is the CPU-path tiled evaluator — the deterministic
//! reference the "tiled render **exactly equals** untiled render (same backend)"
//! gate (C3) rides on. It sits *beside* the frozen [`crate::ng::exec::backend`]
//! whole-tile seam (it drives [`RenderNode::eval_cpu`] directly with
//! ROI-aware tile views) so it needs no reshape of that seam.
//!
//! # How a tile is evaluated
//!
//! For each output tile `T` the executor back-propagates `T` through the graph
//! ([`super::roi::plan_rois`]) to get every node's required output ROI (`T`
//! grown by the composed apron, clamped to the image). It then walks the graph
//! forward, producing each node's sub-tile over exactly that ROI and slicing
//! each consumer's input to precisely the region its `plan()` asked for — so a
//! point-wise node reads a locally-aligned tile and a radius-`r` neighborhood
//! node reads its apron. Because the apron supplies exactly the neighbour pixels
//! the whole-image eval would have seen (edge-replicated at the border), each
//! tile's interior is bit-identical to the untiled result and the stitch is
//! seam-free.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use crate::ng::error::RenderError;
use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::node::{CpuEvalCtx, ParamBlock, RenderNode};
use crate::ng::source::TileSink;
use crate::ng::tile::{CpuTileView, PixelBuf, PixelFormat};
use crate::ng::types::{Extent, NodeId, Roi, TileCoord, TilePrecision};

use super::roi::{clamp_to_extent, plan_rois};
use super::DEFAULT_TILE_SIZE;

/// The order tiles are scheduled in (spec C4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TileOrder {
    /// Row-major (top-left to bottom-right).
    RowMajor,
    /// Center-out (visible-first): tiles nearest the focus point complete first.
    #[default]
    CenterOut,
}

/// Per-render tiling counters — the observable behind the C2/C3/C7 gates.
#[derive(Clone, Debug, Default)]
pub struct TileProbe {
    /// Tiles actually evaluated (in visible order).
    pub tiles_evaluated: u64,
    /// Tiles skipped because they were no longer wanted (off-screen after a pan).
    pub tiles_skipped: u64,
    /// Sum of every node-tile output area (total working pixels touched).
    pub pixels_evaluated: u64,
    /// The largest single node-tile output area (the working-set peak per eval).
    pub max_tile_pixels: u64,
    /// Sum of the *terminal* node's output area over all tiles — the resolution
    /// the render evaluated at (the C2 ≤ ~8 MP metric).
    pub terminal_pixels: u64,
    /// The order tiles completed in (their `(tx, ty)` grid coords).
    pub completion_order: Vec<TileCoord>,
}

impl TileProbe {
    fn note_node(&mut self, out_area: u64) {
        self.pixels_evaluated += out_area;
        self.max_tile_pixels = self.max_tile_pixels.max(out_area);
    }
}

/// A tiled render request over the CPU reference path (tasks C3/C4/C5).
pub struct TileRender<'a> {
    /// The compiled graph.
    pub graph: &'a RenderGraph,
    /// The working (render-scale) image extent — every input-less/source node's
    /// full output extent (spec §3.1 the decimated working resolution).
    pub image_extent: Extent,
    /// The terminal region to produce (in working-scale pixel coords; clamped to
    /// the image).
    pub out_roi: Roi,
    /// The decimation factor (source→working); threads into `plan()`/eval.
    pub scale: f32,
    /// Tile edge, pixels (default [`DEFAULT_TILE_SIZE`]).
    pub tile_size: u32,
    /// Tile completion order.
    pub order: TileOrder,
    /// Center-out focus point (working-scale pixel coords); `None` = ROI center.
    pub focus: Option<(i32, i32)>,
    /// Pre-materialized root tiles keyed by node id — the `src.decoded` seed
    /// (the decimated source). Input-less nodes not present here are evaluated
    /// whole-image once (test generators).
    pub seeds: &'a HashMap<NodeId, PixelBuf>,
    /// Cooperative cancellation (checked at every tile boundary).
    pub cancel: &'a CancelToken,
}

impl<'a> TileRender<'a> {
    /// The `tile_size`²-tile grid over `out_roi`, in the chosen order (C4).
    pub fn tile_grid(&self) -> Vec<Roi> {
        let ts = self.tile_size.max(1);
        let coords: Vec<TileCoord> = self.out_roi.tiles(ts).collect();
        let mut tiles: Vec<Roi> = coords
            .iter()
            .map(|c| tile_rect(*c, ts, self.image_extent, self.out_roi))
            .filter(|r| r.w > 0 && r.h > 0)
            .collect();
        if self.order == TileOrder::CenterOut {
            let (fx, fy) = self.focus.unwrap_or_else(|| roi_center(self.out_roi));
            tiles.sort_by_key(|r| {
                let (cx, cy) = roi_center(*r);
                let dx = (cx - fx) as i64;
                let dy = (cy - fy) as i64;
                dx * dx + dy * dy
            });
        }
        tiles
    }

    /// Render every tile, stitching a single terminal [`PixelBuf`] (task C3).
    pub fn render(&self, probe: &mut TileProbe) -> Result<PixelBuf, RenderError> {
        self.render_with(probe, None, None)
    }

    /// Render, optionally filtering tiles by a `still_wanted` predicate
    /// (off-screen cancellation on `set_view`, C4/C5) and offering completed
    /// tiles to a [`TileSink`] (C5 T2 handoff).
    pub fn render_with(
        &self,
        probe: &mut TileProbe,
        still_wanted: Option<&(dyn Fn(TileCoord) -> bool + Sync)>,
        sink: Option<&TileHandoff>,
    ) -> Result<PixelBuf, RenderError> {
        let ts = self.tile_size.max(1);
        // Whole-image roots (seeds / generators) computed once, shared by tiles.
        let roots = self.materialize_roots()?;

        // Determine the terminal format by producing one tile first? Simpler:
        // allocate the stitch lazily once the first terminal sub-tile is known.
        let mut stitched: Option<PixelBuf> = None;

        for tile in self.tile_grid() {
            if self.cancel.is_cancelled() {
                return Err(RenderError::Cancelled);
            }
            let coord = tile_coord_of(tile, ts);
            if let Some(pred) = still_wanted {
                if !pred(coord) {
                    probe.tiles_skipped += 1;
                    continue;
                }
            }
            let term = self.eval_tile(tile, &roots, probe)?;
            let out = stitched.get_or_insert_with(|| {
                PixelBuf::new_zeroed(
                    term.format,
                    Extent {
                        w: self.out_roi.w,
                        h: self.out_roi.h,
                    },
                )
            });
            blit(out, self.out_roi, &term, tile);
            probe.tiles_evaluated += 1;
            probe.terminal_pixels += tile.w as u64 * tile.h as u64;
            probe.completion_order.push(coord);
            if let Some(h) = sink {
                h.sink.offer_t2(h.image, h.recipe_hash, coord, term.clone());
            }
        }

        Ok(stitched.unwrap_or_else(|| {
            PixelBuf::new_zeroed(
                PixelFormat::Rgba8Unorm,
                Extent {
                    w: self.out_roi.w,
                    h: self.out_roi.h,
                },
            )
        }))
    }

    /// Whole-image root tiles: a seed if present, else a one-shot whole-image
    /// eval of an input-less generator node.
    fn materialize_roots(&self) -> Result<HashMap<NodeIndex, PixelBuf>, RenderError> {
        let mut roots: HashMap<NodeIndex, PixelBuf> = HashMap::new();
        let topo = self.graph.topo_order()?;
        for idx in topo {
            if !self.graph.inputs_of(idx).is_empty() {
                continue; // not a root
            }
            let id = self.graph.node(idx).descriptor().id;
            if let Some(seed) = self.seeds.get(&id) {
                roots.insert(idx, seed.clone());
                continue;
            }
            // Whole-image generator eval (test roots like `test.checker`).
            let full = Roi {
                x: 0,
                y: 0,
                w: self.image_extent.w,
                h: self.image_extent.h,
            };
            let px = self.eval_node_over(idx, full, &[], self.cancel)?;
            roots.insert(idx, px);
        }
        Ok(roots)
    }

    /// Evaluate the graph for one output `tile`, returning the terminal sub-tile.
    fn eval_tile(
        &self,
        tile: Roi,
        roots: &HashMap<NodeIndex, PixelBuf>,
        probe: &mut TileProbe,
    ) -> Result<PixelBuf, RenderError> {
        let plan = plan_rois(self.graph, tile, self.scale, self.image_extent)?;
        let topo = self.graph.topo_order()?;
        // Each node's produced sub-tile, tagged with the ROI it covers.
        let mut produced: HashMap<NodeIndex, (Roi, PixelBuf)> = HashMap::with_capacity(topo.len());

        for idx in topo {
            if self.cancel.is_cancelled() {
                return Err(RenderError::Cancelled);
            }
            let Some(out_roi) = plan.out_roi(idx) else {
                continue; // node not feeding this tile
            };
            if out_roi.w == 0 || out_roi.h == 0 {
                continue;
            }
            let inputs = self.graph.inputs_of(idx);
            if inputs.is_empty() {
                // Root: slice the whole-image root tile to this node's ROI.
                let root = roots.get(&idx).ok_or_else(|| {
                    RenderError::Internal("tiling: root tile not materialized".to_owned())
                })?;
                let sub = slice_subtile(root, image_roi(self.image_extent), out_roi);
                probe.note_node(out_roi.w as u64 * out_roi.h as u64);
                produced.insert(idx, (out_roi, sub));
                continue;
            }

            // Slice each connected input to exactly the region this node's
            // plan() asked for (point-wise → aligned; neighborhood → apron).
            let node = self.graph.node(idx);
            let req_in = node.plan(out_roi, self.scale, self.graph.params(idx)).0;
            let mut sliced: Vec<PixelBuf> = Vec::with_capacity(inputs.len());
            let mut rois: Vec<Roi> = Vec::with_capacity(inputs.len());
            for (port_i, &src) in inputs.iter().enumerate() {
                let (src_roi, src_px) = produced.get(&src).ok_or_else(|| {
                    RenderError::Internal("tiling: upstream sub-tile missing".to_owned())
                })?;
                let src_ext = plan.extent(src).unwrap_or(self.image_extent);
                let want = clamp_to_extent(req_in.get(port_i).copied().unwrap_or(out_roi), src_ext);
                sliced.push(slice_subtile(src_px, *src_roi, want));
                rois.push(want);
            }
            let views: Vec<CpuTileView<'_>> = sliced
                .iter()
                .zip(rois.iter())
                .map(|(px, roi)| CpuTileView {
                    pixels: px,
                    roi: *roi,
                    precision: precision_of(px.format),
                })
                .collect();
            let sub = self.eval_node_views(idx, out_roi, &views, self.cancel)?;
            probe.note_node(out_roi.w as u64 * out_roi.h as u64);
            produced.insert(idx, (out_roi, sub));
        }

        let sink_idx = self
            .graph
            .single_sink()
            .ok_or_else(|| RenderError::Internal("tiling: graph has no single sink".to_owned()))?;
        produced
            .remove(&sink_idx)
            .map(|(_, px)| px)
            .ok_or_else(|| RenderError::Internal("tiling: terminal produced no tile".to_owned()))
    }

    /// Evaluate one node over `out_roi` given already-built input views.
    fn eval_node_views(
        &self,
        idx: NodeIndex,
        out_roi: Roi,
        views: &[CpuTileView<'_>],
        cancel: &CancelToken,
    ) -> Result<PixelBuf, RenderError> {
        let node = self.graph.node(idx);
        let precision = node.precision();
        let mut out = PixelBuf::new_zeroed(working_format(precision), extent_of(out_roi));
        let mut ctx = CpuEvalCtx::new(self.scale, cancel, out_roi, &mut out);
        drive(node, &mut ctx, views, self.graph.params(idx))?;
        Ok(out)
    }

    /// Evaluate an input-less node over `out_roi` (no input views).
    fn eval_node_over(
        &self,
        idx: NodeIndex,
        out_roi: Roi,
        views: &[CpuTileView<'_>],
        cancel: &CancelToken,
    ) -> Result<PixelBuf, RenderError> {
        self.eval_node_views(idx, out_roi, views, cancel)
    }
}

/// A completed-tile handoff to E03's [`TileSink`] (fire-and-forget; task C5).
pub struct TileHandoff {
    /// The sink implementation (E03 mock in tests).
    pub sink: Arc<dyn TileSink>,
    /// The image the tiles belong to.
    pub image: ImageId,
    /// The recipe hash keying the T2 store.
    pub recipe_hash: blake3::Hash,
}

/// Demand-driven 1:1 tile session: tracks which [`TileCoord`]s have been
/// evaluated so a pan only computes newly-visible tiles (task C5).
#[derive(Default)]
pub struct OneToOneSession {
    done: HashSet<TileCoord>,
}

impl OneToOneSession {
    /// A fresh session (nothing evaluated yet).
    pub fn new() -> OneToOneSession {
        OneToOneSession::default()
    }

    /// The set of already-evaluated tile coords.
    pub fn done(&self) -> &HashSet<TileCoord> {
        &self.done
    }

    /// Evaluate only the tiles of `visible` not already done, offering each to
    /// `sink`. Returns the number of tiles freshly evaluated (task C5).
    #[allow(clippy::too_many_arguments)]
    pub fn advance(
        &mut self,
        graph: &RenderGraph,
        image_extent: Extent,
        visible: Roi,
        seeds: &HashMap<NodeId, PixelBuf>,
        sink: Option<&TileHandoff>,
        cancel: &CancelToken,
        probe: &mut TileProbe,
    ) -> Result<u64, RenderError> {
        let ts = DEFAULT_TILE_SIZE;
        let want: Vec<TileCoord> = visible.tiles(ts).collect();
        let mut fresh = 0u64;
        for coord in want {
            if self.done.contains(&coord) {
                continue;
            }
            let rect = tile_rect(coord, ts, image_extent, image_roi(image_extent));
            if rect.w == 0 || rect.h == 0 {
                continue;
            }
            let render = TileRender {
                graph,
                image_extent,
                out_roi: rect,
                scale: 1.0, // OneToOne
                tile_size: ts,
                order: TileOrder::RowMajor,
                focus: None,
                seeds,
                cancel,
            };
            render.render_with(probe, None, sink)?;
            self.done.insert(coord);
            fresh += 1;
        }
        Ok(fresh)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn image_roi(extent: Extent) -> Roi {
    Roi {
        x: 0,
        y: 0,
        w: extent.w,
        h: extent.h,
    }
}

fn extent_of(roi: Roi) -> Extent {
    Extent {
        w: roi.w.max(1),
        h: roi.h.max(1),
    }
}

fn roi_center(r: Roi) -> (i32, i32) {
    (
        (r.x as i64 + r.w as i64 / 2) as i32,
        (r.y as i64 + r.h as i64 / 2) as i32,
    )
}

fn precision_of(format: PixelFormat) -> TilePrecision {
    match format {
        PixelFormat::Rgba32F => TilePrecision::F32,
        _ => TilePrecision::F16,
    }
}

fn working_format(precision: TilePrecision) -> PixelFormat {
    match precision {
        TilePrecision::F16 => PixelFormat::Rgba16F,
        TilePrecision::F32 => PixelFormat::Rgba32F,
    }
}

/// The pixel rectangle a [`TileCoord`] covers, intersected with the image and
/// the request ROI (border tiles are partial).
fn tile_rect(coord: TileCoord, ts: u32, image: Extent, out_roi: Roi) -> Roi {
    let x = coord.tx as i64 * ts as i64;
    let y = coord.ty as i64 * ts as i64;
    let full = Roi {
        x: x as i32,
        y: y as i32,
        w: ts,
        h: ts,
    };
    let img = image_roi(image);
    full.intersect(&img)
        .and_then(|r| r.intersect(&out_roi))
        .unwrap_or(Roi {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        })
}

/// The grid coord a tile rectangle belongs to (top-left / tile_size).
fn tile_coord_of(rect: Roi, ts: u32) -> TileCoord {
    TileCoord {
        tx: (rect.x.max(0) as u32) / ts.max(1),
        ty: (rect.y.max(0) as u32) / ts.max(1),
        scale_q: 0,
    }
}

/// Copy the `want`-region (⊆ `src_roi`) out of `src` (which covers `src_roi`)
/// into a fresh, tightly-packed [`PixelBuf`] covering exactly `want`.
///
/// The fast path (the common one — the executor only ever asks for a region the
/// producer already covers) is a per-row `memcpy`; a fully-in-bounds `want`
/// needs no per-pixel clamp. Any out-of-bounds row/column edge-replicates.
pub fn slice_subtile(src: &PixelBuf, src_roi: Roi, want: Roi) -> PixelBuf {
    let mut out = PixelBuf::new_zeroed(src.format, extent_of(want));
    let bpp = src.format.bytes_per_pixel() as usize;
    let sx0 = want.x as i64 - src_roi.x as i64;
    let sy0 = want.y as i64 - src_roi.y as i64;
    let in_bounds_h = sx0 >= 0 && sx0 + want.w as i64 <= src.extent.w as i64;
    let in_bounds_v = sy0 >= 0 && sy0 + want.h as i64 <= src.extent.h as i64;

    if in_bounds_h && in_bounds_v {
        let row_bytes = want.w as usize * bpp;
        for oy in 0..want.h as usize {
            let sy = (sy0 as usize) + oy;
            let src_off = sy * src.stride as usize + (sx0 as usize) * bpp;
            let dst_off = oy * out.stride as usize;
            out.bytes[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src.bytes[src_off..src_off + row_bytes]);
        }
        return out;
    }

    // Border fallback: edge-replicate out-of-range rows/columns.
    for oy in 0..want.h {
        let sy = (sy0 + oy as i64).clamp(0, (src.extent.h - 1) as i64) as u32;
        for ox in 0..want.w {
            let sx = (sx0 + ox as i64).clamp(0, (src.extent.w - 1) as i64) as u32;
            out.set_rgba_f32(ox, oy, src.get_rgba_f32(sx, sy));
        }
    }
    out
}

/// Blit a terminal sub-tile (covering `tile`) into the stitched output (covering
/// `out_roi`) at the tile's offset — the seam-free stitch (task C3). Per-row
/// `memcpy` (same format, so byte-for-byte identical to the sub-tile).
fn blit(out: &mut PixelBuf, out_roi: Roi, tile: &PixelBuf, tile_roi: Roi) {
    let ox0 = (tile_roi.x - out_roi.x).max(0) as u32;
    let oy0 = (tile_roi.y - out_roi.y).max(0) as u32;
    let bpp = out.format.bytes_per_pixel() as usize;
    let copy_w = tile.extent.w.min(out.extent.w.saturating_sub(ox0)) as usize;
    if copy_w == 0 || out.format != tile.format {
        return;
    }
    let row_bytes = copy_w * bpp;
    for ty in 0..tile.extent.h {
        let dy = oy0 + ty;
        if dy >= out.extent.h {
            break;
        }
        let dst_off = dy as usize * out.stride as usize + ox0 as usize * bpp;
        let src_off = ty as usize * tile.stride as usize;
        out.bytes[dst_off..dst_off + row_bytes]
            .copy_from_slice(&tile.bytes[src_off..src_off + row_bytes]);
    }
}

/// Drive a node's CPU kernel, mapping cancellation/errors to [`RenderError`].
fn drive(
    node: &dyn RenderNode,
    ctx: &mut CpuEvalCtx<'_>,
    views: &[CpuTileView<'_>],
    params: &ParamBlock,
) -> Result<(), RenderError> {
    node.eval_cpu(ctx, views, params).map_err(|e| match e {
        crate::ng::error::NodeError::Cancelled => RenderError::Cancelled,
        source => RenderError::Node {
            node: node.descriptor().id,
            source,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(w: u32, h: u32) -> Extent {
        Extent { w, h }
    }
    fn roi(x: i32, y: i32, w: u32, h: u32) -> Roi {
        Roi { x, y, w, h }
    }

    #[test]
    fn tile_grid_center_out_orders_nearest_first() {
        let seeds = HashMap::new();
        let g = RenderGraph::new();
        let r = TileRender {
            graph: &g,
            image_extent: ext(768, 512),
            out_roi: roi(0, 0, 768, 512),
            scale: 1.0,
            tile_size: 256,
            order: TileOrder::CenterOut,
            focus: None,
            seeds: &seeds,
            cancel: &CancelToken::new(),
        };
        let tiles = r.tile_grid();
        // 3×2 tiles; the center-most tile should sort first.
        assert_eq!(tiles.len(), 6);
        let (cx, cy) = roi_center(tiles[0]);
        // Nearest to the ROI center (384, 256): the middle-column tiles.
        assert!(
            (cx - 384).abs() <= 128 && (cy - 256).abs() <= 128,
            "first tile at {cx},{cy}"
        );
    }

    #[test]
    fn slice_subtile_clamps_and_copies() {
        let mut src = PixelBuf::new_zeroed(PixelFormat::Rgba16F, ext(4, 4));
        for y in 0..4 {
            for x in 0..4 {
                src.set_rgba_f32(x, y, [x as f32, y as f32, 0.0, 1.0]);
            }
        }
        // Slice the interior 2×2 at (1,1) out of a buffer covering (0,0,4,4).
        let sub = slice_subtile(&src, roi(0, 0, 4, 4), roi(1, 1, 2, 2));
        assert_eq!(sub.extent, ext(2, 2));
        assert_eq!(sub.get_rgba_f32(0, 0)[0], 1.0);
        assert_eq!(sub.get_rgba_f32(1, 1)[1], 2.0);
    }
}
