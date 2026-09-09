// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `ViewXform`, the image↔screen mapping (E08 spec §6.4, task C1).
//!
//! One authority for compositing AND (Phase F) gizmos: **screen space** is
//! logical points, oriented (what's actually painted this frame); **image
//! space** is RAW, unrotated pixel coordinates, origin top-left of the
//! as-decoded/rendered image, `x` right, `y` down. This is deliberately the
//! same space gizmo geometry will be stored in (spec §6.6: "gizmo geometry
//! is stored in image space... survives zoom/pan/resize") so a future
//! `Crop`/`MaskPin`'s coordinates never need to be re-derived when the user
//! rotates the view, only `ViewXform` changes.
//!
//! **M1 orientation note.** `lightbox-render`'s node graph does not yet
//! apply EXIF orientation to rendered pixels, see
//! `lightbox-core::render_source`'s own deviation note: orientation/geometry
//! nodes are explicitly E11's job, deferred past M1. `ViewXform` implements
//! the full 8-way EXIF transform anyway (property-tested below) so the type
//! is correct and Phase-F-ready the day E11 lands; until then the canvas
//! (`view.rs`) always builds it with `Orientation::O1` (identity), which is
//! exactly the E01/F5 loupe's existing behavior, no visible change.

use eframe::egui;
use lightbox_types::Orientation;

/// Image↔screen mapping for one canvas frame (spec §6.4). Covers zoom, pan,
/// orientation, and pixels-per-point. `Copy`/cheap, rebuilt fresh every
/// frame from the canvas's current state.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ViewXform {
    /// Raw (unrotated) image size, pixels.
    image_px: egui::Vec2,
    orientation: Orientation,
    /// The screen area (logical points) the image is composited into.
    view_rect: egui::Rect,
    /// 1.0 = "100%" (one image pixel = one physical pixel). `ZoomMode::Fit`
    /// (`view.rs`) is resolved to a concrete percent by the caller before
    /// building this transform, `ViewXform` itself has no notion of "fit".
    zoom_percent: f32,
    /// `egui::Context::pixels_per_point()` for this frame.
    ppp: f32,
    /// Screen-space offset (points) from the centered position.
    pan: egui::Vec2,
}

impl ViewXform {
    /// Builds the transform for one frame.
    pub fn new(
        image_px: egui::Vec2,
        orientation: Orientation,
        view_rect: egui::Rect,
        zoom_percent: f32,
        ppp: f32,
        pan: egui::Vec2,
    ) -> ViewXform {
        ViewXform {
            image_px,
            orientation,
            view_rect,
            zoom_percent: zoom_percent.max(0.0),
            ppp: if ppp > 0.0 { ppp } else { 1.0 },
            pan,
        }
    }

    /// Points-per-image-pixel at the current zoom (`zoom_percent / ppp`).
    fn scale(&self) -> f32 {
        self.zoom_percent / self.ppp
    }

    /// The effective zoom, 1.0 = 100 % (spec §6.4 `ViewXform::zoom`).
    /// `#[allow(dead_code)]`: part of the frozen §6.4 surface (a future
    /// zoom readout / Phase F's gizmo hit-tolerance scaling); `view.rs`
    /// tracks the resolved percent itself for now, see its module docs.
    #[allow(dead_code)]
    pub fn zoom(&self) -> f32 {
        self.zoom_percent
    }

    /// The size (logical points) the oriented image occupies on screen at
    /// the current zoom, `Fit`'s sizing and drag-pan clamping both need
    /// this (`view.rs`).
    pub fn display_size_screen(&self) -> egui::Vec2 {
        oriented_size(self.orientation, self.image_px) * self.scale()
    }

    /// The raw (unrotated) image size in pixels, gizmos need it for
    /// normalized-UV math (the eyedropper loupe) without re-plumbing the
    /// entry dimensions (`canvas/gizmo.rs`).
    pub fn image_size_px(&self) -> egui::Vec2 {
        self.image_px
    }

