// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The add-in-place import pipeline (spec §5 T17/T18).
//!
//! Three primitives, layered so **E04** can reuse the lower ones with its
//! own file lists (card detection, watched folders):
//!
//! 1. [`discover_files`], deterministic, extension-filtered walk.
//! 2. [`import_files`], probe + hash + batched insert of a given list,
//!    with import-session bracketing, throttled progress, cancellation.
//! 3. [`import_add_in_place`], 1 ⊕ 2 over a source directory.
//!
//! Batching: `ImportOptions::batch_size` files per **single WAL
//! transaction** (spec §7 burst-import mitigation); a cancel between files
//! commits completed batches only and discards the partially staged one
//! no partial rows, ever (T17 AC).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use lightbox_catalog::{NewAsset, WriterHandle};
use lightbox_decode::{hash_file, probe, ProbeError};
use lightbox_jobs::CancelToken;
use lightbox_types::{ContentHash, FolderId, Orientation, RootId};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use unicode_normalization::UnicodeNormalization;

use crate::report::{ImportEvent, ImportOptions, ImportOutcome, ImportReport};
use crate::IngestError;

/// File extensions the M0 importer picks up (case-insensitive): the fixture
/// corpus's seven raw mounts plus the common raw/non-raw suspects. E04 owns
/// the full format matrix.
pub const KNOWN_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "dng", "jpeg", "jpg", "nef", "orf", "pef", "png", "raf", "rw2", "tif",
    "tiff",
];

/// Fixed-width RFC3339 UTC (matches the catalog's timestamp convention:
/// lexicographic == chronological). `pub(crate)`: E04's `working_set` module
/// reuses this and [`system_time_rfc3339_utc`] verbatim (same mtime
/// formatting convention) rather than duplicating them.
pub(crate) const RFC3339_MICROS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

/// What a discovery walk found.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    /// Importable files (known extension), sorted by path, deterministic
    /// batch composition regardless of filesystem enumeration order.
    pub files: Vec<PathBuf>,
    /// Walk-level failures (unreadable subdirectory, …); folded into
    /// [`ImportReport::errors`] by [`import_files`].
    pub errors: Vec<(PathBuf, String)>,
}

impl Discovery {
    /// Wraps an externally produced file list (E04 primitives).
    pub fn from_files(files: Vec<PathBuf>) -> Discovery {
        Discovery {
            files,
            errors: Vec::new(),
        }
    }
}

/// Walks `source_dir` and collects importable files (spec T17: `walkdir`
/// discovery filtered by known extensions).
///
/// Hidden entries (name starting with `.`, e.g. AppleDouble `._IMG.jpg`,
/// `.DS_Store` trees) below the root are skipped. Cancellation stops the
/// walk early and returns what was found so far, the subsequent
/// [`import_files`] observes the same token before committing anything.
pub fn discover_files(
    source_dir: &Path,
    recursive: bool,
    cancel: &CancelToken,
) -> Result<Discovery, IngestError> {
    let meta = std::fs::metadata(source_dir).map_err(|e| IngestError::InvalidSource {
        path: source_dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    if !meta.is_dir() {
        return Err(IngestError::InvalidSource {
            path: source_dir.to_path_buf(),
            reason: "not a directory".to_owned(),
        });
    }

    let mut discovery = Discovery::default();
    let walker = walkdir::WalkDir::new(source_dir)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()));
    for entry in walker {
        if cancel.is_cancelled() {
            break;
        }
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() && has_known_extension(entry.path()) {
                    discovery.files.push(entry.into_path());
                }
            }
            Err(err) => {
                let at = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| source_dir.to_path_buf());
                discovery.errors.push((at, err.to_string()));
            }
        }
    }
    discovery.files.sort();
    Ok(discovery)
}

