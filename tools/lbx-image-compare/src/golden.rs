// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Golden-image workflow (spec §5 T22): committed layout, bless flow,
//! failure artifacts.
//!
//! Layout (spec verbatim): `<goldens_root>/<node>/<pv>/<case>.png`, where
//! `<goldens_root>` is `crates/lightbox-render/goldens` for the render
//! nodes and `<pv>` is spelled `pv<N>`.
//!
//! Bless flow: `LIGHTBOX_BLESS=1 cargo test` regenerates goldens from the
//! current output — and **fails in CI** (blessing is a deliberate local,
//! reviewed act; CI must only ever verify).
//!
//! On mismatch the harness writes failure artifacts (actual output + ΔE
//! heatmap + the golden it was compared against) under the caller's failure
//! directory so CI can upload them (T22 AC).

use std::path::{Path, PathBuf};

use crate::{compare, diff_heatmap, CompareError, CompareReport, Rgba8Image, Tolerance};

/// Identifies one golden: `<node>/pv<pv>/<case>.png`.
#[derive(Copy, Clone, Debug)]
pub struct GoldenSpec<'a> {
    /// The render node id, e.g. `"display.transform"`.
    pub node: &'a str,
    /// The process version the golden is keyed by (architecture §4.5: goldens
    /// are per-PV — old PVs stay renderable, and testable, forever).
    pub pv: u16,
    /// Case name, e.g. `"o6-fit32"`.
    pub case: &'a str,
}

impl GoldenSpec<'_> {
    /// The golden's path under `root` (spec layout `<node>/<pv>/<case>.png`).
    pub fn path(&self, root: &Path) -> PathBuf {
        root.join(self.node)
            .join(format!("pv{}", self.pv))
            .join(format!("{}.png", self.case))
    }

    fn artifact(&self, dir: &Path, suffix: &str) -> PathBuf {
        dir.join(self.node)
            .join(format!("pv{}", self.pv))
            .join(format!("{}-{suffix}.png", self.case))
    }
}

/// How a golden check is configured.
#[derive(Clone, Debug)]
pub struct GoldenConfig {
    /// Where committed goldens live (e.g. `crates/lightbox-render/goldens`).
    pub goldens_root: PathBuf,
    /// Where failure artifacts are written (CI uploads this tree). Tests use
    /// `$CARGO_TARGET_TMPDIR/golden-failures`.
    pub failure_dir: PathBuf,
    /// Pass bar. Pixel epics use [`crate::GOLDEN_TOLERANCE`] (§4.4).
    pub tolerance: Tolerance,
    /// When `true`, (re)write the golden instead of comparing. Normally
    /// driven by the `LIGHTBOX_BLESS` env var — see [`bless_requested`].
    pub bless: bool,
}

impl GoldenConfig {
    /// The standard configuration: §4.4 tolerance, bless from the
    /// `LIGHTBOX_BLESS` env var.
    pub fn new(goldens_root: impl Into<PathBuf>, failure_dir: impl Into<PathBuf>) -> GoldenConfig {
        GoldenConfig {
            goldens_root: goldens_root.into(),
            failure_dir: failure_dir.into(),
            tolerance: crate::GOLDEN_TOLERANCE,
            bless: bless_requested(),
        }
    }
}

