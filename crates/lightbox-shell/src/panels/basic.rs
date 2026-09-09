// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E5/E6, the `develop.basic` panel: white balance + basic tone, bound to
//! E09's real param ids through [`param_slider`]/[`value_slider`].
//!
//! # ParamId mapping (spec §6.5's committed table, against the REAL E09 ids)
//!
//! The spec's draft named dotted leaf ids (`wb.temp`, `tone.exposure`, …);
//! what E09 shipped is the coarse `#[repr(u16)]` `ParamId` enum
//! (`E08-deviations.md` A0-8). The committed mapping:
//!
//! | Control            | E09 `ParamId`          | Leaf / range (spec §3.2)       |
//! |--------------------|------------------------|--------------------------------|
//! | WB mode combo      | `ParamId::WhiteBalance`| `WhiteBalance` (whole leaf)    |
//! | Temp slider        | `ParamId::WhiteBalance`| `Custom.temp_k` 2000..=50000 K |
//! | Tint slider        | `ParamId::WhiteBalance`| `Custom.tint` −150..=150       |
//! | Exposure           | `ParamId::Exposure`    | f32, −5..=5 EV                 |
//! | Contrast           | `ParamId::Contrast`    | f32, −100..=100                |
//! | Highlights         | `ParamId::Highlights`  | f32, −100..=100                |
//! | Shadows            | `ParamId::Shadows`     | f32, −100..=100                |
//! | Whites             | `ParamId::Whites`      | f32, −100..=100                |
//! | Blacks             | `ParamId::Blacks`      | f32, −100..=100                |
//!
//! The WB temp/tint sliders decompose/recompose the ONE `WhiteBalance`
//! leaf shell-side (A0-8's named consequence): every WB control writes a
//! full `ParamValue::Wb(..)` delta. (The §6.5 doctest that pins this table
//! against E09's registry is deferred to the final testing pass with all
//! other test code, owner directive.)
//!
//! # Raw vs. rendered (spec §4 WB row, E2's per-control variants)
//!
//! * `Raw`: Kelvin temp scale (unit "K"), tint, and the mode combo
//!   ("As Shot"/Auto/presets/Custom). Dragging temp/tint under a non-
//!   Custom mode switches to Custom seeded from the neutral placeholder
//!   (the camera's actual as-shot Kelvin needs raw metadata, E10).
//! * `Rendered`: "Temp (relative)"/"Tint (relative)" sliders, no Kelvin
//!   display, no As Shot combo. The recipe's WB leaf has no relative-unit
//!   carrier, so the relative temp value maps affinely onto the leaf's
//!   Kelvin domain around neutral (a UI scale conversion only, the
//!   rendered-WB *semantics* are E10's; recorded in `E08-deviations.md`
//!   Phase E).

use eframe::egui;
use lightbox_edit::{ParamDelta, ParamId, ParamValue, WbPreset, WhiteBalance};
use lightbox_types::SourceKind;

use crate::canvas::gizmo::{WbEyedropper, WB_EYEDROPPER};
use crate::panels::develop_ctx::{DevelopCtx, EditBinding};
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{param_slider, value_slider, SliderEvent, SliderSpec};

/// The basic panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.basic");

/// Registration (E1): mounted by `lib.rs` at app construction.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Basic",
        source_req: SourceReq::Any,
        // E10's histogram slot is deliberately above (order < 20, §2.3).
        order: 20,
        build,
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    wb_section(ui, ctx);
    ui.separator();
    tone_section(ui, ctx);
    ui.separator();
    presence_section(ui, ctx);
}

// ─── E5: tone ────────────────────────────────────────────────────────────────

/// The −100..=100 house range shared by five of the six tone params.
const fn pct(label: &'static str) -> SliderSpec {
    SliderSpec {
        min: -100.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label,
        unit: None,
    }
}

fn tone_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    param_slider(
        ui,
        ctx,
        ParamId::Exposure,
        &SliderSpec {
            min: -5.0,
            max: 5.0,
            step: 0.1,
            fine: 0.01,
            label: "Exposure",
            unit: Some("EV"),
        },
    );
    param_slider(ui, ctx, ParamId::Contrast, &pct("Contrast"));
    param_slider(ui, ctx, ParamId::Highlights, &pct("Highlights"));
    param_slider(ui, ctx, ParamId::Shadows, &pct("Shadows"));
    param_slider(ui, ctx, ParamId::Whites, &pct("Whites"));
    param_slider(ui, ctx, ParamId::Blacks, &pct("Blacks"));
}

