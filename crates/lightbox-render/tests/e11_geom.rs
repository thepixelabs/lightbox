// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E11 Phase-E geometry engine slice (tasks **E1-E4**) integration gate.
//!
//! Mirrors `tests/e10_tone_recovery.rs`/`tests/e10_global_basic.rs`'s harness
//! exactly (same `shipping_compiler`/`SharedDevice`/`NullDevice`/`SynthSource`
//! seams) so E10 and E11's node goldens speak one comparator. Covers:
//!
//! - structural: an identity `Geometry` elides both `geom.warp`/`geom.crop`;
//!   a touched `angle` adds exactly `geom.warp`; a touched crop/flip adds
//!   exactly `geom.crop`; both touched wires `geom.warp` before `geom.crop`
//!   (spec §5 pipeline order), the E1/E4 identity-elision-trace ACs;
//! - **the end-to-end contract**, through the real `Engine::submit` path (not
//!   just node-level math): a crop changes the rendered output's extent and
//!   selects the right pixel region (task E4); a straighten angle visibly
//!   rotates content while leaving the extent unchanged (task E1); a flip
//!   mirrors content (task E4's "orientation" fast path);
//! - goldens + CPU/GPU parity (task E2) on a small warp corpus;
//! - a 90°-turn node-level check (task E1's "4×90° composes to identity" AC,
//!   at the resample level, the recipe's own `angle` field is range-clamped
//!   to ±45°, so a full 90° turn isn't reachable through `Engine::submit`
//!   today; see `docs/plan/epics/E11-deviations.md`);
//! - ROI back-mapping through the warp integrated with the engine's
//!   reference tiled-CPU-path harness (task E3), mirroring
//!   `ToneRecoveryNode`'s own B13 pattern (the production v1 executor
//!   doesn't yet route through real ROI-tiled CPU eval either, see that
//!   node's `plan` doc comment).
//!
//! GPU legs `SKIP` (report, never fake a number) when no adapter is
//! available, the authoritative run is the single-process merge on `main`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lightbox_edit::leaves::{Crop, Flip};
use lightbox_edit::{Geometry, Recipe};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::exec::tiling::{TileOrder, TileProbe, TileRender};
use lightbox_render::ng::graph::RenderGraph;
use lightbox_render::ng::node::param::{ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::nodes::geometry::{GeomCropNode, GeomWarpNode};
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, CpuEvalCtx, CpuTileView, DeviceError, DeviceProvider,
    Engine, EngineConfig, Extent, GpuEvalCtx, NodeDescriptor, NodeError, NodeId, OutFormat,
    OutputPayload, ParamBlock, ParamValue, PixelBuf, PixelFormat, PortDecl, PortType, RenderNode,
    RenderPriority, RenderRequest, RenderScale, RenderState, RenderTarget, Roi, SourceColorimetry,
    SourceDesc, SourceError, SourceImage, SourceKind, SourceProvider, SourceQuality, SourceWant,
    TileView,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_render_testkit::corpus::{
    compare_srgb8_to_golden, goldens_root, synth_source, CorpusKind, CorpusSourceProbe,
};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors tests/e10_tone_recovery.rs) ────────────────────────────────

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

/// Renders `recipe` over an engine seeded with a `w x h` source, requesting a
/// `w x h` ROI (the source's own full extent, the convention `SourceDesc`'s
/// "nominal" compile-time extent, `req.roi`, must match for geometry's baked
/// `src_w`/`src_h` params to describe the real image; see
/// `docs/plan/epics/E11-deviations.md`). Returns the full output `PixelBuf`
/// (not just texels) so callers can inspect its **own** extent, the
/// crop/rotate proof this file's E4 test needs.
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

fn recipe_with_geom(f: impl FnOnce(&mut Geometry)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.geometry);
    r
}

fn e11_goldens_root() -> PathBuf {
    goldens_root().join("global").join("geometry")
}

// ── structural: identity elision + exact node placement (E1/E4 ACs) ──────────

