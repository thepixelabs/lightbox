// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! End-to-end render through the headless session (E01 spec §5 T23 AC,
//! carried forward by E05 Phase F5: "Engine renders fixture previews
//! end-to-end"): catalog → import real fixtures → `Session::engine()` →
//! `Engine::submit`/`poll` with `RenderTarget::Buffer`, the exact path
//! `lightbox-cli render` and the shell's loupe/canvas ride on. No UI type
//! anywhere (seam 1).
//!
//! **F5 note:** this now drives `lightbox_render::ng::Engine` (the E05
//! node-graph engine), not the E01 seed, see
//! `docs/plan/epics/E05-deviations.md`.
//!
//! Requires the fixture corpus: `cargo xtask fixtures` (CI fetches it
//! before `cargo test`, same as the decode/ingest fixture tests).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightbox_core::{
    CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageQuery, Session, SortOrder,
};
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    Extent, OutFormat, OutputPayload, RenderError, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, RenderTicket, Roi, SourceError,
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

fn request(image: ImageId, w: u32, h: u32) -> RenderRequest {
    RenderRequest {
        image,
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi { x: 0, y: 0, w, h },
        scale: RenderScale::Fit(Extent { w, h }),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

fn wait_terminal(session: &Session, ticket: &RenderTicket) -> RenderState {
    let engine = session.engine();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match engine.poll(ticket) {
            RenderState::Queued | RenderState::Rendering { .. } => {
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

    // --- The raw: renders at the requested fit extent. ---
    let cr3 = image_by_filename(&session, "canon-eos-r6.cr3");
    let ticket = session.engine().submit(request(cr3, 240, 160));
    let buf = match wait_terminal(&session, &ticket) {
        RenderState::Complete(out) => match out.payload {
            OutputPayload::Pixels(px) => px,
            other => panic!("expected Pixels for a Buffer target, got {other:?}"),
        },
        other => panic!("expected Complete for the CR3, got {other:?}"),
    };
    assert_eq!(
        (buf.extent.w, buf.extent.h),
        (240, 160),
        "renders at the requested extent"
    );
    assert_eq!(buf.bytes.len(), 240 * 160 * 4);
    assert!(
        buf.bytes.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
        "rendered preview has content"
    );
    assert!(
        buf.bytes.chunks_exact(4).all(|p| p[3] == 255),
        "JPEG-sourced render is opaque"
    );

    // Byte-stable across runs on the same machine (T24 determinism AC, the
    // `lightbox-cli render --cpu` byte-stability check rides on this path).
    let ticket = session.engine().submit(request(cr3, 240, 160));
    match wait_terminal(&session, &ticket) {
        RenderState::Complete(out) => match out.payload {
            OutputPayload::Pixels(again) => {
                assert_eq!(again.bytes, buf.bytes, "CPU render must be byte-stable");
            }
            other => panic!("expected Pixels, got {other:?}"),
        },
        other => panic!("expected Complete on re-render, got {other:?}"),
    }

    // --- The JPEG original serves as its own preview (16×16 → fit 8×8). ---
    let jpg = image_by_filename(&session, "lightbox-tiny.jpg");
    let ticket = session.engine().submit(request(jpg, 8, 8));
    match wait_terminal(&session, &ticket) {
        RenderState::Complete(out) => match out.payload {
            OutputPayload::Pixels(px) => {
                assert_eq!((px.extent.w, px.extent.h), (8, 8));
            }
            other => panic!("expected Pixels, got {other:?}"),
        },
        other => panic!("expected Complete for the JPEG, got {other:?}"),
    }

    // --- No usable embedded preview → a Source failure, never a crash. ---
    let dng = image_by_filename(&session, "sigma-fp.dng");
    let ticket = session.engine().submit(request(dng, 64, 64));
    match wait_terminal(&session, &ticket) {
        RenderState::Failed(RenderError::Source(SourceError::NotFound)) => {}
        other => panic!("expected Failed(Source(NotFound)) for the previewless DNG, got {other:?}"),
    }

    // --- Unknown image id → a Source failure through the same lifecycle. ---
    let ticket = session.engine().submit(request(ImageId(9_999_999), 64, 64));
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

/// **Regression (owner-reported): "I can't straighten an image, render
/// failed: compile: no node registered for NodeId geom.warp under process
/// version".**
///
/// `Session` used to hand-enumerate its `NodeRegistry` (source, resize,
/// display, plus `register_global_nodes`) rather than calling the render
/// crate's own `shipping_registry`. E11's `register_geometry_nodes` was
/// added to the latter and never to the copy, so the shipped app carried a
/// registry with no `geom.warp` / `geom.crop`: Straighten and Crop compiled
/// to nodes the app could not construct and failed at render time.
///
/// Nothing caught it because every other test builds its engine from
/// `shipping_registry` / `shipping_compiler`, the one thing the app did
/// not do. This test therefore goes through **`Session::engine()`**
/// deliberately: it is the app's own registry that has to satisfy the
/// recipe, which is the property that was actually broken. A compile-only
/// test against `shipping_compiler` would have passed throughout.
#[test]
fn session_renders_geometry_stages_straighten_and_crop() {
    let tmp = TempDir::new().unwrap();
    let import_src = tmp.path().join("photos");
    stage_fixtures(&import_src, &["lightbox-tiny.jpg"]);

    let core = Core::start(CoreConfig::default()).expect("core start");
    let session = core
        .create_catalog(&tmp.path().join("geom.lbdata"), None)
        .expect("create catalog");
    import_dir(&session, &import_src);
    let image = image_by_filename(&session, "lightbox-tiny.jpg");

    // Straighten on its own (`geom.warp`), the reported failure.
    let mut straighten = Recipe::identity(PV_M0);
    straighten.geometry.angle = 4.0;
    let mut req = request(image, 64, 64);
    req.recipe = straighten;
    match wait_terminal(&session, &session.engine().submit(req)) {
        RenderState::Complete(_) => {}
        other => panic!(
            "straighten must render through the session's own registry, got {other:?} \
             — a `no node registered for NodeId geom.warp` compile error here means \
             the app's registry has drifted from `shipping_registry` again"
        ),
    }

    // Crop on its own (`geom.crop`), broken by the same omission, and
    // never reported only because the owner reached for Straighten first.
    let mut crop = Recipe::identity(PV_M0);
    crop.geometry.crop.left = 0.2;
    crop.geometry.crop.top = 0.2;
    crop.geometry.crop.right = 0.8;
    crop.geometry.crop.bottom = 0.8;
    let mut req = request(image, 64, 64);
    req.recipe = crop;
    match wait_terminal(&session, &session.engine().submit(req)) {
        RenderState::Complete(_) => {}
        other => panic!("crop (geom.crop) must render through the session, got {other:?}"),
    }
}
