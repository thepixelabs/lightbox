// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Lightbox's neutral-dark editor theme.
//!
//! A color editor must present a **neutral** (untinted) dark-gray surround:
//! tinted or colored UI chrome measurably biases how the eye reads the
//! photo's colors, the reason Lightroom, Capture One, and every serious
//! editor use a muted neutral gray. Every gray token in [`tokens`] has
//! `r == g == b`; the one accent (`accent()` family) exists only for
//! selection/active/focus, and stays silent at rest even there, no glow,
//! no ring, no tint on a control that isn't selected, hovered, or being
//! interacted with. See [`tokens`]'s module doc comment for the full
//! may/may-not-appear rule.
//!
//! ## The depth system (replaces the old single-ramp theme)
//!
//! Where the pre-overhaul theme used one flat gray per role (panel, window,
//! extreme), this one is a six-level **elevation** ramp
//! (`ELEV_N1_CANVAS` … `ELEV_4_OVERLAY`, see `tokens`'s "Elevation /
//! surfaces" group): lightness increases with elevation, because a
//! floating popup catches more ambient light than the floor it hovers
//! over, and a recessed well is the darkest thing in the window short of
//! the canvas itself. Every recessed surface gets the same two-stroke
//! *inset* bevel, every raised surface the same two-stroke *outset* bevel
//! ([`paint::bevel_inset`] / [`paint::bevel_outset`]), richness comes from
//! depth and gradient, never from hue, and large flat expanses (rails,
//! canvas surround) stay flat to avoid banding and keep the eye off the
//! chrome and on the photo.
//!
//! This module (`mod.rs`) wires the token layer into `egui::Visuals` for
//! the *stock* widget dispatch (buttons, combo boxes, popups, …) that
//! hasn't been individually re-skinned yet. It intentionally cannot
//! reproduce the two-stroke bevel or the multi-layer glow through
//! `Visuals` alone, `WidgetVisuals` only has one `bg_stroke` slot, so a
//! per-widget custom `paint()` calling into [`paint`] is what actually
//! ships the full spec treatment (house slider knob glow, filmstrip
//! selection glow, panel header gradient, …). Consider the `Visuals` below
//! the "sane, on-palette default" every not-yet-reskinned control gets for
//! free, not the final word.
//!
//! `egui::Widgets` also has no fourth "disabled" `WidgetVisuals` slot
//! disabled state is opacity-driven (`Ui::disable()` / `Context::disabled`
//! machinery), not a color swap. [`tokens::CONTROL_DISABLED`] and
//! [`tokens::TEXT_DISABLED`] exist for call sites that hand-paint a
//! disabled-looking control directly, not for `Visuals` itself.

pub mod fonts;
pub mod paint;
pub mod tokens;

/// Test-only rendered specimen sheet, the visual counterpart to the unit
/// tests, since bevels, gradients, glows, and font fallback are correct or
/// wrong to an *eye*, not to an assertion. See the module docs for how to
/// dump it as a PNG.
#[cfg(test)]
pub mod gallery;

/// Test-only pixel-verification harness for the develop rail's never-yet-
/// reskinned panels (Looks/Presets/History), proves the "black text on
/// grey background" defect's root cause and this fix's effect. Kept
/// separate from [`gallery`] (owned by a concurrent agent this session).
#[cfg(test)]
pub mod panel_gallery;

/// Test-only kittest glue. Any harness that paints themed widgets must go
/// through [`test_support::themed`], see that module for why a bare
/// harness panics on the weight-role font families.
#[cfg(test)]
pub mod test_support;

use eframe::egui::{self, Context, CornerRadius, Stroke, Theme, Visuals};

use tokens as tk;

