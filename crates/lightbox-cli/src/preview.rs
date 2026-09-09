// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E03 Phase D (T14) + Phase F (T21/T23) headless driver: `preview
//! build/stat/verify/purge/relocate` over the `Command::BuildPreviews`/
//! `Queries::cache_stats`/`PreviewService` seam, the exact scenario the
//! T14 acceptance criterion names ("import fixture → `preview build --tier
//! 1` → `Ready` events → `stat` reports rows/bytes matching disk"), run
//! headlessly with zero shell dependency (mirrors `edit.rs`'s existing
//! pattern for E09's CLI surface). `verify --full`/`purge --scope`/
//! `relocate` close the §11 DoD's "operability" bar for T21's fuller
//! `verify_store(Full)`/`PurgeScope`/journaled-relocate surface, Phase D's
//! `verify_quick`/`purge_all` (the narrower, pre-Phase-F primitives, D-8)
//! stay the default for backward compatibility with every existing script.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::bail;
use lightbox_core::{
    BuildPriority, CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageQuery,
    ProgressSink, PurgeScope, Session, SortOrder, Tier, VerifyMode,
};
use lightbox_types::ImageId;

use crate::edit::bad_subcommand;
use crate::{parse_num, required_catalog, with_usage, Flags, UsageError};

fn open_session(catalog: &std::path::Path) -> anyhow::Result<(Core, Session)> {
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(catalog, None)?;
    Ok((core, session))
}

fn close_quiet(session: Session) -> anyhow::Result<()> {
    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok(())
}

fn parse_tier(s: &str) -> Result<Tier, UsageError> {
    match s {
        "0" => Ok(Tier::T0),
        "1" => Ok(Tier::T1),
        "2" => Ok(Tier::T2),
        other => Err(UsageError(format!(
            "--tier expects 0, 1, or 2, got {other:?}"
        ))),
    }
}

fn parse_priority(s: &str) -> Result<BuildPriority, UsageError> {
    match s {
        "visible" => Ok(BuildPriority::Visible),
        "neighbor" => Ok(BuildPriority::Neighbor),
        "bulk" => Ok(BuildPriority::Bulk),
        other => Err(UsageError(format!(
            "--priority expects visible, neighbor, or bulk, got {other:?}"
        ))),
    }
}

pub(crate) fn cmd_preview(args: &[String]) -> anyhow::Result<u8> {
    match args.first().map(String::as_str) {
        Some("build") => cmd_preview_build(&args[1..]),
        Some("stat") => cmd_preview_stat(&args[1..]),
        Some("verify") => cmd_preview_verify(&args[1..]),
        Some("purge") => cmd_preview_purge(&args[1..]),
        Some("relocate") => cmd_preview_relocate(&args[1..]),
        _ => bad_subcommand("preview", "build|stat|verify|purge|relocate"),
    }
}

