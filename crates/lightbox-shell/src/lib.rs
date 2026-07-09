// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-shell` — the thin, replaceable egui/eframe UI shell.
//!
//! **E08 Phase A rescope**: this crate is the **editor shell** of the v2.0
//! product (`docs/plan/epics/E08-editor-shell.md`) — there is no library, no
//! grid view, no culling. The window *is* the editor: a top bar (open
//! affordances + view controls), a central canvas, a bottom session
//! filmstrip, a right develop-panel rail (placeholder until Phase E), and a
//! status bar. Files enter by drag-and-drop, the OS open dialog, the
//! transient live-filesystem folder explorer (mandate v2.2), or launch-time
//! argv paths — every entry point normalizes to exactly one
//! `Command::OpenWorkingSet` (E04's command, consumed via
//! `Session::working_set()`/`Event::WorkingSet*`). Opening a working set
//! auto-activates its first `Ready` entry into the canvas; every displayed
//! pixel is still produced by `Engine::submit` and composited **zero-copy**
//! on the ONE `wgpu::Device` the shell shares with the engine (architecture
//! §2.3 seam 2).
//!
//! **Zero-copy invariants (code-review checklist):**
//! * NO `map_async`/CPU readback anywhere in the frame path — this crate
//!   never maps GPU memory (grep it). Thumbnails are CPU-decoded pixels
//!   uploaded once as egui textures; the canvas is the engine's texture
//!   registered via `register_native_texture`.
//! * The engine receives the *shell's* device: asserted by a debug
//!   assertion, and enforced at runtime by wgpu itself.
//!
//! **Phase A scope** (`docs/plan/epics/E08-editor-shell.md` §8 Phase A):
//! chassis rescope, entry intake (drop/dialog/launch), the working-set view
//! model, replace-set semantics, the smoke driver, and the mandate-v2.2
//! folder-explorer UI. **Phase B** completed the session filmstrip (B1–B5:
//! geometry, virtualized chrome, §6.3 badges, nav/multi-select,
//! resize/collapse/overflow polish — see `filmstrip.rs`'s module docs,
//! including the Phase-G persistence seam for the strip height/collapse
//! prefs). **Phase C** generalizes the E01/F5 loupe into the `canvas`
//! module (`ViewXform`, the zoom ladder, recipe-driven submit, progressive
//! tier→engine display, and canvas states — see `canvas/mod.rs`'s module
//! docs). **Phase D** adds the remappable keymap (`keymap/` — spec §6.8):
//! every keyboard route now goes through one per-frame dispatcher that
//! runs **before** any widget is built (matched chords are consumed;
//! text-input focus suppresses non-modifier chords), with `keymap.toml`
//! overrides and the ⌘/ cheat-sheet overlay. The per-widget key handlers
//! Phases B/C carried (`filmstrip::nav_delta`, the canvas's arrow/Z/Space
//! block) are gone — `nav.*`/`view.*` actions replaced them, exactly the
//! collapse their TODOs promised. The develop-panel rail is still a
//! placeholder (Phase E, toggled by `panel.toggle_rail`); the prefs store
//! (Phase G) does not exist yet; the D5 rebind editor is the phase's
//! named cut-line (the registry API it needs is complete and tested).

mod canvas;
mod empty_state;
mod explorer;
mod filmstrip;
mod intake;
mod keymap;
mod smoke;
mod thumbs;
mod working_set;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use lightbox_core::{
    ClosePolicy, Command, Core, CoreConfig, Event, ItemState, OpenOrigin, OpenRequest, Session,
};
use lightbox_render::GpuContext;
use lightbox_types::ImageId;

