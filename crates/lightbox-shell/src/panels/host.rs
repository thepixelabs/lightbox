// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E1, the develop-panel host (spec §6.5): the [`PanelDef`] registry and
//! the dock renderer (`dock_ui`): collapsible sections, per-panel
//! open state, panel-solo mode, and rail scroll.
//!
//! ## Docking (multi-column rails, drag/drop/snap)
//!
//! Panels are not nailed to the right-hand rail. Every registered panel
//! history and presets included, is one **dockable** section, and where
//! it sits is [`crate::panels::layout::DockLayout`]: two rails (left and
//! right), each an ordered list of columns, each column an ordered stack
//! of panels with its own width. The shipped default is the pre-docking
//! rail exactly as it was: one right-hand column holding everything, in
//! registration order.
//!
//! * **Drag**, the header band is the handle (`Sense::click_and_drag`, so
//!   a click still expands/collapses). A drag publishes a [`PanelDrag`]
//!   payload through egui's `DragAndDrop`; [`PanelHost::finish_dnd`]
//!   called once per frame, after both rails have painted, hit-tests the
//!   pointer against this frame's geometry, paints the snap preview and
//!   the cursor ghost, and applies the move on release.
//! * **Snap zones**, inside a column: an insertion bar between sections.
//!   Within [`NEW_COLUMN_EDGE_PT`] of a rail's window edge, or in the
//!   [`CANVAS_DROP_STRIP_PT`] strip flanking the canvas: a new column
//!   (that strip is how the empty left rail gets its first one). Anywhere
//!   else, over the canvas, the filmstrip, the top bar, the release is a
//!   cancel, and `Esc` aborts mid-drag (egui's own `DragAndDrop` plugin).
//! * **Resize**, each rail paints its own splitters: the seam between two
//!   columns moves width from one to the other, and the rail's
//!   canvas-facing edge resizes the whole rail. Both clamp to the column
//!   band, and the canvas is never squeezed below
//!   [`crate::panels::layout::MIN_CANVAS_PT`]. That is why `lib.rs` shows
//!   both rails as `exact_size`, non-resizable panels: egui's own resize
//!   handle only understands one edge, and there can now be several.
//! * **Parity**, every move is also a header right-click command
//!   (`header_context_menu`), so the arrangement is reachable without a
//!   pointer drag, and "Reset panel layout" is in the rail's own Layout
//!   menu.
//!
//! **Persistence** ([`PanelHost::snapshot`]/[`PanelHost::restore`]): the
//! layout, the collapsed sections and the solo flag ride machine-scope
//! prefs (`MachinePrefs::panel_layout`). `lib.rs` restores after every
//! panel has registered and writes back whenever
//! [`PanelHost::take_dirty`] reports a change, one write per gesture (a
//! splitter drag writes on release, not per frame).
//!
//! **E2, SourceKind adaptivity (spec §4):** `dock_ui` filters
//! [`SourceReq::RawOnly`] panels out **entirely** (absent, never
//! disabled-but-visible) when the active entry's `SourceKind` is
//! `Rendered`. Open/solo state is keyed by [`PanelId`], never by file
//! so switching the active file between a raw and a JPEG re-filters the
//! rail without losing the user's layout (§4: "open/collapse state
//! preserved per `PanelId`").
//!
//! **How E10/E11/E12 add a panel:** `host.register(PanelDef { .. })` with a
//! fresh dotted id (duplicate ids panic at registration, mirroring
//! `KeymapRegistry::register`) and an `order` that slots between the E08
//! panels (basic = 20, curve = 30, history = 90, E10's histogram slot
//! above basic is < 20 by design, spec §2.3). `order` seeds the DEFAULT
//! dock layout; once a user has dragged panels around, their column order
//! wins and a newly registered panel is appended to the last right-hand
//! column (see [`crate::panels::layout::DockLayout::sync_registered`]).
//!
//! **Chrome re-skin (theme spec §2, §5, §9):** `dock_ui` no longer builds
//! on `egui::CollapsingHeader`, that widget gave AccessKit semantics for
//! free but had zero styling hooks for the gradient header band, the
//! procedural disclosure triangle, or the solo/active accent treatment
//! the design spec calls for. `panel_header_ui` reproduces the header by
//! hand (gradient band, triangle, UPPERCASE+tracking label, hairline) and
//! re-emits the exact AccessKit node `CollapsingHeader` used to provide
//! (`WidgetType::CollapsingHeader` → `Role::Button`), keyed to the
//! **source** `PanelDef.title` (mixed case) even though the label paints
//! UPPERCASE, the accessible name is never the paint-time transform.
//! Panel-solo/active state additionally rides the same `WidgetInfo` call
//! as an accesskit `Toggled` flag (`WidgetInfo::selected`'s `selected`
//! parameter maps to `set_toggled`, not `set_selected`, in this egui
//! version, see `panel_header_ui`'s doc comment), which doubles as this
//! module's headless test hook for a visual-only property (§2's 2px
//! accent bar + wash) a paint-free harness can't otherwise assert.
//!
//! ## Width containment (why `dock_ui` reserves its own rect)
//!
//! A develop panel whose body wants more width than the rail has used to
//! **erase every left-aligned thing in the rail**. The chain, start to
//! finish:
//!
//! 1. A body overflows horizontally (the HSL band-tab row wanted 637pt in
//!    a 456pt rail). `Region::expand_to_include_rect` grows a `Ui`'s
//!    `max_rect`, not just its `min_rect`, so the overflow propagates up
//!    through every enclosing `Ui`, and widens the budget every LATER
//!    sibling sees, so the row never wraps and the overflow compounds.
//! 2. `ScrollArea`'s non-scrolled axis is documented to follow its
//!    content: for `ScrollArea::vertical().auto_shrink([false, false])`
//!    the horizontal arm is `(false, false) =>
//!    inner_size.max(content_size)`, "expand to fit content" (egui 0.35
//!    `scroll_area.rs`). So the scroll area hands the overflow onward
//!    instead of absorbing it.
//! 3. `Panel::show_inside_dyn` takes its final rect from the content
//!    `Frame`'s rect, then clamps only its **size** to `size_range.max`
//!    while keeping the panel's fixed (right) edge, which for a right
//!    panel re-anchors the rect by moving its LEFT edge rightwards, past
//!    where the panel actually painted, and off the window on the right.
//! 4. `parent_ui.set_cursor` then hands `CentralPanel` everything left of
//!    that shifted edge, including the strip the rail really painted in.
//!    `CentralPanel` is shown last, so its canvas surround paints straight
//!    over the rail's left-hand column: headings, panel titles, disclosure
//!    triangles and slider labels all vanish, while right-aligned content
//!    (values, "Solo") survives. Measured at 1200x900: the rail laid out
//!    at x 720..1200 but reported `[905.6, 1385.6]`.
//! 5. The inflated rect is persisted into `PanelState`, so the next frame
//!    starts from it and the rail stays pinned at `size_range.max`.
//!
//! [`PanelHost::dock_ui`] breaks the chain at step 2 by reserving the
//! rail's rect before building into it. The containment is asserted by
//! `theme::gallery`'s `develop_rail_stays_inside_its_panel` (which
//! composes the rail exactly as `lib.rs` does, a `Panel::right` plus a
//! canvas painted afterwards, because the rail renders perfectly in
//! isolation and only misbehaves in that composition).

use std::collections::{HashMap, HashSet};

use eframe::egui;
use lightbox_core::PanelLayoutPrefs;
use lightbox_types::SourceKind;

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::layout::{
    DockLayout, DockSide, DropTarget, DEFAULT_COLUMN_PT, MAX_COLUMN_PT, MIN_CANVAS_PT,
    MIN_COLUMN_PT,
};
use crate::theme::{fonts, paint, tokens};

/// Stable panel identity ("develop.basic", "develop.curve", …), the key
/// for open/solo state (spec §6.5).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PanelId(pub &'static str);

/// Which sources a panel applies to (spec §2.4/§4: `RawOnly` panels are
/// **hidden**, never disabled, for `Rendered` sources).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SourceReq {
    /// Shown for every source kind.
    Any,
    /// Shown only when the active entry is `SourceKind::Raw`.
    RawOnly,
}