    /// Logical points per image pixel at the current zoom, the scale a
    /// gizmo needs to convert a screen-space tolerance/offset into image
    /// space (or vice versa). `0.0` at degenerate zoom.
    #[allow(dead_code)] // Phase-F convention surface (gizmo.md); the shipped
                        // eyedropper derives drag deltas from clamped position pairs instead.
    pub fn points_per_image_px(&self) -> f32 {
        self.scale()
    }

    /// Maps a RAW image-space pixel coordinate to a screen point (logical
    /// points). Never fails, a point outside the image just maps outside
    /// `view_rect`.
    pub fn image_to_screen(&self, image_px: egui::Vec2) -> egui::Pos2 {
        let (dx, dy) = orient_forward(
            self.orientation,
            image_px.x,
            image_px.y,
            self.image_px.x,
            self.image_px.y,
        );
        let disp = oriented_size(self.orientation, self.image_px);
        let rel = egui::vec2(dx - disp.x / 2.0, dy - disp.y / 2.0) * self.scale();
        self.view_rect.center() + self.pan + rel
    }

    /// Inverse of [`ViewXform::image_to_screen`]: `None` when `screen` falls
    /// outside the composited image's bounds (spec C1 AC). Production
    /// consumers since Phase F: gizmo hit-testing and click→pick mapping
    /// (`canvas/gizmo.rs`).
    pub fn screen_to_image(&self, screen: egui::Pos2) -> Option<egui::Vec2> {
        let scale = self.scale();
        if scale <= 0.0 {
            return None;
        }
        let disp = oriented_size(self.orientation, self.image_px);
        let rel = (screen - (self.view_rect.center() + self.pan)) / scale;
        let dx = rel.x + disp.x / 2.0;
        let dy = rel.y + disp.y / 2.0;
        const EPS: f32 = 1e-3;
        if dx < -EPS || dy < -EPS || dx > disp.x + EPS || dy > disp.y + EPS {
            return None;
        }
        let (x, y) = orient_inverse(self.orientation, dx, dy, self.image_px.x, self.image_px.y);
        if x < -EPS || y < -EPS || x > self.image_px.x + EPS || y > self.image_px.y + EPS {
            return None;
        }
        Some(egui::vec2(x, y))
    }

    /// Like [`ViewXform::screen_to_image`], but a point outside the image
    /// clamps to the nearest in-bounds pixel instead of returning `None`
    /// Phase F's drag-continuation mapping (a captured gizmo drag keeps
    /// tracking while the pointer leaves the image, `canvas/gizmo.rs`).
    /// `None` only at degenerate zoom.
    pub fn screen_to_image_clamped(&self, screen: egui::Pos2) -> Option<egui::Vec2> {
        let scale = self.scale();
        if scale <= 0.0 {
            return None;
        }
        let disp = oriented_size(self.orientation, self.image_px);
        let rel = (screen - (self.view_rect.center() + self.pan)) / scale;
        let dx = (rel.x + disp.x / 2.0).clamp(0.0, disp.x);
        let dy = (rel.y + disp.y / 2.0).clamp(0.0, disp.y);
        let (x, y) = orient_inverse(self.orientation, dx, dy, self.image_px.x, self.image_px.y);
        Some(egui::vec2(
            x.clamp(0.0, self.image_px.x),
            y.clamp(0.0, self.image_px.y),
        ))
    }
}

/// The oriented (display-space) size, raw pixel units, swaps width/height
/// for the 90°/270° family (O5-O8). Exposed for `view.rs`'s fit-percent
/// calculation (needs the display footprint without a full [`ViewXform`]).
pub fn oriented_size(o: Orientation, image_px: egui::Vec2) -> egui::Vec2 {
    let (w, h) = orient_dims(o, image_px.x, image_px.y);
    egui::vec2(w, h)
}

