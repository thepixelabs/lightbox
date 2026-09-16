// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The Detail panel's two nodes, `global.sharpen` and
//! `global.noise_reduction`, through the REAL engine: identity elision, stage
//! order, goldens, CPU/GPU parity, and the behavioural claims each node's
//! module doc makes (sharpening raises edge acutance, masking spares flat
//! areas, denoising actually removes noise).
//!
//! Mirrors `tests/e10_texture.rs`'s harness exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` seams), so
//! the two files can be read side by side.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::{NoiseReduction, Recipe, Sharpen};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent,
    NodeId, OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest,
    RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry, SourceDesc, SourceError,
    SourceImage, SourceKind, SourceProvider, SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{
    assert_edit_is_not_a_no_op, bit_adjacent, delta_e_stats, max_channel_delta, psnr,
    TOLERANCE_PSNR_DB,
};
use lightbox_render_testkit::corpus::{
    compare_srgb8_to_golden, goldens_root, synth_source, CorpusKind,
};
use lightbox_types::{ImageId, PV_M0};

const SHARPEN: NodeId = NodeId("global.sharpen");
const NOISE_REDUCTION: NodeId = NodeId("global.noise_reduction");

// ── seams (mirrors `tests/e10_texture.rs`) ─────────────────────────────────

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

fn e10_goldens_root() -> PathBuf {
    goldens_root().join("global")
}

fn source_desc() -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

// ── structural: identity elision and stage order ───────────────────────────

#[test]
fn identity_recipe_adds_neither_detail_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc())
        .expect("identity recipe compiles");
    assert!(graph.node_index(SHARPEN).is_none());
    assert!(graph.node_index(NOISE_REDUCTION).is_none());
}

#[test]
fn touched_sharpen_adds_exactly_the_sharpen_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| g.detail.sharpen.amount = 60.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(graph.node_index(SHARPEN).is_some());
    assert!(graph.node_index(NOISE_REDUCTION).is_none());
    assert!(graph.node_index(NodeId("global.texture")).is_none());
}

#[test]
fn touched_noise_reduction_adds_exactly_the_noise_reduction_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| g.detail.nr.chroma = 40.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    assert!(graph.node_index(NOISE_REDUCTION).is_some());
    assert!(graph.node_index(SHARPEN).is_none());
}

/// The shaping controls alone cannot change a pixel, so a recipe carrying
/// only a radius/detail/masking setting must still elide both nodes rather
/// than pay for a no-op spatial filter.
#[test]
fn shaping_controls_alone_still_elide_both_nodes() {
    let compiler = lightbox_render::ng::shipping_compiler();
    for recipe in [
        recipe_with(|g| g.detail.sharpen.radius = 3.0),
        recipe_with(|g| g.detail.sharpen.detail = 100.0),
        recipe_with(|g| g.detail.sharpen.masking = 100.0),
        recipe_with(|g| g.detail.nr.luma_detail = 100.0),
        recipe_with(|g| g.detail.nr.chroma_detail = 100.0),
    ] {
        let graph = compiler
            .compile(&recipe, PV_M0, &source_desc())
            .expect("recipe compiles");
        assert!(graph.node_index(SHARPEN).is_none());
        assert!(graph.node_index(NOISE_REDUCTION).is_none());
    }
}

/// **Denoise before sharpen, or you sharpen the noise.** The order the two
/// nodes are spliced in is a correctness property of the chain, not a
/// stylistic one, so it is pinned structurally rather than left to the
/// reading of `build_tone_color_segment`.
#[test]
fn noise_reduction_runs_before_sharpening() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.detail.sharpen.amount = 50.0;
        g.detail.nr.luma = 50.0;
        g.detail.nr.chroma = 50.0;
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    let nr = graph.node_index(NOISE_REDUCTION).expect("nr node present");
    let sharpen = graph.node_index(SHARPEN).expect("sharpen node present");
    assert_eq!(
        graph.inputs_of(sharpen),
        vec![nr],
        "sharpening must read the DENOISED image, i.e. noise reduction is its \
         immediate upstream"
    );
}

