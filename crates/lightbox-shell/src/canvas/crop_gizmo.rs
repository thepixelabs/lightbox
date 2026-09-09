// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `geom.crop` tool, the interactive crop/straighten-aware crop rectangle
//! (E11 geometry TOOL UI slice, `canvas/gizmo.md` §6's checklist). Models
//! [`crate::canvas::gizmo::WbEyedropper`]'s shape: a plain struct
//! implementing [`Gizmo`], mounted by `panels::geometry`'s tool button /
//! aspect-preset buttons through `DevelopCtx.gizmos`, same as the
//! eyedropper.
//!
//! # Geometry storage: normalized fractions, not raw pixels
//!
//! `canvas/gizmo.md` convention 1 recommends storing gizmo geometry in raw
//! image pixels. This gizmo deliberately stores its rectangle as
//! **normalized `[0,1]` fractions**, [`lightbox_edit::Crop`]'s own
//! domain, instead: it is the value that is actually committed
//! (`ParamId::Crop`), it is exactly as zoom/pan/resize-stable as a pixel
//! rect (a fraction of the image is a fraction of the image at any zoom),
//! and storing it this way means every interaction computation ends in the
//! same units the committed [`ParamValue::Crop`] delta needs, with no
//! pixel↔fraction conversion drift accumulating across a drag.
//!
//! # Lazy rect resolution (`Seed`)
//!
//! At construction (a panel button click) there is no [`ViewXform`] yet
//! only [`Gizmo::on_event`]/[`Gizmo::hit`]/[`Gizmo::paint`] ever receive one.
//! A fresh gizmo therefore starts in [`Seed::Explicit`] (re-entering the
//! tool: seed from the currently committed crop) or [`Seed::AutoFit`] (an
//! aspect-preset click: seed to the largest achievable rect of that aspect).
//! [`CropGizmo::ensure_resolved`] materializes `self.rect` the first time a
//! real `xf` is available, always the `Hover` event, since
//! `GizmoLayer::route` dispatches `Hover` before any `hit()`/`DragStart`
//! call in the same routed frame (`gizmo.rs`'s own routing order), so
//! `self.rect` is guaranteed valid before hit-testing, dragging, or
//! painting ever consult it.
//!
//! # Constrain-crop (straighten-aware bound)
//!
//! `self.angle_deg` (the recipe's straighten angle at activation, this
//! gizmo never edits `ParamId::Angle` itself, only reads it) feeds
//! [`lightbox_render::ng::warp::largest_inscribed_rect`] over
//! [`WarpField::warped_src_polygon`] (task E9's engine helper, see that
//! module's docs) to compute the safe-crop bound: every drag result is
//! clamped into this bound, and an [`Seed::AutoFit`] seed IS this bound
//! (optionally aspect-locked). This keeps the crop rectangle inside the
//! rotated image's actual valid content whenever a straighten angle is
//! active, per the task brief.
//!
//! # What this gizmo does NOT do
//!
//! No drag-outside-to-straighten (optional per the task brief, the
//! existing `ParamId::Angle` slider already covers straighten, and
//! layering a second straighten gesture onto this same drag surface adds
//! real complexity for a marked-optional affordance). No 90°
//! rotate/orientation handling (`lightbox-edit`'s `Flip` has no 90°-step
//! variant to drive, see `docs/plan/epics/E11-deviations.md` E-scope-3 and
//! this phase's own added section).

use eframe::egui;
use lightbox_edit::{Crop, ParamDelta, ParamId, ParamValue};
use lightbox_render::ng::warp::{largest_inscribed_rect, WarpField};
use lightbox_render::ng::Extent;

use crate::canvas::gizmo::{
    Gizmo, GizmoEffect, GizmoEvent, GizmoId, GizmoPaintCtx, HitId, HIT_TOLERANCE_PT,
};
use crate::canvas::xform::ViewXform;
use crate::keymap::ContextId;

/// The crop gizmo's stable identity (`canvas/gizmo.md`'s suggested naming).
pub const GEOM_CROP: GizmoId = GizmoId("geom.crop");

/// Its keymap sub-context. No action targets it yet (Esc/Enter resolve
/// generically via `CTX_GIZMO`, same as the eyedropper), reserved per
/// `gizmo.md` §3 for any future crop-specific chord.
const CTX_GEOM_CROP: ContextId = ContextId("editor.gizmo.geom_crop");

/// Minimum crop dimension, normalized fraction, guards against a
/// degenerate zero-area rect mid-drag.
const MIN_FRAC: f32 = 0.02;

/// What aspect a [`CropGizmo`] locks its rectangle to (panel's aspect-preset
/// row). Resolved to a concrete `width/height` ratio against a real
/// [`ViewXform`] (`Original` needs the actual image dimensions, which are
/// only known once a frame has routed).
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum AspectPreset {
    /// No lock.
    Free,
    /// The source image's own aspect ratio.
    Original,
    /// A fixed `width / height` ratio.
    Ratio(f32),
}

