// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-core::prefs`, the two-scope preferences store (E08 spec §6.7,
//! Phase G G1/G2).
//!
//! **Machine scope** ([`MachinePrefs`]) lives in `prefs.toml` in the same
//! per-user app-data directory as `keymap.toml` and `edits.lbdata` (E04's
//! [`crate::default_store_dir`]'s parent, see [`default_prefs_dir`]; do NOT
//! introduce a second config-dir convention). The file discipline mirrors
//! the shell's `keymap/overrides.rs` (D3):
//!
//! * **Tolerant load** (spec §7: "corrupt prefs/keymap file → defaults +
//!   notice, never fatal"): an unparsable file leaves every pref on its
//!   default and surfaces a notice via [`PrefsStore::load_notice`]; a bad
//!   *field* is skipped per-field and reported, the rest of the file still
//!   applies; top-level keys this build doesn't know are preserved verbatim
//!   across the next save (forward-compat with newer builds' prefs).
//! * **Atomic save**: temp file + rename in the target directory, a crash
//!   mid-save never truncates the previous file. A failed save is a logged
//!   warning, never fatal (the in-memory value still applies this session).
//!
//! **Catalog scope** ([`CatalogPrefs`]) lives in the catalog's
//! `catalog_settings` key/value table (migration 0006), bound per session
//! via [`PrefsStore::bind_catalog`] (reached from the shell through
//! [`crate::Session::bind_prefs`], `Catalog` itself never crosses seam 1).
//! Rows are delta-only (a value returning to its default deletes the row),
//! values are parsed tolerantly (a bad row logs + keeps the default), and
//! writes go through the catalog's writer transaction (spec §6.7
//! `set_catalog`: "writer txn + watch notify").
//!
//! **Watch channels.** Both scopes hand out `tokio::sync::watch` receivers;
//! [`PrefsStore::set_machine`]/[`PrefsStore::set_catalog`] notify exactly
//! once per call (the G1 AC), whether or not the closure changed anything.
//!
//! **Consumers** (spec §6.7 table, E08 wires, owners act): the shell reads
//! `gpu_mode` before session construction (core receives `gpu: None` for
//! `Off`); [`JobKnobs`] fold into [`crate::CoreConfig`] at `Core::start`;
//! `Session::open` reads the catalog scope to build the preview store's
//! caps/retention/root (so `SetCacheLimits`/`RelocateCacheStore` effects
//! survive a restart); E13 will read `vram_budget_override_mb` (stored now,
//! consumed later, the knob is inert at M1 by design).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lightbox_catalog::Catalog;
use lightbox_preview::CacheLimits;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// The format version this build writes into `prefs.toml`.
const PREFS_VERSION: i64 = 1;

/// Where `prefs.toml` (and `keymap.toml`) live: the parent of E04's
/// [`crate::default_store_dir`] (`~/Library/Application Support/Lightbox`
/// on macOS), the directory convention Phase D reserved (D0-3).
pub fn default_prefs_dir() -> PathBuf {
    let store = crate::default_store_dir();
    match store.parent() {
        Some(dir) => dir.to_path_buf(),
        None => store, // unreachable in practice
    }
}

/// GPU engine mode (spec §6.7). `Off` ⇒ the shell passes `gpu: None` at
/// session construction and the engine runs its CPU path, restart-required
/// at M1 (spec Q6; no live engine rebuild exists yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuMode {
    /// GPU when available, else CPU (the engine's own `BackendPref::Auto`).
    #[default]
    Auto,
    /// Force the CPU engine (`gpu: None` at session construction).
    Off,
}

