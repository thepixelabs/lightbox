// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase A task A13, gesture coalescing, end to end.
//!
//! AC (spec §7 A13): "a 200-event slider drag produces EXACTLY 1 history
//! step and ≤ a handful of in-flight renders (probe asserts no per-event
//! render/commit)."
//!
//! E08/E09 already shipped the two halves this test proves compose:
//! - `EditHub::begin_gesture`/`update_gesture`/`Command::Edit(CommitGesture)`
//!   (D2: gesture updates are direct, synchronous, in-memory, no bus
//!   round-trip; `tests/edit_hub.rs` already proves single-update
//!   semantics).
//! - `RenderScheduler`'s latest-wins `Coalescer` (E05 task B6): one in-flight
//!   plus one pending slot per image; the pure core already bounds a
//!   1000-item burst to <= 2 dispatches.
//!
//! What was missing, and what this test adds, is the specific A13 proof
//! that a REAL 200-event develop-param drag (through `EditHub`, feeding
//! `Session::render_scheduler()` exactly as the shell canvas's
//! `submit_if_changed` does every frame) coalesces on BOTH axes at once:
//! one durable history step, and a render-submission count nowhere near the
//! event count. Drives a real `ExposureNode` render (`Session` registers
//! E10's Phase-A global nodes, see `E10-deviations.md` A-5) over a real
//! decodable image (the pinned tiny JPEG, self-contained, no
//! `cargo xtask fixtures` dependency).

use std::path::Path;
use std::time::Duration;

use lightbox_core::{
    ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, ImageQuery, ParamDelta, ParamId,
    ParamValue, Session, SortOrder, StepLabel,
};
use lightbox_types::{ImageId, PV_M0};
use tempfile::TempDir;

const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

/// The pinned, self-made CC0 16×16 JFIF JPEG (same asset `lbx-perf`'s
/// corpus and the shell smoke embed), works from a bare checkout, no
/// fixture-corpus dependency.
const TINY_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

fn write_real_jpeg(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("a13.jpg"), TINY_JPEG).unwrap();
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

/// **A13 acceptance:** a 200-event slider drag produces exactly one history
/// step and a bounded ("a handful") number of render-scheduler dispatches
/// never one commit or one render per event.
#[test]
fn two_hundred_event_slider_drag_coalesces_history_and_renders() {
    let tmp = TempDir::new().unwrap();
    let core = start_core();
    let session = core
        .create_catalog(&tmp.path().join("cat.lbdata"), None)
        .expect("create");
    let src = tmp.path().join("src");
    write_real_jpeg(&src);
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    let scheduler = session.render_scheduler();
    let mut rx = session.events();

    hub.begin_gesture(image, StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");

    let baseline_submissions = scheduler.submissions();
    const EVENTS: u32 = 200;

    // Simulate a real 200-sample slider drag (-2.0 EV -> +2.0 EV): every
    // event updates the in-memory gesture (D2, no bus round-trip) AND feeds
    // the render scheduler exactly the way the canvas's `submit_if_changed`
    // does once per changed `recipe_rev` each frame (spec §6.5 C3).
    for i in 0..EVENTS {
        let ev = -2.0 + (i as f32 / (EVENTS - 1) as f32) * 4.0;
        hub.update_gesture(image, exposure_delta(ev))
            .expect("update_gesture");
        let working = hub
            .working_recipe(image)
            .expect("open image has a working recipe");
        scheduler.set_recipe(image, (*working).clone(), PV_M0);
    }

    // No durable commit fired mid-drag, `EditCommitted` only ever comes
    // from `dispatch_commit_gesture`, never from `update_gesture`'s D2
    // in-memory path. Drain whatever is on the channel (tolerating a
    // `Lagged` from the 200 `EditWorkingChanged` events) and confirm.
    let mut saw_committed = false;
    loop {
        match rx.try_recv() {
            Ok(ev) => {
                if matches!(ev, Event::EditCommitted { image: i, .. } if i == image) {
                    saw_committed = true;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
        }
    }
    assert!(
        !saw_committed,
        "no EditCommitted may fire before CommitGesture — a durable step per drag \
         event would defeat coalescing"
    );

    // Exactly ONE durable history step for the whole 200-event drag.
    session.submit(Command::Edit(EditCommand::CommitGesture { image }));
    let seq = wait_for_event(&mut rx, |ev| match ev {
        Event::EditCommitted { image: i, seq, .. } if *i == image => Some(*seq),
        Event::CommandFailed { error, .. } => panic!("commit failed: {error}"),
        _ => None,
    });
    assert_eq!(
        seq, 1,
        "the whole 200-event drag must coalesce to exactly one history step"
    );

    let state = session.query().edit_state(image).expect("edit_state");
    assert_eq!(state.head_seq, 1, "still exactly one committed step");
    match state.recipe.get(ParamId::Exposure) {
        ParamValue::F32(v) => assert!(
            (v - 2.0).abs() < 1e-4,
            "the final drag value must win: expected 2.0 EV, got {v}"
        ),
        other => panic!("expected a scalar exposure value, got {other:?}"),
    }

    // Render side: latest-wins coalescing must not have dispatched anywhere
    // near 200 engine submissions for 200 rapid recipe updates.
    assert!(
        scheduler.wait_idle(Duration::from_secs(20)),
        "render scheduler reached quiescence"
    );
    let dispatched = scheduler.submissions() - baseline_submissions;
    println!(
        "[A13] {EVENTS} update_gesture events -> {dispatched} engine submissions \
         (render latest-wins coalescing)"
    );
    assert!(dispatched >= 1, "at least the final state must render");
    assert!(
        dispatched <= 20,
        "expected a HANDFUL of dispatches for a {EVENTS}-event burst, got {dispatched} \
         — RenderScheduler's Coalescer (E05 task B6) should bound this far below the \
         event count"
    );
    assert!(
        (dispatched as u32) < EVENTS,
        "must be strictly fewer dispatches than events — proves coalescing, not a \
         per-event render (got {dispatched} for {EVENTS} events)"
    );

    session.close(ClosePolicy::Skip.into()).unwrap();
}
