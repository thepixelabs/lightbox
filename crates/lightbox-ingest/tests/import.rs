// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the add-in-place import primitives (spec §5
//! T17/T18 acceptance criteria that live below the core façade; the
//! event/command-bus side is covered in `lightbox-core`'s tests).

use std::path::{Path, PathBuf};

use lightbox_catalog::Catalog;
use lightbox_ingest::{
    discover_files, import_add_in_place, import_files, ImportEvent, ImportOptions,
};
use lightbox_jobs::CancelToken;
use tempfile::TempDir;

/// A source tree:
/// ```text
/// src/
///   IMG_0001.jpg … IMG_000n.jpg
///   notes.txt              (filtered: unknown extension)
///   .hidden.jpg            (filtered: hidden)
///   sub/
///     NESTED_0001.cr3 … NESTED_000m.cr3
/// ```
fn source_tree(n_root: usize, m_sub: usize) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for i in 1..=n_root {
        std::fs::write(
            src.join(format!("IMG_{i:04}.jpg")),
            format!("root jpeg payload {i}"),
        )
        .unwrap();
    }
    for i in 1..=m_sub {
        std::fs::write(
            src.join("sub").join(format!("NESTED_{i:04}.cr3")),
            format!("nested raw payload {i}"),
        )
        .unwrap();
    }
    std::fs::write(src.join("notes.txt"), "not an image").unwrap();
    std::fs::write(src.join(".hidden.jpg"), "hidden").unwrap();
    (dir, src)
}

fn temp_catalog(dir: &Path) -> Catalog {
    Catalog::create(&dir.join("cat.lbdata")).expect("create catalog")
}

fn no_events() -> impl FnMut(ImportEvent) {
    |_| {}
}

/// `ImportOptions` is `#[non_exhaustive]`: build via `default()` + override.
fn opts_with(f: impl FnOnce(&mut ImportOptions)) -> ImportOptions {
    let mut opts = ImportOptions::default();
    f(&mut opts);
    opts
}

