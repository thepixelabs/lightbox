// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The Lightbox default look family (§1.7 tier 2, spec §3.4, §5.2). **Owner:
//! Phase E (E1–E7).**
//!
//! This module owns four things:
//!
//! 1. **The `.lblook` file format v1** — a magic-tagged, versioned CBOR
//!    container ([`LBLOOK_MAGIC`] + [`LBLOOK_VERSION`] header, CBOR body) with a
//!    memory-safe [`load_look`] loader/validator and [`Look::to_bytes`] writer
//!    (E1). A look's [`ProfileId`] is the `xxh3-128` of its canonical
//!    serialization, so identical content always yields the identical id.
//! 2. **The look evaluator** ([`Look::eval`]) with `0..=200 %` amount semantics
//!    (E2): `amount = 0` is **bit-exact identity**, `< 100 %` is an
//!    identity-lerp, `> 100 %` is bounded linear extrapolation, output clamped
//!    to the valid range. The same amount rule the B8 resolve
//!    ([`crate::transform::resolve_input_transform`]) bakes into the
//!    GPU-upload form.
//! 3. **The authored looks** — [`author_lightbox_color_v1`] (the original
//!    scene-referred default, §1.7 tier 2) and [`author_lightbox_neutral`]
//!    (identity). These are the source of truth the committed
//!    `assets/color/looks/*.lblook` assets are generated from (E4/E6); their
//!    provenance carries the **no-Adobe-derived-data affidavit** (E5).
//! 4. **The authoring harness data** — a deterministic synthetic
//!    [`scene_corpus`] and [`render_contact_sheet`] (base vs look) that the
//!    `lightbox-cli look-dev` contact sheet and the E7 look golden ride on
//!    (E3/E7).
//!
//! # E09 default (documented seam)
//!
//! The recipe default a raw source resolves to is `look_ref =
//! "Lightbox Color v1"`, `look_amount =` [`DEFAULT_LOOK_AMOUNT`] (`= 1.0`). Per
//! spec §5.3 the default look applies to **raw sources only** — non-raw sources
//! already carry a rendered look, so [`default_look_applies`] returns `false`
//! for them and E10/E09 pass `look = None` to
//! [`crate::transform::resolve_input_transform`] in that case.

pub use crate::error::LookError;
use crate::lut::{HueSatEncoding, HueSatLut, HueSatTable};
use crate::matrix::{spaces, Mat3, Spline1D, Vec3};
use crate::profile::ProfileId;
use crate::transform::{apply_huesat, working_linear_to_srgb8};

/// Magic prefix of a `.lblook` container (`b"LBLK"`).
pub const LBLOOK_MAGIC: [u8; 4] = *b"LBLK";
/// The `.lblook` format version this build reads and writes.
pub const LBLOOK_VERSION: u16 = 1;

/// The default look's authored name (the `ProfileRef.look_ref` E09 resolves for
/// raw sources — spec §3.4 / task E6).
pub const DEFAULT_LOOK_NAME: &str = "Lightbox Color v1";
/// The default look amount (`1.0` = authored strength — spec task E6).
pub const DEFAULT_LOOK_AMOUNT: f32 = 1.0;

/// The largest HueSat delta cube a `.lblook` may declare (untrusted-input
/// hygiene: bundled today, user-installable tomorrow — never allocate on an
/// attacker-chosen dims product).
const MAX_HUE_SAT_NODES: usize = 1 << 20;

/// Provenance block for a look: author, license, review record — manifest
/// checked (spec §3.4/§4.3; **no Adobe-derived data at any authoring step**).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LookProvenance {
    /// Authoring entity (e.g. `"lightbox-authored"`).
    pub author: String,
    /// License (e.g. `"project"`).
    pub license: String,
    /// Perceptual-review record id (E5), when present.
    pub review_record: Option<String>,
}

/// A Lightbox look loaded from a versioned `.lblook` (spec §3.4).
#[derive(Clone, Debug)]
pub struct Look {
    /// Content id (`xxh3-128` of the canonical `.lblook` bytes).
    pub id: ProfileId,
    /// Display name.
    pub name: String,
    /// `.lblook` format version.
    pub version: u16,
    /// Scene-referred tone curve, applied per channel in working-space linear
    /// RGB (identity for "Lightbox Neutral").
    pub tone_curve: Spline1D,
    /// Optional hue/sat shaping (applied after the tone curve, via HSV).
    pub hue_sat: Option<HueSatLut>,
    /// Provenance.
    pub provenance: LookProvenance,
}

/// The CBOR body of a `.lblook` v1 (everything after the magic + version
/// header). The [`ProfileId`] is **derived** from the serialized bytes, never
/// stored, so a round-trip is byte-stable and the id cannot drift from content.
#[derive(serde::Serialize, serde::Deserialize)]
struct LookBodyV1 {
    name: String,
    tone_curve: Spline1D,
    hue_sat: Option<HueSatLut>,
    provenance: LookProvenance,
}

