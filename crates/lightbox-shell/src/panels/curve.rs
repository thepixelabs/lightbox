// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E7/E10 Phase C (tasks **C4-C5**), the tone-curve widget (`develop.curve`):
//! point + parametric curve editing, bound to `ParamId::ToneCurve`'s ONE
//! `ToneCurveSet` unit (composite/R/G/B point curves + the parametric
//! region sliders, `lightbox-edit`'s `ToneCurveSet`).
//!
//! # What E08's E7 landing already gave this widget (extended here, not
//! re-authored)
//!
//! click-empty-space-adds-a-point, drag-an-existing-point-moves-it (one
//! gesture per drag), double-click-deletes (while `> 2` points remain),
//! monotone-x clamping via [`clamp_drag`]/[`x_bounds`]/[`insert_point`] (so
//! `ToneCurve::validate`'s strictly-increasing invariant can never be
//! violated through the UI), ↑/↓ keyboard nudge on the selected point. All
//! of that core interaction code is kept verbatim; C4/C5 add:
//!
//! - **C4**: a histogram backdrop hook ([`paint_histogram_backdrop`], inert
//!   until E10 Phase D's histogram compute pass exists, see its own doc
//!   comment), snap-to-grid while holding Ctrl/Cmd during a drag
//!   ([`snap_to_grid`]), and a per-curve Reset button.
//! - **C5**: channel tabs (RGB/Red/Green/Blue, the composite curve is
//!   labeled "RGB", not the spec's "Luma": see
//!   `nodes::global::tone_curve`'s module docs in `lightbox-render` for why
//!   the composite curve is applied per-channel, not via a luma ratio) and a
//!   Point/Parametric mode toggle (parametric only exists on the composite
//!   channel, Lightroom's own Region sliders are master-curve-only). The
//!   graph now draws the REAL composed shape (point spline + parametric
//!   delta, sampled from `lightbox-render`'s own
//!   `nodes::common::curve1d::{MonotoneCubic, parametric_delta}`, the exact
//!   functions `ToneCurveNode` bakes its LUTs from, so this is a true
//!   WYSIWYG preview, not a re-derived approximation) instead of E08's
//!   straight point-to-point segments (which were deliberately naive: "the
//!   engine owns the real interpolation", that interpolation now exists).
//!
//! M1 edited the **composite RGB** curve only; per-channel and parametric
//! editing were named as this epic's follow-on and land here.

use eframe::egui;
use lightbox_edit::{
    CurvePoint, ParamDelta, ParamId, ParamValue, ParametricCurve, ToneCurve, ToneCurveSet,
    MAX_CURVE_POINTS, MIN_SPLIT_GAP,
};
use lightbox_render::ng::nodes::common::curve1d::{parametric_delta, MonotoneCubic};
// E10 D12/D13: the live histogram backdrop's data source, see
// `paint_histogram_backdrop`'s doc comment (this landing).
use lightbox_render::ng::HistogramData;

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};
use crate::theme::{paint, tokens};

/// The curve panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.curve");

/// Minimum x separation between adjacent control points (≈ half an 8-bit
/// step): keeps `ToneCurve::validate`'s strict monotone-x invariant with
/// headroom, and keeps points visually separable.
pub const MIN_X_GAP: f32 = 1.0 / 256.0;

/// Hit radius around a control point, in screen points.
const HIT_RADIUS_PT: f32 = 10.0;

/// Keyboard nudge steps (normalized 0..1 output space).
const NUDGE: f32 = 0.02;
const NUDGE_FINE: f32 = 0.005;

/// Snap grid step for Ctrl/Cmd-held drags (C4 "snap" affordance), coarse
/// enough to be a deliberate snap, fine enough to place a point at any
/// eighth-stop-ish position.
const SNAP_STEP: f32 = 0.05;

/// Registration (E1): mounted by `lib.rs` at app construction.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Tone Curve",
        source_req: SourceReq::Any,
        order: 30,
        build,
    }
}

// ─── C5: channels + mode ────────────────────────────────────────────────────

/// Which of [`ToneCurveSet`]'s four curves the point editor is showing.
/// `Rgb` is the composite/master curve (spec's "Luma" row label, see the
/// module docs for why this codebase applies it per-channel, not via luma).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Channel {
    Rgb,
    R,
    G,
    B,
}

impl Channel {
    const ALL: [Channel; 4] = [Channel::Rgb, Channel::R, Channel::G, Channel::B];

    fn label(self) -> &'static str {
        match self {
            Channel::Rgb => "RGB",
            Channel::R => "Red",
            Channel::G => "Green",
            Channel::B => "Blue",
        }
    }

    fn curve(self, set: &ToneCurveSet) -> &ToneCurve {
        match self {
            Channel::Rgb => &set.rgb,
            Channel::R => &set.r,
            Channel::G => &set.g,
            Channel::B => &set.b,
        }
    }

    fn set_curve(self, set: &mut ToneCurveSet, c: ToneCurve) {
        match self {
            Channel::Rgb => set.rgb = c,
            Channel::R => set.r = c,
            Channel::G => set.g = c,
            Channel::B => set.b = c,
        }
    }
}

/// Point editing (E08's E7 UI) vs. the parametric region-slider UI (E10
/// C5). Composed together on the rendered pixel (spec C2/C3) regardless of
/// which one is on screen, this is purely a VIEW selection, never a
/// mutually-exclusive data mode (co-existence is tested below).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Point,
    Parametric,
}

// ─── Pure point-editing math (E7's testable core, extended by C4) ──────────

