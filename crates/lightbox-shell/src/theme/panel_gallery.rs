// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Pixel-verification harness for the develop rail's never-yet-reskinned
//! panels (`panels::looks`, `panels::presets`, `panels::history`), the
//! ones the owner flagged as having "black text... hard to read... on the
//! grey background."
//!
//! # The mechanism this proves
//!
//! `panels::host::rail_contents_ui` wraps EVERY panel body (reskinned or
//! not) in `egui::Frame::new().fill(tokens::ELEV_1_PANEL)`, a **dark-only,
//! theme-blind** literal (see that function's source, right next to the
//! `(def.build)(ui, ctx)` call). The reskinned panels (Basic/HSL/Curve/
//! Grading) are immune to a theme flip because their own controls
//! (`panels::widgets::value_slider`) hardcode `.color(tokens::TEXT_PRIMARY)`
//! / `.color(tokens::TEXT_SECONDARY)` directly, they never consult
//! `ctx.style().visuals` for text color. Looks/Presets/History do not: they
//! call plain `ui.label`/`ui.weak`/`RichText::strong()`/`selectable_label`,
//! which resolve through `Style::text_color()` →
//! `Visuals::override_text_color`. When the active theme is Light
//! (`theme::light_visuals`), that override is `tokens::LIGHT_TEXT_PRIMARY`
//! (`#1c1c1c`, near-black), painted onto the panel body's permanently-dark
//! `#292929` background. That pairing is the reported defect.
//!
//! This module renders the REAL panel bodies (via the public `PanelDef`
//! each panel's `def()` returns, `def().build` is a public fn pointer even
//! though the `build` item itself is private to its module) inside the
//! REAL host `Frame`, forces the active theme explicitly per render, and
//! asserts the resulting contrast, proving the defect under Light and the
//! fix's effect under Dark (now the shipped default, `lightbox_core::
//! prefs::Theme::default()`).
//!
//! Does **not** touch `theme::gallery` (owned by a concurrent agent this
//! session), this is a new, parallel, `#[cfg(test)]`-only file, wired in
//! by `theme::mod`'s `#[cfg(test)] pub mod panel_gallery;`.

#![cfg(test)]

use eframe::egui;
use lightbox_core::InstalledLookRow;
use lightbox_edit::{
    CreativeLut, HistoryStepMeta, ParamDelta, ParamId, ParamValue, PresetId, PresetMeta,
    SnapshotMeta, StepLabel,
};
use lightbox_types::{HistoryStepId, SnapshotId};

use crate::canvas::gizmo::GizmoLayer;
use crate::panels::develop_ctx::{DevelopCtx, EditBinding};
use crate::theme::tokens;

/// A stub [`EditBinding`] carrying just enough demo content (a couple of
/// installed looks, presets, and history steps) to render each panel's real
/// list rows, the same "test double populated with a few rows" shape
/// `panels::looks`'/`panels::history`'s own `RecordingBinding` test doubles
/// use, kept minimal since this harness only needs to *paint*, not exercise
/// gesture wiring.
struct StubBinding {
    current_lut: Option<CreativeLut>,
    looks: Vec<InstalledLookRow>,
    presets: Vec<PresetMeta>,
    history: Vec<HistoryStepMeta>,
    snapshots: Vec<SnapshotMeta>,
}

