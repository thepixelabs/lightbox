// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The on-canvas gizmo layer — Phase F's module (E08 spec §6.6). Phase C
//! leaves only the seam: a stable mounting point (`GizmoLayer`) so
//! `EditorCanvas`'s call sites don't have to change shape again when
//! Phase F lands the real `Gizmo` trait, hit routing, and the WB-eyedropper
//! reference gizmo. `EditorCanvas::ui` does not take a `&mut GizmoLayer`
//! parameter yet — there is no consumer (E11/E12 land even later) and no
//! keymap sub-context to push, so a functional stub here would just be
//! dead code that Phase F would need to rewrite anyway.
//!
//! Phase F builds, per spec §6.6/§8 F1-F3:
//! * `GizmoId`/`HitId`/`GizmoEvent`/`GizmoEffect`/the `Gizmo` trait,
//! * `GizmoLayer` proper — active-gizmo stack, input routing (a gizmo hit
//!   wins over pan/zoom), drag capture, a paint pass above the composited
//!   image, and keymap sub-context push/pop,
//! * the WB-eyedropper reference gizmo + `canvas/gizmo.md` (the frozen
//!   interaction conventions + worked example E11/E12 build against).

/// Placeholder for Phase F's active-gizmo stack (spec §6.6). Empty by
/// design — see the module docs for why a functional stub isn't built
/// ahead of a consumer. `#[allow(dead_code)]`: nothing constructs this yet
/// (`EditorCanvas::ui` gains a `&mut GizmoLayer` parameter in Phase F) —
/// the same "reserved seam" posture `filmstrip::FilmstripState::selected`
/// already carries in this crate.
#[derive(Default)]
#[allow(dead_code)]
pub struct GizmoLayer {
    _private: (),
}

#[allow(dead_code)]
impl GizmoLayer {
    /// An empty layer (no active gizmo).
    pub fn new() -> GizmoLayer {
        GizmoLayer::default()
    }
}
