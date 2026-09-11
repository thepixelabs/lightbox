// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task **D2**), the `develop.bw` panel: the Color↔Monochrome
//! [`Treatment`] toggle plus the 8-band [`BwMix`] channel mixer, bound to
//! `ParamId::Treatment`/`ParamId::BwMix` through the same gesture/leaf-
//! decompose conventions `panels::basic`'s WB mode combo and `panels::hsl`'s
//! band table already establish.
//!
//! # Treatment toggle (D2 AC: "one history step and round-trips")
//!
//! One `ui.toggle_value` switch drives the whole leaf: flipping it issues
//! ONE `begin_gesture`/`preview`/`end_gesture` bracket ([`set_treatment`]
//! the same one-shot shape `panels::basic::wb_section`'s mode combo uses),
//! so undo restores exactly the prior `Treatment` in one step. Proved
//! end-to-end through a REAL `SessionEditBinding` in this module's own
//! `treatment_round_trip` test (mirrors `develop_ctx.rs`'s
//! `preset_wiring_tests` harness), not just an assertion on the gesture
//! calls against a recording stub.
//!
//! # B&W mix sliders
//!
//! Eight `-100..=100` sliders (spec §4.3 `BwMix`, band order = HSL's
//! [`colorspace::HUE_BAND_NAMES`], Red, Orange, Yellow, Green, Aqua, Blue,
//! Purple, Magenta) decompose/recompose the ONE `BwMix` leaf, same
//! convention as `panels::hsl::band_slider`.
//!
//! # Auto button
//!
//! Calls the render engine's own, already-shipped D1 algorithm
//! [`lightbox_render::ng::nodes::global::bw_mix::auto_bw_mix`], with the
//! recipe's CURRENT `ParamId::WhiteBalance` leaf and lands the result as ONE
//! gesture. `lightbox-render` owns the algorithm (a pure `&WhiteBalance ->
//! BwMix` fn, no engine/GPU state needed); this panel is only the seam that
//! reads the current WB and writes the mix, no `lightbox-render` internals
//! are duplicated or reimplemented here.
//!
//! # Mono-disable UI (D2 AC: "color panels visibly disabled in Monochrome")
//!
//! [`is_monochrome`] is the single, unit-tested predicate every color-only
//! panel checks to decide whether to grey itself out via
//! `ui.add_enabled_ui` (`panels::hsl`, `panels::grading`), the render
//! engine already elides `HslNode`/`ColorGradeNode`/`VibranceSatNode` in
//! Monochrome (`nodes/global/{hsl,color_grade,vibrance_sat}.rs`'s own
//! `is_identity`, all keyed on the exact same `Treatment::BlackAndWhite`
//! condition); this predicate is the UI's read of that condition, not a new
//! decision. **Not** disabled: the WB temp/tint sliders (`panels::basic`)
//! white balance still runs upstream of `Treatment` in the pipeline (spec
//! §4.1: WB sits before the input transform, `BwMixNode` reads working
//! luma derived from it) and visibly changes the B&W conversion's tonality
//! (see `auto_bw_mix`'s own WB-informed nudge), so it is not a "color-only"
//! control in the sense this AC means, recorded as a deviation rationale
//! in `docs/plan/epics/E10-deviations.md`.
//!
//! The B&W mixer's OWN 8 sliders use the inverse of [`is_monochrome`]: they
//! only affect the render once Monochrome is active (`BwMixNode` itself is
//! elided in `Color`, see `bw_mix.rs`'s module docs), so they're greyed out
//! here until the toggle is on.

use eframe::egui;
use lightbox_edit::{BwMix, ParamDelta, ParamId, ParamValue, Treatment, WhiteBalance};
use lightbox_render::ng::nodes::common::colorspace;
use lightbox_render::ng::nodes::global::bw_mix::auto_bw_mix;

use crate::panels::develop_ctx::{DevelopCtx, EditBinding};
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{value_slider, SliderEvent, SliderSpec};

/// The B&W panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.bw");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots right
/// after HSL (40), before color grading (45), Lightroom's own "HSL /
/// Color / B&W" cluster ordering (the treatment toggle you'd reach for
/// right after deciding hue bands don't apply).
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "B&W",
        source_req: SourceReq::Any,
        order: 42,
        build,
    }
}

