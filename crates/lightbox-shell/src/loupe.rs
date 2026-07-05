// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The loupe (spec T26): every displayed pixel is produced by
//! `Engine::submit` and composited **zero-copy** — the finished texture on
//! the shared device is registered with egui
//! (`egui_wgpu::Renderer::register_native_texture`) and drawn; there is no
//! `map_async`/CPU readback anywhere in this file (T7/T26 invariant).
//!
//! A fresh `RenderRequest` is submitted on every relevant change (image
//! navigation, resize, zoom toggle) onto ONE viewport id — the engine's
//! per-viewport latest-wins coalescing supersedes stale in-flight renders,
//! so an out-of-date size is never composited over a newer one. Fit mode
//! renders `FitWithin(viewport)`; 100 % renders at `Native` scale from the
//! embedded preview (honest about the M0 source resolution) and pans by
//! compositing only.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use lightbox_core::ImageSummary;
use lightbox_edit::Recipe;
use lightbox_render::{
    Engine, RenderOutput, RenderRequest, RenderScale, RenderState, RenderTarget, RenderTicket, Roi,
    ViewportId,
};
use lightbox_types::{ImageId, PV_M0};

use crate::grid::fit_rect;
use crate::ShellOutcome;

/// The loupe's coalescing bucket (spec §4.3: one in-flight render per
/// viewport, latest wins).
const LOUPE_VIEWPORT: ViewportId = ViewportId(1);

/// Zoom modes (T26: fit / 100 % toggle).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Zoom {
    /// Fit the viewport (`RenderScale::FitWithin`).
    Fit,
    /// 1 image pixel : 1 physical pixel (`RenderScale::Native`), drag pan.
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

/// The (image, zoom, output-size) triple a submitted render answers for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct SubmitKey {
    image: ImageId,
    zoom: Zoom,
    /// Requested output size in physical px (0×0 under `Native`, which is
    /// size-independent — resizes then re-composite without re-rendering).
    out: [u32; 2],
}

/// The engine texture currently composited (registered with egui).
struct Displayed {
    texture_id: egui::TextureId,
    /// Keeps the pool from recycling the texture while displayed.
    _tex: Arc<wgpu::Texture>,
    size: [u32; 2],
}

/// Loupe state — see the module docs.
pub struct LoupeView {
    render_state: eframe::egui_wgpu::RenderState,
    outcome: Arc<ShellOutcome>,
    ticket: Option<RenderTicket>,
    displayed: Option<Displayed>,
    last_key: Option<SubmitKey>,
    zoom: Zoom,
    pan: egui::Vec2,
    error: Option<String>,
    /// Navigation → texture-swap latency probe (T26 AC: < 50 ms p95).
    nav_started: Option<Instant>,
    /// Rolling nav-swap latencies (ms), newest last, bounded.
    pub nav_swap_ms: Vec<f32>,
}

impl LoupeView {
    /// A loupe compositing through `render_state`'s egui renderer.
    pub fn new(
        render_state: eframe::egui_wgpu::RenderState,
        outcome: Arc<ShellOutcome>,
    ) -> LoupeView {
        LoupeView {
            render_state,
            outcome,
            ticket: None,
            displayed: None,
            last_key: None,
            zoom: Zoom::Fit,
            pan: egui::Vec2::ZERO,
            error: None,
            nav_started: None,
            nav_swap_ms: Vec::new(),
        }
    }

    /// Reset for a fresh entry from the grid.
    pub fn enter(&mut self) {
        self.zoom = Zoom::Fit;
        self.pan = egui::Vec2::ZERO;
        self.error = None;
        self.last_key = None; // force a submit for the (possibly new) image
        self.nav_started = Some(Instant::now());
    }

    /// Release engine + egui resources when leaving the loupe.
    pub fn exit(&mut self, engine: &Engine) {
        if let Some(ticket) = self.ticket.take() {
            engine.cancel(&ticket);
        }
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
        self.ticket.is_some()
    }

    /// Renders one loupe frame for `rows[idx]`.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
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
        let max_dim = engine
            .gpu()
            .map_or(8192, |g| g.limits.max_texture_dimension_2d);
        let view_px = [
            ((view_rect.width() * ppp).round() as u32).clamp(1, max_dim),
            ((view_rect.height() * ppp).round() as u32).clamp(1, max_dim),
        ];

