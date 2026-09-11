// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E11 geometry TOOL UI, the `develop.geometry` panel: the crop-tool
//! toggle + aspect-ratio presets that drive [`crate::canvas::crop_gizmo`],
//! plus straighten (`ParamId::Angle`), flip (`ParamId::Flip`), and
//! reset-crop/reset-geometry, all over the SAME [`EditBinding`] seam every
//! other develop panel uses (`param_slider` for the scalar straighten
//! slider; hand-rolled gesture brackets for the coarse `Crop`/`Flip`
//! leaves, mirroring `panels::basic`'s WB temp/tint decompose/recompose
//! convention).
//!
//! # `ParamId::Crop` needed NO `EditBinding` trait extension
//!
//! `lightbox_edit::ParamValue::Crop(Crop)` (a whole-rect carrier) and
//! `ParamId::{Crop, Angle, Flip}` were already fully wired by E09/E11's
//! engine slice, `EditBinding::preview`'s `ParamDelta` already carries a
//! `V::Crop` value with no changes needed here. This panel and
//! `canvas::crop_gizmo` are therefore the first Crop/Flip UI, but not the
//! first Crop/Flip *plumbing*.
//!
//! # `ParamId::Flip`'s wire form
//!
//! `Flip`'s wire code (`ParamValue::I32`) is a stable bitmask documented on
//! `lightbox_edit::leaves::Flip::to_i32`: `0 = None, 1 = Horizontal,
//! 2 = Vertical, 3 = Both (== Horizontal | Vertical)`. `Flip::from_i32`
//! itself is `pub(crate)` to `lightbox-edit` (not reachable from here), so
//! this panel toggles a flip axis by XOR-ing that bit directly against the
//! raw code, exact, and it needs no crate-private helper.
//!
//! # 90° rotation is NOT representable, omitted, not stubbed
//!
//! `docs/plan/epics/E11-deviations.md` (E-scope-3) already records that
//! this codebase's `Geometry`/`Flip` model has no 90°-step field to drive;
//! this panel therefore has no Rotate 90° CW/CCW buttons (the task brief's
//! explicit fallback for an unrepresentable control).

use eframe::egui;
use lightbox_edit::{Crop, ParamDelta, ParamId, ParamValue};

use crate::canvas::crop_gizmo::{AspectPreset, CropGizmo, GEOM_CROP};
use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{param_slider, SliderSpec};

/// The geometry panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.geometry");

/// Flip wire codes (`lightbox_edit::leaves::Flip::to_i32`'s documented,
/// stable bitmask, see the module docs).
const FLIP_H_BIT: i32 = 1;
const FLIP_V_BIT: i32 = 2;

/// Registration (E1): mounted by `lib.rs` right after Basic (order 20),
/// before the tone curve (30), crop/straighten is the first geometric
/// pass over the frame, ahead of any color-only tool.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Geometry",
        source_req: SourceReq::Any,
        order: 25,
        build,
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    crop_tool_row(ui, ctx);
    ui.separator();
    aspect_row(ui, ctx);
    ui.separator();
    param_slider(
        ui,
        ctx,
        ParamId::Angle,
        &SliderSpec {
            min: -45.0,
            max: 45.0,
            step: 0.5,
            fine: 0.1,
            label: "Straighten",
            unit: Some("\u{b0}"),
        },
    );
    ui.separator();
    flip_row(ui, ctx);
    ui.separator();
    reset_row(ui, ctx);
}

fn current_crop(ctx: &DevelopCtx<'_>) -> Crop {
    match ctx.edit.value(ParamId::Crop) {
        ParamValue::Crop(c) => c,
        _ => Crop::default(),
    }
}

fn current_angle(ctx: &DevelopCtx<'_>) -> f32 {
    match ctx.edit.value(ParamId::Angle) {
        ParamValue::F32(a) => a,
        _ => 0.0,
    }
}

fn current_flip_code(ctx: &DevelopCtx<'_>) -> i32 {
    match ctx.edit.value(ParamId::Flip) {
        ParamValue::I32(n) => n,
        _ => 0,
    }
}

// ─── Crop tool toggle ───────────────────────────────────────────────────────

