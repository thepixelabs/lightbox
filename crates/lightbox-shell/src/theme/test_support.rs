// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Test-only glue for driving themed UI under `egui_kittest`.
//!
//! ## Why this exists
//!
//! [`crate::theme::fonts`] binds the typographic weight roles (Medium,
//! SemiBold) as **named** font families, because epaint resolves one face
//! per family, there is no weight axis, so "Inter SemiBold" has to be its
//! own family. epaint **panics** if text is laid out in a named family that
//! isn't bound on that `Context`:
//!
//! ```text
//! FontFamily::Name("InterSemiBold") is not bound to any fonts
//! ```
//!
//! The shipped app is fine: `theme::install` runs at construction, long
//! before the first frame paints. Tests are the hazard. A kittest harness
//! builds a bare `egui::Context`, and, this is the part that bites
//! **`Harness::new_ui` / `build_ui` invoke the UI closure once during
//! construction**, so there is no gap in which to call `install` on
//! `harness.ctx` before something paints. Worse, `Context::set_fonts` is
//! deferred to the *next* `begin_pass`, so installing from inside the
//! closure doesn't rescue the frame it was called on either.
//!
//! [`themed`] resolves both problems: it installs on the first invocation
//! and paints nothing that frame, so every frame that does paint has the
//! families bound.
//!
//! ```no_run
//! # use egui_kittest::Harness;
//! # fn my_ui(_: &mut eframe::egui::Ui, _: &mut ()) {}
//! let mut harness = Harness::builder()
//!     .with_size(eframe::egui::vec2(320.0, 480.0))
//!     .build_ui_state(crate::theme::test_support::themed_state(my_ui), ());
//! harness.run(); // frame 0 installs, frame 1+ paints
//! ```
//!
//! Note the harness must be stepped (`run`/`step`) before querying the
//! AccessKit tree, frame 0 deliberately produces no widgets.

use eframe::egui;

/// Wraps a stateless kittest UI closure so the theme, fonts included, is
/// installed before anything paints. The first frame installs and draws
/// nothing; later frames run `app` normally.
///
/// It also pins the harness to **Dark**, the app's own default. A bare
/// `Context` leaves `theme_preference` at `System`, so chrome that resolves
/// per theme (`theme::Chrome`) would paint whichever palette the *build
/// machine* happens to prefer, making any pixel assertion pass or fail on
/// the host's appearance setting rather than on the code. A harness that
/// deliberately exercises the light palette calls `set_theme` itself each
/// frame (see `theme::panel_gallery`), which overrides this.
pub fn themed<'a>(mut app: impl FnMut(&mut egui::Ui) + 'a) -> impl FnMut(&mut egui::Ui) + 'a {
    let mut installed = false;
    move |ui| {
        if !installed {
            crate::theme::install(ui.ctx());
            ui.ctx().set_theme(egui::ThemePreference::Dark);
            installed = true;
            // `set_fonts` lands at the next `begin_pass`; painting now
            // would resolve a not-yet-bound named family and panic.
            ui.ctx().request_repaint();
            return;
        }
        app(ui);
    }
}

/// [`themed`] for the `build_ui_state` / `new_ui_state` closure shape.
pub fn themed_state<'a, S>(
    mut app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
) -> impl FnMut(&mut egui::Ui, &mut S) + 'a {
    let mut installed = false;
    move |ui, state| {
        if !installed {
            crate::theme::install(ui.ctx());
            ui.ctx().set_theme(egui::ThemePreference::Dark);
            installed = true;
            ui.ctx().request_repaint();
            return;
        }
        app(ui, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    /// The regression this module exists for: painting text in a weight
    /// role must not panic under a kittest harness, and the widget must
    /// still reach the AccessKit tree once the harness is stepped.
    #[test]
    fn themed_wrapper_binds_named_families_before_paint() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(200.0, 80.0))
            .build_ui(themed(|ui| {
                ui.painter().text(
                    ui.max_rect().left_top(),
                    egui::Align2::LEFT_TOP,
                    "HEADER",
                    crate::theme::fonts::panel_header_font(),
                    crate::theme::tokens::TEXT_PRIMARY,
                );
                ui.label("body");
            }));
        harness.run();
        harness.get_by_label("body");
    }

    /// Same guarantee for the stateful closure shape.
    #[test]
    fn themed_state_wrapper_binds_named_families_before_paint() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(200.0, 80.0))
            .build_ui_state(
                themed_state(|ui: &mut egui::Ui, s: &mut u32| {
                    *s += 1;
                    ui.painter().text(
                        ui.max_rect().left_top(),
                        egui::Align2::LEFT_TOP,
                        "SECTION",
                        crate::theme::fonts::section_label_font(),
                        crate::theme::tokens::TEXT_TERTIARY,
                    );
                    ui.label("stateful");
                }),
                0u32,
            );
        harness.run();
        harness.get_by_label("stateful");
        assert!(
            *harness.state() > 0,
            "app closure must run on the frames after the install frame"
        );
    }
}
