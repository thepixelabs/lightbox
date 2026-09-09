// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Reusable epaint drawing primitives for Lightbox's dark theme.
//!
//! These are the building blocks the per-widget re-skin passes (house
//! slider, buttons, filmstrip, status bar, …) compose into full controls.
//! Nothing here knows what a "slider" or a "button" is, it only knows how
//! to paint a gradient rounded rect, a bevel pair, a glow stack, a hairline.
//!
//! Performance: every function here takes `&Painter` and issues shapes
//! directly (`Painter::add`), matching the rest of egui's immediate-mode
//! API, no `Path`/`Mesh` is retained across frames. The mesh builders
//! ([`vgradient_rounded_rect`], [`vgradient_circle`]) build a small `Vec`
//! per call (unavoidable, `Mesh` owns its vertex/index buffers), but
//! vertex counts are capped low (≤64 / ≤48, see each function's doc) so
//! this stays well inside the <2.5ms/frame budget even for a 50-knob
//! panel repaint. The stroke/line helpers ([`bevel_outset`], [`hairline_h`],
//! [`glow_circle`], …) allocate nothing, they call straight through to
//! `Painter::hline`/`circle_filled`/`rect_stroke`.

// Foundation phase: these primitives have no in-crate caller yet by
// design, the per-widget re-skin passes that call into them land as
// separate, parallel phases (see `tokens.rs`'s matching note). The
// `#[cfg(test)]` module below does exercise the private helpers directly.
#![allow(dead_code)]

use eframe::egui::{
    pos2, Color32, CornerRadius, Mesh, Painter, Pos2, Rangef, Rect, Shadow, Stroke, StrokeKind,
};

use super::tokens;

// =======================================================================
// Mesh-tessellated vertical gradients
// =======================================================================

/// Number of arc segments per corner for [`vgradient_rounded_rect`]. 4
/// segments (5 points) per corner is plenty smooth at the radii this theme
/// uses (3-10px) and keeps the fan well under the 64-vertex budget:
/// `4 corners * (segments+1) + 1 center = 21` vertices at the default.
const RECT_CORNER_SEGMENTS: usize = 4;

/// Segments for [`vgradient_circle`]'s fan. `32 + 1 center = 33` vertices,
/// under the 48-vertex budget, smooth enough for a 6-10px knob.
const CIRCLE_SEGMENTS: usize = 32;
const _: () = assert!(
    CIRCLE_SEGMENTS < 48,
    "vgradient_circle must stay under the 48-vertex budget"
);

/// Build the closed outline of a rounded rect as a `Vec<Pos2>`, clockwise,
/// starting at the left-middle of the top-left corner. Falls back to the
/// plain 4-corner rectangle when `radius` rounds to zero (or the rect is
/// too small to fit it), so callers get "a plain gradient band" for free
/// at `radius == 0` per the spec requirement.
fn rounded_rect_points(rect: Rect, radius: f32) -> Vec<Pos2> {
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
        ];
    }

    let corners = [
        (pos2(rect.left() + r, rect.top() + r), 180.0_f32, 270.0_f32),
        (pos2(rect.right() - r, rect.top() + r), 270.0_f32, 360.0_f32),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0_f32, 90.0_f32),
        (
            pos2(rect.left() + r, rect.bottom() - r),
            90.0_f32,
            180.0_f32,
        ),
    ];

    let mut points = Vec::with_capacity(4 * (RECT_CORNER_SEGMENTS + 1));
    for (center, start_deg, end_deg) in corners {
        let start = start_deg.to_radians();
        let end = end_deg.to_radians();
        for i in 0..=RECT_CORNER_SEGMENTS {
            let t = i as f32 / RECT_CORNER_SEGMENTS as f32;
            let a = start + (end - start) * t;
            points.push(pos2(center.x + r * a.cos(), center.y + r * a.sin()));
        }
    }
    points
}

#[inline]
fn vertical_t(y: f32, top: f32, height: f32) -> f32 {
    if height.abs() < f32::EPSILON {
        0.0
    } else {
        ((y - top) / height).clamp(0.0, 1.0)
    }
}

