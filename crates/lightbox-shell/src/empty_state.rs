// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The empty-state drop zone (E08 spec §2.1 item 2, task A1): with no
//! working set, the window is a full-bleed drop target that reflects
//! `hovered_files` *before* the drop lands (highlight + count). A free
//! function (not a method on `LightboxApp`) so it is directly testable with
//! `egui_kittest::Harness::new_ui` — no `Session`/GPU needed.

use eframe::egui;

use crate::intake::HoverAffordance;

/// Renders the empty-state drop zone into the current UI's available rect.
pub fn empty_state_ui(ui: &mut egui::Ui, hover: Option<HoverAffordance>) {
    let rect = ui.available_rect_before_wrap();
    let hovering = hover.is_some();
    let visuals = ui.visuals();
    let stroke_color = if hovering {
        visuals.selection.stroke.color
    } else {
        visuals.weak_text_color()
    };
    ui.painter().rect_stroke(
        rect.shrink(12.0),
        8.0,
        egui::Stroke::new(if hovering { 2.5 } else { 1.0 }, stroke_color),
        egui::StrokeKind::Inside,
    );
    if hovering {
        ui.painter().rect_filled(
            rect.shrink(12.0),
            8.0,
            visuals.selection.bg_fill.gamma_multiply(0.15),
        );
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.centered_and_justified(|ui| {
            let text = match hover {
                Some(HoverAffordance { count: Some(n) }) => {
                    format!("Drop {n} photo{} to open", if n == 1 { "" } else { "s" })
                }
                Some(HoverAffordance { count: None }) => "Drop to open".to_owned(),
                None => "Drop photos or a folder to start editing\n— or use Open Files… / Open Folder… / Browse Folders… above".to_owned(),
            };
            ui.label(egui::RichText::new(text).size(16.0).color(stroke_color));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    /// A1 AC: the empty state, with no hover, invites the three intake
    /// affordances (drop / dialog / browse).
    #[test]
    fn no_hover_shows_the_generic_invitation() {
        let mut harness = Harness::new_ui(|ui| {
            empty_state_ui(ui, None);
        });
        harness.run();
        harness.get_by_label_contains("Drop photos or a folder");
    }

    /// A1 AC: `hovered_files` with a known count highlights + shows the
    /// count *before* the drop lands.
    #[test]
    fn hover_with_known_count_shows_the_count() {
        let mut harness = Harness::new_ui(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: Some(3) }));
        });
        harness.run();
        harness.get_by_label_contains("Drop 3 photos to open");
    }

    /// A1 AC: a platform that hovers without reporting paths still
    /// highlights (graceful degradation, §6.1/R4).
    #[test]
    fn hover_without_a_known_count_still_shows_a_highlight() {
        let mut harness = Harness::new_ui(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: None }));
        });
        harness.run();
        harness.get_by_label_contains("Drop to open");
    }

    #[test]
    fn singular_count_does_not_pluralize() {
        let mut harness = Harness::new_ui(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: Some(1) }));
        });
        harness.run();
        harness.get_by_label_contains("Drop 1 photo to open");
    }
}