/// True when the environment asks to regenerate goldens
/// (`LIGHTBOX_BLESS` set to anything but `""`/`"0"`).
pub fn bless_requested() -> bool {
    match std::env::var("LIGHTBOX_BLESS") {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// True under CI (the `CI` env var, set by every mainstream CI service).
fn running_in_ci() -> bool {
    match std::env::var("CI") {
        Ok(v) => !v.is_empty() && !v.eq_ignore_ascii_case("false"),
        Err(_) => false,
    }
}

/// What a successful [`check_golden`] did.
#[derive(Debug)]
pub enum GoldenOutcome {
    /// Compared against the committed golden and passed. The report carries
    /// the measured ΔE/PSNR for logging.
    Matched(CompareReport),
    /// Bless mode: the golden was (re)written from `actual`.
    Blessed {
        /// Where the golden was written.
        path: PathBuf,
    },
}

/// The payload of [`GoldenError::Mismatch`] (boxed to keep the error slim).
#[derive(Debug)]
pub struct GoldenMismatch {
    /// Node id.
    pub node: String,
    /// Process version.
    pub pv: u16,
    /// Case name.
    pub case: String,
    /// The measured difference.
    pub report: CompareReport,
    /// The tolerance it failed.
    pub tolerance: Tolerance,
    /// Where the actual/heatmap/golden artifacts were written.
    pub artifact_dir: PathBuf,
}

impl std::fmt::Display for GoldenMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "golden mismatch for {}/pv{}/{}: {} exceeds {} — failure artifacts in {} \
             (re-bless with LIGHTBOX_BLESS=1 only if the change is intended and reviewed)",
            self.node,
            self.pv,
            self.case,
            self.report,
            self.tolerance,
            self.artifact_dir.display(),
        )
    }
}

/// Golden check failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GoldenError {
    /// `LIGHTBOX_BLESS` was set under CI. Blessing regenerates the reference
    /// images from current output — CI must only verify (T22 AC).
    #[error("LIGHTBOX_BLESS is set in CI — goldens must be blessed locally and committed")]
    BlessInCi,
    /// No committed golden at `path`. Bless locally to create it.
    #[error(
        "missing golden {path}: run `LIGHTBOX_BLESS=1 cargo test` locally, eyeball the new \
         golden, and commit it"
    )]
    MissingGolden {
        /// The expected golden path.
        path: PathBuf,
    },
    /// The output does not match the committed golden within tolerance.
    #[error("{0}")]
    Mismatch(Box<GoldenMismatch>),
    /// The comparison itself failed (size mismatch, bad PNG, IO).
    #[error("golden compare failed: {0}")]
    Compare(#[from] CompareError),
}

/// Checks `actual` against the committed golden for `spec` — or, in bless
/// mode, rewrites the golden (never in CI).
///
/// On mismatch, writes `<case>-actual.png`, `<case>-heatmap.png` and
/// `<case>-golden.png` under `cfg.failure_dir` and returns
/// [`GoldenError::Mismatch`] describing the measured ΔE2000/PSNR.
pub fn check_golden(
    cfg: &GoldenConfig,
    spec: &GoldenSpec<'_>,
    actual: &Rgba8Image,
) -> Result<GoldenOutcome, GoldenError> {
    check_golden_inner(cfg, spec, actual, running_in_ci())
}

