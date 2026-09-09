// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D task **D13**, the develop histogram widget
//! (`develop.histogram`): an RGB-channel overlay histogram plus
//! highlight/shadow clip-indicator triangles, driven by
//! [`lightbox_render::ng::ClipStats`] (D12's `HistogramPass`). Mounted above
//! the Basic panel, `order` 10, `< 20` by design (spec §2.3; see
//! `panels::host`'s own doc on the E10 histogram slot).
//!
//! # Data source
//!
//! [`DevelopCtx::edit`]'s [`EditBinding::histogram`] (`develop_ctx.rs`), the
//! canvas's live, non-blocking `HistogramPass` reduction of the most
//! recently displayed frame, pushed once per frame by `lib.rs` from
//! `EditorCanvas::latest_histogram`. `None` before the first frame renders
//! (a fresh canvas, or no active image) shows the panel's empty state, the
//! panel never blocks or synchronously reads pixels itself.
//!
//! # Clip indicators
//!
//! The two corner triangles mirror the conventional photo-editor layout
//! (top-left = shadow-clip, top-right = highlight-clip): dim/outline while
//! `ClipStats::shadow_fraction`/`highlight_fraction` is `0.0`, lit (blue /
//! red) the instant ANY pixel clips, the same `> 0.0` threshold
//! `nodes::sched::clip_overlay`'s `J`-key canvas pass uses, so the panel and
//! the on-canvas overlay always agree about whether an image is clipping.
//!
//! [`DevelopCtx::edit`]: crate::panels::develop_ctx::DevelopCtx::edit
//! [`EditBinding::histogram`]: crate::panels::develop_ctx::EditBinding::histogram

use eframe::egui;
use lightbox_render::ng::{ClipStats, HistogramData, NUM_BINS};

use crate::panels::develop_ctx::DevelopCtx;
use crate::panels::host::{PanelDef, PanelId, SourceReq};
use crate::theme::{fonts, paint, tokens};

/// The histogram panel's stable id.
pub const PANEL_ID: PanelId = PanelId("develop.histogram");

/// Registration (E1): mounted by `lib.rs` at app construction, above the
/// Basic panel.
pub fn def() -> PanelDef {
    PanelDef {
        id: PANEL_ID,
        title: "Histogram",
        source_req: SourceReq::Any,
        order: 10,
        build,
    }
}

fn build(ui: &mut egui::Ui, ctx: &mut DevelopCtx<'_>) {
    let width = ui.available_width().max(80.0);
    let height = (width * 0.32).clamp(56.0, 130.0);
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());

    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter().with_clip_rect(rect);
    // Recessed well (design spec §1/§4): the histogram reads as an inset
    // instrument set into the panel, not a flat gray box, `WELL_BG` fill
    // now, an inset bevel pair painted last (over the content, like the old
    // frame stroke was), replacing the old flat `extreme_bg_color` rect +
    // plain `noninteractive.bg_stroke` outline.
    painter.rect_filled(rect, tokens::RADIUS_CONTROL, tokens::WELL_BG);

    match ctx.edit.histogram() {
        Some(data) => {
            paint_rgb_overlay(&painter, rect, data);
            paint_clip_triangles(&painter, rect, &data.clip);
        }
        None => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                // §9's typography table has no dedicated "graph placeholder"
                // role; `tooltip_font()` is the closest existing small
                // general-text style (Inter Regular 12px) and matches the
                // previous literal `FontId::proportional(12.0)` exactly.
                fonts::tooltip_font(),
                tokens::TEXT_SECONDARY,
            );
        }
    }
    paint::bevel_inset(&painter, rect, tokens::RADIUS_CONTROL);
}

// ─── RGB overlay ────────────────────────────────────────────────────────────

// These three are **content, not chrome** (design spec §5): the R/G/B
// channel tints are literal photographic data, the same reading Lightroom
// and every other histogram give their channels, not a decorative choice
// subject to the theme's neutral-chrome rule. Do not route them through
// `theme::tokens::ACCENT_*`, and do not "fix" them to be neutral in a
// future theme pass; they are named consts here only so a future editor
// can find and re-tune the exact tint values, not so they can be retired
// in favor of a token.
const R_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 20, 20, 110);
const G_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 90, 30, 110);
const B_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 45, 90, 110);

