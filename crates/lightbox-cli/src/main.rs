// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-cli`, headless driver over `lightbox-core` (E01 spec §3.10,
//! T27).
//!
//! ```text
//! lightbox-cli create --catalog <dir>.lbdata
//! lightbox-cli import --catalog <dir> --add <src-dir> [--recursive]
//! lightbox-cli list   --catalog <dir> [--folder <id>] [--limit <n>] [--json]
//! lightbox-cli render --catalog <dir> --image <id> --out <out.png> [--width <n>] [--cpu]
//! lightbox-cli backup --catalog <dir>
//! lightbox-cli check  --catalog <dir>
//! lightbox-cli look-dev --look <file.lblook> --out <dir> [--amount <f>]
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
//! `3` catalog corrupt/refused (`check` distinguishes ok/corrupt, T27 AC).
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
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    ActiveBackend, Extent, OutFormat, OutputPayload, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, Roi,
};
use lightbox_render::GpuContext;
use lightbox_types::{FolderId, ImageId};

// E02 Phase H (H2): the decode → color → look reference subcommands.
mod e02;
// E09 Phase B follow-up (T11): `edit`/`history`/`step-to`/`undo`/`redo`/
// `snapshot`/`preset`/`xmp` subcommands over the `Command::Edit`/`EditHub`
// seam.
mod edit;
// E15 core slice: `export`, single-image (`Session::engine()` direct) and
// batch (`Command::Export`/`Event::Export*`) forms.
mod export;
// E04 (spec §4.6, mandate v2.2 addendum): `open`, the headless
// Command::OpenWorkingSet driver; `browse`, the headless folder-explorer
// harness.
mod open;
// E03 Phase D (T14): `preview build/stat/verify/purge` over the
// `Command::BuildPreviews`/`Queries::cache_stats`/`PreviewService` seam.
mod preview;
// E02 Phase H (H6/H7): the seam-handoff contract + threat-model notes, as
// rustdoc committed alongside the crates (no separate report .md).
mod seams;

/// Exit code for usage errors (bad flags, missing arguments).
pub(crate) const EXIT_USAGE: u8 = 2;
/// Exit code when the catalog is corrupt or refused (`check`, T27 AC).
const EXIT_CORRUPT: u8 = 3;

const USAGE: &str = "\
lightbox-cli — headless Lightbox driver (E01 spec §3.10 + E02 spec §1.1)

