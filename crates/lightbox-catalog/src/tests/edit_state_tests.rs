// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E09 Phase B (T5-DAOs/T6) acceptance criteria: the edit-state write DAOs
//! and their matching `ReaderHandle` read surface.

use lightbox_types::{AssetId, ProcessVersion, PV_M0};
use rusqlite::params;

use super::{seed_assets, seed_folder, temp_catalog};
use crate::CatalogError;

fn one_image(
    catalog: &crate::Catalog,
    dir: &std::path::Path,
) -> (AssetId, lightbox_types::ImageId) {
    let (_root, folder) = seed_folder(catalog, dir);
    seed_assets(catalog, folder, "e", 1, 0, &[])[0]
}

#[test]
fn upsert_edit_recipe_enforces_pv_immutability() {
    let (dir, catalog) = temp_catalog();
    let (_asset, image) = one_image(&catalog, dir.path());

    // A crafted violation: image.process_version is PV_M0 (1); write pv=2.
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_edit_recipe(image, ProcessVersion(2), 1, b"doc-bytes", 0))
        .unwrap_err();
    assert!(matches!(err, CatalogError::InvalidArg(_)), "{err:?}");

    // The correct pv writes cleanly and round-trips.
    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_edit_recipe(image, PV_M0, 1, b"doc-bytes", 0))
        .unwrap();
    let row = catalog.reader().edit_state_row(image).unwrap().unwrap();
    assert_eq!(row.pv, PV_M0);
    assert_eq!(row.doc, b"doc-bytes");
    assert_eq!(row.head_seq, 0);

    // A second write with a different pv is STILL rejected (the row exists
    // now, but the check is against image.process_version, which never
    // moved) — the ON CONFLICT clause also never writes the pv column.
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_edit_recipe(image, ProcessVersion(9), 1, b"x", 1))
        .unwrap_err();
    assert!(matches!(err, CatalogError::InvalidArg(_)), "{err:?}");
    assert_eq!(
        catalog.reader().edit_state_row(image).unwrap().unwrap().pv,
        PV_M0,
        "pv must not have moved"
    );
}

#[test]
fn edit_state_row_is_none_until_first_write_d1() {
    let (dir, catalog) = temp_catalog();
    let (_asset, image) = one_image(&catalog, dir.path());
    // D1: no row for an untouched image.
    assert_eq!(catalog.reader().edit_state_row(image).unwrap(), None);
}

