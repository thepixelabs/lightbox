// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase C (task **C13**), the `develop.grading` panel: the
//! [`ColorWheel`]-style widget (hue/sat puck + luminance slider + fine-drag
//! modifier) for `ParamId::ColorGrade`'s ONE [`ColorGrade`] leaf (shadows/
//! midtones/highlights/global wheels + blending + balance, spec §4.3
//! `ColorGrade`), the UI half of tasks C11-C12's colour-grade math
//! (`lightbox-render`'s `nodes::global::color_grade`).
//!
//! # Layout
//!
//! A zone-tab row (Shadows / Midtones / Highlights / Global, [`Zone::ALL`]),
//! mirroring `panels::curve`'s Channel-tab pattern. The active zone shows one
//! wheel ([`color_wheel`]: a circular hue/sat puck over a decorative
//! rainbow-disc backdrop) plus its Luminance slider and a per-wheel Reset.
//! Below the tabs, shared Blending/Balance sliders (spec's own "zone overlap
//! width" / "zone boundary shift" semantics, `nodes::global::color_grade`'s
//! `zone_weights`) apply across all three tonal wheels at once.
//!
//! # `ColorWheel` interaction (task C13 AC)
//!
//! - **Click/drag** sets the puck directly under the pointer (absolute
//!   positioning, angle = hue, radial distance = saturation).
//! - **Fine-drag modifier (Shift)**: instead of snapping to the raw pointer
//!   position, the puck moves by this frame's pointer delta scaled down
//!   ([`FINE_DRAG_SCALE`]) from its OWN current position, the same
//!   "modifier changes the sensitivity, not the semantics" shape
//!   `widgets::value_slider`'s Shift-drag (`×0.1`) already establishes for
//!   plain sliders.
//! - **Keyboard-accessible**: arrow nudge while the wheel has focus (←/→ =
//!   hue, ↑/↓ = saturation; Shift = fine step), the same focus-then-arrow
//!   convention `panels::curve`'s point editor uses for its selected point.
//! - **Per-wheel reset**: a Reset button restores ONLY the active zone's
//!   wheel to identity (hue/sat/lum all neutral) in one gesture, never the
//!   whole `ColorGrade` (the same "channel-blind reset would discard other
//!   work" discipline `panels::curve::reset_active` documents).
//!
//! No widget-local state: every control reads `ctx.edit.value(ParamId::
//! ColorGrade)` fresh each frame and writes back through the
//! `begin_gesture`/`preview`/`end_gesture` lifecycle, the panel always
//! reflects the recipe (the same A12 discipline `panels::hsl` restates).

use eframe::egui;
use lightbox_edit::{ColorGrade, GradeWheel, ParamDelta, ParamId, ParamValue, Treatment};

use crate::panels::bw::is_monochrome;
use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};
use crate::theme::{paint, tokens};

/// The grading panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.grading");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots after
/// the HSL panel (40), before history (90).
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Color Grading",
        source_req: SourceReq::Any,
        order: 45,
        build,
    }
}

/// The four grading wheels (spec §4.3 `ColorGrade`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Zone {
    Shadows,
    Midtones,
    Highlights,
    Global,
}

impl Zone {
    const ALL: [Zone; 4] = [
        Zone::Shadows,
        Zone::Midtones,
        Zone::Highlights,
        Zone::Global,
    ];

    fn label(self) -> &'static str {
        match self {
            Zone::Shadows => "Shadows",
            Zone::Midtones => "Midtones",
            Zone::Highlights => "Highlights",
            Zone::Global => "Global",
        }
    }

    fn wheel(self, cg: &ColorGrade) -> GradeWheel {
        match self {
            Zone::Shadows => cg.shadows,
            Zone::Midtones => cg.midtones,
            Zone::Highlights => cg.highlights,
            Zone::Global => cg.global,
        }
    }

    fn set_wheel(self, cg: &mut ColorGrade, w: GradeWheel) {
        match self {
            Zone::Shadows => cg.shadows = w,
            Zone::Midtones => cg.midtones = w,
            Zone::Highlights => cg.highlights = w,
            Zone::Global => cg.global = w,
        }
    }
}

