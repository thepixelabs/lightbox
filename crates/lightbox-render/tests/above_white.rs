// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Every node this wave added, driven over pixels **above white** at full
//! strength. Nothing here tests that a node works on a recovered highlight;
//! it tests that no node **corrupts** one.
//!
//! # Why this file exists
//!
//! Highlight recovery (`e3be42c`) lets working-space values above 1.0 reach
//! the develop graph; before it, nothing could. Every node in this wave was
//! written and tested against `[0, 1]`. A reviewer traced what each does to a
//! pixel at 2.28 and found one that corrupted the picture: `geom.defringe`
//! read a recovered sky, strongly negative on its opponent axis and sitting
//! on a high-contrast edge, as a textbook magenta fringe, and zeroed the
//! green of every shadow pixel within its window. That is fixed, and this
//! file is the gate that stops the next node doing the same.
//!
//! # What is and is not observable here
//!
//! The engine's output is display-transformed, and the display transform
//! clamps to white, correctly, because a display cannot show above white. So
//! the working values themselves are never visible at this seam. What is
//! visible: a recovered plateau, all three channels above 1.0, must come out
//! as pure white under every node, and a neutral dark field beside it must
//! stay neutral. A node that pulls any channel of the plateau below 1.0 shows
//! up as a channel below 255; a node that strips green from the field shows
//! up as a magenta cast. Both are exactly the corruption signatures, in the
//! one domain the engine hands back.
//!
//! What this file deliberately does not assert: that sharpening, noise
//! reduction or grain do anything **inside** the plateau. They do not, today,
//! because each encodes luma through `companion_encode`, which clamps at 1.
//! That is the same class as `clarity` and is on the companion-domain
//! roadmap; pinning "does nothing" here would freeze the limitation rather
//! than guard against corruption.

use std::sync::Arc;

