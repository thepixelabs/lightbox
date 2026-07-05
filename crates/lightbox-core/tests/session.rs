// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Headless integration tests for the core façade (spec §5 T15/T16/T17/T18
//! acceptance criteria; §6 "UI-free command-bus integration tests").
//!
//! Everything here drives `lightbox-core` exactly the way the shell and the
//! CLI do: `submit` → broadcast events → `query()` — no direct catalog
//! access on the happy paths (seam-1 proof).

use std::path::Path;
use std::time::Duration;

use lightbox_core::{
    CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageQuery, Session, SortOrder,
};
use lightbox_types::{Flag, ImageId, ImportSessionId};
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

/// Waits (on the current thread) until `pred` matches a broadcast event.
fn wait_for_event<T>(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    session: &Session,
    mut pred: impl FnMut(&Event) -> Option<T>,
) -> T {
    let _ = session; // events don't need the session; kept for call-site clarity
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

/// Import `src` through the command bus; returns the import session id.
fn import_dir(session: &Session, src: &Path) -> (ImportSessionId, lightbox_core::ImportReport) {
    let mut rx = session.events();
    session.submit(Command::ImportAddInPlace {
        source_dir: src.to_path_buf(),
        recursive: true,
    });
    wait_for_event(&mut rx, session, |ev| match ev {
        Event::ImportFinished { session, report } => Some((*session, report.clone())),
        Event::CommandFailed { error, .. } => panic!("import failed: {error}"),
        _ => None,
    })
}

fn first_image(session: &Session) -> ImageId {
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

/// T15 AC: headless create → close → reopen → close, with the exit-backup
/// policy observable at each step.
#[test]
fn lifecycle_create_close_reopen_close_with_backup_policy() {
    let tmp = TempDir::new().unwrap();
    let lbdata = tmp.path().join("cat.lbdata");
    let core = start_core();

    // Create; close skipping the backup (tests may opt out — spec §3.8).
    let session = core.create_catalog(&lbdata, None).expect("create");
    let report = session.close(ClosePolicy::Skip.into()).expect("close");
    assert!(report.backup.is_none());

    // Reopen; Auto policy with no backup on record ⇒ backup runs.
    let session = core.open_catalog(&lbdata, None).expect("reopen");
    let report = session.close(CloseOpts::default()).expect("close");
    let first_backup = report.backup.expect("auto policy runs the first backup");
    assert!(first_backup.path.is_file());
    assert!(first_backup.path.starts_with(lbdata.join("backups")));

    // Reopen; Auto policy with a fresh (< 24 h) backup ⇒ skipped.
    let session = core.open_catalog(&lbdata, None).expect("reopen again");
    let report = session.close(CloseOpts::default()).expect("close");
    assert!(
        report.backup.is_none(),
        "backup must be skipped while the last one is fresh"
    );

    // Always forces one regardless.
    let session = core.open_catalog(&lbdata, None).expect("reopen once more");
    let report = session.close(ClosePolicy::Always.into()).expect("close");
    assert!(report.backup.is_some());
}

/// T16 AC: submit `SetRating` → observe `CatalogChanged` → `images_page`
/// reflects it. Same for `SetFlag`.
#[test]
fn set_rating_and_flag_round_trip_through_events_and_queries() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 3);
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();
    import_dir(&session, &src);
    let image = first_image(&session);

    let mut rx = session.events();
    session.submit(Command::SetRating {
        image,
        rating: Some(4),
    });
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.images.contains(&image) => Some(()),
        Event::CommandFailed { error, .. } => panic!("SetRating failed: {error}"),
        _ => None,
    });
    let detail = session.query().image_detail(image).unwrap();
    assert_eq!(detail.rating, Some(4));

    session.submit(Command::SetFlag {
        image,
        flag: Flag::Pick,
    });
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.images.contains(&image) => Some(()),
        Event::CommandFailed { error, .. } => panic!("SetFlag failed: {error}"),
        _ => None,
    });
    assert_eq!(
        session.query().image_detail(image).unwrap().flag,
        Flag::Pick
    );

    // Clearing the rating goes through the same path.
    session.submit(Command::SetRating {
        image,
        rating: None,
    });
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.images.contains(&image) => Some(()),
        _ => None,
    });
    assert_eq!(session.query().image_detail(image).unwrap().rating, None);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T16 AC: a failing command emits `CommandFailed` and leaves no partial
