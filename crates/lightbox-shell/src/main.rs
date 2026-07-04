// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox` — the desktop app binary (E01 Phase 2: seam tracer bullet).
//!
//! ```text
//! lightbox                    # run the shell
//! lightbox --smoke [FRAMES]   # run FRAMES (default 60) frames, verify the
//!                             # zero-copy seam held, exit 0/1 (CI smoke)
//! ```

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use lightbox_shell::ShellOptions;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let mut options = ShellOptions::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--smoke" => {
                let frames = args
                    .next()
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .unwrap_or_else(|e| {
                        eprintln!("--smoke FRAMES must be a number: {e}");
                        std::process::exit(2);
                    })
                    .unwrap_or(60);
                options.smoke_frames = Some(frames.max(1));
            }
            "--help" | "-h" => {
                println!("usage: lightbox [--smoke [FRAMES]]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}\nusage: lightbox [--smoke [FRAMES]]");
                return ExitCode::from(2);
            }
        }
    }

    let smoke = options.smoke_frames.is_some();
    match lightbox_shell::run(options) {
        Ok(outcome) => {
            let frames = outcome.frames.load(Ordering::Acquire);
            let swaps = outcome.texture_swaps.load(Ordering::Acquire);
            let proven = outcome.seam_proven.load(Ordering::Acquire);
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
                println!("OK: engine texture composited zero-copy on the shared wgpu device");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("shell failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}
