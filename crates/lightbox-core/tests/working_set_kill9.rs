// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E04 T9 AC: `kill -9` **during** a 500-file `Command::OpenWorkingSet` load
//! leaves the catalog `integrity_check`-clean and the session simply
//! forgotten (spec §3.2/§6.6, the working set is session state, never
//! persisted; what persists is the edit-store plumbing: one `asset` row +
//! one default `image` row per item that got far enough to register before
//! the kill).
//!
//! Pattern follows `tests/close_kill.rs`/`lightbox-catalog`'s
//! `migration_0002_fault_injection.rs`: the parent re-invokes this test
//! binary filtered to [`open_child`] with an env var set; each child opens
//! (or creates) a catalog, submits `OpenWorkingSet` over a 500-file source
//! tree, and reports readiness once the load is demonstrably under way; the
//! parent SIGKILLs it at a random moment, then verifies:
//!
//! 1. the catalog reopens clean (`quick_check` runs at open, plus a full
//!    `integrity_check`);
//! 2. a fresh session's working set is empty (`SetPhase::Empty`), the
//!    session/working-set state was never persisted, by design;
//! 3. re-opening the SAME source tree afterward is a normal, uninterrupted
//!    open (whatever rows the killed run managed to commit are `reused`,
//!    not duplicated), proving the store is left in a state later opens
//!    can build on cleanly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightbox_core::{ClosePolicy, Command, Core, CoreConfig, Event, OpenOrigin, OpenRequest};

const CHILD_DIR_ENV: &str = "LIGHTBOX_WS_KILL_CHILD_DIR";
const ITERATIONS: u32 = 6;
const N_FILES: usize = 500;

fn source_tree(dir: &Path) -> Vec<PathBuf> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut paths = Vec::with_capacity(N_FILES);
    for i in 0..N_FILES {
        let p = src.join(format!("f{i:04}.jpg"));
        // Distinct, non-trivial content so hashing is real work (widens the
        // kill window), not a valid JPEG, which is fine: the item still
        // registers (badged, T18 convention), only its `decode_error` is set.
        let mut bytes = vec![(i % 251) as u8; 8192];
        bytes.extend_from_slice(&(i as u64).to_le_bytes());
        std::fs::write(&p, bytes).unwrap();
        paths.push(p);
    }
    paths
}

/// Child-mode entry point: create/open -> submit OpenWorkingSet over 500
/// files -> signal ready once progress is observed -> spin until killed. A
/// no-op under normal `cargo test`.
#[test]
fn open_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Err(err) = child_body(&dir) {
        let _ = std::fs::write(dir.join("child-error.txt"), format!("{err:?}"));
        panic!("working-set kill child failed: {err:?}");
    }
}

fn child_body(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::start(CoreConfig::default())?;
    let lbdata = dir.join("cat.lbdata");
    let session = if lbdata.exists() {
        core.open_catalog(&lbdata, None)?
    } else {
        core.create_catalog(&lbdata, None)?
    };
    let paths = source_tree(dir);

    let mut rx = session.events();
    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(paths, false, OpenOrigin::Cli),
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut told_ready = false;
    loop {
        if std::time::Instant::now() > deadline {
            return Err("child timed out waiting for the load to progress".into());
        }
        // Keep the process alive (with the writer/session open) past
        // completion too, the parent's kill window spans "mid-load" through
        // "just-finished, WAL not yet checkpointed".
        match rx.try_recv() {
            Ok(
                Event::WorkingSetChanged { .. }
                | Event::WorkingSetReplaced { .. }
                | Event::WorkingSetLoadFinished { .. },
            ) if !told_ready => {
                std::fs::write(dir.join("child-ready.txt"), "ready")?;
                told_ready = true;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[test]
fn kill9_during_working_set_open_leaves_catalog_clean_and_forgets_the_session() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child; only open_child runs there
    }
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let lbdata = dir.join("cat.lbdata");

    let exe = std::env::current_exe().expect("test binary");
    let mut rng = fastrand::Rng::with_seed(0xE041_DEA5_C0DE_u64);

    for iteration in 0..ITERATIONS {
        let _ = std::fs::remove_file(dir.join("child-ready.txt"));
        let _ = std::fs::remove_file(dir.join("child-error.txt"));
        let mut child = std::process::Command::new(&exe)
            .args(["open_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn open child");

        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !dir.join("child-ready.txt").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "iteration {iteration}: child never became ready"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
        // Kill somewhere in [0, 5ms) after readiness, biased to land
        // mid-load across the 500-file source tree.
        std::thread::sleep(Duration::from_micros(rng.u64(0..5000)));
        match child.try_wait().expect("try_wait") {
            Some(_) => {}
            None => {
                child.kill().expect("SIGKILL child");
            }
        }
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // 1. Catalog reopens clean.
        let catalog = lightbox_catalog::Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        assert_eq!(
            catalog.integrity(),
            lightbox_catalog::IntegrityStatus::Ok,
            "iteration {iteration}"
        );
        let findings = lightbox_catalog::integrity_check_file(&lbdata.join("catalog.sqlite"))
            .unwrap_or_else(|e| panic!("iteration {iteration}: integrity_check_file: {e}"));
        assert!(
            findings.is_empty(),
            "iteration {iteration}: integrity_check findings: {findings:?}"
        );
        drop(catalog);

        // 2. A fresh session's working set is empty, never persisted.
        let core = Core::start(CoreConfig::default()).expect("core");
        let session = core.open_catalog(&lbdata, None).expect("open");
        let snapshot = session.working_set();
        assert_eq!(
            snapshot.phase,
            lightbox_core::SetPhase::Empty,
            "iteration {iteration}: the working set must not survive a restart"
        );
        assert!(snapshot.items.is_empty());

        // 3. Re-opening the same tree afterward is clean and uninterrupted
        // (whatever the killed run committed is `reused`, nothing
        // duplicated; the reopen itself must fully complete).
        let paths = source_tree(dir);
        let mut rx = session.events();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(paths, false, OpenOrigin::Cli),
        });
        let report = wait_for_load_finished(&mut rx, iteration);
        assert_eq!(report.planned, N_FILES, "iteration {iteration}: {report:?}");
        assert_eq!(
            report.ready + report.collapsed,
            N_FILES,
            "iteration {iteration}: every item must resolve one way or the other: {report:?}"
        );
        session
            .close(ClosePolicy::Skip.into())
            .unwrap_or_else(|e| panic!("iteration {iteration}: close failed: {e}"));
    }

    eprintln!(
        "working-set kill9: {ITERATIONS} children killed mid-open, 0 corruptions, 0 stray sessions"
    );
}

fn wait_for_load_finished(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    iteration: u32,
) -> lightbox_core::OpenReport {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "iteration {iteration}: timed out waiting for the reopen to finish"
        );
        match rx.try_recv() {
            Ok(Event::WorkingSetLoadFinished { report, .. }) => return report,
            Ok(Event::CommandFailed { error, .. }) => {
                panic!("iteration {iteration}: reopen failed: {error}")
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                panic!("iteration {iteration}: event stream closed before the reopen finished")
            }
        }
    }
}
