// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The About window, what a macOS app puts behind "About <App>": who made
//! it, which build this is, and where to find it on the web.
//!
//! Deliberately not a splash screen and not a dialog with an OK button:
//! it is a small floating window at popup elevation (§11), dismissed by
//! its own close button or `Esc`, carrying nothing the user has to act on.
//!
//! Reached from the top bar's "About" button and the `app.about` keymap
//! action (so it is rebindable and shows up in ⌘/ like every other
//! command). The renderer/config readouts are here rather than in
//! Preferences because they are the first things anyone asks for in a bug
//! report, and About is where people look for them.

use eframe::egui;

use crate::theme::{fonts, paint, tokens as tk};

/// The product name as it appears in the window, the bundle, and the
/// wordmark. One definition so they cannot drift.
pub const APP_NAME: &str = "Lightbox";

/// This build's version (`Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The owner's site, linked from the window.
pub const HOMEPAGE: &str = "https://www.pixelabs.net";

/// The copyright line. Kept as a constant so the bundle's `Info.plist`
/// (`NSHumanReadableCopyright`, written by `cargo xtask bundle-mac`) and
/// the window can be checked against each other.
pub const COPYRIGHT: &str = "© 2026 PixeLabs";

/// One line about what the app is, the same framing as the mandate's
/// first sentence, short enough to read at a glance.
const TAGLINE: &str = "Local-first raw photo editor";

/// Where the license text can be read.
pub const LICENSE_URL: &str = "https://www.gnu.org/licenses/agpl-3.0.html";

/// The canonical source repository, named in the attribution term.
pub const SOURCE_URL: &str = "https://github.com/thepixelabs/lightbox";

/// The **Appropriate Legal Notices** the AGPL defines in section 0 and
/// requires of an interactive interface in section 5(d): the copyright
/// notice (above), the absence of warranty, the fact that the work may be
/// conveyed under this license, and how to view it.
///
/// This is not decoration. Section 5(d) says a derivative need only display
/// these notices if the Program itself does, so the section 7(b) attribution
/// term in `COPYING.additional-terms` has nothing to attach to unless this
/// window shows them. It also discharges the notice obligations that travel
/// with the permissive dependencies in the crate graph.
const WARRANTY: &str = "This program comes with ABSOLUTELY NO WARRANTY.\n\
     It is free software, and you are welcome to redistribute it under the \
     terms of the GNU Affero General Public License, version 3 or later, \
     together with the attribution term in COPYING.additional-terms.";

/// Notices that must travel with any binary distribution of this program,
/// because the licenses of components in the crate graph require it.
const THIRD_PARTY: &str = "This software is based in part on the work of the \
     Independent JPEG Group. The bundled Inter and JetBrains Mono typefaces \
     are used under the SIL Open Font License 1.1. Builds configured with the \
     `libraw` feature dynamically link LibRaw (LGPL-2.1) in a separate \
     process.";

/// The About window's state (just whether it is up).
pub struct AboutWindow {
    open: bool,
}

/// The bits of live app state the window reports.
pub struct AboutInfo<'a> {
    /// GPU adapter name, e.g. "Apple M5 Max".
    pub adapter: &'a str,
    /// Graphics backend, e.g. "Metal".
    pub backend: &'a str,
    /// Where `prefs.toml`/`keymap.toml` live.
    pub config_dir: &'a std::path::Path,
    /// The open edit store (`…/edits.lbdata`), if a session is up.
    pub store_dir: &'a std::path::Path,
}

impl AboutWindow {
    /// Starts closed.
    pub fn new() -> AboutWindow {
        AboutWindow { open: false }
    }

    /// The `app.about` action target (and the top bar's button).
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Renders the window (no-op while closed).
    pub fn ui(&mut self, ctx: &egui::Context, info: &AboutInfo<'_>) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let resp = egui::Window::new("About Lightbox")
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(crate::floating_frame(
                tk::ELEV_3_POPUP,
                paint::shadow_popup(),
            ))
            .show(ctx, |ui| self.contents(ui, info));
        crate::finish_floating_window(ctx, &resp);
        self.open = open;
    }