impl StubBinding {
    /// One applied + one not-applied look, two presets (one grouped), one
    /// head history step + one older step, one snapshot, enough for every
    /// list row shape (`selectable_label` checked/unchecked, plain
    /// frameless buttons, group/family headers) to actually paint.
    fn demo() -> StubBinding {
        StubBinding {
            current_lut: Some(CreativeLut {
                id: "hash-applied".to_owned(),
                amount: 100.0,
            }),
            looks: vec![
                InstalledLookRow {
                    id: 1,
                    kind: "cube3d".to_owned(),
                    name: "Kodak Portra".to_owned(),
                    family: Some("Film".to_owned()),
                    rel_path: "ha/hash-applied.cube".to_owned(),
                    content_hash: "hash-applied".to_owned(),
                    source: "user".to_owned(),
                    license: None,
                    installed_at: 0,
                },
                InstalledLookRow {
                    id: 2,
                    kind: "cube3d".to_owned(),
                    name: "Fuji Velvia".to_owned(),
                    family: Some("Film".to_owned()),
                    rel_path: "hb/hash-other.cube".to_owned(),
                    content_hash: "hash-other".to_owned(),
                    source: "user".to_owned(),
                    license: None,
                    installed_at: 0,
                },
            ],
            presets: vec![
                PresetMeta {
                    id: PresetId("preset-1".to_owned()),
                    name: "Moody Portrait".to_owned(),
                    group: Some("Portraits".to_owned()),
                },
                PresetMeta {
                    id: PresetId("preset-2".to_owned()),
                    name: "Clean & Bright".to_owned(),
                    group: None,
                },
            ],
            history: vec![
                HistoryStepMeta {
                    id: HistoryStepId(2),
                    seq: 2,
                    label: StepLabel::Param(ParamId::Exposure),
                    ts: "2026-08-18T00:00:00Z".to_owned(),
                    is_head: true,
                },
                HistoryStepMeta {
                    id: HistoryStepId(1),
                    seq: 1,
                    label: StepLabel::Param(ParamId::WhiteBalance),
                    ts: "2026-08-18T00:00:00Z".to_owned(),
                    is_head: false,
                },
            ],
            snapshots: vec![SnapshotMeta {
                id: SnapshotId(1),
                name: "Before retouch".to_owned(),
                ts: "2026-08-18T00:00:00Z".to_owned(),
            }],
        }
    }
}

impl EditBinding for StubBinding {
    fn value(&self, p: ParamId) -> ParamValue {
        match p {
            ParamId::CreativeLut => ParamValue::Lut(self.current_lut.clone()),
            _ => ParamValue::F32(0.0),
        }
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
        true
    }
    fn can_redo(&self) -> bool {
        false
    }
    fn reset_all(&mut self) {}

    fn undo(&mut self) {}
    fn redo(&mut self) {}
    fn history(&self) -> &[HistoryStepMeta] {
        &self.history
    }
    fn restore_step(&mut self, _seq: u64) {}
    fn clear_history(&mut self) {}
    fn snapshots(&self) -> &[SnapshotMeta] {
        &self.snapshots
    }
    fn create_snapshot(&mut self, _name: &str) {}
    fn restore_snapshot(&mut self, _snapshot: SnapshotId) {}
    fn rename_snapshot(&mut self, _snapshot: SnapshotId, _name: &str) {}

    fn presets(&self) -> &[PresetMeta] {
        &self.presets
    }
    fn installed_looks(&self) -> &[InstalledLookRow] {
        &self.looks
    }
}

/// Mirrors `panels::host::rail_contents_ui`'s exact panel-body wrapper: the
/// same `Frame`, filled with the same dark-only `tokens::ELEV_1_PANEL`
/// literal, every panel body renders inside in the real app, reproduced
/// here (rather than driving the whole `PanelHost::dock_ui`) so this
/// harness stays focused on exactly the mechanism under test, without the
/// header band / disclosure triangle / rail heading `theme::gallery::
/// develop_rail` already covers elsewhere.
fn panel_body_frame() -> egui::Frame {
    let (pad_l, pad_r, pad_t, pad_b) = tokens::PANEL_BODY_PADDING;
    egui::Frame::new()
        .fill(tokens::ELEV_1_PANEL)
        .inner_margin(egui::Margin {
            left: pad_l as i8,
            right: pad_r as i8,
            top: pad_t as i8,
            bottom: pad_b as i8,
        })
}

