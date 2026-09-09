// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Phase E end-to-end: presets (T23/T24/T26) + settings transfer (T25/T27) at the
//! `lightbox-edit` layer, the whole Phase-E logic exercised against the on-disk
//! preset store and the in-memory [`EditSink`] Phase-B stand-in.
//!
//! The **catalog-backed** legs, the `Command::Edit` dispatcher arms, the
//! `lightbox-cli` scenario, the kill-9 leg, and the SQLite criterion numbers, are
//! DEFERRED to Phase B (which owns `EditStore`/`EditHub`/the command bus); see
//! `docs/plan/epics/E09-deviations.md` E-5.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use proptest::prelude::*;

use lightbox_edit::params::{ParamDelta, ParamGroup, ParamId, ParamSubset, ParamValue};
use lightbox_edit::transfer::{
    sync_to, CommitStep, EditSink, NeverCancel, SyncOptions, TransferError,
};
use lightbox_edit::{
    preview_recipe, DevelopPreset, PresetOrigin, PresetStore, Recipe, StepLabel, Treatment,
};
use lightbox_types::{ImageId, PV_M0};

const ALL_GROUPS: [ParamGroup; 14] = [
    ParamGroup::BaseProfile,
    ParamGroup::WhiteBalance,
    ParamGroup::Tone,
    ParamGroup::Curve,
    ParamGroup::ColorMixer,
    ParamGroup::ColorGrading,
    ParamGroup::BwMix,
    ParamGroup::Presence,
    ParamGroup::Detail,
    ParamGroup::Optics,
    ParamGroup::Geometry,
    ParamGroup::Effects,
    ParamGroup::Masks,
    ParamGroup::Retouch,
];

/// A recipe non-neutral across several groups, built with only scalar/bool/enum
/// param values (no leaf-struct literals needed from an integration test).
fn rich_recipe() -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(1.0)); // Tone
    d.0.insert(ParamId::Contrast, ParamValue::F32(20.0)); // Tone
    d.0.insert(ParamId::Vibrance, ParamValue::F32(15.0)); // Presence
    d.0.insert(ParamId::Clarity, ParamValue::F32(10.0)); // Presence
    d.0.insert(ParamId::Angle, ParamValue::F32(3.0)); // Geometry
    d.0.insert(ParamId::Defringe, ParamValue::F32(5.0)); // Optics
    d.0.insert(ParamId::ChromaticAberration, ParamValue::Bool(true)); // Optics
    d.0.insert(
        ParamId::Treatment,
        ParamValue::Treatment(Treatment::BlackAndWhite),
    ); // BwMix
    r.apply(&d).unwrap();
    r
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// ── T23: cold scan, refresh, quarantine ───────────────────────────────────────

#[test]
fn refresh_picks_up_externally_dropped_preset() {
    let dir = tempfile::tempdir().unwrap();
    let a = PresetStore::open(dir.path().to_path_buf()).unwrap();
    let subset = ParamSubset::from_groups([ParamGroup::Tone]);
    a.create_from(&rich_recipe(), "First", None, &subset)
        .unwrap();
    assert_eq!(a.list().len(), 1);

    // A second store drops a valid preset into the same dir behind A's back.
    let b = PresetStore::open(dir.path().to_path_buf()).unwrap();
    b.create_from(&rich_recipe(), "Second", Some("Looks"), &subset)
        .unwrap();

    // A doesn't see it until refresh (no watcher, §0.6).
    assert_eq!(a.list().len(), 1);
    a.refresh().unwrap();
    assert_eq!(a.list().len(), 2);
    // group-as-folder: the grouped one reports its folder.
    let grouped = a.list().into_iter().find(|m| m.name == "Second").unwrap();
    assert_eq!(grouped.group.as_deref(), Some("Looks"));
}