impl Look {
    /// Serializes this look to `.lblook` v1 bytes: `[b"LBLK"][version: u16 LE]
    /// [CBOR body]`. The `id` field is not written — [`load_look`] recomputes it
    /// from the canonical bytes, so `to_bytes` ∘ `load_look` is the identity on
    /// `(name, version, tone_curve, hue_sat, provenance)` and yields a stable
    /// `id` (E1 round-trip AC).
    pub fn to_bytes(&self) -> Result<Vec<u8>, LookError> {
        let body = LookBodyV1 {
            name: self.name.clone(),
            tone_curve: self.tone_curve.clone(),
            hue_sat: self.hue_sat.clone(),
            provenance: self.provenance.clone(),
        };
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(&LBLOOK_MAGIC);
        out.extend_from_slice(&LBLOOK_VERSION.to_le_bytes());
        ciborium::into_writer(&body, &mut out)
            .map_err(|e| LookError::Malformed(format!("CBOR encode failed: {e}")))?;
        Ok(out)
    }

    /// Reference look evaluator with `0..=200 %` amount semantics (spec §3.4,
    /// task E2). Input and output are **working-space linear RGB**.
    ///
    /// * `amount == 0.0` ⇒ **bit-exact identity** (the input is returned
    ///   unchanged, short-circuiting every stage so no float round-trip can
    ///   perturb it).
    /// * `0 < amount < 1` ⇒ identity-lerp: `x + amount·(authored(x) − x)`.
    /// * `1 ≤ amount ≤ 2` ⇒ bounded linear extrapolation of the same lerp.
    /// * Tone-curve output is clamped to `[0, 1]` (the curve's domain); shaping
    ///   sat/value scales are floored at `0`.
    ///
    /// Stage order matches [`crate::transform::ResolvedInputTransform::eval_cpu`]
    /// (§5.2): tone curve (per channel) → hue/sat shaping (via HSV). The B8
    /// resolve bakes the identical amount rule into its sampled, GPU-upload
    /// form; [`crate::transform`]'s tests pin the two paths in agreement.
    #[must_use]
    pub fn eval(&self, rgb: [f32; 3], amount: f32) -> [f32; 3] {
        let a = amount.clamp(0.0, 2.0);
        // amount = 0 is the identity — bit-exact, no stages, no HSV round-trip.
        if a == 0.0 {
            return rgb;
        }
        let tone = |x: f32| -> f32 {
            let y = self.tone_curve.eval(x);
            (x + a * (y - x)).clamp(0.0, 1.0)
        };
        let mut out = [tone(rgb[0]), tone(rgb[1]), tone(rgb[2])];
        if let Some(hs) = &self.hue_sat {
            let table = resolve_look_hue_sat(hs, a);
            out = apply_huesat(&table, out);
        }
        out
    }
}

/// Amount-scales a look's hue/sat shaping toward identity and resolves it to the
/// upload-ready [`HueSatTable`] form (task E2). The per-node identity is
/// `[Δhue = 0°, sat× = 1, val× = 1]`; each node is lerped from identity by
/// `amount` (`> 1` extrapolates), so `amount = 0` yields an exact identity
/// table. Called by both [`Look::eval`] and the B8 resolve so the reference and
/// GPU-upload paths share one amount rule.
///
/// Looks author their shaping with [`HueSatEncoding::Linear`] (see
/// [`author_lightbox_color_v1`]); the resolved table indexes on a linear value
/// coordinate, so no encoding remap is needed here.
pub(crate) fn resolve_look_hue_sat(lut: &HueSatLut, amount: f32) -> HueSatTable {
    let a = amount.clamp(0.0, 2.0);
    let deltas = lut
        .deltas
        .iter()
        .map(|d| {
            [
                a * d[0],
                (1.0 + a * (d[1] - 1.0)).max(0.0),
                (1.0 + a * (d[2] - 1.0)).max(0.0),
            ]
        })
        .collect();
    HueSatTable {
        dims: lut.dims,
        deltas,
    }
}

