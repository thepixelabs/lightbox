// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Corpus manifest + `GoldenCase` runner + per-PV golden matrix (spec §6; tasks
//! **A16** / **D3**).
//!
//! Owner: **A-core** (A16 first single-node golden + manifest/runner) and **D**
//! (per-PV matrix, D3). The manifest (de)serialization here is real; the
//! rendering/comparison runner is a stub until A16 wires the engine.
//!
//! Corpus: ≥ 6 synthetic sources — gradients / checker / low-key / high-key /
//! high-frequency / wide-gamut / 100 MP-synthetic — committed with provenance
//! (own-shot / CC0 only). Goldens rendered from the **CPU reference path**.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::node::{NodeDescriptor, ParamsSchema, ParamsSchemaRef};
use lightbox_render::ng::{
    CpuBackend, CpuEvalCtx, CpuTileView, Executor, Extent, GpuEvalCtx, NodeCache, NodeError, NodeId,
    ParamBlock, ParamValue, PixelBuf, PixelFormat, PortDecl, PortType, RenderGraph, RenderNode,
    RenderScale, Roi, TileView, PV_TEST_999,
};
use lightbox_types::ProcessVersion;
use serde::{Deserialize, Serialize};

use crate::compare::{delta_e_stats, psnr, DeltaEStats, TOLERANCE_PSNR_DB};
use crate::probes::{BlurRProbe, CheckerProbe, GainProbe};

/// The corpus manifest — the committed list of test sources (spec §6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CorpusManifest {
    /// Manifest format version.
    pub version: u32,
    /// The test sources.
    pub sources: Vec<CorpusSource>,
}

impl CorpusManifest {
    /// Serialize to pretty JSON (the committed on-disk form).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("CorpusManifest serializes")
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> Result<CorpusManifest, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// One synthetic corpus source (spec §6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CorpusSource {
    /// Stable source name (keys goldens).
    pub name: String,
    /// The synthetic pattern kind.
    pub kind: CorpusKind,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Provenance note (own-shot / CC0 only — §8 surface 3 applies to test data).
    pub provenance: String,
}

/// The synthetic corpus pattern families (spec §6).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpusKind {
    /// Smooth gradients.
    Gradient,
    /// Hard-edged checker.
    Checker,
    /// Low-key (shadow-weighted).
    LowKey,
    /// High-key (highlight-weighted).
    HighKey,
    /// High-frequency detail.
    HighFrequency,
    /// Wide-gamut saturated.
    WideGamut,
    /// 100 MP-synthetic (VRAM-budget stress).
    Synthetic100Mp,
}

/// Generate the synthetic source pixels for a [`CorpusKind`] at `w`×`h`, as a
/// working-format (`Rgba16F`) tile — the seed the tiled/scale/soak executors
/// consume as `src.decoded` (spec §6 corpus; tasks C2/C10). Deterministic and
/// self-contained (own-math patterns, no external data → §8 surface-3 clean).
pub fn synth_source(kind: CorpusKind, w: u32, h: u32) -> PixelBuf {
    let w = w.max(1);
    let h = h.max(1);
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w, h });
    let bpp = PixelFormat::Rgba16F.bytes_per_pixel() as usize;
    px.par_fill_rows(|y, row| {
        for x in 0..w {
            PixelBuf::encode_pixel(
                PixelFormat::Rgba16F,
                &mut row[x as usize * bpp..],
                synth_pixel(kind, x, y, w, h),
            );
        }
    });
    px
}

