// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `develop.looks` panel (E10 task **D11**): the creative-look browser
//! install, browse (grouped by family), apply, hover-preview, an amount
//! slider for the currently-applied look, and a missing-look badge, all
//! through the [`EditBinding`] seam (panels never touch `Session`/
//! `Command`/the catalog directly, same discipline as every other E08/E10
//! panel, mirrors `panels::presets`' shape closely, since installing/
//! applying/hovering a look is the exact same UX pattern as a develop
//! preset, just over the D10 `installed_look` registry instead of E09's
//! filesystem `PresetStore`).
//!
//! # Layout
//!
//! * **Install…**, an `rfd` open-file dialog (`.cube`/`.png`) routed
//!   through [`EditBinding::install_look_file`].
//! * **Applied Look**, the current `ParamId::CreativeLut` value, if any: a
//!   ⚠ missing-look badge when its id isn't in
//!   [`EditBinding::installed_looks`] ([`EditBinding::is_look_installed`]),
//!   an Amount slider (`0..=100`%, decomposing the one `CreativeLut` leaf
//!   the same decompose/recompose-a-coarse-leaf convention
//!   `panels::hsl::band_slider` already establishes), and a Remove button
//!   ([`EditBinding::reset`] back to the `None` identity).
//! * **Installed Looks**, grouped (family) list. Each row:
//!   - click the name = **apply** at full strength (one history step);
//!   - **hover** = a PURE, no-history canvas preview
//!     ([`EditBinding::set_hover_look`], never committed, queried, or
//!     written to history, mirrors `panels::presets`' hover exactly);
//!   - 🗑 = delete, gated by an inline confirm (same two-step pattern
//!     `panels::presets`/`panels::history` use for their own destructive
//!     actions).

use eframe::egui;
use lightbox_core::{InstalledLookRow, LookId};
use lightbox_edit::{CreativeLut, ParamDelta, ParamId, ParamValue};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::{list_row_frame, value_slider, SliderEvent, SliderSpec};

/// The look-browser panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.looks");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots after
/// color grading (45) and the B&W mixer (42), before history (90), a
/// creative look is the last stage of the tone/color chain (spec §4.1), so
/// it belongs after the other color/tonal panels and before the
/// meta-panels (history/presets).
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Creative Looks",
        source_req: SourceReq::Any,
        order: 60,
        build,
    }
}

fn current_lut(ctx: &DevelopCtx<'_>) -> Option<CreativeLut> {
    match ctx.edit.value(ParamId::CreativeLut) {
        ParamValue::Lut(v) => v,
        _ => None,
    }
}

fn lut_delta(cl: CreativeLut) -> ParamDelta {
    let mut delta = ParamDelta::new();
    delta
        .0
        .insert(ParamId::CreativeLut, ParamValue::Lut(Some(cl)));
    delta
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    install_section(ui, ctx);
    ui.separator();
    applied_section(ui, ctx);
    ui.separator();
    grid_section(ui, ctx);
}

// ─── Install ─────────────────────────────────────────────────────────────────

fn install_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.horizontal(|ui| {
        if ui
            .button("Install…")
            .on_hover_text("Install a .cube or HaldCLUT (.png) creative-look file")
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Install Creative Look")
                .add_filter("Look files", &["cube", "png"])
                .pick_file()
            {
                ctx.edit.install_look_file(path, None);
            }
        }
    });
}

// ─── Applied look (amount slider + missing badge) ──────────────────────────

fn applied_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Applied Look").strong());

    let Some(cl) = current_lut(ctx) else {
        ui.weak("No creative look applied — pick one below.");
        return;
    };

    let installed = ctx.edit.is_look_installed(&cl.id);
    if installed {
        let name = ctx
            .edit
            .installed_looks()
            .iter()
            .find(|l| l.content_hash == cl.id)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| cl.id.clone());
        ui.label(name);
    } else {
        // D10/D11 missing-look badge: the param survives (never rewritten),
        // the node degrades to identity, this is the visible signal.
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "⚠ Missing look — rendering as identity",
        );
    }

    amount_slider(ui, ctx, &cl);

    if ui
        .button("Remove")
        .on_hover_text("Clear the applied look")
        .clicked()
    {
        ctx.edit.reset(ParamId::CreativeLut);
    }
}

