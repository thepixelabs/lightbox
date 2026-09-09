// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase C (task **C9**), the `develop.hsl` panel: the 8-band HSL color
//! mixer, bound to `ParamId::Hsl`'s ONE [`HslTable`] leaf (the same
//! decompose/recompose-a-coarse-leaf convention `panels::basic`'s WB
//! temp/tint sliders and `panels::curve`'s parametric region sliders already
//! establish).
//!
//! # Layout
//!
//! A tab row of the 8 band names (spec-fixed order: Red, Orange, Yellow,
//! Green, Aqua, Blue, Purple, Magenta, [`colorspace::HUE_BAND_NAMES`]),
//! each preceded by a small colour chip painted from
//! [`colorspace::HUE_BAND_CHIP_SRGB8`] (the SAME 8 reference swatches the
//! render-side hue-band centers are pinned to, so the chip a user clicks is
//! the actual color the band's math treats as its center, not an
//! independently-chosen decorative palette). Selecting a tab shows that
//! band's Hue/Saturation/Luminance sliders (each `-100..=100`, spec §4.3
//! `HslBand`) plus a per-band Reset button, mirrors `panels::curve`'s
//! Channel-tab pattern exactly (same tab-bar widget shape, same
//! `ui.selectable_label` + persistent-`Ui`-data active-tab state).
//!
//! # No widget-local state (A12's own discipline, extended here)
//!
//! Every slider reads its value from `ctx.edit.value(ParamId::Hsl)` every
//! frame (never a cached copy) and writes back through
//! `ctx.edit.begin_gesture`/`preview`/`end_gesture`, the panel always
//! reflects the recipe, proven the same way `panels::basic`'s own test does
//! (mutate the binding from OUTSIDE any widget interaction and confirm the
//! very next frame's render reflects it).

use eframe::egui;
use lightbox_edit::{HslBand, HslTable, ParamDelta, ParamId, ParamValue, Treatment};
use lightbox_render::ng::nodes::common::colorspace;

use crate::panels::bw::is_monochrome;
use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};
use crate::theme::tokens;

/// The HSL panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.hsl");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots after
/// the tone curve (30), before color grading (45).
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "HSL",
        source_req: SourceReq::Any,
        order: 40,
        build,
    }
}

fn hsl_delta(table: HslTable) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::Hsl, ParamValue::Hsl(table));
    delta
}

fn current_treatment(ctx: &DevelopCtx<'_>) -> Treatment {
    match ctx.edit.value(ParamId::Treatment) {
        ParamValue::Treatment(t) => t,
        _ => Treatment::Color,
    }
}

/// **D2 AC ("color panels visibly disabled in Monochrome")**: the render
/// engine already elides `HslNode` once `Treatment::BlackAndWhite` is
/// active (`nodes/global/hsl.rs`'s own `is_identity`), this greys the
/// whole band editor via [`is_monochrome`] (`panels::bw`'s single
/// unit-tested predicate for the condition) so the UI reflects that, rather
/// than leaving inert sliders interactive.
fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let mono = is_monochrome(current_treatment(ctx));

    let band_id = ui.make_persistent_id("develop.hsl.band");
    let mut band: usize = ui.data(|d| d.get_temp(band_id)).unwrap_or(0);
    band = band.min(colorspace::N_BANDS - 1);

    ui.add_enabled_ui(!mono, |ui| {
        let table = match ctx.edit.value(ParamId::Hsl) {
            ParamValue::Hsl(t) => t,
            _ => HslTable::default(),
        };

        ui.horizontal_wrapped(|ui| {
            for i in 0..colorspace::N_BANDS {
                band_tab(ui, i, band == i, &mut band);
            }
        });

        ui.separator();

        let b = table.bands[band];
        band_slider(ui, ctx, &table, band, "Hue", b.hue, |band, v| {
            band.hue = v as f32
        });
        band_slider(ui, ctx, &table, band, "Saturation", b.sat, |band, v| {
            band.sat = v as f32
        });
        band_slider(ui, ctx, &table, band, "Luminance", b.lum, |band, v| {
            band.lum = v as f32
        });

        if ui
            .button("Reset band")
            .on_hover_text("Reset this band's hue/saturation/luminance to identity")
            .clicked()
        {
            reset_band(ctx, &table, band);
        }
    });
    ui.data_mut(|d| d.insert_temp(band_id, band));
}

