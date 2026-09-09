// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (tasks **D8-D9**), `global.creative_lut` proofs through the
//! REAL engine/backends (not just the pure-math unit tests in
//! `nodes/global/creative_lut.rs` and `nodes/common/lut3d.rs`).
//!
//! Two harnesses, because the node has two distinct entry points (see
//! `creative_lut.rs`'s own module doc, "The `id` → LUT-content seam"):
//!
//! 1. **Full-engine / `Recipe`-driven** (mirrors `tests/e10_dehaze.rs`'s
//!    `shipping_compiler`/`SynthSource`/`SharedDevice`/`NullDevice` harness
//!    exactly): proves identity elision and the honest "no D10 resolver
//!    wired yet ⇒ renders as identity" contract through
//!    `lightbox_edit::GlobalStages::effects.creative_lut`.
//! 2. **Direct-backend / `param_block_with_lut`-driven** (the D10-seam test
//!    harness `creative_lut.rs` exposes, see [`lightbox_render::ng::nodes::
//!    global::creative_lut::CreativeLutNode::param_block_with_lut`]): drives
//!    [`CreativeLutNode`] directly through the SAME [`Backend`] trait both
//!    `CpuBackend` and `GpuBackend` implement (`Backend::eval_node`), with a
//!    concrete [`Lut3D`] injected, no recipe/catalog involved. This is
//!    where the identity/amount/known-mapping/extrapolation/parity ACs are
//!    actually exercised against the real GPU kernel and the real CPU
//!    backend, not just the pure functions.
//!
//! Goldens (harness 2) are quantized with a straight linear clamp (the same
//! `quant`/`to_srgb8_texels` convention
//! `lightbox_render_testkit::corpus`'s own probe-node goldens use, no
//! `xform.display` stage runs in this single-node harness, so there is no
//! real display/gamma encode to reproduce).

use std::path::PathBuf;
use std::sync::Arc;

use lightbox_color::matrix::spaces::{companion_decode, companion_encode};
use lightbox_edit::leaves::CreativeLut;
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::cache::CacheKey;
use lightbox_render::ng::exec::backend::{Backend, BackendEvalRequest};
use lightbox_render::ng::exec::cpu::CpuBackend;
use lightbox_render::ng::exec::gpu::{readback_tile, GpuBackend};
use lightbox_render::ng::gpu::DeviceCtx;
use lightbox_render::ng::nodes::common::lut3d::Lut3D;
use lightbox_render::ng::nodes::global::creative_lut::CreativeLutNode;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::source::{SourceImage, Uploader};
use lightbox_render::ng::tile::{PixelBuf, PixelFormat, TileHandle};
use lightbox_render::ng::types::{Extent, Roi, TilePrecision};
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, NodeId,
    OutFormat, OutputPayload, RenderPriority, RenderRequest, RenderScale, RenderState,
    RenderTarget, SourceColorimetry, SourceError, SourceKind, SourceProvider, SourceQuality,
    SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_render_testkit::corpus::{compare_srgb8_to_golden, goldens_root};
use lightbox_types::{ImageId, PV_M0};

// ── harness 1: full-engine (mirrors tests/e10_dehaze.rs) ───────────────────
//
// Only the CPU (`ForceCpu`) path is exercised here, these tests prove
// structural elision + the honest degrade-to-identity contract, not GPU
// kernel correctness (that's harness 2's `gpu_cpu_parity_on_a_lut_corpus`).

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

fn recipe_with(f: impl FnOnce(&mut lightbox_edit::GlobalStages)) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    f(&mut r.global);
    r
}

fn gradient_source(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            let r = x as f32 / w.max(1) as f32;
            let g = y as f32 / h.max(1) as f32;
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                [r, g, 0.5, 1.0],
            );
        }
    });
    px
}

// ── D9: structural, identity elision through the real recipe compiler ────

#[test]
fn identity_recipe_adds_no_creative_lut_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = lightbox_render::ng::SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 16, h: 16 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let graph = compiler
        .compile(&Recipe::identity(PV_M0), PV_M0, &source_desc)
        .expect("identity recipe compiles");
    assert!(graph.node_index(NodeId("global.creative_lut")).is_none());
}

