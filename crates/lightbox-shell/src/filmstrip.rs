// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The session filmstrip (E08 spec §6.3) — repurposes the retired E01
//! grid's virtualization math turned 90° (spec §3 crate map: "`grid.rs` →
//! `filmstrip.rs`, horizontal repurpose").
//!
//! **Phase B (B1–B5) is implemented here** on top of the Phase-A skeleton:
//!
//! * **B1** — pure strip geometry ([`strip_layout`], [`visible_range`],
//!   [`fit_rect`]), unit-tested like the retired grid's `layout`/
//!   `visible_indices` were.
//! * **B2** — virtualized [`filmstrip_ui`] over a lazy per-index cell
//!   accessor + the unchanged [`ThumbCache`] `want`/`end_frame` discipline
//!   (cells materialize **only** for the visible index range); Loading
//!   shimmer and Failed placard cells; active ring; click activates.
//! * **B3** — §6.3 cell chrome: `RAW` tag, edited dot (from the E09
//!   `edit_index` projection via [`EditedBadges`]), error/duplicate badges
//!   with hover reasons, clipped filename label. **No stars, no flags, no
//!   labels** — the library rating UI is retired (spec §2.2).
//! * **B4** — [`nav_delta`] (repeatable ←/→, one step per key-repeat
//!   event), instant scroll-to-active, plain-wheel horizontal scrolling,
//!   ⌘/⇧-click multi-select retained (dormant) for M2 batch ops. The
//!   nav-swap latency probe stays where Phase A left it (`EditorCanvas::
//!   nav_swap_ms` as of Phase C, surfaced in the F1 overlay); the formal
//!   p95 gate is H2.
//! * **B5** — drag-resize (48–160 pt via the host panel's `size_range`,
//!   read back into [`FilmstripState`]), collapse toggle
//!   ([`collapsed_bar_ui`]), and the overflow position indicator
//!   ("34/212").
//!
//! **Phase G persistence seam:** [`FilmstripState::height_pt`] and
//! [`FilmstripState::collapsed`] are **in-session only** for now. Phase G's
//! machine-scope prefs store (spec §6.7 `MachinePrefs::filmstrip_height_pt`,
//! plus a collapsed flag alongside it) should construct [`FilmstripState`]
//! from the persisted values at boot and write these accessors back on
//! change. Do NOT invent a separate prefs file here.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use eframe::egui;
use lightbox_core::{ItemState, Session, SetEpoch, WorkingSetItem};
use lightbox_types::{ImageId, SourceKind};

use crate::thumbs::{bucket_for, ThumbCache, ThumbState};

/// Space between cells, logical points.
const SPACING: f32 = 6.0;
/// Height of the filename strip under each cell, logical points.
const LABEL_H: f32 = 14.0;
/// Smallest useful thumbnail edge (a 48 pt strip still shows an image).
const MIN_CELL: f32 = 24.0;

/// Strip-height drag-resize bounds, logical points (spec §8 B5: 48–160 pt).
pub const MIN_HEIGHT_PT: f32 = 48.0;
/// See [`MIN_HEIGHT_PT`].
pub const MAX_HEIGHT_PT: f32 = 160.0;
/// Startup strip height until Phase G restores a persisted value.
pub const DEFAULT_HEIGHT_PT: f32 = 96.0;
/// Height of the collapsed strip bar (expand affordance + indicator).
pub const COLLAPSED_BAR_PT: f32 = 24.0;

/// What a filmstrip frame asks the app to do.
#[derive(Debug, PartialEq)]
pub enum FilmstripAction {
    /// Activate this entry (click) — the loupe should now show it.
    Activate(usize),
}

// ─── B1: pure geometry ──────────────────────────────────────────────────────

/// Pure cell geometry for one frame (unit-tested like the retired grid's
/// `layout` was) — the horizontal axis' equivalent of `GridLayout`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StripLayout {
    /// Cell edge (image area is `cell`×`cell`), logical points.
    pub cell: f32,
    /// Horizontal pitch including inter-cell spacing.
    pub cell_w: f32,
    /// Total scrollable content width for `total` entries.
    pub total_w: f32,
}

/// Computes one frame's strip geometry.
pub fn strip_layout(cell: f32, total: usize) -> StripLayout {
    let cell_w = cell + SPACING;
    StripLayout {
        cell,
        cell_w,
        total_w: total as f32 * cell_w,
    }
}

/// Index range of the entries intersecting the horizontal viewport
/// `[left, right)`, clamped to `total`.
pub fn visible_range(l: &StripLayout, total: usize, left: f32, right: f32) -> Range<usize> {
    if total == 0 || right <= 0.0 || l.cell_w <= 0.0 {
        return 0..0;
    }
    let first = (left / l.cell_w).floor().max(0.0) as usize;
    let last = (right / l.cell_w).ceil().max(0.0) as usize;
    (first.min(total))..(last.min(total))
}

/// Largest aspect-preserving rect for `size` centered in `container` (never
/// upscales). Shared with the loupe's fit-zoom.
pub fn fit_rect(container: egui::Rect, size: [f32; 2]) -> egui::Rect {
    let scale = (container.width() / size[0])
        .min(container.height() / size[1])
        .min(1.0);
    let fitted = egui::vec2(size[0] * scale, size[1] * scale);
    egui::Rect::from_center_size(container.center(), fitted)
}

// ─── B2/B3: the per-cell view model ────────────────────────────────────────

