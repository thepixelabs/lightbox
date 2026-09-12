// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E09 Phase A property + acceptance tests (spec §5 T2-T4, §6).
//!
//! - First failing test (a): `recipe_default_roundtrip` (T3 target).
//! - CBOR: deterministic encode, decode identity, unknown-key byte-preservation,
//!   `RecipeRead::NewerSchema` (read-only, never rewritten).
//! - Delta laws (§3.1): `apply∘diff` identity, per-delta idempotence,
//!   `diff(a,a)` empty, `extract ⊆ subset`, `merge` last-wins.
//! - `canonical_hash` determinism / discrimination.

use proptest::prelude::*;

use lightbox_edit::{
    group_of, BwMix, CborValue, ColorGrade, Crop, CurvePoint, Flip, GradeWheel, Grain, HslBand,
    HslTable, LensCorrection, NoiseReduction, Optics, ParamDelta, ParamGroup, ParamId, ParamSubset,
    ParamValue, ParametricCurve, PostCropVignette, Presence, ProfileKind, ProfileRef, Recipe,
    RecipeRead, Sharpen, ToneCurve, ToneCurveSet, Transform, Treatment, Upright, WhiteBalance,
    RECIPE_SCHEMA,
};
use lightbox_types::{MaskId, ProcessVersion, RetouchOpId, PV_M0};

// ── strategies ───────────────────────────────────────────────────────────────

/// A finite f32 in `[lo, hi]` (never NaN/Inf, so PartialEq is total & clamping is
/// a no-op, the laws are stated over in-range values).
fn f_in(lo: f32, hi: f32) -> impl Strategy<Value = f32> {
    (0u32..=1000).prop_map(move |k| {
        // Clamp guards against float rounding landing a hair outside [lo, hi],
        // which would make `apply` record a spurious clamp.
        (lo + (hi - lo) * (k as f32) / 1000.0).clamp(lo, hi)
    })
}

fn arb_wb() -> impl Strategy<Value = WhiteBalance> {
    prop_oneof![
        Just(WhiteBalance::AsShot),
        Just(WhiteBalance::Auto),
        (f_in(2000.0, 50000.0), f_in(-150.0, 150.0))
            .prop_map(|(temp_k, tint)| WhiteBalance::Custom { temp_k, tint }),
    ]
}

/// A valid tone curve: strictly-increasing x, `<=64` points, all in `[0,1]`.
fn arb_curve() -> impl Strategy<Value = ToneCurve> {
    prop::collection::vec(f_in(0.0, 1.0), 0..8).prop_map(|ys| {
        // Build strictly-increasing x by placing points on a fixed grid.
        let n = ys.len();
        let points = ys
            .into_iter()
            .enumerate()
            .map(|(i, y)| CurvePoint {
                x: if n <= 1 {
                    i as f32
                } else {
                    i as f32 / (n as f32 - 1.0)
                },
                y,
            })
            .collect::<Vec<_>>();
        ToneCurve { points }
    })
}

/// An arbitrary parametric curve whose splits are already at their
/// clamp-stable default (`[0.25, 0.50, 0.75]`, same convention `arb_curve`'s
/// fixed x-fractions use), `ParametricCurve::clamp` is a no-op on any value
/// this strategy produces, so the round-trip laws below (`apply(diff(a,b)) ==
/// b`) hold without a spurious clamp perturbing `b`'s value.
fn arb_parametric() -> impl Strategy<Value = ParametricCurve> {
    (
        f_in(-100.0, 100.0),
        f_in(-100.0, 100.0),
        f_in(-100.0, 100.0),
        f_in(-100.0, 100.0),
    )
        .prop_map(|(highlights, lights, darks, shadows)| ParametricCurve {
            highlights,
            lights,
            darks,
            shadows,
            splits: [0.25, 0.50, 0.75],
        })
}

fn arb_curveset() -> impl Strategy<Value = ToneCurveSet> {
    (
        arb_curve(),
        arb_curve(),
        arb_curve(),
        arb_curve(),
        arb_parametric(),
    )
        .prop_map(|(rgb, r, g, b, parametric)| ToneCurveSet {
            rgb,
            r,
            g,
            b,
            parametric,
        })
}

fn arb_hslband() -> impl Strategy<Value = HslBand> {
    (
        f_in(-100.0, 100.0),
        f_in(-100.0, 100.0),
        f_in(-100.0, 100.0),
    )
        .prop_map(|(hue, sat, lum)| HslBand { hue, sat, lum })
}

