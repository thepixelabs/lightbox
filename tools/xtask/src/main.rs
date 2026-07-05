// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `xtask` — workspace task runner (invoked as `cargo xtask …`).
//!
//! Owned by **E01** (spec §2): fixture fetch with pinned hashes (T4),
//! CI helpers, and the migration-registry lint (T9).

mod exit_drill;
mod fixtures;
mod generate;
mod hashing;
mod migrations_lint;
mod native_deps;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::bail;

const USAGE: &str = "\
Usage: cargo xtask <command>

Commands:
  fixtures [--offline] [--force]   fetch/generate + verify the pinned fixture
                                   corpus into fixtures/ (see fixtures/manifest.toml)
  hash <file>...                   print xxh3-128 and sha256 of files (for
                                   authoring manifest pins)
  lint-migrations                  cross-check docs/plan/migrations.md against
                                   crates/lightbox-catalog/migrations/ (CI gate)
  lint-native-deps                 cross-check Cargo.lock's *-sys crates against
                                   native-inventory.toml (SBOM surface-2
                                   placeholder, CI gate)
  exit-drill [--files N] [--raw-copies N] [--no-shell]
                                   T29 M0 exit drill on THIS machine: 1k-file
                                   import, kill -9 mid-import → clean reopen,
                                   CLI round-trip, shell smoke
";

/// Workspace root (xtask lives in `tools/xtask`).
pub(crate) fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tools/xtask has a workspace root two levels up")
        .to_path_buf()
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fixtures") => cmd_fixtures(&args[1..]),
        Some("hash") => cmd_hash(&args[1..]),
        Some("lint-migrations") => migrations_lint::lint(&workspace_root()),
        Some("lint-native-deps") => native_deps::lint(&workspace_root()),
        Some("exit-drill") => cmd_exit_drill(&args[1..]),
        Some("--help" | "-h" | "help") | None => {
            eprint!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

fn cmd_fixtures(args: &[String]) -> anyhow::Result<()> {
    let mut opts = fixtures::EnsureOptions::default();
    for arg in args {
        match arg.as_str() {
            "--offline" => opts.offline = true,
            "--force" => opts.force = true,
            other => bail!("unknown fixtures flag {other:?}\n\n{USAGE}"),
        }
    }
    let root = workspace_root();
    let outcomes = fixtures::ensure_fixtures(
        &root.join("fixtures/manifest.toml"),
        &root.join("fixtures"),
        opts,
    )?;
    let (mut cached, mut downloaded, mut generated) = (0u32, 0u32, 0u32);
    for (name, kind, outcome) in &outcomes {
        let label = match outcome {
            fixtures::Outcome::Cached => {
                cached += 1;
                "cached    "
            }
            fixtures::Outcome::Downloaded => {
                downloaded += 1;
                "downloaded"
            }
            fixtures::Outcome::Generated => {
                generated += 1;
                "generated "
            }
        };
        println!("  {label} [{kind:7}] {name}", kind = kind.as_str());
    }
    println!(
        "fixtures ok: {} total ({cached} cached, {downloaded} downloaded, {generated} generated)",
        outcomes.len()
    );
    Ok(())
}

fn cmd_exit_drill(args: &[String]) -> anyhow::Result<()> {
    let mut opts = exit_drill::Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--files" => {
                opts.files = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--files requires a value"))?
                    .parse()?;
            }
            "--raw-copies" => {
                opts.raw_copies = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--raw-copies requires a value"))?
                    .parse()?;
            }
            "--no-shell" => opts.no_shell = true,
            other => bail!("unknown exit-drill flag {other:?}\n\n{USAGE}"),
        }
    }
    exit_drill::run(&workspace_root(), opts)
}

fn cmd_hash(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        bail!("hash: expected at least one file path\n\n{USAGE}");
    }
    for arg in args {
        let path = PathBuf::from(arg);
        let xxh3 = hashing::xxh3_hex(hashing::xxh3_128_file(&path)?);
        let sha256 = hashing::sha256_file(&path)?;
        println!("{arg}\n  xxh3_128 = \"{xxh3}\"\n  sha256 = \"{sha256}\"");
    }
    Ok(())
}