/// UI theme (spec §6.7), applied live via `egui::ThemePreference`.
///
/// **Default is `Dark`, not `System`** (theme spec "Motion & reduced-motion"
/// §: "default to Dark regardless of OS, since a mid-shoot theme flip
/// triggered by an OS-wide toggle would be jarring, and dark-by-default is
/// the established professional-tool convention this app is chasing").
/// `System` remains a selectable override for users who want it, but must
/// never be the out-of-box default: the develop rail's panel bodies
/// (`panels::host::rail_contents_ui`) fill unconditionally with the dark-
/// only `theme::tokens::ELEV_1_PANEL`, so a `System` default resolving to an
/// OS-light appearance would drive not-yet-individually-reskinned panel
/// text (Looks/Presets/History/Geometry) through `theme::light_visuals`'s
/// near-black `override_text_color` onto that permanently-dark background
/// exactly the "black text on grey" defect this default change fixes at the
/// root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the OS. Selectable, but NOT the default, see the enum doc.
    System,
    /// The out-of-box default (see the enum doc).
    #[default]
    Dark,
    Light,
}

/// Per-class job-system overrides (spec §6.7 `JobKnobs`), `None` = the
/// built-in default ([`lightbox_jobs::JobConfig::default`]'s core-derived
/// sizing). Folded into [`crate::CoreConfig`] at `Core::start` via
/// [`JobKnobs::apply_to`]; restart-required (live re-sizing is E06's later
/// seam).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobKnobs {
    /// Tokio worker threads (`None` = one per core).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_threads: Option<usize>,
    /// Concurrent `Class::Interactive` jobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_slots: Option<usize>,
    /// Concurrent `Class::Foreground` jobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_slots: Option<usize>,
    /// Concurrent `Class::Background` jobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_slots: Option<usize>,
    /// Event broadcast-channel capacity override → `CoreConfig::
    /// event_capacity` (the G5 "advanced" knob; grouped here because it is
    /// applied at the same `Core::start` moment as the job sizing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_capacity: Option<usize>,
}

impl JobKnobs {
    /// Folds the overrides into a [`lightbox_jobs::JobConfig`] (fields with
    /// `None` keep the config's existing value).
    pub fn apply_to(&self, cfg: &mut lightbox_jobs::JobConfig) {
        if self.worker_threads.is_some() {
            cfg.worker_threads = self.worker_threads;
        }
        if let Some(n) = self.interactive_slots {
            cfg.interactive_slots = n;
        }
        if let Some(n) = self.foreground_slots {
            cfg.foreground_slots = n;
        }
        if let Some(n) = self.background_slots {
            cfg.background_slots = n;
        }
    }
}

/// Machine-scope preferences (spec §6.7), `prefs.toml`.
///
/// **Not `Copy`** (it was, through E08 Phase G): [`MachinePrefs::panel_layout`]
/// carries owned `Vec`/`String` data. Read it with [`PrefsStore::machine`],
/// which clones.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachinePrefs {
    /// GPU engine mode (restart-required at M1, Q6).
    pub gpu_mode: GpuMode,
    /// Stored for E13 (which reads it later); the prefs panel shows the
    /// probe value next to it. Inert at M1 by design.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_budget_override_mb: Option<u32>,
    /// Per-class worker overrides → `CoreConfig` at start.
    pub jobs: JobKnobs,
    /// §2.4 folder-drop default (Alt still overrides per-drop).
    pub drop_recursive_default: bool,
    /// Filmstrip strip height, logical points (the B5 persistence seam;
    /// the shell clamps to its own 48-160 pt band).
    pub filmstrip_height_pt: f32,
    /// Filmstrip collapsed state (alongside the height per `filmstrip.rs`'s
    /// module-doc seam note; additive to the spec's §6.7 field list).
    pub filmstrip_collapsed: bool,
    /// UI theme (live).
    pub theme: Theme,
    /// UI accent color as `[r, g, b]`; `None` = the shipped accent
    /// (`#5583a8`). The shell owns the named palette Preferences offers
    /// core stores only the chosen value, so a hand-edited file can name
    /// any color and there is no preset list to keep in sync across the
    /// seam. Live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent_rgb: Option<[u8; 3]>,
    /// Develop-panel dock layout (which panels sit in which rail/column,
    /// column widths, collapsed sections, panel-solo). `None` = the shell's
    /// built-in default (every registered panel in one right-hand column,
    /// in registration order). Written by `panels::host` whenever the user
    /// drags a panel, resizes a column, or collapses a section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel_layout: Option<PanelLayoutPrefs>,
}

