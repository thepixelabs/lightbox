// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask fixtures` — the pinned test-fixture corpus (E01 spec §5 T4).
//!
//! The committed `fixtures/manifest.toml` is the source of truth: URL (or
//! deterministic generator), xxh3-128 pin, license, and provenance per
//! fixture. This command makes `fixtures/` match it:
//!
//! - already present + pin matches → **cached** (no network: idempotent and
//!   offline-safe once fetched);
//! - generated fixtures are (re)built deterministically;
//! - remote fixtures are downloaded via `curl` (temp file + atomic rename);
//! - every byte on disk is verified against its xxh3-128 pin (and upstream
//!   sha256 where published) — a mismatch is an error, never a shrug.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::generate::Generator;
use crate::hashing;

/// Parsed `fixtures/manifest.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The corpus, in order (truncations must follow their source).
    pub fixture: Vec<Fixture>,
}

/// One pinned fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// File name under `fixtures/`.
    pub name: String,
    /// Corpus role: `raw`, `image`, or `corrupt`.
    pub kind: Kind,
    /// SPDX license id of the fixture content (provenance discipline).
    pub license: String,
    /// Human-readable provenance (camera, upstream page, generator note).
    pub origin: String,
    /// Download URL — exactly one of `url` / `generator` must be set.
    #[serde(default)]
    pub url: Option<String>,
    /// Deterministic generator spec — see [`Generator::parse`].
    #[serde(default)]
    pub generator: Option<String>,
    /// Upstream-published sha256 (raw.pixls.us), verified when present.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Our xxh3-128 pin (canonical big-endian hex). Empty string = authoring
    /// mode: the run computes and reports it, then fails.
    pub xxh3_128: String,
}

/// Corpus role of a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Camera raw file (CC0, from raw.pixls.us).
    Raw,
    /// Non-raw image (JPEG/TIFF/PNG).
    Image,
    /// Deliberately truncated/garbage input — must never crash a probe.
    Corrupt,
}

impl Kind {
    /// Manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Raw => "raw",
            Kind::Image => "image",
            Kind::Corrupt => "corrupt",
        }
    }
}

/// How each fixture was satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Present and pin-verified; nothing done.
    Cached,
    /// Fetched from its URL this run.
    Downloaded,
    /// (Re)built by its generator this run.
    Generated,
}

/// Options for [`ensure_fixtures`].
#[derive(Debug, Clone, Copy, Default)]
pub struct EnsureOptions {
    /// Fail instead of downloading (generation still allowed — it is local
    /// and deterministic). Cached fixtures always pass.
    pub offline: bool,
    /// Re-fetch/regenerate even when a file is present (repairs mismatches).
    pub force: bool,
}

/// Ensures `fixtures_dir` matches `manifest_path`. Returns per-fixture outcomes.
pub fn ensure_fixtures(
    manifest_path: &Path,
    fixtures_dir: &Path,
    opts: EnsureOptions,
) -> anyhow::Result<Vec<(String, Kind, Outcome)>> {
    let manifest = load_manifest(manifest_path)?;
    std::fs::create_dir_all(fixtures_dir)
        .with_context(|| format!("cannot create {}", fixtures_dir.display()))?;

    let mut outcomes = Vec::with_capacity(manifest.fixture.len());
    let mut unpinned: Vec<(String, String)> = Vec::new();

    for fixture in &manifest.fixture {
        let path = fixtures_dir.join(&fixture.name);

        let outcome = if !opts.force && path.is_file() {
            match verify(fixture, &path)? {
                Verification::Ok => Outcome::Cached,
                Verification::Unpinned(computed) => {
                    unpinned.push((fixture.name.clone(), computed));
                    Outcome::Cached
                }
                Verification::Mismatch { expected, computed } => bail!(
                    "fixture {} exists but its xxh3-128 is {computed}, manifest pins \
                     {expected} — delete it or rerun with --force to re-fetch",
                    path.display()
                ),
            }
        } else {
            let outcome = materialize(fixture, fixtures_dir, &path, opts)?;
            match verify(fixture, &path)? {
                Verification::Ok => outcome,
                Verification::Unpinned(computed) => {
                    unpinned.push((fixture.name.clone(), computed));
                    outcome
                }
                Verification::Mismatch { expected, computed } => bail!(
                    "freshly materialized fixture {} hashes to {computed}, manifest pins \
                     {expected} — upstream content changed or the manifest is wrong",
                    path.display()
                ),
            }
        };
        outcomes.push((fixture.name.clone(), fixture.kind, outcome));
    }

    if !unpinned.is_empty() {
        eprintln!("fixtures present but UNPINNED — add to fixtures/manifest.toml:");
        for (name, hash) in &unpinned {
            eprintln!("  {name}: xxh3_128 = \"{hash}\"");
        }
        bail!("{} fixture(s) lack an xxh3_128 pin", unpinned.len());
    }
    Ok(outcomes)
}

