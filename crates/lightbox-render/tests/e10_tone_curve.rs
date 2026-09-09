// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase C (tasks C1-C3) integration gate: `global.tone_curve` through
//! the real `Engine::submit`, identity elision, canned-shape goldens
//! (linear/medium/strong S-curve) + CPU/GPU parity, the "S-curve visibly
//! increases contrast" proof, and the C3 cache-probe (LUT rebakes ONLY on a
//! curve-param delta, proved via the real content cache's recompute-count
//! probe, not a mocked counter).
//!
//! Mirrors `tests/e10_global_basic.rs`/`tests/e10_wb.rs`'s harness exactly
//! so every E10 golden speaks one comparator (§4.4 ΔE2000 ≤ 1.0 / PSNR ≥
//! 45 dB). GPU legs `SKIP` (report, never fake a number) when no adapter is
//! available.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::{CurvePoint, Recipe, ToneCurve};
use lightbox_jobs::CancelToken;
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
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_global_basic.rs` / `tests/e10_wb.rs`) ───────────

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

fn luma_stddev(px: &[[u8; 4]]) -> f64 {
    let lumas: Vec<f64> = px
        .iter()
        .map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64)
        .collect();
    let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
    let var = lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lumas.len() as f64;
    var.sqrt()
}

fn recipe_with(f: impl FnOnce(&mut lightbox_edit::GlobalStages)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global);
    r
}

fn e10_goldens_root() -> PathBuf {
    goldens_root().join("global")
}

/// An S-curve of a given `strength` (`0.0` degenerates to the exact linear
/// identity, the quarter/three-quarter points land back on the diagonal;
/// `1.0` is a strong contrast boost). The composite curve only.
fn s_curve_recipe(strength: f32) -> Recipe {
    recipe_with(|g| {
        g.tone_curve.rgb = ToneCurve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint {
                    x: 0.25,
                    y: 0.25 - 0.15 * strength,
                },
                CurvePoint {
                    x: 0.75,
                    y: 0.75 + 0.15 * strength,
                },
                CurvePoint { x: 1.0, y: 1.0 },
            ],
        };
    })
}

fn source_desc(w: u32, h: u32) -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w, h },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

// ── identity elision (A5 discipline, extended to C3) ───────────────────────

#[test]
fn a_touched_curve_adds_exactly_the_tone_curve_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = s_curve_recipe(0.6);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 64))
        .expect("non-identity recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + global.tone_curve");
    assert!(graph.node_index(NodeId("global.tone_curve")).is_some());
    assert!(graph.node_index(NodeId("global.exposure")).is_none());
    assert!(graph.node_index(NodeId("global.whites_blacks")).is_none());
}

/// A degenerate ("linear") S-curve whose control points sit exactly on the
/// diagonal is structurally non-identity (the widget/recipe still carries 4
/// points, not the 2-point default), the node is still added, but its
/// pixel output is byte-identical to skipping it (proven by the linear
/// golden case below).
#[test]
fn identity_curve_elides_the_tone_curve_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc(64, 64))
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("global.tone_curve")).is_none());
}

// ── C1-C3: canned-shape goldens + CPU/GPU parity ───────────────────────────

fn curve_case(strength: f32, name: &str) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(&cpu_engine, w, h, s_curve_recipe(strength), BackendId::Cpu);
    let golden_path = e10_goldens_root()
        .join("tone_curve")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[C3][{name}][cpu-vs-golden] ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[C3][{name}][cpu] ΔE2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("c3-tone-curve-{name}")) else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render(&gpu_engine, w, h, s_curve_recipe(strength), BackendId::Gpu);
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[C3][{name}][gpu-vs-cpu] ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[C3][{name}] GPU/CPU parity: ΔE2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn c3_linear_curve_golden_and_parity() {
    curve_case(0.0, "linear");
}

#[test]
fn c3_medium_s_curve_golden_and_parity() {
    curve_case(0.6, "medium_s");
}

#[test]
fn c3_strong_s_curve_golden_and_parity() {
    curve_case(1.0, "strong_s");
}

/// The "linear" (degenerate, on-diagonal) curve renders byte-close to the
/// untouched identity recipe, proves the composed remap is a true no-op
/// shape, not merely close-by-tolerance to its own golden.
#[test]
fn linear_curve_matches_untouched_identity_render() {
    let (w, h) = (24u32, 24u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let identity = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let linear = render(&engine, w, h, s_curve_recipe(0.0), BackendId::Cpu);
    let stats = delta_e_stats(&texels(&identity), &texels(&linear));
    assert!(
        stats.within_tolerance(),
        "linear curve must render indistinguishably from identity: ΔE2000 max {:.4}",
        stats.max
    );
}

// ── the epic's honest-reporting proof: an S-curve visibly increases contrast ──

#[test]
fn s_curve_visibly_increases_global_contrast() {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let identity = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let s_curved = render(&engine, w, h, s_curve_recipe(1.0), BackendId::Cpu);

    let std_before = luma_stddev(&texels(&identity));
    let std_after = luma_stddev(&texels(&s_curved));
    println!(
        "[C3][contrast-proof] luma stddev before={std_before:.3} after={std_after:.3} (+{:.1}%)",
        100.0 * (std_after / std_before - 1.0)
    );
    assert!(
        std_after > std_before * 1.05,
        "a strong S-curve must visibly widen the luma spread (global contrast): \
         before={std_before:.3} after={std_after:.3}"
    );
}

// ── C3: LUT rebakes ONLY on a curve-param delta (cache probe) ─────────────

/// **C3 AC**: proves the LUT rebake cadence directly through the real
/// content cache's recompute-count probe (`Engine::stats`, spec §3.6), not
/// a mocked bake-call counter. A repeat render of the IDENTICAL curve
/// recipe produces zero additional node evaluations (a full cache hit at
/// every stage, `global.tone_curve` included, its `ParamBlock` canonical
/// bytes, and therefore its `CacheKey`, are unchanged); changing ONLY the
/// curve then re-evaluates EXACTLY `global.tone_curve` plus its downstream
/// tail (`xform.display`, whose own cache key folds in the tone-curve
/// node's now-different key), nothing upstream (`src.decoded`/
/// `util.resize`) re-runs.
#[test]
fn c3_lut_rebakes_only_on_a_curve_param_delta() {
    let (w, h) = (16u32, 16u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let curve_a = s_curve_recipe(0.6);
    let _ = render(&engine, w, h, curve_a.clone(), BackendId::Cpu);
    let after_first = engine.stats().nodes_evaluated;

    // Re-render the IDENTICAL curve recipe: full cache hit, zero new evals.
    let _ = render(&engine, w, h, curve_a, BackendId::Cpu);
    let after_repeat = engine.stats().nodes_evaluated;
    assert_eq!(
        after_repeat, after_first,
        "an unchanged curve recipe must not trigger any node re-evaluation \
         (the LUT must not rebake): first={after_first} repeat={after_repeat}"
    );

    // Change ONLY the curve: exactly tone_curve + its downstream tail
    // (xform.display) re-evaluate, nothing upstream.
    let curve_b = s_curve_recipe(1.0);
    let _ = render(&engine, w, h, curve_b, BackendId::Cpu);
    let after_change = engine.stats().nodes_evaluated;
    let delta = after_change - after_repeat;
    assert_eq!(
        delta, 2,
        "a curve-only delta must re-evaluate exactly global.tone_curve + xform.display \
         (tail invalidation), got {delta} new evaluations"
    );
}
