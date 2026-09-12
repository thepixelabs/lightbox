// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `cargo xtask ci`: every gate CI runs, run here, before you push.
//!
//! This exists so that a red build is something you find in thirty seconds on
//! your own machine rather than five minutes later in a runner log. It runs the
//! same commands `.github/workflows/ci.yml` runs, in the same order, and stops
//! at the first failure with the command that failed printed plainly.
//!
//! **What it cannot check, and why that is fine.** CI runs on Linux and Windows
//! as well as macOS. This runs only on the machine you are sitting at, so it
//! cannot catch a platform-specific break: a Windows line-ending problem, or a
//! test that only fails under the software Vulkan adapter Linux CI uses. Those
//! are exactly what the cloud matrix is for. Everything else, which is almost
//! everything, is caught here for free.

use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Result};

/// One gate: what to say, and what to run.
struct Gate {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

const GATES: &[Gate] = &[
    Gate {
        name: "formatting",
        program: "cargo",
        args: &["fmt", "--all", "--check"],
    },
    Gate {
        name: "clippy, warnings denied",
        program: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    },
    Gate {
        name: "build",
        program: "cargo",
        args: &["build", "--workspace"],
    },
    Gate {
        name: "tests",
        program: "cargo",
        args: &["test", "--workspace"],
    },
    Gate {
        name: "dependency licences",
        program: "cargo",
        args: &["deny", "check"],
    },
    Gate {
        name: "website claims",
        program: "python3",
        args: &["tools/site-checks/check_site.py"],
    },
];

/// Runs every gate. Returns an error naming the first that failed.
pub fn run(root: &std::path::Path, quick: bool) -> Result<()> {
    let started = Instant::now();
    let gates: Vec<&Gate> = if quick {
        // Formatting and clippy catch most of what a push gets rejected for and
        // cost seconds rather than minutes.
        GATES.iter().take(2).collect()
    } else {
        GATES.iter().collect()
    };

    for (i, gate) in gates.iter().enumerate() {
        println!("[{}/{}] {} …", i + 1, gates.len(), gate.name);
        let status = Command::new(gate.program)
            .args(gate.args)
            .current_dir(root)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                bail!(
                    "{} failed ({}).\n\nRe-run it on its own to see why:\n    {} {}",
                    gate.name,
                    s,
                    gate.program,
                    gate.args.join(" ")
                );
            }
            Err(e) => bail!("could not run `{}`: {e}", gate.program),
        }
    }

    println!(
        "\nall {} gate(s) green in {:.0}s. Two things this cannot see: Windows and Linux.",
        gates.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
