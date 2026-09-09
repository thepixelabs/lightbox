// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Design tokens for Lightbox's dark-theme system.
//!
//! Transcribed 1:1 from the design spec's "Full token table" (plus §13's
//! light-theme ramp). Every value here is a direct restatement of a spec
//! number, if a value looks wrong, fix the spec first, then this file.
//! Nothing in this module computes or approximates; `paint.rs` and `mod.rs`
//! are where tokens get combined into actual drawing.
//!
//! ## Two rules that cut across every group below (spec Principle 1, §5)
//!
//! 1. **Chrome stays neutral.** Every elevation/bevel/separator/control/text
//!    token here has `r == g == b`. The **only** hue in the whole palette is
//!    the `ACCENT_*` family and the three `STATUS_*` semantic colors, and
//!    even the accent must stay silent at rest (no glow, no tint) on any
//!    control that is not selected, active, focused, or being dragged.
//! 2. **Where accent may appear** (spec §5): slider knob glow/focus ring,
//!    filmstrip selected/active thumbnail border+glow, active/solo panel
//!    header bar+wash, checked checkbox/radio fill, toggle-on switch track,
//!    text-edit focus border, the single primary-action button per view,
//!    active tab/segmented-control underline. **Where it must not appear**:
//!    canvas surround, rail/panel backgrounds, body text, icons at rest,
//!    disabled states, histogram/scope data. Plain hover on a
//!    not-yet-selected item is neutral brightening only, accent implies
//!    selection.

// This is the foundation phase (theme tokens + paint primitives): most of
// this catalog has no in-crate caller yet by design, the per-widget
// re-skin passes (house slider, buttons, filmstrip, status bar, panels,
// …) that consume it land as separate, parallel phases. Precedent for a
// whole-file dead-code allow at a phase boundary already exists in this
// workspace (`lightbox-rawproxy/src/limits.rs`); granular per-item allows
// aren't practical at catalog scale. Remove this once every token below
// has a real call site.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui::{Color32, Shadow, Vec2};

// ---------------------------------------------------------------------
// Hex/alpha helpers (private, just make the table below readable as the
// spec's own hex literals instead of hand-expanded r/g/b triples).
// ---------------------------------------------------------------------

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

const fn rgba(hex: u32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
        a,
    )
}

// =======================================================================
// Elevation / surfaces (spec §1, §8)
// =======================================================================
//
// Six named levels, lightness increases with elevation. Levels 0-2 are
// docked/flat-lit/no-shadow (part of the window's fixed architecture);
// levels 3-4 float above everything else and get a real `epaint::Shadow`
// (see the SHADOW_* consts below). `ELEV_N1_CANVAS` is a hole, not a card:
// it never gets a bevel or a shadow.

/// −1 (recessed hole): canvas surround; reused as `WELL_BG` for slider
/// grooves / text-edit fills.
pub const ELEV_N1_CANVAS: Color32 = rgb(0x141414);
/// 0 (floor): side rails (explorer, develop rail), filmstrip strip bg.
pub const ELEV_0_BASE: Color32 = rgb(0x1e1e1e);
/// 1 (card): tool-panel body, side-panel card body, collapsed-header
/// bottom stop.
pub const ELEV_1_PANEL: Color32 = rgb(0x292929);
/// 2 (header band), gradient top stop.
///
/// **Revised after visual review.** The spec's original pair
/// (`#303030` → `= ELEV_1_PANEL`) made the band's bottom stop *identical*
/// to the panel body under it, so the header dissolved into its own body
/// and the only thing marking the boundary was a single 1px hairline. The
/// spec's intent ("the header resolves into the panel") turned out, once
/// rendered, to read as "the header isn't there". The top stop now carries
/// +15 over the panel rather than +7, since the bottom stop no longer
/// donates any contrast of its own.
pub const ELEV_2_HEADER_TOP: Color32 = rgb(0x373737);
/// 2 (header band), gradient bottom stop, deliberately **not** aliased to
/// [`ELEV_1_PANEL`]: a small but nonzero step (+5) keeps the band from
/// vanishing where it meets the body, and the real edge signal comes from
/// the engraved seam the header paints along this boundary.
pub const ELEV_2_HEADER_BOTTOM: Color32 = rgb(0x2e2e2e);
/// Header band hover state: gradient lightens uniformly +6 units, enough
/// to read as a state change rather than a rounding error (§2).
pub const ELEV_2_HEADER_TOP_HOVER: Color32 = rgb(0x3d3d3d);
/// Header band hover state, bottom stop (rest +6, same as the top stop).
pub const ELEV_2_HEADER_BOTTOM_HOVER: Color32 = rgb(0x343434);
/// 3 (float): dropdown menus, combo popups, tooltips.
pub const ELEV_3_POPUP: Color32 = rgb(0x383838);
/// 4 (modal): dialog surface.
pub const ELEV_4_OVERLAY: Color32 = rgb(0x414141);
/// Bottom status bar, sits below the floor, anchors the window.
pub const STATUS_BAR_BG: Color32 = rgb(0x1a1a1a);
/// Recessed-well fill (slider groove, text-edit background), same value
/// as [`ELEV_N1_CANVAS`], named separately because its *use* is different
/// (a hole cut into a panel, not the canvas itself).
pub const WELL_BG: Color32 = ELEV_N1_CANVAS;
/// Full-viewport scrim painted behind a level-4 modal.
pub const OVERLAY_SCRIM: Color32 = rgba(0x000000, 130);

