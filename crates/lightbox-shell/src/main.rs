// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox`, the desktop app binary (E08 Phase A: the editor shell).
//!
//! ```text
//! lightbox [--catalog <dir>.lbdata] [--recursive] [PATH...]
//!                                     # open (or create) a catalog;
//!                                     # trailing PATHs (files/folders) are
//!                                     # opened at launch (A4) before the
//!                                     # first frame paints
//! lightbox --smoke [FRAMES]           # CI smoke: throwaway catalog →
//!                                     # OpenWorkingSet → filmstrip →
//!                                     # canvas; verify the zero-copy seam
//!                                     # held, exit 0/1
//! lightbox --perf-strip [FRAMES]      # H2 perf capture: ~300-entry
//!                                     # synthetic set, sawtooth filmstrip
//!                                     # scroll + nav phase, one JSON
//!                                     # summary line, exit 0/1
//! lightbox --catalog <dir> --drill-edit <PHOTOS> [--exposure V]
//!                                     # H4 exit-drill leg 1: open, commit
//!                                     # one Exposure gesture, print
//!                                     # DRILL-EDIT …, hold until kill -9
//! lightbox --catalog <dir> --drill-verify <PHOTOS> \
//!          --expect-image ID --expect-exposure V
//!                                     # H4 exit-drill leg 2: reopen the
//!                                     # same catalog+set, prove the
//!                                     # canvas, assert the restored
//!                                     # recipe, exit 0/1
//! lightbox [PATH...] --screenshot <FILE.png> \
//!          [--screenshot-frames N] [--screenshot-size WxH]
//!                                     # DEV: run the real app on a
//!                                     # throwaway catalog, settle, write
//!                                     # the viewport to a PNG, exit 0/1.
//!                                     # With no PATH it captures the
//!                                     # empty-state drop target; with one
//!                                     # it captures the loaded editor.
//! ```
//!
//! **A4 macOS "Open With" note:** the pinned winit (0.30.13) does not
//! surface `application:openURLs:`/`application:openFile:` through its
//! cross-platform event API at all (confirmed by reading
//! `winit::platform::macos`'s own docs: the *only* path is registering a
//! fully custom `NSApplicationDelegate` via raw `objc2`/`objc2-app-kit`
//! bindings *before* the event loop starts, bypassing winit's delegate
//! entirely, a non-trivial platform-specific shim eframe provides no hook
//! for). Not wired in Phase A; the honest fallback is drop/dialog/CLI-argv,
//! which cover three of the mandate's entry rows. See `E08-deviations.md`
//! (A4) for the full investigation and the E16-packaging follow-up.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use lightbox_shell::{DrillMode, ScreenshotOptions, ShellOptions, DEFAULT_SCREENSHOT_FRAMES};

const USAGE: &str = "usage: lightbox [--catalog <dir>.lbdata] [--recursive] [--smoke [FRAMES]] \
     [--perf-strip [FRAMES]] [--drill-edit <PHOTOS> [--exposure V]] \
     [--drill-verify <PHOTOS> --expect-image ID --expect-exposure V] \
     [--screenshot <FILE.png> [--screenshot-frames N] [--screenshot-size WxH]] [PATH...]\n\
     \n\
     --catalog <dir>.lbdata    edit store to open or create. Default: the per-user\n\
     \x20                         app-data store (macOS: ~/Library/Application\n\
     \x20                         Support/Lightbox/edits.lbdata), NOT a path relative\n\
     \x20                         to the working directory.\n\
     --screenshot <FILE.png>   dev/debug: run the real app on a throwaway catalog,\n\
     \x20                         let it settle, write the viewport to FILE.png, exit.\n\
     \x20                         With no PATH this captures the empty-state drop\n\
     \x20                         target; `lightbox <file> --screenshot out.png`\n\
     \x20                         captures the loaded editor. Never touches your own\n\
     \x20                         catalog, prefs, or keymap.\n\
     --screenshot-frames N     frames to paint before capturing (default 90); the\n\
     \x20                         shutter also waits for decode/render to go quiet.\n\
     --screenshot-size WxH     capture window size in points (default 1100x720).";

