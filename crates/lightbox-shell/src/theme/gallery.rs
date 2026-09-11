// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! A rendered **specimen sheet** for the theme layer.
//!
//! The token table and the paint helpers are the two things in this crate
//! whose correctness is *visual*: a bevel that reads inverted, a gradient
//! that bands, a glow stack that turns into a hard ring, and a font that
//! silently falls back to tofu are all invisible to `cargo test` but
//! obvious to an eye. This module paints every primitive onto one canvas
//! and renders it through the real wgpu path, so a human (or an agent with
//! image input) can look at the result instead of trusting the constants.
//!
//! It is **test-only**, nothing here ships in the binary.
//!
//! ```sh
//! # Write the sheet to target/theme-gallery/*.png and look at it:
//! LIGHTBOX_THEME_GALLERY_OUT=target/theme-gallery \
//!   cargo test -p lightbox-shell --lib theme::gallery -- --nocapture
//! ```
//!
//! Without that env var the test still renders (proving the paint path and
//! the font stack don't panic and produce a non-blank frame) but writes
//! nothing. GPU legs skip gracefully on machines with no wgpu adapter,
//! mirroring `lightbox-render`'s `device_or_skip` convention.

use eframe::egui::{self, Color32, FontId, Pos2, Rangef, Rect, Stroke, StrokeKind, Vec2};

use crate::theme::{fonts, paint, tokens};

/// Paints one labelled row heading.
fn caption(ui: &egui::Ui, at: Pos2, text: &str) {
    ui.painter().text(
        at,
        egui::Align2::LEFT_TOP,
        text,
        fonts::section_label_font(),
        tokens::TEXT_TERTIARY,
    );
}

/// Paints a small label under a swatch.
fn swatch_label(ui: &egui::Ui, at: Pos2, text: &str) {
    ui.painter().text(
        at,
        egui::Align2::LEFT_TOP,
        text,
        FontId::proportional(9.0),
        tokens::TEXT_SECONDARY,
    );
}

/// The elevation ramp, as adjacent swatches, the eye should read a clean
/// monotonic step from the recessed well up to the modal overlay.
fn elevation_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "ELEVATION");
    let y = origin.y + 16.0;
    let (w, h) = (86.0, 44.0);
    let steps = [
        ("well/canvas", tokens::ELEV_N1_CANVAS),
        ("base", tokens::ELEV_0_BASE),
        ("panel", tokens::ELEV_1_PANEL),
        ("popup", tokens::ELEV_3_POPUP),
        ("overlay", tokens::ELEV_4_OVERLAY),
        ("status bar", tokens::STATUS_BAR_BG),
    ];
    for (i, (name, color)) in steps.iter().enumerate() {
        let x = origin.x + i as f32 * (w + tokens::SPACE_2);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        ui.painter()
            .rect_filled(rect, tokens::RADIUS_CONTROL, *color);
        // Several ramp steps sit at or near the sheet's own background
        // value, so without an edge they read as holes in the row rather
        // than as swatches. The border is review scaffolding, not part of
        // the elevation treatment itself.
        ui.painter().rect_stroke(
            rect,
            tokens::RADIUS_CONTROL,
            Stroke::new(tokens::STROKE_HAIRLINE, tokens::TEXT_TERTIARY),
            StrokeKind::Outside,
        );
        swatch_label(ui, Pos2::new(x, y + h + 3.0), name);
    }
    y + h + 18.0
}

/// The header-band gradient plus the two bevel families, side by side.
/// An inset panel must read as a hole; an outset one as a raised card.
fn depth_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "GRADIENT + BEVEL");
    let y = origin.y + 16.0;
    let h = 44.0;

    // Header-band gradient (the collapsible tool-panel header treatment).
    let grad = Rect::from_min_size(Pos2::new(origin.x, y), Vec2::new(180.0, h));
    paint::vgradient_rounded_rect(
        ui.painter(),
        grad,
        tokens::RADIUS_CONTROL,
        tokens::ELEV_2_HEADER_TOP,
        tokens::ELEV_2_HEADER_BOTTOM,
    );
    swatch_label(
        ui,
        Pos2::new(grad.left(), grad.bottom() + 3.0),
        "header gradient",
    );

    // Raised control: fill + outset bevel.
    let out = Rect::from_min_size(
        Pos2::new(grad.right() + tokens::SPACE_3, y),
        Vec2::new(120.0, h),
    );
    ui.painter()
        .rect_filled(out, tokens::RADIUS_CONTROL, tokens::CONTROL_REST);
    paint::bevel_outset(ui.painter(), out, tokens::RADIUS_CONTROL);
    swatch_label(
        ui,
        Pos2::new(out.left(), out.bottom() + 3.0),
        "outset (raised)",
    );

    // Recessed well: dark fill + inset bevel.
    let ins = Rect::from_min_size(
        Pos2::new(out.right() + tokens::SPACE_3, y),
        Vec2::new(120.0, h),
    );
    ui.painter()
        .rect_filled(ins, tokens::RADIUS_CONTROL, tokens::WELL_BG);
    paint::bevel_inset(ui.painter(), ins, tokens::RADIUS_CONTROL);
    swatch_label(
        ui,
        Pos2::new(ins.left(), ins.bottom() + 3.0),
        "inset (well)",
    );

    // Engraved seam across the remaining width.
    let seam_y = y + h + 26.0;
    paint::engraved_seam_h(
        ui.painter(),
        Rangef::new(origin.x, ins.right()),
        seam_y,
        true,
    );
    swatch_label(ui, Pos2::new(origin.x, seam_y + 4.0), "engraved seam");

    seam_y + 20.0
}