/// How `self.rect` is seeded (see the module docs' "Lazy rect resolution").
enum Seed {
    /// Re-entering the tool: use this already-committed crop verbatim
    /// (clamped into the straighten-aware bound).
    Explicit(Crop),
    /// An aspect-preset click: seed to the largest rect achieving the
    /// gizmo's `aspect`, constrained by `angle_deg`.
    AutoFit,
    /// Materialized, `self.rect` is now the live value.
    Resolved,
}

/// The 8 resize handles + the move (drag-inside-body) region. Discriminants
/// double as [`HitId`] wire values (`hit_id`/`from_hit`).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Handle {
    Tl,
    T,
    Tr,
    R,
    Br,
    B,
    Bl,
    L,
    Move,
}

impl Handle {
    fn hit_id(self) -> HitId {
        HitId(match self {
            Handle::Tl => 0,
            Handle::T => 1,
            Handle::Tr => 2,
            Handle::R => 3,
            Handle::Br => 4,
            Handle::B => 5,
            Handle::Bl => 6,
            Handle::L => 7,
            Handle::Move => 8,
        })
    }

    fn from_hit(h: HitId) -> Option<Handle> {
        Some(match h.0 {
            0 => Handle::Tl,
            1 => Handle::T,
            2 => Handle::Tr,
            3 => Handle::R,
            4 => Handle::Br,
            5 => Handle::B,
            6 => Handle::Bl,
            7 => Handle::L,
            8 => Handle::Move,
            _ => return None,
        })
    }
}

/// A captured drag: which handle, and the rect/pointer snapshot at
/// `DragStart`, every `Drag` event recomputes from this snapshot (not an
/// incremental per-frame delta), so float drift never accumulates across a
/// long drag.
struct DragState {
    handle: Handle,
    start_rect: Crop,
    start_image_px: egui::Vec2,
}

/// The interactive crop/straighten-aware crop rectangle (see module docs).
pub struct CropGizmo {
    rect: Crop,
    seed: Seed,
    /// The recipe's straighten angle at activation (degrees), read-only
    /// here; only `panels::basic`-style `ParamId::Angle` slider edits it.
    angle_deg: f32,
    aspect: AspectPreset,
    drag: Option<DragState>,
}

impl CropGizmo {
    /// Seeds from the currently committed crop (the plain "Crop Tool"
    /// toggle, re-entering the tool never discards an existing crop).
    pub fn from_current(current: Crop, angle_deg: f32) -> CropGizmo {
        CropGizmo {
            rect: Crop::default(),
            seed: Seed::Explicit(current),
            angle_deg,
            aspect: AspectPreset::Free,
            drag: None,
        }
    }

    /// Seeds to the largest rect achieving `aspect`, respecting
    /// `angle_deg`'s constrain-crop bound (an aspect-preset button click).
    pub fn fit_aspect(aspect: AspectPreset, angle_deg: f32) -> CropGizmo {
        CropGizmo {
            rect: Crop::default(),
            seed: Seed::AutoFit,
            angle_deg,
            aspect,
            drag: None,
        }
    }

    #[cfg(test)]
    fn current_rect(&self) -> Crop {
        self.rect
    }

    /// `aspect`, resolved to a concrete ratio against `xf` (`Original` reads
    /// the real image dimensions; `Free` is `None`, meaning unconstrained).
    fn resolved_aspect(&self, xf: &ViewXform) -> Option<f64> {
        match self.aspect {
            AspectPreset::Free => None,
            AspectPreset::Ratio(r) if r > 0.0 => Some(r as f64),
            AspectPreset::Ratio(_) => None,
            AspectPreset::Original => {
                let s = xf.image_size_px();
                if s.x > 0.0 && s.y > 0.0 {
                    Some((s.x / s.y) as f64)
                } else {
                    None
                }
            }
        }
    }

    /// The largest inscribed rect (task E9's engine helper) for `aspect`
    /// under `self.angle_deg`, converted to normalized fractions, both the
    /// [`Seed::AutoFit`] seed value and the drag-clamp bound.
    fn inscribed(&self, xf: &ViewXform, aspect: Option<f64>) -> Crop {
        let ext = extent_from_xf(xf);
        let poly = WarpField::from_angle_deg(self.angle_deg as f64, ext).warped_src_polygon();
        let r = largest_inscribed_rect(&poly, aspect);
        Crop {
            left: (r.x / ext.w as f64) as f32,
            top: (r.y / ext.h as f64) as f32,
            right: ((r.x + r.w) / ext.w as f64) as f32,
            bottom: ((r.y + r.h) / ext.h as f64) as f32,
        }
    }