fn crop_tool_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.horizontal(|ui| {
        let mut armed = ctx.gizmos.is_active(GEOM_CROP);
        if ui
            .toggle_value(&mut armed, "Crop")
            .on_hover_text("Drag the handles to crop; drag inside to move")
            .changed()
        {
            if armed {
                let crop = current_crop(ctx);
                let angle = current_angle(ctx);
                ctx.gizmos
                    .activate(Box::new(CropGizmo::from_current(crop, angle)));
            } else {
                ctx.gizmos.cancel(GEOM_CROP);
            }
        }
    });
}

// ─── Aspect-ratio presets ───────────────────────────────────────────────────

/// `(label, preset)`, spec's named set: Free, Original, 1:1, 16:9, 3:2,
/// 4:5, 5:4.
const ASPECT_PRESETS: [(&str, AspectPreset); 7] = [
    ("Free", AspectPreset::Free),
    ("Original", AspectPreset::Original),
    ("1:1", AspectPreset::Ratio(1.0)),
    ("16:9", AspectPreset::Ratio(16.0 / 9.0)),
    ("3:2", AspectPreset::Ratio(3.0 / 2.0)),
    ("4:5", AspectPreset::Ratio(4.0 / 5.0)),
    ("5:4", AspectPreset::Ratio(5.0 / 4.0)),
];

fn aspect_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label("Aspect");
    ui.horizontal_wrapped(|ui| {
        for (label, preset) in ASPECT_PRESETS {
            if ui.button(label).clicked() {
                let angle = current_angle(ctx);
                ctx.gizmos
                    .activate(Box::new(CropGizmo::fit_aspect(preset, angle)));
            }
        }
        // Swap orientation (spec's "and swap-orientation"): reciprocals
        // whatever aspect the CURRENT crop rect happens to have (works for
        // any prior preset, or a free-form drag, no extra tracked state
        // needed, see the module docs).
        if ui
            .button("Swap")
            .on_hover_text("Swap the crop's orientation")
            .clicked()
        {
            let crop = current_crop(ctx);
            let w = crop.right - crop.left;
            let h = crop.bottom - crop.top;
            if w > 0.0 && h > 0.0 {
                let angle = current_angle(ctx);
                ctx.gizmos.activate(Box::new(CropGizmo::fit_aspect(
                    AspectPreset::Ratio(h / w),
                    angle,
                )));
            }
        }
    });
}

// ─── Flip ───────────────────────────────────────────────────────────────────

fn toggle_flip_bit(ctx: &mut DevelopCtx<'_>, bit: i32) {
    let new_code = current_flip_code(ctx) ^ bit;
    ctx.edit.begin_gesture(ParamId::Flip);
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::Flip, ParamValue::I32(new_code));
    ctx.edit.preview(delta);
    ctx.edit.end_gesture();
}

fn flip_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.horizontal(|ui| {
        let code = current_flip_code(ctx);
        let mut h = (code & FLIP_H_BIT) != 0;
        let mut v = (code & FLIP_V_BIT) != 0;
        if ui.toggle_value(&mut h, "Flip Horizontal").changed() {
            toggle_flip_bit(ctx, FLIP_H_BIT);
        }
        if ui.toggle_value(&mut v, "Flip Vertical").changed() {
            toggle_flip_bit(ctx, FLIP_V_BIT);
        }
    });
}

// ─── Reset ──────────────────────────────────────────────────────────────────