fn arb_hsl() -> impl Strategy<Value = HslTable> {
    prop::array::uniform8(arb_hslband()).prop_map(|bands| HslTable { bands })
}

fn arb_wheel() -> impl Strategy<Value = GradeWheel> {
    (f_in(0.0, 360.0), f_in(0.0, 100.0), f_in(-100.0, 100.0))
        .prop_map(|(hue, sat, lum)| GradeWheel { hue, sat, lum })
}

fn arb_grade() -> impl Strategy<Value = ColorGrade> {
    (
        arb_wheel(),
        arb_wheel(),
        arb_wheel(),
        arb_wheel(),
        f_in(0.0, 100.0),
        f_in(-100.0, 100.0),
    )
        .prop_map(
            |(shadows, midtones, highlights, global, blend, balance)| ColorGrade {
                shadows,
                midtones,
                highlights,
                global,
                blend,
                balance,
            },
        )
}

fn arb_optics() -> impl Strategy<Value = Optics> {
    (
        // The two profile amounts (0..=200, unity 100) and the manual
        // distortion dial (-100..=100, neutral 0) are independent axes; the
        // round trip has to hold across all three, not just the pair.
        prop::option::of((f_in(0.0, 200.0), f_in(0.0, 200.0), f_in(-100.0, 100.0))),
        any::<bool>(),
        f_in(0.0, 100.0),
        f_in(-100.0, 100.0),
    )
        .prop_map(|(lp, ca, defringe, vignette_corr)| Optics {
            lens_profile: lp.map(
                |(distortion, vignetting, manual_distortion)| LensCorrection {
                    profile_id: "lens-x".to_string(),
                    distortion,
                    vignetting,
                    manual_distortion,
                },
            ),
            ca,
            defringe,
            vignette_corr,
        })
}

fn arb_profile() -> impl Strategy<Value = ProfileRef> {
    (prop::option::of(Just("look-a".to_string())), f_in(0.0, 2.0)).prop_map(|(look_ref, amt)| {
        ProfileRef {
            kind: ProfileKind::Matrix,
            id: "matrix-base".to_string(),
            look_ref,
            look_amount: amt,
        }
    })
}

/// An arbitrary VALID recipe with in-range params and **empty extension bags**
/// (`lb_extra`/`xmp_passthrough`/`unknown`), the surface the delta engine
/// addresses (deviations: apply preserves the target's extension bags).
fn arb_recipe() -> impl Strategy<Value = Recipe> {
    (
        // tuple 1: basic tone + wb + profile
        (
            arb_profile(),
            arb_wb(),
            f_in(-5.0, 5.0),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
        ),
        // tuple 2: colour
        (
            arb_curveset(),
            arb_hsl(),
            arb_grade(),
            any::<bool>(),
            prop::array::uniform8(f_in(-100.0, 100.0)),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
        ),
        // tuple 3: presence + detail + optics
        (
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
            f_in(-100.0, 100.0),
            (
                f_in(0.0, 150.0),
                f_in(0.5, 3.0),
                f_in(0.0, 100.0),
                f_in(0.0, 100.0),
            ),
            (
                f_in(0.0, 100.0),
                f_in(0.0, 100.0),
                f_in(0.0, 100.0),
                f_in(0.0, 100.0),
            ),
            arb_optics(),
        ),
        // tuple 4: effects + geometry + structure
        (
            (
                f_in(-100.0, 100.0),
                f_in(0.0, 100.0),
                f_in(-100.0, 100.0),
                f_in(0.0, 100.0),
                f_in(0.0, 100.0),
            ),
            (f_in(0.0, 100.0), f_in(0.0, 100.0), f_in(0.0, 100.0)),
            f_in(-45.0, 45.0),
            0i32..4,
            prop::collection::vec(any::<i64>(), 0..4),
            prop::collection::vec(any::<i64>(), 0..4),
        ),
    )
        .prop_map(
            |(
                (profile, wb, exposure, contrast, highlights, shadows, whites, blacks),
                (curve, hsl, grade, bw_treat, bw_weights, vibrance, saturation),
                (
                    clarity,
                    texture,
                    dehaze,
                    (sh_a, sh_r, sh_d, sh_m),
                    (nr_l, nr_ld, nr_c, nr_cd),
                    optics,
                ),
                (
                    (v_amt, v_mid, v_round, v_feath, v_high),
                    (gr_a, gr_s, gr_r),
                    angle,
                    flip_code,
                    masks,
                    retouch,
                ),
            )| {
                let mut r = Recipe::identity(PV_M0);
                r.base_profile = profile;
                let g = &mut r.global;
                g.white_balance = wb;
                g.exposure = exposure;
                g.contrast = contrast;
                g.highlights = highlights;
                g.shadows = shadows;
                g.whites = whites;
                g.blacks = blacks;
                g.tone_curve = curve;
                g.hsl = hsl;
                g.color_grade = grade;
                g.treatment = if bw_treat {
                    Treatment::BlackAndWhite
                } else {
                    Treatment::Color
                };
                g.bw = BwMix {
                    weights: bw_weights,
                };
                g.vibrance = vibrance;
                g.saturation = saturation;
                g.presence = Presence {
                    clarity,
                    texture,
                    dehaze,
                };
                g.detail.sharpen = Sharpen {
                    amount: sh_a,
                    radius: sh_r,
                    detail: sh_d,
                    masking: sh_m,
                };
                g.detail.nr = NoiseReduction {
                    luma: nr_l,
                    luma_detail: nr_ld,
                    chroma: nr_c,
                    chroma_detail: nr_cd,
                };
                g.optics = optics;
                g.effects.postcrop_vignette = PostCropVignette {
                    amount: v_amt,
                    midpoint: v_mid,
                    roundness: v_round,
                    feather: v_feath,
                    highlights: v_high,
                };
                g.effects.grain = Grain {
                    amount: gr_a,
                    size: gr_s,
                    roughness: gr_r,
                };
                r.geometry.crop = Crop::default();
                r.geometry.angle = angle;
                r.geometry.flip = match flip_code {
                    1 => Flip::Horizontal,
                    2 => Flip::Vertical,
                    3 => Flip::Both,
                    _ => Flip::None,
                };
                r.geometry.upright = Upright::Off;
                r.geometry.transform = Transform::default();
                r.masks = masks.into_iter().map(MaskId).collect();
                r.retouch = retouch.into_iter().map(RetouchOpId).collect();
                r
            },
        )
}

