// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E05 Phase E — device-lost recovery + CPU fallback integration tests
//! (E05.5; tasks E1–E8), on the real Metal adapter of this build box.
//!
//! These drive the engine end-to-end through the frozen `DeviceProvider` seam
//! with a **rebuildable** test device that genuinely creates a fresh wgpu device
//! on `rebuild()`, and a test-only fault injector (`Engine::inject_device_lost`)
//! that simulates the wgpu `device_lost` callback. Every test acquires a
//! headless adapter; if none is available it **reports and skips** rather than
//! faking a result — the authoritative run is the single-process merge on `main`.
//!
//! Covered: E1 detection (event + in-flight tickets resolve `Failed(DeviceLost)`,
//! never hang), E2 rebuild on a fresh device, E3 re-warm (RAM-pinned source
//! re-uploads with zero `SourceProvider::fetch`), E4 degrade policy +
//! `active_backend` + explicit re-enable, **E5 the §10.1 gate** (injected loss ⇒
//! keeps editing at preview resolution), E7 degraded contract (Interactive
//! full-res clamped + badged; Batch full-res allowed), E8 export-safety
//! (submissions refused typed on a lost device).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightbox_jobs::CancelToken;
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::XformDisplayFactory;
use lightbox_render::ng::nodes::resize::UtilResizeFactory;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    ActiveBackend, BackendId, BackendPref, DegradePolicy, DeviceError, DeviceProvider, Engine,
    EngineConfig, EngineEvent, NodeRegistry, OutFormat, OutputPayload, OutputQuality, PixelBuf,
    PixelFormat, PvRange, RenderPriority, RenderRequest, RenderScale, RenderState, RenderTarget,
    RenderTicket, SourceColorimetry, SourceError, SourceImage, SourceProvider, SourceQuality,
    SourceWant,
};
use lightbox_render::ng::{BoxFuture, Extent, NodeId, Roi};
use lightbox_render::GpuContext;
use lightbox_types::{ImageId, PV_M0};

// ── a rebuildable test device (owns instance+adapter; makes fresh devices) ────

/// A `DeviceProvider` that owns a real adapter and can hand out **fresh** devices
/// on `rebuild()` — the honest E2 substrate (the next submit renders on a device
/// that did not exist when the lost one did, so any stale-handle reuse would trip
/// a wgpu validation error). `rebuild()` can be told to fail (E8).
struct RebuildableDevice {
    adapter: wgpu::Adapter,
    info: wgpu::AdapterInfo,
    current: Mutex<DeviceHandles>,
    rebuild_fail: AtomicBool,
    rebuild_count: AtomicUsize,
}

impl RebuildableDevice {
    fn new() -> Option<Arc<RebuildableDevice>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let info = adapter.get_info();
        let handles = Self::make_device(&adapter)?;
        Some(Arc::new(RebuildableDevice {
            adapter,
            info,
            current: Mutex::new(handles),
            rebuild_fail: AtomicBool::new(false),
            rebuild_count: AtomicUsize::new(0),
        }))
    }

    fn make_device(adapter: &wgpu::Adapter) -> Option<DeviceHandles> {
        let desc = GpuContext::device_descriptor(adapter);
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).ok()?;
        Some((Arc::new(device), Arc::new(queue)))
    }

    fn set_rebuild_fail(&self, fail: bool) {
        self.rebuild_fail.store(fail, Ordering::SeqCst);
    }

    fn rebuilds(&self) -> usize {
        self.rebuild_count.load(Ordering::SeqCst)
    }
}

impl DeviceProvider for RebuildableDevice {
    fn current(&self) -> DeviceHandles {
        self.current.lock().unwrap().clone()
    }

    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        self.rebuild_count.fetch_add(1, Ordering::SeqCst);
        if self.rebuild_fail.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(DeviceError::Rebuild("injected rebuild failure".to_owned()))
            });
        }
        match Self::make_device(&self.adapter) {
            Some(handles) => {
                *self.current.lock().unwrap() = handles.clone();
                Box::pin(async move { Ok(handles) })
            }
            None => Box::pin(async {
                Err(DeviceError::Rebuild(
                    "adapter refused a new device".to_owned(),
                ))
            }),
        }
    }

    fn adapter_info(&self) -> Option<wgpu::AdapterInfo> {
        Some(self.info.clone())
    }
}

// ── a counting / optionally-blocking synthetic source (E02 seam) ──────────────

struct SynthSource {
    pixels: PixelBuf,
    fetches: AtomicUsize,
    /// When set, `fetch` spins until the cancel token fires — used to hold a
    /// render "in flight" for the E1 fault-injection window.
    block: AtomicBool,
}

