// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E04 T5/T6/T7 acceptance tests for the working-set loader.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightbox_catalog::Catalog;
use lightbox_jobs::CancelToken;
use tempfile::TempDir;

use super::*;

/// A real (826-byte) JPEG so files probe cleanly (mirrors
/// `tests/import.rs`'s fixture).
const TINY_JPG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/xtask/assets/lightbox-tiny.jpg"
));

/// A valid JPEG payload with a unique trailing pad (ignored by decoders) so
/// every file gets a distinct content hash.
fn jpeg_payload(tag: &str) -> Vec<u8> {
    let mut bytes = TINY_JPG.to_vec();
    bytes.extend_from_slice(b"pad:");
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn write_jpeg(dir: &Path, name: &str, tag: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, jpeg_payload(tag)).unwrap();
    path
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn fixture(name: &str) -> PathBuf {
    let p = fixtures_dir().join(name);
    assert!(
        p.is_file(),
        "fixture {name} missing — run `cargo xtask fixtures`"
    );
    p
}

fn req(paths: &[PathBuf], recursive: bool) -> OpenRequest {
    OpenRequest {
        paths: paths.to_vec(),
        recursive,
        origin: OpenOrigin::Cli,
    }
}

fn plan(req: &OpenRequest) -> SetPlan {
    plan_open(
        req,
        &OpenOptions::default(),
        &CancelToken::new(),
        &mut |_| {},
    )
    .unwrap()
}

// ── pure unit tests: ordering keys ──────────────────────────────────────────

#[test]
fn capture_order_key_offset_bearing_normalizes_to_utc() {
    // Same wall-clock digits, different offsets -> different absolute times.
    let plus_one = capture_order_key(Some("2024-01-01T00:30:00+01:00")).unwrap();
    let utc = capture_order_key(Some("2024-01-01T00:00:00+00:00")).unwrap();
    let minus_one = capture_order_key(Some("2024-01-01T00:00:00-01:00")).unwrap();
    // +01:00 at 00:30 local = 23:30 UTC (previous day), earliest.
    // +00:00 at 00:00 = 00:00 UTC.
    // -01:00 at 00:00 local = 01:00 UTC, latest.
    assert!(plus_one < utc);
    assert!(utc < minus_one);
}

#[test]
fn capture_order_key_naive_is_treated_as_utc() {
    let naive = capture_order_key(Some("2024-01-01T12:00:00")).unwrap();
    let same_as_utc = capture_order_key(Some("2024-01-01T12:00:00+00:00")).unwrap();
    assert_eq!(naive, same_as_utc, "naive values compare as-if-UTC");
}

#[test]
fn capture_order_key_none_and_unparseable_are_none() {
    assert_eq!(capture_order_key(None), None);
    assert_eq!(capture_order_key(Some("not a date")), None);
    assert_eq!(capture_order_key(Some("")), None);
}

#[test]
fn utc_normalize_capture_time_converts_offset_and_keeps_naive_verbatim() {
    let with_offset = Some("2024-01-01T00:30:00+01:00".to_owned());
    assert_eq!(
        utc_normalize_capture_time(&with_offset).as_deref(),
        Some("2023-12-31T23:30:00.000000Z")
    );
    let naive = Some("2024-01-01T12:00:00".to_owned());
    assert_eq!(
        utc_normalize_capture_time(&naive).as_deref(),
        Some("2024-01-01T12:00:00"),
        "naive values are stored verbatim, never invented an offset"
    );
    assert_eq!(utc_normalize_capture_time(&None), None);
}

#[test]
fn order_key_places_untimed_after_timed_and_breaks_ties_by_filename_then_path() {
    let timed = PlannedItem {
        path: PathBuf::from("/a/z.jpg"),
        filename: "z.jpg".to_owned(),
        probe: Ok(fake_probe(Some("2024-01-01T00:00:00Z"))),
        source_kind: None,
        explicit: false,
    };
    let untimed = PlannedItem {
        path: PathBuf::from("/a/a.jpg"),
        filename: "a.jpg".to_owned(),
        probe: Ok(fake_probe(None)),
        source_kind: None,
        explicit: false,
    };
    assert!(
        order_key(&timed) < order_key(&untimed),
        "any timed item sorts before any untimed one"
    );

    let a = PlannedItem {
        path: PathBuf::from("/a/x.jpg"),
        filename: "x.jpg".to_owned(),
        probe: Ok(fake_probe(None)),
        source_kind: None,
        explicit: false,
    };
    let b = PlannedItem {
        path: PathBuf::from("/a/y.jpg"),
        filename: "y.jpg".to_owned(),
        probe: Ok(fake_probe(None)),
        source_kind: None,
        explicit: false,
    };
    assert!(
        order_key(&a) < order_key(&b),
        "untimed items tie-break by filename"
    );
}

fn fake_probe(capture_time: Option<&str>) -> lightbox_decode::AssetProbe {
    lightbox_decode::AssetProbe {
        format: lightbox_decode::ProbedFormat::Jpeg,
        width: 8,
        height: 8,
        orientation: lightbox_types::Orientation::O1,
        camera_make: None,
        camera_model: None,
        capture_time: capture_time.map(str::to_owned),
        file_bytes: 8,
        embedded: Vec::new(),
    }
}

// ── plan_open ────────────────────────────────────────────────────────────

#[test]
fn empty_request_is_an_error() {
    let r = OpenRequest {
        paths: vec![],
        recursive: false,
        origin: OpenOrigin::Cli,
    };
    assert!(matches!(
        plan_open(
            &r,
            &OpenOptions::default(),
            &CancelToken::new(),
            &mut |_| {}
        ),
        Err(OpenError::EmptyRequest)
    ));
}

#[test]
fn missing_path_is_skipped_not_aborted() {
    let tmp = TempDir::new().unwrap();
    let real = write_jpeg(tmp.path(), "real.jpg", "1");
    let missing = tmp.path().join("nope.jpg");

    let plan = plan(&req(&[missing.clone(), real.clone()], false));
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].path, real.canonicalize().unwrap());
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::NotFound) && s.path == missing));
}

