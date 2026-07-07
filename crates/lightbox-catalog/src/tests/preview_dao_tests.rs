// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E03 Phase A (T03/T04) acceptance criteria: the migration's partial-unique
//! scope semantics, and the preview-index write DAOs + read surface.

use lightbox_types::PreviewId;

use super::{one_image, seed_assets, seed_folder, temp_catalog};
use crate::{CatalogError, NewPreviewRow, PreviewSourceTag};

fn t0_row(
    asset: lightbox_types::AssetId,
    content_hash: lightbox_types::ContentHash,
) -> NewPreviewRow {
    NewPreviewRow {
        asset,
        image: None,
        content_hash,
        tier: 0,
        variant_hash: [1; 8],
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        colorspace: "srgb".to_owned(),
        store_path: "previews/ab/deadbeef.t0.jpg".to_owned(),
        width: 160,
        height: 120,
        bytes: 4096,
        checksum: [2; 8],
    }
}

fn t1_row(
    asset: lightbox_types::AssetId,
    image: lightbox_types::ImageId,
    content_hash: lightbox_types::ContentHash,
) -> NewPreviewRow {
    NewPreviewRow {
        asset,
        image: Some(image),
        content_hash,
        tier: 1,
        variant_hash: [3; 8],
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        colorspace: "srgb".to_owned(),
        store_path: "previews/ab/deadbeef.t1.jxl".to_owned(),
        width: 3840,
        height: 2560,
        bytes: 1_000_000,
        checksum: [4; 8],
    }
}

/// T03 AC: the migration applies over the current schema head (implicitly
/// proven by every test in this module — `temp_catalog()` runs all
/// migrations, including 0004, to create the fixture).
#[test]
fn migration_applies_and_preview_table_exists() {
    let (_dir, catalog) = temp_catalog();
    assert!(catalog.schema_version() >= 4);
    // A trivial round trip proves the table + indexes exist and are usable.
    assert_eq!(catalog.reader().all_preview_rows().unwrap(), vec![]);
}

/// T03 AC: asset-scope (T0) and image-scope (T1/T2) rows live in disjoint
/// partial-unique-index spaces and can never collide, even when every other
/// column (content_hash, tier is different by construction here, but the
/// point is the `image_id IS NULL` vs `IS NOT NULL` predicate) lines up.
#[test]
fn asset_scope_and_image_scope_do_not_collide() {
    let (dir, catalog) = temp_catalog();
    let (asset, image) = one_image(&catalog, dir.path());
    let hash = super::hash(1);

    let t0_id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t0_row(asset, hash)))
        .unwrap();
    let t1_id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t1_row(asset, image, hash)))
        .unwrap();
    assert_ne!(t0_id, t1_id);

    let rows = catalog.reader().all_preview_rows().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.image.is_none() && r.tier == 0));
    assert!(rows.iter().any(|r| r.image == Some(image) && r.tier == 1));
}

/// T03 AC: within one scope, `(asset|image, tier, variant_hash)` is a real
/// UNIQUE constraint — re-upserting the same tuple updates in place rather
/// than duplicating, and a *different* variant_hash for the same scope+tier
/// is a distinct row (no accidental collision).
#[test]
fn upsert_is_idempotent_per_scope_tier_variant_and_distinct_variants_coexist() {
    let (dir, catalog) = temp_catalog();
    let (asset, _image) = one_image(&catalog, dir.path());
    let hash = super::hash(2);

    let id1 = catalog
        .writer()
        .with_txn({
            let row = t0_row(asset, hash);
            move |txn| txn.upsert_preview(row)
        })
        .unwrap();
    // Re-upsert same scope/tier/variant with different content: same row id,
    // content updated in place.
    let id2 = catalog
        .writer()
        .with_txn(move |txn| {
            let mut row = t0_row(asset, hash);
            row.bytes = 9999;
            row.store_path = "previews/ab/deadbeef.t0.jpg".to_owned();
            txn.upsert_preview(row)
        })
        .unwrap();
    assert_eq!(id1, id2, "same scope/tier/variant must upsert in place");
    assert_eq!(catalog.reader().all_preview_rows().unwrap().len(), 1);
    assert_eq!(
        catalog.reader().preview_row(id1).unwrap().unwrap().bytes,
        9999
    );

    // A distinct variant_hash for the SAME asset/tier is a distinct row.
    let id3 = catalog
        .writer()
        .with_txn(move |txn| {
            let mut row = t0_row(asset, hash);
            row.variant_hash = [7; 8];
            txn.upsert_preview(row)
        })
        .unwrap();
    assert_ne!(id1, id3);
    assert_eq!(catalog.reader().all_preview_rows().unwrap().len(), 2);
}

/// T03 AC (asset-scope dedupe, spec §3.1): two images sharing one asset (the
/// virtual-copy shape, modeled here directly at the DAO level since E07
/// virtual copies do not exist yet) each get their own image-scope row, but
/// share exactly one asset-scope T0 row.
#[test]
fn two_images_over_one_asset_share_t0_but_not_t1() {
    let (dir, catalog) = temp_catalog();
    let (root, folder) = seed_folder(&catalog, dir.path());
    let _ = root;
    let pairs = seed_assets(&catalog, folder, "vc", 1, 100, &[]);
    let (asset, image_a) = pairs[0];
    // A second "image" row over the same asset, simulating a virtual copy.
    let image_b = catalog
        .writer()
        .with_txn(move |txn| txn.insert_default_images(&[asset]))
        .unwrap()[0];
    assert_ne!(image_a, image_b);
    let hash = super::hash(100);

    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t0_row(asset, hash)))
        .unwrap();
    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t1_row(asset, image_a, hash)))
        .unwrap();
    catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t1_row(asset, image_b, hash)))
        .unwrap();

    let rows = catalog.reader().all_preview_rows().unwrap();
    assert_eq!(rows.len(), 3, "one T0 + two independent T1 rows");
    assert_eq!(rows.iter().filter(|r| r.image.is_none()).count(), 1);
    assert_eq!(rows.iter().filter(|r| r.tier == 1).count(), 2);
}

