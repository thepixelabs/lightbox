// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The optics slice of the geometry segment (`geom.defringe`, `geom.lens`)
//! integration gate. Mirrors `tests/e11_geom.rs`'s harness exactly (same
//! `shipping_compiler`/`SharedDevice`/`NullDevice`/`SynthSource` seams), so
//! the geometry and optics nodes speak one comparator. Covers:
//!
//! - structural: an identity `Optics` elides both optics nodes; a manual
//!   distortion amount adds exactly `geom.lens`; a defringe amount adds
//!   exactly `geom.defringe`; the `ca` toggle on its own adds NOTHING (it
//!   has no measured coefficients to apply, see
//!   `nodes::geometry::lens::resolve_lens_profile`); and with all four
//!   stages live the chain is defringe, lens, warp, crop in that order;
//! - the end-to-end contract through the real `Engine::submit` path:
//!   distortion correction moves content radially without changing the
//!   output extent, and vignetting correction brightens the corners while
//!   leaving the frame centre alone;
//! - committed CPU goldens plus CPU/GPU parity on an optics corpus. Both
//!   matter and they catch different things: parity says the two backends
//!   agree with each other, the goldens say the output has not moved since
//!   it was blessed. `pv-manifests/pv1.json` pins only the three template
//!   stages, so for a dynamically spliced node the golden is the only
//!   guard rail there is.
//!
//! GPU legs `SKIP` (report, never fake a number) when no adapter is
//! available.

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::leaves::{Crop, LensCorrection, Optics};
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

// ── seams (mirrors tests/e11_geom.rs) ────────────────────────────────────────

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
            eprintln!("[{name}] no wgpu adapter available, SKIPPED");
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

fn render_px(engine: &Engine, w: u32, h: u32, recipe: Recipe, expect: BackendId) -> PixelBuf {
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

fn rgba_at(px: &PixelBuf, x: u32, y: u32) -> [u8; 4] {
    let i = (y * px.stride + x * 4) as usize;
    [
        px.bytes[i],
        px.bytes[i + 1],
        px.bytes[i + 2],
        px.bytes[i + 3],
    ]
}

fn recipe_with_optics(f: impl FnOnce(&mut Optics)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global.optics);
    r
}

/// A manual distortion recipe on the schema's own signed `-100..=100`
/// scale. Positive removes barrel distortion.
fn recipe_with_distortion(amount: f32) -> Recipe {
    recipe_with_optics(|o| {
        o.lens_profile = Some(LensCorrection {
            manual_distortion: amount,
            ..LensCorrection::default()
        });
    })
}

fn source_desc(w: u32, h: u32) -> SourceDesc {
    SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w, h },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    }
}

fn flat_grey(w: u32, h: u32, level: f32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    for y in 0..h {
        for x in 0..w {
            px.set_rgba_f32(x, y, [level, level, level, 1.0]);
        }
    }
    px
}

// ── structural: identity elision + exact node placement ─────────────────────

#[test]
fn identity_optics_elides_both_optics_nodes() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc(32, 32))
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("geom.lens")).is_none());
    assert!(graph.node_index(NodeId("geom.defringe")).is_none());
    assert_eq!(
        graph.node_count(),
        3,
        "only the 3 engine-owned stages: src.decoded, util.resize, xform.display"
    );
}

/// Enabling a lens correction at its default is not a correction: the node
/// must still elide, so ticking the panel's checkbox costs nothing.
#[test]
fn a_default_lens_correction_still_elides() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with_optics(|o| o.lens_profile = Some(LensCorrection::default()));
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 3);
}