#[test]
fn discovery_filters_extensions_hidden_files_and_recursion() {
    let (_tmp, src) = source_tree(3, 2);
    let cancel = CancelToken::new();

    let recursive = discover_files(&src, true, &cancel).unwrap();
    assert_eq!(recursive.files.len(), 5, "{:?}", recursive.files);
    assert!(recursive.errors.is_empty());
    // Sorted by path: root files before sub/ files (byte order).
    let names: Vec<String> = recursive
        .files
        .iter()
        .map(|p| p.strip_prefix(&src).unwrap().to_string_lossy().into_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(names.iter().all(|n| !n.contains("notes")));
    assert!(names.iter().all(|n| !n.contains("hidden")));

    let flat = discover_files(&src, false, &cancel).unwrap();
    assert_eq!(flat.files.len(), 3, "{:?}", flat.files);

    // A missing source is an InvalidSource error, not a panic.
    assert!(discover_files(&src.join("nope"), true, &cancel).is_err());
}

#[test]
fn import_lands_rows_folders_and_session_stats() {
    let (tmp, src) = source_tree(3, 2);
    let catalog = temp_catalog(tmp.path());
    let writer = catalog.writer();

    let mut events: Vec<ImportEvent> = Vec::new();
    let outcome = import_add_in_place(
        &writer,
        &src,
        &opts_with(|o| o.progress_min_interval = std::time::Duration::ZERO),
        &CancelToken::new(),
        &mut |ev| events.push(ev),
    )
    .expect("import");

    assert_eq!(outcome.report.imported, 5);
    assert_eq!(outcome.report.skipped_duplicates, 0);
    assert!(
        outcome.report.errors.is_empty(),
        "{:?}",
        outcome.report.errors
    );
    assert!(!outcome.cancelled);

    // Events: one Started (with the discovered count), then per-file
    // Progress ending at done == discovered.
    match &events[0] {
        ImportEvent::Started {
            session,
            discovered,
        } => {
            assert_eq!(*session, outcome.session);
            assert_eq!(*discovered, 5);
        }
        other => panic!("first event must be Started, got {other:?}"),
    }
    let last = events.last().expect("progress events");
    match last {
        ImportEvent::Progress {
            done, discovered, ..
        } => {
            assert_eq!(*done, 5);
            assert_eq!(*discovered, 5);
        }
        other => panic!("last event must be Progress, got {other:?}"),
    }

    let reader = catalog.reader();
    let counts = reader.counts().unwrap();
    assert_eq!(counts.assets, 5);
    assert_eq!(counts.images, 5);
    assert_eq!(counts.import_sessions, 1);
    assert_eq!(counts.roots, 1);
    // Root folder + sub/.
    let tree = reader.folder_tree().unwrap();
    let rels: Vec<&str> = tree.iter().map(|f| f.rel_path.as_str()).collect();
    assert!(rels.contains(&""), "{rels:?}");
    assert!(rels.contains(&"sub"), "{rels:?}");

    // Absolute paths resolve back to real files (add-in-place).
    let page = reader
        .images_page(&lightbox_catalog::ImageQuery {
            folder: None,
            sort: lightbox_catalog::SortOrder::FilenameAsc,
            cursor: None,
            limit: 100,
        })
        .unwrap();
    assert_eq!(page.items.len(), 5);
    for item in &page.items {
        let abs = reader.asset_abs_path(item.asset).unwrap();
        assert!(abs.is_file(), "{} should exist", abs.display());
    }
}

#[test]
fn reimport_skips_duplicates_by_content_hash() {
    let (tmp, src) = source_tree(4, 0);
    let catalog = temp_catalog(tmp.path());
    let writer = catalog.writer();
    let opts = ImportOptions::default();

    let first = import_add_in_place(&writer, &src, &opts, &CancelToken::new(), &mut no_events())
        .expect("first import");
    assert_eq!(first.report.imported, 4);

    let second = import_add_in_place(&writer, &src, &opts, &CancelToken::new(), &mut no_events())
        .expect("re-import");
    assert_eq!(second.report.imported, 0, "T17 AC: re-import imports 0");
    assert_eq!(second.report.skipped_duplicates, 4);
    // Duplicates are not double-counted as unsupported.
    assert_eq!(second.report.unsupported, 0);

    assert_eq!(catalog.reader().counts().unwrap().assets, 4);
    // Both sessions are recorded.
    assert_eq!(catalog.reader().counts().unwrap().import_sessions, 2);
}

/// T17 AC: cancel mid-import commits completed batches only (no partial
/// rows) and the session is finished-with-stats.
#[test]
fn cancel_mid_import_commits_completed_batches_only() {
    let (tmp, src) = source_tree(10, 0);
    let catalog = temp_catalog(tmp.path());
    let writer = catalog.writer();
    let cancel = CancelToken::new();

    let cancel_at_done = 6u64;
    let canceller = cancel.clone();
    let outcome = import_add_in_place(
        &writer,
        &src,
        &opts_with(|o| {
            o.batch_size = 4;
            o.progress_min_interval = std::time::Duration::ZERO;
        }),
        &cancel,
        &mut |ev| {
            if let ImportEvent::Progress { done, .. } = ev {
                if done == cancel_at_done {
                    canceller.cancel();
                }
            }
        },
    )
    .expect("cancelled import still returns an outcome");

    assert!(outcome.cancelled);
    // Files 1–4 committed as batch 1; 5 and 6 were staged into batch 2 and
    // discarded at the cancellation checkpoint before file 7.
    assert_eq!(outcome.report.imported, 4);
    let counts = catalog.reader().counts().unwrap();
    assert_eq!(counts.assets, 4, "completed batches only");
    assert_eq!(counts.images, 4);
    assert_eq!(counts.import_sessions, 1, "session bracketed");

    // The rest imports cleanly afterwards (dup-skip covers the overlap).
    let resume = import_add_in_place(
        &writer,
        &src,
        &ImportOptions::default(),
        &CancelToken::new(),
        &mut no_events(),
    )
    .unwrap();
    assert_eq!(resume.report.imported, 6);
    assert_eq!(resume.report.skipped_duplicates, 4);
    assert_eq!(catalog.reader().counts().unwrap().assets, 10);
}

/// T18: a file that disappears between discovery and staging (stat/hash
/// failure) becomes a per-file error — no row, no aborted batch.
#[test]
fn unreadable_file_is_reported_not_fatal() {
    let (tmp, src) = source_tree(3, 0);
    let catalog = temp_catalog(tmp.path());
    let writer = catalog.writer();

    let discovery = discover_files(&src, true, &CancelToken::new()).unwrap();
    assert_eq!(discovery.files.len(), 3);
    // Delete one discovered file before staging touches it.
    std::fs::remove_file(&discovery.files[1]).unwrap();

    let outcome = import_files(
        &writer,
        &src,
        &discovery,
        &ImportOptions::default(),
        &CancelToken::new(),
        &mut no_events(),
    )
    .expect("import continues past the bad file");

    assert_eq!(outcome.report.imported, 2);
    assert_eq!(outcome.report.errors.len(), 1);
    assert_eq!(outcome.report.errors[0].0, discovery.files[1]);
    assert_eq!(catalog.reader().counts().unwrap().assets, 2);
}

/// Spec §4.3 / OQ-7: stored filenames are NFC-normalized.
#[test]
fn filenames_are_stored_nfc_normalized() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // "café.jpg" spelled NFD: 'e' + COMBINING ACUTE ACCENT (U+0301).
    let nfd_name = "cafe\u{0301}.jpg";
    let nfc_name = "caf\u{e9}.jpg";
    assert_ne!(nfd_name, nfc_name);
    std::fs::write(src.join(nfd_name), "payload").unwrap();

    let catalog = temp_catalog(dir.path());
    let outcome = import_add_in_place(
        &catalog.writer(),
        &src,
        &ImportOptions::default(),
        &CancelToken::new(),
        &mut no_events(),
    )
    .unwrap();
    assert_eq!(outcome.report.imported, 1);

    let reader = catalog.reader();
    let page = reader
        .images_page(&lightbox_catalog::ImageQuery {
            folder: None,
            sort: lightbox_catalog::SortOrder::FilenameAsc,
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].filename, nfc_name);
}

