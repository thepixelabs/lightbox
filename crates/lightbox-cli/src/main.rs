// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-cli` — headless driver over `lightbox-core` (E01 spec §3.10,
//! T27).
//!
//! ```text
//! lightbox-cli create --catalog <dir>.lbdata
//! lightbox-cli import --catalog <dir> --add <src-dir> [--recursive]
//! lightbox-cli list   --catalog <dir> [--folder <id>] [--limit <n>] [--json]
//! lightbox-cli render --catalog <dir> --image <id> --out <out.png> [--width <n>] [--cpu]
//! lightbox-cli backup --catalog <dir>
//! lightbox-cli check  --catalog <dir>
//! ```
//!
//! This binary is the headless proof of seam 1 (spec §8 DoD 4): it drives
//! `create → import → list → render → backup → check` through the
//! [`lightbox_core`] façade with **zero** dependency on `lightbox-shell`,
//! egui or winit (asserted in CI via `cargo tree`). `render` rides the same
//! `Engine::submit`/`poll` ticket lifecycle as the shell's loupe, with
//! `RenderTarget::CpuBuffer` (GPU when an adapter exists, `--cpu` forces the
//! CPU node path).
//!
//! Exit codes: `0` success · `1` failure · `2` usage error ·
//! `3` catalog corrupt/refused (`check` distinguishes ok/corrupt — T27 AC).
//!
//! Error-taxonomy convention (spec T4): `anyhow` is allowed here because
//! this is a binary; library crates use `thiserror`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use lightbox_core::{
    check_catalog, observability, CatalogError, CloseOpts, ClosePolicy, Command, Core, CoreConfig,
    CoreError, Event, ImageQuery, ImageSummary, IntegrityStatus, Session, SortOrder,
};
use lightbox_edit::Recipe;
use lightbox_render::{
    BackendKind, GpuContext, RenderOutput, RenderRequest, RenderScale, RenderState, RenderTarget,
    Roi, ViewportId,
};
use lightbox_types::{FolderId, ImageId, PV_M0};

/// Exit code for usage errors (bad flags, missing arguments).
const EXIT_USAGE: u8 = 2;
/// Exit code when the catalog is corrupt or refused (`check` — T27 AC).
const EXIT_CORRUPT: u8 = 3;

const USAGE: &str = "\
lightbox-cli — headless Lightbox driver (E01 spec §3.10)

USAGE:
  lightbox-cli create --catalog <dir>.lbdata
  lightbox-cli import --catalog <dir> --add <src-dir> [--recursive]
  lightbox-cli list   --catalog <dir> [--folder <id>] [--limit <n>] [--json]
  lightbox-cli render --catalog <dir> --image <id> --out <out.png> [--width <n>] [--cpu]
  lightbox-cli backup --catalog <dir>
  lightbox-cli check  --catalog <dir>

EXIT CODES:
  0 success | 1 failure | 2 usage error | 3 catalog corrupt/refused
";

fn main() -> ExitCode {
    // Quiet by default (stderr is for progress + errors); LIGHTBOX_LOG /
    // RUST_LOG opt into the full tracing firehose.
    let filter =
        if std::env::var(observability::LOG_ENV_VAR).is_ok() || std::env::var("RUST_LOG").is_ok() {
            None
        } else {
            Some("warn".to_owned())
        };
    if let Err(e) = observability::init(&observability::ObservabilityOptions {
        log_dir: None,
        filter,
    }) {
        eprintln!("warning: observability init failed: {e}");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err:#}");
            // A corrupt/refused catalog is a distinct, scriptable outcome.
            if is_corrupt_error(&err) {
                ExitCode::from(EXIT_CORRUPT)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// True when the error chain bottoms out in a catalog-corruption refusal.
fn is_corrupt_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CoreError>(),
            Some(CoreError::Catalog(CatalogError::Corrupt { .. }))
        ) || matches!(
            cause.downcast_ref::<CatalogError>(),
            Some(CatalogError::Corrupt { .. })
        )
    })
}

fn run(args: &[String]) -> anyhow::Result<u8> {
    let Some(cmd) = args.first() else {
        eprint!("{USAGE}");
        return Ok(EXIT_USAGE);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "create" => cmd_create(rest),
        "import" => cmd_import(rest),
        "list" => cmd_list(rest),
        "render" => cmd_render(rest),
        "backup" => cmd_backup(rest),
        "check" => cmd_check(rest),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(0)
        }
        other => {
            eprintln!("unknown subcommand {other:?}\n\n{USAGE}");
            Ok(EXIT_USAGE)
        }
    }
}

