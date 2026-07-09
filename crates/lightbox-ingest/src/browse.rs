// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The live-filesystem folder-explorer primitive (mandate v2.2 —
//! `docs/plan/00-mandate.md`, "a transient, live-filesystem folder explorer
//! … is permitted as a third entry/navigation aid"; E04 spec addendum
//! v2.2). Finder/Explorer-style **immediate-children** directory listing:
//! child subfolders + directly-contained supported image files, for
//! interactive nested-folder navigation.
//!
//! This is deliberately **not** managed import and **not** a library:
//! [`browse_dir`] reads the real filesystem live, on every call, and
//! persists nothing — no catalog rows, no `folder` table writes, no library
//! index. It is the headless engine E08's folder-explorer UI drives one
//! directory at a time as the user clicks into subfolders; `lightbox-cli
//! browse` is its headless harness.
//!
//! Deliberately independent of [`crate::working_set`]: browsing is a pure,
//! synchronous, single-directory read (no probe, no hash, no catalog
//! registration — those only happen once the user actually *opens* what
//! they browsed to, via [`crate::working_set::plan_open`]).

use std::path::{Path, PathBuf};

use crate::pipeline::{has_known_extension, is_hidden};
use crate::IngestError;

/// One directly-contained supported image file (spec addendum v2.2).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ImageEntry {
    /// Absolute path, exactly as returned by `read_dir` (not canonicalized —
    /// this is a live listing, not an identity-bearing open).
    pub path: PathBuf,
    /// The file's name (not NFC-normalized — display only; opening it later
    /// goes through the working-set loader's own normalization).
    pub filename: String,
}

/// What one [`browse_dir`] call found: `dir`'s **immediate** children only
/// (not recursive — nested navigation means calling this again on a
/// clicked-into subdirectory).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DirListing {
    /// Child subdirectories, sorted by name. Hidden (dot-name) directories
    /// are skipped, matching the working-set loader's walk convention.
    pub subdirs: Vec<PathBuf>,
    /// Directly-contained files with a [`crate::KNOWN_EXTENSIONS`]
    /// extension, sorted by filename. Hidden files are skipped.
    pub images: Vec<ImageEntry>,
}

/// Lists `dir`'s immediate children: subfolders + directly-contained
/// supported image files (spec addendum v2.2). Live and ephemeral — every
/// call re-reads the filesystem; nothing is cached or persisted.
///
/// Symlinks are neither followed into subdirectories nor listed as image
/// files (matches [`crate::discover_files`]'s `follow_links(false)`
/// convention — avoids symlink cycles in a browse-into-subfolder UI). A
/// per-entry read error (permission denied, a racing delete) is skipped
/// silently — one bad entry never fails the whole listing; only `dir`
/// itself being missing/unreadable/not-a-directory is an [`IngestError`].
pub fn browse_dir(dir: &Path) -> Result<DirListing, IngestError> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| IngestError::InvalidSource {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    if !meta.is_dir() {
        return Err(IngestError::InvalidSource {
            path: dir.to_path_buf(),
            reason: "not a directory".to_owned(),
        });
    }

    let entries = std::fs::read_dir(dir).map_err(|e| IngestError::InvalidSource {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;

    let mut listing = DirListing::default();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if is_hidden(&name) {
            continue;
        }
        // `file_type()` here does NOT follow symlinks (unlike `metadata()`),
        // matching the walker's `follow_links(false)`.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            listing.subdirs.push(entry.path());
        } else if file_type.is_file() && has_known_extension(&entry.path()) {
            listing.images.push(ImageEntry {
                path: entry.path(),
                filename: name.to_string_lossy().into_owned(),
            });
        }
    }
    listing.subdirs.sort();
    listing.images.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn lists_immediate_children_only_sorted_and_hidden_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("b-folder")).unwrap();
        std::fs::create_dir_all(root.join("a-folder")).unwrap();
        std::fs::create_dir_all(root.join(".hidden-folder")).unwrap();
        touch(root, "z.jpg");
        touch(root, "a.cr3");
        touch(root, "notes.txt"); // unsupported extension
        touch(root, ".hidden.jpg"); // hidden file
        std::fs::create_dir_all(root.join("a-folder/nested")).unwrap();
        touch(&root.join("a-folder"), "nested.jpg"); // NOT immediate — must not appear

        let listing = browse_dir(root).unwrap();

        assert_eq!(
            listing.subdirs,
            vec![root.join("a-folder"), root.join("b-folder")],
            "subdirs sorted, hidden folder excluded, nested folder not surfaced"
        );
        let names: Vec<&str> = listing.images.iter().map(|i| i.filename.as_str()).collect();
        assert_eq!(
            names,
            vec!["a.cr3", "z.jpg"],
            "images sorted, non-photo and hidden excluded"
        );
    }

    #[test]
    fn browsing_a_subdir_returns_its_own_children() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("trip")).unwrap();
        touch(&root.join("trip"), "IMG_0001.nef");

        let top = browse_dir(root).unwrap();
        assert_eq!(top.subdirs, vec![root.join("trip")]);
        assert!(top.images.is_empty());

        let nested = browse_dir(&root.join("trip")).unwrap();
        assert!(nested.subdirs.is_empty());
        assert_eq!(nested.images.len(), 1);
        assert_eq!(nested.images[0].filename, "IMG_0001.nef");
    }

    #[test]
    fn missing_or_non_directory_path_is_an_error_not_a_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(browse_dir(&tmp.path().join("nope")).is_err());

        let file = tmp.path().join("a-file.txt");
        touch(tmp.path(), "a-file.txt");
        assert!(browse_dir(&file).is_err());
        let _ = file;
    }

    #[test]
    fn empty_directory_lists_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let listing = browse_dir(tmp.path()).unwrap();
        assert!(listing.subdirs.is_empty());
        assert!(listing.images.is_empty());
    }

    /// Nothing persists: two calls against a mutated directory reflect the
    /// live state — no stale cache (spec addendum v2.2: "live and
    /// ephemerally — it PERSISTS NOTHING").
    #[test]
    fn every_call_re_reads_the_live_filesystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        assert!(browse_dir(root).unwrap().images.is_empty());
        touch(root, "new.jpg");
        assert_eq!(browse_dir(root).unwrap().images.len(), 1);
        std::fs::remove_file(root.join("new.jpg")).unwrap();
        assert!(browse_dir(root).unwrap().images.is_empty());
    }
}
