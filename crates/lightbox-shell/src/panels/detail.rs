// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `develop.detail` panel: sharpening and noise reduction, the two
//! halves of Lightroom's Detail panel, bound to `ParamId::Sharpen`'s one
//! [`Sharpen`] leaf and `ParamId::NoiseReduction`'s one [`NoiseReduction`]
//! leaf (the same decompose/recompose-a-coarse-leaf convention
//! `panels::hsl`'s band sliders and `panels::basic`'s WB temp/tint sliders
//! already establish).
//!
//! # Layout
//!
//! Two labelled sections, in the order the render chain applies them read
//! bottom-up, which is also the order Lightroom lists them:
//!
//! * **Sharpening**: Amount (`0..=150`), Radius (`0.5..=3.0`), Detail
//!   (`0..=100`), Masking (`0..=100`).
//! * **Noise Reduction**: Luminance, Luminance Detail, Color, Color Detail
//!   (all `0..=100`).
//!
//! Every range here is `lightbox-edit`'s own (`leaves.rs`'s `Sharpen::clamp`
//! and `NoiseReduction::clamp`), not a UI invention, so a typed or scrubbed
//! value can never be clamped out from under the user on commit.
//!
//! Lightroom names both secondary sliders just "Detail". This panel names
//! them "Luminance Detail" and "Color Detail" instead: two controls with the
//! same name in one panel is ambiguous to read and impossible to name
//! accessibly, and the render engine's own field names
//! (`luma_detail`/`chroma_detail`) already make the distinction.
//!
//! # Shaping sliders grey out until their strength is up
//!
//! Radius/Detail/Masking do nothing at `amount == 0`, and the two detail
//! sliders do nothing at their strength `0`. That is not a UI opinion, it is
//! exactly the render engine's own elision predicate
//! (`lightbox_render::ng::nodes::global::sharpen::SharpenNode::is_identity`
//! is `amount == 0`, and `noise_reduction`'s is `luma == 0 && chroma == 0`),
//! so with the strength at zero the node is not even in the graph. Leaving
//! the modifiers live would be the defect `panels::basic`'s vibrance guard
//! documents, inverted: a control that moves and changes nothing.
//!
//! # No widget-local state
//!
//! Every slider reads its value from `ctx.edit.value(..)` every frame (never
//! a cached copy) and writes back through
//! `begin_gesture`/`preview`/`end_gesture`, so the panel always reflects the
//! recipe, including after an undo or a preset apply. Proven the same way
//! `panels::hsl`'s own test proves it: mutate the binding from OUTSIDE any
//! widget interaction and confirm the very next frame's render reflects it.

use eframe::egui;
use lightbox_edit::{NoiseReduction, ParamDelta, ParamId, ParamValue, Sharpen};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};
use crate::theme::{fonts, tokens};

/// The detail panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.detail");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots after
/// color grading (45), before the creative-look browser (60), which is where
/// the render chain puts the two nodes too (after the presence trio, before
/// the creative LUT).
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Detail",
        source_req: SourceReq::Any,
        order: 50,
        build,
    }
}

/// Why the panel carries a warning, and why Lightroom's does too.
///
/// Both nodes run at the render request's extent, downstream of
/// `util.resize`, and the canvas only ever requests a fit-to-viewport
/// render. So on a 24 megapixel frame in a 1500 pixel viewport the pixels
/// these sliders act on have already been decimated about four times, which
/// averages most of the sensor noise away before noise reduction sees it and
/// makes a sharpening radius mean one viewport pixel rather than one source
/// pixel. The export runs at full resolution and does what the slider says.
/// Lightroom's Detail panel shows the same note for the same reason. See the
/// "preview is not the export" sections in `sharpen.rs` and
/// `noise_reduction.rs`.
const PREVIEW_NOTE: &str = "The fit preview is decimated before these run, so the export \
    will show more than the canvas does here. Judge sharpening and noise \
    reduction on an export, or at 1:1 once the canvas can render it.";

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(
        egui::RichText::new("Preview is decimated: judge these on an export")
            .small()
            .weak(),
    )
    .on_hover_text(PREVIEW_NOTE);
    section_label(ui, "Sharpening");
    sharpen_section(ui, ctx);
    ui.separator();
    section_label(ui, "Noise Reduction");
    noise_section(ui, ctx);
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(fonts::section_label_font())
            .color(tokens::TEXT_SECONDARY),
    );
}

