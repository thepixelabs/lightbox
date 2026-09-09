// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! D4, the ⌘/ cheat-sheet overlay (spec §6.8, a named differentiator):
//! every registered action grouped by category, showing **live** bindings
//! (any D3 override, not the shipped default, a rebind shows up without a
//! restart because rows render straight from `KeymapRegistry::binding`
//! every frame), with actions reachable from the *current* context stack
//! highlighted and everything else dimmed.

use eframe::egui;

use super::chord::Platform;
use super::registry::{ActionDef, ContextId, KeymapRegistry};
use crate::theme::{paint, tokens as tk};

/// The overlay's shell-owned state (toggled by the `app.cheatsheet`
/// action; Esc or the close button also dismisses it).
pub struct CheatSheet {
    open: bool,
}

impl CheatSheet {
    /// Starts closed.
    pub fn new() -> CheatSheet {
        CheatSheet { open: false }
    }

    /// The `app.cheatsheet` action target.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Renders the overlay (no-op while closed). `stack` is the same
    /// context stack the dispatcher ran with this frame, it drives the
    /// current-context highlighting.
    pub fn ui(&mut self, ctx: &egui::Context, registry: &KeymapRegistry, stack: &[ContextId]) {
        if !self.open {
            return;
        }
        // Esc dismisses. The dispatcher leaves Esc alone unless a gizmo
        // context claims it (`gizmo.cancel` outranks the overlay then).
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }

        let platform = Platform::current();
        let mut open = self.open;
        // §11: a lighter, glance-and-dismiss overlay, popup elevation,
        // not the modal-weight overlay Preferences/Export use.
        let resp = egui::Window::new("Keyboard Shortcuts")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(crate::floating_frame(
                tk::ELEV_3_POPUP,
                paint::shadow_popup(),
            ))
            .show(ctx, |ui| {
                if let Some(innermost) = stack.last() {
                    ui.weak(format!("Context: {}", innermost.0));
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for category in categories(registry) {
                            ui.add_space(4.0);
                            ui.strong(category);
                            egui::Grid::new(format!("cheatsheet-{category}"))
                                .num_columns(2)
                                .min_col_width(160.0)
                                .show(ui, |ui| {
                                    for def in
                                        registry.defs().iter().filter(|d| d.category == category)
                                    {
                                        let binding = registry
                                            .binding(def.id)
                                            .map(|c| c.display(platform))
                                            .unwrap_or_else(|| "—".to_owned());
                                        if is_active(def, stack) {
                                            ui.label(def.label);
                                            ui.monospace(binding);
                                        } else {
                                            // Not reachable from the current
                                            // context stack: dimmed.
                                            ui.weak(def.label);
                                            ui.weak(binding);
                                        }
                                        ui.end_row();
                                    }
                                });
                        }
                    });
            });
        crate::finish_floating_window(ctx, &resp);
        self.open = open;
    }
}

impl Default for CheatSheet {
    fn default() -> Self {
        CheatSheet::new()
    }
}

/// Categories in first-registration order (stable rows for muscle memory).
/// Shared with the Phase-G prefs panel's Keyboard section (the D5 rebind
/// editor renders the same category grouping this sheet does).
pub(crate) fn categories(registry: &KeymapRegistry) -> Vec<&'static str> {
    let mut seen = Vec::new();
    for def in registry.defs() {
        if !seen.contains(&def.category) {
            seen.push(def.category);
        }
    }
    seen
}

