// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox` — the desktop app binary (E01 Phase 7: grid + loupe shell).
//!
//! ```text
//! lightbox [--catalog <dir>.lbdata]   # open (or create) a catalog
//! lightbox --smoke [FRAMES]           # CI smoke: throwaway catalog →
//!                                     # import → grid → loupe; verify the
//!                                     # zero-copy seam held, exit 0/1
//! lightbox --perf-scroll [FRAMES]     # T28 nightly: scripted grid-scroll
//!                                     # frame-time capture; prints one
//!                                     # JSON summary line, exit 0/1
//! ```

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use lightbox_shell::ShellOptions;

const USAGE: &str =
    "usage: lightbox [--catalog <dir>.lbdata] [--smoke [FRAMES]] [--perf-scroll [FRAMES]]";

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
            "--perf-scroll" => {
                // Optional FRAMES: consume the next arg only when numeric.
                let frames = match args.peek().map(|v| v.parse::<u64>()) {
                    Some(Ok(n)) => {
                        args.next();
                        n
                    }
                    _ => 600,
                };
                options.perf_scroll_frames = Some(frames.max(60));
            }
            "--catalog" => {
                let Some(path) = args.next() else {
                    eprintln!("--catalog requires a path\n{USAGE}");
                    return ExitCode::from(2);
                };
                options.catalog = Some(path.into());
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    if options.smoke_frames.is_some() && options.perf_scroll_frames.is_some() {
        eprintln!("--smoke and --perf-scroll are mutually exclusive\n{USAGE}");
        return ExitCode::from(2);
    }

    let smoke = options.smoke_frames.is_some();
    let perf = options.perf_scroll_frames.is_some();
    match lightbox_shell::run(options) {
        Ok(outcome) => {
            let frames = outcome.frames.load(Ordering::Acquire);
            let swaps = outcome.texture_swaps.load(Ordering::Acquire);
            let proven = outcome.seam_proven.load(Ordering::Acquire);
            if perf {
                if !outcome.perf_ok.load(Ordering::Acquire) {
                    eprintln!("FAIL: grid-scroll perf capture did not complete");
                    return ExitCode::FAILURE;
                }
                return ExitCode::SUCCESS;
            }
            if smoke {
                println!(
                    "seam-2 smoke: frames={frames} texture_swaps={swaps} seam_proven={proven}"
                );
                if !proven || swaps == 0 {
                    eprintln!(
                        "FAIL: no Engine::submit texture was composited on the shared device"
                    );
                    return ExitCode::FAILURE;
                }
                println!(
                    "OK: import → grid → loupe drove an engine texture zero-copy \
                     on the shared wgpu device"
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