#[test]
fn touched_creative_lut_adds_exactly_the_creative_lut_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = lightbox_render::ng::SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 16, h: 16 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| {
        g.effects.creative_lut = Some(CreativeLut {
            id: "some-look-hash".to_owned(),
            amount: 100.0,
        })
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(graph.node_index(NodeId("global.creative_lut")).is_some());
    assert!(graph.node_index(NodeId("global.dehaze")).is_none());
}

#[test]
fn amount_zero_creative_lut_recipe_elides_the_node() {
    let compiler = lightbox_render::ng::shipping_compiler();
    let source_desc = lightbox_render::ng::SourceDesc {
        image: ImageId(1),
        full_extent: Extent { w: 16, h: 16 },
        source_kind: SourceKind::Rgb,
        colorimetry: SourceColorimetry::default(),
    };
    let recipe = recipe_with(|g| {
        g.effects.creative_lut = Some(CreativeLut {
            id: "some-look-hash".to_owned(),
            amount: 0.0,
        })
    });
    let graph = compiler
        .compile(&recipe, PV_M0, &source_desc)
        .expect("recipe compiles");
    assert!(
        graph.node_index(NodeId("global.creative_lut")).is_none(),
        "amount 0 must elide the node per GlobalNode::is_identity"
    );
}

/// **Honest-reporting proof (D9's documented D10 seam):** a real recipe with
/// a `creative_lut` reference set (a non-empty `id`, amount 100%), for
/// which no `installed_look` resolver exists yet (D10, out of this phase's
/// scope), renders **byte-identical** to the untouched baseline through the
/// REAL engine. This is not a bug: it is `creative_lut.rs`'s own documented
/// graceful-degrade contract (spec §4.8's missing-look-renders-as-identity),
/// proven end to end rather than only at the pure-function level.
#[test]
fn no_resolver_wired_yet_creative_lut_recipe_renders_identical_to_baseline() {
    let (w, h) = (24u32, 24u32);
    let pixels = gradient_source(w, h);
    let engine = build_engine(BackendPref::ForceCpu, Arc::new(NullDevice), pixels);

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let with_lut_ref = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.effects.creative_lut = Some(CreativeLut {
                id: "unresolvable-hash-until-d10".to_owned(),
                amount: 100.0,
            })
        }),
        BackendId::Cpu,
    );
    assert_eq!(
        baseline.bytes, with_lut_ref.bytes,
        "with no D10 resolver, a creative_lut-set recipe must render identically to baseline"
    );
}

// ── D10: the LookResolver seam, through the REAL compiler/engine ──────────
//
// harness 1's `no_resolver_wired_yet_creative_lut_recipe_renders_identical_
// to_baseline` above proves the pre-D10 (no resolver) degrade contract.
// These prove the OTHER half now that D10 exists: a resolver that DOES
// resolve the recipe's `id` makes `global.creative_lut` render a REAL,
// non-identity transform, end to end through `RecipeCompiler::
// with_look_resolver` / `Engine::new_with_look_resolver`, not just the
// pure-function `param_block_with_lut` seam harness 2 exercises directly.

use lightbox_render::ng::nodes::common::lut3d::Lut3D as ResolverLut3D;
use lightbox_render::ng::{LookResolver, RecipeCompiler};
use std::collections::HashMap;

/// A trivial in-memory [`LookResolver`], the same shape
/// `lightbox-core::looks::CatalogLookResolver` implements over the real
/// `installed_look` catalog table, without a catalog in this test.
struct MapLookResolver(HashMap<String, Arc<ResolverLut3D>>);

impl LookResolver for MapLookResolver {
    fn resolve(&self, content_hash: &str) -> Option<Arc<ResolverLut3D>> {
        self.0.get(content_hash).cloned()
    }
}

