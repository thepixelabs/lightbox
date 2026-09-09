// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! H3, the final status bar (spec §8 Phase H): entry counts, epoch load
//! progress, the background-**activity indicator** (spinner + label +
//! cancel for the newest Foreground/Background job observed on the core
//! event bus, the minimal E06/E15 seam), and dismissible **notice chips**
//! (prefs/keymap load reports, degraded GPU).
//!
//! **The E06 seam, stated plainly:** at M1 the core exposes *no*
//! cancel-by-ticket surface for its own jobs (`session.rs`'s
//! `dispatch_build_previews` says so explicitly, "E08's activity center,
//! E06, would be the eventual owner of a cancel-by-handle UI affordance").
//! So the [`ActivityModel`] folds what the bus *does* carry today
//! (working-set opens, bulk preview builds) and carries a real
//! [`CancelToken`] only for activities that have one:
//!
//! * a **working-set load** cancels by *replacing with an empty set* (the
//!   §2.4 replace semantics make that safe, no prompt, edits already
//!   persisted), surfaced to the app as [`StatusAction::CancelOpen`];
//! * a **tracked job** ([`ActivityModel::track_job`]) cancels its token
//!   directly, the affordance E06/E15 will hand real handles to, proven
//!   here against a real `JobSystem` job (see the module tests);
//! * a **bulk preview build** shows progress with *no* cancel button
//!   (honest: no handle exists until E06).
//!
//! Rendering is a free function over borrowed state (same pattern as
//! `empty_state_ui`) so `egui_kittest` drives it without a `Session`/GPU.

use eframe::egui;
use lightbox_core::{Event, SetPhase};
use lightbox_jobs::CancelToken;

use crate::theme::{fonts, paint, tokens};

// ─── Chip & progress painting (spec §7) ─────────────────────────────────────
//
// Every status-bar chip (notice, "Processing…") and the determinate
// activity bar share these primitives; `status_bar_ui` composes them per
// element instead of ever hardcoding a color or a font.

/// Builds the uppercase, letter-tracked chip-text galley (spec §7/§9:
/// Inter SemiBold 10px, UPPERCASE, +0.3px tracking). `LayoutJob` (not
/// `Painter::layout_no_wrap`) is the only entry point that exposes
/// `extra_letter_spacing`.
fn chip_galley(
    painter: &egui::Painter,
    text: &str,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: fonts::chip_font(),
            color,
            extra_letter_spacing: 0.3,
            ..Default::default()
        },
    );
    painter.layout_job(job)
}

/// Paints one status-bar pill chip (spec §7): height `CHIP_HEIGHT`,
/// full-pill `RADIUS_CHIP`, `CHIP_PADDING_X` horizontal padding. `bg` /
/// `text_color` are chosen by the caller per chip kind, see
/// [`notice_chip_colors`] for the semantic/neutral split. A hover-only
/// `Sense` (never focus-taking) carries the AccessKit label so kittest can
/// query the chip by its un-uppercased source text, the same convention
/// `canvas::states::label_region` uses.
fn chip(
    ui: &mut egui::Ui,
    text: &str,
    bg: egui::Color32,
    text_color: egui::Color32,
) -> egui::Response {
    let galley = chip_galley(ui.painter(), text, text_color);
    let size = egui::vec2(
        galley.size().x + tokens::CHIP_PADDING_X * 2.0,
        tokens::CHIP_HEIGHT,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, tokens::RADIUS_CHIP, bg);
    let text_pos = rect.left_center() + egui::vec2(tokens::CHIP_PADDING_X, -galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, text_color);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    response
}

/// Maps a notice's `error` flag to its chip fill/text colors (§7: error →
/// `STATUS_ERROR`/`TEXT_ON_CHIP`; anything else is a plain info notice →
/// neutral `CONTROL_HOVER`/`TEXT_PRIMARY`, no fourth hue invented for
/// plain info).
fn notice_chip_colors(error: bool) -> (egui::Color32, egui::Color32) {
    if error {
        (tokens::STATUS_ERROR, tokens::TEXT_ON_CHIP)
    } else {
        (tokens::CONTROL_HOVER, tokens::TEXT_PRIMARY)
    }
}

