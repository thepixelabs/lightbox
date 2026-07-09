// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Drop/hover intake (E08 spec §6.1, task A2): folds one frame's raw egui
//! input into an intake decision — a hover affordance while files are
//! dragged over the window, and (on the frame a drop lands) exactly **one**
//! [`lightbox_core::OpenRequest`] in drop order.
//!
//! **A0 seam simplification (recorded in `E08-deviations.md`):** the spec
//! draft proposes shell-local `IntakeSource`/`OpenRequest` types mirroring
//! E04's. Those already exist, shipped, as `lightbox_core::{OpenOrigin,
//! OpenRequest}` (re-exported off `lightbox-ingest`) — `OpenRequest` is
//! `#[non_exhaustive]` with a `::new(paths, recursive, origin)` constructor,
//! and `OpenOrigin` already has `DragDrop`/`OpenDialog`/`FileAssociation`/
//! `Cli` variants that cover every intake source. Rather than define a
//! redundant parallel type, [`pump`] constructs the real, E04-owned
//! `OpenRequest` directly — one `Command::OpenWorkingSet { request }` away
//! from being submitted.

use eframe::egui;
use lightbox_core::{OpenOrigin, OpenRequest};

/// Hover affordance state while files are dragged over the window, before
/// the drop lands (§6.1). `count` is `None` on platforms that report a
/// hover without paths/counts (graceful degradation, spec R4).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HoverAffordance {
    /// Number of files currently hovering, when the platform reports it.
    pub count: Option<usize>,
}

/// One frame's intake decision.
#[derive(Default)]
pub struct IntakeFrame {
    /// `Some` while files hover the window this frame.
    pub hover: Option<HoverAffordance>,
    /// `Some` on the frame a drop landed — exactly one per drop (A2 AC).
    pub open: Option<OpenRequest>,
    /// Dropped entries this frame with no resolvable path (e.g. a
    /// bytes-only web-backend drop) — never silently lost, counted for the
    /// status line (§6.1: "skipped and counted").
    pub skipped_pathless: usize,
}

/// Folds this frame's raw egui input into an [`IntakeFrame`] (§6.1).
///
/// * `dropped_files` with a path become one `OpenRequest` in drop order
///   (gesture order — the order egui/winit hands them back); entries
///   without a path are skipped and counted, never silently dropped.
/// * `recursive` reflects the Alt modifier held at drop time, or
///   `prefs_recursive_default` when Alt is not held (§2.4; the prefs store
///   itself is Phase G — Phase A callers pass a fixed default).
/// * `hovered_files` drives the hover affordance shown *before* the drop
///   lands.
pub fn pump(input: &egui::InputState, prefs_recursive_default: bool) -> IntakeFrame {
    let mut frame = IntakeFrame::default();

    if !input.raw.hovered_files.is_empty() {
        let all_paths_known = input.raw.hovered_files.iter().all(|f| f.path.is_some());
        frame.hover = Some(HoverAffordance {
            count: all_paths_known.then_some(input.raw.hovered_files.len()),
        });
    }

    if !input.raw.dropped_files.is_empty() {
        let mut paths = Vec::with_capacity(input.raw.dropped_files.len());
        for f in &input.raw.dropped_files {
            match &f.path {
                Some(p) => paths.push(p.clone()),
                None => frame.skipped_pathless += 1,
            }
        }
        if !paths.is_empty() {
            let recursive = input.modifiers.alt || prefs_recursive_default;
            frame.open = Some(OpenRequest::new(paths, recursive, OpenOrigin::DragDrop));
        }
    }

    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn raw_input(
        dropped: Vec<egui::DroppedFile>,
        hovered: Vec<egui::HoveredFile>,
        alt: bool,
    ) -> egui::RawInput {
        egui::RawInput {
            dropped_files: dropped,
            hovered_files: hovered,
            modifiers: egui::Modifiers {
                alt,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn input_state(raw: egui::RawInput) -> egui::InputState {
        let ctx = egui::Context::default();
        // One begin_pass with our RawInput is enough to observe it back via
        // `ctx.input(|i| ...)`.
        ctx.begin_pass(raw);
        ctx.input(|i| i.clone())
    }

    fn dropped(path: &str) -> egui::DroppedFile {
        egui::DroppedFile {
            path: Some(PathBuf::from(path)),
            ..Default::default()
        }
    }

    fn pathless_drop() -> egui::DroppedFile {
        egui::DroppedFile {
            path: None,
            name: "pasted.jpg".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn no_files_yields_an_empty_frame() {
        let input = input_state(raw_input(vec![], vec![], false));
        let frame = pump(&input, false);
        assert!(frame.hover.is_none());
        assert!(frame.open.is_none());
        assert_eq!(frame.skipped_pathless, 0);
    }

    #[test]
    fn hover_reflects_count_before_drop_lands() {
        let input = input_state(raw_input(
            vec![],
            vec![
                egui::HoveredFile {
                    path: Some(PathBuf::from("/a.cr3")),
                    ..Default::default()
                },
                egui::HoveredFile {
                    path: Some(PathBuf::from("/b.jpg")),
                    ..Default::default()
                },
            ],
            false,
        ));
        let frame = pump(&input, false);
        assert_eq!(frame.hover, Some(HoverAffordance { count: Some(2) }));
        assert!(frame.open.is_none(), "hover alone never opens");
    }

    #[test]
    fn hover_without_known_paths_degrades_to_a_count_none_highlight() {
        let input = input_state(raw_input(
            vec![],
            vec![egui::HoveredFile {
                path: None,
                mime: "image/jpeg".to_owned(),
            }],
            false,
        ));
        let frame = pump(&input, false);
        assert_eq!(frame.hover, Some(HoverAffordance { count: None }));
    }

    #[test]
    fn drop_yields_exactly_one_open_request_in_gesture_order() {
        let input = input_state(raw_input(
            vec![dropped("/photos/b.jpg"), dropped("/photos/a-folder")],
            vec![],
            false,
        ));
        let frame = pump(&input, false);
        let open = frame.open.expect("one OpenRequest");
        assert_eq!(
            open.paths,
            vec![
                PathBuf::from("/photos/b.jpg"),
                PathBuf::from("/photos/a-folder")
            ],
            "drop order preserved, not sorted"
        );
        assert_eq!(open.origin, lightbox_core::OpenOrigin::DragDrop);
        assert!(!open.recursive);
    }

    #[test]
    fn alt_held_at_drop_time_sets_recursive() {
        let input = input_state(raw_input(vec![dropped("/photos")], vec![], true));
        let frame = pump(&input, false);
        assert!(frame.open.expect("open").recursive);
    }

    #[test]
    fn prefs_default_sets_recursive_without_alt() {
        let input = input_state(raw_input(vec![dropped("/photos")], vec![], false));
        let frame = pump(&input, true);
        assert!(frame.open.expect("open").recursive);
    }

    #[test]
    fn pathless_entries_are_skipped_and_counted_not_silently_lost() {
        let input = input_state(raw_input(
            vec![dropped("/photos/a.jpg"), pathless_drop()],
            vec![],
            false,
        ));
        let frame = pump(&input, false);
        let open = frame.open.expect("one path resolved");
        assert_eq!(open.paths, vec![PathBuf::from("/photos/a.jpg")]);
        assert_eq!(frame.skipped_pathless, 1);
    }

    #[test]
    fn an_all_pathless_drop_opens_nothing_but_still_counts() {
        let input = input_state(raw_input(vec![pathless_drop()], vec![], false));
        let frame = pump(&input, false);
        assert!(frame.open.is_none());
        assert_eq!(frame.skipped_pathless, 1);
    }
}