/// The swatch's own base radius (excluding the selected ring's extra
/// reach).
const CHIP_SWATCH_RADIUS: f32 = 5.0;

/// The square the chip is allocated in, one point of slack around the
/// swatch so the selected chip's outer ring isn't clipped by its own cell.
const CHIP_DIAMETER: f32 = 10.0;

/// The ring a hue-band chip's swatch paints, given its selection state
/// (design spec §5): a selected chip gets a real selected treatment, a
/// crisp accent border ring sitting *outside* the swatch (radius, stroke
/// width, color), not merely a stroke-color swap on the same outline, the
/// same "2px accent_default border" language the filmstrip uses for its
/// own thumbnail selection state. An unselected chip gets a subtle
/// recessed feel instead: a thin, on-the-swatch inset stroke using the
/// same `BEVEL_INSET_TOP` token every recessed well's top edge uses. Pure
/// and split out from [`band_tab`] so the state→appearance mapping is
/// unit-testable without a `Painter` (see this module's tests).
fn chip_ring_stroke(selected: bool) -> (f32, egui::Stroke) {
    if selected {
        (
            CHIP_SWATCH_RADIUS + 2.0,
            egui::Stroke::new(tokens::STROKE_SELECTION_BORDER, tokens::accent()),
        )
    } else {
        (
            CHIP_SWATCH_RADIUS,
            egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::BEVEL_INSET_TOP),
        )
    }
}

/// One tab: a small colour chip ([`colorspace::HUE_BAND_CHIP_SRGB8`]) plus a
/// `selectable_label` carrying the band name, clicking either selects the
/// band. The chip's hue is a purely decorative, content-driven paint (the
/// label is what carries the AccessKit name/click semantics both
/// production use and this file's tests key off); its selection/rest
/// *chrome* (border treatment, via [`chip_ring_stroke`]), by contrast,
/// comes from theme tokens like every other control in this panel.
///
/// **Why `allocate_ui_with_layout` and not a plain `ui.horizontal`.** The
/// eight tabs live in a `horizontal_wrapped` row, and egui only wraps an
/// item whose desired size it is told *before* placing it (`Placer::
/// next_space`). A nested `ui.horizontal` announces nothing, it takes
/// `available_rect_before_wrap` and simply overflows, and
/// `Region::expand_to_include_rect` then widens the parent's `max_rect`,
/// so the next tab sees a *wider* budget and the row never wraps at all.
/// The eight tabs used to lay out as one 637pt line inside a 456pt rail,
/// which is the overflow that used to break the whole develop rail (see
/// `panels::host`'s "width containment" note). Measuring the tab restores
/// real wrapping.
fn band_tab(ui: &mut egui::Ui, i: usize, selected: bool, band: &mut usize) {
    // No spacing token is an exact match for the previous ad-hoc 3px
    // `SPACE_1` (4px) is the closest defined step on the scale and the
    // 1px difference is not perceptible at this chip size, so it replaces
    // the one-off magic number rather than inventing a new token for a
    // single call site.
    let name = colorspace::HUE_BAND_NAMES[i];
    let label_width = ui
        .painter()
        .layout_no_wrap(
            name.to_owned(),
            egui::TextStyle::Button.resolve(ui.style()),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .x;
    let desired = egui::vec2(
        CHIP_DIAMETER + tokens::SPACE_1 + label_width + 2.0 * ui.spacing().button_padding.x,
        ui.spacing().interact_size.y,
    );
    ui.allocate_ui_with_layout(
        desired,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = tokens::SPACE_1;
            let chip = colorspace::HUE_BAND_CHIP_SRGB8[i];
            let (rect, _resp) =
                ui.allocate_exact_size(egui::Vec2::splat(CHIP_DIAMETER), egui::Sense::hover());
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                let center = rect.center();
                painter.circle_filled(
                    center,
                    CHIP_SWATCH_RADIUS,
                    egui::Color32::from_rgb(chip[0], chip[1], chip[2]),
                );
                let (ring_r, ring_stroke) = chip_ring_stroke(selected);
                painter.circle_stroke(center, ring_r, ring_stroke);
            }
            if ui.selectable_label(selected, name).clicked() {
                *band = i;
            }
        },
    );
}

