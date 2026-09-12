// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `develop.effects` panel: post-crop vignetting and grain, the rail
//! controls for `lightbox_render`'s `fx.vignette` and `fx.grain`
//! nodes.
//!
//! # Why this panel exists
//!
//! `ParamId::PostCropVignette` and `ParamId::Grain` have been in the recipe
//! schema, the edit store and the `crs:` XMP mapping the whole time
//! (`lightbox_edit::leaves::{PostCropVignette, Grain}`,
//! `lightbox_edit::xmp_map::from_xmp`), so a Lightroom preset carrying
//! either has always imported cleanly and been stored faithfully. Until the
//! two render nodes landed there was nothing to show for it and no way to
//! reach the values by hand. This panel is the hand-reachable half.
//!
//! # Shape
//!
//! Both leaves are **coarse** (one `ParamId` addresses a whole struct), so
//! the sliders decompose and recompose them the way `panels::hsl`'s band
//! table and `panels::bw`'s channel mixer already do: read the current
//! leaf, change one field, send the whole leaf back inside one gesture.
//! There is no widget-local state, every frame re-reads the recipe.
//!
//! # The Highlights slider is disabled unless the vignette darkens
//!
//! `highlights` shields bright pixels from a DARKENING vignette; a
//! lightening one has no highlights to rescue, and the render node makes it
//! inert there on purpose (`nodes::global::vignette::apply_vignette`). The
//! slider greys itself out on the same condition rather than moving with no
//! effect, the same posture `panels::basic` takes for vibrance/saturation
//! in Monochrome.

use eframe::egui;
use lightbox_edit::leaves::{Grain, PostCropVignette};
use lightbox_edit::{ParamDelta, ParamId, ParamValue};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};

/// The effects panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.effects");

/// Registration: mounted by `lib.rs` at app construction. Slots after
/// colour grading (45) and before the creative-look browser (60), which is
/// where Lightroom's own Effects panel sits relative to the colour tools.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Effects",
        source_req: SourceReq::Any,
        // 55: after Detail (50) and before Looks (60). Lightroom's own rail
        // runs Detail, then Lens Corrections, then Transform, then Effects, so
        // Effects sits below Detail rather than beside it. Detail also claims
        // 50, and two panels sharing an order would make the rail depend on
        // registration sequence rather than on this number.
        order: 55,
        build,
    }
}

// ── reading and writing the two coarse leaves ─────────────────────────────

/// The current vignette leaf, or the neutral default if the binding hands
/// back an unexpected kind (never in production; the recording doubles in
/// this module's own tests rely on it).
pub fn current_vignette(ctx: &DevelopCtx<'_>) -> PostCropVignette {
    match ctx.edit.value(ParamId::PostCropVignette) {
        ParamValue::Vignette(v) => v,
        _ => PostCropVignette::default(),
    }
}

/// The current grain leaf.
pub fn current_grain(ctx: &DevelopCtx<'_>) -> Grain {
    match ctx.edit.value(ParamId::Grain) {
        ParamValue::Grain(g) => g,
        _ => Grain::default(),
    }
}

fn vignette_delta(v: PostCropVignette) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::PostCropVignette, ParamValue::Vignette(v));
    d
}

fn grain_delta(g: Grain) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Grain, ParamValue::Grain(g));
    d
}

/// True while the vignette is darkening, the only case in which the
/// `highlights` control does anything (see the module docs). Unit-tested
/// directly rather than through a pixel test.
pub fn vignette_darkens(v: &PostCropVignette) -> bool {
    v.amount < 0.0
}

// ── slider specs ──────────────────────────────────────────────────────────

const fn pm100(label: &'static str) -> SliderSpec {
    SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    }
}

const fn zero100(label: &'static str) -> SliderSpec {
    SliderSpec {
        min: 0.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    }
}

// ── the panel ─────────────────────────────────────────────────────────────

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    vignette_section(ui, ctx);
    ui.separator();
    grain_section(ui, ctx);
}