impl SynthSource {
    fn new(pixels: PixelBuf) -> Arc<SynthSource> {
        Arc::new(SynthSource {
            pixels,
            fetches: AtomicUsize::new(0),
            block: AtomicBool::new(false),
        })
    }
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
    fn set_block(&self, block: bool) {
        self.block.store(block, Ordering::SeqCst);
    }
}

impl SourceProvider for SynthSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let pixels = self.pixels.clone();
        let block = self.block.load(Ordering::SeqCst);
        let cancel = cancel.clone();
        Box::pin(async move {
            if block {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancel.is_cancelled() {
                    if Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                return Err(SourceError::Cancelled);
            }
            Ok(as_source_image(pixels))
        })
    }
}

fn as_source_image(pixels: PixelBuf) -> SourceImage {
    let full_extent = pixels.extent;
    SourceImage {
        pixels,
        colorimetry: SourceColorimetry::default(),
        full_extent,
        quality: SourceQuality::Full,
    }
}

fn u8_gradient(w: u32, h: u32) -> PixelBuf {
    let mut bytes = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            bytes[i] = (x * 255 / w.max(1)) as u8;
            bytes[i + 1] = (y * 255 / h.max(1)) as u8;
            bytes[i + 2] = 96;
            bytes[i + 3] = 255;
        }
    }
    PixelBuf {
        bytes,
        format: PixelFormat::Rgba8Unorm,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

// ── engine builder over the PV1 engine-owned chain ────────────────────────────

fn engine_with_policy(
    dp: Arc<dyn DeviceProvider>,
    sp: Arc<dyn SourceProvider>,
    policy: DegradePolicy,
) -> Engine {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("util.resize"),
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .unwrap();
    reg.register(
        NodeId("xform.display"),
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .unwrap();
    // Engine::new registers the built-in PV1 template
    // (src.decoded → util.resize → xform.display).
    Engine::new(
        dp,
        sp,
        reg,
        EngineConfig {
            backend: BackendPref::Auto,
            device_lost_degrade: policy,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds")
}

fn request(w: u32, h: u32, scale: RenderScale, priority: RenderPriority) -> RenderRequest {
    RenderRequest {
        image: ImageId(1),
        recipe: lightbox_edit::Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi { x: 0, y: 0, w, h },
        scale,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority,
        cancel: CancelToken::new(),
    }
}

fn poll_terminal(engine: &Engine, ticket: &RenderTicket) -> RenderState {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = engine.poll(ticket);
        if matches!(
            state,
            RenderState::Complete(_) | RenderState::Failed(_) | RenderState::Cancelled
        ) {
            return state;
        }
        if Instant::now() > deadline {
            panic!("render did not reach a terminal state within 15s: {state:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn render(engine: &Engine, req: RenderRequest) -> lightbox_render::ng::RenderOutput {
    let ticket = engine.submit(req);
    match poll_terminal(engine, &ticket) {
        RenderState::Complete(out) => out,
        other => panic!("expected Complete, got {other:?}"),
    }
}

fn dev_or_skip(name: &str) -> Option<Arc<RebuildableDevice>> {
    match RebuildableDevice::new() {
        Some(d) => Some(d),
        None => {
            eprintln!(
                "[{name}] no wgpu adapter available — SKIPPED (authoritative run is on main)"
            );
            None
        }
    }
}

// ── E1: detection — event + in-flight tickets resolve, never hang ─────────────

#[test]
fn e1_injected_loss_emits_event_and_fails_inflight_not_hang() {
    let Some(dp) = dev_or_skip("e1") else { return };
    let src = SynthSource::new(u8_gradient(32, 32));
    // max_losses high so this loss does NOT degrade — it stays a GPU-lost window.
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 5,
            window: Duration::from_secs(60),
        },
    );
    let mut events = engine.events();

    // Hold a render in flight by blocking the source fetch until cancellation.
    src.set_block(true);
    let ticket = engine.submit(request(
        32,
        32,
        RenderScale::OneToOne,
        RenderPriority::Interactive,
    ));
    // Wait until the worker is actually rendering (blocked in the fetch).
    let start = Instant::now();
    while !matches!(engine.poll(&ticket), RenderState::Rendering { .. }) {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("render never reached Rendering");
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    engine.inject_device_lost("test: forced device drop");

    // The event fires.
    let evt = events.blocking_recv().expect("device-lost event");
    assert!(
        matches!(evt, EngineEvent::DeviceLost { .. }),
        "expected DeviceLost, got {evt:?}"
    );

    // The in-flight ticket resolves Failed(DeviceLost) — never hangs.
    let state = poll_terminal(&engine, &ticket);
    assert!(
        matches!(
            state,
            RenderState::Failed(lightbox_render::ng::RenderError::DeviceLost(_))
        ),
        "in-flight ticket must resolve Failed(DeviceLost), got {state:?}"
    );
}

// ── E2 + E3 + E5(gpu): single loss rebuilds on a fresh device, re-warms ───────

#[test]
fn e2_e3_single_loss_rebuilds_on_fresh_device_without_refetch() {
    let Some(dp) = dev_or_skip("e2e3") else {
        return;
    };
    let src = SynthSource::new(u8_gradient(48, 32));
    // max_losses = 2 so a single loss stays on GPU and rebuilds (no degrade).
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 2,
            window: Duration::from_secs(60),
        },
    );

    // First render on the GPU: fetches + RAM-pins the source.
    let out1 = render(
        &engine,
        request(48, 32, RenderScale::OneToOne, RenderPriority::Interactive),
    );
    assert_eq!(out1.backend, BackendId::Gpu);
    assert_eq!(src.fetches(), 1, "first render fetches once");

    // Lose the device (stays GPU), then render again → rebuild on a fresh device.
    engine.inject_device_lost("test: forced device drop");
    let out2 = render(
        &engine,
        request(48, 32, RenderScale::OneToOne, RenderPriority::Interactive),
    );

    // E2: renders successfully on the rebuilt device (a brand-new wgpu device;
    // any stale handle would have tripped a validation error), still on GPU.
    assert_eq!(
        out2.backend,
        BackendId::Gpu,
        "keeps editing on the rebuilt GPU"
    );
    assert!(
        dp.rebuilds() >= 1,
        "the DeviceProvider was asked to rebuild"
    );
    // E3: the RAM-pinned source re-uploaded onto the new device — zero re-fetch.
    assert_eq!(
        src.fetches(),
        1,
        "re-warm must re-upload the pinned source with no SourceProvider::fetch"
    );
    // Output is a real frame.
    let OutputPayload::Pixels(px) = out2.payload else {
        panic!("expected pixels");
    };
    assert_eq!(px.extent, Extent { w: 48, h: 32 });
}

// ── E5: the §10.1 gate — injected loss ⇒ keeps editing at preview resolution ──

#[test]
fn e5_gate_injected_loss_keeps_editing_at_preview_resolution() {
    let Some(dp) = dev_or_skip("e5") else { return };
    let src = SynthSource::new(u8_gradient(32, 32));
    // max_losses = 1: the first loss degrades to CPU preview immediately.
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 1,
            window: Duration::from_secs(60),
        },
    );
    let mut events = engine.events();

    // Establish a session on the GPU.
    let first = render(
        &engine,
        request(32, 32, RenderScale::OneToOne, RenderPriority::Interactive),
    );
    assert_eq!(first.backend, BackendId::Gpu);
    assert_eq!(first.quality, OutputQuality::FullRes);

    // Injected device-lost mid-session ⇒ degrade to CPU preview.
    engine.inject_device_lost("test: forced device drop");
    // Both DeviceLost and DegradedToCpu are broadcast; drain until we see degrade.
    let mut saw_degrade = false;
    for _ in 0..4 {
        match events.try_recv() {
            Ok(EngineEvent::DegradedToCpu) => {
                saw_degrade = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(saw_degrade, "expected a DegradedToCpu event");
    assert!(matches!(
        engine.active_backend(),
        ActiveBackend::CpuPreviewOnly
    ));

    // The session KEEPS EDITING: successive interactive submits keep producing
    // frames, now at preview resolution on the CPU (the degraded-but-usable
    // contract). "Param change" is modelled as repeated interactive submits;
    // true param-driven pixels await E09, but the render loop stays responsive.
    for _ in 0..3 {
        let out = render(
            &engine,
            request(32, 32, RenderScale::OneToOne, RenderPriority::Interactive),
        );
        assert_eq!(out.backend, BackendId::Cpu, "editing continues on CPU");
        assert_eq!(
            out.quality,
            OutputQuality::PreviewRes,
            "degraded interactive frames are preview resolution"
        );
        let OutputPayload::Pixels(px) = out.payload else {
            panic!("expected pixels");
        };
        // Full-res was 32×32; the degraded interactive frame is clamped smaller.
        assert!(
            px.extent.w < 32 && px.extent.h < 32,
            "preview frame must be sub-full-res, got {:?}",
            px.extent
        );
    }
    // No re-fetch across the degrade: the RAM pin served every frame.
    assert_eq!(src.fetches(), 1, "re-warm across degrade: one fetch total");
}

// ── E4: degrade policy + active_backend + explicit re-enable ──────────────────

#[test]
fn e4_active_backend_and_reenable_gpu() {
    let Some(dp) = dev_or_skip("e4") else { return };
    let src = SynthSource::new(u8_gradient(16, 16));
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 1,
            window: Duration::from_secs(60),
        },
    );

    // Starts on the GPU: active_backend reports the real adapter.
    match engine.active_backend() {
        ActiveBackend::Gpu(info) => assert_eq!(info.backend, dp.info.backend),
        other => panic!("expected Gpu(adapter), got {other:?}"),
    }

    // Degrade → CpuPreviewOnly.
    engine.inject_device_lost("test: forced device drop");
    assert!(matches!(
        engine.active_backend(),
        ActiveBackend::CpuPreviewOnly
    ));

    // Explicit re-enable restores the GPU path (rebuilds the device).
    engine.reenable_gpu().expect("re-enable rebuilds the GPU");
    match engine.active_backend() {
        ActiveBackend::Gpu(_) => {}
        other => panic!("expected Gpu after re-enable, got {other:?}"),
    }
    // And a render lands back on the GPU.
    let out = render(
        &engine,
        request(16, 16, RenderScale::OneToOne, RenderPriority::Interactive),
    );
    assert_eq!(out.backend, BackendId::Gpu);
}

// ── E7: degraded contract — Interactive clamped/badged, Batch full-res ────────

#[test]
fn e7_degraded_contract_clamps_interactive_allows_batch() {
    let Some(dp) = dev_or_skip("e7") else { return };
    let src = SynthSource::new(u8_gradient(32, 32));
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 1,
            window: Duration::from_secs(60),
        },
    );
    // Degrade to CPU preview.
    engine.inject_device_lost("test: forced device drop");
    assert!(matches!(
        engine.active_backend(),
        ActiveBackend::CpuPreviewOnly
    ));

    // Interactive full-res ⇒ clamped to preview-res, badged PreviewRes, sub-full.
    let interactive = render(
        &engine,
        request(32, 32, RenderScale::OneToOne, RenderPriority::Interactive),
    );
    assert_eq!(interactive.backend, BackendId::Cpu);
    assert_eq!(interactive.quality, OutputQuality::PreviewRes);
    let OutputPayload::Pixels(ipx) = interactive.payload else {
        panic!("pixels");
    };
    assert!(
        ipx.extent.w < 32 && ipx.extent.h < 32,
        "no silent full-res on the interactive path: {:?}",
        ipx.extent
    );

    // Batch full-res ⇒ allowed at full resolution, badged FullRes.
    let batch = render(
        &engine,
        request(32, 32, RenderScale::OneToOne, RenderPriority::Batch),
    );
    assert_eq!(batch.backend, BackendId::Cpu);
    assert_eq!(batch.quality, OutputQuality::FullRes);
    let OutputPayload::Pixels(bpx) = batch.payload else {
        panic!("pixels");
    };
    assert_eq!(
        bpx.extent,
        Extent { w: 32, h: 32 },
        "full-res is permitted as a Batch render"
    );
}

