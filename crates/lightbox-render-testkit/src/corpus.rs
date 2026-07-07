// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Corpus manifest + `GoldenCase` runner + per-PV golden matrix (spec §6; tasks
//! **A16** / **D3**).
//!
//! Owner: **A-core** (A16 first single-node golden + manifest/runner) and **D**
//! (per-PV matrix, D3). The manifest (de)serialization here is real; the
//! rendering/comparison runner is a stub until A16 wires the engine.
//!
//! Corpus: ≥ 6 synthetic sources — gradients / checker / low-key / high-key /
//! high-frequency / wide-gamut / 100 MP-synthetic — committed with provenance
//! (own-shot / CC0 only). Goldens rendered from the **CPU reference path**.

use std::path::PathBuf;

use lightbox_types::ProcessVersion;
use serde::{Deserialize, Serialize};

use crate::compare::DeltaEStats;

/// The corpus manifest — the committed list of test sources (spec §6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CorpusManifest {
    /// Manifest format version.
    pub version: u32,
    /// The test sources.
    pub sources: Vec<CorpusSource>,
}

impl CorpusManifest {
    /// Serialize to pretty JSON (the committed on-disk form).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("CorpusManifest serializes")
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> Result<CorpusManifest, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// One synthetic corpus source (spec §6).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CorpusSource {
    /// Stable source name (keys goldens).
    pub name: String,
    /// The synthetic pattern kind.
    pub kind: CorpusKind,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Provenance note (own-shot / CC0 only — §8 surface 3 applies to test data).
    pub provenance: String,
}

/// The synthetic corpus pattern families (spec §6).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpusKind {
    /// Smooth gradients.
    Gradient,
    /// Hard-edged checker.
    Checker,
    /// Low-key (shadow-weighted).
    LowKey,
    /// High-key (highlight-weighted).
    HighKey,
    /// High-frequency detail.
    HighFrequency,
    /// Wide-gamut saturated.
    WideGamut,
    /// 100 MP-synthetic (VRAM-budget stress).
    Synthetic100Mp,
}

/// One golden case: a `(node, pv, source, recipe)` rendered and compared to a
/// committed golden PNG (spec §6/A16).
#[derive(Clone, Debug)]
pub struct GoldenCase {
    /// Dotted node id (e.g. `"xform.display"`).
    pub node: String,
    /// The process version to render under.
    pub pv: ProcessVersion,
    /// The corpus source name.
    pub source: String,
    /// The recipe, as JSON (materialized by A16/D).
    pub recipe_json: String,
    /// Path to the committed golden PNG.
    pub golden_path: PathBuf,
}

/// The outcome of a golden comparison (spec §6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GoldenReport {
    /// ΔE2000 statistics vs the golden.
    pub stats: DeltaEStats,
    /// PSNR (dB) vs the golden.
    pub psnr_db: f64,
    /// Whether the case passed the §4.4 tolerance.
    pub passed: bool,
}

/// Render `case` through the engine (CPU reference path) and compare to its
/// golden (spec §6; task A16). **A16 wires the engine.**
pub fn run_golden(case: &GoldenCase) -> GoldenReport {
    let _ = case;
    unimplemented!("A16 (A-core): GoldenCase runner — render + ΔE2000/PSNR compare")
}

/// Run the per-PV golden matrix (corpus × recipes × PVs; spec §6; task D3).
pub fn run_matrix(cases: &[GoldenCase]) -> Vec<GoldenReport> {
    let _ = cases;
    unimplemented!("D3 (D): per-PV golden matrix runner")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_manifest_json_round_trips() {
        let manifest = CorpusManifest {
            version: 1,
            sources: vec![
                CorpusSource {
                    name: "grad-01".to_owned(),
                    kind: CorpusKind::Gradient,
                    width: 256,
                    height: 256,
                    provenance: "own-shot/CC0".to_owned(),
                },
                CorpusSource {
                    name: "checker-01".to_owned(),
                    kind: CorpusKind::Checker,
                    width: 512,
                    height: 512,
                    provenance: "own-shot/CC0".to_owned(),
                },
            ],
        };
        let json = manifest.to_json();
        let parsed = CorpusManifest::from_json(&json).expect("round-trips");
        assert_eq!(manifest, parsed);
    }
}
