// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The Effects stage (`fx.vignette`, `fx.grain`) through the REAL
//! engine, not just the pure-math proofs in each node's own unit tests.
//!
//! What this file pins:
//!
//! * **structural**: an identity recipe elides both nodes; a touched
//!   vignette/grain adds exactly its own node; the splice order is
//!   `geom.crop -> fx.vignette -> fx.grain -> xform.display`.
//! * **the post-crop contract**: with a crop applied, the vignette's bright
//!   core lands at the centre of the CROPPED canvas. A pre-crop vignette
//!   would put it at the crop's own offset instead, which is the visible
//!   bug this ordering exists to prevent.
//! * **the anti-shimmer contract**: the grain field is a fixed function of
//!   pixel position, so changing `amount` scales the grain in place instead
//!   of reshuffling it (which is what a per-render reseed would look like
//!   while dragging the slider).
//! * **CPU/GPU parity** for both nodes.
//!
//! Harness seams mirror `tests/e10_texture.rs` exactly (same
//! `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice`).

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::leaves::{Crop, Grain, PostCropVignette};
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

const VIGNETTE_ID: NodeId = NodeId("fx.vignette");
const GRAIN_ID: NodeId = NodeId("fx.grain");

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
            eprintln!("[{name}] no wgpu adapter available, SKIPPED (authoritative run is on main)");
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

/// A flat scene-linear patch: no structure of its own, so any variation in
/// the output is the effect under test and nothing else.
fn flat_source(w: u32, h: u32, level: f32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|_y, row| {
        for x in 0..w {
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [level, level, level, 1.0],
            );
        }
    });
    px
}

fn recipe_with(f: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r);
    r
}

fn vignette(amount: f32) -> PostCropVignette {
    PostCropVignette {
        amount,
        ..PostCropVignette::default()
    }
}

fn source_desc(w: u32, h: u32) -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w, h },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

/// Display-domain luma on the 0..255 scale, from an already-decoded texel.
fn luma8(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// The same, read straight out of an `Rgba8Srgb` [`PixelBuf`]
/// (`get_rgba_f32` normalizes to 0..1 for 8-bit formats).
fn luma8_at(px: &PixelBuf, x: u32, y: u32) -> f32 {
    let p = px.get_rgba_f32(x, y);
    255.0 * (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
}

// ── structural: identity elision + splice order ────────────────────────────

#[test]
fn an_identity_recipe_adds_neither_effects_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc(32, 32))
        .expect("identity recipe compiles");
    assert!(graph.node_index(VIGNETTE_ID).is_none());
    assert!(graph.node_index(GRAIN_ID).is_none());
}

/// A non-neutral midpoint/feather/roundness with `amount == 0` renders
/// nothing, so it must not put a node in the graph either.
#[test]
fn a_zero_amount_vignette_is_still_elided() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|r| {
        r.global.effects.postcrop_vignette = PostCropVignette {
            amount: 0.0,
            midpoint: 12.0,
            roundness: -90.0,
            feather: 95.0,
            highlights: 70.0,
        };
        r.global.effects.grain = Grain {
            amount: 0.0,
            size: 80.0,
            roughness: 40.0,
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert!(graph.node_index(VIGNETTE_ID).is_none());
    assert!(graph.node_index(GRAIN_ID).is_none());
}

#[test]
fn a_touched_vignette_adds_exactly_the_vignette_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|r| r.global.effects.postcrop_vignette = vignette(-40.0));
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + fx.vignette");
    assert!(graph.node_index(VIGNETTE_ID).is_some());
    assert!(graph.node_index(GRAIN_ID).is_none());
}

#[test]
fn touched_grain_adds_exactly_the_grain_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|r| {
        r.global.effects.grain = Grain {
            amount: 50.0,
            size: 25.0,
            roughness: 40.0,
        }
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + fx.grain");
    assert!(graph.node_index(GRAIN_ID).is_some());
    assert!(graph.node_index(VIGNETTE_ID).is_none());
}

