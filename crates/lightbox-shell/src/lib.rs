// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-shell`, the thin, replaceable egui/eframe UI shell.
//!
//! **E08 Phase A rescope**: this crate is the **editor shell** of the v2.0
//! product (`docs/plan/epics/E08-editor-shell.md`), there is no library, no
//! grid view, no culling. The window *is* the editor: a top bar (open
//! affordances + view controls), a central canvas, a bottom session
//! filmstrip, a right develop-panel rail (placeholder until Phase E), and a
//! status bar. Files enter by drag-and-drop, the OS open dialog, the
//! transient live-filesystem folder explorer (mandate v2.2), or launch-time
//! argv paths, every entry point normalizes to exactly one
//! `Command::OpenWorkingSet` (E04's command, consumed via
//! `Session::working_set()`/`Event::WorkingSet*`). Opening a working set
//! auto-activates its first `Ready` entry into the canvas; every displayed
//! pixel is still produced by `Engine::submit` and composited **zero-copy**
//! on the ONE `wgpu::Device` the shell shares with the engine (architecture
//! §2.3 seam 2).
//!
//! **Zero-copy invariants (code-review checklist):**
//! * NO `map_async`/CPU readback anywhere in the frame path, this crate
//!   never maps GPU memory (grep it). Thumbnails are CPU-decoded pixels
//!   uploaded once as egui textures; the canvas is the engine's texture
//!   registered via `register_native_texture`. (The one readback in the
//!   binary at all is `egui_wgpu`'s own surface capture behind the
//!   developer-only `--screenshot` flag, see `screenshot.rs`. It is off
//!   unless that flag is passed, and it closes the app immediately after.)
//! * The engine receives the *shell's* device: asserted by a debug
//!   assertion, and enforced at runtime by wgpu itself.
//!
//! **Phase A scope** (`docs/plan/epics/E08-editor-shell.md` §8 Phase A):
//! chassis rescope, entry intake (drop/dialog/launch), the working-set view
//! model, replace-set semantics, the smoke driver, and the mandate-v2.2
//! folder-explorer UI. **Phase B** completed the session filmstrip (B1-B5:
//! geometry, virtualized chrome, §6.3 badges, nav/multi-select,
//! resize/collapse/overflow polish, see `filmstrip.rs`'s module docs,
//! including the Phase-G persistence seam for the strip height/collapse
//! prefs). **Phase C** generalizes the E01/F5 loupe into the `canvas`
//! module (`ViewXform`, the zoom ladder, recipe-driven submit, progressive
//! tier→engine display, and canvas states, see `canvas/mod.rs`'s module
//! docs). **Phase D** adds the remappable keymap (`keymap/`, spec §6.8):
//! every keyboard route now goes through one per-frame dispatcher that
//! runs **before** any widget is built (matched chords are consumed;
//! text-input focus suppresses non-modifier chords), with `keymap.toml`
//! overrides and the ⌘/ cheat-sheet overlay. The per-widget key handlers
//! Phases B/C carried (`filmstrip::nav_delta`, the canvas's arrow/Z/Space
//! block) are gone, `nav.*`/`view.*` actions replaced them, exactly the
//! collapse their TODOs promised. **Phase E** replaces the rail placeholder
//! with the real develop-panel framework (`panels/`, spec §6.5): the
//! `PanelHost` registry (E1) with §4 `SourceKind` filtering (E2), the
//! `SessionEditBinding` adapter over E09's edit session (E3, it also
//! serves the canvas's Phase-C `RecipeSource` seam, so slider previews
//! resubmit the engine in the same frame), the house `param_slider` (E4),
//! the basic WB+tone panel (E5/E6), the point tone-curve widget (E7), and
//! the history & snapshots panel (E8). E9 (info panel) is the phase's named
//! cut-line. **Phase F** lands the gizmo framework (`canvas/gizmo.rs`
//! spec §6.6): the `Gizmo` trait + `GizmoLayer` (input routing where a
//! gizmo hit wins over pan/zoom, drag capture, a zero-copy paint pass
//! above the composited image, keymap sub-context push/pop on
//! `CTX_GIZMO`), and the WB-eyedropper reference gizmo (crosshair + loupe
//! chip drawn from the already-registered texture; a click's
//! `Picked { image_px }` routes to `panels::basic::apply_wb_pick`. **E10**
//! (task A11) landed the real neutral-solve
//! (`panels::basic::apply_wb_pick_from_sample`,
//! `lightbox_color::wb::temp_tint_from_working_neutral`) and proved it end
//! to end through the real `WhiteBalanceNode`
//! (`lightbox-render/tests/e10_wb.rs`), but `apply_wb_pick` itself still
//! resolves a pick position (not a pixel), no pixel-sampling path reaches
//! this call site yet (see `apply_wb_pick`'s doc comment for the named
//! follow-on seam). Phase
//! E's `EyedropperMount` seam is subsumed by the layer. New-gizmo authors
//! (E11/E12): `canvas/gizmo.md` is the frozen author guide. **Phase G**
//! lands the two-scope prefs store (`lightbox_core::prefs`, spec §6.7
//! machine `prefs.toml` next to `keymap.toml`, catalog `catalog_settings`)
//! and the ⌘, preferences panel (`prefs_ui.rs`, GPU/caches/jobs/interface
//! sections with per-row live-vs-restart badges, plus the **un-cut D5
//! rebind editor** as its Keyboard section). Machine prefs are read before
//! `Core::start` (job sizing, event capacity) and session open (`gpu_mode
//! == Off` ⇒ the engine gets `gpu: None` and runs its CPU path
//! restart-required at M1 per Q6); catalog prefs shape the preview store
//! inside `Session::open` and apply live via `Command::SetCacheLimits`.

// The About window (app identity, build facts, the owner's site).
mod about;
mod canvas;
mod drill;
mod empty_state;
mod explorer;
// E15 core slice: the Export dialog (⌘⇧E toggles via `app.export`).
mod export_ui;
mod filmstrip;
mod intake;
mod keymap;
mod panels;
mod perf;
mod prefs_ui;
mod screenshot;
mod smoke;
mod status;
mod theme;
mod thumbs;
mod working_set;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use lightbox_core::prefs::{GpuMode, PrefsStore};
use lightbox_core::{
    ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, ItemState, OpenOrigin, OpenRequest,
    Session,
};
use lightbox_render::GpuContext;
use lightbox_types::ImageId;

use crate::about::{AboutInfo, AboutWindow};
use crate::canvas::{ActiveEntry, CanvasContent, EditorCanvas, GizmoEffect, GizmoLayer};
pub use crate::drill::DrillMode;
use crate::drill::{DrillDriver, DrillState, DRAG_FRAMES};
use crate::empty_state::empty_state_ui;
use crate::explorer::{ExplorerAction, FolderExplorer};
use crate::export_ui::{ExportDialog, ExportPanelCtx};
use crate::filmstrip::{EditedBadges, FilmstripAction, FilmstripState};
use crate::keymap::cheatsheet::CheatSheet;
use crate::keymap::{ActionId, ContextId, KeymapRegistry};
use crate::panels::develop_ctx::EditBinding as _;
use crate::panels::DockSide;
use crate::panels::{DevelopCtx, PanelHost, SessionEditBinding};
use crate::perf::{StripDriver, StripPhase};
use crate::prefs_ui::{PrefsPanelCtx, PrefsWindow};
pub use crate::screenshot::{
    parse_size as parse_screenshot_size, ScreenshotOptions,
    DEFAULT_SETTLE_FRAMES as DEFAULT_SCREENSHOT_FRAMES,
};
use crate::screenshot::{ScreenshotDriver, Step as ScreenshotStep};
use crate::smoke::SmokeDriver;
use crate::status::{push_notice, ActivityModel, Notice, StatusAction, StatusContext};
use crate::theme::{fonts, paint, tokens as tk};
use crate::thumbs::ThumbCache;
use crate::working_set::WorkingSetView;

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    /// Smoke mode (CI): drive open→filmstrip→canvas on a throwaway catalog,
    /// close after the seam is proven and this many frames painted.
    pub smoke_frames: Option<u64>,
    /// H2 perf mode: sawtooth-scroll a ~300-entry synthetic filmstrip for
    /// this many measured frames (+ a nav phase), print one JSON summary
    /// line, close. Throwaway catalog, like smoke.
    pub perf_strip_frames: Option<u64>,
    /// H4 exit-drill legs (edit-commit / verify), need `catalog`.
    pub drill: Option<DrillMode>,
    /// Developer screenshot capture (`--screenshot`): run the REAL app
    /// (including `initial_paths`), let it settle, write the viewport to a
    /// PNG, close. Unlike the three scripted modes above this does NOT
    /// replace the entry pipeline, it only borrows their throwaway
    /// catalog/prefs posture so a capture never touches the developer's own
    /// state and looks the same on every machine. See `screenshot.rs`.
    pub screenshot: Option<ScreenshotOptions>,
    /// The catalog to open (created when missing). Defaults to E04's
    /// per-user app-data store ([`lightbox_core::default_store_dir`])
    /// never a path relative to the working directory, which is `/` when
    /// the app is launched from the Finder. Ignored in
    /// smoke/perf/screenshot modes; REQUIRED for drill modes (the drill's
    /// whole point is a catalog that persists across `kill -9`).
    pub catalog: Option<PathBuf>,
    /// Files/folders to open at launch (A4: argv paths / the future
    /// file-association handler), intake runs before the first frame.
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
    /// H2: a `--perf-strip` capture completed and printed its JSON line.
    pub perf_ok: AtomicBool,
    /// H4: a `--drill-verify` leg passed (image + restored recipe match).
    pub drill_ok: AtomicBool,
    /// `--screenshot` wrote its PNG (the capture handshake completed and the
    /// file landed on disk).
    pub screenshot_ok: AtomicBool,
}

