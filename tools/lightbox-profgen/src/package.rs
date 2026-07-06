// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! G4 — packaging + provenance + the catalog sync descriptor.
//!
//! A profile that has passed [`validate`](crate::validate) is packaged into
//! `assets/color/profiles/<make>/<model>.dcp` with a surface-3
//! `assets/MANIFEST.toml` entry (session id, dcamprof version, project license)
//! and a [`CatalogSyncRow`] — the identity + provenance the app's
//! `camera_profile` table is re-synced from **on catalog open** (spec §4.1/§4.3,
//! like `model_pack`).
//!
//! ## Ownership boundary (spec deviation)
//!
//! Phase G owns the *producer* side here: writing the cleared `.dcp`, emitting
//! the manifest entry, and computing the [`CatalogSyncRow`] the catalog reads.
//! The `camera_profile` **table + migration + sync-on-open** live in
//! `lightbox-catalog` (task **H1**), which this tool does not own. The
//! [`CatalogSyncRow`] is the contract between the two. Recorded in
//! `E02-deviations.md`.
//!
//! ## Content DEFERRED
//!
//! Nothing is written to the real `assets/` tree here — Lightbox ships **ZERO**
//! bundled profiles (spec §0). [`write_package`] targets a caller-supplied root
//! (a work dir in tests); it only touches the shipped tree once G6 produces a
//! validated profile.

use std::path::{Path, PathBuf};

use lightbox_color::CameraProfile;
use lightbox_decode::CameraId;
use serde::{Deserialize, Serialize};

/// The project license string recorded for every bundled color asset.
pub const PROJECT_LICENSE: &str = "project";

/// A surface-3 `assets/MANIFEST.toml` `[[asset]]` entry for a camera profile
/// (spec §4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path relative to `assets/`, e.g. `color/profiles/Sony/ILCE-7M4.dcp`.
    pub path: String,
    /// Asset kind — always `camera-profile` here.
    pub kind: String,
    /// Provenance: `profgen:<session_id>` (spec §4.3).
    pub provenance: String,
    /// License — `project`.
    pub license: String,
    /// Whether the file ships in the app bundle.
    pub bundled: bool,
    /// The dcamprof version that produced the profile (provenance detail; emitted
    /// as a trailing comment so it is human-visible without changing the schema
    /// the checker reads).
    #[serde(skip)]
    pub dcamprof_version: String,
}

/// One `camera_profile` row the catalog re-syncs on open (spec §4.1). Produced
/// here; **inserted by `lightbox-catalog` (H1)**.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogSyncRow {
    /// `ProfileId` hex (primary key).
    pub id: String,
    /// `dcp` | `look` — always `dcp` here.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Normalized make.
    pub camera_make: String,
    /// Normalized model.
    pub camera_model: String,
    /// `bundled` | `user` — always `bundled` here.
    pub source: String,
    /// License (mirrors the manifest entry).
    pub license: String,
    /// Path relative to `assets/`.
    pub file_path: String,
    /// xxh3-128 hex of the `.dcp` bytes.
    pub file_hash: String,
}

/// Everything needed to write + register a packaged profile.
#[derive(Clone, Debug, PartialEq)]
pub struct PackagePlan {
    /// Path relative to `assets/` (manifest + catalog `file_path`).
    pub dest_rel: String,
    /// Absolute destination the profile is written to.
    pub dest_abs: PathBuf,
    /// xxh3-128 hex of the `.dcp` bytes.
    pub file_hash: String,
    /// The surface-3 manifest entry.
    pub entry: ManifestEntry,
    /// The catalog sync row.
    pub catalog_row: CatalogSyncRow,
}

