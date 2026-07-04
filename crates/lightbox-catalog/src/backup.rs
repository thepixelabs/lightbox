// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Exit-time verified backup (spec §3.2 `backup_verified`).
//!
//! Sequence: SQLite online-backup → `PRAGMA integrity_check` **on the copy**
//! → zstd → temp+rename into `backups/YYYY-MM-DD-HHMMSS/catalog.sqlite.zst`
//! → prune to retention. Every fallible step happens in a scratch directory;
//! the dated directory only ever receives a complete, verified, compressed
//! file via `rename`, so a failure at any stage leaves prior backups
//! untouched (spec §5 T12 atomicity AC).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::clock::now_backup_stamp_utc;
use crate::error::{CatalogError, Result};

/// Options for [`crate::Catalog::backup_verified`] (spec §3.2).
#[derive(Clone, Debug)]
pub struct BackupOpts {
    /// How many dated backups to keep (`pre-upgrade-*` safety copies are
    /// never pruned). Clamped to at least 1 — pruning the backup we just
    /// wrote would defeat the point.
    pub retain: u32,
    /// Write backups somewhere other than `<lbdata>/backups/`.
    pub dest_override: Option<PathBuf>,
}

impl Default for BackupOpts {
    fn default() -> Self {
        BackupOpts {
            retain: 10,
            dest_override: None,
        }
    }
}

/// What a successful verified backup produced (spec §3.2).
#[derive(Clone, Debug)]
pub struct BackupReport {
    /// The compressed backup file (`…/YYYY-MM-DD-HHMMSS/catalog.sqlite.zst`).
    pub path: PathBuf,
    /// Size of the compressed file in bytes.
    pub bytes: u64,
    /// Wall time of the whole verified sequence.
    pub took: Duration,
}

/// zstd level: 3 is the library default — good ratio at interactive speed.
const ZSTD_LEVEL: i32 = 3;

/// Test-only injection point between "copy written" and "verify the copy"
/// (spec §5 T12: atomicity verified with injected failure).
pub(crate) type AfterCopyHook<'a> = Option<&'a dyn Fn(&Path) -> std::io::Result<()>>;

/// The full verified-backup pipeline. `src_db` must be a live catalog file;
/// a fresh read connection is opened for the online backup so the writer and
/// the reader pool stay untouched.
pub(crate) fn run_backup(
    src_db: &Path,
    dest_root: &Path,
    retain: u32,
    after_copy: AfterCopyHook<'_>,
) -> Result<BackupReport> {
    let start = Instant::now();
    std::fs::create_dir_all(dest_root)?;

    // Scratch space on the same filesystem as the final destination, so the
    // final rename is atomic.
    let scratch = dest_root.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&scratch)?;

    let outcome = backup_into_scratch(src_db, dest_root, &scratch, after_copy);
    // The scratch dir is removed on success and failure alike; only the
    // rename target survives.
    let _ = std::fs::remove_dir_all(&scratch);
    let path = outcome?;

    let bytes = std::fs::metadata(&path)?.len();
    prune(dest_root, retain.max(1))?;
    let took = start.elapsed();
    tracing::info!(path = %path.display(), bytes, ?took, "verified backup written");
    Ok(BackupReport { path, bytes, took })
}

fn backup_into_scratch(
    src_db: &Path,
    dest_root: &Path,
    scratch: &Path,
    after_copy: AfterCopyHook<'_>,
) -> Result<PathBuf> {
    // 1. Online backup → scratch copy (single-step: the copy is one
    //    consistent snapshot even while the writer keeps committing).
    let copy_path = scratch.join("catalog.sqlite");
    online_copy(src_db, &copy_path)?;

    if let Some(hook) = after_copy {
        hook(&copy_path)?;
    }

    // 2. integrity_check ON THE COPY (spec §3.2 — the whole point: a backup
    //    that fails verification is aborted, never promoted).
    verify_copy(&copy_path)?;

    // 3. zstd-compress the verified copy, still in scratch.
    let zst_path = scratch.join("catalog.sqlite.zst");
    compress(&copy_path, &zst_path)?;

    // 4. Promote: dated dir + rename (atomic on the same filesystem).
    let final_dir = create_dated_dir(dest_root)?;
    let final_path = final_dir.join("catalog.sqlite.zst");
    std::fs::rename(&zst_path, &final_path)?;
    Ok(final_path)
}

/// SQLite online-backup of `src_db` into `dst_path`.
fn online_copy(src_db: &Path, dst_path: &Path) -> Result<()> {
    let src = Connection::open(src_db)?;
    src.busy_timeout(std::time::Duration::from_millis(5000))?;
    let mut dst = Connection::open(dst_path)?;
    let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
    // One step over all pages: the backup API restarts on concurrent writes
    // only *between* steps, so a single full-pass step always completes with
    // a consistent snapshot regardless of writer traffic (T12 AC).
    backup.step(-1).map_err(|e| {
        CatalogError::BackupFailed(format!("online backup of {}: {e}", src_db.display()))
    })?;
    Ok(())
}

/// `PRAGMA integrity_check` on the copy; anything but a single `ok` aborts.
fn verify_copy(copy: &Path) -> Result<()> {
    let conn = Connection::open(copy)
        .map_err(|e| CatalogError::BackupFailed(format!("opening backup copy: {e}")))?;
    let findings = integrity_findings(&conn)
        .map_err(|e| CatalogError::BackupFailed(format!("integrity_check on copy: {e}")))?;
    if findings.is_empty() {
        Ok(())
    } else {
        Err(CatalogError::BackupFailed(format!(
            "integrity_check failed on the backup copy ({} finding{}): {}",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" },
            findings.join("; ")
        )))
    }
}