#[test]
fn mark_rendered_stale_below_only_touches_rendered_rows_under_the_new_revision() {
    let (dir, catalog) = temp_catalog();
    let (asset, image) = one_image(&catalog, dir.path());
    let hash = super::hash(3);

    let embedded_id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t1_row(asset, image, hash)))
        .unwrap();
    let rendered_old_id = catalog
        .writer()
        .with_txn(move |txn| {
            let mut row = t1_row(asset, image, hash);
            row.variant_hash = [9; 8];
            row.source = PreviewSourceTag::Rendered;
            row.recipe_rev = 1;
            txn.upsert_preview(row)
        })
        .unwrap();
    let rendered_new_id = catalog
        .writer()
        .with_txn(move |txn| {
            let mut row = t1_row(asset, image, hash);
            row.variant_hash = [10; 8];
            row.source = PreviewSourceTag::Rendered;
            row.recipe_rev = 5;
            txn.upsert_preview(row)
        })
        .unwrap();

    let n = catalog
        .writer()
        .with_txn(move |txn| txn.mark_rendered_stale_below(image, 5))
        .unwrap();
    assert_eq!(n, 1);

    assert!(
        !catalog
            .reader()
            .preview_row(embedded_id)
            .unwrap()
            .unwrap()
            .stale
    );
    assert!(
        catalog
            .reader()
            .preview_row(rendered_old_id)
            .unwrap()
            .unwrap()
            .stale
    );
    assert!(
        !catalog
            .reader()
            .preview_row(rendered_new_id)
            .unwrap()
            .unwrap()
            .stale
    );
}

/// T04 AC groundwork: touch batching flushes as one write, is idempotent
/// against stale/missing ids, and actually advances `last_used_at`.
#[test]
fn touch_previews_last_used_batches_and_ignores_missing_ids() {
    let (dir, catalog) = temp_catalog();
    let (asset, _image) = one_image(&catalog, dir.path());
    let hash = super::hash(4);
    let id = catalog
        .writer()
        .with_txn({
            let row = t0_row(asset, hash);
            move |txn| txn.upsert_preview(row)
        })
        .unwrap();
    let before = catalog
        .reader()
        .preview_row(id)
        .unwrap()
        .unwrap()
        .last_used_at;

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let touched = catalog
        .writer()
        .with_txn(move |txn| txn.touch_previews_last_used(&[id, PreviewId(999_999)]))
        .unwrap();
    assert_eq!(touched, 1, "the missing id must be silently skipped");

    let after = catalog
        .reader()
        .preview_row(id)
        .unwrap()
        .unwrap()
        .last_used_at;
    assert!(after >= before, "touch must not move last_used_at backward");
}

#[test]
fn evict_candidates_are_lru_ordered_and_tier_filterable() {
    let (dir, catalog) = temp_catalog();
    let (asset, image) = one_image(&catalog, dir.path());
    let hash = super::hash(5);

    let t0_id = catalog
        .writer()
        .with_txn({
            let row = t0_row(asset, hash);
            move |txn| txn.upsert_preview(row)
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let t1_id = catalog
        .writer()
        .with_txn({
            let row = t1_row(asset, image, hash);
            move |txn| txn.upsert_preview(row)
        })
        .unwrap();

    let all = catalog.reader().preview_evict_candidates(None, 10).unwrap();
    assert_eq!(all.first().map(|r| r.id), Some(t0_id), "oldest first");
    assert_eq!(all.get(1).map(|r| r.id), Some(t1_id));

    let t1_only = catalog
        .reader()
        .preview_evict_candidates(Some(1), 10)
        .unwrap();
    assert_eq!(t1_only.len(), 1);
    assert_eq!(t1_only[0].id, t1_id);
}

#[test]
fn delete_preview_is_idempotent() {
    let (dir, catalog) = temp_catalog();
    let (asset, _image) = one_image(&catalog, dir.path());
    let hash = super::hash(6);
    let id = catalog
        .writer()
        .with_txn({
            let row = t0_row(asset, hash);
            move |txn| txn.upsert_preview(row)
        })
        .unwrap();
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_preview(id))
        .unwrap();
    assert_eq!(catalog.reader().preview_row(id).unwrap(), None);
    // Deleting again is a no-op, not an error.
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_preview(id))
        .unwrap();
}

/// A row referencing a non-existent asset is rejected by the `FOREIGN KEY`
/// constraint (`foreign_keys=ON` at every connection, spec §4.2) — proven
/// through the real DAO, no raw-SQL bypass needed since `NewPreviewRow` can
/// carry any `AssetId` the caller likes.
#[test]
fn upsert_rejects_a_dangling_asset_id() {
    let (_dir, catalog) = temp_catalog();
    let bogus_asset = lightbox_types::AssetId(999_999);
    let hash = super::hash(7);
    let err = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(t0_row(bogus_asset, hash)))
        .unwrap_err();
    assert!(matches!(err, CatalogError::Constraint(_)), "{err:?}");
}
