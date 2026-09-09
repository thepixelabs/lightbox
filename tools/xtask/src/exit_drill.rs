// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `cargo xtask exit-drill`, the scripted half of the T29 M0 exit drill
//! (spec §5 T29 / §8 DoD 1) **plus the E08 H4 editor leg**, runnable on
//! real hardware on each of the three platforms:
//!
//! 1. stages a 1 000-file corpus (every intact fixture + pad-unique raw
//!    variants + synthetic JPEGs) in a temp dir,
//! 2. drives the release CLI through create → import (timed) → list →
//!    render (GPU when the machine has one) → check → backup,
//! 3. **kill -9 mid-import** on a fresh catalog, then proves the catalog
//!    reopens clean (`check` exit 0) and a re-import completes it,
//! 4. **the E08 editor leg** (spec §8 H4 / §11 DoD 1): open a fixture set
//!    in the real editor shell → edit via the slider gesture path →
//!    **kill -9** → reopen the same catalog+set → the recipe is restored
//!    and the filmstrip/canvas smoke passes (`--drill-edit` /
//!    `--drill-verify` scripted modes),
//! 5. runs the shell smoke (`lightbox --smoke`), open → filmstrip →
//!    canvas with the engine texture composited zero-copy on the shared
//!    device.
//!
//! Interactive editing (drop a folder, drag sliders, flip ←/→, watch the
//! F1 overlay) is the human half of the drill; this command prints the
//! checklist for it. Results are recorded in the epic handoff docs
//! (`docs/plan/epics/E01-handoff.md`, `E08-deviations.md`).
//!
//! CR3 is the one mount excluded from pad-uniquing: its ISO-BMFF container
//! treats trailing bytes as a (malformed) box, unlike the TIFF/RAF
//! containers, which ignore trailers, verified against the probe walkers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};

/// Intact fixtures staged once, as-is (the "1 k real raws" seed corpus).
const FIXTURE_MIX: &[&str] = &[
    "canon-eos-350d.cr2",
    "canon-eos-r6.cr3",
    "fujifilm-x100.raf",
    "fujifilm-xt1.raf",
    "gradient-8x8.png",
    "gradient-8x8.tiff",
    "lightbox-tiny.jpg",
    "nikon-d4s.nef",
    "nikon-z6.nef",
    "olympus-e1.orf",
    "sigma-fp.dng",
    "sony-ilce7s.arw",
];

/// Raw fixtures whose containers ignore trailing bytes, safe to
/// pad-unique into many distinct-content copies (real raw structure, real
/// probe + embedded-preview work per file).
const PAD_SAFE_RAWS: &[&str] = &[
    "canon-eos-350d.cr2",
    "fujifilm-x100.raf",
    "fujifilm-xt1.raf",
    "nikon-d4s.nef",
    "nikon-z6.nef",
    "olympus-e1.orf",
    "sigma-fp.dng",
    "sony-ilce7s.arw",
];

pub struct Options {
    /// Total corpus size (default 1000).
    pub files: usize,
    /// Pad-unique copies per pad-safe raw (default 10 ⇒ ~80 raw files,
    /// ~1 GB staged from the ~116 MB fixture corpus).
    pub raw_copies: usize,
    /// Skip the windowed shell-smoke leg (headless machines).
    pub no_shell: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            files: 1000,
            raw_copies: 10,
            no_shell: false,
        }
    }
}

