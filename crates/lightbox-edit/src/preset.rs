// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Develop presets (spec §3.7 / Phase E, T23, T24, T26) and the pure hover
//! [`preview_recipe`] (T25).
//!
//! # Decision D3 (kept): presets are app-level files, not catalog rows
//!
//! A preset is one `.xmp` per preset under the platform config dir
//! (`<config>/Lightbox/presets/<group>/<name>.xmp`). The file format **is** our
//! [`Recipe::to_xmp`] mapping (dogfood): a Lightbox preset carries the
//! authoritative `lb:` payload (so export→import is exact) **plus** best-effort
//! `crs:` compatibility fields (so Lightroom can read our presets and so LR
//! presets import by file). This keeps presets outliving any one working set and
//! keeps the edit store (Phase B) as plumbing, not a home for user assets.
//!
//! There is **no directory watcher** (spec §0.6 / OQ5): the index refreshes on
//! [`PresetStore::open`], after the store's own mutations, and on an explicit
//! [`PresetStore::refresh`].
//!
//! # A preset is a `(subset, delta)`
//!
//! [`DevelopPreset::delta`] carries **only the checked groups'** params
//! (`recipe.extract(subset)`), so applying it changes exactly those groups and
//! nothing else, the partial-preset contract (research 02). The extension bags
//! (`xmp_passthrough`/`lb_extra`) are never param-addressable, so a preset carries
//! **no foreign passthrough** (T26).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lightbox_decode::{AssetProbe, ProbedFormat};
use lightbox_types::{Orientation, ProcessVersion};

use crate::params::{group_of, ParamGroup, ParamSubset};
use crate::recipe::Recipe;
use crate::xmp_map::{XmpSource, XmpWriteCtx};

use lightbox_meta::xmp::{ns, sidecar, XmpDoc, XmpValue};

/// `lb:`-namespace preset property names (in [`ns::LB`]).
mod plb {
    /// Stable preset identity (uuid string).
    pub const ID: &str = "PresetId";
    /// Display name.
    pub const NAME: &str = "PresetName";
    /// Group (folder), absent means the ungrouped root.
    pub const GROUP: &str = "PresetGroup";
    /// The carried [`super::ParamSubset`] as comma-joined group tokens.
    pub const GROUPS: &str = "PresetGroups";
    /// Provenance: `"lightbox"` or `"lr"` (so an imported LR preset keeps its
    /// origin across the ingest re-home + later cold scans).
    pub const ORIGIN: &str = "PresetOrigin";
    /// For an `"lr"` origin: `crs:ProcessVersion` as written by the source app.
    pub const SOURCE_PV: &str = "PresetSourcePv";
    /// Written (as `True`) **only** onto a store copy that Lightbox's own
    /// bundled-starter seed installed and that the user has not touched
    /// since, see [`super::DevelopPreset::bundled`] and
    /// [`crate::preset_retire`]. Absent (the default for every file the
    /// store has ever written before this field existed, and for every
    /// user-authored or user-imported preset) means "the user's own", which
    /// retirement never touches.
    pub const BUNDLED: &str = "PresetBundled";
}

/// A preset's stable identity (spec §3.7): a uuid v4 string, stable across
/// renames (it lives inside the `.xmp`, not in the filename).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PresetId(pub String);

impl PresetId {
    /// A fresh random (v4) id, assigned when a preset is first authored/imported.
    pub fn new_v4() -> PresetId {
        PresetId(uuid::Uuid::new_v4().to_string())
    }

    /// A **deterministic** id derived from a file's store-relative path, used for
    /// a *foreign* `.xmp` a user dropped into the preset dir that carries no
    /// `lb:PresetId` yet, so repeated cold scans agree on its identity. (An
    /// [`PresetStore::import_files`] ingest replaces this with a durable in-file
    /// v4.)
    pub fn derived_from_path(rel: &str) -> PresetId {
        let h = twox_hash::XxHash3_128::oneshot(rel.as_bytes()).to_be_bytes();
        PresetId(uuid::Uuid::from_bytes(h).to_string())
    }
}

/// Where a preset came from (spec §3.7).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PresetOrigin {
    /// Authored in Lightbox (or an exported Lightbox preset re-imported).
    ///
    /// **This is NOT a "the app installed it" marker.** It covers both a
    /// preset the user built with `create_from` and one the bundled starter
    /// seed installed, so it can never be used to decide what is safe to
    /// retire, [`DevelopPreset::bundled`] is the field that separates the
    /// two (see [`crate::preset_retire`]).
    Lightbox,
    /// Imported from a Lightroom `crs:` `.xmp` (T26).
    LightroomImport {
        /// `crs:ProcessVersion` as written by the source app, if any.
        source_pv: Option<String>,
    },
}

