// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The render scheduler — the coalescing front-end (spec §3.7/§4.3/§5.1).
//!
//! Owner: **A-gpu** owns the canvas double-buffer + [`CanvasFrame`] publisher
//! (task A13); **B** owns latest-wins coalescing (one in-flight + one pending
//! slot per image, task B6). Consumed by the shell (E08) via
//! [`RenderScheduler::canvas`].
//!
//! # Latest-wins coalescing (task B6)
//!
//! A rapid slider drag produces a burst of `set_recipe` calls. The scheduler
//! keeps **one in-flight render + one pending slot** per image: while a render
//! is in flight, newer requests overwrite the pending slot (dropping every
//! intermediate), and when the in-flight completes the newest pending is
//! submitted. So a burst of *N* rapid updates results in **at most 2** engine
//! submissions once the first render is in flight, and the final frame reflects
//! the **final** params. The pure decision core is [`Coalescer`]; the
//! [`RenderScheduler`] wires it to the [`Engine`] with a background driver that
//! promotes the pending slot as renders finish.

pub mod canvas;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lightbox_edit::Recipe;
use lightbox_jobs::{CancelToken, JobSystem};
use lightbox_types::{ImageId, ProcessVersion};
use tokio::sync::watch;

use crate::ng::engine::{
    Engine, OutFormat, OutputQuality, RenderOutput, RenderPriority, RenderRequest, RenderState,
    RenderTarget, RenderTicket,
};
use crate::ng::types::{Extent, RenderScale, Roi};

/// A zoom factor (1.0 = 1:1). Part of [`ViewState`] (spec §3.7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom(pub f32);

/// The shell's current view of an image (spec §3.7).
#[derive(Clone, Copy, Debug)]
pub struct ViewState {
    /// The viewport extent, pixels.
    pub viewport: Extent,
    /// The current zoom factor.
    pub zoom: Zoom,
    /// The panned/visible region, source-pixel coordinates.
    pub pan: Roi,
}

impl ViewState {
    /// A small default view used until the shell supplies one.
    fn default_view() -> ViewState {
        ViewState {
            viewport: Extent { w: 64, h: 64 },
            zoom: Zoom(1.0),
            pan: Roi {
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            },
        }
    }
}

/// A handle to the E06 jobs system, for registering Interactive-class render
/// work (spec §3.7). **B/F wire the real registration.**
pub struct JobsHandle(pub Arc<JobSystem>);

/// The completed-frame currency the shell subscribes to (spec §3.7). The shell
/// samples only completed `generation`s (double-buffer).
#[derive(Clone, Debug)]
pub struct CanvasFrame {
    /// Monotonic generation; the shell samples only completed ones.
    pub generation: u64,
    /// The output texture view — same-device, zero-copy (§2.3 seam 2).
    pub texture: wgpu::TextureView,
    /// Drives the "Loading"/preview badge (§4.3).
    pub quality: OutputQuality,
    /// Output extent, pixels.
    pub extent: Extent,
}

/// The pure latest-wins decision core (spec §3.7/§4.3; task B6).
///
/// Holds "one in-flight + one pending" bit of state. [`Coalescer::submit`]
/// returns `Some(item)` when the item should be dispatched immediately (nothing
/// in flight) or stores it as the pending slot (overwriting any prior pending)
/// and returns `None`. [`Coalescer::complete`] is called when the in-flight
/// dispatch finishes; it promotes the pending slot (returning it to dispatch) or
/// clears the in-flight flag. This is what bounds a burst of *N* updates to at
/// most 2 dispatches.
#[derive(Debug)]
pub struct Coalescer<T> {
    inflight: bool,
    pending: Option<T>,
}

impl<T> Default for Coalescer<T> {
    fn default() -> Self {
        Coalescer {
            inflight: false,
            pending: None,
        }
    }
}

impl<T> Coalescer<T> {
    /// A fresh, idle coalescer.
    pub fn new() -> Coalescer<T> {
        Coalescer::default()
    }

    /// Offer an item. Returns `Some` to dispatch now (nothing in flight), else
    /// stores it as the latest pending (dropping any older pending) and returns
    /// `None`.
    pub fn submit(&mut self, item: T) -> Option<T> {
        if self.inflight {
            self.pending = Some(item);
            None
        } else {
            self.inflight = true;
            Some(item)
        }
    }

    /// Signal that the in-flight dispatch finished. Returns the promoted pending
    /// item to dispatch next (staying in-flight), or `None` if the slot is now
    /// idle.
    pub fn complete(&mut self) -> Option<T> {
        match self.pending.take() {
            Some(next) => Some(next), // stays in-flight; dispatch `next`
            None => {
                self.inflight = false;
                None
            }
        }
    }

    /// Whether a dispatch is in flight or a pending item is queued.
    pub fn has_work(&self) -> bool {
        self.inflight || self.pending.is_some()
    }
}

