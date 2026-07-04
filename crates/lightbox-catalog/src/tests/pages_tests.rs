// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T10 acceptance criteria: keyset pagination correctness for every
//! [`SortOrder`] (including NULL-key segments), index-only query plans
//! (`EXPLAIN QUERY PLAN`), and stability under interleaved inserts
//! (spec §6 property test).

use lightbox_types::{FolderId, ImageId};
use proptest::prelude::*;

use super::{new_asset, seed_assets, seed_folder, temp_catalog};
use crate::pages::{segment_params, segment_sql};
use crate::{Catalog, ImageQuery, ImageSummary, PageCursor, SortOrder};

const ALL_SORTS: [SortOrder; 5] = [
    SortOrder::CaptureTimeAsc,
    SortOrder::CaptureTimeDesc,
    SortOrder::AddedAsc,
    SortOrder::AddedDesc,
    SortOrder::FilenameAsc,
];

/// Drains all pages of `q`, checking cursor plumbing along the way.
fn drain(catalog: &Catalog, mut q: ImageQuery) -> Vec<ImageSummary> {
    let reader = catalog.reader();
    let mut all: Vec<ImageSummary> = Vec::new();
    loop {
        let page = reader.images_page(&q).unwrap();
        assert!(
            page.items.len() <= q.limit.clamp(1, 1000) as usize,
            "page exceeds limit"
        );
        let done = page.next.is_none();
        all.extend(page.items);
        if done {
            return all;
        }
        q.cursor = page.next;
    }
}

/// The expected total order, computed in Rust from the summaries.
fn sort_expected(items: &mut [ImageSummary], sort: SortOrder) {
    // Tiebreak is (asset.id, image.id); with one image per asset, image id
    // order == asset id order, so image id suffices here.
    items.sort_by(|x, y| {
        let by_key = match sort {
            SortOrder::CaptureTimeAsc => nulls_first(&x.capture_time, &y.capture_time),
            SortOrder::CaptureTimeDesc => nulls_first(&y.capture_time, &x.capture_time),
            // added_at can tie across a whole batch; fall through to ids.
            SortOrder::AddedAsc | SortOrder::AddedDesc => std::cmp::Ordering::Equal,
            SortOrder::FilenameAsc => x.filename.cmp(&y.filename),
        };
        let ids = match sort {
            SortOrder::AddedDesc | SortOrder::CaptureTimeDesc => y.id.0.cmp(&x.id.0),
            _ => x.id.0.cmp(&y.id.0),
        };
        by_key.then(ids)
    });
}

fn nulls_first(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y),
    }
}

#[test]
fn pagination_is_complete_ordered_and_duplicate_free_for_every_sort() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    // NULL capture times interleaved with two duplicate-heavy values and
    // distinct ones — exercises both segments and the dup-key residual.
    let captures = [
        None,
        Some("2026-01-01T00:00:00.000000Z"),
        Some("2026-01-01T00:00:00.000000Z"),
        None,
        Some("2026-02-02T00:00:00.000000Z"),
    ];
    seed_assets(&catalog, folder, "p", 137, 0, &captures);

    for sort in ALL_SORTS {
        for limit in [1, 7, 137, 500] {
            let got = drain(
                &catalog,
                ImageQuery {
                    folder: None,
                    sort,
                    cursor: None,
                    limit,
                },
            );
            assert_eq!(got.len(), 137, "{sort:?} limit {limit}: completeness");
            let mut ids: Vec<i64> = got.iter().map(|s| s.id.0).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), 137, "{sort:?} limit {limit}: no duplicates");

            let mut expected = got.clone();
            sort_expected(&mut expected, sort);
            let got_ids: Vec<i64> = got.iter().map(|s| s.id.0).collect();
            let expected_ids: Vec<i64> = expected.iter().map(|s| s.id.0).collect();
            assert_eq!(got_ids, expected_ids, "{sort:?} limit {limit}: order");
        }
    }
}

