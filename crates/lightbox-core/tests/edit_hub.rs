// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `EditHub` integration tests (spec §3.4/§5 T8 acceptance criteria).
//!
//! Drives the hub exactly the way the shell (gesture path, D2) and the CLI
//! (durable command-bus path) will: `EditHub::begin_gesture`/`update_gesture`
//! directly, `Command::Edit(CommitGesture)` through `Session::submit`.

use std::path::Path;
use std::time::Duration;

use lightbox_core::{
    Command, Core, CoreConfig, Event, ImageQuery, ParamDelta, ParamId, ParamValue, Session,
    SortOrder,
};
use lightbox_types::ImageId;
use tempfile::TempDir;

const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

fn write_files(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 1..=n {
        std::fs::write(
            dir.join(format!("IMG_{i:04}.jpg")),
            format!("payload {i} of {}", dir.display()),
        )
        .unwrap();
    }
}

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

/// Imports one fixture file and returns its image id, through the same
/// command-bus path the CLI/shell use (never direct catalog access).
fn import_one(session: &Session, src_dir: &Path) -> ImageId {
    let mut rx = session.events();
    session.submit(Command::ImportAddInPlace {
        source_dir: src_dir.to_path_buf(),
        recursive: true,
    });
    wait_for_event(&mut rx, |ev| match ev {
        Event::ImportFinished { .. } => Some(()),
        Event::CommandFailed { error, .. } => panic!("import failed: {error}"),
        _ => None,
    });
    let page = session
        .query()
        .images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::FilenameAsc,
            cursor: None,
            limit: 1,
        })
        .expect("images_page");
    page.items.first().expect("at least one image").id
}

fn exposure_delta(v: f32) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(v));
    d
}

/// T8 AC: an `update_gesture` call fires `Event::EditWorkingChanged` in the
/// SAME call stack, no bus round-trip. Proven two ways: (a) `working_recipe`
/// already reflects the change the instant `update_gesture` returns, with no
/// event loop ever polled; (b) the event is already sitting in the channel
/// (a single non-blocking `try_recv`, not a `wait_for_event` poll loop).
#[test]
fn update_gesture_fires_working_changed_synchronously() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");
    let src = tmp.path().join("src");
    write_files(&src, 1);
    let image = import_one(&session, &src);

    let mut rx = session.events();
    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, lightbox_core::StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");
    hub.update_gesture(image, exposure_delta(1.5))
        .expect("update_gesture");

    // (a) synchronous, in-memory: no event-loop involvement needed.
    let working = hub
        .working_recipe(image)
        .expect("open image has a working recipe");
    assert_eq!(working.get(ParamId::Exposure), ParamValue::F32(1.5));

    // (b) the event is already there, a single non-blocking recv finds it.
    let got = rx.try_recv().expect("EditWorkingChanged already queued");
    assert!(
        matches!(got, Event::EditWorkingChanged { image: i } if i == image),
        "expected EditWorkingChanged, got {got:?}"
    );

    session
        .close(lightbox_core::ClosePolicy::Skip.into())
        .unwrap();
}

/// T8/T11 AC: `Command::Edit(CommitGesture)` durably commits the open
/// gesture as one WAL txn and fires `Event::EditCommitted { seq }`.
#[test]
fn commit_gesture_persists_and_emits_edit_committed() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");
    let src = tmp.path().join("src");
    write_files(&src, 1);
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, lightbox_core::StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");
    hub.update_gesture(image, exposure_delta(0.75))
        .expect("update_gesture");

    let mut rx = session.events();
    session.submit(Command::Edit(lightbox_core::EditCommand::CommitGesture {
        image,
    }));
    let seq = wait_for_event(&mut rx, |ev| match ev {
        Event::EditCommitted { image: i, seq, .. } if *i == image => Some(*seq),
        Event::CommandFailed { error, .. } => panic!("commit failed: {error}"),
        _ => None,
    });
    assert_eq!(seq, 1);

    let state = session.query().edit_state(image).expect("edit_state");
    assert!(state.persisted);
    assert_eq!(state.head_seq, 1);
    assert_eq!(state.recipe.get(ParamId::Exposure), ParamValue::F32(0.75));

    session
        .close(lightbox_core::ClosePolicy::Skip.into())
        .unwrap();
}

