// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Journaled cache-store relocation (E03 spec §3.2, Phase F T21):
//! copy-then-flip-then-delete, resumable after a crash, safe under
//! concurrent reads of the OLD root while it runs.
//!
//! **Scope: the E03-owned cache surfaces only.** `Store::root()` is the same
//! directory the catalog's `catalog.sqlite` lives in (spec §3.2's "same
//! directory the catalog lives in"), but `catalog.sqlite`/`backups/` are
//! E01's — this function relocates `previews/`, `rawcache/`, `smartpreview/`,
//! `masks/`, and `store.toml` only, never touching the catalog database.
//! `thumbcache.sqlite` (T22) is also excluded: it is disposable, delete-safe
//! cache with its own live connection held by [`crate::thumbs::ThumbCache`];
//! relocating it mid-write would need coordinating that connection, which
//! this phase's task scope does not ask for — a fresh, empty thumbcache at
//! the new root rebuilds lazily (the same "delete-safe" property T22 already
//! guarantees). Both scope choices are recorded in
//! `docs/plan/epics/E03-deviations.md`, Phase F.
//!
//! **Resumability.** A journal file (`<old_root>/.relocate-journal`, plain
//! text, one `DONE <relpath>` line per successfully copied file, each
//! `fsync`ed before the next copy starts) records progress. A crash mid-copy
//! loses at most the last journal line — never a copied FILE (each file
//! itself lands via [`crate::store::atomic_write`], the same fsync+rename
//! primitive every other store write uses) — so a resumed run at worst
//! re-copies one file it already had, which is idempotent (same source
//! bytes). [`relocate`] detects a journal from an INTERRUPTED run targeting
//! the SAME `new_root` and skips everything already marked `DONE`; a journal
//! targeting a DIFFERENT `new_root` (an abandoned earlier attempt) is
//! discarded and the run starts fresh.
//!
//! **The flip.** Only after every file is copied does this function write
//! `new_root/store.toml` with `relocated_from` set to the old root — the
//! durable signal that the NEW root is now a complete, self-consistent
//! store. Concurrent reads (a `PreviewService` instance already open against
//! the OLD `Store`) are unaffected for the whole copy phase: nothing at
//! `old_root` is deleted or mutated until after the flip succeeds.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::store::{atomic_write, StoreManifest, RESERVED_DIRS};

/// Progress callback (spec §5.2 `relocate(&self, new_root, progress)`).
pub type ProgressSink = Arc<dyn Fn(RelocateProgress) + Send + Sync>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RelocateProgress {
    pub done_files: u64,
    pub total_files: u64,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RelocateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("relocation journal: {0}")]
    Journal(String),
}

fn journal_path(store_root: &Path) -> PathBuf {
    store_root.join(".relocate-journal")
}

