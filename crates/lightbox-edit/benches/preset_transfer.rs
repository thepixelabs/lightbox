// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Phase E perf micro-benches (spec §6 perf table / T27 / T28):
//!
//! - `preset_cold_scan_500`, cold-scan of a 500-preset store (target < 100 ms).
//! - `sync_500_targets`, apply a delta to 500 targets, ~64/txn, one step each
//!   (target < 1 s warm) against an in-memory sink (the real-SQLite number is a
//!   Phase-B integration bench, E09-deviations E-5).
//!
//! The nightly perf-**dashboard** wiring is DEFERRED-to-nightly (E09-deviations
//! E-6); this file is the criterion deliverable.

use std::collections::HashMap;
use std::sync::Mutex;

use criterion::{criterion_group, criterion_main, Criterion};

use lightbox_edit::params::{ParamDelta, ParamGroup, ParamId, ParamSubset, ParamValue};
use lightbox_edit::recipe::Recipe;
use lightbox_edit::transfer::{
    sync_to, CommitStep, EditSink, NeverCancel, SyncOptions, TransferError,
};
use lightbox_edit::{PresetStore, StepLabel};
use lightbox_types::{ImageId, PV_M0};

fn edited_recipe() -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(0.75));
    d.0.insert(ParamId::Contrast, ParamValue::F32(12.0));
    d.0.insert(ParamId::Vibrance, ParamValue::F32(8.0));
    r.apply(&d).unwrap();
    r
}

fn bench_cold_scan(c: &mut Criterion) {
    // Author 500 presets on disk once.
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
    let recipe = edited_recipe();
    let subset = ParamSubset::from_groups([ParamGroup::Tone, ParamGroup::Presence]);
    for i in 0..500 {
        store
            .create_from(&recipe, &format!("preset_{i:03}"), None, &subset)
            .unwrap();
    }

    c.bench_function("preset_cold_scan_500", |b| {
        b.iter(|| {
            let s = PresetStore::open(dir.path().to_path_buf()).unwrap();
            assert_eq!(s.list().len(), 500);
        })
    });
}

#[derive(Default)]
struct MemSink {
    state: Mutex<HashMap<ImageId, Recipe>>,
}
impl EditSink for MemSink {
    fn recipe_of(&self, image: ImageId) -> Result<Recipe, TransferError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .get(&image)
            .cloned()
            .unwrap_or_else(|| Recipe::identity(PV_M0)))
    }
    fn commit_batch(&self, batch: &[CommitStep]) -> Result<(), TransferError> {
        let mut state = self.state.lock().unwrap();
        for s in batch {
            state.insert(s.image, s.new_recipe.clone());
        }
        Ok(())
    }
}

fn bench_sync_500(c: &mut Criterion) {
    let targets: Vec<ImageId> = (0..500).map(ImageId).collect();
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::Exposure, ParamValue::F32(1.0));

    c.bench_function("sync_500_targets", |b| {
        b.iter(|| {
            let sink = MemSink::default();
            let report = sync_to(
                &sink,
                &delta,
                &targets,
                StepLabel::Sync,
                &NeverCancel,
                SyncOptions::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(report.committed, 500);
        })
    });
}

criterion_group!(benches, bench_cold_scan, bench_sync_500);
criterion_main!(benches);
