// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E09 Phase D acceptance + property tests (spec §5 T18–T21, §6).
//!
//! - T19 `Recipe::to_xmp`: `lb:` provenance + `crs:` compatibility emit + foreign
//!   passthrough verbatim; full-packet snapshot golden. exiftool is **absent** on
//!   this host, so packet validity is checked by re-parsing through the `XmpDoc`
//!   substrate (see `E09-deviations.md` D-5), not an exiftool subprocess.
//! - T20 `Recipe::read_xmp`: lb-primary round-trip is struct-equal (proptest);
//!   a `crs:`-only doc routes to `from_lr_crs`.
//! - T21 `from_lr_crs`: per-fixture fidelity partition; legacy PV≤2 skipped;
//!   `MaskGroupBasedCorrections` skipped + preserved; total over fuzzed `crs:`
//!   soup; report serializes.

use std::collections::BTreeSet;
use std::path::PathBuf;

use proptest::prelude::*;

use lightbox_edit::{
    CurvePoint, Recipe, ToneCurve, Treatment, WhiteBalance, XmpSource, XmpWriteCtx,
};
use lightbox_meta::xmp::{ns, ArrayKind, ParseLimits, XmpDoc, XmpValue};
use lightbox_types::PV_M0;

// ── helpers ────────────────────────────────────────────────────────────────

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn parse_fixture(name: &str) -> XmpDoc {
    let bytes = std::fs::read(fixture(name)).expect("read fixture");
    XmpDoc::parse(&bytes, ParseLimits::default()).expect("parse fixture")
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

fn fixed_ctx() -> XmpWriteCtx<'static> {
    XmpWriteCtx {
        app_version: "Lightbox test-fixed 0.0.0",
    }
}

/// Reserialize `to_xmp(recipe)` back into an [`XmpDoc`] — our validity oracle in
/// exiftool's absence (D-5).
fn to_xmp_reparsed(r: &Recipe) -> XmpDoc {
    let doc = r.to_xmp(&fixed_ctx()).unwrap();
    let packet = doc.serialize().unwrap();
    XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).expect("emitted packet must parse")
}

/// A representative edited recipe for the golden + emit tests.
fn edited_recipe() -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    r.base_profile.id = "Camera Standard".to_string();
    let g = &mut r.global;
    g.white_balance = WhiteBalance::Custom {
        temp_k: 5200.0,
        tint: 8.0,
    };
    g.exposure = 0.75;
    g.contrast = 15.0;
    g.highlights = -40.0;
    g.shadows = 30.0;
    g.vibrance = 10.0;
    g.presence.clarity = 12.0;
    g.hsl.bands[0].hue = 10.0;
    g.tone_curve.rgb = ToneCurve {
        points: vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 0.5, y: 0.55 },
            CurvePoint { x: 1.0, y: 1.0 },
        ],
    };
    r.geometry.crop.left = 0.05;
    r.geometry.crop.top = 0.05;
    r.geometry.crop.right = 0.95;
    r.geometry.crop.bottom = 0.95;
    r.geometry.angle = 2.5;
    r
}

/// Every `crs:` local name present in a doc, as `"crs:Name"` (partition domain).
fn crs_keys(doc: &XmpDoc) -> BTreeSet<String> {
    doc.property_names()
        .filter(|(n, _)| *n == ns::CRS)
        .map(|(_, l)| format!("crs:{l}"))
        .collect()
}

// ── T19: to_xmp ──────────────────────────────────────────────────────────────

