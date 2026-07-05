// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The virtualized grid MVP (spec T25).
//!
//! Fixed-size cells (size slider in the top bar), **only visible rows are
//! materialized**: the scroll area reports its viewport, the row range is
//! computed from cell geometry, and exactly those cells query the thumbnail
//! cache — which issues demand-driven [`PreviewProvider`] requests and
//! cancels them on scroll-out (see [`crate::thumbs`]). Placeholder →
//! thumbnail upgrade-in-place; unsupported/decode-error/missing badges;
//! single + shift-range selection seed. **E08** owns the real Library UX.
//!
//! [`PreviewProvider`]: lightbox_preview::PreviewProvider

use std::collections::HashSet;
use std::ops::Range;

use eframe::egui;
use lightbox_core::ImageSummary;
use lightbox_types::ImageId;

use crate::model::{ClickMods, Selection};
use crate::thumbs::{bucket_for, ThumbCache, ThumbState};

/// Space between cells, logical points.
const SPACING: f32 = 8.0;
/// Height of the filename strip under each cell, logical points.
const LABEL_H: f32 = 18.0;

/// What a grid frame asks the app to do.
#[derive(Debug, PartialEq)]
pub enum GridAction {
    /// Open the loupe on this row index (double-click / Enter / E).
    OpenLoupe(usize),
}

/// Pure cell geometry for one frame (unit-tested — the virtualization
/// arithmetic must not drift with UI tweaks).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GridLayout {
    /// Cells per row (≥ 1).
    pub cols: usize,
    /// Cell edge (image area is `cell`×`cell`), logical points.
    pub cell: f32,
    /// Full row pitch including label + spacing.
    pub row_h: f32,
    /// Total content height for `total` images.
    pub total_h: f32,
}

/// Computes the frame's grid geometry.
pub fn layout(avail_w: f32, cell: f32, total: usize) -> GridLayout {
    let cols = (((avail_w + SPACING) / (cell + SPACING)).floor() as usize).max(1);
    let row_h = cell + LABEL_H + SPACING;
    let rows = total.div_ceil(cols);
    GridLayout {
        cols,
        cell,
        row_h,
        total_h: rows as f32 * row_h,
    }
}

/// Index range of the images intersecting the viewport `[top, bottom)`
/// (relative to the content origin), clamped to `total`.
pub fn visible_indices(l: &GridLayout, total: usize, top: f32, bottom: f32) -> Range<usize> {
    if total == 0 || bottom <= 0.0 {
        return 0..0;
    }
    let first_row = (top / l.row_h).floor().max(0.0) as usize;
    let last_row = (bottom / l.row_h).ceil().max(0.0) as usize; // exclusive
    let start = (first_row * l.cols).min(total);
    let end = (last_row * l.cols).min(total);
    start..end
}