#[test]
fn append_truncate_and_replay_range_round_trip() {
    let (dir, catalog) = temp_catalog();
    let (_asset, image) = one_image(&catalog, dir.path());

    catalog
        .writer()
        .with_txn(move |txn| {
            for seq in 1..=5u64 {
                let kf = if seq % 3 == 0 {
                    Some(format!("keyframe-{seq}").into_bytes())
                } else {
                    None
                };
                txn.append_history_step(
                    image,
                    seq,
                    format!("op-{seq}").as_bytes(),
                    format!("delta-{seq}").as_bytes(),
                    format!("inverse-{seq}").as_bytes(),
                    kf.as_deref(),
                )?;
            }
            txn.upsert_edit_recipe(image, PV_M0, 1, b"doc-at-5", 5)
        })
        .unwrap();

    let reader = catalog.reader();
    let page = reader.history_page(image, None, 10).unwrap();
    assert_eq!(page.len(), 5);
    assert_eq!(page[0].seq, 5, "newest-first");
    assert_eq!(page[4].seq, 1);

    // Nearest keyframe at-or-before seq=5 is seq=3.
    let range = reader.history_replay_range(image, 5).unwrap();
    assert_eq!(range.anchor.as_ref().map(|(s, _)| *s), Some(3));
    assert_eq!(
        range.deltas.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![4, 5]
    );

    // seq=2: no keyframe at or before it (the first is at 3) -> anchor None,
    // replay from neutral over deltas 1..=2.
    let range2 = reader.history_replay_range(image, 2).unwrap();
    assert_eq!(range2.anchor, None);
    assert_eq!(
        range2.deltas.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![1, 2]
    );
    drop(reader);

    // Truncate after seq=2: steps 3..=5 go away.
    let removed = catalog
        .writer()
        .with_txn(move |txn| txn.truncate_history_after(image, 2))
        .unwrap();
    assert_eq!(removed, 3);
    assert_eq!(
        catalog
            .reader()
            .history_page(image, None, 10)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(catalog.reader().latest_history_seq(image).unwrap(), Some(2));
}

#[test]
fn rebuild_edit_index_upserts_and_reads_back() {
    let (dir, catalog) = temp_catalog();
    let (_asset, image) = one_image(&catalog, dir.path());

    // Untouched: edit_badges default (D1).
    let badges = catalog.reader().edit_badges(&[image]).unwrap();
    assert_eq!(badges.len(), 1);
    assert!(!badges[0].is_edited);
    assert_eq!(badges[0].crop_ratio, None);
    assert_eq!(badges[0].treatment, None);

    catalog
        .writer()
        .with_txn(move |txn| {
            txn.rebuild_edit_index(image, true, true, false, Some(1.5), Some("color"))
        })
        .unwrap();
    let badges = catalog.reader().edit_badges(&[image]).unwrap();
    assert!(badges[0].is_edited);
    assert!(badges[0].has_masks);
    assert!(!badges[0].has_ai_mask);
    assert_eq!(badges[0].crop_ratio, Some(1.5));
    assert_eq!(badges[0].treatment.as_deref(), Some("color"));

    // Idempotent upsert (second rebuild for the same image updates in place).
    catalog
        .writer()
        .with_txn(move |txn| txn.rebuild_edit_index(image, false, false, false, None, Some("bw")))
        .unwrap();
    let badges = catalog.reader().edit_badges(&[image]).unwrap();
    assert!(!badges[0].is_edited);
    assert_eq!(badges[0].treatment.as_deref(), Some("bw"));
    assert_eq!(catalog.reader().counts().unwrap().images, 1);
}

#[test]
fn snapshot_crud_and_name_uniqueness() {
    let (dir, catalog) = temp_catalog();
    let (_asset, image) = one_image(&catalog, dir.path());

    let id = catalog
        .writer()
        .with_txn(move |txn| txn.insert_snapshot(image, "v1", b"doc-v1"))
        .unwrap();

    let snaps = catalog.reader().snapshots(image).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].id, id);
    assert_eq!(snaps[0].name, "v1");
    assert_eq!(snaps[0].recipe_doc, b"doc-v1");

    // Duplicate name on the same image -> typed Constraint error.
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.insert_snapshot(image, "v1", b"doc-again"))
        .unwrap_err();
    assert!(matches!(err, CatalogError::Constraint(_)), "{err:?}");

    // Rename then delete.
    catalog
        .writer()
        .with_txn(move |txn| txn.rename_snapshot(id, "v1-renamed"))
        .unwrap();
    assert_eq!(
        catalog.reader().snapshots(image).unwrap()[0].name,
        "v1-renamed"
    );
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_snapshot(id))
        .unwrap();
    assert!(catalog.reader().snapshots(image).unwrap().is_empty());

    // Deleting/renaming an unknown snapshot is NotFound.
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.delete_snapshot(id))
        .unwrap_err();
    assert!(matches!(err, CatalogError::NotFound { .. }));
}

#[test]
fn xmp_sync_write_and_read_stamps() {
    let (dir, catalog) = temp_catalog();
    let (asset, _image) = one_image(&catalog, dir.path());

    assert_eq!(catalog.reader().xmp_sync_row(asset).unwrap(), None);

    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_xmp_sync_write(asset, b"hash-1", "2026-01-01", b"rhash-1"))
        .unwrap();
    let row = catalog.reader().xmp_sync_row(asset).unwrap().unwrap();
    assert_eq!(row.sidecar_hash.as_deref(), Some(&b"hash-1"[..]));
    assert!(row.last_written_at.is_some());
    assert_eq!(row.last_read_at, None);

    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_xmp_sync_read(asset, b"hash-2", "2026-01-02", b"rhash-2"))
        .unwrap();
    let row = catalog.reader().xmp_sync_row(asset).unwrap().unwrap();
    assert_eq!(row.sidecar_hash.as_deref(), Some(&b"hash-2"[..]));
    assert!(row.last_read_at.is_some());
    // last_written_at survives the read-stamp upsert (still set from before).
    assert!(row.last_written_at.is_some());
}