USAGE:
  lightbox-cli create --catalog <dir>.lbdata
  lightbox-cli list   --catalog <dir> [--folder <id>] [--limit <n>] [--json]
  lightbox-cli render --catalog <dir> --image <id> --out <out.png> [--width <n>] [--cpu]
  lightbox-cli backup --catalog <dir>
  lightbox-cli check  --catalog <dir>
  lightbox-cli look-dev --look <file.lblook> --out <dir> [--amount <f>]

  E04 working-set loader / folder explorer (headless — the v2.0 entry path):
  lightbox-cli open   <PATH>... [--recursive] [--store <dir>.lbdata] [--json]
  lightbox-cli browse <DIR> [--json]

  E01 managed-import harness (legacy, testing only — retired-dormant as of
  E04; the shell never reaches this path, `open` above is the v2.0 entry):
  lightbox-cli import --catalog <dir> --add <src-dir> [--recursive]

  E02 decode → color → look reference path (headless):
  lightbox-cli probe      --file <path> [--json]
  lightbox-cli decode     --file <path> [--json]
  lightbox-cli render-ref --file <path> --out <out.png> [--look <file.lblook>] [--amount <f>]
  lightbox-cli profile inspect --file <path.dcp|.lblook> [--json]
  lightbox-cli profile install --catalog <dir> --file <path.dcp|.lblook>
  lightbox-cli profile list    --catalog <dir> [--json]
  lightbox-cli look inspect    --file <path.lblook> [--json]

  E09 edit state / history / presets / XMP (headless):
  lightbox-cli edit set   --catalog <dir> --image <id> <param>=<value> [<param>=<value> ...]
  lightbox-cli edit get   --catalog <dir> --image <id> [--json]
  lightbox-cli history    --catalog <dir> --image <id> [--json]
  lightbox-cli step-to    --catalog <dir> --image <id> --seq <n>
  lightbox-cli undo       --catalog <dir> --image <id>
  lightbox-cli redo       --catalog <dir> --image <id>
  lightbox-cli clear-history --catalog <dir> --image <id>
  lightbox-cli snapshot create  --catalog <dir> --image <id> --name <name>
  lightbox-cli snapshot restore --catalog <dir> --image <id> --id <snapshot-id>
  lightbox-cli snapshot list    --catalog <dir> --image <id> [--json]
  lightbox-cli snapshot delete  --catalog <dir> --image <id> --id <snapshot-id>
  lightbox-cli snapshot rename  --catalog <dir> --image <id> --id <snapshot-id> --name <name>
  lightbox-cli preset create --catalog <dir> --image <id> --name <name> [--group <g>] [--groups <g1,g2,...>]
  lightbox-cli preset list   --catalog <dir> [--json]
  lightbox-cli preset apply  --catalog <dir> --image <id> --id <preset-id>
  lightbox-cli preset import --catalog <dir> <file.xmp> [<file2.xmp> ...]
  lightbox-cli preset export --catalog <dir> --id <preset-id> --out <path>
  lightbox-cli preset delete --catalog <dir> --id <preset-id>
  lightbox-cli preset rename --catalog <dir> --id <preset-id> --name <name>
  lightbox-cli xmp write  --catalog <dir> --image <id>
  lightbox-cli xmp read   --catalog <dir> --image <id>
  lightbox-cli xmp status --catalog <dir> --image <id> [--json]

  E03 preview pyramid (headless):
  lightbox-cli preview build    --catalog <dir> --tier <0|1|2> [--image <id> ...] [--priority visible|neighbor|bulk]
  lightbox-cli preview stat     --catalog <dir> [--json]
  lightbox-cli preview verify   --catalog <dir> [--full] [--json]
  lightbox-cli preview purge    --catalog <dir> --yes [--scope previews|rawcache|all]
  lightbox-cli preview relocate --catalog <dir> --new-root <path>

  E15 export (headless, core slice):
  lightbox-cli export --catalog <dir> --image <id> --out <file> --format jpeg|png|tiff \\
      [--quality 1..=100] [--depth 8|16] [--long-edge N] \\
      [--color srgb|adobe-rgb|display-p3] [--sharpen low|standard|high] [--cpu]
  lightbox-cli export --catalog <dir> --images <id>[,<id>...] --out-dir <dir> --format jpeg|png|tiff \\
      [--quality 1..=100] [--depth 8|16] [--long-edge N] \\
      [--color srgb|adobe-rgb|display-p3] [--sharpen low|standard|high] [--suffix <text>] [--cpu]

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
        // E04 (spec §4.6): the v2.0 entry path.
        "open" => open::cmd_open(rest),
        "browse" => open::cmd_browse(rest),
        // (legacy, testing only), retired-dormant as of E04, see the
        // module doc comment on `Command::ImportAddInPlace`.
        "import" => cmd_import(rest),
        "list" => cmd_list(rest),
        "render" => cmd_render(rest),
        "backup" => cmd_backup(rest),
        "check" => cmd_check(rest),
        "look-dev" => cmd_look_dev(rest),
        // E02 Phase H (H2): the decode → color → look reference path.
        "probe" => e02::cmd_probe(rest),
        "decode" => e02::cmd_decode(rest),
        "render-ref" => e02::cmd_render_ref(rest),
        "profile" => e02::cmd_profile(rest),
        "look" => e02::cmd_look(rest),
        // E09 Phase B follow-up (T11): edit state / history / presets / XMP.
        "edit" => edit::cmd_edit(rest),
        "history" => edit::cmd_history(rest),
        "step-to" => edit::cmd_step_to(rest),
        "undo" => edit::cmd_undo(rest),
        "redo" => edit::cmd_redo(rest),
        "clear-history" => edit::cmd_clear_history(rest),
        "snapshot" => edit::cmd_snapshot(rest),
        "preset" => edit::cmd_preset(rest),
        "xmp" => edit::cmd_xmp(rest),
        // E03 Phase D (T14): preview build/stat/verify/purge.
        "preview" => preview::cmd_preview(rest),
        // E15 core slice.
        "export" => export::cmd_export(rest),
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
pub(crate) struct Flags {
    args: Vec<Option<String>>,
}

/// Marker for usage errors so `run`'s caller maps them to exit code 2.
#[derive(Debug)]
pub(crate) struct UsageError(pub(crate) String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{USAGE}", self.0)
    }
}

impl std::error::Error for UsageError {}

impl Flags {
    pub(crate) fn new(args: &[String]) -> Flags {
        Flags {
            args: args.iter().cloned().map(Some).collect(),
        }
    }