/// Paints the three channel bar-charts, semi-transparent and overlaid
/// (Lightroom's own RGB-histogram convention: overlapping channels read as
/// blended secondary colors). Normalized against the SHARED max across all
/// three channels, so relative channel heights stay comparable to each
/// other.
fn paint_rgb_overlay(painter: &egui::Painter, rect: egui::Rect, data: &HistogramData) {
    let max = [&data.r, &data.g, &data.b]
        .into_iter()
        .flat_map(|ch| ch.iter().copied())
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    paint_channel(painter, rect, &data.r, max, R_TINT);
    paint_channel(painter, rect, &data.g, max, G_TINT);
    paint_channel(painter, rect, &data.b, max, B_TINT);
}

/// One channel's bars, the same per-bin `rect_filled` technique
/// `panels::curve`'s `paint_histogram_backdrop` uses (never a
/// `convex_polygon` "area" fill: a histogram's top edge is not convex in
/// general, so a fan-triangulated polygon would render wrong, bars are
/// always correct regardless of shape).
fn paint_channel(
    painter: &egui::Painter,
    rect: egui::Rect,
    bins: &[u32; NUM_BINS],
    max: f32,
    color: egui::Color32,
) {
    let n = NUM_BINS as f32;
    let bar_w = rect.width() / n;
    for (i, &v) in bins.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let height_frac = bin_height_fraction(v, max);
        let h = height_frac * rect.height();
        let x0 = rect.left() + i as f32 * bar_w;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x0, rect.bottom() - h),
            egui::pos2(x0 + bar_w, rect.bottom()),
        );
        painter.rect_filled(bar, 0.0, color);
    }
}

/// `sqrt` scaling (a perceptual, Lightroom-ish histogram, not linear): a
/// single stray pixel in an otherwise-empty bin stays visible instead of
/// vanishing next to a saturated peak. Pure + independently tested.
fn bin_height_fraction(count: u32, max: f32) -> f32 {
    if max <= 0.0 {
        return 0.0;
    }
    (count as f32 / max).sqrt().clamp(0.0, 1.0)
}

// ─── Clip-indicator triangles ───────────────────────────────────────────────

// Also content, not chrome: the universal photo-editor clip-warning
// convention (blue = shadow clip, red = highlight clip) predates and is
// independent of this app's accent color, and is a *functional* signal
// (spec §5's "semantic status" exception, alongside warning/error/success)
// rather than a decorative one, so these stay their own literal consts
// rather than being routed through `ACCENT_*`. Neither maps cleanly onto
// `tokens::STATUS_*` either: there is no "info/blue" status token, and
// forcing the highlight-clip red onto the desaturated `STATUS_ERROR` would
// break the red/blue pairing colorists expect from this exact convention.
const SHADOW_LIT: egui::Color32 = egui::Color32::from_rgb(70, 150, 255);
const HIGHLIGHT_LIT: egui::Color32 = egui::Color32::from_rgb(255, 70, 70);

/// A clip fraction lights its corner triangle the instant ANY pixel clips
/// matching `clip_overlay`'s own `> 0` (not a soft/perceptual) threshold, so
/// the panel and the on-canvas `J` overlay never disagree about whether an
/// image is clipping.
fn corner_lit(fraction: f32) -> bool {
    fraction > 0.0
}

fn paint_clip_triangles(painter: &egui::Painter, rect: egui::Rect, clip: &ClipStats) {
    let size = 10.0_f32.min(rect.height() * 0.3).min(rect.width() * 0.15);
    paint_corner_triangle(
        painter,
        rect,
        Corner::TopLeft,
        corner_lit(clip.shadow_fraction()),
        SHADOW_LIT,
        size,
    );
    paint_corner_triangle(
        painter,
        rect,
        Corner::TopRight,
        corner_lit(clip.highlight_fraction()),
        HIGHLIGHT_LIT,
        size,
    );
}

#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
}