fn all_groups() -> Vec<ParamGroup> {
    // Derived from ParamId::ALL so it stays complete as ids are appended.
    let mut gs: Vec<ParamGroup> = ParamId::ALL.iter().map(|id| group_of(*id)).collect();
    gs.sort_by_key(|g| format!("{g:?}"));
    gs.dedup();
    gs
}

fn arb_subset() -> impl Strategy<Value = ParamSubset> {
    let groups = all_groups();
    proptest::sample::subsequence(groups.clone(), 0..=groups.len())
        .prop_map(ParamSubset::from_groups)
}

// ── first failing test (a) ───────────────────────────────────────────────────

#[test]
fn recipe_default_roundtrip() {
    // T3 first-failing-test (a): default_for → to_cbor → from_cbor equals; the
    // canonical hash is stable (byte-identical here; cross-OS identity is the CI
    // matrix's job).
    let probe = fake_probe();
    let r = Recipe::default_for(PV_M0, &probe);
    assert!(r.is_neutral());
    let bytes = r.to_cbor();
    let back = Recipe::from_cbor(&bytes).unwrap().into_recipe().unwrap();
    assert_eq!(r, back);
    assert_eq!(r.canonical_hash(), back.canonical_hash());
    // Determinism: a second identity recipe hashes identically.
    assert_eq!(Recipe::identity(PV_M0).canonical_hash(), r.canonical_hash());
}

fn fake_probe() -> lightbox_decode::AssetProbe {
    lightbox_decode::AssetProbe {
        format: lightbox_decode::ProbedFormat::Unsupported("test".to_string()),
        width: 6000,
        height: 4000,
        orientation: lightbox_types::Orientation::O1,
        camera_make: None,
        camera_model: None,
        capture_time: None,
        file_bytes: 0,
        embedded: Vec::new(),
    }
}

// ── CBOR ─────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// T3: `recipe → to_cbor → from_cbor ≡ recipe` over arbitrary valid recipes.
    #[test]
    fn cbor_roundtrip_identity(r in arb_recipe()) {
        let bytes = r.to_cbor();
        let back = Recipe::from_cbor(&bytes).unwrap().into_recipe().unwrap();
        prop_assert_eq!(&r, &back);
    }

    /// T3: encoding is a fixpoint, `to_cbor∘from_cbor∘to_cbor == to_cbor`
    /// (deterministic + byte-stable, so `canonical_hash` is stable).
    #[test]
    fn cbor_is_idempotent_fixpoint(r in arb_recipe()) {
        let b1 = r.to_cbor();
        let r2 = Recipe::from_cbor(&b1).unwrap().into_recipe().unwrap();
        let b2 = r2.to_cbor();
        prop_assert_eq!(b1, b2);
        prop_assert_eq!(r.canonical_hash(), r2.canonical_hash());
    }
}