/// A Mesh-tessellated rounded rect with a per-vertex vertical gradient
/// (fan-triangulated from the rect's own center). Renders correctly at
/// `radius == 0` (a plain gradient band) and at the theme's small radii
/// (3-6px). Used for the slider fill bar, panel header bands, and chip
/// backgrounds, anywhere a small/medium surface needs a top-to-bottom
/// gradient (spec Principle 3: large flat expanses stay flat, so this is
/// deliberately not used for rail/canvas backgrounds).
pub fn vgradient_rounded_rect(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    top: Color32,
    bottom: Color32,
) {
    if !rect.is_positive() {
        return;
    }
    let points = rounded_rect_points(rect, radius);
    if points.len() < 3 {
        return;
    }

    let mut mesh = Mesh::default();
    let height = rect.height();
    let center = rect.center();
    let center_color = top.lerp_to_gamma(bottom, vertical_t(center.y, rect.top(), height));
    mesh.colored_vertex(center, center_color);
    for &p in &points {
        let t = vertical_t(p.y, rect.top(), height);
        mesh.colored_vertex(p, top.lerp_to_gamma(bottom, t));
    }

    let n = points.len() as u32;
    for i in 0..n {
        let b = 1 + i;
        let c = 1 + ((i + 1) % n);
        mesh.add_triangle(0, b, c);
    }
    painter.add(mesh);
}

/// A fan-triangulated circle with a per-vertex vertical gradient. Used for
/// the slider knob body (§3 layer 4).
pub fn vgradient_circle(painter: &Painter, center: Pos2, r: f32, top: Color32, bottom: Color32) {
    if r < 0.5 {
        return;
    }

    let mut mesh = Mesh::default();
    let top_y = center.y - r;
    let height = 2.0 * r;
    mesh.colored_vertex(center, top.lerp_to_gamma(bottom, 0.5));

    for i in 0..CIRCLE_SEGMENTS {
        let a = (i as f32 / CIRCLE_SEGMENTS as f32) * std::f32::consts::TAU;
        let p = pos2(center.x + r * a.cos(), center.y + r * a.sin());
        let t = vertical_t(p.y, top_y, height);
        mesh.colored_vertex(p, top.lerp_to_gamma(bottom, t));
    }

    let n = CIRCLE_SEGMENTS as u32;
    for i in 0..n {
        let b = 1 + i;
        let c = 1 + ((i + 1) % n);
        mesh.add_triangle(0, b, c);
    }
    painter.add(mesh);
}

// =======================================================================
// The two-stroke bevel primitive (spec Principle 2)
// =======================================================================

/// Paint the two 1px bevel strokes, inset horizontally by `radius` so they
/// hug the flat top/bottom edges without fighting the rounded corners.
/// Shared by [`bevel_outset`] and [`bevel_inset`], only the color pair
/// (and therefore the "which edge reads lit" direction) differs.
fn bevel_pair(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    top_color: Color32,
    bottom_color: Color32,
) {
    let x0 = rect.left() + radius;
    let x1 = rect.right() - radius;
    if x1 <= x0 || !rect.is_positive() {
        return;
    }
    let x_range = Rangef::new(x0, x1);
    let top_y = painter.round_to_pixel_center(rect.top());
    let bottom_y = painter.round_to_pixel_center(rect.bottom() - 1.0);
    painter.hline(x_range, top_y, Stroke::new(tokens::STROKE_BEVEL, top_color));
    painter.hline(
        x_range,
        bottom_y,
        Stroke::new(tokens::STROKE_BEVEL, bottom_color),
    );
}

/// Outset bevel, light-top / dark-bottom. Every "raised" surface (rest/
/// hover buttons, comboboxes, the top edge of a floating popup) uses this.
pub fn bevel_outset(painter: &Painter, rect: Rect, radius: f32) {
    bevel_pair(
        painter,
        rect,
        radius,
        tokens::BEVEL_OUTSET_TOP,
        tokens::BEVEL_OUTSET_BOTTOM,
    );
}

/// Inset bevel, dark-top / light-bottom. Every "recessed" surface
/// (text-edit wells, the slider groove, a pressed button) uses this.
pub fn bevel_inset(painter: &Painter, rect: Rect, radius: f32) {
    bevel_pair(
        painter,
        rect,
        radius,
        tokens::BEVEL_INSET_TOP,
        tokens::BEVEL_INSET_BOTTOM,
    );
}

// =======================================================================
// Hairlines and the engraved seam
// =======================================================================