/// The §7 activity progress bar's filled-width fraction, clamped to
/// `[0, 1]`, a zero (or corrupt) total reads as "no progress yet" (0.0)
/// rather than dividing by zero.
fn progress_fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// Paints the §7 activity indicator: a determinate `ACTIVITY_BAR_SIZE`
/// (120×3px) bar at the row's current cursor, filled portion uses the
/// slider fill gradient (`fill_gradient_top()/BOTTOM`), unfilled `WELL_BG`.
/// Replaces the old `ui.spinner()`: the spec's hard no-animation-beyond-
/// hover-fades constraint rules out a looping spinner outright.
fn activity_progress_bar(ui: &mut egui::Ui, fraction: f32) {
    let size = tokens::ACTIVITY_BAR_SIZE;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = tokens::pill_radius(size.y);
    ui.painter().rect_filled(rect, radius, tokens::WELL_BG);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let fill =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()));
        paint::vgradient_rounded_rect(
            ui.painter(),
            fill,
            radius,
            tokens::fill_gradient_top(),
            tokens::fill_gradient_bottom(),
        );
    }
}

/// Status-bar body text (§7/§9): Inter Regular 11px, `TEXT_SECONDARY`.
fn status_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(fonts::status_bar_font())
        .color(tokens::TEXT_SECONDARY)
}

// ─── Notices ────────────────────────────────────────────────────────────────

/// One dismissible status-bar notice chip (H3): prefs/keymap load reports,
/// the degraded-GPU notice.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Chip text.
    pub text: String,
    /// Error styling (vs. warning).
    pub error: bool,
}

/// Pushes a notice unless an identical one is already showing (the
/// degraded-GPU event can repeat; one chip is enough).
pub fn push_notice(notices: &mut Vec<Notice>, text: impl Into<String>, error: bool) {
    let text = text.into();
    if !notices.iter().any(|n| n.text == text) {
        notices.push(Notice { text, error });
    }
}

// ─── The activity model ─────────────────────────────────────────────────────

/// What powers the current activity's cancel affordance.
enum ActivityKind {
    /// A working-set open in flight, cancel = replace with an empty set.
    WorkingSetLoad,
    /// A bulk preview build, progress only, no cancel handle at M1.
    PreviewBulk,
    /// A shell-tracked job with a real cancel token (the E06/E15 seam
    /// no M1 core job hands the shell a handle yet; the tests prove the
    /// affordance against a real `JobSystem` job).
    #[allow(dead_code)]
    Job { cancel: CancelToken },
}

/// The newest Foreground/Background activity (spec H3 keeps exactly one
/// the *newest*, on the bar; a full multi-row center is E06's).
pub struct Activity {
    label: String,
    /// `(done, total)` when the source reports progress.
    progress: Option<(u64, u64)>,
    kind: ActivityKind,
}

impl Activity {
    /// The indicator label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// `(done, total)` progress, when known.
    pub fn progress(&self) -> Option<(u64, u64)> {
        self.progress
    }

    /// Whether the bar offers a Cancel button for this activity.
    pub fn cancellable(&self) -> bool {
        !matches!(self.kind, ActivityKind::PreviewBulk)
    }
}

/// Folds core events into the newest-activity slot (newest wins, the H3
/// bar shows one activity; E06's center will show them all).
#[derive(Default)]
pub struct ActivityModel {
    current: Option<Activity>,
}

impl ActivityModel {
    /// Empty model.
    pub fn new() -> ActivityModel {
        ActivityModel { current: None }
    }

    /// The activity to render, if any.
    pub fn current(&self) -> Option<&Activity> {
        self.current.as_ref()
    }

