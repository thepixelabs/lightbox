// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E4, [`param_slider`], the house develop-slider control (spec §6.5,
//! research-02 conventions), plus its reusable core [`value_slider`]:
//!
//! * **click-drag scrub**, relative: dragging the track scrubs the value
//!   (full track width ≙ full range); **Shift = fine** (×0.1);
//! * **double-click = reset**, targets [`EditBinding::default`] for the
//!   param (via [`EditBinding::reset`]);
//! * **click the value = text entry**, Enter/focus-loss commits (clamped
//!   to the spec range), Esc cancels;
//! * **↑/↓ = keyboard nudge** while the control has focus (click it to
//!   focus): ±`step`, Shift = ±`fine`. `←`/`→` are deliberately untouched
//!   (they are `nav.prev`/`nav.next` keymap chords).
//!
//! [`value_slider`] is deliberately value-in/events-out (no `EditBinding`)
//! so controls that decompose a coarse leaf, the WB panel's temp/tint
//! over ONE `ParamId::WhiteBalance` (see `E08-deviations.md` A0-8), can
//! reuse the exact same affordances; [`param_slider`] is the thin scalar
//! (`ParamValue::F32`) binding every tone slider uses. E10/E11/E12 panels
//! should build on these two, not roll their own.
//!
//! ## Paint model (dark-theme re-skin, spec §3)
//!
//! Every visual byte here comes from `crate::theme`, [`tokens`] for the
//! numbers, [`paint`] for the reusable draw primitives, [`fonts`] for the
//! per-role `FontId`s, never a hardcoded color or `FontId` literal. The
//! track and knob are painted in the design spec's exact back-to-front
//! layer order:
//!
//! * **Track** (5 layers): a `WELL_BG` groove at [`tokens::TRACK_HEIGHT`]
//!   with a two-stroke inset bevel ([`paint::bevel_inset`]), then a
//!   vertical-gradient fill bar ([`paint::vgradient_rounded_rect`]) with a
//!   1px top glare.
//! * **Knob** (6 layers): a state-gated glow stack (silent at rest, 4
//!   layers on hover, 5 while dragging, [`paint::glow_circle`]), a crisp
//!   un-blurred 2px keyboard-focus ring drawn independently of hover/drag,
//!   an always-on 2-layer contact shadow ([`paint::contact_shadow_circle`]),
//!   a neutral-metal vertical-gradient body ([`paint::vgradient_circle`]
//!   never accent-tinted; only the glow/ring above carry the accent), an
//!   edge stroke, and a top highlight arc.
//!
//! **Fill origin** ([`tokens::FillOrigin`]) is *inferred* from
//! `spec.min`/`spec.max` (`min < 0.0 && max > 0.0` ⇒ `Center`, everything
//! else ⇒ `Left`) rather than added as a new `SliderSpec` field, every
//! existing call site across the other in-flight panel worktrees keeps
//! compiling untouched, and the inference is correct for every current
//! caller: the symmetric bipolar ranges (Exposure, Contrast, Temp
//! (relative), …) already resolve to the track's geometric center, and
//! the unipolar ones (WB Kelvin, `Amount`, `Blending`, …) already resolve
//! to the left edge. See [`fill_origin`].
//!
//! The click-to-type value field is styled as a recessed well (spec §4):
//! `WELL_BG` fill, inset bevel, and an accent focus treatment, see
//! [`text_edit_well`].
//!
//! [`EditBinding::default`]: crate::panels::develop_ctx::EditBinding::default

use eframe::egui;
use lightbox_edit::{ParamDelta, ParamId, ParamValue};

use crate::panels::develop_ctx::DevelopCtx;
use crate::theme::{fonts, paint, tokens};

/// A slider's spec-range contract (spec §6.5).
pub struct SliderSpec {
    /// Range minimum (values clamp here, including typed entry).
    pub min: f64,
    /// Range maximum.
    pub max: f64,
    /// Keyboard-nudge increment (↑/↓).
    pub step: f64,
    /// Fine increment (Shift+↑/↓); Shift-drag scrubs at ×0.1.
    pub fine: f64,
    /// Row label ("Exposure").
    pub label: &'static str,
    /// Optional unit suffix ("EV", "K").
    pub unit: Option<&'static str>,
}

