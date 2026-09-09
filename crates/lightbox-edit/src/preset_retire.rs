// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Retiring a starter preset that has left the bundled library, **without
//! ever deleting a preset the user owns**.
//!
//! # The defect this exists to fix
//!
//! Seeding only ever *added*. [`PresetStore::import_files`] is idempotent per
//! source path, so re-running it every launch never duplicates anything, but
//! nothing ever removed a preset the library had stopped shipping. The preset
//! store is machine-global (`PresetStore::open_default` →
//! `<config>/Lightbox/presets`), not per-catalog, so it outlives every run and
//! every working set. When the starter library was re-authored (the 22 presets
//! in `Tonal`/`Color`/`Portrait`/`Landscape`/`Black and White` replaced by 36
//! in `Neon`/`Retro`/`Verdant`/`Atmosphere`/`Edge`/`Mono`/`Tonal`), every
//! existing user was left holding 58 presets across 11 groups, the new
//! library plus four dead families.
//!
//! # Why [`PresetOrigin`](crate::PresetOrigin) cannot be the answer
//!
//! `PresetOrigin::Lightbox` means "authored in Lightbox", which covers a
//! starter preset the app installed **and** a preset the user built with
//! `create_from`. Retiring by origin would delete the user's own work. Origin
//! is provenance; it was never an ownership marker.
//!
//! # Two sweeps, each with its own proof
//!
//! **1. The marker sweep, the durable fix.** Every store copy written by
//! [`PresetStore::seed_bundled`] carries `lb:PresetBundled` (see
//! [`DevelopPreset::bundled`]). The flag is set by *the operation*, never read
//! off the file being imported, so nothing but our own seed can produce a
//! marked preset, a hand-crafted or copied `.xmp` claiming the marker still
//! imports unmarked through [`PresetStore::import_files`]. Reconciliation is
//! then a set difference: a marked preset whose `lb:PresetId` is not among the
//! ids the current library just installed has left the library, and goes.
//! Anything unmarked is the user's and is never even considered.
//!
//! **2. The legacy sweep, the one-time archaeology.** Presets already sitting
//! in a user's store predate the marker, so the set difference cannot see
//! them. For those, the 22 `.xmp` files the library shipped before the
//! re-authoring are compiled in verbatim (`assets/retired-starter-presets/`,
//! extracted from git at `d0cd14f^`) and a store preset is retired only when
//! it is [`content_eq`](DevelopPreset::content_eq) to one of them, equal in
//! `id` **and** `name` **and** `group` **and** `subset` **and** `delta` **and**
//! `min_pv` **and** `origin`. Every field a user can change is in that
//! comparison, so:
//!
//! | the user did this | what changes | outcome |
//! |---|---|---|
//! | nothing |, | **retired** |
//! | renamed it | `name` | kept |
//! | moved it to another group | `group` | kept |
//! | edited its recipe | `delta` (and `id`, via re-authoring) | kept |
//! | authored their own preset in a group that happens to share a name | `id`, `name`, `delta` | kept |
//!
//! The `id` is what makes coincidence impossible rather than merely
//! improbable: a user-authored preset gets a fresh [`PresetId::new_v4`], and
//! the 22 retired ids are fixed uuids that only ever entered a store by way of
//! the bundled files that carried them. (Those ids are stable: `assets/presets`
//! was blessed exactly once before being replaced, so every affected store was
//! seeded from the same bytes.)
//!
//! Both reference and candidate are decoded by the *same*
//! [`decode_preset`](crate::preset::decode_preset), so a future change to how
//! a recipe deserializes moves both sides together and can never silently turn
//! "kept" into "retired".
//!
//! # Failure directions
//!
//! Every uncertainty resolves toward keeping data. A malformed store file is
//! quarantined by the scan and never becomes a retirement candidate; an
//! unreadable or absent marker reads as "the user's"; a bundled library that
//! fails to import completely skips the marker sweep entirely rather than
//! mistaking "I could not see it" for "it was removed"; and a bundled library
//! that resolves to no files at all skips it too, otherwise a packaging bug
//! would wipe every starter preset on the machine.
//!
//! Nothing here is silent: every removal is logged at `info` naming the
//! preset, its group, its id, and its path.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use lightbox_meta::xmp::{ParseLimits, XmpDoc};