#[test]
fn identity_geometry_elides_every_geometry_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc)
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("geom.warp")).is_none());
    assert!(graph.node_index(NodeId("geom.crop")).is_none());
    assert_eq!(
        graph.node_count(),
        3,
        "only the 3 engine-owned stages: src.decoded, util.resize, xform.display"
    );
}

#[test]
fn touched_angle_adds_exactly_geom_warp() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with_geom(|g| g.angle = 12.0);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + geom.warp");
    assert!(graph.node_index(NodeId("geom.warp")).is_some());
    assert!(graph.node_index(NodeId("geom.crop")).is_none());
}

#[test]
fn touched_crop_adds_exactly_geom_crop() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with_geom(|g| {
        g.crop = Crop {
            left: 0.1,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4, "3 engine stages + geom.crop");
    assert!(graph.node_index(NodeId("geom.warp")).is_none());
    assert!(graph.node_index(NodeId("geom.crop")).is_some());
}

/// **Task E4 AC / spec §5 pipeline order**: when both are touched,
/// `geom.warp` sits before `geom.crop` in the compiled chain.
#[test]
fn angle_and_crop_together_wire_warp_before_crop() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with_geom(|g| {
        g.angle = 5.0;
        g.crop = Crop {
            left: 0.1,
            top: 0.1,
            right: 0.9,
            bottom: 0.9,
        };
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert_eq!(
        graph.node_count(),
        5,
        "3 engine stages + geom.warp + geom.crop"
    );
    let warp_idx = graph
        .node_index(NodeId("geom.warp"))
        .expect("geom.warp present");
    let crop_idx = graph
        .node_index(NodeId("geom.crop"))
        .expect("geom.crop present");
    // geom.crop's connected input must be geom.warp (not some other stage).
    assert_eq!(graph.inputs_of(crop_idx), vec![warp_idx]);
}

#[test]
fn flip_alone_also_adds_exactly_geom_crop() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 32, h: 32 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with_geom(|g| g.flip = Flip::Horizontal);
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert_eq!(graph.node_count(), 4);
    assert!(graph.node_index(NodeId("geom.crop")).is_some());
}

// ── end-to-end contract, through Engine::submit ───────────────────────────────

/// **Task E4's headline contract**: cropping an un-warped image is a pure
/// canvas/extent change that selects the right pixel region, proven through
/// the real `Engine::submit` path (input extent → output extent; content
/// matches the requested sub-rectangle of the source).
#[test]
fn crop_changes_output_extent_and_selects_the_right_region() {
    let (w, h) = (64u32, 48u32);
    // Gradient: pixel (x,y) -> roughly [x/w, y/h, 0.5, 1.0] in scene-linear
    // working RGB, so the R/G channels double as position markers.
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    // The un-cropped render, for a self-consistent comparison: `out`'s pixels
    // are post-`xform.display` (sRGB8), so comparing against a hand-computed
    // scene-linear expectation would compare across domains, comparing
    // against the identity render's own corresponding pixel sidesteps that
    // entirely (mirrors `flip_horizontal_mirrors_content`'s methodology).
    let baseline = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);

    // Crop to the middle half horizontally, full height.
    let recipe = recipe_with_geom(|g| {
        g.crop = Crop {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 1.0,
        };
    });
    let out = render_px(&engine, w, h, recipe, BackendId::Cpu);

    println!(
        "[E4] crop extent proof: input {w}x{h} -> output {}x{} (expect {}x{})",
        out.extent.w,
        out.extent.h,
        w / 2,
        h
    );
    assert_eq!(out.extent, Extent { w: w / 2, h });

    // Content check: the cropped output's left edge must match the source's
    // x=16 column (left_px = round(0.25*64) = 16), not the source's x=0.
    let out_left = out.get_rgba_f32(0, h / 2);
    let want = baseline.get_rgba_f32(16, h / 2);
    for c in 0..4 {
        assert!(
            (out_left[c] - want[c]).abs() < 1.0 / 200.0,
            "cropped left edge should read source column 16 ch{c}: got {out_left:?} want {want:?}"
        );
    }
    // And it must NOT match the source's own x=0 column (proving the crop
    // really did shift the window, not just resize it), compare the whole
    // pixel (any channel differing is sufficient; a particular channel can
    // legitimately saturate to the same value at both columns after the
    // working->display gamut/tone transform).
    let unshifted = baseline.get_rgba_f32(0, h / 2);
    assert!(
        (0..4).any(|c| (out_left[c] - unshifted[c]).abs() > 1.0 / 50.0),
        "cropped left edge should differ from the un-cropped source's x=0 column: \
         out_left={out_left:?} unshifted={unshifted:?}"
    );
}

