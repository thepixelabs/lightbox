// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! G3 — the profile **validation harness**.
//!
//! A generated `.dcp` is only allowed near `assets/` after it (a) parses with
//! **our own** F-phase engine ([`lightbox_color::dcp::parse_dcp`]) and (b) its
//! patch renders land within the ΔE reject gates versus a reference patch set —
//! the dcamprof reference render and/or the measured chart values (spec §6 G3,
//! F5 tolerances). This is the gate that keeps a corrupted or out-of-tolerance
//! profile out of the product.
//!
//! ΔE is measured with the shared [`lbx_image_compare`] CIEDE2000 (sRGB8 → Lab),
//! the same metric the golden gates use, so "validated here" means the same
//! thing as "passes the render golden".
//!
//! ## What is exercised on this machine (content DEFERRED)
//!
//! With no dcamprof and no target shots, the harness is unit-tested against
//! synthetic patch sets built from the license-clean matrix base — the plumbing
//! (parse → resolve → render → ΔE → gate) is fully covered; **no dcamprof patch
//! ΔE numbers are claimed** and **no `.dcp` content is fabricated** (spec §0).

use std::fmt;

use lightbox_color::{resolve_input_transform, CameraProfile, WbMode};
use serde::{Deserialize, Serialize};

/// How white balance is specified for a reference patch set (serde mirror of
/// [`WbMode`], which is not itself `Serialize`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum WbSpec {
    /// Use the set's `as_shot_neutral`.
    #[default]
    AsShot,
    /// A camera-native neutral triple.
    Neutral {
        /// The neutral RGB.
        neutral: [f64; 3],
    },
    /// A `(Kelvin, tint)` pair.
    TempTint {
        /// Correlated color temperature (Kelvin).
        kelvin: f64,
        /// Green–magenta tint.
        tint: f64,
    },
}

impl WbSpec {
    /// Converts to the color crate's [`WbMode`].
    pub fn to_mode(self) -> WbMode {
        match self {
            WbSpec::AsShot => WbMode::AsShot,
            WbSpec::Neutral { neutral } => WbMode::Neutral(neutral),
            WbSpec::TempTint { kelvin, tint } => WbMode::TempTint { kelvin, tint },
        }
    }
}

/// One reference patch: a camera-native linear input and the expected rendered
/// sRGB (dcamprof reference render, or the sRGB of a measured chart Lab).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Patch label (e.g. `"neutral-5"`, `"skin"`).
    pub name: String,
    /// Camera-native linear RGB fed into the input transform.
    pub camera_rgb: [f32; 3],
    /// Reference rendered sRGB (8-bit).
    pub reference_srgb8: [u8; 3],
}

/// A reference patch set for one illuminant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchSet {
    /// The illuminant these patches were rendered/measured under.
    pub illuminant: String,
    /// The as-shot neutral (used when [`WbSpec::AsShot`]).
    #[serde(default)]
    pub as_shot_neutral: Option<[f64; 3]>,
    /// How to white-balance the render.
    #[serde(default)]
    pub wb: WbSpec,
    /// The reference patches.
    pub patches: Vec<Patch>,
}

/// ΔE reject gates (mean + max), per patch set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gates {
    /// Maximum allowed mean ΔE2000 across the patch set.
    pub mean_de: f64,
    /// Maximum allowed single-patch ΔE2000.
    pub max_de: f64,
}

/// The F5 curated-profile tolerances (spec §7.2): mean ΔE2000 ≤ 0.5, max ≤ 1.5.
pub const F5_GATES: Gates = Gates {
    mean_de: 0.5,
    max_de: 1.5,
};

/// The per-patch ΔE measurement.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchDelta {
    /// The patch label.
    pub name: String,
    /// Its ΔE2000 versus the reference.
    pub de: f64,
}

/// Why validation rejected a profile.
#[derive(Clone, Debug, PartialEq)]
pub enum RejectReason {
    /// The `.dcp` bytes did not parse with our engine.
    Parse(String),
    /// The patch set had no patches to measure.
    NoPatches,
    /// The input transform could not be resolved (singular matrix, etc.).
    Resolve(String),
    /// A ΔE gate was exceeded.
    Gate {
        /// The measured mean ΔE.
        mean_de: f64,
        /// The measured max ΔE.
        max_de: f64,
    },
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RejectReason::Parse(e) => write!(f, "parse failed: {e}"),
            RejectReason::NoPatches => write!(f, "patch set is empty"),
            RejectReason::Resolve(e) => write!(f, "resolve failed: {e}"),
            RejectReason::Gate { mean_de, max_de } => {
                write!(f, "ΔE gate exceeded (mean {mean_de:.4}, max {max_de:.4})")
            }
        }
    }
}