/// A develop preset (spec §3.7).
#[derive(Clone, Debug)]
pub struct DevelopPreset {
    /// Stable identity.
    pub id: PresetId,
    /// Display name.
    pub name: String,
    /// Group (folder), or `None` for the ungrouped root.
    pub group: Option<String>,
    /// Which param groups this preset carries.
    pub subset: ParamSubset,
    /// The partial values (only the carried params).
    pub delta: crate::params::ParamDelta,
    /// The minimum process version this preset applies under.
    pub min_pv: ProcessVersion,
    /// Provenance.
    pub origin: PresetOrigin,
    /// **Durable "Lightbox installed this, the user did not" marker**
    /// (`lb:PresetBundled`, [`crate::preset_retire`]).
    ///
    /// `true` only on a store copy written by
    /// [`PresetStore::seed_bundled`], the app's own starter-library seed.
    /// Every other write path produces `false`:
    ///
    /// * [`PresetStore::create_from`], the user authored it;
    /// * [`PresetStore::import_files`], the user chose the file, whatever
    ///   the file claims (the flag is set by the *operation*, never read
    ///   from untrusted input, so a hand-crafted `.xmp` cannot dress itself
    ///   up as a starter preset and become retirable);
    /// * [`PresetStore::rename`], **renaming clears it**: the moment the
    ///   user edits a bundled preset in any way it becomes *theirs*, and a
    ///   later library change must never delete it. That is the safe
    ///   direction of the trade: the cost of getting it wrong this way is
    ///   one stale preset the user can delete, versus destroying work they
    ///   cannot get back.
    ///
    /// Note `create_from` is also how a user *edits* a bundled preset (the
    /// store has no in-place recipe update), so an edit re-authors the slot
    /// with a fresh id and an unset marker, same outcome as a rename.
    ///
    /// Only ever `true` alongside [`PresetOrigin::Lightbox`].
    pub bundled: bool,
    /// The on-disk `.xmp` path.
    pub path: PathBuf,
}

impl DevelopPreset {
    /// Field-wise equality **ignoring `path` and [`bundled`](Self::bundled)**
    /// (the same preset in two stores has different paths, and `bundled` is a
    /// property of how a *copy* got installed, not of the preset itself).
    /// Used by the export→import round-trip proof (T24) and, load-bearing
    /// by [`crate::preset_retire`] to prove a store file is an untouched copy
    /// of a preset the bundled library used to ship: every field a user could
    /// change (`name`, `group`, `subset`, `delta`, `min_pv`) is compared, so
    /// any edit at all makes the match fail and the preset survive.
    pub fn content_eq(&self, other: &DevelopPreset) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.group == other.group
            && self.subset == other.subset
            && self.delta == other.delta
            && self.min_pv == other.min_pv
            && self.origin == other.origin
    }

    /// A lightweight index entry (for the E08 preset browser via
    /// `Queries::presets`).
    pub fn meta(&self) -> PresetMeta {
        PresetMeta {
            id: self.id.clone(),
            name: self.name.clone(),
            group: self.group.clone(),
        }
    }
}

/// A preset's browser metadata (spec §3.4 `Queries::presets`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetMeta {
    /// Stable identity.
    pub id: PresetId,
    /// Display name.
    pub name: String,
    /// Group (folder), or `None`.
    pub group: Option<String>,
}

/// A per-file result from [`PresetStore::import_files`] (spec §3.7).
#[derive(Debug)]
pub struct PresetImportResult {
    /// The source path that was imported.
    pub source: PathBuf,
    /// The imported preset's metadata on success.
    pub preset: Option<PresetMeta>,
    /// The fidelity report when the source was a foreign `crs:` preset (T26).
    pub report: Option<crate::xmp_map::CrsImportReport>,
    /// The failure reason on error (the store stays up; the file is skipped).
    pub error: Option<String>,
}

/// A quarantined preset file (malformed / unreadable), surfaced, never fatal
/// (spec T23: "quarantined file → typed error entry, store stays up").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetLoadError {
    /// The offending file.
    pub path: PathBuf,
    /// Why it could not be loaded.
    pub reason: String,
}