/// **Task E1's headline contract**: a straighten angle visibly rotates
/// content while the canvas extent is unchanged (this slice's warp never
/// changes extent, `geom.crop` is the separate extent change), through the
/// real `Engine::submit` path.
#[test]
fn straighten_angle_visibly_rotates_content_extent_unchanged() {
    let (w, h) = (48u32, 48u32);
    let pixels = synth_source(CorpusKind::Checker, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let rotated = render_px(
        &engine,
        w,
        h,
        recipe_with_geom(|g| g.angle = 20.0),
        BackendId::Cpu,
    );

    assert_eq!(
        baseline.extent, rotated.extent,
        "warp alone never changes extent"
    );

    let mut changed = 0u32;
    for y in 0..h {
        for x in 0..w {
            if baseline.get_rgba_f32(x, y) != rotated.get_rgba_f32(x, y) {
                changed += 1;
            }
        }
    }
    let frac = changed as f64 / (w * h) as f64;
    println!(
        "[E1/e2e] angle=20deg: {changed} / {} pixels changed ({:.1}%)",
        w * h,
        frac * 100.0
    );
    assert!(
        frac > 0.3,
        "a 20 degree straighten on a checker pattern must visibly change a large fraction of pixels; got {frac:.3}"
    );
}

/// **Task E4's orientation fast path**: a horizontal flip mirrors content
/// through the real `Engine::submit` path, compared against a manually
/// mirrored reference of the identity render (independent verification, not
/// just "did it change").
#[test]
fn flip_horizontal_mirrors_content() {
    let (w, h) = (32u32, 24u32);
    let pixels = synth_source(CorpusKind::Gradient, w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render_px(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let flipped = render_px(
        &engine,
        w,
        h,
        recipe_with_geom(|g| g.flip = Flip::Horizontal),
        BackendId::Cpu,
    );

    assert_eq!(baseline.extent, flipped.extent);
    for y in [0u32, h / 2, h - 1] {
        for x in [0u32, w / 4, w - 1] {
            let base_px = baseline.get_rgba_f32(x, y);
            let flip_px = flipped.get_rgba_f32(w - 1 - x, y);
            for c in 0..4 {
                assert!(
                    (base_px[c] - flip_px[c]).abs() < 1.0 / 200.0,
                    "flip mismatch at ({x},{y}) ch{c}: base={base_px:?} flip(mirrored)={flip_px:?}"
                );
            }
        }
    }
}

// ── goldens + CPU/GPU parity (task E2, a small warp corpus) ──────────────────

fn geom_case(angle: f32, crop: Crop, flip: Flip, name: &str, kind: CorpusKind) {
    let (w, h) = (32u32, 32u32);
    let pixels = synth_source(kind, w, h);

    let cpu_engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels.clone());
    let recipe = recipe_with_geom(|g| {
        g.angle = angle;
        g.crop = crop;
        g.flip = flip;
    });
    let cpu_out = render_px(&cpu_engine, w, h, recipe.clone(), BackendId::Cpu);
    let golden_path = e11_goldens_root()
        .join("pv1")
        .join(format!("{name}_cpu.png"));
    let rendered = texels(&cpu_out);
    let report =
        compare_srgb8_to_golden(&golden_path, cpu_out.extent.w, cpu_out.extent.h, &rendered);
    println!(
        "[E1-E4][{name}][cpu-vs-golden] extent={}x{} \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        cpu_out.extent.w, cpu_out.extent.h, report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[{name}][cpu] \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    let Some(ctx) = device_or_skip(&format!("geom-{name}")) else {
        return;
    };
    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice {
        device: Arc::clone(&ctx.device),
        queue: Arc::clone(&ctx.queue),
    });
    let gpu_engine = build_engine(BackendPref::Auto, dp, pixels);
    let gpu_out = render_px(&gpu_engine, w, h, recipe, BackendId::Gpu);
    assert_eq!(
        cpu_out.extent, gpu_out.extent,
        "[{name}] CPU/GPU extent must match"
    );
    let stats = delta_e_stats(&texels(&cpu_out), &texels(&gpu_out));
    let psnr_db = psnr(&cpu_out.bytes, &gpu_out.bytes);
    println!(
        "[E2][{name}][gpu-vs-cpu parity] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        stats.max, stats.mean, psnr_db
    );
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "[{name}] GPU/CPU parity: \u{394}E2000 max {:.4} / PSNR {psnr_db:.2} dB",
        stats.max
    );
}

