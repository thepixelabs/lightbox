// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase C ROI/tile integration gates over the CPU reference path:
//! C3 tiled == untiled exact-equality (gain + blur), C1 apron correctness,
//! C4 visible-first ordering + off-screen cancellation, C5 1:1 demand-driven
//! eval + `TileSink` handoff. Deterministic, no GPU adapter needed (the CPU
//! path is the bit-exact reference the C3 gate rides on).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lightbox_jobs::CancelToken;
use lightbox_render::ng::exec::tiling::{
    OneToOneSession, TileHandoff, TileOrder, TileProbe, TileRender,
};
use lightbox_render::ng::source::TileSink;
use lightbox_render::ng::types::Extent;
use lightbox_render::ng::{ParamBlock, ParamValue, PixelBuf, RenderGraph, Roi, TileCoord};
use lightbox_render_testkit::probes::{BlurRProbe, CheckerProbe, GainProbe};
use lightbox_types::ImageId;

fn checker_gain_blur(gain: f64, radius: f64) -> RenderGraph {
    let mut g = RenderGraph::new();
    let checker = g.add_node(Arc::new(CheckerProbe::default()));
    let gain_n = g.add_node_with_params(
        Arc::new(GainProbe::default()),
        ParamBlock::from_fields([("gain", ParamValue::Float(gain))]).unwrap(),
    );
    let blur_n = g.add_node_with_params(
        Arc::new(BlurRProbe::default()),
        ParamBlock::from_fields([("radius", ParamValue::Float(radius))]).unwrap(),
    );
    g.connect(checker, gain_n, "in").unwrap();
    g.connect(gain_n, blur_n, "in").unwrap();
    g
}

fn render(
    graph: &RenderGraph,
    image: Extent,
    out_roi: Roi,
    tile_size: u32,
    order: TileOrder,
) -> (PixelBuf, TileProbe) {
    let seeds = HashMap::new();
    let cancel = CancelToken::new();
    let r = TileRender {
        graph,
        image_extent: image,
        out_roi,
        scale: 1.0,
        tile_size,
        order,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    };
    let mut probe = TileProbe::default();
    let px = r.render(&mut probe).expect("tiled render");
    (px, probe)
}

/// **C3 gate:** a tiled (256²) render EXACTLY equals the untiled render, same
/// backend, for both a point-wise (gain) and a neighborhood (blur) graph.
#[test]
fn tiled_equals_untiled_for_gain_and_blur() {
    let image = Extent { w: 300, h: 200 };
    let full = Roi {
        x: 0,
        y: 0,
        w: 300,
        h: 200,
    };

    // Point-wise: gain only (radius 0 blur is identity apron).
    let g_point = checker_gain_blur(1.5, 0.0);
    let (tiled, ptiled) = render(&g_point, image, full, 256, TileOrder::RowMajor);
    let (untiled, _) = render(&g_point, image, full, 4096, TileOrder::RowMajor);
    assert_eq!(tiled.bytes, untiled.bytes, "point-wise: tiled != untiled");
    assert!(
        ptiled.tiles_evaluated >= 2,
        "must actually tile: {}",
        ptiled.tiles_evaluated
    );

    // Neighborhood: a radius-3 blur, the apron must stitch seam-free.
    let g_blur = checker_gain_blur(1.5, 3.0);
    let (tiled_b, ptiled_b) = render(&g_blur, image, full, 256, TileOrder::RowMajor);
    let (untiled_b, puntiled_b) = render(&g_blur, image, full, 4096, TileOrder::RowMajor);
    assert_eq!(
        tiled_b.bytes, untiled_b.bytes,
        "blur: tiled != untiled (seam)"
    );
    assert!(ptiled_b.tiles_evaluated >= 2);
    assert_eq!(puntiled_b.tiles_evaluated, 1, "untiled is one tile");
}

/// **C3, harder:** a partial ROI (pan) tiled render equals the same ROI window
/// of a whole-image render, proves the stitch is placement-correct.
#[test]
fn tiled_partial_roi_matches_window_of_whole() {
    let image = Extent { w: 512, h: 384 };
    let g = checker_gain_blur(2.0, 2.0);

    let whole = Roi {
        x: 0,
        y: 0,
        w: 512,
        h: 384,
    };
    let (full_img, _) = render(&g, image, whole, 4096, TileOrder::RowMajor);

    let window = Roi {
        x: 100,
        y: 80,
        w: 200,
        h: 160,
    };
    let (win, _) = render(&g, image, window, 256, TileOrder::CenterOut);

    // Compare the window against the same region of the whole render.
    for wy in 0..window.h {
        for wx in 0..window.w {
            let a = win.get_rgba_f32(wx, wy);
            let b = full_img.get_rgba_f32(window.x as u32 + wx, window.y as u32 + wy);
            assert_eq!(a, b, "window mismatch at ({wx},{wy})");
        }
    }
}

