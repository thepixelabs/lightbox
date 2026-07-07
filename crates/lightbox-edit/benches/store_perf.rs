// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase B perf micro-benches (spec §6 perf table / T7 / T9):
//!
//! - `commit_p95_with_1k_steps` — `EditStore::commit` against a real (temp)
//!   catalog with 1,000 pre-existing history steps for the image (target:
//!   p95 < 5 ms).
//! - `recipe_at_1000_steps` — `history::recipe_at` at the head and the
//!   midpoint of a 1,000-step fixture (target: < 10 ms).

use std::path::Path;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};

use lightbox_catalog::{Catalog, NewAsset};
use lightbox_edit::history::recipe_at;
use lightbox_edit::params::{ParamDelta, ParamId, ParamValue};
use lightbox_edit::{EditSession, EditStore, StepLabel};
use lightbox_types::{ContentHash, Orientation};

fn seeded_store_with_steps(dir: &Path, n: u32) -> (EditStore, lightbox_types::ImageId) {
    let cat = Catalog::create(&dir.join("t.lbdata")).unwrap();
    let image = cat
        .writer()
        .with_txn(|txn| {
            let root = txn.upsert_root(None, Path::new("/photos"))?;
            let folder = txn.upsert_folder(root, None, "shoot")?;
            let outcome = txn.insert_assets(&[NewAsset {
                folder,
                filename: "a.jpg".to_owned(),
                content_hash: ContentHash([3u8; 16]),
                format: "JPEG".to_owned(),
                camera_make: None,
                camera_model: None,
                capture_time: None,
                width: 640,
                height: 480,
                orientation: Orientation::O1,
                bytes: 1024,
                mtime_utc: None,
                decode_error: None,
                import_session: None,
            }])?;
            Ok(txn.insert_default_images(&outcome.inserted)?[0])
        })
        .unwrap();
    let store = EditStore::new(Arc::new(cat));

    // Exposure clamps to [-5.0, 5.0] (recipe.rs `set_scalar!`); walk a ramp
    // strictly inside that range so every iteration is a real net change —
    // starting at `i=0` from the 0.0 neutral default would otherwise be a
    // no-op commit (`take_commit` returns `None` on no net change, spec
    // §3.3), and `.unwrap()`ing that panics instead of seeding a step.
    for i in 0..n {
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Exposure, ParamValue::F32(exposure_value(i)));
        session.update(d).unwrap();
        let commit = session.take_commit().unwrap();
        store.commit(commit).unwrap();
    }
    (store, image)
}

/// Deterministic, monotone, in-range (`[-5.0, 5.0]`) exposure ramp — every
/// `i` yields a value distinct from its predecessor, so a commit loop over
/// `0..n` never hits the "no net change" no-op (see `seeded_store_with_steps`).
fn exposure_value(i: u32) -> f32 {
    -4.0 + (i as f32) * 0.008
}

fn bench_commit_with_1k_steps(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let (store, image) = seeded_store_with_steps(dir.path(), 1000);

    // Ping-pong between two distinct in-range Contrast values so every
    // criterion sample iteration is a real net change vs. the previous one
    // (a constant value would only commit on the very first call).
    let mut high = true;
    c.bench_function("commit_p95_with_1k_steps", |b| {
        b.iter(|| {
            let v = if high { 5.0 } else { -5.0 };
            high = !high;
            let state = store.open_state(image).unwrap();
            let mut session = EditSession::new(image, state);
            session.begin_gesture(StepLabel::Param(ParamId::Contrast));
            let mut d = ParamDelta::new();
            d.0.insert(ParamId::Contrast, ParamValue::F32(v));
            session.update(d).unwrap();
            let commit = session.take_commit().unwrap();
            store.commit(commit).unwrap();
        })
    });
}

fn bench_recipe_at_1000_steps(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let (store, image) = seeded_store_with_steps(dir.path(), 1000);

    c.bench_function("recipe_at_head_1000_steps", |b| {
        b.iter(|| {
            let r = recipe_at(store.catalog(), image, 1000).unwrap();
            assert_eq!(
                r.get(ParamId::Exposure),
                ParamValue::F32(exposure_value(999))
            );
        })
    });
    c.bench_function("recipe_at_mid_1000_steps", |b| {
        b.iter(|| {
            let _ = recipe_at(store.catalog(), image, 500).unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_commit_with_1k_steps,
    bench_recipe_at_1000_steps
);
criterion_main!(benches);
