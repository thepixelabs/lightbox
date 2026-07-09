// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! D2 — the per-frame dispatcher (spec §6.8).
//!
//! [`dispatch`] runs at the **top of the frame, before any widget is
//! built**: matched key events are *consumed* (removed from
//! `InputState::events`), so no widget ever double-handles a chord —
//! that's what "dispatch before widget input" means in immediate mode.
//!
//! **Text-input suppression.** While a text field owns the keyboard
//! (`Context::egui_wants_keyboard_input`, the same guard the pre-Phase-D
//! per-widget handlers used — formalized here), chords **without** a hard
//! modifier are not dispatched *and not consumed*: the keystroke belongs
//! to the text field (typing "z" in a value box never toggles zoom).
//! Chords carrying ⌘/Ctrl (or raw macOS Control) still dispatch — ⌘Z in a
//! text box reaches `edit.undo`, the desktop-editor convention.
//!
//! **Key-repeat.** OS auto-repeat re-fires actions whose
//! [`super::ActionDef::repeatable`] is true (one action per repeat event —
//! holding → keeps stepping `nav.next`). A repeat of a *non-repeatable*
//! action's chord is consumed but not re-fired (holding Z toggles zoom
//! exactly once, and the "z…zzz" never leaks into some widget).

use eframe::egui;

use super::chord::Chord;
use super::registry::{ActionId, ContextId, KeymapRegistry};

/// Resolves this frame's key events against the registry and context
/// stack. Returns the actions to run, in input order; matched events are
/// consumed. Call once per frame, before building any UI.
pub fn dispatch(
    ctx: &egui::Context,
    registry: &KeymapRegistry,
    stack: &[ContextId],
) -> Vec<ActionId> {
    let text_owns_keyboard = ctx.egui_wants_keyboard_input();
    let mut fired = Vec::new();
    ctx.input_mut(|input| {
        input.events.retain(|ev| {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = ev
            else {
                return true; // not a key-down: none of our business
            };
            let chord = Chord::from_input(*modifiers, *key);
            if text_owns_keyboard && !chord.has_primary_modifier() {
                return true; // suppressed: the text field keeps the keystroke
            }
            let Some(id) = registry.resolve(chord, stack) else {
                return true; // unbound: leave it for widgets
            };
            let repeatable = registry.def(id).is_some_and(|d| d.repeatable);
            if !*repeat || repeatable {
                fired.push(id);
            }
            false // consumed either way — the chord is claimed
        });
    });
    fired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{
        default_registry, CTX_APP, CTX_EDITOR, CTX_LOUPE, NAV_NEXT, VIEW_ZOOM_TOGGLE,
    };
    use eframe::egui::{Event, Key, Modifiers};
    use egui_kittest::Harness;

    /// A minimal app that dispatches at the top of the frame (mirroring
    /// `lib.rs`) and then builds one text box — the D2 AC surface.
    struct App {
        registry: KeymapRegistry,
        fired: Vec<ActionId>,
        text: String,
        want_text_focus: bool,
    }

    fn harness(want_text_focus: bool) -> Harness<'static, App> {
        Harness::new_ui_state(
            |ui, app: &mut App| {
                let stack = [CTX_APP, CTX_EDITOR, CTX_LOUPE];
                let fired = dispatch(ui.ctx(), &app.registry, &stack);
                app.fired.extend(fired);
                let response = ui.text_edit_singleline(&mut app.text);
                if app.want_text_focus {
                    response.request_focus();
                }
            },
            App {
                registry: default_registry(),
                fired: Vec::new(),
                text: String::new(),
                want_text_focus,
            },
        )
    }

    fn count(app: &App, id: ActionId) -> usize {
        app.fired.iter().filter(|f| **f == id).count()
    }

    /// D2 AC: typing "z" in a text box does NOT toggle zoom — the
    /// keystroke stays with the text field.
    #[test]
    fn typing_z_in_a_text_box_never_toggles_zoom() {
        let mut harness = harness(true);
        harness.run(); // focus lands; egui_wants_keyboard_input is true next frame

        harness.key_press(Key::Z);
        harness.event(Event::Text("z".to_owned()));
        harness.run();

        assert_eq!(
            count(harness.state(), VIEW_ZOOM_TOGGLE),
            0,
            "text focus must suppress the bare-key chord"
        );
        assert_eq!(
            harness.state().text,
            "z",
            "the text field got the keystroke"
        );
    }

    /// The control case for the suppression test: with no text focus the
    /// same key IS the zoom chord.
    #[test]
    fn z_without_text_focus_toggles_zoom() {
        let mut harness = harness(false);
        harness.run();

        harness.key_press(Key::Z);
        harness.run();

        assert_eq!(count(harness.state(), VIEW_ZOOM_TOGGLE), 1);
        assert_eq!(
            harness.state().text,
            "",
            "the consumed chord never leaks into the widget"
        );
    }

    /// Spec §6.8: only NON-modifier chords are suppressed by text focus —
    /// ⌘-chords still dispatch (⌘Z in a value box reaches edit.undo).
    #[test]
    fn command_chords_fire_even_while_a_text_box_has_focus() {
        let mut harness = harness(true);
        harness.run();

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();

        assert_eq!(count(harness.state(), crate::keymap::EDIT_UNDO), 1);
    }

    /// D2 AC: holding a nav key repeats nav.next — each OS key-repeat
    /// event (a `pressed` event for an already-down key) re-fires the
    /// repeatable action.
    #[test]
    fn holding_a_nav_key_repeats_nav_next() {
        let mut harness = harness(false);
        harness.run();

        // First press: key goes down (egui marks later presses as repeats
        // while the key stays down — the same synthesis real OS repeat
        // rides through `InputState::begin_pass`).
        harness.key_down(Key::ArrowRight);
        harness.step();
        // Two auto-repeat events, no release in between.
        harness.key_down(Key::ArrowRight);
        harness.step();
        harness.key_down(Key::ArrowRight);
        harness.step();
        harness.key_up(Key::ArrowRight);
        harness.step();

        assert_eq!(
            count(harness.state(), NAV_NEXT),
            3,
            "one nav.next per press + per repeat"
        );
    }

    /// The counterpart: a NON-repeatable action fires once no matter how
    /// long its key is held (and the repeats are still consumed).
    #[test]
    fn non_repeatable_actions_fire_once_while_held() {
        let mut harness = harness(false);
        harness.run();

        harness.key_down(Key::Z);
        harness.step();
        harness.key_down(Key::Z); // auto-repeat
        harness.step();
        harness.key_up(Key::Z);
        harness.step();

        assert_eq!(count(harness.state(), VIEW_ZOOM_TOGGLE), 1);
    }

    /// Chords bound only in contexts that are NOT on the stack pass
    /// through untouched (Esc = gizmo.cancel, gizmo context not pushed).
    #[test]
    fn chords_for_absent_contexts_pass_through() {
        let mut harness = harness(false);
        harness.run();

        harness.key_press(Key::Escape);
        harness.run();

        assert!(harness.state().fired.is_empty());
    }
}
