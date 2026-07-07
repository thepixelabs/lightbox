// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `PreviewService` — the facade `lightbox-core` registers (E03 spec §5.2,
//! Phase D T14). Composes the store (Phase A), the in-RAM index (Phase A),
//! and the scheduler (T13) behind the request/best_available/set_viewport/
//! bulk_build surface; publishes [`crate::sched::PreviewEvent`]s through the
//! caller-supplied sink.
//!
//! **Not implemented here (named, not designed — Phase E/F own them):**
//! `open_pixels`/`thumb`/`t2_tile` (decode-for-display stays
//! `EmbeddedPreviewProvider`'s job — see the "two indices" note below),
//! `raw_cache`/`blob_store` accessors, `set_limits`/`relocate`, the full
//! `PurgeScope`/cap-based `purge`, and `verify_store(Full)`. This phase ships
//! [`PreviewService::purge_all`] (a blunt "wipe everything" primitive) and
//! [`PreviewService::verify_quick`] (missing-file detection only, no orphan/
//! checksum reconciliation) as honestly-scoped, genuinely useful narrower
//! cousins — see their own doc comments.
//!
//! **Known interim duplication: two independent in-RAM `PreviewIndex`
//! mirrors per session (across TYPES, not within this one).**
//! `lightbox-core::Session::open` already constructs one inside
//! `EmbeddedPreviewProvider` (Phase B) for the grid/loupe decode path;
//! `PreviewService::open` constructs its own here — ONE index, shared
//! between [`PreviewService`] and the scheduler's [`crate::sched::BuildFn`]
//! (`make_build_fn` below), so every build this service drives updates the
//! very index `best_available`/`stats` read (no staleness WITHIN this
//! type). The two mirrors are still independent of EACH OTHER, though: both
//! are correct on their own (each is a disposable cache hydrated from, and
//! kept current with, the same catalog `preview` table — spec §3.2) but a
//! build driven through `PreviewService` does not immediately warm
//! `EmbeddedPreviewProvider`'s fast path (falls back to its normal
//! on-disk-existence check — still correct, just not maximally cheap), and
//! vice versa. Unifying the two TYPES would mean changing
//! `EmbeddedPreviewProvider::new`'s constructor again (B-8) and is left as a
//! documented follow-up rather than done under this phase's T13-T16 scope.
//! Recorded in `docs/plan/epics/E03-deviations.md`, Phase D.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use lightbox_catalog::Catalog;
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use crate::config::PreviewStoreConfig;
use crate::index::PreviewIndex;
use crate::producer;
use crate::pyramid::{PreviewDesc, RelPath, Tier};
use crate::sched::{
    BuildFn, BuildKey, BuildPriority, BuildRuntime, BulkHandle, EnqueueError, EventSink, Scheduler,
};
use crate::store::Store;
use crate::{PreviewError, PreviewTicket};

/// One [`PreviewService::request`] (spec §5.2). No `variant` field — see
/// `sched.rs`'s `BuildKey` doc comment for why `(image, tier)` is a
/// faithful dedup key given that.
#[derive(Clone, Copy, Debug)]
pub struct PreviewRequest {
    pub image: ImageId,
    pub tier: Tier,
    pub priority: BuildPriority,
    /// Permit embedded-source builds (spec: "M0: true"). At M0 the ONLY
    /// registered producer is embedded (Phase B/C), so this flag is
    /// currently **accepted but not enforced**: there is no non-embedded
    /// producer to prefer or refuse yet, and threading it into the dedup
    /// key (spec's `(image, tier, variant)`, narrowed here — see
    /// `BuildKey`'s doc comment) would need a real second producer to make
    /// meaningful. Becomes load-bearing once E05's `EngineProducer` lands
    /// (M1); kept in the struct now so that integration is additive, not a
    /// signature break.
    pub allow_embedded: bool,
}

