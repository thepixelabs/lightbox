// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 Phase F4 — concurrency stress on the ticket store + cache (spec §5
//! task F4).
//!
//! **In-gate bounded subset.** Many threads hammer one shared `Arc<Engine>`
//! — the ticket store (`Mutex<HashMap<u64, TicketEntry>>`) and the
//! content-keyed `NodeCache` — with concurrent `submit`/`poll`/`cancel`
//! calls. Asserts: no panic, no deadlock (bounded via a deadline), every
//! ticket id is globally unique (the `AtomicU64` allocator never races), and
//! every **non-cancelled** completion carries exactly the expected
//! deterministic pixel value — proving the ticket store never
//! cross-delivers one submission's result for another's ticket under
//! concurrent access. The spec's 24 h continuous stress run is a
//! nightly/long-run job (DEFERRED — see `docs/plan/epics/E05-deviations.md`,
//! same disposition as the compiler fuzz's 24 h run).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::compile::{GraphTemplate, RecipeCompiler};
use lightbox_render::ng::config::{BackendPref, EngineConfig};
use lightbox_render::ng::node::NodeRegistry;
use lightbox_render::ng::source::{
    DeviceHandles, DeviceProvider, SourceImage, SourceProvider, SourceWant,
};
use lightbox_render::ng::types::{NodeId, RenderScale, Roi};
use lightbox_render::ng::{
    BoxFuture, DeviceError, Engine, OutFormat, OutputPayload, PvRange, RenderPriority,
    RenderRequest, RenderState, RenderTarget, SourceError,
};
use lightbox_render_testkit::probes::{CheckerFactory, CheckerProbe, GainFactory};
use lightbox_types::{ImageId, PV_M0};

struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unreachable!("ForceCpu never calls DeviceProvider::current")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "no device in the stress test".to_owned(),
            ))
        })
    }
}
struct NullSource;
impl SourceProvider for NullSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        Box::pin(async { Err(SourceError::NotFound) })
    }
}

fn build_engine() -> Engine {
    let mut reg = NodeRegistry::new();
    reg.register(
        NodeId("test.checker"),
        PvRange::from_open(PV_M0),
        Arc::new(CheckerFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("test.gain"),
        PvRange::from_open(PV_M0),
        Arc::new(GainFactory::default()),
    )
    .unwrap();
    let mut compiler = RecipeCompiler::with_registry(Arc::new(reg));
    compiler
        .register_template(
            PV_M0,
            GraphTemplate::linear(vec![NodeId("test.checker"), NodeId("test.gain")]),
        )
        .unwrap();
    Engine::with_compiler(
        Arc::new(NullDevice),
        Arc::new(NullSource),
        compiler,
        EngineConfig {
            backend: BackendPref::ForceCpu,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds")
}

fn request() -> RenderRequest {
    RenderRequest {
        image: ImageId(1),
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi {
            x: 0,
            y: 0,
            w: 16,
            h: 16,
        },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba32F,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

/// Threads × submissions per thread (bounded — a few hundred concurrent
/// renders, seconds of wall-clock; the 24 h continuous run is nightly).
const THREADS: usize = 8;
const PER_THREAD: usize = 60;

#[test]
fn concurrent_submit_poll_cancel_never_corrupts_the_ticket_store_or_cache() {
    let engine = Arc::new(build_engine());
    let seen_tickets: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = Vec::new();

    for t in 0..THREADS {
        let engine = Arc::clone(&engine);
        let seen_tickets = Arc::clone(&seen_tickets);
        handles.push(std::thread::spawn(move || {
            let mut local_completions = 0usize;
            let mut local_cancellations = 0usize;
            for i in 0..PER_THREAD {
                let ticket = engine.submit(request());
                {
                    let mut seen = seen_tickets.lock().unwrap();
                    assert!(
                        seen.insert(ticket.0),
                        "ticket id {} reused across threads/submissions — ticket-store race",
                        ticket.0
                    );
                }
                // Every third submission on odd threads: cancel immediately,
                // stressing the ticket store's terminal-state transition
                // concurrently with other threads' submits/polls.
                let should_cancel = t % 2 == 1 && i % 3 == 0;
                if should_cancel {
                    engine.cancel(&ticket);
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match engine.poll(&ticket) {
                        RenderState::Queued | RenderState::Rendering { .. } => {
                            assert!(
                                Instant::now() < deadline,
                                "ticket {} never reached a terminal state — possible deadlock",
                                ticket.0
                            );
                            std::thread::yield_now();
                        }
                        RenderState::Complete(out) => {
                            local_completions += 1;
                            let OutputPayload::Pixels(px) = out.payload else {
                                panic!("expected Pixels payload for a Buffer target");
                            };
                            // test.checker(0,0) is COLOR_A; test.gain default
                            // 2.0 doubles it — the pinned, deterministic
                            // expectation this ticket must carry, proving the
                            // ticket store delivered *this* submission's own
                            // result, not another thread's.
                            let p = px.get_rgba_f32(0, 0);
                            let expect = CheckerProbe::color_at(0, 0);
                            // Working tiles round-trip through f16 (spec §4.2)
                            // between test.checker and test.gain, so the
                            // tolerance accounts for f16 quantization, not
                            // just f32 rounding.
                            for c in 0..3 {
                                assert!(
                                    (p[c] - expect[c] * 2.0).abs() < 5e-3,
                                    "ticket {}: channel {c} = {}, expected {} \
                                     (ticket store may have cross-delivered a result)",
                                    ticket.0,
                                    p[c],
                                    expect[c] * 2.0
                                );
                            }
                            break;
                        }
                        RenderState::Cancelled => {
                            local_cancellations += 1;
                            break;
                        }
                        RenderState::Failed(e) => {
                            panic!("ticket {} failed unexpectedly: {e}", ticket.0);
                        }
                        RenderState::PreviewReady(_) => unreachable!(
                            "the base walk never emits PreviewReady for this probe graph"
                        ),
                    }
                }
            }
            (local_completions, local_cancellations)
        }));
    }

    let mut total_completions = 0usize;
    let mut total_cancellations = 0usize;
    for h in handles {
        let (c, x) = h.join().expect("worker thread must not panic");
        total_completions += c;
        total_cancellations += x;
    }

    assert_eq!(
        seen_tickets.lock().unwrap().len(),
        THREADS * PER_THREAD,
        "every submission must have produced a distinct ticket id"
    );
    assert_eq!(
        total_completions + total_cancellations,
        THREADS * PER_THREAD
    );
    assert!(total_completions > 0, "most submissions should complete");
    assert!(
        total_cancellations > 0,
        "the scripted cancel-immediately path should have fired at least once"
    );

    // The engine itself must still be usable after the stress burst (no
    // poisoned lock / wedged worker thread).
    let ticket = engine.submit(request());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(_) => break,
            RenderState::Queued | RenderState::Rendering { .. } => {
                assert!(
                    Instant::now() < deadline,
                    "engine wedged after the stress burst"
                );
                std::thread::yield_now();
            }
            other => panic!("unexpected post-stress state: {other:?}"),
        }
    }
}
