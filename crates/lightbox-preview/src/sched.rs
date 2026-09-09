// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The demand-driven build scheduler (E03 spec §3.4/§5.6, Phase D T13).
//!
//! Three in-crate priorities, **Visible > Neighbor > Bulk**, with request
//! dedup/coalescing (keyed by `(image, tier)`; see [`BuildKey`]'s doc
//! comment for why the spec's fuller `(image|asset, tier, variant)` key
//! narrows to this at M0), in-place priority upgrade/downgrade, bounded
//! concurrency, cooperative cancellation, and a bounded Bulk backlog with
//! `QueueFull` backpressure (spec §3.4's E04 import-throttle seam).
//!
//! **Cancellation granularity (a documented, deliberate M0 reading of "next
//! step boundary").** [`producer::ensure_t0`]/[`producer::ensure_t1`]
//! (Phase C, frozen and already tested) are synchronous, atomic calls from
//! this scheduler's point of view, they accept no cancellation token and
//! checkpoint no sub-steps. The one checkpoint this scheduler owns is
//! **immediately before a build is handed to the [`BuildRuntime`]**: a
//! request cancelled while still queued (the overwhelming majority of
//! cancellations in practice, a viewport scroll, a superseded Bulk
//! request) is skipped entirely and never runs. A build already handed to a
//! worker runs to completion uninterrupted (there is nowhere left to check).
//! This is exactly the granularity T15's viewport-sweep AC needs ("≥95% of
//! off-screen queued builds cancelled **before start**") and is recorded as
//! a deviation in `docs/plan/epics/E03-deviations.md` (Phase D). Finer-grained
//! mid-build cancellation would require threading a token through Phase C's
//! producer functions, out of this phase's scope (leaves Phase C's
//! surface, and its tests, untouched).
//!
//! [`producer::ensure_t0`]: crate::producer
//! [`producer::ensure_t1`]: crate::producer
//!
//! **Reuse of `lightbox_jobs::CancelToken` (closes deviation A-10's open
//! question).** The spec's §3.4/§5.6 text names `tokio_util::sync::
//! CancellationToken` as an M0-acceptable choice and separately notes (§5.6)
//! that "E03 never imports `lightbox-jobs` types." That boundary was already
//! crossed by Phase B: `embedded.rs`'s `PreviewProvider`/
//! `EmbeddedPreviewProvider` (the E01-seeded trait this crate freezes) takes
//! `lightbox_jobs::Class` in its `request` signature and uses
//! `lightbox_jobs::CancelToken` throughout. Given that precedent, this phase
//! reuses `lightbox_jobs::CancelToken` here too rather than adding a SECOND,
//! parallel cancellation-token type via a new `tokio-util` dependency
//! (`tokio-util` is MIT and would have been permitted, this is a
//! reuse/simplicity call, not a license one).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use crate::pyramid::{PreviewDesc, Tier};
use crate::{PreviewError, PreviewTicket};

/// Build priority classes (spec §3.4/§5.2). Declaration order matches the
/// spec text; [`BuildPriority::rank`] (not derived `Ord`, to keep the public
/// declaration order spec-literal) is what the scheduler actually compares.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum BuildPriority {
    Visible,
    Neighbor,
    Bulk,
}

impl BuildPriority {
    /// Higher rank dequeues first. `Bulk` is deliberately `0` so it indexes
    /// [`State::queues`]'s lowest-priority slot.
    fn rank(self) -> usize {
        match self {
            BuildPriority::Bulk => 0,
            BuildPriority::Neighbor => 1,
            BuildPriority::Visible => 2,
        }
    }
}

/// One build's identity for dedup/priority purposes (spec §3.4: "dedup by
/// `(image, tier, variant)`").
///
/// **Narrowed to `(image, tier)` at M0.** [`crate::PreviewRequest`] (T14)
/// the spec's own §5.2 shape, carries no `variant` field: the caller asks
/// for a tier, not a specific encoded variant. The variant a build actually
/// produces is resolved server-side from [`crate::PreviewStoreConfig`]
/// (`standard_px`/`t1_codec`/`t1_quality`), which is fixed for the lifetime
/// of one [`crate::PreviewService`], so for any given `(image, tier)` there
/// is exactly one variant this service will ever build, making `(image,
/// tier)` a faithful dedup key in practice. A future caller-supplied variant
/// override would need to fold into this key; not exercised at M0 (no such
/// caller exists yet).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct BuildKey {
    pub image: ImageId,
    pub tier: Tier,
}

/// Why [`Scheduler::request`]/[`Scheduler::enqueue_bulk`] refused a request
/// (spec §5.2: "Bulk returns `Err(EnqueueError::QueueFull)` when bounded
/// queue is full").
#[derive(Copy, Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnqueueError {
    #[error("bulk build queue is at its bound (E04's throttle signal, spec §3.4/S3)")]
    QueueFull,
}