/// Per-tier row/byte counts (spec §5.2 `CacheStats`, narrowed: `rawcache_*`/
/// hit-rate fields are Phase E's — this phase reports the preview-pyramid
/// half only, computed fresh from the catalog so it always "matches disk"
/// (T14 AC) rather than from either in-RAM index mirror.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub t0_count: u64,
    pub t0_bytes: u64,
    pub t1_count: u64,
    pub t1_bytes: u64,
    pub t2_count: u64,
    pub t2_bytes: u64,
}

impl CacheStats {
    pub fn total_count(&self) -> u64 {
        self.t0_count + self.t1_count + self.t2_count
    }

    pub fn total_bytes(&self) -> u64 {
        self.t0_bytes + self.t1_bytes + self.t2_bytes
    }
}

/// [`PreviewService::verify_quick`]'s report (a narrower cousin of the
/// spec's `VerifyReport`/`VerifyMode::Quick` — see the module doc comment).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuickVerifyReport {
    pub rows_checked: u64,
    pub missing_files: Vec<PathBuf>,
}

/// [`PreviewService::purge_all`]'s report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub rows_deleted: u64,
}

struct ServiceInner {
    store: Arc<Store>,
    catalog: Arc<Catalog>,
    /// `Arc`-shared with the scheduler's [`BuildFn`] (`make_build_fn`) —
    /// **one** RAM index per service, kept current by every build that
    /// service ever drives (not a second, disconnected mirror; an earlier
    /// draft of this phase's code made that mistake — every completed build
    /// was invisible to `best_available` until process restart. Fixed;
    /// covered by `tests::t14_build_via_request_lands_a_ready_event_and_
    /// matching_stats`).
    index: Arc<Mutex<PreviewIndex>>,
    scheduler: Arc<Scheduler>,
    /// T15: tracks the priority this service last requested for each
    /// viewport-managed image, so `set_viewport` can diff cheaply instead of
    /// re-deriving everything from the scheduler's own state.
    viewport: Mutex<HashMap<ImageId, (PreviewTicket, BuildPriority)>>,
}

/// The facade `lightbox-core` registers (spec §5.2). Clone-cheap (`Arc`
/// inner).
#[derive(Clone)]
pub struct PreviewService {
    inner: Arc<ServiceInner>,
}