    /// Folds one core event (called from the app's `drain_events`).
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::WorkingSetOpening { .. } => {
                self.current = Some(Activity {
                    label: "Opening…".to_owned(),
                    progress: None,
                    kind: ActivityKind::WorkingSetLoad,
                });
            }
            Event::WorkingSetLoadFinished { .. } => {
                if matches!(
                    self.current,
                    Some(Activity {
                        kind: ActivityKind::WorkingSetLoad,
                        ..
                    })
                ) {
                    self.current = None;
                }
            }
            Event::PreviewBulkProgress { done, total } => {
                if done < total {
                    self.current = Some(Activity {
                        label: format!("Building previews {done}/{total}"),
                        progress: Some((*done, *total)),
                        kind: ActivityKind::PreviewBulk,
                    });
                } else if matches!(
                    self.current,
                    Some(Activity {
                        kind: ActivityKind::PreviewBulk,
                        ..
                    })
                ) {
                    self.current = None;
                }
            }
            _ => {}
        }
    }

    /// Tracks a shell-spawned job with a real cancel token (the E06/E15
    /// seam: export progress and future job handles ride this).
    /// `#[allow(dead_code)]`: no M1 core job exposes a handle to track
    /// see `ActivityKind::Job`'s doc; the module tests are the consumer.
    #[allow(dead_code)]
    pub fn track_job(&mut self, label: impl Into<String>, cancel: CancelToken) {
        self.current = Some(Activity {
            label: label.into(),
            progress: None,
            kind: ActivityKind::Job { cancel },
        });
    }

    /// Clears a tracked job (the caller observed the handle finish).
    #[allow(dead_code)] // E06/E15 seam, tests exercise it; no M1 job to clear yet
    pub fn clear_job(&mut self) {
        if matches!(
            self.current,
            Some(Activity {
                kind: ActivityKind::Job { .. },
                ..
            })
        ) {
            self.current = None;
        }
    }

    /// The Cancel button was pressed. Jobs cancel their token directly
    /// (and stay shown as "cancelling…" until the owner clears them);
    /// a working-set load asks the app to replace with an empty set.
    fn cancel_current(&mut self) -> Option<StatusAction> {
        match &self.current {
            Some(Activity {
                kind: ActivityKind::Job { cancel },
                ..
            }) => {
                cancel.cancel();
                if let Some(a) = &mut self.current {
                    if !a.label.ends_with("— cancelling…") {
                        a.label = format!("{} — cancelling…", a.label);
                    }
                }
                None
            }
            Some(Activity {
                kind: ActivityKind::WorkingSetLoad,
                ..
            }) => Some(StatusAction::CancelOpen),
            _ => None,
        }
    }
}

// ─── Rendering ──────────────────────────────────────────────────────────────

/// What the status bar asks the app to do this frame.
#[derive(Debug, PartialEq)]
pub enum StatusAction {
    /// Cancel the in-flight working-set open (replace with an empty set
    /// safe under §2.4 replace semantics; edits auto-persist).
    CancelOpen,
}

/// Everything the bar renders besides the activity/notice state, borrowed
/// from the app once per frame.
pub struct StatusContext<'a> {
    /// Working-set size.
    pub entries: usize,
    /// Active (loupe) index, if any.
    pub active: Option<usize>,
    /// `OpenOptions::max_set_size` hit.
    pub truncated: bool,
    /// Working-set lifecycle phase (drives the load-progress wording).
    pub phase: SetPhase,
    /// Items past `Planned` (epoch load progress numerator).
    pub loaded: usize,
    /// The free-form status message (open reports, command failures).
    pub message: &'a str,
    /// Right-aligned readout (schema/backend/F1 hint).
    pub right_text: &'a str,
}

