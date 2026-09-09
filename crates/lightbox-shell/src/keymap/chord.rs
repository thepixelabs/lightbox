// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! D1, [`Chord`]: one key plus modifiers, with parse/display round-trips
//! on **both** platforms' display strings (spec §6.8).
//!
//! **Canonical modifier form.** egui's [`egui::Modifiers`] is deliberately
//! redundant (`command` mirrors `mac_cmd` on macOS and `ctrl` elsewhere), so
//! raw input for the *same logical chord* hashes differently across
//! platforms. Every `Chord` in this module is therefore **canonicalized**
//! (via [`Chord::new`] / [`Chord::from_input`] / [`Chord::parse`]):
//!
//! * `command`, the logical primary modifier (⌘ on macOS, Ctrl elsewhere);
//! * `ctrl`, **only** the macOS Control key as a *distinct* modifier
//!   (`⌃` / spelled `MacCtrl`); on non-mac platforms Ctrl *is* the primary
//!   modifier, so canonical `ctrl` is always false there;
//! * `mac_cmd`, always false in canonical form.
//!
//! Never construct `Chord { .. }` literally, `Eq`/`Hash` (the registry's
//! reverse index) assume canonical `mods`.
//!
//! **Spelling.** `parse` accepts both display flavors: the macOS
//! glyph-concatenated form (`"⇧⌘Z"`, `"⌘/"`) and the `+`-separated form
//! (`"Ctrl+Shift+Z"`, `"Cmd+O"`). `Ctrl`/`Cmd`/`Command`/`Super`/`Meta` all
//! mean the *logical primary* modifier; the raw macOS Control key is spelled
//! `MacCtrl` (or `⌃`). Key names are egui's ([`egui::Key::from_name`]) plus
//! the arrow glyphs `←→↑↓`.

use eframe::egui;

/// Which platform's display convention to render (spec §6.8:
/// "⌘⇧Z vs Ctrl+Shift+Z").
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    /// macOS: glyph-concatenated, HIG modifier order (⌃⌥⇧⌘).
    MacOs,
    /// Windows/Linux: `+`-separated names (Ctrl+Alt+Shift+K).
    Other,
}

impl Platform {
    /// The compile-target platform.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

/// A key chord: one non-modifier key plus a (canonical) modifier set.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    /// Canonical modifiers, see the module docs; never build this field
    /// by hand.
    pub mods: egui::Modifiers,
    /// The non-modifier key.
    pub key: egui::Key,
}

/// Why a chord string failed to parse (D3's tolerant loader reports these
/// per-row, never fatally).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChordParseError {
    /// Empty (or all-whitespace) input.
    Empty,
    /// A `+`-separated token before the key wasn't a known modifier name.
    UnknownModifier(String),
    /// The key token isn't a key egui knows.
    UnknownKey(String),
}

impl std::fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChordParseError::Empty => write!(f, "empty chord"),
            ChordParseError::UnknownModifier(t) => write!(f, "unknown modifier {t:?}"),
            ChordParseError::UnknownKey(t) => write!(f, "unknown key {t:?}"),
        }
    }
}

impl Chord {
    /// Canonicalizing constructor (see the module docs).
    pub fn new(mods: egui::Modifiers, key: egui::Key) -> Chord {
        Chord {
            mods: canonicalize(mods),
            key,
        }
    }

    /// A chord as observed in a raw [`egui::Event::Key`], same
    /// canonicalization as [`Chord::new`], named for call-site clarity in
    /// the dispatcher.
    pub fn from_input(mods: egui::Modifiers, key: egui::Key) -> Chord {
        Chord::new(mods, key)
    }

    /// Bare key, no modifiers.
    pub fn plain(key: egui::Key) -> Chord {
        Chord::new(egui::Modifiers::NONE, key)
    }

    /// Primary modifier (⌘ / Ctrl) + key.
    pub fn cmd(key: egui::Key) -> Chord {
        Chord::new(egui::Modifiers::COMMAND, key)
    }

