// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `verify_store(Quick|Full)` + `PurgeScope` (E03 spec §3.2/§5.2, Phase F
//! T21).
//!
//! **`Quick`** (run at open, cheap): for every indexed row, does its file
//! exist (T2: does its directory exist)? Missing files are reported, never
//! auto-deleted — a fast, read-only health check.
//!
//! **`Full`** (idle/background or CLI): everything `Quick` does, PLUS a
//! checksum spot-check on T0/T1 rows (re-hashes the stored file against the
//! catalog's `checksum` column — a torn/corrupted file is dropped, both row
//! and file), an orphan sweep (a file/`.t2` directory under `previews/` with
//! no matching catalog row is removed), and the A-13-documented orphaned
//! `.tmp-*` sweep (`Store::sweep_orphan_temp_files`). Every destructive
//! action `Full` takes is on data this store's own contract already calls
//! disposable/reconstructible (spec §3.2) — nothing here can lose a
//! catalog-authoritative fact.

use std::collections::HashSet;
use std::sync::Mutex;

use lightbox_catalog::Catalog;

use crate::index::PreviewIndex;
use crate::pyramid::{RelPath, Tier};
use crate::store::Store;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum VerifyMode {
    /// Cheap: missing-file detection only, never mutates.
    Quick,
    /// Missing files (dropped), checksum spot-check (dropped on mismatch),
    /// orphan files/dirs removed, orphaned `.tmp-*` swept.
    Full,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub rows_checked: u64,
    /// Rows whose file (T0/T1) or directory (T2) does not exist. `Full`
    /// drops these rows; `Quick` only reports them.
    pub missing_files: u64,
    /// `Full`-only: T0/T1 rows whose stored bytes no longer match the
    /// catalog's `checksum` column — dropped (row + file).
    pub checksum_failures: u64,
    /// `Full`-only: files/directories under `previews/` with no matching
    /// catalog row — removed.
    pub orphans_removed: u64,
    /// `Full`-only: orphaned `.tmp-*` residue of a kill mid-`atomic_write`
    /// (A-13) — removed.
    pub orphan_temp_files_removed: u64,
}

/// Which cache(s) a purge targets (spec §5.6 `Command::PurgeCaches
/// (PurgeScope)`).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PurgeScope {
    Previews,
    RawCache,
    All,
}

pub(crate) fn verify_store(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    mode: VerifyMode,
) -> VerifyReport {
    let mut report = VerifyReport::default();
    let rows = catalog.reader().all_preview_rows().unwrap_or_default();
    let mut known_paths: HashSet<String> = HashSet::new();

    for row in &rows {
        report.rows_checked += 1;
        known_paths.insert(row.store_path.clone());
        let tier = Tier::from_u8(row.tier);
        let abs = store.resolve(&RelPath(row.store_path.clone()));
        let exists = if tier == Some(Tier::T2) {
            abs.is_dir()
        } else {
            abs.is_file()
        };
        if !exists {
            report.missing_files += 1;
            if mode == VerifyMode::Full {
                drop_row(catalog, index, row.id);
            }
            continue;
        }
        if mode == VerifyMode::Full && tier != Some(Tier::T2) {
            if let Ok(bytes) = std::fs::read(&abs) {
                let actual = twox_hash::XxHash3_64::oneshot(&bytes).to_le_bytes();
                if actual != row.checksum {
                    report.checksum_failures += 1;
                    drop_row(catalog, index, row.id);
                    let _ = std::fs::remove_file(&abs);
                }
            }
        }
    }

    if mode == VerifyMode::Full {
        report.orphans_removed = sweep_orphan_preview_entries(store, &known_paths);
        report.orphan_temp_files_removed =
            store.sweep_orphan_temp_files(std::time::Duration::from_secs(60));
    }
    report
}

fn drop_row(catalog: &Catalog, index: &Mutex<PreviewIndex>, id: lightbox_types::PreviewId) {
    let _ = catalog.writer().with_txn(move |txn| txn.delete_preview(id));
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(id);
}

