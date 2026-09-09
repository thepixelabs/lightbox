// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The empty-state drop zone (E08 spec §2.1 item 2, task A1): with no
//! working set, the window is a full-bleed drop target that reflects
//! `hovered_files` *before* the drop lands (highlight + count). A free
//! function (not a method on `LightboxApp`) so it is directly testable with
//! `egui_kittest::Harness::new_ui`, no `Session`/GPU needed.
//!
//! This is the app's front door, drag-and-drop is the primary entry path
//! (no library/DAM, per the product cut), so this re-skin (design spec §10)
//! invests more than a bare stroke+label: a dashed marching-ants boundary,
//! a procedurally drawn photo-frame glyph, and a static (non-pulsing, per
//! the spec's motion budget) accent glow on drag-over. Every color/size
//! here comes from `theme::tokens`; nothing is hand-picked.

use eframe::egui;

use crate::intake::HoverAffordance;
use crate::theme::{fonts, paint, tokens as tk};

/// Corner-arc tessellation for the dropzone's dashed rounded-rect outline.
/// `theme::paint` has an equivalent (`rounded_rect_points`) but it is a
/// private helper local to that module's mesh builders, and this is a
/// one-off polyline consumer (feeding `Shape::dashed_line_many`, not a
/// filled mesh), duplicating the small arc loop here is simpler than
/// widening `paint`'s public surface for a single caller.
const DROPZONE_CORNER_SEGMENTS: usize = 6;

/// Builds the closed polyline outline of a rounded rect, clockwise from the
/// left-middle of the top-left corner, with the first point repeated at the
/// end so [`egui::Shape::dashed_line_many`]'s `windows(2)` walk draws the
/// final corner-to-start segment too (it does not auto-close the path).
fn dropzone_outline_points(rect: egui::Rect, radius: f32) -> Vec<egui::Pos2> {
    let r = radius
        .max(0.0)
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5);
    if r < 0.5 {
        return vec![
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
            rect.left_top(),
        ];
    }
    let corners = [
        (
            egui::pos2(rect.left() + r, rect.top() + r),
            180.0_f32,
            270.0_f32,
        ),
        (
            egui::pos2(rect.right() - r, rect.top() + r),
            270.0_f32,
            360.0_f32,
        ),
        (
            egui::pos2(rect.right() - r, rect.bottom() - r),
            0.0_f32,
            90.0_f32,
        ),
        (
            egui::pos2(rect.left() + r, rect.bottom() - r),
            90.0_f32,
            180.0_f32,
        ),
    ];
    let mut points = Vec::with_capacity(4 * (DROPZONE_CORNER_SEGMENTS + 1) + 1);
    for (center, start_deg, end_deg) in corners {
        let start = start_deg.to_radians();
        let end = end_deg.to_radians();
        for i in 0..=DROPZONE_CORNER_SEGMENTS {
            let t = i as f32 / DROPZONE_CORNER_SEGMENTS as f32;
            let a = start + (end - start) * t;
            points.push(egui::pos2(center.x + r * a.cos(), center.y + r * a.sin()));
        }
    }
    if let Some(&first) = points.first() {
        points.push(first);
    }
    points
}