fn grade_delta(cg: ColorGrade) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta
        .0
        .insert(ParamId::ColorGrade, ParamValue::ColorGrade(cg));
    delta
}

fn current_treatment(ctx: &DevelopCtx<'_>) -> Treatment {
    match ctx.edit.value(ParamId::Treatment) {
        ParamValue::Treatment(t) => t,
        _ => Treatment::Color,
    }
}

/// **D2 AC ("color panels visibly disabled in Monochrome")**: the render
/// engine already elides `ColorGradeNode` once `Treatment::BlackAndWhite`
/// is active (`nodes/global/color_grade.rs`'s own `is_identity`), this
/// greys the whole wheel editor via [`is_monochrome`] (`panels::bw`'s
/// single unit-tested predicate) so the UI reflects that.
fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let mono = is_monochrome(current_treatment(ctx));

    let zone_id = ui.make_persistent_id("develop.grading.zone");
    let mut zone: Zone = ui.data(|d| d.get_temp(zone_id)).unwrap_or(Zone::Shadows);

    ui.add_enabled_ui(!mono, |ui| {
        let cg = match ctx.edit.value(ParamId::ColorGrade) {
            ParamValue::ColorGrade(cg) => cg,
            _ => ColorGrade::default(),
        };

        ui.horizontal(|ui| {
            for z in Zone::ALL {
                if ui.selectable_label(zone == z, z.label()).clicked() {
                    zone = z;
                }
            }
        });

        let wheel = zone.wheel(&cg);
        color_wheel(ui, ctx, &cg, zone, wheel);
        wheel_lum_slider(ui, ctx, &cg, zone, wheel.lum);

        if ui
            .button("Reset wheel")
            .on_hover_text("Reset this wheel's hue/saturation/luminance to identity")
            .clicked()
        {
            reset_wheel(ctx, &cg, zone);
        }

        ui.separator();
        blend_balance_row(ui, ctx, &cg);
    });
    ui.data_mut(|d| d.insert_temp(zone_id, zone));
}

// ─── C13: the ColorWheel widget ─────────────────────────────────────────────

/// Fine-drag sensitivity: with the modifier held, the puck moves by this
/// fraction of the raw pointer delta rather than snapping to the pointer.
const FINE_DRAG_SCALE: f32 = 0.2;
/// Coarse keyboard-nudge step (hue degrees / saturation points).
const NUDGE: f32 = 5.0;
/// Fine keyboard-nudge step (Shift held).
const NUDGE_FINE: f32 = 1.0;

/// hue-degrees/saturation → puck screen position (angle = hue, radius =
/// `sat/100 * radius`).
fn wheel_pos(center: egui::Pos2, radius: f32, hue_deg: f32, sat: f32) -> egui::Pos2 {
    let hr = hue_deg.to_radians();
    let r = (sat / 100.0).clamp(0.0, 1.0) * radius;
    center + egui::vec2(hr.cos(), hr.sin()) * r
}

/// Inverse of [`wheel_pos`]: screen position → `(hue_deg, sat)`, radius
/// clamped to the wheel (a drag past the rim saturates at 100%, never
/// escapes the control).
fn pos_to_hue_sat(center: egui::Pos2, radius: f32, pos: egui::Pos2) -> (f32, f32) {
    let v = pos - center;
    let r = v.length().min(radius.max(1.0));
    let mut hue = v.angle().to_degrees();
    if hue < 0.0 {
        hue += 360.0;
    }
    let sat = (r / radius.max(1.0) * 100.0).clamp(0.0, 100.0);
    (hue, sat)
}

