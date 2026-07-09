// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox` — the desktop app binary (E08 Phase A: the editor shell).
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
//! ```
//!
//! **A4 macOS "Open With" note:** the pinned winit (0.30.13) does not
//! surface `application:openURLs:`/`application:openFile:` through its
//! cross-platform event API at all (confirmed by reading
//! `winit::platform::macos`'s own docs: the *only* path is registering a
//! fully custom `NSApplicationDelegate` via raw `objc2`/`objc2-app-kit`
//! bindings *before* the event loop starts, bypassing winit's delegate
//! entirely — a non-trivial platform-specific shim eframe provides no hook
//! for). Not wired in Phase A; the honest fallback is drop/dialog/CLI-argv,
//! which cover three of the mandate's entry rows. See `E08-deviations.md`
//! (A4) for the full investigation and the E16-packaging follow-up.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use lightbox_shell::ShellOptions;

const USAGE: &str =
    "usage: lightbox [--catalog <dir>.lbdata] [--recursive] [--smoke [FRAMES]] [PATH...]";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1).peekable();
    let mut options = ShellOptions::default();
    while let Some(arg) = args.next() {
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
            "--catalog" => {
                let Some(path) = args.next() else {
                    eprintln!("--catalog requires a path\n{USAGE}");
                    return ExitCode::from(2);
                };
                options.catalog = Some(path.into());
            }
            "--recursive" => {
                options.initial_recursive = true;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
            // A4: any non-flag argument is a path to open at launch (files
            // and/or folders, mirrors `lightbox-cli open <PATH>...`).
            other => options.initial_paths.push(PathBuf::from(other)),
        }
    }

    let smoke = options.smoke_frames.is_some();
    if smoke && !options.initial_paths.is_empty() {
        eprintln!("--smoke and launch PATHs are mutually exclusive\n{USAGE}");
        return ExitCode::from(2);
    }

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
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("shell failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}