/// `color` with its alpha replaced by `alpha` (RGB untouched), the exact
/// "token @ N%" convention `theme::tokens` itself uses (its private `rgba`
/// helper), needed here because the icon's fill color is a runtime value
/// (rest = `TEXT_TERTIARY`, drag-over = `accent()`), not a token
/// constant that could be pre-declared at 50% alpha.
fn at_alpha(color: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Procedurally draws the empty-state photo-frame glyph (spec §10): an
/// outer rounded-rect frame, an 8px aperture/sun dot inset top-left, and
/// two overlapping mountain-silhouette triangles across the bottom half
/// pure `egui::Painter`/`Shape` primitives, no raster asset. `color` is the
/// glyph's base color; the triangle fill is derived from it at 50% alpha
/// per the spec (rest = `TEXT_TERTIARY`, drag-over = `accent()`).
fn draw_photo_frame_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let painter = painter.with_clip_rect(rect);

    painter.rect_stroke(
        rect,
        tk::RADIUS_CONTROL,
        egui::Stroke::new(2.0, color),
        egui::StrokeKind::Inside,
    );

    // Aperture/sun dot: 8px diameter, inset from the top-left corner.
    let dot_r = 4.0;
    let dot_center = rect.left_top() + egui::vec2(dot_r + 6.0, dot_r + 6.0);
    painter.circle_filled(dot_center, dot_r, color);

    // Mountain silhouette: two overlapping triangles, bases on the frame's
    // bottom edge, peaks reaching to roughly the vertical midline.
    let fill = at_alpha(color, 128); // "@ 50%" per spec
    let inset = 4.0;
    let base_y = rect.bottom() - inset;
    let (left, top, right, w, h) = (
        rect.left(),
        rect.top(),
        rect.right(),
        rect.width(),
        rect.height(),
    );
    let back = vec![
        egui::pos2(left + w * 0.28, base_y),
        egui::pos2(left + w * 0.66, top + h * 0.30),
        egui::pos2(right - inset, base_y),
    ];
    let front = vec![
        egui::pos2(left + inset, base_y),
        egui::pos2(left + w * 0.34, top + h * 0.48),
        egui::pos2(left + w * 0.62, base_y),
    ];
    painter.add(egui::Shape::convex_polygon(back, fill, egui::Stroke::NONE));
    painter.add(egui::Shape::convex_polygon(front, fill, egui::Stroke::NONE));
}