    /// Materializes `self.rect` from `self.seed` the first time a real `xf`
    /// is available (see the module docs). Idempotent, a no-op once
    /// already [`Seed::Resolved`].
    fn ensure_resolved(&mut self, xf: &ViewXform) {
        if matches!(self.seed, Seed::Resolved) {
            return;
        }
        let seed = std::mem::replace(&mut self.seed, Seed::Resolved);
        self.rect = match seed {
            // An arbitrary already-committed crop is clamped into the
            // ANGLE-only (aspect-free) bound: re-entering the tool must
            // never silently force an existing crop onto a locked aspect
            // it wasn't drawn with.
            Seed::Explicit(r) => clamp_into(r, &self.inscribed(xf, None)),
            Seed::AutoFit => {
                let aspect = self.resolved_aspect(xf);
                self.inscribed(xf, aspect)
            }
            Seed::Resolved => unreachable!("guarded above"),
        };
    }

    fn screen_rect(&self, xf: &ViewXform) -> egui::Rect {
        let img = xf.image_size_px();
        let tl = xf.image_to_screen(egui::vec2(self.rect.left * img.x, self.rect.top * img.y));
        let br = xf.image_to_screen(egui::vec2(
            self.rect.right * img.x,
            self.rect.bottom * img.y,
        ));
        egui::Rect::from_two_pos(tl, br)
    }

    fn handle_points(&self, xf: &ViewXform) -> [(Handle, egui::Pos2); 8] {
        let img = xf.image_size_px();
        let l = self.rect.left * img.x;
        let t = self.rect.top * img.y;
        let r = self.rect.right * img.x;
        let b = self.rect.bottom * img.y;
        let cx = (l + r) * 0.5;
        let cy = (t + b) * 0.5;
        [
            (Handle::Tl, xf.image_to_screen(egui::vec2(l, t))),
            (Handle::T, xf.image_to_screen(egui::vec2(cx, t))),
            (Handle::Tr, xf.image_to_screen(egui::vec2(r, t))),
            (Handle::R, xf.image_to_screen(egui::vec2(r, cy))),
            (Handle::Br, xf.image_to_screen(egui::vec2(r, b))),
            (Handle::B, xf.image_to_screen(egui::vec2(cx, b))),
            (Handle::Bl, xf.image_to_screen(egui::vec2(l, b))),
            (Handle::L, xf.image_to_screen(egui::vec2(l, cy))),
        ]
    }
}

fn extent_from_xf(xf: &ViewXform) -> Extent {
    let s = xf.image_size_px();
    Extent {
        w: s.x.round().max(1.0) as u32,
        h: s.y.round().max(1.0) as u32,
    }
}

/// Clamps `v` into `[lo, hi]`; when `lo > hi` (a bound narrower than the
/// value being clamped can legally range over, an edge case when the
/// straighten-aware bound shrinks below the dragged rect's own size)
/// returns the midpoint rather than panicking (`f32::clamp` requires
/// `lo <= hi`).
fn clamp_range(v: f32, lo: f32, hi: f32) -> f32 {
    if lo <= hi {
        v.clamp(lo, hi)
    } else {
        (lo + hi) * 0.5
    }
}

/// Intersects `r` with `bound` edge-by-edge, then restores [`MIN_FRAC`] if
/// the intersection degenerated. Not aspect-preserving in the extreme case
/// where `bound` is narrower than `r`'s own locked aspect would allow, an
/// accepted simplification (`docs/plan/epics/E11-deviations.md`).
fn clamp_into(mut r: Crop, bound: &Crop) -> Crop {
    let (lo_x, hi_x) = (bound.left.min(bound.right), bound.left.max(bound.right));
    let (lo_y, hi_y) = (bound.top.min(bound.bottom), bound.top.max(bound.bottom));
    r.left = r.left.clamp(lo_x, hi_x);
    r.right = r.right.clamp(lo_x, hi_x);
    r.top = r.top.clamp(lo_y, hi_y);
    r.bottom = r.bottom.clamp(lo_y, hi_y);
    if r.right - r.left < MIN_FRAC {
        let mid = (r.left + r.right) * 0.5;
        r.left = (mid - MIN_FRAC * 0.5).max(lo_x);
        r.right = (r.left + MIN_FRAC).min(hi_x);
    }
    if r.bottom - r.top < MIN_FRAC {
        let mid = (r.top + r.bottom) * 0.5;
        r.top = (mid - MIN_FRAC * 0.5).max(lo_y);
        r.bottom = (r.top + MIN_FRAC).min(hi_y);
    }
    r
}

/// The move-handle update: shifts `start` by `(dx, dy)`, clamped so the
/// shifted rect's size is preserved and it never leaves `bound`.
fn translate(start: &Crop, dx: f32, dy: f32, bound: &Crop) -> Crop {
    let (lo_x, hi_x) = (bound.left.min(bound.right), bound.left.max(bound.right));
    let (lo_y, hi_y) = (bound.top.min(bound.bottom), bound.top.max(bound.bottom));
    let dx = clamp_range(dx, lo_x - start.left, hi_x - start.right);
    let dy = clamp_range(dy, lo_y - start.top, hi_y - start.bottom);
    Crop {
        left: start.left + dx,
        top: start.top + dy,
        right: start.right + dx,
        bottom: start.bottom + dy,
    }
}