/// **The compile-level half of why the schema grew a field.** The two
/// profile amounts are amounts. With no profile to scale they splice
/// nothing, at any value. Only the manual dial corrects.
#[test]
fn profile_amounts_alone_splice_no_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    for amount in [0.0f32, 50.0, 200.0] {
        let recipe = recipe_with_optics(|o| {
            o.lens_profile = Some(LensCorrection {
                profile_id: "Canon RF 15-35mm F2.8 L IS USM".to_owned(),
                distortion: amount,
                vignetting: amount,
                manual_distortion: 0.0,
            });
        });
        let graph = compiler
            .compile(&recipe, PV_M0, &source_desc(32, 32))
            .expect("recipe compiles");
        assert_eq!(
            graph.node_count(),
            3,
            "profile amount {amount} has no profile to scale"
        );
    }
}

#[test]
fn manual_distortion_adds_exactly_geom_lens() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let graph = compiler
        .compile(&recipe_with_distortion(60.0), PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + geom.lens");
    assert!(graph.node_index(NodeId("geom.lens")).is_some());
    assert!(graph.node_index(NodeId("geom.defringe")).is_none());
}

#[test]
fn manual_vignetting_adds_exactly_geom_lens() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with_optics(|o| o.vignette_corr = 40.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4);
    assert!(graph.node_index(NodeId("geom.lens")).is_some());
}

#[test]
fn defringe_adds_exactly_geom_defringe() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let recipe = recipe_with_optics(|o| o.defringe = 50.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(32, 32))
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + geom.defringe");
    assert!(graph.node_index(NodeId("geom.defringe")).is_some());
    assert!(graph.node_index(NodeId("geom.lens")).is_none());
}

/// The honest state of the CA half: there is no lens-profile database, so
/// the toggle has no coefficients and adds no node. This test is the
/// tripwire against a future change that quietly makes the toggle "work" by
/// inventing scale factors.
#[test]
fn the_ca_toggle_alone_adds_no_node_because_there_are_no_coefficients() {
    let compiler = lightbox_render::ng::shipping_compiler();
    for recipe in [
        recipe_with_optics(|o| o.ca = true),
        recipe_with_optics(|o| {
            o.ca = true;
            o.lens_profile = Some(LensCorrection {
                profile_id: "Nikon Z 14-24mm f/2.8 S".to_owned(),
                ..LensCorrection::default()
            });
        }),
    ] {
        let graph = compiler
            .compile(&recipe, PV_M0, &source_desc(32, 32))
            .expect("recipe compiles");
        assert_eq!(
            graph.node_count(),
            3,
            "no measured CA scales exist, so nothing is spliced"
        );
    }
}

/// **The ordering contract**: defringe, then lens, then warp, then crop.
/// Every link is checked, not just the endpoints.
#[test]
fn optics_runs_before_warp_and_crop_in_that_exact_order() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let mut recipe = recipe_with_optics(|o| {
        o.defringe = 40.0;
        o.vignette_corr = 30.0;
        o.lens_profile = Some(LensCorrection {
            manual_distortion: 50.0,
            ..LensCorrection::default()
        });
    });
    recipe.geometry.angle = 5.0;
    recipe.geometry.crop = Crop {
        left: 0.1,
        top: 0.1,
        right: 0.9,
        bottom: 0.9,
    };
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc(64, 48))
        .expect("recipe compiles");
    assert_eq!(
        graph.node_count(),
        7,
        "3 engine stages + defringe + lens + warp + crop"
    );
    let idx = |id: &'static str| {
        graph
            .node_index(NodeId(id))
            .unwrap_or_else(|| panic!("{id}"))
    };
    let defringe = idx("geom.defringe");
    let lens = idx("geom.lens");
    let warp = idx("geom.warp");
    let crop = idx("geom.crop");
    assert_eq!(graph.inputs_of(lens), vec![defringe]);
    assert_eq!(graph.inputs_of(warp), vec![lens]);
    assert_eq!(graph.inputs_of(crop), vec![warp]);
}

// ── end-to-end contract, through Engine::submit ─────────────────────────────

