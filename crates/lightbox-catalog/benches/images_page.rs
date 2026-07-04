// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T10 100 k proof: `images_page` p50/p95 at 100 000 synthetic assets must
//! sit well under the 10 ms AC on a dev laptop (informal at PR; the formal
//! nightly scenario harness is T28). Also benches batched insert throughput.
//!
//! Run: `cargo bench -p lightbox-catalog`

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use lightbox_catalog::{Catalog, ImageQuery, NewAsset, PageCursor, SortOrder};
use lightbox_types::{ContentHash, FolderId, Orientation};

const TOTAL: u64 = 100_000;
const BATCH: u64 = 1_000;

fn synthetic_catalog(dir: &std::path::Path) -> (Catalog, FolderId) {
    let lbdata = dir.join("bench.lbdata");
    let catalog = Catalog::create(&lbdata).expect("create");
    let root_path = dir.join("photos");
    let folder = catalog
        .writer()
        .with_txn(move |txn| {
            let root = txn.upsert_root(None, &root_path)?;
            txn.upsert_folder(root, None, "synthetic")
        })
        .expect("folder");

    // 100 k synthetic assets: ~10% NULL capture times, duplicate-heavy
    // capture buckets (1 s granularity over ~28 h), one image each.
    for base in (0..TOTAL).step_by(BATCH as usize) {
        catalog
            .writer()
            .with_txn(move |txn| {
                let batch: Vec<NewAsset> = (base..base + BATCH).map(|i| synth(folder, i)).collect();
                let outcome = txn.insert_assets(&batch)?;
                txn.insert_default_images(&outcome.inserted)?;
                Ok(())
            })
            .expect("insert batch");
    }
    (catalog, folder)
}

fn synth(folder: FolderId, i: u64) -> NewAsset {
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&i.to_le_bytes());
    hash[8] = 0xB3;
    let capture_time = (!i.is_multiple_of(10)).then(|| {
        let secs = i % 100_000;
        format!(
            "2026-06-{:02}T{:02}:{:02}:{:02}.000000Z",
            1 + secs / 86_400,
            (secs / 3_600) % 24,
            (secs / 60) % 60,
            secs % 60
        )
    });
    NewAsset {
        folder,
        filename: format!("IMG_{i:06}.CR3"),
        content_hash: ContentHash(hash),
        format: "CR3".to_owned(),
        camera_make: Some("Canon".to_owned()),
        camera_model: Some("EOS R5".to_owned()),
        capture_time,
        width: 8192,
        height: 5464,
        orientation: Orientation::O1,
        bytes: 45_000_000,
        mtime_utc: None,
        decode_error: None,
        import_session: None,
    }
}

/// Walks to the middle of the result set to get a deep, realistic cursor.
fn mid_cursor(catalog: &Catalog, sort: SortOrder) -> Option<PageCursor> {
    let reader = catalog.reader();
    let mut cursor = None;
    for _ in 0..(TOTAL / 2 / 1000) {
        let page = reader
            .images_page(&ImageQuery {
                folder: None,
                sort,
                cursor,
                limit: 1000,
            })
            .expect("page");
        cursor = page.next;
        cursor.as_ref()?;
    }
    cursor
}

fn bench_images_page(c: &mut Criterion) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (catalog, folder) = synthetic_catalog(dir.path());

    let mut group = c.benchmark_group("images_page_100k");
    group.measurement_time(Duration::from_secs(10));
    for (name, sort) in [
        ("capture_asc", SortOrder::CaptureTimeAsc),
        ("capture_desc", SortOrder::CaptureTimeDesc),
        ("added_asc", SortOrder::AddedAsc),
        ("filename_asc", SortOrder::FilenameAsc),
    ] {
        let deep = mid_cursor(&catalog, sort);
        group.bench_function(format!("{name}/first_page_200"), |b| {
            let reader = catalog.reader();
            b.iter(|| {
                let page = reader
                    .images_page(&ImageQuery {
                        folder: None,
                        sort,
                        cursor: None,
                        limit: 200,
                    })
                    .expect("page");
                assert_eq!(page.items.len(), 200);
                page
            })
        });
        group.bench_function(format!("{name}/deep_page_200"), |b| {
            let reader = catalog.reader();
            b.iter(|| {
                let page = reader
                    .images_page(&ImageQuery {
                        folder: None,
                        sort,
                        cursor: deep.clone(),
                        limit: 200,
                    })
                    .expect("page");
                assert_eq!(page.items.len(), 200);
                page
            })
        });
    }
    group.bench_function("folder_filtered/first_page_200", |b| {
        let reader = catalog.reader();
        b.iter(|| {
            reader
                .images_page(&ImageQuery {
                    folder: Some(folder),
                    sort: SortOrder::CaptureTimeDesc,
                    cursor: None,
                    limit: 200,
                })
                .expect("page")
        })
    });
    group.finish();

    let mut group = c.benchmark_group("insert_batch");
    group.sample_size(20);
    let mut seed = 10_000_000u64;
    group.bench_function("insert_assets_64_with_images", |b| {
        b.iter(|| {
            let base = seed;
            seed += 64;
            catalog
                .writer()
                .with_txn(move |txn| {
                    let batch: Vec<NewAsset> =
                        (base..base + 64).map(|i| synth(folder, i)).collect();
                    let outcome = txn.insert_assets(&batch)?;
                    txn.insert_default_images(&outcome.inserted)?;
                    Ok(())
                })
                .expect("insert");
        })
    });
    group.finish();
}

criterion_group!(benches, bench_images_page);
criterion_main!(benches);
