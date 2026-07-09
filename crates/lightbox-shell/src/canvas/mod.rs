// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The editor canvas (E08 Phase C, spec §6.4) — generalizes the E01/F5
//! `loupe.rs` into a recipe-driven, progressive, stateful canvas. Module
//! split (spec §3 crate map: `loupe.rs` → `canvas/{view.rs, xform.rs,
//! gizmo.rs, states.rs}`):
//!
//! * [`xform`] — [`ViewXform`], the image↔screen mapping (C1).
//! * [`view`] — the zoom ladder/pan (C2), the [`view::RecipeSource`] seam +
//!   recipe-driven submit (C3), progressive tier→engine display (C4), and
//!   [`view::EditorCanvas`] itself — the `ui()` entry point `lib.rs` calls.
//! * [`states`] — placards/shimmer/chips (C5).
//! * [`gizmo`] — Phase F's seam (a stub — see its module docs).

pub mod gizmo;
pub mod states;
pub mod view;
pub mod xform;

// Re-exports the names `lib.rs` actually spells (the app-facing seam).
// `view::RecipeSource`/`RecipeSnapshot`/`ZoomMode`, `xform::ViewXform`, and
// `states::CanvasPlacard` stay reachable at their full paths for callers
// that need to name them explicitly (Phase E's `EditBinding` adapter,
// Phase F's gizmo hit-testing) without an unused top-level re-export today.
pub use view::{ActiveEntry, CanvasAction, CanvasContent, EditorCanvas, SessionRecipeSource};
