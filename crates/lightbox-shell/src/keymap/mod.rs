// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E08 Phase D — the remappable keymap (spec §6.8): declarative action
//! registry, innermost-context-wins resolution over a per-frame context
//! stack, a dispatcher that runs **before** widget input with text-focus
//! suppression, delta-only `keymap.toml` override persistence, and the
//! ⌘/ cheat-sheet overlay.
//!
//! Module map (spec §3): [`chord`] (D1), [`registry`] (D1), [`dispatch`]
//! (D2), [`overrides`] (D3), [`cheatsheet`] (D4). The rebind editor
//! (`editor.rs`, D5) is the phase's named CUT-LINE — it also needs Phase
//! G's prefs panel as a host, so it lands there; the registry API it needs
//! ([`KeymapRegistry::rebind`]/[`KeymapRegistry::conflicts_with`]/
//! [`KeymapRegistry::reset`]) is complete and tested now.
//!
//! **Context stack (M1).** `lib.rs` pushes `"app"` → `"editor"` every
//! frame, plus `"editor.loupe"` while an entry is active in the canvas.
//! `"editor.filmstrip"` / `"editor.panels"` / `"editor.gizmo.<id>"` are
//! declared for E10/E11/E12 and Phase E/F to push (spec §6.8) — nothing
//! pushes them yet.
//!
//! **How E10/E11/E12 add actions:** `registry.register(ActionDef { .. })`
//! with a fresh dotted id (duplicate ids panic at registration), then match
//! the id where your frame code handles dispatched actions. The cheat
//! sheet and `keymap.toml` overrides pick the new action up with no
//! further wiring.

pub mod cheatsheet;
pub mod chord;
pub mod dispatch;
pub mod overrides;
pub mod registry;

pub use chord::Chord;
// Part of the keymap's public seam (live binding display); runtime callers
// today reach it via `keymap::chord::Platform` (cheatsheet.rs) — this
// re-export is for the D5 editor + E10/E11/E12 registrants.
#[allow(unused_imports)]
pub use chord::Platform;
pub use registry::{ActionDef, ActionId, ContextId, KeymapRegistry};

use eframe::egui::Key;

// ─── Contexts (spec §6.8 stack) ─────────────────────────────────────────────

/// Root context — on every stack.
pub const CTX_APP: ContextId = ContextId("app");
/// The editor workspace (the only mode this product has).
pub const CTX_EDITOR: ContextId = ContextId("editor");
/// The loupe canvas is showing an active entry.
pub const CTX_LOUPE: ContextId = ContextId("editor.loupe");
/// Filmstrip-focused interactions (declared for Phase E/E10 pushers).
#[allow(dead_code)]
pub const CTX_FILMSTRIP: ContextId = ContextId("editor.filmstrip");
/// Panel-rail-focused interactions (declared for Phase E pushers).
#[allow(dead_code)]
pub const CTX_PANELS: ContextId = ContextId("editor.panels");
/// An on-canvas gizmo holds input (pushed by Phase F's `GizmoLayer`).
pub const CTX_GIZMO: ContextId = ContextId("editor.gizmo");

// ─── M1 action ids (spec §6.8 default set) ──────────────────────────────────

/// Open files… (the OS dialog).
pub const APP_OPEN: ActionId = ActionId("app.open");
/// Preferences (target is a Phase-G stub until the panel exists).
pub const APP_PREFS: ActionId = ActionId("app.prefs");
/// Toggle the keyboard-shortcut cheat sheet (D4).
pub const APP_CHEATSHEET: ActionId = ActionId("app.cheatsheet");
/// Undo the active image's last edit (E09 `Command::Edit`).
pub const EDIT_UNDO: ActionId = ActionId("edit.undo");
/// Redo.
pub const EDIT_REDO: ActionId = ActionId("edit.redo");
/// Next filmstrip entry (repeatable).
pub const NAV_NEXT: ActionId = ActionId("nav.next");
/// Previous filmstrip entry (repeatable).
pub const NAV_PREV: ActionId = ActionId("nav.prev");
/// Fit ↔ 100% toggle.
pub const VIEW_ZOOM_TOGGLE: ActionId = ActionId("view.zoom_toggle");
/// Second default binding for the same toggle (spec §6.8 names "Z/Space";
/// `ActionDef.default` holds ONE chord, so the alternate is its own
/// rebindable action — see `E08-deviations.md` Phase D).
pub const VIEW_ZOOM_TOGGLE_ALT: ActionId = ActionId("view.zoom_toggle_alt");
/// Step up the zoom ladder (repeatable).
pub const VIEW_ZOOM_IN: ActionId = ActionId("view.zoom_in");
/// Step down the zoom ladder (repeatable).
pub const VIEW_ZOOM_OUT: ActionId = ActionId("view.zoom_out");
/// Show/hide the right develop rail.
pub const PANEL_TOGGLE_RAIL: ActionId = ActionId("panel.toggle_rail");
/// Collapse/expand the filmstrip.
pub const FILM_TOGGLE: ActionId = ActionId("film.toggle");
/// Cancel the active gizmo (handler lands with Phase F's `GizmoLayer`).
pub const GIZMO_CANCEL: ActionId = ActionId("gizmo.cancel");
/// Commit the active gizmo (handler lands with Phase F).
pub const GIZMO_COMMIT: ActionId = ActionId("gizmo.commit");

