// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T9 acceptance criteria (root/folder DAOs: FK enforcement, upsert
//! idempotence, the `''` root-folder convention) and the T10/T18-facing
//! asset/image/import-session DAOs.

use std::path::Path;

use lightbox_types::{AssetId, Flag, FolderId, ImageId, ImportSessionId, RootId};

use super::{new_asset, seed_assets, seed_folder, temp_catalog};
use crate::{CatalogError, ImageQuery, SortOrder};

#[test]
fn upsert_root_is_idempotent_including_null_uuid() {
    let (_dir, catalog) = temp_catalog();
    let (a, b, c, d) = catalog
        .writer()
        .with_txn(|txn| {
            let a = txn.upsert_root(None, Path::new("/photos"))?;
            let b = txn.upsert_root(None, Path::new("/photos"))?; // NULL uuid dedupe
            let c = txn.upsert_root(Some("vol-1"), Path::new("/photos"))?;
            let d = txn.upsert_root(Some("vol-1"), Path::new("/photos"))?;
            Ok((a, b, c, d))
        })
        .unwrap();
    assert_eq!(a, b, "NULL-uuid upsert must not duplicate");
    assert_eq!(c, d);
    assert_ne!(a, c, "distinct volume_uuid is a distinct root");
    assert_eq!(catalog.reader().counts().unwrap().roots, 2);
}

#[test]
fn upsert_folder_creates_parent_chain_and_is_idempotent() {
    let (_dir, catalog) = temp_catalog();
    let ids = catalog
        .writer()
        .with_txn(|txn| {
            let root = txn.upsert_root(None, Path::new("/photos"))?;
            let deep = txn.upsert_folder(root, None, "2026/07/beach")?;
            let deep_again = txn.upsert_folder(root, None, "2026/07/beach")?;
            let mid = txn.upsert_folder(root, None, "2026/07")?;
            let top = txn.upsert_folder(root, None, "2026")?;
            let root_folder = txn.upsert_folder(root, None, "")?;
            Ok((root, deep, deep_again, mid, top, root_folder))
        })
        .unwrap();
    let (_root, deep, deep_again, _mid, _top, _root_folder) = ids;
    assert_eq!(deep, deep_again);

    let tree = catalog.reader().folder_tree().unwrap();
    let paths: Vec<&str> = tree.iter().map(|f| f.rel_path.as_str()).collect();
    // Chain creation: '' (the root-folder convention), then each ancestor.
    assert_eq!(paths, vec!["", "2026", "2026/07", "2026/07/beach"]);
    // Parent links are the chain.
    assert_eq!(tree[0].parent, None);
    assert_eq!(tree[1].parent, Some(tree[0].id));
    assert_eq!(tree[2].parent, Some(tree[1].id));
    assert_eq!(tree[3].parent, Some(tree[2].id));
    assert_eq!(tree[3].name, "beach");
    assert_eq!(tree[0].name, "");
}

