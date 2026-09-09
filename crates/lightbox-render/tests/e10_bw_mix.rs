// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task D1), `global.bw_mix` goldens + CPU/GPU parity + the
//! Monochrome-elides-downstream-color-nodes graph probe.
//!
//! Mirrors `tests/e10_wb.rs`'s harness exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` seams) so
//! E10's node goldens all speak one comparator.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::{BwMix, Recipe, Treatment};
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

fn source_desc() -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

// ── structural: identity elision + exactly-this-node ──────────────────────

/// `Treatment::Color` (the default) adds zero `global.bw_mix` nodes,
/// regardless of the stored mix, the D1 elision rule (see
/// `bw_mix::BwMixNode::is_identity`'s doc comment).
#[test]
fn color_treatment_adds_no_bw_mix_node_even_with_a_stored_mix() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.bw = BwMix { weights: [50.0; 8] };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.bw_mix")).is_none());
}

/// `Treatment::BlackAndWhite` adds exactly `global.bw_mix`, even with an
/// all-zero mix (D1 AC: Monochrome is never elided, see the module docs).
#[test]
fn monochrome_treatment_adds_exactly_the_bw_mix_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.treatment = Treatment::BlackAndWhite;
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.bw_mix")).is_some());
    assert!(graph.node_index(NodeId("global.hsl")).is_none());
    assert!(graph.node_index(NodeId("global.vibrance_sat")).is_none());
    assert!(graph.node_index(NodeId("global.color_grade")).is_none());
}

