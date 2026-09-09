// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! T11 acceptance criteria: FTS5 row-parity with `asset` under arbitrary
//! insert/delete/rename sequences (property test over the triggers) and
//! diacritic-insensitive prefix search.

use proptest::prelude::*;

use super::{new_asset, seed_folder, temp_catalog};
use crate::Catalog;

/// FTS5 external-content self-check: errors if the index disagrees with the
/// `asset` content table.
fn assert_fts_parity(catalog: &Catalog) {
    catalog
        .writer()
        .with_txn(|txn| {
            txn.raw()
                .execute(
                    "INSERT INTO assets_fts(assets_fts, rank) VALUES ('integrity-check', 1)",
                    [],
                )
                .map_err(|e| crate::CatalogError::Internal(format!("fts integrity: {e}")))?;
            Ok(())
        })
        .expect("assets_fts must stay in row-parity with asset");
}

#[test]
fn search_matches_filename_and_camera_with_prefixes() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let images = catalog
        .writer()
        .with_txn(move |txn| {
            let mut a = new_asset(folder, "IMG_4711.CR3", 1);
            a.camera_make = Some("Canon".to_owned());
            a.camera_model = Some("EOS R5".to_owned());
            let mut b = new_asset(folder, "sunset-beach.jpg", 2);
            b.camera_make = Some("Nikon".to_owned());
            let outcome = txn.insert_assets(&[a, b])?;
            txn.insert_default_images(&outcome.inserted)
        })
        .unwrap();

    let reader = catalog.reader();
    // Filename prefix.
    assert_eq!(reader.search_filenames("IMG", 10).unwrap(), vec![images[0]]);
    assert_eq!(reader.search_filenames("471", 10).unwrap(), vec![images[0]]);
    // Camera prefix (make and model live in the `camera` column).
    assert_eq!(
        reader.search_filenames("cano", 10).unwrap(),
        vec![images[0]]
    );
    assert_eq!(reader.search_filenames("R5", 10).unwrap(), vec![images[0]]);
    // Multi-token AND semantics.
    assert_eq!(
        reader.search_filenames("sunset beach", 10).unwrap(),
        vec![images[1]]
    );
    assert!(reader
        .search_filenames("sunset canon", 10)
        .unwrap()
        .is_empty());
    // No-token and no-match inputs.
    assert!(reader.search_filenames("  *? ", 10).unwrap().is_empty());
    assert!(reader.search_filenames("zzz", 10).unwrap().is_empty());
    // Limit respected.
    assert_eq!(reader.search_filenames("jpg", 0).unwrap().len(), 0);
}

#[test]
fn search_is_diacritic_insensitive() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let images = catalog
        .writer()
        .with_txn(move |txn| {
            let outcome = txn.insert_assets(&[
                new_asset(folder, "Pläne-übersicht.jpg", 1),
                new_asset(folder, "café-noël.jpg", 2),
            ])?;
            txn.insert_default_images(&outcome.inserted)
        })
        .unwrap();

    let reader = catalog.reader();
    // ASCII query hits diacritic filenames (remove_diacritics 2)…
    assert_eq!(
        reader.search_filenames("plane", 10).unwrap(),
        vec![images[0]]
    );
    assert_eq!(
        reader.search_filenames("cafe", 10).unwrap(),
        vec![images[1]]
    );
    assert_eq!(
        reader.search_filenames("noel", 10).unwrap(),
        vec![images[1]]
    );
    // …and diacritic queries hit too.
    assert_eq!(
        reader.search_filenames("übersicht", 10).unwrap(),
        vec![images[0]]
    );
}

/// One step of the T11 property sequence.
#[derive(Clone, Debug)]
enum FtsOp {
    Insert {
        name_seed: u32,
    },
    /// Delete the i-th live asset (mod population).
    Delete {
        pick: u8,
    },
    /// Rename the i-th live asset (exercises the UPDATE trigger; the DAO for
    /// rename is E07, so the test drives the trigger directly).
    Rename {
        pick: u8,
        name_seed: u32,
    },
    /// Change camera fields (the other UPDATE OF column set).
    Recamera {
        pick: u8,
        name_seed: u32,
    },
}