/// And both sit between the presence trio and the creative LUT, the position
/// `nodes/global/mod.rs`'s doc comment claims.
#[test]
fn the_detail_pair_sits_between_dehaze_and_the_creative_lut() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|g| {
        g.presence.dehaze = 30.0;
        g.detail.sharpen.amount = 50.0;
        g.detail.nr.luma = 50.0;
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc())
        .expect("recipe compiles");
    let dehaze = graph
        .node_index(NodeId("global.dehaze"))
        .expect("dehaze node present");
    let nr = graph.node_index(NOISE_REDUCTION).expect("nr node present");
    assert_eq!(
        graph.inputs_of(nr),
        vec![dehaze],
        "noise reduction reads the presence trio's output"
    );
}

// ── goldens + CPU/GPU parity ────────────────────────────────────────────────

/// Which CPU/GPU parity gate a case is held to.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Parity {
    /// The house gate: `\u{394}E2000 <= 1.0` and `PSNR >= 45 dB`
    /// (`lightbox_render_testkit::compare`), what every other E10 node's
    /// parity test uses.
    House,
    /// The shared bit-adjacent gate,
    /// `lightbox_render_testkit::compare::bit_adjacent`: no channel of any
    /// pixel, alpha included, differs by more than one 8-bit code. Its docs
    /// carry the measured table that justifies it.
    ///
    /// A case earns this gate with a number, not by association. Of the
    /// three noise-reduction cases first held to it, two measured inside
    /// the house gate (`nr_luma_100` 0.6452, `nr_chroma_100` 0.9301) and
    /// went back to `House`. Only `nr_then_sharpen` stays: 1.2241 at a
    /// one-code maximum, a bilateral filter's data-dependent weights
    /// leaving 332 of 2304 pixels on a rounding boundary and the sharpen
    /// pass then amplifying the flip.
    ///
    /// Neither gate subsumes the other, which is why both numbers are
    /// always printed: `sharpen_100` (the Gradient corpus, saturated
    /// colours) shows the mirror image, a 2-LSB max difference that
    /// `\u{394}E2000` scores a comfortable 0.80.
    BitAdjacent,
}

/// How many pixels differ at all between two renders, for the log line.
fn differing_pixels(a: &[[u8; 4]], b: &[[u8; 4]]) -> usize {
    a.iter().zip(b.iter()).filter(|(pa, pb)| pa != pb).count()
}