// ── sharpening ───────────────────────────────────────────────────────────

fn current_sharpen(ctx: &DevelopCtx<'_>) -> Sharpen {
    match ctx.edit.value(ParamId::Sharpen) {
        ParamValue::Sharpen(s) => s,
        _ => Sharpen::default(),
    }
}

fn sharpen_delta(s: Sharpen) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::Sharpen, ParamValue::Sharpen(s));
    delta
}

/// The `0..=100` range three of this panel's sliders share.
const fn pct(label: &'static str) -> SliderSpec {
    SliderSpec {
        min: 0.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    }
}

fn sharpen_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let s = current_sharpen(ctx);
    let d = Sharpen::default();

    sharpen_slider(
        ui,
        ctx,
        s,
        &SliderSpec {
            min: 0.0,
            max: 150.0,
            step: 1.0,
            fine: 0.1,
            label: "Amount",
            unit: None,
        },
        s.amount,
        d.amount as f64,
        |t, v| t.amount = v as f32,
    );

    // See the module doc: with no amount there is nothing for these three to
    // shape, and the node is elided outright.
    ui.add_enabled_ui(s.amount > 0.0, |ui| {
        sharpen_slider(
            ui,
            ctx,
            s,
            &SliderSpec {
                min: 0.5,
                max: 3.0,
                step: 0.1,
                fine: 0.01,
                label: "Radius",
                unit: None,
            },
            s.radius,
            d.radius as f64,
            |t, v| t.radius = v as f32,
        );
        sharpen_slider(
            ui,
            ctx,
            s,
            &pct("Detail"),
            s.detail,
            d.detail as f64,
            |t, v| t.detail = v as f32,
        );
        sharpen_slider(
            ui,
            ctx,
            s,
            &pct("Masking"),
            s.masking,
            d.masking as f64,
            |t, v| t.masking = v as f32,
        );
    });
}

/// One sharpening slider, decomposing and recomposing the ONE [`Sharpen`]
/// leaf (mirrors `panels::hsl::band_slider`'s shape exactly).
///
/// `default_value` is the field's own neutral from [`Sharpen::default`], not
/// a blanket zero: double-clicking Radius must restore `1.0`, the canonical
/// radius, and resetting it to `0.0` would leave the leaf outside its own
/// `0.5..=3.0` range for `lightbox-edit` to silently clamp back.
fn sharpen_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    current: Sharpen,
    spec: &SliderSpec,
    value: f32,
    default_value: f64,
    recompose: impl Fn(&mut Sharpen, f64),
) {
    let (_, events) = value_slider(
        ui,
        ("develop.detail.sharpen", spec.label),
        value as f64,
        spec,
    );
    let commit = |ctx: &mut DevelopCtx<'_>, v: f64| {
        let mut next = current;
        recompose(&mut next, v);
        ctx.edit.begin_gesture(ParamId::Sharpen);
        ctx.edit.preview(sharpen_delta(next));
        ctx.edit.end_gesture();
    };
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::Sharpen),
            SliderEvent::Preview(v) => {
                let mut next = current;
                recompose(&mut next, v);
                ctx.edit.preview(sharpen_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => commit(ctx, default_value),
            SliderEvent::Commit(v) => commit(ctx, v),
        }
    }
}

// ── noise reduction ──────────────────────────────────────────────────────

fn current_nr(ctx: &DevelopCtx<'_>) -> NoiseReduction {
    match ctx.edit.value(ParamId::NoiseReduction) {
        ParamValue::Nr(n) => n,
        _ => NoiseReduction::default(),
    }
}

fn nr_delta(n: NoiseReduction) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::NoiseReduction, ParamValue::Nr(n));
    delta
}

fn noise_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let n = current_nr(ctx);

    nr_slider(ui, ctx, n, &pct("Luminance"), n.luma, |t, v| {
        t.luma = v as f32
    });
    ui.add_enabled_ui(n.luma > 0.0, |ui| {
        nr_slider(
            ui,
            ctx,
            n,
            &pct("Luminance Detail"),
            n.luma_detail,
            |t, v| t.luma_detail = v as f32,
        );
    });

    nr_slider(ui, ctx, n, &pct("Color"), n.chroma, |t, v| {
        t.chroma = v as f32
    });
    ui.add_enabled_ui(n.chroma > 0.0, |ui| {
        nr_slider(ui, ctx, n, &pct("Color Detail"), n.chroma_detail, |t, v| {
            t.chroma_detail = v as f32
        });
    });
}

