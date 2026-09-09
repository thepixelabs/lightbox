// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! G5, the curated-profile **auto-selection policy**.
//!
//! Given a normalized [`CameraId`], pick the default profile: the curated DCP
//! bundled for that body **if present**, else the license-clean matrix base
//! (spec §6 G5, Risk 10). A per-image override always wins. This is the
//! content-line's side of the policy `resolve_profile_ref` (in `lightbox-color`,
//! F7) enforces at render time; it is documented here for E10's profile browser.
//!
//! The match is on the **normalized** `(make, model)`, so the §3.6 alias table
//! collapses `"NIKON CORPORATION" / "NIKON Z 6"` and `"Nikon" / "Z6"` to the same
//! lookup. With **ZERO** curated profiles bundled today (spec §0), every lookup
//! resolves to [`Selection::MatrixBase`]; the policy is unit-tested so it is
//! correct the moment a curated batch (G6) lands.

use lightbox_color::ProfileId;
use lightbox_decode::{normalize_camera, CameraId};

/// One curated profile available for auto-selection (one packaged `.dcp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuratedEntry {
    /// The normalized body this profile is for.
    pub camera: CameraId,
    /// The profile id.
    pub profile_id: ProfileId,
    /// Path relative to `assets/`.
    pub path: String,
    /// Display name.
    pub name: String,
}

/// The set of curated profiles the app knows about (from the synced catalog /
/// manifest).
#[derive(Clone, Debug, Default)]
pub struct CuratedCatalog {
    entries: Vec<CuratedEntry>,
}

/// What auto-selection resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// A per-image override profile was pinned and honored.
    Override(ProfileId),
    /// A curated DCP is bundled for this body, the default.
    Curated(ProfileId),
    /// No curated profile, fall back to the tier-1 matrix base (Risk 10).
    MatrixBase,
}

impl CuratedCatalog {
    /// Builds a catalog from curated entries.
    pub fn new(entries: Vec<CuratedEntry>) -> CuratedCatalog {
        CuratedCatalog { entries }
    }

    /// The curated entries.
    pub fn entries(&self) -> &[CuratedEntry] {
        &self.entries
    }

    /// The curated default for a camera (normalized `(make, model)` match), if
    /// one is bundled.
    pub fn default_for(&self, camera: &CameraId) -> Option<&CuratedEntry> {
        self.entries
            .iter()
            .find(|e| camera_matches(&e.camera, camera))
    }

    /// Auto-selects the profile for an image: a per-image `override_id` wins;
    /// else the curated default for the body; else the matrix base.
    pub fn select(&self, camera: &CameraId, override_id: Option<ProfileId>) -> Selection {
        if let Some(id) = override_id {
            return Selection::Override(id);
        }
        match self.default_for(camera) {
            Some(e) => Selection::Curated(e.profile_id),
            None => Selection::MatrixBase,
        }
    }

    /// Convenience: select from raw EXIF make/model (normalizes first).
    pub fn select_for_exif(
        &self,
        raw_make: &str,
        raw_model: &str,
        override_id: Option<ProfileId>,
    ) -> Selection {
        let camera = normalize_camera(raw_make, raw_model);
        self.select(&camera, override_id)
    }
}

/// Two normalized identities name the same body (canonical make + model,
/// case-insensitive; raw EXIF strings are ignored).
fn camera_matches(a: &CameraId, b: &CameraId) -> bool {
    a.make.eq_ignore_ascii_case(&b.make) && a.model.eq_ignore_ascii_case(&b.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(make: &str, model: &str, id_byte: u8) -> CuratedEntry {
        let camera = normalize_camera(make, model);
        CuratedEntry {
            path: format!("color/profiles/{}/{}.dcp", camera.make, camera.model),
            name: format!("{} {} — Curated", camera.make, camera.model),
            camera,
            profile_id: ProfileId([id_byte; 16]),
        }
    }

    #[test]
    fn curated_default_matches_across_alias_spelling() {
        // A curated profile registered under canonical (Nikon, Z6) is found when
        // the image reports the drifted EXIF spelling, the §3.6 alias table.
        let catalog = CuratedCatalog::new(vec![entry("Nikon", "Z6", 0x11)]);
        let queried = normalize_camera("NIKON CORPORATION", "NIKON Z 6");
        assert_eq!(
            catalog.select(&queried, None),
            Selection::Curated(ProfileId([0x11; 16]))
        );
    }

    #[test]
    fn no_curated_profile_falls_back_to_matrix_base() {
        // ZERO bundled profiles today (spec §0): every body → matrix base.
        let catalog = CuratedCatalog::default();
        let cam = normalize_camera("Canon", "Canon EOS R6");
        assert_eq!(catalog.select(&cam, None), Selection::MatrixBase);
    }

    #[test]
    fn per_image_override_wins_over_curated_and_base() {
        let catalog = CuratedCatalog::new(vec![entry("Nikon", "Z6", 0x11)]);
        let cam = normalize_camera("NIKON CORPORATION", "NIKON Z 6");
        let override_id = ProfileId([0x99; 16]);
        // Override honored even though a curated default exists.
        assert_eq!(
            catalog.select(&cam, Some(override_id)),
            Selection::Override(override_id)
        );
        // And even when there is no curated default.
        let other = normalize_camera("Fujifilm", "X-T1");
        assert_eq!(
            catalog.select(&other, Some(override_id)),
            Selection::Override(override_id)
        );
    }

    #[test]
    fn select_for_exif_normalizes_before_lookup() {
        let catalog = CuratedCatalog::new(vec![entry("Nikon", "Z6", 0x22)]);
        assert_eq!(
            catalog.select_for_exif("NIKON CORPORATION", "NIKON Z 6", None),
            Selection::Curated(ProfileId([0x22; 16]))
        );
    }

    #[test]
    fn different_body_does_not_match() {
        let catalog = CuratedCatalog::new(vec![entry("Nikon", "Z6", 0x33)]);
        let cam = normalize_camera("NIKON CORPORATION", "NIKON Z 7");
        assert_eq!(catalog.default_for(&cam), None);
        assert_eq!(catalog.select(&cam, None), Selection::MatrixBase);
    }
}