#[test]
fn malformed_file_is_quarantined_store_stays_up() {
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
    store
        .create_from(
            &rich_recipe(),
            "Good",
            None,
            &ParamSubset::from_groups([ParamGroup::Tone]),
        )
        .unwrap();
    // Drop a garbage .xmp.
    std::fs::write(dir.path().join("broken.xmp"), b"<<<not xml").unwrap();
    store.refresh().unwrap();

    assert_eq!(store.list().len(), 1, "the good preset still loads");
    let q = store.quarantined();
    assert_eq!(q.len(), 1);
    assert!(q[0].path.ends_with("broken.xmp"));
}

// ── T24: create / export / import equality ─────────────────────────────────────

#[test]
fn export_then_import_yields_equal_preset() {
    let dir_a = tempfile::tempdir().unwrap();
    let store_a = PresetStore::open(dir_a.path().to_path_buf()).unwrap();
    let subset = ParamSubset::from_groups([ParamGroup::Tone, ParamGroup::Presence]);
    let created = store_a
        .create_from(&rich_recipe(), "Punchy", Some("Faves"), &subset)
        .unwrap();

    let export_dir = tempfile::tempdir().unwrap();
    let dest = export_dir.path().join("Punchy.xmp");
    store_a.export(&created.id, &dest).unwrap();

    // Import into a fresh store through the public import path.
    let dir_b = tempfile::tempdir().unwrap();
    let store_b = PresetStore::open(dir_b.path().to_path_buf()).unwrap();
    let results = store_b.import_files(&[dest]);
    assert_eq!(results.len(), 1);
    assert!(results[0].error.is_none(), "{:?}", results[0].error);

    let imported_meta = results[0].preset.as_ref().unwrap();
    let imported = store_b.get(&imported_meta.id).unwrap();
    assert!(
        created.content_eq(&imported),
        "export→import changed the preset:\n{created:?}\n{imported:?}"
    );
    assert_eq!(imported.origin, PresetOrigin::Lightbox);
}

// ── T26: LR import, present-key subset detection + report ────────────────────

#[test]
fn lr_modern_preset_imports_with_detected_subset() {
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
    let src = fixtures().join("lr_modern_pv2012.xmp");
    let results = store.import_files(&[src]);
    assert_eq!(results.len(), 1);
    let res = &results[0];
    assert!(res.error.is_none(), "{:?}", res.error);
    let report = res
        .report
        .as_ref()
        .expect("foreign import carries a report");
    // MaskGroupBasedCorrections is skipped-and-preserved (ids-only ownership).
    assert!(report
        .skipped
        .iter()
        .any(|k| k.contains("MaskGroupBasedCorrections")));
    assert_eq!(report.source_pv.as_deref(), Some("11.0"));

    let imported = store.get(&res.preset.as_ref().unwrap().id).unwrap();
    let want: std::collections::BTreeSet<ParamGroup> = [
        ParamGroup::BaseProfile,
        ParamGroup::WhiteBalance,
        ParamGroup::Tone,
        ParamGroup::Curve,
        ParamGroup::ColorMixer,
        ParamGroup::BwMix,
        ParamGroup::Presence,
        ParamGroup::Detail,
        ParamGroup::Optics,
        ParamGroup::Geometry,
    ]
    .into_iter()
    .collect();
    assert_eq!(imported.subset.groups, want, "detected subset mismatch");
    // Masks/Retouch/ColorGrading/Effects were NOT present → not carried.
    assert!(!imported.subset.groups.contains(&ParamGroup::Masks));
    assert!(!imported.subset.groups.contains(&ParamGroup::ColorGrading));
    assert!(matches!(
        imported.origin,
        PresetOrigin::LightroomImport { .. }
    ));
    // A preset carries no foreign passthrough (the recipe's extract is param-only).
    let applied = preview_recipe(&Recipe::identity(PV_M0), &imported);
    assert!(applied.xmp_passthrough.fields.is_empty());
}

// ── T25 + T27: apply/sync as history steps against the Phase-B sink ───────────