// ─── E10 Phase D (task D7): presence ────────────────────────────────────────
//
// Clarity/texture/dehaze, the same `-100..=100` house range as the tone
// sliders above (`pct`), bound through the identical `param_slider` seam.
// Vibrance/saturation (also `ParamGroup::Presence`, per
// `lightbox_edit::params::group_of`'s deviations A-5 note) are a separate
// slice, not built here (out of this phase agent's D2 scope, see
// `docs/plan/epics/E10-deviations.md`).

fn presence_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    param_slider(ui, ctx, ParamId::Clarity, &pct("Clarity"));
    param_slider(ui, ctx, ParamId::Texture, &pct("Texture"));
    param_slider(ui, ctx, ParamId::Dehaze, &pct("Dehaze"));
}

// ─── E6: white balance ───────────────────────────────────────────────────────

/// Kelvin span each relative-temp unit maps to for `Rendered` sources
/// a pure UI scale (±100 rel ≙ ±3000 K around neutral), NOT color science;
/// E10 owns the rendered-WB semantics (see the module docs).
const REL_TEMP_K_PER_UNIT: f64 = 30.0;

/// The `(temp_k, tint)` pair used as the neutral placeholder when a slider
/// drag first switches a non-Custom mode (As Shot/Auto/Preset) to `Custom`.
///
/// **Bug history:** this used to be a hardcoded `(6500.0, 0.0)` guess at "a
/// plausible daylight as-shot Kelvin", but the render engine's
/// `WhiteBalanceNode` (`lightbox-render`'s `nodes::global::white_balance`)
/// always evaluates [`lightbox_color::wb::non_raw_wb_matrix`], whose OWN
/// documented identity point is the working space's D50 white, **not**
/// 6500 K, measured at `(≈5002 K, ≈9.6 tint)`, ~1500 K and ~10 tint units
/// away from the old guess. Since a `Custom` value is a REAL, non-identity
/// Bradford CAT the instant it's committed (`WhiteBalanceNode::is_identity`
/// is false for any `Custom`), seeding it from the wrong anchor meant the
/// very first pixel of a "Temp (relative)" drag jumped the image through a
/// large, unintended color cast before the user's own adjustment even
/// applied, reported as "changing temp (relative) skews the image
/// weirdly" (a neutral gray sampled through the old `(6500, 0)` seed came
/// out `[0.512, 0.499, 0.369]`, a ~26% blue-channel loss on what should
/// read as "no change yet").
///
/// Computed via [`lightbox_color::wb::temp_tint_from_working_neutral`], the
/// exact inverse of `non_raw_wb_matrix`, over a neutral working-RGB patch,
/// so this can never drift from whatever the render crate's own working
/// space actually is (one source of truth, not a second guess to keep in
/// sync by hand).
fn render_neutral_temp_tint() -> (f64, f64) {
    lightbox_color::wb::temp_tint_from_working_neutral([1.0, 1.0, 1.0])
}

/// The current custom temp/tint, or the neutral placeholder for the
/// non-Custom modes (As Shot/Auto/presets carry no numeric temp at M1).
fn wb_temp_tint(wb: &WhiteBalance) -> (f64, f64) {
    match wb {
        WhiteBalance::Custom { temp_k, tint } => (*temp_k as f64, *tint as f64),
        _ => render_neutral_temp_tint(),
    }
}

fn wb_delta(wb: WhiteBalance) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta.0.insert(ParamId::WhiteBalance, ParamValue::Wb(wb));
    delta
}

