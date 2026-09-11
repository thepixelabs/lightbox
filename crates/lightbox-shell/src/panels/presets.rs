// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `develop.presets` panel: preset management (spec §3.7 `PresetStore` +
//! §3.4 `Queries::presets`/`preset_preview_recipe`), browse/apply, create
//! from the current edit, rename/delete, export-to-file, and import, all
//! through the [`EditBinding`] seam (panels never touch `Session`/
//! `Command::Edit`/`PresetStore` directly, same discipline as every other
//! E08/E10 panel).
//!
//! # Layout
//!
//! * **Create Preset**, a name field, an optional folder (group) field,
//!   and a checklist of which param groups to carry (spec §3.1's partial-
//!   preset contract: the checked groups travel together as ONE coherent
//!   style bundle, masks/retouch are deliberately excluded from the
//!   checklist since their content is id-only and doesn't transfer across
//!   images, spec §3.1 ownership rule).
//! * **Import…**, an `rfd` open-files dialog (multi-select `.xmp`) routed
//!   through [`EditBinding::import_preset_files`].
//! * **Presets**, a browsable list of collapsible **dropdown sections**,
//!   one per group (spec §3.7 `PresetStore::list`'s own `(group, name)`
//!   sort, re-bucketed by [`grouped_presets`] so named groups read
//!   alphabetically and any ungrouped presets collect into a single
//!   trailing "Ungrouped" section, never interleaved with the named
//!   groups). Every section header shows its preset count ("Cinematic
//!   (7)") and starts **collapsed**, with an owner-sized library (E10's
//!   content pass ships ~30 presets across ~5-8 groups), an all-expanded
//!   panel is a wall of rows, not a browsable list; collapsed-by-default
//!   scales to that uniformly regardless of how many groups the data ends
//!   up with, and any one group is a single click away. See
//!   [`group_header_ui`]'s doc comment for the header's own chrome
//!   rationale. Each row, once its group is expanded:
//!   - click the name = **apply** (one history step, `StepLabel::Preset`);
//!   - **hover** = a PURE, no-history canvas preview
//!     ([`EditBinding::set_hover_preset`], never committed, queried, or
//!     written to history);
//!   - ✏ = inline rename (same pattern as `history.rs`'s snapshot rename);
//!   - ⬇ = export to an arbitrary file (`rfd` save dialog);
//!   - 🗑 = delete, gated by an inline confirm (same two-step pattern
//!     `history.rs`'s "Clear…" uses).
//!
//!   Collapsing a group never rewrites or discards a row's in-progress
//!   rename/delete-confirm state, the row simply stops being drawn (and
//!   therefore stops reacting to Enter/Esc/clicks) while hidden, and
//!   resumes exactly where it left off the moment its group re-expands.
//!   See [`list_section`]'s doc comment for why this is the honest
//!   behavior, not a special case.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui;
use lightbox_edit::{ParamGroup, ParamSubset, PresetId, PresetMeta};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::panels::widgets::list_row_frame;
use crate::theme::{fonts, tokens};

/// The presets panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.presets");

/// Registration (E1): mounted by `lib.rs` at app construction. Slots after
/// history (90), apply a preset, then check what it did in History.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Presets",
        source_req: SourceReq::Any,
        order: 95,
        build,
    }
}

/// The param groups offered by the "create preset" checklist, in display
/// order. Deliberately excludes [`ParamGroup::Masks`]/[`ParamGroup::Retouch`]
/// (id-only content that doesn't transfer across images, spec §3.1
/// ownership rule), the same groups `preset.rs`'s own tests exercise, minus
/// those two.
const CHECKLIST_GROUPS: &[(ParamGroup, &str)] = &[
    (ParamGroup::BaseProfile, "Profile"),
    (ParamGroup::WhiteBalance, "White Balance"),
    (ParamGroup::Tone, "Basic Tone"),
    (ParamGroup::Curve, "Tone Curve"),
    (ParamGroup::ColorMixer, "HSL"),
    (ParamGroup::ColorGrading, "Color Grading"),
    (ParamGroup::BwMix, "B&W Mixer"),
    (ParamGroup::Presence, "Presence"),
    (ParamGroup::Detail, "Detail"),
    (ParamGroup::Optics, "Optics"),
    (ParamGroup::Geometry, "Geometry"),
    (ParamGroup::Effects, "Effects"),
];