/// Runs `PRAGMA integrity_check`; empty = clean.
pub(crate) fn integrity_findings(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut findings = Vec::new();
    for row in rows {
        let msg = row?;
        if msg != "ok" {
            findings.push(msg);
        }
    }
    Ok(findings)
}

fn compress(src: &Path, dst: &Path) -> Result<()> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(src)?);
    let writer = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut encoder = zstd::stream::Encoder::new(writer, ZSTD_LEVEL)
        .map_err(|e| CatalogError::BackupFailed(format!("zstd encoder: {e}")))?;
    std::io::copy(&mut reader, &mut encoder)?;
    encoder
        .finish()
        .map_err(|e| CatalogError::BackupFailed(format!("zstd finish: {e}")))?;
    Ok(())
}

/// Creates `dest_root/YYYY-MM-DD-HHMMSS/`, suffixing `-2`, `-3`, … when
/// backups land within the same second. Suffixes are allocated as
/// max-existing + 1 and **never reused**, so `(stamp, suffix)` order is
/// creation order even after pruning freed an earlier name — otherwise a
/// re-used plain stamp would sort "oldest" and be pruned as soon as it was
/// written.
fn create_dated_dir(dest_root: &Path) -> Result<PathBuf> {
    let stamp = now_backup_stamp_utc();
    for _ in 0..100 {
        let next = std::fs::read_dir(dest_root)?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let (head, suffix) = parse_dated_name(&name)?;
                (head == stamp).then_some(suffix)
            })
            .max()
            .map(|max| max + 1);
        let name = match next {
            None => stamp.clone(),
            Some(n) => format!("{stamp}-{n}"),
        };
        let dir = dest_root.join(name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            // Race with another backup process: rescan and take the next slot.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(CatalogError::BackupFailed(
        "could not allocate a dated backup directory".into(),
    ))
}

/// Parses `YYYY-MM-DD-HHMMSS[-N]` into `(stamp, N)` (plain = 1) — the only
/// names pruning may touch, ordered by `(stamp, N)`.
fn parse_dated_name(name: &str) -> Option<(&str, u32)> {
    let bytes = name.as_bytes();
    if bytes.len() < 17 {
        return None;
    }
    let (head, tail) = name.split_at(17);
    let head_ok = head.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 | 10 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    if !head_ok {
        return None;
    }
    if tail.is_empty() {
        return Some((head, 1));
    }
    let suffix = tail.strip_prefix('-')?;
    // Suffixing starts at -2; "-0"/"-1" and non-numeric tails are not ours.
    let n: u32 = suffix.parse().ok().filter(|n| *n >= 2)?;
    Some((head, n))
}

/// Newest *verified* backup in `backups_dir` (used by the corrupt-open error
/// message, spec §1.1). A dated dir counts only if the `.zst` is present —
/// i.e. it passed verification and was promoted.
pub(crate) fn newest_backup(backups_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(backups_dir).ok()?;
    let mut best: Option<((String, u32), PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((stamp, suffix)) = parse_dated_name(&name) else {
            continue;
        };
        let key = (stamp.to_owned(), suffix);
        let zst = entry.path().join("catalog.sqlite.zst");
        if !zst.is_file() {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| key > *b) {
            best = Some((key, zst));
        }
    }
    best.map(|(_, p)| p)
}

/// Removes all but the newest `retain` dated backups. Never touches
/// `pre-upgrade-*` copies or anything else that is not a dated backup dir.
fn prune(dest_root: &Path, retain: u32) -> Result<()> {
    let mut dated: Vec<((String, u32), PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dest_root)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((stamp, suffix)) = parse_dated_name(&name) {
            if entry.path().is_dir() {
                dated.push(((stamp.to_owned(), suffix), entry.path()));
            }
        }
    }
    // Fixed-width UTC stamps + numeric same-second suffix: key order is
    // creation order.
    dated.sort_by(|a, b| b.0.cmp(&a.0));
    for ((stamp, suffix), path) in dated.into_iter().skip(retain as usize) {
        tracing::info!(backup = %stamp, suffix, "pruning old backup");
        std::fs::remove_dir_all(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_name_recognition() {
        assert_eq!(
            parse_dated_name("2026-07-05-183000"),
            Some(("2026-07-05-183000", 1))
        );
        assert_eq!(
            parse_dated_name("2026-07-05-183000-2"),
            Some(("2026-07-05-183000", 2))
        );
        assert_eq!(
            parse_dated_name("2026-07-05-183000-12"),
            Some(("2026-07-05-183000", 12))
        );
        assert_eq!(parse_dated_name("2026-07-05-183000-"), None);
        assert_eq!(parse_dated_name("2026-07-05-183000-1"), None); // never emitted
        assert_eq!(parse_dated_name("pre-upgrade-1"), None);
        assert_eq!(parse_dated_name(".tmp-123-456"), None);
        assert_eq!(parse_dated_name("2026-07-05"), None);
        assert_eq!(parse_dated_name("2026-07-05-18300x"), None);
    }

    #[test]
    fn suffix_order_is_creation_order() {
        // (stamp, suffix) keys: -12 must sort after -2 (numeric, not lexicographic).
        let mut keys = [
            ("2026-07-05-183000".to_owned(), 12u32),
            ("2026-07-05-183000".to_owned(), 1),
            ("2026-07-05-183000".to_owned(), 2),
            ("2026-07-05-182959".to_owned(), 1),
        ];
        keys.sort();
        assert_eq!(
            keys.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            vec![1, 1, 2, 12]
        );
    }
}
