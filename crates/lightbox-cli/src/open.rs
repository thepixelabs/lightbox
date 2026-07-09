// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E04 spec §4.6: `lightbox-cli open` — the headless driver over
//! `Command::OpenWorkingSet`/`Event::WorkingSet*`/`Session::working_set()`,
//! the M1 smoke path before E08's shell exists. Also `lightbox-cli browse`
//! (mandate v2.2 addendum): the headless folder-explorer harness — pure
//! filesystem reads, no catalog, no session.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::bail;
use lightbox_core::{
    browse_dir, default_store_dir, ClosePolicy, Command, Core, CoreConfig, Event, ItemState,
    OpenOrigin, OpenRequest, Session,
};
use lightbox_types::SourceKind;

use crate::{with_usage, Flags, UsageError};

/// `lightbox-cli open <PATH>... [--recursive] [--store <dir>.lbdata]
/// [--json]` (spec §4.6).
///
/// Exit codes: `0` every item `Ready`/`DuplicateOf`, `1` any item `Failed`,
/// `2` usage, `3` store corrupt/refused (handled centrally by `main`'s
/// `is_corrupt_error`, same as every other subcommand).
pub(crate) fn cmd_open(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let recursive = flags.take_switch("--recursive");
        let json = flags.take_switch("--json");
        let store = flags
            .take_value("--store")?
            .map(PathBuf::from)
            .unwrap_or_else(default_store_dir);
        let paths: Vec<PathBuf> = flags
            .take_positionals()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        flags.finish()?;
        if paths.is_empty() {
            return Err(UsageError("open requires at least one <PATH>".to_owned()).into());
        }

        // Opens (or creates) the store (spec §4.6).
        if let Some(parent) = store.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let core = Core::start(CoreConfig::default())?;
        let session = if store.join("catalog.sqlite").is_file() {
            core.open_catalog(&store, None)?
        } else {
            core.create_catalog(&store, None)?
        };

        let mut rx = session.events();
        let ticket = session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(paths, recursive, OpenOrigin::Cli),
        });

        let mut last_print = Instant::now() - Duration::from_secs(1);
        let deadline = Instant::now() + Duration::from_secs(600);
        let report = loop {
            match rx.try_recv() {
                Ok(Event::WorkingSetChanged { .. }) => {
                    if last_print.elapsed() >= Duration::from_millis(250) {
                        let snap = session.working_set();
                        let done = snap
                            .items
                            .iter()
                            .filter(|i| !matches!(i.state, ItemState::Planned))
                            .count();
                        eprintln!("loading {done}/{}", snap.items.len());
                        last_print = Instant::now();
                    }
                }
                Ok(Event::WorkingSetLoadFinished { report, .. }) => break report,
                Ok(Event::CommandFailed {
                    ticket: failed,
                    error,
                }) if failed == ticket => bail!("open failed: {error}"),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        bail!("open timed out");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    bail!("event stream closed before the open finished")
                }
            }
        };

        let snapshot = session.working_set();
        let mut any_failed = false;
        if json {
            for (index, item) in snapshot.items.iter().enumerate() {
                if matches!(item.state, ItemState::Failed) {
                    any_failed = true;
                }
                println!("{}", item_json(&session, index, item));
            }
            println!("{}", serde_json::to_string(&report)?);
        } else {
            for (index, item) in snapshot.items.iter().enumerate() {
                if matches!(item.state, ItemState::Failed) {
                    any_failed = true;
                }
                println!("{}", item_line(&session, index, item));
            }
            eprintln!(
                "{} planned, {} ready ({} reused, {} relocated), {} failed, {} collapsed, {}took {:.2}s",
                report.planned,
                report.ready,
                report.reused,
                report.relocated,
                report.failed,
                report.collapsed,
                if report.truncated { "TRUNCATED, " } else { "" },
                report.took.as_secs_f64(),
            );
        }

        session.close(lightbox_core::CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(if any_failed { 1 } else { 0 })
    })
}

fn state_str(state: &ItemState) -> String {
    match state {
        ItemState::Planned => "planned".to_owned(),
        ItemState::Ready { asset, image } => format!("ready(asset={},image={})", asset.0, image.0),
        ItemState::Failed => "failed".to_owned(),
        ItemState::DuplicateOf { index } => format!("duplicate-of({index})"),
    }
}

