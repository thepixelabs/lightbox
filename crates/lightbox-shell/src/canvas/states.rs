// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Canvas states & the degraded-device notice (E08 spec §6.4/§7, task C5):
//! Failed/missing placards, loading shimmer for never-rendered entries, and
//! a non-modal `DeviceDegraded` chip. Every function here paints directly
//! (no `egui::Window`/`Area`/focus-taking widget) — by construction none of
//! this can steal keyboard focus, and the chip is a small corner badge that
//! never covers the canvas's own click/drag `Sense` area (C5 AC).
//!
//! **Failed vs. missing (mapping note).** The spec's §6.4/§7 canvas-states
//! table anticipates a "missing" condition distinct from a decode/probe
//! failure. E04's shipped `WorkingSetItem`/`ItemState` only exposes
//! `Failed` (a free-text `decode_error`, no separate missing-file variant)
//! for the SOURCE-level failure — so [`CanvasPlacard::SourceFailed`] covers
//! it (any reason text the loader reported, including "no such file"-style
//! ones). §7's "missing file (deleted mid-session) → placard + badge on
//! next render failure" is instead exactly [`CanvasPlacard::RenderFailed`]:
//! the entry itself IS `Ready` (never failed to open), but the render
//! attempt fails and nothing has ever been composited for it yet — the
//! same condition AFTER a good frame already exists is instead the
//! stale-frame + error chip (`view.rs`'s `ready_ui`), not a placard.

use eframe::egui;

/// A full-canvas takeover — nothing else is composited this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasPlacard {
    /// Registration/hash still pending (`ItemState::Planned`).
    Loading,
    /// The working-set entry itself failed to open/decode/register — never
    /// reached the canvas as a renderable image.
    SourceFailed {
        /// Human-readable failure reason (never hidden — spec §7).
        reason: String,
    },
    /// The entry is `Ready` but NOTHING has ever been composited for it
    /// (no tier preview, no engine frame) and the latest render attempt
    /// also failed — see the module docs' "missing" mapping note.
    RenderFailed {
        /// Human-readable failure reason.
        reason: String,
    },
    /// Collapsed into an earlier identical-content item.
    Duplicate {
        /// 0-based index of the earlier item.
        of: usize,
    },
}

/// Paints the full-canvas placard for `state` in `rect`.
pub fn placard_ui(ui: &egui::Ui, rect: egui::Rect, state: &CanvasPlacard) {
    match state {
        CanvasPlacard::Loading => loading_shimmer(ui, rect),
        CanvasPlacard::SourceFailed { reason } => {
            text_placard(ui, rect, "!", &format!("failed to open: {reason}"), true);
        }
        CanvasPlacard::RenderFailed { reason } => {
            text_placard(ui, rect, "!", &format!("render failed: {reason}"), true);
        }
        CanvasPlacard::Duplicate { of } => text_placard(
            ui,
            rect,
            "\u{2261}",
            &format!("duplicate of item #{}", of + 1),
            false,
        ),
    }
}

/// The loading shimmer for never-rendered entries — a slow luminance
/// pulse, matching the filmstrip's (`filmstrip::draw_shimmer`) so the two
/// loading affordances read as one visual language.
pub fn loading_shimmer(ui: &egui::Ui, rect: egui::Rect) {
    let t = ui.input(|i| i.time);
    let pulse = ((t * std::f64::consts::TAU / 1.4).sin() * 0.5 + 0.5) as f32;
    let visuals = ui.visuals();
    ui.painter().rect_filled(
        rect,
        4.0,
        visuals
            .weak_text_color()
            .gamma_multiply(0.08 + 0.10 * pulse),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "loading…",
        egui::FontId::proportional(13.0),
        visuals.weak_text_color(),
    );
    label_region(ui, rect, "canvas-loading", "loading");
}

fn text_placard(ui: &egui::Ui, rect: egui::Rect, glyph: &str, message: &str, is_error: bool) {
    let visuals = ui.visuals();
    let color = if is_error {
        egui::Color32::from_rgb(200, 60, 50)
    } else {
        visuals.weak_text_color()
    };
    ui.painter().text(
        rect.center() - egui::vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(22.0),
        color,
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(13.0),
        visuals.weak_text_color(),
    );
    label_region(ui, rect, "canvas-placard", message);
}

