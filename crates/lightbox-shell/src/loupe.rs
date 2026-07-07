// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The loupe (spec T26, carried forward by E05 Phase F5): every displayed
//! pixel is produced by the `ng` render engine and composited **zero-copy**
//! — `RenderScheduler::canvas()` publishes a [`CanvasFrame`] whose texture
//! (already on the shared device) is registered with egui
//! (`egui_wgpu::Renderer::register_native_texture`) and drawn; there is no
//! `map_async`/CPU readback anywhere in this file (T7/T26 invariant).
//!
//! **F5 rewrite.** The E01 seed drove one `Engine::submit`/`poll` ticket per
//! viewport (pull model). The `ng` engine is push-based: [`LoupeView::ui`]
//! calls [`RenderScheduler::set_view`]/[`RenderScheduler::set_recipe`] on any
//! relevant change (the scheduler's latest-wins coalescing supersedes
//! whatever was in flight — spec §3.7/§4.3), and each frame samples the
//! canvas [`tokio::sync::watch`] channel for a newer generation to composite.
//! There is exactly one canvas publisher per session (owned by the
//! `RenderScheduler`, enabled once at session-open in `lib.rs`), so only one
//! image renders to the canvas surface at a time — correct for the one-pane
//! loupe this crate ships; a future multi-pane compare view would need
//! either per-pane engines or an `image` tag on `CanvasFrame` (noted in
//! `docs/plan/epics/E05-deviations.md`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use lightbox_core::ImageSummary;
use lightbox_edit::Recipe;
use lightbox_render::ng::{Extent, OutputQuality, RenderScheduler, Roi, ViewState};
use lightbox_types::{ImageId, PV_M0};

use crate::grid::fit_rect;
use crate::ShellOutcome;

/// Zoom modes (T26: fit / 100 % toggle).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LoupeZoom {
    /// Fit the viewport.
    Fit,
    /// 1 image pixel : 1 physical pixel, drag pan.
    OneToOne,
}

/// What a loupe frame asks the app to do.
#[derive(Debug, PartialEq)]
pub enum LoupeAction {
    /// Back to the grid (G / Escape).
    ExitToGrid,
    /// Navigate to this row index (←/→); the app updates the selection.
    Navigate(usize),
}

/// The (image, zoom, output-size) triple most recently submitted (dedupes
/// redundant `set_view`/`set_recipe` calls — spec §4.3 "submit on relevant
/// change only").
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct SubmitKey {
    image: ImageId,
    zoom: LoupeZoom,
    out: [u32; 2],
}

/// The engine texture currently composited (registered with egui).
struct Displayed {
    texture_id: egui::TextureId,
    size: [u32; 2],
    generation: u64,
}

/// Loupe state — see the module docs.
pub struct LoupeView {
    render_state: eframe::egui_wgpu::RenderState,
    outcome: Arc<ShellOutcome>,
    canvas_rx: Option<tokio::sync::watch::Receiver<lightbox_render::ng::CanvasFrame>>,
    displayed: Option<Displayed>,
    last_key: Option<SubmitKey>,
    /// Set on every dispatch, cleared once a newer generation is observed —
    /// the push-model's "still waiting on a fresher frame" flag
    /// ([`LoupeView::busy`]).
    awaiting_frame: bool,
    last_quality: Option<OutputQuality>,
    zoom: LoupeZoom,
    pan: egui::Vec2,
    error: Option<String>,
    /// Navigation → texture-swap latency probe (T26 AC: < 50 ms p95).
    nav_started: Option<Instant>,
    /// Rolling nav-swap latencies (ms), newest last, bounded.
    pub nav_swap_ms: Vec<f32>,
}

impl LoupeView {
    /// A loupe compositing through `render_state`'s egui renderer, sampling
    /// `scheduler`'s canvas publisher (`None` when the scheduler has no GPU
    /// canvas enabled — e.g. a CPU-only session; the loupe then shows the
    /// "rendering…"/error text forever, which is honest: there is nothing to
    /// composite zero-copy without a GPU canvas).
    pub fn new(
        render_state: eframe::egui_wgpu::RenderState,
        outcome: Arc<ShellOutcome>,
        scheduler: &RenderScheduler,
    ) -> LoupeView {
        LoupeView {
            render_state,
            outcome,
            canvas_rx: scheduler.canvas(),
            displayed: None,
            last_key: None,
            awaiting_frame: false,
            last_quality: None,
            zoom: LoupeZoom::Fit,
            pan: egui::Vec2::ZERO,
            error: None,
            nav_started: None,
            nav_swap_ms: Vec::new(),
        }
    }