// =======================================================================
// Bevel primitive (spec Principle 2, §2, §4)
// =======================================================================
//
// One outset pair (light-top / dark-bottom, for raised controls: buttons,
// comboboxes, floating popups' top edge) and one inset pair (dark-top /
// light-bottom, for recessed controls: text-edit wells, the slider groove,
// pressed buttons). No control in the app should invent a third bevel.

/// Outset bevel, top stroke, light-top half of the "raised" pair.
pub const BEVEL_OUTSET_TOP: Color32 = rgba(0xffffff, 18);
/// Outset bevel, bottom stroke, dark-bottom half of the "raised" pair.
pub const BEVEL_OUTSET_BOTTOM: Color32 = rgba(0x000000, 90);
/// Inset bevel, top stroke, dark-top half of the "recessed" pair.
pub const BEVEL_INSET_TOP: Color32 = rgba(0x000000, 110);
/// Inset bevel, bottom stroke, light-bottom half of the "recessed" pair.
pub const BEVEL_INSET_BOTTOM: Color32 = rgba(0xffffff, 15);

// =======================================================================
// Separator / seam (spec §2)
// =======================================================================

/// The dark stroke of the two-stroke "engraved seam", used at every major
/// boundary (rail/canvas, filmstrip/canvas, status bar top edge, …).
pub const SEPARATOR_HAIRLINE: Color32 = rgba(0x000000, 140);
/// The 4% white highlight drawn immediately adjacent to
/// [`SEPARATOR_HAIRLINE`], on the side nearer the lighter/elevated surface.
pub const SEPARATOR_HAIRLINE_HIGHLIGHT: Color32 = rgba(0xffffff, 10);

// =======================================================================
// Shadows (spec §1, §11; `epaint::Shadow{offset, blur, spread, color}`)
// =======================================================================
//
// The only place shadows appear outside the slider knob's contact shadow
// and the filmstrip thumbnail lift (both below) is levels 3-4 floating
// above the window's docked architecture.

/// Dropdown menus, combo popups, tooltips' bigger sibling (§11 popups).
pub const SHADOW_POPUP: Shadow = Shadow {
    offset: [0, 4],
    blur: 14,
    spread: 0,
    color: rgba(0x000000, 140),
};
/// Tooltip shadow, lighter than [`SHADOW_POPUP`] (§11).
pub const SHADOW_TOOLTIP: Shadow = Shadow {
    offset: [0, 2],
    blur: 8,
    spread: 0,
    color: rgba(0x000000, 115),
};
/// Modal/dialog surface shadow (§1 level 4), paired with [`OVERLAY_SCRIM`].
pub const SHADOW_OVERLAY: Shadow = Shadow {
    offset: [0, 8],
    blur: 28,
    spread: 2,
    color: rgba(0x000000, 170),
};
/// Filmstrip thumbnail lift off the filmstrip floor (§6).
pub const SHADOW_THUMBNAIL: Shadow = Shadow {
    offset: [0, 1],
    blur: 3,
    spread: 0,
    color: rgba(0x000000, 60),
};

/// One layer of the slider knob's 2-layer contact shadow (§3 layer 3):
/// always present, not interaction-dependent. Two filled circles, offset
/// straight down, drawn back-to-front (bigger/fainter first).
pub struct ContactShadowLayer {
    /// Added to the knob's own radius.
    pub radius_delta: f32,
    /// Downward offset of the circle center.
    pub offset: Vec2,
    pub color: Color32,
}

