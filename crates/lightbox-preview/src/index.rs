// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The in-RAM preview index + touch batcher (E03 spec §3.2/§5.2, Phase A
//! T04): a full mirror of the catalog's `preview` table kept in memory so
//! `best_available` never touches disk (spec §5.2: "Sync, in-memory index
//! only... Never does IO. p99 < 1 ms at 100k").
//!
//! **Seam note on `best_available`'s signature.** The spec names
//! `best_available(&self, image: ImageId, min_long_edge: u32)`. This index
//! additionally takes `asset: AssetId` explicitly. Reason: every asset-scope
//! (T0) row carries only `asset_id` (no `image_id` — that is what "asset
//! scope" means), so an image whose asset has ONLY a T0 built (no T1/T2 ever
//! requested) cannot be resolved to its owning asset from the `preview`
//! table alone — that mapping lives in the catalog's `image` table, which is
//! E01's, not E03's, to mirror. Phase D's `PreviewService::best_available`
//! (spec §5.2, T14) is expected to match the exact spec signature by
//! resolving `asset` from the image context it already has at the call site
//! (e.g. `ImageDetail::asset`) and delegating here — zero added IO on that
//! path since the resolution is already in hand, not a fresh catalog query.
//! Recorded in `docs/plan/epics/E03-deviations.md`.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lightbox_catalog::{PreviewRow, PreviewSourceTag, ReaderHandle};
use lightbox_types::{AssetId, ImageId, PreviewId};

use crate::pyramid::{PreviewColorspace, PreviewDesc, PreviewScope, PreviewSource, RelPath, Tier};
use crate::PreviewError;

/// One RAM-resident mirror of a `preview` row (spec §5.1 `PreviewDesc`, plus
/// the id needed to flush touches and the owning ids needed to file the
/// entry under both `by_image`/`by_asset`).
#[derive(Clone, Debug)]
struct Entry {
    id: PreviewId,
    asset: AssetId,
    image: Option<ImageId>,
    tier: Tier,
    desc: PreviewDesc,
}

fn to_desc(row: &PreviewRow) -> Option<PreviewDesc> {
    let tier = Tier::from_u8(row.tier)?;
    let scope = match row.image {
        Some(image) => PreviewScope::Image(image),
        None => PreviewScope::Asset(row.asset),
    };
    Some(PreviewDesc {
        scope,
        tier,
        // Little-endian: the catalog's `variant_hash` BLOB is pure internal
        // cache-key material (never hex-displayed to a user or cross-tool),
        // unlike the content-hash-shaped xxh3-128 values elsewhere in this
        // codebase that standardize on big-endian for `xxhsum`/fixture-pin
        // compatibility (see `lightbox_decode::hash_file`'s doc comment).
        // Phase B/C, when they start calling `upsert_preview` with a real
        // `VariantParams::variant_hash()`, must serialize with
        // `.to_le_bytes()` to match this reader.
        variant: crate::pyramid::VariantHash(u64::from_le_bytes(row.variant_hash)),
        source: match row.source {
            PreviewSourceTag::Embedded => PreviewSource::Embedded,
            PreviewSourceTag::Rendered => PreviewSource::Rendered,
        },
        recipe_rev: row.recipe_rev,
        stale: row.stale,
        width: row.width,
        height: row.height,
        colorspace: if row.colorspace == "srgb" {
            PreviewColorspace::Srgb
        } else {
            PreviewColorspace::TaggedIcc
        },
        store_path: RelPath(row.store_path.clone()),
        bytes: row.bytes,
        built_at: row.built_at,
    })
}

/// Touch-batching thresholds (spec §3.2: "batched in memory and flushed...
/// every ≤5s or ≥64 entries").
const TOUCH_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const TOUCH_FLUSH_COUNT: usize = 64;

/// The in-RAM preview index (spec §5.2 surface, Phase-A-owned half). Phase
/// D's `PreviewService` wraps this with the scheduler/decoded-LRU/producer
/// dispatch that make it the full facade.
pub struct PreviewIndex {
    by_image: HashMap<ImageId, Vec<Entry>>,
    by_asset: HashMap<AssetId, Vec<Entry>>,
    by_id: HashMap<PreviewId, (Option<ImageId>, AssetId, usize)>,
    pending_touches: HashMap<PreviewId, i64>,
    last_flush: Instant,
}