/// transaction (verified by inspection query).
#[test]
fn failing_command_emits_failure_and_commits_nothing() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 2);
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();
    import_dir(&session, &src);
    let counts_before = session.query().counts().unwrap();

    let mut rx = session.events();
    // Nonexistent image: the txn rolls back, nothing changes.
    let ticket = session.submit(Command::SetRating {
        image: ImageId(999_999),
        rating: Some(5),
    });
    let error = wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CommandFailed { ticket: t, error } if *t == ticket => Some(error.clone()),
        Event::CatalogChanged { .. } => panic!("failing command must not change the catalog"),
        _ => None,
    });
    assert!(error.contains("not found"), "{error}");
    assert_eq!(session.query().counts().unwrap(), counts_before);

    // Invalid rating value: rejected, same guarantees.
    let image = first_image(&session);
    let ticket = session.submit(Command::SetRating {
        image,
        rating: Some(9),
    });
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CommandFailed { ticket: t, .. } if *t == ticket => Some(()),
        _ => None,
    });
    assert_eq!(session.query().image_detail(image).unwrap().rating, None);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T17 AC (core side): import lands correct rows with Started/Progress/
/// Finished events; re-import of the same dir skips every file by content
/// hash.
#[test]
fn import_and_reimport_through_the_command_bus() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 8);
    write_files(&src.join("sub"), 3); // nested folder, distinct payloads
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();

    let mut rx = session.events();
    let ticket = session.submit(Command::ImportAddInPlace {
        source_dir: src.clone(),
        recursive: true,
    });
    // ImportStarted correlates with our ticket.
    let started_session = wait_for_event(&mut rx, &session, |ev| match ev {
        Event::ImportStarted { session, ticket: t } if *t == ticket => Some(*session),
        _ => None,
    });
    let (finished_session, report) = wait_for_event(&mut rx, &session, |ev| match ev {
        Event::ImportFinished { session, report } => Some((*session, report.clone())),
        Event::CommandFailed { error, .. } => panic!("import failed: {error}"),
        _ => None,
    });
    assert_eq!(started_session, finished_session);
    assert_eq!(report.imported, 11);
    assert_eq!(report.skipped_duplicates, 0);
    // The bulk CatalogChanged invalidation follows.
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.all_images => Some(()),
        _ => None,
    });

    let counts = session.query().counts().unwrap();
    assert_eq!(counts.assets, 11);
    assert_eq!(counts.images, 11);

    // Re-import: imported == 0, all skipped (T17 AC).
    let (_, report) = import_dir(&session, &src);
    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped_duplicates, 11);
    assert_eq!(session.query().counts().unwrap().assets, 11);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T18 AC: undo restores exact pre-import row counts (FTS included) without
/// touching files on disk.
#[test]
fn undo_import_restores_row_counts_and_fts() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 5);
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();

    let before = session.query().counts().unwrap();
    let (import_session, report) = import_dir(&session, &src);
    assert_eq!(report.imported, 5);
    assert_eq!(session.query().counts().unwrap().assets, 5);
    // FTS sees the filenames…
    assert_eq!(
        session.query().search_filenames("IMG", 100).unwrap().len(),
        5
    );

    let mut rx = session.events();
    session.submit(Command::UndoImport {
        session: import_session,
    });
    wait_for_event(&mut rx, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.all_images => Some(()),
        Event::CommandFailed { error, .. } => panic!("undo failed: {error}"),
        _ => None,
    });

    let after = session.query().counts().unwrap();
    assert_eq!(after.assets, before.assets, "assets restored");
    assert_eq!(after.images, before.images, "images restored (cascade)");
    assert_eq!(
        after.import_sessions, before.import_sessions,
        "session row removed"
    );
    // …and forgets them after undo (delete triggers keep FTS in parity).
    assert_eq!(
        session.query().search_filenames("IMG", 100).unwrap().len(),
        0
    );
    // Files on disk untouched (add-in-place).
    assert_eq!(std::fs::read_dir(&src).unwrap().count(), 5);

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T16 AC: events dropped under a slow subscriber don't wedge the writer
/// (broadcast lag semantics).
#[test]
fn slow_subscriber_lags_without_wedging_the_writer() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 1);
    let mut cfg = CoreConfig::default();
    cfg.event_capacity = 4; // tiny ring: lag is guaranteed below
    let core = Core::start(cfg).unwrap();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();
    import_dir(&session, &src);
    let image = first_image(&session);

    // Slow subscriber: subscribes, never reads until the end.
    let mut slow = session.events();

    // 20 commands through a capacity-4 ring.
    for i in 0..20u8 {
        session.submit(Command::SetRating {
            image,
            rating: Some(i % 5 + 1),
        });
    }
    // The writer kept working: the last rating lands, observed via a fresh
    // query (poll — commands are async).
    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        let rating = session.query().image_detail(image).unwrap().rating;
        if rating == Some(19 % 5 + 1) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer wedged: rating stuck at {rating:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // The slow subscriber observes a Lagged error, then catches up — and a
    // fresh command still reaches it.
    match slow.try_recv() {
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(missed)) => {
            assert!(missed > 0);
        }
        other => panic!("expected Lagged, got {other:?}"),
    }
    session.submit(Command::SetFlag {
        image,
        flag: Flag::Reject,
    });
    wait_for_event(&mut slow, &session, |ev| match ev {
        Event::CatalogChanged { change } if change.images.contains(&image) => Some(()),
        _ => None,
    });

    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// Session is clone-cheap and clones share the bus (spec §3.8: Arc inner).