/// Renders the grid; returns an action when the app should switch views.
/// `visible_out` receives the ids materialized this frame (the thumbnail
/// cache cancels everything else in `end_frame`). `forced_scroll_frac`
/// (perf-scroll scripting, T28) pins the scroll offset to that fraction of
/// the scrollable range this frame.
#[allow(clippy::too_many_arguments)]
pub fn grid_ui(
    ui: &mut egui::Ui,
    rows: &[ImageSummary],
    selection: &mut Selection,
    thumbs: &mut ThumbCache,
    cell_size: f32,
    visible_out: &mut HashSet<ImageId>,
    forced_scroll_frac: Option<f32>,
) -> Option<GridAction> {
    let mut action = None;

    // Enter/E on the focused image opens the loupe (T26 entry points) —
    // unless a text field (the import path) owns the keyboard.
    let typing = ui.ctx().egui_wants_keyboard_input();
    let open_key = !typing
        && ui
            .ctx()
            .input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::E));
    if open_key {
        if let Some(idx) = selection
            .focus()
            .and_then(|id| rows.iter().position(|s| s.id == id))
        {
            action = Some(GridAction::OpenLoupe(idx));
        }
    }

    if rows.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No images yet — import a folder from the bar above.");
        });
        return action;
    }

    let ppp = ui.ctx().pixels_per_point();
    let bucket = bucket_for((cell_size * ppp).ceil() as u32);

    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink(false);
    if let Some(frac) = forced_scroll_frac {
        // Scripted scroll (perf capture): pin the offset to the requested
        // fraction of the scrollable range, estimated from the outer size
        // (the estimate only steers the sweep — virtualization arithmetic
        // still runs off the real viewport inside).
        let l = layout(ui.available_width(), cell_size, rows.len());
        let range = (l.total_h - ui.available_height()).max(0.0);
        scroll_area = scroll_area.vertical_scroll_offset(frac.clamp(0.0, 1.0) * range);
    }
    scroll_area.show_viewport(ui, |ui, viewport| {
        let l = layout(ui.available_width(), cell_size, rows.len());
        ui.set_height(l.total_h);
        let origin = ui.min_rect().min;

        let range = visible_indices(&l, rows.len(), viewport.min.y, viewport.max.y);
        for idx in range {
            let summary = &rows[idx];
            visible_out.insert(summary.id);
            thumbs.want(summary.id, bucket);

            let (row, col) = (idx / l.cols, idx % l.cols);
            let cell_rect = egui::Rect::from_min_size(
                origin + egui::vec2(col as f32 * (l.cell + SPACING), row as f32 * l.row_h),
                egui::vec2(l.cell, l.cell + LABEL_H),
            );

            let response = ui.interact(
                cell_rect,
                ui.id().with(("grid-cell", summary.id.0)),
                egui::Sense::click(),
            );
            if response.double_clicked() {
                selection.click(rows, idx, ClickMods::default());
                action = Some(GridAction::OpenLoupe(idx));
            } else if response.clicked() {
                let mods = ui.ctx().input(|i| ClickMods {
                    shift: i.modifiers.shift,
                    command: i.modifiers.command,
                });
                selection.click(rows, idx, mods);
            }

            draw_cell(ui, cell_rect, summary, selection, thumbs, l.cell, &response);
        }
    });

    action
}