// ---- EXIF orientation transform (8-way; spec §6.4/§6.6) ----
//
// Every orientation is `flip-horizontal` (or not) composed with a
// `rotate-clockwise` by 0/90/180/270°, matching the `Orientation` doc
// comments verbatim (`lightbox-types`): O5 = "mirrored horizontally, then
// rotated 270° CW", O7 = "...then rotated 90° CW", etc. `orient_inverse` is
// the algebraic inverse of `orient_forward` (each is an orthogonal 2×2
// linear map plus a translation, so the inverse is exact, not approximate)
// cross-checked against `orient_forward` by the round-trip property test
// below for all 8 orientations.

fn orient_dims(o: Orientation, w: f32, h: f32) -> (f32, f32) {
    match o {
        Orientation::O1 | Orientation::O2 | Orientation::O3 | Orientation::O4 => (w, h),
        Orientation::O5 | Orientation::O6 | Orientation::O7 | Orientation::O8 => (h, w),
    }
}

fn orient_forward(o: Orientation, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    match o {
        Orientation::O1 => (x, y),
        Orientation::O2 => (w - x, y),
        Orientation::O3 => (w - x, h - y),
        Orientation::O4 => (x, h - y),
        Orientation::O5 => (y, x),
        Orientation::O6 => (h - y, x),
        Orientation::O7 => (h - y, w - x),
        Orientation::O8 => (y, w - x),
    }
}