impl PreviewIndex {
    /// Hydrates the index from the catalog's full `preview` table (spec
    /// T04: "bulk hydration... at startup" — the one-time cost the p99
    /// budget does not count against).
    pub fn load(reader: &ReaderHandle) -> Result<PreviewIndex, PreviewError> {
        let rows = reader
            .all_preview_rows()
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let mut index = PreviewIndex::empty();
        for row in &rows {
            index.upsert(row);
        }
        Ok(index)
    }

    /// An index with no rows (tests, or a store with nothing built yet).
    pub fn empty() -> PreviewIndex {
        PreviewIndex {
            by_image: HashMap::new(),
            by_asset: HashMap::new(),
            by_id: HashMap::new(),
            pending_touches: HashMap::new(),
            last_flush: Instant::now(),
        }
    }

    /// Inserts or replaces the in-RAM mirror of one catalog row (called
    /// after a successful `CatalogTxn::upsert_preview` — Phase B/C/D's write
    /// path). A row with an unrecognized `tier` is ignored (defensive; the
    /// catalog's own DDL already constrains this to `0..=2`).
    pub fn upsert(&mut self, row: &PreviewRow) {
        let Some(desc) = to_desc(row) else { return };
        let entry = Entry {
            id: row.id,
            asset: row.asset,
            image: row.image,
            tier: desc.tier,
            desc,
        };
        self.remove(row.id); // idempotent: replace any prior copy first
        let list = match row.image {
            Some(image) => self.by_image.entry(image).or_default(),
            None => self.by_asset.entry(row.asset).or_default(),
        };
        list.push(entry);
        let pos = list.len() - 1;
        self.by_id.insert(row.id, (row.image, row.asset, pos));
    }

    /// Drops one row from the RAM mirror (Phase F's eviction consumer).
    pub fn remove(&mut self, id: PreviewId) {
        let Some((image, asset, _pos)) = self.by_id.remove(&id) else {
            return;
        };
        let list = match image {
            Some(image) => self.by_image.get_mut(&image),
            None => self.by_asset.get_mut(&asset),
        };
        if let Some(list) = list {
            list.retain(|e| e.id != id);
            // Re-index the survivors' stored positions (small lists — T0/T1/T2
            // per scope is at most a handful of live variants in practice).
            // Collect first so this loop does not hold `list`'s borrow of
            // `self.by_image`/`self.by_asset` open while mutating `self.by_id`.
            let reindexed: Vec<(PreviewId, Option<ImageId>, AssetId, usize)> = list
                .iter()
                .enumerate()
                .map(|(i, e)| (e.id, e.image, e.asset, i))
                .collect();
            for (pid, img, ast, i) in reindexed {
                self.by_id.insert(pid, (img, ast, i));
            }
        }
        self.pending_touches.remove(&id);
    }

    /// In-memory stale-mark mirror of `CatalogTxn::mark_rendered_stale_below`
    /// (spec §5.2 `invalidate_image`'s in-RAM half; Phase D wires both sides
    /// together in one call). Embedded rows are never staled (spec §3.1).
    pub fn mark_rendered_stale_below(&mut self, image: ImageId, new_recipe_rev: u64) {
        if let Some(list) = self.by_image.get_mut(&image) {
            for e in list.iter_mut() {
                if e.desc.source == PreviewSource::Rendered && e.desc.recipe_rev < new_recipe_rev {
                    e.desc.stale = true;
                }
            }
        }
    }

    /// Spec §5.2: "Best existing tier/variant with long edge >= min_long_edge
    /// (or the largest available if none reach it). Never does IO." Ranking
    /// among candidates that satisfy the minimum: highest tier (T2 > T1 >
    /// T0, more fidelity), then non-stale over stale, then higher
    /// `recipe_rev`, then more recently built — each a documented,
    /// deliberate reading of "best" (the spec states the minimum-coverage
    /// rule but not the tie-break; see `pyramid.rs`/module docs for the
    /// `asset` parameter's own deviation note).
    pub fn best_available(
        &self,
        image: ImageId,
        asset: AssetId,
        min_long_edge: u32,
    ) -> Option<PreviewDesc> {
        let empty = Vec::new();
        let image_rows = self.by_image.get(&image).unwrap_or(&empty);
        let asset_rows = self.by_asset.get(&asset).unwrap_or(&empty);
        let candidates = image_rows.iter().chain(asset_rows.iter());

        let long_edge = |e: &Entry| e.desc.width.max(e.desc.height);
        let rank = |e: &Entry| {
            (
                e.tier as u8,
                !e.desc.stale,
                e.desc.recipe_rev,
                e.desc.built_at,
            )
        };

        let covering = candidates
            .clone()
            .filter(|e| long_edge(e) >= min_long_edge)
            .max_by_key(|e| rank(e));
        let chosen = covering.or_else(|| candidates.max_by_key(|e| (long_edge(e), rank(e))));
        chosen.map(|e| e.desc.clone())
    }

