// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The v2.0 working-set loader (E04 spec §4.3): turns an [`OpenRequest`]
//! (a dropped file, a multi-file selection, a folder, an OS open-dialog
//! selection, or a CLI/file-association launch) into an ordered, in-memory
//! session working set. Three phases, matching the spec §3.1 pipeline
//! shape:
//!
//! 1. [`plan_open`] — enumerate + probe + order (fast, no full-file reads).
//! 2. [`load_working_set`] — hash + register each planned item, in set
//!    order, one small WAL transaction per item (item 0 first by
//!    construction).
//!
//! Files open **in place**: no copy, no rename, no import session. Folder
//! expansion reuses [`crate::discover_files`] verbatim (E01, hidden-skip,
//! deterministic sort, cancel checkpoints, error folding all inherited) —
//! this module only adds the session-set semantics around it: explicit-file
//! honoring, the capture-time-then-name ordering key, content-hash identity,
//! and the open-registration DAO call.
//!
//! Seams (E04 spec §2.3): E08 constructs [`OpenRequest`] from the drop
//! handler / `rfd` dialog / platform launch handler and drives this module
//! through `lightbox-core`'s `Command::OpenWorkingSet`; the CLI's `open`
//! subcommand is the interim (and permanent headless-testing) driver. E03's
//! preview pyramid and E09's edit recipes key off the `ImageId`/`AssetId`
//! this module's [`load_working_set`] registers — this module never enqueues
//! a preview build or touches an edit recipe.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightbox_catalog::{CatalogError, OpenedFile, WriterHandle};
use lightbox_decode::{hash_file, probe, AssetProbe, ProbeError};
use lightbox_jobs::CancelToken;
use lightbox_types::{AssetId, ContentHash, ImageId, Orientation, SourceKind};
use unicode_normalization::UnicodeNormalization;

use crate::pipeline::{discover_files, has_known_extension, is_hidden, system_time_rfc3339_utc};

/// One user "open" gesture (E04 spec §2.4). Constructed by E08 (drop /
/// dialog / launch handler) or the CLI; consumed by
/// `lightbox-core::Command::OpenWorkingSet`. A new `OpenRequest` **replaces**
/// the session working set — there is no append gesture in the v1 contract
/// (spec OQ-1).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OpenRequest {
    /// Paths exactly as the OS handed them, in gesture order. Files and
    /// folders may mix (the open dialog allows both).
    pub paths: Vec<PathBuf>,
    /// Folder expansion mode for every folder in this request
    /// (drop-modifier or prefs default — spec §2.4).
    pub recursive: bool,
    /// Where the gesture came from. Tracing/UX copy only — the construction
    /// rules do not vary by origin.
    pub origin: OpenOrigin,
}

/// Where an [`OpenRequest`] came from (spec §2.4 entry table).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum OpenOrigin {
    /// Drag-and-drop onto the window or the empty-state drop zone.
    DragDrop,
    /// The native OS open dialog (`rfd`).
    OpenDialog,
    /// "Open With" / file-association launch.
    FileAssociation,
    /// `lightbox-cli open`.
    Cli,
}

/// Loader knobs (mirrors the [`crate::ImportOptions`] pattern — build via
/// [`OpenOptions::default`] and override fields; `#[non_exhaustive]`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OpenOptions {
    /// Hard cap on planned items; beyond it the plan truncates and reports
    /// (guards a recursive drop of a home directory). Default 10,000 (spec
    /// OQ-4: 10× the mandate's 1,000-folder bar).
    pub max_set_size: usize,
    /// Progress/changed-event coalescing interval. Default 100 ms.
    pub progress_min_interval: Duration,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions {
            max_set_size: 10_000,
            progress_min_interval: Duration::from_millis(100),
        }
    }
}

/// Phase-1 output: the ordered session set before any full-file read (spec
/// §4.3).
#[derive(Clone, Debug)]
pub struct SetPlan {
    /// Final session order (spec §6.3).
    pub items: Vec<PlannedItem>,
    /// Skipped paths, with reasons; shown by E08 on demand.
    pub skipped: Vec<SkippedPath>,
    /// `true` when `OpenOptions::max_set_size` was hit.
    pub truncated: bool,
}