#[test]
fn explicit_file_always_enters_regardless_of_extension_or_hidden_name() {
    let tmp = TempDir::new().unwrap();
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, b"not a photo").unwrap();
    let hidden = tmp.path().join(".hidden.jpg");
    std::fs::write(&hidden, jpeg_payload("hidden")).unwrap();

    let plan = plan(&req(&[txt.clone(), hidden.clone()], false));
    assert_eq!(plan.items.len(), 2, "{:?}", plan.items);
    assert!(plan.items.iter().all(|i| i.explicit));
    let txt_item = plan
        .items
        .iter()
        .find(|i| i.filename == "notes.txt")
        .unwrap();
    // Unknown junk is a SUCCESSFUL probe (ProbedFormat::Unsupported), not an
    // error, badged, never a crash (spec §6.2).
    assert!(txt_item.probe.is_ok());
    assert_eq!(txt_item.source_kind, None);
    let hidden_item = plan
        .items
        .iter()
        .find(|i| i.filename == ".hidden.jpg")
        .unwrap();
    assert!(hidden_item.probe.is_ok());
}

#[test]
fn explicit_malformed_known_extension_file_is_a_visible_planned_failure() {
    let tmp = TempDir::new().unwrap();
    let junk = tmp.path().join("junk.cr2");
    std::fs::write(&junk, b"definitely not a raw file").unwrap();

    let plan = plan(&req(&[junk], false));
    assert_eq!(plan.items.len(), 1, "the item still enters the set");
    assert!(plan.items[0].probe.is_err());
    assert_eq!(plan.items[0].source_kind, None);
}