/// The pipeline-position contract, read straight off the compiled DAG:
/// `geom.crop -> fx.vignette -> fx.grain -> xform.display`.
///
/// Grain last is not cosmetic ordering: film grain sits on top of
/// everything, the vignette's own darkening included. Grain spliced before
/// the vignette would have its own noise multiplied down by the corner
/// falloff.
#[test]
fn effects_sit_after_geometry_and_before_display_with_grain_last() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|r| {
        r.geometry.crop = Crop {
            left: 0.1,
            top: 0.1,
            right: 0.9,
            bottom: 0.9,
        };
        r.global.effects.postcrop_vignette = vignette(-40.0);
        r.global.effects.grain = Grain {
            amount: 50.0,
            size: 25.0,
            roughness: 0.0,
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 64))
        .expect("recipe compiles");

    let crop = graph
        .node_index(NodeId("geom.crop"))
        .expect("geom.crop present");
    let vig = graph.node_index(VIGNETTE_ID).expect("vignette present");
    let grain = graph.node_index(GRAIN_ID).expect("grain present");
    let display = graph
        .node_index(NodeId("xform.display"))
        .expect("xform.display present");

    assert_eq!(
        graph.inputs_of(vig),
        vec![crop],
        "the vignette must read the CROPPED canvas"
    );
    assert_eq!(
        graph.inputs_of(grain),
        vec![vig],
        "grain must sit on top of the vignette"
    );
    assert_eq!(
        graph.inputs_of(display),
        vec![grain],
        "nothing but display may follow grain"
    );
}

/// The vignette's baked canvas is the crop's output extent, not the
/// source's. This is the compile-time half of the post-crop contract; the
/// pixel-level half is `the_vignette_is_centred_on_the_cropped_canvas`.
#[test]
fn the_vignette_bakes_the_post_crop_canvas_not_the_source_extent() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with(|r| {
        r.geometry.crop = Crop {
            left: 0.5,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
        r.global.effects.postcrop_vignette = vignette(-60.0);
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 48))
        .expect("recipe compiles");
    let vig = graph.node_index(VIGNETTE_ID).expect("vignette present");
    let params = graph.params(vig);
    assert_eq!(params.get_i64("canvas_w"), Some(32));
    assert_eq!(params.get_i64("canvas_h"), Some(48));
}

// ── the post-crop contract, at pixel level ─────────────────────────────────