/// What the core slider observed this frame, in input order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliderEvent {
    /// A drag began, start a gesture.
    Begin,
    /// The (clamped) value changed mid-drag, preview it.
    Preview(f64),
    /// The drag ended, commit the gesture.
    End,
    /// Double-click, reset to the binding's default.
    Reset,
    /// A one-shot change (typed entry / keyboard nudge), one gesture.
    Commit(f64),
}

/// Decimal places for display/entry, derived from the nudge step.
fn decimals(spec: &SliderSpec) -> usize {
    if spec.step >= 1.0 {
        0
    } else if spec.step >= 0.1 {
        1
    } else {
        2
    }
}

fn format_value(v: f64, spec: &SliderSpec) -> String {
    let d = decimals(spec);
    match spec.unit {
        Some(unit) => format!("{v:.d$} {unit}"),
        None => format!("{v:+.d$}"),
    }
}

/// The fill origin for `spec`'s range (spec §3), inferred rather than
/// carried as an explicit `SliderSpec` field (see this module's doc
/// comment for why). A range that spans a true signed zero, `min < 0.0
/// && max > 0.0`, is bipolar and fills from the center outward,
/// matching Lightroom convention for Exposure/Temp/Tint/Contrast/
/// Highlights/Shadows-shaped controls. Every other range, including one
/// that merely *touches* zero at a boundary (`0.0..=100.0`,
/// `-100.0..=0.0`), is unipolar and fills from the left.
fn fill_origin(spec: &SliderSpec) -> tokens::FillOrigin {
    if spec.min < 0.0 && spec.max > 0.0 {
        tokens::FillOrigin::Center
    } else {
        tokens::FillOrigin::Left
    }
}

/// The click-to-type value field, styled as a recessed well (spec §4's
/// "text-edit well" family): `WELL_BG` fill, inset bevel, same recipe as
/// the slider groove, and, on focus, the well's own top-edge stroke
/// swaps from black to `accent()` plus the same crisp external
/// focus ring every other focused control in the theme gets. Caret color
/// is `accent()`, scoped to this one widget so it doesn't leak into
/// sibling controls.
fn text_edit_well(ui: &mut egui::Ui, buf: &mut String) -> egui::Response {
    let frame = egui::Frame::NONE
        .fill(tokens::WELL_BG)
        .corner_radius(tokens::RADIUS_CONTROL)
        .inner_margin(egui::Margin::symmetric(6, 2));

    let response = ui
        .scope(|ui| {
            ui.visuals_mut().text_cursor.stroke = egui::Stroke::new(1.5, tokens::accent());
            ui.add(
                egui::TextEdit::singleline(buf)
                    .desired_width(64.0)
                    .font(fonts::slider_value_font())
                    .text_color(tokens::TEXT_PRIMARY)
                    .frame(frame),
            )
        })
        .inner;

    let painter = ui.painter();
    paint::bevel_inset(painter, response.rect, tokens::RADIUS_CONTROL);
    if response.has_focus() {
        // Focus indicator #1 (spec §4): the well's own top edge becomes
        // the accent, overdraw the inset-top stroke `bevel_inset` just
        // laid down, same pixel row, full-opacity accent.
        let x0 = response.rect.left() + tokens::RADIUS_CONTROL;
        let x1 = response.rect.right() - tokens::RADIUS_CONTROL;
        if x1 > x0 {
            paint::hairline_h(
                painter,
                egui::Rangef::new(x0, x1),
                response.rect.top(),
                tokens::accent(),
            );
        }
        // Focus indicator #2: the same crisp, un-blurred 2px external
        // ring every other focused control gets (spec §4/§5).
        painter.rect_stroke(
            response.rect.expand(2.0),
            tokens::RADIUS_CONTROL,
            egui::Stroke::new(tokens::STROKE_FOCUS_RING, tokens::accent_focus_ring()),
            egui::StrokeKind::Outside,
        );
    }
    response
}