#[test]
fn session_clones_share_catalog_and_events() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 1);
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();
    import_dir(&session, &src);
    let image = first_image(&session);

    let clone = session.clone();
    let mut rx = clone.events();
    session.submit(Command::SetRating {
        image,
        rating: Some(2),
    });
    wait_for_event(&mut rx, &clone, |ev| match ev {
        Event::CatalogChanged { .. } => Some(()),
        _ => None,
    });
    assert_eq!(clone.query().image_detail(image).unwrap().rating, Some(2));

    // Closing one handle leaves the clone's queries working (shutdown is
    // last-drop); then close the clone for real.
    session.close(ClosePolicy::Skip.into()).unwrap();
    assert_eq!(clone.query().counts().unwrap().images, 1);
    clone.close(ClosePolicy::Skip.into()).unwrap();
}

/// `BackupNow` through the bus produces a verified backup + event.
#[test]
fn backup_now_command_produces_verified_backup() {
    let tmp = TempDir::new().unwrap();
    let lbdata = tmp.path().join("cat.lbdata");
    let core = start_core();
    let session = core.create_catalog(&lbdata, None).unwrap();

    let mut rx = session.events();
    session.submit(Command::BackupNow);
    let report = wait_for_event(&mut rx, &session, |ev| match ev {
        Event::BackupFinished { report } => Some(report.clone()),
        Event::CommandFailed { error, .. } => panic!("backup failed: {error}"),
        _ => None,
    });
    assert!(report.path.is_file());
    assert!(report.bytes > 0);
    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// T17 AC: event-loop heartbeat — no core-runtime stall > 16 ms during a
/// 1 k-file import against synthetic files.
#[test]
fn heartbeat_no_core_stall_during_1k_import() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("photos");
    write_files(&src, 1000);
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();

    // Interactive heartbeat on the same runtime the import events ride on.
    let max_gap = std::sync::Arc::new(std::sync::Mutex::new(Duration::ZERO));
    let beats = std::sync::Arc::clone(&max_gap);
    let stop = lightbox_jobs::CancelToken::new();
    let heartbeat_stop = stop.clone();
    let heartbeat = core.jobs().spawn(
        lightbox_jobs::Class::Interactive,
        "heartbeat",
        stop.clone(),
        async move {
            let mut last = std::time::Instant::now();
            loop {
                if heartbeat_stop.is_cancelled() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
                let now = std::time::Instant::now();
                let gap = now - last;
                last = now;
                let mut max = beats.lock().unwrap();
                if gap > *max {
                    *max = gap;
                }
            }
        },
    );
    // Let the timer settle before measuring.
    std::thread::sleep(Duration::from_millis(50));
    *max_gap.lock().unwrap() = Duration::ZERO;

    let (_, report) = import_dir(&session, &src);
    assert_eq!(report.imported, 1000);

    let observed = *max_gap.lock().unwrap();
    stop.cancel();
    let _ = heartbeat; // handle drop detaches; token already stopped it

    assert!(
        observed < Duration::from_millis(16),
        "core runtime stalled for {observed:?} during the import (budget 16 ms)"
    );

    // Grid stays browsable during import is the shell's demo; headless we
    // at least prove queries answer immediately after.
    assert_eq!(session.query().counts().unwrap().assets, 1000);
    session.close(ClosePolicy::Skip.into()).unwrap();
}

/// Commands submitted after close fail cleanly instead of hanging.
#[test]
fn commands_after_close_report_failure() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();
    let clone = session.clone();
    session.close(ClosePolicy::Skip.into()).unwrap();

    let mut rx = clone.events();
    let ticket = clone.submit(Command::BackupNow);
    let error = wait_for_event(&mut rx, &clone, |ev| match ev {
        Event::CommandFailed { ticket: t, error } if *t == ticket => Some(error.clone()),
        _ => None,
    });
    assert!(error.contains("closing"), "{error}");
}

/// The engine seam exists headlessly (spec §3.8: `Session::engine`,
/// `Session::previews`).
#[test]
fn engine_and_previews_are_reachable_headless() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .unwrap();

    // Headless with gpu: None ⇒ CPU-only engine (spec §3.4).
    assert_eq!(
        session.engine().backend_kind(),
        lightbox_render::BackendKind::CpuOnly
    );

    // Phase-4 previews: the placeholder fails fast instead of pretending.
    let previews = session.previews();
    let ticket = previews.request(
        ImageId(1),
        lightbox_preview::PreviewClass::Thumb { max_px: 256 },
        lightbox_jobs::Class::Background,
    );
    match previews.poll(&ticket) {
        lightbox_preview::PreviewState::Failed(lightbox_preview::PreviewError::Unavailable(_)) => {}
        other => panic!("expected Unavailable, got {other:?}"),
    }
    session.close(ClosePolicy::Skip.into()).unwrap();
}