#[test]
fn to_xmp_emits_lb_provenance_and_mapped_crs() {
    let r = edited_recipe();
    let back = to_xmp_reparsed(&r);

    // lb: provenance is present and typed.
    assert_eq!(back.get(ns::LB, "Schema").unwrap().as_f64(), Some(1.0));
    assert_eq!(
        back.get(ns::LB, "ProcessVersion").unwrap().as_f64(),
        Some(f64::from(PV_M0.0))
    );
    assert_eq!(
        back.get(ns::LB, "CreatorTool").unwrap().as_str(),
        "Lightbox test-fixed 0.0.0"
    );
    assert!(back.get(ns::LB, "RecipeCbor").is_some());

    // crs: mapped develop fields (T18 table) carry the recipe's values.
    assert_eq!(
        back.get(ns::CRS, "Exposure2012").unwrap().as_f64(),
        Some(0.75)
    );
    assert_eq!(
        back.get(ns::CRS, "Contrast2012").unwrap().as_f64(),
        Some(15.0)
    );
    assert_eq!(
        back.get(ns::CRS, "Highlights2012").unwrap().as_f64(),
        Some(-40.0)
    );
    assert_eq!(
        back.get(ns::CRS, "WhiteBalance").unwrap().as_str(),
        "Custom"
    );
    assert_eq!(
        back.get(ns::CRS, "Temperature").unwrap().as_f64(),
        Some(5200.0)
    );
    assert_eq!(
        back.get(ns::CRS, "HueAdjustmentRed").unwrap().as_f64(),
        Some(10.0)
    );
    assert_eq!(back.get(ns::CRS, "HasCrop").unwrap().as_bool(), Some(true));
    assert_eq!(
        back.get(ns::CRS, "CameraProfile").unwrap().as_str(),
        "Camera Standard"
    );
    assert_eq!(
        back.array_kind(ns::CRS, "ToneCurvePV2012"),
        Some(ArrayKind::Seq)
    );
    // A modern PV2012 head so LR reads our tone params under the right model.
    assert_eq!(
        back.get(ns::CRS, "ProcessVersion").unwrap().as_str(),
        "11.0"
    );

    // A neutral field is omitted (compact emit).
    assert!(back.get(ns::CRS, "Shadows2012").is_some()); // edited → present
    assert!(back.get(ns::CRS, "Whites2012").is_none()); // neutral → omitted
}

#[test]
fn to_xmp_reemits_foreign_passthrough_verbatim() {
    // Import a foreign doc, then export: its foreign fields must survive intact.
    let doc = parse_fixture("lr_modern_pv2012.xmp");
    let imported = Recipe::read_xmp(&doc, &fake_probe()).unwrap();
    let back = to_xmp_reparsed(&imported.recipe);

    assert_eq!(
        back.get(ns::DC, "rights").unwrap().as_str(),
        "© 2026 Test Fixture (CC0)"
    );
    let subj = back.get_array(ns::DC, "subject").unwrap();
    assert_eq!(
        subj.iter().map(XmpValue::as_str).collect::<Vec<_>>(),
        vec!["landscape", "sunset"]
    );
    assert_eq!(back.array_kind(ns::DC, "subject"), Some(ArrayKind::Bag));
    let hier = back.get_array(ns::LR, "hierarchicalSubject").unwrap();
    assert_eq!(hier[0].as_str(), "Places|Iceland");
    assert_eq!(back.get(ns::XMP, "Rating").unwrap().as_str(), "4");
}

#[test]
fn golden_full_packet_snapshot() {
    let doc = edited_recipe().to_xmp(&fixed_ctx()).unwrap();
    let packet = doc.serialize().unwrap();
    let norm = normalize_recipe_cbor(&packet);

    let path = fixture("golden_to_xmp.xmp");
    if std::env::var_os("LB_BLESS").is_some() {
        std::fs::write(&path, &norm).unwrap();
    }
    let golden =
        std::fs::read_to_string(&path).expect("golden missing — regenerate with LB_BLESS=1");
    assert_eq!(
        norm, golden,
        "to_xmp packet drifted from the golden; re-bless with LB_BLESS=1 if intended"
    );

    // The authoritative payload still round-trips exactly (golden normalizes it).
    let reparsed = XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).unwrap();
    let rt = Recipe::read_xmp(&reparsed, &fake_probe()).unwrap();
    assert_eq!(rt.recipe, edited_recipe());
}