pub fn run(root: &Path, opts: Options) -> anyhow::Result<()> {
    let fixtures = root.join("fixtures");
    ensure!(
        fixtures.join("manifest.toml").is_file(),
        "fixture corpus missing — run `cargo xtask fixtures` first"
    );
    for name in FIXTURE_MIX {
        ensure!(
            fixtures.join(name).is_file(),
            "fixture {name} missing — run `cargo xtask fixtures` first"
        );
    }

    println!("== E01 T29 exit drill ==  (os: {})", std::env::consts::OS);

    // -- Build the release binaries under test. ---------------------------
    let t = Instant::now();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(&cargo)
        .current_dir(root)
        .args([
            "build",
            "--release",
            "-p",
            "lightbox-cli",
            "-p",
            "lightbox-shell",
        ])
        .status()
        .context("running cargo build --release")?;
    ensure!(status.success(), "release build failed");
    step("build --release", t.elapsed(), "lightbox-cli + lightbox");
    let exe = std::env::consts::EXE_SUFFIX;
    let cli = root.join(format!("target/release/lightbox-cli{exe}"));
    let shell = root.join(format!("target/release/lightbox{exe}"));

    // -- Stage the 1 k corpus. ---------------------------------------------
    let t = Instant::now();
    let tmp = tempfile::TempDir::with_prefix("lightbox-exit-drill-")?;
    let corpus = tmp.path().join("corpus");
    let staged = stage_corpus(&fixtures, &corpus, opts.files, opts.raw_copies)?;
    step(
        "stage corpus",
        t.elapsed(),
        &format!("{staged} files (fixture mix + padded raws + synthetic JPEGs)"),
    );

    // -- Happy path: create → import → list → render → check → backup. ----
    let catalog = tmp.path().join("drill.lbdata");
    let catalog_arg = catalog.to_string_lossy().into_owned();
    let corpus_arg = corpus.to_string_lossy().into_owned();

    let (out, took) = cli_ok(&cli, &["create", "--catalog", &catalog_arg])?;
    step("create", took, out.lines().next().unwrap_or(""));

    let (out, import_wall) = cli_ok(
        &cli,
        &[
            "import",
            "--catalog",
            &catalog_arg,
            "--add",
            &corpus_arg,
            "--recursive",
        ],
    )?;
    let report = parse_import_report(&out)?;
    ensure!(
        report.imported == staged as u64 && report.errors == 0,
        "import expected {staged} clean rows, got {report:?}"
    );
    step(
        "import 1k",
        import_wall,
        &format!("imported {} errors {}", report.imported, report.errors),
    );

    let (out, took) = cli_ok(
        &cli,
        &["list", "--catalog", &catalog_arg, "--json", "--limit", "5"],
    )?;
    let image_id = first_image_id(&out)?;
    step("list --json", took, &format!("first image id {image_id}"));

    let render_out = tmp.path().join("render.png");
    let (_, took) = cli_ok(
        &cli,
        &[
            "render",
            "--catalog",
            &catalog_arg,
            "--image",
            &image_id.to_string(),
            "--out",
            &render_out.to_string_lossy(),
        ],
    )?;
    let png = std::fs::read(&render_out).context("reading rendered PNG")?;
    ensure!(
        png.starts_with(&[0x89, b'P', b'N', b'G']),
        "render output is not a PNG"
    );
    step(
        "render",
        took,
        &format!("{} bytes (Engine::submit path)", png.len()),
    );

    let (_, took) = cli_ok(&cli, &["check", "--catalog", &catalog_arg])?;
    step("check", took, "integrity ok");

    let (_, took) = cli_ok(&cli, &["backup", "--catalog", &catalog_arg])?;
    let zst = std::fs::read_dir(catalog.join("backups"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            std::fs::read_dir(e.path()).ok().and_then(|mut d| {
                d.find_map(|f| {
                    let p = f.ok()?.path();
                    (p.extension()? == "zst").then_some(p)
                })
            })
        })
        .next();
    ensure!(zst.is_some(), "no .zst backup written");
    step("backup", took, "verified .zst written");

    // -- Crash leg: kill -9 mid-import → reopen clean → complete. ---------
    kill9_leg(&cli, tmp.path(), &corpus_arg, staged, import_wall)?;

    // -- E08 H4 editor leg: edit → kill -9 → reopen → recipe restored. ----
    if opts.no_shell {
        println!("  (skipped) editor kill -9 leg — --no-shell");
    } else {
        editor_leg(&fixtures, &shell, tmp.path())?;
    }

    // -- Shell leg: open → filmstrip → canvas on this hardware. -----------
    if opts.no_shell {
        println!("  (skipped) shell smoke — --no-shell");
    } else {
        let t = Instant::now();
        let out = Command::new(&shell)
            .current_dir(root)
            .args(["--smoke", "120"])
            .output()
            .context("running lightbox --smoke")?;
        ensure!(
            out.status.success(),
            "shell smoke failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        step(
            "shell smoke",
            t.elapsed(),
            "open → filmstrip → canvas, engine texture zero-copy on the shared device",
        );
    }

    println!("\nEXIT DRILL PASS ({})", std::env::consts::OS);
    println!(
        "manual half (witness + record in docs/plan/epics/E08-deviations.md):\n\
         - `{}`: drop a real shoot folder onto the window\n\
         -   drag the Basic-panel sliders, flip ←/→ through the filmstrip,\n\
         -   toggle F1 and confirm frame p95 < 16 ms / nav p95 < 50 ms",
        shell.display()
    );
    Ok(())
}

