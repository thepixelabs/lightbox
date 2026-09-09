// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase F5, M1 integration: `RenderScheduler::enable_canvas` +
//! `RenderTarget::Canvas` publish through the real Metal device, end to end
//! through the PV1 engine-owned node graph (`src.decoded → util.resize →
//! xform.display`) exactly as `lightbox-core`'s session façade drives it.
//!
//! If no adapter is available (a momentarily-contended worktree), the test
//! **reports and skips** rather than faking a GPU result, the authoritative
//! run is the single-process verify pass on `main`.

use std::sync::Arc;
use std::time::Duration;

use lightbox_edit::Recipe;
use lightbox_jobs::{CancelToken, JobConfig, JobSystem};
use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
use lightbox_render::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
use lightbox_render::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent, JobsHandle,
    NodeRegistry, OutputQuality, PixelBuf, PixelFormat, PvRange, RenderScheduler, Roi,
    SourceColorimetry, SourceError, SourceImage, SourceProvider, SourceQuality, SourceWant,
    ViewState, Zoom,
};
use lightbox_types::{ImageId, PV_M0};

struct SharedDevice {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}
impl DeviceProvider for SharedDevice {
    fn current(&self) -> DeviceHandles {
        (Arc::clone(&self.device), Arc::clone(&self.queue))
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        let h = (Arc::clone(&self.device), Arc::clone(&self.queue));
        Box::pin(async move { Ok(h) })
    }
}

/// A fixed-pattern decoded source (E02 seam), a small RGBA8 gradient.
struct SynthSource {
    pixels: PixelBuf,
}
impl SourceProvider for SynthSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let pixels = self.pixels.clone();
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry: SourceColorimetry::default(),
                full_extent,
                quality: SourceQuality::Preview,
            })
        })
    }
}

fn gradient(w: u32, h: u32) -> PixelBuf {
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Srgb, Extent { w, h });
    for y in 0..h {
        for x in 0..w {
            let r = x as f32 / w.max(1) as f32;
            let g = y as f32 / h.max(1) as f32;
            px.set_rgba_f32(x, y, [r, g, 0.4, 1.0]);
        }
    }
    px
}

