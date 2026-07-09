// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T19 acceptance: `probe()` of every pinned fixture returns the committed
//! expected values (`probe_expectations.toml`), the malformed corpus is an
//! `Err` (never a panic), and probing stays metadata-cheap.
//!
//! Requires the fixture corpus: `cargo xtask fixtures` (CI fetches it before
//! the test job; the corpus is hash-pinned so expectations cannot drift).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lightbox_decode::{probe, ProbeError, ProbedFormat};
use serde::Deserialize;

#[derive(Deserialize)]
struct Expectations {
    fixture: Vec<Expect>,
}

#[derive(Deserialize)]
struct Expect {
    name: String,
    /// `"malformed"` for the corrupt corpus; all other fields absent then.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    orientation: Option<u16>,
    #[serde(default)]
    make: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    capture_time: Option<String>,
    #[serde(default)]
    embedded: Vec<ExpectEmbedded>,
}

#[derive(Deserialize)]
struct ExpectEmbedded {
    width: u32,
    height: u32,
    start: u64,
    end: u64,
}

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").exists(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

fn load_expectations() -> Expectations {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/probe_expectations.toml");
    let text = std::fs::read_to_string(&path).expect("read probe_expectations.toml");
    toml::from_str(&text).expect("parse probe_expectations.toml")
}

/// Every fixture on disk must have an expectation entry — a fixture added to
/// the manifest without a committed expectation is a test-plan hole.
#[test]
fn expectations_cover_the_whole_corpus() {
    let dir = fixtures_dir();
    let expected: Vec<String> = load_expectations()
        .fixture
        .into_iter()
        .map(|e| e.name)
        .collect();
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("read fixtures dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n != "manifest.toml" && !n.starts_with('.'))
        .collect();
    on_disk.sort();
    let mut listed = expected.clone();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "fixtures on disk and probe_expectations.toml entries must match 1:1"
    );
}

#[test]
fn probe_matches_committed_expectations() {
    let dir = fixtures_dir();
    for exp in load_expectations().fixture {
        let path = dir.join(&exp.name);

        if let Some(kind) = &exp.error {
            assert_eq!(kind, "malformed", "{}: unknown expected error", exp.name);
            // T19 AC: the malformed corpus returns Err — and never panics.
            let out = probe(&path);
            assert!(
                matches!(out, Err(ProbeError::Malformed(_))),
                "{}: expected Malformed, got {out:?}",
                exp.name
            );
            continue;
        }

        let got = probe(&path).unwrap_or_else(|e| panic!("{}: probe failed: {e}", exp.name));
        assert_eq!(
            got.format.catalog_tag(),
            exp.format.as_deref().expect("format expected"),
            "{}: format",
            exp.name
        );
        // Raw formats must be ProbedFormat::Raw specifically (the tag alone
        // would not catch a Raw→Unsupported regression… but catalog_tag
        // already distinguishes; assert the shape for raws explicitly).
        if !matches!(
            got.format.catalog_tag(),
            "JPEG" | "TIFF" | "PNG" | "UNSUPPORTED"
        ) {
            assert!(
                matches!(got.format, ProbedFormat::Raw(_)),
                "{}: expected Raw(..), got {:?}",
                exp.name,
                got.format
            );
        }
        assert_eq!(got.width, exp.width.expect("width"), "{}: width", exp.name);
        assert_eq!(
            got.height,
            exp.height.expect("height"),
            "{}: height",
            exp.name
        );
        assert_eq!(
            got.orientation.exif_value(),
            exp.orientation.expect("orientation"),
            "{}: orientation",
            exp.name
        );
        assert_eq!(got.camera_make, exp.make, "{}: make", exp.name);
        assert_eq!(got.camera_model, exp.model, "{}: model", exp.name);
        assert_eq!(
            got.capture_time, exp.capture_time,
            "{}: capture_time",
            exp.name
        );
        assert_eq!(
            got.file_bytes,
            std::fs::metadata(&path).unwrap().len(),
            "{}: file_bytes",
            exp.name
        );

        assert_eq!(
            got.embedded.len(),
            exp.embedded.len(),
            "{}: embedded preview count ({:?})",
            exp.name,
            got.embedded
        );
        for (i, (g, e)) in got.embedded.iter().zip(&exp.embedded).enumerate() {
            assert_eq!(
                (g.width, g.height, g.byte_range.start, g.byte_range.end),
                (e.width, e.height, e.start, e.end),
                "{}: embedded[{i}]",
                exp.name
            );
        }
        // Largest-first ordering (spec §3.7).
        let areas: Vec<u64> = got
            .embedded
            .iter()
            .map(|p| u64::from(p.width) * u64::from(p.height))
            .collect();
        assert!(
            areas.windows(2).all(|w| w[0] >= w[1]),
            "{}: embedded not sorted largest-first: {areas:?}",
            exp.name
        );
    }
}

/// E04 T1 AC: every fixture's `SourceKind` matches its `ProbedFormat` (the
/// pinned raw mounts map `Raw`; the JPEG/TIFF/PNG fixtures map `Rendered`;
/// the malformed corpus never reaches `source_kind` — probing itself fails).
#[test]
fn source_kind_matches_probed_format() {
    use lightbox_types::SourceKind;

    let dir = fixtures_dir();
    let mut raw_seen = 0;
    let mut rendered_seen = 0;
    for exp in load_expectations().fixture {
        if exp.error.is_some() {
            continue;
        }
        let path = dir.join(&exp.name);
        let got = probe(&path).expect("probe");
        match &got.format {
            ProbedFormat::Raw(_) => {
                assert_eq!(
                    got.format.source_kind(),
                    Some(SourceKind::Raw),
                    "{}",
                    exp.name
                );
                raw_seen += 1;
            }
            ProbedFormat::Jpeg | ProbedFormat::Tiff | ProbedFormat::Png => {
                assert_eq!(
                    got.format.source_kind(),
                    Some(SourceKind::Rendered),
                    "{}",
                    exp.name
                );
                rendered_seen += 1;
            }
            ProbedFormat::Unsupported(_) => {
                assert_eq!(got.format.source_kind(), None, "{}", exp.name);
            }
        }
    }
    assert!(raw_seen >= 7, "expected at least the pinned raw mounts, got {raw_seen}");
    assert!(rendered_seen >= 2, "expected at least the JPEG+TIFF/PNG fixtures");
}

/// T19 AC: metadata-only — a 45 MB raw probes in < 20 ms on a dev laptop.
/// Timing asserts are flaky under CI load, so the hard budget lives in the
/// T28 perf harness; here we log warm timings and only fail on a blowup
/// that would indicate an accidental full-file read.
#[test]
fn probe_is_metadata_cheap() {
    let dir = fixtures_dir();
    for exp in load_expectations().fixture {
        if exp.error.is_some() {
            continue;
        }
        let path = dir.join(&exp.name);
        let _warmup = probe(&path);
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            let _ = probe(&path).expect("probe");
            best = best.min(t0.elapsed());
        }
        println!("probe {:<24} warm best {:?}", exp.name, best);
        assert!(
            best < Duration::from_millis(250),
            "{}: probe took {best:?} — is something reading the whole file?",
            exp.name
        );
    }
}
