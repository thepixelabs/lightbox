// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `display.transform` golden-image + CPU/GPU parity tests (E01 spec §5
//! T23/T24 acceptance criteria; §6 golden/parity gates).
//!
//! * **Golden**: engine-rendered output vs committed goldens
//!   (`goldens/display.transform/pv1/<case>.png`), per orientation case
//!   1..=8 (spec minimum: 1, 3, 6, 8) plus native-scale cases, at the §4.4
//!   tolerance (max ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB). Goldens are blessed from
//!   the CPU path (`LIGHTBOX_BLESS=1 cargo test`); CI only verifies.
//! * **Parity**: the GPU path (WGSL compute) vs the CPU path (rayon), same
//!   §4.4 tolerance. GPU legs skip gracefully when no adapter exists
//!   CPU-vs-golden still runs everywhere (spec T24 AC).
//! * **Determinism**: the CPU path is byte-stable across runs (the
//!   `lightbox-cli render --cpu` byte-stability AC rides on this).
//!
//! Everything renders through `Engine::submit`/`poll`, the same ticket
//! lifecycle the shell and CLI use, never the node directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lbx_image_compare::{
    check_golden, compare, diff_heatmap, GoldenConfig, GoldenOutcome, GoldenSpec, Rgba8Image,
    GOLDEN_TOLERANCE,
};
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::nodes::display_transform::{DisplayTransformNode, DisplayTransformPlanner};
use lightbox_render::{
    Engine, GpuContext, ImageBufU8, NodeRegistry, RenderOutput, RenderRequest, RenderScale,
    RenderState, RenderTarget, RenderTicket, Roi, SourceError, SourceImage, SourcePixelFormat,
    SourceResolver, SourceTier, ViewportId,
};
use lightbox_types::{ImageId, Orientation, PV_M0};

const NODE: &str = "display.transform";
const PV: u16 = 1;

fn goldens_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

fn failure_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-failures")
}

/// The synthetic source: deterministic, colorful (saturated regions are
/// where hue errors hurt), asymmetric on both axes so every orientation
/// produces a distinct image. Committed goldens derive from THIS function
/// changing it means re-blessing.
fn test_card(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let p: [u8; 4] = if y < h / 4 {
                // Saturated red→green sweep along the top.
                let t = (x * 255 / (w - 1)) as u8;
                [255 - t, t, 0, 255]
            } else if x < w / 6 {
                [0, 64, 255, 255] // saturated azure left bar
            } else if x >= w - w / 6 {
                [250, 250, 250, 255] // near-white right bar
            } else {
                [
                    (x * 255 / (w - 1)) as u8,
                    (y * 255 / (h - 1)) as u8,
                    ((x + y) * 160 / (w + h - 2)) as u8,
                    255,
                ]
            };
            px.extend_from_slice(&p);
        }
    }
    px
}

const CARD_W: u32 = 64;
const CARD_H: u32 = 40;

/// A resolver serving the test card, unoriented, with a fixed orientation
/// tag, exactly what the embedded-preview resolver hands the planner.
struct CardResolver {
    orientation: Orientation,
}

impl SourceResolver for CardResolver {
    fn resolve(
        &self,
        _image: ImageId,
        _scale: RenderScale,
        cancel: &CancelToken,
    ) -> Result<SourceImage, SourceError> {
        if cancel.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        Ok(SourceImage {
            px: Arc::from(test_card(CARD_W, CARD_H).into_boxed_slice()),
            width: CARD_W,
            height: CARD_H,
            format: SourcePixelFormat::Rgba8Srgb,
            orientation: self.orientation,
            tier: SourceTier::EmbeddedPreview,
        })
    }
}

/// An engine wired the product way: `display.transform` under `PV_M0`,
/// the display-transform planner, a source resolver.
fn engine(gpu: Option<GpuContext>, orientation: Orientation) -> Engine {
    let mut registry = NodeRegistry::new();
    registry.register(PV_M0, Arc::new(DisplayTransformNode::new()));
    let engine = Engine::new(gpu, registry, Arc::new(CardResolver { orientation }))
        .expect("engine construction");
    engine.set_planner(Arc::new(DisplayTransformPlanner));
    engine
}

fn wait_terminal(engine: &Engine, ticket: &RenderTicket) -> RenderState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match engine.poll(ticket) {
            RenderState::Pending | RenderState::Running => {
                assert!(Instant::now() < deadline, "ticket never became terminal");
                std::thread::sleep(Duration::from_millis(1));
            }
            terminal => return terminal,
        }
    }
}

/// Renders one request to a CPU buffer through the full ticket lifecycle.
fn render(engine: &Engine, scale: RenderScale, viewport: u64) -> ImageBufU8 {
    let ticket = engine.submit(RenderRequest {
        image: ImageId(1),
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi::Full,
        scale,
        target: RenderTarget::CpuBuffer,
        viewport: ViewportId(viewport),
    });
    match wait_terminal(engine, &ticket) {
        RenderState::Ready(RenderOutput::Cpu(buf)) => buf,
        other => panic!("expected Ready(Cpu), got {other:?}"),
    }
}

fn as_image(buf: &ImageBufU8) -> Rgba8Image {
    Rgba8Image::new(buf.width, buf.height, buf.px.clone()).expect("render output is valid RGBA")
}