/// The slider anatomy at its three interaction states, drawn exactly as
/// §3 stacks it. This is the app's most-touched control, so it gets the
/// most specimen area.
fn slider_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "SLIDER — REST / HOVER / DRAG");
    let mut y = origin.y + 18.0;
    let track_w = 240.0;

    for (label, glow, knob_r, knob_top, knob_bot, edge) in [
        (
            "rest",
            &[][..],
            tokens::KNOB_RADIUS_REST,
            tokens::KNOB_FILL_REST_TOP,
            tokens::KNOB_FILL_REST_BOTTOM,
            tokens::KNOB_EDGE_STROKE_REST,
        ),
        (
            "hover",
            &tokens::accent_glow_hover()[..],
            tokens::KNOB_RADIUS_HOVER_DRAG,
            tokens::KNOB_FILL_HOVER_DRAG_TOP,
            tokens::KNOB_FILL_HOVER_DRAG_BOTTOM,
            tokens::KNOB_EDGE_STROKE_HOVER_DRAG,
        ),
        (
            "drag",
            &tokens::accent_glow_drag()[..],
            tokens::KNOB_RADIUS_HOVER_DRAG,
            tokens::KNOB_FILL_HOVER_DRAG_TOP,
            tokens::KNOB_FILL_HOVER_DRAG_BOTTOM,
            tokens::KNOB_EDGE_STROKE_HOVER_DRAG,
        ),
    ] {
        let track = Rect::from_min_size(
            Pos2::new(origin.x, y),
            Vec2::new(track_w, tokens::TRACK_HEIGHT),
        );
        // 1-3: groove + inset bevel.
        ui.painter()
            .rect_filled(track, tokens::TRACK_RADIUS, tokens::WELL_BG);
        paint::bevel_inset(ui.painter(), track, tokens::TRACK_RADIUS);
        // 4: fill bar, 62% of the track, with the accent gradient.
        let t = 0.62;
        let fill = Rect::from_min_max(
            track.min,
            Pos2::new(track.left() + t * track_w, track.max.y),
        );
        paint::vgradient_rounded_rect(
            ui.painter(),
            fill,
            tokens::TRACK_RADIUS,
            tokens::fill_gradient_top(),
            tokens::fill_gradient_bottom(),
        );
        // 5: top glare on the fill only.
        paint::hairline_h(
            ui.painter(),
            Rangef::new(fill.left() + 1.0, fill.right() - 1.0),
            fill.top() + 0.5,
            tokens::FILL_TOP_GLARE,
        );

        // Knob stack: glow -> contact shadow -> gradient body -> edge -> highlight.
        let c = Pos2::new(track.left() + t * track_w, track.center().y);
        if !glow.is_empty() {
            paint::glow_circle(ui.painter(), c, knob_r, glow);
        }
        paint::contact_shadow_circle(ui.painter(), c, knob_r);
        paint::vgradient_circle(ui.painter(), c, knob_r, knob_top, knob_bot);
        ui.painter()
            .circle_stroke(c, knob_r, Stroke::new(1.0, edge));

        swatch_label(
            ui,
            Pos2::new(track.right() + tokens::SPACE_3, y - 2.0),
            label,
        );
        y += 40.0;
    }

    // Focus ring specimen, must read crisp, never glowy.
    let track = Rect::from_min_size(
        Pos2::new(origin.x, y),
        Vec2::new(track_w, tokens::TRACK_HEIGHT),
    );
    ui.painter()
        .rect_filled(track, tokens::TRACK_RADIUS, tokens::WELL_BG);
    paint::bevel_inset(ui.painter(), track, tokens::TRACK_RADIUS);
    let c = Pos2::new(track.left() + 0.3 * track_w, track.center().y);
    paint::contact_shadow_circle(ui.painter(), c, tokens::KNOB_RADIUS_REST);
    paint::vgradient_circle(
        ui.painter(),
        c,
        tokens::KNOB_RADIUS_REST,
        tokens::KNOB_FILL_REST_TOP,
        tokens::KNOB_FILL_REST_BOTTOM,
    );
    ui.painter().circle_stroke(
        c,
        tokens::KNOB_RADIUS_REST + 4.0,
        Stroke::new(tokens::STROKE_FOCUS_RING, tokens::accent_focus_ring()),
    );
    swatch_label(
        ui,
        Pos2::new(track.right() + tokens::SPACE_3, y - 2.0),
        "keyboard focus (crisp ring, no glow)",
    );

    y + 30.0
}

/// Status chips and the accent family.
fn chip_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "CHIPS + ACCENT");
    let y = origin.y + 16.0;
    let chip_h = 16.0;

    let mut x = origin.x;
    for (text, bg) in [
        ("WARNING", tokens::STATUS_WARNING),
        ("ERROR", tokens::STATUS_ERROR),
        ("SUCCESS", tokens::STATUS_SUCCESS),
        ("NOTICE", tokens::CONTROL_HOVER),
    ] {
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_owned(), fonts::chip_font(), tokens::TEXT_ON_CHIP);
        let w = galley.size().x + 2.0 * tokens::SPACE_2;
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, chip_h));
        ui.painter()
            .rect_filled(rect, tokens::pill_radius(chip_h), bg);
        let ink = if bg == tokens::CONTROL_HOVER {
            tokens::TEXT_PRIMARY
        } else {
            tokens::TEXT_ON_CHIP
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            fonts::chip_font(),
            ink,
        );
        x += w + tokens::SPACE_2;
    }

    // Accent family swatches.
    let ay = y + chip_h + 14.0;
    for (i, (name, c)) in [
        ("default", tokens::accent()),
        ("hover", tokens::accent_hover()),
        ("active", tokens::accent_active()),
        ("focus ring", tokens::accent_focus_ring()),
    ]
    .iter()
    .enumerate()
    {
        let rect = Rect::from_min_size(
            Pos2::new(origin.x + i as f32 * 92.0, ay),
            Vec2::new(84.0, 26.0),
        );
        ui.painter().rect_filled(rect, tokens::RADIUS_CONTROL, *c);
        // text_on_accent must stay legible on every accent fill (§12).
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            *name,
            FontId::proportional(10.0),
            tokens::text_on_accent(),
        );
    }
    ay + 40.0
}

