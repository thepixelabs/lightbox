// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `develop.optics` panel: lens corrections (distortion, vignetting),
//! chromatic aberration, and defringe, bound to E09's real param ids through
//! the same [`EditBinding`] seam every other develop panel uses.
//!
//! # ParamId mapping
//!
//! | Control                       | `ParamId`               | Leaf / range                                    |
//! |-------------------------------|-------------------------|-------------------------------------------------|
//! | Enable lens correction        | `LensProfile`           | `Optics::lens_profile` present or not           |
//! | Distortion                    | `LensProfile`           | `LensCorrection::manual_distortion` -100..=100  |
//! | Profile distortion amount     | `LensProfile`           | `LensCorrection::distortion` 0..=200            |
//! | Profile vignetting amount     | `LensProfile`           | `LensCorrection::vignetting` 0..=200            |
//! | Vignetting                    | `VignetteCorr`          | f32, -100..=100                                 |
//! | Remove Chromatic Aberration   | `ChromaticAberration`   | bool                                            |
//! | Defringe                      | `Defringe`              | f32, 0..=100                                    |
//!
//! Four of those live inside the one `Optics` carrier `ParamId::LensProfile`
//! addresses, so they decompose and recompose the whole leaf shell-side, the
//! same convention `panels::basic`'s WB temp/tint sliders follow.
//!
//! # Distortion is a manual dial, not a profile amount
//!
//! The Distortion slider drives `LensCorrection::manual_distortion`: signed,
//! `-100..=100`, neutral `0`, positive removes barrel. That is Lightroom's
//! Manual tab control and it maps to Adobe's own
//! `crs:LensManualDistortionAmount`.
//!
//! It deliberately does NOT drive `LensCorrection::distortion`, which is the
//! *profile* amount (`crs:LensProfileDistortionScale`, "apply N% of the
//! measured profile"). Driving the manual control through that field would
//! write a sidecar telling other software to scale a lens profile when the
//! user asked for barrel correction. See
//! `lightbox_edit::leaves::LensCorrection`.
//!
//! # Three controls are deliberately shown disabled
//!
//! The two profile amounts and "Remove Chromatic Aberration" all need
//! measured per-lens coefficients that this build has no source for. They
//! are rendered, greyed out, and say why on hover, rather than being hidden
//! (the fields exist and round-trip) or left live and inert (a control that
//! moves and does nothing is worse than one that explains itself).

use eframe::egui;
use lightbox_edit::leaves::{LensCorrection, Optics};
use lightbox_edit::{ParamDelta, ParamId, ParamValue};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{param_slider, value_slider, SliderEvent, SliderSpec};

/// The optics panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.optics");

/// Why the two profile-backed controls are inert, shown on hover and as an
/// inline note so the user is never left guessing at a dead control.
const NO_PROFILE_DB: &str = "Lightbox ships no lens-profile database, so there are no measured \
     coefficients to apply. The manual controls below work.";

/// Registration: mounted by `lib.rs` right after Geometry (order 25) and
/// before the tone curve (30). Optics is a correction of the captured
/// frame, so it belongs beside the other geometric work rather than in the
/// colour stack.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Optics",
        source_req: SourceReq::Any,
        order: 27,
        build,
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    lens_section(ui, ctx);
    ui.separator();
    manual_vignetting_section(ui, ctx);
    ui.separator();
    ca_section(ui, ctx);
    ui.separator();
    reset_row(ui, ctx);
}

// ─── the Optics carrier ─────────────────────────────────────────────────────

/// The current `lens_profile`, as `ParamId::LensProfile`'s carrier reports
/// it. The carrier holds ONLY that field (see `Recipe::value`'s A-4 note).
fn current_lens_profile(ctx: &DevelopCtx<'_>) -> Option<LensCorrection> {
    match ctx.edit.value(ParamId::LensProfile) {
        ParamValue::Lens(o) => o.lens_profile,
        _ => None,
    }
}