/// The golden case matrix: every orientation (spec minimum 1, 3, 6, 8) at a
/// resampling scale, plus native-scale legs for the identity and a
/// transposing orientation.
fn cases() -> Vec<(String, Orientation, RenderScale)> {
    let mut cases: Vec<(String, Orientation, RenderScale)> = (1..=8u16)
        .map(|o| {
            (
                format!("o{o}-fit32"),
                Orientation::from_exif(o).unwrap(),
                RenderScale::FitWithin { w: 32, h: 32 },
            )
        })
        .collect();
    cases.push(("o1-native".into(), Orientation::O1, RenderScale::Native));
    cases.push(("o6-native".into(), Orientation::O6, RenderScale::Native));
    cases
}

/// Expected output size per case (planner policy: fit-within never
/// upscales; native = oriented size). Pinned here so a planner regression
/// cannot silently re-bless differently-sized goldens.
fn expected_size(orientation: Orientation, scale: RenderScale) -> [u32; 2] {
    let oriented = if orientation.transposes() {
        [CARD_H, CARD_W]
    } else {
        [CARD_W, CARD_H]
    };
    match scale {
        RenderScale::Native => oriented,
        // 64×40 fit in 32×32 → ×0.5 → 32×20 (transposed: 20×32).
        _ => [oriented[0] / 2, oriented[1] / 2],
    }
}

/// T23/T24: CPU-vs-golden everywhere; GPU-vs-golden and GPU-vs-CPU parity
/// where an adapter exists. One test so bless mode writes each golden
/// exactly once (from the CPU reference) before any GPU comparison.
#[test]
fn goldens_and_parity_per_orientation_case() {
    let cfg = GoldenConfig::new(goldens_root(), failure_dir());
    let gpu = GpuContext::headless();
    if gpu.is_none() {
        eprintln!("SKIP gpu legs: no wgpu adapter available (CPU-vs-golden still runs)");
    }

    for (i, (case, orientation, scale)) in cases().into_iter().enumerate() {
        let spec = GoldenSpec {
            node: NODE,
            pv: PV,
            case: &case,
        };

        // CPU render through the engine (runs on every machine).
        let cpu_engine = engine(None, orientation);
        let cpu_buf = render(&cpu_engine, scale, i as u64);
        assert_eq!(
            [cpu_buf.width, cpu_buf.height],
            expected_size(orientation, scale),
            "{case}: output size"
        );
        let cpu_img = as_image(&cpu_buf);

        match check_golden(&cfg, &spec, &cpu_img) {
            Ok(GoldenOutcome::Matched(report)) => {
                eprintln!("golden {case}: {report}");
            }
            Ok(GoldenOutcome::Blessed { path }) => {
                eprintln!("BLESSED {case} -> {}", path.display());
            }
            Err(err) => panic!("golden {case}: {err}"),
        }

        // GPU legs (T23 golden AC + T24 parity AC), skipped without adapter.
        let Some(gpu) = gpu.clone() else { continue };
        let gpu_engine = engine(Some(gpu), orientation);
        let gpu_buf = render(&gpu_engine, scale, i as u64);
        assert_eq!(
            [gpu_buf.width, gpu_buf.height],
            [cpu_buf.width, cpu_buf.height],
            "{case}: GPU output size"
        );
        let gpu_img = as_image(&gpu_buf);

        // GPU vs the committed golden (freshly blessed in bless mode).
        let golden = Rgba8Image::read_png(&spec.path(&cfg.goldens_root))
            .expect("golden readable after CPU check");
        let report = compare(&golden, &gpu_img).expect("same size");
        if !report.passes(GOLDEN_TOLERANCE) {
            let dir = cfg.failure_dir.join(NODE).join(format!("pv{PV}"));
            gpu_img
                .write_png(&dir.join(format!("{case}-gpu-actual.png")))
                .ok();
            diff_heatmap(&golden, &gpu_img)
                .and_then(|m| m.write_png(&dir.join(format!("{case}-gpu-heatmap.png"))))
                .ok();
            panic!("GPU vs golden {case}: {report} exceeds {GOLDEN_TOLERANCE}");
        }
        eprintln!("gpu-golden {case}: {report}");

        // GPU vs CPU parity (T24 AC).
        let parity = compare(&cpu_img, &gpu_img).expect("same size");
        assert!(
            parity.passes(GOLDEN_TOLERANCE),
            "GPU/CPU parity {case}: {parity} exceeds {GOLDEN_TOLERANCE}"
        );
    }
}

/// T24 AC: the CPU path is byte-stable across runs (determinism per §4.4).
#[test]
fn cpu_render_is_byte_stable_across_runs() {
    for orientation in [Orientation::O1, Orientation::O6] {
        let a = render(
            &engine(None, orientation),
            RenderScale::FitWithin { w: 32, h: 32 },
            100,
        );
        let b = render(
            &engine(None, orientation),
            RenderScale::FitWithin { w: 32, h: 32 },
            100,
        );
        assert_eq!(a, b, "CPU render must be byte-stable ({orientation:?})");
    }
}

/// The committed goldens directory stays consistent with the case matrix:
/// no stale goldens linger after a case rename (they would silently stop
/// being verified).
#[test]
fn no_orphaned_goldens_are_committed() {
    let dir = goldens_root().join(NODE).join(format!("pv{PV}"));
    if !dir.is_dir() {
        // Not blessed yet (fresh clone mid-bless), the golden test itself
        // will fail with MissingGolden; nothing to check here.
        return;
    }
    let expected: Vec<String> = cases()
        .into_iter()
        .map(|(case, ..)| format!("{case}.png"))
        .collect();
    for entry in std::fs::read_dir(&dir).expect("read goldens dir") {
        let name = entry.expect("dir entry").file_name();
        let name = name.to_string_lossy().into_owned();
        assert!(
            expected.contains(&name),
            "orphaned golden {name} — remove it or add its case"
        );
    }
}
