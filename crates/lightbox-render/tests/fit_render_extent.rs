// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The **fit-render extent contract** gate: a develop edit must never change
//! *which part of the image* a render covers.
//!
//! # The bug this pins
//!
//! Owner report: *"I moved the Tint (relative) slider and it zoomed in on the
//! top left corner for some reason and got stuck on it."*
//!
//! The shell submits `roi = Roi { 0, 0, viewport_w, viewport_h }` with
//! `RenderScale::Fit(viewport)` (`canvas::view::submit_if_changed` →
//! `RenderScheduler::to_request`), while the `SourceProvider` hands over the
//! source at its **own** (much larger) extent, the scale ladder's decimation
//! is `util.resize`'s job, not the provider's. So inside one render there are
//! genuinely two extents in play: the source's, and the request's.
//!
//! `Executor::evaluate` used to seed its target-extent walk from `roi` for
//! *every* input-less node, the source stage included, even though the source
//! stage's real output tile is the full-size decoded source. Every node
//! between `src.decoded` and `util.resize` was therefore sized to the
//! **request** extent while being handed a **source**-extent input tile, and
//! every pointwise kernel, CPU and GPU alike, reads `input[x, y]` over its own
//! *output* extent:
//!
//! ```text
//!   global_white_balance.wgsl:  textureLoad(src, vec2<i32>(gid.xy), 0)
//!   white_balance.rs eval_cpu:  input.get_rgba_f32(x, y)
//! ```
//!
//! which is a **top-left crop** of the source, not a resample of it. The PV1
//! template splices exactly one segment before `util.resize`, the WB segment
//! (`compile::RecipeCompiler::compile`), so moving Temp or Tint cropped the
//! render to the top-left `viewport_w × viewport_h` source pixels, which
//! `canvas::view` then stretched over the whole image rect: "zoomed in on the
//! top left corner". Every other develop node is spliced *after*
//! `util.resize`, where input and target extents already agree, so no other
//! slider showed it, the asymmetry is the fingerprint of the root cause.
//!
//! # What each test guards
//!
//! - [`every_develop_param_keeps_the_fit_render_framing`], the general gate.
//!   The same marked source is rendered at one fixed viewport under an
//!   identity recipe and under each of fourteen off-identity develop recipes;
//!   every frame must land on the same extent *and* keep its markers in the
//!   same place. This is the "none of the other sliders have the same
//!   problem" half of the report, demonstrated rather than asserted.
//! - [`a_viewport_change_then_an_edit_keeps_the_framing`], the axis
//!   `f4af88a` was about (two viewport sizes, then an edit), now that the
//!   develop rail's width genuinely changes the canvas viewport.
//! - [`a_viewport_larger_than_the_source_survives_an_edit`], the same defect
//!   with the extents the other way round, where the crop reads *past* the
//!   source instead of inside it.
//! - [`the_gpu_path_keeps_the_fit_render_framing`], the shell runs on the
//!   GPU; the WGSL kernels dispatch over the output extent the same way.

use std::sync::Arc;

