// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Starter-preset retirement (`lightbox_edit::preset_retire`).
//!
//! This is the one piece of Lightbox that **deletes files out of the user's
//! own data directory**, so the tests are written from the destructive side:
//! most of them assert that something *survives*. The single question each
//! case answers is "did the user touch this?", if yes, it must still be there
//! afterwards, no matter how much it otherwise resembles a starter preset.
//!
//! Every store here is rooted in a `tempfile::TempDir`. Nothing in this file
//! may ever reach `PresetStore::open_default` (`~/Library/Application
//! Support/Lightbox/presets` on macOS), [`temp_store`] is the only way a
//! store is opened, and it asserts its own root is the temp dir it was given.

use std::path::{Path, PathBuf};

use lightbox_edit::params::{ParamDelta, ParamId, ParamSubset, ParamValue};
use lightbox_edit::preset_retire::{MarkerSweepSkipped, RetireReason};
use lightbox_edit::{PresetId, PresetStore, Recipe};
use lightbox_meta::xmp::{ns, sidecar};
use lightbox_types::PV_M0;

// ── fixtures ────────────────────────────────────────────────────────────────

/// The 22 `.xmp` files the starter library shipped **before** it was
/// re-authored, exactly as they were committed. Importing these through
/// `import_files` reproduces, byte for byte, the store state a user who
/// launched the old build is sitting on today.
fn historical_preset_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/retired-starter-presets");
    let mut out = Vec::new();
    collect_xmp(&dir, &mut out);
    assert_eq!(out.len(), 22, "expected the 22 pre-re-authoring presets");
    out
}

/// The starter library as it ships **today** (`assets/presets/`), i.e. what
/// `lightbox-shell`'s `bundled_preset_files()` resolves to on a real launch.
fn current_bundled_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/presets");
    let mut out = Vec::new();
    collect_xmp(&dir, &mut out);
    assert!(!out.is_empty(), "assets/presets/ is empty — bless it first");
    out
}

fn collect_xmp(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_xmp(&p, out);
        } else if p.extension().is_some_and(|x| x == "xmp") {
            found.push(p);
        }
    }
    found.sort();
    out.extend(found);
}

/// Open a preset store rooted in `dir`, the ONLY store constructor this file
/// uses, so no test can drift onto the real per-user preset directory.
fn temp_store(dir: &Path) -> PresetStore {
    let store = PresetStore::open(dir.to_path_buf()).expect("temp preset store opens");
    assert!(
        store.root().starts_with(dir),
        "a test store escaped its temp dir: {}",
        store.root().display()
    );
    store
}

/// A recipe with one distinctive `Tone` value, for building user presets that
/// are clearly not any starter preset.
fn toned(contrast: f32) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Contrast, ParamValue::F32(contrast));
    r.apply(&d).expect("tone delta applies");
    r
}

fn tone_subset() -> ParamSubset {
    ParamSubset::from_groups([lightbox_edit::params::ParamGroup::Tone])
}

/// Build a synthetic "bundled library" directory and return its files, the way
/// `bundled_preset_files()` would. The files are written by `create_from`, so
/// they are unmarked on disk exactly like the real `assets/presets/**`, the
/// bundled marker is applied by `seed_bundled` at import time, never shipped.
fn make_library(dir: &Path, presets: &[(&str, &str, f32)]) -> Vec<PathBuf> {
    let store = temp_store(dir);
    for (group, name, contrast) in presets {
        store
            .create_from(&toned(*contrast), name, Some(group), &tone_subset())
            .expect("library preset is authored");
    }
    let mut out = Vec::new();
    collect_xmp(dir, &mut out);
    out
}

/// A store seeded the way the OLD build seeded it: a plain user-style import
/// of the bundled files, with no marker anywhere.
fn store_with_legacy_starters(root: &Path) -> PresetStore {
    let store = temp_store(root);
    let results = store.import_files(&historical_preset_files());
    assert!(
        results.iter().all(|r| r.error.is_none()),
        "the historical library must import cleanly"
    );
    assert_eq!(store.list().len(), 22);
    for p in store.list() {
        let full = store.get(&p.id).expect("indexed preset loads");
        assert!(
            !full.bundled,
            "{}: a legacy-seeded preset predates the marker",
            p.name
        );
    }
    store
}

fn names(store: &PresetStore) -> Vec<String> {
    let mut v: Vec<String> = store.list().into_iter().map(|m| m.name).collect();
    v.sort();
    v
}

fn groups(store: &PresetStore) -> std::collections::BTreeSet<String> {
    store.list().into_iter().filter_map(|m| m.group).collect()
}

