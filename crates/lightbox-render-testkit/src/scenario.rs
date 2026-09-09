// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Slider-latency + soak scenario harness (spec §6; tasks **F1** / **C10**).
//!
//! Owner: **F** (F1 p95 slider-to-screen at fit-view) and **C** (C10 10k-iteration
//! interactive soak). Drives the engine end-to-end through the scheduler; the
//! p95<100 ms gate of record is nightly on the reference GPU runner (§8).
//!
//! # F1 design note
//!
//! [`run_slider_latency`] scripts a burst of rapid interactive events (a
//! slider drag proxy: `scenario.events` `set_view` pan deltas fired faster
//! than a render completes) against a real [`lightbox_render::ng::Engine`]
//! (GPU when an adapter is available, `RenderScheduler`'s coalescing
//! semantics are backend-agnostic) over the PV1 engine-owned node graph
//! (`src.decoded → util.resize → xform.display`, spec §1's "scaffold-node
//! graphs"). It reuses [`lightbox_render::ng::Coalescer`], the exact pure
//! latest-wins state machine `RenderScheduler` drives internally (spec §3.7
//! task B6), rather than going through `RenderScheduler` itself, because the
//! scheduler does not expose a per-dispatch completion timestamp (it only
//! surfaces the *latest* output, by design, the shell never needs
//! per-frame timing). Driving the same coalescer directly against
//! `Engine::submit`/`poll` gets the per-render **submit→complete** latency
//! the "slider-to-screen" quantity the spec's p95 budget is about, since
//! latest-wins coalescing means the perceived lag during a churn burst is
//! bounded by one render's own latency, not the event count (that is the
//! entire point of coalescing, see `ng::sched` module docs). Percentiles are
//! computed over every render actually dispatched during the burst (not
//! synthesized).

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
use lightbox_render::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    ActiveBackend, BackendPref, BoxFuture, Coalescer, DeviceError, DeviceProvider, Engine,
    EngineConfig, Extent, NodeRegistry, OutFormat, PixelBuf, PvRange, RenderPriority,
    RenderRequest, RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry, SourceError,
    SourceImage, SourceProvider, SourceQuality, SourceWant,
};
use lightbox_types::{ImageId, PV_M0};

use crate::corpus::{synth_source, CorpusKind};

/// A scripted slider-churn scenario (spec F1).
#[derive(Clone, Debug)]
pub struct SliderScenario {
    /// Corpus source name to edit (`"gradient"`, `"checker"`, `"low-key"`,
    /// `"high-key"`, `"high-frequency"`, `"wide-gamut"`; unrecognized names
    /// fall back to `"gradient"`).
    pub source: String,
    /// Number of rapid param events to fire.
    pub events: u32,
    /// Fit-view viewport width, pixels.
    pub viewport_w: u32,
    /// Fit-view viewport height, pixels.
    pub viewport_h: u32,
}

/// Which backend actually ran the scenario (recorded so a CPU fallback run
/// is never mistaken for the GPU number, spec: "do NOT fake a number").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioBackend {
    /// Ran on a real GPU adapter.
    Gpu,
    /// No adapter was available; ran on the CPU reference path.
    Cpu,
}

/// End-to-end slider-to-screen latency percentiles (spec F1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyReport {
    /// Median latency, ms.
    pub p50_ms: f64,
    /// 95th-percentile latency, ms (the <100 ms gate).
    pub p95_ms: f64,
    /// Worst observed latency, ms.
    pub max_ms: f64,
    /// Sample count (one per render actually dispatched, coalescing means
    /// this is normally « `scenario.events`).
    pub samples: usize,
    /// Which backend produced these numbers.
    pub backend: ScenarioBackend,
}

fn parse_kind(name: &str) -> CorpusKind {
    match name {
        "checker" => CorpusKind::Checker,
        "low-key" => CorpusKind::LowKey,
        "high-key" => CorpusKind::HighKey,
        "high-frequency" => CorpusKind::HighFrequency,
        "wide-gamut" => CorpusKind::WideGamut,
        _ => CorpusKind::Gradient,
    }
}