#[test]
fn angle_only_golden_and_parity() {
    geom_case(
        12.0,
        Crop::default(),
        Flip::None,
        "angle_only",
        CorpusKind::Checker,
    );
}

#[test]
fn crop_only_golden_and_parity() {
    geom_case(
        0.0,
        Crop {
            left: 0.2,
            top: 0.1,
            right: 0.9,
            bottom: 0.8,
        },
        Flip::None,
        "crop_only",
        CorpusKind::Gradient,
    );
}

#[test]
fn angle_and_crop_and_flip_golden_and_parity() {
    geom_case(
        -8.0,
        Crop {
            left: 0.1,
            top: 0.1,
            right: 0.9,
            bottom: 0.9,
        },
        Flip::Both,
        "angle_crop_flip",
        CorpusKind::HighFrequency,
    );
}

// ── task E1's "4x90 composes to identity" AC, at the resample level ─────────

/// The recipe's `angle` field is range-clamped to ±45° (the existing,
/// not-to-be-redefined recipe model, see `docs/plan/epics/E11-deviations.md`),
/// so a full 90° turn isn't reachable through `Engine::submit`. This checks
/// the same property `geom.warp`'s resample kernel actually renders, not
/// just `WarpField`'s point-mapping math (already proven in
/// `ng::warp::field::tests::four_quarter_turns_compose_to_identity`), by
/// driving `GeomWarpNode` directly, four times in a row, over a square image,
/// and asserting the result matches the untouched source.
#[test]
fn four_consecutive_90_degree_warp_evals_compose_to_the_original_image() {
    let n = 24u32;
    let mut src = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w: n, h: n });
    for y in 0..n {
        for x in 0..n {
            src.set_rgba_f32(x, y, [x as f32 / n as f32, y as f32 / n as f32, 0.3, 1.0]);
        }
    }

    // One `geom.warp` node, re-driven 4 times in a row over its own previous
    // output (via `SeedProbe`, a trivial pass-through generator), the
    // resample-composition property: 4 quarter turns of a *square* image
    // return, within tolerance, to the original (task E1's AC, at the actual
    // node/kernel level, not just `WarpField`'s point-mapping level, see
    // `ng::warp::field::tests::four_quarter_turns_compose_to_identity`).
    let out_roi = Roi {
        x: 0,
        y: 0,
        w: n,
        h: n,
    };
    let cancel = CancelToken::new();
    let mut current = src.clone();
    for _ in 0..4 {
        let mut graph = RenderGraph::new();
        let gen = graph.add_node(Arc::new(SeedProbe {
            pixels: current.clone(),
        }));
        let params = ParamBlock::from_fields([
            ("angle_deg", ParamValue::Float(90.0)),
            ("src_w", ParamValue::Int(n as i64)),
            ("src_h", ParamValue::Int(n as i64)),
        ])
        .expect("warp params build");
        let warp = graph.add_node_with_params(Arc::new(GeomWarpNode::new()), params);
        graph.connect(gen, warp, "in").expect("seed -> geom.warp");

        let mut probe = TileProbe::default();
        current = TileRender {
            graph: &graph,
            image_extent: Extent { w: n, h: n },
            out_roi,
            scale: 1.0,
            tile_size: n,
            order: TileOrder::RowMajor,
            focus: None,
            seeds: &HashMap::new(),
            cancel: &cancel,
        }
        .render(&mut probe)
        .expect("90deg warp render succeeds");
    }

    let mut max_abs_diff = 0f32;
    for y in 0..n {
        for x in 0..n {
            let a = src.get_rgba_f32(x, y);
            let b = current.get_rgba_f32(x, y);
            for c in 0..4 {
                max_abs_diff = max_abs_diff.max((a[c] - b[c]).abs());
            }
        }
    }
    println!("[E1] 4x90deg node-level composition max abs diff = {max_abs_diff:.6}");
    assert!(
        max_abs_diff < 1e-2,
        "4 successive 90deg geom.warp evals should return to (near) the original image; \
         max_abs_diff={max_abs_diff:.6}"
    );
}

