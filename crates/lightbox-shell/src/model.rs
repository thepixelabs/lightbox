// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The grid's data model: an incrementally (re)loaded image list over the
//! session's keyset-paginated queries, plus the M0 selection seed
//! (single + shift-range + toggle — spec T25; E08 owns the full culling
//! grammar).
//!
//! Loading is spread across frames — at most **one page query per frame**
//! (spec §5.1: `Session::query()` runs on the calling thread; a page is
//! ~single-digit ms at M0 scale, so the frame budget holds) — and
//! double-buffered: the visible list stays intact until the reload
//! completes, so the grid never flickers empty mid-refresh.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use lightbox_core::{ImageQuery, ImageSummary, PageCursor, Session, SortOrder};
use lightbox_types::ImageId;

/// Page size per pump step. 512 rows keeps a full 10 k reload around ~20
/// frames while staying well inside the per-frame query budget.
const PAGE_LIMIT: u32 = 512;

/// The image list shown by grid + loupe.
pub struct ImageListModel {
    rows: Vec<ImageSummary>,
    index_by_id: HashMap<ImageId, usize>,
    reload: Option<Reload>,
    dirty: bool,
    last_throttled: Option<Instant>,
}

struct Reload {
    collected: Vec<ImageSummary>,
    cursor: Option<PageCursor>,
}

impl ImageListModel {
    /// Starts dirty: the first pump kicks off the initial load.
    pub fn new() -> ImageListModel {
        ImageListModel {
            rows: Vec::new(),
            index_by_id: HashMap::new(),
            reload: None,
            dirty: true,
            last_throttled: None,
        }
    }

    /// The current (last fully loaded) rows, in catalog sort order.
    pub fn rows(&self) -> &[ImageSummary] {
        &self.rows
    }

    /// Index of an image in [`Self::rows`].
    pub fn index_of(&self, id: ImageId) -> Option<usize> {
        self.index_by_id.get(&id).copied()
    }

    /// Request a reload (coalesced; at most one runs at a time).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Throttled [`Self::mark_dirty`] for high-frequency triggers
    /// (`ImportProgress` — keeps the grid growing during an import without
    /// re-querying at event rate).
    pub fn mark_dirty_throttled(&mut self, min_interval: Duration) {
        let due = self
            .last_throttled
            .is_none_or(|at| at.elapsed() >= min_interval);
        if due {
            self.last_throttled = Some(Instant::now());
            self.dirty = true;
        }
    }

    /// True while a reload is in flight (callers keep repainting).
    pub fn loading(&self) -> bool {
        self.reload.is_some() || self.dirty
    }

    /// One pump step per frame: starts a pending reload or fetches at most
    /// one page. Returns `true` when `rows()` changed this step.
    pub fn pump(&mut self, session: &Session) -> bool {
        if self.reload.is_none() {
            if !self.dirty {
                return false;
            }
            self.dirty = false;
            self.reload = Some(Reload {
                collected: Vec::new(),
                cursor: None,
            });
        }

        let reload = self.reload.as_mut().expect("reload in progress");
        let page = match session.query().images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::CaptureTimeAsc,
            cursor: reload.cursor.take(),
            limit: PAGE_LIMIT,
        }) {
            Ok(page) => page,
            Err(err) => {
                tracing::warn!(target: "lightbox_shell", %err, "image page query failed");
                self.reload = None;
                return false;
            }
        };
        reload.collected.extend(page.items);
        match page.next {
            Some(next) => {
                reload.cursor = Some(next);
                false
            }
            None => {
                let done = self.reload.take().expect("reload in progress");
                self.rows = done.collected;
                self.index_by_id = self
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.id, i))
                    .collect();
                true
            }
        }
    }
}

/// Click modifiers relevant to selection.
#[derive(Copy, Clone, Debug, Default)]
pub struct ClickMods {
    /// Range-select from the anchor.
    pub shift: bool,
    /// Toggle membership (Cmd on macOS, Ctrl elsewhere).
    pub command: bool,
}

/// Selection seed (spec T25: single + shift-range; E08 grows the grammar).
#[derive(Default)]
pub struct Selection {
    selected: HashSet<ImageId>,
    anchor: Option<ImageId>,
    focus: Option<ImageId>,
}