/// The E08 H4 editor leg: a persistent catalog + a small fixture set,
/// edited through the real slider gesture path, `kill -9`'d after the
/// durable commit, then reopened and verified, recipe restored + the
/// filmstrip/canvas smoke on the reopened session (both scripted shell
/// modes gate on the same seam proof `--smoke` uses).
fn editor_leg(fixtures: &Path, shell: &Path, tmp: &Path) -> anyhow::Result<()> {
    use std::io::BufRead as _;

    let state = tmp.join("editor-drill");
    let photos = state.join("photos");
    std::fs::create_dir_all(&photos)?;
    // Three unique-content JPEGs from the pinned tiny fixture, enough for
    // a real filmstrip, deterministic name order.
    let tiny = std::fs::read(fixtures.join("lightbox-tiny.jpg"))?;
    for i in 0..3 {
        let mut padded = tiny.clone();
        padded.extend_from_slice(format!("editor drill {i}").as_bytes());
        std::fs::write(photos.join(format!("edit-{i}.jpg")), padded)?;
    }
    let catalog = state.join("editor.lbdata");
    let catalog_arg = catalog.to_string_lossy().into_owned();
    let photos_arg = photos.to_string_lossy().into_owned();

    // -- Leg 1: open → slider gesture → durable commit → kill -9. ---------
    let t = Instant::now();
    let mut child = Command::new(shell)
        .args(["--catalog", &catalog_arg, "--drill-edit", &photos_arg])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning lightbox --drill-edit")?;
    let stdout = child.stdout.take().context("drill-edit stdout")?;

    // Read lines on a helper thread so a wedged child can't hang the drill
    // (the shell's own 120 s expiry also guards it).
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let announce = loop {
        match rx.recv_timeout(Duration::from_secs(150)) {
            Ok(line) if line.starts_with("DRILL-EDIT ") => break line,
            Ok(_) => continue,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("drill-edit never announced its commit: {err}");
            }
        }
    };
    // `DRILL-EDIT image=<id> exposure=<v> seq=<n>`.
    let field = |name: &str| -> anyhow::Result<String> {
        announce
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix(&format!("{name}=")))
            .map(str::to_owned)
            .with_context(|| format!("no {name}= field in {announce:?}"))
    };
    let image = field("image")?;
    let exposure = field("exposure")?;

    // The durable commit is on disk, SIGKILL the editor mid-session.
    child.kill().context("kill -9 on the editor child")?;
    let status = child.wait()?;
    let _ = reader.join();
    ensure!(!status.success(), "editor claims success after SIGKILL");
    step(
        "editor edit + kill -9",
        t.elapsed(),
        &format!("committed exposure={exposure} on image={image}, then SIGKILL"),
    );

    // -- Leg 2: reopen → recipe restored → filmstrip/canvas smoke. --------
    let t = Instant::now();
    let out = Command::new(shell)
        .args([
            "--catalog",
            &catalog_arg,
            "--drill-verify",
            &photos_arg,
            "--expect-image",
            &image,
            "--expect-exposure",
            &exposure,
        ])
        .output()
        .context("running lightbox --drill-verify")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ensure!(
        out.status.success() && text.contains("DRILL-VERIFY ok"),
        "drill verify failed:\n{text}"
    );
    step(
        "reopen restores recipe",
        t.elapsed(),
        &format!("exposure={exposure} restored; filmstrip/canvas smoke on the reopened session"),
    );
    Ok(())
}

