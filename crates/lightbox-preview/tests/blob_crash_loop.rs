// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `kill -9` fault-injection harness for atomic blob IO (E03 spec §7
//! T02 AC: "crash-loop test (SIGKILL during 1k writes) leaves zero torn
//! visible files"). Same pattern as `lightbox-catalog`'s
//! `tests/fault_injection.rs`: a child process (this binary, re-invoked with
//! `LIGHTBOX_BLOB_CHILD_DIR` set) hammers [`BlobStore::put`] with distinct
//! content on every call; the parent SIGKILLs it at a random moment and
//! inspects the store directory.
//!
//! Unlike the catalog harness, no journal is needed: `BlobStore` entries are
//! content-addressed and `put` is idempotent (spec §5.5), so losing an
//! uncommitted blob to a kill is not a correctness bug — it is a cache miss,
//! trivially rebuildable. The property under test is narrower and checked
//! directly against the filesystem after every kill:
//!
//! 1. **Every *visible* file self-verifies** — "visible" means reachable
//!    through [`BlobStore`]'s own addressing (its final content-hash name);
//!    `BlobStore::get` never scans a directory or looks up a `.tmp-*` name,
//!    so an orphaned temp file is not "visible" in the sense the spec's AC
//!    means. Any file present under its **final** name must re-hash to that
//!    same name — a torn file that somehow ended up under the final name
//!    (impossible via `rename`, but this harness is what would catch it if
//!    it weren't) fails this check.
//! 2. **A kill mid-write can only ever orphan a `.tmp-*` file, never corrupt
//!    an existing final-named blob** — asserted by construction: a
//!    kill lands either before the temp file is created (no trace at all),
//!    after it's written but before `rename` (a harmless orphaned temp
//!    file — logged, not failed), or after `rename` completes (a fully
//!    valid final file, covered by check 1). Reconciling/sweeping orphaned
//!    temp files on an idle tick is `verify_store(Full)`'s job (spec §3.2,
//!    Phase F) — out of Phase A's scope; this harness only proves they never
//!    masquerade as valid content.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use lightbox_preview::{BlobNamespace, PreviewStoreConfig, Store};

const CHILD_DIR_ENV: &str = "LIGHTBOX_BLOB_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_BLOB_FAULT_ITERS";
/// Spec T02 AC names "1k writes" per crash-loop.
const WRITES_PER_CHILD: u32 = 1000;

/// Child-mode entry point: a no-op under normal `cargo test`.
#[test]
fn blob_fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_workload(Path::new(&dir)) {
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("blob fault child failed: {err:?}");
    }
}

fn child_workload(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = PreviewStoreConfig::with_defaults(dir.join("store.lbdata"));
    let store = Store::open(&cfg)?;
    let blobs = store.blob_store(BlobNamespace::Masks);

    let run_id = u64::from(std::process::id()) ^ nanos_now();
    for i in 0..WRITES_PER_CHILD {
        // Distinct content every call so each iteration is a genuinely new
        // atomic write (content-addressing would make a repeat a no-op).
        let payload = format!("run={run_id} i={i} {}", "x".repeat((i % 97) as usize));
        blobs.put(payload.as_bytes())?;
    }
    Ok(())
}

/// The parent: spawn -> random sleep -> SIGKILL -> reopen -> scan.
#[test]
fn kill9_during_blob_writes_leaves_zero_torn_visible_files() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only blob_fault_child runs there
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());
    let mut total_files_seen_ever = 0usize;
    let mut total_temp_files_ever = 0usize;

    for iteration in 0..iterations {
        let mut child = std::process::Command::new(&exe)
            .args(["blob_fault_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn blob fault child");

        std::thread::sleep(Duration::from_millis(rng.u64(1..=40)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // Reopening must succeed (the manifest write is itself atomic).
        let cfg = PreviewStoreConfig::with_defaults(dir.join("store.lbdata"));
        let _store = Store::open(&cfg).unwrap_or_else(|e| {
            panic!("iteration {iteration}: reopening the store after a kill failed: {e}")
        });

        let (files, temp_files) = scan(&dir.join("store.lbdata"));
        total_temp_files_ever += temp_files;
        for (path, hex_name, bytes) in &files {
            if path.file_name().and_then(|n| n.to_str()) == Some("store.toml") {
                continue; // the manifest, not a content-addressed blob
            }
            let actual = twox_hash::XxHash3_128::oneshot(bytes).to_be_bytes();
            let actual_hex = hex(&actual);
            assert_eq!(
                &actual_hex,
                hex_name,
                "iteration {iteration}: {} is a TORN/corrupt visible file — its name does not \
                 match its own content hash",
                path.display()
            );
        }
        total_files_seen_ever += files.len();
    }

    assert!(
        total_files_seen_ever > 0,
        "harness never observed any blob file — the workload never got to write anything"
    );
    eprintln!(
        "blob crash loop: {iterations} kills x up to {WRITES_PER_CHILD} writes each, \
         0 torn (self-verification-failing) visible files across \
         {total_files_seen_ever} file-observations, {total_temp_files_ever} harmless \
         orphaned temp file(s) (expected residue of a kill mid-write; swept by \
         verify_store(Full), Phase F, not asserted here)"
    );
}

/// Walks `masks/` under the store root; returns `(files, temp_file_count)`
/// where each file is `(path, expected_hex_name, contents)`.
fn scan(store_root: &Path) -> (Vec<(std::path::PathBuf, String, Vec<u8>)>, usize) {
    let mut files = Vec::new();
    let mut temp_files = 0;
    let mut stack = vec![store_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            if name.starts_with(".tmp-") {
                temp_files += 1;
                continue;
            }
            let bytes = std::fs::read(&path).unwrap_or_default();
            files.push((path, name, bytes));
        }
    }
    (files, temp_files)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