impl Selection {
    /// Applies a click on `rows[idx]`.
    pub fn click(&mut self, rows: &[ImageSummary], idx: usize, mods: ClickMods) {
        let Some(id) = rows.get(idx).map(|s| s.id) else {
            return;
        };
        if mods.shift {
            // Range from the anchor (falling back to the clicked cell).
            let a = self
                .anchor
                .and_then(|a| rows.iter().position(|s| s.id == a))
                .unwrap_or(idx);
            let (lo, hi) = (a.min(idx), a.max(idx));
            self.selected = rows[lo..=hi].iter().map(|s| s.id).collect();
            self.focus = Some(id);
        } else if mods.command {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
            self.anchor = Some(id);
            self.focus = Some(id);
        } else {
            self.selected.clear();
            self.selected.insert(id);
            self.anchor = Some(id);
            self.focus = Some(id);
        }
    }

    /// Focus + select exactly `id` (loupe navigation, smoke driver).
    pub fn set_focus(&mut self, id: ImageId) {
        self.selected.clear();
        self.selected.insert(id);
        self.anchor = Some(id);
        self.focus = Some(id);
    }

    /// Whether `id` is in the selection.
    pub fn is_selected(&self, id: ImageId) -> bool {
        self.selected.contains(&id)
    }

    /// The focused image, if any.
    pub fn focus(&self) -> Option<ImageId> {
        self.focus
    }

    /// Number of selected images.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// True when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_types::{AssetId, Flag, Orientation};

    fn rows(n: usize) -> Vec<ImageSummary> {
        (0..n)
            .map(|i| ImageSummary {
                id: ImageId(i as i64 + 1),
                asset: AssetId(i as i64 + 1),
                filename: format!("img-{i:03}.jpg"),
                capture_time: None,
                rating: None,
                flag: Flag::None,
                orientation: Orientation::O1,
                width: 100,
                height: 80,
                missing: false,
                decode_error: false,
            })
            .collect()
    }

    const PLAIN: ClickMods = ClickMods {
        shift: false,
        command: false,
    };
    const SHIFT: ClickMods = ClickMods {
        shift: true,
        command: false,
    };
    const CMD: ClickMods = ClickMods {
        shift: false,
        command: true,
    };

    #[test]
    fn plain_click_selects_single_and_sets_anchor_focus() {
        let rows = rows(5);
        let mut sel = Selection::default();
        sel.click(&rows, 2, PLAIN);
        assert!(sel.is_selected(ImageId(3)));
        assert_eq!(sel.len(), 1);
        assert_eq!(sel.focus(), Some(ImageId(3)));

        sel.click(&rows, 4, PLAIN);
        assert!(!sel.is_selected(ImageId(3)), "previous selection replaced");
        assert!(sel.is_selected(ImageId(5)));
        assert_eq!(sel.len(), 1);
    }

    #[test]
    fn shift_click_selects_range_from_anchor_in_both_directions() {
        let rows = rows(6);
        let mut sel = Selection::default();
        sel.click(&rows, 1, PLAIN);
        sel.click(&rows, 4, SHIFT);
        assert_eq!(sel.len(), 4, "rows 1..=4 selected");
        for (i, row) in rows.iter().enumerate().take(5).skip(1) {
            assert!(sel.is_selected(row.id), "row {i}");
        }
        assert_eq!(sel.focus(), Some(rows[4].id));

        // Backwards range from the SAME anchor.
        sel.click(&rows, 0, SHIFT);
        assert_eq!(sel.len(), 2, "rows 0..=1 selected");
        assert!(sel.is_selected(rows[0].id) && sel.is_selected(rows[1].id));
    }

    #[test]
    fn shift_click_without_anchor_selects_single() {
        let rows = rows(3);
        let mut sel = Selection::default();
        sel.click(&rows, 2, SHIFT);
        assert_eq!(sel.len(), 1);
        assert!(sel.is_selected(rows[2].id));
    }

    #[test]
    fn command_click_toggles_membership() {
        let rows = rows(4);
        let mut sel = Selection::default();
        sel.click(&rows, 0, PLAIN);
        sel.click(&rows, 2, CMD);
        assert_eq!(sel.len(), 2);
        sel.click(&rows, 2, CMD);
        assert_eq!(sel.len(), 1, "second toggle removes");
        assert!(sel.is_selected(rows[0].id));
    }

    #[test]
    fn click_out_of_bounds_is_a_noop() {
        let rows = rows(2);
        let mut sel = Selection::default();
        sel.click(&rows, 7, PLAIN);
        assert!(sel.is_empty());
        assert_eq!(sel.focus(), None);
    }

    #[test]
    fn throttled_dirty_coalesces() {
        let mut model = ImageListModel::new();
        model.dirty = false;
        model.mark_dirty_throttled(Duration::from_secs(3600));
        assert!(model.dirty, "first throttled mark fires");
        model.dirty = false;
        model.mark_dirty_throttled(Duration::from_secs(3600));
        assert!(!model.dirty, "second mark inside the window is coalesced");
    }
}