use lightbox_edit::{Grain, LensCorrection, NoiseReduction, PostCropVignette, Recipe, Sharpen};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent,
    OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, Roi, SourceColorimetry, SourceError, SourceImage, SourceProvider,
    SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{bit_adjacent, delta_e_stats, max_channel_delta, psnr};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_detail.rs`) ────────────────────────────────

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
            eprintln!("[{name}] no wgpu adapter available: SKIPPED (authoritative run is on main)");
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
                    panic!("render did not complete within 30s");
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

// ── the corpus ───────────────────────────────────────────────────────────

const W: u32 = 48;
const H: u32 = 32;
/// Columns strictly left of this are the plateau, at and right of it the field.
const EDGE: u32 = 24;

/// A recovered sky beside a shadow. The plateau is the shape highlight
/// recovery actually produces: green clipped first, red and blue carrying
/// real signal above it, so `[1.9, 1.2, 1.3]` rather than `[2, 2, 2]`. Its
/// opponent channel is strongly negative and it sits on a hard edge, which
/// is what made it look like a fringe. The field is neutral so any cast a
/// node introduces is unambiguous.
fn blown_sky() -> PixelBuf {
    let extent = Extent { w: W, h: H };
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba32F, extent);
    for y in 0..H {
        for x in 0..W {
            let p = if x < EDGE {
                [1.9, 1.2, 1.3, 1.0]
            } else {
                [0.2, 0.2, 0.2, 1.0]
            };
            px.set_rgba_f32(x, y, p);
        }
    }
    px
}

/// Every pixel with all three channels above 1.0 must display as white.
/// A node that pulls a channel of the plateau below 1.0 shows here as a
/// channel below 255. The check runs on the plateau **interior**, at least
/// `margin` columns from the edge, so a node that legitimately blends
/// across the edge (a resample, a blur) is not accused of corrupting the
/// plateau for doing its job at the boundary.
fn assert_plateau_stays_white(label: &str, out: &[[u8; 4]], margin: u32) {
    for y in 0..H {
        for x in 0..EDGE.saturating_sub(margin) {
            let p = out[(y * W + x) as usize];
            assert!(
                p[0] == 255 && p[1] == 255 && p[2] == 255,
                "[{label}] plateau pixel ({x},{y}) came out {p:?}; a recovered highlight \
                 with every channel above white must display as white, so a channel \
                 below 255 means the node pulled it below 1.0"
            );
        }
    }
}

/// The field is neutral going in and must be neutral coming out. A node
/// that strips one channel shows here as a cast. The tolerance is one code:
/// 8-bit rounding can split a neutral by one, not more.
fn assert_field_stays_neutral(label: &str, out: &[[u8; 4]], margin: u32) {
    for y in 0..H {
        for x in (EDGE + margin)..W {
            let p = out[(y * W + x) as usize];
            let hi = p[0].max(p[1]).max(p[2]);
            let lo = p[0].min(p[1]).min(p[2]);
            assert!(
                hi - lo <= 1,
                "[{label}] field pixel ({x},{y}) came out {p:?}; it went in neutral and a \
                 spread of {} codes across channels is a cast the node introduced",
                hi - lo
            );
        }
    }
}

/// The three columns of field nearest the edge are where defringe's window
/// overlaps the plateau, and where it used to zero the green. They must keep
/// their green within a few codes of their red and blue. This is looser than
/// the neutral check because a resampling node may legitimately pull a
/// little plateau light into these columns; what it may not do is take a
/// channel away.
fn assert_edge_field_keeps_green(label: &str, out: &[[u8; 4]]) {
    for y in 0..H {
        for x in EDGE..(EDGE + 3) {
            let p = out[(y * W + x) as usize];
            let rb = p[0].max(p[2]);
            assert!(
                p[1] + 6 >= rb,
                "[{label}] edge-adjacent field pixel ({x},{y}) came out {p:?}: green {} \
                 against red/blue {rb}, which is the magenta rim defringe used to paint \
                 around a recovered highlight",
                p[1]
            );
        }
    }
}

// ── the cases ────────────────────────────────────────────────────────────

struct Case {
    name: &'static str,
    /// Columns of plateau interior to skip next to the edge: the reach of
    /// the node's kernel or resample, so a legitimate boundary blend is not
    /// read as corruption.
    plateau_margin: u32,
    /// Same, on the field side, for the neutrality check.
    field_margin: u32,
    recipe: fn() -> Recipe,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "sharpen_100",
            plateau_margin: 6,
            field_margin: 6,
            recipe: || {
                recipe_with(|g| {
                    g.detail.sharpen = Sharpen {
                        amount: 100.0,
                        radius: 1.5,
                        detail: 50.0,
                        masking: 0.0,
                    }
                })
            },
        },
        Case {
            name: "noise_reduction_100",
            plateau_margin: 8,
            field_margin: 8,
            recipe: || {
                recipe_with(|g| {
                    g.detail.nr = NoiseReduction {
                        luma: 100.0,
                        luma_detail: 50.0,
                        chroma: 100.0,
                        chroma_detail: 50.0,
                    }
                })
            },
        },
        Case {
            name: "vignette_minus_80",
            plateau_margin: 0,
            field_margin: 0,
            recipe: || {
                recipe_with(|g| {
                    g.effects.postcrop_vignette = PostCropVignette {
                        amount: -80.0,
                        ..PostCropVignette::default()
                    }
                })
            },
        },
        Case {
            name: "grain_80",
            plateau_margin: 0,
            field_margin: 0,
            recipe: || {
                recipe_with(|g| {
                    g.effects.grain = Grain {
                        amount: 80.0,
                        size: 40.0,
                        roughness: 50.0,
                    }
                })
            },
        },
        Case {
            name: "lens_distortion",
            plateau_margin: 10,
            field_margin: 10,
            recipe: || {
                recipe_with(|g| {
                    g.optics.lens_profile = Some(LensCorrection {
                        manual_distortion: -40.0,
                        ..LensCorrection::default()
                    })
                })
            },
        },
        Case {
            name: "defringe_100",
            plateau_margin: 4,
            field_margin: 4,
            recipe: || recipe_with(|g| g.optics.defringe = 100.0),
        },
        Case {
            name: "everything_at_once",
            plateau_margin: 10,
            field_margin: 10,
            recipe: || {
                recipe_with(|g| {
                    g.detail.sharpen.amount = 100.0;
                    g.detail.nr.luma = 100.0;
                    g.detail.nr.chroma = 100.0;
                    g.effects.postcrop_vignette.amount = -60.0;
                    g.effects.grain.amount = 60.0;
                    g.optics.lens_profile = Some(LensCorrection {
                        manual_distortion: -30.0,
                        ..LensCorrection::default()
                    });
                    g.optics.defringe = 100.0;
                })
            },
        },
    ]
}

/// The control: with no edit, the plateau is white and the field neutral,
/// so the assertions below are known to be satisfiable and known to be
/// measuring the node rather than the harness.
#[test]
fn the_control_renders_the_plateau_white_and_the_field_neutral() {
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), blown_sky());
    let out = texels(&render(
        &engine,
        W,
        H,
        Recipe::identity(PV_M0),
        BackendId::Cpu,
    ));
    assert_plateau_stays_white("identity", &out, 0);
    assert_field_stays_neutral("identity", &out, 0);
    assert_edge_field_keeps_green("identity", &out);
    let field = out[(H / 2 * W + W - 1) as usize];
    assert!(
        field[0] > 100 && field[0] < 140,
        "the field should display as a mid grey, got {field:?}; if it is white the corpus \
         is not what this file thinks it is"
    );
}

/// Vignette is the one node that is **supposed** to darken the plateau.
/// A -80 vignette reaches most of a 48x32 frame, so "stays white" is the
/// wrong assertion for it. The right one is about how it darkens. A common
/// multiplicative gain takes `[1.9, 1.2, 1.3]` to `[1.9g, 1.2g, 1.3g]`: red
/// stays above white longest, green drops below it first, and the recovered
/// cast becomes visible exactly as it would on a real sky. That preserves
/// the channel ordering green < blue < red. The corruption, a clamp to white
/// **before** the multiply, would give `[g, g, g]`: a neutral grey with the
/// recovered colour thrown away. So for these cases the plateau check is
/// the ordering, not the value.
fn darkens_by_design(name: &str) -> bool {
    name.starts_with("vignette") || name == "everything_at_once"
}

/// For a node that darkens the plateau on purpose: every plateau interior
/// pixel keeps the source's channel ordering, green <= blue <= red, within
/// a code of rounding. A pixel still at white trivially satisfies it. A
/// pixel that came out neutral grey below white fails it, and that is the
/// clamp-before-multiply signature.
fn assert_plateau_keeps_its_cast(label: &str, out: &[[u8; 4]], margin: u32) {
    for y in 0..H {
        for x in 0..EDGE.saturating_sub(margin) {
            let p = out[(y * W + x) as usize];
            let (r, g, b) = (u16::from(p[0]), u16::from(p[1]), u16::from(p[2]));
            assert!(
                g <= b + 1 && b <= r + 1,
                "[{label}] plateau pixel ({x},{y}) came out {p:?}; the source was \
                 [1.9, 1.2, 1.3] so a common gain must leave green <= blue <= red. A \
                 neutral result means the node clamped to white before darkening and \
                 threw the recovered colour away"
            );
        }
    }
}

#[test]
fn no_new_node_corrupts_a_recovered_highlight_on_the_cpu() {
    for case in cases() {
        let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), blown_sky());
        let out = texels(&render(&engine, W, H, (case.recipe)(), BackendId::Cpu));
        if darkens_by_design(case.name) {
            assert_plateau_keeps_its_cast(case.name, &out, case.plateau_margin);
        } else {
            assert_plateau_stays_white(case.name, &out, case.plateau_margin);
        }
        assert_field_stays_neutral(case.name, &out, case.field_margin);
        assert_edge_field_keeps_green(case.name, &out);
        println!(
            "[above-white][{}][cpu] plateau white, field neutral",
            case.name
        );
    }
}

/// The same on the GPU, with parity against the CPU render. A node that
/// handled above-white values differently on the two backends would be a
/// preview that lies about the export.
#[test]
fn no_new_node_corrupts_a_recovered_highlight_on_the_gpu_and_the_backends_agree() {
    let Some(ctx) = device_or_skip("above-white") else {
        return;
    };
    for case in cases() {
        let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), blown_sky());
        let cpu_out = render(&cpu_engine, W, H, (case.recipe)(), BackendId::Cpu);

        let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
            device: Arc::clone(&ctx.device),
            queue: Arc::clone(&ctx.queue),
        });
        let gpu_engine = build_engine(BackendPref::Auto, dp, blown_sky());
        let gpu_out = render(&gpu_engine, W, H, (case.recipe)(), BackendId::Gpu);
        let gpu = texels(&gpu_out);

        if darkens_by_design(case.name) {
            assert_plateau_keeps_its_cast(case.name, &gpu, case.plateau_margin);
        } else {
            assert_plateau_stays_white(case.name, &gpu, case.plateau_margin);
        }
        assert_field_stays_neutral(case.name, &gpu, case.field_margin);
        assert_edge_field_keeps_green(case.name, &gpu);

        let stats = delta_e_stats(&texels(&cpu_out), &gpu);
        let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
        let max_lsb = max_channel_delta(&cpu_out.bytes, &gpu_out.bytes);
        println!(
            "[above-white][{}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={psnr_db:.2}dB \
             max_lsb={max_lsb}",
            case.name, stats.max, stats.mean
        );
        // Grain and noise reduction put a data-dependent gradient under every
        // pixel and are held to the bit-adjacent gate everywhere else in the
        // tree; the same reasoning applies here. See
        // `lightbox_render_testkit::compare::bit_adjacent`.
        let high_frequency = case.name.starts_with("grain")
            || case.name.starts_with("noise")
            || case.name == "everything_at_once";
        if high_frequency {
            assert!(
                bit_adjacent(&cpu_out.bytes, &gpu_out.bytes),
                "[{}] GPU/CPU parity above white: {max_lsb} code max channel difference",
                case.name
            );
        } else {
            assert!(
                stats.within_tolerance(),
                "[{}] GPU/CPU parity above white: \u{394}E2000 max {:.4}",
                case.name,
                stats.max
            );
        }
    }
}
