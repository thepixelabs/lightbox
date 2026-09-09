// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! G1, the target-shot **session** dir + metadata schema.
//!
//! A session is a directory holding the raw target shots plus a `session.toml`
//! metadata file describing the body, the chart, and the capture illuminants
//! everything [`dcamprof`](crate::dcamprof) needs to build a profile and
//! everything [`package`](crate::package) needs to write provenance. The schema
//! is the machine-readable half of the G2 capture-protocol document
//! (`docs/capture-protocol.md`).
//!
//! ## Format note (spec deviation)
//!
//! Spec §6 G1 names "YAML metadata". This is implemented as **TOML**
//! (`session.toml`) to match the repo-wide manifest convention
//! (`assets/MANIFEST.toml`, `*.review.toml`, `scene-corpus.toml`, the camera
//! alias table) and to avoid adding an unvetted YAML crate to the license graph.
//! The schema is field-equivalent; recorded in `E02-deviations.md`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use lightbox_decode::{normalize_camera, CameraId};
use serde::{Deserialize, Serialize};

/// The reference chart a session was shot against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChartKind {
    /// X-Rite / Calibrite ColorChecker Classic (24 patches).
    ColorChecker24,
    /// ColorChecker Digital SG (140 patches).
    ColorCheckerSg,
    /// An IT8.7 transmissive/reflective target.
    It8,
}

impl ChartKind {
    /// The nominal patch count, used by the harness as an upper sanity bound.
    pub fn patch_count(self) -> usize {
        match self {
            ChartKind::ColorChecker24 => 24,
            ChartKind::ColorCheckerSg => 140,
            ChartKind::It8 => 288,
        }
    }
}

/// The camera body a session profiles.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BodyMeta {
    /// EXIF `Make` string as it appears in the raws (normalized on read).
    pub make: String,
    /// EXIF `Model` string as it appears in the raws (normalized on read).
    pub model: String,
}

/// One capture illuminant in a (usually dual-illuminant) session.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CaptureIlluminant {
    /// A short label used to cross-reference the raw shot (e.g. `"StdA"`,
    /// `"D65"`, `"daylight"`).
    pub name: String,
    /// Correlated color temperature in Kelvin, when known (StdA ≈ 2856).
    pub cct: Option<f64>,
}

/// One target-shot raw belonging to a session.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CaptureRaw {
    /// The [`CaptureIlluminant::name`] this shot was taken under.
    pub illuminant: String,
    /// Raw filename, relative to the session dir.
    pub path: String,
}

/// The `session.toml` schema (G1 / G2).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Stable session id, becomes the profile provenance (`profgen:<id>`).
    pub session_id: String,
    /// The profiled body.
    pub body: BodyMeta,
    /// The reference chart.
    pub chart: ChartKind,
    /// The capture illuminants (≥1; ≥2 for a dual-illuminant profile).
    pub illuminants: Vec<CaptureIlluminant>,
    /// The target-shot raws.
    pub raws: Vec<CaptureRaw>,
    /// ISO-8601 capture date, optional.
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Who shot the session, optional.
    #[serde(default)]
    pub operator: Option<String>,
    /// Free-form notes, optional.
    #[serde(default)]
    pub notes: Option<String>,
}

/// The metadata file name inside a session dir.
pub const SESSION_FILE: &str = "session.toml";

/// A loaded session: its root dir + parsed, schema-checked metadata.
#[derive(Clone, Debug)]
pub struct Session {
    /// The session directory.
    pub root: PathBuf,
    /// The parsed metadata.
    pub meta: SessionMeta,
}

