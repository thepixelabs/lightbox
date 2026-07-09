// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The session filmstrip (E08 spec §6.3) — repurposes the retired E01
//! grid's virtualization math turned 90° (spec §3 crate map: "`grid.rs` →
//! `filmstrip.rs`, horizontal repurpose").
//!
//! **Phase-A scope note (recorded in `E08-deviations.md`):** full §6.3 is a
//! Phase B deliverable (B1 strip geometry, B2 `filmstrip_ui`, B3 badges, B4
//! nav/multi-select, B5 resize/polish). A1's chassis AC needs a working
//! "bottom filmstrip strut" and A7's smoke AC needs a "filmstrip row →
//! canvas seam" proof, so this phase ships the *geometry* (B1) and a
//! *minimal* `filmstrip_ui` (the working part of B2: virtualized cells,
//! demand-driven thumbs via the unchanged [`ThumbCache`], active-ring
//! highlight, click-to-activate, Loading/Failed placeholders, filename
//! label) — deliberately without B3's RAW/edited-dot/error-badge chrome,
//! B4's multi-select modifiers, or B5's drag-resize/collapse/overflow
//! indicator. Phase B should extend this module rather than re-author it.

use std::collections::HashSet;
use std::ops::Range;

use eframe::egui;
use lightbox_core::{ItemState, WorkingSetItem};
use lightbox_types::ImageId;

use crate::thumbs::{bucket_for, ThumbCache, ThumbState};

/// Space between cells, logical points.
const SPACING: f32 = 6.0;
/// Height of the filename strip under each cell, logical points.
const LABEL_H: f32 = 14.0;

/// What a filmstrip frame asks the app to do.
#[derive(Debug, PartialEq)]
pub enum FilmstripAction {
    /// Activate this entry (click) — the loupe should now show it.
    Activate(usize),
}

/// Pure cell geometry for one frame (unit-tested like the retired grid's
/// `layout` was) — the horizontal axis' equivalent of `GridLayout`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StripLayout {
    /// Cell edge (image area is `cell`×`cell`), logical points.
    pub cell: f32,
    /// Horizontal pitch including inter-cell spacing.
    pub cell_w: f32,
    /// Total scrollable content width for `total` entries.
    pub total_w: f32,
}

/// Computes one frame's strip geometry.
pub fn strip_layout(cell: f32, total: usize) -> StripLayout {
    let cell_w = cell + SPACING;
    StripLayout {
        cell,
        cell_w,
        total_w: total as f32 * cell_w,
    }
}

/// Index range of the entries intersecting the horizontal viewport
/// `[left, right)`, clamped to `total`.
pub fn visible_range(l: &StripLayout, total: usize, left: f32, right: f32) -> Range<usize> {
    if total == 0 || right <= 0.0 || l.cell_w <= 0.0 {
        return 0..0;
    }
    let first = (left / l.cell_w).floor().max(0.0) as usize;
    let last = (right / l.cell_w).ceil().max(0.0) as usize;
    (first.min(total))..(last.min(total))
}

/// Renders the strip; reuses [`ThumbCache`] verbatim (`want`/cancel-on-
/// scroll-out is driven by the caller's `end_frame`, exactly like the
/// retired grid). `visible_out` receives the `ImageId`s materialized this
/// frame.
pub fn filmstrip_ui(
    ui: &mut egui::Ui,
    entries: &[WorkingSetItem],
    active: Option<usize>,
    thumbs: &mut ThumbCache,
    cell_size: f32,
    visible_out: &mut HashSet<ImageId>,
) -> Option<FilmstripAction> {
    let mut action = None;
    if entries.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.weak("no images open");
        });
        return None;
    }

    let ppp = ui.ctx().pixels_per_point();
    let bucket = bucket_for((cell_size * ppp).ceil() as u32);

    egui::ScrollArea::horizontal()
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            let l = strip_layout(cell_size, entries.len());
            ui.set_width(l.total_w);
            let origin = ui.min_rect().min;

            let range = visible_range(&l, entries.len(), viewport.min.x, viewport.max.x);
            for idx in range {
                let entry = &entries[idx];
                if let ItemState::Ready { image, .. } = entry.state {
                    visible_out.insert(image);
                    thumbs.want(image, bucket);
                }

                let cell_rect = egui::Rect::from_min_size(
                    origin + egui::vec2(idx as f32 * l.cell_w, 0.0),
                    egui::vec2(l.cell, l.cell + LABEL_H),
                );
                let response = ui.interact(
                    cell_rect,
                    ui.id().with(("filmstrip-cell", idx)),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    action = Some(FilmstripAction::Activate(idx));
                }

                draw_cell(
                    ui,
                    cell_rect,
                    entry,
                    active == Some(idx),
                    thumbs,
                    l.cell,
                    &response,
                );
            }
        });

    action
}