/// **The distortion contract**: correcting barrel distortion pushes content
/// outward, so a destination pixel resamples from a smaller source radius,
/// and the output extent does not change (that is `geom.crop`'s job, not
/// this node's).
///
/// The gradient corpus makes this directly measurable: its red channel is
/// `x / w` and its green channel is `y / h`, so a pixel's colour reports
/// where in the source frame it came from. The destination top-left corner
/// must therefore report a position further INTO the frame after the
/// correction than before it.
#[test]
fn distortion_correction_resamples_radially_without_changing_extent() {
    let (w, h) = (64u32, 48u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let base = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let barrel = render_px(&engine, w, h, recipe_with_distortion(100.0), BackendId::Cpu);

    assert_eq!(
        base.extent, barrel.extent,
        "distortion correction must not change the canvas extent"
    );

    let base_corner = rgba_at(&base, 0, 0);
    let barrel_corner = rgba_at(&barrel, 0, 0);
    assert!(
        barrel_corner[0] > base_corner[0] + 8 && barrel_corner[1] > base_corner[1] + 8,
        "correcting barrel must pull the corner's content in from further \
         inside the frame: {base_corner:?} -> {barrel_corner:?}"
    );

    // The frame centre sits at normalized radius zero, so a purely radial
    // correction must leave it alone.
    let (cx, cy) = (w / 2, h / 2);
    let base_centre = rgba_at(&base, cx, cy);
    let barrel_centre = rgba_at(&barrel, cx, cy);
    for c in 0..3 {
        assert!(
            base_centre[c].abs_diff(barrel_centre[c]) <= 2,
            "the frame centre must be untouched: {base_centre:?} -> {barrel_centre:?}"
        );
    }

    // The opposite sign must move the corner the opposite way (here into the
    // edge-clamped region outside the source, so it cannot move inward).
    let pincushion = render_px(
        &engine,
        w,
        h,
        recipe_with_distortion(-100.0),
        BackendId::Cpu,
    );
    let pin_corner = rgba_at(&pincushion, 0, 0);
    assert!(
        pin_corner[0] <= base_corner[0] + 2,
        "pincushion correction must not pull content inward: \
         {base_corner:?} -> {pin_corner:?}"
    );
}

/// **The vignetting contract**: a positive manual amount brightens the
/// corners of a flat field and leaves the centre exactly where it was.
#[test]
fn vignetting_correction_brightens_corners_and_spares_the_centre() {
    let (w, h) = (64u32, 48u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_grey(w, h, 0.18),
    );

    let base = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let brighter = render_px(
        &engine,
        w,
        h,
        recipe_with_optics(|o| o.vignette_corr = 100.0),
        BackendId::Cpu,
    );
    let darker = render_px(
        &engine,
        w,
        h,
        recipe_with_optics(|o| o.vignette_corr = -100.0),
        BackendId::Cpu,
    );

    let (cx, cy) = (w / 2, h / 2);
    for c in 0..3 {
        assert!(
            rgba_at(&base, cx, cy)[c].abs_diff(rgba_at(&brighter, cx, cy)[c]) <= 1,
            "the centre gain is exactly 1"
        );
    }
    for (x, y) in [(0u32, 0u32), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        let b = rgba_at(&base, x, y)[0];
        let up = rgba_at(&brighter, x, y)[0];
        let down = rgba_at(&darker, x, y)[0];
        assert!(up > b + 20, "corner ({x},{y}) must brighten: {b} -> {up}");
        assert!(down < b - 20, "corner ({x},{y}) must darken: {b} -> {down}");
    }
}

/// The defringe half end to end: a flat field has no edges, so a full
/// defringe pass through the real engine must be a no-op on it. The
/// fringe-removal behaviour itself is proven at node level in
/// `nodes::geometry::defringe`'s own tests, where the synthetic fringe can
/// be placed pixel-exactly.
#[test]
fn defringe_leaves_an_edgeless_frame_alone_end_to_end() {
    let (w, h) = (32u32, 32u32);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        flat_grey(w, h, 0.30),
    );
    let base = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let defringed = render_px(
        &engine,
        w,
        h,
        recipe_with_optics(|o| o.defringe = 100.0),
        BackendId::Cpu,
    );
    assert_eq!(base.extent, defringed.extent);
    assert_eq!(
        texels(&base),
        texels(&defringed),
        "an edgeless frame has nothing to defringe"
    );
}

