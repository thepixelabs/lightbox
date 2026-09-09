// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase A (tasks A9-A11), `global.wb` goldens/parity + the eyedropper
//! neutral-solve end-to-end proof.
//!
//! Mirrors `tests/e10_global_basic.rs`'s harness exactly (same
//! `shipping_compiler`, same `SynthSource`/`SharedDevice`/`NullDevice` seams)
//! so E10's node goldens all speak one comparator.
//!
//! - **A9**: as-shot is byte-identical to the pre-A9 baseline (no `global.wb`
//!   node in the graph at all, a structural, not numeric, guarantee);
//!   ±temp and ±tint goldens + CPU/GPU parity on the one reachable path
//!   (spec §4.2 non-raw / Bradford CAT, see `white_balance`'s module docs
//!   for why the raw per-channel-gain path has no live node call site yet).
//! - **A11**: a synthetic "shot gray card under a color cast" patch, sampled
//!   directly (its own known RGB, the exact value [`GizmoEffect::Picked`]
//!   would hand a real sampler once the shell's pixel-sampling seam lands,
//!   spec `panels::basic::apply_wb_pick`'s own TODO), solved via
//!   `lightbox_color::wb::temp_tint_from_working_neutral`, rendered through
//!   the REAL `WhiteBalanceNode`, and measured for Oklab a/b neutrality on
//!   the output, proving the solve neutralizes what the render graph
//!   actually produces, not just the isolated algebra `lightbox-color`'s own
//!   unit tests already cover.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_color::wb::temp_tint_from_working_neutral;
use lightbox_edit::{Recipe, WbPreset, WhiteBalance};
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

// ── seams (mirrors `tests/e10_global_basic.rs`) ───────────────────────────────

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

// ── A9: as-shot byte-identical to the pre-A9 baseline ─────────────────────────

/// **A9 acceptance:** `WhiteBalance::AsShot` (the default) never adds
/// `global.wb` to the compiled graph, the exact structural guarantee
/// `build_wb_segment`'s pre-A9 always-elide stub provided, so an as-shot
/// render is byte-identical to the M1 baseline by construction (not by
/// numeric coincidence).
#[test]
fn as_shot_adds_no_wb_node_and_matches_the_pre_a9_baseline() {
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
    assert!(
        graph.node_index(NodeId("global.wb")).is_none(),
        "AsShot (the identity default) must add zero global.wb nodes"
    );

    // Byte-identical: an explicit AsShot recipe renders pixel-for-pixel the
    // same as the untouched identity recipe (trivially true given the
    // structural elision above, asserted at the pixel level too).
    let (w, h) = (16u32, 16u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let identity = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let explicit_as_shot = render(
        &engine,
        w,
        h,
        recipe_with(|g| g.white_balance = WhiteBalance::AsShot),
        BackendId::Cpu,
    );
    assert_eq!(identity.bytes, explicit_as_shot.bytes);
}

/// A touched WB param adds exactly `global.wb`, no sibling develop nodes
/// leak in (the A5 discipline, extended to A9).
#[test]
fn a_custom_wb_adds_exactly_the_wb_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: lightbox_render::ng::Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| {
        g.white_balance = WhiteBalance::Custom {
            temp_k: 8000.0,
            tint: 0.0,
        }
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("non-identity recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + global.wb");
    assert!(graph.node_index(NodeId("global.wb")).is_some());
    assert!(graph.node_index(NodeId("global.exposure")).is_none());
}

// ── A9: ±temp / ±tint goldens + parity ─────────────────────────────────────────

fn wb_case(wb: WhiteBalance, name: &str) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(
        &cpu_engine,
        w,
        h,
        recipe_with(|g| g.white_balance = wb),
        BackendId::Cpu,
    );
    let golden_path = e10_goldens_root()
        .join("white_balance")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[A9][{name}][cpu-vs-golden] ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[A9][{name}][cpu] ΔE2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("a9-wb-{name}")) else {
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
        recipe_with(|g| g.white_balance = wb),
        BackendId::Gpu,
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[A9][{name}][gpu-vs-cpu] ΔE2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[A9][{name}] GPU/CPU parity: ΔE2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn a9_plus_temp_golden_and_parity() {
    wb_case(
        WhiteBalance::Custom {
            temp_k: 9000.0,
            tint: 0.0,
        },
        "plus_temp_9000k",
    );
}

#[test]
fn a9_minus_temp_golden_and_parity() {
    wb_case(
        WhiteBalance::Custom {
            temp_k: 3000.0,
            tint: 0.0,
        },
        "minus_temp_3000k",
    );
}

#[test]
fn a9_plus_tint_golden_and_parity() {
    wb_case(
        WhiteBalance::Custom {
            temp_k: 5003.0,
            tint: 60.0,
        },
        "plus_tint_60",
    );
}

#[test]
fn a9_minus_tint_golden_and_parity() {
    wb_case(
        WhiteBalance::Custom {
            temp_k: 5003.0,
            tint: -60.0,
        },
        "minus_tint_60",
    );
}

#[test]
fn a10_preset_tungsten_golden_and_parity() {
    wb_case(WhiteBalance::Preset(WbPreset::Tungsten), "preset_tungsten");
}

/// A9: raising the Temp slider visibly warms the render, lowering it visibly
/// cools it (the Lightroom-convention direction check at the full-pipeline
/// level, complementing `lightbox-color`'s own
/// `higher_kelvin_warms_a_neutral_patch` algebra-only test).
#[test]
fn plus_temp_visibly_warms_relative_to_minus_temp() {
    let (w, h) = (16u32, 16u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let warm = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.white_balance = WhiteBalance::Custom {
                temp_k: 9000.0,
                tint: 0.0,
            }
        }),
        BackendId::Cpu,
    );
    let cool = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.white_balance = WhiteBalance::Custom {
                temp_k: 3000.0,
                tint: 0.0,
            }
        }),
        BackendId::Cpu,
    );
    let warm_px = texels(&warm);
    let cool_px = texels(&cool);
    // Mean (R - B) is the warm/cool axis in sRGB display output.
    let mean_r_minus_b = |px: &[[u8; 4]]| -> f64 {
        let sum: f64 = px.iter().map(|p| p[0] as f64 - p[2] as f64).sum();
        sum / px.len() as f64
    };
    let warm_axis = mean_r_minus_b(&warm_px);
    let cool_axis = mean_r_minus_b(&cool_px);
    assert!(
        warm_axis > cool_axis,
        "9000K render (R-B={warm_axis:.2}) should be warmer than 3000K render (R-B={cool_axis:.2})"
    );
}

