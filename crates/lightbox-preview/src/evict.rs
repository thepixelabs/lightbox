// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Preview eviction + retention (E03 spec §3.2, Phase F T19): LRU-to-cap in
//! **tier order** (T2 → T1 → T0, spec §3.2: "T0 is small and is the culling
//! floor"), `store_path` refcount before unlink, the T2 age-retention sweep,
//! and selection-scoped discard (`DiscardPreviews`).
//!
//! **Refcount, then unlink.** Every candidate row is deleted from the
//! catalog FIRST (inside its own WAL txn, the durable, authoritative half
//! of "this preview no longer exists"), then [`lightbox_catalog::ReaderHandle::
//! preview_store_path_refcount`] is checked: only when zero OTHER rows still
//! point at the same `store_path` does this function touch the filesystem
//! (`Store::unlink_tracked`, which itself refuses to unlink a file with a
//! live in-flight reader, spec §3.2's "eviction never unlinks a file with a
//! live read handle"). A file that stays referenced (or live) is simply left
//! in place, harmless, and self-healing: `verify_store(Full)`'s orphan sweep
//! (T21) would adopt-or-remove it later if it ever truly becomes orphaned.
//!
//! **T2 is a directory, not a file.** `Store::unlink_tracked` operates on a
//! single file; a T2 row's `store_path` is its `.t2/` tile directory
//! (`t2.rs`). This module removes it with `remove_dir_all` instead
//! honestly scoped WITHOUT the live-handle guard `Store::unlink_tracked`
//! gives single-file tiers (T0/T1): no read path in this crate holds a
//! [`crate::store::ReadGuard`] for an individual T2 tile file today (only
//! `decode.rs::open_pixels`, the T0/T1 hot path, does). Recorded in
//! `docs/plan/epics/E03-deviations.md`, Phase F.

use std::sync::{Mutex, PoisonError};

use lightbox_catalog::{Catalog, PreviewRow};
use lightbox_types::{AssetId, ImageId};

use crate::config::Retention;
use crate::index::PreviewIndex;
use crate::pyramid::{RelPath, Tier, TierSet};
use crate::sched::{CacheKind, EventSink, PreviewEvent};
use crate::store::{Store, UnlinkOutcome};
use crate::PreviewError;

/// Report shared by [`evict_to_cap`], [`sweep_t2_retention`], and
/// [`discard`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewEvictReport {
    /// Catalog rows deleted.
    pub rows_deleted: u64,
    /// Bytes actually reclaimed on disk (rows whose file stayed referenced
    /// or live are counted in `rows_deleted` but NOT here).
    pub bytes_reclaimed: u64,
    /// Files left in place because another row still references them.
    pub skipped_referenced: u64,
    /// Files left in place because a reader currently holds them open
    /// (never observed for T2, see the module doc comment).
    pub skipped_live: u64,
}

impl PreviewEvictReport {
    fn merge(&mut self, other: PreviewEvictReport) {
        self.rows_deleted += other.rows_deleted;
        self.bytes_reclaimed += other.bytes_reclaimed;
        self.skipped_referenced += other.skipped_referenced;
        self.skipped_live += other.skipped_live;
    }
}

/// Deletes `row`'s catalog entry, then unlinks its file iff no other row
/// still references `store_path` and (T0/T1 only) no reader currently holds
/// it. Emits [`PreviewEvent::Evicted`] for image-scope rows (T0's
/// asset-scope rows have no single `ImageId` to name, see the spec's
/// `Evicted { image, tier }` shape).
fn delete_row(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    events: &EventSink,
    row: &PreviewRow,
) -> Result<PreviewEvictReport, PreviewError> {
    let mut report = PreviewEvictReport::default();
    let id = row.id;
    catalog
        .writer()
        .with_txn(move |txn| txn.delete_preview(id))
        .map_err(|e| PreviewError::Catalog(e.to_string()))?;
    index
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(id);
    report.rows_deleted += 1;
    let tier = Tier::from_u8(row.tier);

    let refcount = catalog
        .reader()
        .preview_store_path_refcount(&row.store_path)
        .unwrap_or(1); // fail safe: assume referenced, never guess-unlink
    if refcount > 0 {
        report.skipped_referenced += 1;
    } else {
        let rel = RelPath(row.store_path.clone());
        if tier == Some(Tier::T2) {
            // T2's store_path is a directory (t2.rs), no single-file
            // live-handle guard exists for it (module doc comment).
            let abs = store.resolve(&rel);
            if std::fs::remove_dir_all(&abs).is_ok() || !abs.exists() {
                report.bytes_reclaimed += row.bytes;
            }
        } else {
            match store.unlink_tracked(&rel) {
                UnlinkOutcome::Removed | UnlinkOutcome::AlreadyGone => {
                    report.bytes_reclaimed += row.bytes;
                }
                UnlinkOutcome::SkippedLive => report.skipped_live += 1,
                UnlinkOutcome::Deferred => report.bytes_reclaimed += row.bytes,
            }
        }
    }

    if let (Some(image), Some(tier)) = (row.image, tier) {
        (events)(PreviewEvent::Evicted { image, tier });
    }
    Ok(report)
}

