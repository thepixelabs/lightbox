// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-shell` — the thin, replaceable egui/eframe UI shell.
//!
//! E01 Phase 2 (T5 + T7): the **zero-copy seam tracer bullet**. The eframe
//! app shares one `wgpu::Device` with the render engine (architecture §2.3
//! seam 2); a `solid.color` render produced by `Engine::submit` is composited
//! into the egui frame by registering the engine's output texture with
//! `egui_wgpu::Renderer::register_native_texture` and drawing it with
//! `ui.image` — re-registered on every texture swap.
//!
//! **Zero-copy invariants (code-review checklist, spec T7):**
//! * NO `map_async` / CPU readback anywhere in the frame path — this crate
//!   never maps GPU memory (grep it).
//! * The engine receives the *shell's* device: asserted by `Arc` identity in
//!   a debug assertion, and enforced at runtime by wgpu itself — registering
//!   a texture created on a different device would fail validation.
//!
//! `PaintCallback` is the documented E05/E08 upgrade path when the loupe
//! needs tiling/gizmos drawn inside the egui paint graph; `ui.image` over a
//! registered native texture is sufficient (and simpler) for M0.
//!
//! Phase 7 (T25–T26) replaces the tracer scene with the real grid + loupe.
//! **E08** owns the full Library/Develop UX.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use eframe::egui;
use lightbox_edit::Recipe;
use lightbox_render::nodes::solid_color::{SolidColorNode, SolidColorPlanner};
use lightbox_render::{
    Engine, GpuContext, NodeRegistry, NullSourceResolver, RenderOutput, RenderRequest, RenderScale,
    RenderState, RenderTarget, RenderTicket, Roi, ViewportId,
};
use lightbox_types::{ImageId, PV_M0};

/// The tracer bullet's single viewport (the loupe's ancestor).
const TRACER_VIEWPORT: ViewportId = ViewportId(1);

/// Color animation rate: distinct hue steps per second. Each step submits a
/// fresh render — deliberately faster than the engine "needs", to exercise
/// latest-wins coalescing under a live UI.
const HUE_STEPS_PER_SEC: f64 = 12.0;

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    /// Smoke mode: run this many frames, assert the seam invariants held,
    /// then close (used by `lightbox --smoke N` and CI).
    pub smoke_frames: Option<u64>,
}

/// What the app observed, for smoke-mode verdicts (shared with [`run`]'s
/// caller because eframe consumes the app on close).
#[derive(Debug, Default)]
pub struct ShellOutcome {
    /// Frames painted.
    pub frames: AtomicU64,
    /// Engine textures registered with egui (texture swaps).
    pub texture_swaps: AtomicU64,
    /// True once an `Engine::submit`-produced texture was composited on the
    /// shared device (the seam-2 proof held at least once).
    pub seam_proven: AtomicBool,
}

/// Boots the eframe shell and blocks until the window closes.
///
/// Returns the observed [`ShellOutcome`]; in smoke mode the caller turns
/// `seam_proven == false` into a nonzero exit code.
pub fn run(options: ShellOptions) -> Result<Arc<ShellOutcome>, eframe::Error> {
    let outcome = Arc::new(ShellOutcome::default());
    let app_outcome = Arc::clone(&outcome);

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Lightbox — zero-copy seam tracer (E01 Phase 2)")
            .with_inner_size([960.0, 640.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(
                // T5: the shared device is created with the SAME descriptor
                // the headless path requests (features/limits in one place).
                eframe::egui_wgpu::WgpuSetupCreateNew {
                    device_descriptor: Arc::new(GpuContext::device_descriptor),
                    ..eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle()
                },
            ),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "lightbox",
        native_options,
        Box::new(move |cc| Ok(Box::new(LightboxApp::new(cc, options, app_outcome)?))),
    )?;
    Ok(outcome)
}

/// The engine texture currently composited into the egui frame.
struct Displayed {
    /// The egui-side handle (`register_native_texture`).
    texture_id: egui::TextureId,
    /// Keeps the wgpu texture alive: the pool only recycles unreferenced
    /// textures, so holding this is what makes swaps flicker-free.
    _tex: Arc<wgpu::Texture>,
    /// Size in physical pixels.
    size: [u32; 2],
}

struct LightboxApp {
    engine: Engine,
    planner: Arc<SolidColorPlanner>,
    gpu: GpuContext,
    render_state: eframe::egui_wgpu::RenderState,
    ticket: Option<RenderTicket>,
    displayed: Option<Displayed>,
    last_size: [u32; 2],
    last_hue_step: Option<u64>,
    last_error: Option<String>,
    options: ShellOptions,
    outcome: Arc<ShellOutcome>,
}

impl LightboxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        options: ShellOptions,
        outcome: Arc<ShellOutcome>,
    ) -> Result<LightboxApp, Box<dyn std::error::Error + Send + Sync>> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or("eframe did not provide a wgpu render state (renderer must be Wgpu)")?;

        // Seam 2: wrap eframe's device — the engine NEVER creates its own
        // under the shell (spec §3.4). wgpu devices are internally
        // ref-counted, so these clones are handles to the ONE device.
        let gpu = GpuContext::from_shared_device(
            render_state.device.clone(),
            render_state.queue.clone(),
            render_state.adapter.get_info(),
        );

        let node = Arc::new(SolidColorNode::new());
        let mut registry = NodeRegistry::new();
        registry.register(PV_M0, node);

        let engine = Engine::new(Some(gpu.clone()), registry, Arc::new(NullSourceResolver))
            .map_err(|e| format!("engine start failed: {e}"))?;
        let planner = Arc::new(SolidColorPlanner::new(hue_color(0)));
        engine.set_planner(planner.clone());

        // T7 debug assertion: the engine's device IS the shell's device.
        // (Runtime enforcement exists too: registering the engine's texture
        // with egui would fail wgpu validation across devices.)
        debug_assert!(
            Arc::ptr_eq(&engine.gpu().expect("gpu engine").device, &gpu.device),
            "engine must render on the shell's wgpu device (seam 2)"
        );

        tracing::info!(target: "lightbox_shell", "shell up, {}", gpu.adapter_report());

        Ok(LightboxApp {
            engine,
            planner,
            gpu,
            render_state,
            ticket: None,
            displayed: None,
            last_size: [0, 0],
            last_hue_step: None,
            last_error: None,
            options,
            outcome,
        })
    }

    /// Registers a finished engine texture with egui, releasing the previous
    /// one. This is the "re-register on texture swap" path of spec T7.
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
            // Dropping `old._tex` lets the pool recycle it for the next frame.
        }
        self.outcome.texture_swaps.fetch_add(1, Ordering::AcqRel);
        self.outcome.seam_proven.store(true, Ordering::Release);
    }

    fn poll_engine(&mut self) {
        let Some(ticket) = self.ticket.clone() else {
            return;
        };
        match self.engine.poll(&ticket) {
            RenderState::Pending | RenderState::Running => {}
            RenderState::Ready(RenderOutput::Texture { tex, view, size }) => {
                self.swap_displayed(tex, &view, size);
                self.last_error = None;
                self.ticket = None;
            }
            RenderState::Ready(RenderOutput::Cpu(_)) => {
                // Unreachable: the shell only submits RenderTarget::Texture.
                tracing::error!(target: "lightbox_shell", "unexpected CPU output in frame path");
                self.ticket = None;
            }
            RenderState::Failed(err) => {
                tracing::error!(target: "lightbox_shell", %err, "render failed");
                self.last_error = Some(err.to_string());
                self.ticket = None;
            }
            RenderState::Cancelled | RenderState::Superseded => {
                self.ticket = None;
            }
        }
    }
}

