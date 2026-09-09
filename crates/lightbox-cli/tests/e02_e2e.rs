// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E02 Phase H (H2) headless E2E: drives the real binary through
//! `probe` / `decode` / `render-ref` / `profile {inspect,install,list}` /
//! `look inspect` over the fixture corpus, the check CI's golden job runs.
//!
//! The default (license-clean) build ships the LibRaw proxy **without** the
//! `libraw` feature, so mosaic-raw `decode` / `render-ref` cannot produce
//! pixels here; the contract this test pins is that they fail with a
//! **structured, non-crashing** error (a real exit code, never a signal), while
//! `probe` and the non-raw pixel path work everywhere. Requires the fixture
//! corpus (`cargo xtask fixtures`).

use std::path::PathBuf;
use std::process::Output;

use lbx_image_compare::Rgba8Image;
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

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn fixtures_dir() -> PathBuf {
    let dir = repo_path("fixtures");
    assert!(
        dir.join("manifest.toml").is_file(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

fn fixture(name: &str) -> PathBuf {
    let p = fixtures_dir().join(name);
    assert!(
        p.is_file(),
        "fixture {name} missing — run `cargo xtask fixtures`"
    );
    p
}

/// `probe` classifies every good corpus file with a real exit code and format,
/// and never crashes on the corrupt ones.
#[test]
fn probe_classifies_the_corpus() {
    for (name, tag) in [
        ("canon-eos-r6.cr3", "CR3"),
        ("nikon-z6.nef", "NEF"),
        ("sony-ilce7s.arw", "ARW"),
        ("gradient-8x8.png", "PNG"),
        ("gradient-8x8.tiff", "TIFF"),
        ("lightbox-tiny.jpg", "JPEG"),
    ] {
        let f = fixture(name);
        let out = cli(&["probe", "--file", f.to_str().unwrap(), "--json"]);
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains(&format!("\"format\": \"{tag}\"")),
            "probe {name}: expected format {tag} in {}",
            stdout(&out)
        );
    }

    // Corrupt raw: a clean structured failure, never a panic/signal.
    let corrupt = fixture("corrupt-truncated.cr2");
    let out = cli(&["probe", "--file", corrupt.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "corrupt probe must exit 1, not crash"
    );
}

/// `decode` runs the non-raw codecs to completion and fails **structurally**
/// (never a crash) on mosaic raw without a libraw-enabled proxy.
#[test]
fn decode_nonraw_works_and_raw_fails_cleanly() {
    for name in ["gradient-8x8.png", "gradient-8x8.tiff", "lightbox-tiny.jpg"] {
        let f = fixture(name);
        let out = cli(&["decode", "--file", f.to_str().unwrap()]);
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("decoded"), "{}", stdout(&out));
    }

    // Mosaic raw: exit 0 (a libraw-enabled proxy is present) OR a structured
    // exit 1, but always a real exit code, never a signal/panic.
    let cr3 = fixture("canon-eos-r6.cr3");
    let out = cli(&["decode", "--file", cr3.to_str().unwrap()]);
    let code = out
        .status
        .code()
        .expect("raw decode must exit, never crash");
    assert!(code == 0 || code == 1, "unexpected exit {code}");
    if code == 1 {
        assert!(
            stderr(&out).to_lowercase().contains("proxy"),
            "structured raw failure must name the proxy: {}",
            stderr(&out)
        );
    }
}

/// `render-ref` renders a non-raw source to a valid sRGB PNG in every build.
#[test]
fn render_ref_nonraw_writes_a_png() {
    let tmp = TempDir::new().unwrap();
    let png = fixture("gradient-8x8.png");
    let out_png = tmp.path().join("ref.png");
    let out = cli(&[
        "render-ref",
        "--file",
        png.to_str().unwrap(),
        "--out",
        out_png.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    let img = Rgba8Image::read_png(&out_png).expect("render-ref output is a PNG");
    assert_eq!((img.width, img.height), (8, 8));
}

/// `look inspect` and `profile inspect` read the bundled look.
#[test]
fn look_and_profile_inspect_the_bundled_look() {
    let look = repo_path("assets/color/looks/lightbox-color-v1.lblook");
    assert!(look.is_file(), "bundled look missing at {}", look.display());

    let out = cli(&["look", "inspect", "--file", look.to_str().unwrap()]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("Lightbox Color v1"),
        "{}",
        stdout(&out)
    );

    let out = cli(&[
        "profile",
        "inspect",
        "--file",
        look.to_str().unwrap(),
        "--json",
    ]);
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("\"kind\": \"look\""),
        "{}",
        stdout(&out)
    );
}

/// `profile install` registers a look in the `camera_profile` registry
/// (migration 0002) and `profile list` reads it back; install is idempotent.
#[test]
fn profile_install_and_list_round_trip() {
    let tmp = TempDir::new().unwrap();
    let cat = tmp.path().join("profiles.lbdata");
    let cat = cat.to_str().unwrap();
    let look = repo_path("assets/color/looks/lightbox-neutral.lblook");

    assert_exit(&cli(&["create", "--catalog", cat]), 0);

    let out = cli(&[
        "profile",
        "install",
        "--catalog",
        cat,
        "--file",
        look.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("installed"), "{}", stdout(&out));

    // Idempotent: a second install is "already present".
    let out = cli(&[
        "profile",
        "install",
        "--catalog",
        cat,
        "--file",
        look.to_str().unwrap(),
    ]);
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("already present"), "{}", stdout(&out));

    let out = cli(&["profile", "list", "--catalog", cat, "--json"]);
    assert_exit(&out, 0);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1, "one installed profile");
    assert_eq!(rows[0]["name"], "Lightbox Neutral");
    assert_eq!(rows[0]["kind"], "look");
    assert_eq!(rows[0]["source"], "user");
}

/// Usage errors on the new subcommands exit 2 without side effects.
#[test]
fn e02_usage_errors_exit_2() {
    assert_exit(&cli(&["probe"]), 2); // missing --file
    assert_exit(&cli(&["render-ref", "--file", "x.png"]), 2); // missing --out
    assert_exit(&cli(&["profile"]), 2); // missing subcommand
    assert_exit(&cli(&["profile", "frobnicate"]), 2); // unknown subcommand
    assert_exit(&cli(&["look"]), 2); // missing subcommand
}
