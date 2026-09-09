// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! G3-G6, the ⌘, preferences & performance panel (spec §8 Phase G), plus
//! the **un-cut D5 rebind editor** as its Keyboard section (Phase D's named
//! cut-line, deferred to "whichever phase builds the prefs panel", this
//! one; see `E08-deviations.md` D0-4 and the Phase G section).
//!
//! One `egui::Window` (same structural weight as the D4 cheat sheet),
//! sectioned: **GPU** (engine mode, adapter readout, VRAM), **Caches**
//! (caps / retention / usage bars / purge / relocate, E03's §5.6 command
//! surface), **Jobs** (per-class worker overrides + the advanced
//! event-capacity knob), **Interface** (theme / drop default / filmstrip /
//! config-file readouts), **Keyboard** (D5: capture-next-chord, conflict
//! steal/cancel, per-row + global reset).
//!
//! **Live vs restart** is documented per row IN the panel (G6): every knob
//! carries a small `live`/`restart` badge, `live` applies this frame
//! (theme, drop default, cache caps via `Command::SetCacheLimits`, strip
//! height/collapse, keyboard rebinds); `restart` is stored now and read at
//! the next `Core::start`/session open (GPU mode, Q6's M1 posture, job
//! sizing, event capacity, T2 retention).
//!
//! **Threading discipline** (spec §7: no SQL in the frame loop): the cache
//! usage bars poll `Queries::cache_stats` at a ~2 s cadence while the
//! window is open (plus an event-driven dirty mark), never per frame;
//! prefs writes are staged through [`StagedValue`] so a drag commits ONE
//! file/txn write on release, not one per frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use lightbox_core::prefs::{GpuMode, PrefsStore, Theme};
use lightbox_core::{CacheStats, Command, CommandTicket, PurgeScope, Session};
use lightbox_render::GpuContext;

use crate::filmstrip::{FilmstripState, MAX_HEIGHT_PT, MIN_HEIGHT_PT};
use crate::keymap::chord::{Chord, Platform};
use crate::keymap::{ActionId, KeymapRegistry};
use crate::theme::{fonts, paint, tokens as tk};

/// How often the open panel re-polls `cache_stats` (reader query, kept off
/// the per-frame path per §7; events additionally mark it dirty).
const STATS_REFRESH: Duration = Duration::from_secs(2);

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Everything the panel touches, borrowed disjointly from `LightboxApp`
/// once per frame.
pub struct PrefsPanelCtx<'a> {
    pub prefs: &'a PrefsStore,
    pub session: &'a Session,
    pub gpu: &'a GpuContext,
    pub registry: &'a mut KeymapRegistry,
    pub filmstrip: &'a mut FilmstripState,
    /// The develop-panel registry, Preferences → Panels switches panels
    /// off/on and resets the dock layout through it.
    pub panels: &'a mut crate::panels::PanelHost,
    /// Where keyboard rebinds persist (`keymap.toml`; a throwaway path in
    /// smoke mode), also the G6 location readout.
    pub keymap_path: &'a Path,
    pub status: &'a mut String,
}

/// A rebind that hit [`KeymapRegistry::rebind`]'s conflict check, awaiting
/// the user's steal/cancel call (D5).
struct PendingConflict {
    id: ActionId,
    chord: Chord,
    with: Vec<ActionId>,
}

/// A `DragValue` staging cell: edits accumulate in-widget during a drag /
/// while focused, and commit exactly once on release (prefs writes are
/// file- or txn-backed, never per-drag-frame).
#[derive(Default)]
struct StagedValue {
    value: Option<f64>,
}

impl StagedValue {
    /// Shows the drag; returns `Some(new)` exactly once, on commit.
    /// `label` is the control's accessible name (H1: every prefs knob is
    /// a named AccessKit slider, not an anonymous number box).
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        current: f64,
        range: std::ops::RangeInclusive<f64>,
        speed: f64,
        suffix: &str,
    ) -> Option<f64> {
        let mut v = self.value.unwrap_or(current);
        let resp = ui.add(
            egui::DragValue::new(&mut v)
                .range(range)
                .speed(speed)
                .suffix(suffix),
        );
        {
            let label = label.to_owned();
            resp.widget_info(move || egui::WidgetInfo::slider(true, v, label.clone()));
        }
        if resp.changed() {
            self.value = Some(v);
        }
        if let Some(staged) = self.value {
            if !resp.dragged() && !resp.has_focus() {
                self.value = None;
                if staged != current {
                    return Some(staged);
                }
            }
        }
        None
    }
}

/// The preferences window (toggled by `app.prefs`, ⌘,).
pub struct PrefsWindow {
    open: bool,