fn source_kind_str(k: Option<SourceKind>) -> &'static str {
    match k {
        Some(SourceKind::Raw) => "raw",
        Some(SourceKind::Rendered) => "rendered",
        Some(_) | None => "-",
    }
}

fn content_hash_hex(session: &Session, state: &ItemState) -> Option<String> {
    let ItemState::Ready { asset, .. } = state else {
        return None;
    };
    session
        .query()
        .asset_content_hash(*asset)
        .ok()
        .map(|h| h.to_hex())
}

fn item_line(session: &Session, index: usize, item: &lightbox_core::WorkingSetItem) -> String {
    let hash = content_hash_hex(session, &item.state).unwrap_or_else(|| "-".to_owned());
    let badge = item
        .decode_error
        .as_deref()
        .map(|e| format!(" [{e}]"))
        .unwrap_or_default();
    let dims = format!("{}x{}", item.width, item.height);
    format!(
        "{:>4}  {:<28}  {:<9}  {:<6}  {:<11}  {:<26}  {:<32}  {}{badge}",
        index,
        state_str(&item.state),
        source_kind_str(item.source_kind),
        item.format,
        dims,
        item.capture_time.as_deref().unwrap_or("-"),
        item.filename,
        hash,
    )
}

fn item_json(session: &Session, index: usize, item: &lightbox_core::WorkingSetItem) -> String {
    let (asset, image, duplicate_of) = match item.state {
        ItemState::Ready { asset, image } => (Some(asset.0), Some(image.0), None),
        ItemState::DuplicateOf { index } => (None, None, Some(index)),
        ItemState::Planned | ItemState::Failed => (None, None, None),
    };
    #[derive(serde::Serialize)]
    struct Line<'a> {
        index: usize,
        state: &'a str,
        asset: Option<i64>,
        image: Option<i64>,
        duplicate_of: Option<usize>,
        content_hash: Option<String>,
        format: &'a str,
        source_kind: &'a str,
        width: u32,
        height: u32,
        capture_time: Option<&'a str>,
        decode_error: Option<&'a str>,
        explicit: bool,
        path: String,
        filename: &'a str,
    }
    let state = match item.state {
        ItemState::Planned => "planned",
        ItemState::Ready { .. } => "ready",
        ItemState::Failed => "failed",
        ItemState::DuplicateOf { .. } => "duplicate_of",
    };
    let line = Line {
        index,
        state,
        asset,
        image,
        duplicate_of,
        content_hash: content_hash_hex(session, &item.state),
        format: &item.format,
        source_kind: source_kind_str(item.source_kind),
        width: item.width,
        height: item.height,
        capture_time: item.capture_time.as_deref(),
        decode_error: item.decode_error.as_deref(),
        explicit: item.explicit,
        path: item.path.display().to_string(),
        filename: &item.filename,
    };
    serde_json::to_string(&line).unwrap_or_else(|_| "{}".to_owned())
}

/// `lightbox-cli browse <DIR> [--json]` (mandate v2.2 addendum): the
/// headless folder-explorer harness — pure filesystem read, no catalog, no
/// session. Prints `dir`'s immediate children (subfolders, then supported
/// image files); `--json` emits the `DirListing` as one JSON object.
pub(crate) fn cmd_browse(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let json = flags.take_switch("--json");
        let positionals = flags.take_positionals();
        flags.finish()?;
        let (dir, extra) = positionals
            .split_first()
            .ok_or_else(|| UsageError("browse requires a <DIR>".to_owned()))?;
        if !extra.is_empty() {
            return Err(UsageError("browse takes exactly one <DIR>".to_owned()).into());
        }
        let dir = PathBuf::from(dir);

        let listing = browse_dir(&dir)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&listing)?);
        } else {
            for sub in &listing.subdirs {
                println!("{}/", sub.display());
            }
            for img in &listing.images {
                println!("{}", img.path.display());
            }
            eprintln!(
                "{} folder(s), {} image(s)",
                listing.subdirs.len(),
                listing.images.len()
            );
        }
        Ok(0)
    })
}