use crate::preset::{
    decode_preset, DevelopPreset, PresetError, PresetId, PresetImportResult, PresetMeta,
    PresetStore,
};

/// The bundled starter library **as it stood before it was re-authored**
/// (commit `d0cd14f`), as `(group, file stem, verbatim .xmp)`.
///
/// These are the exact bytes that were committed under `assets/presets/`, so
/// they decode to exactly the presets a seeded store received. They live in
/// `crates/lightbox-edit/assets/` rather than the repo-root `assets/presets/`
/// on purpose: that directory is generator-owned (regenerated wholesale by
/// `LIGHTBOX_BLESS=1 cargo test -p lightbox-edit --test preset_library`) and
/// gated by the surface-3 `assets/MANIFEST.toml` policy checker. These files
/// are *history*, not generated output, they must never be re-blessed, and a
/// re-bless must never delete them.
///
/// This table is append-only in spirit: when a future library revision drops a
/// preset, the marker sweep handles it and **nothing needs to be added here**.
/// It exists solely to cover the stores seeded before `lb:PresetBundled`
/// existed.
const RETIRED_STARTER_XMP: &[(&str, &str, &str)] = &[
    (
        "Black and White",
        "Classic Mono",
        include_str!("../assets/retired-starter-presets/Black and White/Classic Mono.xmp"),
    ),
    (
        "Black and White",
        "High Contrast Mono",
        include_str!("../assets/retired-starter-presets/Black and White/High Contrast Mono.xmp"),
    ),
    (
        "Black and White",
        "Soft Mono",
        include_str!("../assets/retired-starter-presets/Black and White/Soft Mono.xmp"),
    ),
    (
        "Black and White",
        "Vintage Mono",
        include_str!("../assets/retired-starter-presets/Black and White/Vintage Mono.xmp"),
    ),
    (
        "Color",
        "Cool Blue Hour",
        include_str!("../assets/retired-starter-presets/Color/Cool Blue Hour.xmp"),
    ),
    (
        "Color",
        "Muted Film",
        include_str!("../assets/retired-starter-presets/Color/Muted Film.xmp"),
    ),
    (
        "Color",
        "Split Warm-Cool",
        include_str!("../assets/retired-starter-presets/Color/Split Warm-Cool.xmp"),
    ),
    (
        "Color",
        "Teal and Orange",
        include_str!("../assets/retired-starter-presets/Color/Teal and Orange.xmp"),
    ),
    (
        "Color",
        "Vibrant Pop",
        include_str!("../assets/retired-starter-presets/Color/Vibrant Pop.xmp"),
    ),
    (
        "Color",
        "Warm Glow",
        include_str!("../assets/retired-starter-presets/Color/Warm Glow.xmp"),
    ),
    (
        "Landscape",
        "Landscape Golden Hour",
        include_str!("../assets/retired-starter-presets/Landscape/Landscape Golden Hour.xmp"),
    ),
    (
        "Landscape",
        "Landscape Moody",
        include_str!("../assets/retired-starter-presets/Landscape/Landscape Moody.xmp"),
    ),
    (
        "Landscape",
        "Landscape Vivid",
        include_str!("../assets/retired-starter-presets/Landscape/Landscape Vivid.xmp"),
    ),
    (
        "Portrait",
        "Portrait Gentle Warm",
        include_str!("../assets/retired-starter-presets/Portrait/Portrait Gentle Warm.xmp"),
    ),
    (
        "Portrait",
        "Portrait Natural",
        include_str!("../assets/retired-starter-presets/Portrait/Portrait Natural.xmp"),
    ),
    (
        "Portrait",
        "Portrait Soft Light",
        include_str!("../assets/retired-starter-presets/Portrait/Portrait Soft Light.xmp"),
    ),
    (
        "Tonal",
        "Airy Highlights",
        include_str!("../assets/retired-starter-presets/Tonal/Airy Highlights.xmp"),
    ),
    (
        "Tonal",
        "Clean Bright",
        include_str!("../assets/retired-starter-presets/Tonal/Clean Bright.xmp"),
    ),
    (
        "Tonal",
        "Deep Punch",
        include_str!("../assets/retired-starter-presets/Tonal/Deep Punch.xmp"),
    ),
    (
        "Tonal",
        "Matte Fade",
        include_str!("../assets/retired-starter-presets/Tonal/Matte Fade.xmp"),
    ),
    (
        "Tonal",
        "Punchy Standard",
        include_str!("../assets/retired-starter-presets/Tonal/Punchy Standard.xmp"),
    ),
    (
        "Tonal",
        "Soft Low-Contrast",
        include_str!("../assets/retired-starter-presets/Tonal/Soft Low-Contrast.xmp"),
    ),
];

