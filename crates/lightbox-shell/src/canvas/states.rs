// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Canvas states & the degraded-device notice (E08 spec §6.4/§7, task C5):
//! Failed/missing placards, loading shimmer for never-rendered entries, and
//! a non-modal `DeviceDegraded` chip. Every function here paints directly
//! (no `egui::Window`/`Area`/focus-taking widget), by construction none of
//! this can steal keyboard focus, and the chip is a small corner badge that
//! never covers the canvas's own click/drag `Sense` area (C5 AC).
//!
//! **Before and after (a canvas state, not a widget).** The before/after
//! view is a state of the canvas in exactly the sense the rest of this
//! module is: [`BeforeAfterMode`] is the pure state machine (transitions,
//! the momentary-hold override, and the fall-back-to-after rule), and the
//! label/divider painting beside it follows the same construction as the
//! placards and chips, painted directly, never a focus-taking widget. The
//! machinery that fills the before image (what "before" means, the GPU
//! snapshot, the divider geometry) is `canvas/before_after.rs`; the prose
//! is `canvas/before_after.md`.
//!
//! **Failed vs. missing (mapping note).** The spec's §6.4/§7 canvas-states
//! table anticipates a "missing" condition distinct from a decode/probe
//! failure. E04's shipped `WorkingSetItem`/`ItemState` only exposes
//! `Failed` (a free-text `decode_error`, no separate missing-file variant)
//! for the SOURCE-level failure, so [`CanvasPlacard::SourceFailed`] covers
//! it (any reason text the loader reported, including "no such file"-style
//! ones). §7's "missing file (deleted mid-session) → placard + badge on
//! next render failure" is instead exactly [`CanvasPlacard::RenderFailed`]:
//! the entry itself IS `Ready` (never failed to open), but the render
//! attempt fails and nothing has ever been composited for it yet, the
//! same condition AFTER a good frame already exists is instead the
//! stale-frame + error chip (`view.rs`'s `ready_ui`), not a placard.

use eframe::egui;

use crate::theme::{fonts, paint, tokens};

/// A full-canvas takeover, nothing else is composited this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasPlacard {
    /// Registration/hash still pending (`ItemState::Planned`).
    Loading,
    /// The working-set entry itself failed to open/decode/register, never
    /// reached the canvas as a renderable image.
    SourceFailed {
        /// Human-readable failure reason (never hidden, spec §7).
        reason: String,
    },
    /// The entry is `Ready` but NOTHING has ever been composited for it
    /// (no tier preview, no engine frame) and the latest render attempt
    /// also failed, see the module docs' "missing" mapping note.
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

/// The loading shimmer for never-rendered entries, a slow luminance
/// pulse, matching the filmstrip's (`filmstrip::draw_shimmer`) so the two
/// loading affordances read as one visual language. The pulse is the one
/// legitimate motion on the canvas (spec §8: gated on egui's own repaint
/// cadence via `ui.input(|i| i.time)`, not a `Timer`), everything it's
/// colored with is still a spec token like every other surface here.
pub fn loading_shimmer(ui: &egui::Ui, rect: egui::Rect) {
    let t = ui.input(|i| i.time);
    let pulse = ((t * std::f64::consts::TAU / 1.4).sin() * 0.5 + 0.5) as f32;
    ui.painter().rect_filled(
        rect,
        tokens::RADIUS_CONTROL,
        tokens::TEXT_SECONDARY.gamma_multiply(0.08 + 0.10 * pulse),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "loading…",
        fonts::status_bar_font(),
        tokens::TEXT_SECONDARY,
    );
    label_region(ui, rect, "canvas-loading", "loading");
}