fn detail_case(
    name: &str,
    pixels: PixelBuf,
    parity: Parity,
    f: impl Fn(&mut lightbox_edit::GlobalStages) + Copy,
) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    // Identity first, then the edit, so the golden below is proven to pin an
    // edit that does something. `sharpen_100` shipped over a linear gradient
    // where the Gaussian of a ramp is the ramp, and its golden sat within the
    // house gate of the unsharpened render; this is what catches that.
    let identity = render(&cpu_engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let cpu_out = render(&cpu_engine, w, h, recipe_with(f), BackendId::Cpu);
    let moved = assert_edit_is_not_a_no_op(
        &texels(&identity),
        &texels(&cpu_out),
        &format!("detail/{name}"),
        2.0,
    );
    println!(
        "[detail][{name}][vs-identity] \u{394}E2000 max={:.4} mean={:.4}",
        moved.max, moved.mean
    );
    let golden_path = e10_goldens_root()
        .join("detail")
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[detail][{name}][cpu-vs-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[detail][{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("detail-{name}")) else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render(&gpu_engine, w, h, recipe_with(f), BackendId::Gpu);
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    let max_lsb = max_channel_delta(&cpu_out.bytes, &gpu_out.bytes);
    let differing = differing_pixels(&texels(&cpu_out), &texels(&gpu_out));
    // Both numbers are always printed, whichever gate the case is held to,
    // so a regression is legible either way.
    println!(
        "[detail][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={psnr_db:.2}dB \
         max_lsb={max_lsb} differing={differing}/{}",
        stats.max,
        stats.mean,
        w * h
    );
    assert!(
        psnr_db >= TOLERANCE_PSNR_DB,
        "[detail][{name}] GPU/CPU parity: PSNR {psnr_db:.2} dB below {TOLERANCE_PSNR_DB}"
    );
    match parity {
        Parity::House => assert!(
            stats.within_tolerance(),
            "[detail][{name}] GPU/CPU parity: \u{394}E2000 max {:.4}",
            stats.max
        ),
        Parity::BitAdjacent => assert!(
            bit_adjacent(&cpu_out.bytes, &gpu_out.bytes),
            "[detail][{name}] GPU/CPU parity: {max_lsb} code max channel difference, \
             PSNR {psnr_db:.2} dB"
        ),
    }
}

#[test]
fn sharpen_amount_100_golden_and_parity() {
    // Not the Gradient corpus. The Gaussian of a linear ramp is the ramp, so
    // an unsharp mask over it produces nothing except at the border, and the
    // golden this case first shipped with sat at ΔE2000 0.80 from the
    // unsharpened render, inside the house gate: it would have stayed green
    // with the node deleted. An edge is what sharpening acts on.
    detail_case(
        "sharpen_100",
        flat_plus_edge_pixels(48, 48),
        Parity::House,
        |g| {
            g.detail.sharpen = Sharpen {
                amount: 100.0,
                radius: 1.0,
                detail: 50.0,
                masking: 0.0,
            };
        },
    );
}

#[test]
fn sharpen_on_high_frequency_golden_and_parity() {
    detail_case(
        "sharpen_100_highfreq",
        synth_source(CorpusKind::HighFrequency, 32, 32),
        Parity::House,
        |g| {
            g.detail.sharpen = Sharpen {
                amount: 100.0,
                radius: 1.5,
                detail: 25.0,
                masking: 0.0,
            };
        },
    );
}

#[test]
fn sharpen_with_masking_golden_and_parity() {
    detail_case(
        "sharpen_masked",
        flat_plus_edge_pixels(48, 48),
        Parity::House,
        |g| {
            g.detail.sharpen = Sharpen {
                amount: 150.0,
                radius: 1.0,
                detail: 100.0,
                masking: 100.0,
            };
        },
    );
}

#[test]
fn sharpen_max_radius_golden_and_parity() {
    detail_case(
        "sharpen_radius_3",
        flat_plus_edge_pixels(48, 48),
        Parity::House,
        |g| {
            g.detail.sharpen = Sharpen {
                amount: 80.0,
                radius: 3.0,
                detail: 0.0,
                masking: 0.0,
            };
        },
    );
}

#[test]
fn noise_reduction_luma_golden_and_parity() {
    detail_case(
        "nr_luma_100",
        noise_patch_pixels(48, 48, 0.05, false, 0xD1CE_0001),
        Parity::House,
        |g| {
            g.detail.nr = NoiseReduction {
                luma: 100.0,
                luma_detail: 0.0,
                chroma: 0.0,
                chroma_detail: 0.0,
            };
        },
    )
}

#[test]
fn noise_reduction_chroma_golden_and_parity() {
    detail_case(
        "nr_chroma_100",
        noise_patch_pixels(48, 48, 0.05, true, 0xD1CE_0002),
        Parity::House,
        |g| {
            g.detail.nr = NoiseReduction {
                luma: 0.0,
                luma_detail: 0.0,
                chroma: 100.0,
                chroma_detail: 0.0,
            };
        },
    )
}

#[test]
fn the_whole_detail_panel_together_golden_and_parity() {
    detail_case(
        "nr_then_sharpen",
        noise_patch_pixels(48, 48, 0.04, true, 0xD1CE_0003),
        Parity::BitAdjacent,
        |g| {
            g.detail.nr = NoiseReduction {
                luma: 60.0,
                luma_detail: 40.0,
                chroma: 80.0,
                chroma_detail: 30.0,
            };
            g.detail.sharpen = Sharpen {
                amount: 70.0,
                radius: 1.0,
                detail: 50.0,
                masking: 30.0,
            };
        },
    )
}

// ── synthetic corpora ──────────────────────────────────────────────────────

/// A deterministic xorshift PRNG. No new crate dependency for a test's
/// synthetic noise, the same choice `tests/e10_texture.rs` makes.
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
    fn next_noise(&mut self, sigma: f32) -> f32 {
        (self.next_unit() * 2.0 - 1.0) * 3.0f32.sqrt() * sigma
    }
}

/// A flat mid-grey field plus i.i.d. noise. With `chroma == false` the same
/// noise sample drives all three channels (pure luminance grain); with
/// `chroma == true` each channel gets its own (which is what reads as
/// coloured blotching).
fn noise_patch_pixels(w: u32, h: u32, sigma: f32, chroma: bool, seed: u64) -> PixelBuf {
    let mut rng = XorShift64(seed);
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    // `par_fill_rows` parallelizes across rows, so the noise is generated up
    // front (deterministic, independent of row scheduling) and indexed.
    let noise: Vec<[f32; 3]> = (0..(w * h))
        .map(|_| {
            if chroma {
                [
                    0.4 + rng.next_noise(sigma),
                    0.4 + rng.next_noise(sigma),
                    0.4 + rng.next_noise(sigma),
                ]
            } else {
                let v = 0.4 + rng.next_noise(sigma);
                [v, v, v]
            }
        })
        .collect();
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let c = noise[(y * w + x) as usize];
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [c[0], c[1], c[2], 1.0],
            );
        }
    });
    px
}

