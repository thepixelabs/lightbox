// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E03 Phase E (T18) acceptance criteria: the raw-cache accounting DAO
//! (upsert/touch/delete/evict-candidates/total-bytes) against the
//! `raw_cache_entry` table shipped by migration 0004 in Phase A.

use crate::NewRawCacheEntryRow;

fn row(seed: u64, bytes: u64) -> NewRawCacheEntryRow {
    NewRawCacheEntryRow {
        content_hash: super::hash(seed),
        params_hash: seed.to_be_bytes(),
        payload_schema: 1,
        store_path: format!("rawcache/{seed:02x}/entry-{seed}.zst"),
        bytes,
    }
}

#[test]
fn migration_ships_the_raw_cache_entry_table() {
    let (_dir, catalog) = super::temp_catalog();
    assert!(catalog.schema_version() >= 4);
    assert_eq!(catalog.reader().all_rawcache_rows().unwrap(), vec![]);
    assert_eq!(catalog.reader().rawcache_total_bytes().unwrap(), 0);
}

#[test]
fn upsert_is_idempotent_per_content_hash_params_hash_and_distinct_params_coexist() {
    let (_dir, catalog) = super::temp_catalog();

    let id1 = catalog
        .writer()
        .with_txn({
            let r = row(1, 1000);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    // Re-upsert same key, different bytes: same row id, content updated in place.
    let id2 = catalog
        .writer()
        .with_txn({
            let mut r = row(1, 1000);
            r.bytes = 2000;
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    assert_eq!(
        id1, id2,
        "same (content_hash, params_hash) must upsert in place"
    );
    assert_eq!(catalog.reader().all_rawcache_rows().unwrap().len(), 1);
    assert_eq!(
        catalog
            .reader()
            .rawcache_lookup(super::hash(1), 1u64.to_be_bytes())
            .unwrap()
            .unwrap()
            .bytes,
        2000
    );

    // A distinct params_hash for the SAME content_hash is a distinct row.
    let id3 = catalog
        .writer()
        .with_txn({
            let mut r = row(1, 500);
            r.params_hash = 99u64.to_be_bytes();
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    assert_ne!(id1, id3);
    assert_eq!(catalog.reader().all_rawcache_rows().unwrap().len(), 2);
    assert_eq!(catalog.reader().rawcache_total_bytes().unwrap(), 2500);
}

#[test]
fn touch_by_key_advances_last_used_at_and_tolerates_a_missing_key() {
    let (_dir, catalog) = super::temp_catalog();
    catalog
        .writer()
        .with_txn({
            let r = row(2, 10);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    let before = catalog
        .reader()
        .rawcache_lookup(super::hash(2), 2u64.to_be_bytes())
        .unwrap()
        .unwrap()
        .last_used_at;

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let touched = catalog
        .writer()
        .with_txn(move |txn| {
            txn.touch_rawcache_last_used_by_key(super::hash(2), 2u64.to_be_bytes())
        })
        .unwrap();
    assert!(touched);

    let missing = catalog
        .writer()
        .with_txn(move |txn| {
            txn.touch_rawcache_last_used_by_key(super::hash(999), 999u64.to_be_bytes())
        })
        .unwrap();
    assert!(
        !missing,
        "touching an absent key must not error, just report false"
    );

    let after = catalog
        .reader()
        .rawcache_lookup(super::hash(2), 2u64.to_be_bytes())
        .unwrap()
        .unwrap()
        .last_used_at;
    assert!(after >= before);
}

#[test]
fn delete_by_id_and_by_key_are_both_idempotent() {
    let (_dir, catalog) = super::temp_catalog();
    let id = catalog
        .writer()
        .with_txn({
            let r = row(3, 10);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_rawcache_entry(id))
        .unwrap();
    assert!(catalog
        .reader()
        .rawcache_lookup(super::hash(3), 3u64.to_be_bytes())
        .unwrap()
        .is_none());
    // Second delete (by id): no-op, not an error.
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_rawcache_entry(id))
        .unwrap();

    catalog
        .writer()
        .with_txn({
            let r = row(4, 10);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    let n = catalog
        .writer()
        .with_txn(move |txn| txn.delete_rawcache_entry_by_key(super::hash(4), 4u64.to_be_bytes()))
        .unwrap();
    assert_eq!(n, 1);
    let n2 = catalog
        .writer()
        .with_txn(move |txn| txn.delete_rawcache_entry_by_key(super::hash(4), 4u64.to_be_bytes()))
        .unwrap();
    assert_eq!(
        n2, 0,
        "deleting an already-gone key is a no-op, not an error"
    );
}

#[test]
fn evict_candidates_are_lru_ordered() {
    let (_dir, catalog) = super::temp_catalog();
    catalog
        .writer()
        .with_txn({
            let r = row(5, 10);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    catalog
        .writer()
        .with_txn({
            let r = row(6, 10);
            move |txn| txn.upsert_rawcache_entry(r)
        })
        .unwrap();

    let candidates = catalog.reader().rawcache_evict_candidates(10).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].content_hash, super::hash(5), "oldest first");
    assert_eq!(candidates[1].content_hash, super::hash(6));

    let one = catalog.reader().rawcache_evict_candidates(1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].content_hash, super::hash(5));
}