/// Errors from preset operations (spec §3.7).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PresetError {
    /// Filesystem I/O failed.
    #[error("preset io: {0}")]
    Io(String),
    /// The XMP substrate / mapping layer failed.
    #[error("preset xmp: {0}")]
    Xmp(String),
    /// No preset with this id is known to the store.
    #[error("no such preset: {0:?}")]
    NotFound(PresetId),
    /// A malformed preset file (missing/unparseable required content).
    #[error("malformed preset: {0}")]
    Malformed(String),
}

impl From<crate::xmp_map::XmpMapError> for PresetError {
    fn from(e: crate::xmp_map::XmpMapError) -> PresetError {
        PresetError::Xmp(e.to_string())
    }
}
impl From<lightbox_meta::xmp::XmpError> for PresetError {
    fn from(e: lightbox_meta::xmp::XmpError) -> PresetError {
        PresetError::Xmp(e.to_string())
    }
}

// ── the store ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct Index {
    presets: Vec<Arc<DevelopPreset>>,
    quarantine: Vec<PresetLoadError>,
}

/// The file-backed preset store (spec §3.7 / D3). Cheap-to-share; the index is
/// an in-memory snapshot rebuilt on [`open`](PresetStore::open)/
/// [`refresh`](PresetStore::refresh) and after the store's own mutations.
pub struct PresetStore {
    root: PathBuf,
    index: RwLock<Index>,
}

impl PresetStore {
    /// Open (creating the root dir if needed) and cold-scan the preset dir.
    /// Malformed files are quarantined, never fatal (T23).
    pub fn open(root: PathBuf) -> Result<PresetStore, PresetError> {
        fs::create_dir_all(&root).map_err(|e| PresetError::Io(e.to_string()))?;
        let store = PresetStore {
            root,
            index: RwLock::new(Index::default()),
        };
        store.refresh()?;
        Ok(store)
    }

    /// Open the default per-user preset store (`<config>/Lightbox/presets`).
    pub fn open_default() -> Result<PresetStore, PresetError> {
        PresetStore::open(default_preset_dir())
    }

    /// The root preset directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Cold-scan (or re-scan) the preset dir, rebuilding the index and the
    /// quarantine list (T23). Picks up externally-dropped valid `.xmp` files.
    pub fn refresh(&self) -> Result<(), PresetError> {
        let mut presets: Vec<Arc<DevelopPreset>> = Vec::new();
        let mut quarantine: Vec<PresetLoadError> = Vec::new();

        for (path, group) in scan_xmp_files(&self.root) {
            match load_preset_file(&path, group, &self.root) {
                Ok(p) => presets.push(Arc::new(p)),
                Err(e) => quarantine.push(PresetLoadError {
                    path,
                    reason: e.to_string(),
                }),
            }
        }
        presets.sort_by(|a, b| {
            (a.group.as_deref(), a.name.as_str()).cmp(&(b.group.as_deref(), b.name.as_str()))
        });

        let mut idx = self.index.write().unwrap();
        idx.presets = presets;
        idx.quarantine = quarantine;
        Ok(())
    }

    /// Grouped, sorted preset metadata (spec §3.7).
    pub fn list(&self) -> Vec<PresetMeta> {
        self.index
            .read()
            .unwrap()
            .presets
            .iter()
            .map(|p| p.meta())
            .collect()
    }

    /// The full indexed presets from the last scan (not just their
    /// [`PresetMeta`]). Retirement needs every field it is going to compare,
    /// and one snapshot beats N `get`-by-id round trips.
    pub(crate) fn snapshot(&self) -> Vec<Arc<DevelopPreset>> {
        self.index.read().unwrap().presets.clone()
    }

    /// The quarantined (malformed) files from the last scan.
    pub fn quarantined(&self) -> Vec<PresetLoadError> {
        self.index.read().unwrap().quarantine.clone()
    }

    /// Fetch a full preset by id.
    pub fn get(&self, id: &PresetId) -> Option<Arc<DevelopPreset>> {
        self.index
            .read()
            .unwrap()
            .presets
            .iter()
            .find(|p| &p.id == id)
            .cloned()
    }

    /// Create a preset from a recipe's checked groups (T24). Writes a **partial**
    /// `.xmp` carrying only the subset's fields, then refreshes the index.
    pub fn create_from(
        &self,
        recipe: &Recipe,
        name: &str,
        group: Option<&str>,
        subset: &ParamSubset,
    ) -> Result<DevelopPreset, PresetError> {
        let preset = DevelopPreset {
            id: PresetId::new_v4(),
            name: name.to_string(),
            group: group.map(str::to_string),
            subset: subset.clone(),
            delta: recipe.extract(subset),
            min_pv: recipe.pv,
            origin: PresetOrigin::Lightbox,
            // The user authored this one, never a retirable starter copy,
            // including when this call is how they re-author (i.e. edit) a
            // slot the bundled seed had installed.
            bundled: false,
            path: PathBuf::new(),
        };
        let written = self.write_preset(preset)?;
        self.refresh()?;
        Ok(written)
    }