/// [`RETIRED_STARTER_XMP`] decoded once, on first use.
///
/// A reference that fails to decode is dropped rather than panicking, this
/// runs on the launch path, and losing one comparison costs at most one stale
/// preset surviving, whereas a panic costs the app. `retired_reference_set_is_
/// complete` is the test that keeps that from happening quietly.
fn retired_reference_set() -> &'static [DevelopPreset] {
    static SET: OnceLock<Vec<DevelopPreset>> = OnceLock::new();
    SET.get_or_init(|| {
        RETIRED_STARTER_XMP
            .iter()
            .filter_map(|(group, stem, xmp)| {
                let doc = XmpDoc::parse(xmp.as_bytes(), ParseLimits::default()).ok()?;
                decode_preset(&doc, stem, Some((*group).to_string()), PathBuf::new()).ok()
            })
            .collect()
    })
}

/// True when `candidate` is an **untouched** copy of a preset the starter
/// library shipped before the re-authoring.
///
/// The whole safety argument of the legacy sweep is this one function: it is
/// pure, it compares every user-visible field via
/// [`DevelopPreset::content_eq`], and it has no way to say "close enough".
fn is_unmodified_legacy_starter(candidate: &DevelopPreset) -> bool {
    retired_reference_set()
        .iter()
        .any(|reference| reference.content_eq(candidate))
}

/// Why a preset was retired.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetireReason {
    /// It carried the durable `lb:PresetBundled` marker and its id is no
    /// longer among the ids the bundled library ships. This is the path all
    /// future library changes take.
    DroppedFromLibrary,
    /// It predates the marker and is content-identical to one of the presets
    /// the pre-re-authoring starter library shipped.
    UnmodifiedLegacyStarter,
}

/// One retired preset, enough to explain the removal after the fact.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RetiredPreset {
    /// Identity and display metadata as it was on disk.
    pub meta: PresetMeta,
    /// The file that was removed.
    pub path: PathBuf,
    /// Which sweep claimed it.
    pub reason: RetireReason,
}

/// Why the marker sweep did not run on a given seed.
///
/// Both variants mean "the bundled library could not be established with
/// confidence", and both resolve the same way: retire nothing by marker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MarkerSweepSkipped {
    /// The caller passed no bundled files. Treating an empty library as "every
    /// starter preset was removed" would let a packaging bug wipe the store.
    NoBundledFiles,
    /// At least one bundled file failed to import, so the set of ids the
    /// library ships is incomplete and a set difference against it would
    /// retire presets that are still shipping.
    IncompleteImport {
        /// How many of `total` failed.
        failed: usize,
        /// How many files the caller offered.
        total: usize,
    },
}

/// What one [`PresetStore::seed_bundled`] run did.
#[derive(Debug, Default)]
pub struct BundledSeedReport {
    /// Per-file import results, in the order the caller supplied them.
    pub imported: Vec<PresetImportResult>,
    /// Presets removed from the store, with the reason for each.
    pub retired: Vec<RetiredPreset>,
    /// Files that matched but could not be removed (`(path, reason)`), e.g. a
    /// permissions failure. Never fatal, the seed still succeeds.
    pub unremovable: Vec<(PathBuf, String)>,
    /// Set when the marker sweep was deliberately skipped. The legacy sweep
    /// runs regardless: it does not depend on the bundled library at all.
    pub marker_sweep_skipped: Option<MarkerSweepSkipped>,
}