/// The kill -9 leg: SIGKILL a mid-flight import (self-calibrated against
/// the measured import wall time), then `check` must pass and a re-import
/// must complete the catalog (dup-skip on content hash).
fn kill9_leg(
    cli: &Path,
    tmp: &Path,
    corpus_arg: &str,
    staged: usize,
    import_wall: Duration,
) -> anyhow::Result<()> {
    let mut delay = import_wall.mul_f64(0.4).max(Duration::from_millis(100));
    for attempt in 1..=4 {
        let catalog = tmp.join(format!("crash-{attempt}.lbdata"));
        let catalog_arg = catalog.to_string_lossy().into_owned();
        cli_ok(cli, &["create", "--catalog", &catalog_arg])?;

        let t = Instant::now();
        let mut child = Command::new(cli)
            .args([
                "import",
                "--catalog",
                &catalog_arg,
                "--add",
                corpus_arg,
                "--recursive",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawning import for the kill leg")?;
        std::thread::sleep(delay);
        if child.try_wait()?.is_some() {
            // Import outran the delay, kill missed. Halve and retry.
            delay = (delay / 2).max(Duration::from_millis(20));
            continue;
        }
        child.kill().context("kill -9 on the import child")?;
        let status = child.wait()?;
        ensure!(!status.success(), "child claims success after SIGKILL");
        step(
            "kill -9 mid-import",
            t.elapsed(),
            &format!("killed after {delay:?} (attempt {attempt})"),
        );

        let (_, took) = cli_ok(cli, &["check", "--catalog", &catalog_arg])?;
        step("check after kill", took, "integrity ok — no corruption");

        let (out, took) = cli_ok(
            cli,
            &[
                "import",
                "--catalog",
                &catalog_arg,
                "--add",
                corpus_arg,
                "--recursive",
            ],
        )?;
        let report = parse_import_report(&out)?;
        ensure!(
            report.imported + report.skipped == staged as u64 && report.errors == 0,
            "post-crash re-import expected {staged} total rows, got {report:?}"
        );
        step(
            "re-import completes",
            took,
            &format!(
                "imported {} + dup-skipped {} = {staged}",
                report.imported, report.skipped
            ),
        );
        let (_, took) = cli_ok(cli, &["check", "--catalog", &catalog_arg])?;
        step("final check", took, "integrity ok");
        return Ok(());
    }
    bail!("kill -9 leg: import finished before the kill on every attempt");
}

/// Stages `files` total: the fixture mix + `raw_copies` pad-unique copies
/// of each pad-safe raw + synthetic JPEG fill, in subfolders of ≤ 100.
fn stage_corpus(
    fixtures: &Path,
    corpus: &Path,
    files: usize,
    raw_copies: usize,
) -> anyhow::Result<usize> {
    let tiny = std::fs::read(fixtures.join("lightbox-tiny.jpg"))?;
    let mut raws: Vec<(String, Vec<u8>)> = Vec::new();
    for name in PAD_SAFE_RAWS {
        raws.push((name.to_string(), std::fs::read(fixtures.join(name))?));
    }

    let mut staged = 0usize;
    let dir_for = |i: usize| -> anyhow::Result<PathBuf> {
        let dir = corpus.join(format!("day-{:02}", i / 100));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    };

    for name in FIXTURE_MIX {
        let dir = dir_for(staged)?;
        std::fs::copy(fixtures.join(name), dir.join(name))?;
        staged += 1;
    }
    'raws: for copy in 0..raw_copies {
        for (name, bytes) in &raws {
            if staged >= files {
                break 'raws;
            }
            let dir = dir_for(staged)?;
            let mut padded = bytes.clone();
            padded.extend_from_slice(format!("drill pad {copy:03} {name}").as_bytes());
            std::fs::write(dir.join(format!("pad-{copy:03}-{name}")), padded)?;
            staged += 1;
        }
    }
    let mut i = 0;
    while staged < files {
        let dir = dir_for(staged)?;
        let mut padded = tiny.clone();
        padded.extend_from_slice(format!("drill jpeg {i:06}").as_bytes());
        std::fs::write(dir.join(format!("syn-{i:06}.jpg")), padded)?;
        staged += 1;
        i += 1;
    }
    Ok(staged)
}

#[derive(Debug)]
struct ImportNumbers {
    imported: u64,
    skipped: u64,
    errors: u64,
}

/// Parses the CLI's report line:
/// `imported N (skipped N duplicates, N unsupported, N errors) in X.XXs`.
fn parse_import_report(out: &str) -> anyhow::Result<ImportNumbers> {
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("imported "))
        .with_context(|| format!("no import report line in output:\n{out}"))?;
    let nums: Vec<u64> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    ensure!(nums.len() >= 4, "unparseable import report: {line}");
    Ok(ImportNumbers {
        imported: nums[0],
        skipped: nums[1],
        errors: nums[3],
    })
}