    pub(crate) fn contents(&self, ui: &mut egui::Ui, info: &AboutInfo<'_>) {
        let chrome = crate::theme::Chrome::of(ui.visuals());
        ui.horizontal(|ui| {
            mark(ui, 44.0);
            ui.add_space(tk::SPACE_3);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(APP_NAME)
                        .font(egui::FontId::new(
                            22.0,
                            egui::FontFamily::Name(fonts::FAMILY_INTER_SEMIBOLD.into()),
                        ))
                        .color(chrome.text_primary),
                );
                ui.label(egui::RichText::new(TAGLINE).color(chrome.text_secondary));
                ui.label(
                    egui::RichText::new(format!("Version {VERSION}"))
                        .font(fonts::slider_value_font())
                        .color(tk::TEXT_TERTIARY),
                );
            });
        });

        ui.add_space(tk::SPACE_3);
        ui.separator();
        ui.add_space(tk::SPACE_2);

        egui::Grid::new("about-facts")
            .num_columns(2)
            .min_col_width(96.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Renderer").color(chrome.text_secondary));
                ui.label(
                    egui::RichText::new(format!("{} · {}", info.backend, info.adapter))
                        .font(fonts::slider_value_font()),
                );
                ui.end_row();

                ui.label(egui::RichText::new("Settings").color(chrome.text_secondary));
                path_label(ui, info.config_dir);
                ui.end_row();

                ui.label(egui::RichText::new("Edits").color(chrome.text_secondary));
                path_label(ui, info.store_dir);
                ui.end_row();
            });

        ui.add_space(tk::SPACE_2);
        ui.separator();
        ui.add_space(tk::SPACE_2);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(COPYRIGHT).color(chrome.text_secondary));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.hyperlink_to("pixelabs.net", HOMEPAGE)
                    .on_hover_text(HOMEPAGE);
            });
        });

        ui.add_space(tk::SPACE_2);
        ui.label(
            egui::RichText::new(WARRANTY)
                .color(chrome.text_secondary)
                .font(fonts::filmstrip_meta_font()),
        );
        ui.add_space(tk::SPACE_1);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = tk::SPACE_1;
            ui.hyperlink_to("Read the license", LICENSE_URL)
                .on_hover_text(LICENSE_URL);
            ui.label(egui::RichText::new("·").color(chrome.text_secondary));
            ui.hyperlink_to("Source code", SOURCE_URL)
                .on_hover_text(SOURCE_URL);
        });
        ui.add_space(tk::SPACE_1);
        ui.label(
            egui::RichText::new(THIRD_PARTY)
                .color(chrome.text_secondary)
                .font(fonts::filmstrip_meta_font()),
        );
    }
}

impl Default for AboutWindow {
    fn default() -> Self {
        AboutWindow::new()
    }
}

/// A path that can be long: shown in the mono role, elided in the middle
/// so both the root and the leaf stay readable, full text on hover.
fn path_label(ui: &mut egui::Ui, path: &std::path::Path) {
    let full = path.display().to_string();
    let shown = if full.chars().count() > 34 {
        let tail: String = full
            .chars()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        full.clone()
    };
    ui.label(
        egui::RichText::new(shown)
            .font(fonts::slider_value_font())
            .color(tk::TEXT_TERTIARY),
    )
    .on_hover_text(full);
}