/// Renders `build` (a real `PanelDef.build` fn pointer) inside the real
/// panel-body `Frame`, with the active theme forced explicitly to `theme`
/// (sidesteps depending on kittest's OS-theme-detection default, the
/// harness proves the mechanism for BOTH themes regardless of what the
/// test machine's own appearance setting happens to be).
struct RenderState {
    binding: StubBinding,
    theme: egui::ThemePreference,
    /// `Context::set_theme` mutates `Options::theme_preference`, but a
    /// `Ui`'s `style: Arc<Style>` field is snapshotted at CONSTRUCTION time
    /// (`Ui::new`'s `style.unwrap_or_else(|| ctx.global_style())`, and
    /// every child `Ui` inherits its parent's already-fixed `style`
    /// `Ui::new_child`'s `style.unwrap_or_else(|| Arc::clone(&self.style))`
    /// neither re-queries `ctx` once created). Since this closure
    /// receives an ALREADY-CONSTRUCTED root `ui`, calling `set_theme` here
    /// cannot affect anything painted THIS frame, only a LATER frame's
    /// freshly-constructed root `Ui` sees it. So: the first real-paint
    /// frame only calls `set_theme` and requests a repaint; the frame
    /// after that is the first one whose root `Ui` was actually built
    /// under the forced theme, and is the one this harness renders.
    theme_set: bool,
    build: fn(&mut egui::Ui, &mut DevelopCtx<'_>),
    /// The panel body `Frame`'s own painted rect (logical points, in this
    /// closure's local `Ui` coordinates, NOT yet offset by kittest's own
    /// 8pt `Frame::central_panel` outer margin), captured so the pixel
    /// scan below can stay strictly inside content this harness actually
    /// painted, rather than also sweeping up the harness's own unpainted
    /// gutter (which renders back as opaque black, darker than any real
    /// glyph ink, see `render_panel`'s doc comment).
    panel_rect: egui::Rect,
}

fn render_panel(
    theme: egui::ThemePreference,
    build: fn(&mut egui::Ui, &mut DevelopCtx<'_>),
    width: f32,
) -> egui_kittest::Harness<'static, RenderState> {
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(width, 420.0))
        .with_pixels_per_point(PPP)
        .wgpu()
        .build_ui_state(
            crate::theme::test_support::themed_state(|ui: &mut egui::Ui, s: &mut RenderState| {
                if !s.theme_set {
                    ui.ctx().set_theme(s.theme);
                    s.theme_set = true;
                    ui.ctx().request_repaint();
                    return;
                }
                let frame_response = panel_body_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    let mut gizmos = GizmoLayer::new();
                    let mut ctx = DevelopCtx {
                        source_kind: lightbox_types::SourceKind::Raw,
                        edit: &mut s.binding,
                        gizmos: &mut gizmos,
                    };
                    (s.build)(ui, &mut ctx);
                });
                s.panel_rect = frame_response.response.rect;
            }),
            RenderState {
                binding: StubBinding::demo(),
                theme,
                theme_set: false,
                build,
                panel_rect: egui::Rect::NOTHING,
            },
        );
    // Frame 0: `test_support::themed_state` installs the theme and paints
    // nothing (its documented contract). Frame 1: `set_theme` is called,
    // but this frame's root `Ui` was already built under the OLD
    // preference (see `theme_set`'s doc comment), so it paints nothing on
    // purpose. Frame 2: the first frame whose root `Ui` was constructed
    // under the forced theme, the one `harness.render()` captures.
    harness.run();
    harness.run();
    harness.run();
    harness
}

/// [`RenderState::panel_rect`] converted to device-pixel bounds and clamped
/// to the rendered image's actual size. `egui::Rect`s are already in
/// absolute window-logical coordinates (not local to whatever `Ui` reports
/// them, `Ui::new`/`Ui::new_child` build every `max_rect` from the SAME
/// coordinate space `ctx.content_rect()` anchors), so `panel_rect` already
/// accounts for kittest's own `Frame::central_panel` 8pt outer margin
/// (`render_panel`'s doc comment) with no further offset needed, only the
/// point→pixel scale. The clamp guards against content taller than the
/// harness's declared viewport (unbounded, unscrolled panel bodies can
/// report a `min_rect` past the visible frame) turning into an
/// out-of-bounds read over the raw pixel buffer.
fn panel_rect_device_px(
    panel_rect: egui::Rect,
    image_w: u32,
    image_h: u32,
) -> (u32, u32, u32, u32) {
    let to_px = |v: f32| (v * PPP).round().clamp(0.0, u32::MAX as f32) as u32;
    (
        to_px(panel_rect.left()),
        to_px(panel_rect.top()),
        to_px(panel_rect.right()).min(image_w),
        to_px(panel_rect.bottom()).min(image_h),
    )
}

/// Where to write rendered panels, if the caller asked for them, same
/// `LIGHTBOX_THEME_GALLERY_OUT` convention `theme::gallery` uses.
fn out_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LIGHTBOX_THEME_GALLERY_OUT").map(std::path::PathBuf::from)
}

/// WCAG 2.x relative luminance of an sRGB color (0..=255 channels).
fn relative_luminance(c: [u8; 3]) -> f64 {
    let chan = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * chan(c[0]) + 0.7152 * chan(c[1]) + 0.0722 * chan(c[2])
}