    /// Import LR or Lightbox `.xmp` files (T26). Each source is ingested into the
    /// store as a canonical Lightbox preset (a foreign LR preset gains a durable
    /// `lb:PresetId`); a Lightbox-exported preset re-imports **unchanged** through
    /// this same path.
    ///
    /// This is the **user-chose-these-files** path, so every result is written
    /// with [`DevelopPreset::bundled`] `= false` and is therefore permanently
    /// out of retirement's reach, even if the file itself carries a
    /// `lb:PresetBundled` marker (a copied starter file, or a hostile one).
    /// The app's own starter seed goes through [`PresetStore::seed_bundled`].
    pub fn import_files(&self, paths: &[PathBuf]) -> Vec<PresetImportResult> {
        self.import_batch(paths, false)
    }

    /// The shared import loop behind [`import_files`](Self::import_files) and
    /// [`seed_bundled`](Self::seed_bundled). `bundled` is decided by the
    /// *caller's operation*, never by file content.
    pub(crate) fn import_batch(&self, paths: &[PathBuf], bundled: bool) -> Vec<PresetImportResult> {
        let mut out = Vec::with_capacity(paths.len());
        let mut any_ok = false;
        for src in paths {
            match self.import_one(src, bundled) {
                Ok((meta, report)) => {
                    any_ok = true;
                    out.push(PresetImportResult {
                        source: src.clone(),
                        preset: Some(meta),
                        report,
                        error: None,
                    });
                }
                Err(e) => out.push(PresetImportResult {
                    source: src.clone(),
                    preset: None,
                    report: None,
                    error: Some(e.to_string()),
                }),
            }
        }
        if any_ok {
            let _ = self.refresh();
        }
        out
    }

    /// Rename a preset (T24). Keeps the id (it lives in the file); moves the file
    /// to the new stem and updates `lb:PresetName`.
    ///
    /// **Clears [`DevelopPreset::bundled`]**: a renamed starter preset is the
    /// user's from here on, and a later starter-library change must never
    /// retire it out from under them ([`DevelopPreset::bundled`]'s doc has
    /// the full argument for picking this direction).
    pub fn rename(&self, id: &PresetId, name: &str) -> Result<(), PresetError> {
        let preset = self
            .get(id)
            .ok_or_else(|| PresetError::NotFound(id.clone()))?;
        let renamed = DevelopPreset {
            name: name.to_string(),
            path: PathBuf::new(),
            bundled: false,
            ..(*preset).clone()
        };
        let old_path = preset.path.clone();
        self.write_preset(renamed)?;
        if old_path.exists() && old_path != self.preset_path(name, preset.group.as_deref()) {
            fs::remove_file(&old_path).map_err(|e| PresetError::Io(e.to_string()))?;
        }
        self.refresh()
    }

    /// Delete a preset (T24).
    ///
    /// **DEFERRED (E09-deviations E-3):** the spec asks for OS trash "where
    /// available"; no OS-trash crate is on the vetted §2 dep list (a `trash`
    /// dependency needs its own licensing review), so this performs a permanent
    /// removal. The seam is one function; wiring a vetted trash crate is additive.
    pub fn delete(&self, id: &PresetId) -> Result<(), PresetError> {
        let preset = self
            .get(id)
            .ok_or_else(|| PresetError::NotFound(id.clone()))?;
        if preset.path.exists() {
            fs::remove_file(&preset.path).map_err(|e| PresetError::Io(e.to_string()))?;
        }
        self.refresh()
    }