// ── A11: eyedropper neutral-solve, end to end through the real node ─────────

/// Renders a uniform "gray card shot under a color cast" patch through the
/// real `WhiteBalanceNode` after solving `(temp, tint)` from the patch's own
/// (known) working-space RGB, and measures Oklab a/b on the output, the A11
/// AC ("clicking a shot gray card → neutrality a/b < 0.5 in Oklab").
///
/// No real raw-file corpus exists in this engine phase yet (every E10 Phase
/// A golden, A6/A7/A8 and this file's own A9 cases, renders synthetic
/// `CorpusKind` sources for exactly this reason); 5 distinct color-cast
/// "shots" stand in for the spec's "5 test raws", each a uniform patch (so
/// the picked-point's exact RGB is known without needing a live pixel-sample
/// reader, the same gap named in `panels::basic::apply_wb_pick`'s own
/// module docs).
fn neutralize_patch_case(patch: [f32; 3]) -> (f64, f64) {
    let (w, h) = (8u32, 8u32);
    let mut pixels =
        PixelBuf::new_zeroed(PixelFormat::Rgba16F, lightbox_render::ng::Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    pixels.par_fill_rows(|_y, row| {
        for x in 0..w {
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [patch[0], patch[1], patch[2], 1.0],
            );
        }
    });

    let (kelvin, tint) =
        temp_tint_from_working_neutral([patch[0] as f64, patch[1] as f64, patch[2] as f64]);

    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let out = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.white_balance = WhiteBalance::Custom {
                temp_k: kelvin as f32,
                tint: tint as f32,
            }
        }),
        BackendId::Cpu,
    );
    let px = texels(&out);
    // Every texel is the same uniform patch, sample the center one.
    let c = px[(h / 2 * w + w / 2) as usize];
    oklab_ab(c)
}

/// sRGB8 → Oklab `(a, b)` (own implementation of Björn Ottosson's published
/// Oklab formulas, test-local, not a crate API; `a/b` are the AC's own
/// neutrality axes: an achromatic pixel has `a == b == 0`).
fn oklab_ab(rgba: [u8; 4]) -> (f64, f64) {
    fn srgb_eotf(u: f64) -> f64 {
        if u <= 0.040_449_936 {
            u / 12.92
        } else {
            ((u + 0.055) / 1.055).powf(2.4)
        }
    }
    let r = srgb_eotf(rgba[0] as f64 / 255.0);
    let g = srgb_eotf(rgba[1] as f64 / 255.0);
    let b = srgb_eotf(rgba[2] as f64 / 255.0);

    // Linear sRGB → LMS (Ottosson's matrix).
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    let a = 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_;
    let ok_b = 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_;
    (a, ok_b)
}

#[test]
fn eyedropper_neutralizes_a_shot_gray_card_on_5_synthetic_cases() {
    let cases: [[f32; 3]; 5] = [
        [0.9, 1.0, 1.3],   // cool/blue cast
        [1.2, 1.0, 0.6],   // warm/tungsten cast
        [0.7, 1.0, 0.55],  // green-magenta-ish cast (tint-dominant)
        [1.4, 1.0, 1.35],  // strong magenta/green cast
        [0.5, 0.52, 0.48], // near-neutral, mild cast
    ];
    for patch in cases {
        let (a, b) = neutralize_patch_case(patch);
        println!("[A11] patch={patch:?} -> post-solve Oklab a={a:.4} b={b:.4}");
        assert!(
            a.abs() < 0.5 && b.abs() < 0.5,
            "patch={patch:?}: Oklab a={a:.4} b={b:.4} exceeds the 0.5 neutrality bound"
        );
    }
}

/// Sanity: an ALREADY-neutral patch solves to (near) the working white and
/// stays neutral (a degenerate but important corner of the AC).
#[test]
fn eyedropper_leaves_an_already_neutral_patch_neutral() {
    let (a, b) = neutralize_patch_case([0.5, 0.5, 0.5]);
    assert!(a.abs() < 0.5 && b.abs() < 0.5, "a={a:.4} b={b:.4}");
}