// ---------------------------------------------------------------------------
// Flag parsing (deliberately dependency-free: six fixed subcommands).
// ---------------------------------------------------------------------------

/// Tiny flag cursor: `take_value("--flag")` / `take_switch("--flag")`,
/// then `finish()` rejects leftovers. Usage errors are [`UsageError`].
struct Flags {
    args: Vec<Option<String>>,
}

/// Marker for usage errors so `run`'s caller maps them to exit code 2.
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

impl std::error::Error for UsageError {}

impl Flags {
    fn new(args: &[String]) -> Flags {
        Flags {
            args: args.iter().cloned().map(Some).collect(),
        }
    }

    /// Consumes `--name <value>`; `None` when the flag is absent.
    fn take_value(&mut self, name: &str) -> Result<Option<String>, UsageError> {
        for i in 0..self.args.len() {
            if self.args[i].as_deref() == Some(name) {
                let value = self
                    .args
                    .get_mut(i + 1)
                    .and_then(Option::take)
                    .ok_or_else(|| UsageError(format!("{name} requires a value")))?;
                if value.starts_with("--") {
                    return Err(UsageError(format!("{name} requires a value, got {value}")));
                }
                self.args[i] = None;
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Consumes `--name` as a boolean switch.
    fn take_switch(&mut self, name: &str) -> bool {
        for slot in &mut self.args {
            if slot.as_deref() == Some(name) {
                *slot = None;
                return true;
            }
        }
        false
    }

    /// Errors on any argument no subcommand consumed.
    fn finish(self) -> Result<(), UsageError> {
        let leftover: Vec<String> = self.args.into_iter().flatten().collect();
        if leftover.is_empty() {
            Ok(())
        } else {
            Err(UsageError(format!(
                "unexpected argument(s): {}",
                leftover.join(" ")
            )))
        }
    }
}

/// `--catalog` is required by every subcommand.
fn required_catalog(flags: &mut Flags) -> Result<PathBuf, UsageError> {
    flags
        .take_value("--catalog")?
        .map(PathBuf::from)
        .ok_or_else(|| UsageError("--catalog <dir>.lbdata is required".to_owned()))
}

fn parse_num<T: std::str::FromStr>(name: &str, v: &str) -> Result<T, UsageError> {
    v.parse()
        .map_err(|_| UsageError(format!("{name} expects a number, got {v:?}")))
}

/// Runs a subcommand body, mapping [`UsageError`] to exit code 2.
fn with_usage(f: impl FnOnce() -> anyhow::Result<u8>) -> anyhow::Result<u8> {
    match f() {
        Err(e) if e.is::<UsageError>() => {
            eprintln!("error: {e}");
            Ok(EXIT_USAGE)
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn cmd_create(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        flags.finish()?;

        let core = Core::start(CoreConfig::default())?;
        let session = core.create_catalog(&catalog, None)?;
        let version = session.schema_version();
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        println!("created {} (schema version {version})", catalog.display());
        Ok(0)
    })
}

fn cmd_import(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let src = flags
            .take_value("--add")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("import requires --add <src-dir>".to_owned()))?;
        let recursive = flags.take_switch("--recursive");
        flags.finish()?;

        let core = Core::start(CoreConfig::default())?;
        let session = core.open_catalog(&catalog, None)?;
        // Subscribe BEFORE submitting: broadcast only delivers what comes
        // after the subscription.
        let mut rx = session.events();
        let ticket = session.submit(Command::ImportAddInPlace {
            source_dir: src.clone(),
            recursive,
        });

        let mut last_print = Instant::now() - Duration::from_secs(1);
        let report = loop {
            match rx.try_recv() {
                Ok(Event::ImportProgress {
                    done,
                    discovered,
                    current,
                    ..
                }) => {
                    // ≤ ~4 Hz progress on stderr; stdout stays scriptable.
                    if last_print.elapsed() >= Duration::from_millis(250) {
                        eprintln!("importing {done}/{discovered} — {}", current.display());
                        last_print = Instant::now();
                    }
                }
                Ok(Event::ImportFinished { report, .. }) => break report,
                Ok(Event::CommandFailed {
                    ticket: failed,
                    error,
                }) if failed == ticket => bail!("import failed: {error}"),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    bail!("event stream closed before the import finished")
                }
            }
        };

        for (path, why) in &report.errors {
            eprintln!("error: {}: {why}", path.display());
        }
        println!(
            "imported {} (skipped {} duplicates, {} unsupported, {} errors) in {:.2}s",
            report.imported,
            report.skipped_duplicates,
            report.unsupported,
            report.errors.len(),
            report.took.as_secs_f64(),
        );
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

fn cmd_list(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let folder = flags
            .take_value("--folder")?
            .map(|v| parse_num::<i64>("--folder", &v).map(FolderId))
            .transpose()?;
        let limit = flags
            .take_value("--limit")?
            .map(|v| parse_num::<u64>("--limit", &v))
            .transpose()?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let core = Core::start(CoreConfig::default())?;
        let session = core.open_catalog(&catalog, None)?;
        let rows = collect_rows(&session, folder, limit)?;

        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for s in &rows {
                let badges = [
                    (s.missing, "missing"),
                    (s.decode_error, "decode-error"),
                    (s.width == 0 && s.height == 0, "no-dims"),
                ]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, b)| format!(" [{b}]"))
                .collect::<String>();
                println!(
                    "{:>6}  {}  {:>1}  {:>9}  {:<20}  {}{badges}",
                    s.id.0,
                    s.rating.map_or("-".into(), |r| r.to_string()),
                    match s.flag {
                        lightbox_types::Flag::None => "·",
                        lightbox_types::Flag::Pick => "P",
                        lightbox_types::Flag::Reject => "X",
                    },
                    format!("{}x{}", s.width, s.height),
                    s.capture_time.as_deref().unwrap_or("-"),
                    s.filename,
                );
            }
            eprintln!("{} image(s)", rows.len());
        }
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

/// Pages through `images_page` by keyset cursor (never OFFSET — spec §3.2)
/// until `limit` rows are collected (or the catalog is exhausted).
fn collect_rows(
    session: &Session,
    folder: Option<FolderId>,
    limit: Option<u64>,
) -> anyhow::Result<Vec<ImageSummary>> {
    let mut rows: Vec<ImageSummary> = Vec::new();
    let mut cursor = None;
    loop {
        let want = match limit {
            // ImageQuery clamps to 1..=1000 per page.
            Some(n) => match n.saturating_sub(rows.len() as u64) {
                0 => break,
                left => left.min(1000) as u32,
            },
            None => 1000,
        };
        let page = session.query().images_page(&ImageQuery {
            folder,
            sort: SortOrder::FilenameAsc,
            cursor,
            limit: want,
        })?;
        rows.extend(page.items);
        match page.next {
            Some(next) if limit.is_none_or(|n| (rows.len() as u64) < n) => cursor = Some(next),
            _ => break,
        }
    }
    if let Some(n) = limit {
        rows.truncate(n.min(usize::MAX as u64) as usize);
    }
    Ok(rows)
}

fn cmd_render(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = flags
            .take_value("--image")?
            .map(|v| parse_num::<i64>("--image", &v))
            .transpose()?
            .ok_or_else(|| UsageError("render requires --image <id>".to_owned()))?;
        let out = flags
            .take_value("--out")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("render requires --out <out.png>".to_owned()))?;
        let width = flags
            .take_value("--width")?
            .map(|v| parse_num::<u32>("--width", &v))
            .transpose()?;
        let cpu = flags.take_switch("--cpu");
        flags.finish()?;
        if width == Some(0) {
            return Err(UsageError("--width must be >= 1".to_owned()).into());
        }