// ── 1. the defect itself ────────────────────────────────────────────────────

/// The reported bug, end to end: a store carrying the old 22 plus the new 36
/// (58 presets across 11 groups) comes out of one seed holding only the 36
/// across 7 groups.
#[test]
fn seeding_retires_the_stale_starter_library_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    let bundled = current_bundled_files();
    let report = store.seed_bundled(&bundled).expect("seed succeeds");

    assert_eq!(
        report.import_failures(),
        0,
        "the shipped library must import"
    );
    assert_eq!(
        report.retired.len(),
        22,
        "every unmodified old starter should retire, got {:?}",
        report
            .retired
            .iter()
            .map(|r| &r.meta.name)
            .collect::<Vec<_>>()
    );
    for r in &report.retired {
        assert_eq!(r.reason, RetireReason::UnmodifiedLegacyStarter);
        assert!(!r.path.exists(), "{} still on disk", r.path.display());
    }

    assert_eq!(store.list().len(), bundled.len());
    assert_eq!(
        groups(&store),
        [
            "Atmosphere",
            "Edge",
            "Mono",
            "Neon",
            "Retro",
            "Tonal",
            "Verdant"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
    // The dead families are gone by name, not just by count.
    for gone in [
        "Punchy Standard",
        "Warm Glow",
        "Portrait Natural",
        "Classic Mono",
    ] {
        assert!(
            !names(&store).contains(&gone.to_string()),
            "{gone} survived"
        );
    }
}

/// A second launch retires nothing and changes nothing.
#[test]
fn retirement_is_idempotent_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());
    let bundled = current_bundled_files();

    let first = store.seed_bundled(&bundled).expect("first seed");
    assert_eq!(first.retired.len(), 22);
    let after_first = names(&store);

    let second = store.seed_bundled(&bundled).expect("second seed");
    assert!(
        second.retired.is_empty(),
        "second run retired {:?}",
        second.retired
    );
    assert_eq!(
        names(&store),
        after_first,
        "the store changed on a no-op run"
    );

    let third = store.seed_bundled(&bundled).expect("third seed");
    assert!(third.retired.is_empty());
    assert_eq!(names(&store), after_first);
}

// ── 2. everything the user touched survives ─────────────────────────────────

/// Renamed: same id, same recipe, different name, the user made it theirs.
#[test]
fn a_renamed_old_starter_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    let target = store
        .list()
        .into_iter()
        .find(|m| m.name == "Punchy Standard")
        .expect("the old library had Punchy Standard");
    store.rename(&target.id, "My Punchy").expect("rename works");

    let report = store.seed_bundled(&current_bundled_files()).unwrap();

    assert!(
        names(&store).contains(&"My Punchy".to_string()),
        "a renamed starter preset was destroyed"
    );
    assert_eq!(
        report.retired.len(),
        21,
        "only the 21 untouched starters should retire"
    );
    assert!(report.retired.iter().all(|r| r.meta.name != "My Punchy"));
}

/// Re-grouped: the user dragged the file into a folder of their own. Same id,
/// same name, same recipe, but it is filed where they put it.
#[test]
fn a_regrouped_old_starter_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    let target = store
        .list()
        .into_iter()
        .find(|m| m.name == "Deep Punch")
        .expect("the old library had Deep Punch");
    let full = store.get(&target.id).unwrap();
    let moved_dir = tmp.path().join("My Looks");
    std::fs::create_dir_all(&moved_dir).unwrap();
    std::fs::rename(&full.path, moved_dir.join("Deep Punch.xmp")).unwrap();
    store.refresh().unwrap();

    let report = store.seed_bundled(&current_bundled_files()).unwrap();

    let survivor = store
        .list()
        .into_iter()
        .find(|m| m.name == "Deep Punch")
        .expect("a re-grouped starter preset was destroyed");
    assert_eq!(survivor.group.as_deref(), Some("My Looks"));
    assert_eq!(report.retired.len(), 21);
}