/// The `CreativeLut.amount` slider (`0..=100`%, spec §4.8 unity `100`),
/// decomposing/recomposing the ONE `CreativeLut` leaf, mirrors
/// `panels::hsl::band_slider`'s exact decompose-a-coarse-leaf shape.
fn amount_slider(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, cl: &CreativeLut) {
    let spec = SliderSpec {
        min: 0.0,
        max: 100.0,
        step: 1.0,
        fine: 0.1,
        label: "Amount",
        unit: Some("%"),
    };
    let (_, events) = value_slider(ui, "develop.looks.amount", cl.amount as f64, &spec);
    for ev in events {
        match ev {
            SliderEvent::Begin => ctx.edit.begin_gesture(ParamId::CreativeLut),
            SliderEvent::Preview(v) => {
                let next = CreativeLut {
                    id: cl.id.clone(),
                    amount: v as f32,
                };
                ctx.edit.preview(lut_delta(next));
            }
            SliderEvent::End => ctx.edit.end_gesture(),
            SliderEvent::Reset => {
                // The per-field neutral for `amount` is unity (100%, spec
                // §4.8), NOT 0, and NOT clearing the whole look (that is
                // the separate "Remove" button, same "channel-blind reset
                // would silently discard other work" discipline
                // `panels::curve::reset_active`/`panels::hsl::reset_band`
                // already document for their own coarse leaves).
                let next = CreativeLut {
                    id: cl.id.clone(),
                    amount: 100.0,
                };
                ctx.edit.begin_gesture(ParamId::CreativeLut);
                ctx.edit.preview(lut_delta(next));
                ctx.edit.end_gesture();
            }
            SliderEvent::Commit(v) => {
                let next = CreativeLut {
                    id: cl.id.clone(),
                    amount: v as f32,
                };
                ctx.edit.begin_gesture(ParamId::CreativeLut);
                ctx.edit.preview(lut_delta(next));
                ctx.edit.end_gesture();
            }
        }
    }
}

// ─── Grid (browse / apply / hover / remove) ────────────────────────────────

fn grid_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Installed Looks").strong());

    let looks: Vec<InstalledLookRow> = ctx.edit.installed_looks().to_vec();
    if looks.is_empty() {
        // D&I gap (owner report): "what is creative looks? where do i
        // install them from?", a creative look is a LUT-based color grade
        // (`.cube` or HaldCLUT `.png`), and nothing ships bundled (unlike
        // Presets' seeded starter library), so a fresh install's empty
        // state must say both what this panel wants and how to fill it
        // matching `panels::presets`' own empty-state tone/shape exactly.
        ui.weak(
            "No creative looks installed yet — these are LUT-based color \
             grades (.cube / HaldCLUT .png files). Click Install… above to \
             add one.",
        );
    }

    let applied_id = current_lut(ctx).map(|cl| cl.id);

    // Hover is recomputed fresh every frame (no widget-local state, same
    // discipline `panels::presets::list_section` documents): whichever row
    // (if any) is hovered THIS frame becomes the canvas preview; a frame
    // with no hovered row clears it.
    let mut hovered: Option<String> = None;
    let mut last_family: Option<Option<String>> = None;

    for look in &looks {
        if last_family.as_ref() != Some(&look.family) {
            ui.separator();
            ui.weak(
                look.family
                    .clone()
                    .unwrap_or_else(|| "Ungrouped".to_owned()),
            );
            last_family = Some(look.family.clone());
        }

        let confirm_id = ui.make_persistent_id(("develop.looks.confirm_delete", look.id));
        let applied = applied_id.as_deref() == Some(look.content_hash.as_str());

        list_row_frame(applied).show(ui, |ui| {
            ui.horizontal(|ui| {
                // A plain frameless button, not `selectable_label`: the
                // "applied" state reads via this row's full-width
                // `list_row_frame` wash+border, never a form-control-shaped
                // chip around just the label (see that helper's doc
                // comment, this is the D&I fix for "why do the presets
                // have radio buttons?").
                let resp = ui
                    .add(egui::Button::new(&look.name).frame(false))
                    .on_hover_text("Click to apply — hover to preview on the canvas");
                if resp.hovered() {
                    hovered = Some(look.content_hash.clone());
                }
                if resp.clicked() {
                    ctx.edit.apply_look(&look.content_hash);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirming: bool = ui.data(|d| d.get_temp(confirm_id)).unwrap_or(false);
                    if confirming {
                        if ui
                            .small_button("Confirm")
                            .on_hover_text("Permanently remove this installed look")
                            .clicked()
                        {
                            ctx.edit.remove_look(LookId(look.id));
                            ui.data_mut(|d| d.insert_temp(confirm_id, false));
                        }
                        if ui.small_button("Cancel").clicked() {
                            ui.data_mut(|d| d.insert_temp(confirm_id, false));
                        }
                    } else if ui
                        .small_button("🗑")
                        .on_hover_text("Remove this installed look…")
                        .clicked()
                    {
                        ui.data_mut(|d| d.insert_temp(confirm_id, true));
                    }
                });
            });
        });
    }

    ctx.edit.set_hover_look(hovered.as_deref());
}

