// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Embedded typefaces for Lightbox's dark theme (spec §9).
//!
//! Two OFL-1.1 faces, embedded via `include_bytes!` so the app has zero
//! runtime font dependency:
//! - **Inter** (Regular/Medium/SemiBold), all UI text: panel headers,
//!   labels, buttons, menus, tooltips, filmstrip metadata.
//! - **JetBrains Mono** (Regular), numeric value fields only: slider
//!   values, histogram/EXIF numbers, pixel coordinates. epaint's text
//!   shaper has no OpenType tabular-figure support, so a genuinely
//!   fixed-width face is the correct fix for digit-jitter while scrubbing,
//!   not a CSS-era trick.
//!
//! Both are inserted **ahead of** egui's built-in default fonts, which are
//! kept as fallbacks, the app UI uses glyphs (⏷ ⏶ ✕ 📁 and friends) that
//! Inter and JetBrains Mono don't cover, and losing them would break icon
//! rendering throughout the shell.
//!
//! Licensing: both TTFs live under `crates/lightbox-shell/assets/fonts/`
//! and are covered by `REUSE.toml` annotations (binary files can't carry
//! SPDX header comments); the canonical OFL-1.1 text is checked in at
//! `LICENSES/OFL-1.1.txt`. See `docs/plan/licensing.md` for the full
//! provenance entry.

use std::sync::Arc;

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily, FontId, Style, TextStyle};

const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

const KEY_INTER_REGULAR: &str = "Inter-Regular";
const KEY_INTER_MEDIUM: &str = "Inter-Medium";
const KEY_INTER_SEMIBOLD: &str = "Inter-SemiBold";
const KEY_JETBRAINS_MONO_REGULAR: &str = "JetBrainsMono-Regular";

/// Named custom families for weight roles, egui's `FontFamily` has no
/// weight axis, so a distinct weight is a distinct named family, per §9's
/// per-role table (panel header = SemiBold, section label = Medium, …).
pub const FAMILY_INTER_MEDIUM: &str = "InterMedium";
pub const FAMILY_INTER_SEMIBOLD: &str = "InterSemiBold";

/// Build the full `FontDefinitions`: Inter Regular becomes the head of
/// `FontFamily::Proportional`, JetBrains Mono Regular the head of
/// `FontFamily::Monospace`, and `FontFamily::Name("InterMedium"/
/// "InterSemiBold")` are registered for the weight roles, each falling
/// back through Inter Regular and then egui's stock fonts, so a missing
/// glyph never renders blank.
pub fn font_definitions() -> FontDefinitions {
    let mut defs = FontDefinitions::default();

    defs.font_data.insert(
        KEY_INTER_REGULAR.to_owned(),
        Arc::new(FontData::from_static(INTER_REGULAR)),
    );
    defs.font_data.insert(
        KEY_INTER_MEDIUM.to_owned(),
        Arc::new(FontData::from_static(INTER_MEDIUM)),
    );
    defs.font_data.insert(
        KEY_INTER_SEMIBOLD.to_owned(),
        Arc::new(FontData::from_static(INTER_SEMIBOLD)),
    );
    defs.font_data.insert(
        KEY_JETBRAINS_MONO_REGULAR.to_owned(),
        Arc::new(FontData::from_static(JETBRAINS_MONO_REGULAR)),
    );

    // Prepend as primary; egui's own defaults (kept in the Vec already)
    // become the fallback chain for glyphs Inter/JetBrains Mono lack.
    defs.families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, KEY_INTER_REGULAR.to_owned());
    defs.families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, KEY_JETBRAINS_MONO_REGULAR.to_owned());

    // Weight-role families: their own face first, then the (now
    // Inter-Regular-headed) proportional fallback chain.
    let proportional_fallbacks = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut medium_chain = vec![KEY_INTER_MEDIUM.to_owned()];
    medium_chain.extend(proportional_fallbacks.iter().cloned());
    defs.families
        .insert(FontFamily::Name(FAMILY_INTER_MEDIUM.into()), medium_chain);

    let mut semibold_chain = vec![KEY_INTER_SEMIBOLD.to_owned()];
    semibold_chain.extend(proportional_fallbacks.iter().cloned());
    defs.families.insert(
        FontFamily::Name(FAMILY_INTER_SEMIBOLD.into()),
        semibold_chain,
    );

    defs
}

/// Install the embedded fonts on `ctx`. Call once at startup, before
/// `apply_text_styles`.
pub fn install_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

/// Apply the spec §9 text-style scale to `style.text_styles`. Only the
/// five `egui::TextStyle` slots egui itself dispatches on by default;
/// per-role sizes that don't map to a `TextStyle` (panel header, section
/// label, slider value, …) are exposed as the `*_font()` helpers below so
/// call sites don't hardcode a `FontId`.
pub fn apply_text_styles(style: &mut Style) {
    style.text_styles.extend([
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Monospace, FontId::monospace(12.0)),
        (
            TextStyle::Heading,
            FontId::new(11.0, FontFamily::Name(FAMILY_INTER_SEMIBOLD.into())),
        ),
    ]);
}

// ---------------------------------------------------------------------
// Per-role FontId helpers (spec §9 typography table). Named after the
// spec's own role names so call sites read the same as the design doc.
//
// None of these have an in-crate caller yet, the panels/filmstrip/status
// bar re-skin passes that call them land as separate, parallel phases
// (same foundation-phase story as `theme::tokens`/`theme::paint`).
// ---------------------------------------------------------------------

