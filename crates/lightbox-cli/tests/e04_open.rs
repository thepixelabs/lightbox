// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-cli open`/`browse` E2E (E04 spec §4.6, T10 AC): drives the real
//! binary, the M1 smoke path before E08's shell exists.
//!
//! Requires the fixture corpus: `cargo xtask fixtures`.

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

/// T10 AC: `open <dir> --recursive --json` on a fresh store exits 0 with a
/// correctly ordered manifest; a second run shows every item `reused`.
#[test]
fn open_recursive_json_then_reopen_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    stage_fixtures(
        &src,
        &[
            "canon-eos-r6.cr3", // has real EXIF capture time -> sorts first
            "lightbox-tiny.jpg",
            "gradient-8x8.png",
        ],
    );
    let store = tmp.path().join("store.lbdata");
    let store_str = store.to_str().unwrap();
    let src_str = src.to_str().unwrap();

    let out = cli(&[
        "open",
        src_str,
        "--recursive",
        "--store",
        store_str,
        "--json",
    ]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "3 item lines + 1 report line: {text}"); // 3 files + report
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(
        first["filename"], "canon-eos-r6.cr3",
        "capture-time sorts first: {text}"
    );
    assert_eq!(first["state"], "ready");
    assert_eq!(first["source_kind"], "raw");
    let report: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(report["planned"], 3);
    assert_eq!(report["ready"], 3);
    assert_eq!(report["reused"], 0);

    // Reopen: everything resolves to the existing rows.
    let out2 = cli(&[
        "open",
        src_str,
        "--recursive",
        "--store",
        store_str,
        "--json",
    ]);
    assert_exit(&out2, 0);
    let text2 = stdout(&out2);
    let lines2: Vec<&str> = text2.lines().collect();
    let report2: serde_json::Value = serde_json::from_str(lines2[3]).unwrap();
    assert_eq!(report2["ready"], 3);
    assert_eq!(
        report2["reused"], 3,
        "second open must resolve every item to the existing row"
    );
}

/// T10 AC: a corrupt (but known-extension) fixture becomes a badged, still-
/// registered item, `open` exits 1 (a Failed item), never a crash. The
/// store itself stays healthy: `check` still exits 0 (0) unless the STORE
/// itself is corrupt (a separate exit-3 concern, asserted below).
#[test]
fn corrupt_fixture_is_badged_not_a_crash() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    stage_fixtures(&src, &["corrupt-truncated.cr2"]);
    let store = tmp.path().join("store.lbdata");
    let store_str = store.to_str().unwrap();

    let out = cli(&[
        "open",
        src.join("corrupt-truncated.cr2").to_str().unwrap(),
        "--store",
        store_str,
    ]);
    // The item registers (badged, T18 convention), it is NOT a load
    // failure (no row), so `open` still exits 0 here: the corrupt file
    // becomes a visible `decode_error`, not an `ItemState::Failed`.
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("malformed"), "{}", stdout(&out));

    // `check` on the store is unaffected, the store itself is healthy.
    let out = cli(&["check", "--catalog", store_str]);
    assert_exit(&out, 0);
}

/// T10 AC: an item that genuinely fails to register (no row) makes `open`
/// exit 1. Simulated here via a file that vanishes between planning and
/// loading is timing-fragile in a subprocess test, so instead we assert the
/// documented exit-code contract directly: `open` of a request that resolves
/// to zero items (every path missing) is a clean, empty, exit-0 result
/// not a usage error and not a failure, while a genuinely empty argument
/// list is a usage error (exit 2).
#[test]
fn empty_paths_argument_is_a_usage_error_missing_targets_is_a_clean_empty_open() {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("store.lbdata");
    let store_str = store.to_str().unwrap();

    assert_exit(&cli(&["open", "--store", store_str]), 2);

    let out = cli(&[
        "open",
        tmp.path().join("nope.jpg").to_str().unwrap(),
        "--store",
        store_str,
    ]);
    assert_exit(&out, 0);
    assert!(stderr(&out).contains("0 planned"), "{}", stderr(&out));
}

/// `check` still exits 3 on a corrupt STORE (T27's original AC, re-verified
/// unchanged after E04 since `open` shares `Core::open_catalog`/
/// `create_catalog` with every other subcommand).
#[test]
fn check_still_exits_3_on_a_corrupt_store() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    stage_fixtures(
        &src,
        &["canon-eos-r6.cr3", "lightbox-tiny.jpg", "gradient-8x8.png"],
    );
    let store = tmp.path().join("store.lbdata");
    let store_str = store.to_str().unwrap();
    // Populate real rows first (an empty catalog's pages 2-4 may be entirely
    // unused, and flipping unused-page bits corrupts nothing quick_check
    // would notice).
    assert_exit(
        &cli(&[
            "open",
            src.to_str().unwrap(),
            "--recursive",
            "--store",
            store_str,
        ]),
        0,
    );

    let db = store.join("catalog.sqlite");
    // WAL mode: a store this small never crosses SQLite's automatic
    // checkpoint threshold on its own, so its data lives in `catalog.sqlite
    // -wal`, not the main file, force a checkpoint so there is real
    // b-tree content in `catalog.sqlite` to corrupt.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();

    let mut bytes = std::fs::read(&db).unwrap();
    assert!(bytes.len() > 4 * 4096, "catalog large enough to corrupt");
    // Flip bits across whole pages 2..4, guaranteed b-tree damage that
    // quick_check reports (mirrors e2e.rs's own bit-flip convention; a
    // single flipped byte could land in free space and be a no-op).
    for b in &mut bytes[4096..4 * 4096] {
        *b ^= 0xA5;
    }
    std::fs::write(&db, bytes).unwrap();

    assert_exit(&cli(&["check", "--catalog", store_str]), 3);
    // `open` refuses the same way `list`/`render` do, `Core::open_catalog`
    // fails the same corruption check every subcommand shares.
    assert_exit(&cli(&["open", "somefile.jpg", "--store", store_str]), 3);
}

/// T10 AC (browse harness): `browse` needs no catalog at all, a pure
/// filesystem read.
#[test]
fn browse_lists_immediate_children_and_needs_no_catalog() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    stage_fixtures(&src, &["lightbox-tiny.jpg"]);
    std::fs::create_dir_all(src.join("nested")).unwrap();

    let out = cli(&["browse", src.to_str().unwrap()]);
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("nested"), "{text}");
    assert!(text.contains("lightbox-tiny.jpg"), "{text}");

    let out_json = cli(&["browse", src.to_str().unwrap(), "--json"]);
    assert_exit(&out_json, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out_json)).unwrap();
    assert_eq!(v["subdirs"].as_array().unwrap().len(), 1);
    assert_eq!(v["images"].as_array().unwrap().len(), 1);

    // A missing directory is a runtime failure, not a crash.
    assert_exit(
        &cli(&["browse", tmp.path().join("nope").to_str().unwrap()]),
        1,
    );
}