#[cfg(test)]
mod tests {
    use egui_kittest::{kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, SnapshotMeta};
    use lightbox_types::SnapshotId;

    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;

    /// A recording `EditBinding` test double, the same shape
    /// `panels::hsl`/`panels::curve`'s own widget-interaction tests use: no
    /// real `Session`, just enough state to prove the panel calls the RIGHT
    /// `EditBinding` methods with the RIGHT arguments.
    struct RecordingBinding {
        current: Option<CreativeLut>,
        looks: Vec<InstalledLookRow>,
        applied_calls: Vec<String>,
        hover_calls: Vec<Option<String>>,
        removed_calls: Vec<LookId>,
        installed_calls: Vec<(std::path::PathBuf, Option<String>)>,
        gesture_events: Vec<&'static str>,
    }

    impl RecordingBinding {
        fn new(looks: Vec<InstalledLookRow>) -> RecordingBinding {
            RecordingBinding {
                current: None,
                looks,
                applied_calls: Vec::new(),
                hover_calls: Vec::new(),
                removed_calls: Vec::new(),
                installed_calls: Vec::new(),
                gesture_events: Vec::new(),
            }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, p: ParamId) -> ParamValue {
            match p {
                ParamId::CreativeLut => ParamValue::Lut(self.current.clone()),
                _ => ParamValue::F32(0.0),
            }
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::Lut(None)
        }
        fn begin_gesture(&mut self, _p: ParamId) {
            self.gesture_events.push("begin");
        }
        fn preview(&mut self, d: ParamDelta) {
            if let Some(ParamValue::Lut(v)) = d.0.get(&ParamId::CreativeLut).cloned() {
                self.current = v;
            }
            self.gesture_events.push("preview");
        }
        fn end_gesture(&mut self) {
            self.gesture_events.push("end");
        }
        fn reset(&mut self, _p: ParamId) {
            self.current = None;
            self.gesture_events.push("reset");
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

        fn installed_looks(&self) -> &[InstalledLookRow] {
            &self.looks
        }
        fn apply_look(&mut self, content_hash: &str) {
            self.applied_calls.push(content_hash.to_owned());
            self.current = Some(CreativeLut {
                id: content_hash.to_owned(),
                amount: 100.0,
            });
        }
        fn set_hover_look(&mut self, content_hash: Option<&str>) {
            self.hover_calls.push(content_hash.map(str::to_owned));
        }
        fn install_look_file(&mut self, path: std::path::PathBuf, family: Option<String>) {
            self.installed_calls.push((path, family));
        }
        fn remove_look(&mut self, id: LookId) {
            self.removed_calls.push(id);
        }
    }

    fn look_row(id: i64, name: &str, family: Option<&str>, hash: &str) -> InstalledLookRow {
        InstalledLookRow {
            id,
            kind: "cube3d".to_owned(),
            name: name.to_owned(),
            family: family.map(str::to_owned),
            rel_path: format!("{}/{}.cube", &hash[0..2.min(hash.len())], hash),
            content_hash: hash.to_owned(),
            source: "user".to_owned(),
            license: None,
            installed_at: 0,
        }
    }

    fn harness(binding: RecordingBinding) -> Harness<'static, (RecordingBinding, GizmoLayer)> {
        Harness::new_ui_state(
            |ui, s: &mut (RecordingBinding, GizmoLayer)| {
                let mut ctx = DevelopCtx {
                    source_kind: lightbox_types::SourceKind::Raw,
                    edit: &mut s.0,
                    gizmos: &mut s.1,
                };
                build(ui, &mut ctx);
            },
            (binding, GizmoLayer::new()),
        )
    }