/// Every typographic role at its real size, so a missing font shows up as
/// tofu here rather than in the shipped UI.
fn type_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "TYPE SPECIMEN");
    let mut y = origin.y + 18.0;
    let specimens: [(&str, FontId, Color32); 8] = [
        (
            "PANEL HEADER",
            fonts::panel_header_font(),
            tokens::TEXT_PRIMARY,
        ),
        (
            "SECTION LABEL",
            fonts::section_label_font(),
            tokens::TEXT_TERTIARY,
        ),
        (
            "Slider label",
            fonts::slider_label_font(),
            tokens::TEXT_SECONDARY,
        ),
        (
            "-1.25 EV  +0.00  5500 K",
            fonts::slider_value_font(),
            tokens::TEXT_PRIMARY,
        ),
        (
            "Body / control text",
            fonts::body_font(),
            tokens::TEXT_PRIMARY,
        ),
        (
            "Status bar text",
            fonts::status_bar_font(),
            tokens::TEXT_SECONDARY,
        ),
        (
            "IMG_0421.CR3",
            fonts::filmstrip_meta_font(),
            tokens::TEXT_SECONDARY,
        ),
        (
            "Drop photos here",
            fonts::empty_state_font(),
            tokens::TEXT_SECONDARY,
        ),
    ];
    for (text, font, color) in specimens {
        ui.painter().text(
            Pos2::new(origin.x, y),
            egui::Align2::LEFT_TOP,
            text,
            font.clone(),
            color,
        );
        y += font.size + 8.0;
    }
    y + 6.0
}

/// Selection language: filmstrip cell tiers (§5), unselected, hover,
/// selected, active-with-glow.
fn selection_row(ui: &egui::Ui, origin: Pos2) -> f32 {
    caption(ui, origin, "SELECTION TIERS");
    let y = origin.y + 16.0;
    let size = Vec2::new(64.0, 48.0);

    for (i, tier) in ["unselected", "hover", "selected", "active"]
        .iter()
        .enumerate()
    {
        let rect = Rect::from_min_size(
            Pos2::new(origin.x + i as f32 * (size.x + tokens::SPACE_3), y),
            size,
        );
        // Active cell gets the persistent restrained glow, drawn first.
        if *tier == "active" {
            paint::glow_rect(
                ui.painter(),
                rect,
                tokens::RADIUS_THUMB,
                &tokens::accent_glow_filmstrip_active(),
            );
        }
        let bg = if *tier == "hover" {
            tokens::ELEV_1_PANEL
        } else {
            tokens::ELEV_0_BASE
        };
        ui.painter().rect_filled(rect, tokens::RADIUS_THUMB, bg);
        // Stand-in for photo content so borders read against something.
        ui.painter().rect_filled(
            rect.shrink(6.0),
            tokens::RADIUS_THUMB,
            Color32::from_gray(0x50),
        );
        let border = match *tier {
            "selected" | "active" => Stroke::new(tokens::STROKE_SELECTION_BORDER, tokens::accent()),
            "hover" => Stroke::new(tokens::STROKE_HAIRLINE, tokens::TEXT_TERTIARY),
            _ => Stroke::new(tokens::STROKE_HAIRLINE, tokens::SEPARATOR_HAIRLINE),
        };
        ui.painter()
            .rect_stroke(rect, tokens::RADIUS_THUMB, border, StrokeKind::Outside);
        swatch_label(ui, Pos2::new(rect.left(), rect.bottom() + 4.0), tier);
    }
    y + size.y + 24.0
}

/// A do-nothing [`crate::panels::develop_ctx::EditBinding`] so the gallery
/// can drive the real rail without a session, store, or GPU engine behind
/// it. Values read back as zero; the rail's *chrome* is what's under
/// review here, not its edit plumbing.
#[cfg(test)]
struct InertBinding;

#[cfg(test)]
impl crate::panels::develop_ctx::EditBinding for InertBinding {
    fn value(&self, _p: lightbox_edit::ParamId) -> lightbox_edit::ParamValue {
        lightbox_edit::ParamValue::F32(0.0)
    }
    fn default(&self, _p: lightbox_edit::ParamId) -> lightbox_edit::ParamValue {
        lightbox_edit::ParamValue::F32(0.0)
    }
    fn begin_gesture(&mut self, _p: lightbox_edit::ParamId) {}
    fn preview(&mut self, _d: lightbox_edit::ParamDelta) {}
    fn end_gesture(&mut self) {}
    fn reset(&mut self, _p: lightbox_edit::ParamId) {}
    fn recipe_rev(&self) -> u64 {
        0
    }
    fn can_undo(&self) -> bool {
        false
    }
    fn can_redo(&self) -> bool {
        false
    }
    fn reset_all(&mut self) {}