// ── goldens + CPU/GPU parity ────────────────────────────────────────────────
//
// Parity proves the two backends agree with each other, which is NOT the same
// as proving the output has not changed. `pv-manifests/pv1.json` pins only the
// three template stages and deliberately excludes dynamically spliced nodes,
// so for `geom.lens` and `geom.defringe` these goldens are the only guard rail
// that fires when a coefficient, a kernel, or a placement silently moves.
// Regenerate deliberately with `LIGHTBOX_BLESS=1`, never to make red go away.

fn optics_goldens_root() -> PathBuf {
    goldens_root().join("global").join("optics")
}

/// Every golden case, by name. One list so the per-case gates below and
/// [`every_golden_case_differs_from_the_identity_render`] can never drift
/// apart about what is being pinned.
const GOLDEN_CASES: [&str; 4] = [
    "distortion_only",
    "vignetting_only",
    "defringe_only",
    "all_optics",
];

/// A 32x32 frame with a hard dark-to-bright edge down the middle and a two
/// pixel colour fringe sitting on the bright side of it: purple on the top
/// half, green on the bottom, so one golden pins both signs of the
/// opponent axis.
///
/// The shipped corpus kinds cannot stand in here. `Checker`'s two colours
/// are within 12% of each other in luminance, which is below the defringe
/// edge gate, and `HighFrequency` is neutral grey, so `cd` is zero
/// everywhere. Defringe correctly does nothing on either, which would make
/// its golden a picture of an unedited frame. That is precisely what
/// `every_golden_case_differs_from_the_identity_render` exists to catch,
/// and it did.
fn fringe_source(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let edge = w / 2;
    for y in 0..h {
        let purple = y < h / 2;
        for x in 0..w {
            let p = if x < edge {
                [0.02, 0.02, 0.02, 1.0]
            } else if x < edge + 2 {
                if purple {
                    [0.90, 0.35, 0.90, 1.0]
                } else {
                    [0.50, 0.95, 0.50, 1.0]
                }
            } else {
                [0.90, 0.90, 0.90, 1.0]
            };
            px.set_rgba_f32(x, y, p);
        }
    }
    px
}

/// The source and recipe for one golden case.
fn golden_case(name: &str, w: u32, h: u32) -> (PixelBuf, Recipe) {
    match name {
        // Strong spatial structure, so a radial resample is visible
        // everywhere rather than only near an edge.
        "distortion_only" => (
            synth_source(CorpusKind::Checker, w, h),
            recipe_with_distortion(70.0),
        ),
        // A smooth field, so a radial gain shows up as a gain and not as
        // an interaction with local detail.
        "vignetting_only" => (
            synth_source(CorpusKind::Gradient, w, h),
            recipe_with_optics(|o| o.vignette_corr = -80.0),
        ),
        "defringe_only" => (
            fringe_source(w, h),
            recipe_with_optics(|o| o.defringe = 100.0),
        ),
        // All three at once over the fringe frame, which is the only
        // source here that exercises every half of the segment.
        "all_optics" => (
            fringe_source(w, h),
            recipe_with_optics(|o| {
                o.defringe = 60.0;
                o.vignette_corr = 45.0;
                o.lens_profile = Some(LensCorrection {
                    manual_distortion: -60.0,
                    ..LensCorrection::default()
                });
            }),
        ),
        other => panic!("unknown golden case {other}"),
    }
}

