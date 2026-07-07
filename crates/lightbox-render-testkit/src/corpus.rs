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
use lightbox_render::ng::{
    CpuBackend, Executor, NodeCache, ParamBlock, ParamValue, PixelBuf, RenderGraph, RenderScale,
    Roi,
};
use lightbox_types::ProcessVersion;
use serde::{Deserialize, Serialize};

use crate::compare::{delta_e_stats, psnr, DeltaEStats, TOLERANCE_PSNR_DB};
use crate::probes::{CheckerProbe, GainProbe};

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

    let exec = Executor::new(Arc::new(CpuBackend::new(None)));
    let cache = NodeCache::new();
    let cancel = CancelToken::new();
    let roi = Roi {
        x: 0,
        y: 0,
        w: width,
        h: height,
    };
    let tile = exec
        .evaluate(&graph, roi, RenderScale::OneToOne, &cache, &cancel, None)
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
    let rendered_bytes: Vec<u8> = rendered.iter().flatten().copied().collect();

    let golden_bytes =
        golden_bytes_or_bless(&case.golden_path, GOLDEN_W, GOLDEN_H, &rendered_bytes);
    let golden_texels: Vec<[u8; 4]> = golden_bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();

    let stats = delta_e_stats(&golden_texels, &rendered);
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

/// Run the per-PV golden matrix (corpus × recipes × PVs; spec §6; task D3). The
/// PR-blocking subset runs `run_golden` over each case; **D** extends the
/// case→graph mapping and adds the full nightly matrix.
pub fn run_matrix(cases: &[GoldenCase]) -> Vec<GoldenReport> {
    cases.iter().map(run_golden).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::TOLERANCE_DELTA_E;

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
}