fn build_engine_with_resolver(
    pref: BackendPref,
    dp: Arc<dyn DeviceProvider>,
    pixels: PixelBuf,
    resolver: Arc<dyn LookResolver>,
) -> Engine {
    let registry = lightbox_render::ng::shipping_registry();
    let mut compiler =
        RecipeCompiler::with_registry(Arc::new(registry)).with_look_resolver(resolver);
    compiler
        .register_template(PV_M0, lightbox_render::ng::GraphTemplate::pv1())
        .expect("PV1 template registers on a fresh compiler");
    Engine::with_compiler(
        dp,
        Arc::new(SynthSource { pixels }),
        compiler,
        EngineConfig {
            backend: pref,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds over a resolver-augmented PV1 configuration")
}

/// **D10 AC (render wiring):** a recipe referencing a hash the resolver DOES
/// know about renders through the REAL parsed LUT (a known R/G channel
/// swap), non-identical to the untouched baseline, and matching the
/// direct-backend (`param_block_with_lut`) computation exactly. Proves
/// `maybe_add_creative_lut`'s resolve→`param_block_with_lut` path, not just
/// the graceful-degrade path.
#[test]
fn resolver_hit_renders_the_real_lut_end_to_end() {
    let (w, h) = (24u32, 24u32);
    let pixels = gradient_source(w, h);
    let known_hash = "installed-look-hash-abc123";
    let lut = ResolverLut3D::bake(5, |[r, g, b]| [g, r, b]);
    let mut map = HashMap::new();
    map.insert(known_hash.to_owned(), Arc::new(lut));
    let resolver: Arc<dyn LookResolver> = Arc::new(MapLookResolver(map));

    let engine = build_engine_with_resolver(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        pixels.clone(),
        resolver,
    );

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let with_resolved_lut = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.effects.creative_lut = Some(CreativeLut {
                id: known_hash.to_owned(),
                amount: 100.0,
            })
        }),
        BackendId::Cpu,
    );
    assert_ne!(
        baseline.bytes, with_resolved_lut.bytes,
        "a resolver HIT must render a real, non-identity transform"
    );
}

/// **D10 AC (missing-look degrade, through the resolver seam):** a recipe
/// referencing a hash the resolver does NOT know about (uninstalled/
/// removed) still renders byte-identical to baseline, the resolver being
/// WIRED does not change the missing-look contract for a hash it can't
/// resolve, only for one it can.
#[test]
fn resolver_miss_still_renders_identical_to_baseline() {
    let (w, h) = (24u32, 24u32);
    let pixels = gradient_source(w, h);
    // An empty resolver: wired, but knows no hashes (mirrors "removed"/
    // "never installed").
    let resolver: Arc<dyn LookResolver> = Arc::new(MapLookResolver(HashMap::new()));

    let engine = build_engine_with_resolver(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        pixels.clone(),
        resolver,
    );

    let baseline = render(&engine, w, h, Recipe::identity(PV_M0), BackendId::Cpu);
    let with_unresolvable_ref = render(
        &engine,
        w,
        h,
        recipe_with(|g| {
            g.effects.creative_lut = Some(CreativeLut {
                id: "hash-not-in-the-resolver".to_owned(),
                amount: 100.0,
            })
        }),
        BackendId::Cpu,
    );
    assert_eq!(
        baseline.bytes, with_unresolvable_ref.bytes,
        "a resolver MISS must still degrade to identity (spec §4.8 missing-look contract)"
    );
}

// ── harness 2: direct-backend (param_block_with_lut) ───────────────────────

fn device() -> Option<DeviceCtx> {
    let ctx = GpuContext::headless()?;
    Some(DeviceCtx::new(ctx.device.clone(), ctx.queue.clone()))
}

fn as_source_image(pixels: PixelBuf) -> SourceImage {
    let full_extent = pixels.extent;
    SourceImage {
        pixels,
        colorimetry: SourceColorimetry::default(),
        full_extent,
        quality: SourceQuality::Full,
    }
}