/// One registered develop panel (spec §6.5).
pub struct PanelDef {
    /// Stable identity (open-state key).
    pub id: PanelId,
    /// Section header title.
    pub title: &'static str,
    /// §4 source gating.
    pub source_req: SourceReq,
    /// Rail order, ascending; E10/E11 slot between E08 panels.
    pub order: u16,
    /// Renders the panel body (only called while the section is open).
    pub build: fn(&mut egui::Ui, &mut DevelopCtx<'_>),
}

/// How tall the rail's "Develop" heading row is (heading text + the
/// engraved seam under it), carved off the top of the primary rail before
/// the columns are laid out.
const HEADING_HEIGHT: f32 = 26.0;

/// Width of the draggable seam between two columns (and of a rail's
/// canvas-facing resize edge). Wide enough to grab, narrow enough to read
/// as a seam rather than a gutter.
///
/// `pub(crate)` for the theme gallery's pixel probes, which need to know
/// how far into a rail its first column actually starts.
pub(crate) const SPLITTER_PT: f32 = 6.0;

/// How close to a rail's window-edge the pointer must come, mid-drag, for
/// the drop to mean "start a new column out here" instead of "insert into
/// the column I'm over".
const NEW_COLUMN_EDGE_PT: f32 = 30.0;

/// Width of the drop strip along each side of the canvas, the zone that
/// docks a panel into a brand-new column on that side. Deliberately wider
/// than [`NEW_COLUMN_EDGE_PT`]: it is the only way to reach an empty rail,
/// so it must be easy to hit.
const CANVAS_DROP_STRIP_PT: f32 = 56.0;

/// The drag-and-drop payload: which panel is in flight. Set by
/// [`panel_header_ui`] on drag start, read (and taken) by
/// [`PanelHost::finish_dnd`].
#[derive(Clone, Copy, Debug)]
struct PanelDrag {
    id: PanelId,
    title: &'static str,
}

/// A layout mutation a widget asked for while `self.layout` was borrowed
/// for rendering, applied after the frame's UI is built, exactly like the
/// deferred open/close toggle.
#[derive(Copy, Clone, Debug)]
enum LayoutOp {
    /// Dock `id` at `target` (context-menu commands; the pointer drag goes
    /// straight through [`PanelHost::finish_dnd`]).
    Move { id: PanelId, target: DropTarget },
    /// Back to the shipped default: one right-hand column, every panel.
    Reset,
}

/// One column as it was actually laid out this frame, the input to the
/// drop hit-test.
struct ColumnGeom {
    side: DockSide,
    /// Index into `DockLayout::columns(side)` (columns are stored in
    /// visual order, so this is also the visual slot).
    index: usize,
    rect: egui::Rect,
    /// Each visible panel's full section rect (header + body), top to
    /// bottom, the insertion-point ladder.
    rows: Vec<egui::Rect>,
}

/// Everything the current frame painted that the drop hit-test needs.
/// Rebuilt per rail, per frame, by [`PanelHost::dock_ui`].
#[derive(Default)]
struct FrameGeom {
    /// `[left, right]` rail rects, `None` when that rail isn't shown.
    rails: [Option<egui::Rect>; 2],
    columns: Vec<ColumnGeom>,
}

impl FrameGeom {
    fn slot(side: DockSide) -> usize {
        match side {
            DockSide::Left => 0,
            DockSide::Right => 1,
        }
    }

    fn rail(&self, side: DockSide) -> Option<egui::Rect> {
        self.rails[FrameGeom::slot(side)]
    }

    /// Starts a rail's contribution to this frame: its rect replaces the
    /// previous frame's, and its columns are cleared for re-collection.
    fn begin_rail(&mut self, side: DockSide, rect: egui::Rect) {
        self.rails[FrameGeom::slot(side)] = Some(rect);
        self.columns.retain(|c| c.side != side);
    }

    /// Forgets a rail that isn't shown this frame (so a stale rect can't
    /// answer a hit-test).
    fn clear_rail(&mut self, side: DockSide) {
        self.rails[FrameGeom::slot(side)] = None;
        self.columns.retain(|c| c.side != side);
    }
}

/// One column's placement for this frame, computed before anything is
/// drawn so the splitters and the columns agree on the geometry.
struct PlannedColumn {
    /// Index into `DockLayout::columns(side)`.
    index: usize,
    rect: egui::Rect,
    /// The panels of that column that pass §4 source filtering.
    panels: Vec<PanelId>,
}

/// A draggable seam between two columns, or at a rail's canvas-facing edge.
struct PlannedSplitter {
    rect: egui::Rect,
    kind: SplitterKind,
}

#[derive(Copy, Clone)]
enum SplitterKind {
    /// The rail's canvas-facing edge: resizes `column`, so the whole rail
    /// grows or shrinks and the canvas takes the difference.
    Edge { column: usize },
    /// A seam between two neighbouring columns: width moves from one to
    /// the other, the rail's total stays put.
    Between { left: usize, right: usize },
}

/// The panel registry + dock renderer (spec §6.5, plus the docking model
/// described in this module's docs). One per app.
pub struct PanelHost {
    /// Sorted by (`order`, registration order), kept sorted at `register`.
    defs: Vec<PanelDef>,
    /// Per-panel open state, keyed by [`PanelId`] (persisted machine-scope
    /// through [`PanelHost::snapshot`]).
    open: HashMap<PanelId, bool>,
    /// Panel-solo mode: while on, opening one panel collapses the rest.
    solo: bool,
    /// Panels switched off in Preferences → Panels. Absent from the rails
    /// entirely, but still docked in `layout`, so switching one back on
    /// returns it to the column it came from.
    hidden: HashSet<PanelId>,
    /// Which rail/column each panel is docked in (`panels::layout`).
    layout: DockLayout,
    /// This frame's painted geometry, for the drop hit-test.
    geom: FrameGeom,
    /// Set when layout/solo/open state changed and `prefs.toml` needs a
    /// write; drained by [`PanelHost::take_dirty`].
    dirty: bool,
}

impl PanelHost {
    /// An empty host (panels register at app construction).
    pub fn new() -> PanelHost {
        PanelHost {
            defs: Vec::new(),
            open: HashMap::new(),
            solo: false,
            hidden: HashSet::new(),
            layout: DockLayout::default(),
            geom: FrameGeom::default(),
            dirty: false,
        }
    }

    /// Registers a panel. Panics on a duplicate [`PanelId`], a developer
    /// error, same posture as `KeymapRegistry::register`.
    ///
    /// The new panel is docked immediately (default layout: the last
    /// right-hand column), so a host is always renderable even if
    /// [`PanelHost::restore`] is never called, which is what the gallery
    /// and the unit tests rely on.
    pub fn register(&mut self, def: PanelDef) {
        assert!(
            !self.defs.iter().any(|d| d.id == def.id),
            "duplicate PanelId {:?} registered",
            def.id.0
        );
        // Stable insert: after every existing def with order <= new order.
        let at = self.defs.partition_point(|d| d.order <= def.order);
        self.defs.insert(at, def);
        let ids = self.panel_ids();
        self.layout.sync_registered(&ids);
    }

    /// Every registered panel id, in rail order.
    fn panel_ids(&self) -> Vec<PanelId> {
        self.defs.iter().map(|d| d.id).collect()
    }

    /// Whether `id`'s section is currently expanded (default: open).
    pub fn open_state(&self, id: PanelId) -> bool {
        *self.open.get(&id).unwrap_or(&true)
    }

    /// Panel-solo mode state. `#[allow(dead_code)]`: the app persists solo
    /// through [`PanelHost::snapshot`] rather than reading it directly, so
    /// this accessor exists for tests and for future callers.
    #[allow(dead_code)]
    pub fn solo(&self) -> bool {
        self.solo
    }

    /// The dock layout. `#[allow(dead_code)]`: same as
    /// [`PanelHost::solo`], the app persists via `snapshot`.
    #[allow(dead_code)]
    pub fn layout(&self) -> &DockLayout {
        &self.layout
    }

    /// Restores a persisted layout (machine prefs). Ids this build no
    /// longer registers are dropped and newly registered panels are
    /// appended, see [`DockLayout::sync_registered`].
    pub fn restore(&mut self, prefs: &PanelLayoutPrefs) {
        let ids = self.panel_ids();
        self.layout = DockLayout::from_prefs(prefs, &ids);
        self.solo = prefs.solo;
        self.open.clear();
        for name in &prefs.collapsed {
            if let Some(id) = ids.iter().copied().find(|p| p.0 == name.as_str()) {
                self.open.insert(id, false);
            }
        }
        self.hidden.clear();
        for name in &prefs.hidden {
            if let Some(id) = ids.iter().copied().find(|p| p.0 == name.as_str()) {
                self.hidden.insert(id);
            }
        }
        self.dirty = false;
    }

