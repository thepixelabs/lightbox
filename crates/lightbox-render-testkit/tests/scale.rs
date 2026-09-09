// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! **The §10.1 E05.3 gate (task C2), PR-blocking:** a 45 MP source at `Fit(4K)`
//! evaluates ≤ ~8 MP (probe-counted). Two halves, both exercised here:
//! (a) the scale derivation maps `Fit(4K)` over a 45 MP full extent to a ≤ ~8 MP
//! working extent, and (b) the tile executor provably evaluates every node at
//! exactly that working extent, never at full resolution. Part (c) exercises
//! the real `util.resize` decimation reading a large source down to the fit
//! working tier.

use std::collections::HashMap;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::exec::scale::{derive, RenderResolution};
use lightbox_render::ng::exec::tiling::{TileOrder, TileProbe, TileRender};
use lightbox_render::ng::nodes::decoded::SrcDecodedNode;
use lightbox_render::ng::nodes::resize::decimate_box_cpu;
use lightbox_render::ng::types::{Extent, NodeId};
use lightbox_render::ng::{ParamBlock, ParamValue, PixelBuf, RenderGraph, RenderScale, Roi};
use lightbox_render_testkit::corpus::{synth_source, CorpusKind};
use lightbox_render_testkit::probes::GainProbe;
use std::sync::Arc;

/// ~8 MP ceiling with a little slack (a 45 MP@4K fit lands near 6.9 MP).
const EIGHT_MP: u64 = 8_400_000;

fn src_gain_graph(gain: f64) -> RenderGraph {
    let mut g = RenderGraph::new();
    let src = g.add_node(Arc::new(SrcDecodedNode::new()));
    let gain_n = g.add_node_with_params(
        Arc::new(GainProbe::default()),
        ParamBlock::from_fields([("gain", ParamValue::Float(gain))]).unwrap(),
    );
    g.connect(src, gain_n, "in").unwrap();
    g
}

/// Render the whole working frame with `seed` as the decimated `src.decoded`
/// source, returning the terminal pixels-evaluated probe.
fn render_at(working: Extent, seed: PixelBuf) -> TileProbe {
    let mut seeds: HashMap<NodeId, PixelBuf> = HashMap::new();
    seeds.insert(SrcDecodedNode::ID, seed);
    let g = src_gain_graph(1.25);
    let cancel = CancelToken::new();
    let r = TileRender {
        graph: &g,
        image_extent: working,
        out_roi: Roi {
            x: 0,
            y: 0,
            w: working.w,
            h: working.h,
        },
        scale: 1.0,
        tile_size: 256,
        order: TileOrder::CenterOut,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    };
    let mut probe = TileProbe::default();
    r.render(&mut probe).expect("render at working scale");
    probe
}

#[test]
fn fit_4k_over_45mp_evaluates_at_most_8mp() {
    // (a) Derivation: a 45.1 MP source at a 4K fit derives a ≤ ~8 MP working
    // extent whose binding axis matches the viewport.
    let full = Extent { w: 8192, h: 5504 }; // 45.1 MP
    let res: RenderResolution = derive(RenderScale::Fit(Extent { w: 3840, h: 2160 }), full);
    assert!(
        res.working_pixels() <= EIGHT_MP,
        "derived working {} px exceeds ~8 MP",
        res.working_pixels()
    );
    assert!(res.working_pixels() * 5 < full.w as u64 * full.h as u64);

    // (b) The executor evaluates the whole pipeline at exactly the derived
    // working extent, every node-tile and the terminal total stay ≤ ~8 MP; no
    // node ever runs at the 45 MP full resolution.
    let seed = synth_source(CorpusKind::Gradient, res.extent.w, res.extent.h);
    let probe = render_at(res.extent, seed);
    assert_eq!(
        probe.terminal_pixels,
        res.working_pixels(),
        "terminal evaluated at the derived working resolution"
    );
    assert!(
        probe.terminal_pixels <= EIGHT_MP,
        "terminal evaluated {} px > ~8 MP (the C2 gate)",
        probe.terminal_pixels
    );
    assert!(
        probe.max_tile_pixels <= EIGHT_MP,
        "a single node-tile evaluated {} px > ~8 MP",
        probe.max_tile_pixels
    );
    // Working-set per eval is tile-bounded (256²-ish), far under the frame.
    assert!(
        probe.max_tile_pixels <= 256 * 256,
        "per-tile working set unbounded"
    );
}

/// (c) The real `util.resize` decimation reads a large source down to the fit
/// working tier; the pipeline then evaluates at that reduced resolution
/// (probe-counted << the source pixels).
#[test]
fn util_resize_decimates_a_large_source_to_the_fit_tier() {
    let full = Extent { w: 2400, h: 1600 }; // 3.8 MP real source (kept CI-fast;
                                            // the full 45 MP decimation is a
                                            // reduced-scale item, deviations C2)
    let source = synth_source(CorpusKind::Gradient, full.w, full.h);

    let viewport = Extent { w: 800, h: 450 };
    let res = derive(RenderScale::Fit(viewport), full);
    assert!(res.working_pixels() < full.w as u64 * full.h as u64 / 8);

    // Decimate the 12 MP source to the working tier via the engine's resampler
    // (the util.resize CPU kernel), this is the injection-time decimation.
    let decimated = decimate_box_cpu(&source, res.extent);
    assert_eq!(decimated.extent, res.extent);

    let probe = render_at(res.extent, decimated);
    assert_eq!(probe.terminal_pixels, res.working_pixels());
    assert!(
        probe.terminal_pixels < full.w as u64 * full.h as u64,
        "pipeline still evaluated at full res"
    );
}
