// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E02 Phase H (H3): decode-pipeline perf benches against the §7.5 budgets.
//!
//! | bench | §7.5 budget | ties to |
//! |-------|-------------|---------|
//! | `probe` | ≤ 5 ms p50 | E04 import throughput |
//! | `linearize` | (raw develop throughput) | proxy decode + linearize ≤ 500/900 ms |
//!
//! `probe` runs against real fixture files when the corpus is present
//! (`cargo xtask fixtures`); it self-skips with a note otherwise so the bench
//! never fails a machine without the corpus. `linearize` uses a synthetic
//! in-memory mosaic (no corpus dependency). Non-blocking (nightly).

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use lightbox_decode::{
    linearize, probe, BlackLevels, CfaColor, CfaPattern, MosaicBuffer, MosaicImage, RawColorimetry,
    Rect,
};
use lightbox_types::Orientation;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn bench_probe(c: &mut Criterion) {
    let dir = fixtures_dir();
    let cases = [
        ("cr3", "canon-eos-r6.cr3"),
        ("nef", "nikon-z6.nef"),
        ("jpeg", "lightbox-tiny.jpg"),
    ];
    let mut any = false;
    let mut group = c.benchmark_group("probe");
    for (label, name) in cases {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        any = true;
        group.bench_function(label, |b| b.iter(|| probe(black_box(&path)).unwrap()));
    }
    group.finish();
    if !any {
        eprintln!("probe bench skipped: no fixture corpus (run `cargo xtask fixtures`)");
    }
}

/// A synthetic RGGB mosaic of `side × side` 12-bit samples.
fn synthetic_mosaic(side: u32) -> MosaicImage {
    let n = (side * side) as usize;
    let samples: Vec<u16> = (0..n).map(|i| ((i * 37) % 4096) as u16).collect();
    MosaicImage {
        data: MosaicBuffer { samples },
        width: side,
        height: side,
        cfa: CfaPattern::Bayer([CfaColor::R, CfaColor::G, CfaColor::G, CfaColor::B]),
        active_area: Rect::full(side, side),
        default_crop: Rect::full(side, side),
        black_levels: BlackLevels { levels: [64; 4] },
        white_levels: [4095; 4],
        linearization: None,
        colorimetry: RawColorimetry::default(),
        orientation: Orientation::O1,
    }
}

fn bench_linearize(c: &mut Criterion) {
    let mut group = c.benchmark_group("linearize");
    for side in [1024u32, 2048] {
        let m = synthetic_mosaic(side);
        group.throughput(Throughput::Elements((side * side) as u64));
        group.bench_function(format!("{side}x{side}"), |b| {
            b.iter(|| black_box(linearize(black_box(&m))))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_probe, bench_linearize);
criterion_main!(benches);