/// Loads + validates a `.lblook` container (spec §3.4, task E1). Memory-safe and
/// panic-free over arbitrary input (`.lblook` is bundled today but
/// user-installable content by design).
///
/// Errors:
/// * [`LookError::Malformed`] — short buffer, bad magic, CBOR decode failure,
///   or a structural violation (non-ascending tone-curve x, a HueSat cube whose
///   `deltas` length disagrees with `dims`, or a dims product over
///   [`MAX_HUE_SAT_NODES`]).
/// * [`LookError::UnsupportedVersion`] — the header version is newer than
///   [`LBLOOK_VERSION`].
pub fn load_look(bytes: &[u8]) -> Result<Look, LookError> {
    if bytes.len() < 6 {
        return Err(LookError::Malformed(
            "shorter than the 6-byte header".into(),
        ));
    }
    if bytes[0..4] != LBLOOK_MAGIC {
        return Err(LookError::Malformed("bad magic (not a .lblook)".into()));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != LBLOOK_VERSION {
        return Err(LookError::UnsupportedVersion(version));
    }
    let body: LookBodyV1 = ciborium::from_reader(&bytes[6..])
        .map_err(|e| LookError::Malformed(format!("CBOR decode failed: {e}")))?;

    validate_tone_curve(&body.tone_curve)?;
    if let Some(hs) = &body.hue_sat {
        validate_hue_sat(hs)?;
    }

    // Content id = xxh3-128 over the full canonical serialization.
    let id = ProfileId(twox_hash::XxHash3_128::oneshot(bytes).to_le_bytes());

    Ok(Look {
        id,
        name: body.name,
        version,
        tone_curve: body.tone_curve,
        hue_sat: body.hue_sat,
        provenance: body.provenance,
    })
}

/// Rejects a tone curve whose control points are empty or not strictly
/// ascending in `x` (the [`Spline1D`] evaluator assumes ascending `x`).
fn validate_tone_curve(curve: &Spline1D) -> Result<(), LookError> {
    let pts = &curve.control_points;
    if pts.is_empty() {
        return Err(LookError::Malformed(
            "tone curve has no control points".into(),
        ));
    }
    for w in pts.windows(2) {
        if w[0][0] >= w[1][0] {
            return Err(LookError::Malformed(
                "tone-curve control points must ascend strictly in x".into(),
            ));
        }
    }
    Ok(())
}

/// Rejects a HueSat cube whose declared dims overflow the node cap or disagree
/// with the `deltas` length.
fn validate_hue_sat(lut: &HueSatLut) -> Result<(), LookError> {
    let nodes = (lut.dims[0] as usize)
        .checked_mul(lut.dims[1] as usize)
        .and_then(|n| n.checked_mul(lut.dims[2] as usize))
        .ok_or_else(|| LookError::Malformed("hue/sat dims overflow".into()))?;
    if nodes == 0 {
        return Err(LookError::Malformed(
            "hue/sat dims include a zero axis".into(),
        ));
    }
    if nodes > MAX_HUE_SAT_NODES {
        return Err(LookError::Malformed(format!(
            "hue/sat cube {nodes} nodes exceeds the {MAX_HUE_SAT_NODES} cap"
        )));
    }
    if lut.deltas.len() != nodes {
        return Err(LookError::Malformed(format!(
            "hue/sat deltas length {} disagrees with dims product {nodes}",
            lut.deltas.len()
        )));
    }
    Ok(())
}

/// Whether the Lightbox default look applies to a source of the given kind
/// (spec §5.3): **raw sources** get the default look; **non-raw sources**
/// already carry a rendered look, so it does not. E09/E10 consult this to decide
/// whether to pass a `look` to [`crate::transform::resolve_input_transform`].
#[must_use]
pub fn default_look_applies(is_raw_source: bool) -> bool {
    is_raw_source
}

// ===========================================================================
// Authoring (E4) — the original Lightbox default look family.
//
// Provenance affidavit (E5): every value below is hand-authored from first
// principles (a gentle scene-referred contrast S plus a value-dependent
// saturation shaping with a highlight-desaturation guard). NO Adobe-authored
// profile, look table, tone curve, or rendering was inspected, sampled, fitted,
// or copied at any step. Recorded in assets/color/looks/lightbox-color-v1.review.toml
// and docs/plan/epics/E02-deviations.md.
// ===========================================================================

/// The authored provenance shared by the Lightbox-authored looks.
fn authored_provenance(review_record: Option<&str>) -> LookProvenance {
    LookProvenance {
        author: "lightbox-authored".into(),
        license: "project".into(),
        review_record: review_record.map(str::to_owned),
    }
}

/// Authors **"Lightbox Color v1"** (spec §1.7 tier 2, task E4): the original
/// scene-referred default look. `id` is a placeholder here — the real id is the
/// `xxh3-128` the loader derives from the serialized asset; author via
/// [`Look::to_bytes`] then [`load_look`] to obtain the canonical value.
///
/// Design (working-space linear RGB domain):
/// * **Tone curve** — a gentle contrast S with a shadow *toe* (slope `< 1`
///   below the pivot), a pivot held on identity at ~mid (`x = 0.18`), a mild
///   upper-mid lift, and a highlight *shoulder* (slope `< 1` near white). Applied
///   per channel, so neutrals (`R = G = B`) stay exactly neutral — zero hue skew
///   on the gray axis by construction.
/// * **Hue/sat shaping** — hue-independent, value-dependent: a mild saturation
///   lift through the mids and a **highlight-desaturation guard** (`sat× < 1` as
///   value → 1) that keeps bright, near-clipped colors clean. `Δhue = 0` at every
///   node, so skin/sky hues are not rotated; value is untouched (the tone curve
///   owns luminance). Encoded [`HueSatEncoding::Linear`].
#[must_use]
pub fn author_lightbox_color_v1() -> Look {
    // Gentle scene-referred contrast S (working-linear domain). Monotone under
    // the Fritsch–Carlson spline; pivot on identity at x = 0.18.
    let tone_curve = Spline1D {
        control_points: vec![
            [0.00, 0.000],
            [0.05, 0.042], // toe: slope < 1
            [0.18, 0.180], // pivot on identity (mid luminance preserved)
            [0.45, 0.475], // mild upper-mid lift
            [0.75, 0.775],
            [0.92, 0.930], // shoulder: slope < 1
            [1.00, 1.000],
        ],
    };

    // Value-dependent saturation shaping [Δhue°, sat×, val×] over 5 value nodes
    // (v = 0, .25, .5, .75, 1). Hue-independent, value-preserving.
    let hue_sat = Some(HueSatLut {
        dims: [1, 1, 5],
        deltas: vec![
            [0.0, 1.00, 1.0], // shadows: unchanged
            [0.0, 1.02, 1.0], // low mids: gentle lift
            [0.0, 1.03, 1.0], // mids: gentle lift
            [0.0, 1.00, 1.0], // upper mids: back to neutral
            [0.0, 0.90, 1.0], // highlights: desaturation guard
        ],
        encoding: HueSatEncoding::Linear,
    });

    Look {
        id: ProfileId([0u8; 16]),
        name: DEFAULT_LOOK_NAME.to_owned(),
        version: LBLOOK_VERSION,
        tone_curve,
        hue_sat,
        provenance: authored_provenance(Some("selfreview-lightbox-color-v1-2026-07-06")),
    }
}

/// Authors **"Lightbox Neutral"** (task E6): the identity look. Ships alongside
/// the default so a user (or the taste-risk mitigation, R6) can select a
/// no-op look. Its tone curve is identity and it carries no shaping.
#[must_use]
pub fn author_lightbox_neutral() -> Look {
    Look {
        id: ProfileId([0u8; 16]),
        name: "Lightbox Neutral".to_owned(),
        version: LBLOOK_VERSION,
        tone_curve: Spline1D::identity(),
        hue_sat: None,
        provenance: authored_provenance(None),
    }
}

// ===========================================================================
// Authoring harness (E3) — synthetic scene corpus + contact sheet.
//
// The real photographic CC0 neutral-scene corpus is deferred (no corpus tree on
// this branch — see E02-deviations.md; the LibRaw proxy + raw corpus land in
// Phase C/H). In its place this module ships a *synthetic*, deterministic,
// license-clean (project-generated) scene set that exercises the tonal and hue
// ranges the E5 checklist names: skin, sky, foliage, neutrals, gradients,
// clipped highlights, low-light, high dynamic range. It is honest test content,
// labelled synthetic in the committed scene-corpus manifest.
// ===========================================================================

/// Width of every synthetic scene tile (px).
pub const TILE_W: u32 = 32;
/// Height of every synthetic scene tile (px).
pub const TILE_H: u32 = 20;

/// One synthetic scene: a small tile of **working-space linear RGB** with a
/// category label (spec task E3). Deterministic and project-generated.
#[derive(Clone, Debug)]
pub struct Scene {
    /// Stable scene name.
    pub name: String,
    /// Category (`skin`, `sky`, `foliage`, `neutral`, `clipped`, `low-light`,
    /// `high-dr`, `hue-sweep`).
    pub category: String,
    /// Tile width (px).
    pub width: u32,
    /// Tile height (px).
    pub height: u32,
    /// Working-space linear RGB pixels, row-major.
    pub pixels: Vec<[f32; 3]>,
}

/// A rendered contact-sheet cell: one scene rendered **base** (no look) and
/// **look** (default look at `amount`), both as 8-bit sRGB RGBA (task E3).
#[derive(Clone, Debug)]
pub struct SceneTile {
    /// Scene name.
    pub name: String,
    /// Scene category.
    pub category: String,
    /// Tile width (px).
    pub width: u32,
    /// Tile height (px).
    pub height: u32,
    /// Base render (working → sRGB8, no look), RGBA row-major.
    pub base_rgba8: Vec<u8>,
    /// Look render (look applied, then working → sRGB8), RGBA row-major.
    pub look_rgba8: Vec<u8>,
}

/// The linear-sRGB(D65) → working(ProPhoto-linear, D50) matrix, so scenes can be
/// authored in intuitive sRGB and land in the working space the look operates
/// in. Inverse of [`spaces::working_to_linear_srgb`].
fn srgb_to_working_matrix() -> Mat3 {
    spaces::working_to_linear_srgb()
        .inverse()
        .expect("working ↔ linear-sRGB is invertible")
}

/// An sRGB-encoded `[0,1]³` intent color → working-space linear RGB.
fn srgb_to_working(m: &Mat3, srgb: [f32; 3]) -> [f32; 3] {
    let lin = Vec3([
        spaces::srgb_eotf(srgb[0]) as f64,
        spaces::srgb_eotf(srgb[1]) as f64,
        spaces::srgb_eotf(srgb[2]) as f64,
    ]);
    let w = m.mul_vec(lin).0;
    [w[0] as f32, w[1] as f32, w[2] as f32]
}

/// Builds a tile from a per-pixel closure over normalized `(u, v) ∈ [0,1]²`.
fn tile(name: &str, category: &str, f: impl Fn(f32, f32) -> [f32; 3]) -> Scene {
    let mut pixels = Vec::with_capacity((TILE_W * TILE_H) as usize);
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            let u = x as f32 / (TILE_W - 1) as f32;
            let v = y as f32 / (TILE_H - 1) as f32;
            pixels.push(f(u, v));
        }
    }
    Scene {
        name: name.to_owned(),
        category: category.to_owned(),
        width: TILE_W,
        height: TILE_H,
        pixels,
    }
}