/// First image id from `list --json` output (a top-level array of
/// ImageSummary rows; the row-count footer goes to stderr).
fn first_image_id(out: &str) -> anyhow::Result<i64> {
    let json_start = out.find('[').context("no JSON array in list output")?;
    let json_end = out.rfind(']').context("unterminated JSON array")?;
    let v: serde_json::Value =
        serde_json::from_str(&out[json_start..=json_end]).context("parsing list --json")?;
    v.as_array()
        .and_then(|a| a.first())
        .and_then(|img| img.get("id"))
        .and_then(|id| id.as_i64())
        .context("list --json returned no images")
}

/// Runs the CLI, requiring exit 0; returns combined output + wall time.
fn cli_ok(cli: &Path, args: &[&str]) -> anyhow::Result<(String, Duration)> {
    let t = Instant::now();
    let out = Command::new(cli)
        .args(args)
        .output()
        .with_context(|| format!("running lightbox-cli {args:?}"))?;
    let took = t.elapsed();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ensure!(
        out.status.success(),
        "lightbox-cli {args:?} failed ({:?}):\n{text}",
        out.status.code()
    );
    Ok((text, took))
}

fn step(name: &str, took: Duration, detail: &str) {
    println!(
        "  ok  {name:<22} {:>8.1} ms  {detail}",
        took.as_secs_f64() * 1000.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_report_line_parses() {
        let out =
            "importing …\nimported 1000 (skipped 3 duplicates, 2 unsupported, 1 errors) in 5.20s\n";
        let n = parse_import_report(out).unwrap();
        assert_eq!(n.imported, 1000);
        assert_eq!(n.skipped, 3);
        assert_eq!(n.errors, 1);
    }

    #[test]
    fn list_json_yields_first_id() {
        // Combined stdout+stderr: JSON array + the stderr row-count footer.
        let out = "[\n  {\"id\":42,\"filename\":\"a.cr3\"},\n  {\"id\":43}\n]\n2 image(s)\n";
        assert_eq!(first_image_id(out).unwrap(), 42);
        assert!(first_image_id("[]\n0 image(s)\n").is_err());
    }
}