/// The x-range `points[idx]` may occupy without crossing its neighbors.
fn x_bounds(points: &[CurvePoint], idx: usize) -> (f32, f32) {
    let lo = if idx == 0 {
        0.0
    } else {
        points[idx - 1].x + MIN_X_GAP
    };
    let hi = if idx + 1 == points.len() {
        1.0
    } else {
        points[idx + 1].x - MIN_X_GAP
    };
    (lo, hi.max(lo))
}

/// Snaps `p` onto the [`SNAP_STEP`] grid (C4's "snap" affordance), the
/// caller applies [`clamp_drag`] afterward, so a snapped point can never
/// escape its neighbor-bounded x-range either.
fn snap_to_grid(p: CurvePoint) -> CurvePoint {
    CurvePoint {
        x: (p.x / SNAP_STEP).round() * SNAP_STEP,
        y: (p.y / SNAP_STEP).round() * SNAP_STEP,
    }
}

/// Clamps a drag of `points[idx]` to `(x, y)`: x within neighbor bounds
/// (monotone-x), both into `[0,1]`.
fn clamp_drag(points: &[CurvePoint], idx: usize, x: f32, y: f32) -> CurvePoint {
    let (lo, hi) = x_bounds(points, idx);
    CurvePoint {
        x: x.clamp(lo, hi),
        y: y.clamp(0.0, 1.0),
    }
}

/// Inserts a point at `(x, y)` keeping x sorted with [`MIN_X_GAP`] spacing;
/// `None` when the curve is full or no legal slot exists at `x`.
fn insert_point(points: &mut Vec<CurvePoint>, x: f32, y: f32) -> Option<usize> {
    if points.len() >= MAX_CURVE_POINTS {
        return None;
    }
    let x = x.clamp(0.0, 1.0);
    let idx = points.partition_point(|p| p.x < x);
    let lo = if idx == 0 {
        0.0
    } else {
        points[idx - 1].x + MIN_X_GAP
    };
    let hi = if idx == points.len() {
        1.0
    } else {
        points[idx].x - MIN_X_GAP
    };
    if lo > hi {
        return None; // no room between the neighbors
    }
    points.insert(
        idx,
        CurvePoint {
            x: x.clamp(lo, hi),
            y: y.clamp(0.0, 1.0),
        },
    );
    Some(idx)
}

/// Whether a point may be deleted (a curve keeps ≥ 2 points, the linear
/// identity's endpoints are the floor).
fn can_delete(points: &[CurvePoint]) -> bool {
    points.len() > 2
}

/// Clamps a drag of `splits[idx]` (C5 draggable split handles) to stay
/// `MIN_SPLIT_GAP`-separated from its neighbors (and the `0`/`1` edges)
/// the exact same "clamp against the bounded neighbor range" discipline
/// [`clamp_drag`] uses for control points, so split handles can no more
/// cross each other through the UI than curve points can.
fn clamp_split(splits: &[f32; 3], idx: usize, x: f32) -> f32 {
    let lo = if idx == 0 {
        MIN_SPLIT_GAP
    } else {
        splits[idx - 1] + MIN_SPLIT_GAP
    };
    let hi = if idx == 2 {
        1.0 - MIN_SPLIT_GAP
    } else {
        splits[idx + 1] - MIN_SPLIT_GAP
    };
    x.clamp(lo, hi.max(lo))
}

// ─── The widget ──────────────────────────────────────────────────────────────

fn curve_delta(set: ToneCurveSet) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::ToneCurve, ParamValue::Curve(set));
    delta
}

/// curve-space (0..1, y up) → screen.
fn to_screen(rect: egui::Rect, x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    )
}

/// screen → curve-space (unclamped; callers clamp).
fn to_curve(rect: egui::Rect, pos: egui::Pos2) -> (f32, f32) {
    (
        (pos.x - rect.left()) / rect.width().max(1.0),
        (rect.bottom() - pos.y) / rect.height().max(1.0),
    )
}

fn hit_point(points: &[CurvePoint], rect: egui::Rect, pos: egui::Pos2) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| (i, to_screen(rect, p.x, p.y).distance(pos)))
        .filter(|(_, d)| *d <= HIT_RADIUS_PT)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

/// Curve/parametric control-point radii (design spec §3's house-slider-knob
/// language, sized down from the knob's own `KNOB_RADIUS_*`, a curve
/// editor carries many points on screen at once, where the slider carries
/// exactly one).
const POINT_RADIUS_REST: f32 = 3.5;
const POINT_RADIUS_ACTIVE: f32 = 4.5;

/// The radius a control point/split handle paints at, given its
/// interaction state. Pure and split out from [`paint_control_point`]
/// specifically so the state→appearance mapping is unit-testable without a
/// `Painter` (see this module's tests).
fn point_radius(dragging: bool, hovered: bool) -> f32 {
    if dragging || hovered {
        POINT_RADIUS_ACTIVE
    } else {
        POINT_RADIUS_REST
    }
}

