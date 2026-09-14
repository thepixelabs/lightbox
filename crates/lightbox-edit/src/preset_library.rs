// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The starter preset library (Lightbox preset-management feature): a
//! curated set of **original, provenance-clean** develop-recipe presets,
//! authored directly against E09's param vocabulary, never derived from,
//! imported from, or modeled on any Adobe/Lightroom preset data (the same
//! "no Adobe-derived data" affidavit `lightbox_color::look`'s default-look
//! authoring carries, spec §4.3 / the assets manifest policy).
//!
//! Every number below was reasoned out from E10's own published render
//! calibrations, `color_grade::GRADE_CHROMA_RANGE`/`GRADE_LUM_RANGE`, the
//! Oklab hue anchors in `colorspace::HUE_BAND_CENTERS_DEG` (red 29°, orange
//! 53°, yellow 110°, green 142°, aqua 195°, blue 264°, purple 294°, magenta
//! 328°), `hsl::HUE_RANGE_DEG`/`SAT_RANGE`/`LUM_RANGE`, and
//! `bw_mix::MIX_CHROMA_SCALE`, not transcribed from any third-party preset
//! or film-emulation pack. Grade-wheel hues are therefore **Oklab** angles,
//! which is why a "warm highlight" sits near 50-75° and a "teal shadow" near
//! 195-225°.
//!
//! Each [`LibraryPreset`] is a `(name, group, delta)` triple; its
//! [`LibraryPreset::subset`] is auto-derived from the delta's touched groups
//! (`params::group_of`), every param in a touched group travels together as
//! ONE coherent style bundle (spec §3.1: "applying it changes exactly those
//! groups"), the exact contract `PresetStore::create_from` implements.
//!
//! `crates/lightbox-edit/tests/preset_library.rs` blesses the committed
//! `assets/presets/**/*.xmp` files FROM [`starter_presets`] (same
//! `LIGHTBOX_BLESS=1` convention `lightbox_color::look`'s asset test uses)
//! and gates them against `assets/MANIFEST.toml`'s surface-3 provenance
//! policy. `lightbox-shell` seeds a fresh preset store from that bundled
//! directory on every real launch (see `lightbox-shell`'s `lib.rs`,
//! `bundled_preset_files` + the `ImportPresetFiles` submit next to it).
//!
//! # Families
//!
//! The taxonomy is by **look**, not by subject matter, a colorist reaches
//! for a mood, not for a genre folder. Seven groups, each deep enough to
//! browse as one dropdown:
//!
//! * **Neon**, night city. Magenta/cyan and amber/blue splits over crushed
//!   (but never clipped) blacks, with the saturation pushed per-band so the
//!   signage sings and the sodium mud does not.
//! * **Retro**, the aged-stock register: warm lifted blacks, compressed
//!   whites, muted yellow-greens, and the crossed channels of film that sat
//!   in a hot car for a decade.
//! * **Verdant**, the green axis: oxidised teal, sickly absinthe, damp
//!   moss, emerald night, utilitarian olive. Skin is deliberately drained in
//!   the harsher members.
//! * **Atmosphere**, weather. Haze *added* through negative dehaze, blacks
//!   lifted into milk, contrast pulled out of the midtones; plus one clean
//!   cold-air look that does the exact opposite.
//! * **Edge**, the aggressive end: bleach-bypass desaturation, brutal
//!   S-curves, cross-processed clash, and clarity pushed past polite.
//! * **Mono**, `Treatment::BlackAndWhite` plus an 8-channel `BwMix` chosen
//!   for character (harsh street, soft silver, pseudo-infrared, matte print,
//!   noir) rather than for neutrality. No grade wheels here: `global.bw_mix`
//!   runs *after* `global.color_grade`, so a wheel would only feed chroma
//!   into the mixer rather than tint the result.
//! * **Tonal**, tone/curve-only foundations that state an opinion about
//!   shape and nothing about colour, for stacking under the rest. Like every
//!   other family they deliberately never touch `Exposure`: exposure
//!   correction is scene-specific, not stylistic.

use crate::leaves::{
    BwMix, ColorGrade, CurvePoint, GradeWheel, Grain, HslBand, HslTable, ToneCurve, Treatment,
};
use crate::params::{group_of, ParamDelta, ParamId, ParamSubset, ParamValue};

/// One starter-library preset, ready for `PresetStore::create_from(&recipe,
/// name, group, &subset)` once `delta` has been applied onto an identity
/// recipe (see `crates/lightbox-edit/tests/preset_library.rs`).
#[derive(Clone, Debug)]
pub struct LibraryPreset {
    /// Display name.
    pub name: &'static str,
    /// Folder (group) the preset is filed under.
    pub group: &'static str,
    /// The full delta this preset carries.
    pub delta: ParamDelta,
}

impl LibraryPreset {
    fn new(name: &'static str, group: &'static str, delta: ParamDelta) -> LibraryPreset {
        LibraryPreset { name, group, delta }
    }

    /// The subset auto-derived from the delta's touched groups (spec §3.1:
    /// a preset carries every param in each group it touches, as ONE
    /// coherent bundle, never a hand-picked, possibly-drifting subset).
    pub fn subset(&self) -> ParamSubset {
        ParamSubset::from_groups(self.delta.0.keys().copied().map(group_of))
    }
}

// ── HSL band indices (spec-fixed order, `panels::hsl`'s own doc comment:
// Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta) ──────────────────
const RED: usize = 0;
const ORANGE: usize = 1;
const YELLOW: usize = 2;
const GREEN: usize = 3;
const AQUA: usize = 4;
const BLUE: usize = 5;
const PURPLE: usize = 6;
const MAGENTA: usize = 7;

/// A tiny internal DSL that keeps the preset definitions below readable:
/// each method inserts exactly the `ParamId`s it names, at the values given.
#[derive(Default)]
struct DeltaBuilder(ParamDelta);

impl DeltaBuilder {
    fn new() -> DeltaBuilder {
        DeltaBuilder(ParamDelta::new())
    }

    fn set(mut self, id: ParamId, v: ParamValue) -> DeltaBuilder {
        self.0 .0.insert(id, v);
        self
    }