/// A cheap, purely decorative HSV→sRGB8 conversion for the wheel's rainbow
/// backdrop, a UI orientation aid, NOT the product's color science (the
/// actual grading math is `nodes::global::color_grade`'s clean-room OkLCh
/// implementation; this exists only so the wheel LOOKS like a color wheel).
fn hsv_to_rgb8(h_deg: f32, s: f32, v: f32) -> egui::Color32 {
    let h = h_deg.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    egui::Color32::from_rgb(
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

/// Judgment call (design spec item 4): the rim used to be full saturation/
/// near-full value (`hsv_to_rgb8(hue, 1.0, 0.85)`), which reads garish next
/// to this theme's quiet, low-chroma chrome, nothing else in the panel
/// approaches full saturation, so the wheel used to look like it belonged
/// to a different, louder app. Desaturating to 0.82 and dimming to 0.78
/// pulls it into the same restrained register as the rest of the UI
/// (Principle 1's "quiet at rest," applied to decorative data here rather
/// than to chrome) while leaving hue, the one channel this backdrop
/// exists to communicate, completely untouched, so every band is still
/// unambiguously identifiable at a glance.
const WHEEL_RIM_SAT: f32 = 0.82;
const WHEEL_RIM_VAL: f32 = 0.78;

/// Paints the wheel's rainbow-disc backdrop: a triangle fan from a neutral
/// gray center to hue colors at the rim, using egui's per-vertex color
/// interpolation for the radial+angular gradient.
fn paint_wheel_backdrop(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    const SEGMENTS: usize = 48;
    let mut mesh = egui::Mesh::default();
    let neutral = egui::Color32::from_gray(96);
    mesh.colored_vertex(center, neutral);
    for i in 0..=SEGMENTS {
        let t = i as f32 / SEGMENTS as f32;
        let hue = t * 360.0;
        let p = wheel_pos(center, radius, hue, 100.0);
        mesh.colored_vertex(p, hsv_to_rgb8(hue, WHEEL_RIM_SAT, WHEEL_RIM_VAL));
    }
    for i in 0..SEGMENTS as u32 {
        mesh.add_triangle(0, 1 + i, 2 + i);
    }
    painter.add(egui::Shape::from(mesh));
}

/// The puck's radius, given its interaction state, reuses the house
/// slider knob's own two-tier sizing (spec §3's `KNOB_RADIUS_*`) verbatim,
/// since the puck IS a slider knob repainted onto a circular control. Pure
/// and split out from [`paint_wheel_puck`] so the state→appearance mapping
/// is unit-testable without a `Painter` (see this module's tests).
fn puck_radius(dragging: bool, hovered: bool) -> f32 {
    if dragging || hovered {
        tokens::KNOB_RADIUS_HOVER_DRAG
    } else {
        tokens::KNOB_RADIUS_REST
    }
}

/// Which glow layer stack (if any) the puck's paint order uses, given its
/// interaction state, `None` at rest (spec Principle 1: accent is silent
/// at rest). Pure, same rationale as [`puck_radius`].
fn puck_glow_layers(dragging: bool, hovered: bool) -> Option<Vec<(f32, egui::Color32)>> {
    // Owned, not `&'static`: the accent is a user setting now, so the
    // stacks are derived per call rather than living in a const. The
    // allocation happens only for the one point that is actually hovered
    // or dragged, at rest this returns `None` and allocates nothing.
    if dragging {
        Some(tokens::accent_glow_drag().to_vec())
    } else if hovered {
        Some(tokens::accent_glow_hover().to_vec())
    } else {
        None
    }
}

/// The wheel's puck, rebuilt with the house slider knob's paint language
/// (design spec §3) in place of the old hardcoded `WHITE` fill / `BLACK`
/// stroke: a neutral gradient body (never accent-tinted at rest, matching
/// the knob's own rule), a contact shadow, and, while the wheel is
/// hovered or actively dragged, the same multi-layer accent glow the
/// slider knob uses, so this control reads as part of the same family. (The
/// knob's small top-highlight arc is omitted: `theme::paint` has no
/// arc-stroke primitive, and this file's scope doesn't extend to adding
/// one.)
fn paint_wheel_puck(painter: &egui::Painter, pos: egui::Pos2, dragging: bool, hovered: bool) {
    let active = dragging || hovered;
    let r = puck_radius(dragging, hovered);
    if let Some(layers) = puck_glow_layers(dragging, hovered) {
        paint::glow_circle(painter, pos, r, &layers);
    }
    paint::contact_shadow_circle(painter, pos, r);
    let (top, bottom) = if active {
        (
            tokens::KNOB_FILL_HOVER_DRAG_TOP,
            tokens::KNOB_FILL_HOVER_DRAG_BOTTOM,
        )
    } else {
        (tokens::KNOB_FILL_REST_TOP, tokens::KNOB_FILL_REST_BOTTOM)
    };
    paint::vgradient_circle(painter, pos, r, top, bottom);
    let edge = if active {
        tokens::KNOB_EDGE_STROKE_HOVER_DRAG
    } else {
        tokens::KNOB_EDGE_STROKE_REST
    };
    painter.circle_stroke(pos, r, egui::Stroke::new(1.0, edge));
}

/// The `ColorWheel` widget (task C13): hue/sat puck over a rainbow-disc
/// backdrop, drag/fine-drag/keyboard-nudge per the module docs.
fn color_wheel(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    cg: &ColorGrade,
    zone: Zone,
    wheel: GradeWheel,
) -> egui::Response {
    let size = ui.available_width().clamp(80.0, 160.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.5 - 6.0).max(1.0);

    if response.clicked() {
        response.request_focus();
    }
    if response.drag_started() {
        response.request_focus();
        ctx.edit.begin_gesture(ParamId::ColorGrade);
    }
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let fine = ui.input(|i| i.modifiers.shift);
            let (hue, sat) = if fine {
                let cur = wheel_pos(center, radius, wheel.hue, wheel.sat);
                let target = cur + response.drag_delta() * FINE_DRAG_SCALE;
                pos_to_hue_sat(center, radius, target)
            } else {
                pos_to_hue_sat(center, radius, pos)
            };
            if (hue - wheel.hue).abs() > f32::EPSILON || (sat - wheel.sat).abs() > f32::EPSILON {
                let mut next = *cg;
                let mut w = zone.wheel(&next);
                w.hue = hue;
                w.sat = sat;
                zone.set_wheel(&mut next, w);
                ctx.edit.preview(grade_delta(next));
            }
        }
    }
    if response.drag_stopped() {
        ctx.edit.end_gesture();
    }
    if response.double_clicked() {
        reset_wheel(ctx, cg, zone);
    } else if response.clicked() {
        // A plain click (no drag) sets the puck directly under the pointer
        // in ONE gesture, the same "click vs. drag are both first-class"
        // shape `panels::curve::point_editor`'s click-add path uses.
        response.request_focus();
        if let Some(pos) = response.interact_pointer_pos() {
            let (hue, sat) = pos_to_hue_sat(center, radius, pos);
            let mut next = *cg;
            let mut w = zone.wheel(&next);
            w.hue = hue;
            w.sat = sat;
            zone.set_wheel(&mut next, w);
            ctx.edit.begin_gesture(ParamId::ColorGrade);
            ctx.edit.preview(grade_delta(next));
            ctx.edit.end_gesture();
        }
    }

    // Keyboard nudge (task C13 AC: keyboard-accessible).
    if response.has_focus() {
        let (left, right, up, down, shift) = ui.input_mut(|i| {
            let shift = i.modifiers.shift;
            let mods = if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            };
            (
                i.consume_key(mods, egui::Key::ArrowLeft),
                i.consume_key(mods, egui::Key::ArrowRight),
                i.consume_key(mods, egui::Key::ArrowUp),
                i.consume_key(mods, egui::Key::ArrowDown),
                shift,
            )
        });
        let step = if shift { NUDGE_FINE } else { NUDGE };
        let dh = (right as i32 - left as i32) as f32 * step;
        let ds = (up as i32 - down as i32) as f32 * step;
        if dh != 0.0 || ds != 0.0 {
            let mut next = *cg;
            let mut w = zone.wheel(&next);
            w.hue = (w.hue + dh).rem_euclid(360.0);
            w.sat = (w.sat + ds).clamp(0.0, 100.0);
            zone.set_wheel(&mut next, w);
            ctx.edit.begin_gesture(ParamId::ColorGrade);
            ctx.edit.preview(grade_delta(next));
            ctx.edit.end_gesture();
        }
    }

    if ui.is_rect_visible(rect) {
        let painted_cg = match ctx.edit.value(ParamId::ColorGrade) {
            ParamValue::ColorGrade(cg) => cg,
            _ => ColorGrade::default(),
        };
        let painted_wheel = zone.wheel(&painted_cg);
        let painter = ui.painter().with_clip_rect(rect);
        paint_wheel_backdrop(&painter, center, radius);
        // Inset bevel ring (design spec item 4): so the wheel reads as a
        // recessed dial, not a flat disc floating on the panel. This is a
        // circular restatement of `paint::bevel_inset`'s dark-top/
        // light-bottom pair, that helper's own primitive is two straight
        // hlines (built for rectangular wells), which doesn't apply to a
        // round surface; a uniform two-tone groove (rather than a true
        // directional top-left-lit arc, which would need custom `Path`
        // arcs epaint doesn't provide a helper for) is the simplification
        // chosen here.
        painter.circle_stroke(
            center,
            radius + 1.5,
            egui::Stroke::new(tokens::STROKE_BEVEL, tokens::BEVEL_INSET_TOP),
        );
        painter.circle_stroke(
            center,
            radius,
            egui::Stroke::new(tokens::STROKE_BEVEL, tokens::BEVEL_INSET_BOTTOM),
        );
        let puck = wheel_pos(center, radius, painted_wheel.hue, painted_wheel.sat);
        paint_wheel_puck(
            &painter,
            puck,
            response.dragged(),
            response.hovered() && !response.dragged(),
        );
        if response.has_focus() {
            // Crisp, un-blurred focus ring (spec §5), distinct from the
            // puck's soft decorative hover/drag glow above.
            painter.rect_stroke(
                rect,
                radius,
                egui::Stroke::new(tokens::STROKE_FOCUS_RING, tokens::accent_focus_ring()),
                egui::StrokeKind::Inside,
            );
        }
    }

    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("{} color wheel", zone.label()),
        )
    });
    response
}