    fn undo(&mut self) {}
    fn redo(&mut self) {}
    fn history(&self) -> &[lightbox_edit::HistoryStepMeta] {
        &[]
    }
    fn restore_step(&mut self, _seq: u64) {}
    fn clear_history(&mut self) {}
    fn snapshots(&self) -> &[lightbox_edit::SnapshotMeta] {
        &[]
    }
    fn create_snapshot(&mut self, _name: &str) {}
    fn restore_snapshot(&mut self, _snapshot: lightbox_types::SnapshotId) {}
    fn rename_snapshot(&mut self, _snapshot: lightbox_types::SnapshotId, _name: &str) {}
}

/// The **real** develop rail, genuine [`crate::panels::host::PanelHost`]
/// header bands, disclosure triangles, and panel bodies containing genuine
/// sliders. This is the surface the owner called flat, so it is the one
/// that most needs looking at rather than reasoning about.
#[cfg(test)]
pub fn develop_rail(ui: &mut egui::Ui, host: &mut crate::panels::host::PanelHost) {
    use crate::canvas::gizmo::GizmoLayer;
    use crate::panels::develop_ctx::DevelopCtx;

    let mut binding = InertBinding;
    let mut gizmos = GizmoLayer::new();
    let mut ctx = DevelopCtx {
        source_kind: lightbox_types::SourceKind::Raw,
        edit: &mut binding,
        gizmos: &mut gizmos,
    };
    host.dock_ui(ui, &mut ctx, crate::panels::DockSide::Right);
}

/// Panel bodies for [`develop_rail`], each a stack of real sliders so the
/// rail renders with representative content rather than placeholder text.
#[cfg(test)]
pub fn rail_demo_panels() -> crate::panels::host::PanelHost {
    use crate::panels::host::{PanelDef, PanelHost, PanelId, SourceReq};

    fn tone_body(ui: &mut egui::Ui, _ctx: &mut crate::panels::develop_ctx::DevelopCtx<'_>) {
        slider_rows(
            ui,
            &[
                DemoSlider::bipolar("Exposure", Some("EV"), -5.0, 5.0, 0.05, 1.25),
                DemoSlider::bipolar("Contrast", None, -100.0, 100.0, 1.0, -34.0),
                DemoSlider::bipolar("Highlights", None, -100.0, 100.0, 1.0, -60.0),
                DemoSlider::bipolar("Shadows", None, -100.0, 100.0, 1.0, 25.0),
            ],
        );
    }

    fn presence_body(ui: &mut egui::Ui, _ctx: &mut crate::panels::develop_ctx::DevelopCtx<'_>) {
        slider_rows(
            ui,
            &[
                DemoSlider::bipolar("Texture", None, -100.0, 100.0, 1.0, 12.0),
                DemoSlider::bipolar("Clarity", None, 0.0, 100.0, 1.0, 62.0),
            ],
        );
    }

    let mut host = PanelHost::new();
    host.register(PanelDef {
        id: PanelId("gallery.tone"),
        title: "Basic Tone",
        source_req: SourceReq::Any,
        order: 20,
        build: tone_body,
    });
    host.register(PanelDef {
        id: PanelId("gallery.presence"),
        title: "Presence",
        source_req: SourceReq::Any,
        order: 30,
        build: presence_body,
    });
    host
}

/// How wide a `develop_rail_stays_inside_its_panel` panel body insists on
/// being, whatever the rail hands it. Sized after the real defect that
/// prompted the test: `panels::hsl`'s eight band tabs used to lay out as
/// one 637pt line inside a 456pt rail.
#[cfg(test)]
const OVERSIZED_BODY_PT: f32 = 640.0;

/// The rail's shipped default width (`lib.rs`), mirrored here so the
/// containment test can assert the rail actually holds it.
#[cfg(test)]
const RAIL_DEFAULT_PT: f32 = 280.0;

/// [`rail_demo_panels`] plus one body that demands [`OVERSIZED_BODY_PT`]
/// no matter how narrow the rail is.
///
/// The invariant under test is "an over-wide panel body must be clipped,
/// never allowed to move the rail", so the test supplies its own
/// offender rather than depending on whichever shipped panel happens to
/// overflow this month.
#[cfg(test)]
pub fn rail_demo_panels_with_an_oversized_body() -> crate::panels::host::PanelHost {
    use crate::panels::host::{PanelDef, PanelId, SourceReq};

    fn oversized_body(ui: &mut egui::Ui, _ctx: &mut crate::panels::develop_ctx::DevelopCtx<'_>) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(OVERSIZED_BODY_PT, 20.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, tokens::RADIUS_CONTROL, tokens::CONTROL_REST);
    }

    let mut host = rail_demo_panels();
    host.register(PanelDef {
        id: PanelId("gallery.oversized"),
        title: "Oversized",
        source_req: SourceReq::Any,
        order: 40,
        build: oversized_body,
    });
    host
}

/// One demo row for [`rail_demo_panels`], a `SliderSpec` plus the value
/// to render it at.
#[cfg(test)]
struct DemoSlider {
    label: &'static str,
    unit: Option<&'static str>,
    min: f64,
    max: f64,
    step: f64,
    value: f64,
}

#[cfg(test)]
impl DemoSlider {
    const fn bipolar(
        label: &'static str,
        unit: Option<&'static str>,
        min: f64,
        max: f64,
        step: f64,
        value: f64,
    ) -> Self {
        Self {
            label,
            unit,
            min,
            max,
            step,
            value,
        }
    }
}