impl eframe::App for LightboxApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let pixels_per_point = ctx.pixels_per_point();

        egui::Panel::top(egui::Id::new("tracer-info")).show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "zero-copy seam tracer — {} | swaps: {} | {}",
                    self.gpu.adapter_report(),
                    self.outcome.texture_swaps.load(Ordering::Acquire),
                    match &self.last_error {
                        Some(e) => format!("last error: {e}"),
                        None => "engine ok".to_owned(),
                    },
                ));
            });
        });

        egui::CentralPanel::default().show(root, |ui| {
            // Desired output size in physical pixels (DPI-aware: logical
            // points × pixels_per_point — spec T7 "window resize + DPI").
            let avail = ui.available_size();
            let max_dim = self.gpu.limits.max_texture_dimension_2d;
            let want = [
                ((avail.x * pixels_per_point).round() as u32).clamp(1, max_dim),
                ((avail.y * pixels_per_point).round() as u32).clamp(1, max_dim),
            ];

            // Animate the color; resubmit on any change (size or step).
            // Latest-wins coalescing supersedes stale in-flight requests.
            let hue_step = (ctx.input(|i| i.time) * HUE_STEPS_PER_SEC) as u64;
            if want != self.last_size || Some(hue_step) != self.last_hue_step {
                self.planner.set_color(hue_color(hue_step));
                let ticket = self.engine.submit(RenderRequest {
                    image: ImageId(0),
                    recipe: Recipe::identity(PV_M0),
                    pv: PV_M0,
                    roi: Roi::Full,
                    scale: RenderScale::FitWithin {
                        w: want[0],
                        h: want[1],
                    },
                    target: RenderTarget::Texture,
                    viewport: TRACER_VIEWPORT,
                });
                self.ticket = Some(ticket);
                self.last_size = want;
                self.last_hue_step = Some(hue_step);
            }

            self.poll_engine();

            // Flicker-free: keep compositing the previous texture until the
            // newer ticket lands.
            if let Some(displayed) = &self.displayed {
                let logical = egui::vec2(
                    displayed.size[0] as f32 / pixels_per_point,
                    displayed.size[1] as f32 / pixels_per_point,
                );
                ui.image(egui::load::SizedTexture::new(displayed.texture_id, logical));
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("waiting for the first engine texture…");
                });
            }
        });

        let frames = self.outcome.frames.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(limit) = self.options.smoke_frames {
            if frames >= limit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Keep animating (and keep polling in-flight tickets).
        ctx.request_repaint();
    }
}

/// A pleasant sRGB color from a hue step (tiny HSV→RGB, s/v fixed).
fn hue_color(step: u64) -> [f32; 4] {
    let h = ((step * 7) % 360) as f32; // degrees
    let (s, v) = (0.55_f32, 0.85_f32);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_color_stays_in_unit_range_with_full_alpha() {
        for step in 0..=720 {
            let [r, g, b, a] = hue_color(step);
            for c in [r, g, b] {
                assert!((0.0..=1.0).contains(&c), "step {step}: {c} out of range");
            }
            assert_eq!(a, 1.0);
        }
    }

    #[test]
    fn hue_color_actually_animates() {
        assert_ne!(hue_color(0), hue_color(1));
    }
}