/// The masking control's own corpus: the left half is a near-flat field
/// carrying only a shallow low-amplitude ripple (the "sky" a photographer
/// does not want sharpened), the right half is a hard vertical bar pattern
/// (real edges, which they do).
///
/// The ripple's period is deliberately 4 pixels, not 2. A period-2 pattern
/// is invisible to a 3x3 Sobel (the two columns it differences have the same
/// parity, so the response is identically zero), which would make the mask
/// look perfect for a reason that has nothing to do with the mask. At period
/// 4 the mask genuinely sees the ripple and genuinely decides it is not an
/// edge.
fn flat_plus_edge_pixels(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|_y, row| {
        for x in 0..w {
            let v = if x < w / 2 {
                0.55 + 0.02 * (std::f32::consts::PI * (x as f32) / 2.0).sin()
            } else if ((x - w / 2) / 4).is_multiple_of(2) {
                0.18
            } else {
                0.82
            };
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

/// Mean absolute difference between horizontally adjacent pixels, a plain
/// acutance proxy: sharpening raises it, blurring lowers it.
fn mean_abs_gradient(texels: &[[u8; 4]], w: u32, h: u32) -> f64 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in 0..h as usize {
        for x in 1..w as usize {
            let a = luma8(texels[y * w as usize + x]);
            let b = luma8(texels[y * w as usize + x - 1]);
            sum += (a - b).abs();
            n += 1.0;
        }
    }
    sum / n
}

// ── behavioural claims, through the real engine ────────────────────────────

/// Sharpening has to actually sharpen: edge acutance must rise measurably on
/// a scene that has edges.
#[test]
fn sharpening_raises_edge_acutance() {
    let (w, h) = (48u32, 48u32);
    let pixels = flat_plus_edge_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let base = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let sharp = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.detail.sharpen = Sharpen {
                amount: 100.0,
                radius: 1.0,
                detail: 100.0,
                masking: 0.0,
            };
        }),
        BackendId::Cpu,
    );
    let g0 = mean_abs_gradient(&texels(&base), w, h);
    let g1 = mean_abs_gradient(&texels(&sharp), w, h);
    println!(
        "[detail][engine] acutance base={g0:.4} sharpened={g1:.4} ratio={:.3}",
        g1 / g0
    );
    assert!(g0 > 0.5, "the corpus must have real edges: {g0}");
    assert!(
        g1 > g0 * 1.10,
        "amount=100 must raise acutance by at least 10%: {g0} -> {g1}"
    );
}

