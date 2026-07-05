// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! End-to-end render through the headless session (E01 spec §5 T23 AC:
//! "Engine renders fixture previews end-to-end"): catalog → import real
//! fixtures → `Session::engine()` → `Engine::submit`/`poll` with
//! `RenderTarget::CpuBuffer` — the exact path `lightbox-cli render` (T27)
//! and the shell's loupe (T26) ride on. No UI type anywhere (seam 1).
//!
//! Requires the fixture corpus: `cargo xtask fixtures` (CI fetches it
//! before `cargo test`, same as the decode/ingest fixture tests).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightbox_core::{
    CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageQuery, Session, SortOrder,
};
use lightbox_edit::Recipe;
use lightbox_render::{
    RenderError, RenderOutput, RenderRequest, RenderScale, RenderState, RenderTarget, RenderTicket,
    Roi, ViewportId,
};
use lightbox_types::{ImageId, PV_M0};
use tempfile::TempDir;

const EVENT_TIMEOUT: Duration = Duration::from_secs(60);

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").is_file(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

/// Copies the named fixtures into a fresh import directory.
fn stage_fixtures(dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let fixtures = fixtures_dir();
    for name in names {
        let src = fixtures.join(name);
        assert!(
            src.is_file(),
            "fixture {name} missing — run `cargo xtask fixtures`"
        );
        std::fs::copy(&src, dir.join(name)).unwrap();
    }
}

/// Imports `src` through the command bus and waits for completion.
fn import_dir(session: &Session, src: &Path) {
    let mut rx = session.events();
    session.submit(Command::ImportAddInPlace {
        source_dir: src.to_path_buf(),
        recursive: false,
    });
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "import never finished");
        match rx.try_recv() {
            Ok(Event::ImportFinished { .. }) => return,
            Ok(Event::CommandFailed { error, .. }) => panic!("import failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("event stream broke while importing: {e}"),
        }
    }
}

/// Image id of the given filename, via the query façade.
fn image_by_filename(session: &Session, filename: &str) -> ImageId {
    let page = session
        .query()
        .images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::FilenameAsc,
            cursor: None,
            limit: 100,
        })
        .expect("images_page");
    page.items
        .iter()
        .find(|s| s.filename == filename)
        .unwrap_or_else(|| panic!("{filename} not in catalog"))
        .id
}

fn request(image: ImageId, scale: RenderScale, viewport: u64) -> RenderRequest {
    RenderRequest {
        image,
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi::Full,
        scale,
        target: RenderTarget::CpuBuffer,
        viewport: ViewportId(viewport),
    }
}

fn wait_terminal(session: &Session, ticket: &RenderTicket) -> RenderState {
    let engine = session.engine();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match engine.poll(ticket) {
            RenderState::Pending | RenderState::Running => {
                assert!(Instant::now() < deadline, "render never became terminal");
                std::thread::sleep(Duration::from_millis(1));
            }
            terminal => return terminal,
        }
    }
}

#[test]
fn session_renders_fixture_previews_through_the_engine() {
    let tmp = TempDir::new().unwrap();
    let import_src = tmp.path().join("photos");
    // A raw with a full-size embedded preview, a JPEG original (its own
    // preview), and a raw with NO usable embedded preview (Sigma fp DNG).
    stage_fixtures(
        &import_src,
        &["canon-eos-r6.cr3", "lightbox-tiny.jpg", "sigma-fp.dng"],
    );

    let core = Core::start(CoreConfig::default()).expect("core start");
    let session = core
        .create_catalog(&tmp.path().join("test.lbdata"), None)
        .expect("create catalog");
    import_dir(&session, &import_src);

    // --- The raw: 3408×2272 (orientation 1) fit into 240×160 exactly. ---
    let cr3 = image_by_filename(&session, "canon-eos-r6.cr3");
    let ticket =
        session
            .engine()
            .submit(request(cr3, RenderScale::FitWithin { w: 240, h: 160 }, 1));
    let buf = match wait_terminal(&session, &ticket) {
        RenderState::Ready(RenderOutput::Cpu(buf)) => buf,
        other => panic!("expected Ready(Cpu) for the CR3, got {other:?}"),
    };
    assert_eq!(
        (buf.width, buf.height),
        (240, 160),
        "3:2 fit within 240×160"
    );
    assert_eq!(buf.px.len(), 240 * 160 * 4);
    assert!(
        buf.px.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
        "rendered preview has content"
    );
    assert!(
        buf.px.chunks_exact(4).all(|p| p[3] == 255),
        "JPEG-sourced render is opaque"
    );

    // Byte-stable across runs on the same machine (T24 determinism AC — the
    // `lightbox-cli render --cpu` byte-stability check rides on this path).
    let ticket =
        session
            .engine()
            .submit(request(cr3, RenderScale::FitWithin { w: 240, h: 160 }, 2));
    match wait_terminal(&session, &ticket) {
        RenderState::Ready(RenderOutput::Cpu(again)) => {
            assert_eq!(again, buf, "CPU render must be byte-stable");
        }
        other => panic!("expected Ready(Cpu) on re-render, got {other:?}"),
    }

    // --- The JPEG original serves as its own preview (16×16 → fit 8×8). ---
    let jpg = image_by_filename(&session, "lightbox-tiny.jpg");
    let ticket = session
        .engine()
        .submit(request(jpg, RenderScale::FitWithin { w: 8, h: 8 }, 3));
    match wait_terminal(&session, &ticket) {
        RenderState::Ready(RenderOutput::Cpu(buf)) => {
            assert_eq!((buf.width, buf.height), (8, 8));
        }
        other => panic!("expected Ready(Cpu) for the JPEG, got {other:?}"),
    }

    // --- No usable embedded preview → a Source failure, never a crash. ---
    let dng = image_by_filename(&session, "sigma-fp.dng");
    let ticket = session
        .engine()
        .submit(request(dng, RenderScale::FitWithin { w: 64, h: 64 }, 4));
    match wait_terminal(&session, &ticket) {
        RenderState::Failed(RenderError::Source(msg)) => {
            assert!(
                msg.contains("no source pixels"),
                "expected NotFound-shaped source error, got {msg:?}"
            );
        }
        other => panic!("expected Failed(Source) for the previewless DNG, got {other:?}"),
    }

    // --- Unknown image id → a Source failure through the same lifecycle. ---
    let ticket = session.engine().submit(request(
        ImageId(9_999_999),
        RenderScale::FitWithin { w: 64, h: 64 },
        5,
    ));
    assert!(
        matches!(
            wait_terminal(&session, &ticket),
            RenderState::Failed(RenderError::Source(_))
        ),
        "unknown image must fail as a source error"
    );

    session
        .close(CloseOpts::with_backup(ClosePolicy::Skip))
        .expect("close");
}