/// Renders the empty-state drop zone into the current UI's available rect.
pub fn empty_state_ui(ui: &mut egui::Ui, hover: Option<HoverAffordance>) {
    let rect = ui.available_rect_before_wrap();
    let hovering = hover.is_some();
    let zone_rect = rect.shrink(12.0);

    // Background: the same recessed value as the loaded-canvas surround
    // (spec §10), the canvas, when empty, *is* the drop target, not a
    // separate overlay surface.
    ui.painter()
        .rect_filled(zone_rect, tk::RADIUS_DROPZONE, tk::ELEV_N1_CANVAS);

    let (border_color, icon_color) = if hovering {
        (tk::accent(), tk::accent())
    } else {
        (tk::DROPZONE_BORDER_REST, tk::TEXT_TERTIARY)
    };

    if hovering {
        // Drag-over: static (no pulse, out of the motion budget), flat
        // wash + solid border + the same 4-layer glow stack the house
        // slider uses on hover (spec §10 explicitly reuses it), held for
        // as long as the OS reports files hovering over the window.
        ui.painter()
            .rect_filled(zone_rect, tk::RADIUS_DROPZONE, tk::dropzone_drag_wash());
        paint::glow_rect(
            ui.painter(),
            zone_rect,
            tk::RADIUS_DROPZONE,
            &tk::accent_glow_hover(),
        );
        ui.painter().rect_stroke(
            zone_rect,
            tk::RADIUS_DROPZONE,
            egui::Stroke::new(2.0, border_color),
            egui::StrokeKind::Inside,
        );
    } else {
        // Rest: dashed 2px outline, 6-on/4-off (spec §10). epaint has no
        // dashed-rect primitive, so the rounded-rect outline is
        // tessellated locally and fed through `Shape::dashed_line_many`
        // (present in this epaint version, see `dropzone_outline_points`).
        let outline = dropzone_outline_points(zone_rect, tk::RADIUS_DROPZONE);
        let mut dashes = Vec::new();
        egui::Shape::dashed_line_many(
            &outline,
            egui::Stroke::new(2.0, border_color),
            tk::DROPZONE_DASH_PATTERN.0,
            tk::DROPZONE_DASH_PATTERN.1,
            &mut dashes,
        );
        ui.painter().extend(dashes);
    }

    ui.scope_builder(egui::UiBuilder::new().max_rect(zone_rect), |ui| {
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            // The message is one or two lines depending on hover state, so
            // this is a deliberate approximate placement (icon block starts
            // 30% down the zone) rather than a fake exact-centering pass
            // nothing here is pixel-verified, and it reads centered either
            // way at the sizes this zone actually renders at.
            ui.add_space((zone_rect.height() * 0.30).max(0.0));

            let (icon_rect, _) =
                ui.allocate_exact_size(tk::DROPZONE_ICON_SIZE, egui::Sense::hover());
            draw_photo_frame_icon(ui.painter(), icon_rect, icon_color);

            ui.add_space(12.0);
            let text = match hover {
                Some(HoverAffordance { count: Some(n) }) => {
                    format!("Drop {n} photo{} to open", if n == 1 { "" } else { "s" })
                }
                Some(HoverAffordance { count: None }) => "Drop to open".to_owned(),
                None => "Drop photos or a folder to start editing\nor use Open Files… / Open Folder… / Browse Folders… above".to_owned(),
            };
            ui.label(
                egui::RichText::new(text)
                    .font(fonts::empty_state_font())
                    .color(tk::TEXT_SECONDARY),
            );
        });
    });

    // H1: the zone itself is an AccessKit node (not just its inner text),
    // so assistive tech announces the drop target as a region. Hover-only
    // it never claims focus or competes with the window's drop handling.
    let response = ui.interact(rect, ui.id().with("empty-drop-zone"), egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            "Drop zone: drop photos or a folder to start editing",
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::test_support::themed;
    use egui_kittest::{kittest::Queryable, Harness};

    /// A1 AC: the empty state, with no hover, invites the three intake
    /// affordances (drop / dialog / browse).
    ///
    /// The UI closure is wrapped in [`themed`]: `empty_state_ui` lays out
    /// text in the named `Inter Medium` weight family
    /// ([`fonts::empty_state_font`]), which epaint panics on if it isn't
    /// bound, and a bare kittest `Context` never has `theme::install` run
    /// on it (`Harness::new_ui`/`build_ui` paint their first frame during
    /// *construction*, before test code gets a chance to install anything;
    /// see `theme::test_support`'s module doc for the full story). `themed`
    /// installs on frame 0 (painting nothing) so frame 1+ has the families
    /// bound, hence `harness.run()` (which steps past the install frame)
    /// before every query below.
    #[test]
    fn no_hover_shows_the_generic_invitation() {
        let mut harness = Harness::builder().build_ui(themed(|ui| {
            empty_state_ui(ui, None);
        }));
        harness.run();
        harness.get_by_label_contains("Drop photos or a folder");
    }

    /// A1 AC: `hovered_files` with a known count highlights + shows the
    /// count *before* the drop lands.
    #[test]
    fn hover_with_known_count_shows_the_count() {
        let mut harness = Harness::builder().build_ui(themed(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: Some(3) }));
        }));
        harness.run();
        harness.get_by_label_contains("Drop 3 photos to open");
    }

    /// A1 AC: a platform that hovers without reporting paths still
    /// highlights (graceful degradation, §6.1/R4).
    #[test]
    fn hover_without_a_known_count_still_shows_a_highlight() {
        let mut harness = Harness::builder().build_ui(themed(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: None }));
        }));
        harness.run();
        harness.get_by_label_contains("Drop to open");
    }

    #[test]
    fn singular_count_does_not_pluralize() {
        let mut harness = Harness::builder().build_ui(themed(|ui| {
            empty_state_ui(ui, Some(HoverAffordance { count: Some(1) }));
        }));
        harness.run();
        harness.get_by_label_contains("Drop 1 photo to open");
    }

    /// H1 AC: the drop zone is its own AccessKit node in every hover
    /// state, and it never claims keyboard focus.
    #[test]
    fn drop_zone_region_is_an_accesskit_node_and_never_focuses() {
        for hover in [
            None,
            Some(HoverAffordance { count: None }),
            Some(HoverAffordance { count: Some(2) }),
        ] {
            let mut harness = Harness::builder().build_ui(themed(move |ui| {
                empty_state_ui(ui, hover);
            }));
            harness.run();
            harness.get_by_label_contains("Drop zone:");
            assert!(
                harness.ctx.memory(|m| m.focused()).is_none(),
                "the drop-zone region must never steal focus"
            );
        }
    }
}