/// The active wheel's Luminance slider (mirrors `panels::hsl::band_slider`'s
/// decompose-a-coarse-leaf shape).
fn wheel_lum_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    cg: &ColorGrade,
    zone: Zone,
    value: f32,
) {
    let spec = SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label: "Luminance",
        unit: None,
    };
    let (_, events) = value_slider(ui, ("develop.grading.lum", zone), value as f64, &spec);
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::ColorGrade),
            SliderEvent::Preview(v) => {
                let mut next = *cg;
                let mut w = zone.wheel(&next);
                w.lum = v as f32;
                zone.set_wheel(&mut next, w);
                ctx.edit.preview(grade_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => {
                let mut next = *cg;
                let mut w = zone.wheel(&next);
                w.lum = 0.0;
                zone.set_wheel(&mut next, w);
                ctx.edit.begin_gesture(ParamId::ColorGrade);
                ctx.edit.preview(grade_delta(next));
                ctx.edit.end_gesture();
            }
            SliderEvent::Commit(v) => {
                let mut next = *cg;
                let mut w = zone.wheel(&next);
                w.lum = v as f32;
                zone.set_wheel(&mut next, w);
                ctx.edit.begin_gesture(ParamId::ColorGrade);
                ctx.edit.preview(grade_delta(next));
                ctx.edit.end_gesture();
            }
        }
    }
}