fn orient_inverse(o: Orientation, dx: f32, dy: f32, w: f32, h: f32) -> (f32, f32) {
    match o {
        Orientation::O1 => (dx, dy),
        Orientation::O2 => (w - dx, dy),
        Orientation::O3 => (w - dx, h - dy),
        Orientation::O4 => (dx, h - dy),
        Orientation::O5 => (dy, dx),
        Orientation::O6 => (dy, h - dx),
        Orientation::O7 => (w - dy, h - dx),
        Orientation::O8 => (w - dy, dx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIENTATIONS: [Orientation; 8] = [
        Orientation::O1,
        Orientation::O2,
        Orientation::O3,
        Orientation::O4,
        Orientation::O5,
        Orientation::O6,
        Orientation::O7,
        Orientation::O8,
    ];

    fn approx(a: egui::Vec2, b: egui::Vec2, eps: f32) -> bool {
        (a.x - b.x).abs() <= eps && (a.y - b.y).abs() <= eps
    }

    /// C1 AC: `screen_to_image ∘ image_to_screen ≈ id` within 0.5 px, swept
    /// deterministically across zoom/pan/orientation samples (a "property
    /// test" implemented via sampling rather than `proptest`, this crate
    /// carries no proptest dependency; same convention `E04-deviations.md`
    /// already recorded for this workspace).
    #[test]
    fn round_trip_within_half_a_pixel_across_zoom_pan_orientation() {
        let view_rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 300.0));
        let image_sizes = [
            egui::vec2(800.0, 600.0),
            egui::vec2(4032.0, 3024.0),
            egui::vec2(1.0, 1.0),
            egui::vec2(101.0, 257.0),
        ];
        let zooms = [0.1_f32, 0.25, 0.5, 1.0, 1.7, 2.0, 4.0];
        let ppps = [1.0_f32, 1.5, 2.0];
        let pans = [
            egui::Vec2::ZERO,
            egui::vec2(37.0, -12.0),
            egui::vec2(-500.0, 250.0),
        ];

        for &image_px in &image_sizes {
            for &orientation in &ORIENTATIONS {
                for &zoom in &zooms {
                    for &ppp in &ppps {
                        for &pan in &pans {
                            let xf =
                                ViewXform::new(image_px, orientation, view_rect, zoom, ppp, pan);
                            let samples = [
                                egui::vec2(0.0, 0.0),
                                egui::vec2(image_px.x, 0.0),
                                egui::vec2(0.0, image_px.y),
                                egui::vec2(image_px.x, image_px.y),
                                image_px * 0.5,
                                egui::vec2(image_px.x * 0.3, image_px.y * 0.7),
                            ];
                            for &p in &samples {
                                let screen = xf.image_to_screen(p);
                                let back = xf.screen_to_image(screen).unwrap_or_else(|| {
                                    panic!(
                                        "round-trip lost a point: image_px={image_px:?} \
                                         orientation={orientation:?} zoom={zoom} ppp={ppp} \
                                         pan={pan:?} p={p:?} screen={screen:?}"
                                    )
                                });
                                assert!(
                                    approx(p, back, 0.5),
                                    "round-trip drift > 0.5px: p={p:?} back={back:?} \
                                     (image_px={image_px:?} orientation={orientation:?} \
                                     zoom={zoom} ppp={ppp} pan={pan:?})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// C1 AC: a point outside the image maps to `None`, both far outside
    /// the view and just past the image's own edge (inside the view but
    /// outside the composited image rect, e.g. letterboxing at `Fit`).
    #[test]
    fn outside_the_image_maps_to_none() {
        let view_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let xf = ViewXform::new(
            egui::vec2(800.0, 600.0),
            Orientation::O1,
            view_rect,
            1.0,
            1.0,
            egui::Vec2::ZERO,
        );
        assert!(xf.screen_to_image(egui::pos2(-1000.0, -1000.0)).is_none());
        assert!(xf.screen_to_image(egui::pos2(100_000.0, 5.0)).is_none());
        let just_outside = xf.image_to_screen(egui::vec2(800.0 + 50.0, 300.0));
        assert!(xf.screen_to_image(just_outside).is_none());

        // Degenerate zoom never divides by zero / panics.
        let degenerate = ViewXform::new(
            egui::vec2(800.0, 600.0),
            Orientation::O1,
            view_rect,
            0.0,
            1.0,
            egui::Vec2::ZERO,
        );
        assert!(degenerate.screen_to_image(view_rect.center()).is_none());
    }

    /// The 90°/270° family (O5-O8) swaps the display footprint; the
    /// axis-aligned family (O1-O4) does not.
    #[test]
    fn orientation_swaps_display_dimensions_for_90_and_270() {
        let view_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 1000.0));
        for &(o, swapped) in &[
            (Orientation::O1, false),
            (Orientation::O2, false),
            (Orientation::O3, false),
            (Orientation::O4, false),
            (Orientation::O5, true),
            (Orientation::O6, true),
            (Orientation::O7, true),
            (Orientation::O8, true),
        ] {
            let xf = ViewXform::new(
                egui::vec2(800.0, 600.0),
                o,
                view_rect,
                1.0,
                1.0,
                egui::Vec2::ZERO,
            );
            let size = xf.display_size_screen();
            let (ex, ey) = if swapped {
                (600.0, 800.0)
            } else {
                (800.0, 600.0)
            };
            assert!(
                (size.x - ex).abs() < 0.01 && (size.y - ey).abs() < 0.01,
                "{o:?}: expected {ex}x{ey}, got {size:?}"
            );
        }
    }

    /// The image center always maps to the view center when pan is zero
    /// (a cheap sanity check independent of the round-trip sweep above).
    #[test]
    fn centered_image_center_maps_to_view_center() {
        let view_rect = egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(200.0, 100.0));
        for &o in &ORIENTATIONS {
            let xf = ViewXform::new(
                egui::vec2(800.0, 600.0),
                o,
                view_rect,
                1.0,
                1.0,
                egui::Vec2::ZERO,
            );
            let center_img = egui::vec2(400.0, 300.0); // the raw image's own center
            let screen = xf.image_to_screen(center_img);
            assert!(
                (screen - view_rect.center()).length() < 0.01,
                "{o:?}: image center should map to the view center, got {screen:?}"
            );
        }
    }
}
