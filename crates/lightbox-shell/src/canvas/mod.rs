// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The editor canvas (E08 Phase C, spec §6.4), generalizes the E01/F5
//! `loupe.rs` into a recipe-driven, progressive, stateful canvas. Module
//! split (spec §3 crate map: `loupe.rs` → `canvas/{view.rs, xform.rs,
//! gizmo.rs, states.rs}`):
//!
//! * [`xform`], [`ViewXform`], the image↔screen mapping (C1).
//! * [`view`], the zoom ladder/pan (C2), the [`view::RecipeSource`] seam +
//!   recipe-driven submit (C3), progressive tier→engine display (C4), and
//!   [`view::EditorCanvas`] itself, the `ui()` entry point `lib.rs` calls.
//! * [`states`], placards/shimmer/chips (C5).
//! * [`gizmo`], the Phase-F gizmo framework (spec §6.6): the `Gizmo`
//!   trait, `GizmoLayer` (input routing where a gizmo hit wins over
//!   pan/zoom, drag capture, paint pass, keymap sub-context), and the
//!   WB-eyedropper reference gizmo. New-gizmo authors (E11/E12): read
//!   `canvas/gizmo.md` first.
//! * [`crop_gizmo`], E11 geometry TOOL UI: the interactive crop rectangle
//!   (8 handles + move + rule-of-thirds + constrain-crop), built on `gizmo`.

pub mod crop_gizmo;
pub mod gizmo;
pub mod states;
pub mod view;
pub mod xform;

// Re-exports the names `lib.rs` actually spells (the app-facing seam).
// `view::RecipeSource`/`RecipeSnapshot`/`ZoomMode`, `xform::ViewXform`, and
// `states::CanvasPlacard` stay reachable at their full paths for callers
// that need to name them explicitly (Phase E's `EditBinding` adapter, the
// gizmo module's hit-testing) without an unused top-level re-export today.
pub use gizmo::{GizmoEffect, GizmoLayer};
pub use view::{ActiveEntry, CanvasContent, EditorCanvas, SessionRecipeSource};
