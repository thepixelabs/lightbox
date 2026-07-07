// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-shell` — the thin, replaceable egui/eframe UI shell.
//!
//! E01 Phase 7 (T25/T26): the walking-skeleton Library — an eframe app that
//! opens a real catalog through the headless [`lightbox_core::Session`]
//! (seam 1), imports a folder via the command bus, shows a **virtualized
//! grid** of demand-driven, cancel-on-scroll-out thumbnails, and a **loupe
//! whose every pixel is produced by `Engine::submit`** and composited
//! zero-copy on the ONE `wgpu::Device` the shell shares with the engine
//! (architecture §2.3 seam 2; spec §9 M0 verbatim requirement).
//!
//! **Zero-copy invariants (code-review checklist, spec T7/T26):**
//! * NO `map_async`/CPU readback anywhere in the frame path — this crate
//!   never maps GPU memory (grep it). Thumbnails are CPU-decoded pixels
//!   (spec §3.6) uploaded once as egui textures; the loupe is the engine's
//!   texture registered via `register_native_texture`.
//! * The engine receives the *shell's* device: asserted by `Arc` identity in
//!   a debug assertion, and enforced at runtime by wgpu itself (registering
//!   a texture created on another device fails validation).
//!
//! `PaintCallback` remains the documented E05/E08 upgrade path for
//! tiling/gizmos inside the paint graph. **E08** owns the real Library UX
//! (culling grammar, filmstrip, compare/survey, panels, keymap).

mod grid;
mod loupe;
mod model;
mod perf;
mod smoke;
mod thumbs;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use lightbox_core::{ClosePolicy, Command, Core, CoreConfig, Event, Session};
use lightbox_render::GpuContext;

use crate::grid::GridAction;
use crate::loupe::{LoupeAction, LoupeView};
use crate::model::{ImageListModel, Selection};
use crate::perf::{ScrollDriver, ScrollPhase};
use crate::smoke::SmokeDriver;
use crate::thumbs::ThumbCache;

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    /// Smoke mode (CI): drive import→grid→loupe on a throwaway catalog,
    /// close after the seam is proven and this many frames painted.
    pub smoke_frames: Option<u64>,
    /// Perf mode (nightly, T28): scripted grid-scroll frame-time capture
    /// on a throwaway catalog for this many measured frames, then print a
    /// JSON summary line and close. Mutually exclusive with smoke mode.
    pub perf_scroll_frames: Option<u64>,
    /// The catalog to open (created when missing). Defaults to
    /// `./lightbox.lbdata`. Ignored in smoke/perf modes.
    pub catalog: Option<PathBuf>,
}

/// What the app observed, for smoke-mode verdicts (shared with [`run`]'s
/// caller because eframe consumes the app on close).
#[derive(Debug, Default)]
pub struct ShellOutcome {
    /// Frames painted.
    pub frames: AtomicU64,
    /// Engine textures registered with egui (loupe texture swaps).
    pub texture_swaps: AtomicU64,
    /// True once an `Engine::submit`-produced texture was composited on the
    /// shared device (the seam-2 proof held at least once).
    pub seam_proven: AtomicBool,
    /// True once a `--perf-scroll` capture completed and printed its
    /// summary (false = wedged/expired ⇒ nonzero exit).
    pub perf_ok: AtomicBool,
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
            .with_title("Lightbox")
            .with_inner_size([1100.0, 720.0]),
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

/// Which top-level view is showing (G/E toggle — spec T26).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum View {
    Grid,
    Loupe,
}

/// Import-bar state (M0: a path field, no native file dialog).
#[derive(Default)]
struct ImportUi {
    path: String,
    recursive: bool,
    /// `(done, discovered, current file)` while an import runs.
    active: Option<(u64, u64, String)>,
}

/// Rolling frame-time probe backing the debug overlay (T25 AC: p95 < 16 ms
/// measured here on a dev laptop).
struct FrameStats {
    last: Option<Instant>,
    samples_ms: Vec<f32>,
}

impl FrameStats {
    fn new() -> FrameStats {
        FrameStats {
            last: None,
            samples_ms: Vec::with_capacity(240),
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last.replace(now) {
            let ms = now.duration_since(last).as_secs_f32() * 1000.0;
            if self.samples_ms.len() >= 240 {
                self.samples_ms.remove(0);
            }
            self.samples_ms.push(ms);
        }
    }
}

/// Nearest-rank percentile over a small sample buffer.
fn percentile(samples: &[f32], p: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p * sorted.len() as f32).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

struct LightboxApp {
    session: Session,
    _core: Core,
    events: tokio::sync::broadcast::Receiver<Event>,
    gpu: GpuContext,

    view: View,
    model: ImageListModel,
    selection: Selection,
    thumbs: ThumbCache,
    loupe: LoupeView,
    import_ui: ImportUi,
    cell_size: f32,
    status: String,

