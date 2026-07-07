// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The T0 write path (E03 spec §3.1/§5.3, Phase B T07): turns a
//! [`crate::extract::T0Extract`] into a store file + catalog row, with
//! asset-scope dedupe on both sides.
//!
//! **Dedupe, twice over (T07 AC).**
//! 1. *Store file:* [`crate::pyramid::derive_store_key`] is a pure function
//!    of `(content_hash, scope, tier, variant)` — two calls for the same
//!    asset always land on the same relative path, so [`ensure_t0`] checks
//!    "does this file already exist" before writing (mirrors
//!    [`crate::BlobStore::put`]'s idempotence). Re-extracting an
//!    already-stored asset (a re-import, or a second virtual copy) writes
//!    **zero** new bytes to disk.
//! 2. *Catalog row:* `CatalogTxn::upsert_preview`'s conflict target is
//!    `(asset_id, tier, variant_hash) WHERE image_id IS NULL` (migration
//!    0004) — an upsert, not an insert, so calling this twice for the same
//!    asset never duplicates the row.
//!
//! Both dedupe paths are keyed on **`asset`, not `image`**: T0 is asset-scope
//! (spec §3.1), so two virtual copies (`image_a`/`image_b`) that share one
//! `asset` calling [`ensure_t0`] independently converge on the exact same
//! store file and catalog row — the second call's fast-path (below) usually
//! short-circuits before either dedupe mechanism above is even exercised.
//!
//! **Fast path.** Before touching disk or the catalog writer at all,
//! [`ensure_t0`] consults the in-RAM [`PreviewIndex`] (Phase A, T04) —
//! `best_available` is a synchronous, IO-free lookup — so a warm index
//! (the common case: index is hydrated once at session open) resolves an
//! already-built T0 without any extraction work.

use std::path::Path;
use std::sync::Mutex;

use lightbox_catalog::{Catalog, NewPreviewRow, PreviewSourceTag};
use lightbox_types::{AssetId, ContentHash, ImageId};

use crate::extract::extract_largest_embedded;
use crate::index::PreviewIndex;
use crate::pyramid::{
    derive_store_key, t0_rel_path, PreviewDesc, PreviewScope, ProducerId, Tier, VariantParams,
};
use crate::store::{atomic_write, Store};
use crate::{config::Codec, PreviewError};

/// The fixed [`VariantParams`] for every T0 row (spec §3.1: T0 has no
/// resize/quality/recipe knobs — it is the source bytes, verbatim).
/// `long_edge_px = 0` / `quality = 0` both mean "not applicable, verbatim"
/// (the spec states this convention for `long_edge_px`; `quality` follows
/// the same reading here since a byte-copy has no quality setting of its
/// own to record).
fn t0_variant_params() -> VariantParams {
    VariantParams {
        enc_ver: crate::pyramid::VARIANT_PARAMS_ENC_VER,
        producer: ProducerId::EMBEDDED,
        producer_rev: 1,
        long_edge_px: 0,
        codec: Codec::Jpeg, // T0 is always verbatim JPEG (spec §3.1)
        quality: 0,
        recipe_rev: 0, // edit-independent (spec §3.1: embedded rows never stale)
        process_version: 0,
    }
}

/// Ensures `asset`'s T0 (largest embedded preview) exists in the store +
/// catalog, extracting and writing it if this is the first call for this
/// asset, and returns its descriptor either way (spec §3.1/T07).
///
/// `image` is only used to shape the [`PreviewIndex`] fast-path lookup (T0
/// is asset-scope, but `PreviewIndex::best_available` needs an `image` key
/// to search `by_image` in addition to `by_asset` — see `index.rs`'s module
/// doc comment on that deviation); the row this function writes never
/// carries an `image_id`.
///
/// `Err(PreviewError::NoEmbedded)` when the asset has no usable embedded
/// preview (T06) — never written to the store or catalog, never cached as a
/// negative result (a later probe of the same asset, e.g. after a sidecar
/// tool re-embeds a preview, gets a fresh chance).
pub(crate) fn ensure_t0(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    asset: AssetId,
    content_hash: ContentHash,
    path: &Path,
) -> Result<PreviewDesc, PreviewError> {
    // Fast path: no IO at all if the index already has T0 for this asset.
    if let Some(desc) = lock(index).best_available(image, asset, 0) {
        if desc.tier == Tier::T0 {
            return Ok(desc);
        }
    }

    let extracted = extract_largest_embedded(path)?;
    let params = t0_variant_params();
    let variant = params.variant_hash();
    let key = derive_store_key(content_hash, PreviewScope::Asset(asset), Tier::T0, variant);
    let rel = t0_rel_path(key);
    let abs = store.resolve(&rel);
    if !abs.exists() {
        // Content-addressed: if a file is already at this exact path, it is
        // byte-identical to `extracted.jpeg` by construction (the key is a
        // pure function of content_hash+scope+tier+variant) — no need to
        // read it back and compare, matching `BlobStore::put`'s convention.
        atomic_write(&abs, &extracted.jpeg)?;
    }
    let checksum = twox_hash::XxHash3_64::oneshot(&extracted.jpeg).to_le_bytes();

    let row = NewPreviewRow {
        asset,
        image: None, // T0 is asset-scope (spec §3.1)
        content_hash,
        tier: Tier::T0 as u8,
        variant_hash: variant.to_le_bytes(), // A-7: catalog column is LE
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        colorspace: match extracted.colorspace {
            crate::pyramid::PreviewColorspace::Srgb => "srgb".to_owned(),
            crate::pyramid::PreviewColorspace::TaggedIcc => "icc".to_owned(),
        },
        store_path: rel.as_str().to_owned(),
        width: extracted.width,
        height: extracted.height,
        bytes: extracted.jpeg.len() as u64,
        checksum,
    };

    let id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(row))
        .map_err(|e| PreviewError::Catalog(e.to_string()))?;
    let full_row = catalog
        .reader()
        .preview_row(id)
        .map_err(|e| PreviewError::Catalog(e.to_string()))?
        .ok_or_else(|| {
            PreviewError::Catalog(format!("preview row {} vanished right after upsert", id.0))
        })?;

    let mut index = lock(index);
    index.upsert(&full_row);
    index
        .best_available(image, asset, 0)
        .ok_or_else(|| PreviewError::Catalog("preview row missing from index post-upsert".into()))
}