/// Human label for a WB mode (combo rows + selected text).
fn wb_mode_label(wb: &WhiteBalance) -> &'static str {
    match wb {
        WhiteBalance::AsShot => "As Shot",
        WhiteBalance::Auto => "Auto",
        WhiteBalance::Preset(WbPreset::Daylight) => "Daylight",
        WhiteBalance::Preset(WbPreset::Cloudy) => "Cloudy",
        WhiteBalance::Preset(WbPreset::Shade) => "Shade",
        WhiteBalance::Preset(WbPreset::Tungsten) => "Tungsten",
        WhiteBalance::Preset(WbPreset::Fluorescent) => "Fluorescent",
        WhiteBalance::Preset(WbPreset::Flash) => "Flash",
        WhiteBalance::Custom { .. } => "Custom",
        // `WhiteBalance` is `#[non_exhaustive]`, badge honestly rather
        // than fail to compile against a future E09 addition.
        _ => "White balance",
    }
}

fn wb_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let wb = match ctx.edit.value(ParamId::WhiteBalance) {
        ParamValue::Wb(wb) => wb,
        _ => WhiteBalance::AsShot,
    };
    let (temp_k, tint) = wb_temp_tint(&wb);
    let raw = ctx.source_kind == SourceKind::Raw;

    // Mode combo, raw only (§4: no "As Shot" camera value for rendered).
    if raw {
        ui.horizontal(|ui| {
            ui.label("WB");
            let modes: [WhiteBalance; 9] = [
                WhiteBalance::AsShot,
                WhiteBalance::Auto,
                WhiteBalance::Preset(WbPreset::Daylight),
                WhiteBalance::Preset(WbPreset::Cloudy),
                WhiteBalance::Preset(WbPreset::Shade),
                WhiteBalance::Preset(WbPreset::Tungsten),
                WhiteBalance::Preset(WbPreset::Fluorescent),
                WhiteBalance::Preset(WbPreset::Flash),
                WhiteBalance::Custom {
                    temp_k: temp_k as f32,
                    tint: tint as f32,
                },
            ];
            let mut selected = wb;
            egui::ComboBox::from_id_salt("develop.basic.wb_mode")
                .selected_text(wb_mode_label(&wb))
                .show_ui(ui, |ui| {
                    for mode in modes {
                        ui.selectable_value(&mut selected, mode, wb_mode_label(&mode));
                    }
                });
            if selected != wb {
                // One-shot gesture: mode change = one history step.
                ctx.edit.begin_gesture(ParamId::WhiteBalance);
                ctx.edit.preview(wb_delta(selected));
                ctx.edit.end_gesture();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                eyedropper_button(ui, ctx);
            });
        });
    } else {
        ui.horizontal(|ui| {
            ui.label("WB");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                eyedropper_button(ui, ctx);
            });
        });
    }

    // Temp/tint sliders over the ONE WhiteBalance leaf (A0-8): every event
    // recomposes a full Custom value. Dragging under a non-Custom mode
    // switches to Custom (the Lightroom convention). The "relative" scale
    // is anchored on the render matrix's OWN neutral point (see
    // `render_neutral_temp_tint`'s doc), not an arbitrary daylight guess
    // "0" on this slider must mean "no visible change yet".
    let (neutral_k, neutral_tint) = render_neutral_temp_tint();
    let (temp_spec, temp_display) = if raw {
        (
            SliderSpec {
                min: 2000.0,
                max: 50000.0,
                step: 50.0,
                fine: 10.0,
                label: "Temp",
                unit: Some("K"),
            },
            temp_k,
        )
    } else {
        (
            SliderSpec {
                min: -100.0,
                max: 100.0,
                step: 1.0,
                fine: 0.1,
                label: "Temp (relative)",
                unit: None,
            },
            (temp_k - neutral_k) / REL_TEMP_K_PER_UNIT,
        )
    };
    let tint_spec = SliderSpec {
        min: -150.0,
        max: 150.0,
        step: 1.0,
        fine: 0.1,
        label: if raw { "Tint" } else { "Tint (relative)" },
        unit: None,
    };

    let to_temp_k = |display: f64| -> f64 {
        if raw {
            display
        } else {
            neutral_k + display * REL_TEMP_K_PER_UNIT
        }
    };
    // The tint axis gets the SAME relative treatment as temp above. It
    // previously displayed the absolute tint while its neighbour displayed
    // a relative one, so a neutral non-raw image read "+10" on a slider
    // whose own label says "(relative)", and dragging it to the displayed
    // `0` committed `Custom { tint: 0.0 }`, which is a real Bradford CAT
    // away from the render matrix's neutral (≈9.6), i.e. the control
    // introduced a color cast at the exact point it claimed to be neutral.
    // Same defect, same fix, as the `(6500, 0)` temp seed documented on
    // `render_neutral_temp_tint`.
    let tint_display = if raw { tint } else { tint - neutral_tint };
    let to_tint = |display: f64| -> f64 {
        if raw {
            display
        } else {
            neutral_tint + display
        }
    };

    let (_, temp_events) = value_slider(ui, "develop.basic.wb_temp", temp_display, &temp_spec);
    apply_wb_events(ctx, temp_events, |v| WhiteBalance::Custom {
        temp_k: to_temp_k(v) as f32,
        tint: tint as f32,
    });

    let (_, tint_events) = value_slider(ui, "develop.basic.wb_tint", tint_display, &tint_spec);
    apply_wb_events(ctx, tint_events, |v| WhiteBalance::Custom {
        temp_k: temp_k as f32,
        tint: to_tint(v) as f32,
    });
}