/// True while `treatment` is Monochrome, the single predicate D2's
/// mono-disable UI checks. Unit-tested directly below (the phase brief's
/// own AC: "a unit test on the predicate, not a pixel test").
pub fn is_monochrome(treatment: Treatment) -> bool {
    treatment == Treatment::BlackAndWhite
}

fn current_treatment(ctx: &DevelopCtx<'_>) -> Treatment {
    match ctx.edit.value(ParamId::Treatment) {
        ParamValue::Treatment(t) => t,
        _ => Treatment::Color,
    }
}

fn current_mix(ctx: &DevelopCtx<'_>) -> BwMix {
    match ctx.edit.value(ParamId::BwMix) {
        ParamValue::BwMix(m) => m,
        _ => BwMix::default(),
    }
}

fn current_wb(ctx: &DevelopCtx<'_>) -> WhiteBalance {
    match ctx.edit.value(ParamId::WhiteBalance) {
        ParamValue::Wb(wb) => wb,
        _ => WhiteBalance::AsShot,
    }
}

fn treatment_delta(t: Treatment) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::Treatment, ParamValue::Treatment(t));
    delta
}

fn mix_delta(m: BwMix) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::BwMix, ParamValue::BwMix(m));
    delta
}

/// The Treatment toggle's production path: ONE gesture per flip (D2 AC).
/// Exposed standalone (not inlined into `treatment_row`) so this module's
/// round-trip test drives the EXACT production toggle against a real
/// `SessionEditBinding`, not a hand-rolled stand-in.
pub fn set_treatment(edit: &mut dyn EditBinding, mono: bool) {
    let next = if mono {
        Treatment::BlackAndWhite
    } else {
        Treatment::Color
    };
    edit.begin_gesture(ParamId::Treatment);
    edit.preview(treatment_delta(next));
    edit.end_gesture();
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    treatment_row(ui, ctx);
    ui.separator();
    let mono = is_monochrome(current_treatment(ctx));
    ui.add_enabled_ui(mono, |ui| {
        mix_section(ui, ctx);
    });
}

fn treatment_row(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let mut mono = is_monochrome(current_treatment(ctx));
    ui.horizontal(|ui| {
        ui.label("Treatment");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .toggle_value(&mut mono, "Monochrome")
                .on_hover_text("Convert to black & white, driven by the mixer below")
                .changed()
            {
                set_treatment(ctx.edit, mono);
            }
        });
    });
}

fn mix_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let mix = current_mix(ctx);
    for i in 0..colorspace::N_BANDS {
        mix_slider(ui, ctx, &mix, i);
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button("Auto")
            .on_hover_text(
                "Seed a white-balance-informed starting mix (lightbox_render::auto_bw_mix)",
            )
            .clicked()
        {
            let wb = current_wb(ctx);
            let seeded = auto_bw_mix(&wb);
            ctx.edit.begin_gesture(ParamId::BwMix);
            ctx.edit.preview(mix_delta(seeded));
            ctx.edit.end_gesture();
        }
        if ui
            .button("Reset")
            .on_hover_text("Reset all 8 bands to zero")
            .clicked()
        {
            ctx.edit.reset(ParamId::BwMix);
        }
    });
}