/// One item in the plan (spec §4.3).
#[derive(Clone, Debug)]
pub struct PlannedItem {
    /// Canonical, absolute.
    pub path: PathBuf,
    /// NFC-normalized (E01 convention).
    pub filename: String,
    /// `Ok(probe)` or the probe error string (item still enters the set —
    /// explicit files and malformed known-extension files are *visible
    /// failures*, per the T18 cataloguing convention).
    pub probe: Result<AssetProbe, String>,
    /// Derived from `probe`; `None` = unsupported (no develop surface).
    pub source_kind: Option<SourceKind>,
    /// Named directly in the request vs. discovered by a folder walk.
    pub explicit: bool,
}

/// A path that did not become a [`PlannedItem`] (spec §4.3).
#[derive(Clone, Debug)]
pub struct SkippedPath {
    /// The offending path (canonical when canonicalization succeeded).
    pub path: PathBuf,
    /// Why.
    pub reason: SkipReason,
}

/// Why a path was skipped (spec §4.3, `#[non_exhaustive]`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SkipReason {
    /// Walk-discovered only; explicit files always enter regardless of
    /// extension.
    UnsupportedExtension,
    /// A dot-name below a walked root (E01 rule); an explicitly *chosen*
    /// hidden root/file is still honored.
    Hidden,
    /// The same canonical path appeared earlier in this request.
    DuplicatePath,
    /// The same content (by hash) as an earlier item — load phase (spec
    /// §6.4).
    DuplicateContent {
        /// The index of the earlier item this one collapsed into.
        first_index: usize,
    },
    /// Vanished between the gesture and enumeration.
    NotFound,
    /// The catalog requires UTF-8 paths (spec §4.3 / E01 §4.3 OQ-3).
    NonUtf8Path,
    /// An unreadable subtree, etc. (folded from [`crate::Discovery::errors`]
    /// or a folder that failed to start walking at all).
    WalkError(String),
    /// Truncated by `OpenOptions::max_set_size`.
    SetSizeCap,
}

/// Progress during [`plan_open`] (throttled; spec §4.3).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PlanProgress {
    /// Candidates enumerated so far (walk-discovered + explicit).
    pub enumerated: u64,
    /// Candidates probed so far.
    pub probed: u64,
    /// The path just probed.
    pub current: PathBuf,
}