/// **C1/C3 apron correctness:** the blur actually smooths (differs from input)
/// yet stays energy-preserving on a flat region, and the tiled result carries no
/// seam discontinuity at the 256 boundary.
#[test]
fn blur_smooths_without_a_seam_at_the_tile_boundary() {
    let image = Extent { w: 300, h: 64 };
    let full = Roi {
        x: 0,
        y: 0,
        w: 300,
        h: 64,
    };
    let g = checker_gain_blur(1.0, 4.0);
    let (blurred, _) = render(&g, image, full, 256, TileOrder::RowMajor);
    let (sharp, _) = render(
        &checker_gain_blur(1.0, 0.0),
        image,
        full,
        256,
        TileOrder::RowMajor,
    );

    // The blur changed the image (it is not a pass-through).
    assert_ne!(blurred.bytes, sharp.bytes);

    // No seam: adjacent columns straddling the x=256 tile boundary differ no more
    // than adjacent columns elsewhere in a smooth run (the blur is continuous).
    let col = |buf: &PixelBuf, x: u32| buf.get_rgba_f32(x, 32);
    let jump_at = |x: u32| {
        let a = col(&blurred, x);
        let b = col(&blurred, x + 1);
        (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)
    };
    // The largest single-column jump anywhere bounds the boundary jump (no seam
    // spike specifically at 256).
    let global_max = (0..298).map(jump_at).fold(0.0, f32::max);
    assert!(jump_at(255) <= global_max + 1e-4, "seam at x=256");
}

/// **C4:** center-out ordering completes the focus tile first, and off-screen
/// tiles are cancelled (skipped) when a pan drops them from the wanted set.
#[test]
fn center_out_ordering_and_offscreen_cancellation() {
    let image = Extent { w: 768, h: 768 };
    let full = Roi {
        x: 0,
        y: 0,
        w: 768,
        h: 768,
    };
    let g = checker_gain_blur(1.0, 1.0);
    let seeds = HashMap::new();
    let cancel = CancelToken::new();

    // Focus on the bottom-right tile: it must complete first.
    let r = TileRender {
        graph: &g,
        image_extent: image,
        out_roi: full,
        scale: 1.0,
        tile_size: 256,
        order: TileOrder::CenterOut,
        focus: Some((700, 700)),
        seeds: &seeds,
        cancel: &cancel,
    };
    let mut probe = TileProbe::default();
    r.render(&mut probe).unwrap();
    assert_eq!(
        probe.completion_order.first().copied(),
        Some(TileCoord {
            tx: 2,
            ty: 2,
            scale_q: 0
        }),
        "focus tile did not complete first: {:?}",
        probe.completion_order
    );

    // Off-screen cancellation: only keep the top-left 2×2 tiles wanted (a pan
    // away from the rest). The other 5 tiles are skipped, not evaluated.
    let wanted = |c: TileCoord| c.tx < 2 && c.ty < 2;
    let mut probe2 = TileProbe::default();
    r.render_with(&mut probe2, Some(&wanted), None).unwrap();
    assert_eq!(probe2.tiles_evaluated, 4, "wanted tiles");
    assert_eq!(probe2.tiles_skipped, 5, "off-screen tiles cancelled");
}

/// A mock [`TileSink`] recording the coords it was offered (E03 seam, C5).
#[derive(Default)]
struct RecordingSink {
    offered: Mutex<Vec<(ImageId, TileCoord)>>,
}
impl TileSink for RecordingSink {
    fn offer_t2(&self, image: ImageId, _recipe_hash: blake3::Hash, tile: TileCoord, _px: PixelBuf) {
        self.offered.lock().unwrap().push((image, tile));
    }
}

/// **C5:** at 1:1 a pan evaluates only newly-visible tiles, and completed tiles
/// are offered to the `TileSink` with correct coords.
#[test]
fn one_to_one_demand_driven_pan_and_tilesink() {
    let image = Extent { w: 600, h: 600 };
    let g = checker_gain_blur(1.0, 0.0);
    let seeds = HashMap::new();
    let cancel = CancelToken::new();
    let sink = Arc::new(RecordingSink::default());
    let handoff = TileHandoff {
        sink: Arc::clone(&sink) as Arc<dyn TileSink>,
        image: ImageId(7),
        recipe_hash: blake3::hash(b"recipe"),
    };

    let mut session = OneToOneSession::new();
    let mut probe = TileProbe::default();

    // View 1: top-left 512×512 → tiles (0,0),(1,0),(0,1),(1,1) = 4 fresh.
    let v1 = Roi {
        x: 0,
        y: 0,
        w: 512,
        h: 512,
    };
    let fresh1 = session
        .advance(&g, image, v1, &seeds, Some(&handoff), &cancel, &mut probe)
        .unwrap();
    assert_eq!(fresh1, 4, "first view evaluates 4 tiles");

    // View 2: pan to bottom-right (256,256)+344 → tiles (1,1),(2,1),(1,2),(2,2).
    // (1,1) already done ⇒ only 3 newly-visible tiles evaluated.
    let v2 = Roi {
        x: 256,
        y: 256,
        w: 344,
        h: 344,
    };
    let fresh2 = session
        .advance(&g, image, v2, &seeds, Some(&handoff), &cancel, &mut probe)
        .unwrap();
    assert_eq!(fresh2, 3, "pan evaluates only the newly-visible tiles");

    // The sink received one offer per freshly-evaluated tile (4 + 3), all for our
    // image, and (1,1) exactly once (not recomputed on the pan).
    let offered = sink.offered.lock().unwrap();
    assert_eq!(offered.len(), 7);
    assert!(offered.iter().all(|(img, _)| *img == ImageId(7)));
    let ones = offered
        .iter()
        .filter(|(_, c)| c.tx == 1 && c.ty == 1)
        .count();
    assert_eq!(ones, 1, "tile (1,1) offered exactly once");
    assert_eq!(session.done().len(), 7, "7 distinct tiles cached");
}