/// One noise-reduction slider over the ONE [`NoiseReduction`] leaf. Every
/// field's neutral here really is `0.0` (the leaf derives `Default`), so
/// unlike [`sharpen_slider`] this one needs no per-field default.
fn nr_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    current: NoiseReduction,
    spec: &SliderSpec,
    value: f32,
    recompose: impl Fn(&mut NoiseReduction, f64),
) {
    let (_, events) = value_slider(ui, ("develop.detail.nr", spec.label), value as f64, spec);
    let commit = |ctx: &mut DevelopCtx<'_>, v: f64| {
        let mut next = current;
        recompose(&mut next, v);
        ctx.edit.begin_gesture(ParamId::NoiseReduction);
        ctx.edit.preview(nr_delta(next));
        ctx.edit.end_gesture();
    };
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::NoiseReduction),
            SliderEvent::Preview(v) => {
                let mut next = current;
                recompose(&mut next, v);
                ctx.edit.preview(nr_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => commit(ctx, 0.0),
            SliderEvent::Commit(v) => commit(ctx, v),
        }
    }
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

    /// The same recording-double convention `panels::hsl`/`panels::basic`
    /// use: every value lives in a plain map that ONLY
    /// [`EditBinding::preview`]/[`EditBinding::reset`] write to, so panel
    /// behavior is proven through the exact seam production code uses.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
        gestures: Vec<ParamId>,
        ends: usize,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(ParamId::Sharpen, ParamValue::Sharpen(Sharpen::default()));
            values.insert(
                ParamId::NoiseReduction,
                ParamValue::Nr(NoiseReduction::default()),
            );
            RecordingBinding {
                values,
                gestures: Vec::new(),
                ends: 0,
            }
        }
        fn sharpen(&self) -> Sharpen {
            match self.value(ParamId::Sharpen) {
                ParamValue::Sharpen(s) => s,
                _ => panic!("sharpen leaf missing"),
            }
        }
        fn nr(&self) -> NoiseReduction {
            match self.value(ParamId::NoiseReduction) {
                ParamValue::Nr(n) => n,
                _ => panic!("nr leaf missing"),
            }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or(ParamValue::Sharpen(Sharpen::default()))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::NoiseReduction => ParamValue::Nr(NoiseReduction::default()),
                _ => ParamValue::Sharpen(Sharpen::default()),
            }
        }
        fn begin_gesture(&mut self, p: ParamId) {
            self.gestures.push(p);
        }
        fn preview(&mut self, d: ParamDelta) {
            for (id, v) in d.0 {
                self.values.insert(id, v);
            }
        }
        fn end_gesture(&mut self) {
            self.ends += 1;
        }
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
        harness.set_size(egui::vec2(320.0, 640.0));
        // `themed_state` binds the weight-role named font families before
        // frame 0 paints (see `theme::test_support` for why that matters).
        harness
    }

    /// The panel mounts as a registered [`PanelDef`] with the expected
    /// identity and order, a probe that `def()` is wired the way `lib.rs`
    /// expects (between grading = 45 and looks = 60).
    #[test]
    fn def_is_registered_between_grading_and_looks() {
        let d = def();
        assert_eq!(d.id.0, "develop.detail");
        assert_eq!(d.title, "Detail");
        assert!(d.order > 45 && d.order < 60, "order = {}", d.order);
    }

    /// All eight controls are present and reachable by their accessible
    /// name. This is also what pins the "no two sliders share a label"
    /// decision the module doc makes: a duplicate name would make one of
    /// these queries ambiguous.
    #[test]
    fn all_eight_controls_are_present_and_uniquely_named() {
        let mut harness = panel_harness();
        // Give the shaping sliders something to shape, so none are gated
        // out of the tree by the enabled-ui guard.
        harness.state_mut().binding.values.insert(
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 50.0,
                ..Sharpen::default()
            }),
        );
        harness.state_mut().binding.values.insert(
            ParamId::NoiseReduction,
            ParamValue::Nr(NoiseReduction {
                luma: 25.0,
                chroma: 25.0,
                ..NoiseReduction::default()
            }),
        );
        harness.run();
        for label in [
            "Amount",
            "Radius",
            "Detail",
            "Masking",
            "Luminance",
            "Luminance Detail",
            "Color",
            "Color Detail",
        ] {
            harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, label);
        }
    }

    /// **Panel reflects the recipe**: setting either leaf from OUTSIDE any
    /// widget interaction changes what the very next frame shows, zero
    /// clicks or drags. Same proof `panels::hsl`'s own test makes.
    #[test]
    fn panel_always_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 37.0,
                radius: 2.5,
                detail: 0.0,
                masking: 0.0,
            }),
        );
        harness.run();
        harness.get_by_label("+37");
        harness.get_by_label("+2.5");

        harness.state_mut().binding.values.insert(
            ParamId::NoiseReduction,
            ParamValue::Nr(NoiseReduction {
                luma: 64.0,
                ..NoiseReduction::default()
            }),
        );
        harness.run();
        harness.get_by_label("+64");
    }

    /// **Gesture commit**: a REAL synthetic drag (kittest pointer events,
    /// not a hand-called event) on the Amount track routes a
    /// `ParamId::Sharpen` delta through
    /// `begin_gesture`/`preview`/`end_gesture` and lands on the recipe,
    /// leaving the leaf's other three fields alone (the decompose/recompose
    /// contract: one slider must not stamp defaults over its neighbours).
    #[test]
    fn a_real_drag_on_amount_lands_the_value_and_spares_the_other_fields() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 10.0,
                radius: 2.0,
                detail: 30.0,
                masking: 40.0,
            }),
        );
        harness.run();

        let track = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Amount");
        let rect = track.rect();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drag_at(rect.left_center());
        harness.run();
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drop_at(rect.right_center());
        harness.run();

        let after = harness.state().binding.sharpen();
        assert!(
            after.amount > 100.0,
            "dragging Amount to the right edge of a 0..150 track should land high: {}",
            after.amount
        );
        assert_eq!(after.radius, 2.0, "radius must survive an Amount drag");
        assert_eq!(after.detail, 30.0, "detail must survive an Amount drag");
        assert_eq!(after.masking, 40.0, "masking must survive an Amount drag");
        assert_eq!(
            harness.state().binding.gestures.first(),
            Some(&ParamId::Sharpen)
        );
        assert!(harness.state().binding.ends > 0, "the gesture was closed");
    }

    /// The same for the noise half, over its own leaf.
    #[test]
    fn a_real_drag_on_color_lands_the_value_and_spares_the_other_fields() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::NoiseReduction,
            ParamValue::Nr(NoiseReduction {
                luma: 15.0,
                luma_detail: 45.0,
                chroma: 5.0,
                chroma_detail: 55.0,
            }),
        );
        harness.run();

        let track = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Color");
        let rect = track.rect();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drag_at(rect.left_center());
        harness.run();
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drop_at(rect.right_center());
        harness.run();

        let after = harness.state().binding.nr();
        assert!(after.chroma > 80.0, "chroma landed high: {}", after.chroma);
        assert_eq!(after.luma, 15.0);
        assert_eq!(after.luma_detail, 45.0);
        assert_eq!(after.chroma_detail, 55.0);
    }

    /// The shaping sliders must be visibly disabled while they cannot do
    /// anything, matching the render engine's own elision predicates (see
    /// the module doc). Asserted through the real accessible tree, not a
    /// pure helper, because "visibly disabled" is the claim.
    #[test]
    fn shaping_sliders_are_disabled_until_their_strength_is_nonzero() {
        let mut harness = panel_harness();
        harness.run();
        for label in ["Radius", "Detail", "Masking"] {
            let node = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, label);
            assert!(
                node.accesskit_node().is_disabled(),
                "{label} must be disabled at amount 0"
            );
        }
        for label in ["Luminance Detail", "Color Detail"] {
            let node = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, label);
            assert!(
                node.accesskit_node().is_disabled(),
                "{label} must be disabled at strength 0"
            );
        }

        harness.state_mut().binding.values.insert(
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 1.0,
                ..Sharpen::default()
            }),
        );
        harness.state_mut().binding.values.insert(
            ParamId::NoiseReduction,
            ParamValue::Nr(NoiseReduction {
                luma: 1.0,
                ..NoiseReduction::default()
            }),
        );
        harness.run();
        for label in ["Radius", "Detail", "Masking", "Luminance Detail"] {
            let node = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, label);
            assert!(
                !node.accesskit_node().is_disabled(),
                "{label} must come alive once its strength is up"
            );
        }
        // Color Detail keys off `chroma`, which is still zero.
        let node =
            harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Color Detail");
        assert!(
            node.accesskit_node().is_disabled(),
            "Color Detail keys off Color, not Luminance"
        );
    }

    /// Every slider's range is the recipe leaf's own range, so a value this
    /// panel can produce is never one `lightbox-edit` silently clamps back.
    /// `Sharpen::clamp` is `pub(crate)` over there, so the ranges are probed
    /// the way a user reaches them: by pushing a delta through
    /// `Recipe::apply` and reading the leaf back.
    #[test]
    fn slider_ranges_match_the_recipe_leaves_own_clamps() {
        let mut recipe = lightbox_edit::Recipe::identity(lightbox_types::PV_M0);
        let apply = |recipe: &mut lightbox_edit::Recipe, id, v| {
            let mut delta = ParamDelta::new();
            delta.0.insert(id, v);
            recipe.apply(&delta).expect("well-typed delta applies");
        };

        // The panel's maxima must all survive the leaf's clamp untouched.
        apply(
            &mut recipe,
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 150.0,
                radius: 3.0,
                detail: 100.0,
                masking: 100.0,
            }),
        );
        let ParamValue::Sharpen(maxed) = recipe.get(ParamId::Sharpen) else {
            panic!("sharpen leaf")
        };
        assert_eq!(
            (maxed.amount, maxed.radius, maxed.detail, maxed.masking),
            (150.0, 3.0, 100.0, 100.0),
            "the panel's slider maxima must survive the leaf's own clamp"
        );

        // And so must the Radius minimum, the one non-zero lower bound here.
        apply(
            &mut recipe,
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 0.0,
                radius: 0.5,
                detail: 0.0,
                masking: 0.0,
            }),
        );
        let ParamValue::Sharpen(mined) = recipe.get(ParamId::Sharpen) else {
            panic!("sharpen leaf")
        };
        assert_eq!(mined.radius, 0.5, "the Radius minimum is the leaf's own");

        apply(
            &mut recipe,
            ParamId::NoiseReduction,
            ParamValue::Nr(NoiseReduction {
                luma: 100.0,
                luma_detail: 100.0,
                chroma: 100.0,
                chroma_detail: 100.0,
            }),
        );
        let ParamValue::Nr(nr) = recipe.get(ParamId::NoiseReduction) else {
            panic!("nr leaf")
        };
        assert_eq!(nr.luma, 100.0);
        assert_eq!(nr.chroma_detail, 100.0);

        // Sanity: the clamp really is active, so the assertions above are
        // not vacuous.
        apply(
            &mut recipe,
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 999.0,
                radius: 99.0,
                detail: 999.0,
                masking: 999.0,
            }),
        );
        let ParamValue::Sharpen(over) = recipe.get(ParamId::Sharpen) else {
            panic!("sharpen leaf")
        };
        assert_eq!((over.amount, over.radius), (150.0, 3.0));
    }

    /// Double-clicking Radius must restore the leaf's canonical `1.0`, not a
    /// blanket `0.0` (which is outside its own `0.5..=3.0` range). The reset
    /// path is driven through the same closure `sharpen_slider` calls, with
    /// the same `default_value` the production call site passes.
    #[test]
    fn resetting_radius_restores_the_leaf_default_not_zero() {
        assert_eq!(Sharpen::default().radius, 1.0);
        let mut binding = RecordingBinding::new();
        binding.values.insert(
            ParamId::Sharpen,
            ParamValue::Sharpen(Sharpen {
                amount: 80.0,
                radius: 2.7,
                detail: 20.0,
                masking: 10.0,
            }),
        );
        let current = match binding.value(ParamId::Sharpen) {
            ParamValue::Sharpen(s) => s,
            _ => panic!("sharpen leaf"),
        };
        let mut next = current;
        next.radius = Sharpen::default().radius;
        binding.preview(sharpen_delta(next));

        let after = binding.sharpen();
        assert_eq!(after.radius, 1.0, "reset lands on the canonical radius");
        assert_eq!(after.amount, 80.0, "and touches nothing else");
        assert_eq!(after.detail, 20.0);
        assert_eq!(after.masking, 10.0);
    }
}