/// Paints one cell: thumbnail (or placeholder), selection, label, badges.
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    ui: &egui::Ui,
    cell_rect: egui::Rect,
    summary: &ImageSummary,
    selection: &Selection,
    thumbs: &mut ThumbCache,
    cell: f32,
    response: &egui::Response,
) {
    let painter = ui.painter();
    let selected = selection.is_selected(summary.id);
    let visuals = ui.visuals();

    let bg = if selected {
        visuals.selection.bg_fill
    } else {
        visuals.extreme_bg_color
    };
    painter.rect_filled(cell_rect, 3.0, bg);
    if selection.focus() == Some(summary.id) {
        painter.rect_stroke(
            cell_rect,
            3.0,
            visuals.selection.stroke,
            egui::StrokeKind::Outside,
        );
    }

    let image_rect = egui::Rect::from_min_size(cell_rect.min, egui::vec2(cell, cell)).shrink(3.0);
    let mut badge: Option<(&str, egui::Color32)> = None;
    match thumbs.state(summary.id) {
        ThumbState::Ready(tex, size) => {
            let fitted = fit_rect(image_rect, [size[0] as f32, size[1] as f32]);
            painter.image(
                tex.id(),
                fitted,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        ThumbState::Pending => {
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(16.0),
                visuals.weak_text_color(),
            );
        }
        ThumbState::Failed(reason) => {
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "no preview",
                egui::FontId::proportional(11.0),
                visuals.weak_text_color(),
            );
            response.clone().on_hover_text(format!("preview: {reason}"));
        }
    }

    // Badges (spec T25: unsupported/decode-error; missing for completeness).
    if summary.decode_error {
        badge = Some(("!", egui::Color32::from_rgb(200, 60, 50)));
    } else if summary.missing {
        badge = Some(("?", egui::Color32::from_rgb(200, 150, 40)));
    } else if summary.width == 0 && summary.height == 0 {
        // Catalogued but unreadable dims — the UNSUPPORTED badge case.
        badge = Some(("∅", egui::Color32::from_rgb(120, 120, 130)));
    }
    if let Some((text, color)) = badge {
        let c = egui::pos2(image_rect.right() - 9.0, image_rect.top() + 9.0);
        painter.circle_filled(c, 8.0, color);
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
    }

    // Rating stars, bottom-left of the image area.
    if let Some(rating) = summary.rating {
        painter.text(
            egui::pos2(image_rect.left() + 2.0, image_rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            "★".repeat(rating as usize),
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgb(235, 200, 80),
        );
    }

    // Filename strip.
    let label_pos = egui::pos2(cell_rect.center().x, cell_rect.bottom() - 2.0);
    painter.text(
        label_pos,
        egui::Align2::CENTER_BOTTOM,
        &summary.filename,
        egui::FontId::proportional(11.0),
        if selected {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        },
    );
}

/// Largest aspect-preserving rect for `size` centered in `container`
/// (never upscales — thumbnails smaller than the cell draw 1:1).
pub fn fit_rect(container: egui::Rect, size: [f32; 2]) -> egui::Rect {
    let scale = (container.width() / size[0])
        .min(container.height() / size[1])
        .min(1.0);
    let fitted = egui::vec2(size[0] * scale, size[1] * scale);
    egui::Rect::from_center_size(container.center(), fitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_never_yields_zero_columns() {
        let l = layout(10.0, 144.0, 100);
        assert_eq!(l.cols, 1, "narrow window still lays out one column");
        assert!(l.total_h > 0.0);
    }

    #[test]
    fn layout_columns_match_available_width() {
        // 4 cells of 144 + spacing fit in 640.
        let l = layout(640.0, 144.0, 100);
        assert_eq!(l.cols, 4);
        let rows = 100usize.div_ceil(4);
        assert!((l.total_h - rows as f32 * l.row_h).abs() < f32::EPSILON);
    }

    #[test]
    fn visible_indices_window_the_scroll_position() {
        let l = layout(640.0, 144.0, 1000); // 4 cols
                                            // Viewport at the very top, two rows tall.
        let r = visible_indices(&l, 1000, 0.0, 2.0 * l.row_h);
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 8, "two rows of four");

        // Mid-scroll: rows 10..13 (bottom edge inside row 12 pulls it in).
        let r = visible_indices(&l, 1000, 10.0 * l.row_h, 12.5 * l.row_h);
        assert_eq!(r.start, 40);
        assert_eq!(r.end, 52);

        // Visible-count bound (T25 AC): the window materializes exactly the
        // intersecting rows, never the whole model.
        assert!(r.len() <= 3 * l.cols);
    }

    #[test]
    fn visible_indices_clamp_at_the_end() {
        let l = layout(640.0, 144.0, 10); // 4 cols, 3 rows
        let r = visible_indices(&l, 10, 0.0, 100.0 * l.row_h);
        assert_eq!(r, 0..10, "clamped to the model length");
        let r = visible_indices(&l, 0, 0.0, 100.0);
        assert!(r.is_empty());
        // Scrolled above the content (elastic overscroll): empty tail guard.
        let r = visible_indices(&l, 10, -50.0, -10.0);
        assert!(r.is_empty());
    }

    #[test]
    fn fit_rect_preserves_aspect_and_never_upscales() {
        let container = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // Landscape downscale.
        let r = fit_rect(container, [200.0, 100.0]);
        assert!((r.width() - 100.0).abs() < 0.01);
        assert!((r.height() - 50.0).abs() < 0.01);
        // Small image: 1:1, centered.
        let r = fit_rect(container, [10.0, 20.0]);
        assert!((r.width() - 10.0).abs() < 0.01);
        assert!((r.height() - 20.0).abs() < 0.01);
        assert_eq!(r.center(), container.center());
    }
}
