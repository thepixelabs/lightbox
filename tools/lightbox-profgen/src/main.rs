// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-profgen` — the internal curated-camera-profile generator
//! (E02.5 / Phase G). **NOT shipped in the app** (spec §1.1/§2): it orchestrates
//! `dcamprof` as a GPL-3 **subprocess** (never linked, never bundled) over our
//! own ColorChecker/IT8 target-shot sessions → validated `.dcp` under the
//! project license, then runs the validation harness (parse with the F-phase
//! engine in `lightbox-color`, patch-render ΔE gates) and emits surface-3
//! manifest entries.
//!
//! **Scaffold state (E02 Phase A):** a CLI skeleton that names the pipeline
//! stages. G1/G3/G4/G5/G7 fill the real orchestration; **G2/G6 (physical
//! target-shot capture + first curated batch) are DEFERRED** — no profiles or
//! reference renders are fabricated (spec §0).

use anyhow::Result;

/// The pipeline stages Phase G fills (documented here so the skeleton is a
/// truthful map of the deferred work).
const STAGES: &[&str] = &[
    "session:  load a target-shot session dir (raws + YAML metadata) [G1]",
    "generate: run dcamprof as a GPL-3 SUBPROCESS → .dcp             [G1]",
    "validate: parse via lightbox-color + patch ΔE vs reference      [G3]",
    "package:  emit assets/color/profiles/<make>/<model>.dcp + entry [G4]",
    "select:   normalized CameraId → curated-if-present policy        [G5]",
];

fn main() -> Result<()> {
    eprintln!(
        "lightbox-profgen (E02 Phase A scaffold — internal tool, never shipped).\n\
         dcamprof is invoked as a GPL-3 subprocess only; nothing is linked or bundled.\n\
         Pipeline stages Phase G fills:"
    );
    for stage in STAGES {
        eprintln!("  - {stage}");
    }
    eprintln!(
        "\nG2/G6 (physical ColorChecker/IT8 capture + first curated batch) are DEFERRED:\n\
         they require capture sessions + dcamprof; no profiles are fabricated."
    );
    Ok(())
}