/// Boots the eframe shell and blocks until the window closes.
///
/// Returns the observed [`ShellOutcome`]; in smoke mode the caller turns
/// `seam_proven == false` into a nonzero exit code.
pub fn run(options: ShellOptions) -> Result<Arc<ShellOutcome>, eframe::Error> {
    let outcome = Arc::new(ShellOutcome::default());
    let app_outcome = Arc::clone(&outcome);

    // `--screenshot-size` is the one caller allowed to move the default
    // window off 1100x720 (a capture may want a wider frame to show the rail
    // and filmstrip at review resolution); everything else launches at the
    // app's own default.
    let inner_size = options
        .screenshot
        .as_ref()
        .and_then(|shot| shot.size)
        .unwrap_or([1100.0, 720.0]);

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Lightbox")
            .with_inner_size(inner_size),
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

/// Rolling frame-time probe backing the debug overlay. H2 splits samples
/// by whether the develop rail was open that frame, the §7 budget is
/// "frame p95 < 16 ms **with panels open** and the filmstrip visible".
struct FrameStats {
    last: Option<Instant>,
    samples_ms: Vec<f32>,
    /// The subset of `samples_ms` painted with the develop rail open.
    rail_open_ms: Vec<f32>,
}

impl FrameStats {
    fn new() -> FrameStats {
        FrameStats {
            last: None,
            samples_ms: Vec::with_capacity(240),
            rail_open_ms: Vec::with_capacity(240),
        }
    }

    fn tick(&mut self, rail_open: bool) {
        let now = Instant::now();
        if let Some(last) = self.last.replace(now) {
            let ms = now.duration_since(last).as_secs_f32() * 1000.0;
            if self.samples_ms.len() >= 240 {
                self.samples_ms.remove(0);
            }
            self.samples_ms.push(ms);
            if rail_open {
                if self.rail_open_ms.len() >= 240 {
                    self.rail_open_ms.remove(0);
                }
                self.rail_open_ms.push(ms);
            }
        }
    }
}

/// Every `.xmp` file under the bundled starter preset library
/// (`assets/presets/`, spec: preset-management feature), recursively.
///
/// **Dev-workspace path only**, resolved relative to THIS crate's own
/// `CARGO_MANIFEST_DIR` at compile time (the same convention
/// `lightbox-color`'s `cms.rs` uses to locate `Cargo.lock`), since no E15
/// packaging/distribution step exists yet to relocate bundled assets next to
/// an installed binary. A future packaged build replaces this one function;
/// it returns an empty `Vec` (never panics) when the dev-tree path doesn't
/// exist, so a build that DOES ship elsewhere degrades to "no starter
/// library" rather than failing to start.
fn bundled_preset_files() -> Vec<PathBuf> {
    // Packaged first, dev tree second.
    //
    // `cargo xtask bundle-mac` copies `assets/presets/` into the app's
    // `Contents/Resources/presets/`, so a downloaded Lightbox finds its
    // starter library beside itself. Before that existed, this function knew
    // only the compile-time source path, which meant the shipped app silently
    // started with an empty preset library on every machine that was not the
    // one that built it, while the website advertised thirty-six presets.
    // `tools/site-checks/check_site.py` now fails the build if the bundle
    // stops carrying them.
    let mut out = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        // Contents/MacOS/Lightbox -> Contents/Resources/presets
        if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
            collect_xmp_files(&contents.join("Resources/presets"), &mut out);
        }
        // A plain unbundled binary with a presets/ directory beside it.
        if out.is_empty() {
            if let Some(dir) = exe.parent() {
                collect_xmp_files(&dir.join("presets"), &mut out);
            }
        }
    }

    if out.is_empty() {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/presets"));
        collect_xmp_files(&dir, &mut out);
    }
    out
}

fn collect_xmp_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_xmp_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
        {
            out.push(path);
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

// ─── Frame chrome (theme spec §1/§2/§11) ────────────────────────────────
//
// Today there is exactly one custom `Frame` in the whole app (the canvas
// fill below); every other panel/window uses egui's stock default, which
// reads flat. This section gives the outer window architecture, top bar,
// develop rail, filmstrip, status bar, and every floating dialog, an
// explicit, elevation-correct `Frame` plus the two-stroke "engraved seam"
// at panel boundaries. `panels/host.rs`, `filmstrip.rs`, `status.rs`, and
// the canvas modules are being re-skinned in parallel by other agents
// this crate-root section only owns the composition in `ui()` below, but
// the floating-dialog helpers are `pub(crate)` so `explorer.rs`,
// `prefs_ui.rs`, `export_ui.rs`, and `keymap/cheatsheet.rs` share ONE
// definition instead of five slightly-different copies (all in this
// phase's file scope, no cross-scope edit needed to share them).

/// A docked, flat-lit panel frame (spec §1: elevation levels 0-2 are
/// "docked, flat-lit, no shadow, part of the window's fixed
/// architecture", unlike the floating popups §11 gives a real
/// `epaint::Shadow` to). `margin` is the panel's own inner content inset
/// spec §2's "no panel sets any margin at all" is exactly what this fixes.
fn docked_frame(fill: egui::Color32, margin: egui::Margin) -> egui::Frame {
    egui::Frame::new().fill(fill).inner_margin(margin)
}

/// Panels in this crate frequently auto-size to their own content within
/// the same frame `Panel::show` is called, so measuring a boundary seam
/// from INSIDE the content closure risks painting against last frame's
/// rect. Painting into the panel's own layer right after `.show()`
/// returns (`response.layer_id`) draws over the frame's fill without
/// racing the content layout.
fn panel_painter<R>(
    ctx: &egui::Context,
    resp: &egui::InnerResponse<R>,
) -> (egui::Painter, egui::Rect) {
    (
        ctx.layer_painter(resp.response.layer_id),
        resp.response.rect,
    )
}

/// A real vertical divider for the top bar (spec §2's engraved seam)
/// instead of `ui.separator()`'s single hairline, padded by `SPACE_2` so
/// it reads as a deliberate group divider rather than a stray line.
fn top_bar_separator(ui: &mut egui::Ui) {
    ui.add_space(tk::SPACE_2);
    let height = ui.available_height();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        paint::engraved_seam_v(
            ui.painter(),
            egui::Rangef::new(rect.top(), rect.bottom()),
            rect.center().x,
            true,
        );
    }
    ui.add_space(tk::SPACE_2);
}

/// Shared window chrome for every floating dialog in this crate (spec
/// §11 "popups/menus/tooltips"): `elev` is `tk::ELEV_3_POPUP` for the
/// lighter dialogs (Browse Folders, Keyboard Shortcuts, the F1 frame-stats
/// overlay) or `tk::ELEV_4_OVERLAY` for modal-weight ones (Preferences,
/// Export) per the spec's own steer ("modal-weight dialogs MAY use the
/// overlay elevation"); `shadow` is `paint::shadow_popup()` /
/// `paint::shadow_overlay()` to match. `pub(crate)` (not `pub`): this
/// crate's own dialogs only.
pub(crate) fn floating_frame(elev: egui::Color32, shadow: egui::Shadow) -> egui::Frame {
    egui::Frame::new()
        .fill(elev)
        .corner_radius(egui::CornerRadius::same(tk::RADIUS_POPUP as u8))
        .stroke(egui::Stroke::new(
            tk::STROKE_HAIRLINE,
            tk::SEPARATOR_HAIRLINE,
        ))
        .shadow(shadow)
        .inner_margin(egui::Margin::same(tk::SPACE_3 as i8))
}

/// The "catching light from above" cue (spec §11: `separator_hairline`
/// all around + `bevel_outset_top` on the top edge only), a `Frame` can
/// only express one stroke color for its whole border, so the top-edge
/// highlight is a second pass, painted after `Window::show()` returns
/// into the window's own layer (mirrors [`panel_painter`]'s reasoning for
/// the docked panels above). No-op when the window didn't render this
/// frame (`None`, e.g. `Window::open` was handed `&mut false`).
pub(crate) fn finish_floating_window<R>(
    ctx: &egui::Context,
    resp: &Option<egui::InnerResponse<R>>,
) {
    let Some(resp) = resp else { return };
    let painter = ctx.layer_painter(resp.response.layer_id);
    let rect = resp.response.rect;
    let inset = tk::RADIUS_POPUP;
    paint::hairline_h(
        &painter,
        egui::Rangef::new(rect.left() + inset, rect.right() - inset),
        rect.top() + tk::STROKE_HAIRLINE + 1.0,
        tk::BEVEL_OUTSET_TOP,
    );
}

struct LightboxApp {
    session: Session,
    _core: Core,
    events: tokio::sync::broadcast::Receiver<Event>,
    gpu: GpuContext,