/// WCAG contrast ratio between two sRGB colors (always >= 1.0).
fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (l1, l2) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// The panel body's actual background color, as an `[r, g, b]` triple
/// not sampled from a pixel (kittest's `run_ui` wraps every `build_ui*`
/// closure in `Frame::central_panel(..).outer_margin(8.0)`, so a naive
/// near-corner probe lands in that 8pt gutter rather than on real content;
/// see `render_panel`'s doc comment) but read straight from the same
/// `tokens::ELEV_1_PANEL` constant [`panel_body_frame`] fills with, this
/// harness reproduces `panels::host::rail_contents_ui`'s exact `Frame` call,
/// so the two are architecturally guaranteed equal, not merely observed to
/// be.
fn panel_bg_rgb() -> [u8; 3] {
    [
        tokens::ELEV_1_PANEL.r(),
        tokens::ELEV_1_PANEL.g(),
        tokens::ELEV_1_PANEL.b(),
    ]
}

/// `pixels_per_point` this harness renders at (matches `render_panel`'s
/// `.with_pixels_per_point(PPP)`).
const PPP: f32 = 2.0;

/// Pixel colors from the flat row-major RGBA buffer (`Harness::render`'s
/// raw bytes), restricted to the device-pixel rect `(x0, y0, x1, y1)`
/// [`panel_rect_device_px`]'s output, i.e. strictly the panel `Frame`'s own
/// painted extent. Deliberately NOT a full-canvas scan: `kittest`'s
/// `run_ui` wraps every `build_ui*` closure in `Frame::central_panel(..)
/// .outer_margin(8.0)` (see `render_panel`'s doc comment), and that
/// harness-owned gutter renders back as **opaque black**
/// (`[0,0,0,255]`, the renderer's clear color, not transparent, so an
/// alpha filter alone cannot exclude it), darker than any real glyph ink,
/// which would otherwise falsely win a "find the darkest pixel" scan.
/// Confining the scan to the Frame's own rect sidesteps that gutter
/// entirely rather than trying to out-guess its exact size. Operates on
/// raw bytes (not a named `image`-crate type) because `lightbox-shell` has
/// no direct dependency on the `image` crate, only `egui_kittest`
/// (dev-only) pulls it in transitively.
fn content_pixels(
    raw: &[u8],
    width: u32,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
) -> impl Iterator<Item = [u8; 3]> + '_ {
    (y0..y1).flat_map(move |y| {
        (x0..x1).map(move |x| {
            let i = ((y * width + x) * 4) as usize;
            [raw[i], raw[i + 1], raw[i + 2]]
        })
    })
}

/// The darkest pixel color among [`content_pixels`] whose per-channel gray
/// value is strictly darker than `below_luma`, used to find "the darkest
/// thing that isn't the panel background" (glyph ink, if any is darker
/// than the background) while still tolerating whatever else the real
/// panel body paints (separators, hairlines, a `TextEdit` well). Since
/// `panels::host`'s panel body is always painted from the single solid
/// `ELEV_1_PANEL` fill, this is equivalent to "find the darkest foreground
/// pixel" without needing to special-case every widget that might paint
/// something dark.
fn darkest_pixel_raw(
    raw: &[u8],
    width: u32,
    bounds: (u32, u32, u32, u32),
    below_luma: u8,
) -> Option<[u8; 3]> {
    let mut best: Option<(u8, [u8; 3])> = None;
    for rgb in content_pixels(raw, width, bounds) {
        let [r, g, b] = rgb;
        let gray = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        if gray >= below_luma {
            continue;
        }
        if best.is_none_or(|(g0, _)| gray < g0) {
            best = Some((gray, rgb));
        }
    }
    best.map(|(_, rgb)| rgb)
}

/// The brightest pixel color among [`content_pixels`], used to find the
/// lightest foreground ink under an active Dark theme.
fn brightest_pixel_raw(raw: &[u8], width: u32, bounds: (u32, u32, u32, u32)) -> Option<[u8; 3]> {
    let mut best: Option<(u16, [u8; 3])> = None;
    for rgb in content_pixels(raw, width, bounds) {
        let [r, g, b] = rgb;
        let gray = (r as u16 + g as u16 + b as u16) / 3;
        if best.is_none_or(|(g0, _)| gray > g0) {
            best = Some((gray, rgb));
        }
    }
    best.map(|(_, rgb)| rgb)
}