/// Which glow layer stack (if any) a control point/split handle's paint
/// order uses, given its interaction state, `None` at rest (spec
/// Principle 1: "quiet at rest," accent is silent until something is
/// hovered or being dragged). Pure, same rationale as [`point_radius`].
fn point_glow_layers(dragging: bool, hovered: bool) -> Option<Vec<(f32, egui::Color32)>> {
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

/// Paints one draggable control point or parametric split handle with the
/// house slider knob's paint order (spec §3), reused so every small
/// draggable control in this widget reads as one family: every point gets
/// a contact shadow and a neutral gradient body ("real presence" per the
/// design brief, the body is deliberately never accent-tinted, matching
/// the knob's own "neutral chrome at rest" rule); a hovered or actively
/// dragged point additionally gets a multi-layer accent glow
/// (`paint::glow_circle`, the same layer stacks the knob uses); and a
/// point that carries a persistent selection (`selected`; split handles
/// never do, they only drag) gets a crisp, un-blurred accent ring so it
/// stays visually distinct from a merely-hovered one.
fn paint_control_point(
    painter: &egui::Painter,
    pos: egui::Pos2,
    dragging: bool,
    hovered: bool,
    selected: bool,
) {
    let active = dragging || hovered;
    let r = point_radius(dragging, hovered);
    if let Some(layers) = point_glow_layers(dragging, hovered) {
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
    if selected {
        painter.circle_stroke(
            pos,
            r + 2.0,
            egui::Stroke::new(tokens::STROKE_SELECTION_BORDER, tokens::accent()),
        );
    }
}

/// The composed pixel-affecting shape for `channel` (task C5 "true WYSIWYG
/// preview"): the point curve's monotone-cubic interpolant, plus (for the
/// composite `Rgb` channel only) the parametric region delta at the SAME
/// input x, literally the formula `nodes::global::tone_curve` bakes into
/// its LUTs, called here rather than re-derived.
fn sample_composed(channel: Channel, set: &ToneCurveSet, samples: usize) -> Vec<(f32, f32)> {
    let spline = MonotoneCubic::build(&channel.curve(set).points);
    let n = samples.max(1);
    (0..=n)
        .map(|i| {
            let x = i as f32 / n as f32;
            let y = if channel == Channel::Rgb {
                (spline.eval(x) + parametric_delta(x, &set.parametric)).clamp(0.0, 1.0)
            } else {
                spline.eval(x).clamp(0.0, 1.0)
            };
            (x, y)
        })
        .collect()
}

/// D12/D13 landing: `channel`'s plane of `data` (luma for the composite
/// `Rgb` curve, matching the tone-curve panel's own semantics, see the
/// module doc on why the composite curve is per-channel, not luma-applied
/// but the BACKDROP still shows luma there, Lightroom's own convention for
/// the master-curve histogram), normalized `u32` counts as `f32` heights.
/// Empty (so [`paint_histogram_backdrop`] no-ops) before the canvas's first
/// frame publishes a reduction.
fn histogram_bins_for(channel: Channel, data: Option<&HistogramData>) -> Vec<f32> {
    let Some(data) = data else {
        return Vec::new();
    };
    let bins: &[u32] = match channel {
        Channel::Rgb => &data.luma,
        Channel::R => &data.r,
        Channel::G => &data.g,
        Channel::B => &data.b,
    };
    bins.iter().map(|&c| c as f32).collect()
}

/// Paints a simple normalized-height bar backdrop behind the curve chart
/// (C4 "histogram backdrop"; D12/D13 fed it real bins via
/// [`histogram_bins_for`], this drawing primitive itself is unchanged from
/// C4, exactly the "one-line call-site change" its original doc promised).
fn paint_histogram_backdrop(painter: &egui::Painter, rect: egui::Rect, bins: &[f32]) {
    if bins.is_empty() {
        return;
    }
    let n = bins.len();
    let max = bins.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    let bar_w = rect.width() / n as f32;
    let color = egui::Color32::from_white_alpha(24);
    for (i, &v) in bins.iter().enumerate() {
        let h = (v / max).clamp(0.0, 1.0) * rect.height();
        let x0 = rect.left() + i as f32 * bar_w;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x0, rect.bottom() - h),
            egui::pos2(x0 + bar_w, rect.bottom()),
        );
        painter.rect_filled(bar, 0.0, color);
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let set = match ctx.edit.value(ParamId::ToneCurve) {
        ParamValue::Curve(set) => set,
        _ => ToneCurveSet::default(),
    };

    let base_id = ui.make_persistent_id("develop.curve.widget");
    let channel_id = base_id.with("channel");
    let mode_id = base_id.with("mode");
    let mut channel: Channel = ui.data(|d| d.get_temp(channel_id)).unwrap_or(Channel::Rgb);
    let mut mode: Mode = ui.data(|d| d.get_temp(mode_id)).unwrap_or(Mode::Point);
    if channel != Channel::Rgb {
        // Parametric sliders only ever act on the composite curve (module
        // docs), a non-RGB channel always shows the point editor.
        mode = Mode::Point;
    }

    ui.horizontal(|ui| {
        for c in Channel::ALL {
            if ui.selectable_label(channel == c, c.label()).clicked() {
                channel = c;
            }
        }
    });
    if channel == Channel::Rgb {
        ui.horizontal(|ui| {
            for (m, label) in [(Mode::Point, "Point"), (Mode::Parametric, "Parametric")] {
                if ui.selectable_label(mode == m, label).clicked() {
                    mode = m;
                }
            }
        });
    }
    ui.data_mut(|d| {
        d.insert_temp(channel_id, channel);
        d.insert_temp(mode_id, mode);
    });

    match mode {
        Mode::Point => point_editor(ui, ctx, &set, channel, base_id),
        Mode::Parametric => parametric_editor(ui, ctx, &set, base_id),
    }

    if ui
        .button("Reset")
        .on_hover_text("Reset this curve to identity")
        .clicked()
    {
        reset_active(ctx, &set, channel, mode);
    }
}

/// C4's Reset button: resets ONLY the curve/mode currently on screen, never
/// the whole `ToneCurveSet` (a channel-blind reset would silently discard a
/// user's work on the other three curves), one gesture, one history step.
fn reset_active(ctx: &mut DevelopCtx<'_>, set: &ToneCurveSet, channel: Channel, mode: Mode) {
    let mut next = set.clone();
    if channel == Channel::Rgb && mode == Mode::Parametric {
        next.parametric = ParametricCurve::default();
    } else {
        channel.set_curve(&mut next, ToneCurve::default());
    }
    ctx.edit.begin_gesture(ParamId::ToneCurve);
    ctx.edit.preview(curve_delta(next));
    ctx.edit.end_gesture();
}

// ─── C4 (extended for C5 channels): the point-curve editor ─────────────────

fn point_editor(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    set: &ToneCurveSet,
    channel: Channel,
    base_id: egui::Id,
) {
    let points = channel.curve(set).points.clone();

    let width = ui.available_width().max(80.0);
    let height = width.min(180.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
    // Per-channel id salt: switching channels must never leak one channel's
    // drag/selection state into another's (C5 "switching channels preserves
    // per-channel state", the underlying DATA already is per-channel via
    // `ToneCurveSet`'s 4 fields; this keeps the ephemeral UI selection state
    // equally well-scoped).
    let id = base_id.with(channel);
    let drag_id = id.with("drag");
    let sel_id = id.with("sel");
    let dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let mut selected: Option<usize> = ui.data(|d| d.get_temp(sel_id));
    // Clamp stale indices (undo/restore can shrink the point list between
    // frames).
    if selected.is_some_and(|i| i >= points.len()) {
        selected = None;
    }

    let snap_active = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);

    // ── Interactions (each drag = ONE gesture; add/delete/nudge are
    //    one-shot gestures, spec §6.6's "a drag maps to exactly one
    //    EditBinding gesture" convention, applied to a panel widget). ──
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            match dragging {
                Some(idx) if idx < points.len() => {
                    let (x, y) = to_curve(rect, pos);
                    let raw = if snap_active {
                        snap_to_grid(CurvePoint { x, y })
                    } else {
                        CurvePoint { x, y }
                    };
                    let p = clamp_drag(&points, idx, raw.x, raw.y);
                    if points[idx] != p {
                        let mut next = set.clone();
                        let mut c = channel.curve(&next).clone();
                        c.points[idx] = p;
                        channel.set_curve(&mut next, c);
                        ctx.edit.preview(curve_delta(next));
                    }
                }
                Some(_) => {} // stale index: ignore until drag_stopped
                None => {
                    // First drag frame: grab a point, or add one and drag it.
                    let (x, y) = to_curve(rect, pos);
                    let target = match hit_point(&points, rect, pos) {
                        Some(idx) => {
                            ctx.edit.begin_gesture(ParamId::ToneCurve);
                            Some(idx)
                        }
                        None => {
                            let mut next = set.clone();
                            let mut c = channel.curve(&next).clone();
                            let inserted = insert_point(&mut c.points, x, y);
                            match inserted {
                                Some(idx) => {
                                    channel.set_curve(&mut next, c);
                                    ctx.edit.begin_gesture(ParamId::ToneCurve);
                                    ctx.edit.preview(curve_delta(next));
                                    Some(idx)
                                }
                                None => None,
                            }
                        }
                    };
                    if let Some(idx) = target {
                        ui.data_mut(|d| {
                            d.insert_temp(drag_id, idx);
                            d.insert_temp(sel_id, idx);
                        });
                        selected = Some(idx);
                    }
                }
            }
        }
    }
    if response.drag_stopped() {
        if dragging.is_some() {
            ctx.edit.end_gesture();
        }
        ui.data_mut(|d| d.remove::<usize>(drag_id));
    }
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(idx) = hit_point(&points, rect, pos) {
                if can_delete(&points) {
                    let mut next = set.clone();
                    let mut c = channel.curve(&next).clone();
                    c.points.remove(idx);
                    channel.set_curve(&mut next, c);
                    ctx.edit.begin_gesture(ParamId::ToneCurve);
                    ctx.edit.preview(curve_delta(next));
                    ctx.edit.end_gesture();
                    ui.data_mut(|d| d.remove::<usize>(sel_id));
                    selected = None;
                }
            }
        }
    } else if response.clicked() {
        response.request_focus();
        if let Some(pos) = response.interact_pointer_pos() {
            match hit_point(&points, rect, pos) {
                Some(idx) => {
                    ui.data_mut(|d| d.insert_temp(sel_id, idx));
                    selected = Some(idx);
                }
                None => {
                    // Click-add (one-shot gesture); the new point becomes
                    // the selection.
                    let (x, y) = to_curve(rect, pos);
                    let mut next = set.clone();
                    let mut c = channel.curve(&next).clone();
                    if let Some(idx) = insert_point(&mut c.points, x, y) {
                        channel.set_curve(&mut next, c);
                        ctx.edit.begin_gesture(ParamId::ToneCurve);
                        ctx.edit.preview(curve_delta(next));
                        ctx.edit.end_gesture();
                        ui.data_mut(|d| d.insert_temp(sel_id, idx));
                        selected = Some(idx);
                    }
                }
            }
        }
    }
    // Keyboard nudge on the selected point (y only: ←/→ are nav chords).
    if response.has_focus() {
        if let Some(idx) = selected.filter(|i| *i < points.len()) {
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
            let step = if shift { NUDGE_FINE } else { NUDGE };
            let dy = (up as i32 - down as i32) as f32 * step;
            if dy != 0.0 {
                let p = points[idx];
                let np = clamp_drag(&points, idx, p.x, p.y + dy);
                if np != p {
                    let mut next = set.clone();
                    let mut c = channel.curve(&next).clone();
                    c.points[idx] = np;
                    channel.set_curve(&mut next, c);
                    ctx.edit.begin_gesture(ParamId::ToneCurve);
                    ctx.edit.preview(curve_delta(next));
                    ctx.edit.end_gesture();
                }
            }
        }
    }

    // ── Paint. ────────────────────────────────────────────────────────────
    if ui.is_rect_visible(rect) {
        // Re-read the (possibly just previewed) set for painting.
        let painted_set = match ctx.edit.value(ParamId::ToneCurve) {
            ParamValue::Curve(set) => set,
            _ => ToneCurveSet::default(),
        };
        let points = channel.curve(&painted_set).points.clone();
        let painter = ui.painter().with_clip_rect(rect);
        // Recessed well (design spec §1/§4): fill + inset bevel (painted
        // last, over the content), the same treatment as the histogram
        // panel's well, so every graph in the develop rail reads as one
        // instrument family.
        painter.rect_filled(rect, tokens::RADIUS_CONTROL, tokens::WELL_BG);
        // D12/D13: real bins from the canvas's live histogram reduction.
        let backdrop_bins = histogram_bins_for(channel, ctx.edit.histogram());
        paint_histogram_backdrop(&painter, rect, &backdrop_bins);
        // Quarter grid: faint white-on-dark hairlines. A near-black
        // `SEPARATOR_HAIRLINE` (this theme's usual boundary stroke) would
        // all but vanish against `WELL_BG` (#141414), `
        // SEPARATOR_HAIRLINE_HIGHLIGHT` is the token this theme already
        // uses for "a line that must read on a dark recessed surface"
        // (the seam highlight), reused here for the same reason.
        let grid_x_range = egui::Rangef::new(rect.left(), rect.right());
        let grid_y_range = egui::Rangef::new(rect.top(), rect.bottom());
        for i in 1..4 {
            let t = i as f32 / 4.0;
            paint::hairline_v(
                &painter,
                grid_y_range,
                rect.left() + t * rect.width(),
                tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
            );
            paint::hairline_h(
                &painter,
                grid_x_range,
                rect.top() + t * rect.height(),
                tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
            );
        }
        // Identity diagonal: the same faint grid token, distinctly weaker
        // than the opaque, wider active-curve stroke below by construction
        // (1px translucent vs. 1.5px opaque), not by a separate token.
        painter.line_segment(
            [rect.left_bottom(), rect.right_top()],
            egui::Stroke::new(
                tokens::STROKE_HAIRLINE,
                tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
            ),
        );
        // The REAL composed curve shape (C5): point spline + (composite
        // channel only) the parametric delta, a true WYSIWYG preview.
        // Crisp and opaque so it always reads as the one active line in
        // the well.
        let stroke = egui::Stroke::new(1.5, tokens::TEXT_PRIMARY);
        let sampled = sample_composed(channel, &painted_set, 64);
        for w in sampled.windows(2) {
            painter.line_segment(
                [
                    to_screen(rect, w[0].0, w[0].1),
                    to_screen(rect, w[1].0, w[1].1),
                ],
                stroke,
            );
        }
        // Control points (spec §3's knob language via `paint_control_point`):
        // "hovered" is resolved with the exact same `hit_point` the
        // drag/click interactions above use, so it means exactly what it
        // means for gesture purposes, not an approximation.
        let hovered_idx = response
            .hover_pos()
            .and_then(|pos| hit_point(&points, rect, pos));
        for (i, p) in points.iter().enumerate() {
            let pos = to_screen(rect, p.x, p.y);
            paint_control_point(
                &painter,
                pos,
                dragging == Some(i),
                hovered_idx == Some(i),
                selected == Some(i),
            );
        }
        if response.has_focus() {
            painter.rect_stroke(
                rect,
                tokens::RADIUS_CONTROL,
                egui::Stroke::new(tokens::STROKE_FOCUS_RING, tokens::accent_focus_ring()),
                egui::StrokeKind::Inside,
            );
        }
        paint::bevel_inset(&painter, rect, tokens::RADIUS_CONTROL);
    }

    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("{} tone curve editor", channel.label()),
        )
    });
}