/// The dropdown group header's row height, deliberately shorter than the
/// rail-level collapsible-panel header band
/// ([`tokens::PANEL_HEADER_HEIGHT`], 24px, `panels::host::panel_header_ui`):
/// this is a panel-BODY-level subordinate header one rung further down the
/// hierarchy, and a visibly shorter row is part of how it reads that way.
/// Not part of the spec's token table (no other panel needs a nested-group
/// header height yet), same "local, deliberately smaller, no token backs
/// this exact number" posture as `panels::host::rail_heading_font`'s doc
/// comment.
const GROUP_HEADER_HEIGHT: f32 = 20.0;

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    create_section(ui, ctx);
    ui.separator();
    import_section(ui, ctx);
    ui.separator();
    list_section(ui, ctx);
}

// ─── Create ──────────────────────────────────────────────────────────────────

fn create_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Create Preset").strong());

    let name_id = ui.make_persistent_id("develop.presets.new_name");
    let group_id = ui.make_persistent_id("develop.presets.new_group");
    let subset_id = ui.make_persistent_id("develop.presets.new_subset");

    let mut name: String = ui.data(|d| d.get_temp(name_id)).unwrap_or_default();
    let mut group: String = ui.data(|d| d.get_temp(group_id)).unwrap_or_default();
    let mut subset: BTreeSet<ParamGroup> = ui.data(|d| d.get_temp(subset_id)).unwrap_or_default();

    ui.add(egui::TextEdit::singleline(&mut name).hint_text("Preset name"));
    ui.add(egui::TextEdit::singleline(&mut group).hint_text("Folder (optional)"));

    ui.label("Include:");
    egui::Grid::new("develop.presets.subset_grid")
        .num_columns(2)
        .show(ui, |ui| {
            for (i, (g, label)) in CHECKLIST_GROUPS.iter().enumerate() {
                let mut checked = subset.contains(g);
                if ui.checkbox(&mut checked, *label).changed() {
                    if checked {
                        subset.insert(*g);
                    } else {
                        subset.remove(g);
                    }
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });

    let can_create = !name.trim().is_empty() && !subset.is_empty();
    if ui
        .add_enabled(
            can_create,
            egui::Button::new("Create Preset from Current Edit"),
        )
        .on_hover_text(if subset.is_empty() {
            "Check at least one group to carry"
        } else {
            "Save the current edit's checked groups as a new preset"
        })
        .clicked()
    {
        let group_trimmed = group.trim();
        let group_opt = if group_trimmed.is_empty() {
            None
        } else {
            Some(group_trimmed)
        };
        ctx.edit.create_preset(
            name.trim(),
            group_opt,
            ParamSubset::from_groups(subset.iter().copied()),
        );
        name.clear();
    }

    ui.data_mut(|d| {
        d.insert_temp(name_id, name);
        d.insert_temp(group_id, group);
        d.insert_temp(subset_id, subset);
    });
}

// ─── Import ──────────────────────────────────────────────────────────────────

fn import_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.horizontal(|ui| {
        if ui
            .button("Import…")
            .on_hover_text("Import Lightbox- or Lightroom-authored .xmp preset files")
            .clicked()
        {
            if let Some(paths) = rfd::FileDialog::new()
                .set_title("Import Presets")
                .add_filter("XMP preset", &["xmp"])
                .pick_files()
            {
                ctx.edit.import_preset_files(paths);
            }
        }
    });
}

// ─── List (browse / apply / rename / delete / export) ─────────────────────────