fn fts_op_strategy() -> impl Strategy<Value = FtsOp> {
    prop_oneof![
        (any::<u32>()).prop_map(|name_seed| FtsOp::Insert { name_seed }),
        (any::<u8>()).prop_map(|pick| FtsOp::Delete { pick }),
        (any::<u8>(), any::<u32>()).prop_map(|(pick, name_seed)| FtsOp::Rename { pick, name_seed }),
        (any::<u8>(), any::<u32>())
            .prop_map(|(pick, name_seed)| FtsOp::Recamera { pick, name_seed }),
    ]
}

#[test]
fn fts_keeps_row_parity_under_arbitrary_mutation_sequences() {
    proptest!(ProptestConfig::with_cases(16), |(
        ops in proptest::collection::vec(fts_op_strategy(), 1..40),
    )| {
        let (dir, catalog) = temp_catalog();
        let (_root, folder) = seed_folder(&catalog, dir.path());
        let mut live: Vec<i64> = Vec::new();
        let mut next_hash_seed = 1u64;

        for op in ops {
            match op {
                FtsOp::Insert { name_seed } => {
                    let seed = next_hash_seed;
                    next_hash_seed += 1;
                    let inserted = catalog.writer().with_txn(move |txn| {
                        let a = new_asset(folder, &format!("file-{name_seed}-{seed}.jpg"), seed);
                        Ok(txn.insert_assets(&[a])?.inserted)
                    }).unwrap();
                    live.extend(inserted.iter().map(|a| a.0));
                }
                FtsOp::Delete { pick } => {
                    if live.is_empty() { continue; }
                    let id = live.remove(pick as usize % live.len());
                    catalog.writer().with_txn(move |txn| {
                        txn.raw().execute("DELETE FROM asset WHERE id = ?1", [id]).unwrap();
                        Ok(())
                    }).unwrap();
                }
                FtsOp::Rename { pick, name_seed } => {
                    if live.is_empty() { continue; }
                    let id = live[pick as usize % live.len()];
                    catalog.writer().with_txn(move |txn| {
                        txn.raw().execute(
                            "UPDATE asset SET filename = ?1 WHERE id = ?2",
                            rusqlite::params![format!("renamed-{name_seed}-{id}.jpg"), id],
                        ).unwrap();
                        Ok(())
                    }).unwrap();
                }
                FtsOp::Recamera { pick, name_seed } => {
                    if live.is_empty() { continue; }
                    let id = live[pick as usize % live.len()];
                    catalog.writer().with_txn(move |txn| {
                        txn.raw().execute(
                            "UPDATE asset SET camera_make = ?1, camera_model = ?2 WHERE id = ?3",
                            rusqlite::params![format!("Make{name_seed}"), format!("Model{name_seed}"), id],
                        ).unwrap();
                        Ok(())
                    }).unwrap();
                }
            }
            assert_fts_parity(&catalog);
        }

        // Row-parity on the final state, counted directly.
        let n_live: i64 = live.len() as i64;
        prop_assert_eq!(catalog.reader().counts().unwrap().assets as i64, n_live);
        let fts_rows: i64 = catalog.writer().with_txn(|txn| {
            Ok(txn.raw().query_row("SELECT COUNT(*) FROM assets_fts", [], |r| r.get(0)).unwrap())
        }).unwrap();
        prop_assert_eq!(fts_rows, n_live, "FTS row count must equal asset row count");
    });
}

/// Undo-import must restore exact pre-import row counts, FTS included
/// (T18 AC, proven here with the T11 harness).
#[test]
fn undo_import_restores_fts_row_parity() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    super::seed_assets(&catalog, folder, "keep", 3, 500, &[]);

    let session = catalog
        .writer()
        .with_txn(|txn| txn.begin_import_session("src", "{}"))
        .unwrap();
    catalog
        .writer()
        .with_txn(move |txn| {
            let mut batch = Vec::new();
            for i in 0..4u64 {
                let mut a = new_asset(folder, &format!("undo{i}.jpg"), 600 + i);
                a.import_session = Some(session);
                batch.push(a);
            }
            let outcome = txn.insert_assets(&batch)?;
            txn.insert_default_images(&outcome.inserted)?;
            Ok(())
        })
        .unwrap();
    assert_fts_parity(&catalog);
    assert_eq!(
        catalog.reader().search_filenames("undo", 10).unwrap().len(),
        4
    );

    catalog
        .writer()
        .with_txn(move |txn| txn.remove_import_session(session))
        .unwrap();
    assert_fts_parity(&catalog);
    assert!(catalog
        .reader()
        .search_filenames("undo", 10)
        .unwrap()
        .is_empty());
    assert_eq!(catalog.reader().counts().unwrap().assets, 3);
}