/// Imports a discovered file list under `source_root` (spec T17): per-file
/// probe + xxh3-128 hash, batched `insert_assets` + `insert_default_images`
/// (one WAL transaction per batch), session bracketing, throttled
/// [`ImportEvent`]s, cooperative cancellation between files.
///
/// The session is *always* finished-with-stats, including on cancellation
/// (T17 AC), so `import_session` rows never dangle unless the process
/// dies, which the fault harness covers.
pub fn import_files(
    writer: &WriterHandle,
    source_root: &Path,
    discovery: &Discovery,
    opts: &ImportOptions,
    cancel: &CancelToken,
    on_event: &mut dyn FnMut(ImportEvent),
) -> Result<ImportOutcome, IngestError> {
    let started = Instant::now();
    let batch_size = opts.batch_size.max(1);
    let discovered = discovery.files.len() as u64;

    // Bracket: root + root folder + session row, one transaction.
    let root_path = source_root.to_path_buf();
    let source_str = root_path.to_string_lossy().into_owned();
    let opts_json = serde_json::to_string(opts).unwrap_or_else(|_| "{}".to_owned());
    let (root, session) = writer.with_txn(move |txn| {
        let root = txn.upsert_root(None, &root_path)?;
        txn.upsert_folder(root, None, "")?;
        let session = txn.begin_import_session(&source_str, &opts_json)?;
        Ok((root, session))
    })?;
    on_event(ImportEvent::Started {
        session,
        discovered,
    });
    tracing::info!(
        target: "lightbox_ingest",
        source = %source_root.display(),
        files = discovered,
        session = session.0,
        "import started"
    );

    let mut report = ImportReport {
        errors: discovery.errors.clone(),
        ..ImportReport::default()
    };
    let mut staged: Vec<Staged> = Vec::with_capacity(batch_size);
    let mut done: u64 = 0;
    let mut cancelled = false;
    let mut last_progress: Option<Instant> = None;

    for file in &discovery.files {
        // Cancellation checkpoint between files (spec T17). The partially
        // staged batch is discarded, completed batches only.
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        match stage_file(file, source_root, cancel) {
            StageResult::Staged(s) => staged.push(*s),
            StageResult::Failed(reason) => {
                tracing::warn!(target: "lightbox_ingest", file = %file.display(), %reason, "file not imported");
                report.errors.push((file.clone(), reason));
            }
            StageResult::Cancelled => {
                cancelled = true;
                break;
            }
        }
        done += 1;

        let due = last_progress.is_none_or(|t| t.elapsed() >= opts.progress_min_interval);
        if due || done == discovered {
            last_progress = Some(Instant::now());
            on_event(ImportEvent::Progress {
                session,
                done,
                discovered,
                current: file.clone(),
            });
        }

        if staged.len() >= batch_size {
            commit_batch(writer, root, session, &mut staged, &mut report)?;
        }
    }

    if !cancelled && !staged.is_empty() {
        commit_batch(writer, root, session, &mut staged, &mut report)?;
    }

    report.took = started.elapsed();
    let stats = SessionStats {
        cancelled,
        report: &report,
    };
    let stats_json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_owned());
    writer.with_txn(move |txn| txn.finish_import_session(session, &stats_json))?;

    tracing::info!(
        target: "lightbox_ingest",
        session = session.0,
        imported = report.imported,
        skipped = report.skipped_duplicates,
        errors = report.errors.len(),
        cancelled,
        took = ?report.took,
        "import finished"
    );
    Ok(ImportOutcome {
        session,
        report,
        cancelled,
    })
}