fn vignette_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Post-Crop Vignetting").strong());
    let v = current_vignette(ctx);
    vignette_slider(ui, ctx, &v, VignetteField::Amount);
    vignette_slider(ui, ctx, &v, VignetteField::Midpoint);
    vignette_slider(ui, ctx, &v, VignetteField::Roundness);
    vignette_slider(ui, ctx, &v, VignetteField::Feather);
    ui.add_enabled_ui(vignette_darkens(&v), |ui| {
        vignette_slider(ui, ctx, &v, VignetteField::Highlights);
    });
    if ui
        .button("Reset vignette")
        .on_hover_text("Back to no vignette")
        .clicked()
    {
        ctx.edit.reset(ParamId::PostCropVignette);
    }
}

fn grain_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Grain").strong());
    let g = current_grain(ctx);
    grain_slider(ui, ctx, &g, GrainField::Amount);
    // Size and roughness only shape grain that is actually being added.
    ui.add_enabled_ui(g.amount > 0.0, |ui| {
        grain_slider(ui, ctx, &g, GrainField::Size);
        grain_slider(ui, ctx, &g, GrainField::Roughness);
    });
    if ui
        .button("Reset grain")
        .on_hover_text("Back to no grain")
        .clicked()
    {
        ctx.edit.reset(ParamId::Grain);
    }
}

/// Which field of the one [`PostCropVignette`] leaf a slider row addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VignetteField {
    Amount,
    Midpoint,
    Roundness,
    Feather,
    Highlights,
}

impl VignetteField {
    fn spec(self) -> SliderSpec {
        match self {
            VignetteField::Amount => pm100("Amount"),
            VignetteField::Midpoint => zero100("Midpoint"),
            VignetteField::Roundness => pm100("Roundness"),
            VignetteField::Feather => zero100("Feather"),
            VignetteField::Highlights => zero100("Highlights"),
        }
    }

    fn get(self, v: &PostCropVignette) -> f32 {
        match self {
            VignetteField::Amount => v.amount,
            VignetteField::Midpoint => v.midpoint,
            VignetteField::Roundness => v.roundness,
            VignetteField::Feather => v.feather,
            VignetteField::Highlights => v.highlights,
        }
    }

    fn set(self, v: &mut PostCropVignette, value: f32) {
        match self {
            VignetteField::Amount => v.amount = value,
            VignetteField::Midpoint => v.midpoint = value,
            VignetteField::Roundness => v.roundness = value,
            VignetteField::Feather => v.feather = value,
            VignetteField::Highlights => v.highlights = value,
        }
    }

    /// The neutral value for this field alone (double-click reset resets the
    /// row, not the whole leaf).
    fn neutral(self) -> f32 {
        self.get(&PostCropVignette::default())
    }

    fn id_salt(self) -> &'static str {
        match self {
            VignetteField::Amount => "amount",
            VignetteField::Midpoint => "midpoint",
            VignetteField::Roundness => "roundness",
            VignetteField::Feather => "feather",
            VignetteField::Highlights => "highlights",
        }
    }
}

/// One vignette slider, decomposing/recomposing the ONE
/// [`PostCropVignette`] leaf (same shape as `panels::bw::mix_slider`).
fn vignette_slider(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    v: &PostCropVignette,
    field: VignetteField,
) {
    let spec = field.spec();
    let (_, events) = value_slider(
        ui,
        ("develop.effects.vignette", field.id_salt()),
        field.get(v) as f64,
        &spec,
    );
    let write = |ctx: &mut DevelopCtx<'_>, value: f32, one_shot: bool| {
        let mut next = *v;
        field.set(&mut next, value);
        if one_shot {
            ctx.edit.begin_gesture(ParamId::PostCropVignette);
        }
        ctx.edit.preview(vignette_delta(next));
        if one_shot {
            ctx.edit.end_gesture();
        }
    };
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::PostCropVignette),
            SliderEvent::Preview(value) => write(ctx, value as f32, false),
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => write(ctx, field.neutral(), true),
            SliderEvent::Commit(value) => write(ctx, value as f32, true),
        }
    }
}

/// Which field of the one [`Grain`] leaf a slider row addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GrainField {
    Amount,
    Size,
    Roughness,
}

impl GrainField {
    fn spec(self) -> SliderSpec {
        match self {
            GrainField::Amount => zero100("Amount"),
            GrainField::Size => zero100("Size"),
            GrainField::Roughness => zero100("Roughness"),
        }
    }

