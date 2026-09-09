// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task D4), `global.texture` goldens + CPU/GPU parity + the
//! D4 noise-amplification guard (σ of a flat noise patch grows < 1.2× at
//! `texture = +100`), through the REAL engine (not just the pure-math proof
//! in `texture.rs`'s own unit tests).
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
use lightbox_render_testkit::corpus::{
    compare_srgb8_to_golden, goldens_root, synth_source, CorpusKind,
};
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

// ── structural: identity elision ───────────────────────────────────────────

#[test]
fn identity_recipe_adds_no_texture_node() {
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
    assert!(graph.node_index(NodeId("global.texture")).is_none());
}

#[test]
fn touched_texture_adds_exactly_the_texture_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| g.presence.texture = 30.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.texture")).is_some());
    assert!(graph.node_index(NodeId("global.clarity")).is_none());
    assert!(graph.node_index(NodeId("global.exposure")).is_none());
}

// ── goldens + CPU/GPU parity ────────────────────────────────────────────────

fn texture_case(texture: f32, name: &str, kind: CorpusKind) {
    let pixels = synth_source(kind, 32, 32);
    texture_case_pixels(texture, name, pixels);
}

fn texture_case_pixels(texture: f32, name: &str, pixels: PixelBuf) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.presence.texture = texture),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("texture")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D4][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D4][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("d4-texture-{name}")) else {
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
        recipe_with(|g| g.presence.texture = texture),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[D4][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[D4][{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn texture_plus_100_golden_and_parity() {
    texture_case(100.0, "plus_100", CorpusKind::Gradient);
}

#[test]
fn texture_minus_100_golden_and_parity() {
    texture_case(-100.0, "minus_100", CorpusKind::Gradient);
}

#[test]
fn texture_plus_50_on_high_frequency_golden_and_parity() {
    texture_case(50.0, "plus_50_highfreq", CorpusKind::HighFrequency);
}

/// A golden that actually exercises visible band-pass boosting (see
/// `ripple_pixels`'s doc comment for why the two-level synthetic corpora
/// above understate texture's real effect).
#[test]
fn texture_plus_100_on_ripple_golden_and_parity() {
    texture_case_pixels(100.0, "plus_100_ripple", ripple_pixels(48, 48));
}

// ── D4 AC: noise-amplification guard, through the real engine ─────────────

/// A deterministic xorshift PRNG (no new crate dependency, see
/// `texture.rs`'s own unit test for the identical rationale).
struct XorShift64(u64);
impl XorShift64 {
    fn next_unit(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// A flat i.i.d.-noise patch: mid-gray plus uniform per-pixel noise, no
/// spatial structure at all, the D4 AC's own corpus ("σ of a flat noise
/// patch").
fn noise_patch_pixels(w: u32, h: u32, sigma: f32, seed: u64) -> PixelBuf {
    let mut rng = XorShift64(seed);
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    // par_fill_rows parallelizes across rows, so pre-generate the noise
    // sequentially (deterministic, order-independent of row scheduling)
    // then index into it row-by-row.
    let noise: Vec<f32> = (0..(w * h))
        .map(|_| 0.5 + sigma * (rng.next_unit() * 2.0 - 1.0) * 3.0f32.sqrt())
        .collect();
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let v = noise[(y * w + x) as usize];
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [v, v, v, 1.0],
            );
        }
    });
    px
}

fn luma8(p: [u8; 4]) -> f64 {
    (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0
}

fn std_dev(vals: &[f64]) -> f64 {
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64).sqrt()
}

/// **D4's headline AC**: σ of a flat noise patch grows `< 1.2×` at
/// `texture = +100`, measured through the REAL engine (not just the
/// pure-math `texture.rs` unit test).
#[test]
fn noise_amplification_guard_texture_plus_100() {
    let (w, h) = (64u32, 64u32);
    let pixels = noise_patch_pixels(w, h, 0.06, 0xD1CE_5EED);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let boosted = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.presence.texture = 100.0),
        BackendId::Cpu,
    );

    let base_luma: Vec<f64> = texels(&baseline).into_iter().map(luma8).collect();
    let boosted_luma: Vec<f64> = texels(&boosted).into_iter().map(luma8).collect();
    let sigma_before = std_dev(&base_luma);
    let sigma_after = std_dev(&boosted_luma);
    let ratio = sigma_after / sigma_before;
    println!(
        "[D4][engine] noise patch \u{3c3} before={sigma_before:.4} after={sigma_after:.4} ratio={ratio:.4}"
    );
    assert!(
        ratio < 1.2,
        "texture=+100 must grow noise \u{3c3} by < 1.2\u{d7}: ratio={ratio:.4}"
    );
    // And it must be a REAL non-degenerate measurement (not two near-zero
    // sigmas producing a meaningless ratio).
    assert!(
        sigma_before > 1.0,
        "baseline noise patch must have real variance: \u{3c3}={sigma_before:.4}"
    );
}

// ── D4 honest-reporting proof: texture actually changes pixels ────────────

/// A mid-frequency ripple (period comfortably between `2*R1` and `2*R2`, the
/// band `blur(R1) - blur(R2)` is actually built to capture) riding on a flat
/// mid-gray field. A period-2 checkerboard
/// ([`lightbox_render_testkit::corpus::CorpusKind::HighFrequency`]) is a
/// poor fit here for the same reason it was for clarity
/// (`tests/e10_clarity.rs::ripple_pixels`'s doc comment): its extreme
/// alternation sits mostly OUTSIDE the R1..R2 band and gets clamped down by
/// the halo guard, understating texture's real effect on genuine texture.
fn ripple_pixels(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let phase = 2.0 * std::f32::consts::PI * (x as f32) / 8.0;
            let v = 0.5 + 0.3 * phase.sin();
            let _ = y;
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [v, v, v, 1.0],
            );
        }
    });
    px
}

#[test]
fn texture_plus_100_visibly_changes_a_textured_scene() {
    let (w, h) = (48u32, 48u32);
    let pixels = ripple_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let boosted = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.presence.texture = 100.0),
        BackendId::Cpu,
    );

    let bt = texels(&baseline);
    let ct = texels(&boosted);
    let mean_abs_delta: f64 = bt
        .iter()
        .zip(ct.iter())
        .map(|(a, b)| (a[0] as f64 - b[0] as f64).abs())
        .sum::<f64>()
        / bt.len() as f64;
    println!("[D4][engine] texture=+100 mean |\u{394}| (8-bit) = {mean_abs_delta:.3}");
    assert!(
        mean_abs_delta > 1.0,
        "texture=+100 must visibly change a high-frequency scene: mean_abs_delta={mean_abs_delta:.3}"
    );
}