#[test]
fn walk_discovered_filters_extension_and_hidden_and_counts_skips() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_jpeg(&src, "a.jpg", "a");
    write_jpeg(&src, "b.jpg", "b");
    std::fs::write(src.join("notes.txt"), b"not a photo").unwrap();
    std::fs::write(src.join(".hidden.jpg"), jpeg_payload("hidden")).unwrap();

    let plan = plan(&req(std::slice::from_ref(&src), false));
    assert_eq!(plan.items.len(), 2, "{:?}", plan.items);
    assert!(plan.items.iter().all(|i| !i.explicit));
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::UnsupportedExtension)
            && s.path.ends_with("notes.txt")));
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::Hidden) && s.path.ends_with(".hidden.jpg")));
}

#[test]
fn duplicate_path_in_request_dedups_first_wins() {
    let tmp = TempDir::new().unwrap();
    let a = write_jpeg(tmp.path(), "a.jpg", "a");

    let plan = plan(&req(&[a.clone(), a.clone()], false));
    assert_eq!(plan.items.len(), 1);
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::DuplicatePath)));
}

#[test]
fn explicit_file_inside_a_dropped_folder_dedups_against_the_walk() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let a = write_jpeg(&src, "a.jpg", "a");

    // The folder comes first (gesture order): its walk claims a.jpg; the
    // explicit mention of the same canonical path afterward is a dup.
    let plan = plan(&req(&[src.clone(), a.clone()], false));
    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].path == a.canonicalize().unwrap());
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::DuplicatePath)));
}

#[cfg(unix)]
#[test]
fn non_utf8_explicit_file_is_a_visible_planned_failure() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new().unwrap();
    let bad_name = OsStr::from_bytes(b"bad\xff\xfe.jpg");
    let path = tmp.path().join(bad_name);
    match std::fs::write(&path, jpeg_payload("bad")) {
        Ok(()) => {}
        Err(_) => return, // filesystem refuses invalid UTF-8 outright (e.g. APFS)
    }

    let plan = plan(&req(&[path], false));
    assert_eq!(plan.items.len(), 1);
    assert_eq!(
        plan.items[0].probe.as_ref().unwrap_err(),
        "path is not valid UTF-8"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_walk_discovered_file_is_silently_skipped_and_counted() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_jpeg(&src, "good.jpg", "good");
    let bad_name = OsStr::from_bytes(b"bad\xff\xfe.jpg");
    match std::fs::write(src.join(bad_name), jpeg_payload("bad")) {
        Ok(()) => {}
        Err(_) => return,
    }

    let plan = plan(&req(&[src], false));
    assert_eq!(plan.items.len(), 1, "{:?}", plan.items);
    assert_eq!(plan.items[0].filename, "good.jpg");
    assert!(plan
        .skipped
        .iter()
        .any(|s| matches!(s.reason, SkipReason::NonUtf8Path)));
}

#[test]
fn gesture_order_for_explicit_files_wraps_a_folders_sorted_expansion() {
    let tmp = TempDir::new().unwrap();
    let first = write_jpeg(tmp.path(), "AAA_first.jpg", "first");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Filenames chosen so plain path/filename sort would NOT match
    // capture-time order, proves we actually sort by capture time.
    write_jpeg(&src, "z-earlier.cr3", "z"); // no real EXIF -> None capture_time either way (synthetic jpeg bytes, .cr3 ext -> malformed anyway)
    let last = write_jpeg(tmp.path(), "ZZZ_last.jpg", "last");

    let plan = plan(&req(&[first.clone(), src.clone(), last.clone()], false));
    // Gesture order preserved around the folder's contribution.
    assert_eq!(
        plan.items.first().unwrap().path,
        first.canonicalize().unwrap()
    );
    assert_eq!(
        plan.items.last().unwrap().path,
        last.canonicalize().unwrap()
    );
    assert!(plan.items[0].explicit);
    assert!(plan.items.last().unwrap().explicit);
    assert!(!plan.items[1].explicit);
}