/// **The masking control, which is the one people actually rely on.** At
/// `masking = 100` the flat half of the frame must be left essentially
/// alone while the edged half still sharpens. Measured as the mean absolute
/// 8-bit change against the unsharpened render, half by half.
#[test]
fn masking_spares_the_flat_half_and_keeps_sharpening_the_edged_half() {
    let (w, h) = (48u32, 48u32);
    let pixels = flat_plus_edge_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let base = texels(&render(
        &engine,
        w,
        h,
        Recipe::identity(PV_M0),
        BackendId::Cpu,
    ));

    let with_masking = |masking: f32| {
        let out = texels(&render(
            &engine,
            w,
            h,
            recipe_with(move |g| {
                g.detail.sharpen = Sharpen {
                    amount: 150.0,
                    radius: 1.0,
                    detail: 100.0,
                    masking,
                };
            }),
            BackendId::Cpu,
        ));
        // Mean |delta| over the flat (left) half and the edged (right) half,
        // skipping a 2px margin either side of the seam so neither number
        // includes the artificial edge between the two test regions.
        let (mut flat, mut flat_n, mut edged, mut edged_n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let d = (luma8(out[y * w as usize + x]) - luma8(base[y * w as usize + x])).abs();
                if x + 2 < (w / 2) as usize {
                    flat += d;
                    flat_n += 1.0;
                } else if x > (w / 2) as usize + 2 {
                    edged += d;
                    edged_n += 1.0;
                }
            }
        }
        (flat / flat_n, edged / edged_n)
    };

    let (flat_off, edged_off) = with_masking(0.0);
    let (flat_on, edged_on) = with_masking(100.0);
    println!(
        "[detail][engine] masking off: flat={flat_off:.4} edged={edged_off:.4} | \
         masking 100: flat={flat_on:.4} edged={edged_on:.4}"
    );
    assert!(
        flat_off > 0.5,
        "without masking the flat half must visibly change, or the test proves nothing: {flat_off}"
    );
    assert!(
        flat_on < flat_off * 0.35,
        "masking=100 must cut the flat half's change by at least 65%: {flat_off} -> {flat_on}"
    );
    // 0.6, not 0.9: at masking = 100 the mask is a smoothstep against
    // MASK_FULL_GRADIENT, so even a real edge keeps only ~87% (the bars'
    // encoded step is ~0.46, which reads ~0.155 through `edge_gradient`),
    // and the flat interiors of the 4px bars are correctly suppressed too.
    // Roughly two thirds of the half's total change surviving is the honest
    // number, not a stand-in for "unchanged".
    assert!(
        edged_on > edged_off * 0.6,
        "masking=100 must keep most of the edged half's sharpening: {edged_off} -> {edged_on}"
    );
    assert!(
        edged_on > flat_on * 5.0,
        "with masking on, edges must be sharpened far harder than flat areas: \
         edged={edged_on} flat={flat_on}"
    );
}

/// Luminance denoising has to actually denoise: sigma of a flat luminance
/// noise patch must fall.
#[test]
fn luminance_noise_reduction_lowers_sigma() {
    let (w, h) = (48u32, 48u32);
    let pixels = noise_patch_pixels(w, h, 0.05, false, 0xD1CE_1111);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let base: Vec<f64> = texels(&render(
        &engine,
        w,
        h,
        Recipe::identity(PV_M0),
        BackendId::Cpu,
    ))
    .into_iter()
    .map(luma8)
    .collect();
    let denoised: Vec<f64> = texels(&render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.detail.nr = NoiseReduction {
                luma: 100.0,
                luma_detail: 0.0,
                chroma: 0.0,
                chroma_detail: 0.0,
            };
        }),
        BackendId::Cpu,
    ))
    .into_iter()
    .map(luma8)
    .collect();

    let (s0, s1) = (std_dev(&base), std_dev(&denoised));
    println!(
        "[detail][engine] luma sigma before={s0:.4} after={s1:.4} ratio={:.3}",
        s1 / s0
    );
    assert!(s0 > 1.0, "the baseline patch must be genuinely noisy: {s0}");
    assert!(
        s1 < s0 * 0.6,
        "luma=100/detail=0 must cut luminance noise sigma by at least 40%: {s0} -> {s1}"
    );
}

