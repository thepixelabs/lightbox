// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Starter preset library asset gates (preset-management feature, mirrors
//! `lightbox-color`'s `look_assets.rs` bless/manifest pattern):
//!
//! * The committed `assets/presets/**/*.xmp` files are the canonical
//!   `PresetStore::create_from` serialization of [`starter_presets`]
//!   reproducible under `LIGHTBOX_BLESS=1`, asserted equal otherwise.
//! * The surface-3 `assets/MANIFEST.toml` **policy checker**, scoped to
//!   `presets/`: every file under `assets/presets/` has a cleared manifest
//!   entry, provenance is in the allowed set, and nothing is Adobe-authored
//!   (the same "no Adobe-derived data" constraint `lightbox-color`'s own
//!   checker enforces for `color/`).
//!
//! Bless flow: `LIGHTBOX_BLESS=1 cargo test -p lightbox-edit --test
//! preset_library` regenerates the committed assets locally (refuses in CI)
//! and commit the result.

use std::path::{Path, PathBuf};

use lightbox_edit::preset_library::starter_presets;
use lightbox_edit::{PresetStore, Recipe};
use lightbox_types::PV_M0;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolves")
}

fn presets_dir() -> PathBuf {
    repo_root().join("assets").join("presets")
}

fn bless_requested() -> bool {
    std::env::var("LIGHTBOX_BLESS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn running_in_ci() -> bool {
    match std::env::var("CI") {
        Ok(v) => !v.is_empty() && !v.eq_ignore_ascii_case("false"),
        Err(_) => false,
    }
}

/// E6-style asset gate: the committed `assets/presets/**/*.xmp` files are the
/// canonical serialization of [`starter_presets`]. Bless regenerates them
/// from a clean slate (this directory is entirely generator-owned, no
/// hand-edited content ever lives here); otherwise every authored preset
/// must be present with byte-identical delta/subset/name/group content.
#[test]
fn committed_preset_assets_match_the_starter_library() {
    let dir = presets_dir();
    let library = starter_presets();

    if bless_requested() {
        assert!(!running_in_ci(), "LIGHTBOX_BLESS must never be set in CI");
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).expect("clear the generated preset dir");
        }
        let store = PresetStore::open(dir.clone()).expect("open preset store for bless");
        for p in &library {
            let mut recipe = Recipe::identity(PV_M0);
            recipe
                .apply(&p.delta)
                .expect("preset delta applies cleanly onto identity");
            store
                .create_from(&recipe, p.name, Some(p.group), &p.subset())
                .unwrap_or_else(|e| panic!("create_from failed for {}: {e}", p.name));
        }
        return;
    }

    let store = PresetStore::open(dir.clone()).unwrap_or_else(|e| {
        panic!(
            "missing/unreadable {} ({e}): run `LIGHTBOX_BLESS=1 cargo test -p lightbox-edit \
             --test preset_library` and commit the result",
            dir.display()
        )
    });
    assert!(
        store.quarantined().is_empty(),
        "committed preset assets must all load cleanly: {:?}",
        store.quarantined()
    );

    let loaded = store.list();
    assert_eq!(
        loaded.len(),
        library.len(),
        "the committed asset count drifted from the starter library — re-bless"
    );

    for p in &library {
        let mut recipe = Recipe::identity(PV_M0);
        recipe
            .apply(&p.delta)
            .expect("preset delta applies cleanly onto identity");
        let expected_subset = p.subset();
        let expected_delta = recipe.extract(&expected_subset);

        let meta = loaded
            .iter()
            .find(|m| m.name == p.name && m.group.as_deref() == Some(p.group))
            .unwrap_or_else(|| panic!("missing committed asset for preset {:?}", p.name));
        let full = store
            .get(&meta.id)
            .unwrap_or_else(|| panic!("preset {:?} indexed but not loadable", p.name));

        assert_eq!(full.name, p.name);
        assert_eq!(full.group.as_deref(), Some(p.group));
        assert_eq!(full.subset, expected_subset, "{}: subset drifted", p.name);
        assert_eq!(full.delta, expected_delta, "{}: delta drifted", p.name);
    }
}

// --- Surface-3 manifest policy checker, scoped to `presets/` ---

#[derive(serde::Deserialize)]
struct Manifest {
    #[serde(default)]
    asset: Vec<AssetEntry>,
}

#[derive(serde::Deserialize)]
struct AssetEntry {
    path: String,
    kind: String,
    provenance: String,
    license: String,
    #[serde(default)]
    copyright: Option<String>,
}

fn list_files_rel(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            list_files_rel(base, &p, out);
        } else if p.is_file() {
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// Surface-3 `presets/` policy checker (mirrors `lightbox-color`'s
/// `manifest_policy_checker_clears_every_color_asset`, spec §4.3): every file
/// under `assets/presets/` has a manifest entry; every entry's provenance is
/// cleared (`lightbox-authored` for this originally-authored library) and
/// NOTHING is Adobe-authored (provenance, license, or `copyright`).
#[test]
fn manifest_policy_checker_clears_every_preset_asset() {
    let assets = repo_root().join("assets");
    let manifest_path = assets.join("MANIFEST.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("missing {} ({e})", manifest_path.display()));
    let manifest: Manifest = toml::from_str(&text).expect("MANIFEST.toml parses");

    let preset_entries: Vec<&AssetEntry> = manifest
        .asset
        .iter()
        .filter(|a| a.path.starts_with("presets/"))
        .collect();

    let is_adobe = |s: &str| s.to_lowercase().contains("adobe");
    let provenance_cleared = |p: &str| {
        p == "lightbox-authored" || p.starts_with("profgen:") || p.starts_with("community:")
    };

    for a in &preset_entries {
        assert_eq!(a.kind, "preset", "{}: unexpected kind {:?}", a.path, a.kind);
        assert!(
            provenance_cleared(&a.provenance),
            "{}: provenance {:?} is not in the cleared set",
            a.path,
            a.provenance
        );
        assert!(!is_adobe(&a.provenance), "{}: Adobe provenance", a.path);
        assert!(!is_adobe(&a.license), "{}: Adobe license", a.path);
        assert_eq!(a.license, "project", "{}: expected project license", a.path);
        if let Some(c) = &a.copyright {
            assert!(!is_adobe(c), "{}: Adobe copyright", a.path);
        }
    }

    // Every file physically present under assets/presets/ must be declared.
    let presets_dir = assets.join("presets");
    let mut files = Vec::new();
    list_files_rel(&assets, &presets_dir, &mut files);
    assert!(
        !files.is_empty(),
        "assets/presets/ has no committed files — run the bless flow first"
    );
    for f in &files {
        assert!(
            preset_entries.iter().any(|a| &a.path == f),
            "{f} under assets/presets/ has no MANIFEST.toml entry (surface-3 policy)"
        );
    }

    // Every declared preset entry must correspond to an actual file (no
    // stale manifest rows left behind by a re-bless that removed a preset).
    for a in &preset_entries {
        assert!(
            files.contains(&a.path),
            "MANIFEST.toml declares {} but no such file exists under assets/presets/",
            a.path
        );
    }
}