impl Session {
    /// Loads and schema-checks `<dir>/session.toml`. Does **not** touch the raw
    /// pixels, that is [`dcamprof`](crate::dcamprof)'s job.
    pub fn load(dir: impl AsRef<Path>) -> Result<Session> {
        let root = dir.as_ref().to_path_buf();
        let meta_path = root.join(SESSION_FILE);
        let text = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("reading {}", meta_path.display()))?;
        let meta: SessionMeta =
            toml::from_str(&text).with_context(|| format!("parsing {}", meta_path.display()))?;
        let session = Session { root, meta };
        session.check_schema()?;
        Ok(session)
    }

    /// Builds a session from already-parsed metadata (used by tests / callers
    /// that assemble a session in memory).
    pub fn from_meta(root: impl Into<PathBuf>, meta: SessionMeta) -> Result<Session> {
        let session = Session {
            root: root.into(),
            meta,
        };
        session.check_schema()?;
        Ok(session)
    }

    /// The normalized identity of the profiled body, the join key shared with
    /// curated-profile lookup ([`select`](crate::select)) and the catalog.
    pub fn camera_id(&self) -> CameraId {
        normalize_camera(&self.meta.body.make, &self.meta.body.model)
    }

    /// Whether this session carries the ≥2 illuminants a dual-illuminant DCP
    /// wants (StdA + a daylight per the G2 protocol). Single-illuminant sessions
    /// are allowed but flagged by the harness.
    pub fn is_dual_illuminant(&self) -> bool {
        self.meta.illuminants.len() >= 2
    }

    /// Absolute paths to every target-shot raw, in declared order.
    pub fn raw_paths(&self) -> Vec<PathBuf> {
        self.meta
            .raws
            .iter()
            .map(|r| self.root.join(&r.path))
            .collect()
    }

    /// Structural validation independent of the pixels: non-empty id, at least
    /// one illuminant and one raw, every raw references a declared illuminant,
    /// and the patch count does not exceed the chart's nominal size.
    fn check_schema(&self) -> Result<()> {
        let m = &self.meta;
        if m.session_id.trim().is_empty() {
            bail!("session_id is empty");
        }
        if m.body.make.trim().is_empty() || m.body.model.trim().is_empty() {
            bail!("body.make / body.model must both be set");
        }
        if m.illuminants.is_empty() {
            bail!("a session needs at least one capture illuminant");
        }
        if m.raws.is_empty() {
            bail!("a session needs at least one target-shot raw");
        }
        for raw in &m.raws {
            if !m.illuminants.iter().any(|i| i.name == raw.illuminant) {
                bail!(
                    "raw {:?} references undeclared illuminant {:?}",
                    raw.path,
                    raw.illuminant
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> SessionMeta {
        SessionMeta {
            session_id: "session-2026-07-06-a".into(),
            body: BodyMeta {
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 6".into(),
            },
            chart: ChartKind::ColorChecker24,
            illuminants: vec![
                CaptureIlluminant {
                    name: "StdA".into(),
                    cct: Some(2856.0),
                },
                CaptureIlluminant {
                    name: "daylight".into(),
                    cct: Some(6504.0),
                },
            ],
            raws: vec![
                CaptureRaw {
                    illuminant: "StdA".into(),
                    path: "stda.nef".into(),
                },
                CaptureRaw {
                    illuminant: "daylight".into(),
                    path: "d65.nef".into(),
                },
            ],
            captured_at: Some("2026-07-06".into()),
            operator: None,
            notes: None,
        }
    }

    #[test]
    fn session_round_trips_through_toml() {
        let meta = sample_meta();
        let text = toml::to_string(&meta).unwrap();
        let back: SessionMeta = toml::from_str(&text).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn camera_id_normalizes_the_body() {
        let s = Session::from_meta("/nonexistent", sample_meta()).unwrap();
        let id = s.camera_id();
        // The §3.6 canonical example: spelling drift collapses to (Nikon, Z6).
        assert_eq!(id.make, "Nikon");
        assert_eq!(id.model, "Z6");
        assert!(s.is_dual_illuminant());
    }

    #[test]
    fn schema_rejects_raw_with_unknown_illuminant() {
        let mut meta = sample_meta();
        meta.raws[0].illuminant = "flash".into();
        let err = Session::from_meta("/x", meta).unwrap_err();
        assert!(err.to_string().contains("undeclared illuminant"), "{err}");
    }

    #[test]
    fn schema_rejects_empty_session() {
        let mut meta = sample_meta();
        meta.illuminants.clear();
        meta.raws.clear();
        assert!(Session::from_meta("/x", meta).is_err());
    }

    #[test]
    fn load_reads_session_toml_from_disk() {
        let dir = std::env::temp_dir().join(format!("profgen-sess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = toml::to_string(&sample_meta()).unwrap();
        std::fs::write(dir.join(SESSION_FILE), text).unwrap();

        let session = Session::load(&dir).unwrap();
        assert_eq!(session.meta.session_id, "session-2026-07-06-a");
        assert_eq!(session.raw_paths().len(), 2);
        assert!(session.raw_paths()[0].ends_with("stda.nef"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