    working_set: WorkingSetView,
    thumbs: ThumbCache,
    canvas: EditorCanvas,
    /// E3: the live `EditBinding` adapter over E09's edit session, the
    /// panels' gesture seam AND the canvas's C3 `RecipeSource` (the bound
    /// image renders the in-memory working recipe, gesture previews
    /// included; other images fall back to the Phase-C persisted read it
    /// wraps). Re-bound to the active image once per frame; its read
    /// caches refresh event-driven (`EditCommitted`/`CatalogChanged`).
    binding: SessionEditBinding,
    /// E1: the develop-panel registry + right-rail renderer.
    panel_host: PanelHost,
    /// F1: the on-canvas gizmo layer (spec §6.6), app-owned because its
    /// lifecycle spans the frame: `keymap_stack` pushes `CTX_GIZMO` (+ the
    /// innermost gizmo's own sub-context) while a gizmo is active,
    /// `handle_action` routes `gizmo.cancel`/`gizmo.commit` into it, the
    /// panel rail's tool buttons activate/cancel gizmos through
    /// `DevelopCtx.gizmos`, the canvas routes/paints it inside
    /// `EditorCanvas::ui`, and the end-of-frame drain routes its
    /// `GizmoEffect`s (picks / edit gestures) onto the E3 binding.
    /// Subsumes Phase E's `EyedropperMount` (see `E08-deviations.md` F).
    gizmos: GizmoLayer,
    /// `Some(reason)` once `Event::DeviceDegraded` has fired this session
    /// (spec §7/C5: M0 posture, no device-recovery event exists yet, so
    /// this is sticky rather than cleared; the full rebuild harness is
    /// E05.5). Drives the canvas's non-modal corner chip.
    device_degraded: Option<String>,
    /// B4/B5 strip UI state (selection, height, collapse), in-session;
    /// Phase G persists height/collapsed (see `filmstrip.rs` module docs).
    filmstrip: FilmstripState,
    /// B3 edited-dot cache over `Queries::edit_badges` (event-driven).
    badges: EditedBadges,
    /// D1/D2: the action registry (M1 defaults + `keymap.toml` overrides),
    /// resolved by the per-frame dispatcher at the top of `ui()`.
    registry: KeymapRegistry,
    /// D4: the ⌘/ cheat-sheet overlay.
    cheatsheet: CheatSheet,
    /// G1/G2: the two-scope prefs store (spec §6.7), machine `prefs.toml`
    /// loaded before `Core::start` (job/GPU knobs), catalog scope bound to
    /// this session's `catalog_settings` via `Session::bind_prefs`.
    prefs: PrefsStore,
    /// G3-G6: the ⌘, preferences panel (+ the un-cut D5 rebind editor as
    /// its Keyboard section).
    prefs_window: PrefsWindow,
    /// The About window (`app.about`, and the top bar's button).
    about_window: AboutWindow,
    /// The open edit store (`…/edits.lbdata`), an About readout.
    store_dir: PathBuf,
    /// E15 core slice: the Export dialog (⌘⇧E toggles via `app.export`).
    export_dialog: ExportDialog,
    /// Where keyboard rebinds load/save (`keymap.toml`; a throwaway path
    /// in smoke mode), also the G6 location readout.
    keymap_path: PathBuf,
    /// D2 `panel.toggle_rail`: whether the right develop rail is shown
    /// (in-session; panel/rail layout persistence is still open, see
    /// `E08-deviations.md` Phase G).
    rail_visible: bool,
    explorer: FolderExplorer,
    /// The active image the canvas last rendered, compared each frame so a
    /// change (click/nav/auto-activation) resets zoom/pan exactly once
    /// (mirrors the retired `enter_loupe`'s reset-on-entry behavior).
    last_shown_image: Option<ImageId>,
    status: String,
    /// H3: the newest Foreground/Background activity (spinner + label +
    /// cancel on the status bar), folded from core events.
    activity: ActivityModel,
    /// H3: dismissible status-bar notice chips (prefs/keymap load reports,
    /// degraded GPU).
    notices: Vec<Notice>,

    stats: FrameStats,
    show_overlay: bool,

    outcome: Arc<ShellOutcome>,
    smoke: Option<SmokeDriver>,
    /// H2: the `--perf-strip` scripted capture, when active.
    perf: Option<StripDriver>,
    /// H4: the exit-drill editor leg, when active.
    drill: Option<DrillDriver>,
    /// `--screenshot`: the developer capture handshake, when active.
    screenshot: Option<ScreenshotDriver>,
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

        // Seam 2: wrap eframe's device, the engine NEVER creates its own
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
        let perf = match options.perf_strip_frames {
            Some(frames) if smoke.is_none() => Some(StripDriver::new(frames)?),
            _ => None,
        };
        let drill = match &options.drill {
            Some(mode) if smoke.is_none() && perf.is_none() => Some(DrillDriver::new(mode.clone())),
            _ => None,
        };
        // main.rs makes `--screenshot` mutually exclusive with all three
        // scripted modes, so no `is_none()` guard is needed here.
        let screenshot = match &options.screenshot {
            Some(opts) => Some(ScreenshotDriver::new(opts)?),
            None => None,
        };
        // The three SCRIPTED modes replace the entry pipeline outright (they
        // stage and open their own sets); screenshot mode does not, it runs
        // the real argv/preset entry path below.
        let scripted = smoke.is_some() || perf.is_some() || drill.is_some();
        // Scripted modes (smoke/perf/drill) redirect the config surface
        // away from the user's real files: smoke/perf to their throwaway
        // temp dirs, the drill to its xtask-owned state dir (which must
        // persist across `kill -9`, that's the drill). Screenshot mode
        // takes the same redirection for a different reason: a capture must
        // be reproducible, so it reads DEFAULT prefs/keymap rather than
        // whatever panel heights and theme the developer happens to have
        // persisted, and it must never write to their catalog.
        let script_dir: Option<PathBuf> = smoke
            .as_ref()
            .map(|s| s.lbdata.clone())
            .or_else(|| perf.as_ref().map(|p| p.lbdata.clone()))
            .or_else(|| {
                drill.as_ref().and_then(|_| {
                    // main.rs enforces --catalog for drill modes.
                    options.catalog.clone()
                })
            })
            .or_else(|| screenshot.as_ref().map(|s| s.lbdata.clone()));

        // G1: machine prefs load BEFORE `Core::start`, job sizing, event
        // capacity, and the GPU mode are all construction-time consumers
        // (spec §6.7 table). Scripted runs use a throwaway prefs dir
        // instead of the user's real file (deterministic CI, same posture
        // as the throwaway catalog + skipped keymap overrides).
        let mut prefs = PrefsStore::open_machine(
            &script_dir
                .clone()
                .unwrap_or_else(lightbox_core::prefs::default_prefs_dir),
        );
        let machine = prefs.machine();
        // Install Lightbox's neutral-dark editor palette for both themes, then
        // let the user's preference select between them (G6 live-theme path
        // still works, it only flips the preference, our visuals stay).
        crate::theme::install(&cc.egui_ctx);
        // The accent is a user setting (Preferences → Appearance); apply
        // it before the first frame so nothing paints in the default hue
        // and then swaps.
        crate::theme::set_accent(&cc.egui_ctx, crate::prefs_ui::accent_color(&machine));
        // G6: theme is live, but the FIRST frame needs it applied too
        // set it before any UI is built rather than waiting for the panel's
        // own on-change path.
        cc.egui_ctx
            .set_theme(crate::prefs_ui::egui_theme_preference(machine.theme));

        // G5: `JobKnobs` → `CoreConfig.jobs` at start (restart-required
        // live re-sizing is E06's later seam).
        // Phase-G/E06 seam: if E06 lands a richer merged `JobConfig`,
        // `JobKnobs::apply_to` is the one place to reconcile.
        let mut core_cfg = CoreConfig::default();
        machine.jobs.apply_to(&mut core_cfg.jobs);
        if let Some(cap) = machine.jobs.event_capacity {
            core_cfg.event_capacity = cap.max(64);
        }
        let core = Core::start(core_cfg)?;

        // The edit store. Default: E04's per-user app-data location
        // `~/Library/Application Support/Lightbox/edits.lbdata` on macOS,
        // whose *parent* already holds `prefs.toml` and `keymap.toml`.
        //
        // **Not a relative path.** It used to be `./lightbox.lbdata`, which
        // works from a terminal in the repo and fails for every real user:
        // Finder and the Dock launch an app with the working directory set
        // to `/`, so catalog creation hit the read-only root and the app
        // exited before it could open a window, "I double-click it and
        // nothing happens". Terminal runs never reproduced it, which is
        // exactly why it survived the bundle work's verification.
        let lbdata = match &script_dir {
            Some(dir) => dir.clone(),
            None => options
                .catalog
                .clone()
                .unwrap_or_else(lightbox_core::default_store_dir),
        };
        // G3: `gpu_mode == Off` ⇒ the ENGINE gets no GPU and runs its CPU
        // path (spec §6.7: "core passes None GPU for Off"); the UI itself
        // still renders through eframe's own wgpu surface. Restart-required
        // at M1 (Q6), the panel badges it accordingly.
        let engine_gpu = match machine.gpu_mode {
            GpuMode::Auto => Some(gpu.clone()),
            GpuMode::Off => None,
        };
        let session = if lbdata.join("catalog.sqlite").is_file() {
            core.open_catalog(&lbdata, engine_gpu.clone())?
        } else {
            core.create_catalog(&lbdata, engine_gpu.clone())?
        };
        let events = session.events();

        // G2: bind the catalog scope (loads `catalog_settings`, arms
        // write-through for the panel's cache knobs).
        session.bind_prefs(&mut prefs);

        // T7 seam-2 invariant: the engine renders on the shell's device.
        // **F5 deviation:** `ng::Engine` does not expose the raw device handle
        // for an `Arc::ptr_eq` proof (the old E01 seed did via `Engine::gpu()`);
        // the guarantee is now structural instead of runtime-asserted
        // `Session::open` builds `SharedDeviceProvider` from exactly the
        // `gpu.clone()` passed in here, and `Engine::with_compiler`'s `Auto`
        // path builds its `DeviceCtx` only from `DeviceProvider::current()` (no
        // other device-creation path exists on the `Auto` branch), so the
        // engine's GPU backend is provably built on this device by
        // construction. See `docs/plan/epics/E05-deviations.md`.
        // G3: the assertion only holds when a device was actually handed
        // over, `gpu_mode: Off` legitimately runs the engine CPU-only.
        debug_assert!(
            engine_gpu.is_none()
                || matches!(
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
        // before the first frame is painted. Scripted modes stage/open
        // their own sets instead (see `pump_smoke`/`pump_perf`/`pump_drill`)
        // screenshot mode is NOT one of them, which is the whole point:
        // `lightbox <file> --screenshot out.png` must capture the loaded
        // editor, not an empty one.
        if !scripted && !options.initial_paths.is_empty() {
            session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(
                    options.initial_paths.clone(),
                    options.initial_recursive,
                    OpenOrigin::Cli,
                ),
            });
        }

