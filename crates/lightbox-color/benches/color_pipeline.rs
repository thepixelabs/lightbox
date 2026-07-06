// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E02 Phase H (H3): color-pipeline perf benches against the §7.5 budgets.
//!
//! | bench | §7.5 budget | ties to |
//! |-------|-------------|---------|
//! | `resolve_input_transform` | ≤ 1 ms | WB slider inside the < 100 ms budget |
//! | `eval_cpu` | (per-pixel hot path) | the CPU reference / golden parity anchor |
//! | `build_display_transform` | ≤ 100 ms (first time, then cached) | develop-open |
//!
//! Non-blocking (nightly): criterion records baselines; regressions file a
//! tracked issue (spec §8). Run: `cargo bench -p lightbox-color`.

use criterion::{criterion_group, criterion_main, Criterion};
use lightbox_color::cms::IccProfile;
use lightbox_color::display::{build_display_transform, Intent};
use lightbox_color::{camera_matrix_base, resolve_input_transform, CameraProfile, WbMode};
use lightbox_decode::{normalize_camera, Illuminant, RawColorimetry};
use std::hint::black_box;

/// A synthetic linear-sRGB camera (public colorimetry fields only), the same
/// one the PV1 reference golden uses.
fn srgb_matrix_base() -> CameraProfile {
    const XYZ_D65_TO_SRGB: [[f64; 3]; 3] = [
        [3.240_454_2, -1.537_138_5, -0.498_531_4],
        [-0.969_266_0, 1.876_010_8, 0.041_556_0],
        [0.055_643_4, -0.204_025_9, 1.057_225_2],
    ];
    let raw = RawColorimetry {
        as_shot_neutral: Some([1.0, 1.0, 1.0]),
        illuminant1: Illuminant::D65,
        illuminant2: None,
        color_matrix1: XYZ_D65_TO_SRGB,
        color_matrix2: None,
        forward_matrix1: None,
        forward_matrix2: None,
        analog_balance: None,
        baseline_exposure: 0.0,
    };
    camera_matrix_base(&raw, &normalize_camera("Synthetic", "sRGB-Cam")).expect("matrix base")
}

fn bench_resolve(c: &mut Criterion) {
    let profile = srgb_matrix_base();
    let as_shot = Some([1.0, 1.0, 1.0]);
    c.bench_function("resolve_input_transform/matrix_base_asshot", |b| {
        b.iter(|| {
            resolve_input_transform(
                black_box(&profile),
                black_box(&WbMode::AsShot),
                black_box(as_shot),
                None,
                1.0,
            )
            .unwrap()
        })
    });

    // A WB temp/tint change is the E10 slider path (§5.4): re-resolve only.
    c.bench_function("resolve_input_transform/temp_tint", |b| {
        b.iter(|| {
            resolve_input_transform(
                black_box(&profile),
                black_box(&WbMode::TempTint {
                    kelvin: 5200.0,
                    tint: 4.0,
                }),
                black_box(as_shot),
                None,
                1.0,
            )
            .unwrap()
        })
    });
}

fn bench_eval_cpu(c: &mut Criterion) {
    let profile = srgb_matrix_base();
    let t = resolve_input_transform(&profile, &WbMode::AsShot, Some([1.0, 1.0, 1.0]), None, 1.0)
        .unwrap();
    c.bench_function("eval_cpu/per_pixel", |b| {
        b.iter(|| black_box(t.eval_cpu(black_box([0.18, 0.20, 0.22]))))
    });
}

fn bench_display_bake(c: &mut Criterion) {
    let srgb = IccProfile::srgb();
    let mut group = c.benchmark_group("display");
    group.sample_size(20); // the 65³ bake is heavy; fewer samples suffice.
    group.bench_function("build_display_transform/srgb", |b| {
        b.iter(|| build_display_transform(black_box(&srgb), Intent::RelColorimetric).unwrap())
    });
    group.finish();
}

criterion_group!(benches, bench_resolve, bench_eval_cpu, bench_display_bake);
criterion_main!(benches);