/// The scene-linear working RGBA for one synthetic-corpus pixel.
fn synth_pixel(kind: CorpusKind, x: u32, y: u32, w: u32, h: u32) -> [f32; 4] {
    let fx = x as f32 / w as f32;
    let fy = y as f32 / h as f32;
    match kind {
        CorpusKind::Gradient | CorpusKind::Synthetic100Mp => [fx, fy, 0.5, 1.0],
        CorpusKind::Checker => {
            if (x / 8 + y / 8).is_multiple_of(2) {
                [0.2, 0.45, 0.7, 1.0]
            } else {
                [0.85, 0.15, 0.5, 1.0]
            }
        }
        CorpusKind::LowKey => [0.02 + 0.10 * fx, 0.02 + 0.08 * fy, 0.03, 1.0],
        CorpusKind::HighKey => [0.85 + 0.14 * fx, 0.88 + 0.11 * fy, 0.90, 1.0],
        CorpusKind::HighFrequency => {
            let v = if (x + y).is_multiple_of(2) { 0.9 } else { 0.05 };
            [v, v, v, 1.0]
        }
        CorpusKind::WideGamut => {
            // Saturated primaries rotating across the frame (values can exceed 1.0
            // — scene-linear wide-gamut highlights).
            let sector = ((fx * 3.0) as u32).min(2);
            match sector {
                0 => [1.4 * (1.0 - fy), 0.0, 0.0, 1.0],
                1 => [0.0, 1.3 * (1.0 - fy), 0.0, 1.0],
                _ => [0.0, 0.0, 1.5 * (1.0 - fy), 1.0],
            }
        }
    }
}

/// A source generator node (no inputs) emitting a chosen [`CorpusKind`] pattern
/// at a fixed extent — the varying root of the per-PV golden matrix (task D3).
/// The pattern is [`synth_pixel`] (own-math, deterministic), so a matrix render
/// is a pure function of `(kind, extent, downstream stages)` — a stable golden.
#[derive(Clone, Copy, Debug)]
pub struct CorpusSourceProbe {
    kind: CorpusKind,
    extent: Extent,
}

impl CorpusSourceProbe {
    /// A source probe emitting `kind` at `extent`.
    pub fn new(kind: CorpusKind, extent: Extent) -> CorpusSourceProbe {
        CorpusSourceProbe { kind, extent }
    }
}

static CORPUS_SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
static CORPUS_DESC: NodeDescriptor = NodeDescriptor {
    id: NodeId("test.corpus_src"),
    inputs: &[],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&CORPUS_SCHEMA),
};

impl RenderNode for CorpusSourceProbe {
    fn descriptor(&self) -> &NodeDescriptor {
        &CORPUS_DESC
    }