    /// Exact `(tier, variant)` point lookup within one image's asset/image
    /// scoped rows (Phase C, T12) — unlike [`Self::best_available`]'s
    /// ranked "pick the single best across every live variant" semantics,
    /// this looks for ONE specific variant. T0 never needed this (spec
    /// §3.1: exactly one canonical `VariantParams` per asset, so "any T0
    /// row" already means "the T0 row"); T1 varies by target size/codec/
    /// quality, so `producer::ensure_t1`'s re-entrancy fast path must ask
    /// for the exact variant it is about to build, not whichever T1 row
    /// happens to rank highest overall. Still sync/IO-free (spec §5.2).
    pub fn lookup_variant(
        &self,
        image: ImageId,
        asset: AssetId,
        tier: Tier,
        variant: crate::pyramid::VariantHash,
    ) -> Option<PreviewDesc> {
        let empty = Vec::new();
        let image_rows = self.by_image.get(&image).unwrap_or(&empty);
        let asset_rows = self.by_asset.get(&asset).unwrap_or(&empty);
        image_rows
            .iter()
            .chain(asset_rows.iter())
            .find(|e| e.tier == tier && e.desc.variant == variant)
            .map(|e| e.desc.clone())
    }

    /// Records a touch (spec §3.2): queues the id for the next flush.
    /// `now_unix` is the caller's timestamp (usually "now" — see
    /// [`now_unix_seconds`]) so tests can drive it deterministically. Note
    /// `PreviewDesc` (spec §5.1) carries no `last_used_at` field — only
    /// `built_at` — so there is nothing to update on the RAM-resident
    /// [`PreviewDesc`] itself; the catalog row's `last_used_at` is what
    /// `last_flush` ultimately advances, via `drain_pending_touches`.
    /// Touching an id this index has never seen is harmless (dropped at
    /// flush time by the DAO's own tolerance, `touch_previews_last_used`).
    pub fn touch(&mut self, id: PreviewId, now_unix: i64) {
        self.pending_touches.insert(id, now_unix);
    }

    /// Spec §3.2: flush when ≥64 pending touches OR ≥5s since the last
    /// flush (and at least one is pending — an idle index never "flushes
    /// nothing" on a timer).
    pub fn should_flush(&self) -> bool {
        !self.pending_touches.is_empty()
            && (self.pending_touches.len() >= TOUCH_FLUSH_COUNT
                || self.last_flush.elapsed() >= TOUCH_FLUSH_INTERVAL)
    }

    /// Drains the pending-touch set for a flush (the caller passes the ids
    /// to `CatalogTxn::touch_previews_last_used`). Resets the flush clock
    /// regardless of whether the caller's write actually succeeds — a
    /// dropped batch on a catalog error is no worse than one lost to a kill
    /// (spec §3.2: "losing unflushed touches in a crash is harmless").
    pub fn drain_pending_touches(&mut self) -> Vec<PreviewId> {
        self.last_flush = Instant::now();
        self.pending_touches.drain().map(|(id, _)| id).collect()
    }

    /// Number of rows currently mirrored (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl Default for PreviewIndex {
    fn default() -> PreviewIndex {
        PreviewIndex::empty()
    }
}