/// Shared slider-stack helper for the demo panel bodies.
#[cfg(test)]
fn slider_rows(ui: &mut egui::Ui, rows: &[DemoSlider]) {
    use crate::panels::widgets::{value_slider, SliderSpec};
    for (i, row) in rows.iter().enumerate() {
        let spec = SliderSpec {
            min: row.min,
            max: row.max,
            step: row.step,
            fine: row.step * 0.1,
            label: row.label,
            unit: row.unit,
        };
        // No `add_space` between rows: real develop panels (see
        // `panels/basic.rs`) stack `param_slider` calls back to back and
        // let `ITEM_SPACING.y` do the separating. Adding any here would
        // make this review artifact report a looser pitch than the app
        // actually has.
        let _ = value_slider(ui, ("gallery-rail-slider", row.label, i), row.value, &spec);
    }
}

/// A column of **real** [`crate::panels::widgets::value_slider`] controls,
/// laid out the way a develop panel actually stacks them.
///
/// The rest of this sheet paints primitives directly, which proves the
/// helpers work but not that the shipped control assembles them correctly.
/// This renders the genuine widget, real typography, real spacing, real
/// track and knob, so the thing under review is the thing that ships.
/// The About window's contents, rendered on their own so the **Appropriate
/// Legal Notices** the licence requires (AGPL §5(d)) can be reviewed as
/// pixels rather than reasoned about. If these stop being legible, the
/// section 7(b) attribution term in `COPYING.additional-terms` loses the
/// thing it attaches to.
pub fn about_sheet(ui: &mut egui::Ui) {
    let mut about = crate::about::AboutWindow::new();
    about.toggle();
    let info = crate::about::AboutInfo {
        adapter: "Example GPU",
        backend: "Metal",
        config_dir: std::path::Path::new("/Users/example/Library/Application Support/Lightbox"),
        store_dir: std::path::Path::new(
            "/Users/example/Library/Application Support/Lightbox/edits.lbdata",
        ),
    };
    about.contents(ui, &info);
}

pub fn slider_panel(ui: &mut egui::Ui) {
    use crate::panels::widgets::{value_slider, SliderSpec};

    let bg = ui.available_rect_before_wrap();
    ui.painter().rect_filled(bg, 0.0, tokens::ELEV_1_PANEL);

    // A representative spread: bipolar EV, bipolar signed, unipolar 0-100,
    // and a large-unit temperature, the four range shapes the develop
    // panels actually use.
    let specs = [
        SliderSpec {
            min: -5.0,
            max: 5.0,
            step: 0.05,
            fine: 0.01,
            label: "Exposure",
            unit: Some("EV"),
        },
        SliderSpec {
            min: -100.0,
            max: 100.0,
            step: 1.0,
            fine: 0.1,
            label: "Contrast",
            unit: None,
        },
        SliderSpec {
            min: 0.0,
            max: 100.0,
            step: 1.0,
            fine: 0.1,
            label: "Clarity",
            unit: None,
        },
        SliderSpec {
            min: 2000.0,
            max: 12000.0,
            step: 50.0,
            fine: 10.0,
            label: "Temperature",
            unit: Some("K"),
        },
    ];
    let values = [1.25f64, -34.0, 62.0, 5500.0];

    // Panel-body padding only; rows themselves stack back to back, as a
    // real develop panel does, so the rendered pitch is the app's pitch.
    ui.add_space(tokens::PANEL_BODY_PADDING.2);
    for (i, (spec, value)) in specs.iter().zip(values).enumerate() {
        ui.scope(|ui| {
            ui.set_width(ui.available_width() - 2.0 * tokens::SPACE_3);
            let _ = value_slider(ui, ("gallery-slider", i), value, spec);
        });
    }
}