/// Layer 6 of the knob (spec §3): a short 1px arc over the top ~40% of
/// the knob's circumference, the specular highlight cast by the theme's
/// single top-left light source. Drawn as an open polyline (not a filled
/// mesh), so it stays well outside the vertex-count budgets that govern
/// [`paint::vgradient_circle`].
fn knob_top_highlight_arc(painter: &egui::Painter, center: egui::Pos2, r: f32) {
    const SEGMENTS: usize = 8;
    const SPAN_DEG: f32 = 144.0; // ~40% of 360°, centered on "straight up".
    let start = (-90.0 - SPAN_DEG / 2.0).to_radians();
    let end = (-90.0 + SPAN_DEG / 2.0).to_radians();
    let mut points = Vec::with_capacity(SEGMENTS + 1);
    for i in 0..=SEGMENTS {
        let t = i as f32 / SEGMENTS as f32;
        let a = start + (end - start) * t;
        points.push(egui::pos2(center.x + r * a.cos(), center.y + r * a.sin()));
    }
    painter.line(points, egui::Stroke::new(1.0, tokens::KNOB_TOP_HIGHLIGHT));
}

/// Paints the track (5 layers) and knob (6 layers) in the spec §3 paint
/// order, back to front, state-aware (rest / hover / drag / keyboard
/// focus). `rect` is the full hit row (14px tall); the visible track sits
/// centered inside it at [`tokens::TRACK_HEIGHT`] (6px), a bigger hit
/// area than paint area is correct here (spec §3 point 6).
fn paint_track_and_knob(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    value: f64,
    spec: &SliderSpec,
) {
    let painter = ui.painter();

    // ── Track, layers 1-5. ───────────────────────────────────────────────
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - tokens::TRACK_HEIGHT / 2.0),
        egui::pos2(rect.right(), rect.center().y + tokens::TRACK_HEIGHT / 2.0),
    );
    // 1: groove base fill.
    painter.rect_filled(track, tokens::TRACK_RADIUS, tokens::WELL_BG);
    // 2 + 3: inset-top / inset-bottom bevel strokes (one call paints both).
    paint::bevel_inset(painter, track, tokens::TRACK_RADIUS);

    let t_of = |v: f64| ((v - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0) as f32;
    let val_t = t_of(value);
    let origin_t = match fill_origin(spec) {
        tokens::FillOrigin::Left => 0.0,
        tokens::FillOrigin::Center => t_of(0.0),
    };
    let (a, b) = if val_t >= origin_t {
        (origin_t, val_t)
    } else {
        (val_t, origin_t)
    };
    let fill = egui::Rect::from_min_max(
        egui::pos2(rect.left() + a * rect.width(), track.top()),
        egui::pos2(rect.left() + b * rect.width(), track.bottom()),
    );
    // 4: fill bar, vertical gradient.
    paint::vgradient_rounded_rect(
        painter,
        fill,
        tokens::TRACK_RADIUS,
        tokens::fill_gradient_top(),
        tokens::fill_gradient_bottom(),
    );
    // 5: fill top glare, only meaningful once the bar has visible width.
    if fill.width() > 0.5 {
        paint::hairline_h(
            painter,
            egui::Rangef::new(fill.left(), fill.right()),
            fill.top(),
            tokens::FILL_TOP_GLARE,
        );
    }

    // ── Knob, layers 1-6, centered at the value position. ───────────────
    let dragging = response.dragged();
    let hovering = response.hovered();
    let knob_center = egui::pos2(rect.left() + val_t * rect.width(), rect.center().y);
    let knob_r = if dragging || hovering {
        tokens::KNOB_RADIUS_HOVER_DRAG
    } else {
        tokens::KNOB_RADIUS_REST
    };

    // 1: glow stack, silent at rest (Principle 1: accent is quiet until
    // interacted with).
    if dragging {
        paint::glow_circle(painter, knob_center, knob_r, &tokens::accent_glow_drag());
    } else if hovering {
        paint::glow_circle(painter, knob_center, knob_r, &tokens::accent_glow_hover());
    }
    // 2: crisp keyboard-focus ring, independent of hover/drag, never
    // blurred/alpha-stacked (a11y requirement: must read distinct from
    // the soft decorative glow next to it).
    if response.has_focus() {
        painter.circle_stroke(
            knob_center,
            knob_r + 4.0,
            egui::Stroke::new(tokens::STROKE_FOCUS_RING, tokens::accent_focus_ring()),
        );
    }
    // 3: contact shadow, always present, not interaction-dependent.
    paint::contact_shadow_circle(painter, knob_center, knob_r);
    // 4: knob body, vertical-gradient circle, deliberately neutral metal,
    // never accent-tinted (only the glow/ring above carry the accent).
    let (fill_top, fill_bottom) = if dragging || hovering {
        (
            tokens::KNOB_FILL_HOVER_DRAG_TOP,
            tokens::KNOB_FILL_HOVER_DRAG_BOTTOM,
        )
    } else {
        (tokens::KNOB_FILL_REST_TOP, tokens::KNOB_FILL_REST_BOTTOM)
    };
    paint::vgradient_circle(painter, knob_center, knob_r, fill_top, fill_bottom);
    // 5: knob outer edge stroke.
    let edge_color = if dragging || hovering {
        tokens::KNOB_EDGE_STROKE_HOVER_DRAG
    } else {
        tokens::KNOB_EDGE_STROKE_REST
    };
    painter.circle_stroke(knob_center, knob_r, egui::Stroke::new(1.0, edge_color));
    // 6: top highlight arc.
    knob_top_highlight_arc(painter, knob_center, knob_r);
}