/// Edited recipe, everything else identical: the user kept the name, the
/// group, and the id, and changed only what the preset *does*. This is the
/// case a naive id-or-name match would get catastrophically wrong.
#[test]
fn an_old_starter_whose_recipe_the_user_edited_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    let victim = store
        .list()
        .into_iter()
        .find(|m| m.name == "Punchy Standard")
        .unwrap();
    let donor = store
        .list()
        .into_iter()
        .find(|m| m.name == "Deep Punch")
        .unwrap();
    let victim_path = store.get(&victim.id).unwrap().path.clone();
    let donor_path = store.get(&donor.id).unwrap().path.clone();

    // Transplant the donor's recipe payload onto the victim's file: same
    // `lb:PresetId`, `lb:PresetName`, `lb:PresetGroup` and `lb:PresetGroups`,
    // different tone values. (Both are `tone`-subset presets, so the extracted
    // delta really does change.)
    let donor_doc = sidecar::read(&donor_path).unwrap().unwrap();
    let recipe_cbor = donor_doc
        .get(ns::LB, "RecipeCbor")
        .expect("the historical files carry an lb: recipe payload");
    let mut victim_doc = sidecar::read(&victim_path).unwrap().unwrap();
    victim_doc.set(ns::LB, "RecipeCbor", recipe_cbor).unwrap();
    sidecar::write_atomic(&victim_path, &victim_doc).unwrap();
    store.refresh().unwrap();

    let edited = store
        .get(&victim.id)
        .expect("the edited preset still loads");
    assert_eq!(edited.name, "Punchy Standard");
    assert_eq!(edited.id, victim.id);

    let report = store.seed_bundled(&current_bundled_files()).unwrap();

    assert!(
        store.get(&victim.id).is_some(),
        "an edited starter preset was destroyed — the user's work is gone"
    );
    assert!(report.retired.iter().all(|r| r.meta.id != victim.id));
    assert_eq!(report.retired.len(), 21);
}

/// Re-authoring a bundled slot (which is how the app lets a user "edit" a
/// preset, the store has no in-place recipe update) yields a preset of their
/// own: fresh id, no marker, never retired.
#[test]
fn re_authoring_a_bundled_slot_makes_it_the_users() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();
    let seeded = store
        .list()
        .into_iter()
        .find(|m| m.name == "Nightline")
        .unwrap();
    assert!(
        store.get(&seeded.id).unwrap().bundled,
        "seed should mark it"
    );

    // The user re-authors that same slot with their own values.
    let mine = store
        .create_from(&toned(-45.0), "Nightline", Some("Neon"), &tone_subset())
        .unwrap();
    assert!(!mine.bundled, "a re-authored preset is the user's");
    assert_ne!(mine.id, seeded.id, "re-authoring mints a fresh id");

    // The library drops Nightline entirely.
    let lib_v2 = tempfile::tempdir().unwrap();
    let v2 = make_library(lib_v2.path(), &[("Neon", "Sodium", 30.0)]);
    let report = store.seed_bundled(&v2).unwrap();

    assert!(
        store.get(&mine.id).is_some(),
        "the user's re-authored preset was retired with the library slot"
    );
    assert!(report.retired.is_empty(), "retired {:?}", report.retired);
}

/// A preset the user authored in a group whose *name* a starter family also
/// used (`Tonal` survived the re-authoring) is not a starter preset.
#[test]
fn a_user_preset_sharing_a_group_name_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    let mine = store
        .create_from(&toned(-33.0), "My Own Tonal", Some("Tonal"), &tone_subset())
        .unwrap();
    // …and one carrying a retired family's group name, too.
    let also_mine = store
        .create_from(
            &toned(-34.0),
            "My Portrait Look",
            Some("Portrait"),
            &tone_subset(),
        )
        .unwrap();

    let report = store.seed_bundled(&current_bundled_files()).unwrap();

    assert!(
        store.get(&mine.id).is_some(),
        "user preset in Tonal destroyed"
    );
    assert!(
        store.get(&also_mine.id).is_some(),
        "user preset in a retired family's group destroyed"
    );
    assert_eq!(report.retired.len(), 22);
    // The group survives because the user's preset is still in it.
    assert!(groups(&store).contains("Portrait"));
}

/// A preset the current library ships is never a retirement candidate.
#[test]
fn a_current_bundled_preset_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let store = temp_store(tmp.path());
    let bundled = current_bundled_files();

    store.seed_bundled(&bundled).unwrap();
    let before = names(&store);
    assert_eq!(before.len(), bundled.len());

    let report = store.seed_bundled(&bundled).unwrap();
    assert!(report.retired.is_empty(), "retired {:?}", report.retired);
    assert_eq!(names(&store), before);
}

// ── 3. the durable marker (recurrence prevention) ───────────────────────────