/// One docked column of develop panels ([`PanelLayoutPrefs`]).
///
/// Panel ids are the shell's `PanelId` strings ("develop.basic", …). Core
/// deliberately does not know the set, an id this build's shell no longer
/// registers is dropped at load, and a newly registered panel that the file
/// doesn't mention is appended to the last right-hand column, so upgrading
/// never loses a panel or resurrects a retired one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelColumnPrefs {
    /// Column width in logical points (the shell clamps to its own band).
    pub width_pt: f32,
    /// Panel ids, top to bottom.
    pub panels: Vec<String>,
}

/// The develop rails' persisted dock layout ([`MachinePrefs::panel_layout`]).
///
/// `left`/`right` are the two rails' columns in **visual left-to-right
/// order** on each side, so `right[0]` is the column nearest the canvas and
/// `left[0]` is the one nearest the window edge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelLayoutPrefs {
    /// Left rail columns (empty = no left rail).
    pub left: Vec<PanelColumnPrefs>,
    /// Right rail columns.
    pub right: Vec<PanelColumnPrefs>,
    /// Panel-solo mode (opening one panel collapses the rest).
    pub solo: bool,
    /// Panel ids whose section is collapsed; every other panel is open
    /// (sections default to open, so storing the exceptions keeps the file
    /// small and makes a newly registered panel default-open).
    pub collapsed: Vec<String>,
    /// Panel ids the user has switched **off** in Preferences → Panels.
    /// A hidden panel is absent from the rails entirely but keeps its
    /// place in `left`/`right`, so switching it back on returns it to the
    /// column it came from rather than to the end of the rail.
    #[serde(default)]
    pub hidden: Vec<String>,
}

impl Default for MachinePrefs {
    fn default() -> Self {
        MachinePrefs {
            gpu_mode: GpuMode::Auto,
            vram_budget_override_mb: None,
            jobs: JobKnobs::default(),
            drop_recursive_default: false,
            filmstrip_height_pt: 96.0, // the shell's DEFAULT_HEIGHT_PT
            filmstrip_collapsed: false,
            theme: Theme::Dark,
            accent_rgb: None,
            panel_layout: None,
        }
    }
}

/// The struct fields of [`MachinePrefs`] plus the version row, every other
/// top-level `prefs.toml` key is a newer build's and is preserved verbatim
/// across saves (the keymap store's forward-compat discipline).
const MACHINE_KEYS: &[&str] = &[
    "prefs_version",
    "gpu_mode",
    "vram_budget_override_mb",
    "jobs",
    "drop_recursive_default",
    "filmstrip_height_pt",
    "filmstrip_collapsed",
    "theme",
    "accent_rgb",
    "panel_layout",
];

/// Catalog-scope preferences (spec §6.7), `catalog_settings` rows
/// (migration 0006), one catalog at a time via [`PrefsStore::bind_catalog`].
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogPrefs {
    /// Preview-pyramid byte cap (spec §5.7 default 20 GiB). Applied live
    /// via `Command::SetCacheLimits`; re-applied at session open.
    pub preview_cache_cap_bytes: u64,
    /// Raw-cache byte cap (§3.3 default 5 GiB). Same application path.
    pub raw_cache_cap_bytes: u64,
    /// T2 tile retention; `None` = never evict. Default 30 days. Applied at
    /// store open (restart-required, the store has no live retention
    /// setter at M1).
    pub t2_retention_days: Option<u32>,
    /// Cache-store root override, written by the shell after a successful
    /// `Command::RelocateCacheStore` so the relocation survives restart.
    /// `None` = the catalog's own `.lbdata` directory.
    pub cache_dir_override: Option<PathBuf>,
}

impl Default for CatalogPrefs {
    fn default() -> Self {
        let limits = CacheLimits::default();
        CatalogPrefs {
            preview_cache_cap_bytes: limits.preview_cap_bytes,
            raw_cache_cap_bytes: limits.rawcache_cap_bytes,
            t2_retention_days: Some(30), // PreviewStoreConfig's Retention::Days(30)
            cache_dir_override: None,
        }
    }
}

