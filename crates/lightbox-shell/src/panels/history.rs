// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E8, the `develop.history` panel: a compact history list over E09's
//! query surface (step labels, click-to-restore, clear-with-confirm) and a
//! snapshots section (create named / restore / rename) over E09's
//! commands, all through the [`EditBinding`] seam (its cached
//! `history()`/`snapshots()` reads are event-driven, never per-frame SQL).
//!
//! Undo/redo buttons ride the same binding; the app-wide ⌘Z/⇧⌘Z keymap
//! wiring lives in `lib.rs::handle_action` (`edit.undo`/`edit.redo`,
//! Phase D, verified and re-routed through the binding this phase).
//!
//! [`EditBinding`]: crate::panels::develop_ctx::EditBinding

use eframe::egui;
use lightbox_edit::{ParamId, StepLabel};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::list_row_frame;

/// The history panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.history");

/// Registration (E1): mounted by `lib.rs` at app construction.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "History",
        source_req: SourceReq::Any,
        order: 90,
        build,
    }
}

/// Short display name for the M1 param set; falls back to the debug name
/// for params whose panels land later (E10/E11/E12).
fn param_name(p: ParamId) -> String {
    match p {
        ParamId::WhiteBalance => "White Balance".to_owned(),
        ParamId::Exposure => "Exposure".to_owned(),
        ParamId::Contrast => "Contrast".to_owned(),
        ParamId::Highlights => "Highlights".to_owned(),
        ParamId::Shadows => "Shadows".to_owned(),
        ParamId::Whites => "Whites".to_owned(),
        ParamId::Blacks => "Blacks".to_owned(),
        ParamId::ToneCurve => "Tone Curve".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Human text for a history step's label (spec §3.3 `StepLabel`).
fn step_text(label: &StepLabel) -> String {
    match label {
        StepLabel::Param(p) => param_name(*p),
        StepLabel::Preset { name } => format!("Preset: {name}"),
        StepLabel::Paste => "Paste settings".to_owned(),
        StepLabel::Sync => "Sync settings".to_owned(),
        StepLabel::Reset => "Reset".to_owned(),
        StepLabel::CrsImport => "Lightroom import".to_owned(),
        StepLabel::XmpRead => "Read XMP sidecar".to_owned(),
        StepLabel::SnapshotRestore { name } => format!("Snapshot: {name}"),
        StepLabel::HistoryRestore { seq } => format!("Restore to step {seq}"),
        // `StepLabel` is `#[non_exhaustive]`.
        _ => "Edit".to_owned(),
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    snapshots_section(ui, ctx);
    ui.separator();
    history_section(ui, ctx);
}

// ─── Snapshots ───────────────────────────────────────────────────────────────

fn snapshots_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Snapshots").strong());

    // Create (named): text field + button; Enter in the field also creates.
    let name_id = ui.make_persistent_id("develop.history.snap_name");
    let mut name: String = ui.data(|d| d.get_temp(name_id)).unwrap_or_default();
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(ui.available_width() - 64.0)
                .hint_text("Snapshot name"),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let create = ui
            .add_enabled(!name.trim().is_empty(), egui::Button::new("Create"))
            .clicked();
        if (create || enter) && !name.trim().is_empty() {
            ctx.edit.create_snapshot(name.trim());
            name.clear();
        }
    });
    ui.data_mut(|d| d.insert_temp(name_id, name));

    // List: click a name to restore; ✏ toggles an inline rename field.
    let snapshots = ctx.edit.snapshots().to_vec();
    if snapshots.is_empty() {
        ui.weak("No snapshots yet.");
    }
    for snap in snapshots {
        let rename_id = ui.make_persistent_id(("develop.history.rename", snap.id.0));
        let renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
        ui.horizontal(|ui| match renaming {
            Some(mut buf) => {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut buf).desired_width(ui.available_width() - 40.0),
                );
                let commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if cancel {
                    ui.data_mut(|d| d.remove::<String>(rename_id));
                } else if commit || field.lost_focus() {
                    if !buf.trim().is_empty() && buf.trim() != snap.name {
                        ctx.edit.rename_snapshot(snap.id, buf.trim());
                    }
                    ui.data_mut(|d| d.remove::<String>(rename_id));
                } else {
                    ui.data_mut(|d| d.insert_temp(rename_id, buf));
                }
            }
            None => {
                if ui
                    .add(egui::Button::new(&snap.name).frame(false))
                    .on_hover_text("Restore this snapshot")
                    .clicked()
                {
                    ctx.edit.restore_snapshot(snap.id);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("✏")
                        .on_hover_text("Rename snapshot")
                        .clicked()
                    {
                        ui.data_mut(|d| d.insert_temp(rename_id, snap.name.clone()));
                    }
                });
            }
        });
    }
}