/// The neutral dark palette, Lightbox's default look (spec §1, §4, §11).
pub fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.dark_mode = true;

    // Elevation surfaces (§1).
    v.panel_fill = tk::ELEV_0_BASE; // side rails / floor
    v.window_fill = tk::ELEV_3_POPUP; // dropdowns / menus / tooltips (level 3, float)
    v.faint_bg_color = tk::ELEV_1_PANEL; // alternating-row tint reads as "card" level
    v.extreme_bg_color = tk::WELL_BG; // text-edit wells / darkest
    v.code_bg_color = tk::WELL_BG;

    // Non-interactive: labels, static separators, window/frame outlines.
    v.widgets.noninteractive.bg_fill = tk::ELEV_1_PANEL;
    v.widgets.noninteractive.weak_bg_fill = tk::ELEV_1_PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::SEPARATOR_HAIRLINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::TEXT_SECONDARY);

    // Rest state, the "raised" family at rest (§4).
    v.widgets.inactive.bg_fill = tk::CONTROL_REST;
    v.widgets.inactive.weak_bg_fill = tk::CONTROL_REST;
    v.widgets.inactive.bg_stroke = Stroke::new(tk::STROKE_BEVEL, tk::BEVEL_OUTSET_BOTTOM);
    v.widgets.inactive.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::TEXT_PRIMARY);

    // Hover, neutral brightening only (spec §5: accent must not leak
    // into plain hover on a not-yet-selected control).
    v.widgets.hovered.bg_fill = tk::CONTROL_HOVER;
    v.widgets.hovered.weak_bg_fill = tk::CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(tk::STROKE_BEVEL, tk::BEVEL_OUTSET_BOTTOM);
    v.widgets.hovered.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::TEXT_PRIMARY);

    // Active/pressed, darker fill reads "pushed in"; the accent-colored
    // outline here stands in for the crisp focus ring on stock widgets
    // until they're individually re-skinned with the real
    // `accent_focus_ring()` treatment via `paint::bevel_inset` + a proper
    // ring (spec §4's inset-bevel-flip + focus-ring co-occurrence).
    v.widgets.active.bg_fill = tk::CONTROL_PRESSED;
    v.widgets.active.weak_bg_fill = tk::CONTROL_PRESSED;
    v.widgets.active.bg_stroke = Stroke::new(tk::STROKE_FOCUS_RING, tk::accent_focus_ring());
    v.widgets.active.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::TEXT_PRIMARY);

    // Open combo/menu, same raised-rest treatment as inactive.
    v.widgets.open.bg_fill = tk::CONTROL_REST;
    v.widgets.open.weak_bg_fill = tk::CONTROL_REST;
    v.widgets.open.bg_stroke = Stroke::new(tk::STROKE_BEVEL, tk::BEVEL_OUTSET_BOTTOM);
    v.widgets.open.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::TEXT_PRIMARY);

    // Selection, the checked-menu-item wash (§5/§12), not a bespoke tint.
    v.selection.bg_fill = tk::accent_wash();
    v.selection.stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::accent());

    // Text + accent links + semantic status.
    v.override_text_color = Some(tk::TEXT_PRIMARY);
    v.hyperlink_color = tk::accent();
    v.warn_fg_color = tk::STATUS_WARNING;
    v.error_fg_color = tk::STATUS_ERROR;

    // Corner radii: radius_control (4px) uniformly on widgets, radius_popup
    // (6px) on the floating popup/window/menu surfaces.
    let widget_radius = CornerRadius::same(tk::RADIUS_CONTROL as u8);
    v.widgets.noninteractive.corner_radius = widget_radius;
    v.widgets.inactive.corner_radius = widget_radius;
    v.widgets.hovered.corner_radius = widget_radius;
    v.widgets.active.corner_radius = widget_radius;
    v.widgets.open.corner_radius = widget_radius;

    let popup_radius = CornerRadius::same(tk::RADIUS_POPUP as u8);
    v.window_corner_radius = popup_radius;
    v.menu_corner_radius = popup_radius;
    v.window_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::SEPARATOR_HAIRLINE);

    // Popups/menus/tooltips float above the docked architecture (§1, §11)
    // and are the only stock-`Visuals` surfaces that get a real shadow
    // both `window_shadow` and `popup_shadow` map to the same level-3
    // preset because this app has no `egui::Window`-backed level-4 modal;
    // a hand-built overlay component (using `ELEV_4_OVERLAY` /
    // `SHADOW_OVERLAY` / `OVERLAY_SCRIM` directly) owns that surface.
    v.window_shadow = tk::SHADOW_POPUP;
    v.popup_shadow = tk::SHADOW_POPUP;

    v
}