/// Phase-1: enumerate + probe + order (spec §4.3). Fast (no full-file
/// reads); honors `cancel` between files; emits throttled progress.
///
/// Reuse, not rebuild: folder expansion calls [`crate::discover_files`]
/// verbatim (hidden-skip, sorted, cancel checkpoints, error folding all
/// inherited); [`crate::KNOWN_EXTENSIONS`] stays the single supported-
/// extension const.
pub fn plan_open(
    req: &OpenRequest,
    opts: &OpenOptions,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(PlanProgress),
) -> Result<SetPlan, OpenError> {
    if req.paths.is_empty() {
        return Err(OpenError::EmptyRequest);
    }

    let mut items: Vec<PlannedItem> = Vec::new();
    let mut skipped: Vec<SkippedPath> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut enumerated: u64 = 0;
    let mut probed: u64 = 0;
    let mut last_progress: Option<Instant> = None;

    for requested in &req.paths {
        if cancel.is_cancelled() {
            return Err(OpenError::Cancelled);
        }

        // §6.1: canonicalize first; a missing path is a per-path skip, never
        // an abort.
        let Ok(canonical) = std::fs::canonicalize(requested) else {
            skipped.push(SkippedPath {
                path: requested.clone(),
                reason: SkipReason::NotFound,
            });
            continue;
        };
        let Ok(meta) = std::fs::metadata(&canonical) else {
            skipped.push(SkippedPath {
                path: canonical,
                reason: SkipReason::NotFound,
            });
            continue;
        };

        // §6.1: dedupe canonical paths within one request, first wins. Also
        // catches an explicit file re-named by a later folder's walk (or
        // vice versa) — a single canonical-path set spans the whole plan.
        if !seen.insert(canonical.clone()) {
            skipped.push(SkippedPath {
                path: canonical,
                reason: SkipReason::DuplicatePath,
            });
            continue;
        }

        if meta.is_dir() {
            enumerated += 1;
            match discover_files(&canonical, req.recursive, cancel) {
                Ok(discovery) => {
                    let discovered_set: HashSet<PathBuf> =
                        discovery.files.iter().cloned().collect();
                    for (path, msg) in discovery.errors {
                        skipped.push(SkippedPath {
                            path,
                            reason: SkipReason::WalkError(msg),
                        });
                    }
                    enumerated += discovery.files.len() as u64;

                    let mut group: Vec<PlannedItem> = Vec::with_capacity(discovery.files.len());
                    for file in discovery.files {
                        if cancel.is_cancelled() {
                            return Err(OpenError::Cancelled);
                        }
                        // §6.1: walk-discovered non-UTF8 paths are a silent
                        // (counted) skip — explicit files are the only ones
                        // that get a visible per-item failure for this.
                        if file.to_str().is_none() {
                            skipped.push(SkippedPath {
                                path: file,
                                reason: SkipReason::NonUtf8Path,
                            });
                            continue;
                        }
                        if !seen.insert(file.clone()) {
                            skipped.push(SkippedPath {
                                path: file,
                                reason: SkipReason::DuplicatePath,
                            });
                            continue;
                        }
                        let item = plan_one(&file, false);
                        probed += 1;
                        maybe_plan_progress(
                            on_progress,
                            &mut last_progress,
                            opts,
                            enumerated,
                            probed,
                            &file,
                        );
                        group.push(item);
                    }
                    // §6.3: sort the whole expansion (recursive or not) as
                    // one group by (capture-time key, filename, path).
                    group.sort_by(|a, b| order_key(a).cmp(&order_key(b)));

                    // Skip accounting for what `discover_files` silently
                    // dropped (spec §6.2: "counted so E08 can say '312
                    // files opened, 40 non-photo files skipped'"). A
                    // best-effort companion pass — `discover_files` does not
                    // itself report what it filtered (E01's frozen return
                    // shape), so this re-derives it from the same known-
                    // extension/hidden rules (§9 deviation, see
                    // E04-deviations.md: may over-count entries nested
                    // under a hidden directory, since it does not stop
                    // descent there — informational bookkeeping only, never
                    // affects the actual working-set contents).
                    skipped.extend(hidden_and_unsupported_skips(
                        &canonical,
                        req.recursive,
                        &discovered_set,
                        cancel,
                    ));

                    items.extend(group);
                }
                Err(e) => {
                    // A folder that fails to even start walking is a
                    // per-unit skip, never a whole-request abort (§6.6).
                    skipped.push(SkippedPath {
                        path: canonical,
                        reason: SkipReason::WalkError(e.to_string()),
                    });
                }
            }
        } else {
            enumerated += 1;
            let item = plan_one(&canonical, true);
            probed += 1;
            maybe_plan_progress(
                on_progress,
                &mut last_progress,
                opts,
                enumerated,
                probed,
                &canonical,
            );
            items.push(item);
        }
    }

    // §6.3 point 4: max_set_size truncates AFTER ordering. Applied once, to
    // the final gesture-ordered concatenation across every request unit.
    let truncated = items.len() > opts.max_set_size;
    if truncated {
        let overflow = items.split_off(opts.max_set_size);
        skipped.extend(overflow.into_iter().map(|it| SkippedPath {
            path: it.path,
            reason: SkipReason::SetSizeCap,
        }));
    }

    Ok(SetPlan {
        items,
        skipped,
        truncated,
    })
}