/// A trivial no-input generator that emits `pixels` verbatim, used to chain
/// `geom.warp` evaluations without needing a full `src.decoded` upload.
struct SeedProbe {
    pixels: PixelBuf,
}

static SEED_SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
static SEED_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.seed_probe"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SEED_SCHEMA),
};

impl RenderNode for SeedProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &SEED_DESC
    }
    fn eval_gpu(
        &self,
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        Err(NodeError::Gpu("test.seed_probe is CPU-only".to_owned()))
    }
    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        _inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        let out = ctx.output();
        for y in 0..out.extent.h.min(self.pixels.extent.h) {
            for x in 0..out.extent.w.min(self.pixels.extent.w) {
                out.set_rgba_f32(x, y, self.pixels.get_rgba_f32(x, y));
            }
        }
        Ok(())
    }
}

// ── ROI back-mapping through the warp (task E3) ───────────────────────────────

/// **Task E3 AC**: zooming into a corner of a rotated image evaluates only
/// the needed source tiles, the corner render's total working-pixel count
/// is a small fraction of the whole-image render's, proving `geom.warp`'s
/// `plan` correctly narrows the back-mapped ROI rather than pessimistically
/// requesting the whole source for every output tile.
#[test]
fn corner_zoom_of_warped_image_evaluates_far_fewer_pixels_than_whole_image() {
    let n = 128u32;
    let mut graph = RenderGraph::new();
    let src = graph.add_node(Arc::new(CorpusSourceProbe::new(
        CorpusKind::HighFrequency,
        Extent { w: n, h: n },
    )));
    let params = ParamBlock::from_fields([
        ("angle_deg", ParamValue::Float(15.0)),
        ("src_w", ParamValue::Int(n as i64)),
        ("src_h", ParamValue::Int(n as i64)),
    ])
    .expect("warp params build");
    let warp = graph.add_node_with_params(Arc::new(GeomWarpNode::new()), params);
    graph.connect(src, warp, "in").expect("src -> geom.warp");

    let seeds: HashMap<NodeId, PixelBuf> = HashMap::new();
    let cancel = CancelToken::new();

    let mut whole_probe = TileProbe::default();
    let _whole = TileRender {
        graph: &graph,
        image_extent: Extent { w: n, h: n },
        out_roi: Roi {
            x: 0,
            y: 0,
            w: n,
            h: n,
        },
        scale: 1.0,
        tile_size: 32,
        order: TileOrder::RowMajor,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    }
    .render(&mut whole_probe)
    .expect("whole-image render succeeds");

    let mut corner_probe = TileProbe::default();
    let _corner = TileRender {
        graph: &graph,
        image_extent: Extent { w: n, h: n },
        out_roi: Roi {
            x: 0,
            y: 0,
            w: 32,
            h: 32,
        },
        scale: 1.0,
        tile_size: 32,
        order: TileOrder::RowMajor,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    }
    .render(&mut corner_probe)
    .expect("corner-only render succeeds");

    println!(
        "[E3] corner_pixels_evaluated={} whole_pixels_evaluated={}",
        corner_probe.pixels_evaluated, whole_probe.pixels_evaluated
    );
    assert!(
        corner_probe.pixels_evaluated * 4 < whole_probe.pixels_evaluated,
        "a single-tile corner render must touch far fewer working pixels than the whole \
         16-tile image: corner={} whole={}",
        corner_probe.pixels_evaluated,
        whole_probe.pixels_evaluated
    );
}

