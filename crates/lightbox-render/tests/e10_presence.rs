// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task D7), presence golden + perf consolidation: clarity
//! (D3), texture (D4), and dehaze (D5-D6) folded into one combined-recipe
//! golden/parity scenario and one perf-consolidation timing scenario,
//! mirroring `tests/e10_tone_recovery.rs`/`tests/e10_dehaze.rs`'s own
//! harness (same `shipping_compiler`/`SynthSource`/`SharedDevice`/
//! `NullDevice` seams).

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent,
    NodeId, OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest,
    RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry, SourceDesc, SourceError,
    SourceImage, SourceKind, SourceProvider, SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_render_testkit::corpus::{
    compare_srgb8_to_golden, goldens_root, synth_source, CorpusKind,
};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_dehaze.rs`) ──────────────────────────────────

struct SharedDevice {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}
impl DeviceProvider for SharedDevice {
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
        Box::pin(async { Err(DeviceError::Rebuild("no device in this harness".to_owned())) })
    }
}

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

fn device_or_skip(name: &str) -> Option<GpuContext> {
    match GpuContext::headless() {
        Some(c) => Some(c),
        None => {
            eprintln!(
                "[{name}] no wgpu adapter available — SKIPPED (authoritative run is on main)"
            );
            None
        }
    }
}

fn build_engine(pref: BackendPref, dp: Arc<dyn DeviceProvider>, pixels: PixelBuf) -> Engine {
    let compiler = lightbox_render::ng::shipping_compiler();
    Engine::with_compiler(
        dp,
        Arc::new(SynthSource { pixels }),
        compiler,
        EngineConfig {
            backend: pref,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds over the shipping PV1 configuration")
}

fn render(engine: &Engine, w: u32, h: u32, recipe: Recipe, expect: BackendId) -> PixelBuf {
    let req = RenderRequest {
        image: ImageId(1),
        pv: recipe.pv,
        recipe,
        roi: Roi { x: 0, y: 0, w, h },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Batch,
        cancel: CancelToken::new(),
    };
    let ticket = engine.submit(req);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(out) => {
                assert_eq!(out.backend, expect, "backend provenance");
                let OutputPayload::Pixels(px) = out.payload else {
                    panic!("expected pixels");
                };
                assert_eq!(px.format, PixelFormat::Rgba8Srgb);
                return px;
            }
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => {
                if std::time::Instant::now() > deadline {
                    panic!("render did not complete within 20s");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

fn texels(px: &PixelBuf) -> Vec<[u8; 4]> {
    px.bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

fn recipe_with(f: impl FnOnce(&mut lightbox_edit::GlobalStages)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global);
    r
}

fn e10_goldens_root() -> PathBuf {
    goldens_root().join("global")
}

/// The combined presence recipe: clarity + texture + dehaze all active
/// together (task D7 "fold clarity/texture/dehaze into the existing perf/
/// golden scenarios").
fn all_presence_active(g: &mut lightbox_edit::GlobalStages) {
    g.presence.clarity = 60.0;
    g.presence.texture = 40.0;
    g.presence.dehaze = 50.0;
}

// ── D7: combined-recipe presence golden + CPU/GPU parity ──────────────────

#[test]
fn all_presence_nodes_are_present_together_in_the_compiled_graph() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(all_presence_active);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.clarity")).is_some());
    assert!(graph.node_index(NodeId("global.texture")).is_some());
    assert!(graph.node_index(NodeId("global.dehaze")).is_some());
    // And in the spec §4.1 pinned order: clarity -> texture -> dehaze, all
    // after bw_mix (elided here, Color treatment) and before any later
    // (not-yet-built) creative-lut stage.
}

#[test]
fn presence_combined_golden_and_parity() {
    let (w, h) = (40u32, 40u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(all_presence_active),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("presence")
        .join("pv1")
        .join("combined_cpu.png");
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D7][combined][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D7][combined][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip("d7-presence-combined") else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render(
        &gpu_engine,
        w,
        h,
        recipe_with(all_presence_active),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[D7][combined][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[D7][combined] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

/// Round-trip sanity: toggling all three presence sliders back to zero
/// exactly reproduces the identity render (every one of the three nodes'
/// own "param=0 is an exact identity" property, composed).
#[test]
fn presence_all_zero_matches_the_identity_render_exactly() {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let identity = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let zeroed = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.presence.clarity = 0.0;
            g.presence.texture = 0.0;
            g.presence.dehaze = 0.0;
        }),
        BackendId::Cpu,
    );
    assert_eq!(texels(&identity), texels(&zeroed));
}

// ── D7 perf consolidation: honest local timing (not a reference-hardware
//    gate, see `tests/e10_dehaze.rs::dehaze_8mp_local_timing`'s own
//    precedent for why) ────────────────────────────────────────────────
//
// Run with:
//   cargo test -p lightbox-render --test e10_presence \
//     presence_worst_case_local_timing -- --ignored --nocapture

#[test]
#[ignore = "D7 (deferred): manual local timing probe, not a CI perf gate — no RTX-3060-class/M-series reference runner available here"]
fn presence_worst_case_local_timing() {
    // A moderate (not full 8 MP) extent: dehaze's atmospheric-light
    // single-workgroup reduction is a known, NAMED perf gap at large
    // resolutions (see `docs/plan/epics/E10-deviations.md`'s D5/D6 entry)
    // this probe still documents the honest current number at a size that
    // completes in reasonable local wall-clock time.
    let (w, h) = (1024u32, 768u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());

    let _ = render(
        &cpu_engine,
        w,
        h,
        recipe_with(all_presence_active),
        BackendId::Cpu,
    );
    const RUNS: u32 = 3;
    let mut cpu_ms = Vec::with_capacity(RUNS as usize);
    for _ in 0..RUNS {
        let t0 = std::time::Instant::now();
        let _ = render(
            &cpu_engine,
            w,
            h,
            recipe_with(all_presence_active),
            BackendId::Cpu,
        );
        cpu_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let cpu_min = cpu_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "[D7][local-only, CPU reference path, {w}x{h}={:.2}MP, clarity+texture+dehaze all active] \
         runs={cpu_ms:.2?} min={cpu_min:.2}ms (spec §4.4 budget table: clarity<=8ms + \
         texture<=8ms + dehaze<=12ms = 28ms GPU reference budget for this trio — CPU reference \
         path has no such budget; documents current CPU cost only)",
        (w as f64 * h as f64) / 1e6
    );

    let Some(ctx) = device_or_skip("presence-worst-case-timing") else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let _ = render(
        &gpu_engine,
        w,
        h,
        recipe_with(all_presence_active),
        BackendId::Gpu,
    );
    let mut gpu_ms = Vec::with_capacity(RUNS as usize);
    for _ in 0..RUNS {
        let t0 = std::time::Instant::now();
        let _ = render(
            &gpu_engine,
            w,
            h,
            recipe_with(all_presence_active),
            BackendId::Gpu,
        );
        gpu_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let gpu_min = gpu_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "[D7][local-only, GPU path (whole-render submit incl. readback), {w}x{h}] runs={gpu_ms:.2?} \
         min={gpu_min:.2}ms — includes the full submit/readback round trip, NOT isolated \
         per-node criterion benches; reported as an honest upper bound, not a clean per-node \
         number. KNOWN GAP (named in E10-deviations.md): dehaze's single-workgroup \
         atmospheric-light reduction does not scale its parallelism with image size — this is \
         the dominant cost at large resolutions and the named lever for a follow-up \
         hierarchical (multi-workgroup, two-stage) reduction."
    );
}