    // ── G4 cache section runtime state ──────────────────────────────────
    stats: Option<CacheStats>,
    stats_at: Option<Instant>,
    stats_dirty: bool,
    /// Two-step purge confirm (inline, not a modal).
    purge_confirm: Option<PurgeScope>,
    /// Picked-but-unconfirmed relocation target.
    relocate_confirm: Option<PathBuf>,
    purge_ticket: Option<CommandTicket>,
    relocate_ticket: Option<CommandTicket>,
    /// §5.2 invalid-target fallback badge: the last failed cache op.
    cache_error: Option<String>,

    // ── D5 keyboard section state ────────────────────────────────────────
    /// `Some(action)` while waiting for the next chord.
    capture: Option<ActionId>,
    conflict: Option<PendingConflict>,

    // ── staged drags (commit-on-release, see `StagedValue`) ─────────────
    staged_vram_mb: StagedValue,
    staged_preview_cap: StagedValue,
    staged_raw_cap: StagedValue,
    staged_t2_days: StagedValue,
    staged_workers: StagedValue,
    staged_interactive: StagedValue,
    staged_foreground: StagedValue,
    staged_background: StagedValue,
    staged_event_cap: StagedValue,
    staged_strip_height: StagedValue,
}

impl PrefsWindow {
    /// Starts closed.
    pub fn new() -> PrefsWindow {
        PrefsWindow {
            open: false,
            stats: None,
            stats_at: None,
            stats_dirty: false,
            purge_confirm: None,
            relocate_confirm: None,
            purge_ticket: None,
            relocate_ticket: None,
            cache_error: None,
            capture: None,
            conflict: None,
            staged_vram_mb: StagedValue::default(),
            staged_preview_cap: StagedValue::default(),
            staged_raw_cap: StagedValue::default(),
            staged_t2_days: StagedValue::default(),
            staged_workers: StagedValue::default(),
            staged_interactive: StagedValue::default(),
            staged_foreground: StagedValue::default(),
            staged_background: StagedValue::default(),
            staged_event_cap: StagedValue::default(),
            staged_strip_height: StagedValue::default(),
        }
    }