/// **Task E3's "no seams" AC**: a multi-tile render of a rotated image
/// stitches to the same result as a single-whole-tile render (mirrors
/// `ToneRecoveryNode`'s own B13 apron-correctness pattern exactly).
#[test]
fn tiled_warp_eval_matches_whole_frame_eval_no_seams() {
    let n = 80u32;
    let mut graph = RenderGraph::new();
    let src = graph.add_node(Arc::new(CorpusSourceProbe::new(
        CorpusKind::HighFrequency,
        Extent { w: n, h: n },
    )));
    let params = ParamBlock::from_fields([
        ("angle_deg", ParamValue::Float(18.0)),
        ("src_w", ParamValue::Int(n as i64)),
        ("src_h", ParamValue::Int(n as i64)),
    ])
    .expect("warp params build");
    let warp = graph.add_node_with_params(Arc::new(GeomWarpNode::new()), params);
    graph.connect(src, warp, "in").expect("src -> geom.warp");

    let seeds: HashMap<NodeId, PixelBuf> = HashMap::new();
    let cancel = CancelToken::new();
    let out_roi = Roi {
        x: 0,
        y: 0,
        w: n,
        h: n,
    };

    let mut whole_probe = TileProbe::default();
    let whole = TileRender {
        graph: &graph,
        image_extent: Extent { w: n, h: n },
        out_roi,
        scale: 1.0,
        tile_size: n,
        order: TileOrder::RowMajor,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    }
    .render(&mut whole_probe)
    .expect("whole-frame render succeeds");

    let mut tiled_probe = TileProbe::default();
    let tiled = TileRender {
        graph: &graph,
        image_extent: Extent { w: n, h: n },
        out_roi,
        scale: 1.0,
        tile_size: 24,
        order: TileOrder::RowMajor,
        focus: None,
        seeds: &seeds,
        cancel: &cancel,
    }
    .render(&mut tiled_probe)
    .expect("multi-tile render succeeds");

    assert!(tiled_probe.tiles_evaluated > 1);
    assert_eq!(whole.extent, tiled.extent);
    let mut max_abs_diff = 0f32;
    for y in 0..n {
        for x in 0..n {
            let a = whole.get_rgba_f32(x, y);
            let b = tiled.get_rgba_f32(x, y);
            for c in 0..4 {
                max_abs_diff = max_abs_diff.max((a[c] - b[c]).abs());
            }
        }
    }
    println!("[E3] tiled-vs-whole-frame max abs channel diff = {max_abs_diff:.6}");
    assert!(
        max_abs_diff < 1e-3,
        "tiled warp eval must match whole-frame eval within float rounding; \
         max_abs_diff={max_abs_diff:.6}"
    );
}

// ── constrain-crop (task E9) ──────────────────────────────────────────────────
//
// The `largest_inscribed_rect` property tests (containment over 10k random
// rotations; locked-aspect ratio exactness) live with the implementation in
// `ng::warp::inscribe`'s own `#[cfg(test)]` module (part of this crate's test
// suite), see that file, not duplicated here.
#[test]
fn geom_crop_and_geom_warp_node_ids_are_the_stable_e11_names() {
    assert_eq!(GeomWarpNode::ID, NodeId("geom.warp"));
    assert_eq!(GeomCropNode::ID, NodeId("geom.crop"));
}