/// A non-modal corner chip (the stale-frame render error, or the C5
/// `DeviceDegraded` notice) — painted, never an interactive/focus-taking
/// widget. `anchor_index` stacks multiple chips (0 = closest to the
/// bottom, 1 = above it, …) so the render-error chip and the
/// device-degraded chip can coexist without overlapping.
pub fn chip_ui(ui: &egui::Ui, view_rect: egui::Rect, anchor_index: usize, text: &str, error: bool) {
    let color = if error {
        egui::Color32::from_rgb(220, 90, 70)
    } else {
        egui::Color32::from_rgb(230, 170, 40)
    };
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
    let pad = egui::vec2(10.0, 6.0);
    let size = galley.size() + pad * 2.0;
    let y = view_rect.bottom() - 8.0 - (size.y + 6.0) * anchor_index as f32;
    let pos = egui::pos2(view_rect.right() - size.x - 8.0, y - size.y);
    let rect = egui::Rect::from_min_size(pos, size);
    let painter = ui.painter().with_clip_rect(view_rect);
    painter.rect_filled(rect, 4.0, color.gamma_multiply(0.85));
    painter.galley(rect.min + pad, galley, egui::Color32::WHITE);
    label_region(ui, rect, "canvas-chip", text);
}

/// A hover-only interact region purely so kittest/AccessKit can see the
/// state (`Sense::hover()` never claims keyboard focus and never competes
/// with the canvas's own click/drag `Sense` beneath it — C5 AC: "without
/// stealing focus or blocking input").
fn label_region(ui: &egui::Ui, rect: egui::Rect, salt: &str, label: &str) {
    let response = ui.interact(rect, ui.id().with((salt, label)), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    fn harness_for(paint: impl Fn(&mut egui::Ui) + 'static) -> Harness<'static, ()> {
        let mut harness = Harness::new_ui_state(move |ui, _state: &mut ()| paint(ui), ());
        harness.set_size(egui::vec2(400.0, 300.0));
        harness
    }

    #[test]
    fn loading_shimmer_is_labeled() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            loading_shimmer(ui, rect);
        });
        harness.run();
        harness.get_by_label_contains("loading");
    }

    #[test]
    fn source_failed_placard_shows_the_reason() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            placard_ui(
                ui,
                rect,
                &CanvasPlacard::SourceFailed {
                    reason: "unsupported codec".to_owned(),
                },
            );
        });
        harness.run();
        harness.get_by_label_contains("unsupported codec");
    }

    #[test]
    fn render_failed_placard_shows_the_reason() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            placard_ui(
                ui,
                rect,
                &CanvasPlacard::RenderFailed {
                    reason: "device lost".to_owned(),
                },
            );
        });
        harness.run();
        harness.get_by_label_contains("device lost");
    }

    #[test]
    fn duplicate_placard_is_one_based() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            placard_ui(ui, rect, &CanvasPlacard::Duplicate { of: 4 });
        });
        harness.run();
        harness.get_by_label_contains("duplicate of item #5");
    }

    /// C5 AC: an injected `DeviceDegraded` chip is queryable structurally
    /// and never steals keyboard focus.
    #[test]
    fn device_degraded_chip_shows_without_stealing_focus() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            chip_ui(ui, rect, 1, "GPU device degraded: driver reset", false);
        });
        harness.run();
        harness.get_by_label_contains("GPU device degraded");
        assert!(
            harness.ctx.memory(|m| m.focused()).is_none(),
            "a non-modal chip must never steal keyboard focus"
        );
    }

    /// C5 AC: the chip does not block input to whatever's underneath it —
    /// a click on the (much larger) canvas hit-area beneath the chip still
    /// registers.
    #[test]
    fn chip_does_not_block_a_click_elsewhere_on_the_canvas() {
        let mut harness = Harness::new_ui_state(
            |ui, clicked: &mut bool| {
                let rect = ui.available_rect_before_wrap();
                let response = ui.interact(
                    rect,
                    ui.id().with("canvas-under-test"),
                    egui::Sense::click(),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "canvas-hit-area")
                });
                if response.clicked() {
                    *clicked = true;
                }
                chip_ui(ui, rect, 0, "render failed: timeout", true);
            },
            false,
        );
        harness.set_size(egui::vec2(400.0, 300.0));
        harness.run();
        harness.get_by_label("canvas-hit-area").click();
        harness.run();
        assert!(
            *harness.state(),
            "a click on the canvas (center, away from the corner chip) must still register"
        );
    }
}