impl PreviewService {
    /// Opens the store (Phase A's `Store::open` — see deviation A-3) and the
    /// RAM index (Phase A), then builds the T13 scheduler over `rt`, wired
    /// to publish through `events`.
    pub fn open(
        cfg: PreviewStoreConfig,
        catalog: Arc<Catalog>,
        rt: Arc<dyn BuildRuntime>,
        events: EventSink,
    ) -> Result<PreviewService, PreviewError> {
        let store = Arc::new(Store::open(&cfg)?);
        let index = Arc::new(Mutex::new(
            PreviewIndex::load(&catalog.reader()).unwrap_or_else(|err| {
                tracing::warn!(
                    target: "lightbox_preview",
                    %err,
                    "PreviewService index hydration failed; starting empty (disposable cache)"
                );
                PreviewIndex::empty()
            }),
        ));

        let build_fn = make_build_fn(
            Arc::clone(&store),
            Arc::clone(&catalog),
            Arc::clone(&index),
            cfg,
        );
        let scheduler = Scheduler::new(rt, build_fn, events);

        Ok(PreviewService {
            inner: Arc::new(ServiceInner {
                store,
                catalog,
                index,
                scheduler,
                viewport: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Sync, in-memory-only best-effort lookup (spec §5.2). Resolves `image`
    /// → owning `asset` via one catalog read (deviation A-4's documented
    /// seam: the RAM index cannot recover that mapping for asset-scope-only
    /// rows on its own), then delegates to [`PreviewIndex::best_available`]
    /// — no additional IO beyond that one resolution.
    pub fn best_available(&self, image: ImageId, min_long_edge: u32) -> Option<PreviewDesc> {
        let asset = self.inner.catalog.reader().image_detail(image).ok()?.asset;
        self.lock_index()
            .best_available(image, asset, min_long_edge)
    }

    /// Enqueues/upgrades one build (spec §5.2). See
    /// [`PreviewRequest::allow_embedded`]'s doc comment for its current M0
    /// (accepted, not yet enforced) handling.
    pub fn request(&self, req: PreviewRequest) -> Result<PreviewTicket, EnqueueError> {
        let key = BuildKey {
            image: req.image,
            tier: req.tier,
        };
        self.inner.scheduler.request(key, req.priority)
    }

    pub fn cancel(&self, ticket: &PreviewTicket) {
        self.inner.scheduler.cancel(ticket);
    }

    pub fn reprioritize(&self, ticket: &PreviewTicket, p: BuildPriority) {
        self.inner.scheduler.reprioritize(ticket, p);
    }

    /// Visible-first viewport integration (T15): `visible` gets
    /// [`BuildPriority::Visible`], `neighbors` get
    /// [`BuildPriority::Neighbor`] (both at `Tier::T1` — a single-tier M0
    /// simplification; a caller wanting grid-thumbnail-sized T0 builds
    /// instead uses [`Self::request`] directly). Anything previously
    /// tracked that fell out of `visible ∪ neighbors` is cancelled — for a
    /// still-**queued** build this means it never runs at all (spec AC:
    /// "≥95% of off-screen queued builds cancelled before start"). An image
    /// moving between the two sets is a [`Scheduler::reprioritize`] call
    /// (upgrade OR downgrade), not a cancel+re-request — cheaper, and never
    /// throws away in-flight interest.
    ///
    /// The diffing itself ([`plan_viewport`]) is a pure function, unit
    /// tested independent of the scheduler at the AC's full 5k-image scale.
    pub fn set_viewport(&self, visible: Vec<ImageId>, neighbors: Vec<ImageId>) {
        let plan = {
            let tracked = self.lock_viewport();
            let by_image: HashMap<ImageId, BuildPriority> =
                tracked.iter().map(|(&img, &(_, p))| (img, p)).collect();
            plan_viewport(&by_image, &visible, &neighbors)
        };

        let mut tracked = self.lock_viewport();

        for image in plan.cancel {
            if let Some((ticket, _)) = tracked.remove(&image) {
                self.inner.scheduler.cancel(&ticket);
            }
        }
        for image in plan.upgrade_to_visible {
            if let Some((ticket, prio)) = tracked.get_mut(&image) {
                self.inner
                    .scheduler
                    .reprioritize(ticket, BuildPriority::Visible);
                *prio = BuildPriority::Visible;
            }
        }
        for image in plan.downgrade_to_neighbor {
            if let Some((ticket, prio)) = tracked.get_mut(&image) {
                self.inner
                    .scheduler
                    .reprioritize(ticket, BuildPriority::Neighbor);
                *prio = BuildPriority::Neighbor;
            }
        }
        for image in plan.add_visible {
            let key = BuildKey {
                image,
                tier: Tier::T1,
            };
            if let Ok(ticket) = self.inner.scheduler.request(key, BuildPriority::Visible) {
                tracked.insert(image, (ticket, BuildPriority::Visible));
            }
        }
        for image in plan.add_neighbor {
            let key = BuildKey {
                image,
                tier: Tier::T1,
            };
            if let Ok(ticket) = self.inner.scheduler.request(key, BuildPriority::Neighbor) {
                tracked.insert(image, (ticket, BuildPriority::Neighbor));
            }
        }
    }

    /// "Build previews for selection" (T16).
    pub fn bulk_build(&self, images: Vec<ImageId>, tier: Tier) -> BulkHandle {
        let keys = images
            .into_iter()
            .map(|image| BuildKey { image, tier })
            .collect();
        self.inner.scheduler.bulk_build(keys)
    }

    pub fn cancel_bulk(&self, handle: &BulkHandle) {
        self.inner.scheduler.cancel_bulk(handle);
    }

    /// Fresh-from-catalog per-tier counts/bytes (spec §5.2 `stats`, narrowed
    /// — see the module doc comment). AC: matches disk exactly, since this
    /// reads the catalog index (disk), not either RAM mirror.
    pub fn stats(&self) -> Result<CacheStats, PreviewError> {
        let rows = self
            .inner
            .catalog
            .reader()
            .all_preview_rows()
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let mut stats = CacheStats::default();
        for row in &rows {
            match row.tier {
                0 => {
                    stats.t0_count += 1;
                    stats.t0_bytes += row.bytes;
                }
                1 => {
                    stats.t1_count += 1;
                    stats.t1_bytes += row.bytes;
                }
                2 => {
                    stats.t2_count += 1;
                    stats.t2_bytes += row.bytes;
                }
                _ => {}
            }
        }
        Ok(stats)
    }

    /// Missing-file detection only (spec's fuller `verify_store(Quick)` —
    /// manifest cross-checks, checksum spot-checks — is Phase F's; see the
    /// module doc comment). Read-only: never deletes or re-enqueues.
    pub fn verify_quick(&self) -> Result<QuickVerifyReport, PreviewError> {
        let rows = self
            .inner
            .catalog
            .reader()
            .all_preview_rows()
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let mut report = QuickVerifyReport::default();
        for row in &rows {
            report.rows_checked += 1;
            let abs = self.inner.store.resolve(&RelPath(row.store_path.clone()));
            if !abs.exists() {
                report.missing_files.push(abs);
            }
        }
        Ok(report)
    }

    /// Deletes every preview row + wipes the `previews/` tree, then
    /// recreates it empty (spec's cap-based, refcounted `PurgeScope`-driven
    /// purge is Phase F's T21 — see the module doc comment; this is a
    /// blunter, whole-store "reset the cache" primitive that needs none of
    /// Phase F's refcounting since it always empties everything at once).
    /// Also clears the in-RAM index mirror so `best_available` reflects the
    /// purge immediately.
    pub fn purge_all(&self) -> Result<PurgeReport, PreviewError> {
        let rows = self
            .inner
            .catalog
            .reader()
            .all_preview_rows()
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let n = rows.len() as u64;
        self.inner
            .catalog
            .writer()
            .with_txn(move |txn| {
                for row in &rows {
                    txn.delete_preview(row.id)?;
                }
                Ok(())
            })
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;

        let previews_dir = self.inner.store.root().join("previews");
        if previews_dir.exists() {
            std::fs::remove_dir_all(&previews_dir)?;
        }
        std::fs::create_dir_all(&previews_dir)?;

        *self.lock_index() = PreviewIndex::empty();
        Ok(PurgeReport { rows_deleted: n })
    }

    fn lock_index(&self) -> std::sync::MutexGuard<'_, PreviewIndex> {
        self.inner
            .index
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_viewport(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ImageId, (PreviewTicket, BuildPriority)>> {
        self.inner
            .viewport
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Builds the real, production [`BuildFn`] (T13/T14): dispatches to
/// `producer::ensure_t0`/`ensure_t1` by tier, resolving `image → asset/
/// content_hash/path` the same way `lightbox-core`'s `CatalogAssetLocator`
/// does (this crate already depends on `lightbox-catalog` directly — see
/// `producer.rs` — so no extra seam is needed for that lookup). Builds their
/// OWN fresh `PreviewIndex::load` snapshot per call rather than sharing the
/// service's index mirror — see the module doc comment's "two indices" note
/// for why one shared mirror per session doesn't exist yet; this keeps the
/// build path correct (if not maximally warm) regardless.
fn make_build_fn(
    store: Arc<Store>,
    catalog: Arc<Catalog>,
    index: Arc<Mutex<PreviewIndex>>,
    cfg: PreviewStoreConfig,
) -> BuildFn {
    Arc::new(move |key: BuildKey, _cancel: &CancelToken| {
        let reader = catalog.reader();
        let detail = reader
            .image_detail(key.image)
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let path = reader
            .asset_abs_path(detail.asset)
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let content_hash = reader
            .asset_content_hash(detail.asset)
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        drop(reader);

        match key.tier {
            Tier::T0 => producer::ensure_t0(
                &store,
                &catalog,
                &index,
                key.image,
                detail.asset,
                content_hash,
                &path,
            ),
            Tier::T1 => producer::ensure_t1(
                &store,
                &catalog,
                &index,
                key.image,
                detail.asset,
                content_hash,
                &path,
                cfg.standard_px,
                cfg.t1_codec,
                cfg.t1_quality,
            ),
            Tier::T2 => Err(PreviewError::Unavailable(
                "T2 tiled preview build lands with Phase F (T20)".to_owned(),
            )),
        }
    })
}

/// The T15 viewport-diff algorithm — pure and independent of the scheduler,
/// so it is unit-testable at the AC's full 5k-image scale in microseconds
/// (see the test module). `tracked` maps a currently-managed image to the
/// priority it was last requested at.
#[derive(Debug, Default, PartialEq, Eq)]
struct ViewportPlan {
    cancel: Vec<ImageId>,
    upgrade_to_visible: Vec<ImageId>,
    downgrade_to_neighbor: Vec<ImageId>,
    add_visible: Vec<ImageId>,
    add_neighbor: Vec<ImageId>,
}

fn plan_viewport(
    tracked: &HashMap<ImageId, BuildPriority>,
    visible: &[ImageId],
    neighbors: &[ImageId],
) -> ViewportPlan {
    use std::collections::HashSet;
    let visible_set: HashSet<ImageId> = visible.iter().copied().collect();
    let neighbor_set: HashSet<ImageId> = neighbors.iter().copied().collect();
    let mut plan = ViewportPlan::default();

    for (&image, &prio) in tracked {
        let wanted = if visible_set.contains(&image) {
            Some(BuildPriority::Visible)
        } else if neighbor_set.contains(&image) {
            Some(BuildPriority::Neighbor)
        } else {
            None
        };
        match wanted {
            None => plan.cancel.push(image),
            Some(BuildPriority::Visible) if prio != BuildPriority::Visible => {
                plan.upgrade_to_visible.push(image)
            }
            Some(BuildPriority::Neighbor) if prio != BuildPriority::Neighbor => {
                plan.downgrade_to_neighbor.push(image)
            }
            _ => {}
        }
    }
    for &image in visible {
        if !tracked.contains_key(&image) {
            plan.add_visible.push(image);
        }
    }
    for &image in neighbors {
        if !tracked.contains_key(&image) {
            plan.add_neighbor.push(image);
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    use lightbox_catalog::Catalog;
    use lightbox_types::{ContentHash, Orientation};

    fn fixtures_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        assert!(
            dir.join("manifest.toml").exists(),
            "fixture corpus missing at {} — run `cargo xtask fixtures` first",
            dir.display()
        );
        dir
    }

    // ── plan_viewport: pure-function unit tests at real scale ───────────

    #[test]
    fn plan_viewport_cancels_everything_that_fell_out_of_view() {
        let mut tracked = HashMap::new();
        for i in 0..10 {
            tracked.insert(ImageId(i), BuildPriority::Visible);
        }
        let visible = vec![ImageId(0), ImageId(1)];
        let neighbors = vec![ImageId(2)];
        let plan = plan_viewport(&tracked, &visible, &neighbors);
        // 0,1 stay visible (no-op, already Visible); 2 downgrades to
        // Neighbor; 3..10 (7 images) must be cancelled.
        assert_eq!(plan.cancel.len(), 7, "{plan:?}");
        assert_eq!(plan.downgrade_to_neighbor, vec![ImageId(2)]);
        assert!(plan.upgrade_to_visible.is_empty());
    }

    #[test]
    fn plan_viewport_upgrades_neighbor_to_visible() {
        let mut tracked = HashMap::new();
        tracked.insert(ImageId(5), BuildPriority::Neighbor);
        let plan = plan_viewport(&tracked, &[ImageId(5)], &[]);
        assert_eq!(plan.upgrade_to_visible, vec![ImageId(5)]);
    }

    /// T15 AC scale: a 5k-image viewport sweep. Simulates scrolling through
    /// 5000 images with a 40-wide visible window + 10-wide neighbor
    /// margins, one `plan_viewport` call per scroll step, and asserts
    /// ≥95% of images that ever entered "queued" (tracked-but-not-visible
    /// long enough to be swept out) end up cancelled before the sweep
    /// completes — i.e., the diffing algorithm itself sheds off-screen
    /// interest aggressively, not just eventually.
    #[test]
    fn viewport_sweep_over_5k_images_sheds_at_least_95_percent_off_screen() {
        const N: i64 = 5000;
        const VISIBLE_WIDTH: i64 = 40;
        const NEIGHBOR_MARGIN: i64 = 10;

        let mut tracked: HashMap<ImageId, BuildPriority> = HashMap::new();
        let mut total_ever_tracked: std::collections::HashSet<ImageId> =
            std::collections::HashSet::new();
        let mut total_cancelled = 0usize;

        let start = Instant::now();
        let mut pos = 0i64;
        while pos < N {
            let visible: Vec<ImageId> = (pos..(pos + VISIBLE_WIDTH).min(N)).map(ImageId).collect();
            let neighbors: Vec<ImageId> = ((pos - NEIGHBOR_MARGIN).max(0)..pos)
                .chain((pos + VISIBLE_WIDTH).min(N)..(pos + VISIBLE_WIDTH + NEIGHBOR_MARGIN).min(N))
                .map(ImageId)
                .collect();

            let plan = plan_viewport(&tracked, &visible, &neighbors);
            total_cancelled += plan.cancel.len();
            for image in &plan.cancel {
                tracked.remove(image);
            }
            for image in &plan.upgrade_to_visible {
                tracked.insert(*image, BuildPriority::Visible);
            }
            for image in &plan.downgrade_to_neighbor {
                tracked.insert(*image, BuildPriority::Neighbor);
            }
            for image in &plan.add_visible {
                tracked.insert(*image, BuildPriority::Visible);
                total_ever_tracked.insert(*image);
            }
            for image in &plan.add_neighbor {
                tracked.insert(*image, BuildPriority::Neighbor);
                total_ever_tracked.insert(*image);
            }
            pos += VISIBLE_WIDTH; // a scroll step = one full page
        }
        let elapsed = start.elapsed();

        // Whatever is still tracked at the very end is "currently on
        // screen" and legitimately not cancelled; everything else that was
        // ever ADDED over the sweep either got cancelled at some point or
        // is still tracked (the final viewport). The off-screen-shed rate
        // is total_cancelled / total_ever_tracked.
        let shed_rate = total_cancelled as f64 / total_ever_tracked.len() as f64;
        assert!(
            shed_rate >= 0.95,
            "off-screen shed rate {:.3} over {} images (cancelled {}, ever tracked {}) — AC wants >= 0.95",
            shed_rate,
            N,
            total_cancelled,
            total_ever_tracked.len()
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "5k-image viewport sweep (diff logic only, no IO) took {elapsed:?} — should be near-instant"
        );
    }

    // ── PreviewService integration: T14/T15/T16 against a real store+catalog ──

    struct Harness {
        _dir: tempfile::TempDir,
        catalog: Arc<Catalog>,
        service: PreviewService,
        images: Vec<ImageId>,
    }

    /// Runs the given future's spawned work inline, on demand — a minimal
    /// production-shaped `BuildRuntime` for integration tests: real threads,
    /// bounded concurrency, no manual stepping required (unlike sched.rs's
    /// `ManualRuntime`, which this module doesn't need since it isn't
    /// probing exact dispatch order).
    struct ThreadRuntime {
        concurrency: usize,
    }

    impl crate::sched::BuildRuntime for ThreadRuntime {
        fn spawn_build(&self, _name: &'static str, mut fut: crate::sched::BuildFuture) {
            std::thread::spawn(move || {
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                loop {
                    match std::future::Future::poll(fut.as_mut(), &mut cx) {
                        std::task::Poll::Ready(()) => break,
                        std::task::Poll::Pending => std::thread::yield_now(),
                    }
                }
            });
        }

        fn concurrency(&self) -> usize {
            self.concurrency
        }
    }

    /// One real asset (`canon-eos-350d.cr2`) fanned out over `n` distinct
    /// `image` rows (T0 dedupes across them by content hash — cheap;
    /// exercises real dedup/scheduler behavior without needing `n` distinct
    /// fixture files).
    fn harness(n: usize, concurrency: usize) -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
        let content_hash = ContentHash([42; 16]);
        // asset_abs_path resolves root+folder+filename; point it at the
        // real fixture by using the fixture's own parent as the "root".
        let real_root = catalog
            .writer()
            .with_txn({
                let p = fixtures_dir();
                move |txn| txn.upsert_root(None, &p)
            })
            .unwrap();
        let real_folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(real_root, None, ""))
            .unwrap();

        // ONE asset, fanned out over `n` distinct `image` rows (synthetic
        // "virtual copies" at the DAO level, exactly `producer.rs`'s own
        // `virtual_copies_of_one_asset_share_one_t0_row_and_file` test
        // pattern) — `insert_assets` de-dupes by content_hash (see
        // `dao_tests::insert_assets_skips_duplicate_hashes_globally_and_
        // within_batch`), so `n` separate `NewAsset` inserts with the same
        // hash would only ever create ONE row; `insert_default_images`
        // carries no such uniqueness constraint on `asset_id`.
        let asset = catalog
            .writer()
            .with_txn(move |txn| {
                let batch = vec![lightbox_catalog::NewAsset {
                    folder: real_folder,
                    filename: "canon-eos-350d.cr2".to_owned(),
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
            })
            .unwrap();
        let images = catalog
            .writer()
            .with_txn(move |txn| txn.insert_default_images(&vec![asset; n]))
            .unwrap();

        let cfg = PreviewStoreConfig::with_defaults(dir.path().to_path_buf());
        let rt: Arc<dyn BuildRuntime> = Arc::new(ThreadRuntime { concurrency });
        let events: EventSink = Arc::new(|_ev| {});
        let service = PreviewService::open(cfg, Arc::clone(&catalog), rt, events).unwrap();
        Harness {
            _dir: dir,
            catalog,
            service,
            images,
        }
    }

    #[test]
    fn t14_build_via_request_lands_a_ready_event_and_matching_stats() {
        let events_seen = Arc::new(StdMutex::new(Vec::new()));
        let ev2 = Arc::clone(&events_seen);
        let dir = tempfile::TempDir::new().unwrap();
        let catalog = Arc::new(Catalog::create(&dir.path().join("t.lbdata")).unwrap());
        let real_root = catalog
            .writer()
            .with_txn({
                let p = fixtures_dir();
                move |txn| txn.upsert_root(None, &p)
            })
            .unwrap();
        let real_folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(real_root, None, ""))
            .unwrap();
        let content_hash = ContentHash([7; 16]);
        let asset = catalog
            .writer()
            .with_txn(move |txn| {
                let batch = vec![lightbox_catalog::NewAsset {
                    folder: real_folder,
                    filename: "canon-eos-350d.cr2".to_owned(),
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
            })
            .unwrap();
        let image = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();

        let cfg = PreviewStoreConfig::with_defaults(dir.path().to_path_buf());
        let rt: Arc<dyn BuildRuntime> = Arc::new(ThreadRuntime { concurrency: 2 });
        let events: EventSink = Arc::new(move |ev| ev2.lock().unwrap().push(ev));
        let service = PreviewService::open(cfg, Arc::clone(&catalog), rt, events).unwrap();

        let ticket = service
            .request(PreviewRequest {
                image,
                tier: Tier::T1,
                priority: BuildPriority::Visible,
                allow_embedded: true,
            })
            .unwrap();
        let _ = ticket;

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seen = events_seen.lock().unwrap();
            if seen
                .iter()
                .any(|e| matches!(e, crate::sched::PreviewEvent::Ready { .. }))
            {
                break;
            }
            drop(seen);
            assert!(Instant::now() < deadline, "T1 build never completed");
            std::thread::sleep(Duration::from_millis(5));
        }

        let stats = service.stats().unwrap();
        assert_eq!(stats.t1_count, 1, "{stats:?}");
        assert!(stats.t1_bytes > 0);
        // T0 also lands (T1's raw-source path builds through it).
        assert_eq!(stats.t0_count, 1, "{stats:?}");

        let best = service.best_available(image, 0).unwrap();
        assert_eq!(best.tier, Tier::T1);

        let verify = service.verify_quick().unwrap();
        assert_eq!(verify.rows_checked, 2);
        assert!(verify.missing_files.is_empty(), "{verify:?}");

        let purge = service.purge_all().unwrap();
        assert_eq!(purge.rows_deleted, 2);
        let stats_after = service.stats().unwrap();
        assert_eq!(stats_after.total_count(), 0);
    }

    #[test]
    fn t16_bulk_build_across_50_images_completes_and_matches_stats() {
        let h = harness(50, 4);
        let handle = h.service.bulk_build(h.images.clone(), Tier::T0);

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (done, total) = handle.progress();
            if done >= total {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "bulk T0 build of 50 images never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(handle.progress(), (50, 50));

        // All 50 images share one asset/content_hash: T0 dedupes to exactly
        // one row/file (spec §3.1) even though 50 distinct builds ran.
        let stats = h.service.stats().unwrap();
        assert_eq!(stats.t0_count, 1, "{stats:?}");
        let _ = h.catalog;
    }

    #[test]
    fn t15_set_viewport_cancels_off_screen_and_visible_precedes_neighbor_completion() {
        // concurrency = 1 makes visible-before-neighbor ordering
        // DETERMINISTIC rather than a timing race: with only one worker
        // slot, nothing else can even START until the sole Visible request
        // (dispatched first, since Visible always outranks Neighbor at the
        // same "just enqueued, slot free" moment) finishes — so by
        // construction zero neighbors can be ready before it is.
        let h = harness(11, 1);
        let visible: Vec<ImageId> = h.images[0..1].to_vec();
        let neighbors: Vec<ImageId> = h.images[1..11].to_vec();
        h.service.set_viewport(visible.clone(), neighbors.clone());

        // Checks specifically for a *T1* descriptor, not just "any preview
        // is available": all 11 images share ONE asset (see `harness`), and
        // T1's raw-source path builds the SHARED, asset-scope T0 first
        // (spec §3.1) as a side effect of the very first T1 build — so
        // `best_available(image, 0)` alone would trivially return `Some`
        // for every image the instant that shared T0 lands, regardless of
        // whether THAT image's own T1 build ever ran. Only a `Tier::T1` hit
        // proves this specific image's build completed.
        let t1_ready = |img: ImageId| {
            h.service
                .best_available(img, 0)
                .is_some_and(|d| d.tier == Tier::T1)
        };

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if t1_ready(visible[0]) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the visible image never became ready"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let neighbor_ready_count = neighbors.iter().filter(|&&i| t1_ready(i)).count();
        assert_eq!(
            neighbor_ready_count, 0,
            "visible completions must strictly precede prefetch (neighbor) completions"
        );

        // Now sweep the viewport away entirely: previously-tracked images
        // must be cancelled (querying set_viewport again with disjoint
        // sets) without panicking or leaking — the pure-function diffing
        // behavior itself is covered exhaustively by `plan_viewport`'s own
        // tests above.
        let elsewhere: Vec<ImageId> = h.images[5..8].to_vec();
        h.service.set_viewport(elsewhere, vec![]);
    }
}