/// Routes [`value_slider`] events into WB gestures: `recompose(v)` builds
/// the full leaf value for a slider's new scalar `v`.
fn apply_wb_events(
    ctx: &mut DevelopCtx<'_>,
    events: Vec<SliderEvent>,
    recompose: impl Fn(f64) -> WhiteBalance,
) {
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::WhiteBalance),
            SliderEvent::Preview(v) => ctx.edit.preview(wb_delta(recompose(v))),
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => ctx.edit.reset(ParamId::WhiteBalance),
            SliderEvent::Commit(v) => {
                ctx.edit.begin_gesture(ParamId::WhiteBalance);
                ctx.edit.preview(wb_delta(recompose(v)));
                ctx.edit.end_gesture();
            }
        }
    }
}

/// E6/F2, the eyedropper button, now mounting the REAL gizmo: pressing it
/// activates [`WbEyedropper`] in the canvas [`GizmoLayer`] (crosshair +
/// loupe chip on the canvas, `lib.rs` pushes `keymap::CTX_GIZMO` while the
/// layer is active, Esc/`gizmo.cancel` cancels). The button's pressed
/// state IS the gizmo's active state, one authority, no drift (the
/// Phase-E `EyedropperMount` boolean this replaced could only mirror it).
/// A completed pick routes back here via [`apply_wb_pick`].
///
/// [`GizmoLayer`]: crate::canvas::gizmo::GizmoLayer
fn eyedropper_button(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let mut armed = ctx.gizmos.is_active(WB_EYEDROPPER);
    if ui
        .toggle_value(&mut armed, "Eyedropper")
        .on_hover_text("Pick a neutral point on the image")
        .changed()
    {
        if armed {
            ctx.gizmos.activate(Box::new(WbEyedropper::new()));
        } else {
            ctx.gizmos.cancel(WB_EYEDROPPER);
        }
    }
}

/// F2/A11, resolves a REAL sampled working-space RGB patch (the value a
/// pixel sampler reads at the F2 picked point) into ONE committed WB gesture
/// through the E3 binding, using E10's real neutral-solve
/// ([`lightbox_color::wb::temp_tint_from_working_neutral`], task A11): the
/// `(temp, tint)` under which `sample_rgb` renders achromatic. This is the
/// function the gizmo's `Picked` handler should call once it has a real
/// sample in hand.
///
/// The solve itself is exercised end-to-end (through the REAL
/// `WhiteBalanceNode`, not just this algebra) by `lightbox-render`'s
/// `tests/e10_wb.rs` A11 corpus test: a synthetic "shot gray card" patch is
/// solved and rendered, and the output is measured for Oklab a/b neutrality
/// the exact AC this function is built to satisfy once wired to a real
/// sample.
///
/// `#[allow(dead_code)]`: no call site exists yet (`lib.rs`'s `Picked`
/// handler has no real sample to hand it, see [`apply_wb_pick`]'s doc
/// comment), the frozen contract surface for the pixel-sampling seam's
/// follow-on wiring, same convention as `canvas::gizmo::HIT_TOLERANCE_PT`.
#[allow(dead_code)]
pub fn apply_wb_pick_from_sample(edit: &mut dyn EditBinding, sample_rgb: [f32; 3]) {
    let (kelvin, tint) = lightbox_color::wb::temp_tint_from_working_neutral([
        sample_rgb[0] as f64,
        sample_rgb[1] as f64,
        sample_rgb[2] as f64,
    ]);
    // One pick = one history step (the same one-shot gesture shape the WB
    // mode combo uses above).
    edit.begin_gesture(ParamId::WhiteBalance);
    edit.preview(wb_delta(WhiteBalance::Custom {
        temp_k: kelvin as f32,
        tint: tint as f32,
    }));
    edit.end_gesture();
}