/// A resize-handle update from `start`, anchored at the handle's fixed
/// opposite point, evaluated at the new normalized pointer position
/// `point`. `aspect`, when set, is a **pixel** `width/height` ratio
/// (`img` converts it to the equivalent fraction-space ratio, a
/// normalized-fraction rect only has the same aspect as its pixel rect when
/// the image itself is square, so this conversion is not optional): corner
/// handles grow to cover the cursor while keeping the ratio exact; edge
/// handles derive the perpendicular dimension from the dragged one,
/// centered on the anchor.
fn resize(
    start: &Crop,
    handle: Handle,
    point: (f32, f32),
    aspect: Option<f32>,
    img: egui::Vec2,
) -> Crop {
    let (fx, fy) = point;
    let mut r = *start;
    match handle {
        Handle::Move => r,
        Handle::Tl | Handle::Tr | Handle::Br | Handle::Bl => {
            let (ax, ay) = match handle {
                Handle::Tl => (start.right, start.bottom),
                Handle::Tr => (start.left, start.bottom),
                Handle::Br => (start.left, start.top),
                Handle::Bl => (start.right, start.top),
                _ => unreachable!("corner handles only"),
            };
            let mut w = (fx - ax).abs().max(MIN_FRAC);
            let mut h = (fy - ay).abs().max(MIN_FRAC);
            if let Some(ar) = aspect {
                let ar = ar.max(1e-3);
                // Grow to cover the cursor point while keeping the PIXEL
                // ratio exact: whichever axis implies the larger rect wins.
                let w_px = w * img.x;
                let h_px = h * img.y;
                if w_px / h_px > ar {
                    h = (w_px / ar) / img.y;
                } else {
                    w = (h_px * ar) / img.x;
                }
            }
            let sx = if fx >= ax { 1.0 } else { -1.0 };
            let sy = if fy >= ay { 1.0 } else { -1.0 };
            let (nx, ny) = (ax + sx * w, ay + sy * h);
            r.left = ax.min(nx);
            r.right = ax.max(nx);
            r.top = ay.min(ny);
            r.bottom = ay.max(ny);
            r
        }
        Handle::T | Handle::B => {
            let ay = if handle == Handle::T {
                start.bottom
            } else {
                start.top
            };
            let h = (fy - ay).abs().max(MIN_FRAC);
            if let Some(ar) = aspect {
                let ar = ar.max(1e-3);
                let cx = (start.left + start.right) * 0.5;
                let w = (h * img.y * ar) / img.x;
                r.left = cx - w * 0.5;
                r.right = cx + w * 0.5;
            }
            let sy = if fy >= ay { 1.0 } else { -1.0 };
            let ny = ay + sy * h;
            r.top = ay.min(ny);
            r.bottom = ay.max(ny);
            r
        }
        Handle::L | Handle::R => {
            let ax = if handle == Handle::L {
                start.right
            } else {
                start.left
            };
            let w = (fx - ax).abs().max(MIN_FRAC);
            if let Some(ar) = aspect {
                let ar = ar.max(1e-3);
                let cy = (start.top + start.bottom) * 0.5;
                let h = (w * img.x / ar) / img.y;
                r.top = cy - h * 0.5;
                r.bottom = cy + h * 0.5;
            }
            let sx = if fx >= ax { 1.0 } else { -1.0 };
            let nx = ax + sx * w;
            r.left = ax.min(nx);
            r.right = ax.max(nx);
            r
        }
    }
}

fn crop_delta(rect: Crop) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Crop, ParamValue::Crop(rect));
    d
}

impl Gizmo for CropGizmo {
    fn id(&self) -> GizmoId {
        GEOM_CROP
    }

    /// Handle-style hit-testing (`gizmo.md` convention 2): the 8 handles at
    /// [`HIT_TOLERANCE_PT`], then body containment for the move handle.
    fn hit(&self, screen: egui::Pos2, xf: &ViewXform) -> Option<HitId> {
        if !matches!(self.seed, Seed::Resolved) {
            // Not yet materialized (never observed in practice, `Hover`
            // always routes first, but hit() takes `&self`, so this stays
            // a defensive `None` rather than a panic).
            return None;
        }
        for (h, p) in self.handle_points(xf) {
            if (p - screen).length() <= HIT_TOLERANCE_PT {
                return Some(h.hit_id());
            }
        }
        if self.screen_rect(xf).contains(screen) {
            return Some(Handle::Move.hit_id());
        }
        None
    }

