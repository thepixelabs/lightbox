// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Camera identity normalization (A4, spec §3.6).
//!
//! [`normalize_camera`] turns the raw EXIF `Make`/`Model` strings into a
//! canonical [`CameraId`], the join key shared by probe metadata, curated
//! `.dcp` lookup (E02.5 auto-selection), lensfun matching (E11), and the
//! catalog `camera_model` column. The raw strings are preserved alongside the
//! canonical ones so nothing is lost.
//!
//! The alias table is DATA (`camera_aliases.toml`, `include_str!`-compiled),
//! parsed once and cached. Normalization is deterministic and side-effect-free:
//!
//! 1. **Make**, the first `[[make]]` entry whose (uppercased) `match` is a
//!    prefix of the raw make wins; else the trimmed raw make is kept as-is.
//! 2. **Model**, a leading make token (raw or canonical) is stripped, internal
//!    whitespace is collapsed, then a `[[model]]` alias for `(make, stripped)`
//!    is applied; else the stripped/collapsed model is kept.

use std::sync::OnceLock;

/// Normalized camera identity (spec §3.6). `make`/`model` are the canonical
/// join key; `raw_make`/`raw_model` preserve the original EXIF strings.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CameraId {
    /// Canonical make (e.g. `"Nikon"`).
    pub make: String,
    /// Canonical model (e.g. `"Z6"`).
    pub model: String,
    /// Original EXIF make string, trimmed.
    pub raw_make: String,
    /// Original EXIF model string, trimmed.
    pub raw_model: String,
}

#[derive(serde::Deserialize)]
struct AliasTable {
    #[serde(default)]
    make: Vec<MakeAlias>,
    #[serde(default)]
    model: Vec<ModelAlias>,
}

#[derive(serde::Deserialize)]
struct MakeAlias {
    #[serde(rename = "match")]
    pattern: String,
    canonical: String,
}

#[derive(serde::Deserialize)]
struct ModelAlias {
    make: String,
    #[serde(rename = "match")]
    pattern: String,
    canonical: String,
}

const ALIAS_TOML: &str = include_str!("camera_aliases.toml");

fn aliases() -> &'static AliasTable {
    static TABLE: OnceLock<AliasTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        // The table is a compiled-in constant we author; a parse failure is a
        // build-time authoring bug, surfaced loudly rather than silently
        // degrading normalization.
        toml::from_str(ALIAS_TOML).expect("camera_aliases.toml is valid TOML")
    })
}

/// Collapses runs of ASCII whitespace to single spaces and trims the ends.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Canonicalizes the make; returns the canonical string.
fn canonical_make(raw_make: &str) -> String {
    let upper = raw_make.to_ascii_uppercase();
    for a in &aliases().make {
        if upper.starts_with(&a.pattern.to_ascii_uppercase()) {
            return a.canonical.clone();
        }
    }
    collapse_ws(raw_make)
}

/// Strips a leading make token (`raw_make` or `canonical`) from `model`,
/// case-insensitively, then collapses whitespace.
fn strip_make_prefix(model: &str, raw_make: &str, canonical: &str) -> String {
    let collapsed = collapse_ws(model);
    for make in [canonical, raw_make] {
        let make = make.trim();
        if make.is_empty() {
            continue;
        }
        if collapsed.len() >= make.len()
            && collapsed[..make.len()].eq_ignore_ascii_case(make)
            // Only strip when followed by a separator, so "Nikon" never eats
            // the front of a model that legitimately starts with those letters.
            && collapsed[make.len()..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace())
        {
            return collapse_ws(&collapsed[make.len()..]);
        }
    }
    collapsed
}