/// The M1 default action set, registered (spec §6.8). E10/E11/E12 register
/// theirs on top of this at their own mount points.
pub fn default_registry() -> KeymapRegistry {
    let mut r = KeymapRegistry::new();
    let defs = [
        ActionDef {
            id: APP_OPEN,
            label: "Open files…",
            category: "Application",
            contexts: &[CTX_APP],
            default: Some(Chord::cmd(Key::O)),
            repeatable: false,
        },
        ActionDef {
            id: APP_PREFS,
            label: "Preferences",
            category: "Application",
            contexts: &[CTX_APP],
            default: Some(Chord::cmd(Key::Comma)),
            repeatable: false,
        },
        ActionDef {
            id: APP_CHEATSHEET,
            label: "Keyboard shortcuts",
            category: "Application",
            contexts: &[CTX_APP],
            default: Some(Chord::cmd(Key::Slash)),
            repeatable: false,
        },
        ActionDef {
            id: EDIT_UNDO,
            label: "Undo",
            category: "Edit",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::cmd(Key::Z)),
            repeatable: true,
        },
        ActionDef {
            id: EDIT_REDO,
            label: "Redo",
            category: "Edit",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::cmd_shift(Key::Z)),
            repeatable: true,
        },
        ActionDef {
            id: NAV_NEXT,
            label: "Next photo",
            category: "Navigation",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::plain(Key::ArrowRight)),
            repeatable: true,
        },
        ActionDef {
            id: NAV_PREV,
            label: "Previous photo",
            category: "Navigation",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::plain(Key::ArrowLeft)),
            repeatable: true,
        },
        ActionDef {
            id: VIEW_ZOOM_TOGGLE,
            label: "Toggle Fit / 100%",
            category: "View",
            contexts: &[CTX_LOUPE],
            default: Some(Chord::plain(Key::Z)),
            repeatable: false,
        },
        ActionDef {
            id: VIEW_ZOOM_TOGGLE_ALT,
            label: "Toggle Fit / 100% (alternate)",
            category: "View",
            contexts: &[CTX_LOUPE],
            default: Some(Chord::plain(Key::Space)),
            repeatable: false,
        },
        ActionDef {
            id: VIEW_ZOOM_IN,
            label: "Zoom in",
            category: "View",
            contexts: &[CTX_LOUPE],
            default: Some(Chord::plain(Key::Plus)),
            repeatable: true,
        },
        ActionDef {
            id: VIEW_ZOOM_OUT,
            label: "Zoom out",
            category: "View",
            contexts: &[CTX_LOUPE],
            default: Some(Chord::plain(Key::Minus)),
            repeatable: true,
        },
        ActionDef {
            id: PANEL_TOGGLE_RAIL,
            label: "Show / hide develop panels",
            category: "Workspace",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::plain(Key::Tab)),
            repeatable: false,
        },
        ActionDef {
            id: FILM_TOGGLE,
            label: "Collapse / expand filmstrip",
            category: "Workspace",
            contexts: &[CTX_EDITOR],
            default: Some(Chord::shift(Key::Tab)),
            repeatable: false,
        },
        ActionDef {
            id: GIZMO_CANCEL,
            label: "Cancel tool",
            category: "Tools",
            contexts: &[CTX_GIZMO],
            default: Some(Chord::plain(Key::Escape)),
            repeatable: false,
        },
        ActionDef {
            id: GIZMO_COMMIT,
            label: "Commit tool",
            category: "Tools",
            contexts: &[CTX_GIZMO],
            default: Some(Chord::plain(Key::Enter)),
            repeatable: false,
        },
    ];
    for def in defs {
        r.register(def);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The M1 set registers clean (no duplicate ids — `register` panics)
    /// and every action's default is either unique in its context or a
    /// deliberate, documented share.
    #[test]
    fn m1_default_set_registers_without_internal_conflicts() {
        let r = default_registry();
        for def in r.defs() {
            let Some(chord) = r.binding(def.id) else {
                continue;
            };
            for ctx in def.contexts {
                let holders = r.conflicts_with(chord, *ctx);
                assert_eq!(
                    holders,
                    vec![def.id],
                    "{} must be the only holder of {:?} in {}",
                    def.id.0,
                    chord.display(Platform::current()),
                    ctx.0
                );
            }
        }
    }

    /// Spec §6.8 default spellings, pinned.
    #[test]
    fn m1_defaults_match_the_spec() {
        let r = default_registry();
        let display = |id: ActionId| r.binding(id).map(|c| c.display(Platform::MacOs)).unwrap();
        assert_eq!(display(APP_OPEN), "⌘O");
        assert_eq!(display(APP_PREFS), "⌘,");
        assert_eq!(display(APP_CHEATSHEET), "⌘/");
        assert_eq!(display(EDIT_UNDO), "⌘Z");
        assert_eq!(display(EDIT_REDO), "⇧⌘Z");
        assert_eq!(display(NAV_NEXT), "→");
        assert_eq!(display(NAV_PREV), "←");
        assert_eq!(display(VIEW_ZOOM_TOGGLE), "Z");
        assert_eq!(display(VIEW_ZOOM_TOGGLE_ALT), "Space");
        assert_eq!(display(VIEW_ZOOM_IN), "+");
        assert_eq!(display(VIEW_ZOOM_OUT), "−");
        assert_eq!(display(PANEL_TOGGLE_RAIL), "Tab");
        assert_eq!(display(FILM_TOGGLE), "⇧Tab");
        assert_eq!(display(GIZMO_CANCEL), "Escape");
        assert_eq!(display(GIZMO_COMMIT), "Enter");
    }
}
