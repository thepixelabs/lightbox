// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-profgen` — internal curated-camera-profile generator (E02.5 /
//! Phase G). **NOT shipped in the app** (spec §1.1/§2). See the crate docs in
//! `lib.rs` for the pipeline; this binary is a thin driver over those modules.
//!
//! The GPL boundary is the process boundary: `dcamprof` is exec'd as a
//! subprocess only (never linked, never bundled). G6 real-profile content is
//! DEFERRED — Lightbox ships ZERO bundled profiles — so `generate` reports the
//! deferred status unless built `--features dcamprof` with the tool installed.

use anyhow::Result;

use lightbox_profgen::dcamprof::{self, Dcamprof};
use lightbox_profgen::session::Session;

const USAGE: &str = "\
lightbox-profgen — internal curated-profile content line (E02.5, NOT shipped)

USAGE:
    lightbox-profgen <command> [args]

COMMANDS:
    generate <session-dir>   Load a target-shot session and run the dcamprof
                             pipeline (GPL-3 subprocess). Requires --features
                             dcamprof + an installed dcamprof binary; otherwise
                             reports the DEFERRED status (spec §0).
    stages                   Print the content-line pipeline stages.
    help                     Print this message.

dcamprof is a GPL-3 SUBPROCESS only — never linked, never bundled. See
tools/lightbox-profgen/docs/{capture-protocol,runbook}.md.";

/// The content-line pipeline stages (a truthful map of the tooling + the
/// deferred content).
const STAGES: &[&str] = &[
    "session:  load a target-shot session dir (raws + TOML metadata)  [G1]",
    "generate: run dcamprof as a GPL-3 SUBPROCESS → .dcp              [G1]",
    "validate: parse via lightbox-color + patch ΔE vs reference       [G3]",
    "package:  emit assets/color/profiles/<make>/<model>.dcp + entry  [G4]",
    "select:   normalized CameraId → curated-if-present policy        [G5]",
];

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("stages") => {
            print_stages();
            Ok(())
        }
        Some("generate") => generate(args.get(1).map(String::as_str)),
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn print_stages() {
    println!("Content-line pipeline stages (Phase G):");
    for stage in STAGES {
        println!("  - {stage}");
    }
    println!(
        "\nG2/G6 (physical ColorChecker/IT8 capture + first curated batch) are DEFERRED:\n\
         they require capture sessions + dcamprof; no profiles are fabricated (spec §0)."
    );
}

fn generate(dir: Option<&str>) -> Result<()> {
    let Some(dir) = dir else {
        eprintln!("generate needs a <session-dir>\n\n{USAGE}");
        std::process::exit(2);
    };

    let session = Session::load(dir)?;
    let camera = session.camera_id();
    println!(
        "session {:?}: {} {} ({} illuminant(s), {} raw(s))",
        session.meta.session_id,
        camera.make,
        camera.model,
        session.meta.illuminants.len(),
        session.meta.raws.len(),
    );
    if !session.is_dual_illuminant() {
        eprintln!(
            "warning: single-illuminant session — a dual-illuminant DCP wants StdA + a daylight \
             (see docs/capture-protocol.md)"
        );
    }

    // Show the exact dcamprof wiring, then attempt to run it. On this machine
    // (dcamprof absent / feature off) `run` reports the DEFERRED status rather
    // than fabricating a profile.
    let work = std::env::temp_dir().join(format!("profgen-{}", session.meta.session_id));
    match Dcamprof::discover() {
        Ok(tool) => {
            let plan = tool.plan(&session, &work);
            println!("would run:");
            println!(
                "  {}",
                dcamprof::display_argv(tool.bin(), &plan.make_target_argv)
            );
            println!(
                "  {}",
                dcamprof::display_argv(tool.bin(), &plan.make_profile_argv)
            );
            match tool.run(&session, &work) {
                Ok(record) => {
                    println!(
                        "generated {} ({} bytes, dcamprof {})",
                        record.output_dcp.display(),
                        record.dcp.len(),
                        record.dcamprof_version,
                    );
                    println!("next: validate (G3) → package (G4). See docs/runbook.md.");
                }
                Err(e) => report_deferred(&e.to_string()),
            }
        }
        Err(e) => report_deferred(&e.to_string()),
    }
    Ok(())
}

fn report_deferred(reason: &str) {
    println!("DEFERRED: {reason}");
    println!(
        "G6 (real curated profiles) needs a capture session + dcamprof. Lightbox ships ZERO \n\
         bundled profiles; the tier-1 matrix base + tier-2 look render every body correctly \n\
         (spec §0/R4). See tools/lightbox-profgen/docs/runbook.md for the §12 escalation."
    );
}
