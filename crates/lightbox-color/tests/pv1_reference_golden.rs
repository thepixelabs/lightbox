// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E02 Phase H (H5): the **per-process-version** golden gate for the CPU
//! reference input-transform pipeline.
//!
//! This commits a `pv=1` reference image, a ColorChecker-style patch grid
//! rendered `camera-native linear RGB → camera_matrix_base → resolve_input_
//! transform → display-sRGB` (spec §5.2), and gates it PR-blocking at the
//! §4.4 tolerance (**max ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB**,
//! [`lbx_image_compare::GOLDEN_TOLERANCE`]). It uses only the public
//! `lightbox-color` / `lightbox-decode` surface, no proxy, no bundled assets,
//! deterministic across platforms, so it runs in every CI build (the raw
//! corpus render-ref, which needs the LibRaw proxy, is the libraw-gated
//! complement; see E02-deviations.md).
//!
//! # PV-immutability guard (spec §4.5 / DoD 8)
//!
//! The `pv1` golden is **frozen** once M1 ships. The §5.2 stage order is part of
//! the PV1 contract: any change to it (or to matrix/WB/adaptation math that
//! moves output) is a **new process version**, it must land under a new
//! `goldens/reference/pv2/…` set with its own bless + review, never by
//! re-blessing these `pv1` bytes. `LIGHTBOX_BLESS` is refused in CI so a PR can
//! never quietly rewrite the reference; a local re-bless is only legitimate for
//! a deliberate, reviewed pv1 fix before the freeze.

use std::path::{Path, PathBuf};

use lbx_image_compare::{check_golden, GoldenConfig, GoldenOutcome, GoldenSpec, Rgba8Image};
use lightbox_color::matrix::spaces;
use lightbox_color::{camera_matrix_base, resolve_input_transform, WbMode};
use lightbox_decode::{normalize_camera, Illuminant, RawColorimetry};

/// The reference process version this golden is keyed to (frozen at M1).
const PV1: u16 = 1;

/// A ColorChecker-flavoured neutral→saturated patch set (sRGB 8-bit), the same
/// family the B7 first-failing test round-trips.
const PATCHES: [[u8; 3]; 8] = [
    [128, 128, 128], // neutral gray
    [242, 242, 242], // near-white
    [30, 30, 30],    // near-black
    [200, 60, 55],   // red
    [70, 150, 80],   // green
    [55, 85, 175],   // blue
    [196, 150, 130], // skin
    [110, 145, 190], // sky
];

/// A synthetic **linear-sRGB camera** built from public colorimetry fields
/// alone: `color_matrix1` = XYZ(D65)→sRGB, no forward matrix (so the matrix base
/// exercises the inverse-CM + Bradford→D50 path), D65 as-shot neutral. Because
/// the camera is linear sRGB, a colorimetrically-correct pipeline recovers each
/// sRGB patch, any error in the matrix base, WB solve, working space, or
/// Bradford adaptation moves the output and blows the ΔE gate.
fn srgb_camera() -> RawColorimetry {
    // XYZ(D65) → linear sRGB (Bradford-consistent sRGB primaries, D65 white).
    const XYZ_D65_TO_SRGB: [[f64; 3]; 3] = [
        [3.240_454_2, -1.537_138_5, -0.498_531_4],
        [-0.969_266_0, 1.876_010_8, 0.041_556_0],
        [0.055_643_4, -0.204_025_9, 1.057_225_2],
    ];
    RawColorimetry {
        // D65 white through XYZ→camera normalizes to unit gains for an sRGB cam.
        as_shot_neutral: Some([1.0, 1.0, 1.0]),
        illuminant1: Illuminant::D65,
        illuminant2: None,
        color_matrix1: XYZ_D65_TO_SRGB,
        color_matrix2: None,
        forward_matrix1: None,
        forward_matrix2: None,
        analog_balance: None,
        baseline_exposure: 0.0,
    }
}

/// Renders the patch set as a 4×2 grid of 40×40 tiles (160×80 RGBA8).
fn render_reference_grid() -> Rgba8Image {
    let raw = srgb_camera();
    let camera = normalize_camera("Synthetic", "sRGB-Cam");
    let profile = camera_matrix_base(&raw, &camera).expect("matrix base");
    // Pure matrix base: no DCP, no default look (the look golden is E7's).
    let transform =
        resolve_input_transform(&profile, &WbMode::AsShot, raw.as_shot_neutral, None, 1.0)
            .expect("resolve input transform");

    const TILE: u32 = 40;
    const COLS: u32 = 4;
    const ROWS: u32 = 2;
    let (w, h) = (COLS * TILE, ROWS * TILE);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for (i, patch) in PATCHES.iter().enumerate() {
        let cam = [
            spaces::srgb_eotf(patch[0] as f32 / 255.0),
            spaces::srgb_eotf(patch[1] as f32 / 255.0),
            spaces::srgb_eotf(patch[2] as f32 / 255.0),
        ];
        let [r, g, b] = transform.render_reference_srgb8(cam);
        let (tx, ty) = ((i as u32 % COLS) * TILE, (i as u32 / COLS) * TILE);
        for y in ty..ty + TILE {
            for x in tx..tx + TILE {
                let o = ((y * w + x) * 4) as usize;
                px[o..o + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    Rgba8Image::new(w, h, px).expect("grid image")
}

#[test]
fn pv1_matrix_base_reference_golden() {
    let cfg = GoldenConfig::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens"),
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-failures"),
    );
    let spec = GoldenSpec {
        node: "reference",
        pv: PV1,
        case: "matrix_base_srgb",
    };
    match check_golden(&cfg, &spec, &render_reference_grid()) {
        Ok(GoldenOutcome::Matched(report)) => {
            eprintln!("reference/pv1/matrix_base_srgb golden: {report}");
        }
        Ok(GoldenOutcome::Blessed { path }) => {
            eprintln!("blessed {}", path.display());
        }
        Err(e) => panic!("PV1 reference golden gate failed: {e}"),
    }
}