/// Writes a whole `lens_profile` through the `LensProfile` carrier. The
/// other three `Optics` fields ride along neutral and are ignored on the
/// way in, which is that param id's documented contract.
fn lens_delta(lens_profile: Option<LensCorrection>) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(
        ParamId::LensProfile,
        ParamValue::Lens(Optics {
            lens_profile,
            ..Optics::default()
        }),
    );
    d
}

/// Routes [`value_slider`] events into one `LensProfile` gesture, mirroring
/// `panels::basic::apply_wb_events`. `recompose(v)` builds the whole leaf
/// for the slider's new scalar.
fn apply_lens_events(
    ctx: &mut DevelopCtx<'_>,
    events: Vec<SliderEvent>,
    recompose: impl Fn(f64) -> LensCorrection,
) {
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::LensProfile),
            SliderEvent::Preview(v) => ctx.edit.preview(lens_delta(Some(recompose(v)))),
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => ctx.edit.reset(ParamId::LensProfile),
            SliderEvent::Commit(v) => {
                ctx.edit.begin_gesture(ParamId::LensProfile);
                ctx.edit.preview(lens_delta(Some(recompose(v))));
                ctx.edit.end_gesture();
            }
        }
    }
}

// ─── lens corrections ───────────────────────────────────────────────────────

/// The two profile-amount sliders, which share a range and a reason for
/// being disabled.
const PROFILE_AMOUNT_SPEC: SliderSpec = SliderSpec {
    min: 0.0,
    max: 200.0,
    step: 1.0,
    fine: 0.1,
    label: "",
    unit: None,
};

fn lens_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label("Lens Corrections");
    let current = current_lens_profile(ctx);

    let mut enabled = current.is_some();
    if ui
        .checkbox(&mut enabled, "Enable lens correction")
        .on_hover_text("Manual distortion and vignetting correction for this frame")
        .changed()
    {
        // Enabling seeds `LensCorrection::default()`: profile amounts at
        // unity, manual dial at neutral. Ticking the box never changes a
        // pixel on its own.
        let next = enabled.then(LensCorrection::default);
        ctx.edit.begin_gesture(ParamId::LensProfile);
        ctx.edit.preview(lens_delta(next));
        ctx.edit.end_gesture();
    }

    let profile = match current {
        Some(p) => p,
        None => return,
    };

    ui.label(
        egui::RichText::new("Profile: none installed")
            .small()
            .weak(),
    )
    .on_hover_text(NO_PROFILE_DB);

    // The manual dial. Its own signed field, its own XMP key, no
    // reinterpretation of anything.
    let (_, events) = value_slider(
        ui,
        "develop.optics.distortion",
        profile.manual_distortion as f64,
        &SliderSpec {
            min: -100.0,
            max: 100.0,
            step: 1.0,
            fine: 0.1,
            label: "Distortion",
            unit: None,
        },
    );
    let (profile_id, distortion, vignetting) = (
        profile.profile_id.clone(),
        profile.distortion,
        profile.vignetting,
    );
    apply_lens_events(ctx, events, move |v| LensCorrection {
        profile_id: profile_id.clone(),
        distortion,
        vignetting,
        manual_distortion: v as f32,
    });

    // Present, greyed, and honest: these are the amounts a measured
    // profile's coefficients would be scaled by, and there is no measured
    // profile to scale.
    ui.add_enabled_ui(false, |ui| {
        for (salt, label, value) in [
            (
                "develop.optics.profile_distortion",
                "Profile distortion amount",
                profile.distortion,
            ),
            (
                "develop.optics.profile_vignetting",
                "Profile vignetting amount",
                profile.vignetting,
            ),
        ] {
            let _ = value_slider(
                ui,
                salt,
                value as f64,
                &SliderSpec {
                    label,
                    ..PROFILE_AMOUNT_SPEC
                },
            );
        }
    })
    .response
    .on_hover_text(NO_PROFILE_DB);
}

// ─── manual vignetting ──────────────────────────────────────────────────────