/// The reusable slider core: renders label/value/track, returns the events
/// this frame produced. `value` is the caller's current truth (previews
/// round-trip through the recipe, so the caller passes fresh state every
/// frame).
pub fn value_slider(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    value: f64,
    spec: &SliderSpec,
) -> (egui::Response, Vec<SliderEvent>) {
    let id = ui.id().with(id_salt);
    let edit_id = id.with("edit");
    let mut events = Vec::new();

    // ── Label row: name left, value (click-to-type) right (spec §3, §9). ──
    //
    // Height is pinned rather than auto-sized: an auto-sized row grew to
    // roughly twice what a 12px line needs, and it was the single largest
    // contributor to the rail's slider-to-slider pitch.
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), tokens::SLIDER_LABEL_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new(spec.label)
                    .font(fonts::slider_label_font())
                    .color(tokens::TEXT_SECONDARY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let editing: Option<String> = ui.data(|d| d.get_temp(edit_id));
                match editing {
                    Some(mut buf) => {
                        let resp = text_edit_well(ui, &mut buf);
                        // First frame after the click: grab focus.
                        if ui.data(|d| d.get_temp::<bool>(edit_id.with("fresh")) == Some(true)) {
                            resp.request_focus();
                            ui.data_mut(|d| d.remove::<bool>(edit_id.with("fresh")));
                        }
                        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        let commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if cancel {
                            ui.data_mut(|d| d.remove::<String>(edit_id));
                        } else if commit || resp.lost_focus() {
                            // Enter or focus loss commits (clamped); an
                            // unparseable entry is discarded (no gesture).
                            if let Ok(v) = buf.trim().parse::<f64>() {
                                events.push(SliderEvent::Commit(v.clamp(spec.min, spec.max)));
                            }
                            ui.data_mut(|d| d.remove::<String>(edit_id));
                        } else {
                            ui.data_mut(|d| d.insert_temp(edit_id, buf));
                        }
                    }
                    None => {
                        let text = format_value(value, spec);
                        let resp = ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(text)
                                        .font(fonts::slider_value_font())
                                        .color(tokens::TEXT_PRIMARY),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Click to type a value");
                        if resp.clicked() {
                            let d = decimals(spec);
                            ui.data_mut(|d2| {
                                d2.insert_temp(edit_id, format!("{value:.d$}"));
                                d2.insert_temp(edit_id.with("fresh"), true);
                            });
                        }
                    }
                }
            });
        },
    );

    // ── Track: the scrub/reset/focus surface. ────────────────────────────
    // The interactive row is taller than the 6pt visible groove so the drag
    // target stays forgiving (see `tokens::TRACK_HIT_HEIGHT`).
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), tokens::TRACK_HIT_HEIGHT),
        egui::Sense::click_and_drag(),
    );

    if response.clicked() {
        // Focus enables ↑/↓ nudge; egui clears focus on Esc/click-away.
        response.request_focus();
    }
    if response.double_clicked() {
        events.push(SliderEvent::Reset);
    }
    if response.drag_started() {
        events.push(SliderEvent::Begin);
    }
    if response.dragged() {
        let delta_px = response.drag_delta().x as f64;
        if delta_px != 0.0 && rect.width() > 0.0 {
            let fine = ui.input(|i| i.modifiers.shift);
            let scale = (spec.max - spec.min) / rect.width() as f64;
            let scale = if fine { scale * 0.1 } else { scale };
            let next = (value + delta_px * scale).clamp(spec.min, spec.max);
            if next != value {
                events.push(SliderEvent::Preview(next));
            }
        }
    }
    if response.drag_stopped() {
        events.push(SliderEvent::End);
    }
    if response.has_focus() {
        let (up, down, shift) = ui.input_mut(|i| {
            let shift = i.modifiers.shift;
            let mods = if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            };
            (
                i.consume_key(mods, egui::Key::ArrowUp),
                i.consume_key(mods, egui::Key::ArrowDown),
                shift,
            )
        });
        let inc = if shift { spec.fine } else { spec.step };
        if up {
            events.push(SliderEvent::Commit((value + inc).clamp(spec.min, spec.max)));
        }
        if down {
            events.push(SliderEvent::Commit((value - inc).clamp(spec.min, spec.max)));
        }
    }

    // ── Paint: spec §3's full track+knob layer stack. ───────────────────
    if ui.is_rect_visible(rect) {
        paint_track_and_knob(ui, &response, rect, value, spec);
    }

    // AccessKit value semantics (E4; H1 extends the app-wide pass).
    response.widget_info(|| egui::WidgetInfo::slider(true, value, spec.label));

    (response, events)
}