    /// Consumes `--name <value>`; `None` when the flag is absent.
    pub(crate) fn take_value(&mut self, name: &str) -> Result<Option<String>, UsageError> {
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
    pub(crate) fn take_switch(&mut self, name: &str) -> bool {
        for slot in &mut self.args {
            if slot.as_deref() == Some(name) {
                *slot = None;
                return true;
            }
        }
        false
    }

    /// Consumes every remaining bare (non-`--flag`) argument, in order (E09
    /// T11: `edit set exposure=+1.0 contrast=5.0`, `preset import a.xmp
    /// b.xmp`). Leaves flags untouched for subsequent `take_value`/
    /// `take_switch` calls.
    pub(crate) fn take_positionals(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for slot in &mut self.args {
            if let Some(s) = slot {
                if !s.starts_with("--") {
                    out.push(s.clone());
                    *slot = None;
                }
            }
        }
        out
    }

    /// Errors on any argument no subcommand consumed.
    pub(crate) fn finish(self) -> Result<(), UsageError> {
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
pub(crate) fn required_catalog(flags: &mut Flags) -> Result<PathBuf, UsageError> {
    flags
        .take_value("--catalog")?
        .map(PathBuf::from)
        .ok_or_else(|| UsageError("--catalog <dir>.lbdata is required".to_owned()))
}

pub(crate) fn parse_num<T: std::str::FromStr>(name: &str, v: &str) -> Result<T, UsageError> {
    v.parse()
        .map_err(|_| UsageError(format!("{name} expects a number, got {v:?}")))
}

/// Runs a subcommand body, mapping [`UsageError`] to exit code 2.
pub(crate) fn with_usage(f: impl FnOnce() -> anyhow::Result<u8>) -> anyhow::Result<u8> {
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

/// Pages through `images_page` by keyset cursor (never OFFSET, spec §3.2)
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
        // A clear "no such image" beats a source error out of the engine
        // and its width/height double as the render's real "full extent"
        // (see the roi/scale note just below).
        let detail = session
            .query()
            .image_detail(image)
            .with_context(|| format!("image {} not found in the catalog", image.0))?;

        // The SAME `ng::Engine::submit`/`poll` path as the shell's loupe/canvas
        // (E05 Phase F5, replaces the E01 seed's `Engine::submit`).
        //
        // F5 deviation (see `docs/plan/epics/E05-deviations.md`): the M1
        // executor wired to `Engine::submit` is Phase A/B/E's untiled base
        // walk, not Phase C's tiling/scale executor, Phase C's
        // `ng::exec::scale::derive` is built and gated in `lightbox-render`'s
        // own test suite but not yet threaded into the live per-render path.
        // On the CPU backend the **request `roi`, not `RenderScale`**, is
        // what actually sizes every node's output tile at M1
        // (`exec/cpu/mod.rs` allocates `Extent{w: req.roi.w, h: req.roi.h}`
        // unconditionally). So `roi` is computed here from the catalog's
        // known dimensions via the *same* fit-ratio math Phase C ships
        // (`ng::exec::scale::derive`), not a placeholder, to get a
        // correctly-sized, bounded render; `scale` is still passed through
        // correctly for when the live path picks up Phase C's decimation.
        // Leaving `roi` at an arbitrary/oversized placeholder here previously
        // asked the CPU backend to allocate one absurd (unbounded) tile per
        // node, a real resource-exhaustion bug, not just a cosmetic gap.
        let full = lightbox_render::ng::Extent {
            w: detail.width.max(1),
            h: detail.height.max(1),
        };
        let scale = match width {
            Some(w) => RenderScale::Fit(Extent { w, h: w }),
            None => RenderScale::OneToOne,
        };
        let resolution = lightbox_render::ng::exec::scale::derive(scale, full);

        // The image's durable-or-neutral edit recipe (E09 `Queries::edit_state`
        // a pure read, no `EditHub::open` needed), so `render` actually
        // reflects whatever `edit set` wrote (E10: exposure/contrast/whites/
        // blacks and friends), not always the identity recipe.
        let recipe = session.query().edit_state(image)?.recipe;
        let pv = recipe.pv;

        let engine = session.engine();
        let ticket = engine.submit(RenderRequest {
            image,
            recipe,
            pv,
            roi: Roi {
                x: 0,
                y: 0,
                w: resolution.extent.w,
                h: resolution.extent.h,
            },
            scale,
            target: RenderTarget::Buffer {
                format: OutFormat::Rgba8Srgb,
            },
            priority: RenderPriority::Batch,
            cancel: CancelToken::new(),
        });
        let deadline = Instant::now() + Duration::from_secs(300);
        let buf = loop {
            match engine.poll(&ticket) {
                RenderState::Queued | RenderState::Rendering { .. } => {
                    if Instant::now() > deadline {
                        bail!("render timed out");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                RenderState::Complete(out) | RenderState::PreviewReady(out) => match out.payload {
                    OutputPayload::Pixels(px) => break px,
                    OutputPayload::CanvasGeneration(_) => {
                        bail!("engine returned a canvas generation for a Buffer request (bug)")
                    }
                },
                RenderState::Failed(err) => bail!("render failed: {err}"),
                RenderState::Cancelled => bail!("render did not complete (cancelled)"),
            }
        };

        let (w, h) = (buf.extent.w, buf.extent.h);
        lbx_image_compare::Rgba8Image::new(w, h, buf.bytes)
            .map_err(|e| anyhow!("render output malformed: {e}"))?
            .write_png(&out)
            .map_err(|e| anyhow!("writing {}: {e}", out.display()))?;
        let backend = match engine.active_backend() {
            ActiveBackend::Gpu(info) => format!("gpu ({:?})", info.backend),
            ActiveBackend::CpuPreviewOnly => "cpu".to_owned(),
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

/// `look-dev` (E02 task E3): the Lightbox default-look authoring harness. Loads
/// a `.lblook`, renders the built-in synthetic scene corpus base-vs-look, and
/// writes a self-contained contact sheet (`<out>/index.html` + `<out>/tiles/`).
///
/// This is a headless authoring/dev tool, not a shipped runtime path.
fn cmd_look_dev(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let look_path = flags
            .take_value("--look")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("look-dev requires --look <file.lblook>".to_owned()))?;
        let out = flags
            .take_value("--out")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("look-dev requires --out <dir>".to_owned()))?;
        let amount = flags
            .take_value("--amount")?
            .map(|v| parse_num::<f32>("--amount", &v))
            .transpose()?
            .unwrap_or(1.0);
        flags.finish()?;

        let bytes = std::fs::read(&look_path)
            .with_context(|| format!("reading {}", look_path.display()))?;
        let look = lightbox_color::look::load_look(&bytes)
            .map_err(|e| anyhow!("{}: {e}", look_path.display()))?;

        let tiles_dir = out.join("tiles");
        std::fs::create_dir_all(&tiles_dir)
            .with_context(|| format!("creating {}", tiles_dir.display()))?;

        let cells = lightbox_color::look::render_contact_sheet(&look, amount);
        let mut body = String::new();
        let mut last_cat = String::new();
        for cell in &cells {
            if cell.category != last_cat {
                body.push_str(&format!(
                    "<tr class=\"cat\"><td colspan=\"3\">{}</td></tr>\n",
                    html_escape(&cell.category)
                ));
                last_cat = cell.category.clone();
            }
            write_tile_png(
                &tiles_dir,
                &format!("{}-base", cell.name),
                cell.width,
                cell.height,
                &cell.base_rgba8,
            )?;
            write_tile_png(
                &tiles_dir,
                &format!("{}-look", cell.name),
                cell.width,
                cell.height,
                &cell.look_rgba8,
            )?;
            body.push_str(&format!(
                "<tr><td class=\"name\">{name}</td>\
                 <td><img src=\"tiles/{name}-base.png\" alt=\"base\"></td>\
                 <td><img src=\"tiles/{name}-look.png\" alt=\"look\"></td></tr>\n",
                name = html_escape(&cell.name),
            ));
        }

        let html = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>look-dev: {look}</title>\
             <style>body{{font:13px system-ui;margin:24px}}\
             table{{border-collapse:collapse}}td{{padding:4px 8px;vertical-align:middle}}\
             img{{width:160px;height:100px;image-rendering:pixelated;border:1px solid #ccc}}\
             .cat td{{font-weight:600;padding-top:16px;text-transform:capitalize}}\
             th{{text-align:left;padding:4px 8px}}.name{{font-family:monospace}}</style>\
             <h1>look-dev — {look} @ amount {amount}</h1>\
             <p>{n} synthetic scenes · left = base (no look) · right = look applied.</p>\
             <table><tr><th>scene</th><th>base</th><th>look</th></tr>{body}</table>",
            look = html_escape(&look.name),
            n = cells.len(),
        );
        let index = out.join("index.html");
        std::fs::write(&index, html).with_context(|| format!("writing {}", index.display()))?;
        println!(
            "wrote contact sheet {} ({} scenes, look {:?} @ {amount})",
            index.display(),
            cells.len(),
            look.name,
        );
        Ok(0)
    })
}

/// Writes one RGBA8 tile as a PNG under `dir/<name>.png`.
fn write_tile_png(dir: &Path, name: &str, w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<()> {
    lbx_image_compare::Rgba8Image::new(w, h, rgba.to_vec())
        .map_err(|e| anyhow!("tile {name}: {e}"))?
        .write_png(&dir.join(format!("{name}.png")))
        .map_err(|e| anyhow!("writing tile {name}: {e}"))
}

/// Minimal HTML-attribute/text escaping for the contact sheet.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