        // GPU when an adapter exists; --cpu forces the CPU node path
        // (spec §3.10). Headless GpuContext degrades to None gracefully.
        let gpu = if cpu { None } else { GpuContext::headless() };
        let core = Core::start(CoreConfig::default())?;
        let session = core.open_catalog(&catalog, gpu)?;

        let image = ImageId(image);
        // A clear "no such image" beats a source error out of the engine.
        session
            .query()
            .image_detail(image)
            .with_context(|| format!("image {} not found in the catalog", image.0))?;

        // The SAME Engine::submit/poll path as the shell's loupe (T27).
        let engine = session.engine();
        let ticket = engine.submit(RenderRequest {
            image,
            recipe: Recipe::identity(PV_M0),
            pv: PV_M0,
            roi: Roi::Full,
            scale: match width {
                Some(w) => RenderScale::FitWithin { w, h: w },
                None => RenderScale::Native,
            },
            target: RenderTarget::CpuBuffer,
            viewport: ViewportId(1),
        });
        let deadline = Instant::now() + Duration::from_secs(300);
        let buf = loop {
            match engine.poll(&ticket) {
                RenderState::Pending | RenderState::Running => {
                    if Instant::now() > deadline {
                        bail!("render timed out");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                RenderState::Ready(RenderOutput::Cpu(buf)) => break buf,
                RenderState::Ready(RenderOutput::Texture { .. }) => {
                    bail!("engine returned a texture for a CpuBuffer request (bug)")
                }
                RenderState::Failed(err) => bail!("render failed: {err}"),
                RenderState::Cancelled | RenderState::Superseded => {
                    bail!("render did not complete (cancelled/superseded)")
                }
            }
        };

        let (w, h) = (buf.width, buf.height);
        lbx_image_compare::Rgba8Image::new(w, h, buf.px)
            .map_err(|e| anyhow!("render output malformed: {e}"))?
            .write_png(&out)
            .map_err(|e| anyhow!("writing {}: {e}", out.display()))?;
        let backend = match engine.backend_kind() {
            BackendKind::Gpu(b) => format!("gpu ({b:?})"),
            BackendKind::CpuOnly => "cpu".to_owned(),
        };
        println!("wrote {} ({w}x{h}, {backend})", out.display());
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

fn cmd_backup(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        flags.finish()?;

        let core = Core::start(CoreConfig::default())?;
        let session = core.open_catalog(&catalog, None)?;
        let mut rx = session.events();
        let ticket = session.submit(Command::BackupNow);
        let deadline = Instant::now() + Duration::from_secs(600);
        let report = loop {
            match rx.try_recv() {
                Ok(Event::BackupFinished { report }) => break report,
                Ok(Event::CommandFailed {
                    ticket: failed,
                    error,
                }) if failed == ticket => bail!("backup failed: {error}"),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        bail!("backup timed out");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    bail!("event stream closed before the backup finished")
                }
            }
        };
        println!(
            "backup {} ({} bytes, took {:.2}s)",
            report.path.display(),
            report.bytes,
            report.took.as_secs_f64(),
        );
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

fn cmd_check(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        flags.finish()?;
        check_catalog_at(&catalog)
    })
}

/// `check` body: 0 = clean, [`EXIT_CORRUPT`] = corrupt/refused, `Err` = other
/// failures (missing catalog, schema-too-new, IO).
fn check_catalog_at(catalog: &Path) -> anyhow::Result<u8> {
    match check_catalog(catalog) {
        Ok(check) => match &check.integrity {
            IntegrityStatus::Ok => {
                println!("ok: schema version {}", check.schema_version);
                Ok(0)
            }
            IntegrityStatus::Corrupt(findings) => {
                eprintln!("catalog is corrupt:");
                for f in findings {
                    eprintln!("  {f}");
                }
                Ok(EXIT_CORRUPT)
            }
        },
        Err(CoreError::Catalog(err @ CatalogError::Corrupt { .. })) => {
            // The message names the newest verified backup (spec §1.1).
            eprintln!("catalog is corrupt: {err}");
            Ok(EXIT_CORRUPT)
        }
        Err(other) => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_take_value_switch_and_reject_leftovers() {
        let args: Vec<String> = ["--catalog", "x.lbdata", "--json", "--limit", "5"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut flags = Flags::new(&args);
        assert_eq!(
            flags.take_value("--catalog").unwrap().as_deref(),
            Some("x.lbdata")
        );
        assert!(flags.take_switch("--json"));
        assert!(!flags.take_switch("--json"), "switch consumed once");
        assert_eq!(flags.take_value("--limit").unwrap().as_deref(), Some("5"));
        flags.finish().unwrap();

        let args: Vec<String> = vec!["--stray".to_owned()];
        assert!(Flags::new(&args).finish().is_err());
    }

    #[test]
    fn flags_value_requires_an_argument() {
        let args: Vec<String> = vec!["--catalog".to_owned()];
        let mut flags = Flags::new(&args);
        assert!(flags.take_value("--catalog").is_err());

        // A following flag is not a value.
        let args: Vec<String> = vec!["--catalog".to_owned(), "--json".to_owned()];
        let mut flags = Flags::new(&args);
        assert!(flags.take_value("--catalog").is_err());
    }

    #[test]
    fn unknown_subcommand_is_a_usage_error() {
        let args: Vec<String> = vec!["frobnicate".to_owned()];
        assert_eq!(run(&args).unwrap(), EXIT_USAGE);
        assert_eq!(run(&[]).unwrap(), EXIT_USAGE);
    }
}
