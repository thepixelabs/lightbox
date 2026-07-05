// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`EmbeddedPreviewProvider`] — the M0 [`crate::PreviewProvider`]
//! implementation (spec §3.6, T21).
//!
//! Demand-driven and cancellable: each request becomes (at most) one
//! blocking job on the [`JobSystem`] under the caller-chosen [`Class`]
//! (thumbs: `Background`; loupe source: `Interactive` — spec §3.6).
//! Concurrent requests for the same `(image, class)` share one job
//! (dedup); decoded results land in an **in-memory LRU capped in bytes**
//! (default 256 MiB via `CoreConfig::preview_cache_bytes`).
//!
//! Ticket lifecycle: [`cancel`](crate::PreviewProvider::cancel) both cancels
//! outstanding work *and releases the ticket's state* — callers that stop
//! polling a ticket must cancel it (the grid cancels on scroll-out, spec
//! §5.3). Polling an unknown/cancelled ticket reports
//! [`PreviewError::Cancelled`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use lightbox_jobs::{CancelToken, Class, JobSystem};
use lightbox_types::ImageId;

use crate::pipeline;
use crate::{
    AssetLocator, DecodedImage, PreviewClass, PreviewError, PreviewProvider, PreviewState,
    PreviewTicket,
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

/// Byte-capped LRU over decoded previews. O(n) eviction scan — the cache
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

/// The M0 preview provider: embedded JPEG → decode → (resize+orient) →
/// byte-capped LRU (spec §3.6). E03's tiered on-disk store replaces it
/// behind the same trait.
pub struct EmbeddedPreviewProvider {
    jobs: Arc<JobSystem>,
    locator: Arc<dyn AssetLocator>,
    /// Behind its own `Arc` so decode jobs retire themselves through a weak
    /// handle to the *state*, never extending the provider's lifetime.
    state: Arc<Mutex<ProviderState>>,
    next_ticket: AtomicU64,
    decodes_started: Arc<AtomicU64>,
}

impl EmbeddedPreviewProvider {
    /// `cache_bytes`: the LRU budget (`CoreConfig::preview_cache_bytes`,
    /// default 256 MiB).
    pub fn new(
        jobs: Arc<JobSystem>,
        locator: Arc<dyn AssetLocator>,
        cache_bytes: u64,
    ) -> EmbeddedPreviewProvider {
        EmbeddedPreviewProvider {
            jobs,
            locator,
            state: Arc::new(Mutex::new(ProviderState {
                tickets: HashMap::new(),
                inflight: HashMap::new(),
                cache: ByteLru::new(cache_bytes.max(1)),
            })),
            next_ticket: AtomicU64::new(1),
            decodes_started: Arc::new(AtomicU64::new(0)),
        }
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
        let handle = self.jobs.spawn_blocking(
            prio,
            "preview.embedded.decode",
            inflight.cancel.clone(),
            move |cancel| {
                decodes.fetch_add(1, Ordering::Relaxed);
                let (image, class) = job.key;
                let out = locator.locate(image).and_then(|src| {
                    pipeline::decode_class(&src.path, src.orientation, class, cancel)
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