#[test]
fn folder_expansion_sorts_by_capture_time_then_filename() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Real raw fixtures carry real EXIF capture times; use two with known
    // relative order (derived from the probe itself, not hardcoded, so this
    // stays robust to fixture corpus changes).
    let raws = ["nikon-d4s.nef", "nikon-z6.nef", "sony-ilce7s.arw"];
    let mut expected_order: Vec<(Option<String>, String)> = Vec::new();
    for name in raws {
        let src_fixture = fixture(name);
        let dest_name = format!("copy-{name}");
        std::fs::copy(&src_fixture, src.join(&dest_name)).unwrap();
        let p = lightbox_decode::probe(&src_fixture).unwrap();
        expected_order.push((p.capture_time, dest_name));
    }
    // Also one untimed item (PNG has no EXIF capture time).
    std::fs::copy(fixture("gradient-8x8.png"), src.join("untimed.png")).unwrap();
    expected_order.push((None, "untimed.png".to_owned()));
    expected_order.sort_by(|a, b| {
        let ka = capture_order_key(a.0.as_deref());
        let kb = capture_order_key(b.0.as_deref());
        match (ka, kb) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        }
    });

    let plan = plan(&req(&[src], false));
    let got_order: Vec<String> = plan.items.iter().map(|i| i.filename.clone()).collect();
    let expected_names: Vec<String> = expected_order.into_iter().map(|(_, n)| n).collect();
    assert_eq!(got_order, expected_names, "{:#?}", plan.items);
}

#[test]
fn max_set_size_truncates_and_counts_setsizecap() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..5 {
        write_jpeg(&src, &format!("f{i:02}.jpg"), &format!("{i}"));
    }
    let opts = OpenOptions {
        max_set_size: 2,
        ..OpenOptions::default()
    };
    let plan = plan_open(&req(&[src], false), &opts, &CancelToken::new(), &mut |_| {}).unwrap();
    assert_eq!(plan.items.len(), 2);
    assert!(plan.truncated);
    let cap_skips = plan
        .skipped
        .iter()
        .filter(|s| matches!(s.reason, SkipReason::SetSizeCap))
        .count();
    assert_eq!(cap_skips, 3);
}

#[test]
fn cancellation_stops_the_plan_at_a_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..20 {
        write_jpeg(&src, &format!("f{i:02}.jpg"), &format!("{i}"));
    }
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let mut seen = 0u64;
    let out = plan_open(
        &req(&[src], false),
        &OpenOptions {
            progress_min_interval: Duration::ZERO,
            ..OpenOptions::default()
        },
        &cancel,
        &mut |_p| {
            seen += 1;
            if seen == 3 {
                canceller.cancel();
            }
        },
    );
    assert!(matches!(out, Err(OpenError::Cancelled)));
}

// ── load_working_set ─────────────────────────────────────────────────────

fn temp_catalog(dir: &Path) -> Catalog {
    Catalog::create(&dir.join("cat.lbdata")).expect("create catalog")
}

fn load(catalog: &Catalog, plan: &SetPlan) -> (OpenReport, Vec<LoadEvent>) {
    let mut events = Vec::new();
    let report = load_working_set(
        &catalog.writer(),
        plan,
        &OpenOptions::default(),
        &CancelToken::new(),
        &mut |ev| events.push(ev),
    )
    .unwrap();
    (report, events)
}