    fn eval_gpu(
        &self,
        _ctx: &mut GpuEvalCtx<'_>,
        _inputs: &[TileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        // The matrix renders on the CPU reference path (§4.4 / R3); a GPU kernel
        // is unnecessary for the golden gate.
        Err(NodeError::Gpu(
            "test.corpus_src is a CPU-reference generator (golden matrix)".to_owned(),
        ))
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        _inputs: &[CpuTileView<'_>],
        _params: &ParamBlock,
    ) -> Result<(), NodeError> {
        // Emit absolute-coordinate synthetic pixels so the pattern is
        // position-correct over the whole ROI (the matrix renders whole-image).
        let (kind, w, h) = (self.kind, self.extent.w, self.extent.h);
        let ox = ctx.out_roi.x;
        let oy = ctx.out_roi.y;
        let out = ctx.output();
        let (ow, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|ly, row| {
            let ay = (oy + ly as i32).max(0) as u32;
            for lx in 0..ow {
                let ax = (ox + lx as i32).max(0) as u32;
                PixelBuf::encode_pixel(
                    fmt,
                    &mut row[lx as usize * bpp..],
                    synth_pixel(kind, ax, ay, w, h),
                );
            }
        });
        Ok(())
    }
}

/// One golden case: a `(node, pv, source, recipe)` rendered and compared to a
/// committed golden PNG (spec §6/A16).
#[derive(Clone, Debug)]
pub struct GoldenCase {
    /// Dotted node id (e.g. `"xform.display"`).
    pub node: String,
    /// The process version to render under.
    pub pv: ProcessVersion,
    /// The corpus source name.
    pub source: String,
    /// The recipe, as JSON (materialized by A16/D).
    pub recipe_json: String,
    /// Path to the committed golden PNG.
    pub golden_path: PathBuf,
}

/// The outcome of a golden comparison (spec §6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GoldenReport {
    /// ΔE2000 statistics vs the golden.
    pub stats: DeltaEStats,
    /// PSNR (dB) vs the golden.
    pub psnr_db: f64,
    /// Whether the case passed the §4.4 tolerance.
    pub passed: bool,
}

/// The A16 first-golden extent — `test.checker`'s cell edge (8) × 8 cells.
const GOLDEN_W: u32 = 64;
/// The A16 first-golden extent (see [`GOLDEN_W`]).
const GOLDEN_H: u32 = 64;

/// The repository-committed goldens root for this crate
/// (`…/lightbox-render-testkit/goldens`). Layout mirrors the seed's
/// `<node>/<pv>/<case>.png`.
pub fn goldens_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

/// The A16 first golden: a `test.gain` (×2) over a known `test.checker` input,
/// rendered on the CPU reference path, pinned as `test.gain/pv1/checker-gain.png`
/// (the §10.1 E05.1 gate). D generalizes the case→graph mapping for the matrix.
pub fn first_gain_case() -> GoldenCase {
    GoldenCase {
        node: "test.gain".to_owned(),
        pv: ProcessVersion(1),
        source: "checker-64".to_owned(),
        recipe_json: r#"{"gain":2.0}"#.to_owned(),
        golden_path: goldens_root()
            .join("test.gain")
            .join("pv1")
            .join("checker-gain.png"),
    }
}

/// Render the A16 reference graph — `test.checker → test.gain(gain)` — on the
/// **CPU reference path** ([`Executor`] over [`CpuBackend`]) and read the terminal
/// working tile back to sRGB8 texels. Deterministic: fixed pattern, fixed gain,
/// `f16` storage rounding, straight quantization — identical bytes on every
/// platform, so the committed golden is a stable regression pin.
fn render_checker_gain(width: u32, height: u32, gain: f64) -> Vec<[u8; 4]> {
    let mut graph = RenderGraph::new();
    let checker = graph.add_node(Arc::new(CheckerProbe::default()));
    let params =
        ParamBlock::from_fields([("gain", ParamValue::Float(gain))]).expect("gain param builds");
    let gain_node = graph.add_node_with_params(Arc::new(GainProbe::default()), params);
    graph
        .connect(checker, gain_node, "in")
        .expect("checker→gain edge type-checks");
    render_probe_graph_srgb8(&graph, Extent { w: width, h: height }, ProcessVersion(1))
}

/// Render a probe graph on the **CPU reference path** ([`Executor`] over
/// [`CpuBackend`]) over the whole `extent` at 1:1, keyed under `pv`, and read the
/// terminal working tile back to sRGB8 texels via the same straight quantization
/// the engine's `Buffer` readback uses. Deterministic → the committed golden is a
/// stable regression pin (§4.4 / R3). Shared by the A16 golden and the per-PV
/// matrix (task D3).
fn render_probe_graph_srgb8(graph: &RenderGraph, extent: Extent, pv: ProcessVersion) -> Vec<[u8; 4]> {
    let exec = Executor::new(Arc::new(CpuBackend::new(None)));
    let cache = NodeCache::new();
    let cancel = CancelToken::new();
    let roi = Roi {
        x: 0,
        y: 0,
        w: extent.w,
        h: extent.h,
    };
    let tile = exec
        .evaluate(graph, pv, roi, RenderScale::OneToOne, &cache, &cancel, None)
        .expect("reference render succeeds");
    let px = tile
        .cpu()
        .expect("CPU terminal tile present on the reference path");
    to_srgb8_texels(px)
}

/// Quantize a working tile to sRGB8 texels — the same straight (linear) pack the
/// engine's `Buffer` readback does; display encoding is `xform.display`'s job,
/// not the harness's (E02 guardrail).
fn to_srgb8_texels(px: &PixelBuf) -> Vec<[u8; 4]> {
    let mut out = Vec::with_capacity((px.extent.w as usize) * (px.extent.h as usize));
    for y in 0..px.extent.h {
        for x in 0..px.extent.w {
            let p = px.get_rgba_f32(x, y);
            out.push([quant(p[0]), quant(p[1]), quant(p[2]), quant(p[3])]);
        }
    }
    out
}

#[inline]
fn quant(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Whether `LIGHTBOX_BLESS=1` — regenerate (overwrite) committed goldens.
fn bless_requested() -> bool {
    std::env::var("LIGHTBOX_BLESS").is_ok_and(|v| v == "1")
}

/// Encode `rgba` as an 8-bit RGBA PNG at `path`, creating parent directories.
fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Decode an 8-bit RGBA PNG at `path` to `(w, h, rgba_bytes)`.
fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| "golden output size overflows".to_owned())?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.bit_depth != png::BitDepth::Eight || info.color_type != png::ColorType::Rgba {
        return Err(format!(
            "golden must be 8-bit RGBA, got {:?}/{:?}",
            info.bit_depth, info.color_type
        ));
    }
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}