    /// The persistable snapshot of layout + solo + collapsed sections.
    pub fn snapshot(&self) -> PanelLayoutPrefs {
        let mut collapsed: Vec<String> = self
            .defs
            .iter()
            .filter(|d| !self.open_state(d.id))
            .map(|d| d.id.0.to_owned())
            .collect();
        collapsed.sort();
        let mut hidden: Vec<String> = self.hidden.iter().map(|id| id.0.to_owned()).collect();
        hidden.sort();
        self.layout.to_prefs(self.solo, collapsed, hidden)
    }

    /// Takes the "layout changed, please persist" flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Docks `id` at `target` (the context-menu commands and the pointer
    /// drop both land here). Returns whether the layout changed.
    pub fn move_panel(&mut self, id: PanelId, target: DropTarget) -> bool {
        let moved = self.layout.move_panel(id, target);
        self.dirty |= moved;
        moved
    }

    /// Back to the shipped default: every panel in one right-hand column,
    /// in registration order.
    pub fn reset_layout(&mut self) {
        self.layout = DockLayout::single_right_column(self.panel_ids());
        self.dirty = true;
    }

    /// Opens/collapses one panel. In solo mode, opening a panel collapses
    /// every other panel. Marks the arrangement dirty, so a collapsed
    /// section persists across a restart like everything else about the
    /// rails.
    pub fn set_open(&mut self, id: PanelId, open: bool) {
        if open && self.solo {
            for def in &self.defs {
                self.open.insert(def.id, false);
            }
        }
        self.open.insert(id, open);
        self.dirty = true;
    }

    /// Does `side` have anything to show for this source kind? A rail with
    /// no visible panels isn't rendered at all (§4: a `RawOnly` panel is
    /// absent, never disabled, so a column holding only raw panels
    /// disappears for a JPEG, and comes back for a raw).
    pub fn has_dock(&self, side: DockSide, kind: SourceKind) -> bool {
        self.layout
            .columns(side)
            .iter()
            .any(|c| c.panels.iter().any(|p| self.is_visible(*p, kind)))
    }

    /// The width `side`'s rail wants, splitter seams included, clamped so
    /// the two rails always leave [`MIN_CANVAS_PT`] of canvas in a window
    /// `screen_width` points wide.
    pub fn dock_width(&self, side: DockSide, kind: SourceKind, screen_width: f32) -> f32 {
        let want = |side: DockSide| -> f32 {
            let cols = self.visible_column_indices(side, kind);
            if cols.is_empty() {
                return 0.0;
            }
            let widths: f32 = cols
                .iter()
                .map(|i| self.layout.columns(side)[*i].width)
                .sum();
            widths + SPLITTER_PT * cols.len() as f32
        };
        let mine = want(side);
        if mine == 0.0 {
            return 0.0;
        }
        let budget = (screen_width - MIN_CANVAS_PT - want(side.other())).max(MIN_COLUMN_PT);
        mine.min(budget)
    }

    /// Whether a panel shows this frame: the user's Preferences switch
    /// first, then §4 source gating.
    fn is_visible(&self, id: PanelId, kind: SourceKind) -> bool {
        !self.hidden.contains(&id)
            && self
                .defs
                .iter()
                .find(|d| d.id == id)
                .is_some_and(|d| d.source_req != SourceReq::RawOnly || kind == SourceKind::Raw)
    }

    /// Every registered panel as `(id, title, raw_only)`, in rail order
    /// the Preferences → Panels list.
    pub fn registered(&self) -> Vec<(PanelId, &'static str, bool)> {
        self.defs
            .iter()
            .map(|d| (d.id, d.title, d.source_req == SourceReq::RawOnly))
            .collect()
    }

    /// Is this panel switched off in Preferences?
    pub fn is_hidden(&self, id: PanelId) -> bool {
        self.hidden.contains(&id)
    }

    /// Switches a panel off/on (Preferences → Panels). Hiding never
    /// touches the dock layout, so switching back on restores its place.
    pub fn set_hidden(&mut self, id: PanelId, hidden: bool) {
        let changed = if hidden {
            self.hidden.insert(id)
        } else {
            self.hidden.remove(&id)
        };
        self.dirty |= changed;
    }

    /// Switches every panel back on.
    pub fn show_all(&mut self) {
        self.dirty |= !self.hidden.is_empty();
        self.hidden.clear();
    }

    /// Indices of the columns on `side` that have at least one visible
    /// panel this frame.
    fn visible_column_indices(&self, side: DockSide, kind: SourceKind) -> Vec<usize> {
        self.layout
            .columns(side)
            .iter()
            .enumerate()
            .filter(|(_, c)| c.panels.iter().any(|p| self.is_visible(*p, kind)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Which rail carries the "Develop" heading + Solo toggle: the right
    /// one normally, the left one if the user has dragged every panel over
    /// there (so the toggle can never become unreachable).
    fn heading_side(&self, kind: SourceKind) -> DockSide {
        if self.has_dock(DockSide::Right, kind) {
            DockSide::Right
        } else {
            DockSide::Left
        }
    }

    /// Forgets a rail that isn't on screen this frame, so its last-known
    /// rect can't answer a drop hit-test.
    pub fn forget_dock(&mut self, side: DockSide) {
        self.geom.clear_rail(side);
    }

    /// Renders one rail: the heading row (primary rail only), then its
    /// columns side by side, each an independently scrolling stack of
    /// collapsible panel sections, with draggable seams between them.
    ///
    /// **Width containment** (the shipped bug this module's docs describe):
    /// the rail's rect is reserved up front and every column is built into
    /// a child `Ui` pinned to its own slice of it, so a body that wants
    /// more width than it has gets clipped instead of pushing the panel's
    /// edge across the canvas.
    pub fn dock_ui(&mut self, ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>, side: DockSide) {
        let rail_rect = ui.available_rect_before_wrap();
        let mut rail = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("develop-dock", side.label()))
                .max_rect(rail_rect)
                .layout(*ui.layout()),
        );
        rail.set_clip_rect(rail_rect.intersect(ui.clip_rect()));
        self.dock_contents_ui(&mut rail, ctx, side, rail_rect);
        ui.advance_cursor_after_rect(rail_rect);
    }

    fn dock_contents_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut DevelopCtx<'_>,
        side: DockSide,
        rail_rect: egui::Rect,
    ) {
        self.geom.begin_rail(side, rail_rect);
        let mut ops: Vec<LayoutOp> = Vec::new();

        // The heading row (one rail only) is carved off the top so the
        // columns below it all start at the same y.
        let mut body_rect = rail_rect;
        if side == self.heading_side(ctx.source_kind) {
            let head_rect = egui::Rect::from_min_max(
                rail_rect.min,
                egui::pos2(rail_rect.right(), rail_rect.top() + HEADING_HEIGHT),
            );
            let mut head_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("develop-dock-heading", side.label()))
                    .max_rect(head_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let mut solo = self.solo;
            rail_heading_ui(&mut head_ui, &mut solo, &mut ops);
            if solo != self.solo {
                self.solo = solo;
                self.dirty = true;
            }
            body_rect = egui::Rect::from_min_max(
                egui::pos2(rail_rect.left(), head_rect.bottom()),
                rail_rect.max,
            );
        }

        let (planned, splitters) = self.plan_columns(side, ctx.source_kind, body_rect);

        // Columns.
        let mut toggle: Option<(PanelId, bool)> = None;
        let mut geoms: Vec<ColumnGeom> = Vec::new();
        for column in &planned {
            let mut rows: Vec<egui::Rect> = Vec::new();
            {
                let defs = &self.defs;
                let open = &self.open;
                let solo = self.solo;
                let layout = &self.layout;
                let mut col_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("develop-dock-col", side.label(), column.index))
                        .max_rect(column.rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                col_ui.set_clip_rect(column.rect.intersect(ui.clip_rect()));
                column_ui(
                    &mut col_ui,
                    ColumnRender {
                        side,
                        index: column.index,
                        panels: &column.panels,
                        defs,
                        open,
                        solo,
                        layout,
                    },
                    ctx,
                    &mut toggle,
                    &mut ops,
                    &mut rows,
                );
            }
            geoms.push(ColumnGeom {
                side,
                index: column.index,
                rect: column.rect,
                rows,
            });
        }
        self.geom.columns.extend(geoms);

        // Splitters, painted and dragged after the columns so their seam
        // sits on top of the column edges.
        for splitter in &splitters {
            self.splitter_ui(ui, side, splitter);
        }

        if let Some((id, open)) = toggle {
            self.set_open(id, open);
        }
        for op in ops {
            match op {
                LayoutOp::Move { id, target } => {
                    self.move_panel(id, target);
                }
                LayoutOp::Reset => self.reset_layout(),
            }
        }
    }