fn paint_corner_triangle(
    painter: &egui::Painter,
    rect: egui::Rect,
    corner: Corner,
    lit: bool,
    lit_color: egui::Color32,
    size: f32,
) {
    let color = if lit {
        lit_color
    } else {
        egui::Color32::from_white_alpha(24)
    };
    let points = match corner {
        Corner::TopLeft => vec![
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.left() + size, rect.top()),
            egui::pos2(rect.left(), rect.top() + size),
        ],
        Corner::TopRight => vec![
            egui::pos2(rect.right(), rect.top()),
            egui::pos2(rect.right() - size, rect.top()),
            egui::pos2(rect.right(), rect.top() + size),
        ],
    };
    // A triangle is always convex, so `convex_polygon` is exact here
    // (unlike the channel bars above).
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::EditBinding;
    use egui_kittest::Harness;
    use lightbox_edit::{HistoryStepMeta, ParamDelta, ParamId, ParamValue, SnapshotMeta};
    use lightbox_types::{SnapshotId, SourceKind};

    /// A do-nothing `EditBinding` that optionally carries a fixed
    /// `HistogramData`, the panel's own harness never touches a real
    /// canvas/engine (mirrors every other panel's `StubBinding`, e.g.
    /// `panels::host`'s own test module).
    struct StubBinding {
        histogram: Option<HistogramData>,
    }
    impl EditBinding for StubBinding {
        fn value(&self, _p: ParamId) -> ParamValue {
            ParamValue::F32(0.0)
        }
        fn default(&self, _p: ParamId) -> ParamValue {
            ParamValue::F32(0.0)
        }
        fn begin_gesture(&mut self, _p: ParamId) {}
        fn preview(&mut self, _d: ParamDelta) {}
        fn end_gesture(&mut self) {}
        fn reset(&mut self, _p: ParamId) {}
        fn recipe_rev(&self) -> u64 {
            0
        }
        fn can_undo(&self) -> bool {
            false
        }
        fn can_redo(&self) -> bool {
            false
        }
        fn undo(&mut self) {}
        fn redo(&mut self) {}
        fn history(&self) -> &[HistoryStepMeta] {
            &[]
        }
        fn restore_step(&mut self, _seq: u64) {}
        fn clear_history(&mut self) {}
        fn snapshots(&self) -> &[SnapshotMeta] {
            &[]
        }
        fn create_snapshot(&mut self, _name: &str) {}
        fn restore_snapshot(&mut self, _snapshot: SnapshotId) {}
        fn rename_snapshot(&mut self, _snapshot: SnapshotId, _name: &str) {}
        fn histogram(&self) -> Option<&HistogramData> {
            self.histogram.as_ref()
        }
    }

    fn harness(histogram: Option<HistogramData>) -> Harness<'static, StubBinding> {
        let harness = Harness::new_ui_state(
            crate::theme::test_support::themed_state(|ui, binding: &mut StubBinding| {
                let mut gizmos = GizmoLayer::new();
                let mut ctx = DevelopCtx {
                    source_kind: SourceKind::Raw,
                    edit: binding,
                    gizmos: &mut gizmos,
                };
                build(ui, &mut ctx);
            }),
            StubBinding { histogram },
        );
        // `themed_state` binds the weight-role named font families before
        // frame 0 paints (see `theme::test_support` for why that matters).
        harness
    }

    /// The panel renders (no panic) with NO histogram yet, the pre-first-
    /// frame empty state.
    #[test]
    fn renders_without_panicking_with_no_histogram_yet() {
        let mut h = harness(None);
        h.run();
    }

    /// The panel renders (no panic) with a real, fully-populated histogram,
    /// including its clip triangles.
    #[test]
    fn renders_without_panicking_with_a_clipped_histogram() {
        let mut data = HistogramData::default();
        data.r[255] = 10;
        data.clip.highlight_clipped = 10;
        data.clip.shadow_clipped = 3;
        data.clip.total_pixels = 100;
        let mut h = harness(Some(data));
        h.run();
    }

    // ── Pure logic: exact clip-threshold behavior (the AC's own bar) ───────

    #[test]
    fn corner_lit_is_a_strict_greater_than_zero_threshold() {
        assert!(!corner_lit(0.0));
        assert!(corner_lit(f32::MIN_POSITIVE));
        assert!(corner_lit(1.0));
    }

    /// A synthetic all-black frame -> shadow triangle lit, highlight not.
    #[test]
    fn synthetic_all_black_frame_lights_only_the_shadow_triangle() {
        let clip = ClipStats {
            highlight_clipped: 0,
            shadow_clipped: 64,
            total_pixels: 64,
        };
        assert!(corner_lit(clip.shadow_fraction()));
        assert!(!corner_lit(clip.highlight_fraction()));
    }

    /// A synthetic all-white frame -> highlight triangle lit, shadow not.
    #[test]
    fn synthetic_all_white_frame_lights_only_the_highlight_triangle() {
        let clip = ClipStats {
            highlight_clipped: 64,
            shadow_clipped: 0,
            total_pixels: 64,
        };
        assert!(corner_lit(clip.highlight_fraction()));
        assert!(!corner_lit(clip.shadow_fraction()));
    }

    #[test]
    fn bin_height_fraction_is_sqrt_scaled_and_bounded() {
        assert_eq!(bin_height_fraction(0, 100.0), 0.0);
        assert!((bin_height_fraction(100, 100.0) - 1.0).abs() < 1e-6);
        assert!((bin_height_fraction(25, 100.0) - 0.5).abs() < 1e-6);
        assert_eq!(
            bin_height_fraction(10, 0.0),
            0.0,
            "no div-by-zero on an empty frame"
        );
    }
}
