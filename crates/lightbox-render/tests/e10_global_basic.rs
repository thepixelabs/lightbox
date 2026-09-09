// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase A (tasks A5-A8) integration gate.
//!
//! **The epic's first-failing-test (spec §1):** a fixed synthetic source +
//! `exposure_ev = +1.0` recipe, rendered through the real `Engine::submit`
//! (the same `shipping_compiler`/registry `lightbox-core`'s `Session` and
//! `lightbox-cli render` use), compared against a committed golden within
//! ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, on **both** GPU and CPU backends, see
//! [`a6_plus_one_ev_visibly_brightens_and_matches_golden`]. Plus:
//! - the A5 identity-elision probe (identity `GlobalStages` ⇒ zero E10 nodes
//!   in the compiled graph; a touched param adds exactly its node);
//! - A7/A8 goldens + CPU/GPU parity for `global.contrast` /
//!   `global.whites_blacks`.
//!
//! GPU legs `SKIP` (report, never fake a number) when no adapter is
//! available, mirroring `ng_parity.rs`'s `device_or_skip`, the authoritative
//! run is the single-process merge on `main`.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::Recipe;
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

// ── seams (mirrors `tests/ng_parity.rs`) --------------------------------------

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

/// Builds a real `ng::Engine` over the **shipping** PV1 configuration
/// (`shipping_compiler`, the same registry/template `lightbox-core`'s
/// `Session` and `lightbox-cli render` submit against, E10 nodes included)
/// on `pixels` as the fixed source.
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

fn mean_luma(px: &[[u8; 4]]) -> f64 {
    let sum: f64 = px
        .iter()
        .map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64)
        .sum();
    sum / px.len() as f64
}

fn recipe_with(f: impl FnOnce(&mut lightbox_edit::GlobalStages)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global);
    r
}

fn e10_goldens_root() -> PathBuf {
    goldens_root().join("global")
}

// ── A5: identity elision + non-identity node selection ------------------------

fn source_desc(w: u32, h: u32) -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w, h },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

/// **A5 acceptance:** a recipe with identity globals builds a graph
/// containing ZERO E10 nodes.
#[test]
fn identity_recipe_elides_every_e10_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc(64, 64))
        .expect("identity recipe compiles");
    assert_eq!(
        graph.node_count(),
        3,
        "identity recipe: only the 3 engine-owned stages (src.decoded, util.resize, xform.display)"
    );
    for id in ["global.exposure", "global.contrast", "global.whites_blacks"] {
        assert!(
            graph.node_index(NodeId(id)).is_none(),
            "{id} must be absent from an identity-recipe graph"
        );
    }
}

/// A touched param adds **exactly** its node, no sibling develop nodes leak
/// in, and the fixed engine-owned topology is unaffected.
#[test]
fn a_touched_param_adds_exactly_its_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| g.exposure = 1.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 64))
        .expect("non-identity recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + global.exposure");
    assert!(graph.node_index(NodeId("global.exposure")).is_some());
    assert!(graph.node_index(NodeId("global.contrast")).is_none());
    assert!(graph.node_index(NodeId("global.whites_blacks")).is_none());
}

/// All three Phase-A develop nodes touched at once: exactly 3 nodes added,
/// wired in the §4.1 order (exposure → contrast → whites_blacks).
#[test]
fn every_touched_param_adds_its_node_in_pipeline_order() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.exposure = 0.5;
        g.contrast = 20.0;
        g.whites = 10.0;
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 64))
        .expect("compiles");
    assert_eq!(graph.node_count(), 6, "3 engine stages + 3 develop nodes");
    let exposure = graph.node_index(NodeId("global.exposure")).unwrap();
    let contrast = graph.node_index(NodeId("global.contrast")).unwrap();
    let wb = graph.node_index(NodeId("global.whites_blacks")).unwrap();
    // exposure -> contrast -> whites_blacks, so contrast's (only) input is
    // exposure and whites_blacks' (only) input is contrast.
    assert_eq!(graph.inputs_of(contrast), vec![exposure]);
    assert_eq!(graph.inputs_of(wb), vec![contrast]);
}

// ── A6: the epic's first-failing-test -----------------------------------------