/// Loads a matching-target journal's `DONE` set, if one exists and targets
/// `new_root`. `Ok(None)` (not an error) for "no journal" or "a journal for
/// a different target" — both mean "start fresh".
fn load_resumable_journal(store_root: &Path, new_root: &Path) -> Option<HashSet<String>> {
    let text = std::fs::read_to_string(journal_path(store_root)).ok()?;
    let mut lines = text.lines();
    let target = lines.next()?.strip_prefix("TARGET ")?;
    if Path::new(target) != new_root {
        return None;
    }
    Some(
        lines
            .filter_map(|l| l.strip_prefix("DONE ").map(str::to_owned))
            .collect(),
    )
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Performs (or resumes) a relocation of `store_root`'s E03-owned cache
/// surfaces to `new_root` (spec §3.2 `PreviewService::relocate`, T21). A
/// no-op `Ok(())` if the two paths already match.
pub(crate) fn relocate(
    store_root: &Path,
    new_root: &Path,
    progress: &ProgressSink,
) -> Result<(), RelocateError> {
    if store_root == new_root {
        return Ok(());
    }
    std::fs::create_dir_all(new_root)?;

    let resumed = load_resumable_journal(store_root, new_root);
    let is_resume = resumed.is_some();
    let already_done = resumed.unwrap_or_default();

    let jpath = journal_path(store_root);
    let mut journal = if is_resume {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jpath)?
    } else {
        // Fresh run: truncate any stale journal (different/absent target)
        // and stamp the new one.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&jpath)?;
        writeln!(f, "TARGET {}", new_root.display())?;
        f.sync_data()?;
        f
    };

    let mut files = Vec::new();
    for dir in RESERVED_DIRS {
        collect_files(&store_root.join(dir), &mut files);
    }
    let manifest_path = store_root.join("store.toml");
    if manifest_path.is_file() {
        files.push(manifest_path.clone());
    }

    let total = files.len() as u64;
    let mut done = already_done.len() as u64;
    (progress)(RelocateProgress {
        done_files: done,
        total_files: total,
    });

    for abs in &files {
        let rel = abs
            .strip_prefix(store_root)
            .expect("every collected path is under store_root")
            .to_string_lossy()
            .replace('\\', "/");
        if already_done.contains(&rel) {
            continue;
        }
        let bytes = std::fs::read(abs)?;
        let dest = new_root.join(&rel);
        atomic_write(&dest, &bytes)?;
        writeln!(journal, "DONE {rel}")?;
        journal.sync_data()?;
        done += 1;
        (progress)(RelocateProgress {
            done_files: done,
            total_files: total,
        });
    }

    // The flip: the new root's own manifest now records where it came from.
    let new_manifest_path = new_root.join("store.toml");
    if let Ok(text) = std::fs::read_to_string(&new_manifest_path) {
        if let Ok(mut manifest) = toml::from_str::<StoreManifest>(&text) {
            manifest.relocated_from = Some(store_root.display().to_string());
            let out = toml::to_string_pretty(&manifest)
                .map_err(|e| RelocateError::Journal(format!("re-encoding store.toml: {e}")))?;
            atomic_write(&new_manifest_path, out.as_bytes())?;
        }
    }

    // Delete: best-effort clear of the OLD root's cache surfaces (never
    // fatal — a leftover old-root file is at worst wasted disk, not a
    // correctness problem; the catalog/callers now address the NEW root).
    for dir in RESERVED_DIRS {
        let _ = std::fs::remove_dir_all(store_root.join(dir));
        let _ = std::fs::create_dir_all(store_root.join(dir));
    }
    let _ = std::fs::remove_file(&manifest_path);
    let _ = std::fs::remove_file(&jpath);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn no_op_progress() -> ProgressSink {
        Arc::new(|_p| {})
    }

    fn seed_store(root: &Path, n_files: usize) {
        for dir in RESERVED_DIRS {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for i in 0..n_files {
            let hh = format!("{:02x}", i % 16);
            let dir = root.join("previews").join(&hh);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("f{i}.t0.jpg")), format!("payload-{i}")).unwrap();
        }
        std::fs::write(
            root.join("store.toml"),
            "format = 1\nstore_uuid = \"x\"\ncreated_by = \"t\"\n",
        )
        .unwrap();
    }

    fn all_payloads(root: &Path) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let mut stack = vec![root.join("previews")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.insert(std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        out
    }

    /// T21 AC (core property): every file present at the source ends up
    /// present, byte-identical, at the destination; the old root's cache
    /// dirs are cleared.
    #[test]
    fn relocate_moves_every_file_and_clears_the_old_root() {
        let old = tempfile::TempDir::new().unwrap();
        let new = tempfile::TempDir::new().unwrap();
        seed_store(old.path(), 40);
        let before = all_payloads(old.path());
        assert_eq!(before.len(), 40);

        relocate(old.path(), new.path(), &no_op_progress()).unwrap();

        let after = all_payloads(new.path());
        assert_eq!(before, after, "every file must survive byte-identical");
        assert!(
            all_payloads(old.path()).is_empty(),
            "old root must be cleared"
        );
        assert!(new.path().join("store.toml").is_file());
        let manifest: StoreManifest =
            toml::from_str(&std::fs::read_to_string(new.path().join("store.toml")).unwrap())
                .unwrap();
        assert_eq!(
            manifest.relocated_from.as_deref(),
            Some(old.path().to_string_lossy().as_ref())
        );
    }

    /// T21 AC: resuming a partially-completed relocation (simulated by
    /// seeding a journal with some files already marked DONE, matching what
    /// a killed-mid-copy run would leave behind) completes with zero lost
    /// entries and does not re-copy files unnecessarily.
    #[test]
    fn relocate_resumes_from_an_interrupted_journal_with_zero_lost_entries() {
        let old = tempfile::TempDir::new().unwrap();
        let new = tempfile::TempDir::new().unwrap();
        seed_store(old.path(), 25);
        let before = all_payloads(old.path());

        // Simulate "killed after copying about half": pre-copy a subset by
        // hand and seed a matching journal, exactly the state a resumed
        // process would find.
        let mut files = Vec::new();
        collect_files(&old.path().join("previews"), &mut files);
        files.sort();
        let (first_half, _second_half) = files.split_at(files.len() / 2);

        let mut journal = std::fs::File::create(journal_path(old.path())).unwrap();
        writeln!(journal, "TARGET {}", new.path().display()).unwrap();
        for abs in first_half {
            let rel = abs
                .strip_prefix(old.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(abs).unwrap();
            let dest = new.path().join(&rel);
            atomic_write(&dest, &bytes).unwrap();
            writeln!(journal, "DONE {rel}").unwrap();
        }
        journal.sync_data().unwrap();
        drop(journal);

        let copies = Arc::new(AtomicU64::new(0));
        let copies2 = Arc::clone(&copies);
        let progress: ProgressSink = Arc::new(move |p| {
            copies2.store(p.done_files, Ordering::SeqCst);
        });

        relocate(old.path(), new.path(), &progress).unwrap();

        let after = all_payloads(new.path());
        assert_eq!(before, after, "zero lost entries after resume");
        assert_eq!(
            copies.load(Ordering::SeqCst),
            25 + 1 /* + store.toml */
        );
    }

    #[test]
    fn relocate_to_the_same_root_is_a_no_op() {
        let dir = tempfile::TempDir::new().unwrap();
        seed_store(dir.path(), 5);
        let before = all_payloads(dir.path());
        relocate(dir.path(), dir.path(), &no_op_progress()).unwrap();
        assert_eq!(all_payloads(dir.path()), before);
    }
}