/// One image's render job: the recipe/pv + view that produce a frame, tagged
/// with a monotonic sequence so the scheduler can prove the final frame reflects
/// the final request.
#[derive(Clone)]
struct RenderJob {
    recipe: Recipe,
    pv: ProcessVersion,
    view: ViewState,
    seq: u64,
}

/// Per-image scheduling state.
struct ImageState {
    coalescer: Coalescer<RenderJob>,
    inflight_ticket: Option<RenderTicket>,
    inflight_seq: u64,
    last_view: Option<ViewState>,
    last_recipe: Option<(Recipe, ProcessVersion)>,
    /// The most recently completed `(seq, output)` — proves final-wins.
    last_output: Option<(u64, RenderOutput)>,
}

impl Default for ImageState {
    fn default() -> Self {
        ImageState {
            coalescer: Coalescer::new(),
            inflight_ticket: None,
            inflight_seq: 0,
            last_view: None,
            last_recipe: None,
            last_output: None,
        }
    }
}

/// Shared scheduler state driven by both the public API and the driver thread.
struct SchedShared {
    engine: Arc<Engine>,
    images: Mutex<HashMap<ImageId, ImageState>>,
    seq: AtomicU64,
    submissions: AtomicU64,
    shutdown: AtomicBool,
    #[allow(dead_code)] // real Interactive-class registration is B/F wiring
    jobs: JobsHandle,
}

impl SchedShared {
    /// Build a `Buffer`-target Interactive request from a job (the headless,
    /// CPU-provable path; the canvas double-buffer wiring is task A13).
    fn to_request(image: ImageId, job: &RenderJob) -> RenderRequest {
        RenderRequest {
            image,
            recipe: job.recipe.clone(),
            pv: job.pv,
            roi: job.view.pan,
            scale: RenderScale::Fit(job.view.viewport),
            target: RenderTarget::Buffer {
                format: OutFormat::Rgba8Srgb,
            },
            priority: RenderPriority::Interactive,
            cancel: CancelToken::new(),
        }
    }

    /// Dispatch `job` to the engine, recording the in-flight ticket + seq and
    /// bumping the submission counter. Caller holds the `images` lock.
    fn dispatch(&self, image: ImageId, st: &mut ImageState, job: RenderJob) {
        let req = Self::to_request(image, &job);
        let ticket = self.engine.submit(req);
        st.inflight_ticket = Some(ticket);
        st.inflight_seq = job.seq;
        self.submissions.fetch_add(1, Ordering::Relaxed);
    }

    /// One driver pass: poll every in-flight ticket; on a terminal state record
    /// the output and promote the pending slot. Returns whether any image still
    /// has work in flight or pending.
    fn drive_once(&self) -> bool {
        let mut images = self
            .images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut any_work = false;
        let ids: Vec<ImageId> = images.keys().copied().collect();
        for id in ids {
            let st = images.get_mut(&id).expect("id from keys");
            if let Some(ticket) = st.inflight_ticket {
                match self.engine.poll(&ticket) {
                    RenderState::Complete(out) | RenderState::PreviewReady(out) => {
                        st.last_output = Some((st.inflight_seq, out));
                        st.inflight_ticket = None;
                        if let Some(next) = st.coalescer.complete() {
                            self.dispatch(id, st, next);
                        }
                    }
                    RenderState::Failed(_) | RenderState::Cancelled => {
                        st.inflight_ticket = None;
                        if let Some(next) = st.coalescer.complete() {
                            self.dispatch(id, st, next);
                        }
                    }
                    RenderState::Queued | RenderState::Rendering { .. } => {}
                }
            }
            if st.coalescer.has_work() {
                any_work = true;
            }
        }
        any_work
    }
}

/// Owns latest-wins slots per image (one in-flight + one pending) and publishes
/// completed frames on a watch channel (spec §3.7). The coalescing + engine
/// driving is task **B6**; the canvas texture publisher is task **A13**.
pub struct RenderScheduler {
    shared: Arc<SchedShared>,
    driver: Option<JoinHandle<()>>,
}

