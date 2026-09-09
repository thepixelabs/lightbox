// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E7, the look golden gate (PR-blocking).
//!
//! The synthetic scene corpus rendered through the **committed**
//! `lightbox-color-v1.lblook` at authored strength is stitched into one contact
//! image and compared **exactly** against a committed golden PNG. Exact (not
//! ΔE-tolerant) comparison is deliberate: the E7 acceptance criterion is that a
//! 1-LSB curve perturbation fails the gate, which a perceptual ΔE≤1 tolerance
//! (designed to pass ±1 LSB) would not catch. The `one_lsb_curve_perturbation_*`
//! test is the permanent, same-process proof of that sensitivity.
//!
//! Bless: `LIGHTBOX_BLESS=1 cargo test -p lightbox-color` regenerates the golden
//! locally (refused in CI); the regenerated PNG is human-reviewable in the PR.
//!
//! Cross-platform note (E02-deviations.md): the render is a pure-CPU `f32` path;
//! exact-byte equality assumes deterministic `f32`/`powf` across the CI matrix.
//! If a future 3-OS run shows libm ULP drift, switch the golden compare to a
//! ≤1-LSB bound, the perturbation test keeps the 1-LSB sensitivity guarantee
//! regardless.

use std::path::{Path, PathBuf};

use lbx_image_compare::{bless_requested, Rgba8Image};
use lightbox_color::look::{
    author_lightbox_color_v1, load_look, render_contact_sheet, Look, TILE_H, TILE_W,
};

const GRID_COLS: u32 = 8;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolves")
}

fn golden_path() -> PathBuf {
    // Same <node>/pv<N>/<case>.png shape the render goldens use, under this crate.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("goldens")
        .join("look")
        .join("pv1")
        .join("lightbox-color-v1.png")
}

fn committed_look() -> Look {
    let path = repo_root()
        .join("assets")
        .join("color")
        .join("looks")
        .join("lightbox-color-v1.lblook");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing {} ({e}): run `LIGHTBOX_BLESS=1 cargo test -p lightbox-color` and commit",
            path.display()
        )
    });
    load_look(&bytes).expect("committed look loads")
}

/// Stitches the corpus's look-cells into one contact image (row-major grid).
fn render_grid(look: &Look) -> Rgba8Image {
    let tiles = render_contact_sheet(look, 1.0);
    let n = tiles.len() as u32;
    let rows = n.div_ceil(GRID_COLS);
    let w = GRID_COLS * TILE_W;
    let h = rows * TILE_H;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for (i, t) in tiles.iter().enumerate() {
        let cx = (i as u32 % GRID_COLS) * TILE_W;
        let cy = (i as u32 / GRID_COLS) * TILE_H;
        for ty in 0..TILE_H {
            for tx in 0..TILE_W {
                let src = ((ty * TILE_W + tx) * 4) as usize;
                let dx = cx + tx;
                let dy = cy + ty;
                let dst = ((dy * w + dx) * 4) as usize;
                px[dst..dst + 4].copy_from_slice(&t.look_rgba8[src..src + 4]);
            }
        }
    }
    Rgba8Image::new(w, h, px).expect("grid dims valid")
}

/// Count of differing bytes (diagnostics on mismatch).
fn diff_count(a: &Rgba8Image, b: &Rgba8Image) -> usize {
    a.px.iter().zip(b.px.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn look_golden_matches_committed() {
    let actual = render_grid(&committed_look());
    let path = golden_path();

    if bless_requested() {
        actual.write_png(&path).expect("write golden");
        return;
    }

    let golden = Rgba8Image::read_png(&path).unwrap_or_else(|e| {
        panic!(
            "missing/broken golden {} ({e}): run `LIGHTBOX_BLESS=1 cargo test -p lightbox-color` \
             locally, eyeball the new golden, and commit",
            path.display()
        )
    });
    assert_eq!(
        (golden.width, golden.height),
        (actual.width, actual.height),
        "golden dimensions drifted"
    );
    assert!(
        golden == actual,
        "look golden mismatch ({} bytes differ) — a real look change must be re-blessed and \
         reviewed",
        diff_count(&golden, &actual)
    );
}

/// The committed asset and the code-authored look render identically (the asset
/// is exactly the authored look, ties code ↔ shipped bytes).
#[test]
fn committed_asset_renders_like_the_authored_look() {
    if bless_requested() {
        return; // nothing to compare mid-bless
    }
    let from_asset = render_grid(&committed_look());
    let from_code = render_grid(&author_lightbox_color_v1());
    assert!(
        from_asset == from_code,
        "committed .lblook and author_lightbox_color_v1() diverged ({} bytes)",
        diff_count(&from_asset, &from_code)
    );
}

/// E7 sensitivity proof (verified permanently, not by committing a broken
/// state): a 1-LSB (1/255) perturbation of a single tone-curve control point
/// changes the rendered corpus, so the exact golden gate would fail on it.
#[test]
fn one_lsb_curve_perturbation_breaks_the_golden() {
    if bless_requested() {
        return; // the golden is being (re)written; skip the sensitivity check
    }
    let golden = Rgba8Image::read_png(&golden_path()).expect("golden present");

    let mut perturbed = author_lightbox_color_v1();
    // Nudge the mid control point's y by exactly one 8-bit LSB.
    let mid = perturbed.tone_curve.control_points.len() / 2;
    perturbed.tone_curve.control_points[mid][1] += 1.0 / 255.0;

    let actual = render_grid(&perturbed);
    assert_ne!(actual.width * actual.height, 0, "sanity: non-empty render");
    assert!(
        golden != actual,
        "a 1-LSB curve perturbation produced a bit-identical render — the golden gate is not \
         sensitive enough"
    );
}