    /// Places this frame's visible columns (and the seams between them)
    /// inside `body`. Columns keep their stored widths; if they don't all
    /// fit, a very narrow window, every column is scaled down by the
    /// same factor rather than the outermost one being cut off.
    fn plan_columns(
        &self,
        side: DockSide,
        kind: SourceKind,
        body: egui::Rect,
    ) -> (Vec<PlannedColumn>, Vec<PlannedSplitter>) {
        let indices = self.visible_column_indices(side, kind);
        if indices.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let widths: Vec<f32> = indices
            .iter()
            .map(|i| self.layout.columns(side)[*i].width)
            .collect();
        let seams = SPLITTER_PT * indices.len() as f32;
        let wanted: f32 = widths.iter().sum::<f32>() + seams;
        let scale = if wanted > body.width() && wanted > 0.0 {
            ((body.width() - seams) / (wanted - seams)).max(0.1)
        } else {
            1.0
        };

        let mut columns = Vec::with_capacity(indices.len());
        let mut splitters = Vec::with_capacity(indices.len());
        let mut x = body.left();
        let seam = |x: f32, kind: SplitterKind| PlannedSplitter {
            rect: egui::Rect::from_min_max(
                egui::pos2(x, body.top()),
                egui::pos2(x + SPLITTER_PT, body.bottom()),
            ),
            kind,
        };
        for (slot, index) in indices.iter().copied().enumerate() {
            // Right rail: the seam comes before the column (its canvas
            // side); left rail: after it.
            if side == DockSide::Right {
                let kind = if slot == 0 {
                    SplitterKind::Edge { column: index }
                } else {
                    SplitterKind::Between {
                        left: indices[slot - 1],
                        right: index,
                    }
                };
                splitters.push(seam(x, kind));
                x += SPLITTER_PT;
            }
            let w = widths[slot] * scale;
            columns.push(PlannedColumn {
                index,
                rect: egui::Rect::from_min_max(
                    egui::pos2(x, body.top()),
                    egui::pos2(x + w, body.bottom()),
                ),
                panels: self.layout.columns(side)[index]
                    .panels
                    .iter()
                    .copied()
                    .filter(|p| self.is_visible(*p, kind))
                    .collect(),
            });
            x += w;
            if side == DockSide::Left {
                let kind = if slot + 1 == indices.len() {
                    SplitterKind::Edge { column: index }
                } else {
                    SplitterKind::Between {
                        left: index,
                        right: indices[slot + 1],
                    }
                };
                splitters.push(seam(x, kind));
                x += SPLITTER_PT;
            }
        }
        (columns, splitters)
    }