/// A full-canvas-takeover placard card (spec §8): `ELEV_1_PANEL @ 85%`
/// background so the canvas still reads through at the edges, a 1px
/// `SEPARATOR_HAIRLINE` border, `RADIUS_CONTROL` corners, centered on
/// `rect`. `glyph` is the big status mark above the message; the message
/// itself is `TEXT_SECONDARY` 11px, the same quiet, secondary-readout
/// register §9 gives the status bar's own text.
fn text_placard(ui: &egui::Ui, rect: egui::Rect, glyph: &str, message: &str, is_error: bool) {
    let painter = ui.painter();
    let glyph_color = if is_error {
        tokens::STATUS_ERROR
    } else {
        tokens::TEXT_SECONDARY
    };
    let glyph_galley = painter.layout_no_wrap(
        glyph.to_owned(),
        egui::FontId::proportional(22.0),
        glyph_color,
    );
    let message_galley = painter.layout_no_wrap(
        message.to_owned(),
        fonts::status_bar_font(),
        tokens::TEXT_SECONDARY,
    );

    let gap = 10.0;
    let pad = egui::vec2(tokens::SPACE_5, tokens::SPACE_4);
    let content_size = egui::vec2(
        glyph_galley.size().x.max(message_galley.size().x),
        glyph_galley.size().y + gap + message_galley.size().y,
    );
    let card_rect = egui::Rect::from_center_size(rect.center(), content_size + pad * 2.0);

    let card_bg = {
        let c = tokens::ELEV_1_PANEL;
        egui::Color32::from_rgba_unmultiplied(
            c.r(),
            c.g(),
            c.b(),
            (255.0 * tokens::CANVAS_PLACARD_BG_ALPHA).round() as u8,
        )
    };
    painter.rect_filled(card_rect, tokens::RADIUS_CONTROL, card_bg);
    painter.rect_stroke(
        card_rect,
        tokens::RADIUS_CONTROL,
        egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::SEPARATOR_HAIRLINE),
        egui::StrokeKind::Inside,
    );

    let glyph_pos = egui::pos2(
        card_rect.center().x - glyph_galley.size().x / 2.0,
        card_rect.center().y - content_size.y / 2.0,
    );
    painter.galley(glyph_pos, glyph_galley, glyph_color);
    let message_pos = egui::pos2(
        card_rect.center().x - message_galley.size().x / 2.0,
        card_rect.center().y + content_size.y / 2.0 - message_galley.size().y,
    );
    painter.galley(message_pos, message_galley, tokens::TEXT_SECONDARY);

    label_region(ui, rect, "canvas-placard", message);
}

/// Builds the uppercase, letter-tracked chip-text galley (spec §7/§9:
/// Inter SemiBold 10px, UPPERCASE, +0.3px tracking), the same recipe
/// `status::chip` uses for the status bar's own chips. Duplicated (not
/// shared) since the two modules don't otherwise depend on each other.
fn chip_galley(
    painter: &egui::Painter,
    text: &str,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: fonts::chip_font(),
            color,
            extra_letter_spacing: 0.3,
            ..Default::default()
        },
    );
    painter.layout_job(job)
}

/// A non-modal corner chip (the stale-frame render error, or the C5
/// `DeviceDegraded` notice), painted, never an interactive/focus-taking
/// widget. `anchor_index` stacks multiple chips (0 = closest to the
/// bottom, 1 = above it, …) so the render-error chip and the
/// device-degraded chip can coexist without overlapping. Spec §7/§8: the
/// same pill treatment as the status bar's own chips, plus `shadow_popup`
/// so it reads as floating above the photo rather than flat-docked
/// chrome.
pub fn chip_ui(ui: &egui::Ui, view_rect: egui::Rect, anchor_index: usize, text: &str, error: bool) {
    let bg = if error {
        tokens::STATUS_ERROR
    } else {
        tokens::STATUS_WARNING
    };
    let galley = chip_galley(ui.painter(), text, tokens::TEXT_ON_CHIP);
    let size = egui::vec2(
        galley.size().x + tokens::CHIP_PADDING_X * 2.0,
        tokens::CHIP_HEIGHT,
    );
    let y = view_rect.bottom() - 8.0 - (size.y + 6.0) * anchor_index as f32;
    let pos = egui::pos2(view_rect.right() - size.x - 8.0, y - size.y);
    let rect = egui::Rect::from_min_size(pos, size);
    let painter = ui.painter().with_clip_rect(view_rect);
    painter.add(paint::shadow_popup().as_shape(rect, tokens::RADIUS_CHIP));
    painter.rect_filled(rect, tokens::RADIUS_CHIP, bg);
    let text_pos = rect.left_center() + egui::vec2(tokens::CHIP_PADDING_X, -galley.size().y / 2.0);
    painter.galley(text_pos, galley, tokens::TEXT_ON_CHIP);
    label_region(ui, rect, "canvas-chip", text);
}

// ─── Before and after: the state machine ──────────────────────────────────