    stats: FrameStats,
    show_overlay: bool,

    outcome: Arc<ShellOutcome>,
    smoke: Option<SmokeDriver>,
    perf: Option<ScrollDriver>,
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

        let smoke = match options.smoke_frames {
            Some(frames) => Some(SmokeDriver::new(frames.max(1))?),
            None => None,
        };
        let perf = match options.perf_scroll_frames {
            Some(frames) if smoke.is_none() => Some(ScrollDriver::new(frames)?),
            _ => None,
        };

        let core = Core::start(CoreConfig::default())?;
        let lbdata = match (&smoke, &perf) {
            (Some(smoke), _) => smoke.lbdata.clone(),
            (None, Some(perf)) => perf.lbdata.clone(),
            (None, None) => options
                .catalog
                .clone()
                .unwrap_or_else(|| PathBuf::from("lightbox.lbdata")),
        };
        let session = if lbdata.join("catalog.sqlite").is_file() {
            core.open_catalog(&lbdata, Some(gpu.clone()))?
        } else {
            core.create_catalog(&lbdata, Some(gpu.clone()))?
        };
        let events = session.events();

        // T7/T26 seam-2 invariant: the engine renders on the shell's device.
        // **F5 deviation:** `ng::Engine` does not expose the raw device handle
        // for an `Arc::ptr_eq` proof (the old E01 seed did via `Engine::gpu()`);
        // the guarantee is now structural instead of runtime-asserted —
        // `Session::open` builds `SharedDeviceProvider` from exactly the
        // `gpu.clone()` passed in here, and `Engine::with_compiler`'s `Auto`
        // path builds its `DeviceCtx` only from `DeviceProvider::current()` (no
        // other device-creation path exists on the `Auto` branch), so the
        // engine's GPU backend is provably built on this device by
        // construction. See `docs/plan/epics/E05-deviations.md`.
        debug_assert!(
            matches!(
                session.engine().active_backend(),
                lightbox_render::ng::ActiveBackend::Gpu(_)
            ),
            "engine must be GPU-active when the shell hands it a shared device (seam 2)"
        );

        tracing::info!(
            target: "lightbox_shell",
            catalog = %lbdata.display(),
            "shell up, {}",
            gpu.adapter_report()
        );

        let thumbs = ThumbCache::new(session.previews());
        let loupe = LoupeView::new(
            render_state,
            Arc::clone(&outcome),
            &session.render_scheduler(),
        );
        Ok(LightboxApp {
            session,
            _core: core,
            events,
            gpu,
            view: View::Grid,
            model: ImageListModel::new(),
            selection: Selection::default(),
            thumbs,
            loupe,
            import_ui: ImportUi::default(),
            cell_size: 144.0,
            status: String::new(),
            stats: FrameStats::new(),
            show_overlay: cfg!(debug_assertions),
            outcome,
            smoke,
            perf,
        })
    }

    /// Drains the event broadcast once per frame (spec §5.1 threading
    /// contract). Slow frames lag — they never wedge the writer.
    fn drain_events(&mut self) {
        use tokio::sync::broadcast::error::TryRecvError;
        loop {
            match self.events.try_recv() {
                Ok(Event::CatalogChanged { .. }) => {
                    self.model.mark_dirty();
                    self.thumbs.clear_failures();
                }
                Ok(Event::ImportStarted { .. }) => {
                    self.import_ui.active = Some((0, 0, String::new()));
                }
                Ok(Event::ImportProgress {
                    done,
                    discovered,
                    current,
                    ..
                }) => {
                    self.import_ui.active = Some((
                        done,
                        discovered,
                        current
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ));
                    // Keep the grid growing during the import (M0 exit:
                    // browsable during import) without re-querying at event
                    // rate.
                    self.model.mark_dirty_throttled(Duration::from_millis(500));
                }
                Ok(Event::ImportFinished { report, .. }) => {
                    self.import_ui.active = None;
                    self.status = format!(
                        "imported {} (skipped {} duplicates, {} errors) in {:.1}s",
                        report.imported,
                        report.skipped_duplicates,
                        report.errors.len(),
                        report.took.as_secs_f64(),
                    );
                    self.model.mark_dirty();
                }
                Ok(Event::CommandFailed { error, .. }) => {
                    self.status = format!("command failed: {error}");
                    self.import_ui.active = None;
                }
                Ok(Event::BackupFinished { report }) => {
                    self.status = format!("backup written: {}", report.path.display());
                }
                Ok(Event::DeviceDegraded { reason }) => {
                    self.status = format!("GPU device degraded: {reason}");
                }
                Ok(_) => {}
                Err(TryRecvError::Lagged(_)) => {
                    // Missed events: any of them could have been a
                    // CatalogChanged.
                    self.model.mark_dirty();
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.view {
                View::Grid => {
                    ui.label("Import folder:");
                    let width = (ui.available_width() - 420.0).clamp(120.0, 420.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_ui.path)
                            .hint_text("/path/to/photos")
                            .desired_width(width),
                    );
                    ui.checkbox(&mut self.import_ui.recursive, "recursive");
                    let importing = self.import_ui.active.is_some();
                    let import_clicked = ui
                        .add_enabled(
                            !importing && !self.import_ui.path.trim().is_empty(),
                            egui::Button::new("Import"),
                        )
                        .clicked();
                    if import_clicked {
                        self.session.submit(Command::ImportAddInPlace {
                            source_dir: PathBuf::from(self.import_ui.path.trim()),
                            recursive: self.import_ui.recursive,
                        });
                        self.status = format!("importing {}…", self.import_ui.path.trim());
                    }
                    ui.separator();
                    ui.label("Size:");
                    ui.add(egui::Slider::new(&mut self.cell_size, 64.0..=320.0).show_value(false));
                }
                View::Loupe => {
                    if ui.button("◀ Grid (G)").clicked() {
                        self.exit_loupe();
                    }
                    ui.label("←/→ navigate · Z/Space/double-click zoom · fit ↔ 100%");
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((done, discovered, current)) = &self.import_ui.active {
                    ui.label(format!("importing {done}/{discovered} — {current}"));
                    ui.spinner();
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let selected = if self.selection.is_empty() {
                String::new()
            } else {
                format!(" · {} selected", self.selection.len())
            };
            ui.label(format!("{} images{selected}", self.model.rows().len()));
            ui.separator();
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!(
                    "schema v{} · {:?} · F1 stats",
                    self.session.schema_version(),
                    self.gpu.backend,
                ));
            });
        });
    }