/// What one cell needs to paint (§6.3 chrome). A shell-side projection of
/// core's `#[non_exhaustive]` [`WorkingSetItem`] joined with the E09
/// edit-badge lookup — decoupled so (a) `is_edited` (which lives in the
/// `edit_index` projection, not the working-set snapshot) joins in exactly
/// one place, and (b) kittest can build synthetic sets (B2/B4 ACs) without
/// a real `Session`.
#[derive(Clone, Debug, PartialEq)]
pub struct StripCell {
    /// NFC filename, shown under the thumbnail + in the tooltip.
    pub filename: String,
    /// `SourceKind::Raw` → the `RAW` tag (§6.3).
    pub raw: bool,
    /// The E09 `edit_index` projection → the edited dot (§6.3).
    pub is_edited: bool,
    /// Load state → thumbnail / shimmer / placard.
    pub status: CellStatus,
}

/// Per-cell load state, folded from [`ItemState`].
#[derive(Clone, Debug, PartialEq)]
pub enum CellStatus {
    /// Planned; hash/registration pending — Loading shimmer.
    Loading,
    /// Registered — thumbnail (or shimmer while the thumb decodes).
    Ready(ImageId),
    /// Probe/hash/registration failed — placard + hover reason.
    Failed {
        /// Human-readable failure reason (badged, never hidden).
        reason: String,
    },
    /// Collapsed into an earlier identical-content item.
    Duplicate {
        /// Index of the earlier item this one collapsed into.
        of: usize,
    },
}

/// Projects one core working-set item (+ badge lookup) into a [`StripCell`].
pub fn cell_for(item: &WorkingSetItem, edited: &EditedBadges) -> StripCell {
    let status = match item.state {
        ItemState::Planned => CellStatus::Loading,
        ItemState::Ready { image, .. } => CellStatus::Ready(image),
        ItemState::Failed => CellStatus::Failed {
            reason: item
                .decode_error
                .clone()
                .unwrap_or_else(|| "open failed".to_owned()),
        },
        ItemState::DuplicateOf { index } => CellStatus::Duplicate { of: index },
    };
    StripCell {
        filename: item.filename.clone(),
        raw: item.source_kind == Some(SourceKind::Raw),
        is_edited: match status {
            CellStatus::Ready(image) => edited.is_edited(image),
            _ => false,
        },
        status,
    }
}

/// Event-driven cache of the E09 `edit_index` projection (B3 edited dot).
///
/// The §7 threading contract forbids per-frame SQL in the shell; E09 built
/// `Queries::edit_badges` ("Filmstrip edit badges", one indexed batch
/// SELECT) for exactly this consumer. The cache refreshes **only** when
/// marked dirty — on `Event::EditCommitted` and on working-set events — so
/// steady-state frames never touch the reader.
#[derive(Default)]
pub struct EditedBadges {
    edited: HashMap<ImageId, bool>,
    dirty: bool,
}

impl EditedBadges {
    /// Starts dirty so the first frame with a non-empty set pulls once.
    pub fn new() -> EditedBadges {
        EditedBadges {
            edited: HashMap::new(),
            dirty: true,
        }
    }

    /// Schedule a re-pull (an edit committed / the set changed).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The badge lookup ([`cell_for`] consumer). Unknown images are
    /// untouched-by-definition (E09 D1: no `edit_index` row = neutral).
    pub fn is_edited(&self, image: ImageId) -> bool {
        self.edited.get(&image).copied().unwrap_or(false)
    }

    /// One batch `Queries::edit_badges` pull when dirty; a no-op otherwise.
    /// Failures keep the previous lookup and are logged, never fatal (§7).
    pub fn refresh_if_dirty(&mut self, session: &Session, items: &[WorkingSetItem]) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let ids: Vec<ImageId> = items
            .iter()
            .filter_map(|it| match it.state {
                ItemState::Ready { image, .. } => Some(image),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            self.edited.clear();
            return;
        }
        match session.query().edit_badges(&ids) {
            Ok(badges) => {
                self.edited.clear();
                for b in badges {
                    self.edited.insert(b.image, b.is_edited);
                }
            }
            Err(err) => {
                tracing::warn!(target: "lightbox_shell", %err, "edit-badge refresh failed");
            }
        }
    }
}

// ─── B4/B5: strip UI state ──────────────────────────────────────────────────

/// In-session filmstrip UI state: height/collapse (B5), multi-select (B4),
/// scroll-to-active bookkeeping. See the module docs for the Phase G
/// persistence seam on `height`/`collapsed`.
pub struct FilmstripState {
    height: f32,
    collapsed: bool,
    /// ⌘/⇧-click multi-select — dormant state retained for M2 batch ops
    /// (spec §6.2 "Selection retained, rescoped to active+multi"; it lives
    /// here rather than on `WorkingSetView` so Phase B leaves the view
    /// model untouched — Phase E's batch consumers may lift it).
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    /// The active index last scrolled to (so activation scrolls exactly
    /// once, and user scrolling away is not fought frame-by-frame).
    scrolled_to: Option<usize>,
    epoch: Option<SetEpoch>,
}

impl Default for FilmstripState {
    fn default() -> FilmstripState {
        FilmstripState::new()
    }
}

impl FilmstripState {
    /// Session defaults; Phase G will seed `height`/`collapsed` from the
    /// machine prefs instead (see module docs).
    pub fn new() -> FilmstripState {
        FilmstripState {
            height: DEFAULT_HEIGHT_PT,
            collapsed: false,
            selected: BTreeSet::new(),
            anchor: None,
            scrolled_to: None,
            epoch: None,
        }
    }

    /// Current strip height, logical points — always within
    /// [`MIN_HEIGHT_PT`]..=[`MAX_HEIGHT_PT`]. Phase G persists this.
    pub fn height_pt(&self) -> f32 {
        self.height
    }

    /// Clamps to the B5 spec range (48–160 pt); the host panel's
    /// `size_range` enforces the same bounds on drag.
    pub fn set_height_pt(&mut self, height: f32) {
        self.height = height.clamp(MIN_HEIGHT_PT, MAX_HEIGHT_PT);
    }