/// What a [`Scheduler`] reports as builds progress (spec §5.2, narrowed to
/// what Phase D actually produces, `Evicted`/`CachePressure` are Phase
/// E/F's, not emitted here).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PreviewEvent {
    Ready {
        image: ImageId,
        tier: Tier,
        desc: PreviewDesc,
    },
    /// Spec names a separate `PreviewErrorKind`; [`PreviewError`] is already
    /// `Clone` (lib.rs), so this reuses it directly rather than introducing
    /// a second, informationally-identical enum (documented deviation).
    Failed {
        image: ImageId,
        tier: Tier,
        error: PreviewError,
    },
    /// Spec's `{done, total}` shape, verbatim, no session id (see
    /// [`Scheduler::bulk_build`]'s doc comment on why: at most one
    /// concurrently-observed bulk run is disambiguated by this event alone;
    /// a [`BulkHandle::progress`] read gives an authoritative per-session
    /// count regardless).
    BulkProgress { done: u64, total: u64 },
    /// Phase F (T19): a preview row + (if unreferenced) its file was
    /// reclaimed by cap-based eviction, the T2 retention sweep, or an
    /// explicit `DiscardPreviews`. Fired once per evicted row.
    Evicted { image: ImageId, tier: Tier },
    /// Phase F (T21): a build (preview or raw-cache) hit disk pressure
    /// eviction ran to make room, or, if that still wasn't enough, the build
    /// failed with a typed error rather than panicking or corrupting
    /// anything (spec §5.2, the ENOSPC pre-flight AC).
    CachePressure {
        kind: CacheKind,
        used_bytes: u64,
        cap_bytes: u64,
    },
}

/// Which cache a [`PreviewEvent::CachePressure`] event is about (spec §5.2).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum CacheKind {
    Preview,
    RawCache,
}

/// A future the scheduler hands to a [`BuildRuntime`]. No internal `.await`
/// points at M0 (it wraps a synchronous producer call), `BuildRuntime`
/// impls are free to run it via `spawn_blocking` (recommended: it does real
/// file/CPU work) or any executor that can poll a boxed future to
/// completion.
pub type BuildFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The seam E06 grows into `Class::Background` + pause/activity-center
/// support (spec §5.6). M0: `lightbox-core` backs this with plain tokio
/// (`spawn_blocking`, since builds are synchronous file/CPU work), see
/// `lightbox-core`'s `preview_runtime.rs`.
pub trait BuildRuntime: Send + Sync {
    /// Runs `fut` to completion off the caller's thread. `name` is a
    /// diagnostic label (job/task name), not an identity.
    fn spawn_build(&self, name: &'static str, fut: BuildFuture);

    /// Worker budget (spec §5.7 `workers`: `min(physical_cores, config)`).
    /// The scheduler treats this as its concurrency bound; `BuildRuntime`
    /// itself need not enforce it (the scheduler never hands out more than
    /// `concurrency()` concurrently-running builds).
    fn concurrency(&self) -> usize;
}

/// A sink for [`PreviewEvent`]s (spec §5.2/§5.6): `lightbox-core` relays
/// these onto its own broadcast event bus (`Event::PreviewReady`/etc).
/// Called from whatever thread a build actually finishes on, never the
/// caller of [`Scheduler::request`].
///
/// Callers must eventually [`Scheduler::cancel`] a ticket they no longer
/// care about (mirrors [`crate::PreviewProvider`]'s existing "the caller
/// cancels on scroll-out" contract; there is no auto-GC on completion at
/// M0, matching that provider's own ticket-table lifecycle).
pub type EventSink = Arc<dyn Fn(PreviewEvent) + Send + Sync>;
/// The actual build work a [`Scheduler`] drives, `service.rs` (T14)
/// supplies the real one (dispatching to `producer::ensure_t0`/`ensure_t1`
/// by tier); tests substitute a synthetic one.
pub type BuildFn =
    Arc<dyn Fn(BuildKey, &CancelToken) -> Result<PreviewDesc, PreviewError> + Send + Sync>;

/// One "build previews for selection" run (T16). `total` is fixed at
/// creation; `done` only ever increases (spec AC: "progress monotonic to
/// completion"). A cancelled session's `done` may never reach `total`
/// that is the expected shape of "cancel mid-run", not a bug.
///
/// `done` is a `Mutex<u64>`, not an `AtomicU64`: with `BuildRuntime`
/// implementations that run builds on genuinely concurrent threads, an
/// atomic `fetch_add` alone only guarantees the *values* are a permutation
/// of `1..=total`, it says nothing about the ORDER in which
/// `PreviewEvent::BulkProgress` events are actually emitted to `events`
/// (two threads can fetch_add out of program order relative to when they
/// then call the event sink). Holding this lock across BOTH the increment
/// and the event emission (see `Scheduler::on_build_finished`) serializes
/// the two together, which is what makes the emitted *sequence* of events
/// (not just the set of `done` values) monotonically increasing, the AC's
/// actual meaning for an event stream a subscriber observes in order.
struct BulkSession {
    id: u64,
    total: u64,
    done: Mutex<u64>,
    cancelled: AtomicBool,
    tickets: Mutex<Vec<PreviewTicket>>,
}

/// A handle to one [`Scheduler::bulk_build`] run (T16). Cloning is cheap
/// (`Arc` inner via the session); [`Scheduler::cancel_bulk`] takes `&self`.
#[derive(Clone)]
pub struct BulkHandle {
    session: Arc<BulkSession>,
}

impl BulkHandle {
    pub fn id(&self) -> u64 {
        self.session.id
    }