/// **The headline contract.** On a flat frame cropped to its right half, the
/// vignette's bright core must land at the centre of the 32-wide output. A
/// vignette centred on the ORIGINAL 64-wide frame would put its bright core
/// at output column 0 instead (the original centre is the crop's left
/// edge), which is exactly the visibly off-centre vignette this node's
/// pipeline position exists to prevent.
#[test]
fn the_vignette_is_centred_on_the_cropped_canvas() {
    let (w, h) = (64u32, 48u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_source(w, h, 0.35),
    );
    let recipe = recipe_with(|r| {
        r.geometry.crop = Crop {
            left: 0.5,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
        r.global.effects.postcrop_vignette = vignette(-90.0);
    });
    let out = render(&engine, w, h, recipe, BackendId::Cpu);
    assert_eq!(out.extent, Extent { w: 32, h: 48 });

    let mid_row = h / 2;
    let row: Vec<f32> = (0..out.extent.w)
        .map(|x| {
            let p = out.get_rgba_f32(x, mid_row);
            0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
        })
        .collect();
    // The bright core is a PLATEAU (`t == 0` inside the midpoint radius),
    // not a single peak, so locate its midpoint rather than an argmax,
    // which would just report whichever end of the plateau the iterator
    // saw last.
    let peak = row.iter().cloned().fold(f32::MIN, f32::max);
    let first = row.iter().position(|v| *v >= peak - 1e-6).unwrap();
    let last = row.iter().rposition(|v| *v >= peak - 1e-6).unwrap();
    let core_centre = (first + last) as f32 / 2.0;
    println!(
        "[effects] cropped-canvas bright core spans columns {first}..={last}, centre \
         {core_centre} of 32 (expect 15.5)"
    );
    assert!(
        (14.0..=17.0).contains(&core_centre),
        "the bright core must sit at the centre of the CROPPED canvas, got {core_centre}; \
         a core hugging column 0 would mean the vignette is still centred on the original frame"
    );

    // The decisive discriminator. On the cropped canvas the two halves are
    // mirror images, so they must be equally bright. A vignette still
    // centred on the original 64-wide frame would see this crop as its
    // right half only: monotonically darker left to right, so the two half
    // means would be far apart.
    let half = row.len() / 2;
    let left_mean = row[..half].iter().sum::<f32>() / half as f32;
    let right_mean = row[half..].iter().sum::<f32>() / half as f32;
    println!("[effects] half means: left {left_mean:.5} right {right_mean:.5}");
    assert!(
        (left_mean - right_mean).abs() < 0.01,
        "the cropped canvas's two halves must be equally vignetted: {left_mean} vs {right_mean}"
    );

    // And there is a real falloff to be centred in the first place.
    let left_edge = row[0];
    let right_edge = row[row.len() - 1];
    assert!(
        (left_edge - right_edge).abs() < 0.02,
        "the two edges must be equally dark: {left_edge} vs {right_edge}"
    );
    assert!(
        peak > left_edge + 0.05,
        "there must be a real falloff at all: core {peak} vs edge {left_edge}"
    );
}

/// Sanity companion: without a crop, the same vignette darkens the corners
/// relative to the centre.
#[test]
fn a_negative_vignette_darkens_the_corners_of_an_uncropped_frame() {
    let (w, h) = (64u32, 64u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_source(w, h, 0.35),
    );
    let base = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let out = render(
        &engine,
        w,
        h,
        recipe_with(|r| r.global.effects.postcrop_vignette = vignette(-80.0)),
        BackendId::Cpu,
    );

    let centre = luma8_at(&out, w / 2, h / 2);
    let corner = luma8_at(&out, 0, 0);
    let base_centre = luma8_at(&base, w / 2, h / 2);
    println!("[effects] uncropped vignette: centre {centre} corner {corner} base {base_centre}");
    assert!(corner < centre * 0.5, "corner {corner} vs centre {centre}");
    assert!(
        (centre - base_centre).abs() < 2.0,
        "the bright core must be left essentially untouched: {centre} vs {base_centre}"
    );
}

/// `highlights` is a real control, not decoration: at full strength a
/// blown-out frame comes through the same vignette far less darkened.
#[test]
fn highlights_protection_visibly_rescues_a_bright_frame() {
    let (w, h) = (48u32, 48u32);
    // A bright frame: this is the "vignette crushing a bright sky" case.
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_source(w, h, 1.0),
    );
    let case = |highlights: f32| {
        let out = render(
            &engine,
            w,
            h,
            recipe_with(|r| {
                r.global.effects.postcrop_vignette = PostCropVignette {
                    amount: -80.0,
                    highlights,
                    ..PostCropVignette::default()
                }
            }),
            BackendId::Cpu,
        );
        luma8_at(&out, 0, 0)
    };
    let unprotected = case(0.0);
    let protected = case(100.0);
    println!("[effects] corner luma: highlights=0 -> {unprotected}, highlights=100 -> {protected}");
    assert!(
        protected > unprotected + 20.0,
        "highlight protection must visibly lift the darkened corner: {protected} vs {unprotected}"
    );
}

// ── grain ──────────────────────────────────────────────────────────────────

#[test]
fn grain_adds_visible_variation_to_a_flat_frame() {
    let (w, h) = (64u32, 64u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_source(w, h, 0.35),
    );
    let base = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let grained = render(
        &engine,
        w,
        h,
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 100.0,
                size: 20.0,
                roughness: 0.0,
            }
        }),
        BackendId::Cpu,
    );

    let sd = |px: &PixelBuf| {
        let v: Vec<f32> = texels(px).iter().map(|p| luma8(*p)).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32).sqrt()
    };
    let base_sd = sd(&base);
    let grain_sd = sd(&grained);
    println!("[effects] flat-frame luma sd: base {base_sd:.4} -> grained {grain_sd:.4}");
    assert!(base_sd < 0.6, "the base frame really is flat: sd {base_sd}");
    assert!(
        grain_sd > 4.0,
        "grain must add real variation: sd {grain_sd} (base {base_sd})"
    );
}