/// A neutral light variant for users who choose Light, minimal parity
/// per spec §13 ("dark is the product; light gets one paragraph"). Only
/// the values the spec gives a number for are overridden; every other
/// control state falls back to egui's stock `Visuals::light()` defaults,
/// same spirit as the pre-overhaul theme. Note that [`paint`]'s bevel
/// helpers are hardcoded to the dark bevel pair (`tokens::BEVEL_*`), full
/// light-theme parity for the procedural paint layer is out of scope here
/// (see the deviations note in the handoff), since dark is the product.
///
/// **`widgets.*.fg_stroke` (every interaction state) is explicitly set
/// here**, this is not cosmetic completeness, it fixes a real defect: the
/// pre-fix version left every `widgets.*` slot at egui's stock
/// `Visuals::light()` default, and [`Visuals::strong_text_color`] resolves
/// straight to `widgets.active.text_color()`, **bypassing
/// `override_text_color` entirely** (see that method's body). Stock
/// `Widgets::light().active.fg_stroke` is literal `Color32::BLACK`, so
/// every `RichText::strong()` label in a not-yet-individually-reskinned
/// panel (`panels::looks`/`presets`/`history`'s section headers,
/// `"Applied Look"`/`"Presets"`/`"History"`, all use `.strong()`) rendered
/// **pure black**, not merely `LIGHT_TEXT_PRIMARY`, against those
/// panels' permanently-dark `tokens::ELEV_1_PANEL` body background
/// (`panels::host::rail_contents_ui`'s hardcoded fill, unconditional on
/// theme). Confirmed by rendering `panels::looks` under a forced Light
/// theme and reading back the pixels (`theme::panel_gallery`), the
/// darkest pixel painted was exactly `[0, 0, 0]`, not the expected
/// `LIGHT_TEXT_PRIMARY` `[28, 28, 28]`, before this fix. Every other
/// stock-widget text call (`ui.label`, `ui.weak`, plain buttons) already
/// resolved correctly via `override_text_color`, only the
/// `strong_text_color`/`inactive`/`hovered`/`open` bypass slots were
/// silently uncovered.
pub fn light_visuals() -> Visuals {
    let mut v = Visuals::light();

    v.panel_fill = tk::LIGHT_ELEV_0_BASE;
    v.window_fill = tk::LIGHT_ELEV_3_POPUP;
    v.faint_bg_color = tk::LIGHT_ELEV_1_PANEL;
    v.override_text_color = Some(tk::LIGHT_TEXT_PRIMARY);

    // Every widget-state text color, explicitly, see this fn's doc
    // comment for why leaving any of these at egui's stock default is a
    // real (not merely cosmetic) defect. Mirrors `dark_visuals`'s own
    // "every fg_stroke gets a real token" coverage 1:1, light-token
    // equivalents.
    v.widgets.noninteractive.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::LIGHT_TEXT_SECONDARY);
    v.widgets.inactive.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::LIGHT_TEXT_PRIMARY);
    v.widgets.hovered.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::LIGHT_TEXT_PRIMARY);
    v.widgets.active.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::LIGHT_TEXT_PRIMARY);
    v.widgets.open.fg_stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::LIGHT_TEXT_PRIMARY);

    // Accent carries over unchanged (spec §13: verified against
    // light-panel backgrounds, 3.30:1 component contrast, 5.20:1 text).
    v.selection.bg_fill = tk::accent_wash();
    v.selection.stroke = Stroke::new(tk::STROKE_HAIRLINE, tk::accent());
    v.hyperlink_color = tk::accent();
    v.warn_fg_color = tk::STATUS_WARNING;
    v.error_fg_color = tk::STATUS_ERROR;

    let widget_radius = CornerRadius::same(tk::RADIUS_CONTROL as u8);
    v.widgets.noninteractive.corner_radius = widget_radius;
    v.widgets.inactive.corner_radius = widget_radius;
    v.widgets.hovered.corner_radius = widget_radius;
    v.widgets.active.corner_radius = widget_radius;
    v.widgets.open.corner_radius = widget_radius;

    let popup_radius = CornerRadius::same(tk::RADIUS_POPUP as u8);
    v.window_corner_radius = popup_radius;
    v.menu_corner_radius = popup_radius;
    v.window_shadow = tk::SHADOW_POPUP;
    v.popup_shadow = tk::SHADOW_POPUP;

    v
}