/// Walks `previews/<hh>/*` (one level of `<hh>` fan-out, per the store's own
/// key scheme) and removes any T0/T1 FILE or `.t2` DIRECTORY not present in
/// `known` — spec §3.2 `verify_store(Full)`'s orphan half.
fn sweep_orphan_preview_entries(store: &Store, known: &HashSet<String>) -> u64 {
    let root = store.root().join("previews");
    let Ok(hh_entries) = std::fs::read_dir(&root) else {
        return 0;
    };
    let mut removed = 0u64;
    for hh in hh_entries.flatten() {
        let hh_path = hh.path();
        if !hh_path.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&hh_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(store.root()) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                // A `.t2` tile directory.
                if !known.contains(&rel_str) && std::fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                }
            } else {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with(".tmp-") {
                    continue; // handled by `sweep_orphan_temp_files`, not here
                }
                if !known.contains(&rel_str) && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PreviewStoreConfig;
    use crate::producer;
    use lightbox_types::{ContentHash, Orientation};
    use std::path::{Path, PathBuf};

    fn fixtures_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        assert!(
            dir.join("manifest.toml").exists(),
            "run `cargo xtask fixtures` first"
        );
        dir
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: Store,
        catalog: Catalog,
        index: Mutex<PreviewIndex>,
    }

    fn harness() -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let store =
            Store::open(&PreviewStoreConfig::with_defaults(dir.path().to_path_buf())).unwrap();
        let catalog = Catalog::create(&dir.path().join("t.lbdata")).unwrap();
        let index = Mutex::new(PreviewIndex::load(&catalog.reader()).unwrap());
        Harness {
            _dir: dir,
            store,
            catalog,
            index,
        }
    }

    fn build_t0(h: &Harness) -> crate::pyramid::PreviewDesc {
        let content_hash = ContentHash([44; 16]);
        let root = h
            .catalog
            .writer()
            .with_txn({
                let p = fixtures_dir();
                move |txn| txn.upsert_root(None, &p)
            })
            .unwrap();
        let folder = h
            .catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, ""))
            .unwrap();
        let asset = h
            .catalog
            .writer()
            .with_txn(move |txn| {
                let batch = vec![lightbox_catalog::NewAsset {
                    folder,
                    filename: "canon-eos-350d.cr2".to_owned(),
                    content_hash,
                    format: "CR2".to_owned(),
                    camera_make: None,
                    camera_model: None,
                    capture_time: None,
                    width: 100,
                    height: 100,
                    orientation: Orientation::O1,
                    bytes: 10,
                    mtime_utc: None,
                    decode_error: None,
                    import_session: None,
                }];
                Ok(txn.insert_assets(&batch)?.inserted[0])
            })
            .unwrap();
        let image = h
            .catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();
        producer::ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &fixtures_dir().join("canon-eos-350d.cr2"),
        )
        .unwrap()
    }

    /// T21 AC: `Quick` detects a planted missing file without mutating
    /// anything.
    #[test]
    fn quick_detects_missing_file_without_deleting_the_row() {
        let h = harness();
        let desc = build_t0(&h);
        std::fs::remove_file(h.store.resolve(&desc.store_path)).unwrap();

        let report = verify_store(&h.store, &h.catalog, &h.index, VerifyMode::Quick);
        assert_eq!(report.missing_files, 1);
        assert_eq!(
            h.catalog.reader().all_preview_rows().unwrap().len(),
            1,
            "Quick never deletes"
        );
    }

    /// T21 AC: `Full` detects a planted TORN file (checksum mismatch), an
    /// ORPHAN file (no row), and a MISSING file, each handled correctly.
    #[test]
    fn full_detects_planted_torn_orphan_and_missing_cases() {
        let h = harness();
        let torn = build_t0(&h);
        // Torn: flip a byte in the stored file so its checksum no longer matches.
        let abs = h.store.resolve(&torn.store_path);
        let mut bytes = std::fs::read(&abs).unwrap();
        bytes[0] ^= 0xFF;
        std::fs::write(&abs, &bytes).unwrap();

        // Orphan: a stray file under previews/ with no catalog row.
        let orphan_path = h
            .store
            .root()
            .join("previews")
            .join("ff")
            .join("orphan.t0.jpg");
        std::fs::create_dir_all(orphan_path.parent().unwrap()).unwrap();
        std::fs::write(&orphan_path, b"nobody points at me").unwrap();

        // Orphaned temp file, old enough to sweep.
        let tmp_path = h.store.root().join("previews").join("ff").join(".tmp-99-1");
        std::fs::write(&tmp_path, b"leftover").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        let _ = filetime_touch(&tmp_path, old);

        let report = verify_store(&h.store, &h.catalog, &h.index, VerifyMode::Full);
        assert_eq!(report.checksum_failures, 1, "{report:?}");
        assert_eq!(report.orphans_removed, 1, "{report:?}");
        assert_eq!(report.orphan_temp_files_removed, 1, "{report:?}");
        assert!(!abs.exists(), "the torn file must be dropped");
        assert!(!orphan_path.exists());
        assert!(!tmp_path.exists());
        assert_eq!(h.catalog.reader().all_preview_rows().unwrap().len(), 0);
    }

    /// Sets a file's mtime into the past without pulling in a new crate:
    /// truncate+rewrite doesn't change mtime portably enough, so this uses
    /// `std::fs::File::set_modified` (stable since Rust 1.75, well within
    /// this workspace's 1.96 MSRV).
    fn filetime_touch(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        f.set_modified(when)
    }
}