/// **The anti-shimmer contract, through the engine.** Turning the amount
/// slider from 40 to 80 must scale the grain in place, not reshuffle it: the
/// per-pixel sign of the grain delta is the same field in both renders. A
/// noise field reseeded per render (the thing that makes grain crawl while
/// you drag) would decorrelate those signs to a coin flip.
#[test]
fn changing_the_amount_scales_the_grain_in_place_instead_of_reseeding_it() {
    let (w, h) = (64u32, 64u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_source(w, h, 0.35),
    );
    let case = |amount: f32| {
        render(
            &engine,
            w,
            h,
            recipe_with(|r| {
                r.global.effects.grain = Grain {
                    amount,
                    size: 12.0,
                    roughness: 0.0,
                }
            }),
            BackendId::Cpu,
        )
    };
    let base = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let weak = case(40.0);
    let strong = case(80.0);

    let b = texels(&base);
    let a1 = texels(&weak);
    let a2 = texels(&strong);
    let mut compared = 0usize;
    let mut agreed = 0usize;
    let mut weak_mag = 0.0f64;
    let mut strong_mag = 0.0f64;
    for i in 0..b.len() {
        let base_l = luma8(b[i]);
        let d1 = luma8(a1[i]) - base_l;
        let d2 = luma8(a2[i]) - base_l;
        weak_mag += d1.abs() as f64;
        strong_mag += d2.abs() as f64;
        // Skip pixels whose delta rounded away in 8-bit output; a zero has
        // no sign to agree about.
        if d1.abs() < 0.5 || d2.abs() < 0.5 {
            continue;
        }
        compared += 1;
        if d1.signum() == d2.signum() {
            agreed += 1;
        }
    }
    let ratio = agreed as f64 / compared.max(1) as f64;
    println!(
        "[effects] grain sign agreement across amounts: {agreed}/{compared} = {ratio:.4}, \
         mean |delta| {:.3} -> {:.3}",
        weak_mag / b.len() as f64,
        strong_mag / b.len() as f64
    );
    assert!(compared > 500, "not enough non-zero deltas to judge");
    assert!(
        ratio > 0.99,
        "the grain field must be the SAME field at both amounts: agreement {ratio}"
    );
    assert!(
        strong_mag > weak_mag * 1.5,
        "a bigger amount must mean a bigger delta: {weak_mag} -> {strong_mag}"
    );
}

/// Two independently built engines render byte-identical grain from the
/// same recipe: nothing in the field depends on engine or render identity.
#[test]
fn grain_is_byte_identical_across_independent_engines() {
    let (w, h) = (48u32, 48u32);
    let recipe = || {
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 70.0,
                size: 30.0,
                roughness: 50.0,
            }
        })
    };
    let a = render(
        &build_engine(
            BackendPref::ForceCpu,
            Arc::new(NullDevice),
            flat_source(w, h, 0.35),
        ),
        w,
        h,
        recipe(),
        BackendId::Cpu,
    );
    let b = render(
        &build_engine(
            BackendPref::ForceCpu,
            Arc::new(NullDevice),
            flat_source(w, h, 0.35),
        ),
        w,
        h,
        recipe(),
        BackendId::Cpu,
    );
    assert_eq!(a.bytes, b.bytes, "grain must be reproducible bit-for-bit");
}

// ── CPU/GPU parity ─────────────────────────────────────────────────────────