    /// Export a preset to an arbitrary path (T24). The written file re-imports to
    /// an equal preset.
    pub fn export(&self, id: &PresetId, dest: &Path) -> Result<(), PresetError> {
        let preset = self
            .get(id)
            .ok_or_else(|| PresetError::NotFound(id.clone()))?;
        let doc = preset_to_doc(&preset)?;
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| PresetError::Io(e.to_string()))?;
            }
        }
        sidecar::write_atomic(dest, &doc)?;
        Ok(())
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn import_one(
        &self,
        src: &Path,
        bundled: bool,
    ) -> Result<(PresetMeta, Option<crate::xmp_map::CrsImportReport>), PresetError> {
        let doc = sidecar::read(src)?
            .ok_or_else(|| PresetError::Io(format!("no such file: {}", src.display())))?;
        let stem = file_stem(src);
        // Group placement: an exported Lightbox preset carries lb:PresetGroup; a
        // foreign LR preset lands ungrouped.
        let group = doc.get(ns::LB, plb::GROUP).map(|v| v.as_str());
        let loaded = decode_preset(&doc, &stem, group.clone(), PathBuf::new())?;

        // Re-home into the store as a canonical Lightbox preset. A foreign import
        // gains a durable in-file v4 id (its scan-time path-derived id is replaced).
        let origin_report = match &loaded.origin {
            PresetOrigin::LightroomImport { .. } => Some(build_crs_report(&doc)),
            PresetOrigin::Lightbox => None,
        };
        let id = if doc.contains(ns::LB, plb::ID) {
            loaded.id.clone()
        } else {
            PresetId::new_v4()
        };
        let ingested = DevelopPreset {
            id,
            group,
            // Decided by the operation, NOT by `loaded.bundled` (which came
            // out of a file in a user-writable directory). This is what makes
            // "marked as bundled" unforgeable: only our own seed sets it.
            bundled,
            path: PathBuf::new(),
            ..loaded
        };
        let written = self.write_preset(ingested)?;
        Ok((written.meta(), origin_report))
    }

    /// The canonical on-disk path for a `(name, group)`.
    fn preset_path(&self, name: &str, group: Option<&str>) -> PathBuf {
        let mut p = self.root.clone();
        if let Some(g) = group.filter(|g| !g.is_empty()) {
            p.push(sanitize(g));
        }
        p.push(format!("{}.xmp", sanitize(name)));
        p
    }

    /// Serialize a preset to its canonical path (atomic write). Returns the
    /// preset with its final `path` filled in.
    fn write_preset(&self, mut preset: DevelopPreset) -> Result<DevelopPreset, PresetError> {
        let path = self.preset_path(&preset.name, preset.group.as_deref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| PresetError::Io(e.to_string()))?;
        }
        preset.path = path.clone();
        let doc = preset_to_doc(&preset)?;
        sidecar::write_atomic(&path, &doc)?;
        Ok(preset)
    }
}

/// Pure hover preview (spec §3.7 / T25): the recipe that would result from
/// applying `preset` onto `base`. **No** session, commit, or store mutation, the
/// E08 hover preview and the AI-Looks seam both use this.
pub fn preview_recipe(base: &Recipe, preset: &DevelopPreset) -> Recipe {
    let mut r = base.clone();
    // A preset delta is pre-validated (built via `extract`), so `apply` cannot
    // error here; if a hand-crafted delta ever did, the base is left unchanged.
    let _ = r.apply(&preset.delta);
    r
}

// ── file format (dogfood to_xmp) ──────────────────────────────────────────────

/// Build the `.xmp` [`XmpDoc`] for a preset: `to_xmp` of the seeded recipe
/// (neutral + delta), giving the authoritative `lb:RecipeCbor` **and** LR-
/// readable `crs:` fields for the carried params, plus the `lb:` preset header.
fn preset_to_doc(preset: &DevelopPreset) -> Result<XmpDoc, PresetError> {
    let mut seeded = Recipe::identity(preset.min_pv);
    // Pre-validated delta; ignore the Applied report.
    let _ = seeded.apply(&preset.delta);

    let ctx = XmpWriteCtx::default();
    let mut doc = seeded.to_xmp(&ctx)?;

    doc.set(ns::LB, plb::ID, XmpValue::text(preset.id.0.clone()))?;
    doc.set(ns::LB, plb::NAME, XmpValue::text(preset.name.clone()))?;
    if let Some(g) = &preset.group {
        doc.set(ns::LB, plb::GROUP, XmpValue::text(g.clone()))?;
    }
    doc.set(
        ns::LB,
        plb::GROUPS,
        XmpValue::text(encode_subset(&preset.subset)),
    )?;
    match &preset.origin {
        PresetOrigin::Lightbox => {
            doc.set(ns::LB, plb::ORIGIN, XmpValue::text("lightbox"))?;
        }
        PresetOrigin::LightroomImport { source_pv } => {
            doc.set(ns::LB, plb::ORIGIN, XmpValue::text("lr"))?;
            if let Some(pv) = source_pv {
                doc.set(ns::LB, plb::SOURCE_PV, XmpValue::text(pv.clone()))?;
            }
        }
    }
    // The bundled-starter marker is written ONLY when set, so a preset that
    // is not one serializes byte-for-byte as it always has. That keeps the
    // generator-owned `assets/presets/**` blessed files (authored through
    // `create_from`, hence never bundled) unchanged by this field's
    // introduction, no re-bless, no `assets/MANIFEST.toml` churn.
    if preset.bundled {
        doc.set(ns::LB, plb::BUNDLED, XmpValue::Bool(true))?;
    }
    // LR-compatibility marker (a develop preset has settings).
    doc.set(ns::CRS, "HasSettings", XmpValue::Bool(true))?;
    Ok(doc)
}