/// A flat-ish swatch (mild ±12 % vertical luminance gradient) from an sRGB
/// intent color, in working-space linear RGB.
fn swatch(m: &Mat3, name: &str, cat: &str, srgb: [f32; 3]) -> Scene {
    tile(name, cat, |_, v| {
        let g = 0.88 + 0.24 * v; // 0.88 .. 1.12
        srgb_to_working(m, [srgb[0] * g, srgb[1] * g, srgb[2] * g])
    })
}

/// A vertical top→bottom sRGB gradient tile in working-space linear RGB.
fn gradient_v(m: &Mat3, name: &str, cat: &str, top: [f32; 3], bot: [f32; 3]) -> Scene {
    tile(name, cat, |_, v| {
        let s = [
            top[0] + (bot[0] - top[0]) * v,
            top[1] + (bot[1] - top[1]) * v,
            top[2] + (bot[2] - top[2]) * v,
        ];
        srgb_to_working(m, s)
    })
}

/// The deterministic synthetic scene corpus (≥ 30 scenes across the E5-checklist
/// categories — task E3). Working-space linear RGB. The committed
/// `assets/color/looks/scene-corpus.toml` manifest mirrors these names +
/// categories (asserted by the `look_assets` test).
#[must_use]
#[allow(clippy::vec_init_then_push)] // a readable scene builder: mixed pushes + loops
pub fn scene_corpus() -> Vec<Scene> {
    let m = srgb_to_working_matrix();
    let mut scenes = Vec::new();

    // --- Skin tones (5) ---
    scenes.push(swatch(&m, "skin-light", "skin", [0.95, 0.80, 0.72]));
    scenes.push(swatch(&m, "skin-fair", "skin", [0.90, 0.72, 0.62]));
    scenes.push(swatch(&m, "skin-medium", "skin", [0.80, 0.60, 0.50]));
    scenes.push(swatch(&m, "skin-tan", "skin", [0.70, 0.50, 0.42]));
    scenes.push(swatch(&m, "skin-deep", "skin", [0.55, 0.38, 0.31]));

    // --- Sky (5): pale→deep vertical gradients ---
    scenes.push(gradient_v(
        &m,
        "sky-clear",
        "sky",
        [0.35, 0.55, 0.85],
        [0.70, 0.82, 0.95],
    ));
    scenes.push(gradient_v(
        &m,
        "sky-deep",
        "sky",
        [0.15, 0.35, 0.72],
        [0.45, 0.62, 0.88],
    ));
    scenes.push(gradient_v(
        &m,
        "sky-zenith",
        "sky",
        [0.10, 0.28, 0.66],
        [0.30, 0.50, 0.82],
    ));
    scenes.push(gradient_v(
        &m,
        "sky-haze",
        "sky",
        [0.55, 0.68, 0.82],
        [0.78, 0.85, 0.92],
    ));
    scenes.push(gradient_v(
        &m,
        "sky-dusk",
        "sky",
        [0.42, 0.46, 0.62],
        [0.85, 0.66, 0.52],
    ));

    // --- Foliage (5) ---
    scenes.push(swatch(&m, "foliage-spring", "foliage", [0.45, 0.60, 0.28]));
    scenes.push(swatch(&m, "foliage-summer", "foliage", [0.30, 0.48, 0.20]));
    scenes.push(swatch(&m, "foliage-shade", "foliage", [0.18, 0.32, 0.15]));
    scenes.push(swatch(&m, "foliage-grass", "foliage", [0.40, 0.55, 0.25]));
    scenes.push(swatch(&m, "foliage-conifer", "foliage", [0.16, 0.30, 0.18]));

    // --- Neutrals (6): flat grays + a full black→white sweep ---
    for (name, level) in [
        ("neutral-05", 0.05f32),
        ("neutral-18", 0.18),
        ("neutral-50", 0.50),
        ("neutral-75", 0.75),
        ("neutral-95", 0.95),
    ] {
        scenes.push(tile(name, "neutral", |_, _| {
            srgb_to_working(&m, [level, level, level])
        }));
    }
    scenes.push(tile("neutral-ramp", "neutral", |u, _| {
        srgb_to_working(&m, [u, u, u])
    }));

    // --- Clipped highlights (4): built directly in working-linear, values > 1 ---
    scenes.push(tile("clip-white", "clipped", |u, _| {
        let v = 0.85 + 2.4 * u; // 0.85 .. 3.25
        [v, v, v]
    }));
    scenes.push(tile("clip-warm", "clipped", |u, _| {
        let v = 0.9 + 2.2 * u;
        [v, v * 0.85, v * 0.6] // warm near-clip that pushes into >1
    }));
    scenes.push(tile("clip-sky", "clipped", |u, _| {
        let v = 0.9 + 1.8 * u;
        [v * 0.7, v * 0.85, v] // bright sky rolling into clip
    }));
    scenes.push(tile("clip-specular", "clipped", |u, v| {
        let r = (u * u + v * v).sqrt();
        let s = (1.2 + 2.5 * (1.0 - r)).max(0.2);
        [s, s, s]
    }));

    // --- Low light (4): deep shadows, faint hue ---
    scenes.push(tile("low-neutral", "low-light", |u, _| {
        let v = 0.004 + 0.06 * u;
        [v, v, v]
    }));
    scenes.push(tile("low-warm", "low-light", |u, _| {
        let v = 0.006 + 0.05 * u;
        [v * 1.15, v, v * 0.8]
    }));
    scenes.push(tile("low-cool", "low-light", |u, _| {
        let v = 0.006 + 0.05 * u;
        [v * 0.8, v, v * 1.2]
    }));
    scenes.push(tile("low-foliage", "low-light", |u, _| {
        let v = 0.006 + 0.05 * u;
        [v * 0.7, v, v * 0.5]
    }));

    // --- High dynamic range (3): wide gradients shadow→clip ---
    scenes.push(tile("hdr-sweep", "high-dr", |u, _| {
        let v = 0.003 * (1.0 + u * 800.0); // ~0.003 .. 2.4, near-exponential feel
        [v, v, v]
    }));
    scenes.push(tile("hdr-warm-window", "high-dr", |u, _| {
        let v = 0.01 + 2.4 * u * u;
        [v, v * 0.9, v * 0.75]
    }));
    scenes.push(tile("hdr-backlit", "high-dr", |u, v| {
        let g = (u + v) * 0.5;
        let l = 0.01 + 2.6 * g * g;
        [l * 0.85, l * 0.9, l]
    }));

    // --- Hue sweeps (2): constant value/sat, hue around the wheel ---
    scenes.push(tile("hue-sweep-mid", "hue-sweep", |u, _| {
        srgb_to_working(&m, hsl_ish(u * 360.0, 0.55, 0.5))
    }));
    scenes.push(tile("hue-sweep-bright", "hue-sweep", |u, _| {
        srgb_to_working(&m, hsl_ish(u * 360.0, 0.7, 0.72))
    }));

    scenes
}