/// Load the committed golden at `path`, or — under `LIGHTBOX_BLESS=1` — (re)write
/// it from `rendered` and return that. Panics if a golden is missing without
/// bless (CI must fail on a lost golden, not silently self-certify).
fn golden_bytes_or_bless(path: &Path, w: u32, h: u32, rendered: &[u8]) -> Vec<u8> {
    if bless_requested() {
        write_png(path, w, h, rendered).expect("bless: write golden PNG");
        return rendered.to_vec();
    }
    let (gw, gh, bytes) = read_png(path).unwrap_or_else(|e| {
        panic!("golden {path:?} unreadable ({e}); regenerate with LIGHTBOX_BLESS=1")
    });
    assert_eq!(
        (gw, gh),
        (w, h),
        "golden {path:?} is {gw}x{gh}, render is {w}x{h}"
    );
    bytes
}

/// Render `case` on the CPU reference path and compare to its committed golden
/// (spec §6; task A16). The A16 first golden is the `test.checker → test.gain`
/// reference graph; other node ids currently render the same reference graph
/// (D wires the full case→graph mapping for the per-PV matrix).
pub fn run_golden(case: &GoldenCase) -> GoldenReport {
    let gain = parse_gain(&case.recipe_json).unwrap_or(2.0);
    let rendered = render_checker_gain(GOLDEN_W, GOLDEN_H, gain);
    compare_srgb8_to_golden(&case.golden_path, GOLDEN_W, GOLDEN_H, &rendered)
}

/// Compare rendered sRGB8 texels to a committed golden PNG at `path` within the
/// §4.4 tolerance (ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB); under `LIGHTBOX_BLESS=1`
/// (re)write the golden. Shared by the A16 golden and the per-PV matrix runner.
fn compare_srgb8_to_golden(
    path: &Path,
    w: u32,
    h: u32,
    rendered: &[[u8; 4]],
) -> GoldenReport {
    let rendered_bytes: Vec<u8> = rendered.iter().flatten().copied().collect();
    let golden_bytes = golden_bytes_or_bless(path, w, h, &rendered_bytes);
    let golden_texels: Vec<[u8; 4]> = golden_bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    let stats = delta_e_stats(&golden_texels, rendered);
    let psnr_db = psnr(&golden_bytes, &rendered_bytes);
    let passed = stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB;
    GoldenReport {
        stats,
        psnr_db,
        passed,
    }
}

/// A minimal `{"gain": <f>}` extractor for the first-golden recipe (D replaces
/// this with the real `Recipe` fragment decode).
fn parse_gain(recipe_json: &str) -> Option<f64> {
    let after = recipe_json.split("\"gain\"").nth(1)?;
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

// ── Per-PV golden matrix (task D3) ──────────────────────────────────────────
//
// The matrix renders `corpus_source → gain [→ blur for PV999]` on the CPU
// reference path across corpus × recipe × registered PVs and compares each to a
// committed golden. Its **PVs mirror the compiler's registered set** (PV1 =
// `ProcessVersion(1)`, PV999 = `PV_TEST_999`, re-exported from
// `lightbox-render`), and its **per-PV divergence mirrors the compiler's**: PV1
// is the point-op chain `source → gain`, PV999 adds a `blur_r` stage
// (`source → gain → blur`) — a structurally distinct topology producing distinct
// pixels, so the same recipe pins **distinct** goldens under PV1 vs PV999 (task
// D2). The content cache key folds in `pv` (spec §3.5), so the two PVs never
// pollute each other (proven by `no_cross_pv_cache_pollution`).

/// The PV999 matrix blur-stage radius. A change to this (i.e. an algorithm
/// change to the PV999 kernel) shifts every PV999 golden — the D4 trip.
pub const MATRIX_BLUR_RADIUS: f64 = 2.0;

/// The registered process versions the matrix covers (mirrors the compiler's
/// registered `{PV1, PV999}` in these tests).
pub const MATRIX_PVS: [ProcessVersion; 2] = [ProcessVersion(1), PV_TEST_999];

/// How much of the matrix to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatrixScope {
    /// The fast PR-blocking subset (3 sources × 2 recipes × 2 PVs = 12 cases).
    Subset,
    /// The full nightly matrix (all corpus kinds × more recipes × PVs).
    /// DEFERRED-to-nightly (§8) — its goldens are blessed on the nightly runner,
    /// not committed here.
    Full,
}