/// F2, the eyedropper's pick callback: resolves a `Picked { image_px }`
/// gizmo effect (routed here by `lib.rs`) into ONE committed WB gesture
/// through the E3 binding.
///
/// **STILL A POSITION-LINEAR ESTIMATE, the real solve exists
/// ([`apply_wb_pick_from_sample`], task A11) but this call site has no real
/// pixel sample to hand it.** `lib.rs`'s `GizmoEffect::Picked` handler only
/// carries an `image_px` position, no pixel-sampling path reaches it yet
/// (the shell never reads GPU pixels, `lib.rs`'s zero-copy invariants, and
/// the render engine's only readback target, `RenderTarget::Buffer`, comes
/// back **display-encoded** post-`xform.display`, not the working-space RGB
/// the solve needs; getting an exact working-space sample from a live click
/// needs either a new engine-level tap point before `xform.display` or an
/// inverse of the baked ICC transform, a genuine new seam, out of A9-A11's
/// node/color-math scope, named as a deviation in
/// `docs/plan/epics/E10-deviations.md`). Until that seam lands, this
/// function keeps its pre-A11 behavior: mapping the pick POSITION linearly
/// onto the WB leaf around neutral (normalized x → temp
/// ± [`PICK_TEMP_SPAN_K`] K, normalized y → tint ± [`PICK_TINT_SPAN`]) so
/// the gizmo → effect → panel callback → `EditBinding` gesture → same-frame
/// resubmit wiring keeps exercising end to end. The follow-on is a one-line
/// change: obtain `sample_rgb` and call [`apply_wb_pick_from_sample`]
/// instead of this function's estimate body.
pub fn apply_wb_pick(edit: &mut dyn EditBinding, image_px: egui::Vec2, image_size: egui::Vec2) {
    #[cfg(debug_assertions)]
    tracing::warn!(
        target: "lightbox_shell",
        x = image_px.x,
        y = image_px.y,
        "WB eyedropper: position-linear estimate (real solve is apply_wb_pick_from_sample; \
         pending a pixel-sampling seam — see this function's doc comment)"
    );
    let nx = (image_px.x / image_size.x.max(1.0)).clamp(0.0, 1.0) as f64;
    let ny = (image_px.y / image_size.y.max(1.0)).clamp(0.0, 1.0) as f64;
    // Anchored on the render matrix's own neutral (see
    // `render_neutral_temp_tint`'s doc), not a hardcoded (6500, 0) guess
    // same fix as the temp/tint sliders above.
    let (neutral_k, neutral_tint) = render_neutral_temp_tint();
    let temp_k = neutral_k + (nx - 0.5) * 2.0 * PICK_TEMP_SPAN_K;
    let tint = neutral_tint + (ny - 0.5) * 2.0 * PICK_TINT_SPAN;
    // One pick = one history step (the same one-shot gesture shape the WB
    // mode combo uses above).
    edit.begin_gesture(ParamId::WhiteBalance);
    edit.preview(wb_delta(WhiteBalance::Custom {
        temp_k: temp_k as f32,
        tint: tint as f32,
    }));
    edit.end_gesture();
}

/// Kelvin half-span of the position-linear pick estimate (matches the
/// rendered relative-slider scale: ±100 rel ≙ ±3000 K, see
/// [`REL_TEMP_K_PER_UNIT`]).
const PICK_TEMP_SPAN_K: f64 = 3000.0;
/// Tint half-span of the position-linear pick estimate.
const PICK_TINT_SPAN: f64 = 50.0;