#[test]
fn upsert_folder_rejects_bad_rel_paths_and_bogus_root() {
    let (_dir, catalog) = temp_catalog();
    catalog
        .writer()
        .with_txn(|txn| {
            let root = txn.upsert_root(None, Path::new("/p"))?;
            for bad in ["/abs", "trail/", "a//b", "a/./b", "a/../b"] {
                match txn.upsert_folder(root, None, bad) {
                    Err(CatalogError::InvalidArg(_)) => {}
                    other => panic!("expected InvalidArg for {bad:?}, got {other:?}"),
                }
            }
            // FK enforcement (spec §4.2 foreign_keys=ON).
            match txn.upsert_folder(RootId(9999), None, "x") {
                Err(CatalogError::Constraint(_)) => {}
                other => panic!("expected Constraint, got {other:?}"),
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn insert_assets_skips_duplicate_hashes_globally_and_within_batch() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());

    let outcome = catalog
        .writer()
        .with_txn(move |txn| {
            txn.insert_assets(&[
                new_asset(folder, "a.jpg", 1),
                new_asset(folder, "b.jpg", 2),
                new_asset(folder, "b-copy.jpg", 2), // intra-batch dup hash
            ])
        })
        .unwrap();
    assert_eq!(outcome.inserted.len(), 2);
    assert_eq!(outcome.skipped_duplicates, 1);

    // Re-import of the same content: all skipped (spec §5 T17 semantics).
    let outcome = catalog
        .writer()
        .with_txn(move |txn| {
            txn.insert_assets(&[
                new_asset(folder, "a-elsewhere.jpg", 1),
                new_asset(folder, "b-elsewhere.jpg", 2),
            ])
        })
        .unwrap();
    assert_eq!(outcome.inserted.len(), 0);
    assert_eq!(outcome.skipped_duplicates, 2);
    assert_eq!(catalog.reader().counts().unwrap().assets, 2);

    // Same folder + filename with different content is a constraint error
    // (UNIQUE(folder_id, filename)) — not silently skipped.
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.insert_assets(&[new_asset(folder, "a.jpg", 42)]))
        .unwrap_err();
    assert!(matches!(err, CatalogError::Constraint(_)), "{err:?}");
}

#[test]
fn insert_default_images_enforces_fk_and_creates_one_per_asset() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let pairs = seed_assets(&catalog, folder, "img", 3, 0, &[]);
    assert_eq!(pairs.len(), 3);
    assert_eq!(catalog.reader().counts().unwrap().images, 3);

    let err = catalog
        .writer()
        .with_txn(|txn| txn.insert_default_images(&[AssetId(12345)]))
        .unwrap_err();
    assert!(matches!(err, CatalogError::Constraint(_)), "{err:?}");
}

#[test]
fn rating_flag_and_decode_error_daos() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let pairs = seed_assets(&catalog, folder, "r", 1, 0, &[]);
    let (asset, image) = pairs[0];

    catalog
        .writer()
        .with_txn(move |txn| {
            txn.set_rating(image, Some(5))?;
            txn.set_flag(image, Flag::Pick)?;
            txn.mark_decode_error(asset, "truncated file")?;
            Ok(())
        })
        .unwrap();

    let reader = catalog.reader();
    let detail = reader.image_detail(image).unwrap();
    assert_eq!(detail.rating, Some(5));
    assert_eq!(detail.flag, Flag::Pick);
    assert_eq!(detail.decode_error.as_deref(), Some("truncated file"));

    // Summary row carries the badge bit.
    let page = reader
        .images_page(&ImageQuery {
            folder: Some(folder),
            sort: SortOrder::FilenameAsc,
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert!(page.items[0].decode_error);
    drop(reader);

    // Clearing + validation + NotFound.
    catalog
        .writer()
        .with_txn(move |txn| txn.set_rating(image, None))
        .unwrap();
    assert_eq!(
        catalog.reader().image_detail(image).unwrap().rating,
        None,
        "rating cleared"
    );
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.set_rating(image, Some(6)))
        .unwrap_err();
    assert!(matches!(err, CatalogError::InvalidArg(_)));
    let err = catalog
        .writer()
        .with_txn(|txn| txn.set_rating(ImageId(777_777), Some(3)))
        .unwrap_err();
    assert!(matches!(
        err,
        CatalogError::NotFound {
            entity: "image",
            ..
        }
    ));
}

#[test]
fn import_session_bracket_and_undo() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());

    // Pre-existing content that must survive the undo.
    seed_assets(&catalog, folder, "keep", 2, 100, &[]);

    let session = catalog
        .writer()
        .with_txn(|txn| txn.begin_import_session("/Volumes/CARD", "{}"))
        .unwrap();
    let inserted = catalog
        .writer()
        .with_txn(move |txn| {
            let mut batch = Vec::new();
            for i in 0..5u64 {
                let mut a = new_asset(folder, &format!("card{i}.jpg"), 200 + i);
                a.import_session = Some(session);
                batch.push(a);
            }
            let outcome = txn.insert_assets(&batch)?;
            txn.insert_default_images(&outcome.inserted)?;
            txn.finish_import_session(session, "{\"imported\":5}")?;
            Ok(outcome.inserted)
        })
        .unwrap();
    assert_eq!(inserted.len(), 5);

    let before = catalog.reader().counts().unwrap();
    assert_eq!(
        (before.assets, before.images, before.import_sessions),
        (7, 7, 1)
    );

    let removed = catalog
        .writer()
        .with_txn(move |txn| txn.remove_import_session(session))
        .unwrap();
    assert_eq!(removed.assets, 5);
    assert_eq!(removed.images, 5, "cascade must remove the image rows");

    let after = catalog.reader().counts().unwrap();
    assert_eq!(
        (after.assets, after.images, after.import_sessions),
        (2, 2, 0)
    );

    // Undo of an unknown session.
    let err = catalog
        .writer()
        .with_txn(|txn| txn.remove_import_session(ImportSessionId(4242)))
        .unwrap_err();
    assert!(matches!(err, CatalogError::NotFound { .. }));
}