/// One per-PV matrix case: a corpus source + a recipe (gain, + PV999 blur
/// radius) rendered under one PV and compared to a committed golden.
#[derive(Clone, Debug)]
pub struct MatrixCase {
    /// The synthetic corpus pattern for the source root.
    pub source: CorpusKind,
    /// Stable source name (keys the golden path).
    pub source_name: String,
    /// The `gain` recipe param (the M1 recipe carries no develop fields yet, so
    /// the matrix expresses its recipe set via probe params; maps to real
    /// `Recipe` fields when E09 lands — same as the A16 golden).
    pub gain: f64,
    /// The PV999 blur-stage radius (ignored for PV1, which has no blur stage).
    pub blur_radius: f64,
    /// The process version to render under.
    pub pv: ProcessVersion,
    /// The source/render extent.
    pub extent: Extent,
    /// Path to the committed golden PNG.
    pub golden_path: PathBuf,
}

/// The committed golden path for a matrix case:
/// `goldens/matrix/<source>/gain<ggg>/pv<pv>.png`.
fn matrix_golden_path(source_name: &str, gain: f64, pv: ProcessVersion) -> PathBuf {
    goldens_root()
        .join("matrix")
        .join(source_name)
        .join(format!("gain{:03}", (gain * 100.0).round() as u32))
        .join(format!("pv{}.png", pv.0))
}