    fn get(self, g: &Grain) -> f32 {
        match self {
            GrainField::Amount => g.amount,
            GrainField::Size => g.size,
            GrainField::Roughness => g.roughness,
        }
    }

    fn set(self, g: &mut Grain, value: f32) {
        match self {
            GrainField::Amount => g.amount = value,
            GrainField::Size => g.size = value,
            GrainField::Roughness => g.roughness = value,
        }
    }

    fn neutral(self) -> f32 {
        self.get(&Grain::default())
    }

    fn id_salt(self) -> &'static str {
        match self {
            GrainField::Amount => "amount",
            GrainField::Size => "size",
            GrainField::Roughness => "roughness",
        }
    }
}

/// One grain slider, decomposing/recomposing the ONE [`Grain`] leaf.
fn grain_slider(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, g: &Grain, field: GrainField) {
    let spec = field.spec();
    let (_, events) = value_slider(
        ui,
        ("develop.effects.grain", field.id_salt()),
        field.get(g) as f64,
        &spec,
    );
    let write = |ctx: &mut DevelopCtx<'_>, value: f32, one_shot: bool| {
        let mut next = *g;
        field.set(&mut next, value);
        if one_shot {
            ctx.edit.begin_gesture(ParamId::Grain);
        }
        ctx.edit.preview(grain_delta(next));
        if one_shot {
            ctx.edit.end_gesture();
        }
    };
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::Grain),
            SliderEvent::Preview(value) => write(ctx, value as f32, false),
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => write(ctx, field.neutral(), true),
            SliderEvent::Commit(value) => write(ctx, value as f32, true),
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

    // ── the disable predicate ─────────────────────────────────────────────

    #[test]
    fn highlights_is_live_only_while_the_vignette_darkens() {
        let mut v = PostCropVignette::default();
        assert!(!vignette_darkens(&v), "a neutral vignette does nothing");
        v.amount = 40.0;
        assert!(
            !vignette_darkens(&v),
            "a lightening vignette has no highlights to protect"
        );
        v.amount = -40.0;
        assert!(vignette_darkens(&v));
    }

    // ── recording double (same convention as basic.rs/bw.rs/hsl.rs) ───────

    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
        gestures: Vec<ParamId>,
        ends: usize,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(
                ParamId::PostCropVignette,
                ParamValue::Vignette(PostCropVignette::default()),
            );
            values.insert(ParamId::Grain, ParamValue::Grain(Grain::default()));
            RecordingBinding {
                values,
                gestures: Vec::new(),
                ends: 0,
            }
        }

        fn vignette(&self) -> PostCropVignette {
            match self.values.get(&ParamId::PostCropVignette) {
                Some(ParamValue::Vignette(v)) => *v,
                _ => PostCropVignette::default(),
            }
        }

        fn grain(&self) -> Grain {
            match self.values.get(&ParamId::Grain) {
                Some(ParamValue::Grain(g)) => *g,
                _ => Grain::default(),
            }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or(ParamValue::Vignette(PostCropVignette::default()))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::Grain => ParamValue::Grain(Grain::default()),
                _ => ParamValue::Vignette(PostCropVignette::default()),
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
        harness.set_size(egui::vec2(320.0, 640.0));
        harness
    }

    #[test]
    fn def_slots_between_grading_and_looks() {
        let d = def();
        assert_eq!(d.id.0, "develop.effects");
        assert!(d.order > 45 && d.order < 60);
        assert_eq!(d.title, "Effects");
    }

    /// The panel always reflects the recipe: changing the leaves from
    /// OUTSIDE any widget interaction changes what the very next frame
    /// shows (no widget-local state), mirroring `hsl.rs`/`bw.rs`.
    #[test]
    fn sliders_reflect_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::PostCropVignette,
            ParamValue::Vignette(PostCropVignette {
                amount: -37.0,
                midpoint: 62.0,
                ..PostCropVignette::default()
            }),
        );
        harness.state_mut().binding.values.insert(
            ParamId::Grain,
            ParamValue::Grain(Grain {
                amount: 44.0,
                size: 71.0,
                roughness: 0.0,
            }),
        );
        harness.run();
        harness.get_by_label("-37");
        harness.get_by_label("+62");
        harness.get_by_label("+44");
        harness.get_by_label("+71");

        harness.state_mut().binding.values.insert(
            ParamId::PostCropVignette,
            ParamValue::Vignette(PostCropVignette {
                amount: 18.0,
                ..PostCropVignette::default()
            }),
        );
        harness.run();
        harness.get_by_label("+18");
    }

    /// A REAL synthetic drag on the vignette Amount track routes ONE
    /// `ParamId::PostCropVignette` gesture through
    /// `begin_gesture`/`preview`/`end_gesture` and lands on the recipe,
    /// leaving the leaf's OTHER fields alone (the decompose/recompose
    /// contract).
    #[test]
    fn a_real_drag_lands_one_vignette_gesture_without_disturbing_the_other_fields() {
        let start = PostCropVignette {
            amount: 0.0,
            midpoint: 31.0,
            roundness: -12.0,
            feather: 77.0,
            highlights: 5.0,
        };
        let mut harness = Harness::new_ui_state(
            |ui, s: &mut (PostCropVignette, RecordingBinding)| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut s.1,
                    gizmos: &mut gizmos,
                };
                vignette_slider(ui, &mut ctx, &s.0, VignetteField::Amount);
            },
            (start, RecordingBinding::new()),
        );
        harness.set_size(egui::vec2(320.0, 40.0));
        harness.run();
        let track = harness.get_by_role(eframe::egui::accesskit::Role::Slider);
        let rect = track.rect();
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drag_at(rect.right_center());
        harness.run();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drop_at(rect.left_center());
        harness.run();

        let after = harness.state().1.vignette();
        assert!(
            after.amount < -50.0,
            "dragging Amount to the left edge should land a large negative value: {}",
            after.amount
        );
        assert_eq!(after.midpoint, start.midpoint, "midpoint untouched");
        assert_eq!(after.roundness, start.roundness, "roundness untouched");
        assert_eq!(after.feather, start.feather, "feather untouched");
        assert_eq!(after.highlights, start.highlights, "highlights untouched");
        assert_eq!(
            harness.state().1.gestures,
            vec![ParamId::PostCropVignette],
            "exactly one gesture, on the vignette leaf"
        );
        assert_eq!(harness.state().1.ends, 1, "and exactly one end");
    }

    /// The same for grain: one gesture on `ParamId::Grain`, siblings intact.
    #[test]
    fn a_real_drag_lands_one_grain_gesture_without_disturbing_the_other_fields() {
        let start = Grain {
            amount: 0.0,
            size: 33.0,
            roughness: 66.0,
        };
        let mut harness = Harness::new_ui_state(
            |ui, s: &mut (Grain, RecordingBinding)| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut s.1,
                    gizmos: &mut gizmos,
                };
                grain_slider(ui, &mut ctx, &s.0, GrainField::Amount);
            },
            (start, RecordingBinding::new()),
        );
        harness.set_size(egui::vec2(320.0, 40.0));
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

        let after = harness.state().1.grain();
        assert!(
            after.amount > 50.0,
            "dragging Amount to the right edge should land a large value: {}",
            after.amount
        );
        assert_eq!(after.size, start.size, "size untouched");
        assert_eq!(after.roughness, start.roughness, "roughness untouched");
        assert_eq!(harness.state().1.gestures, vec![ParamId::Grain]);
        assert_eq!(harness.state().1.ends, 1);
    }

    #[test]
    fn the_reset_buttons_clear_their_own_leaf_only() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::PostCropVignette,
            ParamValue::Vignette(PostCropVignette {
                amount: -50.0,
                ..PostCropVignette::default()
            }),
        );
        harness.state_mut().binding.values.insert(
            ParamId::Grain,
            ParamValue::Grain(Grain {
                amount: 60.0,
                size: 20.0,
                roughness: 10.0,
            }),
        );
        harness.run();
        harness.get_by_label("Reset vignette").click();
        harness.run();
        assert_eq!(
            harness.state().binding.vignette(),
            PostCropVignette::default()
        );
        assert_eq!(
            harness.state().binding.grain().amount,
            60.0,
            "resetting the vignette must not touch grain"
        );

        harness.get_by_label("Reset grain").click();
        harness.run();
        assert_eq!(harness.state().binding.grain(), Grain::default());
    }
}