#[test]
fn asset_abs_path_composes_root_folder_filename() {
    let (_dir, catalog) = temp_catalog();
    let asset = catalog
        .writer()
        .with_txn(|txn| {
            let root = txn.upsert_root(None, Path::new("/photos/main"))?;
            let folder = txn.upsert_folder(root, None, "2026/07")?;
            let root_folder = txn.upsert_folder(root, None, "")?;
            let a = txn.insert_assets(&[new_asset(folder, "IMG_0001.CR3", 1)])?;
            let b = txn.insert_assets(&[new_asset(root_folder, "loose.jpg", 2)])?;
            Ok((a.inserted[0], b.inserted[0]))
        })
        .unwrap();

    let reader = catalog.reader();
    assert_eq!(
        reader.asset_abs_path(asset.0).unwrap(),
        Path::new("/photos/main/2026/07/IMG_0001.CR3")
    );
    // '' root folder: no empty path segment sneaks in.
    assert_eq!(
        reader.asset_abs_path(asset.1).unwrap(),
        Path::new("/photos/main/loose.jpg")
    );
    let err = reader.asset_abs_path(AssetId(999_999)).unwrap_err();
    assert!(matches!(err, CatalogError::NotFound { .. }));
}

#[test]
fn non_utf8_root_path_is_rejected() {
    // Non-UTF-8 paths are representable on Unix only; the M0 policy (OQ-3)
    // is a per-file error, which at the DAO level is NonUtf8Path.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(b"/photos/\xFF\xFE");
        let (_dir, catalog) = temp_catalog();
        let err = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_root(None, Path::new(bad)))
            .unwrap_err();
        assert!(matches!(err, CatalogError::NonUtf8Path(_)), "{err:?}");
    }
}

#[test]
fn image_detail_round_trips_asset_metadata() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let image = catalog
        .writer()
        .with_txn(move |txn| {
            let mut a = new_asset(folder, "detail.NEF", 7);
            a.format = "NEF".to_owned();
            a.camera_make = Some("Nikon".to_owned());
            a.camera_model = Some("Z8".to_owned());
            a.capture_time = Some("2026-06-01T12:00:00.000000Z".to_owned());
            a.orientation = lightbox_types::Orientation::O6;
            a.bytes = 45_000_000;
            let outcome = txn.insert_assets(&[a])?;
            Ok(txn.insert_default_images(&outcome.inserted)?[0])
        })
        .unwrap();

    let detail = catalog.reader().image_detail(image).unwrap();
    assert_eq!(detail.format, "NEF");
    assert_eq!(detail.camera_make.as_deref(), Some("Nikon"));
    assert_eq!(detail.camera_model.as_deref(), Some("Z8"));
    assert_eq!(detail.orientation, lightbox_types::Orientation::O6);
    assert_eq!(detail.bytes, 45_000_000);
    assert_eq!(detail.process_version, lightbox_types::PV_M0);
    assert!(!detail.is_virtual);
    assert!(!detail.missing);

    let err = catalog.reader().image_detail(ImageId(31337)).unwrap_err();
    assert!(matches!(err, CatalogError::NotFound { .. }));
}

#[test]
fn folder_id_type_is_not_confusable() {
    // Guard against the classic id-mixup: inserting an asset into a folder id
    // that doesn't exist is an FK error, not silent data corruption.
    let (_dir, catalog) = temp_catalog();
    let err = catalog
        .writer()
        .with_txn(|txn| txn.insert_assets(&[new_asset(FolderId(555), "x.jpg", 0xDEAD_BEEF)]))
        .unwrap_err();
    assert!(matches!(err, CatalogError::Constraint(_)));
}
