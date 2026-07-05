// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lbx-perf` — the E01 T28 perf scenario harness (nightly, non-blocking).
//!
//! ```text
//! lbx-perf [--scenario import-1k|page-query|nav-swap|all]
//!          [--out results.json] [--baseline baselines.json]
//!          [--fixtures DIR] [--files N] [--rows N] [--cpu] [--strict]
//! ```
//!
//! Runs the M0 §7 scenarios headlessly through `lightbox-core` (seam 1),
//! prints a markdown table (the nightly workflow appends it to the job
//! summary), optionally writes machine-readable JSON, and compares against
//! the committed baselines — regressions are *reported*, never blocking
//! (spec §8; `--strict` flips the exit code for local bisection). The
//! grid-scroll frame-time capture is the scripted `lightbox --perf-scroll`
//! shell mode, published by the same nightly workflow.
//!
//! Committed baselines: `tools/lbx-perf/baselines.json` (dev-machine
//! numbers + the absolute §6/§7 budgets).

mod corpus;
mod report;
mod scenarios;

use std::path::PathBuf;
use std::process::ExitCode;

use report::Results;

const USAGE: &str = "\
usage: lbx-perf [options]
  --scenario NAME   import-1k | page-query | nav-swap | all (default: all)
  --out FILE        write results JSON
  --baseline FILE   compare against committed baselines JSON
  --fixtures DIR    fixture corpus location (default: ./fixtures)
  --files N         import-1k file count (default: 1000)
  --rows N          page-query synthetic row count (default: 100000)
  --cpu             force the CPU engine path for nav-swap
  --strict          exit 1 on regressions (default: report-only)
";

struct Opts {
    scenario: String,
    out: Option<PathBuf>,
    baseline: Option<PathBuf>,
    fixtures: Option<PathBuf>,
    files: usize,
    rows: u64,
    cpu: bool,
    strict: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut opts = Opts {
        scenario: "all".to_owned(),
        out: None,
        baseline: None,
        fixtures: None,
        files: 1000,
        rows: 100_000,
        cpu: false,
        strict: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--scenario" => opts.scenario = value("--scenario")?,
            "--out" => opts.out = Some(PathBuf::from(value("--out")?)),
            "--baseline" => opts.baseline = Some(PathBuf::from(value("--baseline")?)),
            "--fixtures" => opts.fixtures = Some(PathBuf::from(value("--fixtures")?)),
            "--files" => {
                opts.files = value("--files")?
                    .parse()
                    .map_err(|e| format!("--files: {e}"))?
            }
            "--rows" => {
                opts.rows = value("--rows")?
                    .parse()
                    .map_err(|e| format!("--rows: {e}"))?
            }
            "--cpu" => opts.cpu = true,
            "--strict" => opts.strict = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    match opts.scenario.as_str() {
        "all" | "import-1k" | "page-query" | "nav-swap" => Ok(opts),
        other => Err(format!("unknown scenario {other:?}\n\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };

    let fixtures = scenarios::fixtures_dir(opts.fixtures.as_deref());
    let mut results = Results::default();
    results
        .meta
        .insert("os".into(), std::env::consts::OS.to_owned());
    results
        .meta
        .insert("arch".into(), std::env::consts::ARCH.to_owned());
    results.meta.insert(
        "fixtures".into(),
        if fixtures.join("manifest.toml").is_file() {
            "present".to_owned()
        } else {
            "missing (synthetic-only corpus)".to_owned()
        },
    );

    let run = |name: &str| opts.scenario == "all" || opts.scenario == name;
    if run("import-1k") {
        match scenarios::run_named("import-1k", || scenarios::import_1k(&fixtures, opts.files)) {
            Ok(m) => {
                results.scenarios.insert("import-1k".into(), m);
            }
            Err(err) => {
                eprintln!("error: {err:#}");
                return ExitCode::FAILURE;
            }
        }
    }
    if run("page-query") {
        match scenarios::run_named("page-query", || scenarios::page_query_100k(opts.rows)) {
            Ok(m) => {
                results.scenarios.insert("page-query-100k".into(), m);
            }
            Err(err) => {
                eprintln!("error: {err:#}");
                return ExitCode::FAILURE;
            }
        }
    }
    if run("nav-swap") {
        let gpu = if opts.cpu {
            None
        } else {
            lightbox_render::GpuContext::headless()
        };
        match scenarios::run_named("nav-swap", || scenarios::nav_swap(&fixtures, gpu)) {
            Ok((m, backend)) => {
                results.meta.insert("nav_swap_backend".into(), backend);
                results.scenarios.insert("nav-swap".into(), m);
            }
            Err(err) => {
                eprintln!("error: {err:#}");
                return ExitCode::FAILURE;
            }
        }
    }

    let baseline = match &opts.baseline {
        Some(path) => match report::load_baseline(path) {
            Ok(b) => Some(b),
            Err(err) => {
                eprintln!("error: {err:#}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let regressions = report::publish_table(&results, baseline.as_ref());

    if let Some(out) = &opts.out {
        let json = serde_json::to_string_pretty(&results.to_json()).expect("results serialize");
        if let Err(err) = std::fs::write(out, json + "\n") {
            eprintln!("error: writing {}: {err}", out.display());
            return ExitCode::FAILURE;
        }
        eprintln!("results written to {}", out.display());
    }

    if regressions > 0 && opts.strict {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