/// Normalizes raw EXIF make/model into a canonical [`CameraId`] (spec §3.6).
/// Deterministic, allocation-only, never fails.
pub fn normalize_camera(raw_make: &str, raw_model: &str) -> CameraId {
    let raw_make_trim = raw_make.trim().to_owned();
    let raw_model_trim = raw_model.trim().to_owned();

    let make = canonical_make(&raw_make_trim);
    let stripped = strip_make_prefix(&raw_model_trim, &raw_make_trim, &make);

    let model = aliases()
        .model
        .iter()
        .find(|m| m.make.eq_ignore_ascii_case(&make) && m.pattern.eq_ignore_ascii_case(&stripped))
        .map(|m| m.canonical.clone())
        .unwrap_or(stripped);

    CameraId {
        make,
        model,
        raw_make: raw_make_trim,
        raw_model: raw_model_trim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(make: &str, model: &str) -> (String, String) {
        let id = normalize_camera(make, model);
        (id.make, id.model)
    }

    #[test]
    fn nikon_z6_spelling_drift() {
        // The canonical spec example (§3.6).
        assert_eq!(
            norm("NIKON CORPORATION", "NIKON Z 6"),
            ("Nikon".into(), "Z6".into())
        );
        assert_eq!(
            norm("NIKON CORPORATION", "NIKON Z 7"),
            ("Nikon".into(), "Z7".into())
        );
        assert_eq!(
            norm("NIKON CORPORATION", "NIKON Z 6_2"),
            ("Nikon".into(), "Z6II".into())
        );
    }

    #[test]
    fn corpus_bodies_normalize_to_expected_keys() {
        assert_eq!(
            norm("Canon", "Canon EOS R6"),
            ("Canon".into(), "EOS R6".into())
        );
        assert_eq!(
            norm("Canon", "Canon EOS 350D DIGITAL"),
            ("Canon".into(), "EOS 350D DIGITAL".into())
        );
        assert_eq!(
            norm("NIKON CORPORATION", "NIKON D4S"),
            ("Nikon".into(), "D4S".into())
        );
        assert_eq!(norm("FUJIFILM", "X100"), ("Fujifilm".into(), "X100".into()));
        assert_eq!(norm("FUJIFILM", "X-T1"), ("Fujifilm".into(), "X-T1".into()));
        assert_eq!(
            norm("OLYMPUS CORPORATION", "E-1"),
            ("Olympus".into(), "E-1".into())
        );
        // Sony ILCE codes are kept verbatim, they are the curated-profile key.
        assert_eq!(norm("SONY", "ILCE-7S"), ("Sony".into(), "ILCE-7S".into()));
        assert_eq!(norm("SIGMA", "SIGMA fp"), ("Sigma".into(), "fp".into()));
    }

    #[test]
    fn raw_strings_are_preserved_and_trimmed() {
        let id = normalize_camera("  NIKON CORPORATION  ", "  NIKON Z 6  ");
        assert_eq!(id.raw_make, "NIKON CORPORATION");
        assert_eq!(id.raw_model, "NIKON Z 6");
        assert_eq!(id.make, "Nikon");
        assert_eq!(id.model, "Z6");
    }

    #[test]
    fn unknown_make_is_kept_as_is() {
        // Unknown make: canonical == raw make, and the model only has its make
        // token stripped when it leads with the *full* make string.
        assert_eq!(
            norm("Acme", "Acme SuperShot 9000"),
            ("Acme".into(), "SuperShot 9000".into())
        );
        // A multi-word make that the model does not fully lead with is kept.
        assert_eq!(
            norm("Acme Cameras", "Acme SuperShot 9000"),
            ("Acme Cameras".into(), "Acme SuperShot 9000".into())
        );
    }

    #[test]
    fn make_prefix_only_strips_on_a_separator() {
        // "Canonball" must not have "Canon" chopped off its front.
        assert_eq!(
            norm("Canon", "Canonball 3000"),
            ("Canon".into(), "Canonball 3000".into())
        );
    }

    #[test]
    fn empty_inputs_do_not_panic() {
        let id = normalize_camera("", "");
        assert_eq!(id.make, "");
        assert_eq!(id.model, "");
    }

    #[test]
    fn alias_table_parses() {
        // Force the OnceLock init; a malformed table would panic here.
        assert!(!aliases().make.is_empty());
        assert!(!aliases().model.is_empty());
    }
}