fn load_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read manifest {}", path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("bad manifest {}", path.display()))?;
    for f in &manifest.fixture {
        match (&f.url, &f.generator) {
            (Some(_), None) | (None, Some(_)) => {}
            _ => bail!(
                "fixture {:?}: exactly one of `url` or `generator` must be set",
                f.name
            ),
        }
        if let Some(g) = &f.generator {
            Generator::parse(g).with_context(|| format!("fixture {:?}", f.name))?;
        }
        if f.name.contains('/') || f.name.contains('\\') || f.name.starts_with('.') {
            bail!("fixture {:?}: name must be a plain file name", f.name);
        }
        if f.license.is_empty() || f.origin.is_empty() {
            bail!(
                "fixture {:?}: license and origin are mandatory (provenance discipline, T4)",
                f.name
            );
        }
    }
    Ok(manifest)
}

enum Verification {
    Ok,
    Unpinned(String),
    Mismatch { expected: String, computed: String },
}

fn verify(fixture: &Fixture, path: &Path) -> anyhow::Result<Verification> {
    let computed = hashing::xxh3_hex(hashing::xxh3_128_file(path)?);
    if let Some(expected_sha) = &fixture.sha256 {
        let sha = hashing::sha256_file(path)?;
        if &sha != expected_sha {
            return Ok(Verification::Mismatch {
                expected: format!("(sha256) {expected_sha}"),
                computed: format!("(sha256) {sha}"),
            });
        }
    }
    if fixture.xxh3_128.is_empty() {
        return Ok(Verification::Unpinned(computed));
    }
    if computed == fixture.xxh3_128.to_lowercase() {
        Ok(Verification::Ok)
    } else {
        Ok(Verification::Mismatch {
            expected: fixture.xxh3_128.clone(),
            computed,
        })
    }
}

fn materialize(
    fixture: &Fixture,
    fixtures_dir: &Path,
    path: &Path,
    opts: EnsureOptions,
) -> anyhow::Result<Outcome> {
    if let Some(spec) = &fixture.generator {
        let bytes = Generator::parse(spec)?.run(fixtures_dir)?;
        write_atomic(path, &bytes)?;
        return Ok(Outcome::Generated);
    }
    let url = fixture.url.as_deref().expect("validated by load_manifest");
    if opts.offline {
        bail!(
            "fixture {} is missing and --offline was given (source: {url})",
            path.display()
        );
    }
    download(url, path)?;
    Ok(Outcome::Downloaded)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = part_path(path);
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot rename {} into place", tmp.display()))?;
    Ok(())
}

fn download(url: &str, path: &Path) -> anyhow::Result<()> {
    let tmp = part_path(path);
    // curl ships on all three dev/CI platforms; shelling out keeps xtask free
    // of a TLS dependency tree.
    let status = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--fail",
            "--retry",
            "3",
        ])
        .arg("--output")
        .arg(&tmp)
        .arg(url)
        .status()
        .context("cannot spawn curl — is it installed and on PATH?")?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("curl failed ({status}) downloading {url}");
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot rename {} into place", tmp.display()))?;
    Ok(())
}