/// Probes one candidate file into a [`PlannedItem`] (spec §6.2): a
/// non-UTF8 path is a visible failure at plan time already (it can never be
/// registered — matches the E01 import convention of no row for what cannot
/// be identified); otherwise probe failures still produce an item (visible
/// failure, badged, never silently dropped).
fn plan_one(path: &Path, explicit: bool) -> PlannedItem {
    let filename = match path.file_name().and_then(|n| n.to_str()) {
        Some(s) => s.nfc().collect::<String>(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if path.to_str().is_none() {
        return PlannedItem {
            path: path.to_path_buf(),
            filename,
            probe: Err("path is not valid UTF-8".to_owned()),
            source_kind: None,
            explicit,
        };
    }
    match probe(path) {
        Ok(p) => {
            let source_kind = p.format.source_kind();
            PlannedItem {
                path: path.to_path_buf(),
                filename,
                probe: Ok(p),
                source_kind,
                explicit,
            }
        }
        Err(e) => PlannedItem {
            path: path.to_path_buf(),
            filename,
            probe: Err(e.to_string()),
            source_kind: None,
            explicit,
        },
    }
}

/// §6.3 ordering key: `(has_no_time, capture_key, filename, path)`.
/// `has_no_time` sorts absent/unparseable capture times after every timed
/// item; within each group, ties break by NFC filename bytewise, then full
/// path — a total order.
fn order_key(item: &PlannedItem) -> (bool, i128, &str, &Path) {
    let capture = item
        .probe
        .as_ref()
        .ok()
        .and_then(|p| capture_order_key(p.capture_time.as_deref()));
    match capture {
        Some(t) => (false, t, item.filename.as_str(), item.path.as_path()),
        None => (true, 0, item.filename.as_str(), item.path.as_path()),
    }
}

/// Parses a probe's `capture_time` (RFC3339 with an offset, or the probe's
/// bare `"YYYY-MM-DDTHH:MM:SS"` naive form) into a sortable nanosecond key.
/// Offset-bearing values normalize to UTC; naive values are compared
/// as-if-UTC (spec §6.3: "cameras without offset metadata sort in
/// local-time order, which is the photographer's expectation anyway").
/// `None` for absent/unparseable input.
fn capture_order_key(capture_time: Option<&str>) -> Option<i128> {
    let s = capture_time?;
    if let Ok(dt) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return Some(dt.unix_timestamp_nanos());
    }
    let naive = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    time::PrimitiveDateTime::parse(s, &naive)
        .ok()
        .map(|pdt| pdt.assume_utc().unix_timestamp_nanos())
}

/// UTC-normalizes `capture_time` for *storage* when it carries an offset;
/// verbatim otherwise (spec §6.5 — closes the E01 Phase-5 deviation
/// "UTC-normalizing at the ingest seam is E04 cleanup"). Distinct from
/// [`capture_order_key`]: this produces the catalog's fixed-width RFC3339
/// UTC string, not a sort key.
fn utc_normalize_capture_time(capture_time: &Option<String>) -> Option<String> {
    let s = capture_time.as_ref()?;
    match time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        Ok(dt) => {
            let utc = dt.to_offset(time::UtcOffset::UTC);
            let format = time::macros::format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].000000Z"
            );
            utc.format(&format).ok().or_else(|| Some(s.clone()))
        }
        Err(_) => Some(s.clone()), // naive (no offset) or unparseable -> verbatim
    }
}

fn maybe_plan_progress(
    on_progress: &mut dyn FnMut(PlanProgress),
    last: &mut Option<Instant>,
    opts: &OpenOptions,
    enumerated: u64,
    probed: u64,
    current: &Path,
) {
    let due = last.is_none_or(|t| t.elapsed() >= opts.progress_min_interval);
    if due {
        *last = Some(Instant::now());
        on_progress(PlanProgress {
            enumerated,
            probed,
            current: current.to_path_buf(),
        });
    }
}

/// Best-effort companion pass (see [`plan_open`]'s doc comment): walks `dir`
/// the same way [`discover_files`] does, classifying every file NOT already
/// in `discovered` as `Hidden` or `UnsupportedExtension`. Cancel-aware
/// (returns whatever was found so far on cancellation — purely
/// informational, never load-bearing).
fn hidden_and_unsupported_skips(
    dir: &Path,
    recursive: bool,
    discovered: &HashSet<PathBuf>,
    cancel: &CancelToken,
) -> Vec<SkippedPath> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_map(std::result::Result::ok);
    for entry in walker {
        if cancel.is_cancelled() {
            break;
        }
        if entry.depth() == 0 || !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if discovered.contains(&path) {
            continue; // already in the plan
        }
        let name = path.file_name().unwrap_or_default();
        let reason = if is_hidden(name) {
            SkipReason::Hidden
        } else if !has_known_extension(&path) {
            SkipReason::UnsupportedExtension
        } else {
            // Shouldn't happen (same rules as discover_files), but never
            // silently drop a path from the accounting.
            SkipReason::UnsupportedExtension
        };
        out.push(SkippedPath { path, reason });
    }
    out
}