/// Every image in the catalog, filename order (mirrors `main.rs`'s
/// `collect_rows`, kept local since this module has no reason to share it
/// across a public boundary).
fn all_images(session: &Session) -> anyhow::Result<Vec<ImageId>> {
    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        let page = session.query().images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::FilenameAsc,
            cursor,
            limit: 1000,
        })?;
        ids.extend(page.items.iter().map(|s| s.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(ids)
}

/// `preview build --catalog <dir> --tier <0|1|2> [--image <id> ...]
/// [--priority visible|neighbor|bulk]` (T14 AC: `preview build --tier 1`
/// alone builds every image in the catalog).
fn cmd_preview_build(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let tier = flags
            .take_value("--tier")?
            .map(|v| parse_tier(&v))
            .transpose()?
            .ok_or_else(|| UsageError("preview build requires --tier <0|1|2>".to_owned()))?;
        let priority = flags
            .take_value("--priority")?
            .map(|v| parse_priority(&v))
            .transpose()?
            // Bulk by default: "build previews for [the whole catalog /
            // selection]" is T16's bulk-build shape, which is what an
            // unqualified `--image`-less invocation means.
            .unwrap_or(BuildPriority::Bulk);
        let mut images = Vec::new();
        while let Some(v) = flags.take_value("--image")? {
            images.push(ImageId(parse_num::<i64>("--image", &v)?));
        }
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        let images = if images.is_empty() {
            all_images(&session)?
        } else {
            images
        };
        if images.is_empty() {
            println!("nothing to build (catalog is empty)");
            close_quiet(session)?;
            return Ok(0);
        }

        let mut rx = session.events();
        session.submit(Command::BuildPreviews {
            images: images.clone(),
            tier,
            priority,
        });

        let mut ready = 0u64;
        let mut failed = 0u64;
        let total = images.len() as u64;
        let mut last_print = Instant::now() - Duration::from_secs(1);
        let deadline = Instant::now() + Duration::from_secs(600);
        while ready + failed < total {
            if Instant::now() > deadline {
                bail!("preview build timed out ({ready} ready, {failed} failed of {total})");
            }
            match rx.try_recv() {
                Ok(Event::PreviewReady { .. }) => ready += 1,
                Ok(Event::PreviewFailed { image, error, .. }) => {
                    failed += 1;
                    eprintln!("preview build failed for image {}: {error}", image.0);
                }
                Ok(Event::PreviewBulkProgress { done, total }) => {
                    if last_print.elapsed() >= Duration::from_millis(250) {
                        eprintln!("building previews {done}/{total}");
                        last_print = Instant::now();
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    bail!("event stream closed before the preview build finished")
                }
            }
        }
        println!(
            "built previews for {total} image(s) at tier {} — {ready} ready, {failed} failed",
            tier_num(tier)
        );
        close_quiet(session)?;
        Ok(0)
    })
}

/// `preview stat --catalog <dir> [--json]`.
fn cmd_preview_stat(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        let stats = session.query().cache_stats()?;
        if json {
            println!(
                "{{\"t0_count\":{},\"t0_bytes\":{},\"t1_count\":{},\"t1_bytes\":{},\"t2_count\":{},\"t2_bytes\":{}}}",
                stats.t0_count, stats.t0_bytes, stats.t1_count, stats.t1_bytes, stats.t2_count, stats.t2_bytes
            );
        } else {
            println!(
                "T0: {} rows, {} bytes\nT1: {} rows, {} bytes\nT2: {} rows, {} bytes\ntotal: {} rows, {} bytes",
                stats.t0_count,
                stats.t0_bytes,
                stats.t1_count,
                stats.t1_bytes,
                stats.t2_count,
                stats.t2_bytes,
                stats.total_count(),
                stats.total_bytes(),
            );
        }
        close_quiet(session)?;
        Ok(0)
    })
}

/// `preview verify --catalog <dir> [--full] [--json]`, default (no
/// `--full`) is missing-file detection only (Phase D's `verify_quick`).
/// `--full` runs Phase F's `verify_store(VerifyMode::Full)` (T21): the same
/// missing-file check PLUS a checksum spot-check on T0/T1 rows (drops
/// torn files), an orphan sweep, and an orphaned-`.tmp-*` sweep, see
/// `lightbox_preview::verify`'s module doc comment. Exit 0 when the store is
/// clean, exit 1 (not the corrupt/refused exit 3, the preview store is
/// disposable by contract, spec §3.2) when `--full` found/fixed anything or
/// the default mode found a missing file.
fn cmd_preview_verify(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let full = flags.take_switch("--full");
        let json = flags.take_switch("--json");
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        if full {
            let report = session.preview_service().verify_store(VerifyMode::Full);
            let dirty = report.missing_files > 0
                || report.checksum_failures > 0
                || report.orphans_removed > 0
                || report.orphan_temp_files_removed > 0;
            if json {
                println!(
                    "{{\"rows_checked\":{},\"missing_files\":{},\"checksum_failures\":{},\
                     \"orphans_removed\":{},\"orphan_temp_files_removed\":{}}}",
                    report.rows_checked,
                    report.missing_files,
                    report.checksum_failures,
                    report.orphans_removed,
                    report.orphan_temp_files_removed
                );
            } else {
                println!(
                    "checked {} row(s); {} missing, {} torn (checksum), \
                     {} orphan(s) swept, {} orphaned temp file(s) swept",
                    report.rows_checked,
                    report.missing_files,
                    report.checksum_failures,
                    report.orphans_removed,
                    report.orphan_temp_files_removed
                );
            }
            close_quiet(session)?;
            return Ok(if dirty { 1 } else { 0 });
        }

        let report = session.preview_service().verify_quick()?;
        let missing = report.missing_files.len();
        if json {
            println!(
                "{{\"rows_checked\":{},\"missing_files\":{}}}",
                report.rows_checked, missing
            );
        } else {
            println!(
                "checked {} row(s); {} missing file(s)",
                report.rows_checked, missing
            );
            for path in &report.missing_files {
                eprintln!("  missing: {}", path.display());
            }
        }
        close_quiet(session)?;
        Ok(if missing == 0 { 0 } else { 1 })
    })
}

