// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase C (tasks C11-C12), `global.color_grade` goldens + CPU/GPU
//! parity + the full-pipeline "color grading changes pixels" proof.
//!
//! Mirrors `tests/e10_wb.rs`'s harness exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` seams) so
//! E10's node goldens all speak one comparator.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::{GradeWheel, Recipe};
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

// ── seams (mirrors `tests/e10_wb.rs`) ─────────────────────────────────────

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

// ── structural: identity elision + exactly-this-node ──────────────────────

/// An identity `ColorGrade` (every wheel neutral) adds zero
/// `global.color_grade` nodes, the A5/C12 identity-elision discipline.
#[test]
fn identity_color_grade_adds_no_node() {
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
    assert!(graph.node_index(NodeId("global.color_grade")).is_none());
}

/// A touched wheel adds exactly `global.color_grade`, no sibling develop
/// nodes leak in (the A5 discipline, extended to C12). A non-default HUE
/// with zero sat/lum is STILL identity (C11's own AC), so this uses a
/// non-zero saturation to actually exercise the node.
#[test]
fn a_touched_wheel_adds_exactly_the_color_grade_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| {
        g.color_grade.highlights = GradeWheel {
            hue: 40.0,
            sat: 60.0,
            lum: 0.0,
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("non-identity recipe compiles");
    assert_eq!(
        graph.node_count(),
        4,
        "3 engine stages + global.color_grade"
    );
    assert!(graph.node_index(NodeId("global.color_grade")).is_some());
    assert!(graph.node_index(NodeId("global.hsl")).is_none());
}

// ── C12: goldens + CPU/GPU parity ──────────────────────────────────────────

fn grade_case(cg: lightbox_edit::ColorGrade, name: &str) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.color_grade = cg),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("color_grade")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[C12][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[C12][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("c12-grade-{name}")) else {
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
        recipe_with(|g| g.color_grade = cg),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[C12][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[C12][{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

/// C12 AC: warm-highlights reference look golden + parity.
#[test]
fn c12_warm_highlights_golden_and_parity() {
    let cg = lightbox_edit::ColorGrade {
        highlights: GradeWheel {
            hue: 40.0,
            sat: 70.0,
            lum: 0.0,
        },
        ..lightbox_edit::ColorGrade::default()
    };
    grade_case(cg, "warm_highlights");
}

/// C12 AC: cool-shadows reference look golden + parity.
#[test]
fn c12_cool_shadows_golden_and_parity() {
    let cg = lightbox_edit::ColorGrade {
        shadows: GradeWheel {
            hue: 220.0,
            sat: 70.0,
            lum: 0.0,
        },
        ..lightbox_edit::ColorGrade::default()
    };
    grade_case(cg, "cool_shadows");
}

/// C12 AC: a combined warm-highlights/cool-shadows split-tone look (the
/// classic "teal and orange" grade) golden + parity, exercises three of the
/// four wheels together (shadows + highlights + a mild global push).
#[test]
fn c12_split_tone_golden_and_parity() {
    let cg = lightbox_edit::ColorGrade {
        shadows: GradeWheel {
            hue: 200.0,
            sat: 50.0,
            lum: -5.0,
        },
        highlights: GradeWheel {
            hue: 35.0,
            sat: 50.0,
            lum: 5.0,
        },
        blend: 60.0,
        balance: -10.0,
        ..lightbox_edit::ColorGrade::default()
    };
    grade_case(cg, "split_tone");
}

// ── C11/C12 honest-reporting proof: color grading actually changes pixels ──

/// A warm-highlights wheel visibly warms the render's bright regions more
/// than its dark regions, through the REAL engine (GPU-capable CPU
/// reference path), not just the pure-math proof in `color_grade.rs`'s own
/// unit tests.
#[test]
fn warm_highlights_wheel_visibly_warms_bright_regions_more_than_dark_ones() {
    let (w, h) = (32u32, 32u32);
    // LowKey/HighKey corpora give genuinely distinct dark-vs-bright samples
    // to compare within one render.
    let dark_pixels = synth_source(CorpusKind::LowKey, w, h);
    let bright_pixels = synth_source(CorpusKind::HighKey, w, h);

    let cg = lightbox_edit::ColorGrade {
        highlights: GradeWheel {
            hue: 40.0,
            sat: 90.0,
            lum: 0.0,
        },
        ..lightbox_edit::ColorGrade::default()
    };
    let recipe_neutral = Recipe::identity(PV_M0);
    let recipe_graded = recipe_with(|g| g.color_grade = cg);

    let dark_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), dark_pixels);
    let dark_before = render(&dark_engine, w, h, recipe_neutral.clone(), BackendId::Cpu);
    let dark_after = render(&dark_engine, w, h, recipe_graded.clone(), BackendId::Cpu);

    let bright_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), bright_pixels);
    let bright_before = render(&bright_engine, w, h, recipe_neutral, BackendId::Cpu);
    let bright_after = render(&bright_engine, w, h, recipe_graded, BackendId::Cpu);

    let mean_r_minus_b = |px: &[[u8; 4]]| -> f64 {
        let sum: f64 = px.iter().map(|p| p[0] as f64 - p[2] as f64).sum();
        sum / px.len() as f64
    };
    let dark_shift = mean_r_minus_b(&texels(&dark_after)) - mean_r_minus_b(&texels(&dark_before));
    let bright_shift =
        mean_r_minus_b(&texels(&bright_after)) - mean_r_minus_b(&texels(&bright_before));
    println!("[C11/C12][engine] dark R-B shift={dark_shift:.3} bright R-B shift={bright_shift:.3}");
    assert!(
        bright_shift > dark_shift + 1.0,
        "a highlights-only warm wheel must warm the bright corpus render's R-B axis much more \
         than the dark corpus render's: dark_shift={dark_shift:.3} bright_shift={bright_shift:.3}"
    );
    // And it must be a REAL, visible shift (not a rounding-noise no-op).
    assert!(
        bright_shift > 2.0,
        "bright-region warm shift should be clearly visible: {bright_shift:.3}"
    );
}