/// `has_grain` selects the gate, and the distinction is not a fudge.
///
/// For a smooth kernel the shared `\u{394}E2000 max <= 1.0` gate is the right
/// one and the vignette cases below hold it. A per-pixel noise field cannot:
/// grain puts a steep gradient under every pixel, so one ULP of difference
/// between a GPU `pow`/fma-contracted lerp and its Rust twin is enough to
/// tip a pixel sitting exactly on an 8-bit quantisation boundary to the
/// adjacent code, and in these tones a SINGLE code step is already worth
/// about 1.0 \u{394}E2000. A max-\u{394}E gate would therefore be testing the
/// output's quantisation grid, not the kernels.
///
/// So grain cases are gated on the stronger, more literal statement that
/// max-\u{394}E is a proxy for: **no pixel, on any channel, differs by more
/// than one 8-bit code**, plus the same PSNR floor and a mean-\u{394}E inside
/// the shared tolerance. A real algorithmic divergence moves all three.
fn parity_case(name: &str, has_grain: bool, recipe: impl Fn() -> Recipe, pixels: PixelBuf) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);
    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render(&cpu_engine, w, h, recipe(), BackendId::Cpu);

    let Some(ctx) = device_or_skip(name) else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render(&gpu_engine, w, h, recipe(), BackendId::Gpu);

    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    let max_code_diff = cpu_out
        .bytes
        .iter()
        .zip(gpu_out.bytes.iter())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    println!(
        "[effects][{name}][gpu-vs-cpu] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB \
         max_code_diff={max_code_diff}",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        psnr_db >= TOLERANCE_PSNR_DB,
        "[{name}] GPU/CPU parity: PSNR {psnr_db:.2} dB"
    );
    assert!(
        stats.mean <= 1.0,
        "[{name}] GPU/CPU parity: mean \u{394}E2000 {:.4}",
        stats.mean
    );
    if has_grain {
        assert!(
            max_code_diff <= 1,
            "[{name}] GPU/CPU parity: a channel differs by {max_code_diff} 8-bit codes"
        );
    } else {
        assert!(
            stats.within_tolerance(),
            "[{name}] GPU/CPU parity: \u{394}E2000 max {:.4}",
            stats.max
        );
    }
}

#[test]
fn vignette_cpu_gpu_parity() {
    parity_case(
        "vignette-minus-70",
        false,
        || {
            recipe_with(|r| {
                r.global.effects.postcrop_vignette = PostCropVignette {
                    amount: -70.0,
                    midpoint: 40.0,
                    roundness: 30.0,
                    feather: 60.0,
                    highlights: 50.0,
                }
            })
        },
        flat_source(64, 48, 0.35),
    );
}

#[test]
fn cropped_vignette_cpu_gpu_parity() {
    parity_case(
        "vignette-cropped",
        false,
        || {
            recipe_with(|r| {
                r.geometry.crop = Crop {
                    left: 0.25,
                    top: 0.1,
                    right: 0.9,
                    bottom: 1.0,
                };
                r.global.effects.postcrop_vignette = vignette(-60.0);
            })
        },
        flat_source(64, 48, 0.35),
    );
}

#[test]
fn grain_cpu_gpu_parity() {
    parity_case(
        "grain-60",
        true,
        || {
            recipe_with(|r| {
                r.global.effects.grain = Grain {
                    amount: 60.0,
                    size: 40.0,
                    roughness: 50.0,
                }
            })
        },
        flat_source(64, 48, 0.35),
    );
}

#[test]
fn vignette_and_grain_together_cpu_gpu_parity() {
    parity_case(
        "vignette-plus-grain",
        true,
        || {
            recipe_with(|r| {
                r.global.effects.postcrop_vignette = vignette(-50.0);
                r.global.effects.grain = Grain {
                    amount: 45.0,
                    size: 25.0,
                    roughness: 30.0,
                };
            })
        },
        flat_source(64, 48, 0.35),
    );
}