        // Preset-management feature: seed the user's preset store from the
        // bundled starter library (`assets/presets/`, `lightbox_edit::
        // preset_library`) on every real launch, never during scripted
        // smoke/perf/drill runs (same `scripted` guard as the argv-open
        // above, so the smoke self-test's tight budget is untouched, while a
        // `--screenshot` capture still shows a populated preset browser).
        // Idempotent: importing a Lightbox-authored file re-homes onto its
        // OWN `lb:PresetId`, so a repeat import on the next launch just
        // rewrites the same canonical file (`PresetStore::import_files`'s
        // doc comment), never a duplicate, never clobbers a renamed/
        // deleted library preset's slot with a "resurrected" copy beyond
        // that one path. Dev-workspace-relative (no E15 packaging yet, see
        // `bundled_preset_files`'s doc comment).
        //
        // `SeedBundledPresets` (NOT `ImportPresetFiles`) because the store is
        // machine-global and seeding must also RETIRE: a starter preset that
        // has left the bundled library is removed here, while anything the
        // user authored, renamed, or edited is untouched
        // (`lightbox_edit::preset_retire`). The command asserts these paths
        // are the whole library, which only holds on this launch-time path.
        if !scripted {
            let library = bundled_preset_files();
            if !library.is_empty() {
                session.submit(Command::Edit(EditCommand::SeedBundledPresets {
                    paths: library,
                }));
            }
        }

        let thumbs = ThumbCache::new(session.previews());
        let canvas = EditorCanvas::new(
            render_state,
            Arc::clone(&outcome),
            &session.render_scheduler(),
            session.previews(),
        );
        let binding = SessionEditBinding::new(session.clone());
        let working_set = WorkingSetView::new(&session);

        // E1: the M1 panel slice. E10/E11/E12 register theirs here later
        // (order slots between these; E10's histogram slot is < 20).
        // E9 (info panel) is the phase's named cut-line, not registered.
        let mut panel_host = PanelHost::new();
        panel_host.register(panels::histogram::def()); // D13: order 10, above Basic
        panel_host.register(panels::basic::def());
        panel_host.register(panels::geometry::def());
        panel_host.register(panels::curve::def());
        panel_host.register(panels::hsl::def());
        panel_host.register(panels::bw::def());
        panel_host.register(panels::grading::def());
        panel_host.register(panels::looks::def());
        panel_host.register(panels::history::def());
        panel_host.register(panels::presets::def());
        // The user's docked arrangement (which rail/column each panel sits
        // in, column widths, collapsed sections, panel-solo, panels
        // switched off), machine scope, restored AFTER every panel has
        // registered so unknown ids can be dropped and newly registered
        // panels appended. Scripted runs read the throwaway prefs dir, so
        // they always get the default one-right-column layout.
        panels::restore_layout(&mut panel_host, &prefs);

        // D1/D3: the M1 action set, plus the user's keymap.toml rebind
        // delta. Scripted runs skip the user's file (deterministic CI,
        // same posture as the throwaway catalog) but still need a
        // `keymap_path` value for the struct (G6's location readout), a
        // throwaway path beside the scripted catalog, mirroring `prefs`'s
        // own substitution above. A corrupt/partial file is NEVER fatal
        // (§7), defaults + an H3 notice chip.
        let keymap_path = match &script_dir {
            Some(dir) => dir.join("keymap.toml"),
            None => keymap::overrides::default_keymap_path(),
        };
        let mut registry = keymap::default_registry();
        // H3: load reports surface as dismissible notice chips (spec §7
        // "corrupt prefs/keymap file → defaults + notice, never fatal").
        let mut notices: Vec<Notice> = Vec::new();
        if let Some(notice) = prefs.load_notice() {
            tracing::warn!(target: "lightbox_shell", "{notice}");
            push_notice(&mut notices, notice, false);
        }
        if script_dir.is_none() {
            match registry.load_overrides(&keymap_path) {
                Ok(report) => {
                    if let Some(notice) = report.notice() {
                        tracing::warn!(target: "lightbox_shell", "{notice}");
                        push_notice(&mut notices, notice, false);
                    }
                }
                Err(err) => {
                    tracing::warn!(target: "lightbox_shell", %err, "keymap overrides unreadable");
                    push_notice(&mut notices, format!("keymap.toml unreadable: {err}"), true);
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
            binding,
            panel_host,
            gizmos: GizmoLayer::new(),
            device_degraded: None,
            filmstrip: {
                // G6: seed height/collapsed from the persisted machine
                // prefs (the Phase-B/D5-4 seam `filmstrip.rs`'s module docs
                // name), `set_height_pt` clamps to the 48-160 pt band.
                let mut fs = FilmstripState::new();
                fs.set_height_pt(machine.filmstrip_height_pt);
                fs.set_collapsed(machine.filmstrip_collapsed);
                fs
            },
            badges: EditedBadges::new(),
            registry,
            cheatsheet: CheatSheet::new(),
            prefs,
            prefs_window: PrefsWindow::new(),
            about_window: AboutWindow::new(),
            store_dir: lbdata.clone(),
            export_dialog: ExportDialog::new(),
            keymap_path,
            rail_visible: true,
            explorer: FolderExplorer::closed(),
            last_shown_image: None,
            status: String::new(),
            activity: ActivityModel::new(),
            notices,
            stats: FrameStats::new(),
            // Perf/frame-stats overlay is OFF by default (opt-in via F1); it used to
            // default on in debug builds, which is exactly what users run and found noisy.
            show_overlay: false,
            outcome,
            smoke,
            perf,
            drill,
            screenshot,
            session,
        })
    }