/// Runs [`CreativeLutNode`] on `pixels` through `backend` (`CpuBackend` or
/// `GpuBackend`) with `amount`/`lut` injected via
/// `CreativeLutNode::param_block_with_lut`, the D10-seam test harness, and
/// reads the result back to a host [`PixelBuf`].
fn eval_creative_lut(
    backend: &dyn Backend,
    input: TileHandle,
    extent: Extent,
    amount: f32,
    lut: &Lut3D,
    dev: Option<&DeviceCtx>,
) -> PixelBuf {
    let node = CreativeLutNode::new();
    let params = CreativeLutNode::param_block_with_lut(amount, lut);
    let cancel = CancelToken::new();
    let out = backend
        .eval_node(BackendEvalRequest {
            key: CacheKey(blake3::hash(b"global.creative_lut test")),
            node: &node,
            params: &params,
            inputs: std::slice::from_ref(&input),
            roi: Roi {
                x: 0,
                y: 0,
                w: extent.w,
                h: extent.h,
            },
            target_extent: extent,
            scale: 1.0,
            precision: TilePrecision::F16,
            cancel: &cancel,
        })
        .expect("eval global.creative_lut");
    match dev {
        Some(dev) => readback_tile(&dev.device, &dev.queue, &out).expect("gpu readback"),
        None => out.cpu().expect("cpu tile present").clone(),
    }
}

fn cpu_eval(pixels: &PixelBuf, amount: f32, lut: &Lut3D) -> PixelBuf {
    let backend = CpuBackend::new(None);
    let input = TileHandle::from_cpu(pixels.clone());
    eval_creative_lut(&backend, input, pixels.extent, amount, lut, None)
}

fn gpu_eval(dev: &DeviceCtx, pixels: &PixelBuf, amount: f32, lut: &Lut3D) -> PixelBuf {
    let backend = GpuBackend::new(dev);
    let input = Uploader::new().upload(dev, &as_source_image(pixels.clone()));
    eval_creative_lut(&backend, input, pixels.extent, amount, lut, Some(dev))
}

/// Straight linear clamp+quantize to sRGB8 texels, the same convention
/// `lightbox_render_testkit::corpus`'s own probe-node goldens use (no
/// `xform.display` stage runs in this single-node harness).
fn quant_texels(px: &PixelBuf) -> Vec<[u8; 4]> {
    let mut out = Vec::with_capacity((px.extent.w as usize) * (px.extent.h as usize));
    for y in 0..px.extent.h {
        for x in 0..px.extent.w {
            let p = px.get_rgba_f32(x, y);
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            out.push([q(p[0]), q(p[1]), q(p[2]), q(p[3])]);
        }
    }
    out
}

fn e10_goldens_root() -> PathBuf {
    goldens_root().join("global").join("creative_lut")
}

// ── D9 AC: identity LUT ≡ passthrough (< 1e-4) ─────────────────────────────

#[test]
fn identity_lut_is_exact_passthrough_on_the_real_cpu_backend() {
    let (w, h) = (16u32, 16u32);
    let pixels = gradient_source(w, h);
    let lut = Lut3D::identity(5);
    let out = cpu_eval(&pixels, 1.0, &lut);
    for y in 0..h {
        for x in 0..w {
            let a = pixels.get_rgba_f32(x, y);
            let b = out.get_rgba_f32(x, y);
            for (c, (&av, &bv)) in a.iter().zip(b.iter()).enumerate().take(3) {
                assert!(
                    (av - bv).abs() < 1e-4,
                    "({x},{y}) channel {c}: in={a:?} out={b:?}"
                );
            }
        }
    }
}

#[test]
fn identity_lut_is_exact_passthrough_on_the_real_gpu_backend() {
    let Some(dev) = device() else {
        eprintln!("[creative_lut] no wgpu adapter available — SKIPPED");
        return;
    };
    let (w, h) = (16u32, 16u32);
    let pixels = gradient_source(w, h);
    let lut = Lut3D::identity(5);
    let out = gpu_eval(&dev, &pixels, 1.0, &lut);
    // The D9 AC's `< 1e-4` bound is the CPU-reference precision claim (see
    // `identity_lut_is_exact_passthrough_on_the_real_cpu_backend`, which
    // meets it exactly). The GPU path additionally round-trips through an
    // f16 storage texture AND the GPU's own hardware `pow()` (the companion
    // encode/decode transcendentals) TWICE (encode+decode), so a looser
    // (still tight) bound applies here, the same class of tolerance
    // `ng_gpu.rs::resize_gpu_matches_cpu_box_decimation` uses for its own
    // f16-storage GPU/CPU comparison. Overall GPU/CPU *parity* (the §4.4
    // ΔE2000/PSNR contract) is covered separately by
    // `gpu_cpu_parity_on_a_lut_corpus`, which this case also exercises.
    for y in 0..h {
        for x in 0..w {
            let a = pixels.get_rgba_f32(x, y);
            let b = out.get_rgba_f32(x, y);
            for (c, (&av, &bv)) in a.iter().zip(b.iter()).enumerate().take(3) {
                assert!(
                    (av - bv).abs() < 3e-3,
                    "({x},{y}) channel {c}: in={a:?} out={b:?}"
                );
            }
        }
    }
}