/// The value inside a scalar `ParamValue`, or `None` for a non-scalar leaf
/// (a programming error at the call site, `param_slider` is for `F32`
/// params only; coarse leaves decompose via [`value_slider`] directly).
fn as_f64(v: &ParamValue) -> Option<f64> {
    match v {
        ParamValue::F32(x) => Some(*x as f64),
        _ => None,
    }
}

fn f32_delta(p: ParamId, v: f64) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(p, ParamValue::F32(v as f32));
    delta
}

/// E4, the house param slider (spec §6.5): binds [`value_slider`]'s
/// affordances to one scalar `ParamId` through the `EditBinding` gesture
/// lifecycle. Every tone slider (and every future E10/E11 scalar control)
/// goes through here.
pub fn param_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    p: ParamId,
    spec: &SliderSpec,
) -> egui::Response {
    let value = as_f64(&ctx.edit.value(p)).unwrap_or_else(|| {
        debug_assert!(false, "param_slider bound to non-scalar param {p:?}");
        0.0
    });
    let (response, events) = value_slider(ui, ("param_slider", p), value, spec);
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(p),
            SliderEvent::Preview(v) => ctx.edit.preview(f32_delta(p, v)),
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => ctx.edit.reset(p),
            SliderEvent::Commit(v) => {
                // One-shot: begin/preview/end = exactly one history step.
                ctx.edit.begin_gesture(p);
                ctx.edit.preview(f32_delta(p, v));
                ctx.edit.end_gesture();
            }
        }
    }
    response
}

