// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! D3, `keymap.toml` override persistence (spec §5.2/§6.8).
//!
//! The file is **delta-only**: one row per *user rebind* (never the full
//! map, shipped defaults stay in code so they can improve across
//! versions). An empty string means "explicitly unbound".
//!
//! ```toml
//! keymap_version = 1
//!
//! [bindings]
//! "nav.next" = "N"
//! "view.zoom_toggle" = ""
//! ```
//!
//! **Tolerant load** (spec §7: "corrupt prefs/keymap file → defaults +
//! notice, never fatal"): an unparsable file leaves the registry on
//! defaults and surfaces a [`LoadReport`]; bad rows are skipped
//! per-row and reported; rows naming action ids this build doesn't know
//! are preserved verbatim across the next save (forward-compat with newer
//! builds' actions). Writes are atomic (temp file + rename in the target
//! directory).
//!
//! **Location / Phase-G seam.** [`default_keymap_path`] resolves to the
//! per-user app-data directory E04's `lightbox_core::default_store_dir`
//! established (`~/Library/Application Support/Lightbox` on macOS
//! `keymap.toml` sits NEXT to `edits.lbdata`). Phase G's machine-scope
//! `prefs.toml` (§6.7) belongs in this same directory: reuse this
//! resolution, do not introduce a second config-dir convention (or a
//! `directories` dependency; see `E04-deviations.md` for why it's
//! hand-rolled).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::chord::Chord;
use super::registry::KeymapRegistry;

/// Where `keymap.toml` lives (see the module docs for the Phase-G seam).
pub fn default_keymap_path() -> PathBuf {
    let store = lightbox_core::default_store_dir();
    match store.parent() {
        Some(dir) => dir.join("keymap.toml"),
        None => store.join("keymap.toml"), // unreachable in practice
    }
}

/// The format version this build writes.
#[allow(dead_code)] // written by `save_overrides` (D5-editor seam, below)
const KEYMAP_VERSION: i64 = 1;

/// One tolerated-and-skipped row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    /// The row's action id, verbatim.
    pub id: String,
    /// Why it was skipped.
    pub reason: String,
}

/// What a load did, surfaced in the status bar when anything was off
/// (H3 upgrades this to a notice chip).
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Rows applied as overrides (including explicit unbinds).
    pub applied: usize,
    /// Rows for unknown action ids, preserved verbatim for the next save.
    pub unknown: usize,
    /// Rows skipped (unparsable chord / wrong type), with reasons.
    pub skipped: Vec<SkippedRow>,
    /// `Some` when the FILE was unusable (TOML syntax error, wrong shape)
    /// the registry stays on defaults.
    pub file_error: Option<String>,
}

impl LoadReport {
    /// A one-line user notice, or `None` when the load was clean.
    pub fn notice(&self) -> Option<String> {
        if let Some(err) = &self.file_error {
            return Some(format!("keymap.toml unreadable, using defaults ({err})"));
        }
        if !self.skipped.is_empty() {
            let ids: Vec<&str> = self.skipped.iter().map(|s| s.id.as_str()).collect();
            return Some(format!(
                "keymap.toml: {} binding(s) skipped ({})",
                self.skipped.len(),
                ids.join(", ")
            ));
        }
        None
    }
}

/// A load that could not even reach the parse stage.
#[derive(Debug)]
pub enum KeymapLoadError {
    /// The file exists but could not be read.
    Io(std::io::Error),
}

impl std::fmt::Display for KeymapLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapLoadError::Io(e) => write!(f, "keymap.toml unreadable: {e}"),
        }
    }
}

impl std::error::Error for KeymapLoadError {}

impl KeymapRegistry {
    /// Loads user overrides from `path` onto this (freshly-built)
    /// registry. Missing file = clean no-op. Corrupt file = defaults +
    /// report (never an error). Only unreadable-but-present files err.
    pub fn load_overrides(&mut self, path: &Path) -> Result<LoadReport, KeymapLoadError> {
        let mut report = LoadReport::default();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(KeymapLoadError::Io(e)),
        };

        let table: toml::Table = match text.parse() {
            Ok(table) => table,
            Err(e) => {
                report.file_error = Some(e.to_string());
                return Ok(report);
            }
        };

        // Forward-compat: a newer format version still loads tolerantly
        // (rows are plain strings either way); the version row exists so a
        // future breaking change CAN gate on it.
        let bindings = match table.get("bindings") {
            Some(toml::Value::Table(bindings)) => bindings,
            Some(_) => {
                report.file_error = Some("[bindings] is not a table".to_owned());
                return Ok(report);
            }
            None => return Ok(report), // an empty delta is a valid file
        };