/// Replace the (long, hash-like) `lb:RecipeCbor` hex with a length marker so the
/// golden is readable and stable while still snapshotting the full packet shape.
fn normalize_recipe_cbor(packet: &str) -> String {
    let open = "<lb:RecipeCbor>";
    let close = "</lb:RecipeCbor>";
    match (packet.find(open), packet.find(close)) {
        (Some(a), Some(b)) if a < b => {
            let hex = &packet[a + open.len()..b];
            format!(
                "{}{open}CBOR:{}bytes{}",
                &packet[..a],
                hex.len() / 2,
                &packet[b..]
            )
        }
        _ => packet.to_string(),
    }
}

// ── T20: read_xmp ────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// lb: path is authoritative: to_xmp → read_xmp is struct-equal.
    #[test]
    fn lb_roundtrip_is_struct_equal(r in arb_recipe()) {
        let doc = r.to_xmp(&fixed_ctx()).unwrap();
        let got = Recipe::read_xmp(&doc, &fake_probe()).unwrap();
        prop_assert_eq!(got.source, XmpSource::Lb);
        prop_assert_eq!(&got.recipe, &r);
    }
}

#[test]
fn crs_only_doc_routes_to_from_lr_crs() {
    let doc = parse_fixture("lr_modern_pv2012.xmp"); // no lb: payload
    let got = Recipe::read_xmp(&doc, &fake_probe()).unwrap();
    assert_eq!(got.source, XmpSource::Crs);
    assert!(!got.recipe.is_neutral());
}

// ── T21: from_lr_crs mechanism + report ──────────────────────────────────────

#[test]
fn from_lr_crs_modern_fixture_partitions_and_maps() {
    let doc = parse_fixture("lr_modern_pv2012.xmp");
    let imp = Recipe::from_lr_crs(&doc, &fake_probe());

    // Provenance.
    assert_eq!(imp.report.source_pv.as_deref(), Some("11.0"));

    // Mapped exact scalars carry their values.
    assert!((imp.recipe.global.exposure - 0.75).abs() < 1e-6);
    assert_eq!(imp.recipe.global.contrast, 15.0);
    assert_eq!(imp.recipe.global.highlights, -40.0);
    assert!(matches!(
        imp.recipe.global.white_balance,
        WhiteBalance::Custom { temp_k, tint } if temp_k == 5200.0 && tint == 8.0
    ));
    assert_eq!(imp.recipe.global.treatment, Treatment::Color);
    assert_eq!(imp.recipe.base_profile.id, "Camera Standard");
    assert_ne!(imp.recipe.geometry.crop, lightbox_edit::Crop::default());

    let mapped: BTreeSet<&str> = imp.report.mapped.iter().map(|f| f.crs.as_str()).collect();
    for k in [
        "crs:Exposure2012",
        "crs:Contrast2012",
        "crs:HueAdjustmentRed",
    ] {
        assert!(mapped.contains(k), "expected {k} mapped");
    }
    let approx: BTreeSet<&str> = imp
        .report
        .approximate
        .iter()
        .map(|f| f.crs.as_str())
        .collect();
    for k in [
        "crs:WhiteBalance",
        "crs:ToneCurvePV2012",
        "crs:CameraProfile",
        "crs:CropAngle",
    ] {
        assert!(approx.contains(k), "expected {k} approximate");
    }
    let skipped: BTreeSet<&str> = imp.report.skipped.iter().map(String::as_str).collect();
    for k in [
        "crs:Version",
        "crs:ProcessVersion",
        "crs:MaskGroupBasedCorrections",
    ] {
        assert!(skipped.contains(k), "expected {k} skipped");
    }

    // Exhaustive partition: mapped ∪ approximate ∪ skipped == every crs: key.
    assert_partition_exhaustive(&doc, &imp.report);
}