impl CatalogPrefs {
    /// The §6.7 consumer projection: these prefs as E03's cap type.
    pub fn cache_limits(&self) -> CacheLimits {
        CacheLimits {
            preview_cap_bytes: self.preview_cache_cap_bytes,
            rawcache_cap_bytes: self.raw_cache_cap_bytes,
        }
    }
}

// Registry-stable `catalog_settings` keys (dotted names; rows are
// delta-only, see `default_row_value`).
const KEY_PREVIEW_CAP: &str = "cache.preview_cap_bytes";
const KEY_RAWCACHE_CAP: &str = "cache.rawcache_cap_bytes";
const KEY_T2_RETENTION: &str = "cache.t2_retention_days";
const KEY_CACHE_DIR: &str = "cache.dir_override";

/// The two-scope prefs store (spec §6.7). One per app process, owned by the
/// shell; core reads the catalog scope itself at session open (see
/// [`load_catalog_prefs`]) so the store's binding order doesn't gate store
/// construction.
pub struct PrefsStore {
    /// `prefs.toml` (machine scope).
    machine_path: PathBuf,
    /// Current machine snapshot + watch fan-out (the sender doubles as the
    /// storage cell, `borrow()` reads, `send_replace()` writes+notifies).
    machine: watch::Sender<MachinePrefs>,
    /// Current catalog snapshot + watch fan-out.
    catalog: watch::Sender<CatalogPrefs>,
    /// Unknown top-level `prefs.toml` keys, preserved verbatim across
    /// saves (forward-compat with newer builds).
    foreign: Mutex<toml::Table>,
    /// The bound catalog (write-through target); `None` until
    /// [`Self::bind_catalog`].
    bound: Mutex<Option<Arc<Catalog>>>,
    /// One-line user notice from the machine load, or `None` when clean.
    load_notice: Option<String>,
}

impl PrefsStore {
    /// Opens the machine scope from `dir/prefs.toml` (tolerant load
    /// missing file = clean defaults; corrupt file = defaults + notice;
    /// bad fields skipped per-field + notice; never an error).
    pub fn open_machine(dir: &Path) -> PrefsStore {
        let machine_path = dir.join("prefs.toml");
        let (prefs, foreign, load_notice) = load_machine_file(&machine_path);
        PrefsStore {
            machine_path,
            machine: watch::Sender::new(prefs),
            catalog: watch::Sender::new(CatalogPrefs::default()),
            foreign: Mutex::new(foreign),
            bound: Mutex::new(None),
            load_notice,
        }
    }

    /// The machine file this store loads/saves (panel readout).
    pub fn machine_path(&self) -> &Path {
        &self.machine_path
    }

    /// One-line user notice when the machine load was anything but clean
    /// (status bar; H3 upgrades it to a notice chip).
    pub fn load_notice(&self) -> Option<&str> {
        self.load_notice.as_deref()
    }

    /// Current machine snapshot.
    pub fn machine(&self) -> MachinePrefs {
        self.machine.borrow().clone()
    }

    /// Current catalog snapshot (defaults until [`Self::bind_catalog`]).
    pub fn catalog(&self) -> CatalogPrefs {
        self.catalog.borrow().clone()
    }

    /// Mutates the machine scope: applies `f`, saves `prefs.toml`
    /// atomically (a failed save logs + keeps the in-memory value), and
    /// notifies the machine watch exactly once.
    pub fn set_machine(&self, f: impl FnOnce(&mut MachinePrefs)) {
        let mut next = self.machine.borrow().clone();
        f(&mut next);
        self.save_machine(&next);
        self.machine.send_replace(next);
    }