fn check_golden_inner(
    cfg: &GoldenConfig,
    spec: &GoldenSpec<'_>,
    actual: &Rgba8Image,
    in_ci: bool,
) -> Result<GoldenOutcome, GoldenError> {
    let golden_path = spec.path(&cfg.goldens_root);
    if cfg.bless {
        if in_ci {
            return Err(GoldenError::BlessInCi);
        }
        actual.write_png(&golden_path)?;
        return Ok(GoldenOutcome::Blessed { path: golden_path });
    }

    if !golden_path.is_file() {
        return Err(GoldenError::MissingGolden { path: golden_path });
    }
    let golden = Rgba8Image::read_png(&golden_path)?;
    let report = compare(&golden, actual)?;
    if report.passes(cfg.tolerance) {
        return Ok(GoldenOutcome::Matched(report));
    }

    // Failure artifacts for humans + CI upload (T22 AC).
    actual.write_png(&spec.artifact(&cfg.failure_dir, "actual"))?;
    diff_heatmap(&golden, actual)?.write_png(&spec.artifact(&cfg.failure_dir, "heatmap"))?;
    golden.write_png(&spec.artifact(&cfg.failure_dir, "golden"))?;
    Err(GoldenError::Mismatch(Box::new(GoldenMismatch {
        node: spec.node.to_owned(),
        pv: spec.pv,
        case: spec.case.to_owned(),
        report,
        tolerance: cfg.tolerance,
        artifact_dir: cfg.failure_dir.clone(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Rgba8Image {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[
                    (x * 255 / w.max(1)) as u8,
                    (y * 255 / h.max(1)) as u8,
                    128,
                    255,
                ]);
            }
        }
        Rgba8Image::new(w, h, px).unwrap()
    }

    fn cfg(root: &Path, bless: bool) -> GoldenConfig {
        GoldenConfig {
            goldens_root: root.join("goldens"),
            failure_dir: root.join("failures"),
            tolerance: crate::GOLDEN_TOLERANCE,
            bless,
        }
    }

    const SPEC: GoldenSpec<'static> = GoldenSpec {
        node: "test.node",
        pv: 1,
        case: "case-a",
    };

    #[test]
    fn bless_then_match_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let img = gradient(16, 12);

        // Bless writes the golden at the spec'd layout.
        let out = check_golden_inner(&cfg(dir.path(), true), &SPEC, &img, false).unwrap();
        let GoldenOutcome::Blessed { path } = out else {
            panic!("expected Blessed, got {out:?}");
        };
        assert_eq!(
            path,
            dir.path().join("goldens/test.node/pv1/case-a.png"),
            "spec layout <node>/<pv>/<case>.png"
        );

        // Identical output matches with ΔE 0 / PSNR ∞.
        match check_golden_inner(&cfg(dir.path(), false), &SPEC, &img, true).unwrap() {
            GoldenOutcome::Matched(report) => {
                assert_eq!(report.max_de, 0.0);
                assert!(report.psnr_db.is_infinite());
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn bless_in_ci_fails() {
        let dir = tempfile::tempdir().unwrap();
        let img = gradient(4, 4);
        let err = check_golden_inner(&cfg(dir.path(), true), &SPEC, &img, true).unwrap_err();
        assert!(matches!(err, GoldenError::BlessInCi));
        assert!(
            !dir.path().join("goldens/test.node/pv1/case-a.png").exists(),
            "no golden may be written in CI"
        );
    }

    #[test]
    fn missing_golden_names_the_path_and_the_bless_flow() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            check_golden_inner(&cfg(dir.path(), false), &SPEC, &gradient(4, 4), true).unwrap_err();
        match &err {
            GoldenError::MissingGolden { path } => {
                assert!(path.ends_with("test.node/pv1/case-a.png"));
            }
            other => panic!("expected MissingGolden, got {other:?}"),
        }
        assert!(err.to_string().contains("LIGHTBOX_BLESS=1"));
    }

    #[test]
    fn mismatch_writes_failure_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let img = gradient(16, 12);
        check_golden_inner(&cfg(dir.path(), true), &SPEC, &img, false).unwrap();

        // A hard shift (inverted red channel) must fail and leave artifacts.
        let mut broken = img.clone();
        for p in broken.px.chunks_exact_mut(4) {
            p[0] = 255 - p[0];
        }
        let err = check_golden_inner(&cfg(dir.path(), false), &SPEC, &broken, true).unwrap_err();
        assert!(matches!(err, GoldenError::Mismatch(_)), "{err}");
        for suffix in ["actual", "heatmap", "golden"] {
            let p = dir
                .path()
                .join(format!("failures/test.node/pv1/case-a-{suffix}.png"));
            assert!(p.is_file(), "missing artifact {}", p.display());
            Rgba8Image::read_png(&p).expect("artifact decodes");
        }
    }

    #[test]
    fn size_mismatch_is_a_compare_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        check_golden_inner(&cfg(dir.path(), true), &SPEC, &gradient(16, 12), false).unwrap();
        let err =
            check_golden_inner(&cfg(dir.path(), false), &SPEC, &gradient(8, 8), true).unwrap_err();
        assert!(matches!(
            err,
            GoldenError::Compare(CompareError::DimensionMismatch { .. })
        ));
    }
}