// ─── C5: the parametric editor ──────────────────────────────────────────────

fn parametric_editor(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    set: &ToneCurveSet,
    base_id: egui::Id,
) {
    let width = ui.available_width().max(80.0);
    let height = width.min(140.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, height + 16.0),
        egui::Sense::click_and_drag(),
    );
    let graph_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, height));
    let handle_y = rect.bottom() - 6.0;

    let id = base_id.with("parametric");
    let drag_id = id.with("drag");
    let dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));

    let splits = set.parametric.splits;

    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            match dragging {
                Some(idx) => {
                    let nx =
                        ((pos.x - graph_rect.left()) / graph_rect.width().max(1.0)).clamp(0.0, 1.0);
                    let clamped = clamp_split(&splits, idx, nx);
                    if (clamped - splits[idx]).abs() > f32::EPSILON {
                        let mut next = set.clone();
                        next.parametric.splits[idx] = clamped;
                        ctx.edit.preview(curve_delta(next));
                    }
                }
                None => {
                    if let Some(idx) = hit_split_handle(&splits, graph_rect, handle_y, pos) {
                        ctx.edit.begin_gesture(ParamId::ToneCurve);
                        ui.data_mut(|d| d.insert_temp(drag_id, idx));
                    }
                }
            }
        }
    }
    if response.drag_stopped() {
        if dragging.is_some() {
            ctx.edit.end_gesture();
        }
        ui.data_mut(|d| d.remove::<usize>(drag_id));
    }

    if ui.is_rect_visible(rect) {
        let painted_set = match ctx.edit.value(ParamId::ToneCurve) {
            ParamValue::Curve(set) => set,
            _ => ToneCurveSet::default(),
        };
        let painter = ui.painter().with_clip_rect(rect);
        // Recessed well, same treatment as the point editor's graph above.
        painter.rect_filled(graph_rect, tokens::RADIUS_CONTROL, tokens::WELL_BG);
        painter.line_segment(
            [graph_rect.left_bottom(), graph_rect.right_top()],
            egui::Stroke::new(
                tokens::STROKE_HAIRLINE,
                tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
            ),
        );
        let stroke = egui::Stroke::new(1.5, tokens::TEXT_PRIMARY);
        let sampled = sample_composed(Channel::Rgb, &painted_set, 64);
        for w in sampled.windows(2) {
            painter.line_segment(
                [
                    to_screen(graph_rect, w[0].0, w[0].1),
                    to_screen(graph_rect, w[1].0, w[1].1),
                ],
                stroke,
            );
        }
        let hovered_idx = response.hover_pos().and_then(|pos| {
            hit_split_handle(&painted_set.parametric.splits, graph_rect, handle_y, pos)
        });
        for (idx, &s) in painted_set.parametric.splits.iter().enumerate() {
            let x = graph_rect.left() + s * graph_rect.width();
            paint::hairline_v(
                &painter,
                egui::Rangef::new(graph_rect.top(), graph_rect.bottom()),
                x,
                tokens::SEPARATOR_HAIRLINE_HIGHLIGHT,
            );
            paint_control_point(
                &painter,
                egui::pos2(x, handle_y),
                dragging == Some(idx),
                hovered_idx == Some(idx),
                false, // split handles carry no persistent selection state
            );
        }
        paint::bevel_inset(&painter, graph_rect, tokens::RADIUS_CONTROL);
    }

    // Region sliders, decompose the ONE `ParamId::ToneCurve` leaf, same
    // pattern `panels::basic`'s WB temp/tint sliders use for their coarse
    // leaf.
    ui.add_space(4.0);
    region_slider(
        ui,
        ctx,
        set,
        "Highlights",
        set.parametric.highlights,
        |p, v| p.highlights = v as f32,
    );
    region_slider(ui, ctx, set, "Lights", set.parametric.lights, |p, v| {
        p.lights = v as f32
    });
    region_slider(ui, ctx, set, "Darks", set.parametric.darks, |p, v| {
        p.darks = v as f32
    });
    region_slider(ui, ctx, set, "Shadows", set.parametric.shadows, |p, v| {
        p.shadows = v as f32
    });

    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            "Parametric tone curve editor",
        )
    });
}