    /// The `app.prefs` action target.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            self.stats_dirty = true; // fresh bars on open
        }
    }

    /// True while the Keyboard section owns the next keystroke, `lib.rs`
    /// bypasses the keymap dispatcher for that frame so the captured chord
    /// is not also executed.
    pub fn is_capturing(&self) -> bool {
        self.open && self.capture.is_some()
    }

    /// Keep the frame pump hot while a purge/relocate is in flight (their
    /// completion arrives as events).
    pub fn busy(&self) -> bool {
        self.purge_ticket.is_some() || self.relocate_ticket.is_some()
    }

    /// `Event::CachePurged` arrived.
    pub fn on_cache_purged(&mut self) {
        self.purge_ticket = None;
        self.cache_error = None;
        self.stats_dirty = true;
    }

    /// `Event::CacheRelocated` arrived (the caller persists the new root
    /// into the catalog prefs, this only clears the progress state).
    pub fn on_cache_relocated(&mut self) {
        self.relocate_ticket = None;
        self.cache_error = None;
        self.stats_dirty = true;
    }

    /// Any preview row was evicted, usage bars are stale.
    pub fn mark_stats_dirty(&mut self) {
        self.stats_dirty = true;
    }

    /// `Event::CommandFailed` arrived; claims it when the ticket is one of
    /// ours (the §5.2 invalid-relocation-target path lands here).
    pub fn on_command_failed(&mut self, ticket: CommandTicket, error: &str) {
        if self.purge_ticket == Some(ticket) || self.relocate_ticket == Some(ticket) {
            self.purge_ticket = None;
            self.relocate_ticket = None;
            self.cache_error = Some(error.to_owned());
        }
    }

    /// Renders the window (no-op while closed).
    pub fn ui(&mut self, ctx: &egui::Context, p: &mut PrefsPanelCtx<'_>) {
        if !self.open {
            self.capture = None;
            self.conflict = None;
            return;
        }

        // D5: consume the captured chord BEFORE building rows, so the
        // "press a key…" button reflects one frame of arming at most.
        self.poll_capture(ctx, p);

        self.refresh_stats_if_due(p);

        let mut open = self.open;
        // §11: modal-weight dialog (it commits real settings, some
        // restart-required), overlay elevation, not the lighter popup
        // level the Browse Folders/Keyboard Shortcuts dialogs use.
        let resp = egui::Window::new("Preferences")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(crate::floating_frame(
                tk::ELEV_4_OVERLAY,
                paint::shadow_overlay(),
            ))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(480.0)
                    .show(ui, |ui| {
                        self.appearance_section(ui, ctx, p);
                        ui.separator();
                        self.panels_section(ui, p);
                        ui.separator();
                        self.interface_section(ui, p);
                        ui.separator();
                        self.gpu_section(ui, p);
                        ui.separator();
                        self.cache_section(ui, p);
                        ui.separator();
                        self.jobs_section(ui, p);
                        ui.separator();
                        self.keyboard_section(ui, p);
                    });
            });
        crate::finish_floating_window(ctx, &resp);
        self.open = open;
    }

    // ── G3: GPU ─────────────────────────────────────────────────────────

    fn gpu_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        let machine = p.prefs.machine();
        ui.strong("GPU");
        egui::Grid::new("prefs-gpu")
            .num_columns(3)
            .min_col_width(140.0)
            .show(ui, |ui| {
                ui.label("Engine");
                let mut mode = machine.gpu_mode;
                egui::ComboBox::from_id_salt("prefs-gpu-mode")
                    .selected_text(match mode {
                        GpuMode::Auto => "Auto (GPU when available)",
                        GpuMode::Off => "Off (CPU engine)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mode, GpuMode::Auto, "Auto (GPU when available)");
                        ui.selectable_value(&mut mode, GpuMode::Off, "Off (CPU engine)");
                    });
                if mode != machine.gpu_mode {
                    p.prefs.set_machine(|m| m.gpu_mode = mode);
                    *p.status = "GPU mode saved — takes effect on restart".to_owned();
                }
                badge(ui, false);
                ui.end_row();

                ui.label("Adapter");
                // The Phase-A/C readout (T5), verbatim.
                ui.add(
                    egui::Label::new(egui::RichText::new(p.gpu.adapter_report()).small()).wrap(),
                );
                ui.label("");
                ui.end_row();

                ui.label("Engine VRAM resident");
                ui.label(fmt_bytes(p.session.engine().stats().vram_bytes));
                ui.label("");
                ui.end_row();

                ui.label("VRAM budget override");
                ui.horizontal(|ui| {
                    let mut auto = machine.vram_budget_override_mb.is_none();
                    if ui.checkbox(&mut auto, "Auto").changed() {
                        let value = if auto { None } else { Some(2048) };
                        p.prefs.set_machine(|m| m.vram_budget_override_mb = value);
                    }
                    if let Some(mb) = machine.vram_budget_override_mb {
                        if let Some(new) = self.staged_vram_mb.show(
                            ui,
                            "VRAM budget override",
                            f64::from(mb),
                            256.0..=65536.0,
                            64.0,
                            " MB",
                        ) {
                            p.prefs
                                .set_machine(|m| m.vram_budget_override_mb = Some(new as u32));
                        }
                    }
                });
                // Honest labeling (spec G3): persisted now, read by E13.
                ui.label(egui::RichText::new("stored for E13 — not consumed yet").weak());
                ui.end_row();
            });
    }

    // ── G4: caches ──────────────────────────────────────────────────────

    fn cache_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        let cat = p.prefs.catalog();
        ui.strong("Caches");
        egui::Grid::new("prefs-caches")
            .num_columns(3)
            .min_col_width(140.0)
            .show(ui, |ui| {
                ui.label("Preview cache cap");
                if let Some(gib) = self.staged_preview_cap.show(
                    ui,
                    "Preview cache cap",
                    cat.preview_cache_cap_bytes as f64 / GIB,
                    1.0..=500.0,
                    0.5,
                    " GiB",
                ) {
                    let bytes = (gib * GIB) as u64;
                    p.prefs.set_catalog(|c| c.preview_cache_cap_bytes = bytes);
                    // Live half (§6.7 consumers): E03 applies the cap now;
                    // Session::open re-applies it after a restart.
                    p.session
                        .submit(Command::SetCacheLimits(p.prefs.catalog().cache_limits()));
                    self.stats_dirty = true;
                }
                badge(ui, true);
                ui.end_row();

                ui.label("Raw decode cache cap");
                if let Some(gib) = self.staged_raw_cap.show(
                    ui,
                    "Raw decode cache cap",
                    cat.raw_cache_cap_bytes as f64 / GIB,
                    1.0..=500.0,
                    0.5,
                    " GiB",
                ) {
                    let bytes = (gib * GIB) as u64;
                    p.prefs.set_catalog(|c| c.raw_cache_cap_bytes = bytes);
                    p.session
                        .submit(Command::SetCacheLimits(p.prefs.catalog().cache_limits()));
                }
                badge(ui, true);
                ui.end_row();

                ui.label("1:1 (T2) retention");
                ui.horizontal(|ui| {
                    let mut keep_forever = cat.t2_retention_days.is_none();
                    if ui.checkbox(&mut keep_forever, "Never evict").changed() {
                        let value = if keep_forever { None } else { Some(30) };
                        p.prefs.set_catalog(|c| c.t2_retention_days = value);
                    }
                    if let Some(days) = cat.t2_retention_days {
                        if let Some(new) = self.staged_t2_days.show(
                            ui,
                            "1:1 (T2) retention days",
                            f64::from(days),
                            1.0..=3650.0,
                            1.0,
                            " days",
                        ) {
                            p.prefs
                                .set_catalog(|c| c.t2_retention_days = Some(new as u32));
                        }
                    }
                });
                badge(ui, false);
                ui.end_row();

                ui.label("Location");
                ui.horizontal(|ui| {
                    match &cat.cache_dir_override {
                        Some(root) => ui.monospace(root.display().to_string()),
                        None => ui.monospace("<catalog>.lbdata (default)"),
                    };
                    if self.relocate_ticket.is_some() {
                        ui.spinner();
                        ui.weak("relocating…");
                    } else if ui.button("Relocate…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Relocate Cache Store")
                            .pick_folder()
                        {
                            self.relocate_confirm = Some(dir);
                        }
                    }
                });
                ui.label("");
                ui.end_row();
            });

        // Inline relocate confirm (progress + completion arrive as
        // `CacheRelocated`/`CommandFailed` events, `lib.rs` routes them
        // back into this window).
        if let Some(target) = self.relocate_confirm.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("Move the cache store to {}?", target.display()));
                if ui.button("Move").clicked() {
                    let ticket = p.session.submit(Command::RelocateCacheStore {
                        new_root: target.clone(),
                    });
                    self.relocate_ticket = Some(ticket);
                    self.relocate_confirm = None;
                    *p.status = format!("relocating cache store to {}…", target.display());
                }
                if ui.button("Cancel").clicked() {
                    self.relocate_confirm = None;
                }
            });
        }

        // §5.2 fallback badge: the last failed cache op (an invalid
        // relocation target lands here via `CommandFailed`).
        if let Some(err) = &self.cache_error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("last cache operation failed: {err}"),
            );
        }

        // Usage bars (E03 `CacheStats`, the preview-pyramid half; the raw
        // cache reports no stats surface at M1).
        match &self.stats {
            Some(stats) => {
                let used = stats.total_bytes();
                let cap = cat.preview_cache_cap_bytes.max(1);
                let frac = (used as f32 / cap as f32).min(1.0);
                ui.add(egui::ProgressBar::new(frac).text(format!(
                    "previews {} / {} ({} rows)",
                    fmt_bytes(used),
                    fmt_bytes(cat.preview_cache_cap_bytes),
                    stats.total_count(),
                )));
                ui.weak(format!(
                    "T0 {} ({})  ·  T1 {} ({})  ·  T2 {} ({})",
                    stats.t0_count,
                    fmt_bytes(stats.t0_bytes),
                    stats.t1_count,
                    fmt_bytes(stats.t1_bytes),
                    stats.t2_count,
                    fmt_bytes(stats.t2_bytes),
                ));
            }
            None => {
                ui.weak("cache stats unavailable");
            }
        }

        // Purge flows (confirm inline; completion arrives as `CachePurged`).
        ui.horizontal(|ui| {
            if self.purge_ticket.is_some() {
                ui.spinner();
                ui.weak("purging…");
            } else if self.purge_confirm.is_none() {
                if ui.button("Purge previews…").clicked() {
                    self.purge_confirm = Some(PurgeScope::Previews);
                }
                if ui.button("Purge raw cache…").clicked() {
                    self.purge_confirm = Some(PurgeScope::RawCache);
                }
                if ui.button("Purge all…").clicked() {
                    self.purge_confirm = Some(PurgeScope::All);
                }
            }
        });
        if let Some(scope) = self.purge_confirm {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Delete all {} from disk? They rebuild on demand.",
                    purge_scope_label(scope)
                ));
                if ui.button("Purge").clicked() {
                    let ticket = p.session.submit(Command::PurgeCaches(scope));
                    self.purge_ticket = Some(ticket);
                    self.purge_confirm = None;
                    *p.status = format!("purging {}…", purge_scope_label(scope));
                }
                if ui.button("Cancel").clicked() {
                    self.purge_confirm = None;
                }
            });
        }
    }

    // ── G5: jobs ────────────────────────────────────────────────────────

    fn jobs_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        let machine = p.prefs.machine();
        ui.strong("Jobs");
        ui.weak(
            "Worker sizing folds into the job system at startup (live re-sizing is E06's seam).",
        );

        // One row per knob: Auto checkbox + a drag while overridden. The
        // closure returns the new Option only when something committed.
        fn override_row(
            ui: &mut egui::Ui,
            staged: &mut StagedValue,
            label: &str,
            current: Option<usize>,
            default_hint: &str,
            range: std::ops::RangeInclusive<f64>,
        ) -> Option<Option<usize>> {
            let mut out = None;
            ui.label(label);
            ui.horizontal(|ui| {
                let mut auto = current.is_none();
                if ui
                    .checkbox(&mut auto, format!("Auto ({default_hint})"))
                    .changed()
                {
                    out = Some(if auto {
                        None
                    } else {
                        Some(*range.start() as usize)
                    });
                }
                if let Some(n) = current {
                    if let Some(new) = staged.show(ui, label, n as f64, range, 1.0, "") {
                        out = Some(Some(new as usize));
                    }
                }
            });
            badge(ui, false);
            ui.end_row();
            out
        }

        egui::Grid::new("prefs-jobs")
            .num_columns(3)
            .min_col_width(140.0)
            .show(ui, |ui| {
                if let Some(v) = override_row(
                    ui,
                    &mut self.staged_workers,
                    "Worker threads",
                    machine.jobs.worker_threads,
                    "one per core",
                    1.0..=128.0,
                ) {
                    p.prefs.set_machine(|m| m.jobs.worker_threads = v);
                }
                if let Some(v) = override_row(
                    ui,
                    &mut self.staged_interactive,
                    "Interactive jobs",
                    machine.jobs.interactive_slots,
                    "2×cores, min 8",
                    1.0..=128.0,
                ) {
                    p.prefs.set_machine(|m| m.jobs.interactive_slots = v);
                }
                if let Some(v) = override_row(
                    ui,
                    &mut self.staged_foreground,
                    "Foreground jobs",
                    machine.jobs.foreground_slots,
                    "cores/2, min 2",
                    1.0..=64.0,
                ) {
                    p.prefs.set_machine(|m| m.jobs.foreground_slots = v);
                }
                if let Some(v) = override_row(
                    ui,
                    &mut self.staged_background,
                    "Background jobs",
                    machine.jobs.background_slots,
                    "cores/4, min 1",
                    1.0..=64.0,
                ) {
                    p.prefs.set_machine(|m| m.jobs.background_slots = v);
                }
            });

        // The G5 "advanced-collapsed" knob.
        egui::CollapsingHeader::new("Advanced")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("prefs-jobs-advanced")
                    .num_columns(3)
                    .min_col_width(140.0)
                    .show(ui, |ui| {
                        if let Some(v) = override_row(
                            ui,
                            &mut self.staged_event_cap,
                            "Event channel capacity",
                            machine.jobs.event_capacity,
                            "1024",
                            64.0..=65536.0,
                        ) {
                            p.prefs.set_machine(|m| m.jobs.event_capacity = v);
                        }
                    });
            });
    }

    // ── G6: interface / consumers ───────────────────────────────────────

    // ── Appearance: theme + accent (all live, all resettable) ───────────

    /// Everything about how the app *looks*, gathered in one place at the
    /// top of the window the way a macOS app's "Appearance" pane is
    /// theme, accent color, and one "Restore defaults" that puts both
    /// back. Theme moved here out of Interface; Interface keeps the
    /// behavioural knobs (folder drops, filmstrip, file locations).
    fn appearance_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        p: &mut PrefsPanelCtx<'_>,
    ) {
        let machine = p.prefs.machine();
        ui.horizontal(|ui| {
            ui.strong("Appearance");
            if ui
                .button("Restore defaults")
                .on_hover_text("Dark theme, Lightbox Blue accent")
                .clicked()
            {
                let defaults = lightbox_core::MachinePrefs::default();
                p.prefs.set_machine(|m| {
                    m.theme = defaults.theme;
                    m.accent_rgb = defaults.accent_rgb;
                });
                ctx.set_theme(egui_theme_preference(defaults.theme));
                crate::theme::set_accent(ctx, accent_color(&defaults));
            }
        });
        egui::Grid::new("prefs-appearance")
            .num_columns(3)
            .min_col_width(140.0)
            .show(ui, |ui| {
                ui.label("Theme");
                let mut theme = machine.theme;
                egui::ComboBox::from_id_salt("prefs-theme")
                    .selected_text(theme_label(theme))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut theme, Theme::System, theme_label(Theme::System));
                        ui.selectable_value(&mut theme, Theme::Dark, theme_label(Theme::Dark));
                        ui.selectable_value(&mut theme, Theme::Light, theme_label(Theme::Light));
                    });
                if theme != machine.theme {
                    p.prefs.set_machine(|m| m.theme = theme);
                    ctx.set_theme(egui_theme_preference(theme)); // live
                }
                badge(ui, true);
                ui.end_row();

                ui.label("Accent color");
                ui.horizontal(|ui| {
                    let current = accent_color(&machine);
                    for (name, hex) in crate::theme::ACCENT_PRESETS {
                        let color = egui::Color32::from_rgb(
                            ((hex >> 16) & 0xFF) as u8,
                            ((hex >> 8) & 0xFF) as u8,
                            (hex & 0xFF) as u8,
                        );
                        if accent_swatch(ui, name, color, color == current).clicked() {
                            let rgb = [color.r(), color.g(), color.b()];
                            p.prefs.set_machine(|m| m.accent_rgb = Some(rgb));
                            crate::theme::set_accent(ui.ctx(), color); // live
                        }
                    }
                    // …and any other color, for anyone who wants one.
                    let mut custom = [current.r(), current.g(), current.b()];
                    let resp = ui.color_edit_button_srgb(&mut custom);
                    resp.on_hover_text("Custom accent");
                    if custom != [current.r(), current.g(), current.b()] {
                        p.prefs.set_machine(|m| m.accent_rgb = Some(custom));
                        crate::theme::set_accent(
                            ui.ctx(),
                            egui::Color32::from_rgb(custom[0], custom[1], custom[2]),
                        );
                    }
                });
                badge(ui, true);
                ui.end_row();
            });
        ui.weak(
            "Chrome stays neutral grey by design — the accent appears only on \
             selection, focus and active controls, so it never competes with \
             the image.",
        );
    }

    // ── Panels: which develop groups exist, and where ───────────────────

    /// The "what do I want to see" pane: every registered develop panel
    /// with a switch, plus the two layout-wide resets. Hiding a panel does
    /// **not** disturb the dock layout, so switching it back on returns it
    /// to the column it came from.
    fn panels_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        ui.horizontal(|ui| {
            ui.strong("Panels");
            if ui
                .button("Show all")
                .on_hover_text("Switch every develop panel back on")
                .clicked()
            {
                p.panels.show_all();
            }
            if ui
                .button("Reset layout")
                .on_hover_text("One right-hand column, every panel, in the shipped order")
                .clicked()
            {
                p.panels.reset_layout();
            }
        });
        ui.weak(
            "Switch off the panels you never use; drag a panel's header in the rail to move it.",
        );

        let registered = p.panels.registered();
        egui::Grid::new("prefs-panels")
            .num_columns(2)
            .min_col_width(200.0)
            .show(ui, |ui| {
                for (i, (id, title, raw_only)) in registered.iter().enumerate() {
                    let mut shown = !p.panels.is_hidden(*id);
                    let resp = ui.checkbox(&mut shown, *title);
                    let resp = if *raw_only {
                        resp.on_hover_text(
                            "Raw files only — hidden automatically for JPEG/TIFF/PNG",
                        )
                    } else {
                        resp
                    };
                    if resp.changed() {
                        p.panels.set_hidden(*id, !shown);
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        // The panel switches ride the same machine-prefs write as the dock
        // layout itself.
        crate::panels::persist_layout_if_dirty(p.panels, p.prefs);
    }

    // ── G6: interface / consumers ───────────────────────────────────────

    fn interface_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        let machine = p.prefs.machine();
        ui.horizontal(|ui| {
            ui.strong("Interface");
            if ui
                .button("Restore defaults")
                .on_hover_text("Folder-drop and filmstrip settings back to shipped values")
                .clicked()
            {
                let defaults = lightbox_core::MachinePrefs::default();
                p.filmstrip.set_height_pt(defaults.filmstrip_height_pt);
                p.filmstrip.set_collapsed(defaults.filmstrip_collapsed);
                let height = p.filmstrip.height_pt();
                p.prefs.set_machine(|m| {
                    m.drop_recursive_default = defaults.drop_recursive_default;
                    m.filmstrip_height_pt = height;
                    m.filmstrip_collapsed = defaults.filmstrip_collapsed;
                });
            }
        });
        egui::Grid::new("prefs-interface")
            .num_columns(3)
            .min_col_width(140.0)
            .show(ui, |ui| {
                ui.label("Folder drops");
                let mut recursive = machine.drop_recursive_default;
                if ui
                    .checkbox(&mut recursive, "Include subfolders")
                    .on_hover_text("Holding Alt while dropping overrides this per drop (§2.4)")
                    .changed()
                {
                    p.prefs
                        .set_machine(|m| m.drop_recursive_default = recursive);
                }
                badge(ui, true);
                ui.end_row();

                ui.label("Filmstrip height");
                if let Some(new) = self.staged_strip_height.show(
                    ui,
                    "Filmstrip height",
                    f64::from(p.filmstrip.height_pt()),
                    f64::from(MIN_HEIGHT_PT)..=f64::from(MAX_HEIGHT_PT),
                    1.0,
                    " pt",
                ) {
                    p.filmstrip.set_height_pt(new as f32); // live (clamped there)
                    let clamped = p.filmstrip.height_pt();
                    p.prefs.set_machine(|m| m.filmstrip_height_pt = clamped);
                }
                badge(ui, true);
                ui.end_row();

                ui.label("Filmstrip");
                let mut collapsed = p.filmstrip.collapsed();
                if ui.checkbox(&mut collapsed, "Collapsed").changed() {
                    p.filmstrip.set_collapsed(collapsed); // live
                    p.prefs.set_machine(|m| m.filmstrip_collapsed = collapsed);
                }
                badge(ui, true);
                ui.end_row();

                // G6: the keymap-file location readout (+ prefs.toml, same
                // directory by the D0-3 seam).
                ui.label("Keymap file");
                ui.monospace(p.keymap_path.display().to_string());
                ui.label("");
                ui.end_row();

                ui.label("Prefs file");
                ui.monospace(p.prefs.machine_path().display().to_string());
                ui.label("");
                ui.end_row();
            });
    }

    // ── D5 (un-cut): the keyboard rebind editor ─────────────────────────

    fn keyboard_section(&mut self, ui: &mut egui::Ui, p: &mut PrefsPanelCtx<'_>) {
        ui.horizontal(|ui| {
            ui.strong("Keyboard");
            if ui
                .button("Restore defaults")
                .on_hover_text("Clears every rebind (other builds' rows in keymap.toml survive)")
                .clicked()
            {
                p.registry.reset_all();
                self.save_keymap(p);
                self.capture = None;
                self.conflict = None;
            }
        });
        ui.weak("Click a shortcut to rebind: press the new keys, Backspace clears, Esc cancels.");

        let platform = Platform::current();
        for category in crate::keymap::cheatsheet::categories(p.registry) {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(category).italics());
            egui::Grid::new(format!("prefs-keys-{category}"))
                .num_columns(3)
                .min_col_width(160.0)
                .show(ui, |ui| {
                    let rows: Vec<(ActionId, &'static str)> = p
                        .registry
                        .defs()
                        .iter()
                        .filter(|d| d.category == category)
                        .map(|d| (d.id, d.label))
                        .collect();
                    for (id, label) in rows {
                        ui.label(label);
                        let text = if self.capture == Some(id) {
                            "press a key…".to_owned()
                        } else {
                            p.registry
                                .binding(id)
                                .map(|c| c.display(platform))
                                .unwrap_or_else(|| "—".to_owned())
                        };
                        if ui
                            .add(egui::Button::new(egui::RichText::new(text).monospace()))
                            .on_hover_text("Click, then press the new shortcut")
                            .clicked()
                        {
                            self.capture = Some(id);
                            self.conflict = None;
                        }
                        if p.registry.is_default(id) {
                            ui.label("");
                        } else if ui
                            .small_button("↺")
                            .on_hover_text("Reset to default")
                            .clicked()
                        {
                            p.registry.reset(id);
                            self.save_keymap(p);
                        }
                        ui.end_row();
                    }
                });
        }

        // Conflict prompt (D5: steal or cancel).
        if let Some(conflict) = &self.conflict {
            let holders: Vec<&str> = conflict
                .with
                .iter()
                .map(|id| p.registry.def(*id).map(|d| d.label).unwrap_or(id.0))
                .collect();
            let chord = conflict.chord.display(platform);
            let (id, new_chord, with) = (conflict.id, conflict.chord, conflict.with.clone());
            ui.horizontal(|ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("{chord} is already bound to {}.", holders.join(", ")),
                );
                if ui.button("Steal").clicked() {
                    // Unbind the holders (never conflicts), then retry.
                    for holder in &with {
                        let _ = p.registry.rebind(*holder, None);
                    }
                    match p.registry.rebind(id, Some(new_chord)) {
                        Ok(()) => self.save_keymap(p),
                        Err(err) => *p.status = format!("rebind failed: {err}"),
                    }
                    self.conflict = None;
                }
                if ui.button("Cancel").clicked() {
                    self.conflict = None;
                }
            });
        }
    }

    /// While armed, consumes the next key-down as the new chord (Esc
    /// cancels, Backspace/Delete = explicit unbind), mirrors the
    /// dispatcher's event-consumption discipline (`keymap/dispatch.rs`);
    /// `lib.rs` skipped dispatch this frame via [`Self::is_capturing`].
    fn poll_capture(&mut self, ctx: &egui::Context, p: &mut PrefsPanelCtx<'_>) {
        let Some(id) = self.capture else { return };
        let mut captured: Option<Option<Chord>> = None;
        let mut cancelled = false;
        ctx.input_mut(|input| {
            input.events.retain(|ev| {
                if captured.is_some() || cancelled {
                    return true;
                }
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                else {
                    return true;
                };
                match key {
                    egui::Key::Escape => cancelled = true,
                    egui::Key::Backspace | egui::Key::Delete => captured = Some(None),
                    _ => captured = Some(Some(Chord::from_input(*modifiers, *key))),
                }
                false // consumed, never double-handled by a widget
            });
        });
        if cancelled {
            self.capture = None;
            return;
        }
        let Some(chord) = captured else { return };
        self.capture = None;
        match p.registry.rebind(id, chord) {
            Ok(()) => self.save_keymap(p),
            Err(conflict) => {
                self.conflict = Some(PendingConflict {
                    id,
                    chord: conflict.chord,
                    with: conflict.with,
                });
            }
        }
    }

    /// Persists the rebind delta (D3's atomic writer). A failed save is a
    /// status notice, never fatal, the in-memory binding still applies.
    fn save_keymap(&mut self, p: &mut PrefsPanelCtx<'_>) {
        if let Err(err) = p.registry.save_overrides(p.keymap_path) {
            *p.status = format!("keymap.toml save failed: {err}");
        }
    }

    /// G4: poll `cache_stats` on the open panel's cadence (dirty-mark +
    /// ~2 s interval, never per frame, spec §7).
    fn refresh_stats_if_due(&mut self, p: &PrefsPanelCtx<'_>) {
        let due = self.stats_dirty || self.stats_at.is_none_or(|at| at.elapsed() >= STATS_REFRESH);
        if !due {
            return;
        }
        match p.session.query().cache_stats() {
            Ok(stats) => self.stats = Some(stats),
            Err(err) => {
                tracing::warn!(target: "lightbox_shell", %err, "cache_stats query failed");
            }
        }
        self.stats_at = Some(Instant::now());
        self.stats_dirty = false;
    }
}