/// The knob contact shadow's two layers, in paint order (back to front).
pub const SHADOW_KNOB_CONTACT: [ContactShadowLayer; 2] = [
    ContactShadowLayer {
        radius_delta: 1.5,
        offset: Vec2::new(0.0, 1.0),
        color: rgba(0x000000, 64), // 25% of 255, rounded
    },
    ContactShadowLayer {
        radius_delta: 0.5,
        offset: Vec2::new(0.0, 0.5),
        color: rgba(0x000000, 38), // 15% of 255, rounded
    },
];

// =======================================================================
// Control fills (spec §4), the "raised" family's four states
// =======================================================================

pub const CONTROL_REST: Color32 = rgb(0x333333);
pub const CONTROL_HOVER: Color32 = rgb(0x3c3c3c);
/// Darker than rest, reads pushed in. Paired with the inset bevel, not
/// the outset one (the fill flip is *part of* the pressed state).
pub const CONTROL_PRESSED: Color32 = rgb(0x2a2a2a);
/// Flat, no bevel at all, the absence of a bevel is what signals "inert".
pub const CONTROL_DISABLED: Color32 = rgb(0x262626);

// =======================================================================
// Text (spec §9, WCAG contrast table)
// =======================================================================

pub const TEXT_PRIMARY: Color32 = rgb(0xe2e2e2);
pub const TEXT_SECONDARY: Color32 = rgb(0xa0a0a0);
/// 3.39:1 against `ELEV_1_PANEL`, passes AA-large/non-text-component only.
/// **Restricted** to section labels (11px, treated as decorative not body)
/// and icon-only glyphs. Never body or value text.
pub const TEXT_TERTIARY: Color32 = rgb(0x7a7a7a);
/// 2.18:1 against `ELEV_1_PANEL`, a **documented WCAG exemption**
/// (disabled/inactive components are exempt from contrast minimums).
/// Do not "fix" this by brightening it, that would make disabled
/// controls read as active.
pub const TEXT_DISABLED: Color32 = rgb(0x5c5c5c);
/// Text on a status-bar chip (warning/error/success/neutral-info).
pub const TEXT_ON_CHIP: Color32 = rgb(0x141414);

// =======================================================================
// Accent (spec §5, §12), the one hue allowed in chrome, and only for
// selection/active/focus. See the module doc comment for the full
// may/may-not-appear rule; it is intentionally not duplicated per-token.
// =======================================================================

/// The **shipped** accent, the owner's original value, the default, and
/// the value every derived state below was transcribed from.
pub const ACCENT_SHIPPED: Color32 = rgb(0x5583a8);

/// The live accent, packed `0x00RRGGBB`. Set once at startup from
/// `MachinePrefs::accent` and again whenever the user picks another one in
/// Preferences.
///
/// **Why an atomic and not a token constant.** Accent is now a user
/// setting, and it is read from paint code all over the crate (sliders,
/// filmstrip, panel headers, curve, HSL, histogram, dropzone), several of
/// them free functions with no access to app state. A process-global with
/// relaxed ordering is the honest shape for "one value, written on a
/// preference change, read by the painter": there is exactly one UI
/// thread, so ordering beyond atomicity buys nothing.
static ACCENT_LIVE: AtomicU32 = AtomicU32::new(0x0055_83a8);

/// Sets the live accent (Preferences → Appearance). Callers must follow
/// with [`crate::theme::install`] so egui's own `Visuals`, selection
/// fill, focus stroke, hyperlink color, pick the new hue up too;
/// [`crate::theme::set_accent`] does both.
pub fn set_accent(accent: Color32) {
    let packed =
        (u32::from(accent.r()) << 16) | (u32::from(accent.g()) << 8) | u32::from(accent.b());
    ACCENT_LIVE.store(packed, Ordering::Relaxed);
}

/// Base accent, the user's chosen hue, source for every derived state.
pub fn accent() -> Color32 {
    rgb(ACCENT_LIVE.load(Ordering::Relaxed))
}

/// `t` of the way from `c` to white, in gamma space (the same space the
/// spec's hand-picked accent ramp was authored in).
fn lighten(c: Color32, t: f32) -> Color32 {
    c.lerp_to_gamma(Color32::WHITE, t)
}