    /// Drains the event broadcast once per frame (spec §5.1 threading
    /// contract). Slow frames lag, they never wedge the writer.
    fn drain_events(&mut self) {
        use tokio::sync::broadcast::error::TryRecvError;
        loop {
            let ev = match self.events.try_recv() {
                Ok(ev) => ev,
                Err(TryRecvError::Lagged(_)) => {
                    self.working_set.on_lagged(&self.session);
                    self.badges.mark_dirty();
                    continue;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            };
            // H3: fold Foreground/Background activity off the same bus
            // (working-set opens, bulk preview builds, the E06/E15 seam).
            self.activity.on_event(&ev);
            match ev {
                ev @ (Event::WorkingSetOpening { .. }
                | Event::WorkingSetReplaced { .. }
                | Event::WorkingSetChanged { .. }
                | Event::WorkingSetLoadFinished { .. }) => {
                    if matches!(ev, Event::WorkingSetOpening { .. }) {
                        // A6: a replace never prompts, but a previously
                        // -failed thumbnail is worth retrying, the new
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
                Event::EditCommitted { image, seq, .. } => {
                    // B3: a durable edit may flip an `is_edited` badge.
                    self.badges.mark_dirty();
                    // E3: a durable step landed, the binding's working
                    // recipe / history / snapshot caches may have moved
                    // (undo/redo/StepTo/snapshot-restore rewrite the
                    // working recipe); non-bound images keep the C3
                    // persisted-read discipline via the wrapped fallback.
                    self.binding.on_edit_committed(image);
                    // H4: the drill's edit leg waits for exactly this.
                    if let Some(drill) = &mut self.drill {
                        drill.on_edit_committed(image, seq, self.working_set.active_image());
                    }
                }
                Event::CatalogChanged { .. } => {
                    // E3/E8: durable edit commands that emit no
                    // `EditCommitted` (ClearHistory, snapshot create/
                    // rename) still emit this, refresh the binding's
                    // read caches on the next bind (dirty-flag, spec §7).
                    self.binding.mark_dirty();
                }
                Event::CommandFailed { ticket, error } => {
                    self.status = format!("command failed: {error}");
                    // G4: the §5.2 invalid-relocation-target fallback badge
                    // (a no-op when `ticket` isn't a cache-panel op).
                    self.prefs_window.on_command_failed(ticket, &error);
                    // E15 core slice: a no-op when `ticket` isn't ours.
                    self.export_dialog.on_command_failed(ticket, &error);
                }
                // E15 core slice: `Command::Export` progress → the dialog's
                // own state (spinner/counts) + a status-bar line per item.
                Event::ExportStarted { ticket, total } => {
                    self.export_dialog.on_export_started(ticket, total);
                }
                Event::ExportProgress {
                    ticket,
                    image,
                    error,
                    done,
                    total,
                    ..
                } => {
                    let ok = error.is_none();
                    self.export_dialog
                        .on_export_progress(ticket, done, total, ok);
                    self.status = match error {
                        None => format!("exported {done}/{total} (image {})", image.0),
                        Some(e) => format!("export failed {done}/{total} (image {}): {e}", image.0),
                    };
                }
                Event::ExportFinished { ticket, report } => {
                    self.export_dialog.on_export_finished(ticket);
                    self.status = format!(
                        "export finished: {} ok, {} failed",
                        report.ok,
                        report.failed.len()
                    );
                }
                Event::BackupFinished { report } => {
                    self.status = format!("backup written: {}", report.path.display());
                }
                Event::DeviceDegraded { reason } => {
                    // C5: the canvas's non-modal chip (sticky, see the
                    // `device_degraded` field doc comment) + the H3
                    // status-bar notice chip (dismissible, deduped).
                    push_notice(
                        &mut self.notices,
                        format!("GPU device degraded: {reason}"),
                        false,
                    );
                    self.device_degraded = Some(reason);
                }
                // G4: cache-panel command completions.
                Event::CacheRelocated { new_root, .. } => {
                    self.status = format!("cache store relocated to {}", new_root.display());
                    self.prefs
                        .set_catalog(|c| c.cache_dir_override = Some(new_root));
                    self.prefs_window.on_cache_relocated();
                }
                Event::CachePurged { report, .. } => {
                    self.status = format!(
                        "purged {} preview row(s), {} raw-cache row(s)",
                        report.rows_deleted, report.rawcache_rows_deleted
                    );
                    self.prefs_window.on_cache_purged();
                }
                Event::PreviewEvicted { .. } => {
                    // G4: usage bars are stale after an eviction.
                    self.prefs_window.mark_stats_dirty();
                }
                _ => {}
            }
        }
    }

    /// Submits exactly one `Command::OpenWorkingSet` (A2/A6: a new drop or
    /// dialog pick always *replaces* the working set, no prompt, ever).
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

    /// E15 core slice: the Export dialog's selection, the filmstrip's
    /// multi-select mapped to `ImageId` (skipping not-yet-`Ready` entries,
    /// same convention `Command::Export`'s own resolution uses for a
    /// missing/unregistered item), or the single active image when nothing
    /// is multi-selected. `filmstrip.selected()` is already documented as
    /// the batch-ops seam (spec M2 sync/export note in `working_set.rs`).
    fn export_selection(&self) -> Vec<ImageId> {
        let selected = self.filmstrip.selected();
        if selected.is_empty() {
            return self.working_set.active_image().into_iter().collect();
        }
        let entries = self.working_set.entries();
        selected
            .iter()
            .filter_map(|&i| entries.get(i))
            .filter_map(|item| match item.state {
                ItemState::Ready { image, .. } => Some(image),
                _ => None,
            })
            .collect()
    }

    /// D2: the frame's context stack (spec §6.8), `app` → `editor`, plus
    /// `editor.loupe` while the canvas shows an active entry, plus, F1
    /// `editor.gizmo` AND the innermost active gizmo's own sub-context
    /// (`editor.gizmo.<id>`) while the gizmo layer is non-empty. The
    /// generic push makes Esc/Enter resolve to `gizmo.cancel`/
    /// `gizmo.commit` (registered against `CTX_GIZMO` since Phase D); the
    /// sub-context lets a future gizmo register shadowing actions of its
    /// own (innermost wins, `keymap/registry.rs`). `editor.panels` stays
    /// unpushed (no action targets it at M1).
    fn keymap_stack(&self) -> Vec<ContextId> {
        let mut stack = vec![keymap::CTX_APP, keymap::CTX_EDITOR];
        if self.working_set.active().is_some() {
            stack.push(keymap::CTX_LOUPE);
        }
        if let Some(sub) = self.gizmos.active_context() {
            stack.push(keymap::CTX_GIZMO);
            if sub != keymap::CTX_GIZMO {
                stack.push(sub);
            }
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
            // G3: the real target, replaces the Phase-D stub status line.
            keymap::APP_PREFS => self.prefs_window.toggle(),
            keymap::APP_ABOUT => self.about_window.toggle(),
            keymap::APP_EXPORT => self.export_dialog.toggle(),
            keymap::APP_CHEATSHEET => self.cheatsheet.toggle(),
            keymap::FILM_TOGGLE => {
                let collapsed = self.filmstrip.collapsed();
                self.filmstrip.set_collapsed(!collapsed);
                // G6: mirror into the persisted machine pref (the panel's
                // own checkbox writes the same field, one seam either way).
                self.prefs
                    .set_machine(|m| m.filmstrip_collapsed = !collapsed);
            }
            keymap::EDIT_UNDO | keymap::EDIT_REDO => {
                // E8: routed through the E3 `EditBinding` adapter, ONE
                // seam for panels and keymap alike. The binding submits
                // the same `Command::Edit(Undo/Redo)` Phase D did, and
                // no-ops when nothing is bound (only a Ready entry has an
                // image to address, §6.4/§6.5, unchanged behavior).
                self.binding.bind(self.working_set.active_image());
                if action == keymap::EDIT_UNDO {
                    self.binding.undo();
                } else {
                    self.binding.redo();
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
            // F1: while a gizmo is active, `editor.gizmo` is on the stack
            // and Esc/Enter resolve here, the layer delivers Cancel/
            // Commit to the innermost gizmo, which pops itself by emitting
            // `Cancelled`/`Done` (drained + routed later this same frame;
            // the context pops when the next frame rebuilds the stack).
            keymap::GIZMO_CANCEL => self.gizmos.cancel_active(),
            keymap::GIZMO_COMMIT => self.gizmos.commit_active(),
            // E11 geometry TOOL UI: `R` toggles the crop gizmo, the same
            // one-authority pattern the Basic panel's eyedropper button
            // uses (`panels::geometry::crop_tool_row` is the click-driven
            // twin of this keyboard path; both read/write nothing but
            // `self.gizmos`/`self.binding`).
            keymap::GEOM_CROP_TOGGLE => {
                if self.gizmos.is_active(canvas::crop_gizmo::GEOM_CROP) {
                    self.gizmos.cancel(canvas::crop_gizmo::GEOM_CROP);
                } else {
                    let crop = match self.binding.value(lightbox_edit::ParamId::Crop) {
                        lightbox_edit::ParamValue::Crop(c) => c,
                        _ => lightbox_edit::Crop::default(),
                    };
                    let angle = match self.binding.value(lightbox_edit::ParamId::Angle) {
                        lightbox_edit::ParamValue::F32(a) => a,
                        _ => 0.0,
                    };
                    self.gizmos
                        .activate(Box::new(canvas::crop_gizmo::CropGizmo::from_current(
                            crop, angle,
                        )));
                }
            }
            // E10 D13: `J` toggles the on-canvas clip overlay, the same
            // one-authority pattern as `VIEW_ZOOM_TOGGLE`/`GEOM_CROP_TOGGLE`
            // above (nothing but `self.canvas`'s own toggle state).
            keymap::HIST_CLIP_OVERLAY_TOGGLE => {
                let enabled = self.canvas.clip_overlay_enabled();
                self.canvas.set_clip_overlay(!enabled);
            }
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

            top_bar_separator(ui);

            // Export had no visible affordance at all, it existed only as
            // the ⌘⇧E chord and a cheat-sheet line. Editing is
            // non-destructive and auto-persists, so there is deliberately no
            // "Save"; that makes Export the ONLY way to get a file out, and
            // an invisible-only-way is how a user concludes their edits go
            // nowhere (reported verbatim: "i don't have a save, or save a
            // copy, does it overwrite the original image?"). Disabled with a
            // reason rather than hidden when nothing is open, so the path
            // out of the app is always discoverable.
            let can_export = self.working_set.active_image().is_some();
            let export = ui
                .add_enabled(can_export, egui::Button::new("Export…"))
                .on_hover_text("Write a NEW file — your original is never modified (⌘⇧E)")
                .on_disabled_hover_text("Open an image first");
            if export.clicked() {
                self.export_dialog.toggle();
            }

            top_bar_separator(ui);

            // §9 typography: "Zoom" in the body role, the resolved
            // mode/percent in the mono slider-value role, one clickable
            // control (click still just toggles Fit/100%, same as before).
            let mut zoom_text = egui::text::LayoutJob::default();
            zoom_text.append(
                "Zoom ",
                0.0,
                egui::TextFormat {
                    font_id: fonts::body_font(),
                    color: tk::TEXT_SECONDARY,
                    ..Default::default()
                },
            );
            zoom_text.append(
                &self.canvas.zoom_mode().label(),
                0.0,
                egui::TextFormat {
                    font_id: fonts::slider_value_font(),
                    color: tk::TEXT_PRIMARY,
                    ..Default::default()
                },
            );
            if ui.button(zoom_text).clicked() {
                self.canvas.toggle_zoom_button();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The app has no OS menu bar of its own (egui draws its
                // own chrome), so About needs a visible home; the top
                // bar's right edge is the closest thing to where a macOS
                // app menu would put it.
                if ui
                    .button("About")
                    .on_hover_text("About Lightbox — version, renderer, and pixelabs.net")
                    .clicked()
                {
                    self.about_window.toggle();
                }
                // H3: load progress moved to the status bar's activity
                // indicator; only the intake skip-count stays up here.
                if intake_skipped > 0 {
                    ui.label(format!(
                        "{intake_skipped} dropped item(s) had no path — skipped"
                    ));
                }
            });
        });
    }

    /// H3: the final status bar (`status.rs`), entry counts, epoch load
    /// progress, the newest Foreground/Background activity (spinner +
    /// label + cancel), and dismissible notice chips.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let loaded = self
            .working_set
            .entries()
            .iter()
            .filter(|it| !matches!(it.state, ItemState::Planned))
            .count();
        let right_text = format!(
            "schema v{} · {:?} · F1 stats",
            self.session.schema_version(),
            self.gpu.backend,
        );
        let bar_ctx = StatusContext {
            entries: self.working_set.entries().len(),
            active: self.working_set.active_index(),
            truncated: self.working_set.truncated(),
            phase: self.working_set.phase(),
            loaded,
            message: &self.status,
            right_text: &right_text,
        };
        let action = status::status_bar_ui(ui, &bar_ctx, &mut self.activity, &mut self.notices);
        if let Some(StatusAction::CancelOpen) = action {
            // Cancelling an in-flight open = replace with an empty set
            // always safe (§2.4: no prompt, edits already auto-persist).
            self.status = "open cancelled".to_owned();
            self.session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(Vec::new(), false, OpenOrigin::Cli),
            });
        }
    }

    fn overlay(&self, ctx: &egui::Context) {
        let thumb_stats = self.thumbs.stats();
        let frame_p95 = percentile(&self.stats.samples_ms, 0.95);
        let frame_max = self.stats.samples_ms.iter().copied().fold(0.0f32, f32::max);
        // H2: the §7 budget is scoped to "with panels open", the rail-open
        // subset is the budget-relevant p95.
        let rail_p95 = percentile(&self.stats.rail_open_ms, 0.95);
        let nav_p95 = percentile(&self.canvas.nav_swap_ms, 0.95);
        // H2: slider→submit latency in FRAMES (§7: input → submit in the
        // same frame; E08 adds ≤ 1 frame).
        let lag_max = self
            .canvas
            .slider_submit_lag_frames
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        let resp = egui::Window::new("frame stats")
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 32.0))
            .resizable(false)
            .collapsible(false)
            // §11: a lighter, glance-and-dismiss debug overlay, popup
            // elevation, not the modal-weight overlay Preferences/Export use.
            .frame(floating_frame(tk::ELEV_3_POPUP, paint::shadow_popup()))
            .show(ctx, |ui| {
                ui.monospace(format!(
                    "frame ms   p95 {frame_p95:6.2}  max {frame_max:6.2} (n={})",
                    self.stats.samples_ms.len()
                ));
                ui.monospace(format!(
                    "rail open  p95 {rail_p95:6.2} ms (n={}, budget <16)",
                    self.stats.rail_open_ms.len()
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
                    "nav swap   p95 {nav_p95:6.2} ms (n={}, budget <50)",
                    self.canvas.nav_swap_ms.len()
                ));
                ui.monospace(format!(
                    "edit→submit lag max {lag_max} frame(s) (n={}, budget ≤1)",
                    self.canvas.slider_submit_lag_frames.len()
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
        finish_floating_window(ctx, &resp);
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

    /// `--screenshot` scripting: settle, ask the compositor for the
    /// viewport's pixels, write the PNG, close (see `screenshot.rs` for the
    /// handshake and why it is not a scripted mode).
    ///
    /// `pipeline_busy` is the same "anything in flight?" predicate that
    /// drives the repaint policy below, holding the shutter until it clears
    /// is what makes `lightbox <file> --screenshot out.png` capture a decoded
    /// canvas rather than a placeholder.
    fn pump_screenshot(&mut self, ctx: &egui::Context, pipeline_busy: bool) {
        let Some(shot) = &mut self.screenshot else {
            return;
        };
        let frames = self.outcome.frames.load(Ordering::Acquire);
        match shot.step(frames, pipeline_busy) {
            ScreenshotStep::Settle | ScreenshotStep::Done => {}
            ScreenshotStep::Request => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            ScreenshotStep::Poll => {
                // The reply rides in on a LATER frame's raw input (the wgpu
                // backend copies the surface after the next paint and maps it
                // back asynchronously), so this is a scan of this frame's
                // events, not a read of a return value.
                let image = ctx.input(|i| {
                    i.events.iter().find_map(|ev| match ev {
                        egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                        _ => None,
                    })
                });
                let Some(image) = image else {
                    return;
                };
                match shot.finish(&image) {
                    Ok([w, h]) => {
                        // The ONE machine-readable line a caller can grep for
                        // (mirrors `--perf-strip`'s JSON summary).
                        println!("screenshot: {w}x{h} px → {}", shot.path().display());
                        self.outcome.screenshot_ok.store(true, Ordering::Release);
                    }
                    Err(err) => {
                        tracing::error!(
                            target: "lightbox_shell",
                            path = %shot.path().display(),
                            %err,
                            "screenshot write failed"
                        );
                    }
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ScreenshotStep::Expired => {
                tracing::error!(
                    target: "lightbox_shell",
                    frames,
                    "screenshot request got no reply — the viewport never delivered its pixels"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// H2 `--perf-strip` scripting: open the staged ~300-entry set once,
    /// then drive the sawtooth scroll (returning the forced fraction for
    /// this frame) and the nav phase through the REAL seams
    /// (`filmstrip_ui`'s scroll offset, `WorkingSetView::nav`).
    fn pump_perf(&mut self, ctx: &egui::Context) -> Option<f32> {
        // Field-split so the driver, view model and canvas borrow
        // disjointly.
        let perf = self.perf.as_mut()?;
        if !perf.submitted {
            perf.submitted = true;
            perf.mark_opened();
            let photos = perf.photos.clone();
            self.session.submit(Command::OpenWorkingSet {
                request: OpenRequest::new(vec![photos], false, OpenOrigin::Cli),
            });
        }
        let loaded = self
            .working_set
            .entries()
            .iter()
            .filter(|it| !matches!(it.state, ItemState::Planned))
            .count();
        let thumb_stats = self.thumbs.stats();
        match perf.pump(
            loaded,
            self.outcome.texture_swaps.load(Ordering::Acquire),
            &self.canvas.nav_swap_ms,
            (thumb_stats.requested_total, thumb_stats.cancelled_total),
        ) {
            StripPhase::Warmup | StripPhase::NavSettle => None,
            StripPhase::Scroll(frac) => Some(frac),
            StripPhase::Nav => {
                self.working_set.nav(1);
                None
            }
            StripPhase::Done(json) => {
                // The ONE machine-readable summary line (H2 AC), consumed
                // by lbx-perf's `perf-strip` scenario + the nightly job.
                println!("{json}");
                self.outcome.perf_ok.store(true, Ordering::Release);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                None
            }
            StripPhase::Expired => {
                tracing::error!(
                    target: "lightbox_shell",
                    "perf-strip run expired before the capture completed"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                None
            }
        }
    }

    /// H4 exit-drill scripting (`--drill-edit` / `--drill-verify`): the
    /// editor leg of `cargo xtask exit-drill`. See `drill.rs`.
    fn pump_drill(&mut self, ctx: &egui::Context) {
        let Some(drill) = self.drill.as_mut() else {
            return;
        };
        if drill.expired() && drill.state != DrillState::Hold {
            eprintln!("DRILL FAIL: timed out in state {:?}", drill.state);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let seam = self.outcome.seam_proven.load(Ordering::Acquire);
        let strip = self.outcome.filmstrip_shown.load(Ordering::Acquire);
        match drill.state {
            DrillState::Submit => {
                let photos = drill.photos();
                self.session.submit(Command::OpenWorkingSet {
                    request: OpenRequest::new(vec![photos], false, OpenOrigin::Cli),
                });
                drill.state = DrillState::WaitCanvas;
            }
            DrillState::WaitCanvas => {
                // The same proof `--smoke` requires: a filmstrip row AND an
                // engine texture composited zero-copy, the "filmstrip/
                // canvas smoke" half of the H4 leg.
                let Some(image) = self.working_set.active_image() else {
                    return;
                };
                if !(seam && strip) {
                    return;
                }
                match &drill.mode {
                    drill::DrillMode::EditCommit { .. } => {
                        // "Edit via a slider": the exact gesture path a
                        // param_slider drag takes (E4/E3 seams).
                        self.binding.begin_gesture(lightbox_edit::ParamId::Exposure);
                        drill.state = DrillState::Dragging(0);
                    }
                    drill::DrillMode::Verify {
                        expect_image,
                        expect_exposure,
                        ..
                    } => {
                        let value = self.binding.value(lightbox_edit::ParamId::Exposure);
                        let v = match value {
                            lightbox_edit::ParamValue::F32(x) => f64::from(x),
                            _ => f64::NAN,
                        };
                        let ok_image = image.0 == *expect_image;
                        let ok_value = (v - f64::from(*expect_exposure)).abs() < 1e-3;
                        if ok_image && ok_value {
                            println!("DRILL-VERIFY ok image={} exposure={v:.4}", image.0);
                            self.outcome.drill_ok.store(true, Ordering::Release);
                        } else {
                            println!(
                                "DRILL-VERIFY FAIL image={} (expect {}) exposure={v:.4} \
                                 (expect {expect_exposure:.4})",
                                image.0, expect_image
                            );
                        }
                        drill.state = DrillState::Closing;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            DrillState::Dragging(n) => {
                let drill::DrillMode::EditCommit { exposure, .. } = &drill.mode else {
                    return;
                };
                if n < DRAG_FRAMES {
                    // One preview per frame, walking toward the target
                    // a realistic short drag (each preview re-renders the
                    // canvas the same frame via the C3 recipe_rev key).
                    let v = exposure * (n + 1) as f32 / DRAG_FRAMES as f32;
                    let mut delta = lightbox_edit::ParamDelta::new();
                    delta.0.insert(
                        lightbox_edit::ParamId::Exposure,
                        lightbox_edit::ParamValue::F32(v),
                    );
                    self.binding.preview(delta);
                    drill.state = DrillState::Dragging(n + 1);
                } else {
                    // ONE coalesced durable commit (E09 discipline).
                    self.binding.end_gesture();
                    drill.state = DrillState::AwaitCommit;
                }
            }
            DrillState::AwaitCommit => {
                // `drain_events` feeds `drill.on_edit_committed`.
                if let Some(seq) = drill.committed_seq {
                    let Some(image) = self.working_set.active_image() else {
                        return;
                    };
                    let v = match self.binding.value(lightbox_edit::ParamId::Exposure) {
                        lightbox_edit::ParamValue::F32(x) => f64::from(x),
                        _ => f64::NAN,
                    };
                    // The line xtask waits for before delivering kill -9.
                    // (Rust's stdout is line-buffered even into a pipe.)
                    println!("DRILL-EDIT image={} exposure={v:.4} seq={seq}", image.0);
                    use std::io::Write as _;
                    let _ = std::io::stdout().flush();
                    drill.state = DrillState::Hold;
                }
            }
            // Hold: keep painting until xtask kills the process. Closing:
            // the viewport-close command is on its way.
            DrillState::Hold | DrillState::Closing => {}
        }
    }
}

impl eframe::App for LightboxApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.stats.tick(self.rail_visible);
        self.drain_events();
        // E3: bind the edit adapter to the active image once per frame
        // (idempotent; refreshes its read caches only when events marked
        // them dirty). A click-activation later this frame catches up on
        // the next frame, the canvas falls back to the persisted read
        // for that one transition frame (see `SessionEditBinding`).
        self.binding.bind(self.working_set.active_image());
        self.thumbs.pump(&ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.show_overlay = !self.show_overlay;
        }

        // D2: keymap dispatch, BEFORE any widget is built, so matched
        // chords are consumed and never double-handled (keymap/dispatch.rs
        // module docs). The stack is rebuilt per frame; handlers run
        // immediately (same frame as the input, §7). G3/D5: while the
        // prefs panel's rebind editor is capturing the next chord, it owns
        // the keyboard instead, running the normal dispatcher over the
        // same input would let the captured key ALSO fire its bound action
        // (e.g. capturing "Z" would both rebind and toggle zoom).
        let stack = self.keymap_stack();
        if !self.prefs_window.is_capturing() {
            for action in keymap::dispatch::dispatch(&ctx, &self.registry, &stack) {
                self.handle_action(action);
            }
        }

        // A2: fold this frame's drop/hover input. G6: `drop_recursive_default`
        // is the persisted §6.7 knob (Alt still overrides per-drop
        // regardless, inside `intake::pump` itself).
        let intake_frame =
            ctx.input(|i| intake::pump(i, self.prefs.machine().drop_recursive_default));
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

        // H2: the `--perf-strip` script (open once, sawtooth scroll, nav
        // phase), resolved before the filmstrip renders so this frame's
        // forced scroll offset applies to this frame.
        let forced_scroll = self.pump_perf(&ctx);

        // §1: top bar docked at floor elevation, engraved seam where it
        // meets the rail/canvas below, the bar itself is the lighter
        // surface, so the highlight stroke sits above the hairline.
        let top_resp = egui::Panel::top(egui::Id::new("lightbox-top"))
            .frame(docked_frame(
                tk::ELEV_0_BASE,
                egui::Margin::symmetric(tk::SPACE_3 as i8, tk::SPACE_2 as i8),
            ))
            .show(root, |ui| self.top_bar(ui, intake_frame.skipped_pathless));
        {
            let (painter, rect) = panel_painter(&ctx, &top_resp);
            paint::engraved_seam_h(
                &painter,
                egui::Rangef::new(rect.left(), rect.right()),
                rect.bottom(),
                false,
            );
        }
        // Status bar: `STATUS_BAR_BG` fill at the frame level only, the
        // top-edge seam is left to `status.rs`'s own owner rather than
        // risk a double-paint (see this phase's report for the reasoning).
        egui::Panel::bottom(egui::Id::new("lightbox-status"))
            .frame(docked_frame(
                tk::STATUS_BAR_BG,
                egui::Margin::symmetric(8, 2),
            ))
            .show(root, |ui| self.status_bar(ui));

        // E1: the develop-panel rail (`panels::PanelHost`), toggled by
        // `panel.toggle_rail` (Tab). Panels address the active *Ready*
        // image through the E3 binding; with no editable image the rail
        // shows a quiet placeholder instead of dead controls.
        if self.rail_visible {
            // E2: the §4 source-kind authority is the working-set item's
            // probe-derived flag; `None` (unsupported) is treated as
            // `Rendered`, the safe side (raw-only surfaces hidden).
            let source_kind = self
                .working_set
                .active()
                .and_then(|item| item.source_kind)
                .unwrap_or(lightbox_types::SourceKind::Rendered);
            let editable = self.working_set.active_image().is_some();
            // D12: push the canvas's latest (non-blocking, possibly one-
            // frame-stale, same lag class as H2's slider-submit probe)
            // histogram reduction into the binding, the one seam the
            // histogram panel reads through (`EditBinding::histogram`).
            self.binding
                .set_latest_histogram(self.canvas.latest_histogram().cloned());
            let panel_host = &mut self.panel_host;
            let binding = &mut self.binding;
            let gizmos = &mut self.gizmos;
            // The rail frames' own horizontal inset is deliberately ZERO.
            // It used to be `SPACE_3`, which stacked with the per-panel
            // body's 12pt padding for 48pt of horizontal inset on a 280pt
            // rail, the panels' bands floated in a gutter and the sliders
            // lost a sixth of the rail. `panels::host` insets the "Develop"
            // heading row itself instead, so the header bands and body
            // cards now run edge to edge like a Lightroom rail's.
            //
            // Both rails are `exact_size` + non-resizable: their widths come
            // from the dock layout (one per column, dragged on the seams
            // `panels::host` paints itself), not from egui's own panel
            // resize handle, which only knows about a single edge.
            let rail_frame = || {
                docked_frame(
                    tk::ELEV_0_BASE,
                    egui::Margin::symmetric(0, tk::SPACE_2 as i8),
                )
            };
            let screen_w = ctx.content_rect().width();
            // Seam painting is identical for both rails; only which edge
            // faces the canvas differs.
            let seam = |resp: &egui::InnerResponse<()>, side: DockSide| {
                let (painter, rect) = panel_painter(&ctx, resp);
                let (x, lighter_right) = match side {
                    DockSide::Left => (rect.right(), false),
                    DockSide::Right => (rect.left(), true),
                };
                paint::engraved_seam_v(
                    &painter,
                    egui::Rangef::new(rect.top(), rect.bottom()),
                    x,
                    lighter_right,
                );
            };

            if editable {
                for side in [DockSide::Left, DockSide::Right] {
                    // The left rail exists only once the user has dragged a
                    // panel over there; the right one disappears only if
                    // they dragged them all away.
                    if !panel_host.has_dock(side, source_kind) {
                        // A rail that isn't on screen must not leave a
                        // stale rect behind for the drop hit-test.
                        panel_host.forget_dock(side);
                        continue;
                    }
                    let panel = match side {
                        DockSide::Left => {
                            egui::Panel::left(egui::Id::new("lightbox-develop-rail-left"))
                        }
                        DockSide::Right => {
                            egui::Panel::right(egui::Id::new("lightbox-develop-rail"))
                        }
                    };
                    let resp = panel
                        .resizable(false)
                        .exact_size(panel_host.dock_width(side, source_kind, screen_w))
                        .frame(rail_frame())
                        .show(root, |ui| {
                            let mut dev_ctx = DevelopCtx {
                                source_kind,
                                edit: binding,
                                gizmos,
                            };
                            panel_host.dock_ui(ui, &mut dev_ctx, side);
                        });
                    seam(&resp, side);
                }
                // The docking gesture spans both rails and the canvas
                // strips between them, so it resolves once, after both
                // have painted.
                self.panel_host.finish_dnd(&ctx);
                panels::persist_layout_if_dirty(&mut self.panel_host, &self.prefs);
            } else {
                // No editable image: one placeholder rail, at its own id
                // and its own fixed width. Distinct from the docked rails
                // on purpose (the filmstrip's collapsed-bar trick): a
                // two-column dock layout must not stretch the placeholder,
                // and the two states must not trade widget identities
                // inside one multi-pass frame.
                for side in [DockSide::Left, DockSide::Right] {
                    panel_host.forget_dock(side);
                }
                let resp = egui::Panel::right(egui::Id::new("lightbox-develop-rail-empty"))
                    .resizable(false)
                    .exact_size(panels::layout::DEFAULT_COLUMN_PT)
                    .frame(rail_frame())
                    .show(root, |ui| {
                        // The empty state has no panel bands to run to the
                        // edge, so it supplies the inset the frame no
                        // longer does.
                        egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(tk::SPACE_3 as i8, 0))
                            .show(ui, |ui| {
                                ui.heading("Develop");
                                ui.weak("Open an image to edit.");
                            });
                    });
                seam(&resp, DockSide::Right);
            }
        }

        // C5: project the active working-set entry's `ItemState` into a
        // `CanvasContent` the canvas owns rendering for (placard/shimmer/
        // normal render, `canvas::states`). Owned (not borrowed) so this
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
                is_raw: bool,
            },
        }
        let projection = self.working_set.active().map(|item| match item.state {
            ItemState::Planned => ActiveProjection::Loading,
            ItemState::Ready { image, .. } => ActiveProjection::Ready {
                image,
                filename: item.filename.clone(),
                width: item.width,
                height: item.height,
                // Probe-derived, the same §4 authority the develop rail uses
                // to decide which raw-only panels exist. Feeds the canvas
                // badge, which has to say "embedded preview" on a raw file
                // and must NOT say it on a JPEG, where the file IS the image.
                is_raw: item.source_kind == Some(lightbox_types::SourceKind::Raw),
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

            // Both branches share the filmstrip's ELEV_0_BASE floor fill
            // and default (8,2) margin, unchanged from egui's prior
            // stock-frame default, so `filmstrip.rs`'s own internal
            // layout math is untouched; only the fill is now explicit and
            // a top-edge engraved seam marks the canvas boundary (§6:
            // "container top edge: separator_hairline + highlight seam
            // against the canvas above", the filmstrip is the lighter
            // surface, below the seam).
            let filmstrip_frame = || docked_frame(tk::ELEV_0_BASE, egui::Margin::symmetric(8, 2));
            if self.filmstrip.collapsed() {
                // Distinct panel id: the collapsed bar's exact size must
                // not be remembered as the expanded strip's height.
                let resp = egui::Panel::bottom(egui::Id::new("lightbox-filmstrip-collapsed"))
                    .exact_size(filmstrip::COLLAPSED_BAR_PT)
                    .frame(filmstrip_frame())
                    .show(root, |ui| {
                        filmstrip::collapsed_bar_ui(ui, &mut self.filmstrip, active, total);
                    });
                let (painter, rect) = panel_painter(&ctx, &resp);
                paint::engraved_seam_h(
                    &painter,
                    egui::Rangef::new(rect.left(), rect.right()),
                    rect.top(),
                    true,
                );
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
                    .frame(filmstrip_frame())
                    .show(root, |ui| {
                        filmstrip::filmstrip_ui(
                            ui,
                            strip,
                            total,
                            active,
                            &cell_of,
                            thumbs,
                            &mut visible,
                            forced_scroll,
                        )
                    });
                let (painter, rect) = panel_painter(&ctx, &shown);
                paint::engraved_seam_h(
                    &painter,
                    egui::Rangef::new(rect.left(), rect.right()),
                    rect.top(),
                    true,
                );
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
        // track, the other projections have no image at all.
        let new_shown = match &projection {
            Some(ActiveProjection::Ready { image, .. }) => Some(*image),
            _ => None,
        };
        if new_shown != self.last_shown_image {
            match new_shown {
                Some(image) => self.canvas.enter(image),
                None => self.canvas.exit(),
            }
            // F1: an active gizmo belongs to the image it was armed on
            // navigation/exit cancels it (its `Cancelled` effect drains as
            // a no-op below; the keymap context pops next frame).
            self.gizmos.cancel_all();
            self.last_shown_image = new_shown;
        }

        // E11 canvas-display fix, Part B (`E11-deviations.md` "geometry
        // canvas display"): sync the crop-tool-active override from THIS
        // frame's final gizmo-stack state, after keymap dispatch
        // (`GEOM_CROP_TOGGLE`/`GIZMO_CANCEL`/`GIZMO_COMMIT` above), the
        // rail's own crop-tool button/aspect-preset clicks (panels are
        // built earlier this frame), and the image-transition
        // `cancel_all` just above have all resolved. While armed, the
        // canvas must render the UNCROPPED image so the user can see
        // outside the current crop to adjust it; `SessionEditBinding`
        // dedups a same-state call (no spurious resubmit).
        self.binding
            .set_crop_tool_active(self.gizmos.is_active(canvas::crop_gizmo::GEOM_CROP));

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(root.style()).fill(crate::theme::canvas_surround()))
            .show(root, |ui| match &projection {
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
                            is_raw,
                        } => CanvasContent::Ready(ActiveEntry {
                            image: *image,
                            filename,
                            width: *width,
                            height: *height,
                            is_raw: *is_raw,
                        }),
                    };
                    let scheduler = self.session.render_scheduler();
                    let max_tex_dim = self.gpu.limits.max_texture_dimension_2d;
                    let idx = self.working_set.active_index().unwrap_or(0);
                    let total = self.working_set.entries().len();
                    let canvas = &mut self.canvas;
                    // E3: the binding IS the canvas's C3 `RecipeSource`, the
                    // active image renders the live working recipe (previews
                    // included, same frame), others the persisted fallback.
                    let recipe_source = &mut self.binding;
                    // F1: the gizmo layer routes before pan/zoom and paints
                    // above the composited image inside `ui`.
                    let gizmos = &mut self.gizmos;
                    let device_degraded = self.device_degraded.as_deref();
                    canvas.ui(
                        ui,
                        &scheduler,
                        max_tex_dim,
                        content,
                        recipe_source,
                        gizmos,
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
                    // Loading/Duplicate cell either, never blank, never a
                    // false "drop here" (§6.4 canvas states).
                    ui.centered_and_justified(|ui| {
                        ui.weak(match self.working_set.phase() {
                            lightbox_core::SetPhase::Planning
                            | lightbox_core::SetPhase::Loading => "opening…",
                            _ => "no previewable images in this set",
                        });
                    });
                }
            });

        // F1/F2: drain this frame's gizmo effects, the app end of the
        // §6.6 contract. Edit* effects map 1:1 onto the E3 binding's
        // gesture lifecycle (one drag = one coalesced history step);
        // `Picked` routes to the owning panel's callback (at M1 only the
        // WB eyedropper emits it). The preview/commit re-renders the same
        // frame via the C3 recipe_rev submit key, exactly like a slider.
        let picked_dims = match &projection {
            Some(ActiveProjection::Ready { width, height, .. }) => Some((*width, *height)),
            _ => None,
        };
        for effect in self.gizmos.take_effects() {
            match effect {
                GizmoEffect::EditBegin(param) => self.binding.begin_gesture(param),
                GizmoEffect::Edit(delta) => self.binding.preview(delta),
                GizmoEffect::EditEnd => self.binding.end_gesture(),
                GizmoEffect::Picked { image_px } => {
                    if let Some((w, h)) = picked_dims {
                        panels::basic::apply_wb_pick(
                            &mut self.binding,
                            image_px,
                            egui::vec2(w as f32, h as f32),
                        );
                        self.status = format!(
                            "white balance set from pick at ({:.0}, {:.0})",
                            image_px.x, image_px.y
                        );
                    }
                }
                // Pops already happened inside the layer; the keymap
                // context follows on the next frame's stack rebuild.
                GizmoEffect::Done | GizmoEffect::Cancelled => {}
            }
        }

        // Cancel-on-scroll-out + O(visible) texture memory. A new epoch's
        // filmstrip only ever iterates its own entries, so a replace (A6)
        // naturally excludes the old epoch's images from `visible`, their
        // in-flight thumb tickets are cancelled here on the very next frame.
        let cap = (visible.len() * 3).max(64);
        self.thumbs.end_frame(&visible, cap);

        // G3-G6: the preferences panel (⌘, toggles via `app.prefs`), plus
        // the un-cut D5 rebind editor as its Keyboard section. Field-split
        // borrows (mirrors the develop-rail's own disjoint-borrow pattern
        // above) so this can take `&mut` on several fields at once.
        {
            let prefs = &self.prefs;
            let session = &self.session;
            let gpu = &self.gpu;
            let registry = &mut self.registry;
            let filmstrip = &mut self.filmstrip;
            let panels = &mut self.panel_host;
            let keymap_path = &self.keymap_path;
            let status = &mut self.status;
            self.prefs_window.ui(
                &ctx,
                &mut PrefsPanelCtx {
                    prefs,
                    session,
                    gpu,
                    registry,
                    filmstrip,
                    panels,
                    keymap_path,
                    status,
                },
            );
        }

        // About: identity + the build facts a bug report needs. Cheap
        // enough to build unconditionally (it no-ops while closed).
        {
            let config_dir = self
                .prefs
                .machine_path()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            let backend = format!("{:?}", self.gpu.backend);
            self.about_window.ui(
                &ctx,
                &AboutInfo {
                    adapter: &self.gpu.adapter_info.name,
                    backend: &backend,
                    config_dir: &config_dir,
                    store_dir: &self.store_dir,
                },
            );
        }

        // E15 core slice: the Export dialog (⌘⇧E toggles via `app.export`).
        // Selection: the filmstrip's multi-select when non-empty (already
        // documented there as the batch-ops seam), else the single active
        // image, "single image + basic batch" with no new selection UI.
        {
            let images = self.export_selection();
            let session = &self.session;
            let status = &mut self.status;
            self.export_dialog.ui(
                &ctx,
                &mut ExportPanelCtx {
                    session,
                    images: &images,
                    status,
                },
            );
        }

        // D4: the cheat-sheet overlay (⌘/ toggles via `app.cheatsheet`),
        // rendered over everything with this frame's live context stack.
        self.cheatsheet.ui(&ctx, &self.registry, &stack);

        if self.show_overlay {
            self.overlay(&ctx);
        }

        self.outcome.frames.fetch_add(1, Ordering::AcqRel);
        self.pump_smoke(&ctx);
        self.pump_drill(&ctx);

        // Repaint policy: run hot while anything is in flight; otherwise a
        // slow idle poll keeps the event pump alive without burning a core.
        // The pipeline half is named separately because `--screenshot` holds
        // its shutter on exactly this predicate, "the app has stopped
        // working, so what's on screen is what it means to show".
        let pipeline_busy = matches!(
            self.working_set.phase(),
            lightbox_core::SetPhase::Planning | lightbox_core::SetPhase::Loading
        ) || self.thumbs.stats().inflight > 0
            || self.canvas.busy()
            || self.explorer.is_open()
            || self.prefs_window.busy() // G4: purge/relocate in flight
            || self.export_dialog.busy() // E15: export in flight
            || self.activity.current().is_some(); // H3: spinner animation
        self.pump_screenshot(&ctx, pipeline_busy);

        let busy = pipeline_busy
            || self.smoke.is_some()
            || self.perf.is_some()
            || self.drill.is_some()
            || self.screenshot.is_some();
        if busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn on_exit(&mut self) {
        // Exit-time verified backup per policy (spec T15/OQ-6); scripted
        // runs and screenshot captures skip it (throwaway/drill-owned
        // catalogs, nothing worth backing up, and the capture should exit
        // promptly).
        let policy = if self.smoke.is_some()
            || self.perf.is_some()
            || self.drill.is_some()
            || self.screenshot.is_some()
        {
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
    fn frame_stats_buffers_are_bounded_and_split_by_rail_state() {
        let mut stats = FrameStats::new();
        for i in 0..100 {
            // H2: rail-open frames feed BOTH buffers; closed only the main.
            stats.tick(i % 2 == 0);
        }
        assert!(
            stats.rail_open_ms.len() < stats.samples_ms.len(),
            "the rail-open subset is strictly smaller when the rail toggles"
        );
        for _ in 0..500 {
            stats.tick(true);
        }
        assert!(stats.samples_ms.len() <= 240);
        assert!(stats.rail_open_ms.len() <= 240);
    }

    /// The bundled starter preset library resolves to exactly the committed
    /// `assets/presets/**/*.xmp` files (the "register the dir so it loads
    /// in the browser" requirement's first half).
    #[test]
    fn bundled_preset_files_finds_the_starter_library() {
        let files = bundled_preset_files();
        assert_eq!(
            files.len(),
            lightbox_edit::preset_library::starter_presets().len(),
            "bundled_preset_files drifted from the starter library's own preset count"
        );
        for f in &files {
            assert_eq!(f.extension().and_then(|e| e.to_str()), Some("xmp"));
        }
    }

    /// **AC: the starter library "loads in the browser."** Submitting
    /// `Command::Edit(SeedBundledPresets { paths: bundled_preset_files() })`
    /// the EXACT command `LightboxApp::new` submits at real-launch startup
    /// against a fresh, empty preset store lands every bundled preset,
    /// browsable via `Queries::presets()` (what the panel's `presets()`
    /// cache reads).
    #[test]
    fn bundled_preset_files_seed_a_fresh_store_through_the_same_command_new_submits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = CoreConfig::default();
        cfg.preset_dir = Some(tmp.path().join("presets"));
        let core = Core::start(cfg).expect("core start");
        let session = core
            .create_catalog(&tmp.path().join("cat.lbdata"), None)
            .expect("create catalog");

        let files = bundled_preset_files();
        assert!(
            !files.is_empty(),
            "the starter library must be non-empty to prove seeding"
        );
        let mut rx = session.events();
        session.submit(Command::Edit(EditCommand::SeedBundledPresets {
            paths: files.clone(),
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the seed import to land"
            );
            match rx.try_recv() {
                Ok(Event::CatalogChanged { .. }) => break,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    panic!("event channel closed while seeding")
                }
            }
        }

        let presets = session.query().presets().expect("presets query");
        assert_eq!(
            presets.len(),
            files.len(),
            "every bundled preset file landed in the fresh store"
        );
        assert!(
            presets
                .iter()
                .any(|p| p.name == "Anvil" && p.group.as_deref() == Some("Tonal")),
            "a representative starter preset is browsable after seeding"
        );
    }
}