/// Evicts LRU-oldest rows, **tier order T2 → T1 → T0** (spec §3.2), until
/// the store's total preview bytes are at or under `cap_bytes`. Each tier is
/// drained (by `last_used_at`, one candidate at a time so a mid-sweep
/// referenced/live skip never wedges the whole pass) before moving to the
/// next, T2 is emptied first (it is both the largest and the least
/// "foundational" tier), T0 last (spec: "the culling floor").
///
/// **Bounded retry on a transient catalog-write failure** (E03 Phase F
/// tail, T23 hardening, mirrors `RawCache::evict_to_cap`'s own documented
/// `MAX_CONSECUTIVE_RACES` convention, E-5): a single `delete_row` failure
/// (e.g. a busy-timeout under heavy concurrent writer/disk load) used to
/// `break` out of the ENTIRE tier immediately, silently leaving every
/// remaining over-cap row at that tier un-evicted for the rest of this
/// call. Observed in practice: `service::tests::
/// t19_t21_t22_facade_methods_compose_end_to_end` (a `cap_bytes = 0`
/// assertion expecting `stats().total_count() == 0`) flaked under a heavily
/// loaded `cargo test --workspace` run with exactly one row surviving.
/// Retrying the SAME tier up to `MAX_CONSECUTIVE_FAILURES` times before
/// giving up converges past a transient failure without spinning forever
/// on a genuinely stuck row.
pub(crate) fn evict_to_cap(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    cap_bytes: u64,
    events: &EventSink,
) -> PreviewEvictReport {
    const MAX_CONSECUTIVE_FAILURES: u32 = 8;
    let mut report = PreviewEvictReport::default();
    for tier in [Tier::T2, Tier::T1, Tier::T0] {
        let mut consecutive_failures = 0u32;
        loop {
            if total_bytes(catalog) <= cap_bytes {
                return report;
            }
            let Some(row) = catalog
                .reader()
                .preview_evict_candidates(Some(tier as u8), 1)
                .unwrap_or_default()
                .into_iter()
                .next()
            else {
                break; // nothing left at this tier; advance to the next
            };
            match delete_row(store, catalog, index, events, &row) {
                Ok(r) => {
                    report.merge(r);
                    consecutive_failures = 0;
                }
                Err(_) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        break; // give up on this tier; try the next
                    }
                    // Transient (e.g. a busy-timeout), retry the SAME
                    // candidate query rather than abandoning the tier.
                }
            }
        }
    }
    report
}

fn total_bytes(catalog: &Catalog) -> u64 {
    catalog
        .reader()
        .all_preview_rows()
        .map(|rows| rows.iter().map(|r| r.bytes).sum())
        .unwrap_or(0)
}

/// T2 age-based retention (spec §3.2: "T2 additionally has an age-based
/// retention sweep... default 30 days"). `Retention::Never` is a no-op.
/// Keyed on `built_at` (age since the rendition was produced), not
/// `last_used_at`, retention is about staleness, not access recency.
pub(crate) fn sweep_t2_retention(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    events: &EventSink,
    retention: Retention,
    now_unix: i64,
) -> PreviewEvictReport {
    let mut report = PreviewEvictReport::default();
    let Retention::Days(days) = retention else {
        return report;
    };
    let cutoff = now_unix - i64::from(days) * 86_400;
    let rows = catalog
        .reader()
        .preview_rows_older_than(Tier::T2 as u8, cutoff)
        .unwrap_or_default();
    for row in &rows {
        if let Ok(r) = delete_row(store, catalog, index, events, row) {
            report.merge(r);
        }
    }
    report
}