// ── E8: export-safety — submissions refused typed on a lost device ────────────

#[test]
fn e8_export_refused_on_lost_device_then_retries_after_rebuild() {
    let Some(dp) = dev_or_skip("e8") else { return };
    let src = SynthSource::new(u8_gradient(24, 24));
    // max_losses = 2 so the single loss stays a GPU-lost window (no degrade).
    let engine = engine_with_policy(
        dp.clone(),
        src.clone(),
        DegradePolicy {
            max_losses: 2,
            window: Duration::from_secs(60),
        },
    );
    // Warm the session on the GPU.
    let _ = render(
        &engine,
        request(24, 24, RenderScale::OneToOne, RenderPriority::Batch),
    );

    // Make rebuild fail, then lose the device: the engine is stuck in the
    // lost-device window.
    dp.set_rebuild_fail(true);
    engine.inject_device_lost("test: forced device drop");

    // A Batch (export) submit during the lost window fails typed.
    let ticket = engine.submit(request(
        24,
        24,
        RenderScale::OneToOne,
        RenderPriority::Batch,
    ));
    let state = poll_terminal(&engine, &ticket);
    assert!(
        matches!(
            state,
            RenderState::Failed(lightbox_render::ng::RenderError::DeviceUnavailable(_))
        ),
        "export on a lost device must fail typed (DeviceUnavailable), got {state:?}"
    );

    // Once the device can rebuild again, E15's retry succeeds on the GPU.
    dp.set_rebuild_fail(false);
    let out = render(
        &engine,
        request(24, 24, RenderScale::OneToOne, RenderPriority::Batch),
    );
    assert_eq!(
        out.backend,
        BackendId::Gpu,
        "retry lands on the rebuilt GPU"
    );
    assert_eq!(out.quality, OutputQuality::FullRes);
}