/// The neutral surround for the loupe canvas in the **dark** theme
/// (`ELEV_N1_CANVAS`, spec §8), a hole, not a card: no bevel, no
/// vignette (a deliberate recommendation against, not an omission, see
/// `tokens`'s §8 doc comment). Kept as the zero-arg default because this
/// is the existing public surface `lib.rs` calls; dark is the default
/// path per the spec's motion/theme-selection note ("default to Dark
/// regardless of OS"). Use [`canvas_surround_for`] where the active theme
/// is known and might be Light.
pub fn canvas_surround() -> egui::Color32 {
    tk::ELEV_N1_CANVAS
}

/// The canvas surround for a given theme. Per spec §13, the canvas does
/// **not** go white in light mode, every pro image tool holds the
/// working surface dark regardless of chrome theme, because a
/// bright-white surround wrecks perceived image contrast the same way a
/// vignette would (§8).
#[allow(dead_code)] // no theme-aware call site until light-mode wiring lands; exercised by this module's own test today
pub fn canvas_surround_for(dark: bool) -> egui::Color32 {
    if dark {
        tk::ELEV_N1_CANVAS
    } else {
        tk::CANVAS_SURROUND_LIGHT
    }
}

/// The surface and ink values for **hand-painted chrome**, resolved for
/// the active theme.
///
/// `Visuals` covers stock widgets, but the chrome this crate paints itself
/// the rail's header bands, the panel-body card, the disclosure triangle
/// reads tokens directly, and a token constant is one theme's value by
/// definition. Reading the dark constants unconditionally is what left the
/// rail painting a `#292929` card under a Light theme's near-black text:
/// the fill and the ink disagreed about which theme was active. Resolving
/// both from the same place makes that disagreement unrepresentable.
///
/// Accent and the semantic status colors are deliberately absent: they are
/// theme-independent by design (spec §13 verified `accent()` against
/// both a dark and a light panel), so call sites keep using the constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// Tool-panel body card fill (elevation 1).
    pub panel: egui::Color32,
    /// Collapsible header band gradient, top → bottom stop.
    pub header: (egui::Color32, egui::Color32),
    /// The same pair for the hovered header.
    pub header_hover: (egui::Color32, egui::Color32),
    /// Primary ink, titles, active disclosure triangle.
    pub text_primary: egui::Color32,
    /// Secondary ink, resting disclosure triangle, subordinate labels.
    pub text_secondary: egui::Color32,
}

impl Chrome {
    /// Resolve for `visuals`, pass `ui.visuals()` at the paint site.
    pub fn of(visuals: &Visuals) -> Chrome {
        if visuals.dark_mode {
            Chrome {
                panel: tk::ELEV_1_PANEL,
                header: (tk::ELEV_2_HEADER_TOP, tk::ELEV_2_HEADER_BOTTOM),
                header_hover: (tk::ELEV_2_HEADER_TOP_HOVER, tk::ELEV_2_HEADER_BOTTOM_HOVER),
                text_primary: tk::TEXT_PRIMARY,
                text_secondary: tk::TEXT_SECONDARY,
            }
        } else {
            Chrome {
                panel: tk::LIGHT_ELEV_1_PANEL,
                header: (tk::LIGHT_ELEV_2_HEADER_TOP, tk::LIGHT_ELEV_2_HEADER_BOTTOM),
                header_hover: (
                    tk::LIGHT_ELEV_2_HEADER_TOP_HOVER,
                    tk::LIGHT_ELEV_2_HEADER_BOTTOM_HOVER,
                ),
                text_primary: tk::LIGHT_TEXT_PRIMARY,
                text_secondary: tk::LIGHT_TEXT_SECONDARY,
            }
        }
    }
}

/// The accent hues Preferences offers, in menu order: a name and its base
/// hex. The first is the shipped one ([`tk::ACCENT_SHIPPED`]).
///
/// Every entry is **desaturated relative to a system accent on purpose**.
/// This is a photo editor: chrome that shouts competes with the image, and
/// §5's "the one hue allowed in chrome" is a hue, not a highlighter. Each
/// is also verified to carry legible ink by
/// `tokens::every_shipped_accent_carries_legible_ink`, so picking one can
/// never produce an unreadable primary button.
pub const ACCENT_PRESETS: &[(&str, u32)] = &[
    ("Lightbox Blue", 0x5583a8),
    ("Graphite", 0x808080),
    ("Teal", 0x4a9490),
    ("Moss", 0x6b9455),
    ("Amber", 0xb08842),
    ("Rust", 0xb0705a),
    ("Plum", 0x9273ab),
    ("Rose", 0xb06a86),
];