    /// Mutates the catalog scope: applies `f`, writes the delta through the
    /// bound catalog's writer transaction (a failed write logs + keeps the
    /// in-memory value; unbound = in-memory only, logged), and notifies the
    /// catalog watch exactly once.
    pub fn set_catalog(&self, f: impl FnOnce(&mut CatalogPrefs)) {
        let mut next = self.catalog.borrow().clone();
        f(&mut next);
        match self.bound.lock().expect("prefs.bound poisoned").as_ref() {
            Some(catalog) => {
                if let Err(err) = write_catalog_prefs(catalog, &next) {
                    tracing::warn!(
                        target: "lightbox_core::prefs",
                        %err,
                        "catalog_settings write failed; prefs apply in-memory this session"
                    );
                }
            }
            None => tracing::warn!(
                target: "lightbox_core::prefs",
                "set_catalog before bind_catalog — prefs apply in-memory only"
            ),
        }
        self.catalog.send_replace(next);
    }

    /// Binds the catalog scope to `catalog`: loads its `catalog_settings`
    /// rows (tolerantly) and arms write-through for [`Self::set_catalog`].
    /// The shell reaches this through [`crate::Session::bind_prefs`].
    pub fn bind_catalog(&mut self, catalog: &Arc<Catalog>) {
        let loaded = load_catalog_prefs(catalog);
        *self.bound.lock().expect("prefs.bound poisoned") = Some(Arc::clone(catalog));
        self.catalog.send_replace(loaded);
    }

    /// A fresh machine-scope watch receiver (current value already marked
    /// seen; `changed()` resolves on the next [`Self::set_machine`]).
    pub fn watch_machine(&self) -> watch::Receiver<MachinePrefs> {
        self.machine.subscribe()
    }

    /// A fresh catalog-scope watch receiver.
    pub fn watch_catalog(&self) -> watch::Receiver<CatalogPrefs> {
        self.catalog.subscribe()
    }

    /// Atomic `prefs.toml` write: known fields from `prefs`, preserved
    /// foreign keys re-emitted verbatim, temp-then-rename. Failure is a
    /// warning, never fatal.
    fn save_machine(&self, prefs: &MachinePrefs) {
        if let Err(err) = self.try_save_machine(prefs) {
            tracing::warn!(
                target: "lightbox_core::prefs",
                path = %self.machine_path.display(),
                %err,
                "prefs.toml save failed; prefs apply in-memory this session"
            );
        }
    }

    fn try_save_machine(&self, prefs: &MachinePrefs) -> std::io::Result<()> {
        use std::io::Write as _;

        let mut root = toml::Table::try_from(prefs).map_err(std::io::Error::other)?;
        root.insert(
            "prefs_version".to_owned(),
            toml::Value::Integer(PREFS_VERSION),
        );
        // Foreign keys (a newer build's) survive verbatim; ours win on a
        // (theoretical) collision because we insert them after.
        let foreign = self.foreign.lock().expect("prefs.foreign poisoned");
        for (key, value) in foreign.iter() {
            root.entry(key.clone()).or_insert_with(|| value.clone());
        }
        drop(foreign);
        let text = toml::to_string(&root).map_err(std::io::Error::other)?;

        let dir = self.machine_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(text.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.machine_path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Loads `prefs.toml`: `(prefs, foreign-keys, notice)`. Never errors.
fn load_machine_file(path: &Path) -> (MachinePrefs, toml::Table, Option<String>) {
    let defaults = MachinePrefs::default();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (defaults, toml::Table::new(), None);
        }
        Err(e) => {
            return (
                defaults,
                toml::Table::new(),
                Some(format!("prefs.toml unreadable — using defaults ({e})")),
            );
        }
    };

    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => {
            return (
                defaults,
                toml::Table::new(),
                Some(format!("prefs.toml unreadable — using defaults ({e})")),
            );
        }
    };

    let mut prefs = defaults;
    let mut skipped: Vec<String> = Vec::new();
    let mut foreign = toml::Table::new();
    for (key, value) in &table {
        if !MACHINE_KEYS.contains(&key.as_str()) {
            // Not ours: a newer build's key. Preserve verbatim.
            foreign.insert(key.clone(), value.clone());
        }
    }