    /// Paints one seam and applies its drag. An `Edge` seam moves the
    /// rail's canvas-facing boundary (the whole rail grows/shrinks); a
    /// `Between` seam moves width from one column to its neighbour and
    /// leaves the rail's total alone.
    fn splitter_ui(&mut self, ui: &mut egui::Ui, side: DockSide, splitter: &PlannedSplitter) {
        let id = ui.make_persistent_id((
            "develop-dock-splitter",
            side.label(),
            match splitter.kind {
                SplitterKind::Edge { column } => (column, usize::MAX),
                SplitterKind::Between { left, right } => (left, right),
            },
        ));
        let response = ui
            .interact(splitter.rect, id, egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);

        let dx = response.drag_delta().x;
        if response.dragged() && dx != 0.0 {
            match splitter.kind {
                SplitterKind::Edge { column } => {
                    let current = self.layout.columns(side)[column].width;
                    // Dragging *away* from the window edge grows the rail.
                    let signed = if side == DockSide::Right { -dx } else { dx };
                    let screen_w = ui.ctx().content_rect().width();
                    let others = self.layout.side_width(side) - current
                        + self.layout.side_width(side.other());
                    let cap = (screen_w - MIN_CANVAS_PT - others).max(MIN_COLUMN_PT);
                    let _ = self
                        .layout
                        .set_width(side, column, (current + signed).min(cap));
                }
                SplitterKind::Between { left, right } => {
                    let (wl, wr) = (
                        self.layout.columns(side)[left].width,
                        self.layout.columns(side)[right].width,
                    );
                    // Keep the pair's total fixed: clamp the transfer to
                    // whatever both columns can absorb.
                    let d = dx
                        .clamp(MIN_COLUMN_PT - wl, MAX_COLUMN_PT - wl)
                        .clamp(wr - MAX_COLUMN_PT, wr - MIN_COLUMN_PT);
                    let _ = self.layout.set_width(side, left, wl + d);
                    let _ = self.layout.set_width(side, right, wr - d);
                }
            }
        }
        // One prefs write per drag, on release, not one per frame.
        if response.drag_stopped() {
            self.dirty = true;
        }

        let seam_x = splitter.rect.center().x;
        let painter = ui.painter();
        paint::engraved_seam_v(
            painter,
            egui::Rangef::new(splitter.rect.top(), splitter.rect.bottom()),
            seam_x,
            side == DockSide::Right,
        );
        if response.dragged() || response.hovered() {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(seam_x - 1.0, splitter.rect.top()),
                    egui::pos2(seam_x + 1.0, splitter.rect.bottom()),
                ),
                egui::CornerRadius::ZERO,
                tokens::accent(),
            );
        }
    }

    /// Ends the frame's drag-and-drop: paints the snap preview for wherever
    /// the panel in flight would land, drags a labelled ghost under the
    /// cursor, and applies the move on release. Call once per frame, after
    /// both rails have rendered.
    pub fn finish_dnd(&mut self, ctx: &egui::Context) {
        let Some(drag) = egui::DragAndDrop::payload::<PanelDrag>(ctx).map(|d| *d) else {
            return;
        };
        let Some(pointer) = ctx
            .pointer_interact_pos()
            .or_else(|| ctx.pointer_latest_pos())
        else {
            return;
        };
        let target = self.hit_test(pointer);

        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("lightbox-dock-dnd"),
        ));
        if let Some(target) = target {
            self.paint_drop_preview(&painter, target, ctx.content_rect());
        }
        paint_drag_ghost(&painter, ctx, pointer, drag.title);
        ctx.set_cursor_icon(if target.is_some() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::NoDrop
        });

        if ctx.input(|i| i.pointer.any_released()) {
            let _ = egui::DragAndDrop::take_payload::<PanelDrag>(ctx);
            if let Some(target) = target {
                if self.move_panel(drag.id, target) {
                    // A panel you just placed should be visible where you
                    // put it, even if it was collapsed before the drag.
                    self.open.insert(drag.id, true);
                }
            }
        }
    }

    /// Where would a drop at `pointer` land? `None` = nowhere (the drag is
    /// over the canvas, the filmstrip or the top bar; releasing there is a
    /// cancel, not a move).
    fn hit_test(&self, pointer: egui::Pos2) -> Option<DropTarget> {
        for side in DockSide::ALL {
            let Some(rail) = self.geom.rail(side) else {
                continue;
            };
            if !rail.contains(pointer) {
                continue;
            }
            // Hard against the window edge: split off a new outermost
            // column rather than joining the one under the pointer.
            let outer_x = match side {
                DockSide::Left => rail.left(),
                DockSide::Right => rail.right(),
            };
            if (pointer.x - outer_x).abs() <= NEW_COLUMN_EDGE_PT {
                return Some(DropTarget::NewColumn {
                    side,
                    at: match side {
                        DockSide::Left => 0,
                        DockSide::Right => self.layout.columns(side).len(),
                    },
                });
            }
            // Otherwise: into the column under the pointer (or, if the
            // pointer is on a seam, the nearest one).
            let column = self
                .geom
                .columns
                .iter()
                .filter(|c| c.side == side)
                .min_by(|a, b| {
                    let d = |c: &&ColumnGeom| (c.rect.center().x - pointer.x).abs();
                    d(a).total_cmp(&d(b))
                })?;
            let row = column
                .rows
                .iter()
                .position(|r| pointer.y < r.center().y)
                .unwrap_or(column.rows.len());
            return Some(DropTarget::Into {
                side,
                column: column.index,
                row,
            });
        }

        // Not over a rail: the two drop strips flanking the canvas, which
        // are how an empty rail gets its first column.
        let band = self.canvas_band()?;
        if !band.1.contains(pointer.y) {
            return None;
        }
        let (canvas_left, canvas_right) = band.0;
        if (canvas_left..=canvas_left + CANVAS_DROP_STRIP_PT).contains(&pointer.x) {
            return Some(DropTarget::NewColumn {
                side: DockSide::Left,
                at: self.layout.columns(DockSide::Left).len(),
            });
        }
        if (canvas_right - CANVAS_DROP_STRIP_PT..=canvas_right).contains(&pointer.x) {
            return Some(DropTarget::NewColumn {
                side: DockSide::Right,
                at: 0,
            });
        }
        None
    }

    /// The canvas's horizontal bounds and the rails' shared vertical band,
    /// derived from whichever rails are on screen. `None` when no rail is
    /// visible at all (nothing to dock into).
    fn canvas_band(&self) -> Option<((f32, f32), egui::Rangef)> {
        let left = self.geom.rail(DockSide::Left);
        let right = self.geom.rail(DockSide::Right);
        let any = left.or(right)?;
        let band = egui::Rangef::new(any.top(), any.bottom());
        let canvas_left = left.map_or(any.left().min(0.0), |r| r.right());
        let canvas_right = right.map_or(any.right(), |r| r.left());
        Some(((canvas_left, canvas_right), band))
    }

    /// The snap preview (theme spec §2's accent language): an insertion bar
    /// for "drop into this column here", a washed outline for "split off a
    /// new column here".
    fn paint_drop_preview(&self, painter: &egui::Painter, target: DropTarget, screen: egui::Rect) {
        match target {
            DropTarget::Into { side, column, row } => {
                let Some(geom) = self
                    .geom
                    .columns
                    .iter()
                    .find(|c| c.side == side && c.index == column)
                else {
                    return;
                };
                let y = match geom.rows.get(row) {
                    Some(rect) => rect.top(),
                    None => geom.rows.last().map_or(geom.rect.top(), |r| r.bottom()),
                };
                let bar = egui::Rect::from_min_max(
                    egui::pos2(geom.rect.left() + 2.0, y - 1.5),
                    egui::pos2(geom.rect.right() - 2.0, y + 1.5),
                );
                painter.rect_filled(geom.rect, tokens::RADIUS_PANEL, tokens::accent_wash());
                paint::glow_rect(
                    painter,
                    bar,
                    tokens::RADIUS_THUMB,
                    &tokens::accent_glow_drag(),
                );
                painter.rect_filled(bar, tokens::RADIUS_THUMB, tokens::accent());
            }
            DropTarget::NewColumn { side, at } => {
                let Some(((canvas_left, canvas_right), band)) = self.canvas_band() else {
                    return;
                };
                let width = DEFAULT_COLUMN_PT.min((canvas_right - canvas_left) * 0.5);
                let x0 = match (side, at == 0) {
                    (DockSide::Left, true) => screen.left(),
                    (DockSide::Left, false) => canvas_left,
                    (DockSide::Right, true) => canvas_right - width,
                    (DockSide::Right, false) => screen.right() - width,
                };
                let rect = egui::Rect::from_min_max(
                    egui::pos2(x0, band.min),
                    egui::pos2(x0 + width, band.max),
                );
                painter.rect_filled(rect, tokens::RADIUS_PANEL, tokens::accent_wash());
                paint::glow_rect(
                    painter,
                    rect,
                    tokens::RADIUS_PANEL,
                    &tokens::accent_glow_drag(),
                );
                painter.rect_stroke(
                    rect,
                    tokens::RADIUS_PANEL,
                    egui::Stroke::new(tokens::STROKE_SELECTION_BORDER, tokens::accent()),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

impl Default for PanelHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything one column needs to render, bundled so the renderer stays a
/// free function (the host's fields are borrowed piecewise while it runs).
struct ColumnRender<'a> {
    side: DockSide,
    index: usize,
    panels: &'a [PanelId],
    defs: &'a [PanelDef],
    open: &'a HashMap<PanelId, bool>,
    solo: bool,
    layout: &'a DockLayout,
}

/// One column: a scrolling stack of collapsible panel sections. Reports
/// the open/close click, any layout command from a header's context menu,
/// and each section's rect (the drop hit-test's insertion ladder).
fn column_ui(
    ui: &mut egui::Ui,
    render: ColumnRender<'_>,
    ctx: &mut DevelopCtx<'_>,
    toggle: &mut Option<(PanelId, bool)>,
    ops: &mut Vec<LayoutOp>,
    rows: &mut Vec<egui::Rect>,
) {
    let (pad_l, pad_r, pad_t, pad_b) = tokens::PANEL_BODY_PADDING;
    let body_margin = egui::Margin {
        left: pad_l as i8,
        right: pad_r as i8,
        top: pad_t as i8,
        bottom: pad_b as i8,
    };

    // An unobtrusive, thin floating scrollbar so it doesn't eat into the
    // 12px panel-body insets (§2).
    let scroll = &mut ui.style_mut().spacing.scroll;
    scroll.floating = true;
    scroll.bar_width = 6.0;
    scroll.floating_width = 3.0;

    egui::ScrollArea::vertical()
        .id_salt(("develop-dock-scroll", render.side.label(), render.index))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for id in render.panels {
                let Some(def) = render.defs.iter().find(|d| d.id == *id) else {
                    continue;
                };
                let open = *render.open.get(id).unwrap_or(&true);
                let solo_active = render.solo && open;

                let section = ui.scope(|ui| {
                    // Flush the header into its own body, §2's hairline
                    // IS the header/body boundary, so no rail-floor gap
                    // should show between them. Normal item spacing
                    // returns between different panels (this override is
                    // scoped to this one `ui.scope`).
                    ui.spacing_mut().item_spacing.y = 0.0;

                    let header = panel_header_ui(ui, def, open, solo_active, render.layout, ops);
                    if header.clicked() {
                        *toggle = Some((def.id, !open));
                    }

                    if open {
                        egui::Frame::new()
                            // Theme-resolved, not the dark constant: a
                            // hardcoded `#292929` card under a Light
                            // theme's near-black ink is how this rail
                            // ended up unreadable.
                            .fill(crate::theme::Chrome::of(ui.visuals()).panel)
                            .inner_margin(body_margin)
                            .show(ui, |ui| {
                                // Restore normal inter-control spacing
                                // inside the body, it inherited the
                                // header-flush override above.
                                ui.spacing_mut().item_spacing.y = tokens::ITEM_SPACING.y;
                                // Make the card read full column width
                                // even when its content doesn't fill it
                                // on its own (`Frame`'s background follows
                                // its content's bounding box).
                                ui.set_min_width(ui.available_width());
                                (def.build)(ui, ctx);
                            });
                    }
                });
                rows.push(section.response.rect);
            }
        });
}

/// The "Develop" rail-level heading's face, distinct from
/// [`fonts::panel_header_font`] (11px, used by the collapsible panel
/// headers below it). §9's typography table has no role for a page-level
/// module title, so this is a deliberate, modest step up in size, Inter
/// SemiBold 13px, sentence case (not UPPERCASE like the panel headers
/// nothing in the brief calls for that treatment here), chosen to
/// outrank the panel headers without competing with the canvas for
/// attention. Documented as a spec deviation: no §9 token backs this
/// exact size.
fn rail_heading_font() -> egui::FontId {
    egui::FontId::new(
        13.0,
        egui::FontFamily::Name(fonts::FAMILY_INTER_SEMIBOLD.into()),
    )
}

/// The rail's own top row, "Develop" + the Solo toggle, restyled per the
/// brief's item 3: the stock `ui.heading()` + `ui.separator()` read cheap
/// next to the re-skinned panel headers below. `solo` is `&mut` so the
/// existing `ui.toggle_value` keeps driving [`PanelHost::solo`] directly;
/// no new state is introduced here.
///
/// The heading row carries its **own** horizontal inset. The rail frame
/// (`lib.rs`) no longer supplies one, so the panel header bands and body
/// cards below run edge to edge the way a Lightroom rail's do, and the
/// controls inside them get those 24pt back (a 280pt rail used to spend
/// 48pt of it on two stacked 12pt insets).
fn rail_heading_ui(ui: &mut egui::Ui, solo: &mut bool, ops: &mut Vec<LayoutOp>) {
    ui.horizontal(|ui| {
        ui.add_space(tokens::SPACE_3);
        ui.label(
            egui::RichText::new("Develop")
                .font(rail_heading_font())
                .color(crate::theme::Chrome::of(ui.visuals()).text_primary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(tokens::SPACE_3);
            ui.toggle_value(solo, "Solo")
                .on_hover_text("Panel-solo mode: opening a panel collapses the others");
            ui.menu_button("Layout", |ui| {
                ui.label(
                    egui::RichText::new("Drag a panel by its header to move it")
                        .color(tokens::TEXT_SECONDARY),
                );
                ui.label(
                    egui::RichText::new("Drop near a window edge for a new column")
                        .color(tokens::TEXT_SECONDARY),
                );
                ui.separator();
                if ui.button("Reset panel layout").clicked() {
                    ops.push(LayoutOp::Reset);
                    ui.close();
                }
            })
            .response
            .on_hover_text("Panel layout: reset the rails to one right-hand column");
        });
    });

    // The engraved-seam boundary (§2) instead of a stock `ui.separator()`
    // the rail floor (`ELEV_0_BASE`) sits above the lighter panel
    // content scrolling beneath it, so the highlight stroke goes below.
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 2.0), egui::Sense::hover());
    paint::engraved_seam_h(
        ui.painter(),
        egui::Rangef::new(rect.left(), rect.right()),
        rect.top(),
        true,
    );
}

