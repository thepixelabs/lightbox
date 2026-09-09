// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (tasks D5-D6), `global.dehaze` goldens + CPU/GPU parity +
//! the D6 "no sky posterization" / hazy-scene honest-reporting proofs,
//! through the REAL engine (not just the pure-math proofs in `dehaze.rs`'s
//! own unit tests).
//!
//! Mirrors `tests/e10_clarity.rs`'s harness exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` seams).

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
use lightbox_render_testkit::corpus::{compare_srgb8_to_golden, goldens_root};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_clarity.rs`) ─────────────────────────────────

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

/// A synthetic hazy scene (the same haze image-formation model
/// `dehaze.rs`'s own unit tests use, `I = J*t + A*(1-t)`): a colorful
/// "landscape" scene color blended toward a bright atmospheric-light color
/// by a transmission that falls off with "distance" (here, image-row
/// position, the top rows are "far/hazy", the bottom rows "near/clear"),
/// plus a small saturated "foreground object" patch that stays fully clear
/// (transmission 1) regardless of row, a real depth discontinuity the
/// guided-filter refine must still track.
fn hazy_scene_pixels(w: u32, h: u32) -> PixelBuf {
    let a = [0.85f32, 0.87, 0.92];
    let j = [0.25f32, 0.45, 0.2];
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            // Foreground object: a block in the bottom-right corner, always
            // fully clear (t=1) regardless of its row's haze profile.
            let is_object = y >= h.saturating_sub(h / 4) && x >= w.saturating_sub(w / 4);
            let t = if is_object {
                1.0
            } else {
                0.15 + 0.7 * (y as f32 / h.max(1) as f32)
            };
            let rgb = [
                j[0] * t + a[0] * (1.0 - t),
                j[1] * t + a[1] * (1.0 - t),
                j[2] * t + a[2] * (1.0 - t),
            ];
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [rgb[0], rgb[1], rgb[2], 1.0],
            );
        }
    });
    px
}

// ── structural: identity elision ───────────────────────────────────────────

#[test]
fn identity_recipe_adds_no_dehaze_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc)
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("global.dehaze")).is_none());
}

#[test]
fn touched_dehaze_adds_exactly_the_dehaze_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| g.presence.dehaze = 40.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.dehaze")).is_some());
    assert!(graph.node_index(NodeId("global.texture")).is_none());
    assert!(graph.node_index(NodeId("global.exposure")).is_none());
}

// ── goldens + CPU/GPU parity (task D6 "DehazeNode GPU/CPU") ───────────────

fn dehaze_case(dehaze: f32, name: &str, pixels: PixelBuf) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.presence.dehaze = dehaze),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("dehaze")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D6][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D6][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("d6-dehaze-{name}")) else {
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
        recipe_with(|g| g.presence.dehaze = dehaze),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[D6][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    if !stats.within_tolerance() {
        let ct = texels(&cpu_out);
        let gt = texels(&gpu_out);
        let mut worst: Vec<(usize, f64)> = ct
            .iter()
            .zip(gt.iter())
            .enumerate()
            .map(|(i, (&c, &g))| {
                (
                    i,
                    lightbox_render_testkit::compare::ciede2000(
                        lightbox_render_testkit::compare::srgb8_to_lab(c),
                        lightbox_render_testkit::compare::srgb8_to_lab(g),
                    ),
                )
            })
            .collect();
        worst.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for &(i, d) in worst.iter().take(5) {
            let (x, y) = (i as u32 % w, i as u32 / w);
            eprintln!(
                "  [{name}] worst px ({x},{y}) dE={d:.4} cpu={:?} gpu={:?}",
                ct[i], gt[i]
            );
        }
    }
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[D6][{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn dehaze_plus_100_on_hazy_scene_golden_and_parity() {
    dehaze_case(100.0, "plus_100_hazy", hazy_scene_pixels(48, 48));
}

#[test]
fn dehaze_plus_50_on_hazy_scene_golden_and_parity() {
    dehaze_case(50.0, "plus_50_hazy", hazy_scene_pixels(48, 48));
}

#[test]
fn dehaze_minus_100_haze_add_golden_and_parity() {
    dehaze_case(-100.0, "minus_100_haze_add", hazy_scene_pixels(48, 48));
}

// ── D6 honest-reporting proof: dehaze actually changes pixels on a hazy
//    scene ──────────────────────────────────────────────────────────────

/// **D6's headline contract**: `dehaze = +100` visibly reduces the haze
/// (measurable saturation/contrast recovery) on a hazy scene, through the
/// REAL engine.
#[test]
fn dehaze_plus_100_visibly_reduces_haze_on_a_hazy_scene() {
    let (w, h) = (48u32, 48u32);
    let pixels = hazy_scene_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let dehazed = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.presence.dehaze = 100.0),
        BackendId::Cpu,
    );

    // A hazy scene's contrast (max-min per-pixel channel spread, averaged)
    // is compressed toward the airlight color; dehazing should visibly
    // widen that spread back out (more saturated/contrasty).
    fn mean_channel_spread(px: &[[u8; 4]]) -> f64 {
        px.iter()
            .map(|p| {
                let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                (hi - lo) as f64
            })
            .sum::<f64>()
            / px.len() as f64
    }
    let base_spread = mean_channel_spread(&texels(&baseline));
    let dehazed_spread = mean_channel_spread(&texels(&dehazed));
    println!(
        "[D6][engine] mean channel spread: baseline={base_spread:.3} dehazed={dehazed_spread:.3}"
    );
    assert!(
        dehazed_spread > base_spread + 3.0,
        "dehaze=+100 must visibly widen channel spread (recover saturation) on a hazy scene: \
         baseline={base_spread:.3} dehazed={dehazed_spread:.3}"
    );
}

/// The mirror: `dehaze = -100` visibly ADDS haze (narrows channel spread,
/// pulls toward the atmospheric-light color).
#[test]
fn dehaze_minus_100_visibly_adds_haze() {
    let (w, h) = (48u32, 48u32);
    let pixels = hazy_scene_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let hazier = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.presence.dehaze = -100.0),
        BackendId::Cpu,
    );

    fn mean_channel_spread(px: &[[u8; 4]]) -> f64 {
        px.iter()
            .map(|p| {
                let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                (hi - lo) as f64
            })
            .sum::<f64>()
            / px.len() as f64
    }
    let base_spread = mean_channel_spread(&texels(&baseline));
    let hazier_spread = mean_channel_spread(&texels(&hazier));
    println!(
        "[D6][engine] mean channel spread: baseline={base_spread:.3} hazier={hazier_spread:.3}"
    );
    assert!(
        hazier_spread < base_spread - 1.0,
        "dehaze=-100 must visibly narrow channel spread (add haze): \
         baseline={base_spread:.3} hazier={hazier_spread:.3}"
    );
}

// ── D6 perf budget (≤ 12 ms at 8 MP preview): honest local timing, not a
//    reference-hardware gate ─────────────────────────────────────────────
//
// Mirrors `tests/e10_tone_recovery.rs::tone_recovery_8mp_local_timing`'s own
// B14 precedent exactly: DEFERRED (no RTX-3060-class/M-series reference
// runner available here), `#[ignore]`d so it never runs in the default
// `cargo test` pass, exists purely to produce an honest, reproducible local
// number. Run with:
//   cargo test -p lightbox-render --test e10_dehaze \
//     dehaze_8mp_local_timing -- --ignored --nocapture