/// Paints one cell: active ring, thumbnail (or Loading/Failed placeholder),
/// filename label. No RAW/edited/error badges yet (Phase B, §6.3).
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    ui: &egui::Ui,
    cell_rect: egui::Rect,
    entry: &WorkingSetItem,
    is_active: bool,
    thumbs: &mut ThumbCache,
    cell: f32,
    response: &egui::Response,
) {
    let painter = ui.painter();
    let visuals = ui.visuals();

    painter.rect_filled(cell_rect, 3.0, visuals.extreme_bg_color);
    if is_active {
        painter.rect_stroke(
            cell_rect,
            3.0,
            visuals.selection.stroke,
            egui::StrokeKind::Outside,
        );
    }

    let image_rect = egui::Rect::from_min_size(cell_rect.min, egui::vec2(cell, cell)).shrink(3.0);
    match entry.state {
        ItemState::Ready { image, .. } => match thumbs.state(image) {
            ThumbState::Ready(tex, size) => {
                let fitted = fit_rect(image_rect, [size[0] as f32, size[1] as f32]);
                painter.image(
                    tex.id(),
                    fitted,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            ThumbState::Pending => draw_placeholder(painter, image_rect, "…", visuals),
            ThumbState::Failed(reason) => {
                draw_placeholder(painter, image_rect, "no preview", visuals);
                response.clone().on_hover_text(format!("preview: {reason}"));
            }
        },
        ItemState::Planned => draw_placeholder(painter, image_rect, "…", visuals),
        ItemState::Failed => {
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "!",
                egui::FontId::proportional(16.0),
                egui::Color32::from_rgb(200, 60, 50),
            );
            let reason = entry.decode_error.as_deref().unwrap_or("open failed");
            response.clone().on_hover_text(reason.to_owned());
        }
        ItemState::DuplicateOf { .. } => draw_placeholder(painter, image_rect, "dup", visuals),
    }

    let label_pos = egui::pos2(cell_rect.center().x, cell_rect.bottom() - 1.0);
    painter.text(
        label_pos,
        egui::Align2::CENTER_BOTTOM,
        &entry.filename,
        egui::FontId::proportional(10.0),
        visuals.text_color(),
    );
}

fn draw_placeholder(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    visuals: &egui::Visuals,
) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        visuals.weak_text_color(),
    );
}

/// Largest aspect-preserving rect for `size` centered in `container` (never
/// upscales). Shared with the loupe's fit-zoom.
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
    fn strip_layout_pitch_and_total_width() {
        let l = strip_layout(100.0, 10);
        assert_eq!(l.cell_w, 106.0);
        assert_eq!(l.total_w, 1060.0);
    }

    #[test]
    fn visible_range_windows_the_scroll_position() {
        let l = strip_layout(100.0, 1000); // pitch 106
        let r = visible_range(&l, 1000, 0.0, 500.0);
        assert_eq!(r.start, 0);
        assert!(r.end >= 4 && r.end <= 6, "roughly 4-5 cells visible: {r:?}");

        let r2 = visible_range(&l, 1000, 1000.0 * 106.0, 1005.0 * 106.0);
        assert_eq!(r2.start, 1000, "clamped to total");
        assert_eq!(r2.end, 1000);
    }

    #[test]
    fn visible_range_clamps_at_the_ends() {
        let l = strip_layout(100.0, 5);
        let r = visible_range(&l, 5, 0.0, 100_000.0);
        assert_eq!(r, 0..5, "clamped to the model length");
        let r = visible_range(&l, 0, 0.0, 100.0);
        assert!(r.is_empty());
        // Scrolled above/before the content (elastic overscroll).
        let r = visible_range(&l, 5, -50.0, -10.0);
        assert!(r.is_empty());
    }

    #[test]
    fn fit_rect_preserves_aspect_and_never_upscales() {
        let container = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let r = fit_rect(container, [200.0, 100.0]);
        assert!((r.width() - 100.0).abs() < 0.01);
        assert!((r.height() - 50.0).abs() < 0.01);
        let r = fit_rect(container, [10.0, 20.0]);
        assert!((r.width() - 10.0).abs() < 0.01);
        assert!((r.height() - 20.0).abs() < 0.01);
        assert_eq!(r.center(), container.center());
    }
}