#[test]
fn load_registers_every_item_and_item0_completes_before_item1_starts() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let a = write_jpeg(tmp.path(), "a.jpg", "a");
    let b = write_jpeg(tmp.path(), "b.jpg", "b");
    let plan = plan(&req(&[a, b], false));

    let (report, events) = load(&catalog, &plan);
    assert_eq!(report.planned, 2);
    assert_eq!(report.ready, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(report.collapsed, 0);

    // Item 0's Ready lands before item 1's, true by construction (a
    // sequential loop), asserted explicitly here as the T7 AC.
    let ready_indices: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            LoadEvent::ItemReady { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(ready_indices, vec![0, 1]);

    assert_eq!(catalog.reader().counts().unwrap().assets, 2);
    assert_eq!(catalog.reader().counts().unwrap().images, 2);
}

#[test]
fn duplicate_content_collapses_with_correct_first_index() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let a = write_jpeg(tmp.path(), "a.jpg", "same");
    let b = write_jpeg(tmp.path(), "b-copy.jpg", "same"); // identical bytes
    let plan = plan(&req(&[a, b], false));

    let (report, events) = load(&catalog, &plan);
    assert_eq!(report.ready, 1);
    assert_eq!(report.collapsed, 1);
    assert!(events.iter().any(|e| matches!(
        e,
        LoadEvent::ItemCollapsed {
            index: 1,
            first_index: 0
        }
    )));
    assert_eq!(catalog.reader().counts().unwrap().assets, 1);
}

#[test]
fn per_item_failure_never_aborts_the_load() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let a = write_jpeg(tmp.path(), "a.jpg", "a");
    let vanishing = write_jpeg(tmp.path(), "vanish.jpg", "v");
    let c = write_jpeg(tmp.path(), "c.jpg", "c");
    let plan = plan(&req(&[a, vanishing.clone(), c], false));

    // Delete the middle file AFTER planning, BEFORE loading.
    std::fs::remove_file(&vanishing).unwrap();

    let (report, events) = load(&catalog, &plan);
    assert_eq!(report.ready, 2);
    assert_eq!(report.failed, 1);
    assert!(events
        .iter()
        .any(|e| matches!(e, LoadEvent::ItemFailed { index: 1, .. })));
    assert_eq!(catalog.reader().counts().unwrap().assets, 2);
}

#[test]
fn cancelled_load_emits_a_truthful_partial_report() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let paths: Vec<PathBuf> = (0..5)
        .map(|i| write_jpeg(tmp.path(), &format!("f{i}.jpg"), &format!("{i}")))
        .collect();
    let plan = plan(&req(&paths, false));

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let mut ready = 0u64;
    let report = load_working_set(
        &catalog.writer(),
        &plan,
        &OpenOptions::default(),
        &cancel,
        &mut |ev| {
            if let LoadEvent::ItemReady { .. } = ev {
                ready += 1;
                if ready == 2 {
                    canceller.cancel();
                }
            }
        },
    )
    .unwrap();
    assert!(
        report.ready < 5,
        "the load must stop before finishing all 5"
    );
    assert!(report.ready >= 2);
}

#[test]
fn reopening_the_same_plan_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| write_jpeg(tmp.path(), &format!("f{i}.jpg"), &format!("{i}")))
        .collect();
    let plan = plan(&req(&paths, false));

    let (first, _) = load(&catalog, &plan);
    assert_eq!(first.ready, 3);
    assert_eq!(first.reused, 0);
    let counts_after_first = catalog.reader().counts().unwrap();

    let (second, _) = load(&catalog, &plan);
    assert_eq!(second.ready, 3);
    assert_eq!(second.reused, 3, "every item resolves to the existing row");
    let counts_after_second = catalog.reader().counts().unwrap();
    assert_eq!(
        counts_after_first.assets, counts_after_second.assets,
        "zero new rows"
    );
    assert_eq!(counts_after_first.images, counts_after_second.images);
}