/// Item states emitted while [`load_working_set`] runs (spec §4.3).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum LoadEvent {
    /// Registered; previews/render/edits may be requested for `image`.
    ItemReady {
        /// The item's index in [`SetPlan::items`].
        index: usize,
        /// The (found-or-created) asset.
        asset: AssetId,
        /// The default image row.
        image: ImageId,
        /// Resolved to a pre-existing asset row (reopen).
        reused: bool,
        /// The content-hash lookup hit at a different path than before.
        relocated: bool,
    },
    /// Hash or registration failed; the item stays visible with a badge.
    ItemFailed {
        /// The item's index in [`SetPlan::items`].
        index: usize,
        /// Why.
        reason: String,
    },
    /// Collapsed into an earlier identical-content item (spec §6.4).
    ItemCollapsed {
        /// The item's index in [`SetPlan::items`].
        index: usize,
        /// The index of the earlier item this one collapsed into.
        first_index: usize,
    },
    /// Throttled progress heartbeat.
    Progress {
        /// Items fully processed so far.
        done: u64,
        /// Total items in the plan.
        total: u64,
    },
}

/// What an open did (broadcast in `Event::WorkingSetLoadFinished`; printed
/// by `lightbox-cli open`).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct OpenReport {
    /// Items in the plan.
    pub planned: usize,
    /// Items that registered successfully (created + reused).
    pub ready: usize,
    /// Subset of `ready` that resolved to pre-existing asset rows (reopen).
    pub reused: usize,
    /// Subset of `reused` where the hash-hit was at a new path (file moved).
    pub relocated: usize,
    /// Items that failed to register (no row).
    pub failed: usize,
    /// Duplicate-content items (spec §6.4).
    pub collapsed: usize,
    /// `true` when [`SetPlan::truncated`] was `true`.
    pub truncated: bool,
    /// Wall time of the load phase.
    pub took: Duration,
}

