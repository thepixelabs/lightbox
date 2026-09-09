// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E04 T9 acceptance: end-to-end seam-1 integration for the working-set
//! loader, `Command::OpenWorkingSet` -> `Event::WorkingSet*` ->
//! `Session::working_set()`, exactly the path E08's shell and
//! `lightbox-cli open` both drive. The `kill -9` mid-load leg lives in the
//! sibling `tests/working_set_kill9.rs` (re-spawns a child process,
//! mirroring `tests/close_kill.rs`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightbox_core::{
    ClosePolicy, Command, Core, CoreConfig, Event, ItemState, OpenOrigin, OpenRequest, Session,
    SetPhase,
};
use lightbox_types::{AssetId, ImageId};
use tempfile::TempDir;

const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

fn start_core() -> Core {
    Core::start(CoreConfig::default()).expect("core start")
}

fn wait_for_event<T>(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    mut pred: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for event"
        );
        match rx.try_recv() {
            Ok(event) => {
                if let Some(out) = pred(&event) {
                    return out;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                panic!("event channel closed while waiting");
            }
        }
    }
}

/// Submits `Command::OpenWorkingSet` and waits for `WorkingSetLoadFinished`.
fn open_paths(
    session: &Session,
    paths: &[PathBuf],
    recursive: bool,
) -> (u64, lightbox_core::OpenReport) {
    let mut rx = session.events();
    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(paths.to_vec(), recursive, OpenOrigin::Cli),
    });
    // Opening must arrive first.
    let opening_epoch = wait_for_event(&mut rx, |ev| match ev {
        Event::WorkingSetOpening { epoch, .. } => Some(*epoch),
        _ => None,
    });
    let (epoch, report) = wait_for_event(&mut rx, |ev| match ev {
        Event::WorkingSetLoadFinished { epoch, report } => Some((*epoch, report.clone())),
        Event::CommandFailed { error, .. } => panic!("open failed: {error}"),
        _ => None,
    });
    assert_eq!(opening_epoch, epoch, "epoch must be stable across the open");
    (epoch, report)
}

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").exists(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

fn stage_fixture(dest_dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dest_dir).unwrap();
    let dest = dest_dir.join(name);
    std::fs::copy(fixtures_dir().join(name), &dest)
        .unwrap_or_else(|e| panic!("staging fixture {name}: {e}"));
    dest
}

