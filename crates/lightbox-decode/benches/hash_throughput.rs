// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T28 micro-bench: streaming xxh3-128 file-hash throughput (`hash_file`,
//! spec §3.7 — 1 MiB chunks, cancel-checked between chunks). The M0 import
//! budget rides on this: 1 k raws ≈ 30 GB hashed at import (§7 R5).
//!
//! Run: `cargo bench -p lightbox-decode`

use std::io::Write;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use lightbox_decode::hash_file;
use lightbox_jobs::CancelToken;

/// Writes `bytes` of deterministic pseudo-random data (seeded — identical
/// across runs, incompressible enough to defeat page-cache-adjacent tricks).
fn synth_file(dir: &std::path::Path, name: &str, bytes: usize) -> PathBuf {
    let path = dir.join(name);
    let mut rng = fastrand::Rng::with_seed(0xB10C_B0C5 ^ bytes as u64);
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("create"));
    let mut chunk = vec![0u8; 1 << 20];
    let mut left = bytes;
    while left > 0 {
        let n = left.min(chunk.len());
        rng.fill(&mut chunk[..n]);
        file.write_all(&chunk[..n]).expect("write");
        left -= n;
    }
    file.flush().expect("flush");
    path
}

fn bench_hash_throughput(c: &mut Criterion) {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let mut group = c.benchmark_group("hash_file");
    // (label, size) — a typical 45 MB raw and an 8 MiB JPEG-sized file.
    // Warm-cache numbers by design: the first iteration faults the file in,
    // criterion's remaining samples measure hash + read-from-cache, i.e. the
    // CPU-side ceiling (cold-media throughput is the drive's business, and
    // the T29 drill measures the end-to-end import on real hardware).
    for (label, size) in [("raw_45mib", 45 << 20), ("jpeg_8mib", 8 << 20)] {
        let path = synth_file(dir.path(), label, size);
        group.throughput(Throughput::Bytes(size as u64));
        group.sample_size(20);
        group.bench_function(label, |b| {
            let cancel = CancelToken::new();
            b.iter(|| hash_file(&path, &cancel).expect("hash"))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hash_throughput);
criterion_main!(benches);