/// One H/S/L slider for `band_idx`, decomposing/recomposing the ONE
/// [`HslTable`] leaf (mirrors `panels::curve::region_slider`'s
/// decompose-a-coarse-leaf shape).
fn band_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    table: &HslTable,
    band_idx: usize,
    label: &'static str,
    value: f32,
    recompose: impl Fn(&mut HslBand, f64),
) {
    let spec = SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    };
    let (_, events) = value_slider(
        ui,
        ("develop.hsl.slider", band_idx, label),
        value as f64,
        &spec,
    );
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::Hsl),
            SliderEvent::Preview(v) => {
                let mut next = *table;
                recompose(&mut next.bands[band_idx], v);
                ctx.edit.preview(hsl_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => {
                let mut next = *table;
                recompose(&mut next.bands[band_idx], 0.0);
                ctx.edit.begin_gesture(ParamId::Hsl);
                ctx.edit.preview(hsl_delta(next));
                ctx.edit.end_gesture();
            }
            SliderEvent::Commit(v) => {
                let mut next = *table;
                recompose(&mut next.bands[band_idx], v);
                ctx.edit.begin_gesture(ParamId::Hsl);
                ctx.edit.preview(hsl_delta(next));
                ctx.edit.end_gesture();
            }
        }
    }
}