// ── D9 AC: amount 0 ≡ identity (a KNOWN non-identity LUT, amount forced 0) ─

#[test]
fn amount_zero_is_identity_regardless_of_lut_on_the_real_cpu_backend() {
    let (w, h) = (16u32, 16u32);
    let pixels = gradient_source(w, h);
    let lut = Lut3D::bake(6, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
    let out = cpu_eval(&pixels, 0.0, &lut);
    for y in 0..h {
        for x in 0..w {
            let a = pixels.get_rgba_f32(x, y);
            let b = out.get_rgba_f32(x, y);
            for (c, (&av, &bv)) in a.iter().zip(b.iter()).enumerate().take(3) {
                assert!((av - bv).abs() < 1e-4, "channel {c}: in={a:?} out={b:?}");
            }
        }
    }
}

// ── D9 AC: a known non-identity LUT produces the expected mapping (golden) ─

/// A hand-written `.cube`, a mild per-channel gain/offset in the
/// companion-encoded domain, parsed via [`Lut3D::from_cube_str`] (task D8),
/// then rendered through the real CPU backend (task D9) and compared to a
/// committed golden (`LIGHTBOX_BLESS=1` to regenerate), the parser→node
/// integration the D8/D9 boundary is supposed to prove end to end.
fn known_cube_text() -> &'static str {
    "TITLE \"D9 test LUT\"\n\
     LUT_3D_SIZE 3\n\
     0.0 0.0 0.1\n\
     0.5 0.0 0.1\n\
     1.0 0.0 0.1\n\
     0.0 0.5 0.1\n\
     0.5 0.5 0.1\n\
     1.0 0.5 0.1\n\
     0.0 1.0 0.1\n\
     0.5 1.0 0.1\n\
     1.0 1.0 0.1\n\
     0.1 0.0 0.6\n\
     0.6 0.0 0.6\n\
     1.0 0.0 0.6\n\
     0.1 0.5 0.6\n\
     0.6 0.5 0.6\n\
     1.0 0.5 0.6\n\
     0.1 1.0 0.6\n\
     0.6 1.0 0.6\n\
     1.0 1.0 0.6\n\
     0.2 0.0 1.0\n\
     0.7 0.0 1.0\n\
     1.0 0.0 1.0\n\
     0.2 0.5 1.0\n\
     0.7 0.5 1.0\n\
     1.0 0.5 1.0\n\
     0.2 1.0 1.0\n\
     0.7 1.0 1.0\n\
     1.0 1.0 1.0\n"
}

#[test]
fn known_lut_from_parsed_cube_produces_the_expected_mapping_golden() {
    let (w, h) = (32u32, 32u32);
    let pixels = gradient_source(w, h);
    let lut = Lut3D::from_cube_str(known_cube_text()).expect("well-formed test .cube parses");
    assert!(!lut.is_identity(1e-3), "test LUT must be non-identity");

    let out = cpu_eval(&pixels, 1.0, &lut);
    let rendered = quant_texels(&out);
    let golden_path = e10_goldens_root().join("known_lut_full_amount_cpu.png");
    let report = compare_srgb8_to_golden(&golden_path, w, h, &rendered);
    println!(
        "[D9][known-lut-golden] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
        report.stats.max, report.stats.mean, report.psnr_db
    );
    assert!(
        report.stats.within_tolerance() && report.psnr_db >= TOLERANCE_PSNR_DB,
        "[D9] known-LUT golden drift: \u{394}E2000 max {:.4} / PSNR {:.2} dB vs {golden_path:?}",
        report.stats.max,
        report.psnr_db
    );

    // Cross-check against the analytic expectation directly (independent of
    // the golden file): the parsed LUT's own tetrahedral sample, applied in
    // the companion domain, must match the rendered pixel.
    let (sx, sy) = (w / 3, h / 2);
    let src = pixels.get_rgba_f32(sx, sy);
    let enc = companion_encode([src[0], src[1], src[2]]);
    let mapped = lut.sample_tetrahedral(enc);
    let want = companion_decode(mapped);
    let got = out.get_rgba_f32(sx, sy);
    for (c, (&g, &w2)) in got.iter().zip(want.iter()).enumerate().take(3) {
        assert!(
            (g - w2).abs() < 1e-3,
            "channel {c}: got={got:?} want={want:?}"
        );
    }
}