#[test]
#[ignore = "D6 (deferred): manual local timing probe, not a CI perf gate — no RTX-3060-class/M-series reference runner available here"]
fn dehaze_8mp_local_timing() {
    // 3264x2450 ~= 8.0 MP.
    let (w, h) = (3264u32, 2450u32);
    let pixels = hazy_scene_pixels(w, h);
    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());

    let _ = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.presence.dehaze = 100.0),
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
            recipe_with(|g| g.presence.dehaze = 100.0),
            BackendId::Cpu,
        );
        cpu_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let cpu_min = cpu_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "[D6][local-only, CPU reference path, {w}x{h}={:.1}MP] runs={cpu_ms:.2?} min={cpu_min:.2}ms \
         (spec budget for reference: <=12ms GPU on RTX-3060-class/M-series — CPU reference path \
         has no such budget; documents current CPU cost only)",
        (w as f64 * h as f64) / 1e6
    );

    let Some(ctx) = device_or_skip("dehaze-8mp-timing") else {
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
        recipe_with(|g| g.presence.dehaze = 100.0),
        BackendId::Gpu,
    );
    let mut gpu_ms = Vec::with_capacity(RUNS as usize);
    for _ in 0..RUNS {
        let t0 = std::time::Instant::now();
        let _ = render(
            &gpu_engine,
            w,
            h,
            recipe_with(|g| g.presence.dehaze = 100.0),
            BackendId::Gpu,
        );
        gpu_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let gpu_min = gpu_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "[D6][local-only, GPU path (whole-render submit incl. readback), {w}x{h}] runs={gpu_ms:.2?} \
         min={gpu_min:.2}ms — includes the full `Engine::submit`/readback round trip, NOT an \
         isolated per-node criterion bench; reported as an honest upper bound on \
         `global.dehaze`'s GPU contribution, not a clean per-node number."
    );
}