#[test]
fn folder_filter_restricts_pages() {
    let (dir, catalog) = temp_catalog();
    let (root, folder_a) = seed_folder(&catalog, dir.path());
    let folder_b = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_folder(root, None, "other"))
        .unwrap();
    seed_assets(&catalog, folder_a, "a", 10, 0, &[]);
    seed_assets(&catalog, folder_b, "b", 5, 1000, &[]);

    for sort in ALL_SORTS {
        let got = drain(
            &catalog,
            ImageQuery {
                folder: Some(folder_b),
                sort,
                cursor: None,
                limit: 3,
            },
        );
        assert_eq!(got.len(), 5, "{sort:?}");
        assert!(got.iter().all(|s| s.filename.contains("-b.jpg")));
    }

    // Unknown folder: empty, no error.
    let got = drain(
        &catalog,
        ImageQuery {
            folder: Some(FolderId(999_999)),
            sort: SortOrder::FilenameAsc,
            cursor: None,
            limit: 3,
        },
    );
    assert!(got.is_empty());
}

#[test]
fn exact_page_boundary_has_no_ghost_page() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "x", 6, 0, &[]);

    let reader = catalog.reader();
    let q = ImageQuery {
        folder: None,
        sort: SortOrder::FilenameAsc,
        cursor: None,
        limit: 6,
    };
    let page = reader.images_page(&q).unwrap();
    assert_eq!(page.items.len(), 6);
    assert!(
        page.next.is_none(),
        "an exactly-full final page must not hand out a cursor"
    );
}

#[test]
fn mismatched_cursor_is_rejected_not_misread() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    seed_assets(&catalog, folder, "m", 4, 0, &[]);

    // A NULL-key cursor cannot address a NOT NULL sort key.
    let cursor = PageCursor::new(None, 1, 1);
    let err = catalog
        .reader()
        .images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::FilenameAsc,
            cursor: Some(cursor),
            limit: 2,
        })
        .unwrap_err();
    assert!(matches!(err, crate::CatalogError::InvalidArg(_)));
}

/// T10 AC: every `images_page` shape runs off an index — no full table scan,
/// no sorter (`USE TEMP B-TREE`). Asserted over the real schema with data.
#[test]
fn every_page_query_plan_is_index_driven() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    let captures = [None, Some("2026-01-01T00:00:00.000000Z")];
    seed_assets(&catalog, folder, "plan", 400, 0, &captures);

    let reader = catalog.reader();
    for sort in ALL_SORTS {
        for segment in sort.segments() {
            for folder_filtered in [false, true] {
                for resuming in [false, true] {
                    let sql = segment_sql(sort, *segment, folder_filtered, resuming);
                    let cursor = resuming.then(|| {
                        PageCursor::new(
                            (!segment.null_key).then(|| "2026-01-01T00:00:00.000000Z".to_owned()),
                            5,
                            5,
                        )
                    });
                    let params = segment_params(
                        *segment,
                        folder_filtered.then_some(folder),
                        cursor.as_ref(),
                        10,
                    );
                    let plan = explain(reader.conn(), &sql, &params);
                    let context =
                        format!("{sort:?} {segment:?} folder={folder_filtered} resume={resuming}\nSQL: {sql}\nPLAN:\n{plan}");
                    for line in plan.lines() {
                        assert!(
                            !line.contains("USE TEMP B-TREE"),
                            "sorter in plan — {context}"
                        );
                        if line.contains("SCAN") && !line.contains("USING INDEX") {
                            panic!("unindexed scan in plan — {context}");
                        }
                    }
                }
            }
        }
    }
}

fn explain(conn: &rusqlite::Connection, sql: &str, params: &[rusqlite::types::Value]) -> String {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            r.get::<_, String>(3)
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
}