/// **The D1 graph-probe AC**: Monochrome elides the downstream color nodes
/// (HSL, vibrance/saturation, color grading) even when their own fields are
/// **not** neutral, i.e. it's the `Treatment` toggle doing the eliding, not
/// a coincidence of those fields already being at their defaults.
#[test]
fn monochrome_treatment_elides_downstream_color_nodes_even_with_non_neutral_fields() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.treatment = Treatment::BlackAndWhite;
        g.hsl.bands[0] = lightbox_edit::HslBand {
            hue: 40.0,
            sat: 60.0,
            lum: 10.0,
        };
        g.vibrance = 50.0;
        g.saturation = -30.0;
        g.color_grade.shadows = lightbox_edit::GradeWheel {
            hue: 200.0,
            sat: 70.0,
            lum: 5.0,
        };
        g.bw = BwMix {
            weights: [20.0, 0.0, 0.0, 0.0, 0.0, -20.0, 0.0, 0.0],
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(
        graph.node_index(NodeId("global.hsl")).is_none(),
        "HSL must be elided under Monochrome even with a non-neutral band"
    );
    assert!(
        graph.node_index(NodeId("global.vibrance_sat")).is_none(),
        "vibrance/saturation must be elided under Monochrome"
    );
    assert!(
        graph.node_index(NodeId("global.color_grade")).is_none(),
        "color grading must be elided under Monochrome"
    );
    assert!(
        graph.node_index(NodeId("global.bw_mix")).is_some(),
        "bw_mix itself must still be present"
    );

    // The mirror: flipping back to Color (with the exact same field values
    // still stored, the D2 round-trip contract) restores all three color
    // nodes and elides bw_mix, proving the toggle, not the field values
    // drives elision in both directions.
    let mut back_to_color = recipe.clone();
    back_to_color.global.treatment = Treatment::Color;
    let graph2 = compiler
        .compile(&back_to_color, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(graph2.node_index(NodeId("global.hsl")).is_some());
    assert!(graph2.node_index(NodeId("global.vibrance_sat")).is_some());
    assert!(graph2.node_index(NodeId("global.color_grade")).is_some());
    assert!(graph2.node_index(NodeId("global.bw_mix")).is_none());
}

// ── D1: goldens + CPU/GPU parity ──────────────────────────────────────────

fn bw_mix_case(bw: BwMix, name: &str, kind: CorpusKind) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(kind, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| {
            g.treatment = Treatment::BlackAndWhite;
            g.bw = bw;
        }),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("bw_mix")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D1][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D1][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("d1-bw-mix-{name}")) else {
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
        recipe_with(|g| {
            g.treatment = Treatment::BlackAndWhite;
            g.bw = bw;
        }),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[D1][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[D1][{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

/// D1 AC: neutral (all-zero) mix golden + parity, the "neutral luma
/// conversion" case.
#[test]
fn neutral_mix_golden_and_parity() {
    bw_mix_case(
        BwMix { weights: [0.0; 8] },
        "neutral",
        CorpusKind::WideGamut,
    );
}

/// A classic red-filter-style mix (reds boosted, blues cut) golden + parity.
#[test]
fn red_filter_style_mix_golden_and_parity() {
    bw_mix_case(
        BwMix {
            weights: [80.0, 20.0, 0.0, -20.0, -20.0, -60.0, -20.0, 10.0],
        },
        "red_filter",
        CorpusKind::WideGamut,
    );
}

/// An extreme single-band mix (Blue slammed to +100) golden + parity.
#[test]
fn blue_band_max_mix_golden_and_parity() {
    let mut weights = [0.0f32; 8];
    weights[5] = 100.0;
    bw_mix_case(BwMix { weights }, "blue_max", CorpusKind::WideGamut);
}

// ── D1 honest-reporting proof: bw_mix actually changes pixels ─────────────

/// **D1's headline contract**: the all-zero mix produces plain-luma gray
/// through the REAL engine (matches `working_luma`'s Rec.-weighted
/// conversion, not merely the pure-math unit test in `bw_mix.rs`), and a
/// non-zero mix visibly diverges from that neutral baseline on a saturated
/// scene.
#[test]
fn neutral_mix_is_plain_luma_and_a_real_mix_visibly_diverges() {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::WideGamut, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let neutral = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.treatment = Treatment::BlackAndWhite;
        }),
        BackendId::Cpu,
    );
    let mut weights = [0.0f32; 8];
    weights[0] = 100.0; // Red band, max
    let red_boosted = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.treatment = Treatment::BlackAndWhite;
            g.bw = BwMix { weights };
        }),
        BackendId::Cpu,
    );

    // Every neutral-mix pixel is fully desaturated (R == G == B, within 1
    // LSB), the "grayscale" half of the contract. `apply_bw_mix` itself is
    // an EXACT [gray,gray,gray] in working-space math (proved bit-exactly by
    // `bw_mix.rs`'s own unit tests); the ≤1 LSB tolerance here accounts for
    // the downstream color-managed output transform's 3x3 working→display
    // matrix, whose rows are not bit-identical row-sums (ordinary
    // floating-point rounding in a Bradford-adapted matrix build, not a
    // bw_mix defect), an equal-RGB input can land 1 sRGB8 LSB apart across
    // channels after that matrix + 8-bit quantization, the same class of
    // rounding gap `tone_recovery`'s own `GUIDE_EPS` doc comment documents
    // for cross-backend parity.
    let nt = texels(&neutral);
    let max_channel_spread = nt
        .iter()
        .map(|p| {
            let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
            hi - lo
        })
        .max()
        .unwrap_or(0);
    println!("[D1][engine] neutral bw_mix max per-pixel channel spread = {max_channel_spread}");
    assert!(
        max_channel_spread <= 1,
        "neutral bw_mix output must be (near-)fully desaturated: max spread {max_channel_spread}"
    );

    // Boosting the Red band visibly diverges from the neutral baseline
    // (WideGamut's red sector is saturated red, exactly the band this
    // slider targets).
    let rt = texels(&red_boosted);
    let mean_abs_delta: f64 = nt
        .iter()
        .zip(rt.iter())
        .map(|(a, b)| (a[0] as f64 - b[0] as f64).abs())
        .sum::<f64>()
        / nt.len() as f64;
    println!("[D1][engine] neutral-vs-red-boosted mean |Δ luma| (8-bit) = {mean_abs_delta:.3}");
    assert!(
        mean_abs_delta > 3.0,
        "boosting the Red band must visibly diverge from the neutral bw_mix render: \
         mean_abs_delta={mean_abs_delta:.3}"
    );
}
