// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`EmbeddedPreviewProvider`], the M0 [`crate::PreviewProvider`]
//! implementation (spec §3.6, T21; store-backed as of E03 Phase B T06-T09).
//!
//! Demand-driven and cancellable: each request becomes (at most) one
//! blocking job on the [`JobSystem`] under the caller-chosen [`Class`]
//! (thumbs: `Background`; loupe source: `Interactive`, spec §3.6).
//! Concurrent requests for the same `(image, class)` share one job
//! (dedup); decoded results land in an **in-memory LRU capped in bytes**
//! (default 256 MiB via `CoreConfig::preview_cache_bytes`), this is the
//! spec §3.5/T09 "RAM LRU of decoded buffers" (`ByteLru`, below).
//!
//! **E03 Phase B wiring (T06-T09).** Each decode job now runs
//! [`crate::producer::ensure_t0`] (T06 extraction + T07 store/catalog write,
//! asset-scope deduped and memoized in the RAM [`crate::PreviewIndex`])
//! before [`crate::decode::open_pixels`] (T08: decode the *stored* T0 JPEG,
//! always baking orientation, see `decode.rs`'s module doc comment). The
//! source of decoded bytes moved from "re-read a byte range of the original
//! file every request" (the E01 seed) to "ensure the on-disk T0 exists once,
//! then decode from it", the pipeline this crate's spec §3.5 describes as
//! the M0 loupe path. [`EmbeddedPreviewProvider::set_viewport`] (T09) adds
//! filmstrip-neighbor prefetch on top of the same request/dedup/LRU
//! machinery that already existed.
//!
//! Ticket lifecycle: [`cancel`](crate::PreviewProvider::cancel) both cancels
//! outstanding work *and releases the ticket's state*, callers that stop
//! polling a ticket must cancel it (the grid cancels on scroll-out, spec
//! §5.3). Polling an unknown/cancelled ticket reports
//! [`PreviewError::Cancelled`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use lightbox_catalog::Catalog;
use lightbox_jobs::{CancelToken, Class, JobSystem};
use lightbox_types::{ImageId, SourceTier};

use crate::{
    decode, producer, AssetLocator, DecodedImage, LocatedAsset, PreviewClass, PreviewError,
    PreviewIndex, PreviewProvider, PreviewState, PreviewTicket, Store,
};

/// Dedup key (spec §3.6: one decode per concurrent `(image, class)`;
/// images map 1:1 to assets until E07's virtual copies).
type Key = (ImageId, PreviewClass);

/// One shared in-flight decode.
struct Inflight {
    key: Key,
    cancel: CancelToken,
    result: OnceLock<Result<Arc<DecodedImage>, PreviewError>>,
}

/// A ticket's view of its request.
enum TicketEntry {
    /// Resolved at request time (cache hit) or upgraded on poll. Holding the
    /// `Arc` here keeps the pixels alive even if the LRU evicts them.
    Ready(Arc<DecodedImage>),
    /// Attached to a shared in-flight job.
    Job(Arc<Inflight>),
}

/// Byte-capped LRU over decoded previews. O(n) eviction scan, the cache
/// holds at most a few hundred entries at M0 sizes.
struct ByteLru {
    cap: u64,
    bytes: u64,
    tick: u64,
    map: HashMap<Key, (Arc<DecodedImage>, u64)>,
}

impl ByteLru {
    fn new(cap: u64) -> ByteLru {
        ByteLru {
            cap,
            bytes: 0,
            tick: 0,
            map: HashMap::new(),
        }
    }

    fn cost(img: &DecodedImage) -> u64 {
        img.px.len() as u64
    }

    fn get(&mut self, key: &Key) -> Option<Arc<DecodedImage>> {
        self.tick += 1;
        let tick = self.tick;
        let (img, last_use) = self.map.get_mut(key)?;
        *last_use = tick;
        Some(Arc::clone(img))
    }