/// The forward path: a preset that leaves the library is retired by its
/// marker, with no archaeology involved.
#[test]
fn a_marked_preset_that_leaves_the_library_is_retired() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let lib_v2 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );
    let v2 = make_library(lib_v2.path(), &[("Neon", "Sodium", 30.0)]);

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();
    assert_eq!(names(&store), vec!["Nightline", "Sodium"]);
    for m in store.list() {
        assert!(store.get(&m.id).unwrap().bundled, "{}: unmarked", m.name);
    }

    let report = store.seed_bundled(&v2).unwrap();
    assert_eq!(report.retired.len(), 1);
    assert_eq!(report.retired[0].meta.name, "Nightline");
    assert_eq!(report.retired[0].reason, RetireReason::DroppedFromLibrary);
    assert_eq!(names(&store), vec!["Sodium"]);
}

/// Renaming a bundled preset clears its marker, the documented safe answer to
/// "what happens when a user edits a bundled preset". It survives the library
/// dropping it.
#[test]
fn renaming_a_bundled_preset_clears_the_marker_and_saves_it() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let lib_v2 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );
    let v2 = make_library(lib_v2.path(), &[("Neon", "Sodium", 30.0)]);

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();
    let seeded = store
        .list()
        .into_iter()
        .find(|m| m.name == "Nightline")
        .unwrap();
    store.rename(&seeded.id, "Nightline Mine").unwrap();

    let renamed = store.get(&seeded.id).expect("renamed preset loads");
    assert!(!renamed.bundled, "rename must clear the bundled marker");

    let report = store.seed_bundled(&v2).unwrap();
    assert!(report.retired.is_empty(), "retired {:?}", report.retired);
    assert!(names(&store).contains(&"Nightline Mine".to_string()));
}

/// The marker is set by the *operation*, never read off the file. A user
/// importing a file that claims to be bundled, a copied starter file, or a
/// hostile one, gets an unmarked preset that retirement can never reach.
#[test]
fn the_marker_cannot_be_forged_through_a_user_import() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let v1 = make_library(lib.path(), &[("Neon", "Nightline", 20.0)]);

    // Produce a genuinely marked file by exporting one out of a seeded store.
    let seeded_dir = tempfile::tempdir().unwrap();
    let seeded = temp_store(seeded_dir.path());
    seeded.seed_bundled(&v1).unwrap();
    let marked = seeded.list().into_iter().next().unwrap();
    assert!(seeded.get(&marked.id).unwrap().bundled);
    let exported = tmp.path().join("claims-to-be-bundled.xmp");
    seeded.export(&marked.id, &exported).unwrap();
    let doc = sidecar::read(&exported).unwrap().unwrap();
    assert_eq!(
        doc.get(ns::LB, "PresetBundled").and_then(|v| v.as_bool()),
        Some(true),
        "the exported file really does carry the marker"
    );

    // The user imports it into their own store.
    let mine_dir = tempfile::tempdir().unwrap();
    let mine = temp_store(mine_dir.path());
    let results = mine.import_files(&[exported]);
    assert!(results.iter().all(|r| r.error.is_none()));
    let imported = mine.list().into_iter().next().unwrap();
    assert!(
        !mine.get(&imported.id).unwrap().bundled,
        "a user import must never produce a marked preset"
    );

    // An empty library therefore cannot claim it (and could not anyway).
    let empty = tempfile::tempdir().unwrap();
    let v_none = make_library(empty.path(), &[]);
    let report = mine.seed_bundled(&v_none).unwrap();
    assert!(report.retired.is_empty());
    assert!(mine.get(&imported.id).is_some());
}

// ── 4. hostile and degraded inputs ──────────────────────────────────────────

/// A malformed file sitting in the store is quarantined by the scan, never
/// becomes a retirement candidate, and does not stop the seed.
#[test]
fn a_malformed_file_in_the_store_does_not_break_seeding() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_legacy_starters(tmp.path());

    std::fs::write(tmp.path().join("garbage.xmp"), b"<not xmp at all").unwrap();
    std::fs::write(tmp.path().join("empty.xmp"), b"").unwrap();
    std::fs::create_dir_all(tmp.path().join("Weird")).unwrap();
    std::fs::write(
        tmp.path().join("Weird/truncated.xmp"),
        b"<?xpacket begin=\"\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF",
    )
    .unwrap();

    let report = store.seed_bundled(&current_bundled_files()).unwrap();

    assert_eq!(report.retired.len(), 22, "the real work still happened");
    assert!(!store.quarantined().is_empty(), "the junk was quarantined");
    // Quarantined files are left exactly where they were, untouched, not
    // deleted, because we cannot prove anything about them.
    assert!(tmp.path().join("garbage.xmp").exists());
    assert!(tmp.path().join("empty.xmp").exists());
    assert!(tmp.path().join("Weird/truncated.xmp").exists());
}