#[test]
fn from_lr_crs_legacy_skips_tone_with_report() {
    let doc = parse_fixture("lr_legacy_pv2.xmp");
    let imp = Recipe::from_lr_crs(&doc, &fake_probe());

    assert_eq!(imp.report.source_pv.as_deref(), Some("5.7"));
    // Legacy tone params are skipped-with-report (OQ4); tone is left neutral.
    let skipped: BTreeSet<&str> = imp.report.skipped.iter().map(String::as_str).collect();
    for k in ["crs:Exposure", "crs:Brightness", "crs:Contrast"] {
        assert!(skipped.contains(k), "legacy {k} must be skipped");
    }
    assert_eq!(imp.recipe.global.exposure, 0.0);
    assert_eq!(imp.recipe.global.contrast, 0.0);
    // A PV-independent field (Saturation) and CameraProfile still map.
    assert_eq!(imp.recipe.global.saturation, 20.0);
    assert_eq!(imp.recipe.base_profile.id, "Adobe Standard");
    assert_partition_exhaustive(&doc, &imp.report);
}

#[test]
fn mask_group_skipped_and_preserved() {
    let doc = parse_fixture("lr_modern_pv2012.xmp");
    let imp = Recipe::from_lr_crs(&doc, &fake_probe());
    assert!(imp
        .report
        .skipped
        .iter()
        .any(|s| s == "crs:MaskGroupBasedCorrections"));

    // Re-emit preserves whatever the substrate captured (D-4 caveat on nesting).
    let back = to_xmp_reparsed(&imp.recipe);
    let mg = back
        .get_array(ns::CRS, "MaskGroupBasedCorrections")
        .expect("mask block preserved");
    assert_eq!(mg[0].as_str(), "mask-group-0-opaque");
}

#[test]
fn crs_import_report_serializes() {
    let doc = parse_fixture("lr_modern_pv2012.xmp");
    let imp = Recipe::from_lr_crs(&doc, &fake_probe());
    let json = serde_json::to_string(&imp.report).expect("report serializes (E08/E16 seam)");
    assert!(json.contains("\"mapped\""));
    assert!(json.contains("\"skipped\""));
    assert!(json.contains("crs:Exposure2012"));
}

/// Assert `mapped ∪ approximate ∪ skipped` covers exactly the doc's `crs:` keys.
fn assert_partition_exhaustive(doc: &XmpDoc, report: &lightbox_edit::CrsImportReport) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for f in report.mapped.iter().chain(report.approximate.iter()) {
        seen.insert(f.crs.clone());
    }
    for s in &report.skipped {
        seen.insert(s.clone());
    }
    let keys = crs_keys(doc);
    assert_eq!(
        seen, keys,
        "report partition must cover exactly every crs: key seen"
    );
}

// ── §6: totality over fuzzed crs: soup + foreign round-trip ───────────────────

/// A pool of `crs:` local names: mapped, approximate, skipped, and pure noise.
const CRS_POOL: &[&str] = &[
    "Exposure2012",
    "Contrast2012",
    "HueAdjustmentRed",
    "ColorGradeShadowHue",
    "ToneCurvePV2012",
    "WhiteBalance",
    "Temperature",
    "CropTop",
    "PerspectiveUpright",
    "Version",
    "ProcessVersion",
    "HasSettings",
    "MaskGroupBasedCorrections",
    "RetouchAreas",
    "Exposure", // legacy
    "TotallyMadeUpKey",
];