/// The app mark: the same photograph `cargo xtask bundle-mac` rasterizes
/// into the `.icns`, so the About window and the Dock icon show the same
/// picture. See `xtask::icon` for the full description of the scene.
///
/// Sunset sky, sun setting behind two ridges, light broken across water,
/// and the **grade line** across it, flat and cool to its left (the file
/// as it comes off the sensor), graded to its right.
///
/// **Square corners, not the icon's squircle.** egui clips rectangularly,
/// so rounding this would mean either overpainting the corners (which ate
/// the outer 12% of the picture when tried) or hand-tessellating four
/// corner cut-outs, a lot of geometry for a 44px decoration. It is framed
/// as a small print instead: a hairline border, no rounding. The Dock icon
/// keeps the squircle, where it belongs.
///
/// Returns the rect it drew into, so a test can probe the picture without
/// assuming where the layout put it.
pub fn mark(ui: &mut egui::Ui, size: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return rect;
    }
    let painter = ui.painter();
    let s = rect.width();

    // Everything below is in the icon's normalised (u, v) space, so the
    // two implementations can be compared line by line.
    let at = |u: f32, v: f32| egui::pos2(rect.left() + u * s, rect.top() + v * s);
    const HORIZON: f32 = 0.62;
    const SUN: (f32, f32) = (0.655, 0.520);

    // Sky.
    paint::vgradient_rounded_rect(
        painter,
        egui::Rect::from_min_max(rect.min, at(1.0, HORIZON)),
        0.0,
        egui::Color32::from_rgb(0x10, 0x1d, 0x38),
        egui::Color32::from_rgb(0xf0, 0xb2, 0x68),
    );
    // Sun, drawn before the ridges, so they occlude it exactly as in the
    // icon (it is setting behind them, not floating in front).
    painter.circle_filled(
        at(SUN.0, SUN.1),
        s * 0.075,
        egui::Color32::from_rgb(0xff, 0xe6, 0xab),
    );
    // Ridges: the far range sits in the glow, the near one is a silhouette.
    for (points, color) in [
        (
            [
                (0.0, 0.560),
                (0.16, 0.475),
                (0.33, 0.545),
                (0.50, 0.455),
                (0.70, 0.535),
                (1.0, 0.495),
            ]
            .as_slice(),
            egui::Color32::from_rgb(0x54, 0x68, 0x8c),
        ),
        (
            [
                (0.0, 0.560),
                (0.14, 0.505),
                (0.30, 0.585),
                (0.46, 0.520),
                (0.62, 0.598),
                (0.80, 0.545),
                (1.0, 0.575),
            ]
            .as_slice(),
            egui::Color32::from_rgb(0x1b, 0x27, 0x40),
        ),
    ] {
        // One quad per span keeps every shape convex, which is what
        // `convex_polygon` tessellates correctly, a single ridge polygon
        // with its zig-zag top edge is not convex.
        for pair in points.windows(2) {
            let ((u0, v0), (u1, v1)) = (pair[0], pair[1]);
            painter.add(egui::Shape::convex_polygon(
                vec![at(u0, v0), at(u1, v1), at(u1, HORIZON), at(u0, HORIZON)],
                color,
                egui::Stroke::NONE,
            ));
        }
    }
    // Water, and the sun's light broken across it.
    paint::vgradient_rounded_rect(
        painter,
        egui::Rect::from_min_max(at(0.0, HORIZON), rect.max),
        0.0,
        egui::Color32::from_rgb(0x2b, 0x41, 0x66),
        egui::Color32::from_rgb(0x0e, 0x17, 0x28),
    );
    // Three stacked segments, widening and fading, the cheap stand-in for
    // the icon's per-pixel ripple falloff. A single opaque bar reads as a
    // post standing in the water.
    for (top, bottom, half_w, alpha) in [
        (HORIZON, 0.70, 0.032, 0.55),
        (0.70, 0.78, 0.042, 0.34),
        (0.78, 0.86, 0.052, 0.18),
    ] {
        painter.rect_filled(
            egui::Rect::from_min_max(at(SUN.0 - half_w, top), at(SUN.0 + half_w, bottom)),
            0.0,
            egui::Color32::from_rgb(0xe8, 0xa8, 0x60).gamma_multiply(alpha),
        );
    }

    // The grade line, and the flat wedge to its left.
    painter.add(egui::Shape::convex_polygon(
        vec![
            rect.left_top(),
            at(0.255 + 0.5 * 0.16, 0.0),
            at(0.255 - 0.5 * 0.16, 1.0),
            rect.left_bottom(),
        ],
        egui::Color32::from_rgba_unmultiplied(0x7d, 0x84, 0x90, 140),
        egui::Stroke::NONE,
    ));
    painter.line_segment(
        [at(0.255 + 0.5 * 0.16, 0.0), at(0.255 - 0.5 * 0.16, 1.0)],
        egui::Stroke::new((s * 0.02).max(1.0), egui::Color32::from_gray(0xdc)),
    );

    // The print's edge.
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x5c, 0x5c, 0x5c)),
        egui::StrokeKind::Inside,
    );
    rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// Renders [`mark`] through the real wgpu path and checks it is the
    /// same picture the Dock icon is.
    ///
    /// The icon's own geometry is covered by `xtask::icon`'s tests; this
    /// covers the *other* implementation of it, which egui draws with a
    /// different API (gradient bands and polygons instead of a per-pixel
    /// ramp) and which no other test touches. Rendering offscreen rather
    /// than screenshotting the running app also means it still works with
    /// the display asleep, which is exactly where the app-level capture
    /// path stops delivering pixels.
    #[test]
    fn the_about_mark_is_the_icon() {
        const PT: f32 = 96.0;
        const PPP: f32 = 2.0;

        let mut harness = Harness::builder()
            .with_size(egui::vec2(PT + 16.0, PT + 16.0))
            .with_pixels_per_point(PPP)
            .wgpu()
            .build_ui_state(
                crate::theme::test_support::themed_state(|ui, drawn: &mut egui::Rect| {
                    *drawn = mark(ui, PT);
                }),
                egui::Rect::NOTHING,
            );
        harness.run();

        // Where the layout actually put it, never assumed.
        let drawn = *harness.state();
        let image = match harness.render() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[about_mark] no wgpu adapter / render failed — SKIPPED ({e})");
                return;
            }
        };

        let px = |u: f32, v: f32| {
            let p = drawn.min + egui::vec2(u, v) * drawn.width();
            let x = (p.x * PPP)
                .round()
                .clamp(0.0, f32::from(image.width() as u16) - 1.0) as u32;
            let y = (p.y * PPP)
                .round()
                .clamp(0.0, f32::from(image.height() as u16) - 1.0) as u32;
            let c = image.get_pixel(x, y).0;
            (f32::from(c[0]), f32::from(c[1]), f32::from(c[2]))
        };
        // Same convention as `theme::gallery`: write the render for human
        // review when the caller asks for it.
        if let Some(dir) = std::env::var_os("LIGHTBOX_THEME_GALLERY_OUT") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("create gallery out dir");
            image
                .save(dir.join("about-mark.png"))
                .expect("write about mark png");
        }

        let saturation = |(r, g, b): (f32, f32, f32)| {
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            if max <= 0.0 {
                0.0
            } else {
                (max - min) / max
            }
        };

        // The sun, sampled just above the ridge that occludes its centre.
        let sun = px(0.655, 0.480);
        assert!(
            sun.0 > 200.0 && sun.1 > 180.0 && sun.0 > sun.2,
            "expected the warm sun in the mark, got {sun:?}"
        );

        // Sky above it is blue.
        let sky = px(0.80, 0.12);
        assert!(
            sky.2 > sky.0 && sky.2 > sky.1,
            "expected sky blue, got {sky:?}"
        );

        // And the grade line does its job: flatter to its left.
        let before = px(0.08, 0.30);
        let after = px(0.60, 0.30);
        assert!(
            saturation(before) < saturation(after),
            "the ungraded wedge must be less saturated: {before:?} vs {after:?}"
        );
    }
}