// ── committed goldens ──────────────────────────────────────────────────────
//
// **Why these exist, and why they are the only guard rail these two nodes
// have.** `pv-manifests/pv1.json` pins the three TEMPLATE stages
// (`src.decoded`, `util.resize`, `xform.display`) and nothing else;
// dynamically spliced develop nodes like `fx.vignette` and `fx.grain` are
// deliberately absent from it. `compile/manifest.rs` names two guard rails
// against a shipped kernel silently changing its output, the manifest sync
// test and the per-PV golden matrix, and for a spliced node only the second
// one can ever fire. The parity tests above prove CPU and GPU agree with
// EACH OTHER; they would keep passing happily if both drifted together.
// These goldens are what makes that drift a build failure.
//
// **Gate.** The shared, strict comparator (`compare_srgb8_to_golden`:
// \u{394}E2000 max <= 1.0 AND PSNR >= 45 dB), unrelaxed, for every case
// including the grain ones. The relaxation the GPU parity cases needed does
// NOT apply here and it would be wrong to carry it over: a golden compares a
// CPU render against a CPU-rendered PNG, one backend, one code path, so
// there is no `pow`/fma ULP divergence to absorb and grain's steep
// per-pixel gradient has nothing to amplify.
//
// Regenerate deliberately with `LIGHTBOX_BLESS=1 cargo test -p
// lightbox-render --test e12_effects`, and read the diff before committing
// it: a changed golden is a changed picture.

fn e12_goldens_root() -> PathBuf {
    // Sits beside `global/geometry` (E11's own goldens), which is the
    // established layout for a spliced segment's cases.
    goldens_root().join("global").join("effects")
}

/// Render `recipe` on the CPU reference path and hold it against the
/// committed golden `{name}_cpu.png`.
///
/// Every case first proves the effect actually did something, by rendering
/// the SAME source with an identity recipe and requiring a material
/// difference. That guards the one failure mode a golden cannot catch on
/// its own: a golden blessed from a render where the node was silently
/// elided or a parameter never reached the kernel pins the bug instead of
/// the behaviour, and then stays green forever.
fn golden_case(name: &str, recipe: Recipe, pixels: PixelBuf) {
    let (w, h) = (pixels.extent.w, pixels.extent.h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let out = render(&engine, w, h, recipe, BackendId::Cpu);
    let (ow, oh) = (out.extent.w, out.extent.h);
    let rendered = texels(&out);

    // A crop changes the extent, so those cases are self-evidently not
    // no-ops and there is nothing to compare against pixel for pixel.
    if baseline.extent == out.extent {
        let base_texels = texels(&baseline);
        let changed = delta_e_stats(&base_texels, &rendered);
        println!(
            "[effects][{name}][vs-identity] \u{394}E2000 max={:.4} mean={:.4}",
            changed.max, changed.mean
        );
        assert!(
            changed.max > 2.0,
            "[{name}] the effect changed nothing measurable against an identity render \
             (\u{394}E2000 max {:.4}); this golden would pin a no-op",
            changed.max
        );
    } else {
        println!(
            "[effects][{name}][vs-identity] extent changed {}x{} -> {ow}x{oh}, not a no-op",
            baseline.extent.w, baseline.extent.h
        );
    }
    let golden_path = e12_goldens_root()
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let report = compare_srgb8_to_golden(&golden_path, ow, oh, &rendered);
    println!(
        "[effects][{name}][cpu-vs-golden] {ow}x{oh} \u{394}E2000 max={:.4} mean={:.4} \
         PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.passed,
        "[{name}] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}; if this change is \
         intended, regenerate with LIGHTBOX_BLESS=1 and review the new picture",
        report.stats.max, report.psnr_db
    );
}

/// Non-square on purpose: a square canvas would make the roundness aspect
/// morph a no-op (see `vignette.rs`'s
/// `roundness_is_inert_on_a_square_canvas`), so a square golden could not
/// catch a regression in it.
fn golden_source() -> PixelBuf {
    synth_source(CorpusKind::Gradient, 48, 32)
}

#[test]
fn vignette_default_golden() {
    golden_case(
        "vignette_minus_70",
        recipe_with(|r| r.global.effects.postcrop_vignette = vignette(-70.0)),
        golden_source(),
    );
}

/// Pins all four shape controls at once: a tight midpoint, a fully circular
/// roundness and a wide feather.
#[test]
fn vignette_round_and_soft_golden() {
    golden_case(
        "vignette_round_soft",
        recipe_with(|r| {
            r.global.effects.postcrop_vignette = PostCropVignette {
                amount: -70.0,
                midpoint: 30.0,
                roundness: 100.0,
                feather: 90.0,
                highlights: 0.0,
            }
        }),
        golden_source(),
    );
}

/// The negative-roundness branch of the aspect morph, and a hard edge
/// (`feather = 0`, which rides the `FEATHER_MIN` floor).
#[test]
fn vignette_oval_and_hard_edged_golden() {
    golden_case(
        "vignette_oval_hard",
        recipe_with(|r| {
            r.global.effects.postcrop_vignette = PostCropVignette {
                amount: -60.0,
                midpoint: 55.0,
                roundness: -100.0,
                feather: 0.0,
                highlights: 0.0,
            }
        }),
        golden_source(),
    );
}

#[test]
fn vignette_positive_golden() {
    golden_case(
        "vignette_plus_70",
        recipe_with(|r| r.global.effects.postcrop_vignette = vignette(70.0)),
        golden_source(),
    );
}

/// Highlights protection pinned on a bright frame, the case it exists for.
/// Half strength, so the golden captures the ramp rather than just the
/// fully-protected endpoint.
#[test]
fn vignette_highlight_protection_golden() {
    golden_case(
        "vignette_highlights_50",
        recipe_with(|r| {
            r.global.effects.postcrop_vignette = PostCropVignette {
                amount: -80.0,
                highlights: 50.0,
                ..PostCropVignette::default()
            }
        }),
        golden_source(),
    );
}

/// The post-crop contract, pinned as a picture: the golden is 24x32 (the
/// cropped canvas) and its bright core is centred in it. If the splice ever
/// moves back before `geom.crop` this golden goes red on the first run.
#[test]
fn vignette_after_crop_golden() {
    golden_case(
        "vignette_after_crop",
        recipe_with(|r| {
            r.geometry.crop = Crop {
                left: 0.5,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            };
            r.global.effects.postcrop_vignette = vignette(-80.0);
        }),
        golden_source(),
    );
}

/// Grain goldens render over a FLAT patch rather than the gradient: the
/// grain field is then the only thing in the picture, so a regression in it
/// shows up as an obviously different image instead of hiding inside a
/// gradient.
fn grain_golden_source() -> PixelBuf {
    flat_source(48, 32, 0.25)
}

#[test]
fn grain_fine_golden() {
    golden_case(
        "grain_fine",
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 70.0,
                size: 10.0,
                roughness: 0.0,
            }
        }),
        grain_golden_source(),
    );
}