/// Panel header (§2, §9): Inter SemiBold 11px. Uppercase + tracking is
/// applied at the call site (this only sets face/size).
#[allow(dead_code)] // panel header re-skin phase
pub fn panel_header_font() -> FontId {
    FontId::new(11.0, FontFamily::Name(FAMILY_INTER_SEMIBOLD.into()))
}

/// Section label / in-panel subhead (§9): Inter Medium 11px.
#[allow(dead_code)] // panel body re-skin phase
pub fn section_label_font() -> FontId {
    FontId::new(11.0, FontFamily::Name(FAMILY_INTER_MEDIUM.into()))
}

/// Slider label, left side (§3, §9): Inter Regular 12px.
#[allow(dead_code)] // house slider re-skin phase
pub fn slider_label_font() -> FontId {
    FontId::proportional(12.0)
}

/// Slider value, right side (§3, §9): JetBrains Mono Regular 12px,
/// tabular, the whole reason this face is embedded.
#[allow(dead_code)] // house slider re-skin phase
pub fn slider_value_font() -> FontId {
    FontId::monospace(12.0)
}

/// Body / control text, buttons, combo items (§9): Inter Regular 13px.
#[allow(dead_code)] // button/combo re-skin phase
pub fn body_font() -> FontId {
    FontId::proportional(13.0)
}

/// Status bar text (§7, §9): Inter Regular 11px.
#[allow(dead_code)] // status bar re-skin phase
pub fn status_bar_font() -> FontId {
    FontId::proportional(11.0)
}

/// Chip / badge text (§7, §9): Inter SemiBold 10px, uppercase applied at
/// the call site.
#[allow(dead_code)] // status bar re-skin phase
pub fn chip_font() -> FontId {
    FontId::new(10.0, FontFamily::Name(FAMILY_INTER_SEMIBOLD.into()))
}

/// Tooltip text (§11, §9): Inter Regular 12px.
#[allow(dead_code)] // popup/tooltip re-skin phase
pub fn tooltip_font() -> FontId {
    FontId::proportional(12.0)
}

/// Filmstrip filename / rating (§6, §9): Inter Regular 10px.
#[allow(dead_code)] // filmstrip re-skin phase
pub fn filmstrip_meta_font() -> FontId {
    FontId::proportional(10.0)
}

/// Empty-state message (§10, §9): Inter Medium 14px.
#[allow(dead_code)] // empty-state/dropzone re-skin phase
pub fn empty_state_font() -> FontId {
    FontId::new(14.0, FontFamily::Name(FAMILY_INTER_MEDIUM.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_definitions_registers_all_four_faces() {
        let defs = font_definitions();
        assert!(defs.font_data.contains_key(KEY_INTER_REGULAR));
        assert!(defs.font_data.contains_key(KEY_INTER_MEDIUM));
        assert!(defs.font_data.contains_key(KEY_INTER_SEMIBOLD));
        assert!(defs.font_data.contains_key(KEY_JETBRAINS_MONO_REGULAR));
    }

    #[test]
    fn proportional_head_is_inter_and_has_fallbacks() {
        let defs = font_definitions();
        let chain = &defs.families[&FontFamily::Proportional];
        assert_eq!(chain.first(), Some(&KEY_INTER_REGULAR.to_owned()));
        assert!(
            chain.len() > 1,
            "egui default fonts must remain as fallbacks for icon glyphs"
        );
    }

    #[test]
    fn monospace_head_is_jetbrains_mono_and_has_fallbacks() {
        let defs = font_definitions();
        let chain = &defs.families[&FontFamily::Monospace];
        assert_eq!(chain.first(), Some(&KEY_JETBRAINS_MONO_REGULAR.to_owned()));
        assert!(
            chain.len() > 1,
            "egui default monospace fonts must remain as fallbacks"
        );
    }

    #[test]
    fn named_weight_families_fall_back_through_regular() {
        let defs = font_definitions();
        let medium = &defs.families[&FontFamily::Name(FAMILY_INTER_MEDIUM.into())];
        assert_eq!(medium.first(), Some(&KEY_INTER_MEDIUM.to_owned()));
        assert!(medium.contains(&KEY_INTER_REGULAR.to_owned()));

        let semibold = &defs.families[&FontFamily::Name(FAMILY_INTER_SEMIBOLD.into())];
        assert_eq!(semibold.first(), Some(&KEY_INTER_SEMIBOLD.to_owned()));
        assert!(semibold.contains(&KEY_INTER_REGULAR.to_owned()));
    }

    #[test]
    fn embedded_ttfs_are_well_formed() {
        // A TrueType file starts with the 0x00010000 sfnt version tag (or
        // 'true'/'OTTO' for other sfnt flavors, Inter/JetBrains Mono ship
        // as plain TrueType, so we check the common case precisely).
        for bytes in [
            INTER_REGULAR,
            INTER_MEDIUM,
            INTER_SEMIBOLD,
            JETBRAINS_MONO_REGULAR,
        ] {
            assert!(bytes.len() > 4);
            assert_eq!(
                &bytes[0..4],
                &[0x00, 0x01, 0x00, 0x00],
                "expected a TrueType sfnt header"
            );
        }
    }
}
