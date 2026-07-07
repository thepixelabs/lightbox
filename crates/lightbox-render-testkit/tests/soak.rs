// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! **C10 — interactive soak** (reduced iteration count in-gate; the 10 k count
//! is the nightly job — see `E05-deviations.md`, C10). Randomized
//! param/zoom/pan frames over the CPU tile executor must show zero render
//! errors, a bounded (tile-sized) working set regardless of zoom, and a final
//! frame that reproduces exactly on a fresh render (no cross-iteration state
//! corruption).

use lightbox_render_testkit::scenario::{run_soak, SoakConfig};

#[test]
fn soak_is_clean_and_deterministic() {
    // Reduced in-gate count (fast, deterministic); the 10 k soak is the nightly
    // job (E05-deviations.md, C10).
    let iterations = 250;
    let report = run_soak(&SoakConfig {
        iterations,
        seed: 0x00C0_FFEE_1234,
    });

    assert_eq!(report.iterations, iterations);
    assert_eq!(report.validation_errors, 0, "soak produced render errors");
    assert!(report.final_matches_fresh, "final frame != fresh render");
    // The tiling working set stays bounded to ~one 256² tile + apron regardless
    // of the randomized zoom levels — VRAM stays in budget throughout.
    assert!(
        report.peak_tile_pixels <= (256 + 8) * (256 + 8),
        "peak per-eval working set {} px is unbounded",
        report.peak_tile_pixels
    );
    eprintln!(
        "[C10] soak clean: {} iterations, peak per-eval working set {} px",
        report.iterations, report.peak_tile_pixels
    );
}

/// The full 10k-iteration soak (spec §8/C10) — `#[ignore]`d so `cargo test`
/// stays fast in-gate; the nightly workflow runs it explicitly
/// (`.github/workflows/nightly.yml`, "Full 10k-iteration interactive soak").
#[test]
#[ignore = "10k iterations — nightly job, not the PR-blocking gate (E05-deviations.md, C10)"]
fn full_10k_soak_is_clean_and_deterministic() {
    let iterations = 10_000;
    let report = run_soak(&SoakConfig {
        iterations,
        seed: 0x00C0_FFEE_1234,
    });

    assert_eq!(report.iterations, iterations);
    assert_eq!(report.validation_errors, 0, "soak produced render errors");
    assert!(report.final_matches_fresh, "final frame != fresh render");
    assert!(
        report.peak_tile_pixels <= (256 + 8) * (256 + 8),
        "peak per-eval working set {} px is unbounded",
        report.peak_tile_pixels
    );
    eprintln!(
        "[C10-full] soak clean: {} iterations, peak per-eval working set {} px",
        report.iterations, report.peak_tile_pixels
    );
}