use lightbox_edit::{CurvePoint, GradeWheel, HslBand, Recipe, ToneCurve, Treatment, WhiteBalance};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent,
    NodeId, OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest,
    RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry, SourceError, SourceImage,
    SourceProvider, SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/e10_wb.rs` / `tests/input_transform.rs`) ────────────

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

/// Hands the engine a fixed source at its **own** extent, exactly as
/// `lightbox_core::render_source::PreviewSourceProvider` does, the provider
/// never resizes to the request, because `util.resize` owns the scale ladder.
struct MarkedSource {
    pixels: PixelBuf,
}
impl SourceProvider for MarkedSource {
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
                colorimetry: SourceColorimetry::srgb(),
                full_extent,
                quality: SourceQuality::Preview,
            })
        })
    }
}

// ── the marked source + its framing probe ────────────────────────────────────

/// Where the markers sit, as a fraction of the source's width/height.
const MARKER_AT: f64 = 0.75;

/// The field colour and the marker colour. They are deliberately far apart in
/// **both** luminance and hue so that no single develop stage can collapse the
/// step: a B&W mixer still sees the luma gap, an HSL band still sees the hue
/// gap, and neither end clips under the modest edits the matrix applies.
const FIELD: [u8; 3] = [72, 84, 116];
const MARKER: [u8; 3] = [206, 172, 108];

/// An sRGB8 source carrying one hard vertical edge at `MARKER_AT` of its width
/// and one hard horizontal edge at `MARKER_AT` of its height. Both edges
/// survive a box-decimating fit render as a one-pixel-wide step, so their
/// position in the output frame *is* the render's framing, readable without
/// knowing anything about what the develop stages did to the colours.
fn marked_source(w: u32, h: u32) -> PixelBuf {
    let x_edge = (f64::from(w) * MARKER_AT).round() as u32;
    let y_edge = (f64::from(h) * MARKER_AT).round() as u32;
    let mut bytes = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if x >= x_edge || y >= y_edge {
                MARKER
            } else {
                FIELD
            };
            bytes.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba8Srgb,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

fn texel(px: &PixelBuf, x: u32, y: u32) -> [i32; 3] {
    let o = (y * px.stride + x * 4) as usize;
    [
        i32::from(px.bytes[o]),
        i32::from(px.bytes[o + 1]),
        i32::from(px.bytes[o + 2]),
    ]
}

/// The channel-sum L1 step between two adjacent texels, the quantity the edge
/// locator maximizes. Colour-agnostic on purpose: every develop stage in the
/// matrix moves the two ends of the step somewhere, but none of them moves
/// them on top of each other.
fn step(a: [i32; 3], b: [i32; 3]) -> i32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).sum()
}

/// The strongest horizontal step along row `y`, as `(x_of_step, magnitude)`.
/// `x_of_step` is the index of the first texel *after* the step.
fn strongest_x_step(px: &PixelBuf, y: u32) -> (u32, i32) {
    (1..px.extent.w)
        .map(|x| (x, step(texel(px, x, y), texel(px, x - 1, y))))
        .max_by_key(|&(_, d)| d)
        .expect("a frame at least 2 texels wide")
}

/// The strongest vertical step down column `x`, as `(y_of_step, magnitude)`.
fn strongest_y_step(px: &PixelBuf, x: u32) -> (u32, i32) {
    (1..px.extent.h)
        .map(|y| (y, step(texel(px, x, y), texel(px, x, y - 1))))
        .max_by_key(|&(_, d)| d)
        .expect("a frame at least 2 texels tall")
}

/// The minimum step magnitude that counts as "the marker edge is visible". A
/// frame cropped to the field region has no edge at all and scores ~0 here;
/// the real edge scores well over 100 for every recipe in the matrix.
const MIN_STEP: i32 = 40;

/// Whether `px` frames the whole marked source: right extent, and both marker
/// edges at `MARKER_AT` of the way across the frame. Returns the complaint
/// rather than panicking so the matrix can report **every** failing parameter
/// in one run, which parameters are affected and which are not is the whole
/// diagnostic here, and a fail-fast assert would only ever name the first.
///
/// The ±1 tolerance absorbs the box filter's rounding when the fit ratio does
/// not divide the edge position exactly; `global.clarity`/`texture`/`dehaze`
/// are local-contrast stages that ring around the step, which shifts its
/// *magnitude* but not the texel it lands on.
fn frames_whole_source(px: &PixelBuf, want: Extent) -> Result<(), String> {
    if px.extent != want {
        return Err(format!(
            "frame extent {}×{}, requested {}×{}",
            px.extent.w, px.extent.h, want.w, want.h
        ));
    }

    let want_x = (f64::from(want.w) * MARKER_AT).round() as u32;
    let want_y = (f64::from(want.h) * MARKER_AT).round() as u32;
    // Probe the row/column that crosses exactly one marker edge: a row above
    // the horizontal marker, and a column left of the vertical one.
    let (got_x, mag_x) = strongest_x_step(px, want.h / 3);
    if mag_x < MIN_STEP {
        return Err(format!(
            "no vertical marker edge in the frame (strongest step {mag_x} at \
             x={got_x}) — the render covers a sub-region of the source, not \
             the whole of it"
        ));
    }
    if got_x.abs_diff(want_x) > 1 {
        return Err(format!(
            "vertical marker edge at x={got_x}, expected {want_x} — the render \
             frames the wrong part of the source"
        ));
    }

    let (got_y, mag_y) = strongest_y_step(px, want.w / 3);
    if mag_y < MIN_STEP {
        return Err(format!(
            "no horizontal marker edge in the frame (strongest step {mag_y} at \
             y={got_y}) — the render covers a sub-region of the source, not \
             the whole of it"
        ));
    }
    if got_y.abs_diff(want_y) > 1 {
        return Err(format!(
            "horizontal marker edge at y={got_y}, expected {want_y} — the \
             render frames the wrong part of the source"
        ));
    }
    Ok(())
}

/// [`frames_whole_source`] as a hard assertion, for the single-case tests.
fn assert_frames_whole_source(px: &PixelBuf, want: Extent, case: &str) {
    if let Err(why) = frames_whole_source(px, want) {
        panic!("[{case}] {why}");
    }
}

// ── engine harness ───────────────────────────────────────────────────────────

fn build_engine(pref: BackendPref, dp: Arc<dyn DeviceProvider>, pixels: PixelBuf) -> Engine {
    Engine::with_compiler(
        dp,
        Arc::new(MarkedSource { pixels }),
        lightbox_render::ng::shipping_compiler(),
        EngineConfig {
            backend: pref,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds over the shipping PV1 configuration")
}

/// One shell-shaped render: `roi` and `Fit` scale both carry the viewport, the
/// way `RenderScheduler::to_request` builds them from `ViewState`.
fn render_at(engine: &Engine, vp: Extent, recipe: Recipe, expect: BackendId) -> PixelBuf {
    let req = RenderRequest {
        image: ImageId(1),
        pv: recipe.pv,
        recipe,
        roi: Roi {
            x: 0,
            y: 0,
            w: vp.w,
            h: vp.h,
        },
        scale: RenderScale::Fit(vp),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    };
    let ticket = engine.submit(req);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(out) => {
                assert_eq!(out.backend, expect, "backend provenance");
                let OutputPayload::Pixels(px) = out.payload else {
                    panic!("expected a Pixels payload from a Buffer target");
                };
                return px;
            }
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => {
                assert!(
                    std::time::Instant::now() <= deadline,
                    "render did not complete within 20s"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

// ── the develop-parameter matrix ─────────────────────────────────────────────

fn recipe_with(f: impl FnOnce(&mut lightbox_edit::GlobalStages)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global);
    r
}

/// An S-curve steep enough to add the node but gentle enough to leave both
/// ends of the marker step un-clipped.
fn s_curve() -> ToneCurve {
    ToneCurve {
        points: vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.25, y: 0.18 },
            CurvePoint { x: 0.75, y: 0.82 },
            CurvePoint { x: 1.0, y: 1.0 },
        ],
    }
}

/// One case: a label, the develop node the recipe is expected to splice in,
/// and the recipe itself. The node id is asserted against the real compiler so
/// a case that silently elides its node (and therefore proves nothing) fails
/// loudly rather than passing vacuously.
struct Case {
    label: &'static str,
    node: Option<NodeId>,
    recipe: Recipe,
}

fn matrix() -> Vec<Case> {
    let case = |label, node, recipe| Case {
        label,
        node,
        recipe,
    };
    vec![
        // The control: no develop node at all. Passes before and after the
        // fix, it is what makes the other rows' failures meaningful.
        case("identity", None, Recipe::identity(PV_M0)),
        // The two WB axes. `wb_tint` is the owner's exact report.
        case(
            "wb_temp",
            Some(NodeId("global.wb")),
            recipe_with(|g| {
                g.white_balance = WhiteBalance::Custom {
                    temp_k: 8000.0,
                    tint: 0.0,
                }
            }),
        ),
        case(
            "wb_tint",
            Some(NodeId("global.wb")),
            recipe_with(|g| {
                g.white_balance = WhiteBalance::Custom {
                    temp_k: 6500.0,
                    tint: 40.0,
                }
            }),
        ),
        case(
            "exposure",
            Some(NodeId("global.exposure")),
            recipe_with(|g| g.exposure = 0.6),
        ),
        case(
            "contrast",
            Some(NodeId("global.contrast")),
            recipe_with(|g| g.contrast = 35.0),
        ),
        case(
            "highlights_shadows",
            Some(NodeId("global.tone_recovery")),
            recipe_with(|g| {
                g.highlights = -30.0;
                g.shadows = 30.0;
            }),
        ),
        case(
            "whites_blacks",
            Some(NodeId("global.whites_blacks")),
            recipe_with(|g| {
                g.whites = 20.0;
                g.blacks = -20.0;
            }),
        ),
        case(
            "tone_curve",
            Some(NodeId("global.tone_curve")),
            recipe_with(|g| g.tone_curve.rgb = s_curve()),
        ),
        case(
            "hsl",
            Some(NodeId("global.hsl")),
            recipe_with(|g| {
                g.hsl.bands[5] = HslBand {
                    hue: 15.0,
                    sat: 30.0,
                    lum: -10.0,
                }
            }),
        ),
        case(
            "vibrance_saturation",
            Some(NodeId("global.vibrance_sat")),
            recipe_with(|g| {
                g.vibrance = 40.0;
                g.saturation = 20.0;
            }),
        ),
        case(
            "color_grade",
            Some(NodeId("global.color_grade")),
            recipe_with(|g| {
                g.color_grade.shadows = GradeWheel {
                    hue: 220.0,
                    sat: 30.0,
                    lum: 0.0,
                }
            }),
        ),
        case(
            "bw_mix",
            Some(NodeId("global.bw_mix")),
            recipe_with(|g| g.treatment = Treatment::BlackAndWhite),
        ),
        case(
            "clarity",
            Some(NodeId("global.clarity")),
            recipe_with(|g| g.presence.clarity = 45.0),
        ),
        case(
            "texture",
            Some(NodeId("global.texture")),
            recipe_with(|g| g.presence.texture = 45.0),
        ),
        case(
            "dehaze",
            Some(NodeId("global.dehaze")),
            recipe_with(|g| g.presence.dehaze = 40.0),
        ),
        // The Detail, Effects and Optics panels. These are the nodes this
        // matrix exists to catch, and they were added by three agents in
        // parallel worktrees, none of whom extended it. Detail and Optics
        // are neighbourhood nodes with a non-trivial `roi_in`; Effects sit
        // after geometry and derive their canvas from the crop. All three
        // are exactly where a framing mistake would hide.
        case(
            "sharpen",
            Some(NodeId("global.sharpen")),
            recipe_with(|g| {
                g.detail.sharpen.amount = 80.0;
                g.detail.sharpen.radius = 1.5;
            }),
        ),
        case(
            "noise_reduction",
            Some(NodeId("global.noise_reduction")),
            recipe_with(|g| {
                g.detail.nr.luma = 60.0;
                g.detail.nr.chroma = 60.0;
            }),
        ),
        case(
            "vignette",
            Some(NodeId("fx.vignette")),
            recipe_with(|g| g.effects.postcrop_vignette.amount = -50.0),
        ),
        case(
            "grain",
            Some(NodeId("fx.grain")),
            recipe_with(|g| g.effects.grain.amount = 40.0),
        ),
        case(
            "lens",
            Some(NodeId("geom.lens")),
            recipe_with(|g| {
                g.optics.lens_profile = Some(lightbox_edit::LensCorrection {
                    manual_distortion: -30.0,
                    ..Default::default()
                });
            }),
        ),
        case(
            "defringe",
            Some(NodeId("geom.defringe")),
            recipe_with(|g| g.optics.defringe = 60.0),
        ),
    ]
}

/// Compile `recipe` through the shipping compiler and assert the case's node
/// really is in the graph, a case whose node got elided would frame correctly
/// for the trivial reason that it is the identity render.
fn assert_case_is_live(case: &Case, src_extent: Extent) {
    use lightbox_render::ng::compile::SourceDesc;
    use lightbox_render::ng::SourceKind;

    let graph = lightbox_render::ng::shipping_compiler()
        .compile(
            &case.recipe,
            PV_M0,
            &SourceDesc {
                image: ImageId(1),
                full_extent: src_extent,
                source_kind: SourceKind::Rgb,
                colorimetry: SourceColorimetry::default(),
            },
        )
        .expect("every matrix recipe compiles under PV1");
    match case.node {
        Some(id) => assert!(
            graph.node_index(id).is_some(),
            "[{}] expected {id} in the compiled graph — this case proves \
             nothing if its node is elided",
            case.label
        ),
        None => assert_eq!(
            graph.node_count(),
            3,
            "[identity] expected the bare 3-stage chain, got {} nodes",
            graph.node_count()
        ),
    }
}

// ── the gate ─────────────────────────────────────────────────────────────────

/// **The general gate (the owner's "make sure none of the other sliders have
/// the same problem").** One source, one viewport, sixteen recipes: an
/// identity control plus every develop stage the PV1 chain can splice in. All
/// of them must return a frame that covers the *whole* source.
///
/// Before the fix, the three `global.wb` rows (`wb_temp`, `wb_tint`) failed
/// with "no vertical marker edge in the frame", the frame held only the
/// top-left 96×72 source texels, which are entirely inside the field region.
/// Every other row passed, because every other develop node is spliced after
/// `util.resize`.
#[test]
fn every_develop_param_keeps_the_fit_render_framing() {
    // Source deliberately much larger than the viewport, and not an integer
    // multiple of it, so the fit ratio exercises the box filter's rounding.
    let src = Extent { w: 320, h: 240 };
    let vp = Extent { w: 96, h: 72 };

    let mut failed = Vec::new();
    for case in matrix() {
        assert_case_is_live(&case, src);
        let engine = build_engine(
            BackendPref::ForceCpu,
            Arc::new(NullDevice),
            marked_source(src.w, src.h),
        );
        let px = render_at(&engine, vp, case.recipe.clone(), BackendId::Cpu);
        if let Err(why) = frames_whole_source(&px, vp) {
            failed.push(format!("  {}: {why}", case.label));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of the develop parameters lost the render's framing:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// **The viewport-change axis `f4af88a` was about, now with an edit on the
/// end.** The develop rail's width change (`5c40d22`) makes a viewport-only
/// resubmit a live path again: the shell re-establishes the view whenever its
/// canvas pixel size moves, then the user's first slider drag lands on top of
/// whatever the previous viewport left in the cache.
#[test]
fn a_viewport_change_then_an_edit_keeps_the_framing() {
    let src = Extent { w: 320, h: 240 };
    let wide = Extent { w: 160, h: 120 };
    let narrow = Extent { w: 96, h: 72 };
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        marked_source(src.w, src.h),
    );

    // Rail closed: a wide canvas, identity recipe.
    let a = render_at(&engine, wide, Recipe::identity(PV_M0), BackendId::Cpu);
    assert_frames_whole_source(&a, wide, "wide/identity");

    // Rail opens: the same recipe at a narrower canvas.
    let b = render_at(&engine, narrow, Recipe::identity(PV_M0), BackendId::Cpu);
    assert_frames_whole_source(&b, narrow, "narrow/identity");

    // The user's first drag of Tint, at the narrow viewport.
    let tinted = recipe_with(|g| {
        g.white_balance = WhiteBalance::Custom {
            temp_k: 6500.0,
            tint: 40.0,
        }
    });
    let c = render_at(&engine, narrow, tinted.clone(), BackendId::Cpu);
    assert_frames_whole_source(&c, narrow, "narrow/tint");

    // …and back out to the wide canvas with the edit still applied.
    let d = render_at(&engine, wide, tinted, BackendId::Cpu);
    assert_frames_whole_source(&d, wide, "wide/tint");
}

/// The same defect with the extents the other way round: a canvas larger than
/// the source. Here the cropping read runs *past* the source instead of inside
/// it, `PixelBuf::get_rgba_f32` is unchecked, so the CPU path indexed out of
/// its own buffer and the render came back
/// `Failed(Internal("a render node panicked during evaluation"))`.
#[test]
fn a_viewport_larger_than_the_source_survives_an_edit() {
    let src = Extent { w: 64, h: 48 };
    let vp = Extent { w: 200, h: 150 };
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        marked_source(src.w, src.h),
    );
    let tinted = recipe_with(|g| {
        g.white_balance = WhiteBalance::Custom {
            temp_k: 6500.0,
            tint: 40.0,
        }
    });
    let px = render_at(&engine, vp, tinted, BackendId::Cpu);
    assert_frames_whole_source(&px, vp, "upscaled/tint");
}

/// The shell renders on the GPU, where the same crop is spelled
/// `textureLoad(src, vec2<i32>(gid.xy), 0)` over the *output* extent. Skips
/// (rather than fails) where no adapter exists; the authoritative run is on
/// the developer machine and CI's GPU lane.
#[test]
fn the_gpu_path_keeps_the_fit_render_framing() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!(
            "[the_gpu_path_keeps_the_fit_render_framing] no wgpu adapter \
             available — SKIPPED"
        );
        return;
    };
    let src = Extent { w: 320, h: 240 };
    let vp = Extent { w: 96, h: 72 };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });

    let mut failed = Vec::new();
    for case in matrix() {
        let engine = build_engine(
            BackendPref::Auto,
            Arc::clone(&dp),
            marked_source(src.w, src.h),
        );
        let px = render_at(&engine, vp, case.recipe.clone(), BackendId::Gpu);
        if let Err(why) = frames_whole_source(&px, vp) {
            failed.push(format!("  {}: {why}", case.label));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of the develop parameters lost the render's framing on the GPU:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