/// T9 AC: mixed corpus (raw + JPEG + corrupt + unsupported + a duplicate
/// copy) opens headlessly through the full command-bus seam with correct
/// per-item states/badges.
#[test]
fn open_mixed_corpus_yields_correct_states_and_badges() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");

    let src = tmp.path().join("src");
    let raw = stage_fixture(&src, "canon-eos-r6.cr3");
    let jpeg = stage_fixture(&src, "lightbox-tiny.jpg");
    let corrupt = stage_fixture(&src, "corrupt-truncated.cr2");
    let dup = src.join("dup-of-jpeg.jpg");
    std::fs::copy(&jpeg, &dup).unwrap();
    let unsupported = src.join("notes.txt");
    std::fs::write(&unsupported, b"not a photo, but explicitly opened").unwrap();

    let paths = vec![raw, jpeg, corrupt, dup, unsupported];
    let (_epoch, report) = open_paths(&session, &paths, false);

    assert_eq!(report.planned, 5);
    assert_eq!(report.collapsed, 1, "the duplicate jpeg copy collapses");
    // raw + jpeg + corrupt (badged, still registered) + unsupported (badged,
    // still registered) all register; only the collapsed dup doesn't count
    // toward `ready` separately.
    assert_eq!(report.ready, 4, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");

    let snapshot = session.working_set();
    assert_eq!(snapshot.phase, SetPhase::Ready);
    assert_eq!(snapshot.items.len(), 5);

    let by_name = |name: &str| {
        snapshot
            .items
            .iter()
            .find(|i| i.filename == name)
            .unwrap_or_else(|| panic!("{name} missing from snapshot: {snapshot:#?}"))
    };

    let raw_item = by_name("canon-eos-r6.cr3");
    assert!(matches!(raw_item.state, ItemState::Ready { .. }));
    assert_eq!(raw_item.source_kind, Some(lightbox_types::SourceKind::Raw));
    assert!(raw_item.decode_error.is_none());

    let jpeg_item = by_name("lightbox-tiny.jpg");
    assert!(matches!(jpeg_item.state, ItemState::Ready { .. }));
    assert_eq!(
        jpeg_item.source_kind,
        Some(lightbox_types::SourceKind::Rendered)
    );

    let dup_item = by_name("dup-of-jpeg.jpg");
    assert!(
        matches!(dup_item.state, ItemState::DuplicateOf { .. }),
        "{:?}",
        dup_item.state
    );

    let corrupt_item = by_name("corrupt-truncated.cr2");
    assert!(matches!(corrupt_item.state, ItemState::Ready { .. }));
    assert!(
        corrupt_item.decode_error.is_some(),
        "malformed known-extension file is a visible failure, badged, still registered"
    );

    let unsupported_item = by_name("notes.txt");
    assert!(matches!(unsupported_item.state, ItemState::Ready { .. }));
    assert_eq!(unsupported_item.format, "UNSUPPORTED");
    assert_eq!(unsupported_item.source_kind, None);
    assert!(unsupported_item.explicit);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T9 AC: moving a file on disk between two opens preserves identity
/// (`relocated`); editing a file in place mints a new identity.
#[test]
fn moved_file_relocates_edited_file_reidentifies() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");

    let original = tmp.path().join("IMG_0001.jpg");
    std::fs::write(&original, b"raw pixel payload one").unwrap();

    let (_epoch1, report1) = open_paths(&session, std::slice::from_ref(&original), false);
    assert_eq!(report1.ready, 1);
    assert_eq!(report1.relocated, 0);
    let (asset1, image1) = ready_ids(&session, "IMG_0001.jpg");

    let moved_dir = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&moved_dir).unwrap();
    let moved = moved_dir.join("IMG_0001_renamed.jpg");
    std::fs::rename(&original, &moved).unwrap();

    let (_epoch2, report2) = open_paths(&session, std::slice::from_ref(&moved), false);
    assert_eq!(report2.ready, 1);
    assert_eq!(report2.reused, 1);
    assert_eq!(report2.relocated, 1);
    let (asset2, image2) = ready_ids(&session, "IMG_0001_renamed.jpg");
    assert_eq!(
        asset1, asset2,
        "same content -> same AssetId across the move"
    );
    assert_eq!(
        image1, image2,
        "same content -> same ImageId across the move"
    );

    // Edit the SAME (moved) path in place -> new content -> new identity.
    std::fs::write(&moved, b"raw pixel payload one, EDITED").unwrap();
    let (_epoch3, report3) = open_paths(&session, &[moved], false);
    assert_eq!(report3.ready, 1);
    assert_eq!(
        report3.reused, 0,
        "new content must not resolve to the old row"
    );
    let (asset3, _image3) = ready_ids(&session, "IMG_0001_renamed.jpg");
    assert_ne!(asset3, asset2, "editing in place mints a new identity");

    session.close(ClosePolicy::Skip.into()).unwrap();
}

fn ready_ids(session: &Session, filename: &str) -> (AssetId, ImageId) {
    let snapshot = session.working_set();
    let item = snapshot
        .items
        .iter()
        .find(|i| i.filename == filename)
        .unwrap_or_else(|| panic!("{filename} not in snapshot"));
    match item.state {
        ItemState::Ready { asset, image } => (asset, image),
        other => panic!("{filename}: expected Ready, got {other:?}"),
    }
}