/// A small HSV→sRGB helper (value/sat in `[0,1]`, hue in degrees) for the hue
/// sweeps — self-contained so the corpus does not reach into `transform`'s
/// private HSV code.
fn hsl_ish(hue: f32, sat: f32, val: f32) -> [f32; 3] {
    let c = val * sat;
    let hp = (hue.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let mmin = val - c;
    [r + mmin, g + mmin, b + mmin]
}

/// Renders the synthetic [`scene_corpus`] to base-vs-look contact-sheet cells at
/// the given look `amount` (task E3). Deterministic; the E7 look golden stitches
/// the `look_rgba8` cells and gates them.
#[must_use]
pub fn render_contact_sheet(look: &Look, amount: f32) -> Vec<SceneTile> {
    scene_corpus()
        .into_iter()
        .map(|s| {
            let n = (s.width * s.height) as usize;
            let mut base = Vec::with_capacity(n * 4);
            let mut lk = Vec::with_capacity(n * 4);
            for px in &s.pixels {
                let b = working_linear_to_srgb8(*px);
                base.extend_from_slice(&[b[0], b[1], b[2], 255]);
                let l = working_linear_to_srgb8(look.eval(*px, amount));
                lk.extend_from_slice(&[l[0], l[1], l[2], 255]);
            }
            SceneTile {
                name: s.name,
                category: s.category,
                width: s.width,
                height: s.height,
                base_rgba8: base,
                look_rgba8: lk,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f32; 3], b: [f32; 3], tol: f32) {
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() <= tol, "{a:?} vs {b:?} @ {k}");
        }
    }

    // ---- E1: format, loader, validator ----

    #[test]
    fn round_trip_is_stable() {
        for look in [author_lightbox_color_v1(), author_lightbox_neutral()] {
            let bytes = look.to_bytes().unwrap();
            let loaded = load_look(&bytes).unwrap();
            assert_eq!(loaded.name, look.name);
            assert_eq!(loaded.version, LBLOOK_VERSION);
            assert_eq!(
                loaded.tone_curve.control_points,
                look.tone_curve.control_points
            );
            assert_eq!(loaded.provenance, look.provenance);
            match (&loaded.hue_sat, &look.hue_sat) {
                (Some(a), Some(b)) => {
                    assert_eq!(a.dims, b.dims);
                    assert_eq!(a.deltas, b.deltas);
                    assert_eq!(a.encoding, b.encoding);
                }
                (None, None) => {}
                _ => panic!("hue_sat presence changed across round-trip"),
            }
            // The id is derived and stable: re-serializing the loaded look
            // reproduces the exact bytes and thus the exact id.
            let bytes2 = loaded.to_bytes().unwrap();
            assert_eq!(bytes, bytes2, "canonical bytes not stable");
            assert_eq!(load_look(&bytes2).unwrap().id, loaded.id);
        }
    }

    #[test]
    fn distinct_content_has_distinct_ids() {
        let a = load_look(&author_lightbox_color_v1().to_bytes().unwrap()).unwrap();
        let b = load_look(&author_lightbox_neutral().to_bytes().unwrap()).unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bytes = author_lightbox_neutral().to_bytes().unwrap();
        bytes[4] = 2; // bump the version LE byte to 2
        assert!(matches!(
            load_look(&bytes),
            Err(LookError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn bad_magic_and_short_buffers_are_structured_errors() {
        assert!(matches!(load_look(&[]), Err(LookError::Malformed(_))));
        assert!(matches!(
            load_look(b"XXXX\x01\x00"),
            Err(LookError::Malformed(_))
        ));
        // Truncated CBOR body after a valid header.
        let mut bytes = author_lightbox_neutral().to_bytes().unwrap();
        bytes.truncate(bytes.len() - 3);
        assert!(matches!(load_look(&bytes), Err(LookError::Malformed(_))));
    }

    #[test]
    fn validator_rejects_bad_curve_and_dims() {
        // Non-ascending tone curve.
        let mut bad = author_lightbox_neutral();
        bad.tone_curve = Spline1D {
            control_points: vec![[0.0, 0.0], [0.0, 1.0]],
        };
        assert!(matches!(
            load_look(&bad.to_bytes().unwrap()),
            Err(LookError::Malformed(_))
        ));

        // deltas length disagrees with dims.
        let mut bad2 = author_lightbox_neutral();
        bad2.hue_sat = Some(HueSatLut {
            dims: [2, 2, 2],
            deltas: vec![[0.0, 1.0, 1.0]; 3], // should be 8
            encoding: HueSatEncoding::Linear,
        });
        assert!(matches!(
            load_look(&bad2.to_bytes().unwrap()),
            Err(LookError::Malformed(_))
        ));
    }

    // ---- E2: amount semantics ----

    #[test]
    fn amount_zero_is_bit_exact_identity() {
        let look = author_lightbox_color_v1();
        for rgb in [
            [0.0, 0.0, 0.0],
            [0.18, 0.18, 0.18],
            [0.3, 0.6, 0.42],
            [1.0, 0.5, 0.25],
            [2.4, 1.1, 0.7], // above 1 (clipped-highlight domain)
        ] {
            let out = look.eval(rgb, 0.0);
            assert_eq!(out, rgb, "amount=0 not bit-exact for {rgb:?}");
        }
    }

    #[test]
    fn amount_one_is_authored_strength() {
        let look = author_lightbox_color_v1();
        let rgb = [0.3, 0.45, 0.2];
        let full = look.eval(rgb, 1.0);
        // The authored look is not the identity (it has real contrast/shaping).
        assert!(
            (full[0] - rgb[0]).abs() + (full[1] - rgb[1]).abs() + (full[2] - rgb[2]).abs() > 1e-3,
            "look at amount 1 should differ from input"
        );
    }

    #[test]
    fn amount_is_an_identity_lerp_below_one() {
        // At 50 % the tone response is the midpoint between identity and full
        // (the shaping is mild, so check on a neutral where shaping is a no-op).
        let look = author_lightbox_color_v1();
        let x = 0.4f32;
        let rgb = [x, x, x]; // neutral: shaping is identity, tone is the whole effect
        let full = look.eval(rgb, 1.0)[0];
        let half = look.eval(rgb, 0.5)[0];
        let expect = x + 0.5 * (full - x);
        assert!((half - expect).abs() < 1e-5, "half={half} expect={expect}");
    }

    #[test]
    fn amount_above_one_extrapolates_and_stays_bounded() {
        let look = author_lightbox_color_v1();
        let x = 0.4f32;
        let rgb = [x, x, x];
        let full = look.eval(rgb, 1.0)[0];
        let over = look.eval(rgb, 2.0)[0];
        let expect = x + 2.0 * (full - x);
        assert!((over - expect).abs() < 1e-5, "extrapolation off");
        // Output is always clamped into [0,1].
        for a in [0.0, 0.5, 1.0, 1.5, 2.0, 5.0] {
            for out in look.eval([0.95, 0.1, 0.5], a) {
                assert!((0.0..=1.0).contains(&out), "unclamped {out} @ amount {a}");
            }
        }
    }

    #[test]
    fn amount_is_continuous() {
        // Small amount steps produce small output steps (no discontinuity across
        // the 0..=2 range for a representative set of colors).
        let look = author_lightbox_color_v1();
        let colors = [[0.2, 0.5, 0.3], [0.8, 0.6, 0.55], [0.05, 0.1, 0.4]];
        for rgb in colors {
            let mut prev = look.eval(rgb, 0.0);
            let mut a = 0.0f32;
            while a < 2.0 {
                a += 0.02;
                let cur = look.eval(rgb, a);
                for k in 0..3 {
                    assert!(
                        (cur[k] - prev[k]).abs() < 0.05,
                        "amount discontinuity near {a} for {rgb:?}: {prev:?} -> {cur:?}"
                    );
                }
                prev = cur;
            }
        }
    }

    #[test]
    fn neutral_look_is_identity_at_full_strength() {
        let look = author_lightbox_neutral();
        for rgb in [[0.1, 0.1, 0.1], [0.4, 0.6, 0.2], [0.9, 0.3, 0.5]] {
            approx(look.eval(rgb, 1.0), rgb, 1e-6);
        }
    }

    #[test]
    fn resolve_look_hue_sat_is_identity_at_amount_zero() {
        let hs = author_lightbox_color_v1().hue_sat.unwrap();
        let table = resolve_look_hue_sat(&hs, 0.0);
        for d in &table.deltas {
            approx(*d, [0.0, 1.0, 1.0], 1e-7);
        }
    }

    // ---- E4: authored look preserves neutrals and skin/sky hue ----

    #[test]
    fn per_channel_curve_keeps_neutrals_neutral() {
        let look = author_lightbox_color_v1();
        for level in [0.05f32, 0.18, 0.5, 0.8] {
            let out = look.eval([level, level, level], 1.0);
            assert!(
                (out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6,
                "neutral {level} skewed to {out:?}"
            );
        }
    }

    #[test]
    fn shaping_does_not_rotate_hue_on_skin_or_sky() {
        // Δhue = 0 at every node ⇒ the shaping never rotates hue. Compare hue of
        // (tone-only) vs (tone+shaping) on representative skin/sky colors.
        let look = author_lightbox_color_v1();
        let mut tone_only = look.clone();
        tone_only.hue_sat = None;
        for rgb in [[0.62, 0.44, 0.36], [0.30, 0.50, 0.82]] {
            let a = look.eval(rgb, 1.0);
            let b = tone_only.eval(rgb, 1.0);
            let ha = hue_of(a);
            let hb = hue_of(b);
            let d = ((ha - hb + 540.0) % 360.0 - 180.0).abs();
            assert!(d < 0.5, "shaping rotated hue by {d}° on {rgb:?}");
        }
    }

    fn hue_of(rgb: [f32; 3]) -> f32 {
        let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        if d <= 1e-9 {
            return 0.0;
        }
        let h = if max == r {
            60.0 * (((g - b) / d) % 6.0)
        } else if max == g {
            60.0 * (((b - r) / d) + 2.0)
        } else {
            60.0 * (((r - g) / d) + 4.0)
        };
        (h + 360.0) % 360.0
    }

    // ---- E3: corpus shape ----

    #[test]
    fn scene_corpus_covers_the_checklist_categories() {
        let corpus = scene_corpus();
        assert!(corpus.len() >= 30, "corpus has {} scenes", corpus.len());
        for cat in [
            "skin",
            "sky",
            "foliage",
            "neutral",
            "clipped",
            "low-light",
            "high-dr",
            "hue-sweep",
        ] {
            assert!(
                corpus.iter().any(|s| s.category == cat),
                "missing category {cat}"
            );
        }
        // Names are unique (they key the golden + manifest).
        let mut names: Vec<&str> = corpus.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate scene name");
        // Every tile is the fixed size and fully populated.
        for s in &corpus {
            assert_eq!(s.width, TILE_W);
            assert_eq!(s.height, TILE_H);
            assert_eq!(s.pixels.len(), (TILE_W * TILE_H) as usize);
        }
    }

    #[test]
    fn contact_sheet_renders_base_and_look() {
        let look = author_lightbox_color_v1();
        let tiles = render_contact_sheet(&look, 1.0);
        assert_eq!(tiles.len(), scene_corpus().len());
        for t in &tiles {
            let bytes = (t.width * t.height * 4) as usize;
            assert_eq!(t.base_rgba8.len(), bytes);
            assert_eq!(t.look_rgba8.len(), bytes);
        }
        // The default look actually changes some cell somewhere (it is not a
        // no-op) but never touches a pure-neutral scene's hue.
        let changed = tiles.iter().any(|t| t.base_rgba8 != t.look_rgba8);
        assert!(changed, "default look had no visible effect on any scene");
    }

    #[test]
    fn non_raw_sources_skip_the_default_look() {
        // Spec §5.3: raw sources get the look; non-raw already carry one.
        assert!(default_look_applies(true));
        assert!(!default_look_applies(false));
    }
}