/// Applies `accent` as the live accent and re-registers both themes'
/// `Visuals` so egui's own selection fill, focus stroke and hyperlink
/// color follow it. Call at startup and whenever the preference changes.
pub fn set_accent(ctx: &Context, accent: egui::Color32) {
    tk::set_accent(accent);
    ctx.set_visuals_of(Theme::Dark, dark_visuals());
    ctx.set_visuals_of(Theme::Light, light_visuals());
}

/// Register Lightbox's visuals for both themes, install the embedded
/// fonts + text-style scale, and apply the spec's base spacing. Call once
/// at startup; `Context::set_theme(pref)` then selects between them.
pub fn install(ctx: &Context) {
    fonts::install_fonts(ctx);
    ctx.set_visuals_of(Theme::Dark, dark_visuals());
    ctx.set_visuals_of(Theme::Light, light_visuals());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = tk::ITEM_SPACING;
        style.spacing.button_padding = tk::BUTTON_PADDING;
        fonts::apply_text_styles(style);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_visuals_is_dark_mode() {
        assert!(dark_visuals().dark_mode);
    }

    /// **Regression (owner-reported "black text on the grey background").**
    /// The rail paints its own chrome from tokens, and a token constant is
    /// one theme's value. Reading the dark constants unconditionally left
    /// the panel card painted `#292929` while a Light theme drew near-black
    /// ink on top of it, the fill and the ink disagreed about the active
    /// theme.
    ///
    /// Whatever the theme, chrome ink must actually be legible on the
    /// chrome surface it is painted on. Asserted as real contrast, not as
    /// "the constants differ", so it stays meaningful if the palette moves.
    #[test]
    fn chrome_ink_is_legible_on_chrome_surfaces_in_both_themes() {
        /// WCAG relative luminance of an opaque sRGB color.
        fn luminance(c: egui::Color32) -> f64 {
            let ch = |v: u8| {
                let s = v as f64 / 255.0;
                if s <= 0.03928 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        }
        fn contrast(a: egui::Color32, b: egui::Color32) -> f64 {
            let (la, lb) = (luminance(a), luminance(b));
            let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
        }

        for dark in [true, false] {
            let visuals = if dark {
                dark_visuals()
            } else {
                light_visuals()
            };
            assert_eq!(
                visuals.dark_mode, dark,
                "the visuals must report the theme they are"
            );
            let c = Chrome::of(&visuals);

            // Primary ink on the panel card, the pairing that broke.
            let ratio = contrast(c.text_primary, c.panel);
            assert!(
                ratio >= 4.5,
                "dark={dark}: primary ink {:?} on panel {:?} is {ratio:.2}:1, \
                 below the 4.5:1 body-text floor",
                c.text_primary,
                c.panel
            );

            // Ink on the header band, at both gradient stops and hovered
            // a band is a gradient, so the worst stop is what matters.
            for (label, (top, bottom)) in [("rest", c.header), ("hover", c.header_hover)] {
                for (stop, bg) in [("top", top), ("bottom", bottom)] {
                    let ratio = contrast(c.text_primary, bg);
                    assert!(
                        ratio >= 4.5,
                        "dark={dark}: header title on the {label} {stop} stop \
                         {bg:?} is {ratio:.2}:1"
                    );
                }
            }

            // Secondary ink (resting disclosure triangle) is a non-text UI
            // component: WCAG 1.4.11's 3:1 applies, not 4.5:1.
            let ratio = contrast(c.text_secondary, c.header.0);
            assert!(
                ratio >= 3.0,
                "dark={dark}: secondary ink {:?} on the header {:?} is \
                 {ratio:.2}:1, below the 3:1 non-text-component floor",
                c.text_secondary,
                c.header.0
            );
        }
    }

    #[test]
    fn light_visuals_is_light_mode() {
        assert!(!light_visuals().dark_mode);
    }

    #[test]
    fn canvas_surround_stays_dark_in_both_themes() {
        // Spec §8/§13: the canvas surround never goes white, even in the
        // light theme, it must always read as darker than the light
        // panel background.
        let light_panel = tk::LIGHT_ELEV_0_BASE;
        let canvas_light = canvas_surround_for(false);
        assert!(canvas_light.r() < light_panel.r());
        assert_eq!(canvas_surround(), canvas_surround_for(true));
    }
}
