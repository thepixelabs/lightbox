// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E03 Phase F (T21) AC, literal text: "kill mid-relocate then reopen →
//! relocation resumes and completes, zero lost entries." `relocate.rs`'s own
//! unit tests prove resumability by hand-seeding a journal to *simulate* a
//! killed-mid-copy state; this harness instead performs a REAL `SIGKILL`
//! against a child process actually running [`crate::relocate::relocate`]
//! partway through a real multi-hundred-file copy, then resumes on the
//! (unkilled) parent thread, same child/parent convention as
//! `t2_crash_loop.rs`/`store_crash_loop.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::relocate::{relocate, ProgressSink};
use crate::store::RESERVED_DIRS;

const CHILD_DIR_ENV: &str = "LIGHTBOX_RELOCATE_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_RELOCATE_FAULT_ITERS";
/// Enough files, each large enough to force real fsync-bound wall-clock
/// time, that a randomized kill reliably lands mid-copy rather than racing
/// past the whole run before the parent's sleep even elapses.
const N_FILES: usize = 150;

#[test]
fn relocate_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    let old = Path::new(&dir).join("old");
    let new = Path::new(&dir).join("new");
    let progress: ProgressSink = Arc::new(|_p| {});
    if let Err(err) = relocate(&old, &new, &progress) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("relocate fault child failed: {err:?}");
    }
}

fn seed_store(root: &Path, n_files: usize) {
    for dir in RESERVED_DIRS {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for i in 0..n_files {
        let hh = format!("{:02x}", i % 16);
        let dir = root.join("previews").join(&hh);
        std::fs::create_dir_all(&dir).unwrap();
        // ~12 KiB/file: big enough that 600 files' worth of fsync-bound
        // copies take real wall-clock time (a randomized short kill window
        // then reliably lands mid-run rather than racing past it).
        let payload = format!("payload-{i}-").repeat(1024);
        std::fs::write(dir.join(format!("f{i}.t0.jpg")), payload).unwrap();
    }
    std::fs::write(
        root.join("store.toml"),
        "format = 1\nstore_uuid = \"x\"\ncreated_by = \"t\"\n",
    )
    .unwrap();
}

/// Every FINAL-named (non-`.tmp-*`) file under `previews/`, keyed by path
/// relative to `root`. A kill can legitimately land mid-`atomic_write`
/// inside `relocate`'s own per-file copy and orphan a `.tmp-*` at the
/// destination (same A-13-documented residue every other atomic-write path
/// in this crate accepts, swept by `verify_store(Full)`, not `relocate`
/// itself); such a file is never "lost data" (its final-named counterpart
/// either already exists or gets (re)written on resume) so it is excluded
/// here, matching `blob_crash_loop.rs`'s convention of tracking temp-file
/// residue separately from the correctness property under test.
fn all_payloads(root: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut stack = vec![root.join("previews")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with(".tmp-") {
                continue; // harmless orphan residue, not part of this property
            }
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            out.insert(rel, std::fs::read_to_string(&p).unwrap_or_default());
        }
    }
    out
}

/// The parent: seed -> spawn child mid-relocate -> `SIGKILL` -> resume on
/// this thread -> verify zero lost entries.
#[test]
fn kill9_mid_relocate_then_reopen_resumes_and_completes_with_zero_lost_entries() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only relocate_fault_child runs there
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());

    for iteration in 0..iterations {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let old = dir.join("old");
        let new = dir.join("new");
        seed_store(&old, N_FILES);
        let before = all_payloads(&old);
        assert_eq!(before.len(), N_FILES);

        let mut child = std::process::Command::new(&exe)
            .args([
                "relocate_crash_loop::relocate_fault_child",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn relocate fault child");

        std::thread::sleep(Duration::from_millis(rng.u64(5..=120)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // Resume + complete on THIS (unkilled) thread, the literal T21 AC.
        let progress: ProgressSink = Arc::new(|_p| {});
        relocate(&old, &new, &progress)
            .unwrap_or_else(|e| panic!("iteration {iteration}: resume failed: {e}"));

        let after = all_payloads(&new);
        if before != after {
            let missing: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
            let extra: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
            let changed: Vec<_> = before
                .iter()
                .filter(|(k, v)| after.get(*k).is_some_and(|av| av != *v))
                .map(|(k, v)| {
                    let av = &after[k];
                    (
                        k.clone(),
                        v.len(),
                        av.len(),
                        v.chars().zip(av.chars()).position(|(a, b)| a != b),
                    )
                })
                .collect();
            panic!(
                "iteration {iteration}: zero lost entries violated — missing={missing:?} \
                 extra={extra:?} changed(key,before_len,after_len,first_diff_idx)={changed:?}"
            );
        }
        assert!(
            all_payloads(&old).is_empty(),
            "iteration {iteration}: old root must be cleared after a completed relocation"
        );
    }

    eprintln!(
        "relocate crash loop: {iterations} kills mid-relocate ({N_FILES} files/run), \
         0 lost entries after resume+complete every time"
    );
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