        // --- Submit on any relevant change; latest-wins coalescing on the
        // engine side supersedes whatever was in flight (T26). ---
        let key = SubmitKey {
            image: summary.id,
            zoom: self.zoom,
            out: match self.zoom {
                Zoom::Fit => view_px,
                Zoom::OneToOne => [0, 0], // Native: size-independent
            },
        };
        if self.last_key != Some(key) {
            let ticket = engine.submit(RenderRequest {
                image: summary.id,
                recipe: Recipe::identity(PV_M0),
                pv: PV_M0,
                roi: Roi::Full,
                scale: match self.zoom {
                    Zoom::Fit => RenderScale::FitWithin {
                        w: view_px[0],
                        h: view_px[1],
                    },
                    Zoom::OneToOne => RenderScale::Native,
                },
                target: RenderTarget::Texture,
                viewport: LOUPE_VIEWPORT,
            });
            self.ticket = Some(ticket);
            self.last_key = Some(key);
        }

        self.poll(engine);

        // --- Composite ---
        let response = ui.allocate_rect(view_rect, egui::Sense::click_and_drag());
        if response.double_clicked() {
            self.toggle_zoom();
        }
        if let Some(d) = &self.displayed {
            let logical = egui::vec2(d.size[0] as f32 / ppp, d.size[1] as f32 / ppp);
            let image_rect = match self.zoom {
                Zoom::Fit => fit_rect(view_rect, [logical.x, logical.y]),
                Zoom::OneToOne => {
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
            Zoom::Fit => Zoom::OneToOne,
            Zoom::OneToOne => Zoom::Fit,
        };
        self.pan = egui::Vec2::ZERO;
    }

    /// Polls the in-flight ticket; swaps the composited texture on `Ready`.
    fn poll(&mut self, engine: &Engine) {
        let Some(ticket) = self.ticket.clone() else {
            return;
        };
        match engine.poll(&ticket) {
            RenderState::Pending | RenderState::Running => {}
            RenderState::Ready(RenderOutput::Texture { tex, view, size }) => {
                self.swap_displayed(tex, &view, size);
                self.error = None;
                self.ticket = None;
                if let Some(started) = self.nav_started.take() {
                    let ms = started.elapsed().as_secs_f32() * 1000.0;
                    self.nav_swap_ms.push(ms);
                    if self.nav_swap_ms.len() > 256 {
                        self.nav_swap_ms.remove(0);
                    }
                }
            }
            RenderState::Ready(RenderOutput::Cpu(_)) => {
                // Unreachable: the loupe only submits RenderTarget::Texture.
                tracing::error!(target: "lightbox_shell", "unexpected CPU output in frame path");
                self.ticket = None;
            }
            RenderState::Failed(err) => {
                tracing::warn!(target: "lightbox_shell", %err, "loupe render failed");
                self.error = Some(err.to_string());
                // Don't keep compositing a stale image over an error state.
                if let Some(old) = self.displayed.take() {
                    self.render_state
                        .renderer
                        .write()
                        .free_texture(&old.texture_id);
                }
                self.ticket = None;
            }
            RenderState::Cancelled | RenderState::Superseded => {
                // A newer submit owns the viewport now; nothing to keep.
                self.ticket = None;
            }
        }
    }

    /// Registers a finished engine texture with egui, releasing the previous
    /// one — flicker-free swap on the SAME shared device (seam 2). Never
    /// copies pixels.
    fn swap_displayed(
        &mut self,
        tex: Arc<wgpu::Texture>,
        view: &wgpu::TextureView,
        size: [u32; 2],
    ) {
        let mut renderer = self.render_state.renderer.write();
        let texture_id = renderer.register_native_texture(
            &self.render_state.device,
            view,
            wgpu::FilterMode::Linear,
        );
        if let Some(old) = self.displayed.replace(Displayed {
            texture_id,
            _tex: tex,
            size,
        }) {
            renderer.free_texture(&old.texture_id);
        }
        self.outcome.texture_swaps.fetch_add(1, Ordering::AcqRel);
        self.outcome.seam_proven.store(true, Ordering::Release);
    }

    /// Minimal info overlay (T26): filename, dims, tier badge, zoom.
    fn info_overlay(
        &self,
        ui: &egui::Ui,
        view_rect: egui::Rect,
        summary: &ImageSummary,
        idx: usize,
        total: usize,
    ) {
        let text = format!(
            "{}  ·  {}×{}  ·  embedded preview  ·  {}  ·  {}/{}",
            summary.filename,
            summary.width,
            summary.height,
            match self.zoom {
                Zoom::Fit => "fit",
                Zoom::OneToOne => "100%",
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
