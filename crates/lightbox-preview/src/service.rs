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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lightbox_catalog::Catalog;
use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use crate::config::{CacheLimits, PreviewStoreConfig, Retention};
use crate::evict;
use crate::index::PreviewIndex;
use crate::producer;
use crate::pyramid::{PreviewDesc, RelPath, Tier, TierSet, VariantHash};
use crate::rawcache::{RawCache, RealDiskSpaceProbe};
use crate::relocate::{self, ProgressSink, RelocateError};
use crate::sched::{
    BuildFn, BuildKey, BuildPriority, BuildRuntime, BulkHandle, EnqueueError, EventSink, Scheduler,
};
use crate::store::Store;
use crate::t2::{self, TileCoord};
use crate::thumbs::ThumbCache;
use crate::verify::{self, PurgeScope, VerifyMode, VerifyReport};
use crate::{EncodedThumb, EncodedTile, PreviewError, PreviewEvictReport, PreviewTicket};

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

/// [`PreviewService::purge_all`]/[`PreviewService::purge`]'s report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// Preview-pyramid rows deleted.
    pub rows_deleted: u64,
    /// Phase F (T21): raw-cache rows deleted (`0` unless `scope` included
    /// `PurgeScope::RawCache`/`All`).
    pub rawcache_rows_deleted: u64,
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
    /// Phase F: retained so eviction/discard/retention (T19) can publish
    /// `PreviewEvent::Evicted`/`CachePressure` directly, not just the
    /// scheduler's own Ready/Failed/BulkProgress stream.
    events: EventSink,
    /// Phase F (T19/T21): the preview-pyramid cap; runtime-settable via
    /// `SetCacheLimits`. The raw cache keeps its OWN copy in sync (see
    /// `set_limits`) rather than sharing this lock — the two caches are
    /// evicted independently.
    limits: Mutex<CacheLimits>,
    t2_retention: Retention,
    /// Phase F (T21, closes deviation E-10): the raw decode cache, finally
    /// composed into the facade the spec's §5.2 `raw_cache()` accessor
    /// always named. Constructed here (not by the caller) exactly the way
    /// `RawCache::new`'s signature was left ready for (E-10's own note).
    raw_cache: RawCache,
    /// Phase F (T22): the hot thumbnail atlas.
    thumbs: ThumbCache,
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
            cfg.clone(),
        );
        let scheduler = Scheduler::new(rt, build_fn, events.clone());

        // Phase F (T21, closes E-10): the raw cache, wired with the real
        // free-space probe and this service's own event sink (so a
        // `CachePressure` event during a raw-cache `put` rides the same
        // stream as everything else).
        let raw_cache = RawCache::with_probe(
            Arc::clone(&store),
            Arc::clone(&catalog),
            cfg.limits,
            cfg.zstd_level,
            Arc::new(RealDiskSpaceProbe),
            Some(events.clone()),
        );
        // Phase F (T22): thumbcache.sqlite, a sibling of the catalog/store
        // (spec §3.2's tree) — never fails to open (see `thumbs.rs`).
        let thumbs = ThumbCache::open(&store.root().join("thumbcache.sqlite"));

        Ok(PreviewService {
            inner: Arc::new(ServiceInner {
                store,
                catalog,
                index,
                scheduler,
                viewport: Mutex::new(HashMap::new()),
                events,
                limits: Mutex::new(cfg.limits),
                t2_retention: cfg.t2_retention,
                raw_cache,
                thumbs,
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
        Ok(PurgeReport {
            rows_deleted: n,
            rawcache_rows_deleted: 0,
        })
    }

    // ── Phase F (T19): eviction, retention, discard ─────────────────────

    /// Runtime cap update (spec §5.6 `SetCacheLimits`) — updates BOTH this
    /// service's own preview-pyramid cap and the composed [`RawCache`]'s
    /// (the two caches are evicted independently, but `SetCacheLimits`
    /// carries both caps in one [`CacheLimits`] value, spec §5.7).
    pub fn set_limits(&self, limits: CacheLimits) {
        *self.lock_limits() = limits;
        self.inner.raw_cache.set_limits(limits);
    }

    fn lock_limits(&self) -> std::sync::MutexGuard<'_, CacheLimits> {
        self.inner
            .limits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// LRU-to-cap eviction, tier order T2 → T1 → T0 (spec §3.2, T19).
    pub fn evict_to_cap(&self) -> PreviewEvictReport {
        let cap = self.lock_limits().preview_cap_bytes;
        evict::evict_to_cap(
            &self.inner.store,
            &self.inner.catalog,
            &self.inner.index,
            cap,
            &self.inner.events,
        )
    }

    /// T2 age-retention sweep (spec §3.2; `PreviewStoreConfig::t2_retention`,
    /// default 30 days). `lightbox-core`/a future E06 idle tick is the
    /// natural periodic caller — this method itself is just the primitive.
    pub fn sweep_t2_retention(&self) -> PreviewEvictReport {
        evict::sweep_t2_retention(
            &self.inner.store,
            &self.inner.catalog,
            &self.inner.index,
            &self.inner.events,
            self.inner.t2_retention,
            crate::index::now_unix_seconds(),
        )
    }

    /// `DiscardPreviews { images, tiers }` (spec §5.6). See
    /// [`crate::evict::discard`]'s doc comment for the T0 asset-scope
    /// sharing note.
    pub fn discard(&self, images: Vec<ImageId>, tiers: TierSet) -> PreviewEvictReport {
        evict::discard(
            &self.inner.store,
            &self.inner.catalog,
            &self.inner.index,
            &self.inner.events,
            &images,
            tiers,
        )
    }

    /// Retries any paths queued for deferred deletion (Windows sharing
    /// violations — spec Risk R4, T19). A no-op on platforms that never
    /// populate the queue. Returns the count removed.
    pub fn retry_deferred_deletes(&self) -> usize {
        self.inner.store.retry_deferred_deletes()
    }

    // ── Phase F (T20): T2 tiled reads ────────────────────────────────────

    /// T2 tile read for the 1:1 loupe (spec §5.2; producer: E05, M1+ — at
    /// M0 only the synthetic test producer ever populates a T2 row).
    /// `Ok(None)` for a tile not built yet, or an image with no T2 row at
    /// all for `variant`.
    pub fn t2_tile(
        &self,
        image: ImageId,
        variant: VariantHash,
        tile: TileCoord,
    ) -> Result<Option<EncodedTile>, PreviewError> {
        let asset = self
            .inner
            .catalog
            .reader()
            .image_detail(image)
            .ok()
            .map(|d| d.asset);
        let Some(asset) = asset else { return Ok(None) };
        let Some(desc) = self
            .lock_index()
            .lookup_variant(image, asset, Tier::T2, variant)
        else {
            return Ok(None);
        };
        t2::t2_tile_read(&self.inner.store, &desc.store_path, tile)
    }

    // ── Phase F (T22): thumbnail atlas ───────────────────────────────────

    /// Grid fast path: an encoded ~`px`-long-edge thumb from
    /// `thumbcache.sqlite`, build-through on miss (spec §5.2). `Ok(None)`
    /// when nothing has been built for `image` at ANY tier yet — this method
    /// builds an encoded thumb from whatever preview tier already exists, it
    /// does not itself trigger T0/T1 extraction (that's `request`/
    /// `bulk_build`'s job). `recipe_rev` is always `0` at M0 (nothing bumps
    /// it yet — mirrors every embedded-sourced `preview` row's own
    /// convention), so a warm hit needs no catalog read at all.
    pub fn thumb(&self, image: ImageId, px: u32) -> Result<Option<EncodedThumb>, PreviewError> {
        const RECIPE_REV: u64 = 0;
        if let Some(bytes) = self.inner.thumbs.lookup(image, RECIPE_REV, px) {
            return Ok(Some(EncodedThumb { bytes, px }));
        }
        let Some(desc) = self.best_available(image, 0) else {
            return Ok(None);
        };
        let detail = self
            .inner
            .catalog
            .reader()
            .image_detail(image)
            .map_err(|e| PreviewError::Catalog(e.to_string()))?;
        let decoded =
            crate::decode::open_pixels(&self.inner.store, &desc, detail.orientation, Some(px))?;
        let rgb = rgba_to_rgb(&decoded.pixels);
        let encoded = crate::codec::resolve(crate::config::Codec::Jpeg)
            .encode(
                &crate::codec::RgbImage {
                    px: rgb,
                    width: decoded.width,
                    height: decoded.height,
                },
                85,
            )
            .map_err(|e| PreviewError::Encode(e.to_string()))?;
        self.inner.thumbs.insert(image, RECIPE_REV, px, 0, &encoded);
        Ok(Some(EncodedThumb { bytes: encoded, px }))
    }

    // ── Phase F (T21): raw cache accessor, relocate, purge, verify_store ──

    /// The composed raw decode cache (spec §5.2; closes deviation E-10).
    pub fn raw_cache(&self) -> &RawCache {
        &self.inner.raw_cache
    }

    /// Journaled relocation of the E03-owned cache surfaces to `new_root`
    /// (spec §5.2/§3.2, T21). See `relocate.rs`'s module doc comment for
    /// exact scope (previews/rawcache/smartpreview/masks + `store.toml`;
    /// never the catalog database or `thumbcache.sqlite`) and the
    /// resumability contract. This call does NOT hot-swap `self` onto the
    /// new root — the caller reopens a fresh session/service pointed at
    /// `new_root` afterward (matching the spec's own framing: "config points
    /// at the new root via the core prefs store").
    pub fn relocate(&self, new_root: &Path, progress: ProgressSink) -> Result<(), RelocateError> {
        relocate::relocate(self.inner.store.root(), new_root, &progress)
    }

    /// Scoped purge (spec §5.6 `PurgeCaches(PurgeScope)`), a fuller sibling
    /// of [`Self::purge_all`] (which is always `PurgeScope::Previews`, kept
    /// for the existing CLI/tests — see D-8).
    pub fn purge(&self, scope: PurgeScope) -> Result<PurgeReport, PreviewError> {
        let mut report = PurgeReport::default();
        if matches!(scope, PurgeScope::Previews | PurgeScope::All) {
            report.rows_deleted = self.purge_all()?.rows_deleted;
        }
        if matches!(scope, PurgeScope::RawCache | PurgeScope::All) {
            report.rawcache_rows_deleted = self
                .inner
                .raw_cache
                .purge_all()
                .map_err(|e| PreviewError::Catalog(e.to_string()))?
                .rows_deleted;
        }
        Ok(report)
    }

    /// The spec's fuller `verify_store(Quick|Full)` (T21) — see `verify.rs`'s
    /// module doc comment. [`Self::verify_quick`] (Phase D, D-8) remains as
    /// the narrower missing-file-only primitive it always was.
    pub fn verify_store(&self, mode: VerifyMode) -> VerifyReport {
        verify::verify_store(
            &self.inner.store,
            &self.inner.catalog,
            &self.inner.index,
            mode,
        )
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

/// Strips alpha from interleaved RGBA8 (Phase F, T22 — [`PreviewService::
/// thumb`]'s JPEG re-encode needs [`crate::codec::RgbImage`], which carries
/// no alpha channel; T0's own convention already carries none either).
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    out
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

        let deadline = Instant::now() + Duration::from_secs(60);
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

        let deadline = Instant::now() + Duration::from_secs(60);
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

        let deadline = Instant::now() + Duration::from_secs(60);
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

    // ── Phase F: the full facade wired together ──────────────────────────

    /// End-to-end proof that T19-T22's additions actually compose through
    /// `PreviewService`, not just in their own modules' unit tests:
    /// `thumb()` build-through + warm hit, `raw_cache()`, `discard`,
    /// `evict_to_cap`, `set_limits`, `verify_store`, and `purge(scope)`.
    #[test]
    fn t19_t21_t22_facade_methods_compose_end_to_end() {
        let h = harness(1, 2);
        let image = h.images[0];

        let ticket = h
            .service
            .request(PreviewRequest {
                image,
                tier: Tier::T1,
                priority: BuildPriority::Visible,
                allow_embedded: true,
            })
            .unwrap();
        let _ = ticket;
        let deadline = Instant::now() + Duration::from_secs(60);
        while h.service.best_available(image, 0).is_none() {
            assert!(Instant::now() < deadline, "T1 build never landed");
            std::thread::sleep(Duration::from_millis(5));
        }

        // T22: thumb() build-through on miss, warm hit returns identical bytes.
        let thumb1 = h
            .service
            .thumb(image, 128)
            .unwrap()
            .expect("something to build from");
        assert!(!thumb1.bytes.is_empty());
        let thumb2 = h.service.thumb(image, 128).unwrap().unwrap();
        assert_eq!(
            thumb1.bytes, thumb2.bytes,
            "warm hit must return the same bytes"
        );

        // A never-requested image with nothing built yet: thumb() has
        // nothing to build FROM, so it is a clean `Ok(None)`, not an error.
        assert!(h.service.thumb(ImageId(999_999), 128).unwrap().is_none());

        // T21: raw_cache() accessor is live (closes E-10) — a direct
        // put/get round-trips through the SAME store this service opened.
        let rc = h.service.raw_cache();
        let key = crate::rawcache::RawCacheKey {
            content_hash: lightbox_types::ContentHash([55; 16]),
            params_hash: 1,
        };
        rc.put(
            key,
            crate::rawcache::RawStageMeta {
                payload_schema: 1,
                width: 4,
                height: 4,
                channels: 1,
                sample: crate::rawcache::SampleFormat::U16,
                color_state: 0,
            },
            crate::rawcache::PlaneData(&[0u8; 32]),
        )
        .unwrap();
        assert!(rc.get(&key).unwrap().is_some());

        // T21: verify_store(Quick) reports a clean store.
        let quick = h.service.verify_store(VerifyMode::Quick);
        assert_eq!(quick.missing_files, 0, "{quick:?}");

        // T19: evict_to_cap with a cap of 0 must reclaim the T1 row (T0 too,
        // since both are over any positive cap).
        let stats_before = h.service.stats().unwrap();
        assert!(stats_before.total_count() > 0);
        h.service.set_limits(CacheLimits {
            preview_cap_bytes: 0,
            rawcache_cap_bytes: u64::MAX,
        });
        let evict_report = h.service.evict_to_cap();
        assert!(evict_report.rows_deleted > 0, "{evict_report:?}");
        assert_eq!(h.service.stats().unwrap().total_count(), 0);

        // T21: purge(RawCache) clears the raw cache without touching an
        // (already-empty) preview store, and reports it distinctly.
        let purge_report = h.service.purge(PurgeScope::RawCache).unwrap();
        assert_eq!(purge_report.rawcache_rows_deleted, 1, "{purge_report:?}");
        assert!(!h.service.raw_cache().contains(&key));
    }
}