/// **The regression proof.** Under an active Light theme, a not-yet-
/// reskinned panel body's stock-widget text resolves through
/// `theme::light_visuals`'s `override_text_color`
/// (`tokens::LIGHT_TEXT_PRIMARY`, `#1c1c1c`) while its background stays the
/// permanently-dark `tokens::ELEV_1_PANEL` (`#292929`,
/// `panels::host::rail_contents_ui`'s hardcoded literal), exactly the
/// owner's "black text... hard to read... on the grey background" report.
/// Contrast target (per the theme spec's WCAG table): body text must clear
/// AA 4.5:1 against `ELEV_1_PANEL`; this asserts the OPPOSITE holds under
/// Light, proving the defect is real and not merely theoretical.
#[test]
fn not_yet_reskinned_panels_fail_contrast_under_light_theme() {
    for (name, build) in [
        ("looks", crate::panels::looks::def().build),
        ("presets", crate::panels::presets::def().build),
        ("history", crate::panels::history::def().build),
    ] {
        let mut harness = render_panel(egui::ThemePreference::Light, build, 280.0);
        let panel_rect = harness.state().panel_rect;
        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[panel_gallery:{name}:light] no wgpu adapter — SKIPPED ({e})");
                continue;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join(format!("panel-{name}-light.png"));
            image.save(&path).expect("write panel png");
            eprintln!("[panel_gallery:{name}:light] wrote {}", path.display());
        }

        let raw = image.as_raw();
        let width = image.width();
        let bounds = panel_rect_device_px(panel_rect, width, image.height());
        let bg = panel_bg_rgb();

        let darkest = darkest_pixel_raw(raw, width, bounds, bg[0])
            .unwrap_or_else(|| panic!("[{name}] found no pixel darker than the panel background — the panel painted no text at all"));
        let ratio = contrast_ratio(darkest, bg);
        assert!(
            ratio < 4.5,
            "[{name}] EXPECTED the Light-theme defect to reproduce (contrast < 4.5:1), \
             but the darkest foreground pixel {darkest:?} against background {bg:?} \
             measured {ratio:.2}:1 — the mechanism this test exists to prove did not occur",
        );
        // Not just low contrast, that darkest pixel should specifically
        // be near `LIGHT_TEXT_PRIMARY`, confirming the mechanism (not some
        // unrelated dark stroke).
        let expect = [
            tokens::LIGHT_TEXT_PRIMARY.r(),
            tokens::LIGHT_TEXT_PRIMARY.g(),
            tokens::LIGHT_TEXT_PRIMARY.b(),
        ];
        let delta: i32 = darkest
            .iter()
            .zip(expect.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .sum();
        assert!(
            delta <= 12,
            "[{name}] darkest pixel {darkest:?} is not close to LIGHT_TEXT_PRIMARY {expect:?} \
             (channel delta sum {delta}) — expected the near-black `override_text_color` leak"
        );
    }
}

/// **The fix's effect.** Under an active Dark theme (the shipped default
/// as of this fix, `lightbox_core::prefs::Theme::default()`), the exact
/// same panel bodies pair `tokens::TEXT_PRIMARY` (`#e2e2e2`) text against
/// the same `ELEV_1_PANEL` (`#292929`) background, the theme spec's own
/// verified-passing combination, so contrast clears WCAG AA 4.5:1.
#[test]
fn not_yet_reskinned_panels_pass_contrast_under_dark_theme() {
    for (name, build) in [
        ("looks", crate::panels::looks::def().build),
        ("presets", crate::panels::presets::def().build),
        ("history", crate::panels::history::def().build),
    ] {
        let mut harness = render_panel(egui::ThemePreference::Dark, build, 280.0);
        let panel_rect = harness.state().panel_rect;
        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[panel_gallery:{name}:dark] no wgpu adapter — SKIPPED ({e})");
                continue;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join(format!("panel-{name}-dark.png"));
            image.save(&path).expect("write panel png");
            eprintln!("[panel_gallery:{name}:dark] wrote {}", path.display());
        }

        let raw = image.as_raw();
        let width = image.width();
        let bounds = panel_rect_device_px(panel_rect, width, image.height());
        let bg = panel_bg_rgb();

        // The brightest pixel (text ink, under Dark theme) against the
        // panel background must clear AA.
        let brightest =
            brightest_pixel_raw(raw, width, bounds).expect("panel painted at least one pixel");
        let ratio = contrast_ratio(brightest, bg);
        assert!(
            ratio >= 4.5,
            "[{name}] Dark-theme body text must clear WCAG AA 4.5:1 against ELEV_1_PANEL — \
             brightest pixel {brightest:?} vs background {bg:?} measured only {ratio:.2}:1"
        );
    }
}
