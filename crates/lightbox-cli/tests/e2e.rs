// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-cli` E2E (E01 spec §5 T27): drives the real binary through
//! `create → import → list → render → check → backup`, asserting exit
//! codes, row counts, and that the render output passes golden compare
//! the headless proof of seam 1 on all 3 CI OSes (CPU render path).
//!
//! Requires the fixture corpus: `cargo xtask fixtures` (CI fetches it
//! before `cargo test`).

use std::path::{Path, PathBuf};
use std::process::Output;

use lbx_image_compare::{check_golden, GoldenConfig, GoldenOutcome, GoldenSpec, Rgba8Image};
use tempfile::TempDir;

/// Runs the built binary with `args`, capturing output.
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

#[test]
fn usage_errors_exit_2_without_touching_anything() {
    // No arguments at all.
    assert_exit(&cli(&[]), 2);
    // Unknown subcommand.
    assert_exit(&cli(&["frobnicate"]), 2);
    // Missing required --catalog.
    assert_exit(&cli(&["list"]), 2);
    // Missing value after a flag.
    assert_exit(&cli(&["check", "--catalog"]), 2);
    // Stray argument.
    assert_exit(&cli(&["check", "--catalog", "x.lbdata", "stray"]), 2);
    // Help exits 0.
    let help = cli(&["--help"]);
    assert_exit(&help, 0);
    assert!(stdout(&help).contains("lightbox-cli"));
}

#[test]
fn missing_catalog_is_a_runtime_failure_not_a_crash() {
    let tmp = TempDir::new().unwrap();
    let cat = tmp.path().join("nope.lbdata");
    let cat = cat.to_str().unwrap();
    assert_exit(&cli(&["list", "--catalog", cat]), 1);
    assert_exit(&cli(&["check", "--catalog", cat]), 1);
}