impl Default for PrefsWindow {
    fn default() -> Self {
        PrefsWindow::new()
    }
}

/// G6: the per-row live-vs-restart badge (the in-panel documentation the
/// spec asks for, not a separate doc). Styled as a real §7 status chip
/// (pill shape, semantic fill, uppercase chip type) instead of a bare
/// colored label.
fn badge(ui: &mut egui::Ui, live: bool) {
    let (label, fill, hover) = if live {
        ("LIVE", tk::STATUS_SUCCESS, "Applies immediately")
    } else {
        (
            "RESTART",
            tk::STATUS_WARNING,
            "Saved now; applies the next time Lightbox starts",
        )
    };
    chip(ui, label, fill).on_hover_text(hover);
}

/// A small status chip (spec §7): pill shape (`RADIUS_CHIP`), `CHIP_HEIGHT`
/// tall, uppercase `chip_font` text in `TEXT_ON_CHIP` on a semantic fill.
fn chip(ui: &mut egui::Ui, label: &str, fill: egui::Color32) -> egui::Response {
    let font = fonts::chip_font();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, tk::TEXT_ON_CHIP);
    let size = egui::vec2(galley.size().x + tk::CHIP_PADDING_X * 2.0, tk::CHIP_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, tk::RADIUS_CHIP, fill);
        ui.painter().galley(
            rect.center() - galley.size() / 2.0,
            galley,
            tk::TEXT_ON_CHIP,
        );
    }
    response
}

