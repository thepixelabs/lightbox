// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-cli preview` E2E for E03 Phase F's operability bar (spec §11
//! DoD #7: "`lightbox-cli preview`/`rawcache` subcommands cover build/stat/
//! verify/purge/relocate headlessly"): drives the real binary through
//! `create → import → build → verify → verify --full → relocate → purge
//! --scope`, asserting exit codes and reported counts — the T21/T23-added
//! half of the T14-established `preview` subcommand (`preview.rs`'s own
//! module doc comment).
//!
//! Requires the fixture corpus: `cargo xtask fixtures` (CI fetches it
//! before `cargo test`).

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

fn cli(args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_lightbox-cli"))
        .args(args)
        .output()
        .expect("spawn lightbox-cli")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[track_caller]
fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "expected exit {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout(out),
        stderr(out)
    );
}

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
        assert!(
            src.is_file(),
            "fixture {name} missing — run `cargo xtask fixtures`"
        );
        std::fs::copy(&src, dir.join(name)).unwrap();
    }
}

#[test]
fn build_verify_full_relocate_purge_scope_flow() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("preview_f.lbdata");
    let cat = catalog_path.to_str().unwrap();
    let photos = tmp.path().join("photos");
    // Both have a real embedded JPEG surrogate — T12 AC: a raw source
    // downscales its own T0, a non-raw JPEG source builds T1 directly. A
    // TIFF/PNG source with no embedded surrogate fails the same documented
    // way T0 extraction does (C-10/T12's own scope note) — deliberately
    // not staged here, this test is about the CLI plumbing, not that AC.
    stage_fixtures(&photos, &["canon-eos-r6.cr3", "lightbox-tiny.jpg"]);

    assert_exit(&cli(&["create", "--catalog", cat]), 0);
    let out = cli(&[
        "import",
        "--catalog",
        cat,
        "--add",
        photos.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("imported 2"), "{}", stdout(&out));

    // --- build (tier 1, whole catalog) ---
    let out = cli(&["preview", "build", "--catalog", cat, "--tier", "1"]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("2 ready, 0 failed"),
        "expected 2 clean T1 builds: {}",
        stdout(&out)
    );

    // --- stat ---
    let out = cli(&["preview", "stat", "--catalog", cat, "--json"]);
    assert_exit(&out, 0);
    let stats: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert!(stats["t1_count"].as_u64().unwrap() >= 2);

    // --- verify (quick, default) and verify --full: both clean, both exit 0 ---
    assert_exit(&cli(&["preview", "verify", "--catalog", cat]), 0);
    let out = cli(&["preview", "verify", "--catalog", cat, "--full", "--json"]);
    assert_exit(&out, 0);
    let report: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(report["missing_files"], 0);
    assert_eq!(report["checksum_failures"], 0);

    // --- relocate: files move, old root ends up empty, new root has them ---
    let new_root = tmp.path().join("relocated");
    let out = cli(&[
        "preview",
        "relocate",
        "--catalog",
        cat,
        "--new-root",
        new_root.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("relocated cache store to"),
        "{}",
        stdout(&out)
    );
    let old_previews = catalog_path.join("previews");
    let old_has_files = std::fs::read_dir(&old_previews)
        .map(|mut it| it.any(|e| e.unwrap().path().is_file()))
        .unwrap_or(false)
        || walk_has_files(&old_previews);
    assert!(!old_has_files, "old root must be cleared after relocate");
    assert!(
        walk_has_files(&new_root.join("previews")),
        "new root must hold the files"
    );

    // --- purge --scope rejects a bad value with a usage error ---
    assert_exit(
        &cli(&[
            "preview",
            "purge",
            "--catalog",
            cat,
            "--yes",
            "--scope",
            "bogus",
        ]),
        2,
    );
    // --- purge without --yes refuses (destructive gate) ---
    assert_exit(
        &cli(&["preview", "purge", "--catalog", cat, "--scope", "rawcache"]),
        2,
    );
    // --- purge --scope all: clears preview rows too (0 rawcache rows — none built here) ---
    let out = cli(&[
        "preview",
        "purge",
        "--catalog",
        cat,
        "--yes",
        "--scope",
        "all",
    ]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("purged 3 preview row(s), 0 raw-cache row(s)"),
        "{}",
        stdout(&out)
    );
    let out = cli(&["preview", "stat", "--catalog", cat, "--json"]);
    assert_exit(&out, 0);
    let stats: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    assert_eq!(stats["t1_count"], 0);
}

fn walk_has_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if walk_has_files(&path) {
                return true;
            }
        } else {
            return true;
        }
    }
    false
}