/// Builds a real PV1-shaped registry (the same three engine-owned nodes
/// `lightbox-core` registers) driving a GPU-backed `Engine`.
fn build_engine(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Engine {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .expect("register src.decoded");
    reg.register(
        UtilResizeNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .expect("register util.resize");
    reg.register(
        XformDisplayNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .expect("register xform.display");

    let dp: Arc<dyn DeviceProvider> = Arc::new(SharedDevice { device, queue });
    let sp: Arc<dyn SourceProvider> = Arc::new(SynthSource {
        pixels: gradient(64, 48),
    });
    Engine::new(
        dp,
        sp,
        reg,
        EngineConfig {
            backend: BackendPref::Auto,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds")
}

/// `enable_canvas` installs a publisher on the engine; a `set_recipe`/
/// `set_view` pair through the PV1 graph publishes a real generation whose
/// texture is readable back and whose quality is badge-able (F5: the shell
/// composites through exactly this path).
#[test]
fn f5_scheduler_enable_canvas_publishes_pv1_frames_on_real_device() {
    let Some(ctx) = lightbox_render::GpuContext::headless() else {
        eprintln!(
            "[f5_scheduler_enable_canvas] no wgpu adapter available — SKIPPED \
             (authoritative run is on main)"
        );
        return;
    };
    let engine = Arc::new(build_engine(ctx.device.clone(), ctx.queue.clone()));
    let jobs = JobsHandle(Arc::new(JobSystem::new(JobConfig::default())));
    let scheduler = RenderScheduler::new(engine, jobs);

    // Before `enable_canvas`, the scheduler has no canvas, `canvas()` is
    // `None` rather than a fabricated placeholder (the F5 deviation from the
    // spec's unconditional signature).
    assert!(
        scheduler.canvas().is_none(),
        "canvas() must be None before enable_canvas"
    );

    scheduler.enable_canvas(
        ctx.device.clone(),
        ctx.queue.clone(),
        Extent { w: 64, h: 48 },
    );
    let mut rx = scheduler.canvas().expect("canvas() is Some once enabled");
    let initial_gen = rx.borrow_and_update().generation;
    assert_eq!(initial_gen, 0, "no frame published yet");

    let image = ImageId(7);
    scheduler.set_view(
        image,
        ViewState {
            viewport: Extent { w: 64, h: 48 },
            zoom: Zoom(1.0),
            pan: Roi {
                x: 0,
                y: 0,
                w: 64,
                h: 48,
            },
        },
    );
    scheduler.set_recipe(image, Recipe::identity(PV_M0), PV_M0);

    assert!(
        scheduler.wait_idle(Duration::from_secs(10)),
        "scheduler reached quiescence"
    );

    // The watch channel observed at least one new generation (the render
    // dispatched a Canvas target once enabled).
    let published = {
        let frame = rx.borrow_and_update();
        frame.generation
    };
    assert!(
        published > initial_gen,
        "expected a published generation > {initial_gen}, got {published}"
    );

    let out = scheduler
        .last_output(image)
        .expect("a completed render is recorded");
    assert_eq!(out.quality, OutputQuality::FullRes);
    assert!(
        matches!(
            out.payload,
            lightbox_render::ng::OutputPayload::CanvasGeneration(_)
        ),
        "F5: scheduler dispatches Canvas targets once enabled, got {:?}",
        out.payload
    );
    assert!(scheduler.last_error(image).is_none());

    // Re-render (a param-identical recipe still re-derives, M1's Recipe
    // carries no develop params yet) publishes a strictly newer generation
    // the shell's "new frame arrived" signal is monotonic.
    scheduler.set_recipe(image, Recipe::identity(PV_M0), PV_M0);
    assert!(scheduler.wait_idle(Duration::from_secs(10)));
    let second = rx.borrow_and_update().generation;
    assert!(
        second > published,
        "a second render must publish a strictly newer generation"
    );
}

/// A scheduler that never calls `enable_canvas` (the CLI/headless/CPU-only
/// path) keeps dispatching `Buffer` targets, `last_output` observes `Pixels`
/// payloads and `canvas()` stays `None` throughout.
#[test]
fn f5_scheduler_without_canvas_stays_on_buffer_targets() {
    let mut reg = NodeRegistry::new();
    reg.register(
        SrcDecodedNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(SrcDecodedFactory::default()),
    )
    .unwrap();
    reg.register(
        UtilResizeNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(UtilResizeFactory::default()),
    )
    .unwrap();
    reg.register(
        XformDisplayNode::ID,
        PvRange::from_open(PV_M0),
        Arc::new(XformDisplayFactory::default()),
    )
    .unwrap();

    struct NullDevice;
    impl DeviceProvider for NullDevice {
        fn current(&self) -> DeviceHandles {
            unimplemented!("CPU-only engine never calls current()")
        }
        fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
            Box::pin(async { Err(DeviceError::Rebuild("no device in test".to_owned())) })
        }
    }

    let dp: Arc<dyn DeviceProvider> = Arc::new(NullDevice);
    let sp: Arc<dyn SourceProvider> = Arc::new(SynthSource {
        pixels: gradient(8, 8),
    });
    let engine = Arc::new(
        Engine::new(
            dp,
            sp,
            reg,
            EngineConfig {
                backend: BackendPref::ForceCpu,
                ..EngineConfig::default()
            },
        )
        .unwrap(),
    );
    let jobs = JobsHandle(Arc::new(JobSystem::new(JobConfig::default())));
    let scheduler = RenderScheduler::new(engine, jobs);

    let image = ImageId(9);
    scheduler.set_view(
        image,
        ViewState {
            viewport: Extent { w: 8, h: 8 },
            zoom: Zoom(1.0),
            pan: Roi {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
        },
    );
    scheduler.set_recipe(image, Recipe::identity(PV_M0), PV_M0);
    assert!(scheduler.wait_idle(Duration::from_secs(5)));

    assert!(scheduler.canvas().is_none());
    let out = scheduler.last_output(image).expect("render completed");
    assert!(
        matches!(out.payload, lightbox_render::ng::OutputPayload::Pixels(_)),
        "headless scheduler must keep dispatching Buffer targets"
    );
}