    /// `(done, total)`, an authoritative, locally-pollable alternative to
    /// listening for `PreviewEvent::BulkProgress` (which carries no session
    /// id, see that variant's doc comment).
    pub fn progress(&self) -> (u64, u64) {
        let done = *self
            .session
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (done, self.session.total)
    }
}

struct PendingMeta {
    priority: BuildPriority,
    waiters: usize,
    bulk: Vec<Arc<BulkSession>>,
}

struct RunningMeta {
    waiters: usize,
    cancel: CancelToken,
    bulk: Vec<Arc<BulkSession>>,
}

struct State {
    /// Indexed by [`BuildPriority::rank`]. Append-only FIFOs with **lazy
    /// deletion**: an upgrade/downgrade appends a fresh entry to the new
    /// rank's queue rather than relocating the old one (O(1) instead of an
    /// O(n) mid-queue removal); [`Scheduler::try_dispatch`] discards a
    /// popped entry whose `pending` metadata no longer matches that rank
    /// (superseded, cancelled, or already dispatched) instead of building
    /// it. Every real entry is discarded or dispatched exactly once, so this
    /// stays amortized O(1) per enqueue/dispatch even under heavy
    /// re-prioritization (T15's 5k-image viewport sweep).
    queues: [VecDeque<BuildKey>; 3],
    /// Keyed by `BuildKey`: builds queued but not yet handed to the
    /// [`BuildRuntime`]. Absence here (with no `running` entry either) is
    /// what marks a `queues[]` entry as stale.
    pending: HashMap<BuildKey, PendingMeta>,
    /// Builds currently running on the `BuildRuntime` (consuming a
    /// concurrency slot).
    running: HashMap<BuildKey, RunningMeta>,
    tickets: HashMap<u64, BuildKey>,
    available_slots: usize,
    /// T16's backlog for `bulk_build`: images accepted into a bulk run but
    /// not yet admitted into the bounded `queues[Bulk]`, see
    /// [`Scheduler::pump_bulk`]'s doc comment for why a second, unbounded
    /// stage exists ahead of the bounded one.
    backlog: VecDeque<(BuildKey, Arc<BulkSession>)>,
    /// Live count of `pending` entries whose priority is currently `Bulk`
    /// (the bound-check AC's actual meaning, "queue never exceeds bound").
    /// **Not** `queues[Bulk.rank()].len()`: that raw queue length includes
    /// stale, lazily-deleted duplicates (superseded upgrades, cancelled
    /// entries, see `queues`'s own doc comment) that would otherwise make
    /// the bound check over-count and reject requests it shouldn't.
    /// Maintained precisely at every site that inserts into, removes from,
    /// or re-prioritizes a `pending` entry.
    bulk_queued: usize,
}

/// The scheduler (T13 core + T16 bulk/backlog). Always held as `Arc<Scheduler>`
/// build-completion closures capture a clone to re-enter [`Self::try_dispatch`]
/// / [`Self::pump_bulk`], so every dispatching method takes `self: &Arc<Self>`.
pub struct Scheduler {
    state: Mutex<State>,
    runtime: Arc<dyn BuildRuntime>,
    build_fn: BuildFn,
    events: EventSink,
    concurrency: usize,
    bulk_bound: usize,
    next_ticket: AtomicU64,
    next_bulk_id: AtomicU64,
}

impl Scheduler {
    /// `build_fn` runs on whatever thread `runtime.spawn_build` picks
    /// never the caller of [`Self::request`]. `events` is called from that
    /// same thread on every completion (Ready/Failed) and bulk-progress tick.
    pub fn new(
        runtime: Arc<dyn BuildRuntime>,
        build_fn: BuildFn,
        events: EventSink,
    ) -> Arc<Scheduler> {
        let concurrency = runtime.concurrency().max(1);
        // Spec §3.4: "bounded (default 4x worker count)".
        let bulk_bound = concurrency * 4;
        Arc::new(Scheduler {
            state: Mutex::new(State {
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                pending: HashMap::new(),
                running: HashMap::new(),
                tickets: HashMap::new(),
                available_slots: concurrency,
                backlog: VecDeque::new(),
                bulk_queued: 0,
            }),
            runtime,
            build_fn,
            events,
            concurrency,
            bulk_bound,
            next_ticket: AtomicU64::new(1),
            next_bulk_id: AtomicU64::new(1),
        })
    }

    /// Worker budget this scheduler was built with (diagnostics/tests).
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Bulk backlog admission bound (diagnostics/tests).
    pub fn bulk_bound(&self) -> usize {
        self.bulk_bound
    }

    /// Enqueues (or dedups/upgrades into) one build at `priority`. `Visible`/
    /// `Neighbor` are always accepted (spec §5.2); `Bulk` is refused with
    /// [`EnqueueError::QueueFull`] once `queues[Bulk]` is at
    /// [`Self::bulk_bound`], unless this call dedups onto an existing
    /// pending/running entry (attaching a waiter never grows the queue).
    pub fn request(
        self: &Arc<Self>,
        key: BuildKey,
        priority: BuildPriority,
    ) -> Result<PreviewTicket, EnqueueError> {
        self.enqueue_internal(key, priority, Vec::new())
    }