/// T9 AC / keep-dormant write audit (spec §5.3): the open path never writes
/// `folder`/`library_root`/`import_session`, asserted against a real
/// command-bus open, headlessly, not just at the DAO layer.
#[test]
fn keep_dormant_write_audit_through_the_command_bus() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");

    let src = tmp.path().join("src");
    stage_fixture(&src, "lightbox-tiny.jpg");
    stage_fixture(&src, "gradient-8x8.png");

    let (_epoch, report) = open_paths(&session, &[src], false);
    assert_eq!(report.ready, 2);

    let counts = session.query().counts().unwrap();
    assert_eq!(counts.roots, 0);
    assert_eq!(counts.folders, 0);
    assert_eq!(counts.import_sessions, 0);
    assert_eq!(counts.assets, 2);
    assert_eq!(counts.images, 2);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T8 AC (event-order / epoch-guard): a second `OpenWorkingSet` submitted
/// while the first is still loading cancels the first, its
/// `WorkingSetLoadFinished` reports partial, and the final
/// `Session::working_set()` snapshot reflects ONLY the second (newer)
/// epoch, never a mutation from the stale one.
#[test]
fn a_second_open_mid_load_supersedes_the_first() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");

    // A largeish first request (10 raw-ish files, synthesized so hashing has
    // real work to do) so there is a realistic window to interleave the
    // second submit while the first is still loading.
    let src1 = tmp.path().join("src1");
    std::fs::create_dir_all(&src1).unwrap();
    let payload = vec![7u8; 4 * 1024 * 1024]; // 4 MiB per file
    let mut first_paths = Vec::new();
    for i in 0u32..8 {
        let p = src1.join(format!("f{i}.jpg"));
        let mut bytes = payload.clone();
        bytes.extend_from_slice(&i.to_le_bytes());
        std::fs::write(&p, bytes).unwrap();
        first_paths.push(p);
    }
    let second = tmp.path().join("second.jpg");
    std::fs::write(&second, b"second request payload").unwrap();

    let mut rx = session.events();
    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(first_paths, false, OpenOrigin::Cli),
    });
    let epoch1 = wait_for_event(&mut rx, |ev| match ev {
        Event::WorkingSetOpening { epoch, .. } => Some(*epoch),
        _ => None,
    });

    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(vec![second.clone()], false, OpenOrigin::Cli),
    });
    let epoch2 = wait_for_event(&mut rx, |ev| match ev {
        Event::WorkingSetOpening { epoch, .. } if *epoch != epoch1 => Some(*epoch),
        _ => None,
    });
    assert_eq!(epoch2, epoch1 + 1);

    // Both loads eventually finish (order not asserted, the first may
    // finish either just-before or after being cancelled at a checkpoint).
    let mut finished = std::collections::HashSet::new();
    while finished.len() < 2 {
        match wait_for_event(&mut rx, |ev| match ev {
            Event::WorkingSetLoadFinished { epoch, .. } => Some(*epoch),
            _ => None,
        }) {
            e if e == epoch1 || e == epoch2 => {
                finished.insert(e);
            }
            _ => {}
        }
    }

    // The live snapshot reflects ONLY epoch 2, the second open's single
    // file, never a stale mutation from epoch 1.
    let snapshot = session.working_set();
    assert_eq!(snapshot.epoch, epoch2);
    assert_eq!(snapshot.items.len(), 1, "{snapshot:#?}");
    assert_eq!(snapshot.items[0].filename, "second.jpg");
    assert!(matches!(snapshot.items[0].state, ItemState::Ready { .. }));

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T8 AC: `Session::close` drains an in-flight open (the in-flight guard
/// covers `working_set.open` exactly like `import.add_in_place`).
#[test]
fn close_drains_an_in_flight_open() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");

    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..20 {
        std::fs::write(src.join(format!("f{i}.jpg")), vec![i as u8; 512 * 1024]).unwrap();
    }
    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(vec![src], false, OpenOrigin::Cli),
    });
    // Close immediately, must not hang or panic even with the open still
    // (possibly) in flight.
    let report = session.close(ClosePolicy::Skip.into());
    assert!(report.is_ok(), "{report:?}");
}
