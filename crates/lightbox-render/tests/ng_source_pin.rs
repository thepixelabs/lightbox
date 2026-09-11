// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The engine's decoded-source pin is a cache, not a leak.
//!
//! `Engine`'s `source_pin` keeps decoded sources in host memory so a render
//! after GPU cache eviction or device loss re-uploads without calling
//! `SourceProvider::fetch` again. It used to be a `HashMap` that was inserted
//! into and never removed from.
//!
//! That was survivable only while every entry was a camera's embedded JPEG
//! preview, around 12 MB for a 2176x1448 frame. Wiring real sensor data into
//! the editor makes each entry a demosaiced frame at `Rgba16F`: the same file
//! becomes roughly 99 MB, and a 45 megapixel body lands near 360 MB. Browsing
//! ten raw files would have held gigabytes that nothing could release.
//!
//! These tests pin the policy, not the numbers: past its budget the least
//! recently used source is dropped, the live one never is, and a single source
//! larger than the entire budget still renders instead of evicting itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent, OutFormat,
    PixelBuf, PixelFormat, RenderPriority, RenderRequest, RenderScale, RenderState, RenderTarget,
    Roi, SourceColorimetry, SourceError, SourceImage, SourceProvider, SourceQuality, SourceWant,
};
use lightbox_types::ImageId;

/// A device provider that is never asked for a device: every test here runs
/// `BackendPref::ForceCpu`, so the CPU path serves each render.
struct NoDevice;
impl DeviceProvider for NoDevice {
    fn current(&self) -> DeviceHandles {
        unreachable!("ForceCpu never asks for a device");
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async { Err(DeviceError::Rebuild("no device in this test".into())) })
    }
}

/// Hands back a fixed-size source per image and counts how often it is asked.
/// The count is the whole point: a pinned source costs zero fetches, an evicted
/// one costs exactly one more.
struct CountingSource {
    bytes_per_image: usize,
    fetches: Arc<AtomicUsize>,
}

impl SourceProvider for CountingSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        // Rgba8 is 4 bytes per pixel, so a square of this edge is the size asked
        // for, near enough for a budget test.
        let px = (self.bytes_per_image / 4) as f64;
        let edge = px.sqrt() as u32;
        let pixels = PixelBuf::new_zeroed(PixelFormat::Rgba8Srgb, Extent { w: edge, h: edge });
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry: SourceColorimetry::default(),
                full_extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

fn engine_with(bytes_per_image: usize, budget: usize) -> (Engine, Arc<AtomicUsize>) {
    let fetches = Arc::new(AtomicUsize::new(0));
    let engine = Engine::with_compiler(
        Arc::new(NoDevice),
        Arc::new(CountingSource {
            bytes_per_image,
            fetches: Arc::clone(&fetches),
        }),
        lightbox_render::ng::shipping_compiler(),
        EngineConfig {
            backend: BackendPref::ForceCpu,
            source_pin_budget_bytes: budget,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds");
    (engine, fetches)
}

fn render(engine: &Engine, image: ImageId) {
    let recipe = Recipe::identity(lightbox_types::PV_M0);
    let req = RenderRequest {
        image,
        pv: recipe.pv,
        recipe,
        roi: Roi {
            x: 0,
            y: 0,
            w: 32,
            h: 32,
        },
        scale: RenderScale::Fit(Extent { w: 32, h: 32 }),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Batch,
        cancel: CancelToken::new(),
    };
    let ticket = engine.submit(req);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match engine.poll(&ticket) {
            RenderState::Complete(_) => return,
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "render did not complete within 20s"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn the_source_pin_evicts_least_recently_used_past_its_budget() {
    const PER_IMAGE: usize = 4 * 1024 * 1024;
    // Room for two of these, not three.
    let (engine, fetches) = engine_with(PER_IMAGE, 10 * 1024 * 1024);

    render(&engine, ImageId(1));
    assert_eq!(engine.source_pin_bytes(), PER_IMAGE, "one pinned");
    render(&engine, ImageId(2));
    assert_eq!(engine.source_pin_bytes(), 2 * PER_IMAGE, "two pinned");
    render(&engine, ImageId(3));

    // The third push takes the total to 12 MB against a 10 MB budget, so the
    // least recently used entry goes. Asserting the exact total rather than
    // just "within budget" is what proves an eviction happened at all: a pin
    // that simply never inserted the third image would also be within budget.
    assert_eq!(
        engine.source_pin_bytes(),
        2 * PER_IMAGE,
        "past the budget the pin must hold exactly two sources, not three"
    );
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        3,
        "each of the three images was fetched exactly once"
    );

    // Image 1 was the least recently used, so it is the one that went. Image 3
    // is the most recent and must still be there. Release is the cheapest way
    // to ask what is resident without widening the public surface further.
    assert!(
        !engine.release_source_pin(ImageId(1)),
        "image 1 was the least recently used and must have been evicted"
    );
    assert!(
        engine.release_source_pin(ImageId(3)),
        "the most recently used source must still be pinned"
    );
    assert!(
        engine.release_source_pin(ImageId(2)),
        "image 2 was within budget and must still be pinned"
    );
    assert_eq!(engine.source_pin_bytes(), 0, "everything released");
}

#[test]
fn a_source_larger_than_the_whole_budget_still_renders() {
    // One image is four times the entire budget. Evicting it would mean
    // re-fetching on every single render, which is worse than being over.
    let (engine, fetches) = engine_with(4 * 1024 * 1024, 1024 * 1024);

    render(&engine, ImageId(1));
    let after_first = fetches.load(Ordering::SeqCst);
    render(&engine, ImageId(1));

    assert_eq!(
        fetches.load(Ordering::SeqCst),
        after_first,
        "the only pinned source must not evict itself"
    );
}

#[test]
fn releasing_a_pin_reclaims_its_bytes() {
    let (engine, _) = engine_with(4 * 1024 * 1024, 64 * 1024 * 1024);
    render(&engine, ImageId(1));
    assert!(engine.source_pin_bytes() > 0, "something must be pinned");

    assert!(engine.release_source_pin(ImageId(1)), "a pin was dropped");
    assert_eq!(
        engine.source_pin_bytes(),
        0,
        "releasing the only pin must reclaim all of its bytes"
    );
    assert!(
        !engine.release_source_pin(ImageId(1)),
        "releasing twice reports nothing was dropped"
    );
}