/// Spec §6 property: pagination completeness / no-dupes under interleaved
/// inserts. Rows present at the start must all be seen exactly once no
/// matter what lands mid-pagination.
#[test]
fn pagination_is_stable_under_interleaved_inserts() {
    proptest!(ProptestConfig::with_cases(12), |(
        sort_idx in 0usize..ALL_SORTS.len(),
        limit in 1u32..7,
        // Between page fetches: how many rows to insert (0 = none).
        interleave in proptest::collection::vec(0u64..5, 0..12),
        // Which capture-time bucket new rows land in.
        capture_bucket in proptest::collection::vec(0u8..3, 0..12),
    )| {
        let sort = ALL_SORTS[sort_idx];
        let (dir, catalog) = temp_catalog();
        let (_root, folder) = seed_folder(&catalog, dir.path());
        let captures = [None, Some("2026-01-01T00:00:00.000000Z"), Some("2026-03-01T00:00:00.000000Z")];
        let initial = seed_assets(&catalog, folder, "base", 25, 0, &captures);
        let initial_ids: std::collections::BTreeSet<i64> =
            initial.iter().map(|(_, img)| img.0).collect();

        let reader = catalog.reader();
        let mut q = ImageQuery { folder: None, sort, cursor: None, limit };
        let mut seen: Vec<i64> = Vec::new();
        let mut step = 0usize;
        let mut next_seed = 10_000u64;
        loop {
            let page = reader.images_page(&q).unwrap();
            let done = page.next.is_none();
            seen.extend(page.items.iter().map(|s| s.id.0));
            if done {
                break;
            }
            q.cursor = page.next;

            // Interleave inserts between pages.
            let n = interleave.get(step).copied().unwrap_or(0);
            let bucket = capture_bucket.get(step).copied().unwrap_or(0) as usize;
            if n > 0 {
                let capture = captures[bucket % captures.len()];
                seed_assets(&catalog, folder, &format!("mid{step}"), n, next_seed, &[capture]);
                next_seed += n;
            }
            step += 1;
        }

        // No image id is ever seen twice…
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        let deduped_len = { sorted.dedup(); sorted.len() };
        prop_assert_eq!(deduped_len, seen.len(), "duplicate rows across pages");
        // …and every initially-present row was seen.
        let seen_set: std::collections::BTreeSet<i64> = seen.into_iter().collect();
        for id in &initial_ids {
            prop_assert!(seen_set.contains(id), "initial image {} skipped", id);
        }
    });
}

#[test]
fn summary_row_mapping_round_trips() {
    let (dir, catalog) = temp_catalog();
    let (_root, folder) = seed_folder(&catalog, dir.path());
    catalog
        .writer()
        .with_txn(move |txn| {
            let mut a = new_asset(folder, "map.CR3", 77);
            a.format = "CR3".to_owned();
            a.capture_time = Some("2026-05-05T05:05:05.000000Z".to_owned());
            a.orientation = lightbox_types::Orientation::O8;
            a.width = 8192;
            a.height = 5464;
            let outcome = txn.insert_assets(&[a])?;
            let images = txn.insert_default_images(&outcome.inserted)?;
            txn.set_rating(images[0], Some(3))?;
            txn.set_flag(images[0], lightbox_types::Flag::Reject)?;
            Ok(())
        })
        .unwrap();

    let page = catalog
        .reader()
        .images_page(&ImageQuery {
            folder: None,
            sort: SortOrder::CaptureTimeAsc,
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let s = &page.items[0];
    assert_eq!(s.filename, "map.CR3");
    assert_eq!(
        s.capture_time.as_deref(),
        Some("2026-05-05T05:05:05.000000Z")
    );
    assert_eq!(s.rating, Some(3));
    assert_eq!(s.flag, lightbox_types::Flag::Reject);
    assert_eq!(s.orientation, lightbox_types::Orientation::O8);
    assert_eq!((s.width, s.height), (8192, 5464));
    assert!(!s.missing);
    assert!(!s.decode_error);
    assert_eq!(s.id, ImageId(1));
}