#[test]
fn unknown_keys_round_trip_byte_preserving() {
    // T3: a newer build's top-level key survives read→write byte-for-byte.
    let mut r = Recipe::identity(PV_M0);
    r.unknown
        .insert("z_future_field".to_string(), CborValue::Integer(42.into()));
    r.unknown.insert(
        "a_future_map".to_string(),
        CborValue::Map(vec![(CborValue::Text("k".into()), CborValue::Bool(true))]),
    );
    let b1 = r.to_cbor();
    let r2 = Recipe::from_cbor(&b1).unwrap().into_recipe().unwrap();
    // Unknown keys preserved with their values.
    assert_eq!(
        r2.unknown.get("z_future_field"),
        Some(&CborValue::Integer(42.into()))
    );
    assert!(r2.unknown.contains_key("a_future_map"));
    // Byte-identical round-trip.
    let b2 = r2.to_cbor();
    assert_eq!(b1, b2);
    assert_eq!(r, r2);
}

#[test]
fn newer_schema_is_read_only_and_byte_preserved() {
    // T3: schema > RECIPE_SCHEMA is locked read-only; original bytes preserved.
    let mut newer = Recipe::identity(PV_M0);
    newer.schema = RECIPE_SCHEMA + 1;
    let raw = newer.to_cbor();
    match Recipe::from_cbor(&raw).unwrap() {
        RecipeRead::NewerSchema { raw: got, schema } => {
            assert_eq!(schema, RECIPE_SCHEMA + 1);
            assert_eq!(got, raw, "newer-schema doc must be preserved verbatim");
        }
        RecipeRead::Ok(_) => panic!("newer schema should not decode as Ok"),
    }
    // A current-schema doc decodes normally.
    assert!(Recipe::from_cbor(&Recipe::identity(PV_M0).to_cbor())
        .unwrap()
        .recipe()
        .is_some());
}

#[test]
fn from_cbor_rejects_garbage() {
    assert!(Recipe::from_cbor(&[0xff, 0x00, 0x13, 0x37]).is_err());
    // A CBOR value that is not a map.
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&42u32, &mut buf).unwrap();
    assert!(Recipe::from_cbor(&buf).is_err());
}

// ── delta laws (§3.1) ────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `a.apply(&b.diff(&a))` ⇒ `a == b` over the param surface.
    #[test]
    fn apply_of_diff_reaches_target(a in arb_recipe(), b in arb_recipe()) {
        let delta = b.diff(&a);
        let mut a2 = a.clone();
        let applied = a2.apply(&delta).unwrap();
        prop_assert!(applied.clamped.is_empty(), "in-range delta should not clamp");
        prop_assert_eq!(&a2, &b);
    }

    /// `apply` is idempotent per delta.
    #[test]
    fn apply_is_idempotent(a in arb_recipe(), b in arb_recipe()) {
        let delta = b.diff(&a);
        let mut once = a.clone();
        once.apply(&delta).unwrap();
        let mut twice = once.clone();
        twice.apply(&delta).unwrap();
        prop_assert_eq!(once, twice);
    }

    /// `diff(a, a)` is empty.
    #[test]
    fn diff_self_is_empty(a in arb_recipe()) {
        prop_assert!(a.diff(&a).is_empty());
    }

    /// `extract(subset)` carries only params in `subset`, and applying it onto a
    /// neutral base then reproduces exactly those params.
    #[test]
    fn extract_is_within_subset(a in arb_recipe(), subset in arb_subset()) {
        let d = a.extract(&subset);
        for id in d.0.keys() {
            prop_assert!(subset.contains(*id), "extract escaped its subset");
        }
        // restrict is a no-op on an already-in-subset delta.
        let r = d.restrict(&subset);
        prop_assert_eq!(d, r);
    }

    /// `restrict` never returns a param outside the subset.
    #[test]
    fn restrict_is_within_subset(a in arb_recipe(), b in arb_recipe(), subset in arb_subset()) {
        let full = b.diff(&a);
        let restricted = full.restrict(&subset);
        for id in restricted.0.keys() {
            prop_assert!(subset.contains(*id));
        }
    }

    /// `canonical_hash` discriminates: distinct recipes almost never collide, and
    /// identical recipes always agree.
    #[test]
    fn canonical_hash_agrees_and_discriminates(a in arb_recipe(), b in arb_recipe()) {
        prop_assert_eq!(a.canonical_hash(), a.clone().canonical_hash());
        if a != b {
            // xxh3-128 collisions on distinct small inputs are astronomically rare.
            prop_assert_ne!(a.canonical_hash(), b.canonical_hash());
        }
    }
}