/// `preview purge --catalog <dir> --yes [--scope previews|rawcache|all]`
/// default scope (`previews`, unspecified `--scope`) matches the pre-Phase-F
/// behavior exactly (`PreviewService::purge_all`, D-8) for backward
/// compatibility with any existing script; `--scope rawcache`/`--scope all`
/// reach Phase F's fuller `PurgeService::purge(PurgeScope)` (T21). `--yes`
/// is required (destructive, no default).
fn cmd_preview_purge(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let confirmed = flags.take_switch("--yes");
        let scope = flags
            .take_value("--scope")?
            .map(|v| parse_purge_scope(&v))
            .transpose()?
            .unwrap_or(PurgeScope::Previews);
        flags.finish()?;
        if !confirmed {
            return Err(UsageError(
                "preview purge is destructive — pass --yes to confirm".to_owned(),
            )
            .into());
        }

        let (_core, session) = open_session(&catalog)?;
        let report = session.preview_service().purge(scope)?;
        println!(
            "purged {} preview row(s), {} raw-cache row(s)",
            report.rows_deleted, report.rawcache_rows_deleted
        );
        close_quiet(session)?;
        Ok(0)
    })
}

fn parse_purge_scope(s: &str) -> Result<PurgeScope, UsageError> {
    match s {
        "previews" => Ok(PurgeScope::Previews),
        "rawcache" => Ok(PurgeScope::RawCache),
        "all" => Ok(PurgeScope::All),
        other => Err(UsageError(format!(
            "--scope expects previews, rawcache, or all, got {other:?}"
        ))),
    }
}

/// `preview relocate --catalog <dir> --new-root <path>`, journaled
/// relocation of the E03-owned cache surfaces (spec §5.2/§3.2, T21; see
/// `lightbox_preview::relocate`'s module doc comment for exact scope and the
/// resumability contract). Synchronous here (a CLI invocation has nothing
/// better to do while it waits), `lightbox-core`'s `Command::
/// RelocateCacheStore` is the async, event-driven equivalent for the shell.
/// Prints progress as it copies; **does not** reopen the session against
/// the new root afterward (the caller does that on the next invocation
/// same posture as `PreviewService::relocate`'s own doc comment).
fn cmd_preview_relocate(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let new_root = flags
            .take_value("--new-root")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("preview relocate requires --new-root <path>".to_owned()))?;
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        // `ProgressSink` is `Arc<dyn Fn(..) + Send + Sync>` (not `FnMut`)
        // a `Mutex` gives the throttling state interior mutability.
        let last_print = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
        let progress: ProgressSink = std::sync::Arc::new(move |p| {
            let mut last = last_print
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.elapsed() >= Duration::from_millis(250) {
                eprintln!("relocating {}/{} files", p.done_files, p.total_files);
                *last = Instant::now();
            }
        });
        session.preview_service().relocate(&new_root, progress)?;
        println!("relocated cache store to {}", new_root.display());
        close_quiet(session)?;
        Ok(0)
    })
}

fn tier_num(t: Tier) -> u8 {
    match t {
        Tier::T0 => 0,
        Tier::T1 => 1,
        Tier::T2 => 2,
    }
}
