// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The develop rails' **dock layout**, which panel sits in which rail and
//! column, and how wide each column is.
//!
//! This module is deliberately pure: no `egui`, no painting, no hit
//! testing. [`panels::host`](crate::panels::host) owns the rendering, the
//! drag gesture and the drop hit-test; everything it decides is expressed
//! as a [`DropTarget`] and applied here, so the interesting invariants
//! (columns never go empty, a panel is never docked twice, an unknown id
//! never survives a reload) are unit-testable without a GPU.
//!
//! ## Model
//!
//! Two rails, [`DockSide::Left`] and [`DockSide::Right`], each hold zero
//! or more [`DockColumn`]s. Columns are stored in **visual left-to-right
//! order** on both sides, so `right[0]` is the column nearest the canvas
//! and `left[0]` is the one nearest the window edge. A rail with no columns
//! simply isn't shown.
//!
//! The **default** layout (fresh install, or `prefs.toml` with no
//! `panel_layout` key) is exactly what the app shipped before docking
//! existed: every registered panel, history and presets included, in one
//! right-hand column, in registration (`PanelDef::order`) order. Nothing
//! moves until the user moves it.
//!
//! ## Reload discipline ([`DockLayout::sync_registered`])
//!
//! The persisted layout names panels by id string, and the registered set
//! changes as epics land panels. So on load: ids this build doesn't
//! register are dropped, duplicates collapse to their first placement,
//! columns left empty are pruned, and registered panels the file never
//! mentioned are appended to the last right-hand column. A layout can
//! therefore survive both an upgrade (new panels appear) and a downgrade
//! (unknown panels vanish) without losing the user's arrangement.

use std::collections::HashSet;

use lightbox_core::{PanelColumnPrefs, PanelLayoutPrefs};

use crate::panels::host::PanelId;

/// A fresh column's width, and the width a rail defaults to, the shipped
/// pre-docking rail width (`lib.rs`, theme spec §2).
pub const DEFAULT_COLUMN_PT: f32 = 280.0;

/// Narrowest a column may be dragged. Below this the develop sliders lose
/// their label/value row to truncation.
pub const MIN_COLUMN_PT: f32 = 200.0;

/// Widest a single column may be dragged.
pub const MAX_COLUMN_PT: f32 = 560.0;

/// Canvas width the rails must always leave behind: however many columns
/// are docked, dragging can never squeeze the image below this.
pub const MIN_CANVAS_PT: f32 = 320.0;

/// Which rail a column lives in.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum DockSide {
    /// The left-hand rail (empty by default, the user drags panels here).
    Left,
    /// The right-hand rail (holds every panel by default).
    Right,
}

impl DockSide {
    /// Both sides in visual order, for iteration.
    pub const ALL: [DockSide; 2] = [DockSide::Left, DockSide::Right];

    /// The other rail.
    pub fn other(self) -> DockSide {
        match self {
            DockSide::Left => DockSide::Right,
            DockSide::Right => DockSide::Left,
        }
    }

    /// Human label, for menu items and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            DockSide::Left => "left",
            DockSide::Right => "right",
        }
    }
}

/// One docked column: a width and the panels stacked in it, top to bottom.
#[derive(Clone, PartialEq, Debug)]
pub struct DockColumn {
    /// Column width in logical points (clamped to
    /// [`MIN_COLUMN_PT`]..=[`MAX_COLUMN_PT`]).
    pub width: f32,
    /// Panels in this column, top to bottom.
    pub panels: Vec<PanelId>,
}

impl DockColumn {
    /// A column of `panels` at the default width.
    pub fn new(panels: Vec<PanelId>) -> DockColumn {
        DockColumn {
            width: DEFAULT_COLUMN_PT,
            panels,
        }
    }
}

/// Where a dragged panel would land, the one value the host's hit-test
/// produces and [`DockLayout::move_panel`] consumes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DropTarget {
    /// Insert into an existing column, at row `row` (0 = above everything).
    Into {
        /// Target rail.
        side: DockSide,
        /// Target column index within that rail.
        column: usize,
        /// Insertion row within the column.
        row: usize,
    },
    /// Split off a brand-new column at visual index `at` in `side`.
    NewColumn {
        /// Target rail.
        side: DockSide,
        /// Visual index the new column takes (existing columns shift right).
        at: usize,
    },
}

/// The dock layout: the two rails' columns.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DockLayout {
    left: Vec<DockColumn>,
    right: Vec<DockColumn>,
}

impl DockLayout {
    /// The shipped default: every panel in one right-hand column.
    pub fn single_right_column(panels: impl IntoIterator<Item = PanelId>) -> DockLayout {
        let panels: Vec<PanelId> = panels.into_iter().collect();
        DockLayout {
            left: Vec::new(),
            right: if panels.is_empty() {
                Vec::new()
            } else {
                vec![DockColumn::new(panels)]
            },
        }
    }