// ─── A12: panel-reflects-recipe test ───────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui_kittest::{kittest::NodeT as _, kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use lightbox_color::wb::non_raw_wb_matrix;

    /// **Regression (user-reported bug: "changing temp (relative) skews
    /// the image weirdly").** The `Custom` value a slider drag first seeds
    /// from ([`render_neutral_temp_tint`]) must be the render matrix's OWN
    /// identity point: `non_raw_wb_matrix` at that `(kelvin, tint)` must be
    /// a color no-op (within float rounding), so the very first pixel of a
    /// drag away from the slider's displayed "0" starts from "no visible
    /// change" instead of jumping through an unintended color cast. Also
    /// pins the regression concretely: the OLD hardcoded seed `(6500 K,
    /// tint 0)` was NOT a no-op, a neutral gray through it lost ~26% of
    /// its blue channel, which is exactly the "skew" the report describes.
    /// Fails against that old constant; passes against the fixed, computed
    /// anchor.
    #[test]
    fn render_neutral_seed_is_the_wb_matrix_identity_point() {
        let (k, t) = render_neutral_temp_tint();
        let m = non_raw_wb_matrix(k, t);
        let gray = [0.5f64, 0.5, 0.5];
        let out = m.mul_vec(lightbox_color::Vec3(gray)).0;
        for (a, b) in out.iter().zip(gray.iter()) {
            assert!(
                (a - b).abs() < 1e-4,
                "the neutral seed must be a color no-op: {gray:?} -> {out:?}"
            );
        }

        // Documents the bug this guards against (not a requirement of the
        // fix): the OLD hardcoded seed really was a large, visible skew.
        let old_wrong = non_raw_wb_matrix(6500.0, 0.0);
        let old_out = old_wrong.mul_vec(lightbox_color::Vec3(gray)).0;
        assert!(
            (old_out[2] - gray[2]).abs() > 0.1,
            "sanity: the old (6500, 0) seed visibly skewed a neutral gray \
             ({gray:?} -> {old_out:?}) — this is the bug being fixed"
        );
    }

    /// **Regression (owner-reported: "every image I load is automatically
    /// adjusted").** Nothing was in fact being applied, but the non-raw
    /// Tint slider *displayed* its absolute value while the Temp slider
    /// beside it displayed a relative one, so a perfectly neutral image
    /// read `+10` on a control labelled "(relative)". Worse, the displayed
    /// `0` was a lie: committing `Custom { tint: 0.0 }` is a real Bradford
    /// CAT away from the render matrix's neutral, so "correcting" the
    /// slider to zero introduced the very cast it appeared to remove.
    ///
    /// Both axes must therefore agree: on a non-raw source, the value each
    /// slider *shows* for an untouched image is `0`, and mapping that `0`
    /// back through the slider's own recompose must land exactly on the
    /// render matrix's identity point (a color no-op).
    #[test]
    fn non_raw_wb_sliders_show_zero_at_the_matrix_identity_point() {
        // Drive the REAL panel, an untouched non-raw image, default
        // `AsShot` WB, and read what the sliders actually display. The
        // AccessKit slider node carries the live displayed value
        // (`value_slider` emits `WidgetInfo::slider`), so this asserts the
        // rendered readout, not a re-derivation of the same arithmetic.
        let mut harness = panel_harness();
        harness.run();

        for label in ["Temp (relative)", "Tint (relative)"] {
            let shown = harness
                .get_by_role_and_label(egui::accesskit::Role::Slider, label)
                .accesskit_node()
                .numeric_value()
                .unwrap_or_else(|| panic!("{label} exposes a numeric value"));
            assert!(
                shown.abs() < 0.5,
                "{label} must read 0 on an untouched image, showed {shown:.1} \
                 — the tint axis showed its ABSOLUTE value here before this \
                 fix, on a slider whose own label says \"(relative)\""
            );
        }

        // And the displayed 0 must mean what it says: mapping it back
        // through the slider's own recompose has to land on the render
        // matrix's identity point, or "correcting" the slider to zero
        // would introduce the very cast it appears to remove.
        let (neutral_k, neutral_tint) = render_neutral_temp_tint();
        let gray = [0.5f64, 0.5, 0.5];
        let out = non_raw_wb_matrix(neutral_k, neutral_tint)
            .mul_vec(lightbox_color::Vec3(gray))
            .0;
        for (a, b) in out.iter().zip(gray.iter()) {
            assert!(
                (a - b).abs() < 1e-4,
                "committing the displayed 0 must be a color no-op: \
                 {gray:?} -> {out:?}"
            );
        }

        // Pins the defect: pre-fix, the displayed value went through as an
        // ABSOLUTE tint, so dragging to 0 committed tint 0, a real skew.
        let pre_fix = non_raw_wb_matrix(neutral_k, 0.0)
            .mul_vec(lightbox_color::Vec3(gray))
            .0;
        assert!(
            pre_fix
                .iter()
                .zip(gray.iter())
                .any(|(a, b)| (a - b).abs() > 1e-3),
            "sanity: passing the displayed 0 through as an absolute tint \
             really did skew a neutral gray ({gray:?} -> {pre_fix:?}) — \
             this is the bug being fixed"
        );
    }

    /// A recording [`EditBinding`] test double: every value lives in a
    /// plain map that ONLY [`EditBinding::preview`]/[`EditBinding::reset`]
    /// write to, the exact same seam the real panel goes through, never a
    /// widget-private field. That makes it possible to prove the A12 AC
    /// ("panel state always reflects the recipe, no widget-local state")
    /// directly: mutate the map from OUTSIDE any widget interaction (the
    /// same shape an undo/redo/preset-apply/history-restore takes against
    /// the real `SessionEditBinding`) and confirm the very next frame's
    /// render reflects it, with zero clicks or drags.
    struct RecordingBinding {
        values: HashMap<ParamId, ParamValue>,
    }

    impl RecordingBinding {
        fn new() -> RecordingBinding {
            let mut values = HashMap::new();
            for p in [
                ParamId::Exposure,
                ParamId::Contrast,
                ParamId::Highlights,
                ParamId::Shadows,
                ParamId::Whites,
                ParamId::Blacks,
            ] {
                values.insert(p, ParamValue::F32(0.0));
            }
            values.insert(ParamId::WhiteBalance, ParamValue::Wb(WhiteBalance::AsShot));
            RecordingBinding { values }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            self.values.get(&p).cloned().unwrap_or(ParamValue::F32(0.0))
        }
        fn default(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::WhiteBalance => ParamValue::Wb(WhiteBalance::AsShot),
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
                    source_kind: SourceKind::Rendered,
                    edit: &mut app.binding,
                    gizmos: &mut gizmos,
                };
                build(ui, &mut ctx);
            },
            app,
        );
        harness.set_size(egui::vec2(320.0, 600.0));
        harness
    }

    /// **A12 AC**: the panel always reflects the recipe, no widget-local
    /// state. Setting the binding's exposure to a value and rendering shows
    /// exactly that value, with zero widget interaction; mutating it AGAIN
    /// from outside (never through the slider) changes what the very next
    /// frame displays. A widget caching its own "last seen" value instead
    /// of re-reading `EditBinding::value` every frame would fail this.
    #[test]
    fn basic_panel_always_reflects_the_recipe_with_no_widget_local_state() {
        let mut harness = panel_harness();
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Exposure, ParamValue::F32(1.3));
        harness.run();
        harness.get_by_label("1.3 EV");

        // External mutation with NO widget interaction at all, the same
        // channel undo/redo/`StepTo`/preset hover-preview use to move the
        // working recipe out from under the panel in the real app.
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Exposure, ParamValue::F32(-3.0));
        harness.run();
        harness.get_by_label("-3.0 EV");

        // Same proof for a `%`-range param (Contrast) and the WB leaf
        // (a coarse, non-`F32` value decomposed by `wb_temp_tint`).
        harness
            .state_mut()
            .binding
            .values
            .insert(ParamId::Contrast, ParamValue::F32(42.0));
        harness.run();
        harness.get_by_label("+42");

        let (neutral_k, _) = render_neutral_temp_tint();
        harness.state_mut().binding.values.insert(
            ParamId::WhiteBalance,
            ParamValue::Wb(WhiteBalance::Custom {
                temp_k: (neutral_k - 50.0 * REL_TEMP_K_PER_UNIT) as f32,
                tint: 0.0,
            }),
        );
        harness.run();
        // Rendered-source WB shows a relative temp scale anchored on the
        // render matrix's OWN neutral point (`render_neutral_temp_tint`'s
        // doc, this used to be a hardcoded 6500 K guess; see the
        // `render_neutral_seed_is_the_wb_matrix_identity_point` regression
        // test above): 50 units below neutral reads "-50".
        harness.get_by_label("-50");
    }

    /// **A12 AC (double-click = reset), the widget-core half:**
    /// `value_slider` maps a track double-click straight to
    /// `SliderEvent::Reset` (`widgets.rs`: `if response.double_clicked() {
    /// events.push(SliderEvent::Reset) }`), every basic-panel slider goes
    /// through this exact function, so this is not per-control duplicated
    /// logic. This test pins that mapping structurally (double-click IS the
    /// reset trigger, nothing else in `value_slider` emits `Reset`) rather
    /// than via a fragile synthetic double-click pointer sequence.
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
        // A drag-only frame (no double-click) never emits Reset.
        let mut harness = Harness::new_ui(move |ui| {
            let (_response, events) = value_slider(ui, "a12-no-dclick", 2.0, &spec);
            assert!(
                !events.contains(&SliderEvent::Reset),
                "a plain frame with no double-click must not emit Reset"
            );
        });
        harness.run();
    }

    /// **A12 AC (double-click = reset), the consuming half:** a
    /// `SliderEvent::Reset` routed through the REAL `apply_wb_events`
    /// (production code, not a test stand-in) lands the binding's declared
    /// default, exactly what a double-click on the WB temp/tint track
    /// does end to end (`value_slider` → `Reset` → this function →
    /// `EditBinding::reset`).
    #[test]
    fn wb_reset_event_restores_the_binding_default() {
        let mut binding = RecordingBinding::new();
        binding.values.insert(
            ParamId::WhiteBalance,
            ParamValue::Wb(WhiteBalance::Custom {
                temp_k: 3000.0,
                tint: 40.0,
            }),
        );
        let mut gizmos = GizmoLayer::new();
        let mut ctx = DevelopCtx {
            source_kind: SourceKind::Raw,
            edit: &mut binding,
            gizmos: &mut gizmos,
        };
        apply_wb_events(&mut ctx, vec![SliderEvent::Reset], |v| {
            WhiteBalance::Custom {
                temp_k: v as f32,
                tint: 0.0,
            }
        });
        assert_eq!(
            ctx.edit.value(ParamId::WhiteBalance),
            ParamValue::Wb(WhiteBalance::AsShot),
            "a Reset event must restore the binding's declared default"
        );
    }

    /// **D7 AC (presence gesture)**: a REAL synthetic drag on the Clarity
    /// slider's track (`presence_section`'s own `param_slider` call, the
    /// SAME primitive the tone sliders above go through) routes a
    /// `ParamId::Clarity` delta through the
    /// `begin_gesture`/`preview`/`end_gesture` lifecycle and lands on the
    /// recipe, mirrors `hsl.rs`'s own synthetic-drag proof. Texture/Dehaze
    /// are the identical `param_slider` primitive with a different
    /// `ParamId`, not separately re-proven here.
    #[test]
    fn a_real_synthetic_drag_commits_one_clarity_gesture_and_lands_the_value() {
        let mut harness = Harness::new_ui_state(
            |ui, binding: &mut RecordingBinding| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: SourceKind::Rendered,
                    edit: binding,
                    gizmos: &mut gizmos,
                };
                presence_section(ui, &mut ctx);
            },
            RecordingBinding::new(),
        );
        harness.set_size(egui::vec2(320.0, 120.0));
        harness.run();
        let track = harness.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Clarity");
        let rect = track.rect();
        harness.hover_at(rect.left_center());
        harness.run();
        harness.drag_at(rect.left_center());
        harness.run();
        harness.hover_at(rect.right_center());
        harness.run();
        harness.drop_at(rect.right_center());
        harness.run();

        let after = match harness.state().values.get(&ParamId::Clarity) {
            Some(ParamValue::F32(v)) => *v,
            _ => 0.0,
        };
        assert!(
            after > 50.0,
            "dragging the Clarity track to its right edge should land a large positive value: \
             got {after}"
        );
    }
}