fn hit_split_handle(
    splits: &[f32; 3],
    graph_rect: egui::Rect,
    handle_y: f32,
    pos: egui::Pos2,
) -> Option<usize> {
    (0..3)
        .map(|i| {
            let x = graph_rect.left() + splits[i] * graph_rect.width();
            (i, egui::pos2(x, handle_y).distance(pos))
        })
        .filter(|(_, d)| *d <= HIT_RADIUS_PT)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

fn region_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    set: &ToneCurveSet,
    label: &'static str,
    value: f32,
    recompose: impl Fn(&mut ParametricCurve, f64),
) {
    let spec = SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    };
    let (_, events) = value_slider(ui, ("develop.curve.parametric", label), value as f64, &spec);
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::ToneCurve),
            SliderEvent::Preview(v) => {
                let mut next = set.clone();
                recompose(&mut next.parametric, v);
                ctx.edit.preview(curve_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => {
                let mut next = set.clone();
                recompose(&mut next.parametric, 0.0);
                ctx.edit.begin_gesture(ParamId::ToneCurve);
                ctx.edit.preview(curve_delta(next));
                ctx.edit.end_gesture();
            }
            SliderEvent::Commit(v) => {
                let mut next = set.clone();
                recompose(&mut next.parametric, v);
                ctx.edit.begin_gesture(ParamId::ToneCurve);
                ctx.edit.preview(curve_delta(next));
                ctx.edit.end_gesture();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    // ── C1-style pure unit tests for the C4/C5 helpers ──────────────────

    #[test]
    fn snap_to_grid_lands_exactly_on_grid_lines() {
        let p = snap_to_grid(CurvePoint { x: 0.47, y: 0.63 });
        assert!((p.x - 0.45).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 0.65).abs() < 1e-6, "y={}", p.y);
    }

    // ── Re-skin: control-point state → appearance mapping (pure) ──────────

    #[test]
    fn point_radius_is_active_size_while_dragging_or_hovered_rest_size_otherwise() {
        assert_eq!(point_radius(false, false), POINT_RADIUS_REST);
        assert_eq!(point_radius(true, false), POINT_RADIUS_ACTIVE);
        assert_eq!(point_radius(false, true), POINT_RADIUS_ACTIVE);
        assert_eq!(point_radius(true, true), POINT_RADIUS_ACTIVE);
    }

    #[test]
    fn point_glow_layers_are_silent_at_rest_and_pick_drag_over_hover() {
        assert!(
            point_glow_layers(false, false).is_none(),
            "spec Principle 1: accent must be silent at rest"
        );
        assert_eq!(
            point_glow_layers(false, true),
            Some(tokens::accent_glow_hover().to_vec())
        );
        assert_eq!(
            point_glow_layers(true, false),
            Some(tokens::accent_glow_drag().to_vec())
        );
        // Dragging takes priority when (transiently) both are true.
        assert_eq!(
            point_glow_layers(true, true),
            Some(tokens::accent_glow_drag().to_vec())
        );
    }

    #[test]
    fn clamp_split_never_crosses_a_neighbor() {
        let splits = [0.25, 0.50, 0.75];
        // Try to drag split 0 past split 1.
        let c = clamp_split(&splits, 0, 0.9);
        assert!(c < splits[1], "split 0 crossed split 1: {c}");
        // Try to drag split 2 below split 1.
        let c = clamp_split(&splits, 2, 0.1);
        assert!(c > splits[1], "split 2 crossed split 1: {c}");
        // Try to drag split 1 past split 2.
        let c = clamp_split(&splits, 1, 0.99);
        assert!(c < splits[2], "split 1 crossed split 2: {c}");
    }

    #[test]
    fn paint_histogram_backdrop_never_panics() {
        let mut harness = Harness::new_ui(crate::theme::test_support::themed(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(100.0, 50.0), egui::Sense::hover());
            let painter = ui.painter();
            paint_histogram_backdrop(painter, rect, &[]);
            paint_histogram_backdrop(painter, rect, &[0.0, 0.0, 0.0]);
            paint_histogram_backdrop(painter, rect, &[1.0, 4.0, 2.0, 8.0, 0.0]);
        }));
        // `themed` binds the weight-role named font families before frame 0
        // paints (see `theme::test_support` for why that matters).
        harness.run();
    }

    // ── C4/C5 interaction tests (egui_kittest) ────────────────────────────

    use std::collections::HashMap;

    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;

    /// The same recording-double convention `panels::basic`'s own tests use
    /// (see that module for the full rationale): every value lives in a
    /// plain map that ONLY [`EditBinding::preview`]/[`EditBinding::reset`]
    /// write to, so panel behavior is proven through the exact same seam
    /// production code uses.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(
                ParamId::ToneCurve,
                ParamValue::Curve(ToneCurveSet::default()),
            );
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or_else(|| ParamValue::Curve(ToneCurveSet::default()))
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::Curve(ToneCurveSet::default())
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

    fn curve_set_of(binding: &RecordingBinding) -> ToneCurveSet {
        match binding.value(ParamId::ToneCurve) {
            ParamValue::Curve(set) => set,
            _ => ToneCurveSet::default(),
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
        harness.set_size(egui::vec2(360.0, 420.0));
        // `themed_state` binds the weight-role named font families before
        // frame 0 paints (see `theme::test_support` for why that matters).
        harness
    }

    /// **C4 AC**: a REAL synthetic click (kittest pointer-button event,
    /// not a direct function call) on empty graph space adds a point, the
    /// production interaction path end to end. `Node::click()` lands at the
    /// node's rect center, which maps to curve-space `(0.5, 0.5)`, well
    /// clear of the two identity-corner points, so it always hits the
    /// "click empty space" branch.
    #[test]
    fn a_real_synthetic_click_on_empty_graph_space_adds_a_point() {
        let mut harness = panel_harness();
        harness.run();
        let before = curve_set_of(&harness.state().binding).rgb.points.len();

        harness
            .get_by_label_contains("RGB tone curve editor")
            .click();
        harness.run();

        let after = curve_set_of(&harness.state().binding);
        assert_eq!(
            after.rgb.points.len(),
            before + 1,
            "a real synthetic click on empty graph space must add exactly one point"
        );
        let added = after
            .rgb
            .points
            .iter()
            .find(|p| p.x > 0.1 && p.x < 0.9)
            .expect("the click-added point sits strictly inside the two identity corners");
        assert!(
            (added.x - 0.5).abs() < 0.15 && (added.y - 0.5).abs() < 0.15,
            "a center click should land near curve-space (0.5, 0.5): {added:?}"
        );
    }

    /// **C4 AC**: a REAL synthetic drag (kittest `hover_at`/`drag_at`/
    /// `drop_at` pointer events across successive frames, the same
    /// primitive `filmstrip.rs`'s own kittest tests use) that tries to drag
    /// the top-right identity point far past its left neighbor never lets
    /// it cross, driven through the actual widget, not just the pure
    /// `clamp_drag` unit tests above.
    #[test]
    fn a_real_synthetic_drag_can_never_cross_a_neighbor() {
        let mut harness = panel_harness();
        harness.run();
        let rect = harness
            .get_by_label_contains("RGB tone curve editor")
            .rect();

        // The default curve's second point sits at curve-space (1,1) ==
        // screen `rect.right_top()`. Grab it and drag far past the left
        // neighbor at (0,0), well past `rect.left()`.
        let start = rect.right_top();
        let target = egui::pos2(rect.left() - 40.0, rect.center().y);

        harness.hover_at(start);
        harness.run();
        harness.drag_at(start);
        harness.run();
        harness.hover_at(target);
        harness.run();
        harness.drop_at(target);
        harness.run();

        let set = curve_set_of(&harness.state().binding);
        let dragged = set
            .rgb
            .points
            .last()
            .expect("the curve still has its rightmost point");
        assert!(
            dragged.x > 0.0,
            "a real synthetic drag must never cross the left neighbor at x=0: got x={}",
            dragged.x
        );
    }

    /// **C4 AC**: "no point-ordering violations possible through the UI"
    /// repeatedly dragging a point far past its right neighbor never lets
    /// it cross (clamp_drag's neighbor bound), across a dense sweep.
    #[test]
    fn dragging_a_point_can_never_cross_its_neighbor() {
        let points = vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.3, y: 0.3 },
            CurvePoint { x: 0.6, y: 0.6 },
            CurvePoint { x: 1.0, y: 1.0 },
        ];
        for target_x in [0.9f32, 1.5, -1.0, 0.6, 0.6001] {
            let p = clamp_drag(&points, 1, target_x, 0.5);
            assert!(
                p.x < points[2].x,
                "dragged point crossed its right neighbor: {}",
                p.x
            );
            assert!(
                p.x > points[0].x,
                "dragged point crossed its left neighbor: {}",
                p.x
            );
        }
        // Same proof for the left-edge point against its right neighbor.
        for target_x in [10.0f32, 0.3, 0.29999] {
            let p = clamp_drag(&points, 0, target_x, 0.5);
            assert!(
                p.x < points[1].x,
                "left point crossed its right neighbor: {}",
                p.x
            );
        }
    }

    /// **C4 AC**: `insert_point` can never place two points with less than
    /// [`MIN_X_GAP`] separation, however it is driven.
    #[test]
    fn insert_point_never_violates_min_gap() {
        let mut points = vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }];
        for x in [0.5f32, 0.500_1, 0.499_9, 0.5, 0.2, 0.8] {
            let _ = insert_point(&mut points, x, 0.5);
        }
        for w in points.windows(2) {
            assert!(
                w[1].x - w[0].x >= MIN_X_GAP - 1e-6,
                "gap {} < MIN_X_GAP between {:?} and {:?}",
                w[1].x - w[0].x,
                w[0],
                w[1]
            );
        }
    }

    /// **C5 AC**: switching channels preserves per-channel state, editing
    /// the red channel's points, then reading the green channel back,
    /// shows the green channel untouched (the data is genuinely per-field
    /// on `ToneCurveSet`, not shared/overwritten by the widget).
    #[test]
    fn switching_channels_preserves_each_channels_own_points() {
        let mut binding = RecordingBinding::new();
        let mut set = curve_set_of(&binding);
        set.r.points = vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.5, y: 0.2 },
            CurvePoint { x: 1.0, y: 1.0 },
        ];
        binding.preview(curve_delta(set));

        let after = curve_set_of(&binding);
        assert_eq!(after.r.points.len(), 3);
        // Green/blue/composite are untouched (still the 2-point default).
        assert_eq!(after.g, ToneCurve::default());
        assert_eq!(after.b, ToneCurve::default());
        assert_eq!(after.rgb, ToneCurve::default());
    }

    /// **C5 AC**: parametric and point modes co-exist (composed), setting
    /// point-curve data does not clobber parametric sliders, and vice
    /// versa; both live simultaneously in the one `ToneCurveSet` and both
    /// compose into `sample_composed`'s output.
    #[test]
    fn parametric_and_point_data_co_exist_without_clobbering() {
        let mut binding = RecordingBinding::new();
        let mut set = curve_set_of(&binding);
        set.rgb.points = vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.5, y: 0.7 },
            CurvePoint { x: 1.0, y: 1.0 },
        ];
        binding.preview(curve_delta(set));

        let mut set2 = curve_set_of(&binding);
        set2.parametric.highlights = 50.0;
        binding.preview(curve_delta(set2));

        let final_set = curve_set_of(&binding);
        // The point edit survived the later parametric edit.
        assert_eq!(final_set.rgb.points.len(), 3);
        assert!((final_set.rgb.points[1].y - 0.7).abs() < 1e-6);
        // The parametric edit landed too.
        assert_eq!(final_set.parametric.highlights, 50.0);
        // Both contribute to the composed sample (point reshape + a
        // highlights-region lift near x=1).
        let composed = sample_composed(Channel::Rgb, &final_set, 32);
        let near_top = composed.last().unwrap();
        assert!(
            near_top.1 > 0.9,
            "composed y near x=1 should stay high: {near_top:?}"
        );
    }

    /// **C4 AC (reset)**: the Reset affordance restores the active curve to
    /// identity without touching the other three.
    #[test]
    fn reset_only_touches_the_active_channel() {
        let mut binding = RecordingBinding::new();
        let mut set = curve_set_of(&binding);
        set.rgb.points = vec![CurvePoint { x: 0.0, y: 0.1 }, CurvePoint { x: 1.0, y: 0.9 }];
        set.r.points = vec![CurvePoint { x: 0.0, y: 0.2 }, CurvePoint { x: 1.0, y: 0.8 }];
        binding.preview(curve_delta(set.clone()));

        let mut gizmos = GizmoLayer::new();
        let mut ctx = DevelopCtx {
            source_kind: lightbox_types::SourceKind::Rendered,
            edit: &mut binding,
            gizmos: &mut gizmos,
        };
        reset_active(&mut ctx, &set, Channel::Rgb, Mode::Point);

        let after = curve_set_of(&binding);
        assert_eq!(after.rgb, ToneCurve::default(), "active channel reset");
        assert_eq!(after.r.points.len(), 2);
        assert!(
            (after.r.points[0].y - 0.2).abs() < 1e-6,
            "untouched channel preserved"
        );
    }

    /// The widget renders across every channel/mode combination without
    /// panicking (a broad interaction smoke test, the finer-grained pure
    /// functions above pin the actual behavior contracts).
    #[test]
    fn panel_renders_across_every_channel_and_mode() {
        let mut harness = panel_harness();
        for _ in 0..4 {
            harness.run();
        }
    }
}