    /// One rail's columns, in visual left-to-right order.
    pub fn columns(&self, side: DockSide) -> &[DockColumn] {
        match side {
            DockSide::Left => &self.left,
            DockSide::Right => &self.right,
        }
    }

    fn columns_mut(&mut self, side: DockSide) -> &mut Vec<DockColumn> {
        match side {
            DockSide::Left => &mut self.left,
            DockSide::Right => &mut self.right,
        }
    }

    /// Where `id` currently sits: `(side, column, row)`.
    pub fn find(&self, id: PanelId) -> Option<(DockSide, usize, usize)> {
        for side in DockSide::ALL {
            for (c, column) in self.columns(side).iter().enumerate() {
                if let Some(r) = column.panels.iter().position(|p| *p == id) {
                    return Some((side, c, r));
                }
            }
        }
        None
    }

    /// Sets one column's width, clamped to the column band. Returns whether
    /// anything changed.
    pub fn set_width(&mut self, side: DockSide, column: usize, width: f32) -> bool {
        let clamped = width.clamp(MIN_COLUMN_PT, MAX_COLUMN_PT);
        match self.columns_mut(side).get_mut(column) {
            Some(col) if col.width != clamped => {
                col.width = clamped;
                true
            }
            _ => false,
        }
    }

    /// Total width of one rail's columns (excluding the host's splitter
    /// hairlines, which the host adds).
    pub fn side_width(&self, side: DockSide) -> f32 {
        self.columns(side).iter().map(|c| c.width).sum()
    }

    /// Moves `id` to `target`, pruning a source column left empty. Returns
    /// whether the layout actually changed (a drop back where the panel
    /// already was is a no-op, and must not dirty the prefs file).
    pub fn move_panel(&mut self, id: PanelId, target: DropTarget) -> bool {
        let origin = self.find(id);

        // Dropping a panel back into its own slot (or the slot just below
        // it, which is the same position) changes nothing.
        if let (Some((os, oc, or)), DropTarget::Into { side, column, row }) = (origin, target) {
            if os == side && oc == column && (row == or || row == or + 1) {
                return false;
            }
        }

        let before = self.clone();

        if let Some((side, column, row)) = origin {
            self.columns_mut(side)[column].panels.remove(row);
        }

        // Prune a source column the move emptied, and shift any target
        // index that sat after it.
        let mut target = target;
        if let Some((side, column, _)) = origin {
            if self.columns(side)[column].panels.is_empty() {
                self.columns_mut(side).remove(column);
                target = shift_after_prune(target, side, column);
            }
        }

        match target {
            DropTarget::Into { side, column, row } => {
                match self.columns_mut(side).get_mut(column) {
                    Some(col) => {
                        let row = row.min(col.panels.len());
                        col.panels.insert(row, id);
                    }
                    // The target column disappeared under us (only reachable
                    // if it was the pruned source column): re-create it there.
                    None => {
                        let at = column.min(self.columns(side).len());
                        self.columns_mut(side).insert(at, DockColumn::new(vec![id]));
                    }
                }
            }
            DropTarget::NewColumn { side, at } => {
                let at = at.min(self.columns(side).len());
                self.columns_mut(side).insert(at, DockColumn::new(vec![id]));
            }
        }

        *self != before
    }

    /// Reconciles a loaded layout with the panels this build registers:
    /// unknown ids dropped, duplicates collapsed, empty columns pruned,
    /// never-mentioned panels appended to the last right-hand column (see
    /// the module docs). Returns whether anything had to be fixed up.
    pub fn sync_registered(&mut self, registered: &[PanelId]) -> bool {
        let before = self.clone();
        let known: HashSet<PanelId> = registered.iter().copied().collect();
        let mut seen: HashSet<PanelId> = HashSet::new();

        for side in DockSide::ALL {
            for column in self.columns_mut(side) {
                column
                    .panels
                    .retain(|p| known.contains(p) && seen.insert(*p));
                column.width = column.width.clamp(MIN_COLUMN_PT, MAX_COLUMN_PT);
            }
            self.columns_mut(side).retain(|c| !c.panels.is_empty());
        }

        let missing: Vec<PanelId> = registered
            .iter()
            .copied()
            .filter(|p| !seen.contains(p))
            .collect();
        if !missing.is_empty() {
            if self.right.is_empty() {
                self.right.push(DockColumn::new(missing));
            } else {
                let last = self.right.len() - 1;
                self.right[last].panels.extend(missing);
            }
        }

        *self != before
    }