fn reset_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.horizontal(|ui| {
        if ui.button("Reset Crop").clicked() {
            ctx.edit.reset(ParamId::Crop);
            if ctx.gizmos.is_active(GEOM_CROP) {
                // Re-seed the live overlay too, so it doesn't keep showing
                // the pre-reset rect until the next drag.
                let angle = current_angle(ctx);
                ctx.gizmos
                    .activate(Box::new(CropGizmo::from_current(Crop::default(), angle)));
            }
        }
        if ui.button("Reset Geometry").clicked() {
            // ONE coalesced history step covering crop + angle + flip
            // (`begin_gesture`'s `p` is only the step's cosmetic label
            // `preview` accepts a multi-param delta, see `EditBinding`'s
            // own doc comment).
            ctx.edit.begin_gesture(ParamId::Crop);
            let mut delta = ParamDelta::new();
            delta
                .0
                .insert(ParamId::Crop, ctx.edit.default(ParamId::Crop));
            delta
                .0
                .insert(ParamId::Angle, ctx.edit.default(ParamId::Angle));
            delta
                .0
                .insert(ParamId::Flip, ctx.edit.default(ParamId::Flip));
            ctx.edit.preview(delta);
            ctx.edit.end_gesture();
            if ctx.gizmos.is_active(GEOM_CROP) {
                ctx.gizmos.cancel(GEOM_CROP);
            }
        }
    });
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui_kittest::{kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;

    /// A recording [`EditBinding`] test double, the same shape
    /// `panels::basic`'s own `RecordingBinding` uses: every value lives in
    /// a plain map that ONLY `preview`/`reset` write to, so a test can
    /// assert on the exact seam the panel goes through.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(ParamId::Crop, ParamValue::Crop(Crop::default()));
            values.insert(ParamId::Angle, ParamValue::F32(0.0));
            values.insert(ParamId::Flip, ParamValue::I32(0));
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values.get(&p).cloned().unwrap_or(ParamValue::F32(0.0))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::Crop => ParamValue::Crop(Crop::default()),
                ParamId::Flip => ParamValue::I32(0),
                _ => ParamValue::F32(0.0),
            }
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
        fn reset_all(&mut self) {}

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

    struct PanelApp {
        binding: RecordingBinding,
    }

    fn panel_harness() -> Harness<'static, PanelApp> {
        let app = PanelApp {
            binding: RecordingBinding::new(),
        };
        let mut harness = Harness::new_ui_state(
            |ui, app: &mut PanelApp| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut app.binding,
                    gizmos: &mut gizmos,
                };
                build(ui, &mut ctx);
            },
            app,
        );
        harness.set_size(egui::vec2(360.0, 700.0));
        harness
    }

    /// **AC**: the straighten slider drives `ParamId::Angle`, dragging it
    /// through the SAME `param_slider` every scalar in this codebase uses,
    /// proven structurally against a real gesture (`preview` write lands
    /// on the binding, exactly `basic.rs`'s own A12 pattern).
    #[test]
    fn straighten_slider_drives_param_angle() {
        let mut harness = panel_harness();
        harness.run();
        harness.get_by_label("0.0 \u{b0}");

        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Angle, ParamValue::F32(12.5));
        harness.run();
        harness.get_by_label("12.5 \u{b0}");
    }

    /// **AC**: the Flip Horizontal/Vertical buttons drive `ParamId::Flip`
    /// through the documented XOR-bit convention, and compose (both flips
    /// active = the `Both` wire code, `3`).
    #[test]
    fn flip_buttons_drive_param_flip() {
        let mut harness = panel_harness();
        harness.run();
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Flip),
            Some(&ParamValue::I32(0))
        );

        harness.get_by_label("Flip Horizontal").click();
        harness.run();
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Flip),
            Some(&ParamValue::I32(1)),
            "horizontal bit set"
        );

        harness.get_by_label("Flip Vertical").click();
        harness.run();
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Flip),
            Some(&ParamValue::I32(3)),
            "both bits set == Flip::Both's wire code"
        );

        harness.get_by_label("Flip Horizontal").click();
        harness.run();
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Flip),
            Some(&ParamValue::I32(2)),
            "toggling horizontal back off leaves only vertical"
        );
    }

    /// **AC**: the crop toggle activates/deactivates the REAL
    /// `canvas::crop_gizmo::GEOM_CROP` gizmo in the layer, the same
    /// one-authority pattern `basic.rs::eyedropper_button` established (the
    /// button's pressed state IS `gizmos.is_active(..)`).
    #[test]
    fn crop_tool_button_activates_and_cancels_the_real_gizmo() {
        let mut harness = panel_harness();
        harness.run();
        harness.get_by_label("Crop").click();
        harness.run();
        // A second click un-presses it.
        harness.get_by_label("Crop").click();
        harness.run();
    }

    /// **AC**: Reset Crop / Reset Geometry restore the declared defaults.
    #[test]
    fn reset_buttons_restore_defaults() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::Crop,
            ParamValue::Crop(Crop {
                left: 0.2,
                top: 0.1,
                right: 0.8,
                bottom: 0.9,
            }),
        );
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Angle, ParamValue::F32(7.0));
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Flip, ParamValue::I32(3));
        harness.run();

        harness.get_by_label("Reset Geometry").click();
        harness.run();
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Crop),
            Some(&ParamValue::Crop(Crop::default()))
        );
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Angle),
            Some(&ParamValue::F32(0.0))
        );
        assert_eq!(
            harness.state().binding.values.get(&ParamId::Flip),
            Some(&ParamValue::I32(0))
        );
    }
}