/// Draws one Lightroom-style collapsible-panel header band (spec §2) and
/// returns its `Response`, this replaces `egui::CollapsingHeader`'s
/// built-in header, so it owns everything that widget used to give away
/// for free: the click-to-toggle hit region, hover/rest visuals, AND the
/// AccessKit node.
///
/// `open` is this frame's expand/collapse state (the caller still owns
/// [`PanelHost::set_open`], this function only reports the click).
/// `solo_active` is `true` exactly when panel-solo mode is on AND this is
/// the one open panel; it drives both the accent bar/wash paint (§2) and
/// the AccessKit toggled flag below.
fn panel_header_ui(
    ui: &mut egui::Ui,
    def: &PanelDef,
    open: bool,
    solo_active: bool,
    layout: &DockLayout,
    ops: &mut Vec<LayoutOp>,
) -> egui::Response {
    // A persistent id keyed by the panel's own stable identity (not this
    // frame's allocation order), so hover/click tracking survives a panel
    // being filtered in/out by §4 source gating.
    let id = ui.make_persistent_id(def.id.0);
    let desired = egui::vec2(ui.available_width(), tokens::PANEL_HEADER_HEIGHT);
    let (_, rect) = ui.allocate_space(desired);
    // `click_and_drag`: a click still toggles the section (egui only
    // reports `clicked()` when the press never became a drag), while a
    // press that travels past the drag threshold picks the panel up, the
    // header IS the drag handle, as in every docking UI.
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    if response.drag_started() {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            PanelDrag {
                id: def.id,
                title: def.title,
            },
        );
    }
    let being_dragged =
        egui::DragAndDrop::payload::<PanelDrag>(ui.ctx()).is_some_and(|drag| drag.id == def.id);

    // Keyboard/pointer parity for the drag (and the discoverable way in):
    // every move the drag can do is also a menu command.
    header_context_menu(&response, def, layout, ops);

    // AccessKit: `CollapsingHeader`'s own role (`Button`, via egui's
    // `WidgetType` → `Role` map) plus the solo-active state as a `Toggled`
    // flag, `WidgetInfo::selected`'s `selected` parameter maps to
    // `Node::set_toggled` in this egui version (`response.rs`,
    // `fill_accesskit_node_from_widget_info`), not `set_selected`, so the
    // paint-free assertion for this is `accesskit_node().toggled()`, not
    // `.is_selected()` (see this module's tests). It is still a real
    // accessibility signal (a screen reader can announce a toggled
    // header), not test-only plumbing. The label is the SOURCE
    // `def.title`, uppercasing below is a paint-time transform only
    // (spec instruction), never the accessible name.
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::CollapsingHeader,
            ui.is_enabled(),
            solo_active,
            def.title,
        )
    });

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        // Surfaces AND ink both come from the same theme resolution, so the
        // band can never be painted in one theme's values while its label
        // is painted in the other's.
        let chrome = crate::theme::Chrome::of(ui.visuals());
        let (grad_top, grad_bottom) = if hovered {
            chrome.header_hover
        } else {
            chrome.header
        };
        let painter = ui.painter();
        paint::vgradient_rounded_rect(painter, rect, 0.0, grad_top, grad_bottom);

        // The panel currently in flight reads as a "hole" its band was
        // lifted out of: the ghost under the cursor is the real one.
        if being_dragged {
            painter.rect_filled(rect, egui::CornerRadius::ZERO, tokens::accent_wash());
        }

        // Active/solo (§2): an accent wash over the gradient, then a 2px
        // accent bar on the header's left edge (painted last so it reads
        // crisp on top of the wash).
        if solo_active {
            painter.rect_filled(rect, egui::CornerRadius::ZERO, tokens::accent_wash());
            let bar = egui::Rect::from_min_size(
                rect.left_top(),
                egui::vec2(tokens::PANEL_SOLO_BAR_WIDTH, rect.height()),
            );
            painter.rect_filled(bar, egui::CornerRadius::ZERO, tokens::accent());
        }

        // Disclosure triangle: procedural fill, not a glyph, points
        // right collapsed, down open, an instant swap (rotation is a
        // discrete state, not motion, per spec).
        let triangle_center = egui::pos2(rect.left() + tokens::SPACE_3, rect.center().y);
        let triangle_color = if hovered {
            chrome.text_primary
        } else {
            chrome.text_secondary
        };
        painter.add(egui::Shape::convex_polygon(
            disclosure_triangle_points(triangle_center, open),
            triangle_color,
            egui::Stroke::NONE,
        ));

        // Header label: UPPERCASE + tracking are paint-time only (§9)
        // the AccessKit name above stays `def.title` verbatim.
        let text_left =
            rect.left() + tokens::SPACE_3 + tokens::DISCLOSURE_TRIANGLE_SIZE.x + tokens::SPACE_2;
        let format = egui::TextFormat {
            font_id: fonts::panel_header_font(),
            color: chrome.text_primary,
            extra_letter_spacing: 0.6,
            ..Default::default()
        };
        let job = egui::text::LayoutJob::simple_format(def.title.to_uppercase(), format);
        let galley = painter.layout_job(job);
        let text_rect = egui::Align2::LEFT_CENTER
            .anchor_size(egui::pos2(text_left, rect.center().y), galley.size());
        painter.galley(text_rect.min, galley, chrome.text_primary);

        // Drag affordance: a two-column grip at the band's right end,
        // shown on hover (a permanent grip on every header would out-shout
        // the disclosure triangle, which is the header's primary action).
        if hovered || being_dragged {
            paint_grip(painter, rect, chrome.text_secondary);
        }

        // Bottom edge: a real engraved cut, not a single flat line. A lone
        // hairline was carrying the entire header/body boundary, which is
        // why the band failed to read as a band in review renders. The
        // lighter surface (the header) sits *above* this seam, hence
        // `lighter_side_below = false`.
        paint::engraved_seam_h(
            painter,
            egui::Rangef::new(rect.left(), rect.right()),
            rect.bottom(),
            false,
        );
    }

    if response.hovered() && !being_dragged {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }

    response
}