/// Builds the packaging plan for a validated profile. **Pure** — computes paths,
/// hash, manifest entry, and catalog row without writing anything.
///
/// `assets_root` is the `assets/` directory the file lands under (the real tree
/// only at ship time; a work dir in tests).
pub fn plan_package(
    profile: &CameraProfile,
    camera: &CameraId,
    session_id: &str,
    dcamprof_version: &str,
    dcp_bytes: &[u8],
    assets_root: impl AsRef<Path>,
) -> PackagePlan {
    let make_dir = sanitize(&camera.make);
    let model_file = sanitize(&camera.model);
    let dest_rel = format!("color/profiles/{make_dir}/{model_file}.dcp");
    let dest_abs = assets_root.as_ref().join(&dest_rel);

    let file_hash = crate::hex(&twox_hash::XxHash3_128::oneshot(dcp_bytes).to_le_bytes());

    let entry = ManifestEntry {
        path: dest_rel.clone(),
        kind: "camera-profile".into(),
        provenance: format!("profgen:{session_id}"),
        license: PROJECT_LICENSE.into(),
        bundled: true,
        dcamprof_version: dcamprof_version.to_owned(),
    };

    let catalog_row = CatalogSyncRow {
        id: crate::hex(&profile.id.0),
        kind: "dcp".into(),
        name: profile.name.clone(),
        camera_make: camera.make.clone(),
        camera_model: camera.model.clone(),
        source: "bundled".into(),
        license: PROJECT_LICENSE.into(),
        file_path: dest_rel.clone(),
        file_hash: file_hash.clone(),
    };

    PackagePlan {
        dest_rel,
        dest_abs,
        file_hash,
        entry,
        catalog_row,
    }
}

/// Renders a manifest entry as an `[[asset]]` TOML block, ready to append to
/// `assets/MANIFEST.toml`. The dcamprof version rides as a trailing comment (the
/// checker keys on `provenance`, humans want the tool version too).
pub fn manifest_entry_toml(entry: &ManifestEntry) -> String {
    let mut block = String::from("[[asset]]\n");
    block.push_str(&format!("path       = {}\n", toml_str(&entry.path)));
    block.push_str(&format!("kind       = {}\n", toml_str(&entry.kind)));
    block.push_str(&format!("provenance = {}", toml_str(&entry.provenance)));
    if !entry.dcamprof_version.is_empty() {
        block.push_str(&format!("  # dcamprof {}", entry.dcamprof_version));
    }
    block.push('\n');
    block.push_str(&format!("license    = {}\n", toml_str(&entry.license)));
    block.push_str(&format!("bundled    = {}\n", entry.bundled));
    block
}

/// The surface-3 provenance guard, applied **before** a profile is written: the
/// same policy the manifest checker enforces (spec §4.3). Provenance must be a
/// `profgen:` (or cleared-community) origin and nothing may indicate Adobe
/// authorship. Keeps an unvetted profile out of the tree at the producer.
pub fn check_provenance(entry: &ManifestEntry) -> Result<(), String> {
    let prov = entry.provenance.trim();
    let cleared = prov.starts_with("profgen:")
        || prov.starts_with("community:")
        || prov == "lightbox-authored";
    if !cleared {
        return Err(format!("provenance {prov:?} is not a cleared origin"));
    }
    for field in [
        prov,
        entry.license.as_str(),
        entry.dcamprof_version.as_str(),
    ] {
        if field.to_ascii_lowercase().contains("adobe") {
            return Err(format!(
                "Adobe authorship marker in {field:?} (constraint 4)"
            ));
        }
    }
    Ok(())
}

/// Writes the packaged `.dcp` to [`PackagePlan::dest_abs`], creating parent
/// dirs. Refuses to write a profile whose provenance is not cleared.
pub fn write_package(plan: &PackagePlan, dcp_bytes: &[u8]) -> std::io::Result<()> {
    if let Err(e) = check_provenance(&plan.entry) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, e));
    }
    if let Some(parent) = plan.dest_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plan.dest_abs, dcp_bytes)
}