/// Current-context highlighting: is any of the action's contexts on the
/// live stack right now?
fn is_active(def: &ActionDef, stack: &[ContextId]) -> bool {
    def.contexts.iter().any(|c| stack.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{
        default_registry, Chord, CTX_APP, CTX_EDITOR, CTX_GIZMO, CTX_LOUPE, NAV_NEXT,
    };
    use eframe::egui::Key;
    use egui_kittest::{kittest::Queryable, Harness};

    /// Context highlighting logic (the visual half is weak-vs-normal text,
    /// which AccessKit doesn't expose, asserted here at the logic seam).
    #[test]
    fn highlight_follows_the_context_stack() {
        let r = default_registry();
        let nav = r.def(NAV_NEXT).unwrap();
        let zoom = r.def(crate::keymap::VIEW_ZOOM_TOGGLE).unwrap();
        let gizmo = r.def(crate::keymap::GIZMO_CANCEL).unwrap();

        let editor_only = [CTX_APP, CTX_EDITOR];
        assert!(is_active(nav, &editor_only));
        assert!(!is_active(zoom, &editor_only), "no loupe on the stack");
        assert!(!is_active(gizmo, &editor_only));

        let in_loupe = [CTX_APP, CTX_EDITOR, CTX_LOUPE];
        assert!(is_active(zoom, &in_loupe));

        let in_gizmo = [CTX_APP, CTX_EDITOR, CTX_LOUPE, CTX_GIZMO];
        assert!(is_active(gizmo, &in_gizmo));
    }

    struct App {
        registry: KeymapRegistry,
        sheet: CheatSheet,
    }

    fn harness() -> Harness<'static, App> {
        let mut app = App {
            registry: default_registry(),
            sheet: CheatSheet::new(),
        };
        app.sheet.toggle(); // open

        // The overlay's window frame now resolves a NAMED weight-role font
        // family (`floating_frame`'s corner radius/border don't, but the
        // window title bar's default `TextStyle::Heading` does), wrap the
        // closure in `theme::test_support::themed_state` so the theme
        // (fonts included) is installed before anything paints. See that
        // module's doc comment: a bare kittest `Context` panics on the
        // first named-family layout, and `run`/`step` must be called at
        // least once before querying AccessKit (frame 0 installs and
        // paints nothing).
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, app: &mut App| {
                let stack = [CTX_APP, CTX_EDITOR, CTX_LOUPE];
                app.sheet.ui(ui.ctx(), &app.registry, &stack);
            }),
            app,
        );
        harness.set_size(eframe::egui::vec2(720.0, 560.0));
        harness
    }

    /// D4 AC (kittest structural snapshot): the overlay exists with its
    /// category groups, action rows, live binding strings, and the
    /// current-context readout.
    #[test]
    fn overlay_structure_groups_by_category_with_live_bindings() {
        let mut harness = harness();
        harness.run();

        harness.get_by_label("Keyboard Shortcuts");
        harness.get_by_label_contains("Context: editor.loupe");
        for category in [
            "Application",
            "Edit",
            "Navigation",
            "View",
            "Workspace",
            "Tools",
        ] {
            harness.get_by_label(category);
        }
        harness.get_by_label("Next photo");
        harness.get_by_label("Undo");
        harness.get_by_label("Toggle Fit / 100%");
        // Live binding strings, platform-correct.
        let platform = Platform::current();
        harness.get_by_label(&Chord::plain(Key::ArrowRight).display(platform));
        harness.get_by_label(&Chord::cmd_shift(Key::Z).display(platform));
    }

    /// D4 AC: a rebind is reflected WITHOUT restart, same harness, same
    /// sheet, next frame shows the override.
    #[test]
    fn a_rebind_shows_up_without_restart() {
        let mut harness = harness();
        harness.run();
        let platform = Platform::current();
        let old = Chord::plain(Key::ArrowRight).display(platform);
        let new = Chord::plain(Key::N).display(platform);
        harness.get_by_label(&old);
        assert_eq!(harness.query_all_by_label(&new).count(), 0);

        harness
            .state_mut()
            .registry
            .rebind(NAV_NEXT, Some(Chord::plain(Key::N)))
            .unwrap();
        harness.run();

        harness.get_by_label(&new);
        assert_eq!(
            harness.query_all_by_label(&old).count(),
            0,
            "the default binding is no longer shown for nav.next"
        );
    }

    /// H1 AC: **every** `ActionDef.label` in the registry is reachable as
    /// an AccessKit accessible name, the cheat sheet renders one row per
    /// registered action, so the sweep proves the whole M1 action set (and
    /// will automatically cover E10/E11/E12 registrations later).
    #[test]
    fn every_action_label_is_reachable_as_an_accessible_name() {
        let mut harness = harness();
        harness.run();
        let labels: Vec<&'static str> = harness
            .state()
            .registry
            .defs()
            .iter()
            .map(|d| d.label)
            .collect();
        assert!(!labels.is_empty());
        for label in labels {
            harness.get_by_label(label);
        }
    }

    /// Esc dismisses the overlay.
    #[test]
    fn escape_closes_the_overlay() {
        let mut harness = harness();
        harness.run();
        harness.get_by_label("Keyboard Shortcuts");

        harness.key_press(Key::Escape);
        harness.run();
        assert_eq!(harness.query_all_by_label("Keyboard Shortcuts").count(), 0);
    }
}