/// Buckets `presets` (already sorted by the store as `(group, name)`, spec
/// §3.7 `PresetStore::list`/`refresh`) into display sections: one per named
/// group, sorted alphabetically, then any ungrouped presets collected into a
/// single trailing "Ungrouped" section. The store's own sort puts `None`
/// groups FIRST (`Option`'s derived `Ord`), this function deliberately
/// re-orders that to LAST, since a dropdown list reads better with the
/// named, browsable categories up top and the catch-all bucket at the
/// bottom. Pure and unit-tested directly, no `Ui`/`egui` dependency, and no
/// group name is ever hardcoded: every section comes straight out of
/// `presets`' own `group` field.
fn grouped_presets(presets: &[PresetMeta]) -> Vec<(Option<String>, Vec<PresetMeta>)> {
    let mut named: BTreeMap<String, Vec<PresetMeta>> = BTreeMap::new();
    let mut ungrouped: Vec<PresetMeta> = Vec::new();
    for p in presets {
        match &p.group {
            Some(g) => named.entry(g.clone()).or_default().push(p.clone()),
            None => ungrouped.push(p.clone()),
        }
    }
    let mut sections: Vec<(Option<String>, Vec<PresetMeta>)> =
        named.into_iter().map(|(g, ps)| (Some(g), ps)).collect();
    if !ungrouped.is_empty() {
        sections.push((None, ungrouped));
    }
    sections
}

/// A section header's display/accessible text: `"<name> (<count>)"`, falling
/// back to "Ungrouped" for the trailing `None` bucket.
fn section_label(group: Option<&str>, count: usize) -> String {
    let name = group.unwrap_or("Ungrouped");
    format!("{name} ({count})")
}

fn list_section(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    ui.label(egui::RichText::new("Presets").strong());

    let presets = ctx.edit.presets().to_vec();
    if presets.is_empty() {
        ui.weak("No presets yet — create one above or import a file.");
    }
    let sections = grouped_presets(&presets);

    // Hover is recomputed fresh every frame (no widget-local state, A12's
    // own discipline): whichever row (if any) is hovered THIS frame becomes
    // the canvas preview; a frame with no hovered row clears it. A row
    // inside a COLLAPSED group is simply never rendered this frame, so it
    // can never contribute a hover, collapsing a group is equivalent to
    // its rows not existing, the same way E2/§4 treats a filtered-out panel
    // (`panels::host::rail_contents_ui`).
    let mut hovered: Option<PresetId> = None;

    for (group, group_presets) in &sections {
        let label = section_label(group.as_deref(), group_presets.len());
        // Keyed by the GROUP identity, not the label text, so the open/
        // closed state survives a preset being added to/removed from the
        // group (which changes the count, and therefore the label) between
        // frames.
        let open_id = ui.make_persistent_id(("develop.presets.group_open", group));
        let open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(false);

        if group_header_ui(ui, group, &label, open).clicked() {
            ui.data_mut(|d| d.insert_temp(open_id, !open));
        }

        if open {
            for p in group_presets {
                preset_row_ui(ui, ctx, p, &mut hovered);
            }
        }
    }

    ctx.edit.set_hover_preset(hovered);
}

/// One preset row: click-to-apply / hover-to-preview, plus the rename/
/// export/delete affordances, unchanged behavior, only now called once per
/// visible row from inside [`list_section`]'s per-group loop instead of a
/// single flat loop over every preset.
fn preset_row_ui(
    ui: &mut egui::Ui,
    ctx: &mut DevelopCtx<'_>,
    p: &PresetMeta,
    hovered: &mut Option<PresetId>,
) {
    let rename_id = ui.make_persistent_id(("develop.presets.rename", &p.id.0));
    let confirm_id = ui.make_persistent_id(("develop.presets.confirm_delete", &p.id.0));
    let renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
    let confirming: bool = ui.data(|d| d.get_temp(confirm_id)).unwrap_or(false);

    // No durable "currently applied" identity exists for a preset (unlike
    // `panels::looks`'s `CreativeLut.id` or `panels::history`'s head step
    // `ApplyPreset` is a one-shot recipe merge, never a value the recipe
    // remembers), so there is no selection state to light up. Every row
    // still renders through `list_row_frame` so its padding/corner-radius
    // chrome matches Looks/History exactly; `selected` is unconditionally
    // `false`.
    list_row_frame(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            match renaming {
                Some(mut buf) => {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut buf)
                            .desired_width(ui.available_width() - 90.0),
                    );
                    let commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if cancel {
                        ui.data_mut(|d| d.remove::<String>(rename_id));
                    } else if commit || field.lost_focus() {
                        if !buf.trim().is_empty() && buf.trim() != p.name {
                            ctx.edit.rename_preset(p.id.clone(), buf.trim());
                        }
                        ui.data_mut(|d| d.remove::<String>(rename_id));
                    } else {
                        ui.data_mut(|d| d.insert_temp(rename_id, buf));
                    }
                }
                None => {
                    let resp = ui
                        .add(egui::Button::new(&p.name).frame(false))
                        .on_hover_text("Click to apply — hover to preview on the canvas");
                    if resp.hovered() {
                        *hovered = Some(p.id.clone());
                    }
                    if resp.clicked() {
                        ctx.edit.apply_preset(p.id.clone());
                    }
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if confirming {
                    if ui
                        .small_button("Confirm")
                        .on_hover_text("Permanently delete this preset")
                        .clicked()
                    {
                        ctx.edit.delete_preset(p.id.clone());
                        ui.data_mut(|d| d.insert_temp(confirm_id, false));
                    }
                    if ui.small_button("Cancel").clicked() {
                        ui.data_mut(|d| d.insert_temp(confirm_id, false));
                    }
                } else {
                    if ui
                        .small_button("🗑")
                        .on_hover_text("Delete preset…")
                        .clicked()
                    {
                        ui.data_mut(|d| d.insert_temp(confirm_id, true));
                    }
                    if ui
                        .small_button("⬇")
                        .on_hover_text("Export to a .xmp file")
                        .clicked()
                    {
                        if let Some(dest) = rfd::FileDialog::new()
                            .set_title("Export Preset")
                            .set_file_name(format!("{}.xmp", p.name))
                            .add_filter("XMP preset", &["xmp"])
                            .save_file()
                        {
                            ctx.edit.export_preset(p.id.clone(), dest);
                        }
                    }
                    if ui
                        .small_button("✏")
                        .on_hover_text("Rename preset")
                        .clicked()
                    {
                        ui.data_mut(|d| d.insert_temp(rename_id, p.name.clone()));
                    }
                }
            });
        });
    });
}

