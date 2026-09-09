// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! T12, the auto-persist proof (spec §5, "first failing test (b)" from
//! §5's epic header, plus the working-set-replacement leg): **edit a file →
//! close everything → reopen the same file (moved, or renamed) → the
//! recipe is back, with zero user action.**
//!
//! # Scope note, why this drives `EditStore::image_for_content_hash`
//! directly, not `import_add_in_place` pointed at the new location
//!
//! The spec's restore chain is `file → ContentHash → asset → default image
//! → edit_recipe` (§4.2), and names the seam explicitly: "`Row
//! find-or-create is E04's loader` … `image_for_content_hash` + `open_state`
//! … are the E09 seam it consumes" (§4.2). E04 (the working-set loader that
//! would re-home a moved/renamed asset's `path`/`filename` hint on
//! `import_add_in_place`) is not built yet (handoff board: "📋 Specced, not
//! built"), today's `CatalogTxn::insert_assets` is a pure dup-skip on
//! `content_hash` (no path-hint update), which is explicitly out of E09's
//! scope. So this test proves the actual E09-owned mechanism, the content-
//! hash keyed restore chain, the same way E04 will consume it, rather than
//! simulating a UI drag-and-drop through a loader that doesn't exist yet.

use std::path::Path;
use std::time::Duration;

use lightbox_core::{
    ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, ImageQuery, ParamDelta, ParamId,
    ParamValue, Session, SortOrder, StepLabel,
};
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;
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

/// Commits one exposure gesture via the durable command bus (the same path
/// the CLI's `edit set` drives) and returns the resulting head_seq.
fn commit_exposure(session: &Session, image: ImageId, v: f32) -> u64 {
    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, StepLabel::Param(ParamId::Exposure))
        .expect("begin_gesture");
    hub.update_gesture(image, exposure_delta(v))
        .expect("update_gesture");
    let mut rx = session.events();
    session.submit(Command::Edit(EditCommand::CommitGesture { image }));
    wait_for_event(&mut rx, |ev| match ev {
        Event::EditCommitted { image: i, seq, .. } if *i == image => Some(*seq),
        Event::CommandFailed { error, .. } => panic!("commit failed: {error}"),
        _ => None,
    })
}

/// The load-bearing test (spec §5 "first failing test (b)"): open → commit
/// an exposure gesture → close → **move the file to a new directory** →
/// reopen (by content hash) → recipe struct-equal, head_seq intact. Then,
/// **in the same catalog**, a second leg: **rename** the file in place and
/// prove the same restore.
#[test]
fn reopen_restores_recipe_after_move_and_rename() {
    let tmp = TempDir::new().unwrap();
    let lbdata = tmp.path().join("cat.lbdata");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let original = src.join("IMG_0001.jpg");
    std::fs::write(&original, b"raw pixel payload, never touched by lightbox").unwrap();

    // ---- open, edit, commit, close ------------------------------------
    let core = start_core();
    let session = core.create_catalog(&lbdata, None).expect("create");
    let image = import_one(&session, &src);
    let seq = commit_exposure(&session, image, 1.25);
    assert_eq!(seq, 1);
    let committed_recipe = session
        .query()
        .edit_state(image)
        .expect("edit_state")
        .recipe;
    assert_eq!(
        committed_recipe.get(ParamId::Exposure),
        ParamValue::F32(1.25)
    );
    session
        .close(ClosePolicy::Skip.into())
        .expect("close (nothing open, but exercises the auto-commit path too)");

    // ---- LEG 1: move the file to an entirely new directory -------------
    let moved_dir = tmp.path().join("moved-elsewhere");
    std::fs::create_dir_all(&moved_dir).unwrap();
    let moved_path = moved_dir.join("IMG_0001.jpg");
    std::fs::rename(&original, &moved_path).expect("move the file");

    let moved_hash =
        lightbox_decode::hash_file(&moved_path, &CancelToken::new()).expect("hash moved file");

    let core2 = start_core();
    let session2 = core2.open_catalog(&lbdata, None).expect("reopen");
    let resolved = session2
        .edits()
        .store()
        .image_for_content_hash(moved_hash)
        .expect("image_for_content_hash")
        .expect("the moved file's bytes are known — same image, new path");
    assert_eq!(
        resolved, image,
        "content-hash identity survives the move — same image id"
    );
    let state = session2.query().edit_state(resolved).expect("edit_state");
    assert!(state.persisted);
    assert_eq!(state.head_seq, seq, "head_seq intact across the move");
    assert_eq!(
        state.recipe, committed_recipe,
        "recipe struct-equal across the move"
    );
    session2.close(ClosePolicy::Skip.into()).unwrap();

    // ---- LEG 2: rename the file in place (same dir, new filename) ------
    let renamed_path = moved_dir.join("IMG_0001_renamed.jpg");
    std::fs::rename(&moved_path, &renamed_path).expect("rename the file");

    let renamed_hash =
        lightbox_decode::hash_file(&renamed_path, &CancelToken::new()).expect("hash renamed file");
    assert_eq!(
        renamed_hash, moved_hash,
        "renaming doesn't change the file's bytes"
    );

    let core3 = start_core();
    let session3 = core3.open_catalog(&lbdata, None).expect("reopen again");
    let resolved3 = session3
        .edits()
        .store()
        .image_for_content_hash(renamed_hash)
        .expect("image_for_content_hash")
        .expect("the renamed file's bytes are still known");
    assert_eq!(resolved3, image, "same image id across the rename too");
    let state3 = session3.query().edit_state(resolved3).expect("edit_state");
    assert_eq!(state3.head_seq, seq);
    assert_eq!(state3.recipe, committed_recipe);
    session3.close(ClosePolicy::Skip.into()).unwrap();
}

/// The working-set-replacement leg (spec §4.2 D1 invariant, T12 AC): an
/// in-flight gesture on an image that leaves the open working set (without
/// the whole app/session closing) is auto-committed, `EditHub::close`
/// alone, not a full `Session::close`.
#[test]
fn working_set_replacement_auto_commits_open_gesture() {
    let tmp = TempDir::new().unwrap();
    let lbdata = tmp.path().join("cat.lbdata");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("IMG_0001.jpg"), b"payload one").unwrap();

    let core = start_core();
    let session = core.create_catalog(&lbdata, None).expect("create");
    let image = import_one(&session, &src);

    let hub = session.edits();
    hub.open(image).expect("open");
    hub.begin_gesture(image, StepLabel::Param(ParamId::Contrast))
        .expect("begin_gesture");
    hub.update_gesture(image, {
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Contrast, ParamValue::F32(30.0));
        d
    })
    .expect("update_gesture");
    assert!(hub.is_open(image));

    // Simulate the working set replacing this image (a new folder dropped,
    // this image scrolled out / evicted), WITHOUT closing the session or
    // the app. Never submit `CommitGesture` explicitly.
    hub.close(image);
    assert!(
        !hub.is_open(image),
        "the image left the registry with the working set"
    );

    // The session (and catalog) are still fully alive, this proves the
    // auto-commit is a property of `EditHub::close`, not of app shutdown.
    let state = session.query().edit_state(image).expect("edit_state");
    assert!(
        state.persisted,
        "the open gesture was auto-committed when it left the working set"
    );
    assert_eq!(state.head_seq, 1);
    assert_eq!(state.recipe.get(ParamId::Contrast), ParamValue::F32(30.0));

    session.close(ClosePolicy::Skip.into()).unwrap();
}