/// Spec §4.3 note c / OQ-3: non-UTF-8 paths are rejected per-file with an
/// `ImportReport.errors` entry (Unix only — such names cannot exist on
/// Windows/NTFS via std).
#[cfg(unix)]
#[test]
fn non_utf8_filename_is_rejected_per_file() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("good.jpg"), "good payload").unwrap();
    let bad_name = OsStr::from_bytes(b"bad\xff\xfe.jpg");
    match std::fs::write(src.join(bad_name), "bad payload") {
        Ok(()) => {}
        // Some filesystems (e.g. APFS) refuse invalid UTF-8 outright; the
        // per-file path is then untestable here — nothing to assert.
        Err(_) => return,
    }

    let catalog = temp_catalog(dir.path());
    let outcome = import_add_in_place(
        &catalog.writer(),
        &src,
        &ImportOptions::default(),
        &CancelToken::new(),
        &mut no_events(),
    )
    .unwrap();
    assert_eq!(outcome.report.imported, 1);
    assert_eq!(outcome.report.errors.len(), 1);
    assert!(outcome.report.errors[0].1.contains("UTF-8"));
    assert_eq!(catalog.reader().counts().unwrap().assets, 1);
}

/// The report round-trips through the session `stats` JSON (what the
/// activity center will read back in E06).
#[test]
fn report_serializes() {
    let (tmp, src) = source_tree(2, 0);
    let catalog = temp_catalog(tmp.path());
    let outcome = import_add_in_place(
        &catalog.writer(),
        &src,
        &ImportOptions::default(),
        &CancelToken::new(),
        &mut no_events(),
    )
    .unwrap();
    let json = serde_json::to_string(&outcome.report).unwrap();
    let back: lightbox_ingest::ImportReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.imported, 2);
}