    fn on_event(&mut self, ev: GizmoEvent, xf: &ViewXform, out: &mut Vec<GizmoEffect>) {
        // Cancel/Commit may arrive with a stale/degenerate `xf`
        // (`Gizmo::on_event`'s documented caveat), never derive geometry
        // from it on those two events.
        if !matches!(ev, GizmoEvent::Cancel | GizmoEvent::Commit) {
            self.ensure_resolved(xf);
        }
        match ev {
            GizmoEvent::Hover { .. } => {}
            GizmoEvent::DragStart { hit, image_px } => {
                let Some(handle) = Handle::from_hit(hit) else {
                    return;
                };
                self.drag = Some(DragState {
                    handle,
                    start_rect: self.rect,
                    start_image_px: image_px,
                });
                // gizmo.md convention 5: one drag = one EditBinding gesture.
                out.push(GizmoEffect::EditBegin(ParamId::Crop));
            }
            GizmoEvent::Drag { hit, image_px, .. } => {
                let Some(ds) = self.drag.as_ref() else {
                    return;
                };
                if Handle::from_hit(hit) != Some(ds.handle) {
                    return; // a stale/mismatched event, ignore defensively
                }
                let img = xf.image_size_px();
                if img.x <= 0.0 || img.y <= 0.0 {
                    return;
                }
                let point = (image_px.x / img.x, image_px.y / img.y);
                let resolved_aspect = self.resolved_aspect(xf);
                let bound = self.inscribed(xf, resolved_aspect);
                let candidate = if ds.handle == Handle::Move {
                    let start_pt = (ds.start_image_px.x / img.x, ds.start_image_px.y / img.y);
                    translate(
                        &ds.start_rect,
                        point.0 - start_pt.0,
                        point.1 - start_pt.1,
                        &bound,
                    )
                } else {
                    let aspect = resolved_aspect.map(|a| a as f32);
                    clamp_into(
                        resize(&ds.start_rect, ds.handle, point, aspect, img),
                        &bound,
                    )
                };
                if candidate != self.rect {
                    self.rect = candidate;
                    out.push(GizmoEffect::Edit(crop_delta(self.rect)));
                }
            }
            GizmoEvent::DragEnd { .. } => {
                if self.drag.take().is_some() {
                    out.push(GizmoEffect::EditEnd);
                }
            }
            GizmoEvent::Click { .. } => {}
            GizmoEvent::Cancel => {
                // Unwind any open gesture before popping (gizmo.md
                // convention 5: emit EditEnd, or never EditBegin, before
                // Cancelled).
                if self.drag.take().is_some() {
                    out.push(GizmoEffect::EditEnd);
                }
                out.push(GizmoEffect::Cancelled);
            }
            GizmoEvent::Commit => {
                if self.drag.take().is_some() {
                    out.push(GizmoEffect::EditEnd);
                }
                out.push(GizmoEffect::Done);
            }
        }
    }

