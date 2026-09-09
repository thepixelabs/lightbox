// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task D3), `global.clarity` goldens + CPU/GPU parity + the
//! D3 halo-metric gate (reusing the B2 metric) at `clarity = +100`.
//!
//! Mirrors `tests/e10_tone_recovery.rs`'s harness exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` seams, and
//! the same `lightbox_render_testkit::halo::evaluate_halo` metric) so E10's
//! spatial-node gates all speak one comparator.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::nodes::global::clarity::{
    apply_clarity, encoded_luma, guided_filter_encoded_luma,
};
use lightbox_render::ng::nodes::global::tone_recovery::working_luma_weights;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, NodeId,
    OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, Roi, SourceColorimetry, SourceDesc, SourceError, SourceImage,
    SourceKind, SourceProvider, SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_render_testkit::corpus::{
    compare_srgb8_to_golden, goldens_root, synth_source, CorpusKind,
};
use lightbox_render_testkit::halo::{evaluate_halo, HaloConfig};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_tone_recovery.rs`) ───────────────────────────

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
fn identity_recipe_adds_no_clarity_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc)
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("global.clarity")).is_none());
}

#[test]
fn touched_clarity_adds_exactly_the_clarity_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| g.presence.clarity = 40.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.clarity")).is_some());
    assert!(graph.node_index(NodeId("global.exposure")).is_none());
    assert!(graph.node_index(NodeId("global.bw_mix")).is_none());
}

// ── goldens + CPU/GPU parity ────────────────────────────────────────────────

fn clarity_case(clarity: f32, name: &str, kind: CorpusKind) {
    let pixels = synth_source(kind, 32, 32);
    clarity_case_pixels(clarity, name, pixels);
}

fn clarity_case_pixels(clarity: f32, name: &str, pixels: PixelBuf) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.presence.clarity = clarity),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("clarity")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D3][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D3][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("d3-clarity-{name}")) else {
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
        recipe_with(|g| g.presence.clarity = clarity),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[D3][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[D3][{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn clarity_plus_100_golden_and_parity() {
    clarity_case(100.0, "plus_100", CorpusKind::HighFrequency);
}

#[test]
fn clarity_minus_100_golden_and_parity() {
    clarity_case(-100.0, "minus_100", CorpusKind::HighFrequency);
}

#[test]
fn clarity_plus_50_on_gradient_golden_and_parity() {
    clarity_case(50.0, "plus_50_gradient", CorpusKind::Gradient);
}

/// A golden that actually exercises visible detail-boosting (see
/// `ripple_pixels`'s doc comment for why the other two-level synthetic
/// corpora above don't).
#[test]
fn clarity_plus_100_on_ripple_texture_golden_and_parity() {
    clarity_case_pixels(100.0, "plus_100_ripple", ripple_pixels(48, 48));
}

// ── D3 AC: halo metric green at clarity +100 (reuses the B2 metric) ────────

fn step_edge_scene(w: u32, h: u32, dark: f32, bright: f32) -> Vec<[f32; 4]> {
    (0..h)
        .flat_map(|_| {
            (0..w).map(move |x| {
                let v = if x < w / 2 { dark } else { bright };
                [v, v, v, 1.0f32]
            })
        })
        .collect()
}

fn quant_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn to_srgb8(scene: &[[f32; 4]]) -> Vec<[u8; 4]> {
    scene
        .iter()
        .map(|p| [quant_u8(p[0]), quant_u8(p[1]), quant_u8(p[2]), 255])
        .collect()
}

/// The real guided-filter clarity, the exact math `ClarityNode::eval_cpu`
/// runs (`encoded_luma` → `guided_filter_encoded_luma` → `apply_clarity`).
fn guided_filter_clarity(scene: &[[f32; 4]], w: u32, h: u32, clarity: f32) -> Vec<[f32; 4]> {
    use lightbox_render::ng::nodes::common::guided::{box_max, box_min};
    use lightbox_render::ng::nodes::global::clarity::GUIDE_RADIUS;

    let weights = working_luma_weights();
    let luma_e: Vec<f32> = scene
        .iter()
        .map(|p| encoded_luma([p[0], p[1], p[2]], weights))
        .collect();
    let base = guided_filter_encoded_luma(&luma_e, w, h);
    let local_min = box_min(&luma_e, w, h, GUIDE_RADIUS);
    let local_max = box_max(&luma_e, w, h, GUIDE_RADIUS);
    scene
        .iter()
        .zip(luma_e.iter())
        .zip(base.iter())
        .enumerate()
        .map(|(i, ((&p, &l), &b))| apply_clarity(p, l, b, local_min[i], local_max[i], clarity))
        .collect()
}

/// **D3 AC**: the halo metric (B2) must be green on a synthetic hard-edge
/// corpus at `clarity = +100`, reusing the exact same
/// `lightbox_render_testkit::halo::evaluate_halo` metric the B2/tone_recovery
/// gate uses. The off-edge reference is the untouched input itself: a
/// well-behaved local-contrast tool changes flat/textureless regions
/// negligibly (`detail ≈ 0` there by construction, see `clarity.rs`'s own
/// `clarity_is_a_no_op_far_off_edges_on_a_flat_patch` unit test), so the
/// input is a faithful "what should the off-edge region look like" reference.
#[test]
fn halo_metric_is_green_at_clarity_plus_100() {
    let (w, h) = (64u32, 32u32);
    let scene = step_edge_scene(w, h, 0.1, 0.8);

    let input = to_srgb8(&scene);
    let real = to_srgb8(&guided_filter_clarity(&scene, w, h, 100.0));

    let cfg = HaloConfig::SYNTHETIC_DEFAULT;
    let report = evaluate_halo(&input, &real, &input, w, h, cfg);
    println!(
        "[D3][halo][clarity=+100] edge_px={} mean_reversal={:.5} (thr {:.5}) off_edge_\u{394}E={:.3} (thr {:.3}) passed={}",
        report.edge_pixel_count,
        report.mean_reversal_energy,
        cfg.reversal_threshold,
        report.off_edge_delta_e_mean,
        cfg.delta_e_threshold,
        report.passed
    );
    assert!(
        report.passed,
        "clarity=+100 must PASS the halo gate: mean_reversal={:.5} off_edge_\u{394}E={:.3}",
        report.mean_reversal_energy, report.off_edge_delta_e_mean
    );
}

/// A "plateau" scene (dark → bright → dark, spec §4.5's "backlit rim"
/// family: a bright rim/highlight bounded by dark on both sides), two
/// strong edges close together, the classic case where an unguarded local-
/// contrast boost overshoots on BOTH sides of the bright plateau.
fn plateau_scene(w: u32, h: u32, dark: f32, bright: f32) -> Vec<[f32; 4]> {
    (0..h)
        .flat_map(|_| {
            (0..w).map(move |x| {
                let v = if x >= w / 3 && x < 2 * w / 3 {
                    bright
                } else {
                    dark
                };
                [v, v, v, 1.0f32]
            })
        })
        .collect()
}

/// The halo gate also holds on a plateau (two nearby edges) at `clarity =
/// +100`.
#[test]
fn halo_metric_is_green_at_clarity_plus_100_on_a_plateau_scene() {
    let (w, h) = (90u32, 32u32);
    let scene = plateau_scene(w, h, 0.08, 0.9);

    let input = to_srgb8(&scene);
    let real = to_srgb8(&guided_filter_clarity(&scene, w, h, 100.0));

    let cfg = HaloConfig::SYNTHETIC_DEFAULT;
    let report = evaluate_halo(&input, &real, &input, w, h, cfg);
    println!(
        "[D3][halo][clarity=+100][plateau] mean_reversal={:.5} (thr {:.5}) passed={}",
        report.mean_reversal_energy, cfg.reversal_threshold, report.passed
    );
    assert!(
        report.passed,
        "clarity=+100 on a plateau scene must PASS the halo gate: mean_reversal={:.5}",
        report.mean_reversal_energy
    );
}