    fn insert(&mut self, key: Key, img: Arc<DecodedImage>) {
        let cost = Self::cost(&img);
        if cost > self.cap {
            return; // larger than the whole budget: serve, don't cache
        }
        self.tick += 1;
        if let Some((old, _)) = self.map.insert(key, (img, self.tick)) {
            self.bytes -= Self::cost(&old);
        }
        self.bytes += cost;
        while self.bytes > self.cap {
            let Some((&victim, _)) = self.map.iter().min_by_key(|(_, (_, tick))| *tick) else {
                break;
            };
            if let Some((evicted, _)) = self.map.remove(&victim) {
                self.bytes -= Self::cost(&evicted);
            }
        }
    }
}

struct ProviderState {
    tickets: HashMap<u64, TicketEntry>,
    /// In-flight decode per key + how many live tickets await it.
    inflight: HashMap<Key, (Arc<Inflight>, usize)>,
    cache: ByteLru,
}

/// Diagnostic counters (tests + debug overlays; not a frozen surface).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProviderStats {
    /// Live (un-cancelled, un-released) tickets.
    pub tickets: usize,
    /// Decode jobs currently tracked as in-flight.
    pub inflight: usize,
    /// Bytes held by the LRU cache.
    pub cache_bytes: u64,
    /// Total decodes that ever started (the dedup probe counter, T21 AC).
    pub decodes_started: u64,
}

/// The M0 preview provider: `ensure_t0` (extract + store, T06/T07) → decode
/// the stored T0 (T08) → byte-capped LRU + neighbor prefetch (T09, spec
/// §3.5/§3.6). E03's tiered on-disk store IS this provider's backing store
/// as of Phase B, the "E03's tiered store replaces it behind the same
/// trait" seam this doc comment used to name is now this file.
pub struct EmbeddedPreviewProvider {
    jobs: Arc<JobSystem>,
    locator: Arc<dyn AssetLocator>,
    /// The `.lbdata` cache store (spec §3.2, Phase A), T0 files land here.
    store: Arc<Store>,
    /// The catalog, `ensure_t0`'s write path (T07) upserts `preview` rows
    /// through it; the RAM index below is hydrated from it at construction.
    catalog: Arc<Catalog>,
    /// The in-RAM preview index (Phase A, T04): `ensure_t0`'s IO-free fast
    /// path, kept current by every successful `ensure_t0` call.
    index: Arc<Mutex<PreviewIndex>>,
    /// Behind its own `Arc` so decode jobs retire themselves through a weak
    /// handle to the *state*, never extending the provider's lifetime.
    state: Arc<Mutex<ProviderState>>,
    /// Live neighbor-prefetch tickets, keyed by image (T09
    /// `set_viewport`), separate from `state`'s ticket table so viewport
    /// churn never contends with request/poll/cancel traffic.
    prefetch: Mutex<HashMap<ImageId, PreviewTicket>>,
    next_ticket: AtomicU64,
    decodes_started: Arc<AtomicU64>,
}