/// A crisp 1px horizontal separator, pixel-center-snapped so it renders
/// sharp regardless of `pixels_per_point`.
pub fn hairline_h(painter: &Painter, x_range: Rangef, y: f32, color: Color32) {
    let y = painter.round_to_pixel_center(y);
    painter.hline(x_range, y, Stroke::new(tokens::STROKE_HAIRLINE, color));
}

/// A crisp 1px vertical separator, pixel-center-snapped.
pub fn hairline_v(painter: &Painter, y_range: Rangef, x: f32, color: Color32) {
    let x = painter.round_to_pixel_center(x);
    painter.vline(x, y_range, Stroke::new(tokens::STROKE_HAIRLINE, color));
}

/// The two-stroke "engraved seam" (spec §2): a [`tokens::SEPARATOR_HAIRLINE`]
/// with a [`tokens::SEPARATOR_HAIRLINE_HIGHLIGHT`] drawn immediately
/// adjacent on the side nearer the lighter/elevated surface. Set
/// `lighter_side_below` to `true` when the elevated surface is *below* `y`
/// (e.g. a rail sitting above a lighter panel), `false` when it's above.
pub fn engraved_seam_h(painter: &Painter, x_range: Rangef, y: f32, lighter_side_below: bool) {
    hairline_h(painter, x_range, y, tokens::SEPARATOR_HAIRLINE);
    let highlight_y = if lighter_side_below { y + 1.0 } else { y - 1.0 };
    hairline_h(
        painter,
        x_range,
        highlight_y,
        tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
    );
}

/// The vertical counterpart to [`engraved_seam_h`]: the same two-stroke
/// seam turned 90°, with the highlight on the side nearer the lighter
/// surface. Set `lighter_side_right` when the elevated surface is to the
/// right of `x`.
///
/// (Promoted here from `lib.rs` when the develop rails' column splitters
/// became its second caller, exactly the condition that function's own
/// doc comment named for hoisting it.)
pub fn engraved_seam_v(painter: &Painter, y_range: Rangef, x: f32, lighter_side_right: bool) {
    hairline_v(painter, y_range, x, tokens::SEPARATOR_HAIRLINE);
    let highlight_x = if lighter_side_right { x + 1.0 } else { x - 1.0 };
    hairline_v(
        painter,
        y_range,
        highlight_x,
        tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
    );
}

// =======================================================================
// Glow stacks
// =======================================================================

/// Stacked translucent circle strokes for knob/dropzone glows. `layers`
/// must be radius-ascending (as every `tokens::ACCENT_GLOW_*` array is)
/// this iterates `.rev()` so the largest/faintest layer paints first and
/// the smallest/most-opaque paints last, right before the opaque knob
/// body covers the center. Zero heap allocation (no sort, just reverse
/// iteration over the caller's slice).
pub fn glow_circle(painter: &Painter, center: Pos2, base_r: f32, layers: &[(f32, Color32)]) {
    for &(delta, color) in layers.iter().rev() {
        let r = (base_r + delta).max(0.0);
        painter.circle_filled(center, r, color);
    }
}

/// Stacked expanding rounded-rect strokes, for the dropzone drag-over
/// border and thumbnail glows. Same radius-ascending / reverse-iteration
/// contract as [`glow_circle`]. Strokes are painted `StrokeKind::Outside`
/// so the glow surrounds the rect without touching its interior.
pub fn glow_rect(painter: &Painter, rect: Rect, radius: f32, layers: &[(f32, Color32)]) {
    for &(delta, color) in layers.iter().rev() {
        let r = (radius + delta).max(0.0) as u8;
        painter.rect_stroke(
            rect.expand(delta),
            CornerRadius::same(r),
            Stroke::new(1.5, color),
            StrokeKind::Outside,
        );
    }
}

// =======================================================================
// Shadow presets (spec: Full token table > Shadows)
// =======================================================================
//
// Thin function wrappers around the `tokens::SHADOW_*` consts, kept here
// too (not just in `tokens.rs`) because the deliverable surface for shadow
// lookups is `theme::paint`, matching every other paint-order helper in
// this module. Use `.as_shape(rect, corner_radius)` on the result to turn
// it into a paintable `RectShape`.

