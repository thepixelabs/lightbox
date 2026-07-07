// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! In-crate test suite for Phase 3 (T8–T12 acceptance criteria). The
//! `kill -9` fault-injection harness (T13) is an integration test —
//! `tests/fault_injection.rs` — because it re-spawns its own test binary.

mod backup_tests;
mod dao_tests;
mod edit_state_tests;
mod fts_tests;
mod open_create_tests;
mod pages_tests;
mod preview_dao_tests;
mod rawcache_dao_tests;

use std::path::Path;

use lightbox_types::{AssetId, ContentHash, FolderId, ImageId, Orientation, RootId};
use tempfile::TempDir;

use crate::{Catalog, NewAsset};

/// A fresh catalog in a tempdir. Keep the `TempDir` alive as long as the
/// catalog.
pub(crate) fn temp_catalog() -> (TempDir, Catalog) {
    let dir = TempDir::new().expect("tempdir");
    let lbdata = dir.path().join("test.lbdata");
    let catalog = Catalog::create(&lbdata).expect("create catalog");
    (dir, catalog)
}

/// Deterministic distinct content hash from a seed.
pub(crate) fn hash(seed: u64) -> ContentHash {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[8..].copy_from_slice(&(!seed).to_le_bytes());
    ContentHash(bytes)
}

/// A minimal plausible `NewAsset`.
pub(crate) fn new_asset(folder: FolderId, name: &str, seed: u64) -> NewAsset {
    NewAsset {
        folder,
        filename: name.to_owned(),
        content_hash: hash(seed),
        format: "JPEG".to_owned(),
        camera_make: None,
        camera_model: None,
        capture_time: None,
        width: 640,
        height: 480,
        orientation: Orientation::O1,
        bytes: 1024,
        mtime_utc: None,
        decode_error: None,
        import_session: None,
    }
}

/// One seeded asset + its default image (the common single-fixture case used
/// by DAO acceptance tests that don't need a whole batch).
pub(crate) fn one_image(catalog: &Catalog, dir: &Path) -> (AssetId, ImageId) {
    let (_root, folder) = seed_folder(catalog, dir);
    seed_assets(catalog, folder, "e", 1, 0, &[])[0]
}

/// Root + root folder + one subfolder, ready for asset inserts.
pub(crate) fn seed_folder(catalog: &Catalog, root_path: &Path) -> (RootId, FolderId) {
    let root_path = root_path.to_path_buf();
    catalog
        .writer()
        .with_txn(move |txn| {
            let root = txn.upsert_root(None, &root_path)?;
            let folder = txn.upsert_folder(root, None, "shoot")?;
            Ok((root, folder))
        })
        .expect("seed folder")
}

/// Inserts `n` assets (+ default images) named `f{i:05}-<tag>.jpg` with the
/// given capture times (cycled). Returns (asset, image) id pairs in insert
/// order.
pub(crate) fn seed_assets(
    catalog: &Catalog,
    folder: FolderId,
    tag: &str,
    n: u64,
    seed_base: u64,
    capture_times: &[Option<&str>],
) -> Vec<(AssetId, ImageId)> {
    let tag = tag.to_owned();
    let captures: Vec<Option<String>> =
        capture_times.iter().map(|c| c.map(str::to_owned)).collect();
    catalog
        .writer()
        .with_txn(move |txn| {
            let mut batch = Vec::new();
            for i in 0..n {
                let mut a = new_asset(folder, &format!("f{i:05}-{tag}.jpg"), seed_base + i);
                if !captures.is_empty() {
                    a.capture_time = captures[(i as usize) % captures.len()].clone();
                }
                batch.push(a);
            }
            let outcome = txn.insert_assets(&batch)?;
            assert_eq!(outcome.skipped_duplicates, 0, "seed hashes must be unique");
            let images = txn.insert_default_images(&outcome.inserted)?;
            Ok(outcome
                .inserted
                .iter()
                .copied()
                .zip(images)
                .collect::<Vec<_>>())
        })
        .expect("seed assets")
}