/// The corpus sources + recipe set for a matrix scope.
fn matrix_axes(scope: MatrixScope) -> (Vec<(CorpusKind, &'static str)>, Vec<f64>) {
    match scope {
        // Fast subset: three representative pattern families (a smooth gradient,
        // a hard-edged checker, and high-frequency detail — the cases where the
        // PV999 blur stage bites hardest) × two recipes.
        MatrixScope::Subset => (
            vec![
                (CorpusKind::Gradient, "gradient"),
                (CorpusKind::Checker, "checker"),
                (CorpusKind::HighFrequency, "highfreq"),
            ],
            vec![1.0, 2.0],
        ),
        // Full nightly: every corpus family × three recipes.
        MatrixScope::Full => (
            vec![
                (CorpusKind::Gradient, "gradient"),
                (CorpusKind::Checker, "checker"),
                (CorpusKind::LowKey, "lowkey"),
                (CorpusKind::HighKey, "highkey"),
                (CorpusKind::HighFrequency, "highfreq"),
                (CorpusKind::WideGamut, "widegamut"),
                (CorpusKind::Synthetic100Mp, "synth100mp"),
            ],
            vec![1.0, 1.5, 2.0],
        ),
    }
}

/// The matrix golden extent (kept small — like the A16 golden — so the
/// PR-blocking subset is fast; the pattern math is size-independent).
const MATRIX_EXTENT: Extent = Extent { w: 64, h: 64 };

/// Build the matrix case list for `scope` (corpus × recipes × [`MATRIX_PVS`]).
pub fn matrix_cases(scope: MatrixScope) -> Vec<MatrixCase> {
    let (sources, gains) = matrix_axes(scope);
    let mut cases = Vec::new();
    for (kind, name) in sources {
        for &gain in &gains {
            for &pv in &MATRIX_PVS {
                cases.push(MatrixCase {
                    source: kind,
                    source_name: name.to_owned(),
                    gain,
                    blur_radius: MATRIX_BLUR_RADIUS,
                    pv,
                    extent: MATRIX_EXTENT,
                    golden_path: matrix_golden_path(name, gain, pv),
                });
            }
        }
    }
    cases
}

/// Build the per-PV graph for a matrix case: `source → gain` under PV1, and the
/// **divergent** `source → gain → blur` under PV999 (task D2). Unknown PVs fall
/// back to the PV1 (no-blur) topology.
fn build_matrix_graph(case: &MatrixCase) -> RenderGraph {
    let mut graph = RenderGraph::new();
    let src = graph.add_node(Arc::new(CorpusSourceProbe::new(case.source, case.extent)));
    let gain_params =
        ParamBlock::from_fields([("gain", ParamValue::Float(case.gain))]).expect("gain builds");
    let gain = graph.add_node_with_params(Arc::new(GainProbe::default()), gain_params);
    graph
        .connect(src, gain, "in")
        .expect("corpus_src→gain type-checks");

    if case.pv == PV_TEST_999 {
        // Divergent PV999 stage: a radius-`blur_radius` box blur.
        let blur_params = ParamBlock::from_fields([("radius", ParamValue::Float(case.blur_radius))])
            .expect("radius builds");
        let blur = graph.add_node_with_params(Arc::new(BlurRProbe::default()), blur_params);
        graph
            .connect(gain, blur, "in")
            .expect("gain→blur type-checks");
    }
    graph
}

/// Render one matrix case on the CPU reference path and compare to its committed
/// golden within the §4.4 tolerance (task D3).
pub fn run_matrix_case(case: &MatrixCase) -> GoldenReport {
    let graph = build_matrix_graph(case);
    let rendered = render_probe_graph_srgb8(&graph, case.extent, case.pv);
    compare_srgb8_to_golden(&case.golden_path, case.extent.w, case.extent.h, &rendered)
}

/// Run the per-PV golden matrix over `cases` (task D3). **Any** case whose drift
/// exceeds ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB reports `passed == false` — the caller
/// (the PR-blocking test) fails the build on any such case.
pub fn run_matrix(cases: &[MatrixCase]) -> Vec<(MatrixCase, GoldenReport)> {
    cases
        .iter()
        .map(|c| (c.clone(), run_matrix_case(c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::TOLERANCE_DELTA_E;
    use lightbox_render::ng::RecomputeProbe;

    #[test]
    fn corpus_manifest_json_round_trips() {
        let manifest = CorpusManifest {
            version: 1,
            sources: vec![
                CorpusSource {
                    name: "grad-01".to_owned(),
                    kind: CorpusKind::Gradient,
                    width: 256,
                    height: 256,
                    provenance: "own-shot/CC0".to_owned(),
                },
                CorpusSource {
                    name: "checker-01".to_owned(),
                    kind: CorpusKind::Checker,
                    width: 512,
                    height: 512,
                    provenance: "own-shot/CC0".to_owned(),
                },
            ],
        };
        let json = manifest.to_json();
        let parsed = CorpusManifest::from_json(&json).expect("round-trips");
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn parse_gain_reads_the_recipe_fragment() {
        assert_eq!(parse_gain(r#"{"gain":2.0}"#), Some(2.0));
        assert_eq!(parse_gain(r#"{ "gain" : 0.5 , "x": 1 }"#), Some(0.5));
        assert_eq!(parse_gain(r#"{"other":1}"#), None);
    }

    #[test]
    fn reference_render_is_deterministic_across_runs() {
        // The CPU reference path is a pure function of (pattern, gain): two
        // renders are byte-identical (the property the committed golden pins).
        let a = render_checker_gain(GOLDEN_W, GOLDEN_H, 2.0);
        let b = render_checker_gain(GOLDEN_W, GOLDEN_H, 2.0);
        assert_eq!(a, b);
        // Top-left cell is COLOR_A [0.20,0.45,0.70,1.0], gained ×2 →
        // [0.40,0.90,1.40,1.0]: blue over-ranges and clamps to 255, alpha stays
        // opaque, and the lanes keep their ordering r < g < b. (Exact bytes are
        // pinned by the committed golden, not hand-computed here.)
        let p = a[0];
        assert_eq!(p[3], 255, "alpha opaque");
        assert_eq!(p[2], 255, "blue clamps at 1.0");
        assert!(p[0] < p[1] && p[1] < p[2], "lane ordering preserved: {p:?}");
    }

    /// **The §10.1 E05.1 gate (task A16): PR-blocking single-node golden.**
    /// Renders `test.checker → test.gain(×2)` on the CPU reference path and
    /// asserts it matches the committed golden within ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB.
    /// Regenerate the golden with `LIGHTBOX_BLESS=1`.
    #[test]
    fn first_single_node_golden_within_tolerance() {
        let case = first_gain_case();
        let report = run_golden(&case);
        assert!(
            report.stats.within_tolerance(),
            "ΔE2000 max {:.4} exceeds {TOLERANCE_DELTA_E} vs {:?}",
            report.stats.max,
            case.golden_path
        );
        assert!(
            report.psnr_db >= TOLERANCE_PSNR_DB,
            "PSNR {:.2} dB below {TOLERANCE_PSNR_DB} vs {:?}",
            report.psnr_db,
            case.golden_path
        );
        assert!(report.passed);
    }

    /// The per-PV matrix renders deterministically (a pure function of source
    /// pattern + gain + pv-topology + f16 rounding) — the property the committed
    /// matrix goldens pin.
    #[test]
    fn matrix_render_is_deterministic() {
        let cases = matrix_cases(MatrixScope::Subset);
        let one = &cases[0];
        let g = build_matrix_graph(one);
        let a = render_probe_graph_srgb8(&g, one.extent, one.pv);
        let g2 = build_matrix_graph(one);
        let b = render_probe_graph_srgb8(&g2, one.extent, one.pv);
        assert_eq!(a, b);
    }

    /// **D3 per-PV golden matrix (PR-blocking subset): corpus × recipe ×
    /// registered PVs, every case within ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB.** Any
    /// drift on any registered PV fails the build. Regenerate committed goldens
    /// with `LIGHTBOX_BLESS=1` (only for a reviewed change).
    #[test]
    fn per_pv_matrix_subset_within_tolerance() {
        let cases = matrix_cases(MatrixScope::Subset);
        assert_eq!(cases.len(), 12, "3 sources × 2 recipes × 2 PVs");
        let mut failures = Vec::new();
        for (case, report) in run_matrix(&cases) {
            if !report.passed {
                failures.push(format!(
                    "{} gain{} pv{}: ΔE2000 max {:.4} (≤{TOLERANCE_DELTA_E}), PSNR {:.2} dB (≥{TOLERANCE_PSNR_DB}) vs {:?}",
                    case.source_name,
                    case.gain,
                    case.pv.0,
                    report.stats.max,
                    report.psnr_db,
                    case.golden_path,
                ));
            }
        }
        assert!(failures.is_empty(), "matrix drift:\n{}", failures.join("\n"));
    }

    /// **D2: the same recipe pins *distinct* goldens under PV1 vs PV999.** The
    /// divergent PV999 topology (`source → gain → blur`) yields materially
    /// different pixels than PV1 (`source → gain`) on a high-frequency source, so
    /// the two committed goldens differ.
    #[test]
    fn pv1_and_pv999_produce_distinct_goldens() {
        let extent = MATRIX_EXTENT;
        let mk = |pv| MatrixCase {
            source: CorpusKind::HighFrequency,
            source_name: "highfreq".to_owned(),
            gain: 1.0,
            blur_radius: MATRIX_BLUR_RADIUS,
            pv,
            extent,
            golden_path: matrix_golden_path("highfreq", 1.0, pv),
        };
        let pv1 = render_probe_graph_srgb8(
            &build_matrix_graph(&mk(ProcessVersion(1))),
            extent,
            ProcessVersion(1),
        );
        let pv999 = render_probe_graph_srgb8(
            &build_matrix_graph(&mk(PV_TEST_999)),
            extent,
            PV_TEST_999,
        );
        assert_ne!(
            pv1, pv999,
            "PV999's blur stage must make its golden differ from PV1's"
        );
    }

    /// **D2: no cross-PV cache pollution — the content key folds in `pv`
    /// (spec §3.5).** Rendering the *same* graph under PV1 then PV999 through one
    /// shared cache re-evaluates every node for the second PV (no PV1 tile is
    /// served for a PV999 key); re-rendering the first PV hits the cache.
    #[test]
    fn no_cross_pv_cache_pollution() {
        let mut graph = RenderGraph::new();
        let src = graph.add_node(Arc::new(CorpusSourceProbe::new(
            CorpusKind::Gradient,
            MATRIX_EXTENT,
        )));
        let params = ParamBlock::from_fields([("gain", ParamValue::Float(2.0))]).unwrap();
        let gain = graph.add_node_with_params(Arc::new(GainProbe::default()), params);
        graph.connect(src, gain, "in").unwrap();

        let probe = Arc::new(RecomputeProbe::new());
        let exec = Executor::with_probe(Arc::new(CpuBackend::new(None)), 256, Arc::clone(&probe));
        let cache = NodeCache::new();
        let cancel = CancelToken::new();
        let roi = Roi {
            x: 0,
            y: 0,
            w: MATRIX_EXTENT.w,
            h: MATRIX_EXTENT.h,
        };
        let run = |pv| {
            exec.evaluate(&graph, pv, roi, RenderScale::OneToOne, &cache, &cancel, None)
                .expect("render ok")
        };

        run(ProcessVersion(1));
        let after_pv1 = probe.snapshot().nodes_evaluated;
        assert_eq!(after_pv1, 2, "PV1 cold: src + gain evaluated");

        run(ProcessVersion(1));
        assert_eq!(
            probe.snapshot().nodes_evaluated,
            after_pv1,
            "PV1 warm: both nodes cache-hit (0 new evals)"
        );

        run(PV_TEST_999);
        assert_eq!(
            probe.snapshot().nodes_evaluated,
            after_pv1 + 2,
            "PV999: distinct pv keys ⇒ both nodes re-evaluated (no PV1 pollution)"
        );
    }

    /// **D4: an algorithm change to a PV999 kernel trips the golden gate.** We
    /// render a PV999 case with a deliberately different blur radius (5 vs the
    /// committed 2) and assert it no longer matches the committed PV999 golden —
    /// i.e. `run_matrix_case` would fail the build. This is the intentional
    /// kernel tweak from D4 as a *permanent, green* regression guard (the real
    /// trip-then-revert against the committed golden was performed once and
    /// recorded in E05-deviations.md).
    #[test]
    fn pv999_kernel_tweak_trips_the_golden_gate() {
        // The committed PV999 golden for (highfreq, gain 2.0).
        let committed = MatrixCase {
            source: CorpusKind::HighFrequency,
            source_name: "highfreq".to_owned(),
            gain: 2.0,
            blur_radius: MATRIX_BLUR_RADIUS,
            pv: PV_TEST_999,
            extent: MATRIX_EXTENT,
            golden_path: matrix_golden_path("highfreq", 2.0, PV_TEST_999),
        };
        // Sanity: the untweaked case passes its committed golden.
        assert!(
            run_matrix_case(&committed).passed,
            "baseline PV999 case must match its committed golden"
        );

        // A "changed kernel": same case, larger blur radius.
        let tweaked = MatrixCase {
            blur_radius: 5.0,
            ..committed.clone()
        };
        let report = run_matrix_case(&tweaked);
        assert!(
            !report.passed,
            "a PV999 blur-radius change (algorithm change) must trip the golden gate \
             (ΔE2000 max {:.4}, PSNR {:.2} dB)",
            report.stats.max, report.psnr_db
        );
    }

    /// The full nightly matrix is DEFERRED-to-nightly (§8): its goldens are
    /// blessed on the nightly runner, not committed here, so this test is
    /// `#[ignore]`d in the PR gate. It documents the wiring (the case set exists)
    /// and runs green under `LIGHTBOX_BLESS=1` on the nightly runner.
    #[test]
    #[ignore = "full per-PV matrix is nightly (§8); goldens blessed on the nightly runner"]
    fn full_matrix_nightly() {
        let cases = matrix_cases(MatrixScope::Full);
        assert_eq!(cases.len(), 7 * 3 * 2);
        for (case, report) in run_matrix(&cases) {
            assert!(report.passed, "nightly matrix drift on {:?}", case.golden_path);
        }
    }
}