/// A malformed file in the *bundled library* means we cannot establish what
/// the library ships, so the marker sweep is skipped wholesale rather than
/// treating the unreadable preset as removed.
#[test]
fn an_incomplete_bundled_library_skips_the_marker_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();
    assert_eq!(store.list().len(), 2);

    // Next release: one library file is unreadable on disk.
    let lib_v2 = tempfile::tempdir().unwrap();
    let mut v2 = make_library(lib_v2.path(), &[("Neon", "Sodium", 30.0)]);
    let broken = lib_v2.path().join("Neon/Broken.xmp");
    std::fs::write(&broken, b"<not xmp at all").unwrap();
    v2.push(broken);

    let report = store.seed_bundled(&v2).unwrap();

    assert_eq!(report.import_failures(), 1, "the corrupt file must fail");
    assert_eq!(
        report.marker_sweep_skipped,
        Some(MarkerSweepSkipped::IncompleteImport {
            failed: 1,
            total: 2
        })
    );
    assert!(
        report.retired.is_empty(),
        "an unreadable library file must not retire anything: {:?}",
        report.retired
    );
    assert_eq!(store.list().len(), 2, "Nightline must survive");
}

/// The same guard for the other way a library file goes missing: the path is
/// listed but no longer there (a partial install, a half-synced app bundle).
#[test]
fn a_missing_bundled_file_skips_the_marker_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();

    let lib_v2 = tempfile::tempdir().unwrap();
    let mut v2 = make_library(lib_v2.path(), &[("Neon", "Sodium", 30.0)]);
    v2.push(lib_v2.path().join("Neon/Vanished.xmp"));

    let report = store.seed_bundled(&v2).unwrap();

    assert_eq!(
        report.marker_sweep_skipped,
        Some(MarkerSweepSkipped::IncompleteImport {
            failed: 1,
            total: 2
        })
    );
    assert!(report.retired.is_empty());
    assert_eq!(store.list().len(), 2, "Nightline must survive");
}

/// An empty bundled library is a packaging failure, not "the library removed
/// everything". Retiring on it would wipe every starter preset on the machine.
#[test]
fn an_empty_bundled_library_retires_nothing_by_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_v1 = tempfile::tempdir().unwrap();
    let v1 = make_library(
        lib_v1.path(),
        &[("Neon", "Nightline", 20.0), ("Neon", "Sodium", 30.0)],
    );

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();

    let report = store.seed_bundled(&[]).unwrap();
    assert_eq!(
        report.marker_sweep_skipped,
        Some(MarkerSweepSkipped::NoBundledFiles)
    );
    assert!(report.retired.is_empty());
    assert_eq!(names(&store), vec!["Nightline", "Sodium"]);
}

/// Seeding into a store that has never existed works, marks everything, and
/// retires nothing.
#[test]
fn seeding_a_fresh_store_marks_the_library_and_retires_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("never-created/presets");
    let store = temp_store(&root);
    let bundled = current_bundled_files();

    let report = store.seed_bundled(&bundled).unwrap();
    assert!(report.retired.is_empty());
    assert!(report.marker_sweep_skipped.is_none());
    assert_eq!(store.list().len(), bundled.len());
    for m in store.list() {
        assert!(store.get(&m.id).unwrap().bundled, "{}: unmarked", m.name);
    }
}

/// The marker survives a plain re-seed (it must, or every second launch would
/// forget which presets the app installed).
#[test]
fn the_marker_survives_a_re_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let v1 = make_library(lib.path(), &[("Neon", "Nightline", 20.0)]);

    let store = temp_store(tmp.path());
    store.seed_bundled(&v1).unwrap();
    store.seed_bundled(&v1).unwrap();

    let m = store.list().into_iter().next().unwrap();
    assert!(store.get(&m.id).unwrap().bundled, "the marker was lost");
}

/// An unknown id is never treated as retirable just because it is missing from
/// the library, only a *marked* preset can be swept that way.
#[test]
fn an_unmarked_preset_absent_from_the_library_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let v1 = make_library(lib.path(), &[("Neon", "Sodium", 30.0)]);

    let store = temp_store(tmp.path());
    let mine = store
        .create_from(&toned(11.0), "Private", Some("Neon"), &tone_subset())
        .unwrap();
    assert!(!mine.bundled);

    let report = store.seed_bundled(&v1).unwrap();
    assert!(report.retired.is_empty());
    assert!(store.get(&mine.id).is_some(), "a user preset was retired");
    assert_ne!(mine.id, PresetId("".into()));
}