/// Which before/after arrangement the canvas is in.
///
/// Four states, matching what the feature is actually used for:
///
/// * [`BeforeAfterMode::AfterOnly`], the ordinary canvas.
/// * [`BeforeAfterMode::BeforeOnly`], the whole frame with no edits.
///   Reached **only** by holding the momentary key, never latched, so it
///   cannot be left on by accident. It is also the loudest state: the
///   canvas paints a warning-coloured border around the whole view for it
///   ([`before_only_border`]), because a full-frame before image is the
///   one arrangement that could be mistaken for the edited photo.
/// * [`BeforeAfterMode::SideBySide`], two panes, the same image twice.
/// * [`BeforeAfterMode::Split`], one image, one draggable divider.
///
/// The cycle deliberately skips `BeforeOnly`: a latched before-only view
/// is the state a photographer forgets they are in, and the hold key gives
/// the same view for as long as it is genuinely wanted.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BeforeAfterMode {
    /// The edited image, the canvas's normal state.
    #[default]
    AfterOnly,
    /// The unedited image, full frame (momentary).
    BeforeOnly,
    /// Two panes: before on the left, after on the right.
    SideBySide,
    /// One image, split by a draggable divider: before left, after right.
    Split,
}

impl BeforeAfterMode {
    /// The next latched mode: after only, side by side, split, back to
    /// after only. `BeforeOnly` is not on the cycle (see the type docs).
    pub fn cycled(self) -> BeforeAfterMode {
        match self {
            BeforeAfterMode::AfterOnly => BeforeAfterMode::SideBySide,
            BeforeAfterMode::SideBySide => BeforeAfterMode::Split,
            BeforeAfterMode::Split | BeforeAfterMode::BeforeOnly => BeforeAfterMode::AfterOnly,
        }
    }

    /// This mode overridden by the momentary hold key: holding it shows
    /// the before image alone from whatever mode is latched, and releasing
    /// returns exactly where the user was.
    pub fn with_hold(self, held: bool) -> BeforeAfterMode {
        if held {
            BeforeAfterMode::BeforeOnly
        } else {
            self
        }
    }

    /// Whether painting this mode needs a captured before image.
    pub fn needs_before(self) -> bool {
        self != BeforeAfterMode::AfterOnly
    }

    /// What the canvas should actually paint, given the hold key and
    /// whether a before image exists to paint.
    ///
    /// The fall-back is the unambiguity rule in code: with no before image
    /// available, every before-showing mode degrades to the plain after
    /// canvas. Showing the after image under no label is recoverable;
    /// showing the after image under a BEFORE label is the mistake that
    /// makes somebody ship the wrong edit.
    pub fn effective(self, held: bool, before_available: bool) -> BeforeAfterMode {
        let wanted = self.with_hold(held);
        if wanted.needs_before() && !before_available {
            BeforeAfterMode::AfterOnly
        } else {
            wanted
        }
    }

    /// Short name for the status bar / cheat sheet.
    pub fn label(self) -> &'static str {
        match self {
            BeforeAfterMode::AfterOnly => "After",
            BeforeAfterMode::BeforeOnly => "Before",
            BeforeAfterMode::SideBySide => "Before/After",
            BeforeAfterMode::Split => "Split",
        }
    }
}

// ─── Before and after: painting ───────────────────────────────────────────

/// Which side a pane label names.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaneSide {
    /// The unedited image.
    Before,
    /// The edited image.
    After,
}

impl PaneSide {
    fn text(self) -> &'static str {
        match self {
            PaneSide::Before => "before",
            PaneSide::After => "after",
        }
    }
}

/// Paints a pane label at the bottom centre of `pane`.
///
/// Deliberately asymmetric: BEFORE gets the `STATUS_WARNING` chip
/// treatment (the same "this is not the normal state" register the
/// device-degraded chip uses), AFTER gets the quiet placard treatment. The
/// eye should be drawn to the half that is *not* the edit. A pane narrower
/// than the label paints nothing rather than a label that spills into the
/// other half and lies about which side it names.
pub fn pane_label(ui: &egui::Ui, pane: egui::Rect, side: PaneSide) {
    let painter = ui.painter().with_clip_rect(pane);
    let (bg, fg) = match side {
        PaneSide::Before => (tokens::STATUS_WARNING, tokens::TEXT_ON_CHIP),
        PaneSide::After => (
            {
                let c = tokens::ELEV_1_PANEL;
                egui::Color32::from_rgba_unmultiplied(
                    c.r(),
                    c.g(),
                    c.b(),
                    (255.0 * tokens::CANVAS_PLACARD_BG_ALPHA).round() as u8,
                )
            },
            tokens::TEXT_SECONDARY,
        ),
    };
    let galley = chip_galley(&painter, side.text(), fg);
    let size = egui::vec2(
        galley.size().x + tokens::CHIP_PADDING_X * 2.0,
        tokens::CHIP_HEIGHT,
    );
    if pane.width() < size.x + tokens::SPACE_2 * 2.0 || pane.height() < size.y + tokens::SPACE_2 {
        return;
    }
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            pane.center().x - size.x / 2.0,
            pane.bottom() - size.y - tokens::SPACE_2,
        ),
        size,
    );
    painter.add(paint::shadow_popup().as_shape(rect, tokens::RADIUS_CHIP));
    painter.rect_filled(rect, tokens::RADIUS_CHIP, bg);
    if side == PaneSide::After {
        painter.rect_stroke(
            rect,
            tokens::RADIUS_CHIP,
            egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::SEPARATOR_HAIRLINE),
            egui::StrokeKind::Inside,
        );
    }
    let text_pos = rect.left_center() + egui::vec2(tokens::CHIP_PADDING_X, -galley.size().y / 2.0);
    painter.galley(text_pos, galley, fg);
    label_region(ui, rect, "canvas-pane-label", side.text());
}