/// The spec §3.10 flow in order: create → import → list → render → check →
/// backup, plus duplicate-skip, render determinism, unknown-image failure,
/// and the bit-flipped-catalog `check` (T27 ACs).
#[test]
fn create_import_list_render_check_backup_flow() {
    let tmp = TempDir::new().unwrap();
    let catalog_path = tmp.path().join("e2e.lbdata");
    let cat = catalog_path.to_str().unwrap();
    let photos = tmp.path().join("photos");
    stage_fixtures(
        &photos,
        &[
            "canon-eos-r6.cr3",      // real raw with a full-size embedded preview
            "lightbox-tiny.jpg",     // JPEG original (its own preview)
            "gradient-8x8.png",      // supported non-JPEG format
            "corrupt-truncated.cr2", // catalogued with decode_error, never a crash
        ],
    );

    // --- create ---
    let out = cli(&["create", "--catalog", cat]);
    assert_exit(&out, 0);
    // Latest migration is 0007 (E10 installed_look); a fresh create is fully migrated.
    assert!(
        stdout(&out).contains("schema version 7"),
        "{}",
        stdout(&out)
    );
    assert!(catalog_path.join("catalog.sqlite").is_file());
    // Creating over an existing catalog is refused (exit 1, not corruption).
    assert_exit(&cli(&["create", "--catalog", cat]), 1);

    // --- import ---
    let out = cli(&[
        "import",
        "--catalog",
        cat,
        "--add",
        photos.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("imported 4"),
        "expected 4 rows imported: {}",
        stdout(&out)
    );
    // Re-import of the same directory: dup-skip on content hash (T17 AC).
    let out = cli(&[
        "import",
        "--catalog",
        cat,
        "--add",
        photos.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("imported 0 (skipped 4 duplicates"),
        "expected all-duplicates skip: {}",
        stdout(&out)
    );

    // --- list (--json is the machine-readable row-count assertion) ---
    let out = cli(&["list", "--catalog", cat, "--json"]);
    assert_exit(&out, 0);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let rows = rows.as_array().expect("JSON array");
    assert_eq!(rows.len(), 4, "row count after import");
    let cr3 = rows
        .iter()
        .find(|r| r["filename"] == "canon-eos-r6.cr3")
        .expect("CR3 row present");
    assert_eq!(cr3["width"], 3408);
    assert_eq!(cr3["height"], 2272);
    let cr3_id = cr3["id"].as_i64().expect("image id").to_string();
    let corrupt = rows
        .iter()
        .find(|r| r["filename"] == "corrupt-truncated.cr2")
        .expect("corrupt row catalogued");
    assert_eq!(corrupt["decode_error"], true, "decode-error badge data");
    // --limit caps the listing.
    let out = cli(&["list", "--catalog", cat, "--json", "--limit", "2"]);
    assert_exit(&out, 0);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);

    // --- render (CPU path: runs on every CI OS; same Engine::submit/poll
    // path as the shell's loupe) ---
    let render_png = tmp.path().join("render.png");
    let out = cli(&[
        "render",
        "--catalog",
        cat,
        "--image",
        &cr3_id,
        "--out",
        render_png.to_str().unwrap(),
        "--width",
        "240",
        "--cpu",
    ]);
    assert_exit(&out, 0);
    let rendered = Rgba8Image::read_png(&render_png).expect("render output is a PNG");
    assert_eq!(
        (rendered.width, rendered.height),
        (240, 160),
        "3:2 CR3 fit within 240x240"
    );

    // Golden compare (T27 AC: "render output passes golden compare").
    let cfg = GoldenConfig::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens"),
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-failures"),
    );
    let spec = GoldenSpec {
        node: "cli.render",
        pv: 1,
        case: "canon-eos-r6-fit240",
    };
    match check_golden(&cfg, &spec, &rendered) {
        Ok(GoldenOutcome::Matched(report)) => {
            eprintln!("cli.render golden: {report}");
        }
        Ok(GoldenOutcome::Blessed { path }) => {
            eprintln!("blessed {}", path.display());
        }
        Err(e) => panic!("golden compare failed: {e}"),
    }

    // Byte-stable across two runs (T24 AC: `render --cpu` determinism).
    let render2_png = tmp.path().join("render2.png");
    let out = cli(&[
        "render",
        "--catalog",
        cat,
        "--image",
        &cr3_id,
        "--out",
        render2_png.to_str().unwrap(),
        "--width",
        "240",
        "--cpu",
    ]);
    assert_exit(&out, 0);
    assert_eq!(
        std::fs::read(&render_png).unwrap(),
        std::fs::read(&render2_png).unwrap(),
        "CPU render must be byte-stable across runs"
    );

    // Unknown image id: clean failure, exit 1.
    let out = cli(&[
        "render",
        "--catalog",
        cat,
        "--image",
        "999999",
        "--out",
        tmp.path().join("nope.png").to_str().unwrap(),
        "--cpu",
    ]);
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("not found"), "{}", stderr(&out));

    // --- check (clean) ---
    let out = cli(&["check", "--catalog", cat]);
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("ok: schema version 7"));

    // --- backup ---
    let out = cli(&["backup", "--catalog", cat]);
    assert_exit(&out, 0);
    assert!(stdout(&out).starts_with("backup "), "{}", stdout(&out));
    let backups: Vec<_> = std::fs::read_dir(catalog_path.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(backups.len(), 1, "one dated backup dir");
    assert!(
        backups[0].join("catalog.sqlite.zst").is_file(),
        "verified zstd backup present"
    );

    // --- check on a corrupted catalog: exit 3 (T27 AC: "exit code
    // distinguishes ok/corrupt, tested against a bit-flipped catalog") ---
    let bad = tmp.path().join("bad.lbdata");
    std::fs::create_dir_all(&bad).unwrap();
    let mut bytes = std::fs::read(catalog_path.join("catalog.sqlite")).unwrap();
    assert!(bytes.len() > 4 * 4096, "catalog large enough to corrupt");
    // Flip bits across whole pages 2..4, guaranteed b-tree damage that
    // quick_check reports (a single flipped byte could land in free space).
    for b in &mut bytes[4096..4 * 4096] {
        *b ^= 0xA5;
    }
    std::fs::write(bad.join("catalog.sqlite"), &bytes).unwrap();
    let out = cli(&["check", "--catalog", bad.to_str().unwrap()]);
    assert_exit(&out, 3);
    assert!(
        stderr(&out).contains("corrupt"),
        "corruption named: {}",
        stderr(&out)
    );
}