/// **A6 / the epic's first-failing-test.** A fixed synthetic source with
/// `exposure_ev = +1.0` rendered through the real `Engine::submit`:
/// - visibly brightens (mean rendered luma strictly greater than the ev=0
///   render of the same source);
/// - matches a committed golden within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB on the
///   **CPU** reference backend (deterministic, always runs);
/// - CPU/GPU parity within the same tolerance when a GPU adapter is present.
#[test]
fn a6_plus_one_ev_visibly_brightens_and_matches_golden() {
    let (w, h) = (32u32, 32u32);
    // Low-key source (dim, well below 1.0) so +1 EV's ×2 gain is visible
    // without uniformly clipping to white, the clean "visibly brightens"
    // demonstration the epic asks for.
    let pixels = synth_source(CorpusKind::LowKey, w, h);

    // CPU leg: always runs (no adapter needed), deterministic, the
    // committed-golden regression pin.
    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_identity = render(&cpu_engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let cpu_plus1ev = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.exposure = 1.0),
        BackendId::Cpu,
    );

    // A ×2 *linear* gain compresses to roughly ×2^(1/2.4) ≈ 1.35-1.4 in the
    // sRGB-encoded 8-bit output (the display transform's transfer function),
    // so the encoded-domain brighten is real but sub-linear, assert a
    // comfortably-conservative bound under that, well above measurement noise.
    let luma_before = mean_luma(&texels(&cpu_identity));
    let luma_after = mean_luma(&texels(&cpu_plus1ev));
    assert!(
        luma_after > luma_before * 1.2,
        "+1 EV must visibly brighten: before={luma_before:.2} after={luma_after:.2}"
    );

    let golden_path = e10_goldens_root()
        .join("exposure")
        .join("pv1")
        .join("plus1ev_lowkey_cpu.png");
    let rendered = texels(&cpu_plus1ev);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[A6][cpu] before-mean-luma={luma_before:.3} after-mean-luma={luma_after:.3} \
         ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance(),
        "[A6][cpu] ΔE2000 max {:.4} exceeds 1.0 vs {golden_path:?}",
        report.stats.max
    );
    assert!(
        report.psnr_db >= TOLERANCE_PSNR_DB,
        "[A6][cpu] PSNR {:.2} dB below {TOLERANCE_PSNR_DB} vs {golden_path:?}",
        report.psnr_db
    );

    // GPU leg: CPU/GPU parity on the same +1 EV recipe (skips honestly if no
    // adapter, never fakes a GPU number).
    let Some(ctx) = device_or_skip("a6-exposure-parity") else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_plus1ev = render(
        &gpu_engine,
        w,
        h,
        recipe_with(|g| g.exposure = 1.0),
        BackendId::Gpu,
    );
    let gpu_texels = texels(&gpu_plus1ev);
    let cpu_texels = texels(&cpu_plus1ev);
    let stats = delta_e_stats(&cpu_texels, &gpu_texels);
    let psnr_db = psnr(&cpu_plus1ev.bytes, &gpu_plus1ev.bytes);
    println!(
        "[A6][gpu-vs-cpu] ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance(),
        "[A6] GPU/CPU parity: ΔE2000 max {:.4} exceeds 1.0",
        stats.max
    );
    assert!(
        psnr_db >= TOLERANCE_PSNR_DB,
        "[A6] GPU/CPU parity: PSNR {psnr_db:.2} dB below {TOLERANCE_PSNR_DB}"
    );
}

// ── A7: contrast goldens + parity ---------------------------------------------

fn contrast_case(amount: f32, name: &str) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.contrast = amount),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("contrast")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[A7][{name}][cpu] ΔE2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("a7-contrast-{name}")) else {
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
        recipe_with(|g| g.contrast = amount),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[A7][{name}] GPU/CPU parity: ΔE2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn a7_contrast_plus100_golden_and_parity() {
    contrast_case(100.0, "plus100_gradient");
}

#[test]
fn a7_contrast_minus100_golden_and_parity() {
    contrast_case(-100.0, "minus100_gradient");
}

// ── A8: whites/blacks goldens + parity ----------------------------------------

fn whites_blacks_case(whites: f32, blacks: f32, name: &str) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| {
            g.whites = whites;
            g.blacks = blacks;
        }),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("whites_blacks")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[A8][{name}][cpu] ΔE2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("a8-whites-blacks-{name}")) else {
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
            g.whites = whites;
            g.blacks = blacks;
        }),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[A8][{name}] GPU/CPU parity: ΔE2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn a8_whites_plus100_golden_and_parity() {
    whites_blacks_case(100.0, 0.0, "whites_plus100");
}

#[test]
fn a8_blacks_minus100_golden_and_parity() {
    whites_blacks_case(0.0, -100.0, "blacks_minus100");
}

#[test]
fn a8_mixed_endpoints_golden_and_parity() {
    whites_blacks_case(60.0, -60.0, "mixed_60");
}
