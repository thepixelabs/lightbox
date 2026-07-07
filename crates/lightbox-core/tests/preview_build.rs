// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E03 Phase D (T14) headless end-to-end: `create → import fixture →
//! Command::BuildPreviews{tier: 1} → PreviewReady events → Queries::
//! cache_stats reports rows/bytes matching disk` — the exact scenario the
//! T14 acceptance criterion names, driven at the `lightbox-core` façade
//! level (the same seam `lightbox-cli preview build/stat` rides on).
//!
//! Requires the fixture corpus: `cargo xtask fixtures`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightbox_core::{
    BuildPriority, CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageQuery, Session,
    SortOrder, Tier,
};
use lightbox_types::ImageId;
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

fn stage_fixtures(dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let fixtures = fixtures_dir();
    for name in names {
        let src = fixtures.join(name);
        assert!(src.is_file(), "fixture {name} missing");
        std::fs::copy(&src, dir.join(name)).unwrap();
    }
}

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

fn all_images(session: &Session) -> Vec<ImageId> {
    let page = session
        .query()
        .images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::FilenameAsc,
            cursor: None,
            limit: 100,
        })
        .expect("images_page");
    page.items.iter().map(|s| s.id).collect()
}

#[test]
fn import_build_previews_t1_then_stats_matches_disk() {
    let tmp = TempDir::new().unwrap();
    let import_src = tmp.path().join("photos");
    // A raw with a full embedded preview + a plain JPEG (T1 builds directly
    // from it, no T0 row — spec §3.1) + a raw with NO embedded preview
    // (typed-failure coverage): the same corpus shape `render_end_to_end.rs`
    // already exercises.
    stage_fixtures(
        &import_src,
        &["canon-eos-r6.cr3", "lightbox-tiny.jpg", "sigma-fp.dng"],
    );

    let core = Core::start(CoreConfig::default()).expect("core start");
    let session = core
        .create_catalog(&tmp.path().join("test.lbdata"), None)
        .expect("create catalog");
    import_dir(&session, &import_src);

    let images = all_images(&session);
    assert_eq!(images.len(), 3, "all three assets registered");

    let mut rx = session.events();
    session.submit(Command::BuildPreviews {
        images: images.clone(),
        tier: Tier::T1,
        priority: BuildPriority::Visible,
    });

    // Every image resolves to exactly one terminal outcome (Ready or
    // Failed — the DNG with no embedded preview is expected to fail
    // typed, not silently vanish).
    let mut ready = 0usize;
    let mut failed = 0usize;
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while ready + failed < images.len() {
        assert!(
            Instant::now() < deadline,
            "preview builds never finished (ready={ready}, failed={failed})"
        );
        match rx.try_recv() {
            Ok(Event::PreviewReady { tier, .. }) => {
                assert_eq!(tier, Tier::T1);
                ready += 1;
            }
            Ok(Event::PreviewFailed { tier, .. }) => {
                assert_eq!(tier, Tier::T1);
                failed += 1;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("event stream broke while building previews: {e}"),
        }
    }
    assert_eq!(ready, 2, "CR3 + JPEG build a T1 successfully");
    assert_eq!(
        failed, 1,
        "the no-embedded-preview DNG fails typed, not silently"
    );

    // `Queries::cache_stats` — fresh from the catalog, must match what's
    // actually on disk (T14 AC's literal wording).
    let stats = session.query().cache_stats().expect("cache_stats");
    assert_eq!(stats.t1_count, 2, "{stats:?}");
    assert!(stats.t1_bytes > 0);
    // The CR3's T1 goes through its own T0 first (spec §3.1: raw source →
    // downscale the stored embedded JPEG); the plain JPEG source does not.
    assert_eq!(stats.t0_count, 1, "{stats:?}");

    let lbdata = tmp.path().join("test.lbdata");
    let previews_dir = lbdata.join("previews");
    let on_disk_files = count_files(&previews_dir);
    // T0 (1) + T1 (2) = 3 files on disk, matching the row counts above.
    assert_eq!(
        on_disk_files, 3,
        "row counts must match what's actually on disk"
    );

    // `Queries::preview_state` resolves the best-available descriptor for
    // one image without any extra IO beyond the RAM index.
    let cr3 = images
        .iter()
        .copied()
        .find(|&id| {
            session
                .query()
                .preview_state(id)
                .is_some_and(|d| d.tier == lightbox_core::Tier::T1)
        })
        .expect("at least one image resolves a T1 preview_state");
    let desc = session.query().preview_state(cr3).unwrap();
    assert_eq!(desc.tier, Tier::T1);

    session
        .close(CloseOpts::with_backup(ClosePolicy::Skip))
        .unwrap();
}

fn count_files(root: &Path) -> usize {
    fn walk(p: &Path, n: &mut usize) {
        let Ok(entries) = std::fs::read_dir(p) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, n);
            } else {
                *n += 1;
            }
        }
    }
    let mut n = 0;
    walk(root, &mut n);
    n
}