    /// Serializes to the machine-prefs shape (`prefs.toml`).
    pub fn to_prefs(
        &self,
        solo: bool,
        collapsed: Vec<String>,
        hidden: Vec<String>,
    ) -> PanelLayoutPrefs {
        fn columns(cols: &[DockColumn]) -> Vec<PanelColumnPrefs> {
            cols.iter()
                .map(|c| PanelColumnPrefs {
                    width_pt: c.width,
                    panels: c.panels.iter().map(|p| p.0.to_owned()).collect(),
                })
                .collect()
        }
        PanelLayoutPrefs {
            left: columns(&self.left),
            right: columns(&self.right),
            solo,
            collapsed,
            hidden,
        }
    }

    /// Rebuilds from `prefs.toml`, resolving id strings against the panels
    /// this build registers. Always run through [`Self::sync_registered`],
    /// so the result is valid whatever the file said.
    pub fn from_prefs(prefs: &PanelLayoutPrefs, registered: &[PanelId]) -> DockLayout {
        fn columns(src: &[PanelColumnPrefs], registered: &[PanelId]) -> Vec<DockColumn> {
            src.iter()
                .map(|c| DockColumn {
                    width: if c.width_pt.is_finite() {
                        c.width_pt.clamp(MIN_COLUMN_PT, MAX_COLUMN_PT)
                    } else {
                        DEFAULT_COLUMN_PT
                    },
                    panels: c
                        .panels
                        .iter()
                        .filter_map(|name| {
                            registered.iter().copied().find(|p| p.0 == name.as_str())
                        })
                        .collect(),
                })
                .collect()
        }
        let mut layout = DockLayout {
            left: columns(&prefs.left, registered),
            right: columns(&prefs.right, registered),
        };
        layout.sync_registered(registered);
        layout
    }
}