fn part_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("manifest.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    const GENERATED_ONLY: &str = r#"
[[fixture]]
name = "tiny.png"
kind = "image"
license = "CC0-1.0"
origin = "generated"
generator = "png-gradient-8x8"
xxh3_128 = "PIN_PNG"

[[fixture]]
name = "noise.bin"
kind = "corrupt"
license = "CC0-1.0"
origin = "generated"
generator = "garbage:1024"
xxh3_128 = "PIN_NOISE"

[[fixture]]
name = "noise-head.bin"
kind = "corrupt"
license = "CC0-1.0"
origin = "generated"
generator = "truncate:noise.bin:100"
xxh3_128 = "PIN_HEAD"
"#;

    /// Fills real pins into the template by generating once out-of-band.
    fn pinned_manifest(dir: &Path) -> String {
        let png = Generator::PngGradient8x8.run(dir).unwrap();
        let noise = Generator::Garbage { bytes: 1024 }.run(dir).unwrap();
        let head = &noise[..100];
        GENERATED_ONLY
            .replace("PIN_PNG", &hashing::xxh3_hex(hashing::xxh3_128_bytes(&png)))
            .replace(
                "PIN_NOISE",
                &hashing::xxh3_hex(hashing::xxh3_128_bytes(&noise)),
            )
            .replace(
                "PIN_HEAD",
                &hashing::xxh3_hex(hashing::xxh3_128_bytes(head)),
            )
    }

    #[test]
    fn ensure_is_idempotent_and_offline_safe_once_cached() {
        // T4 acceptance criterion, network-free variant.
        let tmp = tempfile::tempdir().unwrap();
        let fixtures_dir = tmp.path().join("fixtures");
        std::fs::create_dir(&fixtures_dir).unwrap();
        let manifest = write_manifest(tmp.path(), &pinned_manifest(&fixtures_dir));

        let first = ensure_fixtures(&manifest, &fixtures_dir, EnsureOptions::default()).unwrap();
        assert!(first.iter().all(|(_, _, o)| *o == Outcome::Generated));

        // Second run: everything cached, and --offline succeeds.
        let second = ensure_fixtures(
            &manifest,
            &fixtures_dir,
            EnsureOptions {
                offline: true,
                force: false,
            },
        )
        .unwrap();
        assert!(second.iter().all(|(_, _, o)| *o == Outcome::Cached));
    }

    #[test]
    fn offline_fails_for_missing_remote_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let fixtures_dir = tmp.path().join("fixtures");
        let manifest = write_manifest(
            tmp.path(),
            r#"
[[fixture]]
name = "remote.raw"
kind = "raw"
license = "CC0-1.0"
origin = "example"
url = "https://example.invalid/remote.raw"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
xxh3_128 = "00000000000000000000000000000000"
"#,
        );
        let err = ensure_fixtures(
            &manifest,
            &fixtures_dir,
            EnsureOptions {
                offline: true,
                force: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--offline"), "{err}");
    }

    #[test]
    fn corrupted_cache_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let fixtures_dir = tmp.path().join("fixtures");
        std::fs::create_dir(&fixtures_dir).unwrap();
        let manifest = write_manifest(tmp.path(), &pinned_manifest(&fixtures_dir));

        ensure_fixtures(&manifest, &fixtures_dir, EnsureOptions::default()).unwrap();
        // Flip a byte in a cached fixture.
        let victim = fixtures_dir.join("tiny.png");
        let mut bytes = std::fs::read(&victim).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&victim, &bytes).unwrap();

        let err = ensure_fixtures(&manifest, &fixtures_dir, EnsureOptions::default()).unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");

        // --force regenerates and heals it.
        let healed = ensure_fixtures(
            &manifest,
            &fixtures_dir,
            EnsureOptions {
                offline: true,
                force: true,
            },
        )
        .unwrap();
        assert!(healed.iter().all(|(_, _, o)| *o == Outcome::Generated));
    }

    #[test]
    fn manifest_validation_rejects_bad_entries() {
        let tmp = tempfile::tempdir().unwrap();
        // Both url and generator.
        let both = write_manifest(
            tmp.path(),
            r#"
[[fixture]]
name = "x.png"
kind = "image"
license = "CC0-1.0"
origin = "x"
url = "https://example.invalid/x"
generator = "png-gradient-8x8"
xxh3_128 = ""
"#,
        );
        assert!(load_manifest(&both).is_err());
        // Neither.
        let neither = write_manifest(
            tmp.path(),
            r#"
[[fixture]]
name = "x.png"
kind = "image"
license = "CC0-1.0"
origin = "x"
xxh3_128 = ""
"#,
        );
        assert!(load_manifest(&neither).is_err());
        // Path traversal in name.
        let traversal = write_manifest(
            tmp.path(),
            r#"
[[fixture]]
name = "../evil"
kind = "image"
license = "CC0-1.0"
origin = "x"
generator = "png-gradient-8x8"
xxh3_128 = ""
"#,
        );
        assert!(load_manifest(&traversal).is_err());
    }

    #[test]
    fn unpinned_fixture_reports_hash_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let fixtures_dir = tmp.path().join("fixtures");
        let manifest = write_manifest(
            tmp.path(),
            r#"
[[fixture]]
name = "tiny.png"
kind = "image"
license = "CC0-1.0"
origin = "generated"
generator = "png-gradient-8x8"
xxh3_128 = ""
"#,
        );
        let err = ensure_fixtures(&manifest, &fixtures_dir, EnsureOptions::default()).unwrap_err();
        assert!(err.to_string().contains("pin"), "{err}");
        // The file was still materialized so the author can inspect it.
        assert!(fixtures_dir.join("tiny.png").is_file());
    }

    #[test]
    fn real_manifest_parses_and_covers_the_spec_corpus() {
        // Guards the committed manifest: parseable, licensed, corrupt files
        // present, and the seven spec'd raw mounts covered (spec §5 T4).
        let root = crate::workspace_root();
        let manifest = load_manifest(&root.join("fixtures/manifest.toml")).unwrap();
        assert!(
            manifest.fixture.iter().all(|f| !f.license.is_empty()),
            "every fixture must carry a license"
        );
        assert!(
            manifest.fixture.iter().all(|f| !f.xxh3_128.is_empty()),
            "every committed fixture must be pinned"
        );
        let corrupt = manifest
            .fixture
            .iter()
            .filter(|f| f.kind == Kind::Corrupt)
            .count();
        assert!(
            corrupt >= 2,
            "spec requires 2 deliberately corrupt fixtures"
        );
        for ext in ["cr2", "cr3", "nef", "arw", "raf", "orf", "dng"] {
            assert!(
                manifest
                    .fixture
                    .iter()
                    .any(|f| f.kind == Kind::Raw && f.name.ends_with(ext)),
                "no raw fixture for .{ext}"
            );
        }
        for ext in ["jpg", "tiff", "png"] {
            assert!(
                manifest.fixture.iter().any(|f| f.name.ends_with(ext)),
                "no fixture for .{ext}"
            );
        }
    }
}
