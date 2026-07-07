// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! **C6 — the progressive ladder, end-to-end.** Real renders (a display-only
//! preview tier, a preview-resolution render, a full-resolution render) are
//! sequenced through [`Ladder`], and the emitted [`RenderState`] transitions are
//! asserted to be the three-stage sequence PreviewReady(PreviewTier) →
//! PreviewReady(PreviewRes) → Complete(FullRes). The first-`PreviewReady`
//! wall-clock is recorded (indicative locally; the ≤ 100 ms warm-store gate is
//! on the reference machine — see `E05-deviations.md`, C6/F1).

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::colorimetry::OutputColorimetry;
use lightbox_render::ng::exec::progressive::Ladder;
use lightbox_render::ng::exec::tiling::{TileOrder, TileProbe, TileRender};
use lightbox_render::ng::types::Extent;
use lightbox_render::ng::{
    BackendId, OutputPayload, OutputQuality, ParamBlock, ParamValue, PixelBuf, RenderGraph,
    RenderOutput, RenderState, Roi,
};
use lightbox_render_testkit::probes::{CheckerProbe, GainProbe};
use lightbox_types::PV_M0;

/// Render `checker → gain` at `extent` on the CPU tile executor → a `RenderOutput`.
fn render_output(extent: Extent, quality: OutputQuality) -> RenderOutput {
    let mut g = RenderGraph::new();
    let checker = g.add_node(Arc::new(CheckerProbe::default()));
    let gain = g.add_node_with_params(
        Arc::new(GainProbe::default()),
        ParamBlock::from_fields([("gain", ParamValue::Float(1.5))]).unwrap(),
    );
    g.connect(checker, gain, "in").unwrap();

    let seeds = HashMap::new();
    let cancel = CancelToken::new();
    let render = TileRender {
        graph: &g,
        image_extent: extent,
        out_roi: Roi {
            x: 0,
            y: 0,
            w: extent.w,
            h: extent.h,
        },
        scale: 1.0,
        tile_size: 256,
        order: TileOrder::CenterOut,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    };
    let mut probe = TileProbe::default();
    let px: PixelBuf = render.render(&mut probe).unwrap();
    RenderOutput {
        payload: OutputPayload::Pixels(px),
        colorimetry: OutputColorimetry::default(),
        backend: BackendId::Cpu,
        pv: PV_M0,
        quality,
    }
}

#[test]
fn progressive_ladder_end_to_end_three_stage_sequence() {
    // Tiers at increasing resolution: display-only preview (small), preview-res,
    // full-res. The ladder stamps the fidelity badge; the tier closures render.
    let ladder = Ladder {
        preview_tier: Box::new(|| {
            Ok(render_output(
                Extent { w: 64, h: 48 },
                OutputQuality::FullRes,
            ))
        }),
        preview_res: Box::new(|| {
            Ok(render_output(
                Extent { w: 320, h: 240 },
                OutputQuality::FullRes,
            ))
        }),
        full_res: Box::new(|| {
            Ok(render_output(
                Extent { w: 640, h: 480 },
                OutputQuality::FullRes,
            ))
        }),
    };

    let cancel = CancelToken::new();
    let mut states: Vec<RenderState> = Vec::new();
    let report = ladder.run(&cancel, &mut |s| states.push(s)).unwrap();

    // Three fidelity tiers, in order.
    assert_eq!(
        report.qualities,
        vec![
            OutputQuality::PreviewTier,
            OutputQuality::PreviewRes,
            OutputQuality::FullRes,
        ]
    );
    assert!(report.completed);

    // The emitted RenderState sequence, with the correct badges + real pixels.
    assert!(matches!(states[0], RenderState::Rendering { .. }));
    match &states[1] {
        RenderState::PreviewReady(o) => {
            assert_eq!(o.quality, OutputQuality::PreviewTier);
            assert!(matches!(o.payload, OutputPayload::Pixels(_)));
        }
        s => panic!("stage 1 not PreviewReady: {s:?}"),
    }
    match &states[2] {
        RenderState::PreviewReady(o) => assert_eq!(o.quality, OutputQuality::PreviewRes),
        s => panic!("stage 2 not PreviewReady: {s:?}"),
    }
    match &states[3] {
        RenderState::Complete(o) => {
            assert_eq!(o.quality, OutputQuality::FullRes);
            if let OutputPayload::Pixels(px) = &o.payload {
                assert_eq!(px.extent, Extent { w: 640, h: 480 });
            } else {
                panic!("full-res payload not pixels");
            }
        }
        s => panic!("final not Complete: {s:?}"),
    }

    eprintln!(
        "[C6] first PreviewReady in {:?} (indicative; ≤100ms gate is on the reference machine)",
        report.first_preview
    );
}