/// Current time as unix seconds — the convenience "now" for
/// [`PreviewIndex::touch`] callers that don't need a deterministic clock.
pub fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_catalog::NewPreviewRow;
    use lightbox_types::ContentHash;

    /// Test-only synthetic row builder. Bundles the less-interesting fields
    /// (`tier`/`stale`/`source`/`recipe_rev`/`built_at`) into `RowShape` so
    /// clippy's `too_many_arguments` stays honest instead of suppressed.
    struct RowShape {
        tier: u8,
        stale: bool,
        source: PreviewSourceTag,
        recipe_rev: u64,
        built_at: i64,
    }

    fn row(id: i64, asset: i64, image: Option<i64>, w: u32, h: u32, shape: RowShape) -> PreviewRow {
        PreviewRow {
            id: PreviewId(id),
            asset: AssetId(asset),
            image: image.map(ImageId),
            content_hash: ContentHash([0; 16]),
            tier: shape.tier,
            variant_hash: (shape.tier as u64).to_le_bytes(),
            source: shape.source,
            recipe_rev: shape.recipe_rev,
            stale: shape.stale,
            colorspace: "srgb".to_owned(),
            store_path: format!("previews/aa/row{id}"),
            width: w,
            height: h,
            bytes: 1,
            checksum: [0; 8],
            built_at: shape.built_at,
            last_used_at: shape.built_at,
        }
    }

    fn shape(
        tier: u8,
        stale: bool,
        source: PreviewSourceTag,
        recipe_rev: u64,
        built_at: i64,
    ) -> RowShape {
        RowShape {
            tier,
            stale,
            source,
            recipe_rev,
            built_at,
        }
    }

    #[test]
    fn empty_index_returns_none() {
        let idx = PreviewIndex::empty();
        assert_eq!(idx.best_available(ImageId(1), AssetId(1), 0), None);
    }

    #[test]
    fn prefers_higher_tier_when_both_cover_the_minimum() {
        let mut idx = PreviewIndex::empty();
        idx.upsert(&row(
            1,
            1,
            None,
            200,
            150,
            shape(0, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        idx.upsert(&row(
            2,
            1,
            Some(1),
            3840,
            2560,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        let best = idx.best_available(ImageId(1), AssetId(1), 100).unwrap();
        assert_eq!(best.tier, Tier::T1);
    }

    #[test]
    fn falls_back_to_largest_available_when_nothing_covers_the_minimum() {
        let mut idx = PreviewIndex::empty();
        idx.upsert(&row(
            1,
            1,
            None,
            160,
            120,
            shape(0, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        idx.upsert(&row(
            2,
            1,
            Some(1),
            320,
            240,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        let best = idx.best_available(ImageId(1), AssetId(1), 10_000).unwrap();
        assert_eq!(
            best.tier,
            Tier::T1,
            "the larger of the two, even though neither covers the minimum"
        );
    }

    #[test]
    fn prefers_non_stale_over_stale_at_the_same_tier() {
        let mut idx = PreviewIndex::empty();
        // Two T1 rows for the same image at different variants: one stale, one not.
        let mut fresh = row(
            1,
            1,
            Some(1),
            3840,
            2560,
            shape(1, false, PreviewSourceTag::Rendered, 5, 1),
        );
        fresh.variant_hash = [9; 8];
        let mut stale = row(
            2,
            1,
            Some(1),
            3840,
            2560,
            shape(1, true, PreviewSourceTag::Rendered, 3, 2),
        );
        stale.variant_hash = [8; 8];
        idx.upsert(&stale);
        idx.upsert(&fresh);
        let best = idx.best_available(ImageId(1), AssetId(1), 100).unwrap();
        assert!(!best.stale);
        assert_eq!(best.recipe_rev, 5);
    }

    #[test]
    fn asset_scope_t0_is_visible_when_asset_is_supplied() {
        let mut idx = PreviewIndex::empty();
        idx.upsert(&row(
            1,
            7,
            None,
            500,
            400,
            shape(0, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        assert_eq!(
            idx.best_available(ImageId(99), AssetId(7), 100)
                .unwrap()
                .tier,
            Tier::T0
        );
        // A different asset id sees nothing.
        assert_eq!(idx.best_available(ImageId(99), AssetId(8), 100), None);
    }

    /// T12: `lookup_variant` finds the EXACT variant even when a
    /// different-variant row of the same tier would outrank it under
    /// `best_available`'s ranking (higher `recipe_rev`/`built_at`).
    #[test]
    fn lookup_variant_finds_the_exact_variant_not_just_the_best_ranked_one() {
        let mut idx = PreviewIndex::empty();
        let mut older = row(
            1,
            1,
            Some(1),
            1280,
            853,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        );
        older.variant_hash = [1; 8];
        let mut newer = row(
            2,
            1,
            Some(1),
            3840,
            2560,
            shape(1, false, PreviewSourceTag::Embedded, 0, 99),
        );
        newer.variant_hash = [2; 8];
        idx.upsert(&older);
        idx.upsert(&newer);

        // best_available would pick the newer/larger row...
        let best = idx.best_available(ImageId(1), AssetId(1), 0).unwrap();
        assert_eq!(
            best.variant,
            crate::pyramid::VariantHash(u64::from_le_bytes([2; 8]))
        );

        // ...but lookup_variant finds the OLDER variant specifically, when asked.
        let exact = idx
            .lookup_variant(
                ImageId(1),
                AssetId(1),
                Tier::T1,
                crate::pyramid::VariantHash(u64::from_le_bytes([1; 8])),
            )
            .unwrap();
        assert_eq!((exact.width, exact.height), (1280, 853));

        // A variant that was never built: None, not a fallback guess.
        assert!(idx
            .lookup_variant(
                ImageId(1),
                AssetId(1),
                Tier::T1,
                crate::pyramid::VariantHash(u64::from_le_bytes([9; 8])),
            )
            .is_none());
    }

    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let mut idx = PreviewIndex::empty();
        idx.upsert(&row(
            1,
            1,
            Some(1),
            100,
            100,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        assert_eq!(idx.len(), 1);
        let mut updated = row(
            1,
            1,
            Some(1),
            200,
            200,
            shape(1, false, PreviewSourceTag::Embedded, 0, 2),
        );
        updated.variant_hash = [1; 8]; // same key as the first insert's default
        idx.upsert(&updated);
        assert_eq!(idx.len(), 1, "same id must replace in place, not duplicate");
    }

    #[test]
    fn remove_drops_the_row_and_touch_state() {
        let mut idx = PreviewIndex::empty();
        idx.upsert(&row(
            1,
            1,
            Some(1),
            100,
            100,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        ));
        idx.touch(PreviewId(1), 42);
        idx.remove(PreviewId(1));
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.best_available(ImageId(1), AssetId(1), 0), None);
        let drained = idx.drain_pending_touches();
        assert!(
            drained.is_empty(),
            "removed rows must not linger in the touch queue"
        );
    }

    #[test]
    fn mark_rendered_stale_below_only_affects_rendered_rows_under_the_revision() {
        let mut idx = PreviewIndex::empty();
        let mut embedded = row(
            1,
            1,
            Some(1),
            100,
            100,
            shape(1, false, PreviewSourceTag::Embedded, 0, 1),
        );
        embedded.variant_hash = [1; 8];
        let mut rendered_old = row(
            2,
            1,
            Some(1),
            100,
            100,
            shape(1, false, PreviewSourceTag::Rendered, 1, 1),
        );
        rendered_old.variant_hash = [2; 8];
        let mut rendered_new = row(
            3,
            1,
            Some(1),
            100,
            100,
            shape(1, false, PreviewSourceTag::Rendered, 5, 1),
        );
        rendered_new.variant_hash = [3; 8];
        idx.upsert(&embedded);
        idx.upsert(&rendered_old);
        idx.upsert(&rendered_new);

        idx.mark_rendered_stale_below(ImageId(1), 5);

        let list = idx.by_image.get(&ImageId(1)).unwrap();
        let stale_by_id: HashMap<i64, bool> = list.iter().map(|e| (e.id.0, e.desc.stale)).collect();
        assert!(!stale_by_id[&1], "embedded rows are never staled");
        assert!(stale_by_id[&2]);
        assert!(!stale_by_id[&3], "at or above the new revision stays fresh");
    }

    #[test]
    fn touch_batching_flushes_at_count_threshold_not_before() {
        let mut idx = PreviewIndex::empty();
        for i in 0..63 {
            idx.touch(PreviewId(i), 100);
            assert!(
                !idx.should_flush(),
                "must not flush before the count threshold (i={i})"
            );
        }
        idx.touch(PreviewId(1000), 100);
        assert!(
            idx.should_flush(),
            "64th pending touch must trip the count threshold"
        );
        let drained = idx.drain_pending_touches();
        assert_eq!(drained.len(), 64);
        assert!(
            !idx.should_flush(),
            "an empty pending set must never report flush-worthy"
        );
    }

    #[test]
    fn touch_batching_flushes_after_the_time_threshold() {
        let mut idx = PreviewIndex::empty();
        idx.touch(PreviewId(1), 100);
        assert!(!idx.should_flush(), "one touch, no time elapsed: must wait");
        idx.last_flush =
            Instant::now() - (TOUCH_FLUSH_INTERVAL + std::time::Duration::from_millis(1));
        assert!(
            idx.should_flush(),
            "past the time threshold with pending touches must flush"
        );
    }

    /// Spec §6 budget / T04 AC: `best_available` p99 < 1 ms over 100k
    /// synthetic rows, sync in-memory only.
    #[test]
    fn best_available_meets_the_p99_budget_at_100k_rows() {
        let mut idx = PreviewIndex::empty();
        let n_images = 100_000u32;
        for i in 0..n_images {
            let asset = i as i64;
            let image = i as i64;
            idx.upsert(&row(
                i as i64 * 2 + 1,
                asset,
                None,
                200,
                150,
                shape(0, false, PreviewSourceTag::Embedded, 0, 1),
            ));
            idx.upsert(&row(
                i as i64 * 2 + 2,
                asset,
                Some(image),
                3840,
                2560,
                shape(1, false, PreviewSourceTag::Embedded, 0, 1),
            ));
        }
        assert_eq!(idx.len() as u32, n_images * 2);

        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(2000);
        for i in 0..2000u32 {
            let image = ImageId((i % n_images) as i64);
            let asset = AssetId((i % n_images) as i64);
            let start = Instant::now();
            let got = idx.best_available(image, asset, 1280);
            samples.push(start.elapsed());
            assert!(got.is_some());
        }
        samples.sort();
        let p99 = samples[(samples.len() as f64 * 0.99) as usize - 1];
        assert!(
            p99 < std::time::Duration::from_millis(1),
            "p99 lookup latency {p99:?} exceeds the 1ms budget (spec §6)"
        );
    }

    /// T04 AC: hydrating the RAM mirror from a real catalog round-trips.
    #[test]
    fn load_hydrates_from_a_real_catalog() {
        let dir = tempfile::TempDir::new().unwrap();
        let catalog = lightbox_catalog::Catalog::create(&dir.path().join("t.lbdata")).unwrap();
        let (root, folder) = (
            catalog
                .writer()
                .with_txn({
                    let p = dir.path().join("photos");
                    move |txn| txn.upsert_root(None, &p)
                })
                .unwrap(),
            0,
        );
        let _ = folder;
        let folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, "shoot"))
            .unwrap();
        let asset = catalog
            .writer()
            .with_txn(move |txn| {
                let batch = vec![lightbox_catalog::NewAsset {
                    folder,
                    filename: "a.jpg".to_owned(),
                    content_hash: ContentHash([1; 16]),
                    format: "JPEG".to_owned(),
                    camera_make: None,
                    camera_model: None,
                    capture_time: None,
                    width: 100,
                    height: 100,
                    orientation: lightbox_types::Orientation::O1,
                    bytes: 10,
                    mtime_utc: None,
                    decode_error: None,
                    import_session: None,
                }];
                Ok(txn.insert_assets(&batch)?.inserted[0])
            })
            .unwrap();
        let image = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();
        catalog
            .writer()
            .with_txn(move |txn| {
                txn.upsert_preview(NewPreviewRow {
                    asset,
                    image: None,
                    content_hash: ContentHash([1; 16]),
                    tier: 0,
                    variant_hash: [1; 8],
                    source: PreviewSourceTag::Embedded,
                    recipe_rev: 0,
                    colorspace: "srgb".to_owned(),
                    store_path: "previews/aa/x.t0.jpg".to_owned(),
                    width: 100,
                    height: 100,
                    bytes: 10,
                    checksum: [0; 8],
                })
            })
            .unwrap();

        let idx = PreviewIndex::load(&catalog.reader()).unwrap();
        assert_eq!(idx.len(), 1);
        let best = idx.best_available(image, asset, 0).unwrap();
        assert_eq!(best.tier, Tier::T0);
    }
}