    /// Collapsed flag (B5). Phase G persists this.
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Sets the collapsed flag (the `film.toggle` action binds here in
    /// Phase D). Re-expanding snaps back to the active cell (the scroll
    /// bookkeeping resets so the next frame re-issues scroll-to-active).
    pub fn set_collapsed(&mut self, collapsed: bool) {
        if self.collapsed && !collapsed {
            self.scrolled_to = None;
        }
        self.collapsed = collapsed;
    }

    /// The multi-select set (session indices, sorted) — the M2 batch-op
    /// seam (spec B4 "retained for future batch ops"); no shipped consumer
    /// yet, exercised by the Phase B selection tests.
    #[allow(dead_code)]
    pub fn selected(&self) -> &BTreeSet<usize> {
        &self.selected
    }

    /// Per-frame reconciliation with the working set: an epoch change
    /// clears selection/anchor/scroll bookkeeping (A6 replace semantics);
    /// within an epoch, indices past the (possibly shrunken) set drop out.
    pub fn sync_set(&mut self, epoch: SetEpoch, total: usize) {
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.selected.clear();
            self.anchor = None;
            self.scrolled_to = None;
        }
        self.selected.retain(|&i| i < total);
        if self.anchor.is_some_and(|a| a >= total) {
            self.anchor = None;
        }
    }

    /// Click selection semantics (B4): plain = single select + anchor;
    /// ⇧ = anchor..=idx range; ⌘ (Ctrl on Win/Linux) = toggle membership.
    /// Every click also activates — batch ops (M2) read `selected()`.
    fn click(&mut self, idx: usize, total: usize, mods: egui::Modifiers) {
        if idx >= total {
            return;
        }
        if mods.shift {
            let a = self.anchor.unwrap_or(idx).min(total - 1);
            let (lo, hi) = (a.min(idx), a.max(idx));
            self.selected = (lo..=hi).collect();
        } else if mods.command {
            if !self.selected.insert(idx) {
                self.selected.remove(&idx);
            }
            self.anchor = Some(idx);
        } else {
            self.selected.clear();
            self.selected.insert(idx);
            self.anchor = Some(idx);
        }
    }
}

/// B4 `nav.next`/`nav.prev`: net arrow-key steps this frame, counted per
/// event so holding the key repeats (one step per OS key-repeat). Text
/// focus suppresses it — the same `egui_wants_keyboard_input` guard the
/// loupe's T26 keys use; the caller gates on the loupe *not* being mounted
/// so exactly one component acts per press.
///
/// TODO(E08 Phase D): both this and the loupe's arrow handling collapse
/// into the keymap registry's `nav.next`/`nav.prev` actions (repeatable).
pub fn nav_delta(ctx: &egui::Context) -> isize {
    if ctx.egui_wants_keyboard_input() {
        return 0;
    }
    ctx.input(|i| {
        i.events
            .iter()
            .map(|ev| match ev {
                egui::Event::Key {
                    key: egui::Key::ArrowRight,
                    pressed: true,
                    ..
                } => 1,
                egui::Event::Key {
                    key: egui::Key::ArrowLeft,
                    pressed: true,
                    ..
                } => -1,
                _ => 0,
            })
            .sum()
    })
}

// ─── B2/B3/B5: rendering ────────────────────────────────────────────────────

/// The overflow position indicator ("34/212", spec §8 B5) — 1-based active
/// position over the set size.
fn position_indicator(active: Option<usize>, total: usize) -> String {
    match active {
        Some(i) => format!("{}/{}", i + 1, total),
        None => format!("–/{total}"),
    }
}

/// Renders the strip; reuses [`ThumbCache`] verbatim (`want`/cancel-on-
/// scroll-out is driven by the caller's `end_frame`, exactly like the
/// retired grid). `cell_of` is invoked **only** for visible indices (the
/// B2 virtualization AC); `visible_out` receives the `ImageId`s
/// materialized this frame. Cell size derives from the strip's height, so
/// the B5 drag-resize needs no extra plumbing.
pub fn filmstrip_ui(
    ui: &mut egui::Ui,
    state: &mut FilmstripState,
    total: usize,
    active: Option<usize>,
    cell_of: &dyn Fn(usize) -> StripCell,
    thumbs: &mut ThumbCache,
    visible_out: &mut HashSet<ImageId>,
) -> Option<FilmstripAction> {
    let mut action = None;
    if total == 0 {
        ui.centered_and_justified(|ui| {
            ui.weak("no images open");
        });
        return None;
    }

    let strip_rect = ui.available_rect_before_wrap();
    let cell_size = (strip_rect.height() - LABEL_H - 6.0).clamp(MIN_CELL, MAX_HEIGHT_PT);
    let ppp = ui.ctx().pixels_per_point();
    let bucket = bucket_for((cell_size * ppp).ceil() as u32);

    // B4: a plain vertical wheel scrolls the (only-direction) strip.
    ui.style_mut().always_scroll_the_only_direction = true;

    egui::ScrollArea::horizontal()
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            let l = strip_layout(cell_size, total);
            ui.set_width(l.total_w);
            let origin = ui.min_rect().min;

            // B4 scroll-to-active: exactly once per activation change, and
            // instant (nav-swap latency budget — animation would drag the
            // strip behind rapid ←/→ repeats).
            if active != state.scrolled_to {
                if let Some(idx) = active.filter(|&i| i < total) {
                    let target = egui::Rect::from_min_size(
                        origin + egui::vec2(idx as f32 * l.cell_w, 0.0),
                        egui::vec2(l.cell, l.cell + LABEL_H),
                    );
                    ui.scroll_to_rect_animation(
                        target,
                        Some(egui::Align::Center),
                        egui::style::ScrollAnimation::none(),
                    );
                }
                state.scrolled_to = active;
            }

            let range = visible_range(&l, total, viewport.min.x, viewport.max.x);
            for idx in range {
                // Materialized ONLY for visible indices (B2 AC).
                let cell = cell_of(idx);
                if let CellStatus::Ready(image) = cell.status {
                    visible_out.insert(image);
                    thumbs.want(image, bucket);
                }

                let cell_rect = egui::Rect::from_min_size(
                    origin + egui::vec2(idx as f32 * l.cell_w, 0.0),
                    egui::vec2(l.cell, l.cell + LABEL_H),
                );
                let response = ui.interact(
                    cell_rect,
                    ui.id().with(("filmstrip-cell", idx)),
                    egui::Sense::click(),
                );
                let is_active = active == Some(idx);
                // H1 seam: the cell's accessible name carries the §6.3
                // badge state — also what the B3 structural tests query.
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        accessible_label(&cell, is_active),
                    )
                });
                if response.clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    state.click(idx, total, mods);
                    action = Some(FilmstripAction::Activate(idx));
                }

                draw_cell(
                    ui,
                    cell_rect,
                    &cell,
                    is_active,
                    state.selected.contains(&idx),
                    thumbs,
                    l.cell,
                    &response,
                );
            }
        });

    // B5 overlay chrome (top-right): overflow indicator + collapse toggle.
    let pad = 4.0;
    let btn_size = egui::vec2(20.0, 15.0);
    let btn_rect = egui::Rect::from_min_size(
        egui::pos2(
            strip_rect.right() - btn_size.x - pad,
            strip_rect.top() + pad,
        ),
        btn_size,
    );
    let indicator = position_indicator(active, total);
    let ind_size = egui::vec2(10.0 + 7.0 * indicator.len() as f32, 15.0);
    let ind_rect = egui::Rect::from_min_size(
        egui::pos2(btn_rect.left() - ind_size.x - pad, strip_rect.top() + pad),
        ind_size,
    );
    ui.painter().rect_filled(
        ind_rect.union(btn_rect).expand(2.0),
        4.0,
        ui.visuals().extreme_bg_color.gamma_multiply(0.85),
    );
    ui.put(
        ind_rect,
        egui::Label::new(egui::RichText::new(indicator).small()).selectable(false),
    );
    if ui
        .put(btn_rect, egui::Button::new("⏷").small())
        .on_hover_text("Collapse the filmstrip")
        .clicked()
    {
        state.set_collapsed(true);
    }

    action
}