/// Phase-2: hash + register each planned item, **in set order** (item 0 —
/// the loupe file — completes first, by construction: this loop is
/// sequential). One small WAL transaction per item so the first image is
/// never queued behind a batch (spec §6.5). Content duplicates collapse
/// (spec §6.4). Never aborts on per-item failure.
pub fn load_working_set(
    writer: &WriterHandle,
    plan: &SetPlan,
    opts: &OpenOptions,
    cancel: &CancelToken,
    on_event: &mut dyn FnMut(LoadEvent),
) -> Result<OpenReport, OpenError> {
    let started = Instant::now();
    let total = plan.items.len() as u64;
    let mut report = OpenReport {
        planned: plan.items.len(),
        truncated: plan.truncated,
        ..OpenReport::default()
    };
    let mut hashes_seen: HashMap<ContentHash, usize> = HashMap::new();
    let mut done: u64 = 0;
    let mut last_progress: Option<Instant> = None;

    for (index, item) in plan.items.iter().enumerate() {
        if cancel.is_cancelled() {
            break; // truthful partial report (spec §6.6)
        }

        // Registration-time UTF-8 gate (spec §6.1/E01 §4.3 note c): a
        // non-UTF8 path was already a visible plan-time failure for
        // explicit files; walk-discovered ones never reach here (skipped in
        // phase 1). Either way, it gets no row — content_hash identity
        // requires the catalog's UTF-8 abs_path/filename columns.
        let Some(abs_path) = item.path.to_str() else {
            report.failed += 1;
            on_event(LoadEvent::ItemFailed {
                index,
                reason: "path is not valid UTF-8".to_owned(),
            });
            done += 1;
            maybe_load_progress(on_event, &mut last_progress, opts, done, total);
            continue;
        };

        let meta = match std::fs::metadata(&item.path) {
            Ok(m) => m,
            Err(e) => {
                report.failed += 1;
                on_event(LoadEvent::ItemFailed {
                    index,
                    reason: format!("stat failed: {e}"),
                });
                done += 1;
                maybe_load_progress(on_event, &mut last_progress, opts, done, total);
                continue;
            }
        };

        let hash = match hash_file(&item.path, cancel) {
            Ok(h) => h,
            Err(ProbeError::Cancelled) => break,
            Err(e) => {
                report.failed += 1;
                on_event(LoadEvent::ItemFailed {
                    index,
                    reason: format!("hash failed: {e}"),
                });
                done += 1;
                maybe_load_progress(on_event, &mut last_progress, opts, done, total);
                continue;
            }
        };

        // §6.4: content-duplicate collapse (this epoch/run only).
        if let Some(&first_index) = hashes_seen.get(&hash) {
            report.collapsed += 1;
            on_event(LoadEvent::ItemCollapsed { index, first_index });
            done += 1;
            maybe_load_progress(on_event, &mut last_progress, opts, done, total);
            continue;
        }
        hashes_seen.insert(hash, index);

        let mtime_utc = meta.modified().ok().and_then(system_time_rfc3339_utc);
        let opened = OpenedFile {
            abs_path: abs_path.to_owned(),
            filename: item.filename.clone(),
            content_hash: hash,
            format: match &item.probe {
                Ok(p) => p.format.catalog_tag().to_owned(),
                Err(_) => "UNSUPPORTED".to_owned(),
            },
            camera_make: item.probe.as_ref().ok().and_then(|p| p.camera_make.clone()),
            camera_model: item
                .probe
                .as_ref()
                .ok()
                .and_then(|p| p.camera_model.clone()),
            capture_time: item
                .probe
                .as_ref()
                .ok()
                .and_then(|p| utc_normalize_capture_time(&p.capture_time)),
            width: item.probe.as_ref().map(|p| p.width).unwrap_or(0),
            height: item.probe.as_ref().map(|p| p.height).unwrap_or(0),
            orientation: item
                .probe
                .as_ref()
                .map(|p| p.orientation)
                .unwrap_or(Orientation::O1),
            bytes: meta.len(),
            mtime_utc,
            decode_error: item
                .probe
                .as_ref()
                .err()
                .map(|e| format!("probe failed: {e}")),
        };

        match writer.with_txn(move |txn| txn.ensure_open_asset(&opened)) {
            Ok(outcome) => {
                report.ready += 1;
                if !outcome.created {
                    report.reused += 1;
                }
                if outcome.relocated {
                    report.relocated += 1;
                }
                on_event(LoadEvent::ItemReady {
                    index,
                    asset: outcome.asset,
                    image: outcome.image,
                    reused: !outcome.created,
                    relocated: outcome.relocated,
                });
            }
            Err(e) => {
                report.failed += 1;
                on_event(LoadEvent::ItemFailed {
                    index,
                    reason: format!("catalog: {e}"),
                });
            }
        }
        done += 1;
        maybe_load_progress(on_event, &mut last_progress, opts, done, total);
    }

    report.took = started.elapsed();
    Ok(report)
}

fn maybe_load_progress(
    on_event: &mut dyn FnMut(LoadEvent),
    last: &mut Option<Instant>,
    opts: &OpenOptions,
    done: u64,
    total: u64,
) {
    let due = last.is_none_or(|t| t.elapsed() >= opts.progress_min_interval);
    if due || done == total {
        *last = Some(Instant::now());
        on_event(LoadEvent::Progress { done, total });
    }
}

/// Errors that abort an open outright (per-item problems never do — they
/// land in [`OpenReport`]/[`LoadEvent::ItemFailed`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// `OpenRequest::paths` was empty.
    #[error("empty open request")]
    EmptyRequest,
    /// A catalog transaction failed outright (not a per-item registration
    /// failure — those are [`LoadEvent::ItemFailed`]).
    #[error("catalog: {0}")]
    Catalog(#[from] CatalogError),
    /// The operation observed cancellation at a checkpoint and stopped.
    #[error("cancelled")]
    Cancelled,
    /// Filesystem error outside the per-item paths (e.g. enumerating a
    /// request unit).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests;