/// CPU render vs its committed golden, then GPU vs that same CPU render.
/// Mirrors `tests/e11_geom.rs::geom_case` so both node families' gates
/// speak one comparator.
fn optics_case(name: &str) {
    let (w, h) = (32u32, 32u32);
    let (pixels, recipe) = golden_case(name, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let cpu_out = render_px(&cpu_engine, w, h, recipe.clone(), BackendId::Cpu);

    let golden_path = optics_goldens_root()
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report =
        compare_srgb8_to_golden(&golden_path, cpu_out.extent.w, cpu_out.extent.h, &rendered);
    println!(
        "[optics][{name}][cpu-vs-golden] extent={}x{} \u{394}E2000 max={:.4} mean={:.4} \
         PSNR={:.2}dB",
        cpu_out.extent.w, cpu_out.extent.h, report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(gpu) = device_or_skip(&format!("optics-{name}")) else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&gpu.device),
        queue: Arc::clone(&gpu.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render_px(&gpu_engine, w, h, recipe, BackendId::Gpu);

    assert_eq!(cpu_out.extent, gpu_out.extent, "[{name}] extents differ");
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[optics][{name}][gpu-vs-cpu parity] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn distortion_golden_and_parity() {
    optics_case("distortion_only");
}

#[test]
fn vignetting_golden_and_parity() {
    optics_case("vignetting_only");
}

#[test]
fn defringe_golden_and_parity() {
    optics_case("defringe_only");
}

#[test]
fn all_optics_together_golden_and_parity() {
    optics_case("all_optics");
}

/// What the `defringe_only` golden actually depicts, asserted rather than
/// left to whoever opens the PNG: both fringes lose most of their cast and
/// the luminance edge itself does not move. A byte-for-byte golden pins
/// that a render has not changed; this pins that what it froze was a
/// correction.
#[test]
fn the_defringe_golden_depicts_a_fringe_being_removed() {
    let (w, h) = (32u32, 32u32);
    let (pixels, recipe) = golden_case("defringe_only", w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
    let base = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let fixed = render_px(&engine, w, h, recipe, BackendId::Cpu);

    // The fringe occupies columns 16 and 17: purple on row 8, green on
    // row 24 (see `fringe_source`). Cast is measured on the green channel,
    // which is the one the opponent axis moves.
    for (row, label) in [(8u32, "purple"), (24, "green")] {
        let before = rgba_at(&base, 16, row);
        let after = rgba_at(&fixed, 16, row);
        let cast_before = before[1] as i32 - before[0] as i32;
        let cast_after = after[1] as i32 - after[0] as i32;
        assert!(
            cast_after.abs() * 2 < cast_before.abs(),
            "[{label}] fringe must lose most of its cast: {cast_before} -> {cast_after}"
        );
    }

    // Well clear of the fringe on both sides, the edge is untouched: this
    // control moves colour, never pixels.
    for x in [4u32, 28] {
        assert_eq!(
            rgba_at(&base, x, 8),
            rgba_at(&fixed, x, 8),
            "column {x} is not a fringe and must be identical"
        );
    }
}

/// A golden only guards what it can see change. This proves every case in
/// [`GOLDEN_CASES`] renders something the identity recipe does not, so no
/// golden is silently pinning an unedited frame and quietly passing
/// forever.
#[test]
fn every_golden_case_differs_from_the_identity_render() {
    let (w, h) = (32u32, 32u32);
    for name in GOLDEN_CASES {
        let (pixels, recipe) = golden_case(name, w, h);
        let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);
        let base = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
        let edited = render_px(&engine, w, h, recipe, BackendId::Cpu);
        assert_ne!(
            texels(&base),
            texels(&edited),
            "[{name}] the golden would pin an unedited frame"
        );
    }
}

/// The node ids are a stable public surface (cache keys and the
/// `invalidates` hint both name them), so a rename is a deliberate act.
#[test]
fn optics_node_ids_are_the_stable_names() {
    use lightbox_render::ng::nodes::geometry::{GeomDefringeNode, GeomLensNode};
    assert_eq!(GeomLensNode::ID, NodeId("geom.lens"));
    assert_eq!(GeomDefringeNode::ID, NodeId("geom.defringe"));
}