#[test]
fn image_for_content_hash_resolves_across_a_simulated_move() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let pairs = seed_assets(&catalog, folder, "mover", 1, 777, &[]);
    let (asset, image) = pairs[0];
    let hash = super::hash(777);

    assert_eq!(
        catalog.reader().image_for_content_hash(hash).unwrap(),
        Some(image)
    );

    // Simulate a file move: new folder + filename, same content_hash. There
    // is no product-level "move" DAO yet (E04 loader territory) so the test
    // uses the crate-internal `raw()` escape hatch to mutate the asset row
    // directly — `image_for_content_hash` must still resolve by hash alone.
    catalog
        .writer()
        .with_txn(move |txn| {
            txn.raw().execute(
                "UPDATE asset SET filename = ?1 WHERE id = ?2",
                params!["moved-elsewhere.jpg", asset.0],
            )?;
            Ok(())
        })
        .unwrap();

    assert_eq!(
        catalog.reader().image_for_content_hash(hash).unwrap(),
        Some(image),
        "identity follows content_hash, not path"
    );
    assert_eq!(
        catalog
            .reader()
            .image_for_content_hash(super::hash(999_999))
            .unwrap(),
        None
    );
}

#[test]
fn fk_cascade_delete_image_removes_edit_rows() {
    let (dir, catalog) = temp_catalog();
    let (asset, image) = one_image(&catalog, dir.path());

    catalog
        .writer()
        .with_txn(move |txn| {
            txn.upsert_edit_recipe(image, PV_M0, 1, b"doc", 1)?;
            txn.append_history_step(image, 1, b"op", b"delta", b"inverse", None)?;
            txn.rebuild_edit_index(image, true, false, false, None, Some("color"))?;
            txn.insert_snapshot(image, "s1", b"snap-doc")?;
            txn.upsert_xmp_sync_write(asset, b"h", "2026-01-01", b"rh")?;
            Ok(())
        })
        .unwrap();

    // Sanity: everything is there before the delete.
    assert!(catalog.reader().edit_state_row(image).unwrap().is_some());
    assert_eq!(
        catalog
            .reader()
            .history_page(image, None, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(catalog.reader().snapshots(image).unwrap().len(), 1);
    assert!(catalog.reader().edit_badges(&[image]).unwrap()[0].is_edited);

    catalog
        .writer()
        .with_txn(move |txn| {
            txn.raw()
                .execute("DELETE FROM image WHERE id = ?1", params![image.0])?;
            Ok(())
        })
        .unwrap();

    let reader = catalog.reader();
    assert_eq!(
        reader.edit_state_row(image).unwrap(),
        None,
        "edit_recipe cascaded"
    );
    assert!(
        reader.history_page(image, None, 10).unwrap().is_empty(),
        "history_step cascaded"
    );
    assert!(
        reader.snapshots(image).unwrap().is_empty(),
        "snapshot cascaded"
    );
    assert!(
        !reader.edit_badges(&[image]).unwrap()[0].is_edited,
        "edit_index cascaded (falls back to D1 default)"
    );
    // xmp_sync is keyed by asset, not image — it survives an image delete and
    // cascades only when the asset itself is removed.
    assert!(reader.xmp_sync_row(asset).unwrap().is_some());
    drop(reader);

    catalog
        .writer()
        .with_txn(move |txn| {
            txn.raw()
                .execute("DELETE FROM asset WHERE id = ?1", params![asset.0])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        catalog.reader().xmp_sync_row(asset).unwrap(),
        None,
        "xmp_sync cascaded on asset delete"
    );
}