#[test]
fn merge_is_last_wins() {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(1.0));
    d.0.insert(ParamId::Contrast, ParamValue::F32(3.0));
    let mut later = ParamDelta::new();
    later.0.insert(ParamId::Exposure, ParamValue::F32(2.0));
    d.merge(later);
    assert_eq!(d.0.get(&ParamId::Exposure), Some(&ParamValue::F32(2.0)));
    assert_eq!(d.0.get(&ParamId::Contrast), Some(&ParamValue::F32(3.0)));
}

// ── validation / clamping (T2) ───────────────────────────────────────────────

#[test]
fn apply_clamps_out_of_range_scalars() {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(999.0));
    d.0.insert(ParamId::Contrast, ParamValue::F32(-999.0));
    let report = r.apply(&d).unwrap();
    assert_eq!(r.global.exposure, 5.0);
    assert_eq!(r.global.contrast, -100.0);
    assert!(report.clamped.contains(&ParamId::Exposure));
    assert!(report.clamped.contains(&ParamId::Contrast));
    assert!(report.changed.contains(&ParamId::Exposure));
}

#[test]
fn apply_nan_clamps_to_neutral() {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(f32::NAN));
    let report = r.apply(&d).unwrap();
    assert!(r.global.exposure.is_finite());
    assert!(report.clamped.contains(&ParamId::Exposure));
}

#[test]
fn apply_rejects_wrong_value_kind() {
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    // Exposure expects F32, not Bool.
    d.0.insert(ParamId::Exposure, ParamValue::Bool(true));
    assert!(matches!(
        r.apply(&d),
        Err(lightbox_edit::RecipeError::TypeMismatch(ParamId::Exposure))
    ));
}

#[test]
fn tone_curve_validation() {
    // > 64 points → error.
    let too_many = ToneCurve {
        points: (0..70)
            .map(|i| CurvePoint {
                x: i as f32 / 69.0,
                y: 0.5,
            })
            .collect(),
    };
    let mut r = Recipe::identity(PV_M0);
    let mut d = ParamDelta::new();
    d.0.insert(
        ParamId::ToneCurve,
        ParamValue::Curve(ToneCurveSet {
            rgb: too_many,
            ..Default::default()
        }),
    );
    assert!(matches!(
        r.apply(&d),
        Err(lightbox_edit::RecipeError::TooManyCurvePoints(70))
    ));

    // Non-monotone x → error.
    let non_mono = ToneCurve {
        points: vec![CurvePoint { x: 0.5, y: 0.0 }, CurvePoint { x: 0.5, y: 1.0 }],
    };
    let mut d2 = ParamDelta::new();
    d2.0.insert(
        ParamId::ToneCurve,
        ParamValue::Curve(ToneCurveSet {
            rgb: non_mono,
            ..Default::default()
        }),
    );
    assert!(matches!(
        Recipe::identity(PV_M0).apply(&d2),
        Err(lightbox_edit::RecipeError::NonMonotoneCurve)
    ));
}

#[test]
fn is_neutral_tracks_edits() {
    let mut r = Recipe::identity(PV_M0);
    assert!(r.is_neutral());
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(1.0));
    r.apply(&d).unwrap();
    assert!(!r.is_neutral());
    // Foreign passthrough also makes a recipe non-neutral.
    let mut r2 = Recipe::identity(PV_M0);
    r2.xmp_passthrough
        .fields
        .insert("dc:subject".to_string(), CborValue::Bool(true));
    assert!(!r2.is_neutral());
}

#[test]
fn pv_flows_into_hash() {
    // Different pv → different canonical hash (pv is part of the recipe content).
    let a = Recipe::identity(ProcessVersion(1));
    let b = Recipe::identity(ProcessVersion(2));
    assert_ne!(a.canonical_hash(), b.canonical_hash());
}

#[test]
fn extension_bags_survive_cbor() {
    let mut r = Recipe::identity(PV_M0);
    r.lb_extra
        .insert("myKnob".to_string(), CborValue::Float(0.25));
    r.xmp_passthrough
        .fields
        .insert("dc:rights".to_string(), CborValue::Text("me".into()));
    let back = Recipe::from_cbor(&r.to_cbor())
        .unwrap()
        .into_recipe()
        .unwrap();
    assert_eq!(r, back);
}