impl EmbeddedPreviewProvider {
    /// `cache_bytes`: the decoded-preview LRU budget (spec §3.5/§5.7
    /// `decoded_lru_bytes`; `CoreConfig::preview_cache_bytes` at M0, default
    /// 256 MiB, the core config's own default, not yet reconciled with the
    /// store config's 512 MiB default; both are pre-existing M0 knobs, not
    /// something Phase B changes). `store`/`catalog` back the T06/T07
    /// extraction+write path; opening the store is the caller's job (spec
    /// §5.2 `PreviewService::open` composes `Store::open`, Phase A's
    /// `Store::open` today, `lightbox-core`'s `Session::open` calls it,
    /// see `E03-deviations.md`).
    ///
    /// Index hydration (`PreviewIndex::load`) failure is non-fatal: the RAM
    /// index is a disposable cache over the catalog (spec §3.2's "caches are
    /// disposable by contract"), so a hydration error starts this provider
    /// with an empty index (logged) rather than making construction
    /// fallible, every `ensure_t0` call still works correctly against a
    /// cold index, just without the fast-path skip.
    pub fn new(
        jobs: Arc<JobSystem>,
        locator: Arc<dyn AssetLocator>,
        cache_bytes: u64,
        store: Arc<Store>,
        catalog: Arc<Catalog>,
    ) -> EmbeddedPreviewProvider {
        let index = match PreviewIndex::load(&catalog.reader()) {
            Ok(index) => index,
            Err(err) => {
                tracing::warn!(
                    target: "lightbox_preview",
                    %err,
                    "preview index hydration failed; starting empty (the index is a disposable cache)"
                );
                PreviewIndex::empty()
            }
        };
        EmbeddedPreviewProvider {
            jobs,
            locator,
            store,
            catalog,
            index: Arc::new(Mutex::new(index)),
            state: Arc::new(Mutex::new(ProviderState {
                tickets: HashMap::new(),
                inflight: HashMap::new(),
                cache: ByteLru::new(cache_bytes.max(1)),
            })),
            prefetch: Mutex::new(HashMap::new()),
            next_ticket: AtomicU64::new(1),
            decodes_started: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Neighbor prefetch (spec §3.5/§5.2 `set_viewport`, T09): warms the
    /// decoded LRU for `neighbors` at `Class::Background` priority (the job
    /// system's own doc comment names this class for exactly this:
    /// "opportunistic work nobody is waiting for... prefetch"). `visible`
    /// is not actively requested here (the grid/loupe already requests
    /// visible cells through the normal path, spec's Visible priority), it
    /// is only consulted so this call never demotes/cancels a currently
    /// visible image's prefetch tracking.
    ///
    /// A Phase B reading of the spec's fuller "visible-first, cancel/demote
    /// off-screen work" scheduler behavior (§3.4, Phase D's T13/T15): any
    /// previously-tracked prefetch whose image fell out of `visible ∪
    /// neighbors` is cancelled immediately (releasing its ticket and, if it
    /// was the last waiter, its job); every image in `neighbors` not already
    /// tracked gets a fresh `Class::Background` request. A request that
    /// resolves synchronously (an LRU hit) is not tracked, its pixels are
    /// already cached, there is nothing left to cancel later.
    ///
    /// **Self-reaping.** A tracked ticket whose job has since *finished* (not
    /// just gone out of view) is released too, on every call, otherwise a
    /// long-lived viewport that never changes would pin every prefetched
    /// `DecodedImage` in `state.tickets` forever via its `TicketEntry::Ready`
    /// upgrade (see `poll`), bypassing the whole point of the byte-capped
    /// LRU it's supposed to just be a warm-up for. Reaping happens here
    /// (call-driven) rather than via a background sweep, Phase D's
    /// scheduler is the natural home for a proactive one; a UI that drives
    /// `set_viewport` on every scroll/frame reaps promptly in practice.
    pub fn set_viewport(&self, visible: &[ImageId], neighbors: &[ImageId]) {
        let wanted: HashSet<ImageId> = visible.iter().chain(neighbors.iter()).copied().collect();
        let mut prefetch = self.lock_prefetch();

        let done_or_stale: Vec<ImageId> = prefetch
            .iter()
            .filter(|(image, ticket)| {
                !wanted.contains(*image) || !matches!(self.poll(ticket), PreviewState::Pending)
            })
            .map(|(image, _)| *image)
            .collect();
        for image in done_or_stale {
            if let Some(ticket) = prefetch.remove(&image) {
                self.cancel(&ticket);
            }
        }

        for &image in neighbors {
            if prefetch.contains_key(&image) {
                continue;
            }
            let ticket = self.request(image, PreviewClass::Loupe, Class::Background);
            if matches!(self.poll(&ticket), PreviewState::Pending) {
                prefetch.insert(image, ticket);
            } else {
                self.cancel(&ticket); // already resolved (or failed): nothing to track
            }
        }
    }

    /// Live prefetch-tracked tickets (diagnostics/tests, T09 AC).
    pub fn prefetch_len(&self) -> usize {
        self.lock_prefetch().len()
    }

    fn lock_prefetch(&self) -> std::sync::MutexGuard<'_, HashMap<ImageId, PreviewTicket>> {
        self.prefetch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Diagnostic counters (T21 AC: after a cancel storm, `tickets`,
    /// `inflight` and the job system's running counts return to zero and
    /// `cache_bytes` stays under the cap).
    pub fn stats(&self) -> ProviderStats {
        let state = self.lock();
        ProviderStats {
            tickets: state.tickets.len(),
            inflight: state.inflight.len(),
            cache_bytes: state.cache.bytes,
            decodes_started: self.decodes_started.load(Ordering::Relaxed),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProviderState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn spawn_decode(&self, key: Key, prio: Class) -> Arc<Inflight> {
        let inflight = Arc::new(Inflight {
            key,
            cancel: CancelToken::new(),
            result: OnceLock::new(),
        });
        let job = Arc::clone(&inflight);
        let locator = Arc::clone(&self.locator);
        let decodes = Arc::clone(&self.decodes_started);
        let provider_state = SharedState(Arc::clone(&self.state));
        let store = Arc::clone(&self.store);
        let catalog = Arc::clone(&self.catalog);
        let index = Arc::clone(&self.index);
        let handle = self.jobs.spawn_blocking(
            prio,
            "preview.embedded.decode",
            inflight.cancel.clone(),
            move |cancel| {
                decodes.fetch_add(1, Ordering::Relaxed);
                let (image, class) = job.key;
                let out = locator.locate(image).and_then(|src| {
                    decode_via_store(&store, &catalog, &index, image, &src, class, cancel)
                });
                let out = out.map(Arc::new);
                // Publish, then (under the lock) cache + retire from the
                // in-flight table. Requests racing this window either attach
                // (and see the published result on poll) or hit the cache.
                let cache_it = out.clone();
                let _ = job.result.set(out.clone());
                let mut state = provider_state.lock();
                if let Some((current, _)) = state.inflight.get(&job.key) {
                    if Arc::ptr_eq(current, &job) {
                        state.inflight.remove(&job.key);
                    }
                }
                if let Ok(img) = cache_it {
                    if !cancel.is_cancelled() {
                        state.cache.insert(job.key, img);
                    }
                }
                drop(state);
                match out {
                    Ok(_) => Ok(()),
                    Err(PreviewError::Cancelled) => Err(lightbox_jobs::JobError::Cancelled),
                    Err(e) => {
                        tracing::debug!(
                            target: "lightbox_preview",
                            image = image.0,
                            error = %e,
                            "embedded preview decode failed"
                        );
                        // The failure is the *ticket's* outcome, delivered by
                        // poll(); the job itself completed its work.
                        Ok(())
                    }
                }
            },
        );
        drop(handle); // detached: lifecycle is tracked via `inflight`
        inflight
    }
}

/// One request's worth of work through the T06-T09 pipeline: `ensure_t0`
/// (extract + store/catalog write, memoized) → decode the stored T0,
/// downscaling for thumbs and always baking orientation (T08). Checkpoints
/// on `cancel` between the two coarse stages, matching the granularity the
/// E01-seeded pipeline this replaces already used (spec §3.4's fully
/// cooperative, between-every-substep cancellation is Phase D's scheduler,
/// not Phase B's).
fn decode_via_store(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    src: &LocatedAsset,
    class: PreviewClass,
    cancel: &CancelToken,
) -> Result<DecodedImage, PreviewError> {
    let check = |c: &CancelToken| -> Result<(), PreviewError> {
        if c.is_cancelled() {
            Err(PreviewError::Cancelled)
        } else {
            Ok(())
        }
    };

    check(cancel)?;
    let desc = producer::ensure_t0(
        store,
        catalog,
        index,
        image,
        src.asset,
        src.content_hash,
        &src.path,
    )?;

    check(cancel)?;
    let max_long_edge = match class {
        PreviewClass::Loupe => None,
        PreviewClass::Thumb { max_px } => Some(max_px),
    };
    let decoded = decode::open_pixels(store, &desc, src.orientation, max_long_edge)?;

    check(cancel)?;
    Ok(DecodedImage {
        px: decoded.pixels,
        width: decoded.width,
        height: decoded.height,
        // The tag the stored container/row resolved to, carried through instead
        // of dropped, this is the seam where a Display-P3 file used to become
        // "assumed sRGB" (see `DecodedImage::colorspace`).
        colorspace: decoded.colorspace,
        orientation_applied: true, // T08: decode-for-display always bakes it
        tier: SourceTier::EmbeddedPreview,
    })
}

/// Newtype so the closure only captures the state mutex, not the provider.
struct SharedState(Arc<Mutex<ProviderState>>);

impl SharedState {
    fn lock(&self) -> std::sync::MutexGuard<'_, ProviderState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PreviewProvider for EmbeddedPreviewProvider {
    fn request(&self, image: ImageId, class: PreviewClass, class_prio: Class) -> PreviewTicket {
        let id = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let key: Key = (image, class);
        let mut state = self.lock();

        if let Some(img) = state.cache.get(&key) {
            state.tickets.insert(id, TicketEntry::Ready(img));
            return PreviewTicket::new(id);
        }
        if let Some((inflight, waiters)) = state.inflight.get_mut(&key) {
            *waiters += 1;
            let entry = TicketEntry::Job(Arc::clone(inflight));
            state.tickets.insert(id, entry);
            return PreviewTicket::new(id);
        }
        drop(state);
        let inflight = self.spawn_decode(key, class_prio);
        let mut state = self.lock();
        state.inflight.insert(key, (Arc::clone(&inflight), 1));
        state.tickets.insert(id, TicketEntry::Job(inflight));
        PreviewTicket::new(id)
    }

    fn poll(&self, t: &PreviewTicket) -> PreviewState {
        let mut state = self.lock();
        let entry = match state.tickets.get(&t.id()) {
            None => return PreviewState::Failed(PreviewError::Cancelled),
            Some(TicketEntry::Ready(img)) => return PreviewState::Ready(Arc::clone(img)),
            Some(TicketEntry::Job(job)) => Arc::clone(job),
        };
        match entry.result.get() {
            None => PreviewState::Pending,
            Some(Ok(img)) => {
                // Upgrade so the pixels stay alive past LRU eviction.
                let img = Arc::clone(img);
                state
                    .tickets
                    .insert(t.id(), TicketEntry::Ready(Arc::clone(&img)));
                PreviewState::Ready(img)
            }
            Some(Err(e)) => PreviewState::Failed(e.clone()),
        }
    }

    fn cancel(&self, t: &PreviewTicket) {
        let mut state = self.lock();
        let Some(entry) = state.tickets.remove(&t.id()) else {
            return; // idempotent
        };
        if let TicketEntry::Job(job) = entry {
            let mut cancel_job = false;
            if let Some((current, waiters)) = state.inflight.get_mut(&job.key) {
                if Arc::ptr_eq(current, &job) {
                    *waiters = waiters.saturating_sub(1);
                    if *waiters == 0 {
                        state.inflight.remove(&job.key);
                        cancel_job = true;
                    }
                }
            }
            drop(state);
            if cancel_job {
                job.cancel.cancel();
            }
        }
    }
}

impl std::fmt::Debug for EmbeddedPreviewProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("EmbeddedPreviewProvider")
            .field("tickets", &stats.tickets)
            .field("inflight", &stats.inflight)
            .field("cache_bytes", &stats.cache_bytes)
            .field("decodes_started", &stats.decodes_started)
            .finish_non_exhaustive()
    }
}