/// Per-wheel Reset (task C13 AC): restores ONLY `zone`'s wheel to identity
/// blending/balance and the other three wheels are untouched.
fn reset_wheel(ctx: &mut DevelopCtx<'_>, cg: &ColorGrade, zone: Zone) {
    let mut next = *cg;
    zone.set_wheel(&mut next, GradeWheel::default());
    ctx.edit.begin_gesture(ParamId::ColorGrade);
    ctx.edit.preview(grade_delta(next));
    ctx.edit.end_gesture();
}

// ─── Shared blending/balance sliders ────────────────────────────────────────

fn blend_balance_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, cg: &ColorGrade) {
    let blend_spec = SliderSpec {
        min: 0.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label: "Blending",
        unit: None,
    };
    let (_, events) = value_slider(ui, "develop.grading.blend", cg.blend as f64, &blend_spec);
    for ev in events {
        apply_blend_balance(ctx, cg, ev, |g, v| g.blend = v as f32, 0.0);
    }

    let balance_spec = SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label: "Balance",
        unit: None,
    };
    let (_, events) = value_slider(
        ui,
        "develop.grading.balance",
        cg.balance as f64,
        &balance_spec,
    );
    for ev in events {
        apply_blend_balance(ctx, cg, ev, |g, v| g.balance = v as f32, 0.0);
    }
}