/// The list-row background for a clickable, "click to apply / hover to
/// preview" row (`panels::looks`/`panels::presets`/`panels::history`) that
/// is currently the applied/selected item, spec §5's selection language:
/// **accent border/wash for the applied item, neutral brightening for
/// plain hover, accent must never leak into non-selected hover.**
///
/// Exists because `egui::selectable_label`'s "checked" state (`Button::
/// selectable`) paints a small, isolated, pill-shaped fill sized to the
/// text, which reads as a checked radio/checkbox form control, not as
/// "the current row in a list" (an owner-reported confusion: "why do the
/// presets have radio buttons?"). A list row's selection state should read
/// the way a file manager, a Spotify queue, or Lightroom's own preset
/// panel show it: a wash spanning the FULL row, not a toggle chip hugging
/// the label. Pair with a plain `egui::Button::new(..).frame(false)` for
/// the row's clickable label (never `selectable_label`), this frame's
/// background paints BEHIND that content regardless of call order (`Frame`
/// inserts its background shape at a reserved index, see its own docs), so
/// wrapping the row's existing `ui.horizontal(..)` in this frame is a
/// drop-in change.
///
/// Unselected rows get `Frame::new()` (fully transparent, zero visual or
/// layout cost) rather than any hover treatment: the frameless `Button`
/// inside already only reads via the app-wide dark-theme text tokens
/// (untouched by this change), and adding a hover wash here would need a
/// second render pass to know the row was hovered before its background
/// paints, a bigger change than this bug fix calls for. Revisit if a
/// future pass wants the "neutral brightening on hover" half of the spec
/// language too.
pub fn list_row_frame(selected: bool) -> egui::Frame {
    let mut frame = egui::Frame::new()
        .corner_radius(tokens::RADIUS_CONTROL)
        .inner_margin(egui::Margin::symmetric(4, 2));
    if selected {
        frame = frame
            .fill(tokens::accent_wash())
            .stroke(egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::accent()));
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::test_support::{themed, themed_state};
    use eframe::egui::accesskit::Role;
    use egui_kittest::{kittest::NodeT as _, kittest::Queryable, Harness};

    /// H1 AC ("sliders with value semantics"): the house slider exposes an
    /// AccessKit `Slider` role node named by its label and carrying the
    /// live numeric value.
    #[test]
    fn value_slider_exposes_accesskit_slider_role_with_numeric_value() {
        let spec = SliderSpec {
            min: -5.0,
            max: 5.0,
            step: 0.1,
            fine: 0.01,
            label: "Exposure",
            unit: Some("EV"),
        };
        // The re-skinned label/value text is painted through
        // `theme::fonts`' weight-role font families (e.g. `panel_header_font`
        // elsewhere in the theme); `themed` installs the real theme (fonts +
        // named families) on frame 0 before anything paints, so a bare test
        // `Context` never hits an unbound-family panic. See
        // `theme::test_support`'s module doc for why this can't just be a
        // post-construction `theme::install(&harness.ctx)` call. Frame 0
        // paints nothing, so the one `harness.run()` below lands on frame 1
        // (the first frame that actually paints the slider).
        let mut harness = Harness::new_ui(themed(move |ui| {
            let _ = value_slider(ui, "h1-slider", 1.25, &spec);
        }));
        harness.run();

        // Both the text label and the track exist; the ROLE query pins the
        // track node (the one with slider semantics).
        let node = harness.get_by_role_and_label(Role::Slider, "Exposure");
        let value = node
            .accesskit_node()
            .numeric_value()
            .expect("slider carries a numeric value");
        assert!((value - 1.25).abs() < 1e-6, "live value exposed: {value}");
    }

    /// Fill-origin inference (spec §3): a range spanning a true signed
    /// zero is bipolar and fills from the center; everything else
    /// including a range that only *touches* zero at a boundary, is
    /// unipolar and fills from the left. Pure logic, asserted directly
    /// against every shape of range the panel worktrees actually
    /// construct today (`pct()`'s ±100 bands, WB's raw Kelvin span, the
    /// `Amount`/`Blending` 0..=100 sliders, Angle's ±45°, Tint's ±150).
    #[test]
    fn fill_origin_is_center_for_bipolar_ranges_and_left_otherwise() {
        let bipolar = [
            (-5.0, 5.0),     // Exposure
            (-100.0, 100.0), // Contrast / Highlights / Shadows / HSL / grading / …
            (-45.0, 45.0),   // Straighten angle
            (-150.0, 150.0), // Tint
        ];
        for (min, max) in bipolar {
            let spec = SliderSpec {
                min,
                max,
                step: 1.0,
                fine: 0.1,
                label: "x",
                unit: None,
            };
            assert_eq!(
                fill_origin(&spec),
                tokens::FillOrigin::Center,
                "range {min}..{max} spans a signed zero and must fill from center"
            );
        }

        let unipolar = [
            (0.0, 100.0),      // Amount / Blending
            (2000.0, 50000.0), // WB Kelvin (raw)
            (-100.0, 0.0),     // touches zero only at the upper boundary
        ];
        for (min, max) in unipolar {
            let spec = SliderSpec {
                min,
                max,
                step: 1.0,
                fine: 0.1,
                label: "x",
                unit: None,
            };
            assert_eq!(
                fill_origin(&spec),
                tokens::FillOrigin::Left,
                "range {min}..{max} does not span a signed zero and must fill from the left"
            );
        }
    }

    /// The re-skin must not change the `EditBinding` gesture lifecycle: a
    /// real synthetic drag (hover → drag → move → drop, the same sequence
    /// `basic.rs`'s own synthetic-drag tests use) must still emit exactly
    /// `Begin` first, `End` last, with at least one `Preview` in between
    /// the shape `param_slider` relies on to bracket exactly one undo
    /// step. Purely visual properties (glow alpha, gradient stops, …)
    /// aren't asserted here, they can't be observed headlessly, but the
    /// event contract can, and is.
    #[test]
    fn drag_gesture_sequence_is_unchanged_by_the_reskin() {
        let spec = SliderSpec {
            min: -5.0,
            max: 5.0,
            step: 0.1,
            fine: 0.01,
            label: "Exposure",
            unit: Some("EV"),
        };
        let mut harness = Harness::new_ui_state(
            themed_state(move |ui, log: &mut Vec<SliderEvent>| {
                let (_response, events) = value_slider(ui, "gesture-order", 0.0, &spec);
                log.extend(events);
            }),
            Vec::<SliderEvent>::new(),
        );
        harness.set_size(egui::vec2(320.0, 40.0));
        harness.run();

        let track = harness.get_by_role_and_label(Role::Slider, "Exposure");
        let rect = track.rect();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drag_at(rect.left_center());
        harness.run();
        // `drag_at` only presses the button; the drag *delta* comes from a
        // subsequent pointer move while it's held, `hover_at` mid-drag,
        // matching `basic.rs`'s own synthetic-drag tests exactly.
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drop_at(rect.right_center());
        harness.run();

        let log = harness.state();
        assert_eq!(
            log.first(),
            Some(&SliderEvent::Begin),
            "a drag gesture must still begin with Begin: {log:?}"
        );
        assert!(
            log.iter().any(|e| matches!(e, SliderEvent::Preview(_))),
            "a scrub must still preview at least one value: {log:?}"
        );
        assert_eq!(
            log.last(),
            Some(&SliderEvent::End),
            "a drag gesture must still end with End: {log:?}"
        );
    }

    /// **A12 AC (double-click = reset), the widget-core half**, kept
    /// alongside the gesture-order test above: a plain (non-double-click)
    /// frame must never emit `Reset`, `value_slider` maps a track
    /// double-click, and only a double-click, straight to
    /// `SliderEvent::Reset`.
    #[test]
    fn double_click_is_the_only_source_of_a_reset_event() {
        let spec = SliderSpec {
            min: -5.0,
            max: 5.0,
            step: 0.1,
            fine: 0.01,
            label: "Exposure",
            unit: Some("EV"),
        };
        let mut harness = Harness::new_ui(themed(move |ui| {
            let (_response, events) = value_slider(ui, "no-dclick", 2.0, &spec);
            assert!(
                !events.contains(&SliderEvent::Reset),
                "a plain frame with no double-click must not emit Reset"
            );
        }));
        harness.run();
    }
}