    // Per-field tolerant apply (a bad field is skipped + reported; the
    // rest of the file still lands, the keymap store's per-row
    // discipline, adapted to fields).
    fn field<T: serde::de::DeserializeOwned>(
        table: &toml::Table,
        key: &str,
        skipped: &mut Vec<String>,
    ) -> Option<T> {
        let value = table.get(key)?;
        match value.clone().try_into() {
            Ok(v) => Some(v),
            Err(e) => {
                skipped.push(format!("{key} ({e})"));
                None
            }
        }
    }

    if let Some(v) = field(&table, "gpu_mode", &mut skipped) {
        prefs.gpu_mode = v;
    }
    if let Some(v) = field(&table, "vram_budget_override_mb", &mut skipped) {
        prefs.vram_budget_override_mb = Some(v);
    }
    if let Some(v) = field(&table, "jobs", &mut skipped) {
        prefs.jobs = v;
    }
    if let Some(v) = field(&table, "drop_recursive_default", &mut skipped) {
        prefs.drop_recursive_default = v;
    }
    if let Some(v) = field(&table, "filmstrip_height_pt", &mut skipped) {
        prefs.filmstrip_height_pt = v;
    }
    if let Some(v) = field(&table, "filmstrip_collapsed", &mut skipped) {
        prefs.filmstrip_collapsed = v;
    }
    if let Some(v) = field(&table, "theme", &mut skipped) {
        prefs.theme = v;
    }
    if let Some(v) = field(&table, "accent_rgb", &mut skipped) {
        prefs.accent_rgb = Some(v);
    }
    if let Some(v) = field(&table, "panel_layout", &mut skipped) {
        prefs.panel_layout = Some(v);
    }

    let notice = if skipped.is_empty() {
        None
    } else {
        Some(format!(
            "prefs.toml: {} setting(s) skipped ({})",
            skipped.len(),
            skipped.join(", ")
        ))
    };
    (prefs, foreign, notice)
}

/// Reads the catalog scope from `catalog_settings` (missing rows = the
/// defaults; a bad row logs + keeps the default, never fatal). Also
/// called directly by `Session::open` so the persisted caps/retention/root
/// shape the preview store before any `PrefsStore` is bound.
pub(crate) fn load_catalog_prefs(catalog: &Catalog) -> CatalogPrefs {
    let mut prefs = CatalogPrefs::default();
    let reader = catalog.reader();
    let get = |key: &str| -> Option<String> {
        match reader.get_setting(key) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "lightbox_core::prefs",
                    key,
                    %err,
                    "catalog_settings read failed; using the default"
                );
                None
            }
        }
    };
    let warn_bad = |key: &str, raw: &str| {
        tracing::warn!(
            target: "lightbox_core::prefs",
            key,
            raw,
            "catalog_settings row unparsable; using the default"
        );
    };

    if let Some(raw) = get(KEY_PREVIEW_CAP) {
        match raw.parse::<u64>() {
            Ok(v) => prefs.preview_cache_cap_bytes = v,
            Err(_) => warn_bad(KEY_PREVIEW_CAP, &raw),
        }
    }
    if let Some(raw) = get(KEY_RAWCACHE_CAP) {
        match raw.parse::<u64>() {
            Ok(v) => prefs.raw_cache_cap_bytes = v,
            Err(_) => warn_bad(KEY_RAWCACHE_CAP, &raw),
        }
    }
    if let Some(raw) = get(KEY_T2_RETENTION) {
        if raw == "never" {
            prefs.t2_retention_days = None;
        } else {
            match raw.parse::<u32>() {
                Ok(v) => prefs.t2_retention_days = Some(v),
                Err(_) => warn_bad(KEY_T2_RETENTION, &raw),
            }
        }
    }
    if let Some(raw) = get(KEY_CACHE_DIR) {
        if raw.is_empty() {
            warn_bad(KEY_CACHE_DIR, &raw);
        } else {
            prefs.cache_dir_override = Some(PathBuf::from(raw));
        }
    }
    prefs
}