impl RenderScheduler {
    /// A scheduler driving `engine`, registering work through `jobs`. Spawns a
    /// background driver thread that promotes pending renders as they finish.
    pub fn new(engine: Arc<Engine>, jobs: JobsHandle) -> RenderScheduler {
        let shared = Arc::new(SchedShared {
            engine,
            images: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            submissions: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            jobs,
        });
        let driver_shared = Arc::clone(&shared);
        let driver = std::thread::Builder::new()
            .name("lbx-render-sched".to_owned())
            .spawn(move || {
                while !driver_shared.shutdown.load(Ordering::Relaxed) {
                    driver_shared.drive_once();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .expect("scheduler driver thread spawns");
        RenderScheduler {
            shared,
            driver: Some(driver),
        }
    }

    fn next_seq(&self) -> u64 {
        self.shared.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Set the recipe for `image` (latest-wins debounce; task B6). Coalesces
    /// with the in-flight render: while one is in flight this only overwrites the
    /// pending slot.
    pub fn set_recipe(&self, image: ImageId, recipe: Recipe, pv: ProcessVersion) {
        let seq = self.next_seq();
        let mut images = self
            .shared
            .images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let st = images.entry(image).or_default();
        st.last_recipe = Some((recipe.clone(), pv));
        let view = st.last_view.unwrap_or_else(ViewState::default_view);
        let job = RenderJob {
            recipe,
            pv,
            view,
            seq,
        };
        if let Some(job) = st.coalescer.submit(job) {
            self.shared.dispatch(image, st, job);
        }
    }

    /// Set the view for `image` (roi/zoom churn; task B6/C4). Re-renders the
    /// current recipe at the new view, coalescing latest-wins.
    pub fn set_view(&self, image: ImageId, view: ViewState) {
        let seq = self.next_seq();
        let mut images = self
            .shared
            .images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let st = images.entry(image).or_default();
        st.last_view = Some(view);
        let Some((recipe, pv)) = st.last_recipe.clone() else {
            return; // nothing to render until a recipe is set
        };
        let job = RenderJob {
            recipe,
            pv,
            view,
            seq,
        };
        if let Some(job) = st.coalescer.submit(job) {
            self.shared.dispatch(image, st, job);
        }
    }

    /// Total engine submissions so far — the B6 coalescing probe.
    pub fn submissions(&self) -> u64 {
        self.shared.submissions.load(Ordering::Relaxed)
    }

    /// The sequence number of the most recently completed frame for `image`
    /// (the "final frame reflects final params" probe: after quiescence this
    /// equals the last `set_recipe`/`set_view` sequence).
    pub fn last_completed_seq(&self, image: ImageId) -> Option<u64> {
        let images = self
            .shared
            .images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        images
            .get(&image)
            .and_then(|s| s.last_output.as_ref().map(|(seq, _)| *seq))
    }

    /// The most recently completed [`RenderOutput`] for `image`.
    pub fn last_output(&self, image: ImageId) -> Option<RenderOutput> {
        let images = self
            .shared
            .images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        images
            .get(&image)
            .and_then(|s| s.last_output.as_ref().map(|(_, out)| out.clone()))
    }

    /// Block until every image is idle (no in-flight, no pending) or `timeout`
    /// elapses; returns `true` if it reached quiescence. A test/soak helper.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let idle = {
                let images = self
                    .shared
                    .images
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                images
                    .values()
                    .all(|s| s.inflight_ticket.is_none() && !s.coalescer.has_work())
            };
            if idle {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Subscribe to completed canvas frames (spec §3.7; task A13). The canvas
    /// double-buffer publisher requires the shell's shared GPU device and is
    /// wired by **A-gpu (A13)** / M1 integration (**F5**); the headless
    /// coalescing path (B6) drives `Buffer`-target renders observable via
    /// [`RenderScheduler::last_output`].
    pub fn canvas(&self) -> watch::Receiver<CanvasFrame> {
        unimplemented!(
            "A13 (A-gpu)/F5: RenderScheduler::canvas — GPU canvas double-buffer publisher \
             (needs the shell device); B6 coalescing is observable via last_output/submissions"
        )
    }
}

impl Drop for RenderScheduler {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── B6: the pure coalescing core bounds a burst to ≤ 2 dispatches ───────
    #[test]
    fn coalescer_bounds_a_burst_to_two_dispatches() {
        let mut c: Coalescer<u32> = Coalescer::new();
        let mut dispatched = Vec::new();
        // First submit dispatches immediately; the next 999 only overwrite the
        // pending slot (latest-wins).
        for i in 0..1000u32 {
            if let Some(item) = c.submit(i) {
                dispatched.push(item);
            }
        }
        assert_eq!(
            dispatched,
            vec![0],
            "only the first of the burst dispatches"
        );
        // The in-flight completes → promote the latest pending (999).
        let next = c.complete();
        assert_eq!(next, Some(999), "final params win");
        dispatched.push(next.unwrap());
        // That completes with nothing pending → idle.
        assert_eq!(c.complete(), None);
        assert!(!c.has_work());
        assert_eq!(
            dispatched.len(),
            2,
            "≤ 2 dispatches for a 1000-update burst"
        );
    }

    #[test]
    fn coalescer_interleaved_completions_stay_bounded() {
        let mut c: Coalescer<u32> = Coalescer::new();
        let mut dispatches = 0;
        for i in 0..10u32 {
            if c.submit(i).is_some() {
                dispatches += 1;
            }
            if c.complete().is_some() {
                dispatches += 1;
            }
        }
        while c.complete().is_some() {
            dispatches += 1;
        }
        assert!(dispatches <= 10);
        assert!(!c.has_work());
    }
}