/// Discovery ⊕ import over a source directory, what
/// `Command::ImportAddInPlace` runs (spec §3.8).
///
/// The source directory is canonicalized so the stored `library_root` path
/// is stable across `.`/`..`/symlink spellings of the same directory.
pub fn import_add_in_place(
    writer: &WriterHandle,
    source_dir: &Path,
    opts: &ImportOptions,
    cancel: &CancelToken,
    on_event: &mut dyn FnMut(ImportEvent),
) -> Result<ImportOutcome, IngestError> {
    let root = std::fs::canonicalize(source_dir).map_err(|e| IngestError::InvalidSource {
        path: source_dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    let discovery = discover_files(&root, opts.recursive, cancel)?;
    import_files(writer, &root, &discovery, opts, cancel, on_event)
}

/// The session `stats` JSON: the report plus the cancellation marker
/// (spec T17 AC "session marked finished-with-stats" on cancel).
#[derive(serde::Serialize)]
struct SessionStats<'a> {
    cancelled: bool,
    report: &'a ImportReport,
}

/// A file staged for the next batch transaction: everything `NewAsset`
/// needs except the `FolderId`, which only exists once the batch
/// transaction upserts the folder chain.
#[derive(Clone, Debug)]
struct Staged {
    /// `'/'`-separated path of the containing dir relative to the root
    /// (`""` = the root itself).
    rel_dir: String,
    /// NFC-normalized filename (spec §4.3 / OQ-7).
    filename: String,
    hash: ContentHash,
    format: String,
    camera_make: Option<String>,
    camera_model: Option<String>,
    capture_time: Option<String>,
    width: u32,
    height: u32,
    orientation: Orientation,
    bytes: u64,
    mtime_utc: Option<String>,
    decode_error: Option<String>,
}

enum StageResult {
    Staged(Box<Staged>),
    /// The file gets no row; the reason lands in `ImportReport::errors`.
    Failed(String),
    /// The hash observed cancellation mid-file.
    Cancelled,
}

/// Probe + hash one file. Never aborts the import: every failure is either
/// catalogued (`decode_error`) or reported per-file (see crate docs).
fn stage_file(file: &Path, root: &Path, cancel: &CancelToken) -> StageResult {
    // Path pieces must be UTF-8 (spec §4.3 note c / OQ-3).
    let Ok(rel) = file.strip_prefix(root) else {
        return StageResult::Failed(format!("not under import root {}", root.display()));
    };
    let Some(rel_str) = rel.to_str() else {
        return StageResult::Failed("path is not valid UTF-8".to_owned());
    };
    let Some(filename_raw) = rel.file_name().and_then(|n| n.to_str()) else {
        return StageResult::Failed("filename is not valid UTF-8".to_owned());
    };
    let filename: String = filename_raw.nfc().collect();
    let rel_dir = match rel_str.rsplit_once(std::path::MAIN_SEPARATOR) {
        Some((dir, _)) => dir.replace(std::path::MAIN_SEPARATOR, "/"),
        None => String::new(),
    };

    let meta = match std::fs::metadata(file) {
        Ok(m) => m,
        Err(e) => return StageResult::Failed(format!("stat failed: {e}")),
    };

    // Content hash first: a file we cannot read at all gets no row
    // (content_hash is NOT NULL, it IS the identity).
    let hash = match hash_file(file, cancel) {
        Ok(h) => h,
        Err(ProbeError::Cancelled) => return StageResult::Cancelled,
        Err(e) => return StageResult::Failed(format!("hash failed: {e}")),
    };

    // Probe failures are catalogued with `decode_error` set (spec T18).
    let (probe_data, decode_error) = match probe(file) {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(format!("probe failed: {e}"))),
    };

    let mtime_utc = meta.modified().ok().and_then(system_time_rfc3339_utc);
    let staged = match probe_data {
        Some(p) => Staged {
            rel_dir,
            filename,
            hash,
            format: p.format.catalog_tag().to_owned(),
            camera_make: p.camera_make,
            camera_model: p.camera_model,
            capture_time: p.capture_time,
            width: p.width,
            height: p.height,
            orientation: p.orientation,
            bytes: p.file_bytes,
            mtime_utc,
            decode_error,
        },
        None => Staged {
            rel_dir,
            filename,
            hash,
            format: "UNSUPPORTED".to_owned(),
            camera_make: None,
            camera_model: None,
            capture_time: None,
            width: 0,
            height: 0,
            orientation: Orientation::O1,
            bytes: meta.len(),
            mtime_utc,
            decode_error,
        },
    };
    StageResult::Staged(Box::new(staged))
}