/// The panel header's right-click menu, the pointer drag's keyboard- and
/// discoverability-parity twin (a drag is fast once you know it exists;
/// this is how you find out, and how you move a panel without a pointer
/// drag at all).
fn header_context_menu(
    response: &egui::Response,
    def: &PanelDef,
    layout: &DockLayout,
    ops: &mut Vec<LayoutOp>,
) {
    /// Docking a panel at the far end of a rail: onto the last existing
    /// column there, or into that rail's first column if it has none.
    fn append_to(layout: &DockLayout, side: DockSide) -> DropTarget {
        match layout.columns(side).len() {
            0 => DropTarget::NewColumn { side, at: 0 },
            n => DropTarget::Into {
                side,
                column: n - 1,
                row: layout.columns(side)[n - 1].panels.len(),
            },
        }
    }

    response.context_menu(|ui| {
        ui.label(
            egui::RichText::new(def.title)
                .font(fonts::panel_header_font())
                .color(tokens::TEXT_SECONDARY),
        );
        ui.separator();
        let here = layout.find(def.id);
        for side in DockSide::ALL {
            let label = format!("Move to {} rail", side.label());
            let already_alone = matches!(here, Some((s, c, _))
                if s == side && layout.columns(s)[c].panels.len() == 1
                    && c + 1 == layout.columns(s).len());
            if ui
                .add_enabled(!already_alone, egui::Button::new(label))
                .clicked()
            {
                ops.push(LayoutOp::Move {
                    id: def.id,
                    target: append_to(layout, side),
                });
                ui.close();
            }
        }
        ui.separator();
        for side in DockSide::ALL {
            let label = format!("Move to a new {} column", side.label());
            if ui.button(label).clicked() {
                // Outboard of everything already docked on that side.
                ops.push(LayoutOp::Move {
                    id: def.id,
                    target: DropTarget::NewColumn {
                        side,
                        at: match side {
                            DockSide::Left => 0,
                            DockSide::Right => layout.columns(side).len(),
                        },
                    },
                });
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Reset panel layout").clicked() {
            ops.push(LayoutOp::Reset);
            ui.close();
        }
    });
}

/// The panel riding the cursor mid-drag: a small popup-elevation band
/// carrying the panel's title, so it is always obvious *what* is being
/// docked even when the pointer is far from the rail it came from.
fn paint_drag_ghost(
    painter: &egui::Painter,
    ctx: &egui::Context,
    pointer: egui::Pos2,
    title: &str,
) {
    let chrome = crate::theme::Chrome::of(&ctx.global_style().visuals);
    let galley = painter.layout_no_wrap(
        title.to_uppercase(),
        fonts::panel_header_font(),
        chrome.text_primary,
    );
    let padding = egui::vec2(tokens::SPACE_3, tokens::SPACE_2);
    let size = galley.size() + padding * 2.0;
    // Offset clear of the cursor hot-spot, then held inside the window so
    // a drag toward an edge never pushes the label off screen.
    let screen = ctx.content_rect();
    let min = egui::pos2(
        (pointer.x + 16.0).min(screen.right() - size.x - 4.0),
        (pointer.y + 12.0).min(screen.bottom() - size.y - 4.0),
    );
    let rect = egui::Rect::from_min_size(min, size);

    painter.add(paint::shadow_popup().as_shape(rect, tokens::RADIUS_CONTROL));
    paint::vgradient_rounded_rect(
        painter,
        rect,
        tokens::RADIUS_CONTROL,
        chrome.header_hover.0,
        chrome.header_hover.1,
    );
    painter.rect_stroke(
        rect,
        tokens::RADIUS_CONTROL,
        egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::accent()),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + padding, galley, chrome.text_primary);
}

/// The header band's drag grip: two columns of three dots at its right
/// end, in the same ink as the resting disclosure triangle.
fn paint_grip(painter: &egui::Painter, band: egui::Rect, color: egui::Color32) {
    let right = band.right() - tokens::SPACE_3;
    for col in 0..2 {
        for row in 0..3 {
            let center = egui::pos2(
                right - col as f32 * 3.0,
                band.center().y + (row as f32 - 1.0) * 3.0,
            );
            painter.circle_filled(center, 0.9, color);
        }
    }
}

