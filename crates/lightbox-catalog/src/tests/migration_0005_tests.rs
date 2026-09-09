// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E04 T3 acceptance: `0005_open_in_place` upgrades a populated,
//! fully-migrated-to-`0004` store, asset/image ids and content preserved,
//! the two-level cascade (`edit_recipe`/`history_step`/`snapshot`/`xmp_sync`/
//! `preview` all chain `ON DELETE CASCADE` off `image`/`asset`) survives the
//! `asset` table rebuild, FTS search still hits, and the pre-upgrade copy is
//! written. The `kill -9` mid-upgrade leg lives in the sibling integration
//! test `tests/migration_0005_fault_injection.rs` (re-spawns a child
//! process, mirroring `migration_0002_fault_injection.rs`).

use lightbox_types::PV_M0;
use rusqlite::params;

use super::{seed_assets, seed_folder};
use crate::migrate::MIGRATIONS;
use crate::{Catalog, CatalogTxn, IntegrityStatus, NewPreviewRow, PreviewSourceTag};

#[test]
fn upgrade_from_0004_preserves_everything_across_the_asset_rebuild() {
    let dir = tempfile::TempDir::new().unwrap();
    let lbdata = dir.path().join("cat.lbdata");

    // --- build a fully-migrated-to-0004 (pre-E04) populated store ---
    let (asset, image, other_image) = {
        let catalog = Catalog::create_with_migrations(&lbdata, &MIGRATIONS[..4]).unwrap();
        assert_eq!(catalog.schema_version(), 4, "seed catalog must be pre-0005");
        let (_root, folder) = seed_folder(&catalog, dir.path());
        let pairs = seed_assets(
            &catalog,
            folder,
            "mig5",
            2,
            0,
            &[Some("2026-01-01T00:00:00.000000Z")],
        );
        let (asset, image) = pairs[0];
        let (_other_asset, other_image) = pairs[1];

        // Two-level cascade content: edit_recipe -> image, history_step ->
        // image, snapshot -> image, xmp_sync -> asset, preview -> asset AND
        // image (T0 + T1 rows).
        catalog
            .writer()
            .with_txn(move |txn| {
                txn.upsert_edit_recipe(image, PV_M0, 1, b"recipe-doc", 1)?;
                txn.append_history_step(image, 1, b"op", b"delta", b"inverse", None)?;
                txn.rebuild_edit_index(image, true, false, false, None, Some("color"))?;
                txn.insert_snapshot(image, "before-upgrade", b"snap-doc")?;
                txn.upsert_xmp_sync_write(asset, b"sidecar-hash", "2026-01-01", b"recipe-hash")?;
                txn.upsert_preview(NewPreviewRow {
                    asset,
                    image: None,
                    content_hash: catalog_content_hash(txn, asset),
                    tier: 0,
                    variant_hash: [0u8; 8],
                    source: PreviewSourceTag::Embedded,
                    recipe_rev: 0,
                    colorspace: "srgb".to_owned(),
                    store_path: "t0/deadbeef.jpg".to_owned(),
                    width: 256,
                    height: 170,
                    bytes: 12_345,
                    checksum: [1u8; 8],
                })?;
                txn.upsert_preview(NewPreviewRow {
                    asset,
                    image: Some(image),
                    content_hash: catalog_content_hash(txn, asset),
                    tier: 1,
                    variant_hash: [1u8; 8],
                    source: PreviewSourceTag::Rendered,
                    recipe_rev: 1,
                    colorspace: "srgb".to_owned(),
                    store_path: "t1/deadbeef.jpg".to_owned(),
                    width: 2048,
                    height: 1365,
                    bytes: 456_789,
                    checksum: [2u8; 8],
                })?;
                Ok(())
            })
            .unwrap();

        // FTS hits before the upgrade (sanity).
        let hits = catalog.reader().search_filenames("f00000", 10).unwrap();
        assert!(
            !hits.is_empty(),
            "FTS must find the seeded filename pre-upgrade"
        );

        (asset, image, other_image)
    }; // catalog (and its writer thread) drops here, WAL flushed on drop.

    // --- reopen: runs the 0005 migration via the rebuilds_tables procedure ---
    let catalog = Catalog::open(&lbdata).unwrap();
    assert_eq!(catalog.schema_version(), MIGRATIONS.len() as u32);
    assert_eq!(catalog.integrity(), IntegrityStatus::Ok);

    // foreign_key_check clean post-upgrade (belt-and-braces beyond the
    // runner's own commit-time gate).
    let violations: i64 = catalog
        .writer()
        .with_txn(|txn| {
            let mut stmt = txn.raw().prepare("PRAGMA foreign_key_check")?;
            let n = stmt.query_map([], |_| Ok(()))?.count();
            Ok(n as i64)
        })
        .unwrap();
    assert_eq!(
        violations, 0,
        "foreign_key_check must be clean post-upgrade"
    );

    let reader = catalog.reader();

    // Asset/image ids and row content preserved byte-for-byte.
    let counts = reader.counts().unwrap();
    assert_eq!(counts.assets, 2);
    assert_eq!(counts.images, 2);
    let detail = reader.image_detail(image).unwrap();
    assert_eq!(detail.asset, asset);
    assert_eq!(
        detail.capture_time.as_deref(),
        Some("2026-01-01T00:00:00.000000Z")
    );
    // The legacy row still has its managed-tree folder (unlike an
    // open-in-place row, whose `folder_id` is NULL), and its path resolves
    // via the LEFT JOIN folder/root fallback (T4's reader change), not the
    // new `abs_path` column (which stays NULL for legacy rows).
    assert!(
        detail.folder.is_some(),
        "legacy managed row keeps its folder_id"
    );
    let abs = reader.asset_abs_path(asset).unwrap();
    assert!(
        abs.ends_with("shoot/f00000-mig5.jpg")
            || abs.to_string_lossy().ends_with("shoot\\f00000-mig5.jpg"),
        "path must still compose root ⊕ folder.rel_path ⊕ filename for a legacy row: {}",
        abs.display()
    );

    // The OTHER image (untouched by the edit-state seeding) still resolves
    // too, proves the rebuild didn't just get lucky on the one row we care
    // about.
    let other_detail = reader.image_detail(other_image).unwrap();
    assert!(reader.asset_abs_path(other_detail.asset).is_ok());

    // Two-level cascade content intact, edit_recipe/history/snapshot chain
    // off `image`, xmp_sync off `asset`.
    assert!(
        reader.edit_state_row(image).unwrap().is_some(),
        "edit_recipe survived"
    );
    assert_eq!(
        reader.history_page(image, None, 10).unwrap().len(),
        1,
        "history_step survived"
    );
    assert_eq!(
        reader.snapshots(image).unwrap().len(),
        1,
        "snapshot survived"
    );
    assert!(
        reader.xmp_sync_row(asset).unwrap().is_some(),
        "xmp_sync survived"
    );
    assert!(
        reader.edit_badges(&[image]).unwrap()[0].is_edited,
        "edit_index survived"
    );

    // preview rows (both asset-scope T0 and image-scope T1) survived.
    assert_eq!(
        reader.preview_asset_scope_rows(asset).unwrap().len(),
        1,
        "T0 preview survived"
    );
    assert_eq!(
        reader.preview_rows_for_image(image).unwrap().len(),
        1,
        "T1 preview survived"
    );

    // FTS still hits post-rebuild (the migration's `assets_fts('rebuild')`).
    let hits = reader.search_filenames("f00000", 10).unwrap();
    assert!(
        !hits.is_empty(),
        "FTS must still find the filename post-upgrade"
    );

    // Pre-upgrade copy written and itself integrity-clean.
    let pre = lbdata.join("backups/pre-upgrade-4/catalog.sqlite");
    assert!(
        pre.is_file(),
        "missing copy-on-write pre-upgrade snapshot at {}",
        pre.display()
    );
    let findings = crate::integrity_check_file(&pre).unwrap();
    assert!(
        findings.is_empty(),
        "pre-upgrade snapshot corrupt: {findings:?}"
    );
}

/// Test-only helper: real code derives `content_hash` from the probe, not a
/// round-trip catalog lookup, this just avoids re-threading the seeded hash
/// through `seed_assets`' return type for a field the preview DAO denormalizes.
fn catalog_content_hash(
    txn: &CatalogTxn<'_>,
    asset: lightbox_types::AssetId,
) -> lightbox_types::ContentHash {
    let bytes: Vec<u8> = txn
        .raw()
        .query_row(
            "SELECT content_hash FROM asset WHERE id = ?1",
            params![asset.0],
            |r| r.get(0),
        )
        .unwrap();
    lightbox_types::ContentHash(bytes.try_into().unwrap())
}