    /// The five non-exposure basic-tone sliders (spec §4.3), each `-100..=100`.
    fn tone(
        self,
        contrast: f32,
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
    ) -> DeltaBuilder {
        self.set(ParamId::Contrast, ParamValue::F32(contrast))
            .set(ParamId::Highlights, ParamValue::F32(highlights))
            .set(ParamId::Shadows, ParamValue::F32(shadows))
            .set(ParamId::Whites, ParamValue::F32(whites))
            .set(ParamId::Blacks, ParamValue::F32(blacks))
    }

    fn presence(self, vibrance: f32, saturation: f32, clarity: f32) -> DeltaBuilder {
        self.set(ParamId::Vibrance, ParamValue::F32(vibrance))
            .set(ParamId::Saturation, ParamValue::F32(saturation))
            .set(ParamId::Clarity, ParamValue::F32(clarity))
    }

    /// The other half of the `Presence` group: texture + dehaze, each
    /// `-100..=100`. **Negative dehaze ADDS atmospheric haze** (the recovery
    /// branch of `global.dehaze`), the Atmosphere family's main tool, and
    /// the reason it is worth reaching past `presence` for these two.
    fn texture_dehaze(self, texture: f32, dehaze: f32) -> DeltaBuilder {
        self.set(ParamId::Texture, ParamValue::F32(texture))
            .set(ParamId::Dehaze, ParamValue::F32(dehaze))
    }

    /// Film grain (`fx.grain`), each `0..=100`. Until the grain node existed
    /// this leaf imported, stored, and rendered nothing, so no shipped preset
    /// set it; the one preset named for it is the first.
    fn grain(self, amount: f32, size: f32, roughness: f32) -> DeltaBuilder {
        self.set(
            ParamId::Grain,
            ParamValue::Grain(Grain {
                amount,
                size,
                roughness,
            }),
        )
    }

    /// Sets one HSL band's hue/sat/lum (index per the `RED..MAGENTA` consts
    /// above); starts from the neutral 8-band table each call so multiple
    /// `.hsl_band(..)` calls on the same builder compose onto one leaf.
    fn hsl_band(self, index: usize, hue: f32, sat: f32, lum: f32) -> DeltaBuilder {
        let mut table = match self.0 .0.get(&ParamId::Hsl) {
            Some(ParamValue::Hsl(t)) => *t,
            _ => HslTable::default(),
        };
        table.bands[index] = HslBand { hue, sat, lum };
        self.set(ParamId::Hsl, ParamValue::Hsl(table))
    }

    /// Sets one color-grading wheel (Shadows/Midtones/Highlights/Global);
    /// like `hsl_band`, composes onto the one `ColorGrade` leaf. `hue` is an
    /// **Oklab** angle (see the module docs' anchor table).
    fn grade_wheel(self, zone: GradeZone, hue: f32, sat: f32, lum: f32) -> DeltaBuilder {
        let mut cg = match self.0 .0.get(&ParamId::ColorGrade) {
            Some(ParamValue::ColorGrade(cg)) => *cg,
            _ => ColorGrade::default(),
        };
        let wheel = GradeWheel { hue, sat, lum };
        match zone {
            GradeZone::Shadows => cg.shadows = wheel,
            GradeZone::Midtones => cg.midtones = wheel,
            GradeZone::Highlights => cg.highlights = wheel,
            GradeZone::Global => cg.global = wheel,
        }
        self.set(ParamId::ColorGrade, ParamValue::ColorGrade(cg))
    }

    /// Zone blending (`0` = near-hard cut between shadow/mid/highlight
    /// zones, `100` = wide overlap) and balance (positive grows the shadow
    /// zone at the highlight zone's expense).
    fn grade_blend_balance(self, blend: f32, balance: f32) -> DeltaBuilder {
        let mut cg = match self.0 .0.get(&ParamId::ColorGrade) {
            Some(ParamValue::ColorGrade(cg)) => *cg,
            _ => ColorGrade::default(),
        };
        cg.blend = blend;
        cg.balance = balance;
        self.set(ParamId::ColorGrade, ParamValue::ColorGrade(cg))
    }

    /// A tone curve from `(x, y)` control points, `0.0..=1.0` each, x
    /// strictly increasing (`ToneCurve::validate` rejects anything else).
    fn curve(self, points: &[(f32, f32)]) -> DeltaBuilder {
        let mut set = crate::leaves::ToneCurveSet {
            rgb: ToneCurve {
                points: points.iter().map(|&(x, y)| CurvePoint { x, y }).collect(),
            },
            ..Default::default()
        };
        set.clamp_points();
        self.set(ParamId::ToneCurve, ParamValue::Curve(set))
    }

    /// Sets `Treatment::BlackAndWhite` + the 8-channel B&W mixer (same band
    /// order as `hsl_band`'s `RED..MAGENTA` consts).
    fn black_and_white(self, weights: [f32; 8]) -> DeltaBuilder {
        self.set(
            ParamId::Treatment,
            ParamValue::Treatment(Treatment::BlackAndWhite),
        )
        .set(ParamId::BwMix, ParamValue::BwMix(BwMix { weights }))
    }

    fn build(self) -> ParamDelta {
        self.0
    }
}

#[derive(Clone, Copy)]
enum GradeZone {
    Shadows,
    Midtones,
    Highlights,
    Global,
}