// ── D3 honest-reporting proof: clarity actually changes pixels ────────────

/// A mid-frequency ripple riding on a flat mid-gray field, genuine
/// "texture" (moderate, oscillating local contrast at a period comfortably
/// wider than a single pixel but still local relative to [`GUIDE_RADIUS`]),
/// the class of content clarity is actually built for. **Neither** of the
/// two-level synthetic corpora used elsewhere in this file exercise this: a
/// period-2 checkerboard ([`CorpusKind::HighFrequency`]) and a hard step
/// edge both drive the guided filter's edge-preserving `a -> 1` almost
/// everywhere (maximal local variance either way), which collapses `detail`
/// (and therefore clarity's effect) to ~0, a real, measured property of
/// the algorithm (see this fn's own git history / the module's `AutoBwMix`-
/// style honest-reporting discipline), not a defect. A period-20 ripple
/// keeps local variance moderate, so the guided filter's `base` genuinely
/// smooths across several ripple cycles instead of tracking every one as an
/// "edge", `detail` stays visibly non-zero, and clarity has something real
/// to boost.
fn ripple_pixels(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, lightbox_render::ng::Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let phase = 2.0 * std::f32::consts::PI * (x as f32) / 20.0;
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

/// Clarity +100 visibly increases local contrast on a genuine mid-frequency
/// texture (the tool actually does something), through the REAL engine.
#[test]
fn clarity_plus_100_visibly_increases_local_contrast() {
    let (w, h) = (48u32, 48u32);
    let pixels = ripple_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let boosted = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.presence.clarity = 100.0),
        BackendId::Cpu,
    );

    let bt = texels(&baseline);
    let ct = texels(&boosted);
    let mean_abs_delta: f64 = bt
        .iter()
        .zip(ct.iter())
        .map(|(a, b)| {
            ((a[0] as f64 - b[0] as f64).abs()
                + (a[1] as f64 - b[1] as f64).abs()
                + (a[2] as f64 - b[2] as f64).abs())
                / 3.0
        })
        .sum::<f64>()
        / bt.len() as f64;
    println!("[D3][engine] clarity=+100 mean |\u{394}| (8-bit) = {mean_abs_delta:.3}");
    assert!(
        mean_abs_delta > 1.0,
        "clarity=+100 must visibly change a textured scene: mean_abs_delta={mean_abs_delta:.3}"
    );
}