    /// Dimmed excluded region, rule-of-thirds grid, border, and the 8
    /// handles, all above the composited image (spec's crop-tool UI ask).
    fn paint(&self, painter: &egui::Painter, xf: &ViewXform, _ctx: &GizmoPaintCtx) {
        if !matches!(self.seed, Seed::Resolved) {
            return;
        }
        let img = xf.image_size_px();
        if img.x <= 0.0 || img.y <= 0.0 {
            return;
        }
        let full = egui::Rect::from_two_pos(
            xf.image_to_screen(egui::Vec2::ZERO),
            xf.image_to_screen(img),
        );
        let crop = self.screen_rect(xf);

        let dim = egui::Color32::from_black_alpha(140);
        let bands = [
            egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, crop.min.y)),
            egui::Rect::from_min_max(egui::pos2(full.min.x, crop.max.y), full.max),
            egui::Rect::from_min_max(
                egui::pos2(full.min.x, crop.min.y),
                egui::pos2(crop.min.x, crop.max.y),
            ),
            egui::Rect::from_min_max(
                egui::pos2(crop.max.x, crop.min.y),
                egui::pos2(full.max.x, crop.max.y),
            ),
        ];
        for band in bands {
            if band.width() > 0.0 && band.height() > 0.0 {
                painter.rect_filled(band, 0.0, dim);
            }
        }

        let grid = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(120));
        for i in 1..3 {
            let fx = crop.min.x + crop.width() * (i as f32 / 3.0);
            painter.line_segment(
                [egui::pos2(fx, crop.min.y), egui::pos2(fx, crop.max.y)],
                grid,
            );
            let fy = crop.min.y + crop.height() * (i as f32 / 3.0);
            painter.line_segment(
                [egui::pos2(crop.min.x, fy), egui::pos2(crop.max.x, fy)],
                grid,
            );
        }

        painter.rect_stroke(
            crop,
            0.0,
            egui::Stroke::new(1.5, egui::Color32::WHITE),
            egui::StrokeKind::Outside,
        );

        let handle_r = 4.0;
        for (_, p) in self.handle_points(xf) {
            let hr = egui::Rect::from_center_size(p, egui::vec2(handle_r * 2.0, handle_r * 2.0));
            painter.rect_filled(hr, 1.0, egui::Color32::WHITE);
            painter.rect_stroke(
                hr,
                1.0,
                egui::Stroke::new(1.0, egui::Color32::BLACK),
                egui::StrokeKind::Outside,
            );
        }
    }

    fn context(&self) -> ContextId {
        CTX_GEOM_CROP
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_types::Orientation;

    /// A 1:1, unpanned, unzoomed transform whose `view_rect` exactly matches
    /// the image size, `image_to_screen`/`screen_to_image` degenerate to
    /// the identity, so test assertions can reason directly in image pixels.
    fn test_xf(w: f32, h: f32) -> ViewXform {
        ViewXform::new(
            egui::vec2(w, h),
            Orientation::O1,
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h)),
            1.0,
            1.0,
            egui::Vec2::ZERO,
        )
    }

    #[test]
    fn identity_xf_maps_image_px_to_the_same_screen_point() {
        let xf = test_xf(200.0, 100.0);
        let p = xf.image_to_screen(egui::vec2(150.0, 80.0));
        assert!((p.x - 150.0).abs() < 1e-3 && (p.y - 80.0).abs() < 1e-3);
    }

    #[test]
    fn explicit_seed_resolves_to_the_given_crop_at_zero_angle() {
        let xf = test_xf(200.0, 100.0);
        let seeded = Crop {
            left: 0.1,
            top: 0.2,
            right: 0.9,
            bottom: 0.8,
        };
        let mut g = CropGizmo::from_current(seeded, 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);
        assert!(out.is_empty(), "Hover never emits an effect");
        let got = g.current_rect();
        assert!((got.left - 0.1).abs() < 1e-4);
        assert!((got.top - 0.2).abs() < 1e-4);
        assert!((got.right - 0.9).abs() < 1e-4);
        assert!((got.bottom - 0.8).abs() < 1e-4);
    }

    #[test]
    fn auto_fit_free_at_zero_angle_is_near_the_full_frame() {
        let xf = test_xf(200.0, 100.0);
        let mut g = CropGizmo::fit_aspect(AspectPreset::Free, 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);
        let r = g.current_rect();
        assert!(r.left < 0.02 && r.top < 0.02, "{r:?}");
        assert!(r.right > 0.98 && r.bottom > 0.98, "{r:?}");
    }

    #[test]
    fn auto_fit_locked_aspect_matches_the_requested_ratio() {
        let xf = test_xf(200.0, 100.0);
        let mut g = CropGizmo::fit_aspect(AspectPreset::Ratio(2.0), 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);
        let r = g.current_rect();
        let w = (r.right - r.left) * 200.0;
        let h = (r.bottom - r.top) * 100.0;
        assert!((w / h - 2.0).abs() < 1e-3, "w={w} h={h}");
    }

    /// **Proof (`param_slider`-equivalent for the crop gizmo)**: dragging
    /// the bottom-right handle emits the exact `EditBegin(Crop) ->
    /// Edit(delta) -> EditEnd` bracket (`gizmo.md` convention 5), and the
    /// delta's `right`/`bottom` land at exactly the dragged fraction.
    #[test]
    fn dragging_the_br_handle_grows_right_and_bottom_and_emits_the_gesture_bracket() {
        let xf = test_xf(200.0, 100.0);
        let mut g = CropGizmo::from_current(Crop::default(), 0.0);
        let mut out = Vec::new();
        g.on_event(
            GizmoEvent::Hover {
                image_px: Some(egui::vec2(100.0, 50.0)),
            },
            &xf,
            &mut out,
        );
        assert!(out.is_empty());

        let hit = g
            .hit(egui::pos2(200.0, 100.0), &xf)
            .expect("BR handle sits at the image's bottom-right corner");
        assert_eq!(hit, Handle::Br.hit_id());

        out.clear();
        g.on_event(
            GizmoEvent::DragStart {
                hit,
                image_px: egui::vec2(200.0, 100.0),
            },
            &xf,
            &mut out,
        );
        assert!(
            matches!(out.as_slice(), [GizmoEffect::EditBegin(ParamId::Crop)]),
            "{out:?}"
        );

        out.clear();
        g.on_event(
            GizmoEvent::Drag {
                hit,
                image_px: egui::vec2(150.0, 80.0),
                delta_px: egui::Vec2::ZERO,
            },
            &xf,
            &mut out,
        );
        match out.as_slice() {
            [GizmoEffect::Edit(delta)] => {
                let ParamValue::Crop(c) = delta.0.get(&ParamId::Crop).cloned().unwrap() else {
                    panic!("expected a Crop value");
                };
                assert!((c.right - 0.75).abs() < 1e-4, "{c:?}");
                assert!((c.bottom - 0.8).abs() < 1e-4, "{c:?}");
                assert_eq!(c.left, 0.0);
                assert_eq!(c.top, 0.0);
            }
            other => panic!("expected exactly one Edit effect, got {other:?}"),
        }

        out.clear();
        g.on_event(GizmoEvent::DragEnd { hit }, &xf, &mut out);
        assert!(matches!(out.as_slice(), [GizmoEffect::EditEnd]), "{out:?}");
    }

    #[test]
    fn aspect_locked_corner_drag_preserves_the_ratio_throughout() {
        let xf = test_xf(200.0, 100.0);
        let mut g = CropGizmo::fit_aspect(AspectPreset::Ratio(2.0), 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);

        let hit = Handle::Br.hit_id();
        out.clear();
        let start = g.current_rect();
        let start_br = egui::vec2(start.right * 200.0, start.bottom * 100.0);
        g.on_event(
            GizmoEvent::DragStart {
                hit,
                image_px: start_br,
            },
            &xf,
            &mut out,
        );
        out.clear();
        g.on_event(
            GizmoEvent::Drag {
                hit,
                image_px: egui::vec2(190.0, 60.0),
                delta_px: egui::Vec2::ZERO,
            },
            &xf,
            &mut out,
        );
        assert!(!out.is_empty(), "the drag must have moved the rect");
        let r = g.current_rect();
        // Convert back to PIXEL dimensions before checking the ratio, a
        // `width/height` aspect target is a pixel ratio, not a
        // fraction-space one, whenever the image itself isn't square (this
        // gizmo's own `resize` doc comment explains why).
        let w_px = (r.right - r.left) * 200.0;
        let h_px = (r.bottom - r.top) * 100.0;
        assert!((w_px / h_px - 2.0).abs() < 1e-3, "{r:?}");
    }

    #[test]
    fn move_handle_translates_without_resizing_and_clamps_to_the_frame() {
        let xf = test_xf(200.0, 100.0);
        let seeded = Crop {
            left: 0.1,
            top: 0.1,
            right: 0.4,
            bottom: 0.4,
        };
        let mut g = CropGizmo::from_current(seeded, 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);

        let hit = Handle::Move.hit_id();
        out.clear();
        g.on_event(
            GizmoEvent::DragStart {
                hit,
                image_px: egui::vec2(50.0, 25.0), // inside the seeded rect
            },
            &xf,
            &mut out,
        );
        out.clear();
        // Try to shove it far past the right/bottom edge, must clamp,
        // never resize.
        g.on_event(
            GizmoEvent::Drag {
                hit,
                image_px: egui::vec2(500.0, 500.0),
                delta_px: egui::Vec2::ZERO,
            },
            &xf,
            &mut out,
        );
        let r = g.current_rect();
        let w = r.right - r.left;
        let h = r.bottom - r.top;
        assert!((w - 0.3).abs() < 1e-4, "move must not resize: {r:?}");
        assert!((h - 0.3).abs() < 1e-4, "move must not resize: {r:?}");
        assert!(r.right <= 1.0 + 1e-4 && r.bottom <= 1.0 + 1e-4, "{r:?}");
    }

    /// **gizmo.md convention 5**: Cancel mid-drag unwinds the open gesture
    /// (`EditEnd`) before popping (`Cancelled`), never leaves a gesture
    /// dangling.
    #[test]
    fn cancel_mid_drag_unwinds_the_gesture_before_cancelling() {
        let xf = test_xf(200.0, 100.0);
        let mut g = CropGizmo::from_current(Crop::default(), 0.0);
        let mut out = Vec::new();
        g.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);
        let hit = Handle::Br.hit_id();
        out.clear();
        g.on_event(
            GizmoEvent::DragStart {
                hit,
                image_px: egui::vec2(200.0, 100.0),
            },
            &xf,
            &mut out,
        );
        out.clear();
        g.on_event(GizmoEvent::Cancel, &xf, &mut out);
        assert!(
            matches!(
                out.as_slice(),
                [GizmoEffect::EditEnd, GizmoEffect::Cancelled]
            ),
            "{out:?}"
        );
    }

    #[test]
    fn constrain_crop_bound_shrinks_the_auto_fit_rect_as_angle_grows() {
        let xf = test_xf(200.0, 200.0);
        let mut free = CropGizmo::fit_aspect(AspectPreset::Free, 0.0);
        let mut tilted = CropGizmo::fit_aspect(AspectPreset::Free, 20.0);
        let mut out = Vec::new();
        free.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);
        out.clear();
        tilted.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut out);

        let area = |c: Crop| (c.right - c.left) as f64 * (c.bottom - c.top) as f64;
        assert!(
            area(tilted.current_rect()) < area(free.current_rect()),
            "a straighten angle must shrink the largest achievable crop \
             (constrain-crop, task E9's helper)"
        );
    }

    // ── real-binding integration (no mocks; mirrors
    //    `develop_ctx.rs::preset_wiring_tests`' exact harness shape) ────────
    mod real_binding {
        use super::*;
        use crate::panels::develop_ctx::{EditBinding, SessionEditBinding};
        use lightbox_core::{Command, Core, CoreConfig, Event, OpenOrigin, OpenRequest, Session};
        use lightbox_types::ImageId;
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};
        use tokio::sync::broadcast::error::TryRecvError;

        const TINY_JPEG: &[u8] = include_bytes!("../../../../tools/xtask/assets/lightbox-tiny.jpg");
        const EVENT_TIMEOUT: Duration = Duration::from_secs(15);

        fn start_session(tmp: &Path) -> (Core, Session) {
            let mut cfg = CoreConfig::default();
            cfg.preset_dir = Some(tmp.join("presets"));
            let core = Core::start(cfg).expect("core start");
            let session = core
                .create_catalog(&tmp.join("cat.lbdata"), None)
                .expect("create catalog");
            (core, session)
        }

        fn stage_jpeg(dir: &Path, name: &str) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, TINY_JPEG).unwrap();
            path
        }

        fn open_one_image(session: &Session, path: PathBuf) -> ImageId {
            let mut rx = session.events();
            session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(vec![path], false, OpenOrigin::Cli),
            });
            let deadline = Instant::now() + EVENT_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the working set to load"
                );
                match rx.try_recv() {
                    Ok(Event::WorkingSetLoadFinished { .. }) => break,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Closed) => panic!("event channel closed while opening"),
                }
            }
            crate::working_set::WorkingSetView::new(session)
                .active_image()
                .expect("the first Ready entry auto-activates (spec §2.4)")
        }

        fn drain_until_catalog_changed(rx: &mut tokio::sync::broadcast::Receiver<Event>) {
            let deadline = Instant::now() + EVENT_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for Event::CatalogChanged"
                );
                match rx.try_recv() {
                    Ok(Event::CatalogChanged { .. }) => return,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Closed) => panic!("event channel closed while draining"),
                }
            }
        }

        /// Routes gizmo effects onto `binding` exactly as `lib.rs`'s own
        /// per-frame drain loop does (the production `Edit*` mapping), so
        /// this test proves the REAL wiring, not a re-implementation of it.
        fn apply_effects(binding: &mut SessionEditBinding, effects: Vec<GizmoEffect>) {
            for effect in effects {
                match effect {
                    GizmoEffect::EditBegin(p) => binding.begin_gesture(p),
                    GizmoEffect::Edit(d) => binding.preview(d),
                    GizmoEffect::EditEnd => binding.end_gesture(),
                    GizmoEffect::Picked { .. } | GizmoEffect::Done | GizmoEffect::Cancelled => {}
                }
            }
        }

        /// **AC**: a crop-gizmo drag produces the expected `ParamId::Crop`
        /// gesture (begin -> preview -> end) against a REAL
        /// `SessionEditBinding`, the committed recipe's crop rect changes
        /// by exactly the dragged amount, lands as ONE history step, and
        /// `undo` restores the pre-drag rect exactly.
        #[test]
        fn crop_gizmo_drag_commits_one_history_step_and_undo_restores() {
            let tmp = tempfile::TempDir::new().unwrap();
            let (_core, session) = start_session(tmp.path());
            let photo = stage_jpeg(&tmp.path().join("photos"), "a.jpg");
            let image = open_one_image(&session, photo);

            let mut rx = session.events();
            let mut binding = SessionEditBinding::new(session.clone());
            binding.bind(Some(image));
            assert_eq!(
                binding.value(ParamId::Crop),
                ParamValue::Crop(Crop::default()),
                "starts neutral (full frame)"
            );

            let xf = test_xf(200.0, 100.0);
            let mut gizmo = CropGizmo::from_current(Crop::default(), 0.0);
            let mut effects = Vec::new();
            gizmo.on_event(GizmoEvent::Hover { image_px: None }, &xf, &mut effects);
            let hit = Handle::Br.hit_id();
            gizmo.on_event(
                GizmoEvent::DragStart {
                    hit,
                    image_px: egui::vec2(200.0, 100.0),
                },
                &xf,
                &mut effects,
            );
            gizmo.on_event(
                GizmoEvent::Drag {
                    hit,
                    image_px: egui::vec2(150.0, 80.0),
                    delta_px: egui::Vec2::ZERO,
                },
                &xf,
                &mut effects,
            );
            gizmo.on_event(GizmoEvent::DragEnd { hit }, &xf, &mut effects);

            apply_effects(&mut binding, effects);
            drain_until_catalog_changed(&mut rx);
            binding.mark_dirty();
            binding.bind(Some(image));

            let ParamValue::Crop(committed) = binding.value(ParamId::Crop) else {
                panic!("expected a Crop value")
            };
            assert!((committed.right - 0.75).abs() < 1e-4, "{committed:?}");
            assert!((committed.bottom - 0.8).abs() < 1e-4, "{committed:?}");
            assert_eq!(committed.left, 0.0);
            assert_eq!(committed.top, 0.0);
            assert!(
                binding.can_undo(),
                "the drag must have landed as exactly one durable history step"
            );

            binding.undo();
            drain_until_catalog_changed(&mut rx);
            binding.mark_dirty();
            binding.bind(Some(image));
            assert_eq!(
                binding.value(ParamId::Crop),
                ParamValue::Crop(Crop::default()),
                "undo restores the pre-drag crop exactly"
            );
        }
    }
}