fn manual_vignetting_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label("Manual");
    param_slider(
        ui,
        ctx,
        ParamId::VignetteCorr,
        &SliderSpec {
            min: -100.0,
            max: 100.0,
            step: 1.0,
            fine: 0.1,
            label: "Vignetting",
            unit: None,
        },
    )
    .on_hover_text("Positive brightens the corners, negative darkens them");
}

// ─── chromatic aberration + defringe ────────────────────────────────────────

fn current_ca(ctx: &DevelopCtx<'_>) -> bool {
    matches!(
        ctx.edit.value(ParamId::ChromaticAberration),
        ParamValue::Bool(true)
    )
}

fn ca_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label("Chromatic Aberration");

    let mut ca = current_ca(ctx);
    ui.add_enabled_ui(false, |ui| {
        ui.checkbox(&mut ca, "Remove Chromatic Aberration");
    })
    .response
    .on_hover_text(NO_PROFILE_DB);
    ui.label(
        egui::RichText::new("Needs a measured lens profile; not available yet.")
            .small()
            .weak(),
    );

    param_slider(
        ui,
        ctx,
        ParamId::Defringe,
        &SliderSpec {
            min: 0.0,
            max: 100.0,
            step: 1.0,
            fine: 0.1,
            label: "Defringe",
            unit: None,
        },
    )
    .on_hover_text("Removes purple and green fringes on high-contrast edges");
}

// ─── reset ──────────────────────────────────────────────────────────────────