/// One dropdown group header, click toggles `open`.
///
/// **Deliberately lighter than [`panels::host::panel_header_ui`]**, not a
/// smaller copy of it: no gradient band, no active/solo accent bar, Inter
/// **Medium** ([`fonts::section_label_font`]) instead of SemiBold, sentence
/// case instead of UPPERCASE+tracking, and a shorter row
/// ([`GROUP_HEADER_HEIGHT`] vs. [`tokens::PANEL_HEADER_HEIGHT`]). Every one
/// of those is a rung this header steps DOWN from the rail-level treatment,
/// so a "Cinematic (7)" row reads as nested *inside* "Presets", never as a
/// second, competing header band directly under the first.
///
/// [`egui::CollapsingHeader`] was considered and rejected here too, for a
/// different reason than it was rejected at the rail level: this repo
/// already treats "hand-paint chrome through `theme::tokens`/`paint`/
/// `fonts`, resolved via `theme::Chrome::of`" as the house pattern for every
/// collapsible header (`panel_header_ui`'s own doc comment), and reusing
/// that exact toolkit here, rather than switching to a stock widget for
/// just this one rung, is what makes the *degree* of visual weight (one
/// notch down, not a totally different control) a deliberate, tunable
/// choice instead of "whatever `CollapsingHeader`'s default styling happens
/// to look like". The stock widget's own styling hooks are also the reason
/// `panel_header_ui` replaced it one level up (`panels::host`'s module doc
/// comment), nothing about that reasoning changes at this level, it only
/// gets applied with a visibly smaller brush.
///
/// AccessKit mirrors `panel_header_ui`'s own proxy exactly: `open` rides as
/// the node's `Toggled` flag (`WidgetInfo::selected`'s `selected` param
/// see that function's doc comment for why `.toggled()`, not
/// `.is_selected()`, is the right headless query), giving expand/collapse a
/// paint-free assertion the same way the rail's own headers already have
/// one.
fn group_header_ui(
    ui: &mut egui::Ui,
    id_key: &Option<String>,
    label: &str,
    open: bool,
) -> egui::Response {
    let id = ui.make_persistent_id(("develop.presets.group_header", id_key));
    let desired = egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
    let (_, rect) = ui.allocate_space(desired);
    let response = ui.interact(rect, id, egui::Sense::click());

    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::CollapsingHeader,
            ui.is_enabled(),
            open,
            label,
        )
    });

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        // Theme-resolved ink, never a hardcoded literal, the exact
        // discipline `panel_header_ui`'s own doc comment calls out (a
        // recent bug read chrome from the dark constants unconditionally
        // while the active theme was Light).
        let chrome = crate::theme::Chrome::of(ui.visuals());
        let ink = if hovered {
            chrome.text_primary
        } else {
            chrome.text_secondary
        };
        let painter = ui.painter();

        let triangle_center = egui::pos2(rect.left() + tokens::SPACE_1, rect.center().y);
        painter.add(egui::Shape::convex_polygon(
            disclosure_triangle_points(triangle_center, open),
            ink,
            egui::Stroke::NONE,
        ));

        let text_left =
            rect.left() + tokens::SPACE_1 + tokens::DISCLOSURE_TRIANGLE_SIZE.x + tokens::SPACE_1;
        let format = egui::TextFormat {
            font_id: fonts::section_label_font(),
            color: ink,
            ..Default::default()
        };
        let job = egui::text::LayoutJob::simple_format(label.to_owned(), format);
        let galley = painter.layout_job(job);
        let text_rect = egui::Align2::LEFT_CENTER
            .anchor_size(egui::pos2(text_left, rect.center().y), galley.size());
        painter.galley(text_rect.min, galley, ink);
    }

    response
}