/// Read the durable bundled-starter marker back off a parsed store file.
/// Anything other than a well-formed truthy value, absent, `False`,
/// garbage, reads as "the user's own", which is the direction that keeps
/// data (spec: a malformed file must never become *more* deletable).
fn decode_bundled(doc: &XmpDoc) -> bool {
    doc.get(ns::LB, plb::BUNDLED)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The provenance stored in the file, if any (survives the ingest re-home of an
/// imported LR preset). `default_origin` is used when the file predates the
/// `lb:PresetOrigin` marker.
fn decode_origin(doc: &XmpDoc, default_origin: PresetOrigin) -> PresetOrigin {
    match doc.get(ns::LB, plb::ORIGIN).map(|v| v.as_str()).as_deref() {
        Some("lightbox") => PresetOrigin::Lightbox,
        Some("lr") => PresetOrigin::LightroomImport {
            source_pv: doc.get(ns::LB, plb::SOURCE_PV).map(|v| v.as_str()).or(
                match default_origin {
                    PresetOrigin::LightroomImport { source_pv } => source_pv,
                    PresetOrigin::Lightbox => None,
                },
            ),
        },
        _ => default_origin,
    }
}

/// Load a preset from a file discovered by the scan; `group` is the physical
/// folder (authoritative for in-store files).
fn load_preset_file(
    path: &Path,
    group: Option<String>,
    root: &Path,
) -> Result<DevelopPreset, PresetError> {
    let doc = sidecar::read(path)?
        .ok_or_else(|| PresetError::Malformed("file vanished during scan".into()))?;
    let stem = file_stem(path);
    let mut preset = decode_preset(&doc, &stem, group, path.to_path_buf())?;
    // A foreign file with no in-file id gets a stable, path-derived id so repeat
    // scans agree.
    if !doc.contains(ns::LB, plb::ID) {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        preset.id = PresetId::derived_from_path(&rel);
    }
    Ok(preset)
}

/// Decode a preset from a parsed [`XmpDoc`] (shared by scan + import, and by
/// [`crate::preset_retire`]'s compiled-in reference set, both sides of a
/// retirement comparison must go through this one decoder, so that any future
/// change to how a recipe deserializes moves them together and can never
/// silently turn "kept" into "retired").
pub(crate) fn decode_preset(
    doc: &XmpDoc,
    stem: &str,
    group: Option<String>,
    path: PathBuf,
) -> Result<DevelopPreset, PresetError> {
    let name = doc
        .get(ns::LB, plb::NAME)
        .map(|v| v.as_str())
        .unwrap_or_else(|| stem.to_string());
    let probe = neutral_probe();

    if doc.contains(ns::LB, crate::xmp_map::lb::RECIPE_CBOR) {
        // Lightbox-authored: authoritative lb: read-back.
        let from = Recipe::read_xmp(doc, &probe)?;
        debug_assert_eq!(from.source, XmpSource::Lb);
        let recipe = from.recipe;
        let subset = doc
            .get(ns::LB, plb::GROUPS)
            .map(|v| decode_subset(&v.as_str()))
            .unwrap_or_else(|| non_neutral_subset(&recipe));
        let id = doc
            .get(ns::LB, plb::ID)
            .map(|v| PresetId(v.as_str()))
            .unwrap_or_else(PresetId::new_v4);
        Ok(DevelopPreset {
            id,
            name,
            group,
            delta: recipe.extract(&subset),
            min_pv: recipe.pv,
            origin: decode_origin(doc, PresetOrigin::Lightbox),
            bundled: decode_bundled(doc),
            subset,
            path,
        })
    } else {
        // Foreign LR crs: preset, present-key subset detection via the report.
        let crs = Recipe::from_lr_crs(doc, &probe);
        let subset = detect_subset_from_report(&crs.report);
        let default_origin = PresetOrigin::LightroomImport {
            source_pv: crs.report.source_pv.clone(),
        };
        Ok(DevelopPreset {
            id: PresetId::new_v4(),
            name,
            group,
            delta: crs.recipe.extract(&subset),
            min_pv: crs.recipe.pv,
            origin: decode_origin(doc, default_origin),
            bundled: decode_bundled(doc),
            subset,
            path,
        })
    }
}

/// The `CrsImportReport` for a foreign preset (for `PresetImportResult`).
fn build_crs_report(doc: &XmpDoc) -> crate::xmp_map::CrsImportReport {
    Recipe::from_lr_crs(doc, &neutral_probe()).report
}

// ── subset <-> tokens, and lightbox-path -> group (present-key detection) ─────

/// The stable persisted token for a param group (in `lb:PresetGroups`).
fn group_token(g: ParamGroup) -> &'static str {
    use ParamGroup as G;
    match g {
        G::BaseProfile => "profile",
        G::WhiteBalance => "wb",
        G::Tone => "tone",
        G::Curve => "curve",
        G::ColorMixer => "hsl",
        G::ColorGrading => "grade",
        G::BwMix => "bw",
        G::Presence => "presence",
        G::Detail => "detail",
        G::Optics => "optics",
        G::Geometry => "geometry",
        G::Effects => "effects",
        G::Masks => "masks",
        G::Retouch => "retouch",
    }
}

fn group_from_token(t: &str) -> Option<ParamGroup> {
    use ParamGroup as G;
    Some(match t {
        "profile" => G::BaseProfile,
        "wb" => G::WhiteBalance,
        "tone" => G::Tone,
        "curve" => G::Curve,
        "hsl" => G::ColorMixer,
        "grade" => G::ColorGrading,
        "bw" => G::BwMix,
        "presence" => G::Presence,
        "detail" => G::Detail,
        "optics" => G::Optics,
        "geometry" => G::Geometry,
        "effects" => G::Effects,
        "masks" => G::Masks,
        "retouch" => G::Retouch,
        _ => return None,
    })
}

fn encode_subset(s: &ParamSubset) -> String {
    let mut toks: Vec<&str> = s.groups.iter().copied().map(group_token).collect();
    toks.sort_unstable();
    toks.join(",")
}

fn decode_subset(s: &str) -> ParamSubset {
    let groups: BTreeSet<ParamGroup> = s
        .split(',')
        .filter(|t| !t.is_empty())
        .filter_map(group_from_token)
        .collect();
    ParamSubset { groups }
}

/// The groups a recipe is non-neutral in (a fallback subset when `lb:PresetGroups`
/// is absent).
fn non_neutral_subset(recipe: &Recipe) -> ParamSubset {
    let neutral = Recipe::identity(recipe.pv);
    let delta = recipe.diff(&neutral);
    ParamSubset {
        groups: delta.0.keys().copied().map(group_of).collect(),
    }
}

/// Present-key subset detection (T26): the groups that a foreign `crs:` import's
/// fidelity report actually mapped/approximated. `skipped` (foreign / unmapped)
/// keys are deliberately excluded, they carry no develop settings we own.
fn detect_subset_from_report(report: &crate::xmp_map::CrsImportReport) -> ParamSubset {
    let mut groups: BTreeSet<ParamGroup> = BTreeSet::new();
    for f in report.mapped.iter().chain(report.approximate.iter()) {
        if let Some(g) = group_of_lightbox_path(&f.lightbox) {
            groups.insert(g);
        }
    }
    ParamSubset { groups }
}

/// Map a `CrsImportReport` field path (e.g. `"global.exposure"`) to its group.
/// The paths are exactly those emitted by [`Recipe::from_lr_crs`].
fn group_of_lightbox_path(p: &str) -> Option<ParamGroup> {
    use ParamGroup as G;
    let g = if p == "base_profile" {
        G::BaseProfile
    } else if p.starts_with("global.white_balance") {
        G::WhiteBalance
    } else if matches!(
        p,
        "global.exposure"
            | "global.contrast"
            | "global.highlights"
            | "global.shadows"
            | "global.whites"
            | "global.blacks"
    ) {
        G::Tone
    } else if p == "global.tone_curve" {
        G::Curve
    } else if p.starts_with("global.hsl") {
        G::ColorMixer
    } else if p.starts_with("global.color_grade") {
        G::ColorGrading
    } else if p == "global.treatment" || p == "global.bw" {
        G::BwMix
    } else if p.starts_with("global.presence") || p == "global.vibrance" || p == "global.saturation"
    {
        G::Presence
    } else if p.starts_with("global.detail") {
        G::Detail
    } else if p.starts_with("global.optics") {
        G::Optics
    } else if p.starts_with("global.effects") {
        G::Effects
    } else if p.starts_with("geometry") {
        G::Geometry
    } else {
        return None;
    };
    Some(g)
}

// ── filesystem helpers ────────────────────────────────────────────────────────

/// Discover `.xmp` files: the root (group `None`) and each immediate subdir
/// (group = subdir name). Presets live at depth 0-1 (`<root>/<group>/<name>.xmp`).
fn scan_xmp_files(root: &Path) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_xmp(&path) {
            out.push((path, None));
        } else if path.is_dir() {
            let group = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|g| !g.starts_with('.')); // skip hidden (e.g. a future .trash)
            if let Some(group) = group {
                if let Ok(sub) = fs::read_dir(&path) {
                    for e in sub.flatten() {
                        let p = e.path();
                        if is_xmp(&p) {
                            out.push((p, Some(group.clone())));
                        }
                    }
                }
            }
        }
    }
    out
}