#[test]
fn grain_coarse_golden() {
    golden_case(
        "grain_coarse",
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 70.0,
                size: 100.0,
                roughness: 0.0,
            }
        }),
        grain_golden_source(),
    );
}

/// The roughness octave, which nothing else in the golden set exercises.
#[test]
fn grain_rough_golden() {
    golden_case(
        "grain_rough",
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 70.0,
                size: 40.0,
                roughness: 100.0,
            }
        }),
        grain_golden_source(),
    );
}

/// The realistic setting, and the one `GRAIN_RANGE`'s calibration note is
/// anchored to.
#[test]
fn grain_subtle_golden() {
    golden_case(
        "grain_subtle",
        recipe_with(|r| {
            r.global.effects.grain = Grain {
                amount: 25.0,
                size: 25.0,
                roughness: 40.0,
            }
        }),
        grain_golden_source(),
    );
}

/// Both stages together over real content, which also pins the ORDER: grain
/// is applied on top of the vignette, so the darkened corners still carry
/// full-strength grain. Swap the two nodes and this golden changes.
#[test]
fn vignette_plus_grain_golden() {
    golden_case(
        "vignette_plus_grain",
        recipe_with(|r| {
            r.global.effects.postcrop_vignette = PostCropVignette {
                amount: -55.0,
                highlights: 60.0,
                ..PostCropVignette::default()
            };
            r.global.effects.grain = Grain {
                amount: 40.0,
                size: 25.0,
                roughness: 50.0,
            };
        }),
        golden_source(),
    );
}