/// The header's disclosure triangle, same right-collapsed/down-open
/// orientation as `panels::host`'s own panel header, that function
/// (`disclosure_triangle_points`) is private to its module, so this is a
/// small, deliberate local copy rather than a shared dependency (the brief
/// scope for this pass excludes touching `panels/host.rs`).
fn disclosure_triangle_points(center: egui::Pos2, open: bool) -> Vec<egui::Pos2> {
    let half_w = tokens::DISCLOSURE_TRIANGLE_SIZE.x * 0.5;
    let half_h = tokens::DISCLOSURE_TRIANGLE_SIZE.y * 0.5;
    [
        egui::vec2(-half_w, -half_h),
        egui::vec2(-half_w, half_h),
        egui::vec2(half_w, 0.0),
    ]
    .into_iter()
    .map(|v| center + if open { egui::vec2(-v.y, v.x) } else { v })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;
    use crate::theme::test_support::themed_state;
    use egui_kittest::{kittest::Queryable, Harness};
    use lightbox_edit::{HistoryStepMeta, ParamDelta, ParamId, ParamValue, SnapshotMeta};
    use lightbox_types::SnapshotId;

    // ── Pure grouping/sort logic, no `Ui`, fully assertable ────────────

    fn meta(id: &str, name: &str, group: Option<&str>) -> PresetMeta {
        PresetMeta {
            id: PresetId(id.to_owned()),
            name: name.to_owned(),
            group: group.map(str::to_owned),
        }
    }

    #[test]
    fn grouped_presets_empty_input_yields_no_sections() {
        assert_eq!(grouped_presets(&[]), Vec::new());
    }

    #[test]
    fn grouped_presets_sorts_named_groups_alphabetically_and_moves_ungrouped_last() {
        // Deliberately fed out of order, and with the ungrouped preset
        // FIRST (matching the store's own `(group, name)` sort, where
        // `None < Some(_)`), proving this function re-orders it to the
        // end rather than just passing the store's order through.
        let input = vec![
            meta("u1", "Loose One", None),
            meta("z1", "Zebra Look", Some("Zebra")),
            meta("a1", "Apple Look", Some("Apple")),
            meta("m1", "Mono A", Some("Mono")),
        ];
        let sections = grouped_presets(&input);
        let names: Vec<Option<String>> = sections.iter().map(|(g, _)| g.clone()).collect();
        assert_eq!(
            names,
            vec![
                Some("Apple".to_owned()),
                Some("Mono".to_owned()),
                Some("Zebra".to_owned()),
                None,
            ],
            "named groups sort alphabetically; the ungrouped bucket is always last"
        );
    }

    #[test]
    fn grouped_presets_preserves_within_group_input_order() {
        // The store already sorts within a group by name (spec §3.7); this
        // function must not re-sort on top of that, it only buckets.
        let input = vec![
            meta("c1", "Charlie", Some("Cinematic")),
            meta("a1", "Alpha", Some("Cinematic")),
            meta("b1", "Bravo", Some("Cinematic")),
        ];
        let sections = grouped_presets(&input);
        assert_eq!(sections.len(), 1);
        let names: Vec<&str> = sections[0].1.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Charlie", "Alpha", "Bravo"]);
    }

    /// Brief's explicit shape requirement: the layout must hold together
    /// for a singleton group AND a ten-preset group side by side.
    #[test]
    fn grouped_presets_handles_a_singleton_group_alongside_a_ten_preset_group() {
        let mut input = vec![meta("solo", "Solo Look", Some("Rare"))];
        for i in 0..10 {
            input.push(meta(&format!("p{i}"), &format!("Look {i}"), Some("Big")));
        }
        let sections = grouped_presets(&input);
        let rare = sections
            .iter()
            .find(|(g, _)| g.as_deref() == Some("Rare"))
            .unwrap();
        let big = sections
            .iter()
            .find(|(g, _)| g.as_deref() == Some("Big"))
            .unwrap();
        assert_eq!(rare.1.len(), 1);
        assert_eq!(big.1.len(), 10);
    }

    #[test]
    fn section_label_formats_name_and_count_and_falls_back_to_ungrouped() {
        assert_eq!(section_label(Some("Cinematic"), 7), "Cinematic (7)");
        assert_eq!(section_label(None, 3), "Ungrouped (3)");
    }

    // ── Widget behavior, AccessKit tree, `themed_state` (fonts bind a
    // named weight-role family: `section_label_font` = "InterMedium") ────

    struct RecordingBinding {
        presets: Vec<PresetMeta>,
        apply_calls: Vec<PresetId>,
        hover_calls: Vec<Option<PresetId>>,
        delete_calls: Vec<PresetId>,
    }

    impl RecordingBinding {
        fn new(presets: Vec<PresetMeta>) -> RecordingBinding {
            RecordingBinding {
                presets,
                apply_calls: Vec::new(),
                hover_calls: Vec::new(),
                delete_calls: Vec::new(),
            }
        }
    }

    impl EditBinding for RecordingBinding {
        fn value(&self, _p: ParamId) -> ParamValue {
            ParamValue::F32(0.0)
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::F32(0.0)
        }
        fn begin_gesture(&mut self, _p: ParamId) {}
        fn preview(&mut self, _d: ParamDelta) {}
        fn end_gesture(&mut self) {}
        fn reset(&mut self, _p: ParamId) {}
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

        fn presets(&self) -> &[PresetMeta] {
            &self.presets
        }
        fn apply_preset(&mut self, preset: PresetId) {
            self.apply_calls.push(preset);
        }
        fn delete_preset(&mut self, preset: PresetId) {
            self.delete_calls.push(preset);
        }
        fn set_hover_preset(&mut self, preset: Option<PresetId>) {
            self.hover_calls.push(preset);
        }
    }

    fn harness(binding: RecordingBinding) -> Harness<'static, (RecordingBinding, GizmoLayer)> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(280.0, 700.0))
            .build_ui_state(
                themed_state(|ui, s: &mut (RecordingBinding, GizmoLayer)| {
                    let mut ctx = DevelopCtx {
                        source_kind: lightbox_types::SourceKind::Raw,
                        edit: &mut s.0,
                        gizmos: &mut s.1,
                    };
                    list_section(ui, &mut ctx);
                }),
                (binding, GizmoLayer::new()),
            );
        // Frame 0 only installs the theme (`themed_state`'s documented
        // contract), every call site's own `h.run()` below is the first
        // real paint.
        h.run();
        h
    }

    #[test]
    fn groups_start_collapsed_and_a_header_click_expands_then_collapses_them() {
        let presets = vec![meta("p1", "Kodak Portra", Some("Film"))];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();

        h.get_by_label("Film (1)"); // the header itself always renders
        assert_eq!(
            h.query_all_by_label("Kodak Portra").count(),
            0,
            "collapsed by default: the row must not be in the tree yet"
        );

        h.get_by_label("Film (1)").click();
        h.run();
        h.get_by_label("Kodak Portra"); // now present

        h.get_by_label("Film (1)").click();
        h.run();
        assert_eq!(
            h.query_all_by_label("Kodak Portra").count(),
            0,
            "clicking the header again collapses it back"
        );
    }

    #[test]
    fn group_headers_show_the_group_name_and_preset_count() {
        let presets = vec![
            meta("c1", "Neon Nights", Some("Cinematic")),
            meta("c2", "Golden Hour", Some("Cinematic")),
            meta("u1", "Plain", None),
        ];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();
        h.get_by_label("Cinematic (2)");
        h.get_by_label("Ungrouped (1)");
    }

    #[test]
    fn clicking_an_expanded_preset_row_applies_it() {
        let presets = vec![meta("p1", "Moody Portrait", Some("Portraits"))];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();
        h.get_by_label("Portraits (1)").click();
        h.run();
        h.get_by_label("Moody Portrait").click();
        h.run();
        assert_eq!(h.state().0.apply_calls, vec![PresetId("p1".to_owned())]);
    }

    /// Hovering an expanded row sets the hover preview to its id; a frame
    /// that ends with nothing hovered clears it (`set_hover_preset(None)`
    /// fires every frame with no row under the pointer), mirrors
    /// `panels::looks`' own "hover recomputed fresh every frame" proof
    /// shape, but exercises the POSITIVE case too via a real synthetic
    /// hover (`Harness::hover_at`, the same idiom `panels::widgets`' own
    /// slider tests use).
    #[test]
    fn hovering_an_expanded_row_sets_and_clears_the_preview() {
        let presets = vec![meta("p1", "Clean & Bright", Some("Tonal"))];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();
        h.get_by_label("Tonal (1)").click();
        h.run();
        assert!(
            h.state().0.hover_calls.iter().any(|c| c.is_none()),
            "a frame with no pointer interaction must still clear the hover"
        );

        let rect = h.get_by_label("Clean & Bright").rect();
        h.hover_at(rect.center());
        h.run();
        assert_eq!(
            h.state().0.hover_calls.last(),
            Some(&Some(PresetId("p1".to_owned()))),
            "hovering the row sets the preview to its id"
        );

        h.hover_at(egui::pos2(-1000.0, -1000.0));
        h.run();
        assert_eq!(
            h.state().0.hover_calls.last(),
            Some(&None),
            "moving off the row clears the preview again"
        );
    }

    #[test]
    fn delete_confirm_two_step_calls_delete_preset() {
        let presets = vec![meta("p1", "Agfa Vista", Some("Film"))];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();
        h.get_by_label("Film (1)").click();
        h.run();
        h.get_by_label("🗑").click();
        h.run();
        h.get_by_label("Confirm").click();
        h.run();
        assert_eq!(h.state().0.delete_calls, vec![PresetId("p1".to_owned())]);
    }

    /// **Brief AC:** "if a dropdown hides a row mid-rename, make sure that
    /// state resolves sanely rather than stranding the edit." Proven here
    /// as: entering rename mode replaces the row's name BUTTON with a text
    /// field (so `get_by_label("Kodak")`, the button's own accessible
    /// name, stops matching; the `TextEdit`'s accesskit node carries
    /// "Kodak" as its VALUE, not its label, so it does not satisfy a label
    /// query). Collapsing the row's group, then re-expanding it, must land
    /// back in rename mode (the name button stays absent throughout), the
    /// in-progress edit is paused while hidden, never silently discarded or
    /// force-committed.
    #[test]
    fn renaming_state_survives_collapsing_and_reopening_its_group() {
        let presets = vec![meta("p1", "Kodak", Some("Film"))];
        let mut h = harness(RecordingBinding::new(presets));
        h.run();
        h.get_by_label("Film (1)").click(); // expand
        h.run();
        h.get_by_label("Kodak"); // the plain apply button, not renaming yet

        h.get_by_label("✏").click(); // enter rename mode
        h.run();
        assert_eq!(
            h.query_all_by_label("Kodak").count(),
            0,
            "the name button is replaced by a text field while renaming"
        );

        h.get_by_label("Film (1)").click(); // collapse
        h.run();
        assert_eq!(
            h.query_all_by_label("Kodak").count(),
            0,
            "collapsed: the row (button or field) does not exist right now"
        );

        h.get_by_label("Film (1)").click(); // re-expand
        h.run();
        assert_eq!(
            h.query_all_by_label("Kodak").count(),
            0,
            "re-expanding must resume rename mode, not silently drop it \
             (a reverted row would show the \"Kodak\" button again)"
        );
    }
}