/// The accent the app should paint with, given a prefs snapshot:
/// the user's chosen `[r, g, b]`, or the shipped accent when unset.
pub fn accent_color(machine: &lightbox_core::MachinePrefs) -> egui::Color32 {
    match machine.accent_rgb {
        Some([r, g, b]) => egui::Color32::from_rgb(r, g, b),
        None => tk::ACCENT_SHIPPED,
    }
}

/// One accent choice: a filled swatch that reads as selected by a ring,
/// not by a checkmark, the macOS accent-picker idiom. Named for
/// AccessKit, since color alone is not an accessible label.
fn accent_swatch(
    ui: &mut egui::Ui,
    name: &str,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let size = egui::vec2(20.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    {
        let name = name.to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, name.clone())
        });
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let center = rect.center();
        painter.circle_filled(center, 7.0, color);
        if selected {
            painter.circle_stroke(
                center,
                9.0,
                egui::Stroke::new(2.0, crate::theme::Chrome::of(ui.visuals()).text_primary),
            );
        } else if response.hovered() {
            painter.circle_stroke(center, 9.0, egui::Stroke::new(1.0, tk::TEXT_TERTIARY));
        }
    }
    response.on_hover_text(name.to_owned())
}

fn theme_label(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "System",
        Theme::Dark => "Dark",
        Theme::Light => "Light",
    }
}

/// The §6.7 `Theme` → egui mapping (G6: applied live via `set_theme`).
pub fn egui_theme_preference(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
    }
}

fn purge_scope_label(scope: PurgeScope) -> &'static str {
    match scope {
        PurgeScope::Previews => "preview pyramids",
        PurgeScope::RawCache => "raw decode cache entries",
        PurgeScope::All => "previews and raw cache entries",
    }
}

/// Human-readable byte count (binary units, cache caps are GiB knobs).
fn fmt_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KIB * KIB * KIB {
        format!("{:.1} GiB", b / (KIB * KIB * KIB))
    } else if b >= KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}