    /// Reset for a fresh entry from the grid.
    pub fn enter(&mut self) {
        self.zoom = LoupeZoom::Fit;
        self.pan = egui::Vec2::ZERO;
        self.error = None;
        self.last_key = None; // force a submit for the (possibly new) image
        self.nav_started = Some(Instant::now());
    }

    /// Release egui resources when leaving the loupe. The scheduler itself
    /// needs no explicit stop — a new image simply supersedes the old one's
    /// slot via latest-wins (spec §3.7); there is no per-ticket cancel to
    /// issue in the push model.
    pub fn exit(&mut self) {
        if let Some(old) = self.displayed.take() {
            self.render_state
                .renderer
                .write()
                .free_texture(&old.texture_id);
        }
        self.last_key = None;
    }

    /// True while a render is in flight (keep repainting).
    pub fn busy(&self) -> bool {
        self.awaiting_frame
    }

    /// Renders one loupe frame for `rows[idx]`.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        scheduler: &RenderScheduler,
        max_tex_dim: u32,
        rows: &[ImageSummary],
        idx: usize,
    ) -> Option<LoupeAction> {
        let summary = rows.get(idx)?;
        let mut action = None;

        // --- Keys (T26: ←/→ nav, fit/100 % toggle, G/E view toggle) ---
        // unless some text field owns the keyboard.
        if !ui.ctx().egui_wants_keyboard_input() {
            ui.ctx().input(|i| {
                if i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::G) {
                    action = Some(LoupeAction::ExitToGrid);
                }
                if i.key_pressed(egui::Key::ArrowRight) && idx + 1 < rows.len() {
                    action = Some(LoupeAction::Navigate(idx + 1));
                }
                if i.key_pressed(egui::Key::ArrowLeft) && idx > 0 {
                    action = Some(LoupeAction::Navigate(idx - 1));
                }
                if i.key_pressed(egui::Key::Z) || i.key_pressed(egui::Key::Space) {
                    self.toggle_zoom();
                }
            });
        }
        if let Some(LoupeAction::Navigate(_)) = action {
            self.pan = egui::Vec2::ZERO;
            self.nav_started = Some(Instant::now());
        }

        let ppp = ui.ctx().pixels_per_point();
        let view_rect = ui.available_rect_before_wrap();
        let view_px = [
            ((view_rect.width() * ppp).round() as u32).clamp(1, max_tex_dim),
            ((view_rect.height() * ppp).round() as u32).clamp(1, max_tex_dim),
        ];

        // --- Submit on any relevant change; latest-wins coalescing on the
        // scheduler side supersedes whatever was in flight (T26/§3.7). ---
        let key = SubmitKey {
            image: summary.id,
            zoom: self.zoom,
            out: view_px,
        };
        if self.last_key != Some(key) {
            let view = ViewState {
                viewport: Extent {
                    w: view_px[0],
                    h: view_px[1],
                },
                zoom: lightbox_render::ng::Zoom(match self.zoom {
                    LoupeZoom::Fit => 1.0,
                    LoupeZoom::OneToOne => 1.0,
                }),
                pan: Roi {
                    x: 0,
                    y: 0,
                    w: view_px[0],
                    h: view_px[1],
                },
            };
            scheduler.set_view(summary.id, view);
            // The M1 recipe carries no develop params yet (E09) — identity
            // under PV1, resubmitted on every relevant view change so a fresh
            // image always gets a render dispatched even without a param
            // change (set_view alone only re-renders when a recipe already
            // exists for that image — see `RenderScheduler::set_view`).
            scheduler.set_recipe(summary.id, Recipe::identity(PV_M0), PV_M0);
            self.last_key = Some(key);
            self.awaiting_frame = true;
        }

        self.poll(scheduler, summary.id);

        // --- Composite ---
        let response = ui.allocate_rect(view_rect, egui::Sense::click_and_drag());
        if response.double_clicked() {
            self.toggle_zoom();
        }
        if let Some(d) = &self.displayed {
            let logical = egui::vec2(d.size[0] as f32 / ppp, d.size[1] as f32 / ppp);
            let image_rect = match self.zoom {
                LoupeZoom::Fit => fit_rect(view_rect, [logical.x, logical.y]),
                LoupeZoom::OneToOne => {
                    // Drag pan, clamped so the image never leaves the view.
                    if response.dragged() {
                        self.pan += response.drag_delta();
                    }
                    let max_off = ((logical - view_rect.size()) * 0.5).max(egui::Vec2::ZERO);
                    self.pan = self.pan.clamp(-max_off, max_off);
                    egui::Rect::from_center_size(view_rect.center() + self.pan, logical)
                }
            };
            ui.painter().with_clip_rect(view_rect).image(
                d.texture_id,
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            let text = match &self.error {
                Some(err) => format!("render failed: {err}"),
                None => "rendering…".to_owned(),
            };
            ui.painter().text(
                view_rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }

        self.info_overlay(ui, view_rect, summary, idx, rows.len());
        action
    }

    fn toggle_zoom(&mut self) {
        self.zoom = match self.zoom {
            LoupeZoom::Fit => LoupeZoom::OneToOne,
            LoupeZoom::OneToOne => LoupeZoom::Fit,
        };
        self.pan = egui::Vec2::ZERO;
    }

    /// Samples the canvas watch channel for a newer generation; swaps the
    /// composited texture and surfaces the last error/quality badge.
    fn poll(&mut self, scheduler: &RenderScheduler, image: ImageId) {
        if let Some(err) = scheduler.last_error(image) {
            // Keep compositing the last good frame (F5: a transient failure
            // never blanks the loupe) but surface the error for the badge.
            self.error = Some(err);
        }
        let Some(rx) = &mut self.canvas_rx else {
            return;
        };
        let frame = rx.borrow_and_update();
        let already_shown = self
            .displayed
            .as_ref()
            .is_some_and(|d| d.generation == frame.generation);
        if already_shown || frame.generation == 0 {
            return;
        }
        let (texture, quality, extent, generation) = (
            frame.texture.clone(),
            frame.quality,
            frame.extent,
            frame.generation,
        );
        drop(frame);
        self.swap_displayed(&texture, [extent.w, extent.h], generation);
        self.last_quality = Some(quality);
        self.error = None;
        self.awaiting_frame = false;
        if let Some(started) = self.nav_started.take() {
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.nav_swap_ms.push(ms);
            if self.nav_swap_ms.len() > 256 {
                self.nav_swap_ms.remove(0);
            }
        }
    }

    /// Registers a finished engine texture with egui, releasing the previous
    /// one — flicker-free swap on the SAME shared device (seam 2). Never
    /// copies pixels.
    fn swap_displayed(&mut self, view: &wgpu::TextureView, size: [u32; 2], generation: u64) {
        let mut renderer = self.render_state.renderer.write();
        let texture_id = renderer.register_native_texture(
            &self.render_state.device,
            view,
            wgpu::FilterMode::Linear,
        );
        if let Some(old) = self.displayed.replace(Displayed {
            texture_id,
            size,
            generation,
        }) {
            renderer.free_texture(&old.texture_id);
        }
        self.outcome.texture_swaps.fetch_add(1, Ordering::AcqRel);
        self.outcome.seam_proven.store(true, Ordering::Release);
    }

    /// Minimal info overlay (T26): filename, dims, quality badge, zoom.
    fn info_overlay(
        &self,
        ui: &egui::Ui,
        view_rect: egui::Rect,
        summary: &ImageSummary,
        idx: usize,
        total: usize,
    ) {
        let quality = match self.last_quality {
            Some(OutputQuality::PreviewTier) => "preview",
            Some(OutputQuality::PreviewRes) => "preview-res",
            Some(OutputQuality::FullRes) => "full-res",
            None => "…",
        };
        let text = format!(
            "{}  ·  {}×{}  ·  {}  ·  {}  ·  {}/{}",
            summary.filename,
            summary.width,
            summary.height,
            quality,
            match self.zoom {
                LoupeZoom::Fit => "fit",
                LoupeZoom::OneToOne => "100%",
            },
            idx + 1,
            total,
        );
        let painter = ui.painter();
        let pos = view_rect.left_top() + egui::vec2(8.0, 8.0);
        let galley =
            painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
        let bg = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(12.0, 8.0));
        painter.rect_filled(bg, 4.0, egui::Color32::from_black_alpha(140));
        painter.galley(pos + egui::vec2(6.0, 4.0), galley, egui::Color32::WHITE);
    }
}