/// Renders the H3 status bar. Returns an action when the user cancelled
/// the in-flight open.
pub fn status_bar_ui(
    ui: &mut egui::Ui,
    ctx: &StatusContext<'_>,
    activity: &mut ActivityModel,
    notices: &mut Vec<Notice>,
) -> Option<StatusAction> {
    // §7: 24px bar, `STATUS_BAR_BG`, an engraved seam against the content
    // above, painted at the panel's own top-left regardless of how tall
    // egui's auto-sizing pass reports the still-uncertain available rect
    // as being (the rails/filmstrip floor above reads lighter than this
    // below-floor bar, so the highlight stroke sits on the far side).
    let full = ui.available_rect_before_wrap();
    let bar_rect = egui::Rect::from_min_size(
        full.min,
        egui::vec2(full.width(), tokens::STATUS_BAR_HEIGHT),
    );
    ui.painter()
        .rect_filled(bar_rect, 0.0, tokens::STATUS_BAR_BG);
    paint::engraved_seam_h(
        ui.painter(),
        egui::Rangef::new(bar_rect.left(), bar_rect.right()),
        bar_rect.top(),
        false,
    );
    ui.set_min_height(tokens::STATUS_BAR_HEIGHT);

    let mut action = None;
    ui.horizontal(|ui| {
        ui.add_space(tokens::SPACE_2);

        // ── Entry counts. ────────────────────────────────────────────────
        let active = ctx
            .active
            .map(|i| format!(" · {}/{}", i + 1, ctx.entries))
            .unwrap_or_default();
        let truncated = if ctx.truncated {
            " · truncated (max set size reached)"
        } else {
            ""
        };
        ui.label(status_text(format!(
            "{} images{active}{truncated}",
            ctx.entries
        )));

        // ── Epoch load progress. ─────────────────────────────────────────
        if matches!(ctx.phase, SetPhase::Planning | SetPhase::Loading) {
            ui.separator();
            match ctx.phase {
                // Planning has no known total yet, genuinely
                // indeterminate, so a static chip stands in for the old
                // spinner (spec: no looping animation beyond egui's own
                // hover fades).
                SetPhase::Planning => {
                    chip(
                        ui,
                        "Processing…",
                        tokens::CONTROL_HOVER,
                        tokens::TEXT_PRIMARY,
                    );
                }
                _ => {
                    activity_progress_bar(
                        ui,
                        progress_fraction(ctx.loaded as u64, ctx.entries as u64),
                    );
                }
            }
            let label = match ctx.phase {
                SetPhase::Planning => "opening…".to_owned(),
                _ => format!("opening… {}/{}", ctx.loaded, ctx.entries),
            };
            ui.label(status_text(label));
        }

        // ── Activity indicator (newest Foreground/Background job). ──────
        if let Some(current) = activity.current() {
            ui.separator();
            let mut text = current.label().to_owned();
            match current.progress() {
                Some((done, total)) => {
                    activity_progress_bar(ui, progress_fraction(done, total));
                    if !text.contains('/') {
                        text = format!("{text} ({done}/{total})");
                    }
                }
                // No progress handle, genuinely indeterminate (spec §7:
                // a static "Processing…" chip, not a spinner).
                None => {
                    chip(
                        ui,
                        "Processing…",
                        tokens::CONTROL_HOVER,
                        tokens::TEXT_PRIMARY,
                    );
                }
            }
            ui.label(status_text(text));
            if current.cancellable() && ui.small_button("Cancel").clicked() {
                action = activity.cancel_current();
            }
        }

        // ── Notice chips (dismissible). ──────────────────────────────────
        if !notices.is_empty() {
            ui.separator();
        }
        let mut dismiss: Option<usize> = None;
        for (i, notice) in notices.iter().enumerate() {
            if i > 0 {
                ui.add_space(tokens::SPACE_1);
            }
            let (bg, text_color) = notice_chip_colors(notice.error);
            chip(ui, &notice.text, bg, text_color);
            if ui
                .small_button("✕")
                .on_hover_text("Dismiss this notice")
                .clicked()
            {
                dismiss = Some(i);
            }
        }
        if let Some(i) = dismiss {
            notices.remove(i);
        }

        if !ctx.message.is_empty() {
            ui.separator();
            ui.label(status_text(ctx.message));
        }

        // An EXPLICIT id (`UiBuilder::id`, not `id_salt`/`push_id`) for this
        // scope: without it, egui's auto-id for an un-salted widget is
        // derived from how many siblings were added before it in this `Ui`
        // (`Ui::new_child`'s `unique_id = stable_id.with(next_auto_id_salt)`
        // note `push_id`/`id_salt` only makes the SCOPE's `stable_id`
        // position-independent, not its `unique_id`, so the widget *inside*
        // the scope still inherits the outer position through
        // `next_auto_id_salt`; only an explicit `id` breaks that chain).
        // Every widget above this one (the load-progress readout, the
        // activity indicator, notice chips) is CONDITIONALLY present, so the
        // count preceding this right-anchored label changes frame to frame
        // (e.g. the moment a working-set load finishes and its indicator
        // disappears). The label's on-screen RECT stays put (it's
        // right-aligned), but its auto-id flips, exactly egui's "Widget
        // rect changed id between passes" warning, reproduced live by
        // `cargo run --smoke` and `--drill-edit` (both transition through
        // `SetPhase::Loading` → `Ready`).
        let right_id = ui.id().with("status.right");
        ui.scope_builder(egui::UiBuilder::new().id(right_id), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(status_text(ctx.right_text));
            });
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::NodeT as _, kittest::Queryable, Harness};
    use lightbox_jobs::{Class, JobConfig, JobError, JobSystem};

    struct BarState {
        activity: ActivityModel,
        notices: Vec<Notice>,
        phase: SetPhase,
        action: Option<StatusAction>,
    }

    fn harness(state: BarState) -> Harness<'static, BarState> {
        // This module's chips paint in `chip_font()`, a named weight-role
        // family, see `theme::test_support`'s doc for why a bare kittest
        // harness panics on that without `themed_state`.
        let mut harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, s: &mut BarState| {
                let ctx = StatusContext {
                    entries: 12,
                    active: Some(2),
                    truncated: false,
                    phase: s.phase,
                    loaded: 7,
                    message: "opened 12 in 0.3s",
                    right_text: "schema v6 · Metal · F1 stats",
                };
                if let Some(action) = status_bar_ui(ui, &ctx, &mut s.activity, &mut s.notices) {
                    s.action = Some(action);
                }
            }),
            state,
        );
        harness.set_size(egui::vec2(1100.0, 40.0));
        harness
    }

    /// H3 AC (kittest structural snapshot): counts, load progress, the
    /// activity indicator + cancel, and notice chips are all present.
    #[test]
    fn status_bar_shows_counts_progress_activity_and_notices() {
        let mut activity = ActivityModel::new();
        activity.track_job("Exporting 3 files", CancelToken::new());
        let mut notices = Vec::new();
        push_notice(&mut notices, "keymap.toml: 1 binding(s) skipped", false);
        let mut harness = harness(BarState {
            activity,
            notices,
            phase: SetPhase::Loading,
            action: None,
        });
        // The load spinner repaints continuously, drive single frames
        // (`step`) instead of `run`'s settle-loop.
        harness.step();

        harness.get_by_label("12 images · 3/12");
        harness.get_by_label("opening… 7/12");
        harness.get_by_label("Exporting 3 files");
        harness.get_by_label("Cancel");
        harness.get_by_label_contains("keymap.toml: 1 binding(s) skipped");
        harness.get_by_label_contains("schema v6");
    }

    /// A notice chip's ✕ dismisses exactly that notice.
    #[test]
    fn notice_chips_dismiss() {
        let mut notices = Vec::new();
        push_notice(&mut notices, "prefs.toml unreadable — using defaults", true);
        let mut harness = harness(BarState {
            activity: ActivityModel::new(),
            notices,
            phase: SetPhase::Ready,
            action: None,
        });
        harness.run();
        harness.get_by_label_contains("prefs.toml unreadable");
        harness.get_by_label("✕").click();
        harness.run();
        assert_eq!(
            harness
                .query_all_by_label_contains("prefs.toml unreadable")
                .count(),
            0
        );
        assert!(harness.state().notices.is_empty());
    }

    /// Duplicate notices coalesce (the degraded-GPU event can repeat).
    #[test]
    fn push_notice_dedupes_identical_text() {
        let mut notices = Vec::new();
        push_notice(&mut notices, "GPU device degraded: reset", false);
        push_notice(&mut notices, "GPU device degraded: reset", false);
        assert_eq!(notices.len(), 1);
    }

    /// The event fold: bulk-preview progress shows (progress-labeled, no
    /// cancel, no handle exists at M1) and clears on completion. The
    /// working-set variants carry a crate-private `CommandTicket`/
    /// `OpenReport` and cannot be fabricated here (E08 A0-6 discipline);
    /// their fold is exercised by the live app path and simulated at the
    /// module seam in `cancelling_a_load_returns_the_cancel_open_action`.
    #[test]
    fn activity_model_folds_bulk_preview_events() {
        let mut model = ActivityModel::new();
        assert!(model.current().is_none());

        model.on_event(&Event::PreviewBulkProgress { done: 3, total: 10 });
        let current = model.current().expect("bulk build shows as activity");
        assert_eq!(current.label(), "Building previews 3/10");
        assert_eq!(current.progress(), Some((3, 10)));
        assert!(!current.cancellable(), "no bulk cancel handle until E06");

        model.on_event(&Event::PreviewBulkProgress {
            done: 10,
            total: 10,
        });
        assert!(model.current().is_none(), "completion clears the slot");
    }

    /// The tracked-job seam (E06/E15): track shows, clear removes.
    #[test]
    fn tracked_jobs_show_and_clear() {
        let mut model = ActivityModel::new();
        model.track_job("Synthetic job", CancelToken::new());
        assert_eq!(model.current().unwrap().label(), "Synthetic job");
        assert!(model.current().unwrap().cancellable());
        model.clear_job();
        assert!(model.current().is_none());
    }

    /// H3 AC: **cancel actually cancels**, a real, slow synthetic
    /// Background job on a real `JobSystem`, cancelled by clicking the
    /// status bar's Cancel button, resolves as `JobError::Cancelled`.
    #[test]
    fn status_bar_cancel_cancels_a_real_background_job() {
        let jobs = JobSystem::new(JobConfig::default());
        let cancel = CancelToken::new();
        // A job that never finishes on its own: only cancellation resolves
        // it (JobSystem::spawn races the token against the future).
        let mut handle = jobs.spawn(Class::Background, "synthetic-slow", cancel.clone(), async {
            std::future::pending::<Result<(), JobError>>().await
        });

        let mut activity = ActivityModel::new();
        activity.track_job("Synthetic slow job", cancel);
        let mut harness = harness(BarState {
            activity,
            notices: Vec::new(),
            phase: SetPhase::Ready,
            action: None,
        });
        // The activity spinner repaints continuously, single frames.
        harness.step();
        harness.get_by_label("Cancel").click();
        harness.step();

        // The click cancelled the token; the job must resolve Cancelled.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let result = loop {
            if let Some(result) = handle.try_result() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "job did not resolve after cancel"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(
            matches!(result, Err(JobError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
        // The bar reflects the in-flight cancellation on the next frame
        // (the label paints before the button processes its click).
        harness.step();
        harness.get_by_label_contains("cancelling…");
    }

    /// **Regression (egui "Widget rect changed id between passes"
    /// warning, reproduced live by `cargo run -p lightbox-shell --
    /// --smoke` and `--drill-edit`):** the right-aligned schema/backend
    /// readout must keep the SAME id across a frame where the
    /// load-progress spinner and the activity indicator are present vs.
    /// absent, exactly the `SetPhase::Loading` → `Ready` transition every
    /// working-set open goes through. `accesskit::NodeId` is a direct,
    /// bijective cast of `egui::Id` ([`egui::Id::accesskit_id`]), so
    /// comparing it here is a faithful proxy for the internal id egui's
    /// own pass-to-pass consistency check inspects. Fails before the
    /// `push_id` fix (the label's un-salted auto-id shifts because fewer
    /// widgets precede it in the row once the spinner/activity widgets
    /// disappear); passes after.
    #[test]
    fn right_aligned_readout_keeps_a_stable_id_across_a_variable_prefix() {
        let mut activity = ActivityModel::new();
        activity.track_job("Exporting 3 files", CancelToken::new());
        let mut harness = harness(BarState {
            activity,
            notices: Vec::new(),
            phase: SetPhase::Loading, // extra spinner+label widgets present
            action: None,
        });
        // The load spinner repaints continuously, single frames (`step`),
        // matching this module's other spinner-driving tests.
        harness.step();
        let id_with_extra_widgets = harness
            .get_by_label_contains("schema v")
            .accesskit_node()
            .id();

        // Both conditional groups (load-progress, activity indicator)
        // disappear, mirrors an open finishing while nothing else is
        // running, the exact live-repro transition.
        harness.state_mut().activity.clear_job();
        harness.state_mut().phase = SetPhase::Ready;
        harness.run();
        let id_without_extra_widgets = harness
            .get_by_label_contains("schema v")
            .accesskit_node()
            .id();

        assert_eq!(
            id_with_extra_widgets, id_without_extra_widgets,
            "the right-aligned readout's id must not depend on how many \
             conditional widgets preceded it in the row"
        );
    }

    /// Cancelling a working-set load surfaces `CancelOpen` for the app
    /// (which replaces with an empty set, §2.4 replace semantics).
    #[test]
    fn cancelling_a_load_returns_the_cancel_open_action() {
        let mut model = ActivityModel::new();
        // Simulate what `on_event(WorkingSetOpening)` installs (the real
        // Event is #[non_exhaustive] and cannot be constructed here; the
        // event→activity fold itself is exercised by the live app path).
        model.current = Some(Activity {
            label: "Opening…".to_owned(),
            progress: None,
            kind: ActivityKind::WorkingSetLoad,
        });
        let mut harness = harness(BarState {
            activity: model,
            notices: Vec::new(),
            phase: SetPhase::Ready,
            action: None,
        });
        // The activity spinner repaints continuously, single frames.
        harness.step();
        harness.get_by_label("Cancel").click();
        harness.step();
        assert_eq!(harness.state().action, Some(StatusAction::CancelOpen));
    }

    // ── Chip/token mapping and progress math (pure, no harness needed) ───

    /// §7: error notices get the semantic error chip; anything else is a
    /// plain info notice and gets the neutral chip, no fourth hue.
    #[test]
    fn notice_chip_colors_map_error_and_info_correctly() {
        assert_eq!(
            notice_chip_colors(true),
            (tokens::STATUS_ERROR, tokens::TEXT_ON_CHIP)
        );
        assert_eq!(
            notice_chip_colors(false),
            (tokens::CONTROL_HOVER, tokens::TEXT_PRIMARY)
        );
    }

    #[test]
    fn progress_fraction_clamps_and_avoids_div_by_zero() {
        assert_eq!(progress_fraction(0, 10), 0.0);
        assert_eq!(progress_fraction(5, 10), 0.5);
        assert_eq!(progress_fraction(10, 10), 1.0);
        assert_eq!(progress_fraction(15, 10), 1.0, "overshoot clamps to 1.0");
        assert_eq!(
            progress_fraction(3, 0),
            0.0,
            "zero total never divides by zero"
        );
    }
}