use crate::canvas::{ActiveEntry, CanvasContent, EditorCanvas, SessionRecipeSource};
use crate::empty_state::empty_state_ui;
use crate::explorer::{ExplorerAction, FolderExplorer};
use crate::filmstrip::{EditedBadges, FilmstripAction, FilmstripState};
use crate::keymap::cheatsheet::CheatSheet;
use crate::keymap::{ActionId, ContextId, KeymapRegistry};
use crate::smoke::SmokeDriver;
use crate::thumbs::ThumbCache;
use crate::working_set::WorkingSetView;

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    /// Smoke mode (CI): drive open→filmstrip→canvas on a throwaway catalog,
    /// close after the seam is proven and this many frames painted.
    pub smoke_frames: Option<u64>,
    /// The catalog to open (created when missing). Defaults to
    /// `./lightbox.lbdata`. Ignored in smoke mode.
    pub catalog: Option<PathBuf>,
    /// Files/folders to open at launch (A4: argv paths / the future
    /// file-association handler) — intake runs before the first frame.
    pub initial_paths: Vec<PathBuf>,
    /// Folder-expansion mode for `initial_paths`.
    pub initial_recursive: bool,
}

/// What the app observed, for smoke-mode verdicts (shared with [`run`]'s
/// caller because eframe consumes the app on close).
#[derive(Debug, Default)]
pub struct ShellOutcome {
    /// Frames painted.
    pub frames: AtomicU64,
    /// Engine textures registered with egui (canvas texture swaps).
    pub texture_swaps: AtomicU64,
    /// True once an `Engine::submit`-produced texture was composited on the
    /// shared device (the seam-2 proof held at least once).
    pub seam_proven: AtomicBool,
    /// True once the filmstrip rendered at least one entry row (A7: the
    /// "filmstrip row → canvas seam" smoke proof).
    pub filmstrip_shown: AtomicBool,
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

/// Rolling frame-time probe backing the debug overlay.
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

    working_set: WorkingSetView,
    thumbs: ThumbCache,
    canvas: EditorCanvas,
    /// C3 recipe seam: E09's persisted `EditStore::recipe_of`, cached with
    /// the same dirty-flag discipline `badges` uses (marked dirty on
    /// `Event::EditCommitted` in `drain_events`). Phase E's live
    /// `EditBinding` replaces this — `EditorCanvas` doesn't change either
    /// way (see `canvas::RecipeSource`'s doc comment).
    recipe_source: SessionRecipeSource,
    /// `Some(reason)` once `Event::DeviceDegraded` has fired this session
    /// (spec §7/C5: M0 posture — no device-recovery event exists yet, so
    /// this is sticky rather than cleared; the full rebuild harness is
    /// E05.5). Drives the canvas's non-modal corner chip.
    device_degraded: Option<String>,
    /// B4/B5 strip UI state (selection, height, collapse) — in-session;
    /// Phase G persists height/collapsed (see `filmstrip.rs` module docs).
    filmstrip: FilmstripState,
    /// B3 edited-dot cache over `Queries::edit_badges` (event-driven).
    badges: EditedBadges,
    /// D1/D2: the action registry (M1 defaults + `keymap.toml` overrides),
    /// resolved by the per-frame dispatcher at the top of `ui()`.
    registry: KeymapRegistry,
    /// D4: the ⌘/ cheat-sheet overlay.
    cheatsheet: CheatSheet,
    /// D2 `panel.toggle_rail`: whether the right develop rail is shown
    /// (in-session; Phase G persists workspace layout).
    rail_visible: bool,
    explorer: FolderExplorer,
    /// The active image the canvas last rendered — compared each frame so a
    /// change (click/nav/auto-activation) resets zoom/pan exactly once
    /// (mirrors the retired `enter_loupe`'s reset-on-entry behavior).
    last_shown_image: Option<ImageId>,
    status: String,

    stats: FrameStats,
    show_overlay: bool,

    outcome: Arc<ShellOutcome>,
    smoke: Option<SmokeDriver>,
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