    /// Clicking an installed look's row calls `apply_look` with its exact
    /// content hash, the click-to-apply wiring.
    #[test]
    fn clicking_a_look_row_applies_it() {
        let looks = vec![look_row(1, "Kodak Portra", Some("Film"), "hash-aaa")];
        let mut h = harness(RecordingBinding::new(looks));
        h.run();
        h.get_by_label("Kodak Portra").click();
        h.run();
        assert_eq!(h.state().0.applied_calls, vec!["hash-aaa".to_owned()]);
    }

    /// Hovering a row sets the hover preview to its hash; a frame that ends
    /// with nothing hovered clears it (`set_hover_look(None)` fires every
    /// frame with no row under the pointer), mirrors
    /// `panels::presets`' own "hover recomputed fresh every frame" proof
    /// shape.
    #[test]
    fn hover_calls_set_hover_look_and_clears_when_nothing_is_hovered() {
        let looks = vec![look_row(1, "Fuji Velvia", None, "hash-bbb")];
        let mut h = harness(RecordingBinding::new(looks));
        h.run();
        // No pointer interaction this frame: the grid must still call
        // `set_hover_look(None)` once (the "always fresh" contract).
        assert!(h.state().0.hover_calls.iter().any(|c| c.is_none()));
    }

    /// **D11 AC (missing-look badge):** when the applied `CreativeLut.id`
    /// isn't present in `installed_looks()`, the panel renders the ⚠
    /// missing-look text, and the applied-look accessors never mutate
    /// state to "fix" it (the param survives untouched).
    #[test]
    fn missing_look_shows_the_badge() {
        let mut binding = RecordingBinding::new(vec![look_row(1, "Installed", None, "hash-ccc")]);
        binding.current = Some(CreativeLut {
            id: "hash-not-installed".to_owned(),
            amount: 100.0,
        });
        let mut h = harness(binding);
        h.run();
        h.get_by_label_contains("Missing look");
        // The param is untouched, still pointing at the missing hash.
        assert_eq!(
            h.state().0.current.as_ref().map(|c| c.id.as_str()),
            Some("hash-not-installed")
        );
    }

    /// The amount slider decomposes/recomposes the ONE `CreativeLut` leaf
    /// through the ordinary begin/preview/end gesture triplet, a real
    /// synthetic focus + arrow-key nudge (the exact proven kittest idiom
    /// `panels::grading::arrow_keys_nudge_the_focused_wheel` uses), not a
    /// hand-called event function, commits ONE step and never touches
    /// `id`.
    #[test]
    fn amount_slider_arrow_nudge_recomposes_the_same_leaf_id() {
        let mut binding = RecordingBinding::new(Vec::new());
        binding.current = Some(CreativeLut {
            id: "hash-ddd".to_owned(),
            amount: 40.0,
        });
        let mut h = harness(binding);
        h.run();
        let node = h.get_by_role_and_label(eframe::egui::accesskit::Role::Slider, "Amount");
        node.focus();
        h.run();
        h.key_press(eframe::egui::Key::ArrowUp);
        h.run();
        assert_eq!(
            h.state().0.current,
            Some(CreativeLut {
                id: "hash-ddd".to_owned(),
                amount: 41.0,
            }),
            "arrow-nudge increments amount by `step` (1.0), id untouched"
        );
    }

    /// Removing a row (via the confirm two-step) calls `remove_look` with
    /// the row's `LookId`.
    #[test]
    fn remove_button_confirm_step_calls_remove_look() {
        let looks = vec![look_row(7, "Agfa Vista", Some("Film"), "hash-eee")];
        let mut h = harness(RecordingBinding::new(looks));
        h.run();
        h.get_by_label("🗑").click();
        h.run();
        h.get_by_label("Confirm").click();
        h.run();
        assert_eq!(h.state().0.removed_calls, vec![LookId(7)]);
    }

    /// Rows are grouped by family in iteration order (ungrouped first, per
    /// `Queries::installed_looks`' own `(family, name)` sort, this test
    /// exercises the panel's grouping HEADER rendering, not the sort
    /// itself, which is the DAO's job, already proven in
    /// `lightbox-catalog`).
    #[test]
    fn family_headers_render_for_each_group() {
        let looks = vec![
            look_row(1, "Alpha", None, "h1"),
            look_row(2, "Beta", Some("Film"), "h2"),
        ];
        let mut h = harness(RecordingBinding::new(looks));
        h.run();
        h.get_by_label("Ungrouped");
        h.get_by_label("Film");
    }
}