/// `t` of the way from `c` to black.
fn darken(c: Color32, t: f32) -> Color32 {
    c.lerp_to_gamma(Color32::BLACK, t)
}

/// How far each derived accent state sits from the base, as a lerp toward
/// white (positive) or black (negative). These reproduce the spec's
/// hand-authored ramp for the shipped accent to within one 8-bit step
/// asserted by `derived_accent_states_match_the_spec_ramp`, and are what
/// lets any other accent inherit the same relationships.
const ACCENT_HOVER_LIGHTEN: f32 = 0.10;
const ACCENT_ACTIVE_DARKEN: f32 = 0.08;
const ACCENT_FOCUS_LIGHTEN: f32 = 0.218;
const FILL_TOP_LIGHTEN: f32 = 0.15;
const FILL_BOTTOM_DARKEN: f32 = 0.10;

/// Hover fill on accent-*bearing* controls only (primary button, toggle-on)
/// never plain hover on a not-yet-selected item.
pub fn accent_hover() -> Color32 {
    lighten(accent(), ACCENT_HOVER_LIGHTEN)
}

/// Pressed fill. Darkened 8% toward black (not the naive 12%) specifically
/// to keep [`text_on_accent`] on top of it at ≥4.5:1 (binding WCAG
/// constraint).
pub fn accent_active() -> Color32 {
    darken(accent(), ACCENT_ACTIVE_DARKEN)
}

/// Crisp keyboard-focus ring color, all components. Always 2px, **never**
/// blurred or alpha-stacked, a11y requires it stay visually distinct from
/// the soft decorative hover/drag glow next to it.
pub fn accent_focus_ring() -> Color32 {
    lighten(accent(), ACCENT_FOCUS_LIGHTEN)
}