/// The collapsed strip (B5): a slim bar with the expand affordance and the
/// overflow position indicator — mounted by the app instead of
/// [`filmstrip_ui`] while [`FilmstripState::collapsed`] holds.
pub fn collapsed_bar_ui(
    ui: &mut egui::Ui,
    state: &mut FilmstripState,
    active: Option<usize>,
    total: usize,
) {
    ui.horizontal(|ui| {
        if ui
            .small_button("⏶")
            .on_hover_text("Expand the filmstrip")
            .clicked()
        {
            state.set_collapsed(false);
        }
        ui.weak("Filmstrip");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(position_indicator(active, total)).small());
        });
    });
}

/// The cell's accessible name (AccessKit; H1 seam) — filename plus the §6.3
/// badge state, so structural tests and screen readers see the same chrome
/// the painter draws.
fn accessible_label(cell: &StripCell, is_active: bool) -> String {
    let mut parts = vec![cell.filename.clone()];
    if cell.raw {
        parts.push("RAW".to_owned());
    }
    if cell.is_edited {
        parts.push("edited".to_owned());
    }
    match &cell.status {
        CellStatus::Loading => parts.push("loading".to_owned()),
        CellStatus::Failed { reason } => parts.push(format!("failed: {reason}")),
        CellStatus::Duplicate { of } => parts.push(format!("duplicate of #{}", of + 1)),
        CellStatus::Ready(_) => {}
    }
    if is_active {
        parts.push("active".to_owned());
    }
    parts.join(" · ")
}

/// Paints one cell: active/selected ring, thumbnail (or Loading shimmer /
/// Failed placard), §6.3 badges (RAW tag, edited dot, error/duplicate),
/// clipped filename label, hover tooltip with the failure reason.
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    ui: &egui::Ui,
    cell_rect: egui::Rect,
    cell: &StripCell,
    is_active: bool,
    is_selected: bool,
    thumbs: &mut ThumbCache,
    cell_size: f32,
    response: &egui::Response,
) {
    // Clip to (just past) the cell so long filenames never bleed into the
    // neighbor cells.
    let painter = ui.painter().with_clip_rect(cell_rect.expand(2.0));
    let visuals = ui.visuals();

    painter.rect_filled(cell_rect, 3.0, visuals.extreme_bg_color);
    if is_active {
        painter.rect_stroke(
            cell_rect,
            3.0,
            visuals.selection.stroke,
            egui::StrokeKind::Outside,
        );
    } else if is_selected {
        // Multi-selected but not active: a fainter ring.
        let mut stroke = visuals.selection.stroke;
        stroke.color = stroke.color.gamma_multiply(0.5);
        painter.rect_stroke(cell_rect, 3.0, stroke, egui::StrokeKind::Outside);
    }

    let image_rect =
        egui::Rect::from_min_size(cell_rect.min, egui::vec2(cell_size, cell_size)).shrink(3.0);
    let mut hover: Vec<String> = vec![cell.filename.clone()];
    if cell.raw {
        hover.push("RAW".to_owned());
    }
    if cell.is_edited {
        hover.push("edited".to_owned());
    }
    match &cell.status {
        CellStatus::Ready(image) => match thumbs.state(*image) {
            ThumbState::Ready(tex, size) => {
                let fitted = fit_rect(image_rect, [size[0] as f32, size[1] as f32]);
                painter.image(
                    tex.id(),
                    fitted,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            ThumbState::Pending => draw_shimmer(&painter, image_rect, ui, visuals),
            ThumbState::Failed(reason) => {
                hover.push(format!("preview: {reason}"));
                painter.text(
                    image_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "no preview",
                    egui::FontId::proportional(12.0),
                    visuals.weak_text_color(),
                );
            }
        },
        CellStatus::Loading => draw_shimmer(&painter, image_rect, ui, visuals),
        CellStatus::Failed { reason } => {
            hover.push(reason.clone());
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "!",
                egui::FontId::proportional(16.0),
                egui::Color32::from_rgb(200, 60, 50),
            );
        }
        CellStatus::Duplicate { of } => {
            hover.push(format!("duplicate of #{}", of + 1));
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                "dup",
                egui::FontId::proportional(12.0),
                visuals.weak_text_color(),
            );
        }
    }

    // B3 badges — painted over the image area.
    if cell.raw {
        let tag_rect = egui::Rect::from_min_size(
            image_rect.min + egui::vec2(2.0, 2.0),
            egui::vec2(24.0, 11.0),
        );
        painter.rect_filled(tag_rect, 2.0, egui::Color32::from_black_alpha(160));
        painter.text(
            tag_rect.center(),
            egui::Align2::CENTER_CENTER,
            "RAW",
            egui::FontId::proportional(8.0),
            egui::Color32::from_gray(220),
        );
    }
    if cell.is_edited {
        let center = egui::pos2(image_rect.right() - 6.0, image_rect.top() + 6.0);
        painter.circle_filled(center, 3.0, visuals.selection.stroke.color);
        painter.circle_stroke(
            center,
            3.0,
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120)),
        );
    }

    let label_pos = egui::pos2(cell_rect.center().x, cell_rect.bottom() - 1.0);
    painter.text(
        label_pos,
        egui::Align2::CENTER_BOTTOM,
        &cell.filename,
        egui::FontId::proportional(10.0),
        visuals.text_color(),
    );

    response.clone().on_hover_text(hover.join("\n"));
}