/// Paints the whole-view warning border for [`BeforeAfterMode::BeforeOnly`].
///
/// The border exists because before-only is the only arrangement with
/// nothing on screen to compare against: without it, an unedited frame is
/// just a photo. Two pixels of `STATUS_WARNING` inside the view edge, the
/// same colour the pane label uses, so the two read as one statement.
pub fn before_only_border(ui: &egui::Ui, view_rect: egui::Rect) {
    ui.painter().with_clip_rect(view_rect).rect_stroke(
        view_rect,
        tokens::RADIUS_CONTROL,
        egui::Stroke::new(2.0, tokens::STATUS_WARNING),
        egui::StrokeKind::Inside,
    );
}

/// Paints the split divider at screen x `x`, with a grab handle at mid
/// height. `active` (hovered or being dragged) brightens it.
///
/// A dark hairline under a light one, so the divider stays visible over
/// both a blown sky and a black shadow: a single-colour line disappears
/// into half the photographs anyone would want to compare.
pub fn split_divider(ui: &egui::Ui, view_rect: egui::Rect, x: f32, active: bool) {
    if x <= view_rect.left() || x >= view_rect.right() {
        return; // off screen (zoomed in past it); nothing to draw
    }
    let painter = ui.painter().with_clip_rect(view_rect);
    let top = view_rect.top();
    let bottom = view_rect.bottom();
    let line = if active {
        tokens::TEXT_PRIMARY
    } else {
        tokens::TEXT_SECONDARY
    };
    painter.line_segment(
        [egui::pos2(x - 1.0, top), egui::pos2(x - 1.0, bottom)],
        egui::Stroke::new(1.0, tokens::SEPARATOR_HAIRLINE),
    );
    painter.line_segment(
        [egui::pos2(x + 1.0, top), egui::pos2(x + 1.0, bottom)],
        egui::Stroke::new(1.0, tokens::SEPARATOR_HAIRLINE),
    );
    painter.line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(1.0, line),
    );

    // The grab handle, sized to the 8 pt hit tolerance either side.
    let handle =
        egui::Rect::from_center_size(egui::pos2(x, view_rect.center().y), egui::vec2(14.0, 40.0));
    painter.add(paint::shadow_popup().as_shape(handle, tokens::RADIUS_CONTROL));
    painter.rect_filled(
        handle,
        tokens::RADIUS_CONTROL,
        if active {
            tokens::CONTROL_HOVER
        } else {
            tokens::CONTROL_REST
        },
    );
    painter.rect_stroke(
        handle,
        tokens::RADIUS_CONTROL,
        egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::SEPARATOR_HAIRLINE),
        egui::StrokeKind::Inside,
    );
    for dx in [-3.0_f32, 0.0, 3.0] {
        painter.line_segment(
            [
                egui::pos2(handle.center().x + dx, handle.top() + 12.0),
                egui::pos2(handle.center().x + dx, handle.bottom() - 12.0),
            ],
            egui::Stroke::new(1.0, tokens::TEXT_TERTIARY),
        );
    }
    label_region(ui, handle, "canvas-split-divider", "split divider");
}