struct HeadlessDevice {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}
impl DeviceProvider for HeadlessDevice {
    fn current(&self) -> DeviceHandles {
        (Arc::clone(&self.device), Arc::clone(&self.queue))
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        let h = (Arc::clone(&self.device), Arc::clone(&self.queue));
        Box::pin(async move { Ok(h) })
    }
}
struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unreachable!("ForceCpu never calls DeviceProvider::current")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "no device in the F1 harness".to_owned(),
            ))
        })
    }
}

/// A fixed synthetic-corpus source (the F1 "over the corpus" requirement,
/// spec §6, the engine never decodes).
struct SynthSource {
    pixels: PixelBuf,
}
impl SourceProvider for SynthSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let pixels = self.pixels.clone();
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry: SourceColorimetry::default(),
                full_extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

/// Builds a real `ng::Engine` over the PV1 scaffold-node graph
/// (`src.decoded → util.resize → xform.display`) on a GPU device when one is
/// available, else the CPU reference path, never fakes a GPU number.
fn build_engine(source: PixelBuf) -> (Engine, ScenarioBackend) {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .expect("register src.decoded");
    reg.register(
        UtilResizeNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .expect("register util.resize");
    reg.register(
        XformDisplayNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .expect("register xform.display");

    let sp: Arc<dyn SourceProvider> = Arc::new(SynthSource { pixels: source });

    match lightbox_render::GpuContext::headless() {
        Some(ctx) => {
            let dp: Arc<dyn DeviceProvider> = Arc::new(HeadlessDevice {
                device: ctx.device.clone(),
                queue: ctx.queue.clone(),
            });
            let engine = Engine::new(
                dp,
                sp,
                reg,
                EngineConfig {
                    backend: BackendPref::Auto,
                    ..EngineConfig::default()
                },
            )
            .expect("engine builds on the real adapter");
            let backend = match engine.active_backend() {
                ActiveBackend::Gpu(_) => ScenarioBackend::Gpu,
                ActiveBackend::CpuPreviewOnly => ScenarioBackend::Cpu,
            };
            (engine, backend)
        }
        None => {
            let engine = Engine::new(
                Arc::new(NullDevice),
                sp,
                reg,
                EngineConfig {
                    backend: BackendPref::ForceCpu,
                    ..EngineConfig::default()
                },
            )
            .expect("CPU-only engine builds");
            (engine, ScenarioBackend::Cpu)
        }
    }
}

/// One scripted "slider" event: a fit-view render request at a slightly
/// panned viewport (a drag proxy, see the module docs on why M1's
/// param-less `Recipe` makes a literal develop-slider unrepresentable).
fn churn_request(image: ImageId, viewport: Extent, pan_x: i32) -> RenderRequest {
    RenderRequest {
        image,
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi {
            x: pan_x,
            y: 0,
            w: viewport.w,
            h: viewport.h,
        },
        scale: RenderScale::Fit(viewport),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

/// Measure slider-to-screen p95 for `scenario` (spec F1): fires
/// `scenario.events` rapid churn events (spaced faster than a render
/// completes, a real drag) through [`Coalescer`] + `Engine::submit`/`poll`,
/// timing every render actually dispatched from its submit to its terminal
/// state. See the module docs for why this, not a literal
/// `RenderScheduler` call, is the faithful "through the scheduler" measure
/// at M1.
pub fn run_slider_latency(scenario: &SliderScenario) -> LatencyReport {
    let viewport = Extent {
        w: scenario.viewport_w.max(1),
        h: scenario.viewport_h.max(1),
    };
    let source = synth_source(parse_kind(&scenario.source), 4000, 3000);
    let (engine, backend) = build_engine(source);
    let image = ImageId(1);

    let mut coalescer: Coalescer<i32> = Coalescer::new();
    let mut latencies: Vec<Duration> = Vec::new();
    let mut inflight: Option<(lightbox_render::ng::RenderTicket, Instant)> = None;

    // Drains any terminal in-flight ticket, records its latency, and
    // promotes the pending slot (mirrors `RenderScheduler`'s own
    // `drive_once`, task B6).
    let drain = |engine: &Engine,
                 coalescer: &mut Coalescer<i32>,
                 inflight: &mut Option<(lightbox_render::ng::RenderTicket, Instant)>,
                 latencies: &mut Vec<Duration>| {
        if let Some((ticket, started)) = inflight.take() {
            match engine.poll(&ticket) {
                RenderState::Complete(_) | RenderState::PreviewReady(_) => {
                    latencies.push(started.elapsed());
                    if let Some(pan) = coalescer.complete() {
                        let t = Instant::now();
                        let ticket = engine.submit(churn_request(image, viewport, pan));
                        *inflight = Some((ticket, t));
                    }
                }
                RenderState::Failed(_) | RenderState::Cancelled => {
                    if let Some(pan) = coalescer.complete() {
                        let t = Instant::now();
                        let ticket = engine.submit(churn_request(image, viewport, pan));
                        *inflight = Some((ticket, t));
                    }
                }
                RenderState::Queued | RenderState::Rendering { .. } => {
                    *inflight = Some((ticket, started));
                }
            }
        }
    };

    // Fire events at ~120 Hz (8 ms apart), faster than a typical render
    // completes, so coalescing actually engages (a real slider drag).
    for i in 0..scenario.events {
        drain(&engine, &mut coalescer, &mut inflight, &mut latencies);
        let pan = i as i32;
        if let Some(pan) = coalescer.submit(pan) {
            let t = Instant::now();
            let ticket = engine.submit(churn_request(image, viewport, pan));
            inflight = Some((ticket, t));
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    // Drain whatever is left (in-flight + any promoted pending) to quiescence.
    let deadline = Instant::now() + Duration::from_secs(30);
    while (inflight.is_some() || coalescer.has_work()) && Instant::now() < deadline {
        drain(&engine, &mut coalescer, &mut inflight, &mut latencies);
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut ms: Vec<f64> = latencies.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if ms.is_empty() {
            return 0.0;
        }
        let rank = ((p * ms.len() as f64).ceil() as usize).clamp(1, ms.len());
        ms[rank - 1]
    };

    LatencyReport {
        p50_ms: pct(0.5),
        p95_ms: pct(0.95),
        max_ms: ms.last().copied().unwrap_or(0.0),
        samples: ms.len(),
        backend,
    }
}

/// A randomized param/zoom/pan soak configuration (spec C10).
#[derive(Clone, Copy, Debug)]
pub struct SoakConfig {
    /// Iteration count (10k for the nightly gate; reduced locally, see
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
    /// Peak per-eval working-tile pixel count (the tiling working-set ceiling
    /// bounded regardless of zoom, so VRAM stays in budget).
    pub peak_tile_pixels: u64,
    /// Whether the final frame equals an independent fresh render of the same
    /// state (no cross-iteration state corruption).
    pub final_matches_fresh: bool,
}

/// A tiny seeded xorshift64* RNG, soak determinism without a crate dep.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **F1 harness proof, run for real on this box's Metal adapter (or the
    /// CPU path if no adapter is momentarily available).** This does **not**
    /// assert the < 100 ms budget, that gate of record is nightly on the
    /// self-hosted reference GPU runner (§8; this dev box is not that
    /// runner, per `docs/plan/epics/E05-deviations.md`). It asserts the
    /// harness itself is sound (real samples, backend recorded honestly) and
    /// prints the observed numbers so a local run is always visible, never
    /// silently skipped.
    #[test]
    fn f1_slider_latency_harness_runs_for_real_and_reports_actuals() {
        let scenario = SliderScenario {
            source: "gradient".to_owned(),
            events: 150,
            viewport_w: 1024,
            viewport_h: 768,
        };
        let report = run_slider_latency(&scenario);
        eprintln!(
            "[F1] backend={:?} samples={} p50={:.2}ms p95={:.2}ms max={:.2}ms \
             (local dev-box number; NOT the §8 nightly reference-runner gate of record)",
            report.backend, report.samples, report.p50_ms, report.p95_ms, report.max_ms
        );
        assert!(report.samples > 0, "the churn burst must dispatch renders");
        assert!(
            report.samples < scenario.events as usize,
            "coalescing must drop at least some of a {}-event burst (got {} dispatches)",
            scenario.events,
            report.samples
        );
        assert!(report.p95_ms.is_finite() && report.p95_ms >= report.p50_ms);
    }
}