/// What parsing argv decided. Split out of `main` so the whole flag matrix
/// including the mutual-exclusion rules, is unit-testable without booting a
/// window (there is no other seam here that a test can hold on to).
#[derive(Debug)]
enum Parsed {
    /// Boot the shell with these options.
    Run(Box<ShellOptions>),
    /// `--help`/`-h`: print usage, exit 0.
    Help,
    /// Usage error; the message is the first line, `USAGE` follows.
    Usage(String),
}

fn parse_args<I: Iterator<Item = String>>(args: I) -> Parsed {
    let mut args = args.peekable();
    let mut options = ShellOptions::default();
    // Flags assembled after the parse (so their order on the line is free).
    let mut drill_edit: Option<PathBuf> = None;
    let mut drill_verify: Option<PathBuf> = None;
    let mut exposure: f32 = 0.75;
    let mut expect_image: Option<i64> = None;
    let mut expect_exposure: Option<f32> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut screenshot_frames: Option<u64> = None;
    let mut screenshot_size: Option<[f32; 2]> = None;
    while let Some(arg) = args.next() {
        // Value-flag helper: bails out of `parse_args` on a missing value.
        macro_rules! value_of {
            ($flag:literal) => {
                match args.next() {
                    Some(v) => v,
                    None => return Parsed::Usage(concat!($flag, " requires a value").into()),
                }
            };
        }
        match arg.as_str() {
            "--smoke" => {
                // Optional FRAMES: consume the next arg only when numeric.
                let frames = match args.peek().map(|v| v.parse::<u64>()) {
                    Some(Ok(n)) => {
                        args.next();
                        n
                    }
                    _ => 60,
                };
                options.smoke_frames = Some(frames.max(1));
            }
            "--perf-strip" => {
                let frames = match args.peek().map(|v| v.parse::<u64>()) {
                    Some(Ok(n)) => {
                        args.next();
                        n
                    }
                    _ => 300,
                };
                options.perf_strip_frames = Some(frames.max(60));
            }
            "--drill-edit" => drill_edit = Some(PathBuf::from(value_of!("--drill-edit"))),
            "--drill-verify" => drill_verify = Some(PathBuf::from(value_of!("--drill-verify"))),
            "--exposure" => match value_of!("--exposure").parse::<f32>() {
                Ok(v) => exposure = v,
                Err(err) => return Parsed::Usage(format!("--exposure: {err}")),
            },
            "--expect-image" => match value_of!("--expect-image").parse::<i64>() {
                Ok(v) => expect_image = Some(v),
                Err(err) => return Parsed::Usage(format!("--expect-image: {err}")),
            },
            "--expect-exposure" => match value_of!("--expect-exposure").parse::<f32>() {
                Ok(v) => expect_exposure = Some(v),
                Err(err) => return Parsed::Usage(format!("--expect-exposure: {err}")),
            },
            // Dev/debug capture. A PATH-taking flag, unlike --smoke's
            // optional-numeric one: the file name is never optional.
            "--screenshot" => screenshot = Some(PathBuf::from(value_of!("--screenshot"))),
            "--screenshot-frames" => match value_of!("--screenshot-frames").parse::<u64>() {
                Ok(v) => screenshot_frames = Some(v.max(1)),
                Err(err) => return Parsed::Usage(format!("--screenshot-frames: {err}")),
            },
            "--screenshot-size" => {
                let raw = value_of!("--screenshot-size");
                match lightbox_shell::parse_screenshot_size(&raw) {
                    Some(size) => screenshot_size = Some(size),
                    None => {
                        return Parsed::Usage(format!(
                        "--screenshot-size: expected WxH in points (e.g. 1600x1000), got {raw:?}"
                    ))
                    }
                }
            }
            "--catalog" => {
                options.catalog = Some(PathBuf::from(value_of!("--catalog")));
            }
            "--recursive" => {
                options.initial_recursive = true;
            }
            "--help" | "-h" => return Parsed::Help,
            other if other.starts_with("--") => {
                return Parsed::Usage(format!("unknown argument: {other}"))
            }
            // A4: any non-flag argument is a path to open at launch (files
            // and/or folders, mirrors `lightbox-cli open <PATH>...`).
            other => options.initial_paths.push(PathBuf::from(other)),
        }
    }

    // Assemble drill modes (H4) + validate their required companions.
    match (drill_edit, drill_verify) {
        (Some(_), Some(_)) => {
            return Parsed::Usage("--drill-edit and --drill-verify are mutually exclusive".into())
        }
        (Some(photos), None) => options.drill = Some(DrillMode::EditCommit { photos, exposure }),
        (None, Some(photos)) => {
            let (Some(expect_image), Some(expect_exposure)) = (expect_image, expect_exposure)
            else {
                return Parsed::Usage(
                    "--drill-verify needs --expect-image and --expect-exposure".into(),
                );
            };
            options.drill = Some(DrillMode::Verify {
                photos,
                expect_image,
                expect_exposure,
            });
        }
        (None, None) => {}
    }

    // Assemble the screenshot mode. Its two modifier flags are useless (and
    // therefore almost certainly a typo) without it.
    match screenshot {
        Some(path) => {
            options.screenshot = Some(ScreenshotOptions {
                path,
                settle_frames: screenshot_frames.unwrap_or(DEFAULT_SCREENSHOT_FRAMES),
                size: screenshot_size,
            });
        }
        None if screenshot_frames.is_some() || screenshot_size.is_some() => {
            return Parsed::Usage(
                "--screenshot-frames/--screenshot-size require --screenshot <FILE.png>".into(),
            )
        }
        None => {}
    }

    let scripted_modes = usize::from(options.smoke_frames.is_some())
        + usize::from(options.perf_strip_frames.is_some())
        + usize::from(options.drill.is_some());
    if scripted_modes > 1 {
        return Parsed::Usage("--smoke / --perf-strip / --drill-* are mutually exclusive".into());
    }
    if scripted_modes > 0 && !options.initial_paths.is_empty() {
        return Parsed::Usage("scripted modes and launch PATHs are mutually exclusive".into());
    }
    // Screenshot mode runs the REAL entry pipeline (that is the point: argv
    // PATHs must open so the capture shows a loaded editor), so it cannot
    // share a run with a scripted mode that stages its own set instead.
    if scripted_modes > 0 && options.screenshot.is_some() {
        return Parsed::Usage(
            "--screenshot is not compatible with --smoke / --perf-strip / --drill-* \
             (pass PATHs instead: `lightbox <file> --screenshot out.png`)"
                .into(),
        );
    }
    if options.drill.is_some() && options.catalog.is_none() {
        return Parsed::Usage(
            "--drill-* requires --catalog (the drill's catalog must persist)".into(),
        );
    }

    Parsed::Run(Box::new(options))
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let options = match parse_args(std::env::args().skip(1)) {
        Parsed::Run(options) => *options,
        Parsed::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Parsed::Usage(msg) => {
            eprintln!("{msg}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let smoke = options.smoke_frames.is_some();
    let perf = options.perf_strip_frames.is_some();
    let drill = options.drill.clone();
    let screenshot = options.screenshot.clone();

    match lightbox_shell::run(options) {
        Ok(outcome) => {
            let frames = outcome.frames.load(Ordering::Acquire);
            let swaps = outcome.texture_swaps.load(Ordering::Acquire);
            let proven = outcome.seam_proven.load(Ordering::Acquire);
            let filmstrip_shown = outcome.filmstrip_shown.load(Ordering::Acquire);
            if smoke {
                println!(
                    "seam-2 smoke: frames={frames} texture_swaps={swaps} \
                     filmstrip_shown={filmstrip_shown} seam_proven={proven}"
                );
                if !filmstrip_shown || !proven || swaps == 0 {
                    eprintln!(
                        "FAIL: the filmstrip never showed a row and/or no Engine::submit \
                         texture was composited on the shared device"
                    );
                    return ExitCode::FAILURE;
                }
                println!(
                    "OK: OpenWorkingSet → filmstrip → canvas drove an engine texture \
                     zero-copy on the shared wgpu device"
                );
            }
            if perf && !outcome.perf_ok.load(Ordering::Acquire) {
                eprintln!("FAIL: the perf-strip capture did not complete");
                return ExitCode::FAILURE;
            }
            if let Some(shot) = &screenshot {
                // The driver already printed the `screenshot: WxH → PATH`
                // line on success (and logged the reason on failure).
                if !outcome.screenshot_ok.load(Ordering::Acquire) {
                    eprintln!(
                        "FAIL: no screenshot was written to {} (frames={frames})",
                        shot.path.display()
                    );
                    return ExitCode::FAILURE;
                }
            }
            match drill {
                Some(DrillMode::EditCommit { .. }) => {
                    // This leg is supposed to die by kill -9 while holding;
                    // reaching a natural exit means it never got there.
                    eprintln!("FAIL: --drill-edit exited instead of holding for kill -9");
                    return ExitCode::FAILURE;
                }
                Some(DrillMode::Verify { .. }) if !outcome.drill_ok.load(Ordering::Acquire) => {
                    eprintln!("FAIL: drill verify did not confirm the restored recipe");
                    return ExitCode::FAILURE;
                }
                _ => {}
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("shell failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a borrowed argv the way `main` parses `std::env::args()`.
    fn parse(argv: &[&str]) -> Parsed {
        parse_args(argv.iter().map(|s| (*s).to_string()))
    }

    fn run_options(argv: &[&str]) -> ShellOptions {
        match parse(argv) {
            Parsed::Run(o) => *o,
            other => panic!("expected Run for {argv:?}, got {other:?}"),
        }
    }

    fn usage_error(argv: &[&str]) -> String {
        match parse(argv) {
            Parsed::Usage(msg) => msg,
            other => panic!("expected a usage error for {argv:?}, got {other:?}"),
        }
    }

    /// The front door: no PATHs, defaults for everything the flag doesn't say.
    #[test]
    fn screenshot_alone_captures_the_empty_state_with_defaults() {
        let o = run_options(&["--screenshot", "/tmp/lbx-empty.png"]);
        let shot = o.screenshot.expect("screenshot mode armed");
        assert_eq!(shot.path, PathBuf::from("/tmp/lbx-empty.png"));
        assert_eq!(shot.settle_frames, DEFAULT_SCREENSHOT_FRAMES);
        assert_eq!(shot.size, None, "keeps the app's own 1100x720 default");
        assert!(o.initial_paths.is_empty());
        assert!(o.smoke_frames.is_none() && o.perf_strip_frames.is_none());
    }

    /// The loaded editor: argv PATHs coexist with the flag (they do NOT with
    /// the scripted modes), in either order on the command line.
    #[test]
    fn screenshot_coexists_with_launch_paths_in_either_order() {
        for argv in [
            &["fixtures/lightbox-tiny.jpg", "--screenshot", "/tmp/e.png"][..],
            &["--screenshot", "/tmp/e.png", "fixtures/lightbox-tiny.jpg"][..],
        ] {
            let o = run_options(argv);
            assert_eq!(
                o.initial_paths,
                vec![PathBuf::from("fixtures/lightbox-tiny.jpg")],
                "{argv:?}"
            );
            assert!(o.screenshot.is_some(), "{argv:?}");
        }
    }

    #[test]
    fn screenshot_frames_and_size_override_the_defaults() {
        let o = run_options(&[
            "--screenshot",
            "/tmp/e.png",
            "--screenshot-frames",
            "240",
            "--screenshot-size",
            "1600x1000",
        ]);
        let shot = o.screenshot.expect("screenshot mode armed");
        assert_eq!(shot.settle_frames, 240);
        assert_eq!(shot.size, Some([1600.0, 1000.0]));
    }

    /// Zero frames would request the capture before the first paint; clamp
    /// rather than reject (the flag is a hint, not a contract).
    #[test]
    fn screenshot_frames_are_clamped_to_at_least_one() {
        let o = run_options(&["--screenshot", "/tmp/e.png", "--screenshot-frames", "0"]);
        assert_eq!(o.screenshot.unwrap().settle_frames, 1);
    }

    #[test]
    fn screenshot_flag_errors_are_specific() {
        assert!(usage_error(&["--screenshot"]).contains("requires a value"));
        assert!(
            usage_error(&["--screenshot", "/tmp/e.png", "--screenshot-frames", "lots"])
                .contains("--screenshot-frames")
        );
        assert!(
            usage_error(&["--screenshot", "/tmp/e.png", "--screenshot-size", "huge"])
                .contains("--screenshot-size")
        );
        assert!(
            usage_error(&["--screenshot-frames", "10"]).contains("require --screenshot"),
            "a modifier without the flag it modifies is a typo, not a no-op"
        );
        assert!(usage_error(&["--screenshot-size", "800x600"]).contains("require --screenshot"));
    }

    /// Scripted modes stage their own working set and forbid argv PATHs, so a
    /// capture cannot ride along on one, refuse rather than silently
    /// screenshot the wrong app state.
    #[test]
    fn screenshot_is_refused_alongside_every_scripted_mode() {
        for argv in [
            &["--smoke", "60", "--screenshot", "/tmp/e.png"][..],
            &["--perf-strip", "--screenshot", "/tmp/e.png"][..],
            &[
                "--catalog",
                "/tmp/c.lbdata",
                "--drill-edit",
                "/tmp/photos",
                "--screenshot",
                "/tmp/e.png",
            ][..],
        ] {
            assert!(
                usage_error(argv).contains("--screenshot is not compatible"),
                "{argv:?}"
            );
        }
    }

    /// **Do not regress `--smoke`'s contract.** The existing flag matrix
    /// parses exactly as it did before screenshot mode was folded in.
    #[test]
    fn the_pre_existing_flag_matrix_is_unchanged() {
        assert_eq!(run_options(&["--smoke"]).smoke_frames, Some(60));
        assert_eq!(run_options(&["--smoke", "120"]).smoke_frames, Some(120));
        assert_eq!(
            run_options(&["--smoke", "0"]).smoke_frames,
            Some(1),
            "clamped to a paintable minimum"
        );
        assert_eq!(run_options(&["--perf-strip"]).perf_strip_frames, Some(300));
        assert_eq!(
            run_options(&["--perf-strip", "10"]).perf_strip_frames,
            Some(60),
            "floored at the measurable minimum"
        );

        let o = run_options(&[
            "--catalog",
            "/tmp/c.lbdata",
            "--recursive",
            "a.jpg",
            "b.jpg",
        ]);
        assert_eq!(o.catalog, Some(PathBuf::from("/tmp/c.lbdata")));
        assert!(o.initial_recursive);
        assert_eq!(
            o.initial_paths,
            vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")]
        );

        let o = run_options(&[
            "--catalog",
            "/tmp/c.lbdata",
            "--drill-verify",
            "/tmp/photos",
            "--expect-image",
            "7",
            "--expect-exposure",
            "0.5",
        ]);
        assert!(matches!(
            o.drill,
            Some(DrillMode::Verify {
                expect_image: 7,
                ..
            })
        ));

        assert!(matches!(parse(&["--help"]), Parsed::Help));
        assert!(matches!(parse(&["-h"]), Parsed::Help));
        assert!(usage_error(&["--nope"]).contains("unknown argument"));
        assert!(usage_error(&["--smoke", "--perf-strip"]).contains("mutually exclusive"));
        assert!(usage_error(&["--smoke", "a.jpg"]).contains("launch PATHs"));
        assert!(usage_error(&["--drill-edit", "/tmp/photos"]).contains("requires --catalog"));
    }
}