    /// Primary modifier + Shift + key.
    pub fn cmd_shift(key: egui::Key) -> Chord {
        Chord::new(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, key)
    }

    /// Shift + key.
    pub fn shift(key: egui::Key) -> Chord {
        Chord::new(egui::Modifiers::SHIFT, key)
    }

    /// True when the chord carries a "hard" modifier (primary or raw
    /// macOS Control). The D2 dispatcher lets these through even while a
    /// text field owns the keyboard; everything else (bare keys,
    /// Shift/Alt combos, i.e. typing) is suppressed (spec §6.8).
    pub fn has_primary_modifier(&self) -> bool {
        self.mods.command || self.mods.ctrl
    }

    /// Parses either platform's display string, or the canonical config
    /// spelling ([`Chord::to_config_string`]).
    pub fn parse(s: &str) -> Result<Chord, ChordParseError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ChordParseError::Empty);
        }
        if s == "+" {
            return Ok(Chord::plain(egui::Key::Plus));
        }

        if s.contains('+') {
            // `+`-separated form. A trailing '+' means the key itself is
            // "+" ("Ctrl++").
            let (mods_str, key_str) = match s.strip_suffix('+') {
                Some(rest) => (rest, "+"),
                None => {
                    let idx = s.rfind('+').expect("contains('+') checked");
                    (&s[..idx], &s[idx + 1..])
                }
            };
            let mut mods = egui::Modifiers::NONE;
            for token in mods_str.split('+').filter(|t| !t.trim().is_empty()) {
                mods |= parse_modifier(token.trim())?;
            }
            return Ok(Chord {
                mods,
                key: parse_key(key_str.trim())?,
            });
        }

        // Glyph-concatenated (mac) form, or a bare key name.
        let mut mods = egui::Modifiers::NONE;
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            let m = match c {
                '⌃' => egui::Modifiers::CTRL, // canonical raw-Control (never `command`)
                '⌥' => egui::Modifiers::ALT,
                '⇧' => egui::Modifiers::SHIFT,
                '⌘' => egui::Modifiers::COMMAND,
                _ => break,
            };
            // A trailing glyph with nothing after it is the KEY, not a
            // modifier (e.g. Key::Questionmark has no glyph collision, but
            // guard anyway).
            if rest.len() == c.len_utf8() {
                break;
            }
            mods |= m;
            rest = &rest[c.len_utf8()..];
        }
        Ok(Chord {
            mods,
            key: parse_key(rest)?,
        })
    }

    /// The user-facing binding string (cheat-sheet, D5's future editor).
    pub fn display(&self, platform: Platform) -> String {
        match platform {
            Platform::MacOs => {
                // Apple HIG modifier order: Control, Option, Shift, Command.
                let mut out = String::new();
                if self.mods.ctrl {
                    out.push('⌃');
                }
                if self.mods.alt {
                    out.push('⌥');
                }
                if self.mods.shift {
                    out.push('⇧');
                }
                if self.mods.command {
                    out.push('⌘');
                }
                out.push_str(&key_display(self.key));
                out
            }
            Platform::Other => {
                let mut parts: Vec<&str> = Vec::new();
                if self.mods.command {
                    parts.push("Ctrl");
                }
                if self.mods.ctrl {
                    // The raw macOS Control key has no non-mac equivalent;
                    // this spelling exists only so display→parse stays a
                    // bijection (it can never arrive from real non-mac
                    // input, `canonicalize` folds Ctrl into `command`
                    // there).
                    parts.push("MacCtrl");
                }
                if self.mods.alt {
                    parts.push("Alt");
                }
                if self.mods.shift {
                    parts.push("Shift");
                }
                let key = key_display(self.key);
                parts.push(&key);
                parts.join("+")
            }
        }
    }

    /// The canonical, platform-neutral spelling persisted to `keymap.toml`
    /// (D3): `Cmd`/`MacCtrl`/`Alt`/`Shift` + egui's [`egui::Key::name`].
    #[allow(dead_code)] // reached via `save_overrides` (D5-editor seam)
    pub fn to_config_string(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.mods.command {
            parts.push("Cmd");
        }
        if self.mods.ctrl {
            parts.push("MacCtrl");
        }
        if self.mods.alt {
            parts.push("Alt");
        }
        if self.mods.shift {
            parts.push("Shift");
        }
        parts.push(self.key.name());
        parts.join("+")
    }
}