pub fn shadow_popup() -> Shadow {
    tokens::SHADOW_POPUP
}
pub fn shadow_tooltip() -> Shadow {
    tokens::SHADOW_TOOLTIP
}
pub fn shadow_overlay() -> Shadow {
    tokens::SHADOW_OVERLAY
}
pub fn shadow_thumbnail() -> Shadow {
    tokens::SHADOW_THUMBNAIL
}

/// The slider knob's 2-layer contact shadow (§3 layer 3), always present,
/// not interaction-dependent. Two stacked filled circles, offset straight
/// down, drawn back-to-front per [`tokens::SHADOW_KNOB_CONTACT`].
pub fn contact_shadow_circle(painter: &Painter, center: Pos2, r: f32) {
    for layer in &tokens::SHADOW_KNOB_CONTACT {
        painter.circle_filled(center + layer.offset, r + layer.radius_delta, layer.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, Context, Id, LayerId, Order, Rect};

    fn rect(min: Pos2, max: Pos2) -> Rect {
        Rect::from_min_max(min, max)
    }

    fn test_painter() -> Painter {
        let ctx = Context::default();
        // A frame must have started for `Context::pixels_per_point` and
        // co. to be valid; `Context::default()` gives sane defaults (1.0
        // ppp) without needing a full `run()` cycle.
        Painter::new(
            ctx,
            LayerId::new(Order::Middle, Id::new("test")),
            Rect::EVERYTHING,
        )
    }

    #[test]
    fn rounded_rect_points_zero_radius_is_plain_rect() {
        let r = rect(pos2(0.0, 0.0), pos2(10.0, 4.0));
        let pts = rounded_rect_points(r, 0.0);
        assert_eq!(pts.len(), 4);
    }

    #[test]
    fn rounded_rect_points_small_radius_is_reasonable() {
        let r = rect(pos2(0.0, 0.0), pos2(20.0, 6.0));
        let pts = rounded_rect_points(r, 3.0);
        // 4 corners * (segments + 1) points, no center yet.
        assert_eq!(pts.len(), 4 * (RECT_CORNER_SEGMENTS + 1));
        assert!(
            pts.len() <= 63,
            "must fit the +1 center under the 64-vertex budget"
        );
    }

    #[test]
    fn rounded_rect_points_radius_clamped_to_half_extent() {
        // A radius bigger than the rect must not produce NaN/garbage.
        let r = rect(pos2(0.0, 0.0), pos2(4.0, 4.0));
        let pts = rounded_rect_points(r, 100.0);
        for p in pts {
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }

    #[test]
    fn vgradient_rounded_rect_degenerate_does_not_panic() {
        let painter = test_painter();
        // Zero-height, zero-width, and negative (empty) rects must all be
        // safely ignored, not panic.
        vgradient_rounded_rect(
            &painter,
            rect(pos2(0.0, 0.0), pos2(10.0, 0.0)),
            3.0,
            Color32::WHITE,
            Color32::BLACK,
        );
        vgradient_rounded_rect(
            &painter,
            rect(pos2(0.0, 0.0), pos2(0.0, 10.0)),
            3.0,
            Color32::WHITE,
            Color32::BLACK,
        );
        vgradient_rounded_rect(&painter, Rect::NOTHING, 3.0, Color32::WHITE, Color32::BLACK);
    }

    #[test]
    fn vgradient_circle_degenerate_does_not_panic() {
        let painter = test_painter();
        vgradient_circle(
            &painter,
            pos2(0.0, 0.0),
            0.0,
            Color32::WHITE,
            Color32::BLACK,
        );
        vgradient_circle(
            &painter,
            pos2(0.0, 0.0),
            -5.0,
            Color32::WHITE,
            Color32::BLACK,
        );
    }

    #[test]
    fn glow_circle_zero_layers_does_not_panic() {
        let painter = test_painter();
        glow_circle(&painter, pos2(0.0, 0.0), 6.0, &[]);
    }

    #[test]
    fn bevel_pair_degenerate_width_does_not_panic() {
        let painter = test_painter();
        // A rect narrower than 2*radius must skip drawing, not underflow.
        bevel_outset(&painter, rect(pos2(0.0, 0.0), pos2(2.0, 10.0)), 4.0);
        bevel_inset(&painter, Rect::NOTHING, 4.0);
    }
}