// ─── History ─────────────────────────────────────────────────────────────────

fn history_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let confirm_id = ui.make_persistent_id("develop.history.confirm_clear");
    let confirming: bool = ui.data(|d| d.get_temp(confirm_id)).unwrap_or(false);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("History").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if confirming {
                if ui.small_button("Cancel").clicked() {
                    ui.data_mut(|d| d.insert_temp(confirm_id, false));
                }
                if ui.add(egui::Button::new("Confirm clear").small()).clicked() {
                    ctx.edit.clear_history();
                    ui.data_mut(|d| d.insert_temp(confirm_id, false));
                }
            } else if ui
                .small_button("Clear…")
                .on_hover_text("Drop the history log (keeps the current edit)")
                .clicked()
            {
                ui.data_mut(|d| d.insert_temp(confirm_id, true));
            }
        });
    });

    // Undo/redo, same binding seam the ⌘Z/⇧⌘Z keymap actions route
    // through in `lib.rs::handle_action`.
    ui.horizontal(|ui| {
        if ui
            .add_enabled(ctx.edit.can_undo(), egui::Button::new("Undo"))
            .clicked()
        {
            ctx.edit.undo();
        }
        if ui
            .add_enabled(ctx.edit.can_redo(), egui::Button::new("Redo"))
            .clicked()
        {
            ctx.edit.redo();
        }

        // Reset to as-shot. `EditCommand::ResetEdits` shipped with E09 and
        // had no caller in the shell or the CLI, so the one thing a
        // photographer reaches for first ("put it back how it was") was
        // implemented and unreachable. It goes here rather than in Basic
        // because it resets the whole recipe, not one panel, and because
        // this is where undo already lives.
        //
        // It writes one history step, so it is itself undoable. That is why
        // it needs no confirmation, unlike Clear above, which destroys the
        // log and cannot be undone.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Reset")
                .on_hover_text("Put every control back to as-shot. Undoable.")
                .clicked()
            {
                ctx.edit.reset_all();
            }
        });
    });

    // Newest-first steps (E09 `Queries::edit_history` order), plus the
    // seq-0 "Original" baseline row (`StepTo { seq: 0 }` is E09-valid).
    let steps: Vec<(u64, String, bool)> = ctx
        .edit
        .history()
        .iter()
        .map(|s| (s.seq, step_text(&s.label), s.is_head))
        .collect();
    let at_original = !steps.iter().any(|(_, _, head)| *head);
    for (seq, text, is_head) in steps {
        let label = if is_head {
            egui::RichText::new(text).strong()
        } else {
            egui::RichText::new(text)
        };
        // A plain frameless button, not `selectable_label`: the current
        // step reads via `list_row_frame`'s full-row wash+border, never a
        // form-control-shaped chip around just the label (owner report:
        // "why do the presets have radio buttons?", this list has the
        // exact same `selectable_label` shape presets.rs's row already
        // avoids).
        if list_row_frame(is_head)
            .show(ui, |ui| {
                ui.add(egui::Button::new(label).frame(false))
                    .on_hover_text(format!("Restore to step {seq}"))
            })
            .inner
            .clicked()
        {
            ctx.edit.restore_step(seq);
        }
    }
    if list_row_frame(at_original)
        .show(ui, |ui| {
            ui.add(egui::Button::new(egui::RichText::new("Original").italics()).frame(false))
                .on_hover_text("Restore the unedited original (step 0)")
        })
        .inner
        .clicked()
    {
        ctx.edit.restore_step(0);
    }
}