/// Writes the catalog scope as delta rows through one writer transaction
/// (default values delete their row, see the migration file header).
fn write_catalog_prefs(
    catalog: &Arc<Catalog>,
    prefs: &CatalogPrefs,
) -> lightbox_catalog::Result<()> {
    let defaults = CatalogPrefs::default();
    let row = |differs: bool, value: String| -> Option<String> { differs.then_some(value) };
    let rows: Vec<(&'static str, Option<String>)> = vec![
        (
            KEY_PREVIEW_CAP,
            row(
                prefs.preview_cache_cap_bytes != defaults.preview_cache_cap_bytes,
                prefs.preview_cache_cap_bytes.to_string(),
            ),
        ),
        (
            KEY_RAWCACHE_CAP,
            row(
                prefs.raw_cache_cap_bytes != defaults.raw_cache_cap_bytes,
                prefs.raw_cache_cap_bytes.to_string(),
            ),
        ),
        (
            KEY_T2_RETENTION,
            row(
                prefs.t2_retention_days != defaults.t2_retention_days,
                match prefs.t2_retention_days {
                    Some(days) => days.to_string(),
                    None => "never".to_owned(),
                },
            ),
        ),
        (
            KEY_CACHE_DIR,
            prefs
                .cache_dir_override
                .as_ref()
                // `to_string_lossy`: TEXT column; a non-UTF-8 cache path
                // would round-trip lossily (recorded in E08-deviations.md).
                .map(|p| p.to_string_lossy().into_owned()),
        ),
    ];
    catalog.writer().with_txn(move |txn| {
        for (key, value) in &rows {
            txn.set_setting(key, value.as_deref())?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(width_pt: f32, panels: &[&str]) -> PanelColumnPrefs {
        PanelColumnPrefs {
            width_pt,
            panels: panels.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    /// The develop rails' dock layout is a nested table in `prefs.toml`,
    /// not a scalar, so it gets the machine scope's whole contract
    /// exercised end to end against a real file: write, reopen, reread.
    #[test]
    fn panel_layout_round_trips_through_the_machine_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PrefsStore::open_machine(dir.path());
        assert_eq!(store.machine().panel_layout, None, "absent by default");

        let layout = PanelLayoutPrefs {
            left: vec![column(320.0, &["develop.history"])],
            right: vec![
                column(280.0, &["develop.basic", "develop.curve"]),
                column(240.0, &["develop.presets"]),
            ],
            solo: true,
            collapsed: vec!["develop.curve".to_owned()],
            hidden: vec!["develop.bw".to_owned()],
        };
        store.set_machine(|m| m.panel_layout = Some(layout.clone()));

        let reopened = PrefsStore::open_machine(dir.path());
        assert_eq!(reopened.load_notice(), None, "a clean file must not warn");
        assert_eq!(reopened.machine().panel_layout, Some(layout));
    }

    /// Tolerant load (spec §7): a corrupt `panel_layout` costs the user
    /// their panel arrangement and nothing else, every other setting
    /// still applies, an unknown key from a newer build still survives the
    /// next save, and the whole thing is a notice, never an error.
    #[test]
    fn a_corrupt_panel_layout_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("prefs.toml"),
            "prefs_version = 1\n\
             theme = \"light\"\n\
             panel_layout = \"not-a-table\"\n\
             from_a_newer_build = 42\n",
        )
        .expect("seed prefs.toml");

        let store = PrefsStore::open_machine(dir.path());
        let machine = store.machine();
        assert_eq!(machine.theme, Theme::Light, "sound fields still apply");
        assert_eq!(machine.panel_layout, None, "the bad field is dropped");
        let notice = store.load_notice().expect("a skipped field must notify");
        assert!(notice.contains("panel_layout"), "notice was {notice:?}");

        // And the forward-compat key survives the save the shell performs
        // the first time the user touches anything.
        store.set_machine(|m| m.filmstrip_collapsed = true);
        let text = std::fs::read_to_string(dir.path().join("prefs.toml")).expect("read back");
        assert!(
            text.contains("from_a_newer_build"),
            "a newer build's key must survive our save: {text}"
        );
    }
}