/// The §2 disclosure triangle's 3 points, centered at `center`. The "open"
/// (down-pointing) orientation is a literal 90° rotation of the same
/// right-pointing ("collapsed") shape, computed rather than hand-authored
/// twice, so both orientations are provably the same size and can't drift
/// apart.
fn disclosure_triangle_points(center: egui::Pos2, open: bool) -> Vec<egui::Pos2> {
    let half_w = tokens::DISCLOSURE_TRIANGLE_SIZE.x * 0.5;
    let half_h = tokens::DISCLOSURE_TRIANGLE_SIZE.y * 0.5;
    // Right-pointing (collapsed): a vertical base on the left, apex on the
    // right.
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
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use lightbox_edit::{HistoryStepMeta, ParamDelta, ParamId, ParamValue, SnapshotMeta};
    use lightbox_types::SnapshotId;

    /// A do-nothing `EditBinding`, the host tests exercise registration/
    /// filtering/headers, not editing.
    struct StubBinding;
    impl EditBinding for StubBinding {
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

    fn stub_body(ui: &mut egui::Ui, _ctx: &mut DevelopCtx<'_>) {
        ui.label("panel body");
    }

    struct RailState {
        host: PanelHost,
        kind: SourceKind,
    }

    fn rail_harness(kind: SourceKind) -> Harness<'static, RailState> {
        let host = two_panel_host();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(280.0, 500.0))
            .build_ui_state(
                themed_state(|ui, s: &mut RailState| {
                    let mut binding = StubBinding;
                    let mut gizmos = GizmoLayer::new();
                    let mut ctx = DevelopCtx {
                        source_kind: s.kind,
                        edit: &mut binding,
                        gizmos: &mut gizmos,
                    };
                    s.host.dock_ui(ui, &mut ctx, DockSide::Right);
                }),
                RailState { host, kind },
            );
        // Frame 0 only installs the theme (`panel_header_font()` etc. bind
        // NAMED font families that epaint panics on if used before
        // `theme::install` runs, see `theme::test_support`'s doc comment)
        // and paints nothing; burn it here so every call site's own
        // `harness.run()` is the first REAL paint, exactly as before this
        // wrapper existed.
        harness.run();
        harness
    }

    /// Two test panels registered on a fresh host, the same pair
    /// [`rail_harness`] uses, hoisted so the docking harness can reuse it.
    fn two_panel_host() -> PanelHost {
        let mut host = PanelHost::new();
        host.register(PanelDef {
            id: PanelId("test.any"),
            title: "Any-Source Panel",
            source_req: SourceReq::Any,
            order: 20,
            build: stub_body,
        });
        host.register(PanelDef {
            id: PanelId("test.rawonly"),
            title: "Raw-Only Panel",
            source_req: SourceReq::RawOnly,
            order: 30,
            build: stub_body,
        });
        host
    }

    /// The app's real composition, small enough to unit-test: a left rail,
    /// a right rail, canvas between them, and the one `finish_dnd` call
    /// that resolves a drag spanning all three. Panel positions come from
    /// the dock layout, exactly as in `lib.rs`.
    fn dock_harness(host: PanelHost, kind: SourceKind) -> Harness<'static, RailState> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(
                themed_state(|ui, s: &mut RailState| {
                    let mut binding = StubBinding;
                    let mut gizmos = GizmoLayer::new();
                    let mut ctx = DevelopCtx {
                        source_kind: s.kind,
                        edit: &mut binding,
                        gizmos: &mut gizmos,
                    };
                    let full = ui.available_rect_before_wrap();
                    for side in DockSide::ALL {
                        if !s.host.has_dock(side, s.kind) {
                            s.host.forget_dock(side);
                            continue;
                        }
                        let w = s.host.dock_width(side, s.kind, full.width());
                        let rect = match side {
                            DockSide::Left => egui::Rect::from_min_max(
                                full.min,
                                egui::pos2(full.left() + w, full.bottom()),
                            ),
                            DockSide::Right => egui::Rect::from_min_max(
                                egui::pos2(full.right() - w, full.top()),
                                full.max,
                            ),
                        };
                        let mut rail = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt(("harness-dock", side.label()))
                                .max_rect(rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        s.host.dock_ui(&mut rail, &mut ctx, side);
                    }
                    s.host.finish_dnd(ui.ctx());
                }),
                RailState { host, kind },
            );
        harness.run();
        harness
    }

    /// Drags from `from` to `to` with the primary button, as a real
    /// pointer would: press, travel (in two hops, so egui's press/drag
    /// discrimination sees motion while the button is held), release.
    fn drag(harness: &mut Harness<'static, RailState>, from: egui::Pos2, to: egui::Pos2) {
        harness.event(egui::Event::PointerMoved(from));
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.event(egui::Event::PointerMoved(from + (to - from) * 0.5));
        harness.event(egui::Event::PointerMoved(to));
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run();
    }

    /// The shipped default is the pre-docking rail: one right-hand column
    /// holding every panel, in registration order, and no left rail.
    #[test]
    fn a_fresh_host_docks_every_panel_in_one_right_column() {
        let host = two_panel_host();
        assert!(host.layout().columns(DockSide::Left).is_empty());
        assert_eq!(host.layout().columns(DockSide::Right).len(), 1);
        assert_eq!(
            host.layout().columns(DockSide::Right)[0].panels,
            vec![PanelId("test.any"), PanelId("test.rawonly")]
        );
    }

    /// **The drag.** Grab a panel's header band, drop it in the strip
    /// along the canvas's left edge: it leaves the right rail and the left
    /// rail appears, holding it.
    #[test]
    fn dragging_a_header_to_the_canvas_left_edge_docks_it_in_the_left_rail() {
        let mut harness = dock_harness(two_panel_host(), SourceKind::Raw);
        let header = harness.get_by_label("Any-Source Panel").rect().center();
        drag(&mut harness, header, egui::pos2(20.0, 300.0));

        let layout = harness.state().host.layout();
        assert_eq!(
            layout.columns(DockSide::Left).len(),
            1,
            "the drop must create the left rail's first column"
        );
        assert_eq!(
            layout.columns(DockSide::Left)[0].panels,
            vec![PanelId("test.any")]
        );
        assert_eq!(
            layout.columns(DockSide::Right)[0].panels,
            vec![PanelId("test.rawonly")],
            "and the panel must leave the column it came from"
        );
        // Both rails now render: both headers are still reachable.
        harness.get_by_label("Any-Source Panel");
        harness.get_by_label("Raw-Only Panel");
    }

    /// Dropping over the canvas (neither rail, neither edge strip) is a
    /// cancel: nothing moves.
    #[test]
    fn dropping_over_the_canvas_leaves_the_layout_alone() {
        let mut harness = dock_harness(two_panel_host(), SourceKind::Raw);
        let before = harness.state().host.snapshot();
        let header = harness.get_by_label("Any-Source Panel").rect().center();
        drag(&mut harness, header, egui::pos2(450.0, 300.0));
        assert_eq!(harness.state().host.snapshot(), before);
    }

    /// A second column on the same rail renders alongside the first
    /// both panels visible at once, no scrolling, side by side.
    #[test]
    fn a_second_right_column_renders_beside_the_first() {
        let mut host = two_panel_host();
        assert!(host.move_panel(
            PanelId("test.rawonly"),
            DropTarget::NewColumn {
                side: DockSide::Right,
                at: 1
            }
        ));
        let mut harness = dock_harness(host, SourceKind::Raw);
        harness.run();

        assert_eq!(
            harness.state().host.layout().columns(DockSide::Right).len(),
            2
        );
        let left_col = harness.get_by_label("Any-Source Panel").rect();
        let right_col = harness.get_by_label("Raw-Only Panel").rect();
        assert!(
            left_col.right() <= right_col.left() + 1.0,
            "columns must be laid out side by side, not stacked \
             (left {left_col:?}, right {right_col:?})"
        );
        assert!(
            right_col.right() <= 900.0 + 1.0,
            "the outer column must stay inside the window"
        );
    }

    /// Keyboard/pointer parity: every drag has a menu command. Right-click
    /// a header, pick "Move to left rail", and the panel docks there
    /// without a pointer drag ever happening.
    #[test]
    fn the_header_context_menu_moves_a_panel_without_dragging() {
        let mut harness = dock_harness(two_panel_host(), SourceKind::Raw);
        harness.get_by_label("Any-Source Panel").click_secondary();
        harness.run();
        harness.get_by_label("Move to left rail").click();
        harness.run();

        assert_eq!(
            harness.state().host.layout().columns(DockSide::Left)[0].panels,
            vec![PanelId("test.any")]
        );
    }

    /// The layout, the solo flag and every collapsed section survive a
    /// `prefs.toml` round trip, the persistence contract `lib.rs` drives
    /// through `snapshot`/`restore`.
    #[test]
    fn layout_solo_and_collapsed_state_round_trip_through_prefs() {
        let mut host = two_panel_host();
        host.move_panel(
            PanelId("test.rawonly"),
            DropTarget::NewColumn {
                side: DockSide::Left,
                at: 0,
            },
        );
        host.set_open(PanelId("test.any"), false);
        host.solo = true;
        let snapshot = host.snapshot();
        assert!(host.take_dirty(), "a move must ask for a prefs write");
        assert!(!host.take_dirty(), "and the flag must drain");

        let mut restored = two_panel_host();
        restored.restore(&snapshot);
        assert_eq!(restored.layout(), host.layout());
        assert!(restored.solo());
        assert!(!restored.open_state(PanelId("test.any")));
        assert!(restored.open_state(PanelId("test.rawonly")));
        assert!(
            !restored.take_dirty(),
            "restoring is not a change worth writing back"
        );
    }

    /// H1 AC (panel headers): every rail section header is an AccessKit
    /// node named by its `PanelDef.title`, plus the rail heading itself.
    #[test]
    fn rail_headers_are_accessible_names() {
        let mut harness = rail_harness(SourceKind::Raw);
        harness.run();
        harness.get_by_label("Develop");
        harness.get_by_label("Solo");
        harness.get_by_label("Any-Source Panel");
        harness.get_by_label("Raw-Only Panel");
    }

    /// E2/§4 in the AccessKit tree: a `RawOnly` panel is ABSENT (not
    /// disabled) for a Rendered source.
    #[test]
    fn raw_only_panels_are_absent_from_the_tree_for_rendered_sources() {
        let mut harness = rail_harness(SourceKind::Rendered);
        harness.run();
        harness.get_by_label("Any-Source Panel");
        assert_eq!(
            harness.query_all_by_label("Raw-Only Panel").count(),
            0,
            "RawOnly panels must be hidden, not disabled (§4)"
        );
    }

    /// The custom header still does what `CollapsingHeader` did for free:
    /// clicking it toggles `PanelHost`'s open state.
    #[test]
    fn header_click_toggles_the_panel_open_state() {
        let mut harness = rail_harness(SourceKind::Raw);
        harness.run();
        assert!(harness.state().host.open_state(PanelId("test.any")));

        harness.get_by_label("Any-Source Panel").click();
        harness.run();
        assert!(!harness.state().host.open_state(PanelId("test.any")));

        harness.get_by_label("Any-Source Panel").click();
        harness.run();
        assert!(harness.state().host.open_state(PanelId("test.any")));
    }

    /// §2 solo/active state: turn solo mode on, then re-open a
    /// previously-closed panel, `PanelHost::set_open` must collapse every
    /// OTHER panel (the real "opening a panel collapses the rest"
    /// behavior), and the reopened header must carry the accent bar/wash's
    /// AccessKit counterpart. The 2px accent bar and wash overlay
    /// themselves are visual-only properties a paint-free harness can't
    /// inspect pixel-by-pixel; `panel_header_ui`'s `WidgetInfo::selected`
    /// call is the honest headless proxy, it sets the exact same
    /// `solo_active` boolean the paint code branches on, surfaced as
    /// AccessKit's `Toggled` state (see `panel_header_ui`'s doc comment
    /// for why `toggled()`, not `is_selected()`, is the right query here).
    #[test]
    fn solo_active_panel_reports_accesskit_toggled_and_collapses_the_rest() {
        let mut harness = rail_harness(SourceKind::Raw);
        harness.run();

        // Both panels start open (default-open, spec §6.5). Close
        // "Any-Source Panel" first so the upcoming solo-open below
        // exercises the real closed->open transition, not a no-op click.
        harness.get_by_label("Any-Source Panel").click();
        harness.run();

        // Turn solo mode on, then reopen "Any-Source Panel": `set_open`
        // collapses every OTHER panel when one opens under solo
        // "Raw-Only Panel" (still open from its default) must close too.
        harness.get_by_label("Solo").click();
        harness.run();
        harness.get_by_label("Any-Source Panel").click();
        harness.run();

        let active = harness.get_by_label("Any-Source Panel");
        assert_eq!(
            active.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True),
            "the solo-active panel header must report AccessKit toggled=true"
        );

        let inactive = harness.get_by_label("Raw-Only Panel");
        assert_eq!(
            inactive.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::False),
            "a collapsed, non-solo-active header must not report toggled"
        );
    }
}