fn apply_blend_balance(
    ctx: &mut DevelopCtx<'_>,
    cg: &ColorGrade,
    ev: SliderEvent,
    recompose: impl Fn(&mut ColorGrade, f64),
    default: f64,
) {
    match ev {
        SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::ColorGrade),
        SliderEvent::Preview(v) => {
            let mut next = *cg;
            recompose(&mut next, v);
            ctx.edit.preview(grade_delta(next));
        }
        SliderEvent::End => ctx.edit.end_gesture(),
        SliderEvent::Reset => {
            let mut next = *cg;
            recompose(&mut next, default);
            ctx.edit.begin_gesture(ParamId::ColorGrade);
            ctx.edit.preview(grade_delta(next));
            ctx.edit.end_gesture();
        }
        SliderEvent::Commit(v) => {
            let mut next = *cg;
            recompose(&mut next, v);
            ctx.edit.begin_gesture(ParamId::ColorGrade);
            ctx.edit.preview(grade_delta(next));
            ctx.edit.end_gesture();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui_kittest::{kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;

    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(
                ParamId::ColorGrade,
                ParamValue::ColorGrade(ColorGrade::default()),
            );
            values.insert(ParamId::Treatment, ParamValue::Treatment(Treatment::Color));
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or_else(|| ParamValue::ColorGrade(ColorGrade::default()))
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::ColorGrade(ColorGrade::default())
        }
        fn begin_gesture(&mut self, _p: ParamId) {}
        fn preview(&mut self, d: ParamDelta) {
            for (id, v) in d.0 {
                self.values.insert(id, v);
            }
        }
        fn end_gesture(&mut self) {}
        fn reset(&mut self, p: ParamId) {
            let d = self.default(p);
            self.values.insert(p, d);
        }
        fn recipe_rev(&self) -> u64 {
            0
        }
        fn can_undo(&self) -> bool {
            false
        }
        fn can_redo(&self) -> bool {
            false
        }
        fn undo(&mut self) {}
        fn redo(&mut self) {}
        fn history(&self) -> &[HistoryStepMeta] {
            &[]
        }
        fn restore_step(&mut self, _seq: u64) {}
        fn clear_history(&mut self) {}
        fn snapshots(&self) -> &[SnapshotMeta] {
            &[]
        }
        fn create_snapshot(&mut self, _name: &str) {}
        fn restore_snapshot(&mut self, _snapshot: SnapshotId) {}
        fn rename_snapshot(&mut self, _snapshot: SnapshotId, _name: &str) {}
    }

    fn cg_of(binding: &RecordingBinding) -> ColorGrade {
        match binding.value(ParamId::ColorGrade) {
            ParamValue::ColorGrade(cg) => cg,
            _ => ColorGrade::default(),
        }
    }

    struct PanelApp {
        binding: RecordingBinding,
    }

    fn panel_harness() -> Harness<'static, PanelApp> {
        let app = PanelApp {
            binding: RecordingBinding::new(),
        };
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, app: &mut PanelApp| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut app.binding,
                    gizmos: &mut gizmos,
                };
                build(ui, &mut ctx);
            }),
            app,
        );
        harness.set_size(egui::vec2(320.0, 420.0));
        // `themed_state` binds the weight-role named font families before
        // frame 0 paints (see `theme::test_support` for why that matters).
        harness
    }

    // ── pure math ────────────────────────────────────────────────────────

    #[test]
    fn wheel_pos_and_pos_to_hue_sat_round_trip() {
        let center = egui::pos2(50.0, 50.0);
        let radius = 40.0;
        for hue in [0.0f32, 30.0, 90.0, 180.0, 270.0, 359.0] {
            for sat in [0.0f32, 25.0, 50.0, 75.0, 100.0] {
                let p = wheel_pos(center, radius, hue, sat);
                let (h2, s2) = pos_to_hue_sat(center, radius, p);
                if sat > 0.5 {
                    // Hue is ill-defined exactly at the center (sat≈0).
                    let dh = (h2 - hue + 540.0).rem_euclid(360.0) - 180.0;
                    assert!(dh.abs() < 0.1, "hue={hue} sat={sat}: got hue={h2}");
                }
                assert!((s2 - sat).abs() < 0.1, "hue={hue} sat={sat}: got sat={s2}");
            }
        }
    }

    #[test]
    fn pos_to_hue_sat_clamps_beyond_the_rim() {
        let center = egui::pos2(0.0, 0.0);
        let (_, sat) = pos_to_hue_sat(center, 40.0, egui::pos2(1000.0, 0.0));
        assert!((sat - 100.0).abs() < 1e-3, "sat={sat}");
    }

    // ── Re-skin: wheel rim judgment call + puck state → appearance ────────

    #[test]
    fn wheel_rim_saturation_and_value_are_tightened_below_full() {
        // Design judgment call (spec item 4): the rim must sit below full
        // saturation/value (the old 1.0/0.85 read garish against this
        // theme's neutral chrome) but not so desaturated that hue becomes
        // hard to read, pins the chosen range so a future edit doesn't
        // silently drift back toward either extreme.
        // Both are `const`, so clippy (rightly) wants the check as a
        // compile-time assertion rather than a runtime one.
        const { assert!(WHEEL_RIM_SAT > 0.6 && WHEEL_RIM_SAT < 1.0) };
        const { assert!(WHEEL_RIM_VAL > 0.6 && WHEEL_RIM_VAL < 1.0) };
    }

    #[test]
    fn hsv_to_rgb8_at_rim_saturation_still_orders_channels_by_hue() {
        // Hue legibility check: tightening the rim's saturation/value must
        // not blur which hue a color reads as, the dominant-channel
        // ordering for a representative hue (red, 0°) must match what
        // full-saturation would give.
        let full = hsv_to_rgb8(0.0, 1.0, 1.0);
        let rim = hsv_to_rgb8(0.0, WHEEL_RIM_SAT, WHEEL_RIM_VAL);
        assert!(full.r() > full.g() && full.r() > full.b());
        assert!(rim.r() > rim.g() && rim.r() > rim.b());
    }

    #[test]
    fn puck_radius_is_active_size_while_dragging_or_hovered_rest_size_otherwise() {
        assert_eq!(puck_radius(false, false), tokens::KNOB_RADIUS_REST);
        assert_eq!(puck_radius(true, false), tokens::KNOB_RADIUS_HOVER_DRAG);
        assert_eq!(puck_radius(false, true), tokens::KNOB_RADIUS_HOVER_DRAG);
    }

    #[test]
    fn puck_glow_layers_are_silent_at_rest_and_pick_drag_over_hover() {
        assert!(
            puck_glow_layers(false, false).is_none(),
            "spec Principle 1: accent must be silent at rest"
        );
        assert_eq!(
            puck_glow_layers(false, true),
            Some(tokens::accent_glow_hover().to_vec())
        );
        assert_eq!(
            puck_glow_layers(true, false),
            Some(tokens::accent_glow_drag().to_vec())
        );
    }

    // ── C13 AC: panel-reflects-recipe (no widget-local state) ─────────────

    #[test]
    fn panel_always_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        let mut cg = ColorGrade::default();
        cg.shadows.lum = 22.0;
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::ColorGrade, ParamValue::ColorGrade(cg));
        harness.run();
        harness.get_by_label("+22");

        cg.shadows.lum = -8.0;
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::ColorGrade, ParamValue::ColorGrade(cg));
        harness.run();
        harness.get_by_label("-8");
    }

    // ── C13 AC: gesture-commit ─────────────────────────────────────────────

    /// A real synthetic click on the wheel (kittest pointer down+up at the
    /// SAME position, no movement, so egui reports `clicked()` not
    /// `dragged()`) commits a `ParamId::ColorGrade` delta through one
    /// gesture and moves the active zone's hue/sat off identity.
    #[test]
    fn a_real_synthetic_click_on_the_wheel_commits_a_grade_gesture() {
        let mut harness = panel_harness();
        harness.run();
        let wheel = harness.get_by_label_contains("Shadows color wheel");
        let rect = wheel.rect();
        // Click well off-center (near the rim) so saturation moves clearly.
        let target = egui::pos2(rect.right() - 4.0, rect.center().y);

        harness.hover_at(target);
        harness.run();
        harness.drag_at(target);
        harness.run();
        harness.drop_at(target);
        harness.run();

        let cg = cg_of(&harness.state().binding);
        assert!(
            cg.shadows.sat > 50.0,
            "clicking near the rim should push saturation up: sat={}",
            cg.shadows.sat
        );
    }

    /// Keyboard nudge (task C13 AC: keyboard-accessible), a real synthetic
    /// click focuses the wheel, then arrow keys nudge hue/saturation.
    #[test]
    fn arrow_keys_nudge_the_focused_wheel() {
        let mut harness = panel_harness();
        harness.run();
        let wheel = harness.get_by_label_contains("Shadows color wheel");
        wheel.focus();
        harness.run();
        harness.key_press(egui::Key::ArrowUp);
        harness.run();

        let cg = cg_of(&harness.state().binding);
        assert!(
            cg.shadows.sat > 0.0,
            "ArrowUp on a focused wheel should raise saturation: sat={}",
            cg.shadows.sat
        );
    }

    /// Per-wheel reset (task C13 AC): resets only the active zone's wheel.
    #[test]
    fn reset_wheel_only_touches_the_active_zone() {
        let mut binding = RecordingBinding::new();
        let cg = ColorGrade {
            shadows: GradeWheel {
                hue: 200.0,
                sat: 40.0,
                lum: 10.0,
            },
            highlights: GradeWheel {
                hue: 40.0,
                sat: 30.0,
                lum: -5.0,
            },
            ..ColorGrade::default()
        };
        binding.preview(grade_delta(cg));

        {
            let mut gizmos = GizmoLayer::new();
            let mut ctx = DevelopCtx {
                source_kind: lightbox_types::SourceKind::Rendered,
                edit: &mut binding,
                gizmos: &mut gizmos,
            };
            reset_wheel(&mut ctx, &cg, Zone::Shadows);
        }

        let after = cg_of(&binding);
        assert_eq!(after.shadows, GradeWheel::default(), "active zone reset");
        assert_eq!(after.highlights.hue, 40.0, "untouched zone preserved");
        assert_eq!(after.highlights.sat, 30.0);
        assert_eq!(after.highlights.lum, -5.0);
    }

    /// The panel mounts as a registered [`PanelDef`] with the expected
    /// identity/order, a probe that `def()` is wired the way `lib.rs`
    /// expects (order between hsl=40 and history=90).
    #[test]
    fn def_is_registered_between_hsl_and_history() {
        let d = def();
        assert_eq!(d.id.0, "develop.grading");
        assert!(d.order > 40 && d.order < 90);
    }

    /// **D2 AC ("color panels visibly disabled in Monochrome")**, the
    /// wiring half: with a real accessible-tree probe (not just the
    /// `is_monochrome` unit test in `panels::bw`), the Shadows wheel's node
    /// carries AccessKit's disabled state once the binding's Treatment
    /// flips to Monochrome, and is enabled again for Color.
    #[test]
    fn panel_disables_the_wheel_editor_when_monochrome() {
        use egui_kittest::kittest::NodeT as _;

        let mut harness = panel_harness();
        harness.run();
        let wheel = harness.get_by_label_contains("Shadows color wheel");
        assert!(
            !wheel.accesskit_node().is_disabled(),
            "Color treatment (the default): the wheel editor stays interactive"
        );

        harness.state_mut().binding.values.insert(
            ParamId::Treatment,
            ParamValue::Treatment(Treatment::BlackAndWhite),
        );
        harness.run();
        let wheel = harness.get_by_label_contains("Shadows color wheel");
        assert!(
            wheel.accesskit_node().is_disabled(),
            "Monochrome treatment: the color-grading wheel editor must be visibly disabled (D2 \
             AC)"
        );
    }
}
