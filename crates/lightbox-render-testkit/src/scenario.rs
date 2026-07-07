// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Slider-latency + soak scenario harness (spec §6; tasks **F1** / **C10**).
//!
//! Owner: **F** (F1 p95 slider-to-screen at fit-view) and **C** (C10 10k-iteration
//! interactive soak). Drives the engine end-to-end through the scheduler; the
//! p95<100 ms gate of record is nightly on the reference GPU runner (§8).

/// A scripted slider-churn scenario (spec F1).
#[derive(Clone, Debug)]
pub struct SliderScenario {
    /// Corpus source name to edit.
    pub source: String,
    /// Number of rapid param events to fire.
    pub events: u32,
    /// Fit-view viewport width, pixels.
    pub viewport_w: u32,
    /// Fit-view viewport height, pixels.
    pub viewport_h: u32,
}

/// End-to-end slider-to-screen latency percentiles (spec F1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyReport {
    /// Median latency, ms.
    pub p50_ms: f64,
    /// 95th-percentile latency, ms (the <100 ms gate).
    pub p95_ms: f64,
    /// Sample count.
    pub samples: usize,
}

/// Measure slider-to-screen p95 for `scenario` (spec F1). **F1 wires it.**
pub fn run_slider_latency(scenario: &SliderScenario) -> LatencyReport {
    let _ = scenario;
    unimplemented!("F1 (F): slider-latency scenario harness (p95 slider-to-screen)")
}

/// A randomized param/zoom/pan soak configuration (spec C10).
#[derive(Clone, Copy, Debug)]
pub struct SoakConfig {
    /// Iteration count (10k for the nightly gate; reduced locally — see
    /// `E05-deviations.md`, C10).
    pub iterations: u64,
    /// RNG seed (soak is seeded-deterministic).
    pub seed: u64,
}

/// The result of a soak run (spec C10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoakReport {
    /// Iterations actually run.
    pub iterations: u64,
    /// Render/validation errors observed (must be 0).
    pub validation_errors: u64,
    /// Peak per-eval working-tile pixel count (the tiling working-set ceiling —
    /// bounded regardless of zoom, so VRAM stays in budget).
    pub peak_tile_pixels: u64,
    /// Whether the final frame equals an independent fresh render of the same
    /// state (no cross-iteration state corruption).
    pub final_matches_fresh: bool,
}

/// A tiny seeded xorshift64* RNG — soak determinism without a crate dep.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % n.max(1) as u64) as u32
    }
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// One soak step: the interactive state a frame is rendered from.
#[derive(Clone, Copy, Debug)]
struct SoakStep {
    image: lightbox_render::ng::Extent,
    out_roi: lightbox_render::ng::Roi,
    gain: f32,
    radius: f64,
    order: lightbox_render::ng::exec::tiling::TileOrder,
}

/// Render one soak step through the CPU tile executor, returning the stitched
/// frame + its working-set peak.
fn render_step(
    step: &SoakStep,
    cancel: &lightbox_jobs::CancelToken,
) -> Result<(lightbox_render::ng::PixelBuf, u64), lightbox_render::ng::RenderError> {
    use lightbox_render::ng::exec::tiling::{TileProbe, TileRender};
    use lightbox_render::ng::{ParamBlock, ParamValue, RenderGraph};
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::probes::{BlurRProbe, CheckerProbe, GainProbe};

    // test.checker (generator root) → test.gain(gain) → test.blur_r(radius).
    let mut graph = RenderGraph::new();
    let checker = graph.add_node(Arc::new(CheckerProbe::default()));
    let gain = graph.add_node_with_params(
        Arc::new(GainProbe::default()),
        ParamBlock::from_fields([("gain", ParamValue::Float(step.gain as f64))])
            .expect("gain param"),
    );
    let blur = graph.add_node_with_params(
        Arc::new(BlurRProbe::default()),
        ParamBlock::from_fields([("radius", ParamValue::Float(step.radius))])
            .expect("radius param"),
    );
    graph.connect(checker, gain, "in").expect("checker→gain");
    graph.connect(gain, blur, "in").expect("gain→blur");

    let seeds = HashMap::new();
    let render = TileRender {
        graph: &graph,
        image_extent: step.image,
        out_roi: step.out_roi,
        scale: 1.0,
        tile_size: 256,
        order: step.order,
        focus: None,
        seeds: &seeds,
        cancel,
    };
    let mut probe = TileProbe::default();
    let frame = render.render(&mut probe)?;
    Ok((frame, probe.max_tile_pixels))
}

/// Run the interactive soak: `cfg.iterations` randomized param/zoom/pan frames
/// over the tile executor, asserting zero errors, a bounded working set, and
/// that the final frame reproduces on a fresh render (spec C10). The 10 k count
/// is the nightly gate; a reduced count runs in-gate (recorded in deviations).
pub fn run_soak(cfg: &SoakConfig) -> SoakReport {
    use lightbox_render::ng::{Extent, Roi};

    let cancel = lightbox_jobs::CancelToken::new();
    let mut rng = Rng::new(cfg.seed);
    let mut errors = 0u64;
    let mut peak = 0u64;
    let mut last: Option<(SoakStep, lightbox_render::ng::PixelBuf)> = None;

    // A few zoom levels (working extents) modelling fit/1:1 ladders. Kept modest
    // so the in-gate reduced-count soak stays fast; the nightly 10 k soak uses
    // the full corpus resolutions (see E05-deviations.md, C10).
    let zooms = [
        Extent { w: 288, h: 192 },
        Extent { w: 160, h: 120 },
        Extent { w: 96, h: 72 },
    ];

    for _ in 0..cfg.iterations {
        let image = zooms[rng.below(zooms.len() as u32) as usize];
        // Random pan: a sub-ROI (or the full frame) within the working image.
        let full = rng.below(3) == 0;
        let out_roi = if full {
            Roi {
                x: 0,
                y: 0,
                w: image.w,
                h: image.h,
            }
        } else {
            let rw = (image.w / 2).max(1) + rng.below((image.w / 2).max(1));
            let rh = (image.h / 2).max(1) + rng.below((image.h / 2).max(1));
            let x = rng.below(image.w.saturating_sub(rw).max(1)) as i32;
            let y = rng.below(image.h.saturating_sub(rh).max(1)) as i32;
            Roi {
                x,
                y,
                w: rw.min(image.w),
                h: rh.min(image.h),
            }
        };
        let order = if rng.below(2) == 0 {
            lightbox_render::ng::exec::tiling::TileOrder::RowMajor
        } else {
            lightbox_render::ng::exec::tiling::TileOrder::CenterOut
        };
        let step = SoakStep {
            image,
            out_roi,
            gain: 0.25 + rng.unit() * 3.0,
            radius: rng.below(3) as f64,
            order,
        };
        match render_step(&step, &cancel) {
            Ok((frame, tile_peak)) => {
                peak = peak.max(tile_peak);
                last = Some((step, frame));
            }
            Err(_) => errors += 1,
        }
    }

    let final_matches_fresh = match &last {
        Some((step, frame)) => render_step(step, &cancel)
            .map(|(fresh, _)| fresh.bytes == frame.bytes)
            .unwrap_or(false),
        None => true,
    };

    SoakReport {
        iterations: cfg.iterations,
        validation_errors: errors,
        peak_tile_pixels: peak,
        final_matches_fresh,
    }
}