/// Paints the whole specimen sheet into `ui`.
pub fn sheet(ui: &mut egui::Ui) {
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, 0.0, tokens::ELEV_0_BASE);

    let x = full.left() + tokens::SPACE_5;
    let mut y = full.top() + tokens::SPACE_4;

    y = elevation_row(ui, Pos2::new(x, y)) + tokens::SPACE_4;
    y = depth_row(ui, Pos2::new(x, y)) + tokens::SPACE_4;
    y = slider_row(ui, Pos2::new(x, y)) + tokens::SPACE_2;
    y = chip_row(ui, Pos2::new(x, y)) + tokens::SPACE_2;
    y = selection_row(ui, Pos2::new(x, y)) + tokens::SPACE_2;
    let _ = type_row(ui, Pos2::new(x, y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// Where to write rendered sheets, if the caller asked for them.
    fn out_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("LIGHTBOX_THEME_GALLERY_OUT").map(std::path::PathBuf::from)
    }

    /// Renders the sheet through the real wgpu path and, when
    /// `LIGHTBOX_THEME_GALLERY_OUT` is set, writes it as a PNG for human
    /// review. Asserts only what is honestly machine-checkable: that the
    /// frame renders and is not a uniform blank.
    #[test]
    fn theme_gallery_renders() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(980.0, 900.0))
            .with_pixels_per_point(2.0)
            .wgpu()
            .build_ui(crate::theme::test_support::themed(sheet));
        harness.run();

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[theme_gallery] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        // A blank or single-color frame means the paint path silently did
        // nothing; the sheet has dozens of distinct tones by construction.
        let distinct: std::collections::HashSet<[u8; 4]> = image.pixels().map(|p| p.0).collect();
        assert!(
            distinct.len() > 32,
            "specimen sheet rendered {} distinct colors — paint path is likely a no-op",
            distinct.len()
        );

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("theme-gallery.png");
            image.save(&path).expect("write gallery png");
            eprintln!("[theme_gallery] wrote {}", path.display());
        }
    }

    /// Renders the About window's contents so the licence notices can be
    /// checked for legibility and overflow. See [`super::about_sheet`].
    #[test]
    fn about_window_renders() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(380.0, 560.0))
            .with_pixels_per_point(2.0)
            .wgpu()
            .build_ui(crate::theme::test_support::themed(about_sheet));
        harness.run();

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[about_window] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("about-window.png");
            image.save(&path).expect("write about png");
            eprintln!("[about_window] wrote {}", path.display());
        }
    }

    /// Renders the **real** house slider, not a replica of it, a column
    /// of `value_slider`s as a develop panel stacks them, with a synthetic
    /// cursor parked on the second row so its hover treatment is captured
    /// too. This is the review artifact that actually reflects shipped
    /// code.
    #[test]
    fn slider_panel_renders() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(300.0, 260.0))
            .with_pixels_per_point(2.0)
            .wgpu()
            .build_ui(crate::theme::test_support::themed(slider_panel));
        harness.run();

        // Park the pointer over the second slider's track to capture the
        // hover glow. Coordinates are logical points, matched to the
        // layout above (row pitch = label row + track + SPACE_3).
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(egui::pos2(150.0, 96.0)));
        harness.run();

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[slider_panel] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("slider-panel.png");
            image.save(&path).expect("write slider panel png");
            eprintln!("[slider_panel] wrote {}", path.display());
        }
    }

    /// Renders the **real** develop rail at its shipped width: genuine
    /// header bands, disclosure triangles, solo toggle, and panel bodies
    /// full of genuine sliders. The rail is the surface that prompted this
    /// overhaul, so it gets looked at rather than reasoned about.
    #[test]
    fn develop_rail_renders() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(280.0, 460.0))
            .with_pixels_per_point(2.0)
            .wgpu()
            .build_ui_state(
                crate::theme::test_support::themed_state(
                    |ui, host: &mut crate::panels::host::PanelHost| {
                        // The rail sits on the base floor in the real app;
                        // fill it so the header bands have their true
                        // surround rather than a transparent one.
                        let bg = ui.available_rect_before_wrap();
                        ui.painter().rect_filled(bg, 0.0, tokens::ELEV_0_BASE);
                        develop_rail(ui, host);
                    },
                ),
                rail_demo_panels(),
            );
        harness.run();

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[develop_rail] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("develop-rail.png");
            image.save(&path).expect("write develop rail png");
            eprintln!("[develop_rail] wrote {}", path.display());
        }
    }

    /// Renders the **docked** rails: a left rail, a two-column right rail,
    /// the canvas between them, and a drag in flight so the snap preview
    /// and the cursor ghost are in the picture too. This is the review
    /// artifact for the docking work, the arrangement is otherwise only
    /// reachable by actually dragging panels around.
    #[test]
    fn docked_rails_render() {
        use crate::panels::host::{PanelDef, PanelId, SourceReq};
        use crate::panels::layout::{DockSide, DropTarget};
        use egui_kittest::kittest::Queryable as _;

        fn detail_body(ui: &mut egui::Ui, _ctx: &mut crate::panels::develop_ctx::DevelopCtx<'_>) {
            slider_rows(
                ui,
                &[
                    DemoSlider::bipolar("Sharpen", None, 0.0, 150.0, 1.0, 40.0),
                    DemoSlider::bipolar("Radius", None, 0.5, 3.0, 0.1, 1.0),
                ],
            );
        }

        let ppp = 2.0;
        let mut host = rail_demo_panels();
        host.register(PanelDef {
            id: PanelId("gallery.detail"),
            title: "Detail",
            source_req: SourceReq::Any,
            order: 40,
            build: detail_body,
        });
        // The two moves the drag gesture performs: Tone over to the left
        // rail, Detail into a second right-hand column beside Presence.
        host.move_panel(
            PanelId("gallery.tone"),
            DropTarget::NewColumn {
                side: DockSide::Left,
                at: 0,
            },
        );
        host.move_panel(
            PanelId("gallery.detail"),
            DropTarget::NewColumn {
                side: DockSide::Right,
                at: 1,
            },
        );

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 460.0))
            .with_pixels_per_point(ppp)
            .wgpu()
            .build_ui_state(
                crate::theme::test_support::themed_state(
                    |ui, host: &mut crate::panels::host::PanelHost| {
                        use crate::canvas::gizmo::GizmoLayer;
                        use crate::panels::develop_ctx::DevelopCtx;

                        let full = ui.available_rect_before_wrap();
                        ui.painter().rect_filled(full, 0.0, tokens::ELEV_N1_CANVAS);

                        let mut binding = InertBinding;
                        let mut gizmos = GizmoLayer::new();
                        let mut ctx = DevelopCtx {
                            source_kind: lightbox_types::SourceKind::Raw,
                            edit: &mut binding,
                            gizmos: &mut gizmos,
                        };
                        for side in DockSide::ALL {
                            if !host.has_dock(side, ctx.source_kind) {
                                host.forget_dock(side);
                                continue;
                            }
                            let w = host.dock_width(side, ctx.source_kind, full.width());
                            let rect = match side {
                                DockSide::Left => egui::Rect::from_min_max(
                                    full.min,
                                    Pos2::new(full.left() + w, full.bottom()),
                                ),
                                DockSide::Right => egui::Rect::from_min_max(
                                    Pos2::new(full.right() - w, full.top()),
                                    full.max,
                                ),
                            };
                            ui.painter().rect_filled(rect, 0.0, tokens::ELEV_0_BASE);
                            let mut rail = ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt(("gallery-dock", side.label()))
                                    .max_rect(rect)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            host.dock_ui(&mut rail, &mut ctx, side);
                        }
                        host.finish_dnd(ui.ctx());
                    },
                ),
                host,
            );
        harness.run();

        // Pick "Detail" up and hold the pointer over the left rail's
        // column: one frame carrying the grip, the ghost and the snap
        // insertion bar.
        //
        // **Units.** AccessKit bounds come back through the root node's
        // `pixels_per_point` scale transform (egui `context.rs`
        // `set_transform`), i.e. in *physical* pixels, while
        // `Event::Pointer*` positions are logical points. At ppp 1 the two
        // coincide and nobody notices; at 2 the grab lands nowhere near
        // the header unless it is divided back down.
        let grab = harness.get_by_label("Detail").rect().center() / ppp;
        harness.event(egui::Event::PointerMoved(grab));
        harness.event(egui::Event::PointerButton {
            pos: grab,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.event(egui::Event::PointerMoved(Pos2::new(600.0, 240.0)));
        harness.event(egui::Event::PointerMoved(Pos2::new(150.0, 300.0)));
        harness.run();

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[docked_rails] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("docked-rails.png");
            image.save(&path).expect("write docked rails png");
            eprintln!("[docked_rails] wrote {}", path.display());
        }
    }

    /// Device pixels per point for the containment test's render. Matches
    /// the other gallery renders.
    const CONTAINMENT_PPP: f32 = 2.0;

    /// A colour no theme token uses, so "did the canvas paint over the
    /// rail?" is an exact question with no false positives.
    const CANVAS_MARKER: Color32 = Color32::from_rgb(255, 0, 255);

    /// Anything at or above this luminance inside the rail is a glyph: the
    /// two text tokens sit at 226 (`TEXT_PRIMARY`) and 160
    /// (`TEXT_SECONDARY`), while the brightest rail *background* is the
    /// header gradient's top at 55. A 150 threshold separates them with
    /// room to spare on both sides.
    const GLYPH_LUMA: f32 = 150.0;

    /// How far in from the rail's left edge the "is the left-aligned
    /// content actually on screen?" probe looks. A header's disclosure
    /// triangle sits 12pt into its column and its title starts 26pt in,
    /// and a slider's name label starts 12pt in; the column itself starts
    /// one splitter width inside the rail. 40pt past that catches all
    /// three without reaching the right-aligned values that survived the
    /// bug.
    const LEFT_COLUMN_PT: f32 = 40.0 + crate::panels::host::SPLITTER_PT;

    /// What the containment test observes about one frame of the real
    /// `Panel::right` + canvas composition.
    struct RailInPanel {
        host: crate::panels::host::PanelHost,
        /// The rect `Panel::right` *reported* for the rail.
        rail_rect: Rect,
        /// What the panel left for the canvas painted after it.
        canvas_rect: Rect,
        /// The whole composition, so "hanging off the window" is testable.
        root_rect: Rect,
    }

    /// One rendered pixel as RGBA, from the flat row-major buffer
    /// `Harness::render` hands back. Taken as raw bytes so the test needs
    /// no direct dependency on the `image` crate.
    fn pixel(raw: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]
    }

    /// Relative luminance of a rendered pixel, for the glyph probe.
    fn luma(px: [u8; 4]) -> f32 {
        0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32
    }

    /// Rows (device pixels) where the rail is painting a collapsible
    /// panel's header band, found by sampling just inside the rail's left
    /// edge. The bands are the one place in the rail painted with the
    /// header gradient (`ELEV_2_HEADER_TOP` 0x37 → `ELEV_2_HEADER_BOTTOM`
    /// 0x2e); the surfaces above and below it, `ELEV_0_BASE` 0x1e for the
    /// rail floor, `ELEV_1_PANEL` 0x29 for a panel body, sit clear of
    /// that range. Locating the bands from the render instead of hardcoded
    /// y offsets is what keeps this assertion from breaking on a 1px
    /// layout shift.
    ///
    /// Returns one inclusive `(top, bottom)` row range per contiguous run.
    fn header_band_rows(raw: &[u8], width: u32, height: u32, rail: Rect) -> Vec<(u32, u32)> {
        // Probe just inside the first COLUMN, not the rail: a rail's
        // canvas-facing edge is now its resize splitter, painted in the
        // rail floor, so a probe at the rail's own left edge would sample
        // the seam forever and find no bands at all.
        let probe_x = ((rail.left() + crate::panels::host::SPLITTER_PT + 2.0) * CONTAINMENT_PPP)
            .round() as u32;
        let mut bands: Vec<(u32, u32)> = Vec::new();
        for y in 0..height {
            let v = pixel(raw, width, probe_x, y)[0];
            // The gradient's own two ends plus the dither between them.
            let in_band = (0x2c..=0x39).contains(&v);
            match (in_band, bands.last_mut()) {
                (true, Some(last)) if last.1 + 1 == y => last.1 = y,
                (true, _) => bands.push((y, y)),
                (false, _) => {}
            }
        }
        // Drop runs too thin to be a 24pt band, the engraved seams under
        // the heading row and each band land in the same value window.
        bands.retain(|(top, bottom)| bottom - top > (CONTAINMENT_PPP * 8.0) as u32);
        bands
    }

    /// **The regression test for the bug that erased every left-aligned
    /// label in the develop rail.**
    ///
    /// The rail renders perfectly in isolation, `develop_rail_renders`
    /// above proves that, and it proved it all through the outage. The
    /// defect only existed in the *composition*: a panel body wider than
    /// the rail pushed its width demand out through the `ScrollArea` into
    /// `Panel::right`, which re-anchored the panel's reported rect to the
    /// right of where it had painted, whereupon the canvas, shown last
    /// claimed the difference and painted over the rail's left-hand
    /// column. See `panels::host`'s "width containment" note for the full
    /// chain.
    ///
    /// So this test reproduces the composition (a right panel with the
    /// shipped size range, then a canvas filled over whatever is left) and
    /// asserts both halves of the invariant:
    ///
    /// * **geometry**, the rail holds its default width and its reported
    ///   rect neither hangs off the window nor overlaps the canvas;
    /// * **pixels**, every header band still has glyph-bright pixels in
    ///   its left column. AccessKit coverage cannot see this: a node named
    ///   "Exposure" exists whether or not its glyphs reach the screen.
    #[test]
    fn develop_rail_stays_inside_its_panel() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 520.0))
            .with_pixels_per_point(CONTAINMENT_PPP)
            .wgpu()
            .build_ui_state(
                crate::theme::test_support::themed_state(|ui, s: &mut RailInPanel| {
                    s.root_rect = ui.available_rect_before_wrap();
                    let host = &mut s.host;
                    let rail = egui::Panel::right(egui::Id::new("gallery-rail-containment"))
                        .resizable(true)
                        .default_size(RAIL_DEFAULT_PT)
                        .size_range(240.0..=480.0)
                        .frame(
                            egui::Frame::new()
                                .fill(tokens::ELEV_0_BASE)
                                .inner_margin(egui::Margin::symmetric(0, tokens::SPACE_2 as i8)),
                        )
                        .show(ui, |ui| develop_rail(ui, host));
                    s.rail_rect = rail.response.rect;
                    // The canvas paints LAST over whatever the panel left
                    // behind, the `CentralPanel`-after-`Panel::right`
                    // ordering from `lib.rs`, and precisely the half an
                    // isolated rail harness never exercises.
                    s.canvas_rect = ui.available_rect_before_wrap();
                    ui.painter().rect_filled(s.canvas_rect, 0.0, CANVAS_MARKER);
                }),
                RailInPanel {
                    host: rail_demo_panels_with_an_oversized_body(),
                    rail_rect: Rect::NOTHING,
                    canvas_rect: Rect::NOTHING,
                    root_rect: Rect::NOTHING,
                },
            );
        // Frame 0 installs the theme. The rest give the panel every chance
        // to drift: the shipped bug pinned it at `size_range.max` by frame
        // 2 and it stayed there, so a stable width here is a real signal.
        for _ in 0..12 {
            harness.run();
        }

        let RailInPanel {
            rail_rect,
            canvas_rect,
            root_rect,
            ..
        } = *harness.state();

        assert!(
            (rail_rect.width() - RAIL_DEFAULT_PT).abs() < 1.0,
            "the rail drifted off its {RAIL_DEFAULT_PT}pt default to {}pt \
             ({rail_rect:?}) — an over-wide panel body is ratcheting the \
             panel toward its size_range maximum",
            rail_rect.width(),
        );
        assert!(
            rail_rect.right() <= root_rect.right() + 0.5,
            "the rail reported a rect running {}pt past the right of the \
             window ({rail_rect:?} vs {root_rect:?}) — its reported rect \
             has been re-anchored away from where it painted",
            rail_rect.right() - root_rect.right(),
        );
        assert!(
            canvas_rect.right() <= rail_rect.left() + 0.5,
            "the canvas overlaps the rail by {}pt ({canvas_rect:?} vs \
             {rail_rect:?}) — the canvas paints last, so that overlap is \
             the rail's left-hand column being erased",
            canvas_rect.right() - rail_rect.left(),
        );

        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[rail_containment] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        if let Some(dir) = out_dir() {
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            let path = dir.join("develop-rail-in-panel.png");
            image.save(&path).expect("write rail containment png");
            eprintln!("[rail_containment] wrote {}", path.display());
        }

        // Nothing may paint the canvas colour inside the rail's rect.
        let (width, height) = (image.width(), image.height());
        let raw = image.as_raw();
        let px = |v: f32| (v * CONTAINMENT_PPP).round() as u32;
        let (x0, x1) = (px(rail_rect.left()), px(rail_rect.right()).min(width));
        let (y0, y1) = (px(rail_rect.top()), px(rail_rect.bottom()).min(height));
        for y in y0..y1 {
            for x in x0..x1 {
                assert_ne!(
                    pixel(raw, width, x, y),
                    [CANVAS_MARKER.r(), CANVAS_MARKER.g(), CANVAS_MARKER.b(), 255],
                    "the canvas painted over the rail at device pixel ({x}, {y})",
                );
            }
        }

        // Every header band must still have glyphs in its left column.
        let bands = header_band_rows(raw, width, height, rail_rect);
        assert!(
            bands.len() >= 3,
            "found {} header bands in the render, expected the 3 registered \
             panels — the bands themselves are missing, so the glyph probe \
             below would be vacuous",
            bands.len(),
        );
        let column_right = px(rail_rect.left() + LEFT_COLUMN_PT).min(width);
        for (top, bottom) in bands {
            let lit = (top..=bottom)
                .flat_map(|y| (x0..column_right).map(move |x| (x, y)))
                .filter(|&(x, y)| luma(pixel(raw, width, x, y)) >= GLYPH_LUMA)
                .count();
            assert!(
                lit > 20,
                "header band at device rows {top}..={bottom} has {lit} glyph-bright \
                 pixels in its leftmost {LEFT_COLUMN_PT}pt — its disclosure triangle \
                 and title are not reaching the screen",
            );
        }
    }
}