/// `DiscardPreviews { images, tiers }` (spec §5.6): removes rows + files for
/// `tiers` across `images`. **T0 is asset-scoped** (spec §3.1), discarding
/// it for one image discards the SHARED row for every virtual copy of that
/// image's asset, matching T0's sharing model rather than a per-image
/// concept it doesn't have. Each distinct asset's T0 is only visited once
/// even if multiple `images` share it.
pub(crate) fn discard(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    events: &EventSink,
    images: &[ImageId],
    tiers: TierSet,
) -> PreviewEvictReport {
    let mut report = PreviewEvictReport::default();
    let mut seen_assets: std::collections::HashSet<AssetId> = std::collections::HashSet::new();

    for &image in images {
        if tiers.t1 || tiers.t2 {
            let rows = catalog
                .reader()
                .preview_rows_for_image(image)
                .unwrap_or_default();
            for row in rows {
                let Some(tier) = Tier::from_u8(row.tier) else {
                    continue;
                };
                if !tiers.contains(tier) {
                    continue;
                }
                if let Ok(r) = delete_row(store, catalog, index, events, &row) {
                    report.merge(r);
                }
            }
        }
        if tiers.t0 {
            if let Ok(detail) = catalog.reader().image_detail(image) {
                if seen_assets.insert(detail.asset) {
                    let rows = catalog
                        .reader()
                        .preview_asset_scope_rows(detail.asset)
                        .unwrap_or_default();
                    for row in rows {
                        if let Ok(r) = delete_row(store, catalog, index, events, &row) {
                            report.merge(r);
                        }
                    }
                }
            }
        }
    }
    report
}