/// Chroma denoising has to actually remove the coloured blotching, measured
/// on the `R - G` opponent signal rather than on luma (which chroma NR is
/// built NOT to touch).
#[test]
fn chroma_noise_reduction_lowers_chroma_sigma_without_flattening_luma() {
    let (w, h) = (48u32, 48u32);
    let pixels = noise_patch_pixels(w, h, 0.05, true, 0xD1CE_2222);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let base = texels(&render(
        &engine,
        w,
        h,
        Recipe::identity(PV_M0),
        BackendId::Cpu,
    ));
    let denoised = texels(&render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.detail.nr = NoiseReduction {
                luma: 0.0,
                luma_detail: 0.0,
                chroma: 100.0,
                chroma_detail: 0.0,
            };
        }),
        BackendId::Cpu,
    ));

    let rg = |t: &[[u8; 4]]| -> Vec<f64> { t.iter().map(|p| p[0] as f64 - p[1] as f64).collect() };
    let (c0, c1) = (std_dev(&rg(&base)), std_dev(&rg(&denoised)));
    let l0 = std_dev(&base.iter().copied().map(luma8).collect::<Vec<_>>());
    let l1 = std_dev(&denoised.iter().copied().map(luma8).collect::<Vec<_>>());
    println!("[detail][engine] chroma sigma {c0:.4} -> {c1:.4}, luma sigma {l0:.4} -> {l1:.4}");
    assert!(
        c0 > 1.0,
        "the baseline patch must carry real chroma noise: {c0}"
    );
    assert!(
        c1 < c0 * 0.5,
        "chroma=100/detail=0 must at least halve the chroma noise: {c0} -> {c1}"
    );
    assert!(
        l1 > l0 * 0.85,
        "chroma-only NR must leave luminance grain broadly intact: {l0} -> {l1}"
    );
}

/// A hard edge must survive the denoiser. This is the property that
/// separates a bilateral filter from the box blur the brief rules out, so it
/// is asserted end to end, not just in the node's own unit tests.
#[test]
fn a_hard_edge_survives_full_strength_noise_reduction() {
    let (w, h) = (48u32, 48u32);
    let pixels = flat_plus_edge_pixels(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let base = texels(&render(
        &engine,
        w,
        h,
        Recipe::identity(PV_M0),
        BackendId::Cpu,
    ));
    let denoised = texels(&render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.detail.nr = NoiseReduction {
                luma: 100.0,
                luma_detail: 0.0,
                chroma: 100.0,
                chroma_detail: 0.0,
            };
        }),
        BackendId::Cpu,
    ));

    // The bar pattern lives in the right half; its acutance there is what
    // must survive.
    let right = |t: &[[u8; 4]]| -> f64 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in 0..h as usize {
            for x in ((w / 2) as usize + 2)..w as usize {
                sum += (luma8(t[y * w as usize + x]) - luma8(t[y * w as usize + x - 1])).abs();
                n += 1.0;
            }
        }
        sum / n
    };
    let (g0, g1) = (right(&base), right(&denoised));
    println!("[detail][engine] edge acutance under NR: {g0:.4} -> {g1:.4}");
    assert!(
        g1 > g0 * 0.85,
        "full-strength NR must keep at least 85% of a hard edge's acutance: {g0} -> {g1}"
    );
}