/// Filesystem-safe token: keep ASCII alphanumerics, `-` and `_`; map everything
/// else (spaces, separators, dots) to `-`. Dots are dropped deliberately so a
/// hostile `..` in a make/model can never escape the profiles dir.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Minimal TOML basic-string quoting for the fields we emit (no control chars in
/// paths/provenance; escape `\` and `"`).
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_color::camera_matrix_base;
    use lightbox_decode::{normalize_camera, Illuminant, RawColorimetry};

    fn profile_and_camera() -> (CameraProfile, CameraId) {
        let colorimetry = RawColorimetry {
            as_shot_neutral: Some([0.9642, 1.0, 0.8249]),
            illuminant1: Illuminant::D50,
            illuminant2: None,
            color_matrix1: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            color_matrix2: None,
            forward_matrix1: None,
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        };
        let cam = normalize_camera("SONY", "ILCE-7M4");
        let profile = camera_matrix_base(&colorimetry, &cam).unwrap();
        (profile, cam)
    }

    #[test]
    fn plan_lays_out_path_hash_and_provenance() {
        let (profile, cam) = profile_and_camera();
        let bytes = b"fake dcp bytes for hashing";
        let plan = plan_package(
            &profile,
            &cam,
            "session-2026-07-06",
            "1.0.14",
            bytes,
            "/assets",
        );

        assert_eq!(plan.dest_rel, "color/profiles/Sony/ILCE-7M4.dcp");
        assert_eq!(
            plan.dest_abs,
            PathBuf::from("/assets/color/profiles/Sony/ILCE-7M4.dcp")
        );
        assert_eq!(plan.entry.provenance, "profgen:session-2026-07-06");
        assert_eq!(plan.entry.kind, "camera-profile");
        assert!(plan.entry.bundled);

        // The hash is xxh3-128 of the bytes, and it matches the catalog row.
        let expected = crate::hex(&twox_hash::XxHash3_128::oneshot(bytes).to_le_bytes());
        assert_eq!(plan.file_hash, expected);
        assert_eq!(plan.catalog_row.file_hash, expected);
        assert_eq!(plan.catalog_row.camera_make, "Sony");
        assert_eq!(plan.catalog_row.camera_model, "ILCE-7M4");
        assert_eq!(plan.catalog_row.source, "bundled");
        assert_eq!(plan.catalog_row.id, crate::hex(&profile.id.0));
    }

    #[test]
    fn manifest_entry_toml_parses_back_to_the_same_fields() {
        let (profile, cam) = profile_and_camera();
        let plan = plan_package(&profile, &cam, "session-x", "1.0.14", b"x", "/assets");
        let block = manifest_entry_toml(&plan.entry);

        assert!(block.contains("# dcamprof 1.0.14"), "{block}");

        // The block is a valid [[asset]] table matching the entry (dcamprof
        // version is a comment, so serde skips it).
        #[derive(serde::Deserialize)]
        struct Doc {
            asset: Vec<ManifestEntry>,
        }
        let doc: Doc = toml::from_str(&block).unwrap();
        assert_eq!(doc.asset.len(), 1);
        let mut expected = plan.entry.clone();
        expected.dcamprof_version = String::new(); // skipped in TOML
        assert_eq!(doc.asset[0], expected);
    }

    #[test]
    fn provenance_guard_rejects_uncleared_and_adobe() {
        let (profile, cam) = profile_and_camera();
        let plan = plan_package(&profile, &cam, "s", "1.0", b"x", "/a");
        assert!(check_provenance(&plan.entry).is_ok());

        let mut bad = plan.entry.clone();
        bad.provenance = "mystery-origin".into();
        assert!(check_provenance(&bad).is_err());

        let mut adobe = plan.entry.clone();
        adobe.dcamprof_version = "Adobe DNG Converter".into();
        assert!(check_provenance(&adobe).is_err());
    }

    #[test]
    fn write_package_writes_bytes_and_creates_dirs() {
        let (profile, cam) = profile_and_camera();
        let root = std::env::temp_dir().join(format!("profgen-pkg-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let bytes = b"round-trip these dcp bytes";
        let plan = plan_package(&profile, &cam, "s", "1.0", bytes, &root);

        write_package(&plan, bytes).unwrap();
        let read_back = std::fs::read(&plan.dest_abs).unwrap();
        assert_eq!(read_back, bytes);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sanitize_makes_filesystem_safe_tokens() {
        assert_eq!(sanitize("EOS R6"), "EOS-R6");
        assert_eq!(sanitize("Z6II"), "Z6II");
        assert_eq!(sanitize("../etc"), "etc");
        assert_eq!(sanitize("///"), "unknown");
    }
}