/// The Loading shimmer: a slow luminance pulse over the image area (repaint
/// is already driven by the busy/in-flight repaint policy in `lib.rs`).
fn draw_shimmer(painter: &egui::Painter, rect: egui::Rect, ui: &egui::Ui, visuals: &egui::Visuals) {
    let t = ui.input(|i| i.time);
    let pulse = ((t * std::f64::consts::TAU / 1.4).sin() * 0.5 + 0.5) as f32;
    painter.rect_filled(
        rect,
        3.0,
        visuals
            .weak_text_color()
            .gamma_multiply(0.10 + 0.15 * pulse),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── B1: geometry (mirrors the retired grid's layout tests, horizontal
    //    axis) ────────────────────────────────────────────────────────────

    #[test]
    fn strip_layout_pitch_and_total_width() {
        let l = strip_layout(100.0, 10);
        assert_eq!(l.cell_w, 106.0);
        assert_eq!(l.total_w, 1060.0);
    }

    /// The horizontal analog of the grid's 1-column clamp: an empty model
    /// yields zero content width and an empty visible range — never a
    /// negative or degenerate window.
    #[test]
    fn empty_strip_collapses_to_zero_width() {
        let l = strip_layout(100.0, 0);
        assert_eq!(l.total_w, 0.0);
        assert!(visible_range(&l, 0, 0.0, 1000.0).is_empty());
    }

    #[test]
    fn visible_range_windows_the_scroll_position() {
        let l = strip_layout(100.0, 1000); // pitch 106
        let r = visible_range(&l, 1000, 0.0, 500.0);
        assert_eq!(r.start, 0);
        assert!(r.end >= 4 && r.end <= 6, "roughly 4-5 cells visible: {r:?}");

        let r2 = visible_range(&l, 1000, 1000.0 * 106.0, 1005.0 * 106.0);
        assert_eq!(r2.start, 1000, "clamped to total");
        assert_eq!(r2.end, 1000);
    }

    /// Window arithmetic: a viewport starting mid-cell still includes that
    /// (partially visible) cell on both edges.
    #[test]
    fn visible_range_includes_partially_visible_cells() {
        let l = strip_layout(100.0, 100); // pitch 106
        let r = visible_range(&l, 100, 150.0, 350.0);
        assert_eq!(r.start, 1, "cell 1 spans 106..206 — half-visible at 150");
        assert_eq!(r.end, 4, "cell 3 spans 318..418 — clipped at 350");
    }

    #[test]
    fn visible_range_clamps_at_the_ends() {
        let l = strip_layout(100.0, 5);
        let r = visible_range(&l, 5, 0.0, 100_000.0);
        assert_eq!(r, 0..5, "clamped to the model length");
        let r = visible_range(&l, 0, 0.0, 100.0);
        assert!(r.is_empty());
        // Scrolled above/before the content (elastic overscroll).
        let r = visible_range(&l, 5, -50.0, -10.0);
        assert!(r.is_empty());
    }

    /// Overscroll/degenerate-pitch guard: a zero or negative cell width can
    /// never divide-by-zero or produce a bogus range.
    #[test]
    fn visible_range_guards_degenerate_pitch() {
        let l = StripLayout {
            cell: 0.0,
            cell_w: 0.0,
            total_w: 0.0,
        };
        assert!(visible_range(&l, 10, 0.0, 500.0).is_empty());
    }

    #[test]
    fn fit_rect_preserves_aspect_and_never_upscales() {
        let container = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let r = fit_rect(container, [200.0, 100.0]);
        assert!((r.width() - 100.0).abs() < 0.01);
        assert!((r.height() - 50.0).abs() < 0.01);
        let r = fit_rect(container, [10.0, 20.0]);
        assert!((r.width() - 10.0).abs() < 0.01);
        assert!((r.height() - 20.0).abs() < 0.01);
        assert_eq!(r.center(), container.center());
    }

    // ── B4/B5: state-machine units ────────────────────────────────────────

    #[test]
    fn click_selection_semantics() {
        let mods = |shift: bool, command: bool| egui::Modifiers {
            shift,
            command,
            ..Default::default()
        };
        let mut s = FilmstripState::new();
        s.click(2, 10, mods(false, false));
        assert_eq!(s.selected().iter().copied().collect::<Vec<_>>(), [2]);

        // ⇧: anchor..=idx range.
        s.click(5, 10, mods(true, false));
        assert_eq!(
            s.selected().iter().copied().collect::<Vec<_>>(),
            [2, 3, 4, 5]
        );

        // ⌘: toggle membership.
        s.click(3, 10, mods(false, true));
        assert_eq!(s.selected().iter().copied().collect::<Vec<_>>(), [2, 4, 5]);
        s.click(7, 10, mods(false, true));
        assert!(s.selected().contains(&7));

        // Plain click resets to a single selection.
        s.click(1, 10, mods(false, false));
        assert_eq!(s.selected().iter().copied().collect::<Vec<_>>(), [1]);

        // Out-of-range clicks are ignored.
        s.click(99, 10, mods(false, false));
        assert_eq!(s.selected().iter().copied().collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn sync_set_clears_selection_on_epoch_change_and_clamps_within_epoch() {
        let mut s = FilmstripState::new();
        s.sync_set(1, 10);
        s.click(4, 10, egui::Modifiers::default());
        s.click(
            8,
            10,
            egui::Modifiers {
                command: true,
                ..Default::default()
            },
        );
        assert_eq!(s.selected().len(), 2);

        // Same epoch, shrunken set: out-of-range indices drop out.
        s.sync_set(1, 5);
        assert_eq!(s.selected().iter().copied().collect::<Vec<_>>(), [4]);

        // New epoch (A6 replace): everything clears.
        s.sync_set(2, 5);
        assert!(s.selected().is_empty());
    }

    #[test]
    fn height_clamps_to_the_b5_spec_range() {
        let mut s = FilmstripState::new();
        assert_eq!(s.height_pt(), DEFAULT_HEIGHT_PT);
        s.set_height_pt(300.0);
        assert_eq!(s.height_pt(), MAX_HEIGHT_PT);
        s.set_height_pt(10.0);
        assert_eq!(s.height_pt(), MIN_HEIGHT_PT);
        s.set_height_pt(120.0);
        assert_eq!(s.height_pt(), 120.0);
    }

    #[test]
    fn position_indicator_is_one_based() {
        assert_eq!(position_indicator(Some(33), 212), "34/212");
        assert_eq!(position_indicator(None, 7), "–/7");
    }

    // ── B2–B5 widget tests (egui_kittest, structural per spec §9) ─────────

    use egui_kittest::{kittest::Queryable, Harness};
    use lightbox_jobs::Class;
    use lightbox_preview::{PreviewClass, PreviewProvider, PreviewState, PreviewTicket};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// Never-completing provider: counts requests/cancels so the tests can
    /// assert the unchanged `want`/`end_frame` discipline. Thumbs staying
    /// `Pending` is fine — the §6.3 chrome under test is painter/AccessKit
    /// state, not pixels.
    #[derive(Default)]
    struct StubProvider {
        next: AtomicU64,
        requests: Mutex<Vec<ImageId>>,
        cancels: Mutex<Vec<u64>>,
    }

    impl PreviewProvider for StubProvider {
        fn request(&self, image: ImageId, _class: PreviewClass, _prio: Class) -> PreviewTicket {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            self.requests.lock().unwrap().push(image);
            PreviewTicket::new(id)
        }

        fn poll(&self, _t: &PreviewTicket) -> PreviewState {
            PreviewState::Pending
        }

        fn cancel(&self, t: &PreviewTicket) {
            self.cancels.lock().unwrap().push(t.id());
        }
    }

    /// The test app: mirrors `lib.rs`'s frame loop (nav → strip/collapsed
    /// bar → activation → `end_frame`) over synthetic cells.
    struct StripApp {
        provider: Arc<StubProvider>,
        state: FilmstripState,
        thumbs: ThumbCache,
        cells: Vec<StripCell>,
        active: Option<usize>,
        /// `cell_of` invocations last frame (B2 virtualization assertion).
        materialized_last_frame: Cell<usize>,
        visible_last_frame: HashSet<ImageId>,
    }

    fn ready_cells(n: usize) -> Vec<StripCell> {
        (0..n)
            .map(|i| StripCell {
                filename: format!("IMG_{i:04}.jpg"),
                raw: false,
                is_edited: false,
                status: CellStatus::Ready(ImageId(i as i64 + 1)),
            })
            .collect()
    }

    fn strip_harness(cells: Vec<StripCell>, active: Option<usize>) -> Harness<'static, StripApp> {
        let provider = Arc::new(StubProvider::default());
        let app = StripApp {
            thumbs: ThumbCache::new(Arc::clone(&provider) as Arc<dyn PreviewProvider>),
            provider,
            state: FilmstripState::new(),
            cells,
            active,
            materialized_last_frame: Cell::new(0),
            visible_last_frame: HashSet::new(),
        };
        let mut harness = Harness::new_ui_state(
            |ui, app: &mut StripApp| {
                // B4 nav — the same clamped stepping `WorkingSetView::nav`
                // applies in the real app.
                let d = nav_delta(ui.ctx());
                if d != 0 && !app.cells.is_empty() {
                    let cur = app.active.unwrap_or(0) as isize;
                    app.active = Some((cur + d).clamp(0, app.cells.len() as isize - 1) as usize);
                }

                if app.state.collapsed() {
                    let total = app.cells.len();
                    collapsed_bar_ui(ui, &mut app.state, app.active, total);
                    return;
                }

                let count = Cell::new(0usize);
                let mut visible = HashSet::new();
                let action = {
                    let cells = &app.cells;
                    let cell_of = |i: usize| {
                        count.set(count.get() + 1);
                        cells[i].clone()
                    };
                    filmstrip_ui(
                        ui,
                        &mut app.state,
                        cells.len(),
                        app.active,
                        &cell_of,
                        &mut app.thumbs,
                        &mut visible,
                    )
                };
                if let Some(FilmstripAction::Activate(idx)) = action {
                    app.active = Some(idx);
                }
                // Mirrors lib.rs: cancel-on-scroll-out + O(visible) cap.
                let cap = (visible.len() * 3).max(64);
                app.thumbs.end_frame(&visible, cap);
                app.materialized_last_frame.set(count.get());
                app.visible_last_frame = visible;
            },
            app,
        );
        harness.set_size(egui::vec2(800.0, 110.0));
        harness
    }

    /// B2 AC: only visible cells materialize — the lazy accessor is invoked
    /// for the visible window, never the whole 200-entry set; thumb
    /// requests match the visible set.
    #[test]
    fn only_visible_cells_materialize() {
        let mut harness = strip_harness(ready_cells(200), Some(0));
        harness.run();

        let app = harness.state();
        let materialized = app.materialized_last_frame.get();
        assert!(materialized > 0, "something rendered");
        assert!(
            materialized <= 30,
            "an 800pt viewport must not materialize 200 cells (got {materialized})"
        );
        assert_eq!(app.visible_last_frame.len(), materialized);
        assert!(
            app.thumbs.stats().inflight <= materialized,
            "thumb demand stays bounded by the visible set"
        );
        // The window is the strip's left edge (active = 0).
        assert!(app.visible_last_frame.contains(&ImageId(1)));
        assert!(!app.visible_last_frame.contains(&ImageId(100)));
    }

    /// B2 AC: cells scrolling out have their in-flight thumb requests
    /// cancelled (the unchanged `end_frame` discipline), driven here by the
    /// B4 scroll-to-active jump.
    #[test]
    fn scroll_out_cancels_inflight_thumb_requests() {
        let mut harness = strip_harness(ready_cells(200), Some(0));
        harness.run();
        assert!(harness.state().thumbs.stats().inflight > 0);

        // Jump the activation to the far end: the strip scrolls there, the
        // left-edge cells leave the viewport, their requests cancel.
        harness.state_mut().active = Some(150);
        harness.run_steps(3);

        let app = harness.state();
        let stats = app.thumbs.stats();
        assert!(
            stats.cancelled_total > 0,
            "scroll-out must cancel in-flight thumbs (stats: {stats:?})"
        );
        assert_eq!(
            stats.cancelled_total as usize,
            app.provider.cancels.lock().unwrap().len(),
            "cancellations reached the provider"
        );
        assert!(
            app.visible_last_frame.contains(&ImageId(151)),
            "the strip scrolled to the newly active cell"
        );
        assert!(!app.visible_last_frame.contains(&ImageId(1)));
    }

    /// B2 AC: clicking a cell activates that entry.
    #[test]
    fn click_activates_the_entry() {
        let mut harness = strip_harness(ready_cells(8), Some(0));
        harness.run();
        harness.get_by_label_contains("IMG_0003").click();
        harness.run();

        let app = harness.state();
        assert_eq!(app.active, Some(3), "click activated the entry");
        assert_eq!(
            app.state.selected().iter().copied().collect::<Vec<_>>(),
            [3],
            "plain click resets the selection to the clicked cell"
        );
    }

    /// B4: ⌘-click toggles membership through the real modifier plumbing
    /// (the pure semantics are covered in `click_selection_semantics`).
    #[test]
    fn command_click_extends_the_selection() {
        let mut harness = strip_harness(ready_cells(8), Some(0));
        harness.run();
        harness.get_by_label_contains("IMG_0002").click();
        harness.run();
        harness
            .get_by_label_contains("IMG_0005")
            .click_modifiers(egui::Modifiers::COMMAND);
        harness.run();

        let app = harness.state();
        assert_eq!(
            app.state.selected().iter().copied().collect::<Vec<_>>(),
            [2, 5],
            "⌘-click adds to the selection"
        );
    }

    /// B3 AC: a mixed set (raw ready / JPEG ready+edited / failed /
    /// loading) exposes the §6.3 chrome — asserted structurally via the
    /// cells' AccessKit names (spec §9: E08's visual tests are structural,
    /// immune to render-backend drift), and NO star/flag/rating glyph
    /// exists anywhere in the tree (the library rating UI is retired).
    #[test]
    fn mixed_set_shows_badges_and_no_rating_chrome() {
        let cells = vec![
            StripCell {
                filename: "IMG_0001.CR3".to_owned(),
                raw: true,
                is_edited: false,
                status: CellStatus::Ready(ImageId(1)),
            },
            StripCell {
                filename: "beach.jpg".to_owned(),
                raw: false,
                is_edited: true,
                status: CellStatus::Ready(ImageId(2)),
            },
            StripCell {
                filename: "broken.jpg".to_owned(),
                raw: false,
                is_edited: false,
                status: CellStatus::Failed {
                    reason: "unsupported codec".to_owned(),
                },
            },
            StripCell {
                filename: "slow.jpg".to_owned(),
                raw: false,
                is_edited: false,
                status: CellStatus::Loading,
            },
        ];
        let mut harness = strip_harness(cells, Some(0));
        harness.run();

        harness.get_by_label_contains("IMG_0001.CR3 · RAW");
        harness.get_by_label_contains("beach.jpg · edited");
        harness.get_by_label_contains("broken.jpg · failed: unsupported codec");
        harness.get_by_label_contains("slow.jpg · loading");

        for retired in ["star", "flag", "rating", "★", "⚑", "☆"] {
            assert_eq!(
                harness.query_all_by_label_contains(retired).count(),
                0,
                "retired rating chrome {retired:?} must not exist (spec §2.2/§6.3)"
            );
        }
    }

    /// B4 AC: arrow-key nav through a 200-entry synthetic set stays
    /// virtualized (per-frame materialization bounded by the viewport, not
    /// the set) and does not regress badly on wall-clock frame cost. The
    /// formal p95 budget gate (< 16 ms frame / < 50 ms nav-swap on the dev
    /// baseline, release profile) is H2's perf harness — this is the
    /// regression tripwire, with debug-build headroom.
    #[test]
    fn arrow_nav_through_200_entries_stays_virtualized() {
        let mut harness = strip_harness(ready_cells(200), Some(0));
        harness.run();

        let mut frame_ms: Vec<f32> = Vec::with_capacity(199);
        for _ in 0..199 {
            harness.key_press(egui::Key::ArrowRight);
            let t0 = std::time::Instant::now();
            harness.step();
            frame_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
            let materialized = harness.state().materialized_last_frame.get();
            assert!(
                materialized <= 60,
                "nav must stay O(visible), materialized {materialized} cells"
            );
        }
        assert_eq!(harness.state().active, Some(199), "one step per press");

        // Let the final scroll settle, then confirm the window followed.
        harness.run_steps(3);
        assert!(harness.state().visible_last_frame.contains(&ImageId(200)));

        frame_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = frame_ms[((frame_ms.len() as f32 * 0.95) as usize).min(frame_ms.len() - 1)];
        assert!(
            p95 < 200.0,
            "nav frame p95 regressed badly: {p95:.2} ms in a headless debug harness \
             (formal 16 ms/50 ms release gates land with H2)"
        );
    }

    /// B5 AC: the collapse toggle works in-session (strip → bar → strip)
    /// and the overflow indicator is correct in both states.
    #[test]
    fn collapse_toggle_and_overflow_indicator() {
        let mut harness = strip_harness(ready_cells(212), Some(33));
        harness.run();
        harness.get_by_label_contains("34/212");

        harness.get_by_label("⏷").click();
        harness.run();
        assert!(harness.state().state.collapsed());
        harness.get_by_label_contains("34/212"); // indicator survives collapse
        assert_eq!(
            harness.query_all_by_label_contains("IMG_00").count(),
            0,
            "no cells materialize while collapsed"
        );

        harness.get_by_label("⏶").click();
        harness.run();
        assert!(!harness.state().state.collapsed());
        harness.get_by_label_contains("IMG_0033");
    }

    // ── B3 plumbing: EditedBadges over a real headless session ────────────

    use lightbox_core::{
        ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, OpenOrigin, OpenRequest,
        ParamDelta, ParamId, ParamValue, StepLabel,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const EVENT_TIMEOUT: Duration = Duration::from_secs(60);
    const TINY_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

    fn stage_unique_jpeg(dir: &Path, name: &str, pad: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let mut bytes = TINY_JPEG.to_vec();
        bytes.extend_from_slice(pad.as_bytes());
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn wait_for_event(
        rx: &mut tokio::sync::broadcast::Receiver<Event>,
        mut pred: impl FnMut(&Event) -> bool,
    ) {
        let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for an event"
            );
            match rx.try_recv() {
                Ok(event) => {
                    if pred(&event) {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    panic!("event channel closed while waiting")
                }
            }
        }
    }

    /// B3 AC plumbing: the edited dot's source of truth. An untouched set
    /// has no edited badges; committing one real E09 gesture flips exactly
    /// that image after a dirty-refresh; the refresh is a no-op while
    /// clean (no per-frame SQL — §7).
    #[test]
    fn edited_badges_follow_the_edit_index_projection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Core::start(CoreConfig::default()).expect("core start");
        let session = core
            .create_catalog(&tmp.path().join("badges.lbdata"), None)
            .expect("create catalog");
        let photos = tmp.path().join("photos");
        let a = stage_unique_jpeg(&photos, "a.jpg", "a");
        let b = stage_unique_jpeg(&photos, "b.jpg", "b");

        let mut rx = session.events();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(vec![a, b], false, OpenOrigin::Cli),
        });
        wait_for_event(&mut rx, |ev| {
            matches!(ev, Event::WorkingSetLoadFinished { .. })
        });

        let snapshot = session.working_set();
        let ids: Vec<ImageId> = snapshot
            .items
            .iter()
            .filter_map(|it| match it.state {
                ItemState::Ready { image, .. } => Some(image),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);

        let mut badges = EditedBadges::new();
        badges.refresh_if_dirty(&session, &snapshot.items);
        assert!(!badges.is_edited(ids[0]), "untouched image has no badge");
        assert!(!badges.is_edited(ids[1]));

        // One real E09 gesture on the first image.
        let hub = session.edits();
        hub.open(ids[0]).expect("edit-open");
        hub.begin_gesture(ids[0], StepLabel::Param(ParamId::Exposure))
            .expect("begin_gesture");
        let mut delta = ParamDelta::new();
        delta.0.insert(ParamId::Exposure, ParamValue::F32(1.5));
        hub.update_gesture(ids[0], delta).expect("update_gesture");
        session.submit(Command::Edit(EditCommand::CommitGesture { image: ids[0] }));
        wait_for_event(
            &mut rx,
            |ev| matches!(ev, Event::EditCommitted { image, .. } if *image == ids[0]),
        );

        // Clean cache: refresh is a no-op until marked dirty (the event
        // handler's job in lib.rs).
        badges.refresh_if_dirty(&session, &snapshot.items);
        assert!(
            !badges.is_edited(ids[0]),
            "refresh without a dirty mark must not re-query"
        );

        badges.mark_dirty();
        badges.refresh_if_dirty(&session, &snapshot.items);
        assert!(badges.is_edited(ids[0]), "committed edit flips the badge");
        assert!(!badges.is_edited(ids[1]), "the other image stays untouched");

        session
            .close(ClosePolicy::Skip.into())
            .expect("clean close");
    }
}