/// Never used in production (`CachePressure` is emitted by `service.rs`
/// call sites that already know the `CacheKind`); kept here as the one place
/// documenting the shape a caller constructs.
#[allow(dead_code)]
fn _cache_kind_doc_anchor() -> CacheKind {
    CacheKind::Preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PreviewStoreConfig;
    use crate::index::PreviewIndex;
    use crate::producer;
    use lightbox_types::{ContentHash, Orientation};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn fixtures_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        assert!(
            dir.join("manifest.toml").exists(),
            "fixture corpus missing at {} — run `cargo xtask fixtures` first",
            dir.display()
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

    fn insert_image(
        catalog: &Catalog,
        filename: &str,
        content_hash: ContentHash,
    ) -> (AssetId, ImageId) {
        let root = catalog
            .writer()
            .with_txn({
                let p = fixtures_dir();
                move |txn| txn.upsert_root(None, &p)
            })
            .unwrap();
        let folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, ""))
            .unwrap();
        let asset = catalog
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
            .unwrap();
        let image = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();
        (asset, image)
    }

    fn no_op_events() -> EventSink {
        Arc::new(|_ev| {})
    }

    /// T19 AC: eviction respects tier order, an over-cap store with T0/T1
    /// rows evicts T1 (LRU-oldest first within the tier) before it ever
    /// touches T0.
    #[test]
    fn eviction_respects_tier_order_t1_before_t0() {
        let h = harness();
        let content_hash = ContentHash([30; 16]);
        let (asset, image) = insert_image(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let t0 = producer::ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        let t1 = producer::ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            crate::config::StandardSize::Fixed(1600),
            crate::config::Codec::Jpeg,
            90,
        )
        .unwrap();
        let total = t0.bytes + t1.bytes;

        // Cap just under the total: exactly one row must go. It must be T1.
        let report = evict_to_cap(&h.store, &h.catalog, &h.index, total - 1, &no_op_events());
        assert_eq!(report.rows_deleted, 1, "{report:?}");
        assert!(report.bytes_reclaimed > 0);

        let stats: Vec<_> = h.catalog.reader().all_preview_rows().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].tier, 0, "T0 must survive; T1 was evicted first");
        assert!(!h
            .store
            .resolve(&RelPath(t1.store_path.as_str().to_owned()))
            .exists());
        assert!(h
            .store
            .resolve(&RelPath(t0.store_path.as_str().to_owned()))
            .exists());
    }

    /// T19 AC (stress): a reader holding the T0 file open (via
    /// `Store::begin_read`) during an eviction pass must never have its file
    /// unlinked out from under it, the row is still deleted (catalog is the
    /// source of truth), but the FILE stays until the guard drops, then a
    /// second sweep reclaims it.
    #[test]
    fn eviction_never_unlinks_a_file_with_a_live_read_guard() {
        let h = harness();
        let content_hash = ContentHash([31; 16]);
        let (asset, image) = insert_image(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");
        let t0 = producer::ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
        )
        .unwrap();

        let guard = h.store.begin_read(&t0.store_path);
        let report = evict_to_cap(&h.store, &h.catalog, &h.index, 0, &no_op_events());
        assert_eq!(report.rows_deleted, 1, "the row is still deleted");
        assert_eq!(report.skipped_live, 1, "{report:?}");
        let abs = h.store.resolve(&t0.store_path);
        assert!(
            abs.exists(),
            "the live-referenced file must survive the sweep"
        );
        drop(guard);

        // Row is already gone from the catalog; a fresh sweep (any cap) now
        // finds no candidate row left to re-delete, but the ORPHANED file
        // (row deleted, file still present) is exactly what
        // `verify_store(Full)`'s orphan sweep (T21) reconciles, not this
        // function's job on a second call with no matching row.
        assert!(
            h.catalog.reader().all_preview_rows().unwrap().is_empty(),
            "the row must not resurrect"
        );
    }

    /// T19 AC: `DiscardPreviews` removes rows+files for a selection; T0 is
    /// asset-scoped, so discarding it for one virtual copy discards the
    /// SHARED row for both.
    #[test]
    fn discard_removes_selected_tiers_and_shares_t0_across_virtual_copies() {
        let h = harness();
        let content_hash = ContentHash([32; 16]);
        let path = fixtures_dir().join("canon-eos-350d.cr2");
        let (asset, image_a) = insert_image(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image_b = h
            .catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();

        producer::ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image_a,
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        producer::ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image_a,
            asset,
            content_hash,
            &path,
            crate::config::StandardSize::Fixed(1600),
            crate::config::Codec::Jpeg,
            90,
        )
        .unwrap();
        assert_eq!(h.catalog.reader().all_preview_rows().unwrap().len(), 2);

        // Discard T0 through image_b (a different virtual copy of the same
        // asset), the shared row must vanish, T1 (image_a-scoped) survives.
        let report = discard(
            &h.store,
            &h.catalog,
            &h.index,
            &no_op_events(),
            &[image_b],
            TierSet::only(Tier::T0),
        );
        assert_eq!(report.rows_deleted, 1, "{report:?}");
        let remaining = h.catalog.reader().all_preview_rows().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].tier, 1);
    }

    /// T19 AC: T2 rows past retention are swept; rows within retention
    /// survive.
    #[test]
    fn t2_retention_sweep_removes_only_rows_past_the_age_cutoff() {
        let h = harness();
        let content_hash = ContentHash([33; 16]);
        let (asset, image) = insert_image(&h.catalog, "synthetic.dng", content_hash);
        let desc = crate::t2::ensure_t2_synthetic(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            600,
            500,
            256,
        )
        .unwrap();
        let t2_dir = h.store.resolve(&desc.store_path);
        assert!(t2_dir.is_dir());

        let now = crate::index::now_unix_seconds();
        // Well within a 30-day retention: no-op.
        let report = sweep_t2_retention(
            &h.store,
            &h.catalog,
            &h.index,
            &no_op_events(),
            Retention::Days(30),
            now,
        );
        assert_eq!(report.rows_deleted, 0, "{report:?}");
        assert!(t2_dir.is_dir());

        // Retention of 0 days with "now" as the cutoff moment: built_at (a
        // few milliseconds ago) is < now, so it is swept.
        let report = sweep_t2_retention(
            &h.store,
            &h.catalog,
            &h.index,
            &no_op_events(),
            Retention::Days(0),
            now + 1,
        );
        assert_eq!(report.rows_deleted, 1, "{report:?}");
        assert!(!t2_dir.exists(), "the whole tile directory must be removed");

        // `Retention::Never` is always a no-op, even against ancient rows.
        let _ = Path::new(""); // silence unused import warning path variance across cfgs
    }
}