    fn enqueue_internal(
        self: &Arc<Self>,
        key: BuildKey,
        priority: BuildPriority,
        bulk: Vec<Arc<BulkSession>>,
    ) -> Result<PreviewTicket, EnqueueError> {
        let ticket_id = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = self.lock();
            if let Some(running) = st.running.get_mut(&key) {
                running.waiters += 1;
                running.bulk.extend(bulk);
            } else if st.pending.contains_key(&key) {
                let old_priority = {
                    let pending = st.pending.get_mut(&key).expect("checked above");
                    pending.waiters += 1;
                    pending.bulk.extend(bulk);
                    pending.priority
                };
                if priority.rank() > old_priority.rank() {
                    // Bulk is rank 0 (the lowest), so an "upgrade" (strictly
                    // higher rank) can only ever move AWAY from Bulk, never
                    // into it, this arm's bulk_queued bookkeeping is a
                    // decrement-only in practice, symmetric handling kept
                    // for clarity/robustness rather than assumed.
                    if old_priority == BuildPriority::Bulk {
                        st.bulk_queued = st.bulk_queued.saturating_sub(1);
                    }
                    st.pending.get_mut(&key).expect("checked above").priority = priority;
                    if priority == BuildPriority::Bulk {
                        st.bulk_queued += 1;
                    }
                    st.queues[priority.rank()].push_back(key);
                }
            } else {
                if priority == BuildPriority::Bulk && st.bulk_queued >= self.bulk_bound {
                    return Err(EnqueueError::QueueFull);
                }
                st.pending.insert(
                    key,
                    PendingMeta {
                        priority,
                        waiters: 1,
                        bulk,
                    },
                );
                st.queues[priority.rank()].push_back(key);
                if priority == BuildPriority::Bulk {
                    st.bulk_queued += 1;
                }
            }
            st.tickets.insert(ticket_id, key);
        }
        self.try_dispatch();
        Ok(PreviewTicket::new(ticket_id))
    }

    /// Releases one caller's interest. The last waiter of a still-**queued**
    /// build removes it entirely, it will never run (spec AC: "cancel takes
    /// effect at the next step boundary", the checkpoint this module's own
    /// doc comment names). The last waiter of an already-**running** build
    /// cancels that build's [`CancelToken`] (meaningful only for the narrow
    /// window before the `BuildRuntime` actually polls it, see the token's
    /// use in [`Self::dispatch_one`]); the build itself, once actually
    /// running, is not interruptible at M0. Unknown/already-cancelled
    /// tickets are a no-op (idempotent, matches [`crate::PreviewProvider::
    /// cancel`]'s existing contract).
    pub fn cancel(&self, ticket: &PreviewTicket) {
        let mut st = self.lock();
        let Some(key) = st.tickets.remove(&ticket.id()) else {
            return;
        };
        if let Some(pending) = st.pending.get_mut(&key) {
            pending.waiters = pending.waiters.saturating_sub(1);
            if pending.waiters == 0 {
                if let Some(removed) = st.pending.remove(&key) {
                    if removed.priority == BuildPriority::Bulk {
                        st.bulk_queued = st.bulk_queued.saturating_sub(1);
                    }
                }
                // The queues[] entry is left in place; try_dispatch's
                // pending-match check discards it lazily.
            }
        } else if let Some(running) = st.running.get_mut(&key) {
            running.waiters = running.waiters.saturating_sub(1);
            if running.waiters == 0 {
                running.cancel.cancel();
            }
        }
    }

    /// In-place priority change for a still-**queued** ticket (spec §5.2
    /// `reprioritize`; §3.4 "a Visible enqueue upgrades \[a queued Bulk
    /// build\] in place"). A no-op for an unknown ticket or one whose build
    /// is already running (can't preempt already-dispatched synchronous
    /// work at M0, same limitation [`Self::cancel`] documents).
    pub fn reprioritize(self: &Arc<Self>, ticket: &PreviewTicket, new_priority: BuildPriority) {
        {
            let mut st = self.lock();
            let Some(&key) = st.tickets.get(&ticket.id()) else {
                return;
            };
            if st.pending.contains_key(&key) {
                let old_priority = st.pending[&key].priority;
                if old_priority != new_priority {
                    if old_priority == BuildPriority::Bulk {
                        st.bulk_queued = st.bulk_queued.saturating_sub(1);
                    }
                    st.pending.get_mut(&key).expect("checked above").priority = new_priority;
                    if new_priority == BuildPriority::Bulk {
                        st.bulk_queued += 1;
                    }
                    st.queues[new_priority.rank()].push_back(key);
                }
            }
        }
        self.try_dispatch();
    }

    /// Distinct builds currently queued (not yet dispatched), the
    /// authoritative count (unlike a raw `queues[]` length, which may hold
    /// stale duplicates; see [`State::queues`]'s doc comment).
    pub fn pending_len(&self) -> usize {
        self.lock().pending.len()
    }

    /// Builds currently occupying a concurrency slot.
    pub fn running_len(&self) -> usize {
        self.lock().running.len()
    }