/// The starter library: original, provenance-clean develop presets across
/// seven look families (module docs). Stable order = library display order.
pub fn starter_presets() -> Vec<LibraryPreset> {
    vec![
        // ── Neon, wet streets under signage. The shared grammar: a cool
        // shadow wheel for the asphalt reflecting sky, a hot highlight wheel
        // for the tubes themselves, blacks pulled down but held off the clip
        // point by a lifted curve toe, and per-band HSL doing the saturation
        // work so only the sign colours gain. ────────────────────────────
        LibraryPreset::new(
            "Neon Rain",
            "Neon",
            DeltaBuilder::new()
                .tone(26.0, 26.0, -10.0, -16.0, -24.0)
                .grade_wheel(GradeZone::Shadows, 205.0, 38.0, -6.0)
                .grade_wheel(GradeZone::Highlights, 330.0, 26.0, -4.0)
                .grade_wheel(GradeZone::Global, 300.0, 6.0, 0.0)
                .grade_blend_balance(35.0, -10.0)
                .hsl_band(ORANGE, 0.0, -14.0, 0.0)
                .hsl_band(AQUA, 0.0, 22.0, 0.0)
                .hsl_band(BLUE, 0.0, 20.0, -8.0)
                .hsl_band(PURPLE, 0.0, 16.0, 0.0)
                .hsl_band(MAGENTA, 0.0, 24.0, 0.0)
                .presence(14.0, -4.0, 18.0)
                .curve(&[
                    (0.0, 0.02),
                    (0.18, 0.12),
                    (0.5, 0.5),
                    (0.8, 0.86),
                    (1.0, 0.96),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Sodium Vapour",
            "Neon",
            DeltaBuilder::new()
                .tone(22.0, 36.0, -6.0, -22.0, -18.0)
                .grade_wheel(GradeZone::Shadows, 268.0, 28.0, -5.0)
                .grade_wheel(GradeZone::Highlights, 72.0, 24.0, -5.0)
                .grade_blend_balance(45.0, 0.0)
                .hsl_band(ORANGE, 0.0, 8.0, 0.0)
                .hsl_band(YELLOW, -18.0, 10.0, 0.0)
                .hsl_band(AQUA, 0.0, -15.0, 0.0)
                .hsl_band(BLUE, 0.0, 18.0, -10.0)
                .presence(12.0, -6.0, 12.0)
                .curve(&[(0.0, 0.015), (0.25, 0.2), (0.72, 0.78), (1.0, 0.95)])
                .build(),
        ),
        LibraryPreset::new(
            "Chrome and Cyan",
            "Neon",
            DeltaBuilder::new()
                .tone(32.0, 28.0, -18.0, -14.0, -22.0)
                .grade_wheel(GradeZone::Shadows, 212.0, 44.0, -8.0)
                .grade_wheel(GradeZone::Midtones, 200.0, 20.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 190.0, 16.0, -4.0)
                .grade_blend_balance(30.0, 8.0)
                .hsl_band(RED, 0.0, 15.0, 0.0)
                .hsl_band(ORANGE, 0.0, -20.0, 0.0)
                .hsl_band(YELLOW, 0.0, -30.0, 0.0)
                .hsl_band(GREEN, 0.0, -25.0, 0.0)
                .hsl_band(AQUA, 0.0, 30.0, 5.0)
                .hsl_band(BLUE, 0.0, 25.0, 0.0)
                .presence(-6.0, -10.0, 26.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.22, 0.15),
                    (0.55, 0.58),
                    (0.8, 0.86),
                    (1.0, 0.96),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Midnight Arcade",
            "Neon",
            DeltaBuilder::new()
                .tone(26.0, 32.0, -12.0, -20.0, -28.0)
                .grade_wheel(GradeZone::Shadows, 288.0, 38.0, -7.0)
                .grade_wheel(GradeZone::Highlights, 320.0, 22.0, -4.0)
                .grade_wheel(GradeZone::Global, 300.0, 8.0, -2.0)
                .grade_blend_balance(40.0, -6.0)
                .hsl_band(YELLOW, 0.0, -25.0, 0.0)
                .hsl_band(GREEN, 0.0, -30.0, 0.0)
                .hsl_band(AQUA, 0.0, 12.0, 0.0)
                .hsl_band(BLUE, 20.0, 18.0, 0.0)
                .hsl_band(PURPLE, 0.0, 26.0, 2.0)
                .hsl_band(MAGENTA, 0.0, 22.0, 0.0)
                .presence(15.0, -5.0, 14.0)
                .curve(&[
                    (0.0, 0.03),
                    (0.2, 0.14),
                    (0.5, 0.5),
                    (0.78, 0.84),
                    (1.0, 0.95),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Wet Asphalt",
            "Neon",
            DeltaBuilder::new()
                .tone(34.0, 32.0, -4.0, -16.0, -26.0)
                .grade_wheel(GradeZone::Shadows, 210.0, 26.0, -6.0)
                .grade_wheel(GradeZone::Highlights, 60.0, 12.0, -3.0)
                .grade_blend_balance(55.0, 12.0)
                .hsl_band(ORANGE, 0.0, -12.0, 0.0)
                .hsl_band(GREEN, 0.0, -20.0, -10.0)
                .hsl_band(AQUA, 0.0, 10.0, -8.0)
                .hsl_band(BLUE, 0.0, 14.0, -12.0)
                .presence(4.0, -12.0, 32.0)
                .texture_dehaze(26.0, 10.0)
                .curve(&[(0.0, 0.0), (0.3, 0.22), (0.7, 0.78), (1.0, 0.95)])
                .build(),
        ),
        LibraryPreset::new(
            "Signage Bloom",
            "Neon",
            DeltaBuilder::new()
                .tone(-8.0, 20.0, 22.0, -20.0, 14.0)
                .grade_wheel(GradeZone::Shadows, 216.0, 30.0, 4.0)
                .grade_wheel(GradeZone::Highlights, 336.0, 22.0, 2.0)
                .grade_blend_balance(60.0, -14.0)
                .hsl_band(RED, 0.0, 18.0, 0.0)
                .hsl_band(AQUA, 0.0, 18.0, 0.0)
                .hsl_band(BLUE, 0.0, 14.0, 0.0)
                .hsl_band(MAGENTA, 0.0, 22.0, 0.0)
                .presence(18.0, -4.0, -14.0)
                .texture_dehaze(-8.0, -18.0)
                .curve(&[
                    (0.0, 0.06),
                    (0.28, 0.3),
                    (0.68, 0.74),
                    (0.86, 0.88),
                    (1.0, 0.94),
                ])
                .build(),
        ),
        // ── Retro, aged stock. Blacks lift into the paper, whites
        // compress before they reach 1.0, and the channels cross the way
        // dye layers do once they have faded at different rates. ─────────
        LibraryPreset::new(
            "Sunday Chrome",
            "Retro",
            DeltaBuilder::new()
                .tone(12.0, 26.0, 16.0, -20.0, 18.0)
                .grade_wheel(GradeZone::Shadows, 48.0, 20.0, 6.0)
                .grade_wheel(GradeZone::Highlights, 66.0, 12.0, -2.0)
                .grade_wheel(GradeZone::Global, 55.0, 6.0, 0.0)
                .grade_blend_balance(55.0, 5.0)
                .hsl_band(RED, -12.0, 10.0, 0.0)
                .hsl_band(ORANGE, 0.0, 12.0, 0.0)
                .hsl_band(YELLOW, 0.0, -22.0, 8.0)
                .hsl_band(GREEN, 25.0, -28.0, 6.0)
                .hsl_band(AQUA, 0.0, -18.0, 0.0)
                .hsl_band(BLUE, 0.0, -10.0, 5.0)
                .presence(-8.0, -6.0, -6.0)
                .curve(&[
                    (0.0, 0.08),
                    (0.25, 0.28),
                    (0.6, 0.66),
                    (0.84, 0.86),
                    (1.0, 0.93),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Instant Fade",
            "Retro",
            DeltaBuilder::new()
                .tone(-18.0, -12.0, 26.0, -20.0, 26.0)
                .grade_wheel(GradeZone::Shadows, 190.0, 22.0, 8.0)
                .grade_wheel(GradeZone::Highlights, 62.0, 20.0, 4.0)
                .grade_blend_balance(70.0, 0.0)
                .hsl_band(RED, 0.0, -12.0, 0.0)
                .hsl_band(ORANGE, 0.0, -8.0, 8.0)
                .hsl_band(YELLOW, 0.0, -20.0, 6.0)
                .hsl_band(GREEN, 0.0, -26.0, 0.0)
                .hsl_band(AQUA, 0.0, -14.0, 0.0)
                .hsl_band(BLUE, 0.0, -18.0, 6.0)
                .presence(-14.0, -18.0, -12.0)
                .curve(&[(0.0, 0.11), (0.3, 0.33), (0.65, 0.68), (1.0, 0.93)])
                .build(),
        ),
        LibraryPreset::new(
            "Super 8",
            "Retro",
            DeltaBuilder::new()
                .tone(18.0, 32.0, 10.0, -22.0, 6.0)
                .grade_wheel(GradeZone::Shadows, 40.0, 22.0, 2.0)
                .grade_wheel(GradeZone::Midtones, 110.0, 12.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 58.0, 16.0, -4.0)
                .grade_blend_balance(50.0, -8.0)
                .hsl_band(RED, 0.0, 12.0, -10.0)
                .hsl_band(ORANGE, 0.0, 10.0, 0.0)
                .hsl_band(YELLOW, 0.0, -14.0, -6.0)
                .hsl_band(GREEN, 0.0, -20.0, -6.0)
                .hsl_band(BLUE, 0.0, -22.0, 0.0)
                .presence(-4.0, -8.0, 8.0)
                .texture_dehaze(30.0, 0.0)
                .curve(&[
                    (0.0, 0.05),
                    (0.22, 0.24),
                    (0.62, 0.7),
                    (0.86, 0.88),
                    (1.0, 0.94),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Bell-Bottom Brown",
            "Retro",
            DeltaBuilder::new()
                .tone(10.0, 26.0, 14.0, -18.0, 12.0)
                .grade_wheel(GradeZone::Shadows, 44.0, 20.0, 4.0)
                .grade_wheel(GradeZone::Midtones, 60.0, 14.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 70.0, 14.0, -3.0)
                .grade_wheel(GradeZone::Global, 52.0, 6.0, 0.0)
                .grade_blend_balance(60.0, 6.0)
                .hsl_band(RED, 10.0, 8.0, -8.0)
                .hsl_band(ORANGE, 0.0, 20.0, -4.0)
                .hsl_band(YELLOW, -22.0, 16.0, -8.0)
                .hsl_band(GREEN, -25.0, -18.0, -10.0)
                .hsl_band(AQUA, 0.0, -30.0, 0.0)
                .hsl_band(BLUE, 0.0, -28.0, -6.0)
                .hsl_band(PURPLE, 0.0, -20.0, 0.0)
                .presence(-6.0, -4.0, 4.0)
                .curve(&[(0.0, 0.06), (0.3, 0.3), (0.7, 0.72), (1.0, 0.93)])
                .build(),
        ),
        LibraryPreset::new(
            "Expired Stock",
            "Retro",
            DeltaBuilder::new()
                .tone(6.0, -16.0, 20.0, -14.0, 20.0)
                .grade_wheel(GradeZone::Shadows, 320.0, 28.0, 6.0)
                .grade_wheel(GradeZone::Highlights, 100.0, 24.0, 4.0)
                .grade_blend_balance(65.0, -10.0)
                .hsl_band(RED, 0.0, -10.0, 6.0)
                .hsl_band(ORANGE, 20.0, -14.0, 0.0)
                .hsl_band(YELLOW, 14.0, 10.0, 6.0)
                .hsl_band(GREEN, 0.0, 8.0, 6.0)
                .hsl_band(AQUA, 0.0, -22.0, 0.0)
                .hsl_band(BLUE, 18.0, -12.0, 8.0)
                .hsl_band(MAGENTA, 0.0, 22.0, 0.0)
                .presence(-10.0, -6.0, -8.0)
                .curve(&[(0.0, 0.1), (0.26, 0.3), (0.62, 0.66), (1.0, 0.94)])
                .build(),
        ),
        LibraryPreset::new(
            "Warm Archive",
            "Retro",
            DeltaBuilder::new()
                .tone(8.0, 24.0, 16.0, -16.0, 12.0)
                .grade_wheel(GradeZone::Shadows, 46.0, 16.0, 4.0)
                .grade_wheel(GradeZone::Highlights, 56.0, 10.0, -2.0)
                .grade_blend_balance(60.0, 0.0)
                .hsl_band(RED, -8.0, 6.0, 0.0)
                .hsl_band(ORANGE, 0.0, -6.0, 6.0)
                .hsl_band(YELLOW, 0.0, -12.0, 0.0)
                .hsl_band(GREEN, 0.0, -16.0, 4.0)
                .hsl_band(BLUE, 0.0, -8.0, 0.0)
                .presence(10.0, -6.0, -8.0)
                .curve(&[(0.0, 0.05), (0.3, 0.32), (0.72, 0.76), (1.0, 0.94)])
                .build(),
        ),
        // ── Verdant, the green axis. Green and aqua are pushed with HSL
        // (which runs before the wheels) so the *material* goes green;
        // orange/red are drained by the same mechanism so skin reads as
        // unwell rather than merely tinted. ──────────────────────────────
        LibraryPreset::new(
            "Verdigris",
            "Verdant",
            DeltaBuilder::new()
                .tone(20.0, 22.0, 4.0, -12.0, -12.0)
                .grade_wheel(GradeZone::Shadows, 196.0, 36.0, -4.0)
                .grade_wheel(GradeZone::Midtones, 160.0, 20.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 150.0, 16.0, 3.0)
                .grade_blend_balance(45.0, 0.0)
                .hsl_band(RED, 0.0, -10.0, 0.0)
                .hsl_band(ORANGE, 0.0, -18.0, 0.0)
                .hsl_band(YELLOW, 25.0, -20.0, 0.0)
                .hsl_band(GREEN, 18.0, 20.0, -6.0)
                .hsl_band(AQUA, 0.0, 26.0, 0.0)
                .hsl_band(BLUE, 0.0, 10.0, 0.0)
                .presence(0.0, -6.0, 14.0)
                .curve(&[(0.0, 0.02), (0.28, 0.24), (0.72, 0.8), (1.0, 1.0)])
                .build(),
        ),
        LibraryPreset::new(
            "Absinthe",
            "Verdant",
            DeltaBuilder::new()
                .tone(26.0, 26.0, -8.0, -14.0, -18.0)
                .grade_wheel(GradeZone::Shadows, 128.0, 30.0, -6.0)
                .grade_wheel(GradeZone::Midtones, 116.0, 24.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 104.0, 20.0, 4.0)
                .grade_blend_balance(40.0, 4.0)
                .hsl_band(RED, 0.0, -22.0, 0.0)
                .hsl_band(ORANGE, 0.0, -26.0, -8.0)
                .hsl_band(YELLOW, 0.0, 16.0, 6.0)
                .hsl_band(GREEN, 0.0, 26.0, 8.0)
                .hsl_band(AQUA, 0.0, -10.0, 0.0)
                .hsl_band(BLUE, 0.0, -30.0, -12.0)
                .hsl_band(MAGENTA, 0.0, -20.0, 0.0)
                .presence(-8.0, -12.0, 22.0)
                .curve(&[
                    (0.0, 0.02),
                    (0.22, 0.16),
                    (0.55, 0.56),
                    (0.82, 0.9),
                    (1.0, 1.0),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Moss and Slate",
            "Verdant",
            DeltaBuilder::new()
                .tone(16.0, -26.0, 12.0, -6.0, -8.0)
                .grade_wheel(GradeZone::Shadows, 146.0, 26.0, -4.0)
                .grade_wheel(GradeZone::Highlights, 232.0, 14.0, 3.0)
                .grade_blend_balance(60.0, 10.0)
                .hsl_band(RED, 0.0, -12.0, 0.0)
                .hsl_band(ORANGE, 0.0, -14.0, 0.0)
                .hsl_band(YELLOW, 26.0, -18.0, -8.0)
                .hsl_band(GREEN, 0.0, -8.0, -10.0)
                .hsl_band(AQUA, 0.0, 12.0, -6.0)
                .hsl_band(BLUE, 0.0, 8.0, -6.0)
                .presence(-6.0, -10.0, 20.0)
                .texture_dehaze(26.0, 0.0)
                .curve(&[(0.0, 0.02), (0.3, 0.26), (0.7, 0.76), (1.0, 0.98)])
                .build(),
        ),
        LibraryPreset::new(
            "Emerald Night",
            "Verdant",
            DeltaBuilder::new()
                .tone(32.0, 28.0, -16.0, -16.0, -24.0)
                .grade_wheel(GradeZone::Shadows, 152.0, 40.0, -8.0)
                .grade_wheel(GradeZone::Highlights, 186.0, 26.0, 5.0)
                .grade_wheel(GradeZone::Global, 165.0, 10.0, -2.0)
                .grade_blend_balance(32.0, -6.0)
                .hsl_band(RED, 0.0, -16.0, 0.0)
                .hsl_band(ORANGE, 0.0, -22.0, 0.0)
                .hsl_band(YELLOW, 0.0, -20.0, 0.0)
                .hsl_band(GREEN, 0.0, 28.0, 0.0)
                .hsl_band(AQUA, 0.0, 24.0, 4.0)
                .hsl_band(BLUE, 0.0, 12.0, -10.0)
                .presence(8.0, -4.0, 20.0)
                .curve(&[
                    (0.0, 0.015),
                    (0.2, 0.13),
                    (0.5, 0.5),
                    (0.82, 0.9),
                    (1.0, 1.0),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Olive Drab",
            "Verdant",
            DeltaBuilder::new()
                .tone(6.0, -14.0, 18.0, -14.0, 10.0)
                .grade_wheel(GradeZone::Shadows, 96.0, 20.0, 4.0)
                .grade_wheel(GradeZone::Midtones, 104.0, 16.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 84.0, 14.0, 2.0)
                .grade_blend_balance(65.0, 0.0)
                .hsl_band(RED, 0.0, -18.0, 0.0)
                .hsl_band(ORANGE, 0.0, -16.0, 0.0)
                .hsl_band(YELLOW, -12.0, -14.0, -8.0)
                .hsl_band(GREEN, -20.0, -20.0, -6.0)
                .hsl_band(AQUA, 0.0, -24.0, 0.0)
                .hsl_band(BLUE, 0.0, -26.0, -6.0)
                .hsl_band(MAGENTA, 0.0, -22.0, 0.0)
                .presence(-16.0, -14.0, 6.0)
                .curve(&[(0.0, 0.07), (0.32, 0.32), (0.7, 0.72), (1.0, 0.95)])
                .build(),
        ),
        // ── Atmosphere, weather, not subject. Negative dehaze puts the
        // air back into the frame; the curve's toe and shoulder do the rest.
        // "Cold Front" is the deliberate inverse: air cut, not added. ────
        LibraryPreset::new(
            "Harbour Mist",
            "Atmosphere",
            DeltaBuilder::new()
                .tone(-22.0, -16.0, 24.0, -18.0, 22.0)
                .grade_wheel(GradeZone::Shadows, 214.0, 22.0, 8.0)
                .grade_wheel(GradeZone::Midtones, 206.0, 12.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 200.0, 10.0, 4.0)
                .grade_blend_balance(75.0, 0.0)
                .hsl_band(RED, 0.0, -12.0, 0.0)
                .hsl_band(ORANGE, 0.0, -14.0, 0.0)
                .hsl_band(YELLOW, 0.0, -20.0, 0.0)
                .hsl_band(GREEN, 0.0, -22.0, 6.0)
                .hsl_band(AQUA, 0.0, -10.0, 8.0)
                .hsl_band(BLUE, 0.0, -14.0, 8.0)
                .presence(-12.0, -14.0, -18.0)
                .texture_dehaze(-10.0, -26.0)
                .curve(&[(0.0, 0.12), (0.32, 0.36), (0.68, 0.7), (1.0, 0.92)])
                .build(),
        ),
        LibraryPreset::new(
            "Monsoon",
            "Atmosphere",
            DeltaBuilder::new()
                .tone(14.0, -28.0, 10.0, -8.0, -6.0)
                .grade_wheel(GradeZone::Shadows, 222.0, 30.0, -3.0)
                .grade_wheel(GradeZone::Highlights, 204.0, 14.0, 2.0)
                .grade_blend_balance(55.0, 12.0)
                .hsl_band(ORANGE, 0.0, -16.0, -6.0)
                .hsl_band(YELLOW, 0.0, -22.0, 0.0)
                .hsl_band(GREEN, 0.0, -24.0, -8.0)
                .hsl_band(AQUA, 0.0, 10.0, -8.0)
                .hsl_band(BLUE, 0.0, 14.0, -10.0)
                .presence(-4.0, -8.0, 22.0)
                .texture_dehaze(24.0, -10.0)
                .curve(&[(0.0, 0.04), (0.28, 0.26), (0.66, 0.72), (1.0, 0.96)])
                .build(),
        ),
        LibraryPreset::new(
            "Smog Halo",
            "Atmosphere",
            DeltaBuilder::new()
                .tone(-14.0, 16.0, 20.0, -18.0, 16.0)
                .grade_wheel(GradeZone::Shadows, 40.0, 16.0, 6.0)
                .grade_wheel(GradeZone::Midtones, 62.0, 12.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 74.0, 22.0, 6.0)
                .grade_blend_balance(70.0, -8.0)
                .hsl_band(RED, 0.0, -14.0, 0.0)
                .hsl_band(ORANGE, 0.0, -6.0, 8.0)
                .hsl_band(YELLOW, 0.0, -10.0, 6.0)
                .hsl_band(GREEN, 0.0, -30.0, 4.0)
                .hsl_band(AQUA, 0.0, -28.0, 0.0)
                .hsl_band(BLUE, 0.0, -24.0, 6.0)
                .presence(-16.0, -12.0, -16.0)
                .texture_dehaze(-14.0, -22.0)
                .curve(&[
                    (0.0, 0.1),
                    (0.3, 0.34),
                    (0.66, 0.72),
                    (0.88, 0.92),
                    (1.0, 0.95),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Cold Front",
            "Atmosphere",
            DeltaBuilder::new()
                .tone(22.0, 26.0, 8.0, -16.0, -10.0)
                .grade_wheel(GradeZone::Shadows, 236.0, 20.0, -4.0)
                .grade_wheel(GradeZone::Highlights, 214.0, 10.0, 0.0)
                .grade_blend_balance(50.0, 0.0)
                .hsl_band(RED, 0.0, -8.0, 0.0)
                .hsl_band(ORANGE, 0.0, -12.0, 4.0)
                .hsl_band(YELLOW, 0.0, -16.0, 6.0)
                .hsl_band(GREEN, 0.0, -18.0, 0.0)
                .hsl_band(AQUA, 0.0, 14.0, 6.0)
                .hsl_band(BLUE, 0.0, 16.0, 4.0)
                .presence(6.0, -6.0, 12.0)
                .texture_dehaze(14.0, 12.0)
                .curve(&[(0.0, 0.0), (0.26, 0.22), (0.7, 0.76), (1.0, 0.95)])
                .build(),
        ),
        LibraryPreset::new(
            "Fog Bank",
            "Atmosphere",
            DeltaBuilder::new()
                .tone(-34.0, -10.0, 30.0, -26.0, 34.0)
                .grade_wheel(GradeZone::Shadows, 208.0, 18.0, 10.0)
                .grade_wheel(GradeZone::Highlights, 198.0, 8.0, 5.0)
                .grade_blend_balance(85.0, 0.0)
                .hsl_band(RED, 0.0, -26.0, 0.0)
                .hsl_band(ORANGE, 0.0, -24.0, 0.0)
                .hsl_band(YELLOW, 0.0, -30.0, 0.0)
                .hsl_band(GREEN, 0.0, -34.0, 0.0)
                .hsl_band(AQUA, 0.0, -18.0, 0.0)
                .hsl_band(BLUE, 0.0, -20.0, 10.0)
                .hsl_band(PURPLE, 0.0, -26.0, 0.0)
                .hsl_band(MAGENTA, 0.0, -28.0, 0.0)
                .presence(-20.0, -26.0, -24.0)
                .texture_dehaze(-20.0, -34.0)
                .curve(&[(0.0, 0.18), (0.35, 0.42), (0.7, 0.72), (1.0, 0.88)])
                .build(),
        ),
        // ── Edge, the aggressive end. Hard S-curves with a near-zero toe,
        // saturation controlled per band (never a global slam), and clarity
        // pushed past polite. "Hard Cut" deliberately states no colour
        // opinion at all so it stacks under any of the others. ───────────
        LibraryPreset::new(
            "Bleach District",
            "Edge",
            DeltaBuilder::new()
                .tone(46.0, 32.0, -24.0, -14.0, -28.0)
                .grade_wheel(GradeZone::Shadows, 220.0, 16.0, -6.0)
                .grade_wheel(GradeZone::Highlights, 210.0, 8.0, -4.0)
                .grade_blend_balance(40.0, 6.0)
                .hsl_band(RED, 0.0, -34.0, 0.0)
                .hsl_band(ORANGE, 0.0, -38.0, -6.0)
                .hsl_band(YELLOW, 0.0, -40.0, 0.0)
                .hsl_band(GREEN, 0.0, -44.0, 0.0)
                .hsl_band(AQUA, 0.0, -30.0, 0.0)
                .hsl_band(BLUE, 0.0, -22.0, 0.0)
                .hsl_band(PURPLE, 0.0, -32.0, 0.0)
                .hsl_band(MAGENTA, 0.0, -34.0, 0.0)
                .presence(-10.0, -30.0, 34.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.18, 0.08),
                    (0.5, 0.5),
                    (0.8, 0.88),
                    (1.0, 0.96),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Concrete Brutal",
            "Edge",
            DeltaBuilder::new()
                .tone(42.0, 36.0, -22.0, -16.0, -32.0)
                .grade_wheel(GradeZone::Shadows, 228.0, 18.0, -8.0)
                .grade_wheel(GradeZone::Highlights, 40.0, 10.0, -4.0)
                .grade_blend_balance(45.0, 10.0)
                .hsl_band(RED, 0.0, -14.0, 0.0)
                .hsl_band(ORANGE, 0.0, -20.0, 0.0)
                .hsl_band(YELLOW, 0.0, -26.0, 0.0)
                .hsl_band(GREEN, 0.0, -30.0, -12.0)
                .hsl_band(AQUA, 0.0, -12.0, -10.0)
                .hsl_band(BLUE, 0.0, 10.0, -12.0)
                .presence(-6.0, -18.0, 38.0)
                .texture_dehaze(32.0, 12.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.2, 0.1),
                    (0.52, 0.52),
                    (0.8, 0.88),
                    (1.0, 0.95),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Scorched",
            "Edge",
            DeltaBuilder::new()
                .tone(44.0, 40.0, -26.0, -26.0, -30.0)
                .grade_wheel(GradeZone::Shadows, 20.0, 22.0, -10.0)
                .grade_wheel(GradeZone::Midtones, 36.0, 12.0, 0.0)
                .grade_wheel(GradeZone::Highlights, 48.0, 14.0, -10.0)
                .grade_blend_balance(35.0, -8.0)
                .hsl_band(RED, 0.0, 4.0, -10.0)
                .hsl_band(ORANGE, 0.0, 6.0, -6.0)
                .hsl_band(YELLOW, -20.0, 10.0, 0.0)
                .hsl_band(GREEN, 0.0, -34.0, -14.0)
                .hsl_band(AQUA, 0.0, -32.0, 0.0)
                .hsl_band(BLUE, 0.0, -28.0, -16.0)
                .presence(6.0, -8.0, 30.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.16, 0.06),
                    (0.5, 0.5),
                    (0.8, 0.9),
                    (1.0, 0.95),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Hard Cut",
            "Edge",
            DeltaBuilder::new()
                .tone(56.0, 40.0, -28.0, -24.0, -34.0)
                .presence(4.0, -8.0, 36.0)
                .texture_dehaze(30.0, 0.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.15, 0.04),
                    (0.5, 0.5),
                    (0.78, 0.84),
                    (1.0, 0.93),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Static Burn",
            "Edge",
            DeltaBuilder::new()
                .tone(38.0, 38.0, -16.0, -22.0, -24.0)
                .grade_wheel(GradeZone::Shadows, 192.0, 38.0, -8.0)
                .grade_wheel(GradeZone::Highlights, 96.0, 28.0, -6.0)
                // Near-hard zone cut (blend 25) so the cross-process split
                // reads as two colours, not as one muddy average.
                .grade_blend_balance(25.0, 0.0)
                // The clash is carried by per-band HSL (multiplicative on
                // chroma, so neutrals are left alone) and by the split-tone
                // wheels, never by global Saturation, which would drag the
                // pale flat regions up with everything else.
                .hsl_band(RED, 0.0, 20.0, 0.0)
                .hsl_band(ORANGE, 0.0, -20.0, 0.0)
                .hsl_band(YELLOW, 0.0, 20.0, 2.0)
                .hsl_band(GREEN, 0.0, 14.0, 0.0)
                .hsl_band(AQUA, 0.0, 26.0, 0.0)
                .hsl_band(BLUE, 0.0, 20.0, -12.0)
                .hsl_band(MAGENTA, 0.0, 22.0, 0.0)
                .presence(14.0, 0.0, 26.0)
                .curve(&[
                    (0.0, 0.02),
                    (0.2, 0.12),
                    (0.5, 0.52),
                    (0.76, 0.8),
                    (1.0, 0.93),
                ])
                .build(),
        ),
        // ── Mono, `Treatment::BlackAndWhite` + an 8-channel `BwMix` chosen
        // for character. The mixer is chroma-gated (`MIX_CHROMA_SCALE`), so
        // saturation is left at identity: draining chroma upstream would
        // only disarm the mixer. ─────────────────────────────────────────
        LibraryPreset::new(
            "Street Hard",
            "Mono",
            DeltaBuilder::new()
                .tone(52.0, 32.0, -26.0, -18.0, -30.0)
                .black_and_white([-25.0, -35.0, -10.0, -20.0, -45.0, -70.0, -40.0, -20.0])
                .presence(0.0, 0.0, 32.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.16, 0.06),
                    (0.5, 0.5),
                    (0.84, 0.95),
                    (1.0, 1.0),
                ])
                .build(),
        ),
        LibraryPreset::new(
            "Silver Soft",
            "Mono",
            DeltaBuilder::new()
                .tone(-12.0, -14.0, 22.0, -14.0, 18.0)
                .black_and_white([15.0, 30.0, 20.0, 5.0, -10.0, -25.0, -5.0, 10.0])
                .presence(0.0, 0.0, -14.0)
                .curve(&[(0.0, 0.08), (0.3, 0.34), (0.7, 0.74), (1.0, 0.96)])
                .build(),
        ),
        LibraryPreset::new(
            "Infrared Bloom",
            "Mono",
            DeltaBuilder::new()
                .tone(28.0, 24.0, 8.0, -12.0, -14.0)
                .black_and_white([70.0, 60.0, 80.0, 90.0, -60.0, -85.0, -30.0, 40.0])
                .presence(0.0, 0.0, 10.0)
                .curve(&[(0.0, 0.02), (0.26, 0.24), (0.7, 0.82), (1.0, 1.0)])
                .build(),
        ),
        LibraryPreset::new(
            "Ash Print",
            "Mono",
            DeltaBuilder::new()
                .tone(-8.0, -18.0, 16.0, -22.0, 26.0)
                .black_and_white([-10.0, 5.0, 10.0, -5.0, -15.0, -30.0, -10.0, -5.0])
                .presence(0.0, 0.0, -6.0)
                .curve(&[(0.0, 0.14), (0.32, 0.36), (0.68, 0.7), (1.0, 0.9)])
                .build(),
        ),
        LibraryPreset::new(
            "Noir Grain",
            "Mono",
            DeltaBuilder::new()
                .tone(46.0, 28.0, -24.0, -16.0, -28.0)
                .black_and_white([-45.0, -20.0, 5.0, -30.0, -50.0, -65.0, -55.0, -40.0])
                .presence(0.0, 0.0, 24.0)
                .texture_dehaze(32.0, 0.0)
                // Medium-fine, fairly rough: the pushed Tri-X look the name
                // promises. Strong enough to read at print size, not so strong
                // it eats the tonal work above.
                .grain(35.0, 30.0, 60.0)
                .curve(&[
                    (0.0, 0.0),
                    (0.2, 0.1),
                    (0.55, 0.58),
                    (0.86, 0.96),
                    (1.0, 1.0),
                ])
                .build(),
        ),
        // ── Tonal, shape only, no colour opinion. These are the bases the
        // colour families are built on; they stay useful precisely because
        // they say nothing about hue. ────────────────────────────────────
        LibraryPreset::new(
            "Anvil",
            "Tonal",
            DeltaBuilder::new()
                .tone(42.0, 30.0, -12.0, -16.0, -22.0)
                .build(),
        ),
        LibraryPreset::new(
            "Chalk Lift",
            "Tonal",
            DeltaBuilder::new()
                .tone(-16.0, -22.0, 30.0, -22.0, 26.0)
                .build(),
        ),
        LibraryPreset::new(
            "Glasshouse",
            "Tonal",
            DeltaBuilder::new().tone(-4.0, 8.0, 24.0, 22.0, 4.0).build(),
        ),
        LibraryPreset::new(
            "Iron Curve",
            "Tonal",
            DeltaBuilder::new()
                .tone(10.0, 24.0, 12.0, -16.0, -8.0)
                .curve(&[
                    (0.0, 0.03),
                    (0.24, 0.2),
                    (0.5, 0.5),
                    (0.76, 0.8),
                    (1.0, 0.94),
                ])
                .build(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library is well-formed: in range, no no-op deltas, unique
    /// `(group, name)` keys (the store's own file-naming discipline
    /// `PresetStore::preset_path` keys on that pair).
    #[test]
    fn starter_presets_are_well_formed() {
        let presets = starter_presets();
        assert!(
            (28..=40).contains(&presets.len()),
            "expected 28-40 starter presets, got {}",
            presets.len()
        );
        for p in &presets {
            assert!(!p.delta.0.is_empty(), "{}: empty delta", p.name);
            assert!(!p.subset().groups.is_empty(), "{}: empty subset", p.name);
            assert!(!p.name.is_empty());
            assert!(!p.group.is_empty());
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &presets {
            assert!(
                seen.insert((p.group, p.name)),
                "duplicate (group, name): {:?}",
                (p.group, p.name)
            );
        }
    }

    /// Every family is populated deeply enough to browse as one dropdown
    /// (4-8 members), the grouping is the library's navigation surface.
    #[test]
    fn every_family_is_a_browsable_size() {
        let presets = starter_presets();
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for p in &presets {
            *counts.entry(p.group).or_default() += 1;
        }
        assert!(counts.len() >= 5, "expected at least 5 families");
        for (group, n) in &counts {
            assert!(
                (4..=8).contains(n),
                "family {group:?} has {n} presets, outside the 4-8 browsable range"
            );
        }
    }

    /// The Mono family sets BOTH `Treatment::BlackAndWhite` and a non-zero
    /// `BwMix`, the "set treatment flag + tone/curve" DELIVER requirement.
    /// It must also leave `Saturation` at identity: `global.bw_mix` is
    /// chroma-gated and runs downstream of `global.vibrance_sat`, so a
    /// negative saturation would silently disarm the mixer.
    #[test]
    fn mono_family_sets_treatment_and_mixer_without_draining_chroma() {
        let presets = starter_presets();
        let bw: Vec<_> = presets.iter().filter(|p| p.group == "Mono").collect();
        assert_eq!(bw.len(), 5, "expected 5 Mono presets");
        for p in bw {
            assert_eq!(
                p.delta.0.get(&ParamId::Treatment),
                Some(&ParamValue::Treatment(Treatment::BlackAndWhite))
            );
            match p.delta.0.get(&ParamId::BwMix) {
                Some(ParamValue::BwMix(mix)) => {
                    assert!(
                        mix.weights.iter().any(|w| *w != 0.0),
                        "{}: all-zero BwMix",
                        p.name
                    );
                }
                other => panic!("{}: expected a BwMix value, got {other:?}", p.name),
            }
            if let Some(ParamValue::F32(sat)) = p.delta.0.get(&ParamId::Saturation) {
                assert_eq!(
                    *sat, 0.0,
                    "{}: Mono preset drains chroma before the mixer",
                    p.name
                );
            }
        }
    }

    /// Every authored value survives `Recipe::apply`'s clamp/validate
    /// unchanged, i.e. nothing below is silently folded back into range
    /// (out-of-domain grade hues, non-monotone curves, over-range sliders).
    #[test]
    fn every_preset_applies_without_being_clamped() {
        use crate::params::ParamSubset;
        use crate::Recipe;
        use lightbox_types::PV_M0;

        for p in starter_presets() {
            let mut recipe = Recipe::identity(PV_M0);
            recipe
                .apply(&p.delta)
                .unwrap_or_else(|e| panic!("{}: delta rejected: {e:?}", p.name));
            let subset = ParamSubset::from_groups(p.delta.0.keys().copied().map(group_of));
            let round = recipe.extract(&subset);
            for (id, want) in &p.delta.0 {
                let got = round.0.get(id).unwrap_or_else(|| {
                    panic!(
                        "{}: {id:?} did not survive the apply/extract round-trip",
                        p.name
                    )
                });
                assert_eq!(got, want, "{}: {id:?} was clamped on apply", p.name);
            }
        }
    }
}