        let core = Core::start(CoreConfig::default())?;
        let lbdata = match &smoke {
            Some(smoke) => smoke.lbdata.clone(),
            None => options
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

        // T7 seam-2 invariant: the engine renders on the shell's device.
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

        // A4: argv paths (or any other pre-launch caller of `run`) open
        // before the first frame is painted. Smoke mode stages/opens its
        // own fixture instead (see `pump_smoke`).
        if smoke.is_none() && !options.initial_paths.is_empty() {
            session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(
                    options.initial_paths.clone(),
                    options.initial_recursive,
                    OpenOrigin::Cli,
                ),
            });
        }

        let thumbs = ThumbCache::new(session.previews());
        let canvas = EditorCanvas::new(
            render_state,
            Arc::clone(&outcome),
            &session.render_scheduler(),
            session.previews(),
        );
        let recipe_source = SessionRecipeSource::new(session.clone());
        let working_set = WorkingSetView::new(&session);

        // D1/D3: the M1 action set, plus the user's keymap.toml rebind
        // delta. Smoke runs skip the user's file (deterministic CI, same
        // posture as the throwaway catalog). A corrupt/partial file is
        // NEVER fatal (§7) — defaults + a status notice.
        let mut registry = keymap::default_registry();
        let mut status = String::new();
        if smoke.is_none() {
            match registry.load_overrides(&keymap::overrides::default_keymap_path()) {
                Ok(report) => {
                    if let Some(notice) = report.notice() {
                        tracing::warn!(target: "lightbox_shell", "{notice}");
                        status = notice;
                    }
                }
                Err(err) => {
                    tracing::warn!(target: "lightbox_shell", %err, "keymap overrides unreadable");
                    status = err.to_string();
                }
            }
        }

        Ok(LightboxApp {
            working_set,
            _core: core,
            events,
            gpu,
            thumbs,
            canvas,
            recipe_source,
            device_degraded: None,
            filmstrip: FilmstripState::new(),
            badges: EditedBadges::new(),
            registry,
            cheatsheet: CheatSheet::new(),
            rail_visible: true,
            explorer: FolderExplorer::closed(),
            last_shown_image: None,
            status,
            stats: FrameStats::new(),
            show_overlay: cfg!(debug_assertions),
            outcome,
            smoke,
            session,
        })
    }

    /// Drains the event broadcast once per frame (spec §5.1 threading
    /// contract). Slow frames lag — they never wedge the writer.
    fn drain_events(&mut self) {
        use tokio::sync::broadcast::error::TryRecvError;
        loop {
            match self.events.try_recv() {
                Ok(
                    ev @ (Event::WorkingSetOpening { .. }
                    | Event::WorkingSetReplaced { .. }
                    | Event::WorkingSetChanged { .. }
                    | Event::WorkingSetLoadFinished { .. }),
                ) => {
                    if matches!(ev, Event::WorkingSetOpening { .. }) {
                        // A6: a replace never prompts, but a previously
                        // -failed thumbnail is worth retrying — the new
                        // epoch may reopen the very same content hash (same
                        // `ImageId`) at a fixed/relocated path.
                        self.thumbs.clear_failures();
                    }
                    if let Event::WorkingSetLoadFinished { report, .. } = &ev {
                        self.status = format!(
                            "opened {} (reused {}, relocated {}, {} failed, {} duplicates) in {:.1}s",
                            report.ready,
                            report.reused,
                            report.relocated,
                            report.failed,
                            report.collapsed,
                            report.took.as_secs_f64(),
                        );
                    }
                    self.working_set.on_event(&ev, &self.session);
                    // B3: new/changed entries need a fresh badge pull.
                    self.badges.mark_dirty();
                }
                Ok(Event::EditCommitted { image, .. }) => {
                    // B3: a durable edit may flip an `is_edited` badge.
                    self.badges.mark_dirty();
                    // C3: the persisted recipe for this image changed —
                    // the canvas's next `recipe_for` re-reads it (no
                    // per-frame SQL otherwise, spec §7).
                    self.recipe_source.mark_dirty(image);
                }
                Ok(Event::CommandFailed { error, .. }) => {
                    self.status = format!("command failed: {error}");
                }
                Ok(Event::BackupFinished { report }) => {
                    self.status = format!("backup written: {}", report.path.display());
                }
                Ok(Event::DeviceDegraded { reason }) => {
                    self.status = format!("GPU device degraded: {reason}");
                    // C5: the canvas's non-modal chip (sticky — see the
                    // `device_degraded` field doc comment).
                    self.device_degraded = Some(reason);
                }
                Ok(_) => {}
                Err(TryRecvError::Lagged(_)) => {
                    self.working_set.on_lagged(&self.session);
                    self.badges.mark_dirty();
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            }
        }
    }

    /// Submits exactly one `Command::OpenWorkingSet` (A2/A6: a new drop or
    /// dialog pick always *replaces* the working set — no prompt, ever).
    fn open(&mut self, request: OpenRequest) {
        self.status = format!("opening {} item(s)…", request.paths.len());
        self.session.submit(Command::OpenWorkingSet { request });
    }

    /// `app.open` / the "Open Files…" button: the OS multi-select dialog
    /// → one `OpenWorkingSet` (A3 path, now also keymap-reachable).
    fn open_files_dialog(&mut self) {
        if let Some(paths) = rfd::FileDialog::new().set_title("Open Images").pick_files() {
            self.open(OpenRequest::new(paths, false, OpenOrigin::OpenDialog));
        }
    }

    /// D2: the frame's context stack (spec §6.8) — `app` → `editor`, plus
    /// `editor.loupe` while the canvas shows an active entry. Phase E/F
    /// push `editor.panels` / `editor.gizmo.*` when they exist.
    fn keymap_stack(&self) -> Vec<ContextId> {
        let mut stack = vec![keymap::CTX_APP, keymap::CTX_EDITOR];
        if self.working_set.active().is_some() {
            stack.push(keymap::CTX_LOUPE);
        }
        stack
    }

    /// D2: one dispatched action → its M1 target. Every arm goes through
    /// the same seam a click would (filmstrip nav = `WorkingSetView::nav`,
    /// zoom = the canvas's C2 entry points, undo/redo = E09's
    /// `Command::Edit`, open = the A3 intake path).
    fn handle_action(&mut self, action: ActionId) {
        match action {
            keymap::APP_OPEN => self.open_files_dialog(),
            keymap::APP_PREFS => {
                // Stub target: the performance-preferences panel is Phase
                // G — the action exists (and is rebindable) now.
                self.status = "Preferences land in E08 Phase G.".to_owned();
            }
            keymap::APP_CHEATSHEET => self.cheatsheet.toggle(),
            keymap::EDIT_UNDO | keymap::EDIT_REDO => {
                // Only a Ready entry has an image to address (§6.4/§6.5).
                if let Some(image) = self.working_set.active_image() {
                    let cmd = if action == keymap::EDIT_UNDO {
                        lightbox_core::EditCommand::Undo { image }
                    } else {
                        lightbox_core::EditCommand::Redo { image }
                    };
                    self.session.submit(Command::Edit(cmd));
                }
            }
            keymap::NAV_NEXT => self.working_set.nav(1),
            keymap::NAV_PREV => self.working_set.nav(-1),
            keymap::VIEW_ZOOM_TOGGLE | keymap::VIEW_ZOOM_TOGGLE_ALT => {
                self.canvas.toggle_zoom_button();
            }
            keymap::VIEW_ZOOM_IN => self.canvas.zoom_step(1),
            keymap::VIEW_ZOOM_OUT => self.canvas.zoom_step(-1),
            keymap::PANEL_TOGGLE_RAIL => self.rail_visible = !self.rail_visible,
            keymap::FILM_TOGGLE => {
                let collapsed = self.filmstrip.collapsed();
                self.filmstrip.set_collapsed(!collapsed);
            }
            // Registered per §6.8 so they're rebindable/visible in the
            // cheat sheet, but their context (`editor.gizmo`) is never on
            // the stack until Phase F's GizmoLayer pushes it.
            keymap::GIZMO_CANCEL | keymap::GIZMO_COMMIT => {}
            other => {
                debug_assert!(false, "dispatched action {:?} has no handler", other.0);
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, intake_skipped: usize) {
        ui.horizontal(|ui| {
            if ui.button("Open Files…").clicked() {
                self.open_files_dialog();
            }
            if ui.button("Open Folder…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Open Folder")
                    .pick_folder()
                {
                    self.open(OpenRequest::new(vec![dir], false, OpenOrigin::OpenDialog));
                }
            }
            if ui.button("Browse Folders…").clicked() {
                self.explorer.open_at(explorer::default_start_dir());
            }
            ui.separator();
            if ui
                .button(format!("Zoom: {}", self.canvas.zoom_mode().label()))
                .clicked()
            {
                self.canvas.toggle_zoom_button();
            }

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match self.working_set.phase() {
                    lightbox_core::SetPhase::Planning | lightbox_core::SetPhase::Loading => {
                        ui.spinner();
                        ui.label(format!(
                            "opening… {} found",
                            self.working_set.entries().len()
                        ));
                    }
                    _ => {
                        if intake_skipped > 0 {
                            ui.label(format!(
                                "{intake_skipped} dropped item(s) had no path — skipped"
                            ));
                        }
                    }
                },
            );
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let n = self.working_set.entries().len();
            let active = self
                .working_set
                .active_index()
                .map(|i| format!(" · {}/{}", i + 1, n))
                .unwrap_or_default();
            let truncated = if self.working_set.truncated() {
                " · truncated (max set size reached)"
            } else {
                ""
            };
            ui.label(format!("{n} images{active}{truncated}"));
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
        let nav_p95 = percentile(&self.canvas.nav_swap_ms, 0.95);
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
                    self.canvas.nav_swap_ms.len()
                ));
                ui.monospace(format!(
                    "images     {} (epoch {})",
                    self.working_set.entries().len(),
                    self.working_set.epoch()
                ));
                ui.monospace(format!(
                    "swaps      {}",
                    self.outcome.texture_swaps.load(Ordering::Acquire)
                ));
            });
    }

    /// Perf/A7 smoke scripting: submit the open once, then wait for the
    /// working set to auto-activate (§2.4) before requiring the seam proof.
    fn pump_smoke(&mut self, ctx: &egui::Context) {
        let Some(smoke) = &mut self.smoke else {
            return;
        };
        if !smoke.submitted {
            let photos = smoke.photos.clone();
            smoke.submitted = true;
            self.session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(vec![photos], false, OpenOrigin::Cli),
            });
        }

        let filmstrip_shown = self.outcome.filmstrip_shown.load(Ordering::Acquire);
        let frames = self.outcome.frames.load(Ordering::Acquire);
        let proven = self.outcome.seam_proven.load(Ordering::Acquire);
        let smoke = self.smoke.as_ref().expect("smoke mode");
        if smoke.done(frames, filmstrip_shown, proven) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if smoke.expired(frames) {
            tracing::error!(
                target: "lightbox_shell",
                frames,
                filmstrip_shown,
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
        self.thumbs.pump(&ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.show_overlay = !self.show_overlay;
        }

        // D2: keymap dispatch — BEFORE any widget is built, so matched
        // chords are consumed and never double-handled (keymap/dispatch.rs
        // module docs). The stack is rebuilt per frame; handlers run
        // immediately (same frame as the input, §7).
        let stack = self.keymap_stack();
        for action in keymap::dispatch::dispatch(&ctx, &self.registry, &stack) {
            self.handle_action(action);
        }

        // A2: fold this frame's drop/hover input. The recursive default is
        // fixed at `false` in Phase A — the real prefs knob (§6.7
        // `drop_recursive_default`) lands with Phase G; Alt still overrides
        // per-drop regardless.
        let intake_frame = ctx.input(|i| intake::pump(i, false));
        if intake_frame.skipped_pathless > 0 {
            self.status = format!(
                "{} dropped item(s) had no resolvable path — skipped",
                intake_frame.skipped_pathless
            );
        }
        if let Some(request) = intake_frame.open {
            // A2 AC: exactly one `OpenWorkingSet` per drop.
            self.open(request);
        }

        if let Some(action) = self.explorer.ui(&ctx) {
            match action {
                ExplorerAction::OpenImage(path) => {
                    self.open(OpenRequest::new(vec![path], false, OpenOrigin::OpenDialog));
                }
                ExplorerAction::OpenFolder(dir) => {
                    self.open(OpenRequest::new(vec![dir], false, OpenOrigin::OpenDialog));
                }
            }
        }

        egui::Panel::top(egui::Id::new("lightbox-top"))
            .show(root, |ui| self.top_bar(ui, intake_frame.skipped_pathless));
        egui::Panel::bottom(egui::Id::new("lightbox-status")).show(root, |ui| self.status_bar(ui));

        // Right develop-panel rail placeholder (A1 chassis AC; real content
        // is Phase E's `panels/` framework). `panel.toggle_rail` (Tab)
        // shows/hides it.
        if self.rail_visible {
            egui::Panel::right(egui::Id::new("lightbox-develop-rail"))
                .resizable(false)
                .default_size(220.0)
                .show(root, |ui| {
                    ui.heading("Develop");
                    ui.weak("Panels land in E08 Phase E.");
                });
        }

        // C5: project the active working-set entry's `ItemState` into a
        // `CanvasContent` the canvas owns rendering for (placard/shimmer/
        // normal render — `canvas::states`). Owned (not borrowed) so this
        // doesn't hold a live borrow of `self.working_set` across the rest
        // of the frame (mirrors the pre-Phase-C `ready` tuple's same
        // clone-out-of-the-view-model discipline).
        enum ActiveProjection {
            Loading,
            SourceFailed(String),
            Duplicate(usize),
            Ready {
                image: ImageId,
                filename: String,
                width: u32,
                height: u32,
            },
        }
        let projection = self.working_set.active().map(|item| match item.state {
            ItemState::Planned => ActiveProjection::Loading,
            ItemState::Ready { image, .. } => ActiveProjection::Ready {
                image,
                filename: item.filename.clone(),
                width: item.width,
                height: item.height,
            },
            ItemState::Failed => ActiveProjection::SourceFailed(
                item.decode_error
                    .clone()
                    .unwrap_or_else(|| "open failed".to_owned()),
            ),
            ItemState::DuplicateOf { index } => ActiveProjection::Duplicate(index),
        });

        // Bottom filmstrip strut (Phase B: resizable / collapsible, badge
        // chrome from the event-driven edit-badge cache).
        let mut visible: HashSet<lightbox_types::ImageId> = HashSet::new();
        if !self.working_set.is_empty() {
            let total = self.working_set.entries().len();
            let active = self.working_set.active_index();
            self.filmstrip.sync_set(self.working_set.epoch(), total);
            self.badges
                .refresh_if_dirty(&self.session, self.working_set.entries());

            if self.filmstrip.collapsed() {
                // Distinct panel id: the collapsed bar's exact size must
                // not be remembered as the expanded strip's height.
                egui::Panel::bottom(egui::Id::new("lightbox-filmstrip-collapsed"))
                    .exact_size(filmstrip::COLLAPSED_BAR_PT)
                    .show(root, |ui| {
                        filmstrip::collapsed_bar_ui(ui, &mut self.filmstrip, active, total);
                    });
            } else {
                let entries = self.working_set.entries();
                let badges = &self.badges;
                let strip = &mut self.filmstrip;
                let thumbs = &mut self.thumbs;
                let cell_of = |i: usize| filmstrip::cell_for(&entries[i], badges);
                let shown = egui::Panel::bottom(egui::Id::new("lightbox-filmstrip"))
                    .resizable(true)
                    .default_size(strip.height_pt())
                    .size_range(filmstrip::MIN_HEIGHT_PT..=filmstrip::MAX_HEIGHT_PT)
                    .show(root, |ui| {
                        filmstrip::filmstrip_ui(
                            ui,
                            strip,
                            total,
                            active,
                            &cell_of,
                            thumbs,
                            &mut visible,
                        )
                    });
                // B5/Phase-G seam: mirror the panel's drag-resized height
                // into the state Phase G will persist (§6.7).
                strip.set_height_pt(shown.response.rect.height());
                if let Some(FilmstripAction::Activate(idx)) = shown.inner {
                    self.working_set.set_active(idx);
                }
                self.outcome.filmstrip_shown.store(true, Ordering::Release);
            }
        }

        // A changed (or newly-absent) active `Ready` image resets the
        // canvas exactly once per transition (mirrors the retired
        // `enter_loupe`'s reset-on-entry behavior, plus a matching release
        // on the way out). Only `Ready` entries carry a stable `ImageId` to
        // track — the other projections have no image at all.
        let new_shown = match &projection {
            Some(ActiveProjection::Ready { image, .. }) => Some(*image),
            _ => None,
        };
        if new_shown != self.last_shown_image {
            match new_shown {
                Some(image) => self.canvas.enter(image),
                None => self.canvas.exit(),
            }
            self.last_shown_image = new_shown;
        }

        egui::CentralPanel::default().show(root, |ui| match &projection {
            Some(proj) => {
                let content = match proj {
                    ActiveProjection::Loading => CanvasContent::Loading,
                    ActiveProjection::SourceFailed(reason) => {
                        CanvasContent::SourceFailed { reason }
                    }
                    ActiveProjection::Duplicate(of) => CanvasContent::Duplicate { of: *of },
                    ActiveProjection::Ready {
                        image,
                        filename,
                        width,
                        height,
                    } => CanvasContent::Ready(ActiveEntry {
                        image: *image,
                        filename,
                        width: *width,
                        height: *height,
                    }),
                };
                let scheduler = self.session.render_scheduler();
                let max_tex_dim = self.gpu.limits.max_texture_dimension_2d;
                let idx = self.working_set.active_index().unwrap_or(0);
                let total = self.working_set.entries().len();
                let canvas = &mut self.canvas;
                let recipe_source = &mut self.recipe_source;
                let device_degraded = self.device_degraded.as_deref();
                canvas.ui(
                    ui,
                    &scheduler,
                    max_tex_dim,
                    content,
                    recipe_source,
                    idx,
                    total,
                    device_degraded,
                );
            }
            None if self.working_set.is_empty() => {
                empty_state_ui(ui, intake_frame.hover);
            }
            None => {
                // Set is non-empty but nothing is active: auto-activation
                // only ever picks a `Ready` entry (§2.4) and none exists
                // yet, and the user hasn't manually clicked a Failed/
                // Loading/Duplicate cell either — never blank, never a
                // false "drop here" (§6.4 canvas states).
                ui.centered_and_justified(|ui| {
                    ui.weak(match self.working_set.phase() {
                        lightbox_core::SetPhase::Planning | lightbox_core::SetPhase::Loading => {
                            "opening…"
                        }
                        _ => "no previewable images in this set",
                    });
                });
            }
        });

        // Cancel-on-scroll-out + O(visible) texture memory. A new epoch's
        // filmstrip only ever iterates its own entries, so a replace (A6)
        // naturally excludes the old epoch's images from `visible` — their
        // in-flight thumb tickets are cancelled here on the very next frame.
        let cap = (visible.len() * 3).max(64);
        self.thumbs.end_frame(&visible, cap);

        // D4: the cheat-sheet overlay (⌘/ toggles via `app.cheatsheet`),
        // rendered over everything with this frame's live context stack.
        self.cheatsheet.ui(&ctx, &self.registry, &stack);

        if self.show_overlay {
            self.overlay(&ctx);
        }

        self.outcome.frames.fetch_add(1, Ordering::AcqRel);
        self.pump_smoke(&ctx);

        // Repaint policy: run hot while anything is in flight; otherwise a
        // slow idle poll keeps the event pump alive without burning a core.
        let busy = matches!(
            self.working_set.phase(),
            lightbox_core::SetPhase::Planning | lightbox_core::SetPhase::Loading
        ) || self.thumbs.stats().inflight > 0
            || self.canvas.busy()
            || self.explorer.is_open()
            || self.smoke.is_some();
        if busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn on_exit(&mut self) {
        // Exit-time verified backup per policy (spec T15/OQ-6); smoke runs
        // skip it (throwaway catalog).
        let policy = if self.smoke.is_some() {
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