/// Folds egui's redundant modifier encoding into the canonical form the
/// module docs define.
fn canonicalize(mods: egui::Modifiers) -> egui::Modifiers {
    let primary = mods.command || mods.mac_cmd;
    // Raw Control survives only where it's a *distinct* key: on macOS
    // (`mac_cmd` present or `command` unset). On non-mac input, `ctrl` and
    // `command` are the same physical key, fold them.
    let raw_ctrl = mods.ctrl && (mods.mac_cmd || !primary);
    egui::Modifiers {
        alt: mods.alt,
        ctrl: raw_ctrl,
        shift: mods.shift,
        mac_cmd: false,
        command: primary,
    }
}

fn parse_modifier(token: &str) -> Result<egui::Modifiers, ChordParseError> {
    // `Ctrl` is the LOGICAL primary modifier (it displays as "Ctrl" on
    // non-mac); the raw macOS Control key is spelled `MacCtrl`/`⌃`.
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "meta" | "win" | "ctrl" | "control" | "⌘" => {
            Ok(egui::Modifiers::COMMAND)
        }
        "macctrl" | "⌃" => Ok(egui::Modifiers::CTRL),
        "alt" | "option" | "opt" | "⌥" => Ok(egui::Modifiers::ALT),
        "shift" | "⇧" => Ok(egui::Modifiers::SHIFT),
        _ => Err(ChordParseError::UnknownModifier(token.to_owned())),
    }
}

fn parse_key(token: &str) -> Result<egui::Key, ChordParseError> {
    let aliased = match token {
        "←" => "ArrowLeft",
        "→" => "ArrowRight",
        "↑" => "ArrowUp",
        "↓" => "ArrowDown",
        other => other,
    };
    egui::Key::from_name(aliased).ok_or_else(|| ChordParseError::UnknownKey(token.to_owned()))
}