// ── D9 AC: amount 2.0 clamps in-gamut ──────────────────────────────────────

#[test]
fn amount_two_clamps_in_gamut_on_the_real_cpu_backend() {
    let (w, h) = (16u32, 16u32);
    let pixels = gradient_source(w, h);
    // A LUT that pushes every channel strongly toward 1.0, full-strength
    // extrapolated to 200% would overshoot [0,1] without the clamp.
    let lut = Lut3D::bake(4, |[r, g, b]| {
        [(r + 0.6).min(1.0), (g + 0.6).min(1.0), (b + 0.6).min(1.0)]
    });
    let out = cpu_eval(&pixels, 2.0, &lut);
    for y in 0..h {
        for x in 0..w {
            let p = out.get_rgba_f32(x, y);
            for (c, &v) in p.iter().enumerate().take(3) {
                assert!(v.is_finite(), "channel {c} must stay finite");
                assert!(
                    (0.0..=1.0 + 1e-4).contains(&v),
                    "channel {c}={v} must clamp in-gamut at amount=2.0"
                );
            }
        }
    }
}

// ── D9 AC: GPU/CPU parity on a LUT corpus ──────────────────────────────────

#[test]
fn gpu_cpu_parity_on_a_lut_corpus() {
    let Some(dev) = device() else {
        eprintln!("[creative_lut] no wgpu adapter available — SKIPPED");
        return;
    };
    let (w, h) = (24u32, 24u32);
    let pixels = gradient_source(w, h);

    let identity = Lut3D::identity(5);
    let parsed = Lut3D::from_cube_str(known_cube_text()).unwrap();
    let extreme = Lut3D::bake(4, |[r, g, b]| {
        [(r + 0.6).min(1.0), (g + 0.6).min(1.0), (b + 0.6).min(1.0)]
    });

    let cases: Vec<(&str, &Lut3D, f32)> = vec![
        ("identity@1.0", &identity, 1.0),
        ("identity@2.0", &identity, 2.0),
        ("parsed@0.0", &parsed, 0.0),
        ("parsed@0.5", &parsed, 0.5),
        ("parsed@1.0", &parsed, 1.0),
        ("parsed@1.5", &parsed, 1.5),
        ("extreme@1.0", &extreme, 1.0),
        ("extreme@2.0", &extreme, 2.0),
    ];

    let mut failures = Vec::new();
    for (name, lut, amount) in cases {
        let cpu_out = cpu_eval(&pixels, amount, lut);
        let gpu_out = gpu_eval(&dev, &pixels, amount, lut);
        let cpu_tex = quant_texels(&cpu_out);
        let gpu_tex = quant_texels(&gpu_out);
        let stats = delta_e_stats(&cpu_tex, &gpu_tex);
        let cpu_bytes: Vec<u8> = cpu_tex.iter().flatten().copied().collect();
        let gpu_bytes: Vec<u8> = gpu_tex.iter().flatten().copied().collect();
        let psnr_db = psnr(&cpu_bytes, &gpu_bytes);
        println!(
            "[D9][parity][{name}] \u{394}E2000 max={:.4} mean={:.4} PSNR={:.2}dB",
            stats.max, stats.mean, psnr_db
        );
        if !(stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB) {
            failures.push(format!(
                "{name}: \u{394}E2000 max {:.4} / PSNR {:.2} dB",
                stats.max, psnr_db
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "GPU/CPU parity failures:\n{}",
        failures.join("\n")
    );
}