#[derive(Default)]
struct MemEditStore {
    state: Mutex<HashMap<ImageId, Recipe>>,
    steps: Mutex<Vec<(ImageId, StepLabel)>>,
}
impl EditSink for MemEditStore {
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
        let mut st = self.state.lock().unwrap();
        let mut steps = self.steps.lock().unwrap();
        for s in batch {
            st.insert(s.image, s.new_recipe.clone());
            steps.push((s.image, s.label.clone()));
        }
        Ok(())
    }
}

#[test]
fn apply_preset_to_many_is_one_labelled_step_each() {
    let dir = tempfile::tempdir().unwrap();
    let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
    let subset = ParamSubset::from_groups([ParamGroup::Tone]);
    let preset = store
        .create_from(&rich_recipe(), "Tone", None, &subset)
        .unwrap();

    let sink = MemEditStore::default();
    let targets: Vec<ImageId> = (0..130).map(ImageId).collect();
    let report = sync_to(
        &sink,
        &preset.delta,
        &targets,
        StepLabel::Preset {
            name: preset.name.clone(),
        },
        &NeverCancel,
        SyncOptions::default(),
        |_| {},
    )
    .unwrap();

    assert_eq!(report.committed, 130);
    assert_eq!(report.txns, 3); // ceil(130/64)
    let steps = sink.steps.lock().unwrap();
    assert_eq!(steps.len(), 130, "one step per target");
    assert!(steps.iter().all(|(_, l)| l.kind() == "preset"));
    // Every target now carries the tone edit.
    assert_eq!(
        sink.recipe_of(ImageId(0)).unwrap().get(ParamId::Exposure),
        ParamValue::F32(1.0)
    );
    assert_eq!(
        sink.recipe_of(ImageId(129)).unwrap().get(ParamId::Contrast),
        ParamValue::F32(20.0)
    );
}

// ── property tests (spec §6) ───────────────────────────────────────────────────

fn subset_strategy() -> impl Strategy<Value = ParamSubset> {
    proptest::collection::vec(any::<bool>(), ALL_GROUPS.len()).prop_map(|flags| {
        ParamSubset::from_groups(
            ALL_GROUPS
                .iter()
                .zip(flags)
                .filter_map(|(g, on)| on.then_some(*g)),
        )
    })
}

proptest! {
    // Each case writes/reads real `.xmp` files, so keep the count modest.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// T24: a preset applied to neutral changes exactly the checked groups.
    #[test]
    fn apply_changes_only_checked_groups(subset in subset_strategy()) {
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
        let preset = store.create_from(&rich_recipe(), "P", None, &subset).unwrap();

        let neutral = Recipe::identity(PV_M0);
        let previewed = preview_recipe(&neutral, &preset);
        for id in previewed.diff(&neutral).0.keys() {
            prop_assert!(subset.contains(*id), "changed {:?} outside subset", id);
        }
    }

    /// T24: create_from → persisted `.xmp` → reload preserves the delta exactly.
    #[test]
    fn create_from_roundtrips_delta(subset in subset_strategy()) {
        let recipe = rich_recipe();
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
        let created = store.create_from(&recipe, "P", None, &subset).unwrap();

        // Reload from a cold scan of a fresh store over the same dir.
        let reopened = PresetStore::open(dir.path().to_path_buf()).unwrap();
        let loaded = reopened.get(&created.id).expect("preset reloads");

        prop_assert_eq!(loaded.subset.clone(), subset.clone());
        prop_assert_eq!(loaded.delta.clone(), recipe.extract(&subset));
        prop_assert!(created.content_eq(&loaded));
    }
}

/// A hand check that `DevelopPreset` is `Send + Sync` (it rides `Arc` in the store
/// and the E08 browser).
fn _assert_send_sync<T: Send + Sync>() {}
#[test]
fn preset_is_send_sync() {
    _assert_send_sync::<DevelopPreset>();
    _assert_send_sync::<PresetStore>();
}