fn lock(index: &Mutex<PreviewIndex>) -> std::sync::MutexGuard<'_, PreviewIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PreviewStoreConfig;
    use lightbox_types::Orientation;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        assert!(
            dir.join("manifest.toml").exists(),
            "fixture corpus missing at {} — run `cargo xtask fixtures` first",
            dir.display()
        );
        dir
    }

    /// A fresh store + catalog + hydrated (empty) index, all rooted in one
    /// tempdir — the fixture every T07 test shares.
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

    /// Inserts one asset (with a real content hash of `path`'s bytes) and
    /// returns its `AssetId` — enough scaffolding for `ensure_t0`, without
    /// pulling in a real folder/import flow.
    fn insert_asset(catalog: &Catalog, filename: &str, content_hash: ContentHash) -> AssetId {
        // A synthetic root path is enough: these tests pass the real fixture
        // path to `ensure_t0` directly, never resolving it back through the
        // catalog (`asset_abs_path`), so nothing here needs to exist on disk.
        let root = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_root(None, Path::new("/synthetic-root")))
            .unwrap();
        let folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, "shoot"))
            .unwrap();
        catalog
            .writer()
            .with_txn({
                let filename = filename.to_owned();
                move |txn| {
                    let batch = vec![lightbox_catalog::NewAsset {
                        folder,
                        filename,
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
                }
            })
            .unwrap()
    }

    /// T07 AC: extracting T0 writes the expected store file + a
    /// `source='embedded'` index row at the §3.1 key scheme path.
    #[test]
    fn ensure_t0_writes_the_store_file_and_index_row() {
        let h = harness();
        let content_hash = ContentHash([3; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image = ImageId(1);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let desc = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(desc.tier, Tier::T0);
        assert_eq!(desc.source, crate::pyramid::PreviewSource::Embedded);
        assert_eq!((desc.width, desc.height), (3456, 2304));
        assert!(desc.store_path.as_str().starts_with("previews/"));
        assert!(desc.store_path.as_str().ends_with(".t0.jpg"));
        let abs = h.store.resolve(&desc.store_path);
        assert!(abs.is_file(), "T0 file must exist on disk");
        assert!(std::fs::metadata(&abs).unwrap().len() > 0);
    }

    /// T07 AC: re-import of an identical file (same asset, called again)
    /// creates ZERO new store files.
    #[test]
    fn reextraction_of_the_same_asset_writes_no_new_files() {
        let h = harness();
        let content_hash = ContentHash([4; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let first = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        let preview_dir = h.store.root().join("previews");
        let count_before = count_files(&preview_dir);
        assert_eq!(count_before, 1, "exactly one T0 file after the first call");

        let second = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(first.store_path, second.store_path);
        assert_eq!(count_files(&preview_dir), count_before, "no new files");

        // Drop the RAM index's memory of this and force the on-disk-check
        // path too (simulates a cold-index re-import racing a warm file).
        h.index.lock().unwrap().remove(
            h.catalog
                .reader()
                .preview_lookup(asset, None, Tier::T0 as u8, first.variant.to_le_bytes())
                .unwrap()
                .unwrap()
                .id,
        );
        let third = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(first.store_path, third.store_path);
        assert_eq!(
            count_files(&preview_dir),
            count_before,
            "still no new files"
        );
    }

    /// T07 AC: two virtual copies over one asset share one T0 row/file.
    #[test]
    fn virtual_copies_of_one_asset_share_one_t0_row_and_file() {
        let h = harness();
        let content_hash = ContentHash([5; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let copy_a = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(10),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        let copy_b = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(11),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(copy_a.store_path, copy_b.store_path);
        assert_eq!(copy_a.variant, copy_b.variant);
        assert_eq!(count_files(&h.store.root().join("previews")), 1);
        assert_eq!(h.index.lock().unwrap().len(), 1, "one row, not two");
    }

    /// T06/T07: a raw with no usable embedded preview never writes a file or
    /// row, and reports the typed `NoEmbedded` error.
    #[test]
    fn no_embedded_preview_writes_nothing() {
        let h = harness();
        let content_hash = ContentHash([6; 16]);
        let asset = insert_asset(&h.catalog, "sigma-fp.dng", content_hash);
        let path = fixtures_dir().join("sigma-fp.dng");

        let err = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap_err();
        assert!(matches!(err, PreviewError::NoEmbedded), "{err:?}");
        assert!(
            !h.store.root().join("previews").join("00").exists() || {
                count_files(&h.store.root().join("previews")) == 0
            }
        );
        assert_eq!(h.index.lock().unwrap().len(), 0);
    }

    fn count_files(root: &Path) -> usize {
        fn walk(p: &Path, n: &mut usize) {
            let Ok(entries) = std::fs::read_dir(p) else {
                return;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, n);
                } else {
                    *n += 1;
                }
            }
        }
        let mut n = 0;
        walk(root, &mut n);
        n
    }
}