/// One batch = one WAL transaction: folder upserts → `insert_assets`
/// (dup-skip on content hash) → `insert_default_images`.
fn commit_batch(
    writer: &WriterHandle,
    root: RootId,
    session: lightbox_types::ImportSessionId,
    staged: &mut Vec<Staged>,
    report: &mut ImportReport,
) -> Result<(), IngestError> {
    let batch = std::mem::take(staged);
    // Report probe failures per file (they are catalogued regardless).
    let probe_failures: Vec<(String, String, String)> = batch
        .iter()
        .filter_map(|s| {
            s.decode_error
                .as_ref()
                .map(|e| (s.rel_dir.clone(), s.filename.clone(), e.clone()))
        })
        .collect();
    let unsupported_flags: Vec<bool> = batch.iter().map(|s| s.format == "UNSUPPORTED").collect();

    let outcome = writer.with_txn(move |txn| {
        let mut folders: HashMap<String, FolderId> = HashMap::new();
        let mut assets = Vec::with_capacity(batch.len());
        for s in &batch {
            let folder = match folders.get(&s.rel_dir) {
                Some(f) => *f,
                None => {
                    let f = txn.upsert_folder(root, None, &s.rel_dir)?;
                    folders.insert(s.rel_dir.clone(), f);
                    f
                }
            };
            assets.push(NewAsset {
                folder,
                filename: s.filename.clone(),
                content_hash: s.hash,
                format: s.format.clone(),
                camera_make: s.camera_make.clone(),
                camera_model: s.camera_model.clone(),
                capture_time: s.capture_time.clone(),
                width: s.width,
                height: s.height,
                orientation: s.orientation,
                bytes: s.bytes,
                mtime_utc: s.mtime_utc.clone(),
                decode_error: s.decode_error.clone(),
                import_session: Some(session),
            });
        }
        let inserted = txn.insert_assets(&assets)?;
        txn.insert_default_images(&inserted.inserted)?;
        Ok(inserted)
    })?;

    report.imported += outcome.inserted.len() as u64;
    report.skipped_duplicates += outcome.skipped_duplicates;
    let mut skipped = outcome.skipped.iter().peekable();
    for (idx, is_unsupported) in unsupported_flags.iter().enumerate() {
        if skipped.next_if(|s| **s == idx).is_some() {
            continue; // duplicate, not imported, not counted as unsupported
        }
        if *is_unsupported {
            report.unsupported += 1;
        }
    }
    for (rel_dir, filename, error) in probe_failures {
        let mut path = PathBuf::from(rel_dir);
        path.push(filename);
        report.errors.push((path, error));
    }
    Ok(())
}

/// `pub(crate)`: reused by [`crate::working_set`]'s skip-accounting pass and
/// [`crate::browse`]'s hidden-skip rule (E04), the same E01 dot-name rule.
pub(crate) fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// `pub(crate)`: reused by [`crate::browse::browse_dir`] (E04), same
/// extension-claim rule as the walk, kept in one place.
pub(crate) fn has_known_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| KNOWN_EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)))
}

pub(crate) fn system_time_rfc3339_utc(t: SystemTime) -> Option<String> {
    time::OffsetDateTime::from(t).format(&RFC3339_MICROS).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_is_case_insensitive_and_strict() {
        assert!(has_known_extension(Path::new("a/IMG_1.CR3")));
        assert!(has_known_extension(Path::new("a/IMG_1.jpg")));
        assert!(has_known_extension(Path::new("a/IMG_1.JPEG")));
        assert!(!has_known_extension(Path::new("a/notes.txt")));
        assert!(!has_known_extension(Path::new("a/noext")));
        assert!(!has_known_extension(Path::new("a/.cr3"))); // pure dotfile has no extension
    }

    #[test]
    fn hidden_names() {
        assert!(is_hidden(std::ffi::OsStr::new(".DS_Store")));
        assert!(is_hidden(std::ffi::OsStr::new("._IMG_1.jpg")));
        assert!(!is_hidden(std::ffi::OsStr::new("IMG_1.jpg")));
    }

    #[test]
    fn mtime_formatting_is_fixed_width_utc() {
        let s = system_time_rfc3339_utc(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(s, "1970-01-01T00:00:00.000000Z");
    }
}