/// Accepted or rejected (with a reason).
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Within gates.
    Accepted,
    /// Rejected, with cause.
    Rejected(RejectReason),
}

/// The full validation report for one patch set.
#[derive(Clone, Debug)]
pub struct ValidationReport {
    /// The illuminant validated.
    pub illuminant: String,
    /// Number of patches measured (0 when rejected before render).
    pub patch_count: usize,
    /// Mean ΔE2000 (0 when rejected before render).
    pub mean_de: f64,
    /// Max ΔE2000 (0 when rejected before render).
    pub max_de: f64,
    /// Per-patch ΔE.
    pub per_patch: Vec<PatchDelta>,
    /// The gates applied.
    pub gates: Gates,
    /// The verdict.
    pub outcome: Outcome,
}

impl ValidationReport {
    /// True when the profile passed the gates.
    pub fn accepted(&self) -> bool {
        matches!(self.outcome, Outcome::Accepted)
    }

    fn rejected(illuminant: &str, gates: Gates, reason: RejectReason) -> ValidationReport {
        ValidationReport {
            illuminant: illuminant.to_owned(),
            patch_count: 0,
            mean_de: 0.0,
            max_de: 0.0,
            per_patch: Vec::new(),
            gates,
            outcome: Outcome::Rejected(reason),
        }
    }
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} — {} patch(es), ΔE2000 mean {:.4} / max {:.4} (gates mean ≤ {}, max ≤ {})",
            self.illuminant,
            match &self.outcome {
                Outcome::Accepted => "ACCEPTED".to_owned(),
                Outcome::Rejected(r) => format!("REJECTED: {r}"),
            },
            self.patch_count,
            self.mean_de,
            self.max_de,
            self.gates.mean_de,
            self.gates.max_de,
        )
    }
}

/// Renders each patch through `profile` and measures ΔE2000 versus the reference,
/// then applies `gates`. The core harness — testable with an in-memory profile,
/// no `.dcp` file or dcamprof required.
pub fn validate_profile(profile: &CameraProfile, set: &PatchSet, gates: Gates) -> ValidationReport {
    if set.patches.is_empty() {
        return ValidationReport::rejected(&set.illuminant, gates, RejectReason::NoPatches);
    }

    let rit =
        match resolve_input_transform(profile, &set.wb.to_mode(), set.as_shot_neutral, None, 1.0) {
            Ok(r) => r,
            Err(e) => {
                return ValidationReport::rejected(
                    &set.illuminant,
                    gates,
                    RejectReason::Resolve(e.to_string()),
                )
            }
        };

    let mut per_patch = Vec::with_capacity(set.patches.len());
    let mut sum = 0.0f64;
    let mut max = 0.0f64;
    for p in &set.patches {
        let rendered = rit.render_reference_srgb8(p.camera_rgb);
        let de = lbx_image_compare::ciede2000(
            lbx_image_compare::srgb8_to_lab(rendered),
            lbx_image_compare::srgb8_to_lab(p.reference_srgb8),
        );
        sum += de;
        max = max.max(de);
        per_patch.push(PatchDelta {
            name: p.name.clone(),
            de,
        });
    }
    let mean = sum / per_patch.len() as f64;

    let outcome = if mean <= gates.mean_de && max <= gates.max_de {
        Outcome::Accepted
    } else {
        Outcome::Rejected(RejectReason::Gate {
            mean_de: mean,
            max_de: max,
        })
    };

    ValidationReport {
        illuminant: set.illuminant.clone(),
        patch_count: per_patch.len(),
        mean_de: mean,
        max_de: max,
        per_patch,
        gates,
        outcome,
    }
}