/// The key part of a display string. Arrow glyphs beat egui's `⏴⏵⏶⏷`;
/// everything else uses [`egui::Key::symbol_or_name`] (which
/// [`egui::Key::from_name`] round-trips).
fn key_display(key: egui::Key) -> String {
    match key {
        egui::Key::ArrowLeft => "←".to_owned(),
        egui::Key::ArrowRight => "→".to_owned(),
        egui::Key::ArrowUp => "↑".to_owned(),
        egui::Key::ArrowDown => "↓".to_owned(),
        other => other.symbol_or_name().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    fn sample_chords() -> Vec<Chord> {
        vec![
            Chord::plain(Key::Z),
            Chord::plain(Key::Space),
            Chord::plain(Key::Tab),
            Chord::plain(Key::Escape),
            Chord::plain(Key::Enter),
            Chord::plain(Key::ArrowRight),
            Chord::plain(Key::ArrowLeft),
            Chord::plain(Key::Plus),
            Chord::plain(Key::Minus),
            Chord::cmd(Key::O),
            Chord::cmd(Key::Comma),
            Chord::cmd(Key::Slash),
            Chord::cmd(Key::Z),
            Chord::cmd_shift(Key::Z),
            Chord::shift(Key::Tab),
            Chord::cmd(Key::Plus),
            Chord::new(Modifiers::CTRL, Key::C), // raw macOS Control
            Chord::new(Modifiers::ALT | Modifiers::COMMAND, Key::ArrowDown),
        ]
    }

    /// D1 AC: parse/display round-trip on BOTH platforms' display strings.
    #[test]
    fn parse_display_round_trips_on_both_platforms() {
        for chord in sample_chords() {
            for platform in [Platform::MacOs, Platform::Other] {
                let shown = chord.display(platform);
                let parsed = Chord::parse(&shown)
                    .unwrap_or_else(|e| panic!("{shown:?} ({platform:?}) failed to parse: {e}"));
                assert_eq!(parsed, chord, "round-trip through {shown:?} ({platform:?})");
            }
        }
    }

    /// D3 relies on the canonical config spelling round-tripping too.
    #[test]
    fn config_string_round_trips() {
        for chord in sample_chords() {
            let s = chord.to_config_string();
            assert_eq!(Chord::parse(&s).unwrap(), chord, "config spelling {s:?}");
        }
    }

    #[test]
    fn known_display_strings_look_right() {
        assert_eq!(Chord::cmd_shift(Key::Z).display(Platform::MacOs), "⇧⌘Z");
        assert_eq!(
            Chord::cmd_shift(Key::Z).display(Platform::Other),
            "Ctrl+Shift+Z"
        );
        assert_eq!(Chord::cmd(Key::Slash).display(Platform::MacOs), "⌘/");
        assert_eq!(Chord::plain(Key::ArrowRight).display(Platform::MacOs), "→");
        assert_eq!(Chord::plain(Key::ArrowRight).display(Platform::Other), "→");
        assert_eq!(Chord::cmd(Key::Plus).display(Platform::Other), "Ctrl++");
    }

    #[test]
    fn raw_input_canonicalizes_identically_across_platforms() {
        // macOS ⌘Z: winit/egui set BOTH mac_cmd and command.
        let mac = Chord::from_input(
            Modifiers {
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
            Key::Z,
        );
        // Windows/Linux Ctrl+Z: egui sets BOTH ctrl and command.
        let win = Chord::from_input(
            Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            },
            Key::Z,
        );
        let parsed = Chord::parse("Cmd+Z").unwrap();
        assert_eq!(mac, parsed);
        assert_eq!(win, parsed);
        assert_eq!(mac, win);
    }

    #[test]
    fn raw_mac_control_stays_distinct_from_the_primary_modifier() {
        // macOS Control (no Cmd): ctrl set, command NOT set.
        let mac_ctrl = Chord::from_input(
            Modifiers {
                ctrl: true,
                ..Default::default()
            },
            Key::C,
        );
        assert_ne!(mac_ctrl, Chord::cmd(Key::C));
        assert_eq!(mac_ctrl.display(Platform::MacOs), "⌃C");
        assert_eq!(Chord::parse("⌃C").unwrap(), mac_ctrl);
        assert_eq!(Chord::parse("MacCtrl+C").unwrap(), mac_ctrl);
        // macOS ⌘⌃: both survive canonicalization.
        let both = Chord::from_input(
            Modifiers {
                ctrl: true,
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
            Key::C,
        );
        assert!(both.mods.command && both.mods.ctrl);
    }

    #[test]
    fn parse_rejects_garbage_with_named_errors() {
        assert_eq!(Chord::parse(""), Err(ChordParseError::Empty));
        assert_eq!(Chord::parse("   "), Err(ChordParseError::Empty));
        assert_eq!(
            Chord::parse("Hyper+Z"),
            Err(ChordParseError::UnknownModifier("Hyper".to_owned()))
        );
        assert_eq!(
            Chord::parse("Cmd+Bogus"),
            Err(ChordParseError::UnknownKey("Bogus".to_owned()))
        );
    }

    #[test]
    fn parse_accepts_friendly_aliases() {
        assert_eq!(
            Chord::parse("Command+Shift+Z").unwrap(),
            Chord::cmd_shift(Key::Z)
        );
        assert_eq!(Chord::parse("Esc").unwrap(), Chord::plain(Key::Escape));
        assert_eq!(Chord::parse("Return").unwrap(), Chord::plain(Key::Enter));
        assert_eq!(
            Chord::parse("Right").unwrap(),
            Chord::plain(Key::ArrowRight)
        );
        assert_eq!(Chord::parse("ctrl+o").unwrap(), Chord::cmd(Key::O));
    }
}