#[test]
fn moved_file_relocates_identity_across_two_opens() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let original = write_jpeg(&src_dir, "IMG_0001.jpg", "move-me");

    let plan1 = plan(&req(std::slice::from_ref(&original), false));
    let (report1, events1) = load(&catalog, &plan1);
    assert_eq!(report1.ready, 1);
    assert_eq!(
        report1.relocated, 0,
        "first open is a fresh insert, not a relocation"
    );
    let (asset1, image1) = ready_ids(&events1, 0);

    let moved_dir = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&moved_dir).unwrap();
    let moved = moved_dir.join("IMG_0001_renamed.jpg");
    std::fs::rename(&original, &moved).unwrap();

    let plan2 = plan(&req(&[moved], false));
    let (report2, events2) = load(&catalog, &plan2);
    assert_eq!(report2.ready, 1);
    assert_eq!(report2.reused, 1);
    assert_eq!(report2.relocated, 1);
    let (asset2, image2) = ready_ids(&events2, 0);
    assert_eq!(
        asset1, asset2,
        "same content -> same AssetId across the move"
    );
    assert_eq!(
        image1, image2,
        "same content -> same ImageId across the move"
    );

    assert_eq!(
        catalog.reader().counts().unwrap().assets,
        1,
        "no duplicate row"
    );
}

#[test]
fn same_path_new_content_mints_a_new_identity_and_clears_the_stale_claimant() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let path = tmp.path().join("IMG_0001.jpg");
    std::fs::write(&path, jpeg_payload("original")).unwrap();

    let plan1 = plan(&req(std::slice::from_ref(&path), false));
    let (_report1, events1) = load(&catalog, &plan1);
    let (asset1, _image1) = ready_ids(&events1, 0);

    let canonical_path = path.canonicalize().unwrap();

    // Edit the file in place (same path, new bytes -> new content hash).
    std::fs::write(&path, jpeg_payload("edited")).unwrap();
    let plan2 = plan(&req(std::slice::from_ref(&path), false));
    let (report2, events2) = load(&catalog, &plan2);
    assert_eq!(report2.ready, 1);
    assert_eq!(
        report2.reused, 0,
        "new content -> a NEW asset row, not a reuse"
    );
    let (asset2, _image2) = ready_ids(&events2, 0);
    assert_ne!(asset1, asset2, "same-path-new-content mints a new identity");

    // The old asset's abs_path hint is cleared (it no longer resolves the
    // path, the path now belongs to the new content, spec §3.3/§4.4 step 4).
    let old_path = catalog.reader().asset_abs_path(asset1);
    assert!(
        old_path.is_err() || old_path.unwrap() != canonical_path,
        "the stale claimant's abs_path hint must be cleared"
    );
    // The new asset resolves the path correctly.
    assert_eq!(
        catalog.reader().asset_abs_path(asset2).unwrap(),
        canonical_path
    );
    assert_eq!(
        catalog.reader().counts().unwrap().assets,
        2,
        "two distinct rows now exist"
    );
}

#[test]
fn keep_dormant_write_audit_open_path_never_touches_managed_tree_tables() {
    let tmp = TempDir::new().unwrap();
    let catalog = temp_catalog(tmp.path());
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_jpeg(&src, "a.jpg", "a");
    write_jpeg(&src, "b.jpg", "b");

    let plan = plan(&req(&[src], false));
    let (report, _events) = load(&catalog, &plan);
    assert_eq!(report.ready, 2);

    let counts = catalog.reader().counts().unwrap();
    assert_eq!(counts.roots, 0, "open path must never write library_root");
    assert_eq!(counts.folders, 0, "open path must never write folder");
    assert_eq!(
        counts.import_sessions, 0,
        "open path must never write import_session"
    );
    assert_eq!(counts.assets, 2);
    assert_eq!(counts.images, 2);
}

fn ready_ids(
    events: &[LoadEvent],
    index: usize,
) -> (lightbox_types::AssetId, lightbox_types::ImageId) {
    events
        .iter()
        .find_map(|e| match e {
            LoadEvent::ItemReady {
                index: i,
                asset,
                image,
                ..
            } if *i == index => Some((*asset, *image)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ItemReady event for index {index}: {events:?}"))
}