/// Parses `.dcp` bytes with the F-phase engine, then validates. A parse failure
/// is a rejection (not a panic) — untrusted-input hygiene carries through.
pub fn parse_and_validate(dcp_bytes: &[u8], set: &PatchSet, gates: Gates) -> ValidationReport {
    match lightbox_color::dcp::parse_dcp(dcp_bytes) {
        Ok(profile) => validate_profile(&profile, set, gates),
        Err(e) => {
            ValidationReport::rejected(&set.illuminant, gates, RejectReason::Parse(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_color::camera_matrix_base;
    use lightbox_decode::{normalize_camera, Illuminant, RawColorimetry};

    /// A license-clean matrix-base profile whose native space is XYZ(D50)
    /// (ColorMatrix1 = identity, single D50 illuminant). Stands in for a curated
    /// DCP for the plumbing tests — it is a real, derived profile, not fabricated
    /// content.
    fn test_profile() -> CameraProfile {
        let colorimetry = RawColorimetry {
            as_shot_neutral: Some([0.9642, 1.0, 0.8249]), // D50 white, Y=1
            illuminant1: Illuminant::D50,
            illuminant2: None,
            color_matrix1: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            color_matrix2: None,
            forward_matrix1: None,
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        };
        let cam = normalize_camera("Synthetic", "MatrixBase");
        camera_matrix_base(&colorimetry, &cam).unwrap()
    }

    /// A gray/color ramp of camera-native inputs, rendered by `profile` to build
    /// a self-consistent reference set (ΔE ≈ 0 against itself).
    fn self_reference(profile: &CameraProfile) -> PatchSet {
        let rit = resolve_input_transform(
            profile,
            &WbMode::Neutral([0.9642, 1.0, 0.8249]),
            None,
            None,
            1.0,
        )
        .unwrap();
        let inputs: [[f32; 3]; 6] = [
            [0.05, 0.05, 0.05],
            [0.20, 0.20, 0.20],
            [0.50, 0.50, 0.50],
            [0.30, 0.10, 0.08],
            [0.10, 0.30, 0.12],
            [0.08, 0.12, 0.35],
        ];
        let patches = inputs
            .iter()
            .enumerate()
            .map(|(i, &rgb)| Patch {
                name: format!("patch-{i}"),
                camera_rgb: rgb,
                reference_srgb8: rit.render_reference_srgb8(rgb),
            })
            .collect();
        PatchSet {
            illuminant: "D50".into(),
            as_shot_neutral: Some([0.9642, 1.0, 0.8249]),
            wb: WbSpec::Neutral {
                neutral: [0.9642, 1.0, 0.8249],
            },
            patches,
        }
    }

    #[test]
    fn sample_profile_passes_its_own_reference() {
        let profile = test_profile();
        let set = self_reference(&profile);
        let report = validate_profile(&profile, &set, F5_GATES);
        assert!(report.accepted(), "self-reference must pass: {report}");
        assert!(report.max_de < 0.5, "self-render ΔE ~ 0: {report}");
        assert_eq!(report.patch_count, 6);
    }

    #[test]
    fn out_of_tolerance_profile_is_rejected_with_a_report() {
        let profile = test_profile();
        let mut set = self_reference(&profile);
        // Perturb the references far past the gate (a big green push on every
        // patch — an obviously-wrong profile).
        for p in &mut set.patches {
            p.reference_srgb8[1] = p.reference_srgb8[1].saturating_add(70);
        }
        let report = validate_profile(&profile, &set, F5_GATES);
        assert!(!report.accepted());
        match report.outcome {
            Outcome::Rejected(RejectReason::Gate { max_de, .. }) => {
                assert!(max_de > F5_GATES.max_de, "{report}");
            }
            other => panic!("expected a gate rejection, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_dcp_is_rejected_not_panicked() {
        let profile = test_profile();
        let set = self_reference(&profile);
        // Feed garbage bytes to the parse-then-validate path.
        let report = parse_and_validate(b"this is not a DCP container", &set, F5_GATES);
        assert!(!report.accepted());
        assert!(
            matches!(report.outcome, Outcome::Rejected(RejectReason::Parse(_))),
            "{report}"
        );
    }

    #[test]
    fn empty_patch_set_is_rejected() {
        let profile = test_profile();
        let set = PatchSet {
            illuminant: "D50".into(),
            as_shot_neutral: None,
            wb: WbSpec::AsShot,
            patches: Vec::new(),
        };
        let report = validate_profile(&profile, &set, F5_GATES);
        assert_eq!(report.outcome, Outcome::Rejected(RejectReason::NoPatches));
    }

    #[test]
    fn patch_set_round_trips_through_toml() {
        let profile = test_profile();
        let set = self_reference(&profile);
        let text = toml::to_string(&set).unwrap();
        let back: PatchSet = toml::from_str(&text).unwrap();
        assert_eq!(set, back);
    }
}