/// The per-band Reset affordance (task C9 AC): restores ONLY the active
/// band's hue/sat/lum to identity, in one gesture, the same "channel-blind
/// reset would silently discard other work" discipline
/// `panels::curve::reset_active` documents for its own per-curve reset.
fn reset_band(ctx: &mut DevelopCtx<'_>, table: &HslTable, band_idx: usize) {
    let mut next = *table;
    next.bands[band_idx] = HslBand::default();
    ctx.edit.begin_gesture(ParamId::Hsl);
    ctx.edit.preview(hsl_delta(next));
    ctx.edit.end_gesture();
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui_kittest::{kittest::NodeT as _, kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;

    // ── Re-skin: chip selection state → ring appearance (pure) ────────────

    #[test]
    fn selected_chip_gets_a_wider_accent_ring_unselected_gets_the_recessed_token() {
        let (r_sel, stroke_sel) = chip_ring_stroke(true);
        let (r_unsel, stroke_unsel) = chip_ring_stroke(false);
        assert!(
            r_sel > r_unsel,
            "the selected ring must sit outside the swatch, not on it: {r_sel} vs {r_unsel}"
        );
        assert_eq!(stroke_sel.color, tokens::accent());
        assert_eq!(stroke_sel.width, tokens::STROKE_SELECTION_BORDER);
        assert_eq!(stroke_unsel.color, tokens::BEVEL_INSET_TOP);
    }

    /// The same recording-double convention `panels::basic`/`panels::curve`
    /// use: every value lives in a plain map that ONLY
    /// [`EditBinding::preview`]/[`EditBinding::reset`] write to, so panel
    /// behavior is proven through the exact same seam production code uses.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(ParamId::Hsl, ParamValue::Hsl(HslTable::default()));
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or_else(|| ParamValue::Hsl(HslTable::default()))
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::Hsl(HslTable::default())
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

    fn table_of(binding: &RecordingBinding) -> HslTable {
        match binding.value(ParamId::Hsl) {
            ParamValue::Hsl(t) => t,
            _ => HslTable::default(),
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

    /// **C9 AC (panel-reflects-recipe)**: setting the binding's HSL table
    /// from OUTSIDE any widget interaction changes what the very next frame
    /// shows, zero clicks or drags, same proof `panels::basic`'s own test
    /// makes for the tone sliders.
    #[test]
    fn panel_always_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        let mut table = HslTable::default();
        table.bands[0].hue = 37.0; // Red band (the default active tab)
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Hsl, ParamValue::Hsl(table));
        harness.run();
        harness.get_by_label("+37");

        // Mutate again from outside, the same channel undo/redo/StepTo
        // uses in the real app.
        table.bands[0].hue = -12.0;
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Hsl, ParamValue::Hsl(table));
        harness.run();
        harness.get_by_label("-12");
    }

    /// **C9 AC (gesture-commit)**: a REAL synthetic drag (kittest pointer
    /// events, not a hand-called event) on `band_slider`'s track, the exact
    /// production widget every H/S/L control in this panel goes through
    /// routes a `ParamId::Hsl` delta through the
    /// `begin_gesture`/`preview`/`end_gesture` lifecycle and lands on the
    /// recipe.
    #[test]
    fn a_real_synthetic_drag_commits_one_hsl_gesture_and_lands_the_value() {
        let table = HslTable::default();
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, s: &mut (HslTable, RecordingBinding)| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut s.1,
                    gizmos: &mut gizmos,
                };
                band_slider(ui, &mut ctx, &s.0, 5, "Hue", 0.0, |b, v| b.hue = v as f32);
            }),
            (table, RecordingBinding::new()),
        );
        harness.set_size(egui::vec2(320.0, 40.0));
        // `themed_state` binds the weight-role named font families before
        // frame 0 paints (see `theme::test_support` for why that matters).
        harness.run();
        let track = harness.get_by_role(eframe::egui::accesskit::Role::Slider);
        let rect = track.rect();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drag_at(rect.left_center());
        harness.run();
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drop_at(rect.right_center());
        harness.run();

        let after = table_of(&harness.state().1);
        assert!(
            after.bands[5].hue > 50.0,
            "dragging the Blue band's Hue track to its right edge should land a large positive \
             value: got {}",
            after.bands[5].hue
        );
    }

    /// The panel mounts as a registered [`PanelDef`] with the expected
    /// identity/order, a probe that `def()` is wired the way `lib.rs`
    /// expects (order between curve=30 and grading=45).
    #[test]
    fn def_is_registered_between_curve_and_grading() {
        let d = def();
        assert_eq!(d.id.0, "develop.hsl");
        assert!(d.order > 30 && d.order < 45);
    }

    /// **D2 AC ("color panels visibly disabled in Monochrome")**, the
    /// wiring half: with a real accessible-tree probe (not just the
    /// `is_monochrome` unit test in `panels::bw`), the band editor's slider
    /// nodes carry AccessKit's disabled state once the binding's Treatment
    /// flips to Monochrome, and are enabled again for Color.
    #[test]
    fn panel_disables_the_band_editor_when_monochrome() {
        let mut harness = panel_harness();
        harness.run();
        let track = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Hue");
        assert!(
            !track.accesskit_node().is_disabled(),
            "Color treatment (the default): the band editor stays interactive"
        );

        harness.state_mut().binding.values.insert(
            ParamId::Treatment,
            ParamValue::Treatment(Treatment::BlackAndWhite),
        );
        harness.run();
        let track = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Hue");
        assert!(
            track.accesskit_node().is_disabled(),
            "Monochrome treatment: the HSL band editor must be visibly disabled (D2 AC)"
        );
    }
}