    /// Pending entries specifically at `priority` (diagnostics/tests).
    pub fn pending_len_at(&self, priority: BuildPriority) -> usize {
        self.lock()
            .pending
            .values()
            .filter(|m| m.priority == priority)
            .count()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drains ready-to-run work into the `BuildRuntime` up to the
    /// concurrency bound. Visible-first: always checks rank 2 (Visible)
    /// before 1 (Neighbor) before 0 (Bulk), this IS the spec §3.4
    /// visible-first guarantee ("Visible work always dequeues first").
    fn try_dispatch(self: &Arc<Self>) {
        loop {
            let dispatched = {
                let mut st = self.lock();
                if st.available_slots == 0 {
                    return;
                }
                let key = loop {
                    let Some(rank) = [2usize, 1, 0]
                        .into_iter()
                        .find(|&r| !st.queues[r].is_empty())
                    else {
                        return; // nothing queued at any priority
                    };
                    let candidate = st.queues[rank]
                        .pop_front()
                        .expect("checked non-empty above");
                    match st.pending.get(&candidate) {
                        Some(meta) if meta.priority.rank() == rank => break candidate,
                        // Stale: superseded by an upgrade/downgrade, already
                        // dispatched, or fully cancelled while queued.
                        // Discard and keep looking.
                        _ => continue,
                    }
                };
                let meta = st.pending.remove(&key).expect("just matched above");
                if meta.priority == BuildPriority::Bulk {
                    st.bulk_queued = st.bulk_queued.saturating_sub(1);
                }
                st.available_slots -= 1;
                let cancel = CancelToken::new();
                st.running.insert(
                    key,
                    RunningMeta {
                        waiters: meta.waiters,
                        cancel: cancel.clone(),
                        bulk: meta.bulk,
                    },
                );
                (key, cancel)
            };
            self.dispatch_one(dispatched.0, dispatched.1);
        }
    }

    fn dispatch_one(self: &Arc<Self>, key: BuildKey, cancel: CancelToken) {
        let this = Arc::clone(self);
        let build_fn = Arc::clone(&self.build_fn);
        let fut: BuildFuture = Box::pin(async move {
            let result = if cancel.is_cancelled() {
                Err(PreviewError::Cancelled)
            } else {
                build_fn(key, &cancel)
            };
            this.on_build_finished(key, result);
        });
        self.runtime
            .spawn_build("lightbox_preview.scheduled_build", fut);
    }

    fn on_build_finished(
        self: &Arc<Self>,
        key: BuildKey,
        result: Result<PreviewDesc, PreviewError>,
    ) {
        let bulk_sessions = {
            let mut st = self.lock();
            st.available_slots += 1;
            st.running.remove(&key).map(|m| m.bulk).unwrap_or_default()
        };
        match &result {
            Ok(desc) => (self.events)(PreviewEvent::Ready {
                image: key.image,
                tier: key.tier,
                desc: desc.clone(),
            }),
            Err(err) => (self.events)(PreviewEvent::Failed {
                image: key.image,
                tier: key.tier,
                error: err.clone(),
            }),
        }
        for session in &bulk_sessions {
            // Lock held across BOTH the increment and the emit, see
            // `BulkSession::done`'s doc comment for why that (not just the
            // increment) is what needs to be atomic here.
            let mut done_guard = session.done.lock().unwrap_or_else(PoisonError::into_inner);
            *done_guard += 1;
            let done = *done_guard;
            (self.events)(PreviewEvent::BulkProgress {
                done,
                total: session.total,
            });
            drop(done_guard);
        }
        self.pump_bulk();
        self.try_dispatch();
    }

    // ── T16: bulk build + backlog pump ──────────────────────────────────

    /// "Build previews for selection" (T16): accepts the whole `keys` list
    /// immediately (no per-caller backpressure, that's [`Self::request`]'s
    /// `Bulk`-priority `QueueFull` signal, aimed at E04's import loop, spec
    /// S3) and trickles them into the bounded `queues[Bulk]` as capacity
    /// frees. This is why the backlog is a *second*, unbounded stage ahead
    /// of the bounded one: "build previews for this 1000-image selection"
    /// must not fail outright just because more than `bulk_bound` images
    /// were selected, spec's T16 AC ("queue never exceeds bound") is about
    /// the bounded execution queue, not the caller-facing selection size.
    pub fn bulk_build(self: &Arc<Self>, keys: Vec<BuildKey>) -> BulkHandle {
        let id = self.next_bulk_id.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(BulkSession {
            id,
            total: keys.len() as u64,
            done: Mutex::new(0),
            cancelled: AtomicBool::new(false),
            tickets: Mutex::new(Vec::new()),
        });
        {
            let mut st = self.lock();
            for key in keys {
                st.backlog.push_back((key, Arc::clone(&session)));
            }
        }
        self.pump_bulk();
        BulkHandle { session }
    }

    /// Stops admitting `handle`'s remaining backlog into the bounded queue
    /// and cancels every ticket already issued for it (T16 AC: "cancel
    /// mid-run stops within worker-step latency"). Already-dispatched
    /// builds for this session run to completion (still tick `done`/emit
    /// `BulkProgress`, [`BulkSession::total`] is fixed at creation, so a
    /// cancelled run's `done` simply stops advancing once its in-flight
    /// builds finish, never reaching `total`).
    pub fn cancel_bulk(&self, handle: &BulkHandle) {
        handle.session.cancelled.store(true, Ordering::Release);
        let tickets: Vec<PreviewTicket> = {
            let mut guard = handle
                .session
                .tickets
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        for t in &tickets {
            self.cancel(t);
        }
    }

    /// Admits backlog entries into `queues[Bulk]` up to [`Self::bulk_bound`].
    /// Called after every `bulk_build` (initial fill) and every build
    /// completion (`on_build_finished`, freeing room). Skips (drops, does
    /// NOT enqueue) entries whose session has since been cancelled.
    fn pump_bulk(self: &Arc<Self>) {
        loop {
            let next = {
                let mut st = self.lock();
                if st.bulk_queued >= self.bulk_bound {
                    None
                } else {
                    st.backlog.pop_front()
                }
            };
            let Some((key, session)) = next else {
                return;
            };
            if session.cancelled.load(Ordering::Acquire) {
                continue;
            }
            match self.enqueue_internal(key, BuildPriority::Bulk, vec![Arc::clone(&session)]) {
                Ok(ticket) => session
                    .tickets
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(ticket),
                Err(EnqueueError::QueueFull) => {
                    // Race against another concurrent admitter: put it back
                    // and stop, the next completion's `on_build_finished`
                    // will retry.
                    self.lock().backlog.push_front((key, session));
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;
    use std::thread;
    use std::time::Duration;

    use lightbox_types::ImageId;

    fn key(n: i64) -> BuildKey {
        BuildKey {
            image: ImageId(n),
            tier: Tier::T1,
        }
    }

    fn desc_for(k: BuildKey) -> PreviewDesc {
        crate::pyramid::PreviewDesc {
            scope: crate::pyramid::PreviewScope::Image(k.image),
            tier: k.tier,
            variant: crate::pyramid::VariantHash(0),
            source: crate::pyramid::PreviewSource::Embedded,
            recipe_rev: 0,
            stale: false,
            width: 100,
            height: 100,
            colorspace: crate::pyramid::PreviewColorspace::Srgb,
            store_path: crate::pyramid::RelPath("previews/aa/x.t1.jpg".to_owned()),
            bytes: 1,
            built_at: 0,
        }
    }

    /// Runs spawned futures to completion **on demand**, one at a time, in
    /// the calling test thread, fully deterministic (spec's `concurrency()`
    /// governs how many builds the scheduler ever hands over at once; this
    /// runtime just controls WHEN each already-handed-over one is polled).
    /// A no-op waker is correct here because the scheduler's futures never
    /// actually await anything (they wrap a synchronous producer call)
    /// the same assumption `lightbox_jobs::JobHandle::try_result` relies on.
    #[derive(Default)]
    struct ManualRuntime {
        concurrency: usize,
        queued: StdMutex<VecDeque<BuildFuture>>,
    }

    impl ManualRuntime {
        fn new(concurrency: usize) -> Arc<ManualRuntime> {
            Arc::new(ManualRuntime {
                concurrency,
                queued: StdMutex::new(VecDeque::new()),
            })
        }

        /// Runs every future currently queued (including ones newly queued
        /// as a side effect of running an earlier one) until none remain.
        fn run_all(&self) {
            loop {
                let next = self.queued.lock().unwrap().pop_front();
                let Some(mut fut) = next else { return };
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                loop {
                    match fut.as_mut().poll(&mut cx) {
                        std::task::Poll::Ready(()) => break,
                        std::task::Poll::Pending => std::thread::yield_now(),
                    }
                }
            }
        }

        /// Runs exactly one already-queued future (panics if none is
        /// queued), for tests asserting dispatch order step by step.
        fn run_one(&self) {
            let mut fut = self
                .queued
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected a queued build");
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            loop {
                match fut.as_mut().poll(&mut cx) {
                    std::task::Poll::Ready(()) => break,
                    std::task::Poll::Pending => std::thread::yield_now(),
                }
            }
        }
    }

    impl BuildRuntime for ManualRuntime {
        fn spawn_build(&self, _name: &'static str, fut: BuildFuture) {
            self.queued.lock().unwrap().push_back(fut);
        }

        fn concurrency(&self) -> usize {
            self.concurrency
        }
    }

    /// A real, concurrency-exercising runtime: one OS thread per build.
    struct ThreadRuntime {
        concurrency: usize,
    }

    impl BuildRuntime for ThreadRuntime {
        fn spawn_build(&self, _name: &'static str, mut fut: BuildFuture) {
            thread::spawn(move || {
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                loop {
                    match fut.as_mut().poll(&mut cx) {
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

    fn counting_build_fn() -> (BuildFn, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&count);
        let f: BuildFn = Arc::new(move |key, _cancel| {
            c2.fetch_add(1, Ordering::SeqCst);
            Ok(desc_for(key))
        });
        (f, count)
    }

    fn no_op_events() -> EventSink {
        Arc::new(|_ev| {})
    }

    // ── T13 AC: dedup collapses N identical requests to 1 build ─────────

    #[test]
    fn dedup_collapses_n_identical_requests_to_one_build() {
        let runtime = ManualRuntime::new(1);
        let (build_fn, count) = counting_build_fn();
        let sched = Scheduler::new(
            Arc::clone(&runtime) as Arc<dyn BuildRuntime>,
            build_fn,
            no_op_events(),
        );

        let k = key(1);
        let mut tickets = Vec::new();
        for _ in 0..50 {
            tickets.push(sched.request(k, BuildPriority::Visible).unwrap());
        }
        runtime.run_all();

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "50 identical requests must build exactly once"
        );
        // All 50 tickets are cancel-safe even after completion (idempotent, no panic).
        for t in &tickets {
            sched.cancel(t);
        }
    }

    // ── T13 AC: a Visible enqueue overtakes queued Bulk 100% of trials ──

    #[test]
    fn visible_overtakes_queued_bulk_every_trial() {
        const TRIALS: usize = 100;
        for trial in 0..TRIALS {
            let runtime = ManualRuntime::new(1);
            let dispatch_order = Arc::new(StdMutex::new(Vec::<BuildKey>::new()));
            let order2 = Arc::clone(&dispatch_order);
            let build_fn: BuildFn = Arc::new(move |k, _cancel| {
                order2.lock().unwrap().push(k);
                Ok(desc_for(k))
            });
            let sched = Scheduler::new(
                Arc::clone(&runtime) as Arc<dyn BuildRuntime>,
                build_fn,
                no_op_events(),
            );

            // Saturate the single slot with a throwaway build, then queue 3
            // Bulk builds behind it (concurrency=1 means none of them start
            // until the first one is stepped; bulk_bound = 4*concurrency = 4,
            // so 3 queued + the running occupier stays within bound).
            let occupier = sched
                .request(key(1000 + trial as i64), BuildPriority::Bulk)
                .unwrap();
            let _ = occupier;
            for i in 0..3 {
                sched
                    .request(key(2000 + trial as i64 * 100 + i), BuildPriority::Bulk)
                    .unwrap();
            }
            assert_eq!(
                sched.pending_len(),
                3,
                "trial {trial}: 3 bulk builds must be queued, not running"
            );

            // A fresh Visible request arrives after the Bulk backlog exists.
            let visible_key = key(3000 + trial as i64);
            sched.request(visible_key, BuildPriority::Visible).unwrap();
            assert_eq!(sched.pending_len(), 4, "trial {trial}");

            // Step the occupier: this frees the only slot and re-dispatches.
            runtime.run_one();

            // The SECOND thing ever dispatched (first was the occupier) must
            // be the Visible key, not any of the 9 queued Bulk builds.
            runtime.run_one();
            let order = dispatch_order.lock().unwrap();
            assert_eq!(
                order.get(1),
                Some(&visible_key),
                "trial {trial}: Visible must overtake queued Bulk; dispatch order was {order:?}"
            );
        }
    }

    // ── T13 AC: cancel takes effect at the next step boundary ───────────

    #[test]
    fn cancelling_a_queued_build_prevents_it_from_ever_running() {
        let runtime = ManualRuntime::new(1);
        let (build_fn, count) = counting_build_fn();
        let sched = Scheduler::new(
            Arc::clone(&runtime) as Arc<dyn BuildRuntime>,
            build_fn,
            no_op_events(),
        );

        // Saturate the one slot so the next request stays queued.
        let _occupier = sched.request(key(1), BuildPriority::Visible).unwrap();
        let victim = sched.request(key(2), BuildPriority::Visible).unwrap();
        assert_eq!(sched.pending_len(), 1);

        sched.cancel(&victim);
        assert_eq!(
            sched.pending_len(),
            0,
            "cancelled queued build must be dropped immediately"
        );

        // Free the slot: try_dispatch runs, but there is nothing left to
        // dispatch for the cancelled key.
        runtime.run_all();
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "only the occupier ever actually built"
        );
    }

    // ── T13 AC: Bulk `try_enqueue` over bound returns `QueueFull` ────────

    #[test]
    fn bulk_over_bound_returns_queue_full_and_recovers_after_cancel() {
        let runtime = ManualRuntime::new(1); // bulk_bound = 4
        let (build_fn, _count) = counting_build_fn();
        let sched = Scheduler::new(
            Arc::clone(&runtime) as Arc<dyn BuildRuntime>,
            build_fn,
            no_op_events(),
        );
        assert_eq!(sched.bulk_bound(), 4);

        // #1 dispatches immediately (consumes the sole slot); #2..#5 queue
        // (bulk_bound=4 queued entries); #6 must be refused.
        let mut tickets = Vec::new();
        for i in 0..5 {
            tickets.push(sched.request(key(i), BuildPriority::Bulk).unwrap());
        }
        assert_eq!(sched.pending_len(), 4, "4 queued at the bound");
        let refused = sched.request(key(999), BuildPriority::Bulk);
        assert!(
            matches!(refused, Err(EnqueueError::QueueFull)),
            "{refused:?}"
        );

        // Cancel one queued entry: frees a queue slot, the next enqueue
        // succeeds again.
        sched.cancel(&tickets[1]); // tickets[0] is the running occupier
        assert_eq!(sched.pending_len(), 3);
        let recovered = sched.request(key(1000), BuildPriority::Bulk);
        assert!(recovered.is_ok(), "{recovered:?}");
    }

    // ── Bounded concurrency ──────────────────────────────────────────────

    #[test]
    fn concurrency_never_exceeds_the_configured_bound() {
        let max_seen = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let max2 = Arc::clone(&max_seen);
        let cur2 = Arc::clone(&current);
        let build_fn: BuildFn = Arc::new(move |k, _cancel| {
            let now = cur2.fetch_add(1, Ordering::SeqCst) + 1;
            max2.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
            cur2.fetch_sub(1, Ordering::SeqCst);
            Ok(desc_for(k))
        });
        let runtime = Arc::new(ThreadRuntime { concurrency: 4 });
        let sched = Scheduler::new(runtime, build_fn, no_op_events());

        // Visible priority: never subject to the Bulk-only QueueFull bound
        // (unlike Bulk, which is bounded to 4x concurrency, irrelevant to
        // what this test is actually probing).
        for i in 0..40 {
            sched.request(key(i), BuildPriority::Visible).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sched.running_len() > 0 || sched.pending_len() > 0 {
            assert!(std::time::Instant::now() < deadline, "builds never drained");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            max_seen.load(Ordering::SeqCst) <= 4,
            "observed {} concurrent builds, bound was 4",
            max_seen.load(Ordering::SeqCst)
        );
    }

    // ── T16 AC: 1k-image bulk run, queue never exceeds bound, progress
    //    monotonic to completion, index/store (here: build_fn call count)
    //    left consistent. Uses a cheap synthetic build_fn (real
    //    ensure_t0/ensure_t1 encode latency is exercised in
    //    `service.rs`'s integration tests at a scale that fits a
    //    PR-blocking suite; see the Phase D deviations entry). ──

    #[test]
    fn bulk_build_1k_images_bounds_the_queue_and_progresses_monotonically() {
        let runtime = Arc::new(ThreadRuntime { concurrency: 8 }); // bulk_bound = 32
        let (build_fn, count) = counting_build_fn();
        let progress_log = Arc::new(StdMutex::new(Vec::<(u64, u64)>::new()));
        let log2 = Arc::clone(&progress_log);
        let max_bulk_queue = Arc::new(AtomicUsize::new(0));
        let events: EventSink = {
            let log = log2;
            Arc::new(move |ev| {
                if let PreviewEvent::BulkProgress { done, total } = ev {
                    log.lock().unwrap().push((done, total));
                }
            })
        };
        let sched = Scheduler::new(runtime, build_fn, events);

        let keys: Vec<BuildKey> = (0..1000).map(key).collect();
        let handle = sched.bulk_build(keys);

        let watcher_sched = Arc::clone(&sched);
        let watcher_bound = watcher_sched.bulk_bound();
        let max2 = Arc::clone(&max_bulk_queue);
        let watching = Arc::new(AtomicBool::new(true));
        let watching2 = Arc::clone(&watching);
        let watcher = thread::spawn(move || {
            while watching2.load(Ordering::Acquire) {
                let n = watcher_sched.pending_len_at(BuildPriority::Bulk);
                max2.fetch_max(n, Ordering::SeqCst);
                thread::sleep(Duration::from_micros(200));
            }
            let _ = watcher_bound;
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let (done, total) = handle.progress();
            if done >= total {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "1k bulk build never finished (done={done}/{total})"
            );
            thread::sleep(Duration::from_millis(2));
        }
        watching.store(false, Ordering::Release);
        watcher.join().unwrap();

        assert_eq!(
            count.load(Ordering::SeqCst),
            1000,
            "every image must have built exactly once"
        );
        assert_eq!(handle.progress(), (1000, 1000));
        assert!(
            max_bulk_queue.load(Ordering::SeqCst) <= sched.bulk_bound(),
            "bulk queue exceeded its bound: saw {}, bound {}",
            max_bulk_queue.load(Ordering::SeqCst),
            sched.bulk_bound()
        );

        let log = progress_log.lock().unwrap();
        assert_eq!(
            log.len(),
            1000,
            "one BulkProgress event per completed build"
        );
        let mut prev = 0u64;
        for &(done, total) in log.iter() {
            assert!(
                done > prev,
                "progress must be strictly increasing: {done} after {prev}"
            );
            assert_eq!(total, 1000);
            prev = done;
        }
        assert_eq!(prev, 1000);
    }

    #[test]
    fn cancel_bulk_stops_admitting_new_work_promptly() {
        let runtime = Arc::new(ThreadRuntime { concurrency: 2 }); // bulk_bound = 8
        let started = Arc::new(AtomicUsize::new(0));
        let s2 = Arc::clone(&started);
        let build_fn: BuildFn = Arc::new(move |k, _cancel| {
            s2.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            Ok(desc_for(k))
        });
        let sched = Scheduler::new(runtime, build_fn, no_op_events());

        let keys: Vec<BuildKey> = (0..500).map(key).collect();
        let handle = sched.bulk_build(keys);
        thread::sleep(Duration::from_millis(5)); // let a few start
        sched.cancel_bulk(&handle);
        let started_at_cancel = started.load(Ordering::SeqCst);

        // Give any already-running builds worker-step latency to finish,
        // then assert no meaningful further admission happened.
        thread::sleep(Duration::from_millis(100));
        let started_after = started.load(Ordering::SeqCst);
        assert!(
            started_after <= started_at_cancel + sched_concurrency(&handle),
            "cancel_bulk let far more work start afterwards: {started_at_cancel} -> {started_after}"
        );
        let (done, total) = handle.progress();
        assert!(
            done < total,
            "a cancelled 500-image run must not reach completion"
        );
    }

    /// Small helper so the assertion above has a named slack budget instead
    /// of a magic number (at most one in-flight build per worker could have
    /// been mid-dispatch at the moment of cancel).
    fn sched_concurrency(_h: &BulkHandle) -> usize {
        2
    }

    // ── in-place upgrade / downgrade ─────────────────────────────────────

    #[test]
    fn reprioritize_moves_a_queued_build_between_priority_classes() {
        let runtime = ManualRuntime::new(1);
        let (build_fn, _count) = counting_build_fn();
        let sched = Scheduler::new(
            Arc::clone(&runtime) as Arc<dyn BuildRuntime>,
            build_fn,
            no_op_events(),
        );

        let _occupier = sched.request(key(1), BuildPriority::Visible).unwrap();
        let t = sched.request(key(2), BuildPriority::Bulk).unwrap();
        assert_eq!(sched.pending_len_at(BuildPriority::Bulk), 1);
        assert_eq!(sched.pending_len_at(BuildPriority::Visible), 0);

        sched.reprioritize(&t, BuildPriority::Visible);
        assert_eq!(sched.pending_len_at(BuildPriority::Bulk), 0);
        assert_eq!(sched.pending_len_at(BuildPriority::Visible), 1);

        // Downgrade back.
        sched.reprioritize(&t, BuildPriority::Neighbor);
        assert_eq!(sched.pending_len_at(BuildPriority::Visible), 0);
        assert_eq!(sched.pending_len_at(BuildPriority::Neighbor), 1);
    }
}