/// Same hue at `alpha`, for the wash/glow stacks below.
fn accent_alpha(alpha: u8) -> Color32 {
    let c = accent();
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

/// Active/solo panel header wash, checked menu-item fill.
pub fn accent_wash() -> Color32 {
    accent_alpha(36)
}

/// Slider knob hover glow, and the dropzone drag-over border glow (§10
/// reuses this exact stack). 4 layers, radius deltas ascending, draw
/// `.iter().rev()` (largest/faintest first) so the opaque knob body
/// painted afterward covers the inner overlap. See [`crate::theme::paint::glow_circle`].
pub fn accent_glow_hover() -> [(f32, Color32); 4] {
    [
        (2.0, accent_alpha(90)),
        (4.0, accent_alpha(55)),
        (6.0, accent_alpha(30)),
        (8.0, accent_alpha(14)),
    ]
}

/// Slider knob while actively dragged. 5 layers, same ascending-radius
/// convention as [`accent_glow_hover`].
pub fn accent_glow_drag() -> [(f32, Color32); 5] {
    [
        (2.0, accent_alpha(130)),
        (4.0, accent_alpha(90)),
        (6.0, accent_alpha(55)),
        (8.0, accent_alpha(28)),
        (10.0, accent_alpha(12)),
    ]
}

/// Filmstrip *active* (single primary/most-recent selection) thumbnail's
/// persistent, non-hover-gated glow (§5), 2 layers, restrained on
/// purpose: a full 4-layer glow per thumbnail would be loud with many
/// selected at once. *Selected-but-not-active* thumbnails get the 2px
/// [`accent`] border only, no glow at all.
pub fn accent_glow_filmstrip_active() -> [(f32, Color32); 2] {
    [(2.0, accent_alpha(56)), (4.0, accent_alpha(26))]
}

/// The contrast floor for text on an accent fill (WCAG AA for body text).
/// The spec attached it to the shipped accent's *darkest* state, and that
/// is still the binding case for any accent.
pub const MIN_INK_CONTRAST: f32 = 4.5;

/// Label/text on any accent-filled surface. The spec pinned this to
/// near-black because that is what clears [`MIN_INK_CONTRAST`] against
/// every state of the *shipped* accent. Now that the accent is a user
/// setting, the pin becomes a rule with the same result: **keep the
/// spec's near-black unless it actually fails** against the darkest
/// accent state ([`accent_active`]), and only then switch to near-white.
///
/// Deliberately a threshold, not an argmax. Picking whichever ink merely
/// contrasts *more* would flip the shipped accent to white text, black
/// scores 4.53 there and white 4.64, changing an appearance the spec
/// chose, over a hair of ratio, for no accessibility gain.
pub fn text_on_accent() -> Color32 {
    if contrast_ratio(TEXT_ON_ACCENT_DARK, accent_active()) >= MIN_INK_CONTRAST {
        TEXT_ON_ACCENT_DARK
    } else {
        TEXT_ON_ACCENT_LIGHT
    }
}

/// The near-black half of [`text_on_accent`] (the spec's original value).
pub const TEXT_ON_ACCENT_DARK: Color32 = rgb(0x000000);
/// The near-white half, for accents too dark to carry black text.
pub const TEXT_ON_ACCENT_LIGHT: Color32 = rgb(0xffffff);

/// WCAG 2.1 relative luminance.
fn relative_luminance(c: Color32) -> f32 {
    fn channel(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG 2.1 contrast ratio between two opaque colors (1.0..=21.0).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

// =======================================================================
// Semantic status (spec §7, §12), the three functional exceptions to
// Principle 1. Always paired with text or an icon shape, never color
// alone (contrast-verified for both chip-text and dot/icon use in §WCAG).
// =======================================================================

pub const STATUS_WARNING: Color32 = rgb(0xc9974a);
pub const STATUS_ERROR: Color32 = rgb(0xcc6b6b);
pub const STATUS_SUCCESS: Color32 = rgb(0x5c9e6f);

// =======================================================================
// Slider (spec §3, the house `param_slider`)
// =======================================================================

pub const TRACK_HEIGHT: f32 = 6.0;
pub const TRACK_RADIUS: f32 = 3.0; // full pill: radius = height / 2
/// The house slider's fill bar, top and bottom gradient stops, accent
/// lightened 15% / darkened 10%, so a re-accented app gets re-accented
/// sliders instead of blue ones under a green selection.
pub fn fill_gradient_top() -> Color32 {
    lighten(accent(), FILL_TOP_LIGHTEN)
}
pub fn fill_gradient_bottom() -> Color32 {
    darken(accent(), FILL_BOTTOM_DARKEN)
}
/// 1px line along the top pixel row of the fill bar only.
pub const FILL_TOP_GLARE: Color32 = rgba(0xffffff, 26);
pub const KNOB_RADIUS_REST: f32 = 6.0;
pub const KNOB_RADIUS_HOVER_DRAG: f32 = 7.0;
pub const KNOB_FILL_REST_TOP: Color32 = rgb(0xd9d9d9);
pub const KNOB_FILL_REST_BOTTOM: Color32 = rgb(0xa8a8a8);
pub const KNOB_FILL_HOVER_DRAG_TOP: Color32 = rgb(0xe6e6e6);
pub const KNOB_FILL_HOVER_DRAG_BOTTOM: Color32 = rgb(0xb8b8b8);
pub const KNOB_EDGE_STROKE_REST: Color32 = rgb(0x6e6e6e);
pub const KNOB_EDGE_STROKE_HOVER_DRAG: Color32 = rgb(0x7a7a7a);
/// Short 1px arc over the top ~40% of circumference.
pub const KNOB_TOP_HIGHLIGHT: Color32 = rgba(0xffffff, 64);

/// A slider's fill origin (spec §3): unipolar params fill from the left
/// edge, bipolar params (Temp, Tint, Exposure, Contrast, Highlights/
/// Shadows, anything spanning a signed range around a true zero) fill
/// from the **center** outward in either direction, matching Lightroom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillOrigin {
    Left,
    Center,
}

// =======================================================================
// Radius scale (spec: Full token table)
// =======================================================================

pub const RADIUS_CONTROL: f32 = 4.0;
pub const RADIUS_THUMB: f32 = 3.0;
pub const RADIUS_PANEL: f32 = 6.0;
pub const RADIUS_POPUP: f32 = 6.0;
pub const RADIUS_DROPZONE: f32 = 10.0;
/// Full pill (chip height 16px, `radius_chip == height / 2`).
pub const RADIUS_CHIP: f32 = 8.0;

/// A full-pill radius for a given height (`RADIUS_TRACK`, `RADIUS_CHIP`,
/// and any other "radius = height / 2" surface in the spec).
#[inline]
pub const fn pill_radius(height: f32) -> f32 {
    height * 0.5
}

// =======================================================================
// Spacing scale (spec: Full token table)
// =======================================================================

pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_5: f32 = 24.0;
/// Vertical spacing tightened from the spec's 6.0 after measuring the
/// rendered rail: a pro editor is dense, and every extra vertical point
/// here is multiplied by the ~20 sliders a develop session scrolls
/// through. Horizontal spacing is unchanged, it was never the problem.
pub const ITEM_SPACING: Vec2 = Vec2::new(8.0, 4.0);
pub const BUTTON_PADDING: Vec2 = Vec2::new(10.0, 6.0);

// =======================================================================
// Stroke widths (spec: Full token table)
// =======================================================================

pub const STROKE_HAIRLINE: f32 = 1.0;
pub const STROKE_BEVEL: f32 = 1.0;
pub const STROKE_SELECTION_BORDER: f32 = 2.0;
pub const STROKE_FOCUS_RING: f32 = 2.0;

// =======================================================================
// Motion (spec §Motion & reduced-motion)
// =======================================================================
//
// The theme's *only* animated behavior is the hover fill/alpha fade on
// hover-capable controls. Everything else (glows, rings, gradients) is
// drawn at final state every frame, nothing else needs a reduced-motion
// fallback because nothing else moves.

pub const MOTION_HOVER_FADE_MS: u64 = 80;

/// `MOTION_HOVER_FADE_MS`, or `0` when the OS reports reduced-motion.
#[inline]
pub const fn motion_hover_fade_ms(reduced_motion: bool) -> u64 {
    if reduced_motion {
        0
    } else {
        MOTION_HOVER_FADE_MS
    }
}

// =======================================================================
// Cross-cutting layout constants
// =======================================================================
//
// Not part of the spec's "Full token table" (which is elevation/color/
// radius/spacing/stroke/motion only) but pulled out of the prose sections
// because more than one downstream widget needs the same number. This is
// NOT exhaustive, a widget-local measurement that only that widget uses
// belongs at its own call site, not here.

/// Collapsible tool-panel header band height (§2), tightened from the
/// spec's 28.0, see [`ITEM_SPACING`] for the density rationale.
pub const PANEL_HEADER_HEIGHT: f32 = 24.0;
/// Panel body inset padding: left, right, top, bottom (§2). Horizontal
/// padding is unchanged; the vertical pair is tightened, since that is
/// what compounds down a scrolling rail.
pub const PANEL_BODY_PADDING: (f32, f32, f32, f32) = (12.0, 12.0, 8.0, 8.0);

/// The house slider's **interactive** row height, deliberately taller
/// than the 6pt visible groove ([`TRACK_HEIGHT`]) so the drag target stays
/// forgiving. Was a bare `14.0` literal inside `value_slider`; promoted
/// here both to cut 4pt of per-slider height and because an untokened
/// magic number is a defect in a token-driven system. The visible track is
/// unchanged, only the invisible grab padding shrinks, from 4pt a side to
/// 2pt.
pub const TRACK_HIT_HEIGHT: f32 = 10.0;

/// Height of the house slider's label/value row. Pinned rather than
/// auto-sized: an auto-sized row was consuming roughly twice what a 12px
/// line needs, which was the single largest contributor to the rail's
/// slider pitch.
pub const SLIDER_LABEL_ROW_HEIGHT: f32 = 16.0;
/// Disclosure triangle size, width × height (§2).
pub const DISCLOSURE_TRIANGLE_SIZE: Vec2 = Vec2::new(6.0, 5.0);
/// Active/solo panel header's left-edge accent bar width (§2).
pub const PANEL_SOLO_BAR_WIDTH: f32 = 2.0;

/// Filmstrip cell gap between thumbnails, and internal padding around each
/// thumbnail (§6).
pub const FILMSTRIP_CELL_GAP: f32 = 6.0;
pub const FILMSTRIP_CELL_PADDING: f32 = 8.0;
/// Filmstrip badge hit area (pick/reject/edited) and its glyph size (§6).
pub const FILMSTRIP_BADGE_SIZE: f32 = 14.0;
pub const FILMSTRIP_BADGE_GLYPH_SIZE: f32 = 10.0;
/// Filmstrip name/rating strip height (§6).
pub const FILMSTRIP_META_STRIP_HEIGHT: f32 = 16.0;
/// Badge backing opacity multiplier over `ELEV_3_POPUP` (§6).
pub const FILMSTRIP_BADGE_BACKING_ALPHA: f32 = 0.90;

/// Status bar height (§7).
pub const STATUS_BAR_HEIGHT: f32 = 24.0;
/// Status chip height and horizontal padding (§7).
pub const CHIP_HEIGHT: f32 = 16.0;
pub const CHIP_PADDING_X: f32 = 8.0;
/// Activity indicator progress bar: height × width, docked at the status
/// bar's left edge (§7).
pub const ACTIVITY_BAR_SIZE: Vec2 = Vec2::new(120.0, 3.0);

/// Canvas placard / degraded-mode chip background opacity over
/// `ELEV_1_PANEL` (§8), the canvas must still read through at the edges.
pub const CANVAS_PLACARD_BG_ALPHA: f32 = 0.85;

/// Empty-state dropzone dash pattern: dash length, gap length (§10).
pub const DROPZONE_DASH_PATTERN: (f32, f32) = (6.0, 4.0);
/// Empty-state icon bounding box (§10).
pub const DROPZONE_ICON_SIZE: Vec2 = Vec2::new(64.0, 48.0);
/// Dropzone border at rest, `TEXT_TERTIARY @ 60%`.
pub const DROPZONE_BORDER_REST: Color32 = rgba(0x7a7a7a, 153);
/// Dropzone background wash while a drag-over is in progress (§10).
pub fn dropzone_drag_wash() -> Color32 {
    accent_alpha(13)
}

// =======================================================================
// §13 Light theme (minimal parity, dark is the product)
// =======================================================================
//
// Only what the spec explicitly gives a number for. Everything else (the
// remaining control/selection states) falls back to egui's stock
// `Visuals::light()` defaults in `mod.rs::light_visuals()`, same spirit as
// the pre-overhaul theme. `ACCENT_DEFAULT` / `TEXT_ON_ACCENT` carry over
// unchanged, both were verified against light-panel backgrounds (§13).

pub const LIGHT_ELEV_0_BASE: Color32 = rgb(0xf2f2f2);
pub const LIGHT_ELEV_1_PANEL: Color32 = rgb(0xe8e8e8);
pub const LIGHT_ELEV_2_HEADER_TOP: Color32 = rgb(0xececec);
pub const LIGHT_ELEV_2_HEADER_BOTTOM: Color32 = rgb(0xe4e4e4);
/// Header hover in light mode **darkens** by the same 6 units the dark
/// theme lightens by, on a light surface, "closer to the pointer" reads
/// as more ink, not more light.
pub const LIGHT_ELEV_2_HEADER_TOP_HOVER: Color32 = rgb(0xe6e6e6);
pub const LIGHT_ELEV_2_HEADER_BOTTOM_HOVER: Color32 = rgb(0xdedede);
pub const LIGHT_ELEV_3_POPUP: Color32 = rgb(0xfbfbfb);
pub const LIGHT_TEXT_PRIMARY: Color32 = rgb(0x1c1c1c);
pub const LIGHT_TEXT_SECONDARY: Color32 = rgb(0x4a4a4a);
/// Light theme needs a subtler shadow / stronger highlight than dark does.
pub const LIGHT_BEVEL_OUTSET_TOP: Color32 = rgba(0xffffff, 102); // 40%
pub const LIGHT_BEVEL_OUTSET_BOTTOM: Color32 = rgba(0x000000, 26); // 10%
/// The canvas surround does **not** go white in light mode (spec §13)
/// every pro image tool holds the working surface dark regardless of
/// chrome theme, because a bright-white surround wrecks perceived image
/// contrast the same way a vignette would (§8). Kept a mid-dark neutral.
pub const CANVAS_SURROUND_LIGHT: Color32 = rgb(0x3a3a3a);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_chrome_is_truly_neutral() {
        // Principle 1: every gray token has r == g == b. Spot-check the
        // elevation ramp plus control fills, the tokens most likely to
        // grow an accidental tint if someone hand-edits a hex value.
        for c in [
            ELEV_N1_CANVAS,
            ELEV_0_BASE,
            ELEV_1_PANEL,
            ELEV_2_HEADER_TOP,
            ELEV_2_HEADER_BOTTOM,
            ELEV_3_POPUP,
            ELEV_4_OVERLAY,
            STATUS_BAR_BG,
            CONTROL_REST,
            CONTROL_HOVER,
            CONTROL_PRESSED,
            CONTROL_DISABLED,
            TEXT_PRIMARY,
            TEXT_SECONDARY,
            TEXT_TERTIARY,
            TEXT_DISABLED,
        ] {
            assert_eq!(c.r(), c.g(), "expected neutral gray, got {c:?}");
            assert_eq!(c.g(), c.b(), "expected neutral gray, got {c:?}");
        }
    }

    #[test]
    fn glow_stacks_are_radius_ascending() {
        // paint::glow_circle relies on iterating these in reverse to
        // paint largest/faintest first without a heap allocation.
        for w in accent_glow_hover().windows(2) {
            assert!(w[0].0 < w[1].0);
        }
        for w in accent_glow_drag().windows(2) {
            assert!(w[0].0 < w[1].0);
        }
        for w in accent_glow_filmstrip_active().windows(2) {
            assert!(w[0].0 < w[1].0);
        }
    }

    /// The accent ramp used to be six hand-transcribed spec hexes. It is
    /// now derived, so this pins the derivation to those exact hexes for
    /// the shipped accent: any drift in the lerp factors shows up here
    /// rather than as a slightly-off hover state nobody notices.
    ///
    /// Tolerance is one 8-bit step, `lerp_to_gamma` rounds where the
    /// spec's author rounded by eye.
    #[test]
    fn derived_accent_states_match_the_spec_ramp() {
        fn close(got: Color32, want: Color32, what: &str) {
            for (g, w) in [
                (got.r(), want.r()),
                (got.g(), want.g()),
                (got.b(), want.b()),
            ] {
                assert!(
                    g.abs_diff(w) <= 1,
                    "{what}: derived {got:?} is not the spec's {want:?}"
                );
            }
        }
        set_accent(ACCENT_SHIPPED);
        close(accent(), rgb(0x5583a8), "ACCENT_DEFAULT");
        close(accent_hover(), rgb(0x668fb1), "ACCENT_HOVER");
        close(accent_active(), rgb(0x4e799b), "ACCENT_ACTIVE");
        close(accent_focus_ring(), rgb(0x7a9ebb), "ACCENT_FOCUS_RING");
        close(fill_gradient_top(), rgb(0x6e96b5), "FILL_GRADIENT_TOP");
        close(
            fill_gradient_bottom(),
            rgb(0x4c7697),
            "FILL_GRADIENT_BOTTOM",
        );
        assert_eq!(accent_wash().a(), 36, "wash alpha");
        assert_eq!(
            text_on_accent(),
            TEXT_ON_ACCENT_DARK,
            "the shipped accent must keep the spec's near-black ink"
        );
    }

    /// The binding WCAG constraint the spec attached to `TEXT_ON_ACCENT`,
    /// now that the accent is a user setting: whichever ink
    /// [`text_on_accent`] picks must clear [`MIN_INK_CONTRAST`] against
    /// the *darkest* accent state, for every accent the app offers. This
    /// is what makes the preset list a curated set rather than a swatch
    /// grid, a hue that cannot carry legible text does not ship.
    #[test]
    fn every_shipped_accent_carries_legible_ink() {
        for (name, hex) in crate::theme::ACCENT_PRESETS {
            set_accent(rgb(*hex));
            let ink = text_on_accent();
            let ratio = contrast_ratio(ink, accent_active());
            assert!(
                ratio >= MIN_INK_CONTRAST,
                "accent {name} ({hex:#08x}): ink {ink:?} contrast {ratio:.2} < {MIN_INK_CONTRAST}"
            );
            assert_eq!(
                ink, TEXT_ON_ACCENT_DARK,
                "every shipped accent is light enough for the spec's near-black ink; \
                 {name} would need white, which means it is too dark to ship"
            );
        }
        set_accent(ACCENT_SHIPPED);
    }

    /// …and a custom accent dark enough to defeat near-black flips the ink
    /// rather than shipping unreadable text. (Preferences offers a free
    /// color picker, so this case is reachable.)
    #[test]
    fn a_very_dark_custom_accent_flips_the_ink_to_white() {
        set_accent(rgb(0x1d2f42));
        assert_eq!(text_on_accent(), TEXT_ON_ACCENT_LIGHT);
        assert!(contrast_ratio(text_on_accent(), accent_active()) >= MIN_INK_CONTRAST);
        set_accent(ACCENT_SHIPPED);
    }

    #[test]
    fn reduced_motion_zeroes_hover_fade() {
        assert_eq!(motion_hover_fade_ms(false), MOTION_HOVER_FADE_MS);
        assert_eq!(motion_hover_fade_ms(true), 0);
    }
}