        for (id, value) in bindings {
            let Some(raw) = value.as_str() else {
                report.skipped.push(SkippedRow {
                    id: id.clone(),
                    reason: format!("expected a string, got {}", value.type_str()),
                });
                continue;
            };
            let Some(def) = self.def_by_str(id) else {
                // Not ours: another build's action. Preserve verbatim.
                self.foreign.insert(id.clone(), raw.to_owned());
                report.unknown += 1;
                continue;
            };
            let def_id = def.id;
            if raw.trim().is_empty() {
                self.force_bind(def_id, None); // explicit unbind
                report.applied += 1;
                continue;
            }
            match Chord::parse(raw) {
                Ok(chord) => {
                    self.force_bind(def_id, Some(chord));
                    report.applied += 1;
                }
                Err(e) => report.skipped.push(SkippedRow {
                    id: id.clone(),
                    reason: e.to_string(),
                }),
            }
        }
        Ok(report)
    }

    /// Writes the override DELTA (plus preserved unknown-id rows) to
    /// `path`, atomically: temp file in the same directory, then rename
    /// a crash mid-save never truncates the previous file.
    ///
    /// No runtime caller until the D5 rebind editor (this phase's named
    /// cut-line, see `registry::RebindConflict`'s doc): until then the
    /// file is user-authored and this build only loads it. Test-proven so
    /// D5 is pure UI.
    #[allow(dead_code)]
    pub fn save_overrides(&self, path: &Path) -> std::io::Result<()> {
        // Deterministic row order (BTreeMap), ours and foreign interleaved
        // by id, stable diffs for users who keep dotfiles in git.
        let mut rows: BTreeMap<String, String> = self
            .foreign
            .iter()
            .map(|(id, raw)| (id.clone(), raw.clone()))
            .collect();
        for (id, chord) in self.overrides_delta() {
            let value = chord.map(|c| c.to_config_string()).unwrap_or_default();
            rows.insert(id.0.to_owned(), value);
        }

        let mut bindings = toml::Table::new();
        for (id, value) in rows {
            bindings.insert(id, toml::Value::String(value));
        }
        let mut root = toml::Table::new();
        root.insert(
            "keymap_version".to_owned(),
            toml::Value::Integer(KEYMAP_VERSION),
        );
        root.insert("bindings".to_owned(), toml::Value::Table(bindings));
        let text = toml::to_string(&root).map_err(std::io::Error::other)?;

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(text.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{default_registry, ActionId, EDIT_UNDO, NAV_NEXT, VIEW_ZOOM_TOGGLE};
    use eframe::egui::{Key, Modifiers};
    use proptest::prelude::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn missing_file_is_a_clean_noop() {
        let dir = tmpdir();
        let mut r = default_registry();
        let report = r.load_overrides(&dir.path().join("keymap.toml")).unwrap();
        assert_eq!(report.applied, 0);
        assert!(report.notice().is_none());
        assert!(r.is_default(NAV_NEXT));
    }

    /// D3 AC: a corrupt file falls back to defaults with a surfaced
    /// `LoadReport`, never a hard failure.
    #[test]
    fn corrupt_file_keeps_defaults_and_reports() {
        let dir = tmpdir();
        let path = dir.path().join("keymap.toml");
        std::fs::write(&path, "keymap_version = [this is not toml").unwrap();
        let mut r = default_registry();
        let report = r.load_overrides(&path).unwrap();
        assert!(report.file_error.is_some());
        let notice = report.notice().expect("a corrupt file must surface");
        assert!(
            notice.contains("defaults"),
            "notice names the fallback: {notice}"
        );
        for def in r.defs() {
            assert!(r.is_default(def.id), "{} must stay default", def.id.0);
        }
    }

    /// D3 AC: an unknown-id row survives a save (forward-compat with a
    /// newer build's actions).
    #[test]
    fn unknown_id_rows_survive_a_save() {
        let dir = tmpdir();
        let path = dir.path().join("keymap.toml");
        std::fs::write(
            &path,
            "keymap_version = 1\n[bindings]\n\"e12.mask.paint\" = \"B\"\n",
        )
        .unwrap();

        let mut r = default_registry();
        let report = r.load_overrides(&path).unwrap();
        assert_eq!(report.unknown, 1);
        assert!(report.notice().is_none(), "unknown ids are not a problem");

        r.rebind(NAV_NEXT, Some(Chord::plain(Key::N))).unwrap();
        r.save_overrides(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"e12.mask.paint\" = \"B\""),
            "verbatim row preserved:\n{text}"
        );
        assert!(text.contains("\"nav.next\" = \"N\""), "{text}");
        assert!(text.contains("keymap_version = 1"), "{text}");
    }

    #[test]
    fn bad_rows_are_skipped_and_reported_while_good_rows_apply() {
        let dir = tmpdir();
        let path = dir.path().join("keymap.toml");
        std::fs::write(
            &path,
            concat!(
                "keymap_version = 1\n",
                "[bindings]\n",
                "\"nav.next\" = \"N\"\n",
                "\"nav.prev\" = \"Hyper+Q\"\n", // unknown modifier
                "\"edit.undo\" = 3\n",          // wrong type
            ),
        )
        .unwrap();

        let mut r = default_registry();
        let report = r.load_overrides(&path).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped.len(), 2);
        let notice = report.notice().expect("skips surface");
        assert!(notice.contains("nav.prev") && notice.contains("edit.undo"));

        assert_eq!(r.binding(NAV_NEXT), Some(Chord::plain(Key::N)));
        assert!(r.is_default(ActionId("nav.prev")), "bad row left default");
        assert!(r.is_default(EDIT_UNDO), "wrong-typed row left default");
    }

    #[test]
    fn explicit_unbind_round_trips() {
        let dir = tmpdir();
        let path = dir.path().join("keymap.toml");
        let mut a = default_registry();
        a.rebind(VIEW_ZOOM_TOGGLE, None).unwrap();
        a.save_overrides(&path).unwrap();

        let mut b = default_registry();
        b.load_overrides(&path).unwrap();
        assert_eq!(b.binding(VIEW_ZOOM_TOGGLE), None);
        assert!(!b.is_default(VIEW_ZOOM_TOGGLE));
    }

    /// The save is delta-only: a registry on all-defaults writes an empty
    /// bindings table, and the atomic write leaves no temp litter behind.
    #[test]
    fn default_registry_saves_an_empty_delta_atomically() {
        let dir = tmpdir();
        let path = dir.path().join("keymap.toml");
        default_registry().save_overrides(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("keymap_version = 1"));
        assert!(
            !text.contains("nav.next") && !text.contains("app.open"),
            "defaults are never written out:\n{text}"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("keymap.toml")]);
    }

    #[test]
    fn keymap_path_sits_next_to_the_e04_store_dir() {
        let path = default_keymap_path();
        assert_eq!(path.file_name().unwrap(), "keymap.toml");
        assert_eq!(
            path.parent(),
            lightbox_core::default_store_dir().parent(),
            "same directory Phase G's prefs.toml will use (§6.7 seam)"
        );
    }

    // ── D3 AC: round-trip property test ────────────────────────────────

    /// Rebindable chord generator: enough key/modifier variety to shake
    /// out spelling bugs, canonical by construction.
    fn chord_strategy() -> impl Strategy<Value = Chord> {
        let keys = prop_oneof![
            Just(Key::A),
            Just(Key::N),
            Just(Key::Num7),
            Just(Key::F5),
            Just(Key::Space),
            Just(Key::Tab),
            Just(Key::Enter),
            Just(Key::Escape),
            Just(Key::ArrowUp),
            Just(Key::ArrowDown),
            Just(Key::Plus),
            Just(Key::Minus),
            Just(Key::Slash),
            Just(Key::Comma),
        ];
        (
            keys,
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(key, alt, shift, command, raw_ctrl)| {
                Chord::new(
                    Modifiers {
                        alt,
                        shift,
                        command,
                        ctrl: raw_ctrl,
                        mac_cmd: false,
                    },
                    key,
                )
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Any set of rebinds/unbinds survives save → load bit-exact
        /// (same live binding for every action, same foreign rows).
        #[test]
        fn overrides_round_trip_through_the_file(
            rebinds in proptest::collection::vec(
                (0usize..15, proptest::option::of(chord_strategy())),
                0..10,
            )
        ) {
            let dir = tmpdir();
            let path = dir.path().join("keymap.toml");

            let mut a = default_registry();
            let ids: Vec<ActionId> = a.defs().iter().map(|d| d.id).collect();
            for (idx, chord) in rebinds {
                // force_bind: the property is persistence fidelity, not
                // conflict policy (rebind() conflicts are tested in D1).
                a.force_bind(ids[idx % ids.len()], chord);
            }
            a.save_overrides(&path).unwrap();

            let mut b = default_registry();
            let report = b.load_overrides(&path).unwrap();
            prop_assert!(report.file_error.is_none());
            prop_assert!(report.skipped.is_empty());
            for id in &ids {
                prop_assert_eq!(
                    a.binding(*id), b.binding(*id),
                    "binding of {} diverged after round-trip", id.0
                );
                prop_assert_eq!(a.is_default(*id), b.is_default(*id));
            }

            // Idempotence: saving the loaded registry reproduces the file.
            let first = std::fs::read_to_string(&path).unwrap();
            b.save_overrides(&path).unwrap();
            prop_assert_eq!(first, std::fs::read_to_string(&path).unwrap());
        }
    }
}