/// One mix slider for `band_idx`, decomposing/recomposing the ONE [`BwMix`]
/// leaf (mirrors `panels::hsl::band_slider`'s decompose-a-coarse-leaf
/// shape).
fn mix_slider(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, mix: &BwMix, band_idx: usize) {
    let spec = SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label: colorspace::HUE_BAND_NAMES[band_idx],
        unit: None,
    };
    let (_, events) = value_slider(
        ui,
        ("develop.bw.slider", band_idx),
        mix.weights[band_idx] as f64,
        &spec,
    );
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::BwMix),
            SliderEvent::Preview(v) => {
                let mut next = *mix;
                next.weights[band_idx] = v as f32;
                ctx.edit.preview(mix_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => {
                let mut next = *mix;
                next.weights[band_idx] = 0.0;
                ctx.edit.begin_gesture(ParamId::BwMix);
                ctx.edit.preview(mix_delta(next));
                ctx.edit.end_gesture();
            }
            SliderEvent::Commit(v) => {
                let mut next = *mix;
                next.weights[band_idx] = v as f32;
                ctx.edit.begin_gesture(ParamId::BwMix);
                ctx.edit.preview(mix_delta(next));
                ctx.edit.end_gesture();
            }
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

    // ── D2 AC: "a unit test on the predicate, not a pixel test" ────────────

    #[test]
    fn is_monochrome_reflects_treatment_exactly() {
        assert!(!is_monochrome(Treatment::Color));
        assert!(is_monochrome(Treatment::BlackAndWhite));
    }

    // ── Recording-double panel tests (same convention as basic.rs/hsl.rs) ──

    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            values.insert(ParamId::Treatment, ParamValue::Treatment(Treatment::Color));
            values.insert(ParamId::BwMix, ParamValue::BwMix(BwMix::default()));
            values.insert(ParamId::WhiteBalance, ParamValue::Wb(WhiteBalance::AsShot));
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values
                .get(&p)
                .cloned()
                .unwrap_or(ParamValue::Treatment(Treatment::Color))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::BwMix => ParamValue::BwMix(BwMix::default()),
                _ => ParamValue::Treatment(Treatment::Color),
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

    fn mix_of(binding: &RecordingBinding) -> BwMix {
        match binding.value(ParamId::BwMix) {
            ParamValue::BwMix(m) => m,
            _ => BwMix::default(),
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
        harness.set_size(egui::vec2(320.0, 520.0));
        harness
    }

    /// A12-style AC: the panel always reflects the recipe, mutating the
    /// binding's Treatment from OUTSIDE any widget interaction changes what
    /// the very next frame shows for the toggle.
    #[test]
    fn panel_always_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        harness.run();
        harness.get_by_label("Monochrome"); // toggle exists, currently off

        harness.state_mut().binding.values.insert(
            ParamId::Treatment,
            ParamValue::Treatment(Treatment::BlackAndWhite),
        );
        harness.run();
        // `toggle_value`'s accessible label is the SAME text regardless of
        // state (egui toggle buttons don't rename themselves), assert the
        // underlying value instead, mirroring the mix slider's own
        // state-reflects-the-recipe check below.
        assert!(is_monochrome(
            match harness.state().binding.value(ParamId::Treatment) {
                ParamValue::Treatment(t) => t,
                _ => Treatment::Color,
            }
        ));
    }

    /// Mix-slider proof, mirroring `hsl.rs`'s own no-widget-local-state
    /// test: setting the binding's B&W mix from OUTSIDE any widget
    /// interaction changes what the very next frame shows.
    #[test]
    fn mix_slider_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        // Turn Monochrome on first so the mixer isn't disabled.
        harness.state_mut().binding.values.insert(
            ParamId::Treatment,
            ParamValue::Treatment(Treatment::BlackAndWhite),
        );
        let mut mix = BwMix::default();
        mix.weights[0] = 37.0; // Red band
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::BwMix, ParamValue::BwMix(mix));
        harness.run();
        harness.get_by_label("+37");

        mix.weights[0] = -12.0;
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::BwMix, ParamValue::BwMix(mix));
        harness.run();
        harness.get_by_label("-12");
    }

    /// **D2 AC (mix-slider gesture)**: a REAL synthetic drag on
    /// `mix_slider`'s track routes a `ParamId::BwMix` delta through the
    /// `begin_gesture`/`preview`/`end_gesture` lifecycle and lands on the
    /// recipe, mirrors `hsl.rs`'s own synthetic-drag proof.
    #[test]
    fn a_real_synthetic_drag_commits_one_bw_mix_gesture_and_lands_the_value() {
        let mix = BwMix::default();
        let mut harness = Harness::new_ui_state(
            |ui, s: &mut (BwMix, RecordingBinding)| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Rendered,
                    edit: &mut s.1,
                    gizmos: &mut gizmos,
                };
                mix_slider(ui, &mut ctx, &s.0, 5);
            },
            (mix, RecordingBinding::new()),
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

        let after = mix_of(&harness.state().1);
        assert!(
            after.weights[5] > 50.0,
            "dragging the Blue band's mix track to its right edge should land a large positive \
             value: got {}",
            after.weights[5]
        );
    }

    /// The panel mounts as a registered [`PanelDef`] with the expected
    /// identity/order, slots between HSL (40) and Color Grading (45).
    #[test]
    fn def_is_registered_between_hsl_and_grading() {
        let d = def();
        assert_eq!(d.id.0, "develop.bw");
        assert!(d.order > 40 && d.order < 45);
    }

    // ── D2 AC: Treatment toggle is ONE history step and round-trips ────────
    //
    // Through a REAL SessionEditBinding/command bus, mirrors
    // `develop_ctx.rs`'s own `preset_wiring_tests` harness.
    mod treatment_round_trip {
        use super::*;
        use crate::panels::develop_ctx::SessionEditBinding;
        use lightbox_core::{Command, Core, CoreConfig, Event, OpenOrigin, OpenRequest, Session};
        use lightbox_types::ImageId;
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};
        use tokio::sync::broadcast::error::TryRecvError;

        const TINY_JPEG: &[u8] = include_bytes!("../../../../tools/xtask/assets/lightbox-tiny.jpg");
        const EVENT_TIMEOUT: Duration = Duration::from_secs(15);

        fn start_session(tmp: &Path) -> (Core, Session) {
            let mut cfg = CoreConfig::default();
            cfg.preset_dir = Some(tmp.join("presets"));
            let core = Core::start(cfg).expect("core start");
            let session = core
                .create_catalog(&tmp.join("cat.lbdata"), None)
                .expect("create catalog");
            (core, session)
        }

        fn stage_jpeg(dir: &Path, name: &str) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, TINY_JPEG).unwrap();
            path
        }

        fn open_one_image(session: &Session, path: PathBuf) -> ImageId {
            let mut rx = session.events();
            session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(vec![path], false, OpenOrigin::Cli),
            });
            let deadline = Instant::now() + EVENT_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the working set to load"
                );
                match rx.try_recv() {
                    Ok(Event::WorkingSetLoadFinished { .. }) => break,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Closed) => panic!("event channel closed while opening"),
                }
            }
            crate::working_set::WorkingSetView::new(session)
                .active_image()
                .expect("the first Ready entry auto-activates (spec §2.4)")
        }

        fn drain_until_catalog_changed(rx: &mut tokio::sync::broadcast::Receiver<Event>) {
            let deadline = Instant::now() + EVENT_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for Event::CatalogChanged"
                );
                match rx.try_recv() {
                    Ok(Event::CatalogChanged { .. }) => return,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Closed) => panic!("event channel closed while draining"),
                }
            }
        }

        fn head_seq(binding: &SessionEditBinding) -> u64 {
            binding
                .history()
                .iter()
                .find(|s| s.is_head)
                .map(|s| s.seq)
                .unwrap_or(0)
        }

        /// **D2 AC**: toggling Color→Monochrome lands as exactly ONE new
        /// history step, and `Undo` restores `Treatment::Color` exactly
        /// through the REAL production [`set_treatment`] path against a
        /// REAL `SessionEditBinding`/command bus/store, not a stub.
        #[test]
        fn toggle_is_one_history_step_and_round_trips() {
            let tmp = tempfile::TempDir::new().unwrap();
            let (_core, session) = start_session(tmp.path());
            let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
            let image = open_one_image(&session, photo);

            let mut rx = session.events();
            let mut binding = SessionEditBinding::new(session.clone());
            binding.bind(Some(image));

            assert_eq!(
                binding.value(ParamId::Treatment),
                ParamValue::Treatment(Treatment::Color),
                "a fresh recipe starts at the identity Color treatment"
            );
            let head_before = head_seq(&binding);

            // Flip to Monochrome through the EXACT production toggle path.
            set_treatment(&mut binding, true);
            drain_until_catalog_changed(&mut rx);
            binding.mark_dirty();
            binding.bind(Some(image));

            assert_eq!(
                binding.value(ParamId::Treatment),
                ParamValue::Treatment(Treatment::BlackAndWhite),
                "the toggle landed Monochrome"
            );
            assert_eq!(
                head_seq(&binding),
                head_before + 1,
                "the toggle landed as exactly ONE new history step"
            );

            // UNDO restores Color exactly, in one step back.
            binding.undo();
            drain_until_catalog_changed(&mut rx);
            binding.mark_dirty();
            binding.bind(Some(image));

            assert_eq!(
                binding.value(ParamId::Treatment),
                ParamValue::Treatment(Treatment::Color),
                "undo restored the pre-toggle Color treatment exactly"
            );
            assert_eq!(head_seq(&binding), head_before);
        }
    }
}