fn reset_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    if ui.button("Reset Optics").clicked() {
        // ONE coalesced history step over all four optics params (the
        // `begin_gesture` id is only the step's label; `preview` takes a
        // multi-param delta, see `EditBinding`'s own doc comment).
        ctx.edit.begin_gesture(ParamId::LensProfile);
        let mut delta = ParamDelta::new();
        for p in [
            ParamId::LensProfile,
            ParamId::ChromaticAberration,
            ParamId::Defringe,
            ParamId::VignetteCorr,
        ] {
            let d = ctx.edit.default(p);
            delta.0.insert(p, d);
        }
        ctx.edit.preview(delta);
        ctx.edit.end_gesture();
    }
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

    /// The same recording test double `panels::geometry` and `panels::basic`
    /// use: every value lives in a plain map that ONLY `preview`/`reset`
    /// write to, so a test asserts on the exact seam the panel goes through.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(ParamId::LensProfile, ParamValue::Lens(Optics::default()));
            values.insert(ParamId::ChromaticAberration, ParamValue::Bool(false));
            values.insert(ParamId::Defringe, ParamValue::F32(0.0));
            values.insert(ParamId::VignetteCorr, ParamValue::F32(0.0));
            RecordingBinding { values }
        }

        fn lens(&self) -> Option<LensCorrection> {
            match self.values.get(&ParamId::LensProfile) {
                Some(ParamValue::Lens(o)) => o.lens_profile.clone(),
                _ => None,
            }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values.get(&p).cloned().unwrap_or(ParamValue::F32(0.0))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::LensProfile => ParamValue::Lens(Optics::default()),
                ParamId::ChromaticAberration => ParamValue::Bool(false),
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

    /// **AC**: ticking "Enable lens correction" creates the `lens_profile`
    /// leaf at its neutral default, so arming the section changes no pixel;
    /// unticking removes it again.
    #[test]
    fn enable_checkbox_seeds_a_neutral_profile_and_clears_it() {
        let mut harness = panel_harness();
        harness.run();
        assert_eq!(harness.state().binding.lens(), None);

        harness.get_by_label("Enable lens correction").click();
        harness.run();
        let seeded = harness.state().binding.lens().expect("leaf created");
        assert_eq!(seeded, LensCorrection::default());
        assert_eq!(
            seeded.manual_distortion, 0.0,
            "the manual dial must arm at neutral"
        );

        harness.get_by_label("Enable lens correction").click();
        harness.run();
        assert_eq!(harness.state().binding.lens(), None);
    }

    /// **AC**: the Distortion slider reads `manual_distortion` directly,
    /// on its own signed scale with no shifting.
    ///
    /// The other sliders are parked on distinct values so each reading
    /// below identifies exactly one widget.
    #[test]
    fn distortion_slider_reads_the_manual_field_directly() {
        let mut harness = panel_harness();
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Defringe, ParamValue::F32(22.0));
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::VignetteCorr, ParamValue::F32(11.0));
        harness.state_mut().binding.values.insert(
            ParamId::LensProfile,
            ParamValue::Lens(Optics {
                lens_profile: Some(LensCorrection::default()),
                ..Optics::default()
            }),
        );
        harness.run();
        harness.get_by_label("+0"); // the neutral manual dial
        harness.get_by_label("+22");
        harness.get_by_label("+11");

        for shown in [(60.0f32, "+60"), (-60.0, "-60"), (-100.0, "-100")] {
            let (value, label) = shown;
            harness.state_mut().binding.values.insert(
                ParamId::LensProfile,
                ParamValue::Lens(Optics {
                    lens_profile: Some(LensCorrection {
                        manual_distortion: value,
                        ..LensCorrection::default()
                    }),
                    ..Optics::default()
                }),
            );
            harness.run();
            harness.get_by_label(label);
        }
    }

    /// **AC / the reason the schema grew a field**: the Distortion slider
    /// must never write `LensCorrection::distortion`. That field is the
    /// profile amount and is emitted as `crs:LensProfileDistortionScale`;
    /// a manual correction landing there would make the written sidecar
    /// mean something the user never asked for in other software.
    #[test]
    fn the_distortion_slider_never_touches_the_profile_amount() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::LensProfile,
            ParamValue::Lens(Optics {
                lens_profile: Some(LensCorrection::default()),
                ..Optics::default()
            }),
        );
        harness.run();

        // Drive the real widget through a real gesture, not a synthetic
        // write: the slider's own keyboard nudge is one `Commit` event
        // through the same `apply_lens_events` a drag uses.
        harness
            .get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Distortion")
            .focus();
        harness.run();
        harness.key_press(egui::Key::ArrowUp);
        harness.run();

        let lens = harness.state().binding.lens().expect("leaf still present");
        assert_ne!(
            lens.manual_distortion, 0.0,
            "the nudge must land on the manual dial"
        );
        assert_eq!(
            lens.distortion,
            LensCorrection::default().distortion,
            "the profile distortion amount must be untouched at unity"
        );
        assert_eq!(
            lens.vignetting,
            LensCorrection::default().vignetting,
            "the profile vignetting amount must be untouched at unity"
        );
    }

    /// **AC**: Defringe and Vignetting drive their own scalar params.
    #[test]
    fn defringe_and_vignetting_sliders_read_their_params() {
        let mut harness = panel_harness();
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Defringe, ParamValue::F32(35.0));
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::VignetteCorr, ParamValue::F32(-42.0));
        harness.run();
        harness.get_by_label("+35");
        harness.get_by_label("-42");
    }

    /// **AC**: Reset Optics restores all four params in one gesture.
    #[test]
    fn reset_restores_every_optics_param() {
        let mut harness = panel_harness();
        harness.state_mut().binding.values.insert(
            ParamId::LensProfile,
            ParamValue::Lens(Optics {
                lens_profile: Some(LensCorrection {
                    distortion: 30.0,
                    ..LensCorrection::default()
                }),
                ..Optics::default()
            }),
        );
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Defringe, ParamValue::F32(80.0));
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::VignetteCorr, ParamValue::F32(60.0));
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::ChromaticAberration, ParamValue::Bool(true));
        harness.run();

        harness.get_by_label("Reset Optics").click();
        harness.run();

        let b = &harness.state().binding;
        assert_eq!(b.lens(), None);
        assert_eq!(
            b.values.get(&ParamId::Defringe),
            Some(&ParamValue::F32(0.0))
        );
        assert_eq!(
            b.values.get(&ParamId::VignetteCorr),
            Some(&ParamValue::F32(0.0))
        );
        assert_eq!(
            b.values.get(&ParamId::ChromaticAberration),
            Some(&ParamValue::Bool(false))
        );
    }
}