fn is_xmp(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

fn file_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "preset".to_string())
}

/// Sanitize a name/group into a safe single path component.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | ' ' | '-' | '_' | '.' | '(' | ')' => c,
            _ => '_',
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "preset".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Resolve the per-user config dir without the `directories` crate (E09-deviations
/// E-1): platform env vars only, so no `option-ext`/MPL-2.0 enters the license
/// surface. Falls back to a workspace-local `.lightbox` if the environment is bare.
fn default_preset_dir() -> PathBuf {
    let base = config_base();
    base.join("Lightbox").join("presets")
}

fn config_base() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config");
        }
    }
    PathBuf::from(".lightbox")
}

/// A neutral probe for the read side (no as-shot seeding needed for presets).
fn neutral_probe() -> AssetProbe {
    AssetProbe {
        format: ProbedFormat::Unsupported("preset".to_string()),
        width: 0,
        height: 0,
        orientation: Orientation::O1,
        camera_make: None,
        camera_model: None,
        capture_time: None,
        file_bytes: 0,
        embedded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ParamDelta, ParamId, ParamValue};
    use lightbox_types::PV_M0;

    fn edited() -> Recipe {
        let mut r = Recipe::identity(PV_M0);
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Exposure, ParamValue::F32(1.25));
        d.0.insert(ParamId::Contrast, ParamValue::F32(20.0));
        d.0.insert(ParamId::Vibrance, ParamValue::F32(15.0));
        r.apply(&d).unwrap();
        r
    }

    #[test]
    fn subset_token_bijection_total() {
        for g in [
            ParamGroup::BaseProfile,
            ParamGroup::WhiteBalance,
            ParamGroup::Tone,
            ParamGroup::Curve,
            ParamGroup::ColorMixer,
            ParamGroup::ColorGrading,
            ParamGroup::BwMix,
            ParamGroup::Presence,
            ParamGroup::Detail,
            ParamGroup::Optics,
            ParamGroup::Geometry,
            ParamGroup::Effects,
            ParamGroup::Masks,
            ParamGroup::Retouch,
        ] {
            assert_eq!(group_from_token(group_token(g)), Some(g));
        }
    }

    #[test]
    fn create_apply_changes_only_checked_groups() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
        let subset = ParamSubset::from_groups([ParamGroup::Tone]);
        let p = store
            .create_from(&edited(), "ToneOnly", None, &subset)
            .unwrap();

        let neutral = Recipe::identity(PV_M0);
        let previewed = preview_recipe(&neutral, &p);
        // Every changed param is in a checked group.
        for id in previewed.diff(&neutral).0.keys() {
            assert!(subset.contains(*id), "changed {id:?} outside the subset");
        }
        // The checked group actually moved.
        assert_ne!(
            previewed.get(ParamId::Contrast),
            neutral.get(ParamId::Contrast)
        );
        // Vibrance (Presence, unchecked) is untouched.
        assert_eq!(
            previewed.get(ParamId::Vibrance),
            neutral.get(ParamId::Vibrance)
        );
    }

    #[test]
    fn preview_is_pure() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::open(dir.path().to_path_buf()).unwrap();
        let subset = ParamSubset::from_groups([ParamGroup::Tone, ParamGroup::Presence]);
        let p = store.create_from(&edited(), "P", None, &subset).unwrap();
        let base = Recipe::identity(PV_M0);
        let before = base.clone();
        let _ = preview_recipe(&base, &p);
        assert_eq!(base, before, "preview_recipe mutated its base");
    }
}