fn arb_crs_value() -> impl Strategy<Value = XmpValue> {
    prop_oneof![
        (-1000.0f64..1000.0).prop_map(XmpValue::Real),
        any::<bool>().prop_map(XmpValue::Bool),
        "[^<>&]{0,12}".prop_map(XmpValue::text),
        Just(XmpValue::text("not-a-number")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `from_lr_crs` is TOTAL: never panics, the report partition is exhaustive,
    /// and the report serializes — over arbitrary crs: soup.
    #[test]
    fn from_lr_crs_total_over_soup(
        picks in prop::collection::vec((0usize..CRS_POOL.len(), arb_crs_value()), 0..24),
        foreign in prop::collection::vec("[a-z]{1,6}", 0..4),
    ) {
        let mut doc = XmpDoc::new();
        for (i, v) in &picks {
            doc.set(ns::CRS, CRS_POOL[*i], v.clone()).unwrap();
        }
        // A tone-curve-shaped array sometimes, to exercise the array path.
        if !picks.is_empty() {
            doc.set_array(
                ns::CRS,
                "ToneCurvePV2012",
                ArrayKind::Seq,
                &[XmpValue::text("0, 0"), XmpValue::text("255, 255")],
            ).unwrap();
        }
        for (k, name) in foreign.iter().enumerate() {
            doc.set(ns::DC, &format!("{name}{k}"), XmpValue::text("x")).unwrap();
        }

        let imp = Recipe::from_lr_crs(&doc, &fake_probe()); // must not panic

        // Exhaustive partition.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for f in imp.report.mapped.iter().chain(imp.report.approximate.iter()) {
            seen.insert(f.crs.clone());
        }
        for s in &imp.report.skipped {
            seen.insert(s.clone());
        }
        prop_assert_eq!(seen, crs_keys(&doc));

        // Report serializes (E08/E16 seam).
        prop_assert!(serde_json::to_string(&imp.report).is_ok());

        // The reconstructed recipe is valid CBOR (never a torn value).
        let bytes = imp.recipe.to_cbor();
        prop_assert!(Recipe::from_cbor(&bytes).is_ok());
    }
}

#[test]
fn foreign_props_survive_read_write_intact() {
    // A doc with only foreign fields (no lb:, no develop crs:) → passthrough →
    // re-emit; every foreign property is byte/value-identical (§6 contract).
    let mut doc = XmpDoc::new();
    doc.set(ns::DC, "rights", XmpValue::text("© nobody"))
        .unwrap();
    doc.set_array(
        ns::DC,
        "subject",
        ArrayKind::Bag,
        &[XmpValue::text("a"), XmpValue::text("b")],
    )
    .unwrap();
    doc.set(ns::XMP, "Rating", XmpValue::Int(3)).unwrap();

    let imp = Recipe::from_lr_crs(&doc, &fake_probe());
    let back = to_xmp_reparsed(&imp.recipe);

    assert_eq!(back.get(ns::DC, "rights").unwrap().as_str(), "© nobody");
    assert_eq!(
        back.get_array(ns::DC, "subject")
            .unwrap()
            .iter()
            .map(XmpValue::as_str)
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(back.get(ns::XMP, "Rating").unwrap().as_str(), "3");
}

// ── arbitrary valid recipe (compact; lb: round-trip is exact regardless) ─────

prop_compose! {
    fn arb_recipe()(
        exposure in -5.0f32..5.0,
        contrast in -100.0f32..100.0,
        shadows in -100.0f32..100.0,
        vibrance in -100.0f32..100.0,
        clarity in -100.0f32..100.0,
        hue_red in -100.0f32..100.0,
        angle in -45.0f32..45.0,
        bw in any::<bool>(),
        temp in 2000.0f32..50000.0,
        crop_left in 0.0f32..0.4,
    ) -> Recipe {
        let mut r = Recipe::identity(PV_M0);
        let g = &mut r.global;
        g.exposure = exposure;
        g.contrast = contrast;
        g.shadows = shadows;
        g.vibrance = vibrance;
        g.presence.clarity = clarity;
        g.hsl.bands[0].hue = hue_red;
        g.treatment = if bw { Treatment::BlackAndWhite } else { Treatment::Color };
        g.white_balance = WhiteBalance::Custom { temp_k: temp, tint: 0.0 };
        r.geometry.angle = angle;
        r.geometry.crop.left = crop_left;
        r.geometry.crop.right = 1.0 - crop_left;
        r
    }
}