/// Re-indexes a [`DropTarget`] after the column at `(side, pruned)` was
/// removed: anything that pointed past it shifts one to the left.
fn shift_after_prune(target: DropTarget, side: DockSide, pruned: usize) -> DropTarget {
    match target {
        DropTarget::Into {
            side: s,
            column,
            row,
        } if s == side && column > pruned => DropTarget::Into {
            side: s,
            column: column - 1,
            row,
        },
        DropTarget::NewColumn { side: s, at } if s == side && at > pruned => {
            DropTarget::NewColumn {
                side: s,
                at: at - 1,
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PanelId = PanelId("test.a");
    const B: PanelId = PanelId("test.b");
    const C: PanelId = PanelId("test.c");

    fn default_layout() -> DockLayout {
        DockLayout::single_right_column([A, B, C])
    }

    /// The shipped default is the pre-docking rail: one right column, every
    /// panel in registration order, no left rail.
    #[test]
    fn default_is_one_right_column_in_registration_order() {
        let layout = default_layout();
        assert!(layout.columns(DockSide::Left).is_empty());
        assert_eq!(layout.columns(DockSide::Right).len(), 1);
        assert_eq!(layout.columns(DockSide::Right)[0].panels, vec![A, B, C]);
        assert_eq!(layout.find(C), Some((DockSide::Right, 0, 2)));
    }

    /// Dragging to the other rail creates that rail's first column and
    /// leaves the source column with the rest.
    #[test]
    fn moving_to_the_empty_left_rail_creates_a_column_there() {
        let mut layout = default_layout();
        assert!(layout.move_panel(
            B,
            DropTarget::NewColumn {
                side: DockSide::Left,
                at: 0
            }
        ));
        assert_eq!(layout.columns(DockSide::Left).len(), 1);
        assert_eq!(layout.columns(DockSide::Left)[0].panels, vec![B]);
        assert_eq!(layout.columns(DockSide::Right)[0].panels, vec![A, C]);
    }

    /// A second right-hand column is exactly the same operation, this is
    /// the "more than one column" case.
    #[test]
    fn splitting_a_second_right_column_keeps_visual_order() {
        let mut layout = default_layout();
        assert!(layout.move_panel(
            A,
            DropTarget::NewColumn {
                side: DockSide::Right,
                at: 1
            }
        ));
        let right = layout.columns(DockSide::Right);
        assert_eq!(right.len(), 2);
        assert_eq!(
            right[0].panels,
            vec![B, C],
            "canvas-side column keeps the rest"
        );
        assert_eq!(right[1].panels, vec![A], "the new column lands outboard");
    }

    /// Emptying a column removes it, and a target index past the pruned
    /// column is re-based so the panel lands where the user aimed.
    #[test]
    fn emptying_a_column_prunes_it_and_rebases_the_target() {
        let mut layout = default_layout();
        layout.move_panel(
            A,
            DropTarget::NewColumn {
                side: DockSide::Right,
                at: 0,
            },
        );
        assert_eq!(layout.columns(DockSide::Right).len(), 2);

        // A is alone in right[0]; move it into right[1], right[0] must
        // vanish and the insert must still land in the (now only) column.
        assert!(layout.move_panel(
            A,
            DropTarget::Into {
                side: DockSide::Right,
                column: 1,
                row: 1
            }
        ));
        let right = layout.columns(DockSide::Right);
        assert_eq!(right.len(), 1);
        assert_eq!(right[0].panels, vec![B, A, C]);
    }

    /// Re-ordering inside one column works, and dropping a panel back on
    /// its own slot is a no-op (must not dirty `prefs.toml`).
    #[test]
    fn reordering_within_a_column_and_the_no_op_drop() {
        let mut layout = default_layout();
        assert!(layout.move_panel(
            C,
            DropTarget::Into {
                side: DockSide::Right,
                column: 0,
                row: 0
            }
        ));
        assert_eq!(layout.columns(DockSide::Right)[0].panels, vec![C, A, B]);

        for row in [0, 1] {
            assert!(
                !layout.move_panel(
                    C,
                    DropTarget::Into {
                        side: DockSide::Right,
                        column: 0,
                        row
                    }
                ),
                "dropping a panel on its own slot must report no change"
            );
            assert_eq!(layout.columns(DockSide::Right)[0].panels, vec![C, A, B]);
        }
    }

    /// The lone panel of a lone column dropped onto a new column beside
    /// itself must not duplicate it or lose it.
    #[test]
    fn a_lone_panel_split_into_a_new_column_survives() {
        let mut layout = DockLayout::single_right_column([A]);
        layout.move_panel(
            A,
            DropTarget::NewColumn {
                side: DockSide::Right,
                at: 1,
            },
        );
        assert_eq!(layout.columns(DockSide::Right).len(), 1);
        assert_eq!(layout.columns(DockSide::Right)[0].panels, vec![A]);
    }

    /// Widths clamp to the drag band.
    #[test]
    fn column_widths_clamp() {
        let mut layout = default_layout();
        assert!(layout.set_width(DockSide::Right, 0, 10_000.0));
        assert_eq!(layout.columns(DockSide::Right)[0].width, MAX_COLUMN_PT);
        assert!(layout.set_width(DockSide::Right, 0, 0.0));
        assert_eq!(layout.columns(DockSide::Right)[0].width, MIN_COLUMN_PT);
        assert!(
            !layout.set_width(DockSide::Right, 7, 300.0),
            "no such column"
        );
    }

    /// A `prefs.toml` round trip preserves rails, order and widths.
    #[test]
    fn prefs_round_trip() {
        let mut layout = default_layout();
        layout.move_panel(
            B,
            DropTarget::NewColumn {
                side: DockSide::Left,
                at: 0,
            },
        );
        layout.set_width(DockSide::Left, 0, 320.0);

        let prefs = layout.to_prefs(true, vec!["test.a".to_owned()], Vec::new());
        assert!(prefs.solo);
        assert_eq!(prefs.left[0].width_pt, 320.0);

        let restored = DockLayout::from_prefs(&prefs, &[A, B, C]);
        assert_eq!(restored, layout);
    }

    /// Reload discipline: unknown ids vanish, duplicates collapse, empty
    /// columns are pruned, and a newly registered panel appears at the end
    /// of the last right-hand column rather than going missing.
    #[test]
    fn sync_registered_drops_unknowns_and_appends_new_panels() {
        let prefs = PanelLayoutPrefs {
            left: vec![PanelColumnPrefs {
                width_pt: 300.0,
                panels: vec!["test.b".to_owned(), "gone.panel".to_owned()],
            }],
            right: vec![
                PanelColumnPrefs {
                    width_pt: 260.0,
                    panels: vec!["retired.only".to_owned()],
                },
                PanelColumnPrefs {
                    width_pt: 260.0,
                    panels: vec!["test.a".to_owned(), "test.b".to_owned()],
                },
            ],
            solo: false,
            collapsed: Vec::new(),
            hidden: Vec::new(),
        };

        let layout = DockLayout::from_prefs(&prefs, &[A, B, C]);
        assert_eq!(layout.columns(DockSide::Left)[0].panels, vec![B]);
        assert_eq!(
            layout.columns(DockSide::Right).len(),
            1,
            "the all-unknown column is pruned"
        );
        assert_eq!(
            layout.columns(DockSide::Right)[0].panels,
            vec![A, C],
            "duplicate B kept its first (left-rail) placement; new C appended"
        );
    }

    /// A layout file from a build with no `panel_layout` key at all yields
    /// the shipped default once synced.
    #[test]
    fn empty_prefs_yield_the_shipped_default() {
        let layout = DockLayout::from_prefs(&PanelLayoutPrefs::default(), &[A, B, C]);
        assert_eq!(layout, default_layout());
    }
}