/// A hover-only interact region purely so kittest/AccessKit can see the
/// state (`Sense::hover()` never claims keyboard focus and never competes
/// with the canvas's own click/drag `Sense` beneath it, C5 AC: "without
/// stealing focus or blocking input").
fn label_region(ui: &egui::Ui, rect: egui::Rect, salt: &str, label: &str) {
    let response = ui.interact(rect, ui.id().with((salt, label)), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    // `chip_ui` paints in `chip_font()`, a named weight-role family, every
    // harness below goes through `theme::test_support::themed_state` (see
    // that module's doc) so a bare kittest `Context` never hits it unbound.

    fn harness_for(paint: impl Fn(&mut egui::Ui) + 'static) -> Harness<'static, ()> {
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(move |ui, _state: &mut ()| paint(ui)),
            (),
        );
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

    // ── Before and after: the state machine (pure) ──────────────────────

    /// The cycle visits every latched arrangement and comes home, and it
    /// never latches before-only (that state exists only while the
    /// momentary key is held).
    #[test]
    fn the_before_after_cycle_returns_to_after_and_never_latches_before_only() {
        let mut mode = BeforeAfterMode::default();
        assert_eq!(mode, BeforeAfterMode::AfterOnly);
        let mut seen = Vec::new();
        for _ in 0..3 {
            mode = mode.cycled();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            vec![
                BeforeAfterMode::SideBySide,
                BeforeAfterMode::Split,
                BeforeAfterMode::AfterOnly,
            ]
        );
        assert!(
            !seen.contains(&BeforeAfterMode::BeforeOnly),
            "before-only must not be reachable by cycling; it is momentary"
        );
    }

    /// Holding the key shows before-only from any mode, and releasing it
    /// puts the user back exactly where they were.
    #[test]
    fn the_hold_key_overrides_every_mode_and_releasing_restores_it() {
        for mode in [
            BeforeAfterMode::AfterOnly,
            BeforeAfterMode::SideBySide,
            BeforeAfterMode::Split,
        ] {
            assert_eq!(mode.with_hold(true), BeforeAfterMode::BeforeOnly);
            assert_eq!(mode.with_hold(false), mode);
        }
    }

    /// **AC (the unambiguity rule):** with no before image captured, every
    /// before-showing mode paints the plain after canvas. The one thing
    /// that must never happen is the after image under a BEFORE label.
    #[test]
    fn without_a_before_image_every_mode_degrades_to_after_only() {
        for mode in [
            BeforeAfterMode::AfterOnly,
            BeforeAfterMode::SideBySide,
            BeforeAfterMode::Split,
        ] {
            for held in [false, true] {
                assert_eq!(
                    mode.effective(held, false),
                    BeforeAfterMode::AfterOnly,
                    "mode={mode:?} held={held} with no before image"
                );
            }
        }
        // With one available, the wanted mode stands.
        assert_eq!(
            BeforeAfterMode::Split.effective(false, true),
            BeforeAfterMode::Split
        );
        assert_eq!(
            BeforeAfterMode::Split.effective(true, true),
            BeforeAfterMode::BeforeOnly
        );
    }

    /// Both pane labels are structurally queryable, so "which half am I
    /// looking at" is answerable by a test and not only by eye.
    #[test]
    fn both_pane_labels_are_painted_and_labeled() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            let (left, right) = rect.split_left_right_at_fraction(0.5);
            pane_label(ui, left, PaneSide::Before);
            pane_label(ui, right, PaneSide::After);
        });
        harness.run();
        harness.get_by_label_contains("before");
        harness.get_by_label_contains("after");
    }

    /// A pane too small for its label paints nothing rather than a label
    /// that spills across the divider and names the wrong half.
    #[test]
    fn a_pane_narrower_than_its_label_paints_no_label() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            let sliver = egui::Rect::from_min_size(rect.min, egui::vec2(6.0, rect.height()));
            pane_label(ui, sliver, PaneSide::Before);
        });
        harness.run();
        assert!(
            harness.query_by_label_contains("before").is_none(),
            "a sliver pane must not claim a label"
        );
    }

    /// The divider handle is queryable (the keyboard route is tested in
    /// `before_after.rs`; this pins that the mouse target exists at all).
    #[test]
    fn the_split_divider_paints_a_grab_handle() {
        let mut harness = harness_for(|ui| {
            let rect = ui.available_rect_before_wrap();
            split_divider(ui, rect, rect.center().x, false);
        });
        harness.run();
        harness.get_by_label_contains("split divider");
    }

    /// C5 AC: the chip does not block input to whatever's underneath it
    /// a click on the (much larger) canvas hit-area beneath the chip still
    /// registers.
    #[test]
    fn chip_does_not_block_a_click_elsewhere_on_the_canvas() {
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, clicked: &mut bool| {
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
            }),
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