impl BundledSeedReport {
    /// How many supplied files failed to import.
    pub fn import_failures(&self) -> usize {
        self.imported.iter().filter(|r| r.error.is_some()).count()
    }
}

impl PresetStore {
    /// Seed the store from Lightbox's **own** bundled starter library, then
    /// retire the starter presets that library no longer ships.
    ///
    /// This is the app-owned seeding path and the only thing that may mark a
    /// preset as bundled, it must never be wired to a user-chosen file list
    /// (that is [`import_files`](PresetStore::import_files), which marks
    /// nothing and retires nothing).
    ///
    /// Import runs first so that the current library's ids are known from its
    /// own results rather than from a second, possibly-disagreeing parse; a
    /// preset the current library just installed is then excluded from
    /// retirement outright, before either sweep looks at it.
    ///
    /// Errors only if the index cannot be rebuilt. Per-file import failures
    /// and per-file removal failures are reported, never fatal.
    pub fn seed_bundled(&self, bundled: &[PathBuf]) -> Result<BundledSeedReport, PresetError> {
        let imported = self.import_batch(bundled, true);

        // The ids the library ships, taken from the imports that actually
        // succeeded.
        let mut shipped: BTreeSet<PresetId> = BTreeSet::new();
        for result in &imported {
            if let Some(meta) = &result.preset {
                shipped.insert(meta.id.clone());
            }
        }

        let failed = imported.iter().filter(|r| r.error.is_some()).count();
        let marker_sweep_skipped = if bundled.is_empty() {
            Some(MarkerSweepSkipped::NoBundledFiles)
        } else if failed > 0 {
            Some(MarkerSweepSkipped::IncompleteImport {
                failed,
                total: bundled.len(),
            })
        } else {
            None
        };
        let marker_sweep = marker_sweep_skipped.is_none();

        let mut report = BundledSeedReport {
            imported,
            retired: Vec::new(),
            unremovable: Vec::new(),
            marker_sweep_skipped,
        };

        // Re-scan so the candidate set reflects what the import just wrote
        // (`import_batch` refreshes only when something succeeded).
        self.refresh()?;

        let mut doomed: Vec<(Arc<DevelopPreset>, RetireReason)> = Vec::new();
        for preset in self.snapshot() {
            // A preset the CURRENT library just installed is off limits to
            // both sweeps, unconditionally.
            if shipped.contains(&preset.id) {
                continue;
            }
            if marker_sweep && preset.bundled {
                doomed.push((preset, RetireReason::DroppedFromLibrary));
            } else if is_unmodified_legacy_starter(&preset) {
                doomed.push((preset, RetireReason::UnmodifiedLegacyStarter));
            }
        }

        for (preset, reason) in doomed {
            match fs::remove_file(&preset.path) {
                Ok(()) => {
                    tracing::info!(
                        target: "lightbox_edit",
                        preset = %preset.name,
                        group = %preset.group.as_deref().unwrap_or("(ungrouped)"),
                        id = %preset.id.0,
                        path = %preset.path.display(),
                        reason = ?reason,
                        "retired a starter preset the bundled library no longer ships"
                    );
                    report.retired.push(RetiredPreset {
                        meta: preset.meta(),
                        path: preset.path.clone(),
                        reason,
                    });
                }
                // Already gone (a concurrent delete, or a stale index entry):
                // the desired end state, so not a failure.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(
                        target: "lightbox_edit",
                        preset = %preset.name,
                        path = %preset.path.display(),
                        error = %e,
                        "could not retire a stale starter preset; leaving it in place"
                    );
                    report
                        .unremovable
                        .push((preset.path.clone(), e.to_string()));
                }
            }
        }

        if !report.retired.is_empty() {
            self.refresh()?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every compiled-in historical preset decodes. If this fails, the legacy
    /// sweep has silently gone partially blind (see `retired_reference_set`).
    #[test]
    fn retired_reference_set_is_complete() {
        assert_eq!(
            RETIRED_STARTER_XMP.len(),
            22,
            "the pre-re-authoring library shipped 22 presets"
        );
        let set = retired_reference_set();
        assert_eq!(
            set.len(),
            RETIRED_STARTER_XMP.len(),
            "a compiled-in historical preset failed to decode"
        );
        for p in set {
            assert!(!p.name.is_empty());
            assert!(p.group.is_some());
            assert!(!p.delta.0.is_empty(), "{}: empty delta", p.name);
            // Every historical starter preset was Lightbox-authored and, by
            // definition, predates the bundled marker.
            assert_eq!(p.origin, crate::PresetOrigin::Lightbox);
            assert!(!p.bundled, "{}: reference must be unmarked", p.name);
        }
    }

    /// The 22 ids are distinct and are the exact uuids committed under
    /// `assets/presets/` before the re-authoring. Pinning them here means a
    /// corrupted or swapped-in reference file fails the suite rather than
    /// changing what retirement deletes.
    #[test]
    fn retired_reference_ids_are_the_committed_ones() {
        let set = retired_reference_set();
        let ids: BTreeSet<&str> = set.iter().map(|p| p.id.0.as_str()).collect();
        assert_eq!(ids.len(), 22, "duplicate id among the historical presets");
        for expected in [
            "45063599-cfd4-4f7a-b316-1753fc24ed66", // Black and White/Classic Mono
            "40f08aa0-4441-412d-899d-0d952903e510", // Black and White/High Contrast Mono
            "46a23531-40eb-442f-ba2a-d6dfbeb3bf6c", // Black and White/Soft Mono
            "4ea07287-aa53-4e6e-bcc2-f55937a25b83", // Black and White/Vintage Mono
            "3999cb2c-bbcb-4587-9fc8-c610bbdb5003", // Color/Cool Blue Hour
            "8ee5a5d3-dd99-4537-8c59-ca40d50b65e5", // Color/Muted Film
            "c6aeeb34-5b3a-4b99-976d-dff25b7e3dfd", // Color/Split Warm-Cool
            "a754c24e-01e3-49c3-b7f3-c87ad150d18b", // Color/Teal and Orange
            "27ea3044-1d68-40f3-8c71-a3e1404225ce", // Color/Vibrant Pop
            "de32745b-ec7d-4d34-b5b2-1c5364830306", // Color/Warm Glow
            "1ec5fe22-d354-49a8-879a-ab785296eb30", // Landscape/Landscape Golden Hour
            "8ce2fa0c-5a6c-4d87-af34-8878f23b3f7f", // Landscape/Landscape Moody
            "f2b2800f-2483-48a5-b99f-dcb0477a5e59", // Landscape/Landscape Vivid
            "8787f876-8b8f-4146-aad9-553fe2716f05", // Portrait/Portrait Gentle Warm
            "d100145d-63c2-46c6-8d0d-2bdb676d91d1", // Portrait/Portrait Natural
            "4efa15cc-5ec1-44b2-a191-94961efc1674", // Portrait/Portrait Soft Light
            "207d9cf7-e167-4740-b868-9ce8235a1a54", // Tonal/Airy Highlights
            "527c687f-ca0c-41ba-b05f-fbfa3207c598", // Tonal/Clean Bright
            "4ba8fd8f-c258-4abf-88b3-475b4851dc29", // Tonal/Deep Punch
            "0f762ed4-1085-4200-946f-f9c61d1caced", // Tonal/Matte Fade
            "585384a9-7b71-4e27-8a99-ff1a8a5e612d", // Tonal/Punchy Standard
            "0d5f3d98-05d1-4e3c-83fc-f87d5c8e0af7", // Tonal/Soft Low-Contrast
        ] {
            assert!(ids.contains(expected), "missing historical id {expected}");
        }
    }

    /// The retired families are exactly the ones the re-authoring dropped,
    /// and `Tonal`, the one group name that survived, contributes no name
    /// that the current library also uses (which would make a *current*
    /// preset look historical if ids ever collided).
    #[test]
    fn retired_reference_groups_are_the_dropped_families() {
        let set = retired_reference_set();
        let groups: BTreeSet<&str> = set.iter().filter_map(|p| p.group.as_deref()).collect();
        assert_eq!(
            groups,
            BTreeSet::from(["Black and White", "Color", "Landscape", "Portrait", "Tonal"])
        );
    }
}