/// T11 AC: a second `CommitGesture` with no open gesture is a typed no-op
/// it completes (the caller never hangs) and changes nothing durable.
#[test]
fn second_commit_with_no_open_gesture_is_a_typed_noop() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");
    let src = tmp.path().join("src");
    write_files(&src, 1);
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, lightbox_core::StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");
    hub.update_gesture(image, exposure_delta(0.5))
        .expect("update_gesture");

    let mut rx = session.events();
    session.submit(Command::Edit(lightbox_core::EditCommand::CommitGesture {
        image,
    }));
    wait_for_event(&mut rx, |ev| match ev {
        Event::EditCommitted { image: i, .. } if *i == image => Some(()),
        _ => None,
    });

    // No gesture is open now; a second CommitGesture must complete (not
    // hang) via the generic CatalogChanged completion signal, and it must
    // NOT produce a second EditCommitted (nothing to commit).
    session.submit(Command::Edit(lightbox_core::EditCommand::CommitGesture {
        image,
    }));
    let saw_catalog_changed = wait_for_event(&mut rx, |ev| match ev {
        Event::CatalogChanged { change } if change.images.contains(&image) => Some(true),
        Event::EditCommitted { image: i, seq, .. } if *i == image => {
            panic!("unexpected second EditCommitted at seq {seq} — not a no-op")
        }
        _ => None,
    });
    assert!(saw_catalog_changed);

    let state = session.query().edit_state(image).expect("edit_state");
    assert_eq!(state.head_seq, 1, "still exactly one committed step");

    session
        .close(lightbox_core::ClosePolicy::Skip.into())
        .unwrap();
}

/// T8 AC: closing a session with an open (uncommitted) gesture persists it
/// "an edit is never held only in memory past its gesture commit."
#[test]
fn close_with_open_gesture_auto_commits() {
    let tmp = TempDir::new().unwrap();
    let lbdata = tmp.path().join("cat.lbdata");
    let core = start_core();
    let session = core.create_catalog(&lbdata, None).expect("create");
    let src = tmp.path().join("src");
    write_files(&src, 1);
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, lightbox_core::StepLabel::Param(ParamId::Contrast))
        .expect("begin_gesture");
    hub.update_gesture(image, {
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Contrast, ParamValue::F32(20.0));
        d
    })
    .expect("update_gesture");
    // Deliberately never submit CommitGesture, close() must auto-commit it.
    session
        .close(lightbox_core::ClosePolicy::Skip.into())
        .expect("close");

    // Reopen a FRESH session on the same catalog and confirm the recipe
    // restored (the §3.1.1 promise, in miniature).
    let core2 = start_core();
    let session2 = core2.open_catalog(&lbdata, None).expect("reopen");
    let state = session2.query().edit_state(image).expect("edit_state");
    assert!(state.persisted, "the auto-committed step must be durable");
    assert_eq!(state.head_seq, 1);
    assert_eq!(state.recipe.get(ParamId::Contrast), ParamValue::F32(20.0));
    session2
        .close(lightbox_core::ClosePolicy::Skip.into())
        .unwrap();
}

/// T8 AC: `working_recipe` is practically lock-free for readers, a
/// contention smoke test. Many reader threads hammer `working_recipe` while
/// the main thread drives hundreds of gesture updates; nothing panics, and
/// the whole thing finishes well inside a generous bound (a real deadlock or
/// a full-registry-lock-per-read design would stall this).
#[test]
fn working_recipe_reads_are_lock_free_under_contention() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");
    let src = tmp.path().join("src");
    write_files(&src, 1);
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, lightbox_core::StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut readers = Vec::new();
    for _ in 0..8 {
        let hub = std::sync::Arc::clone(&hub);
        let stop = std::sync::Arc::clone(&stop);
        readers.push(std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = hub.working_recipe(image);
                reads += 1;
            }
            reads
        }));
    }

    let start = std::time::Instant::now();
    for i in 0..2_000 {
        hub.update_gesture(image, exposure_delta((i % 1000) as f32 * 0.001))
            .expect("update_gesture");
    }
    let elapsed = start.elapsed();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let total_reads: u64 = readers.into_iter().map(|h| h.join().unwrap()).sum();

    assert!(
        elapsed < Duration::from_secs(10),
        "2000 gesture updates under 8-way read contention took {elapsed:?} — \
         looks like readers are blocking on a writer-held lock"
    );
    assert!(total_reads > 0, "reader threads made no progress");

    session
        .close(lightbox_core::ClosePolicy::Skip.into())
        .unwrap();
}