    fn overlay(&self, ctx: &egui::Context) {
        let thumb_stats = self.thumbs.stats();
        let frame_p95 = percentile(&self.stats.samples_ms, 0.95);
        let frame_max = self.stats.samples_ms.iter().copied().fold(0.0f32, f32::max);
        let nav_p95 = percentile(&self.loupe.nav_swap_ms, 0.95);
        egui::Window::new("frame stats")
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 32.0))
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.monospace(format!(
                    "frame ms   p95 {frame_p95:6.2}  max {frame_max:6.2} (n={})",
                    self.stats.samples_ms.len()
                ));
                ui.monospace(format!(
                    "thumbs     cached {} inflight {} failed {}",
                    thumb_stats.cached, thumb_stats.inflight, thumb_stats.failed
                ));
                ui.monospace(format!(
                    "requests   issued {} cancelled {}",
                    thumb_stats.requested_total, thumb_stats.cancelled_total
                ));
                ui.monospace(format!(
                    "nav swap   p95 {nav_p95:6.2} ms (n={})",
                    self.loupe.nav_swap_ms.len()
                ));
                ui.monospace(format!("images     {}", self.model.rows().len()));
                ui.monospace(format!(
                    "swaps      {}",
                    self.outcome.texture_swaps.load(Ordering::Acquire)
                ));
            });
    }

    fn enter_loupe(&mut self, idx: usize) {
        if let Some(summary) = self.model.rows().get(idx) {
            self.selection.set_focus(summary.id);
            self.loupe.enter();
            self.view = View::Loupe;
        }
    }

    fn exit_loupe(&mut self) {
        self.loupe.exit();
        self.view = View::Grid;
    }

    /// Perf scripting (T28 grid-scroll capture): submit the import once,
    /// then drive a forced sawtooth scroll through the REAL grid path.
    /// Returns the scroll fraction to force this frame, if any.
    fn pump_perf(&mut self, ctx: &egui::Context) -> Option<f32> {
        let perf = self.perf.as_mut()?;
        if !perf.submitted {
            perf.submitted = true;
            let photos = perf.photos.clone();
            self.session.submit(Command::ImportAddInPlace {
                source_dir: photos,
                recursive: false,
            });
        }
        let stats = self.thumbs.stats();
        match perf.pump(
            self.model.rows().len(),
            (stats.requested_total, stats.cancelled_total),
        ) {
            ScrollPhase::Warmup => None,
            ScrollPhase::Scroll(frac) => Some(frac),
            ScrollPhase::Done(json) => {
                // The nightly workflow scrapes this line into the summary.
                println!("{json}");
                self.outcome.perf_ok.store(true, Ordering::Release);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                None
            }
            ScrollPhase::Expired => {
                tracing::error!(
                    target: "lightbox_shell",
                    "perf-scroll run expired before the capture completed"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                None
            }
        }
    }

    /// Smoke scripting: submit the import once, hop into the loupe once a
    /// row exists, close when proven (or wedged — nonzero exit).
    fn pump_smoke(&mut self, ctx: &egui::Context) {
        let Some(smoke) = &mut self.smoke else {
            return;
        };
        if !smoke.submitted {
            let photos = smoke.photos.clone();
            smoke.submitted = true;
            self.session.submit(Command::ImportAddInPlace {
                source_dir: photos,
                recursive: false,
            });
        }
        let want_loupe = !smoke.opened_loupe && !self.model.rows().is_empty();
        if want_loupe {
            self.smoke.as_mut().expect("smoke mode").opened_loupe = true;
            self.enter_loupe(0);
        }

        let smoke = self.smoke.as_ref().expect("smoke mode");
        let frames = self.outcome.frames.load(Ordering::Acquire);
        let proven = self.outcome.seam_proven.load(Ordering::Acquire);
        if smoke.done(frames, proven) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if smoke.expired(frames) {
            tracing::error!(
                target: "lightbox_shell",
                frames,
                proven,
                "smoke run expired before the seam was proven"
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for LightboxApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.stats.tick();
        self.drain_events();
        self.model.pump(&self.session);
        self.thumbs.pump(&ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.show_overlay = !self.show_overlay;
        }

        egui::Panel::top(egui::Id::new("lightbox-top")).show(root, |ui| self.top_bar(ui));
        egui::Panel::bottom(egui::Id::new("lightbox-status")).show(root, |ui| self.status_bar(ui));

        let forced_scroll = self.pump_perf(&ctx);
        let mut visible: HashSet<lightbox_types::ImageId> = HashSet::new();
        egui::CentralPanel::default().show(root, |ui| match self.view {
            View::Grid => {
                let action = grid::grid_ui(
                    ui,
                    self.model.rows(),
                    &mut self.selection,
                    &mut self.thumbs,
                    self.cell_size,
                    &mut visible,
                    forced_scroll,
                );
                if let Some(GridAction::OpenLoupe(idx)) = action {
                    self.enter_loupe(idx);
                }
            }
            View::Loupe => {
                let scheduler = self.session.render_scheduler();
                let max_tex_dim = self.gpu.limits.max_texture_dimension_2d;
                let idx = self
                    .selection
                    .focus()
                    .and_then(|id| self.model.index_of(id));
                match idx {
                    Some(idx) => {
                        match self
                            .loupe
                            .ui(ui, &scheduler, max_tex_dim, self.model.rows(), idx)
                        {
                            Some(LoupeAction::ExitToGrid) => self.exit_loupe(),
                            Some(LoupeAction::Navigate(next)) => {
                                if let Some(s) = self.model.rows().get(next) {
                                    self.selection.set_focus(s.id);
                                }
                            }
                            None => {}
                        }
                    }
                    // The focused image vanished (undo-import): back to grid.
                    None => self.exit_loupe(),
                }
            }
        });

        // Cancel-on-scroll-out + O(visible) texture memory (T25). The cap
        // keeps a small navigation cushion above the visible set.
        let cap = (visible.len() * 3).max(64);
        self.thumbs.end_frame(&visible, cap);

        if self.show_overlay {
            self.overlay(&ctx);
        }

        self.outcome.frames.fetch_add(1, Ordering::AcqRel);
        self.pump_smoke(&ctx);

        // Repaint policy: run hot while anything is in flight; otherwise a
        // slow idle poll keeps the event pump alive without burning a core.
        let busy = self.model.loading()
            || self.thumbs.stats().inflight > 0
            || self.loupe.busy()
            || self.import_ui.active.is_some()
            || self.smoke.is_some()
            || self.perf.is_some();
        if busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn on_exit(&mut self) {
        // Exit-time verified backup per policy (spec T15/OQ-6); smoke and
        // perf runs skip it (throwaway catalogs).
        let policy = if self.smoke.is_some() || self.perf.is_some() {
            ClosePolicy::Skip
        } else {
            ClosePolicy::Auto
        };
        match self.session.clone().close(policy.into()) {
            Ok(report) => {
                if let Some(backup) = report.backup {
                    tracing::info!(
                        target: "lightbox_shell",
                        path = %backup.path.display(),
                        "exit-time verified backup written"
                    );
                }
            }
            Err(err) => {
                tracing::error!(target: "lightbox_shell", %err, "session close failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(percentile(&[5.0], 0.95), 5.0);
        let v: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        assert_eq!(percentile(&v, 0.95), 95.0);
        assert_eq!(percentile(&v, 0.5), 50.0);
        // Unsorted input is handled.
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 1.0), 3.0);
    }

    #[test]
    fn frame_stats_buffer_is_bounded() {
        let mut stats = FrameStats::new();
        for _ in 0..500 {
            stats.tick();
        }
        assert!(stats.samples_ms.len() <= 240);
    }
}
