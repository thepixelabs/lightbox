// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Changing white balance does not re-fetch the source, and that is why the
//! Temp slider cannot move a raw file's pixels live yet.
//!
//! A raw file's white balance is baked into the camera-to-working matrix at
//! decode time (`lightbox_core`'s `raw_source` module docs), so moving the
//! Temp slider makes the cached source stale. The engine does not know that.
//! This measures it rather than asserting it from a reading of the code,
//! because the claim is the whole reason
//! `lightbox_core::render_source::source_wb_for` exists.
//!
//! **This test pins current, wrong behaviour on purpose.** When
//! `lightbox-render` folds the white balance into `Shared::source_key`
//! (`crates/lightbox-render/src/ng/engine.rs:498`) this test will fail, and
//! the fix is to invert it: assert the second render *does* fetch, and delete
//! `source_wb_for`. The failure is the notification.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use lightbox_edit::{Recipe, WhiteBalance};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    shipping_registry, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig,
    Extent, OutFormat, PixelBuf, PixelFormat, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, Roi, SourceColorimetry, SourceError, SourceImage, SourceProvider,
    SourceQuality, SourceWant,
};
use lightbox_types::{ImageId, PV_M0};

const IMAGE: ImageId = ImageId(1);
const SIDE: u32 = 8;

/// A source that hands back a flat mid-grey and counts how often it is asked.
struct CountingSource(AtomicUsize);

impl SourceProvider for CountingSource {
    fn fetch(
        &self,
        _image: ImageId,
        _want: SourceWant,
        _cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            let extent = Extent { w: SIDE, h: SIDE };
            let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, extent);
            for y in 0..SIDE {
                for x in 0..SIDE {
                    px.set_rgba_f32(x, y, [0.5, 0.5, 0.5, 1.0]);
                }
            }
            Ok(SourceImage {
                pixels: px,
                colorimetry: SourceColorimetry::WORKING_LINEAR,
                full_extent: extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

/// The CPU backend never consults the device seam, so neither method is
/// reachable; both refuse rather than fabricate a device.
struct NoDevice;

impl DeviceProvider for NoDevice {
    fn current(&self) -> (Arc<wgpu::Device>, Arc<wgpu::Queue>) {
        unreachable!("BackendPref::ForceCpu never asks for a device")
    }

    fn rebuild(
        &self,
    ) -> BoxFuture<'static, Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "CPU-only test engine has no device".to_owned(),
            ))
        })
    }
}

fn render(engine: &Engine, recipe: Recipe) {
    let ticket = engine.submit(RenderRequest {
        image: IMAGE,
        recipe,
        pv: PV_M0,
        roi: Roi {
            x: 0,
            y: 0,
            w: SIDE,
            h: SIDE,
        },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "render never finished"
        );
        match engine.poll(&ticket) {
            RenderState::Complete(_) => return,
            RenderState::Failed(e) => panic!("render failed: {e:?}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
}

fn custom(temp_k: f32) -> Recipe {
    let mut r = Recipe::identity(PV_M0);
    r.global.white_balance = WhiteBalance::Custom { temp_k, tint: 0.0 };
    r
}

/// Two renders of the same image with different white balance, and only one
/// `SourceProvider::fetch`. `Engine::release_source_pin` in between does not
/// change that, because it drops the host RAM pin while the pinned uploaded
/// tile in the node cache, which `Shared::process` consults first
/// (`crates/lightbox-render/src/ng/engine.rs:416`), survives.
#[test]
fn a_white_balance_change_does_not_re_fetch_the_source_yet() {
    let source = Arc::new(CountingSource(AtomicUsize::new(0)));
    let engine = Engine::new(
        Arc::new(NoDevice),
        Arc::clone(&source) as Arc<dyn SourceProvider>,
        shipping_registry(),
        EngineConfig {
            backend: BackendPref::ForceCpu,
            ..EngineConfig::default()
        },
    )
    .expect("CPU engine");

    render(&engine, Recipe::identity(PV_M0));
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        1,
        "the first render must fetch exactly once"
    );

    render(&engine, custom(3000.0));
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        1,
        "if this is now 2, lightbox-render started keying the source stage on \
         white balance: invert this test and delete lightbox_core::render_source::source_wb_for"
    );

    assert!(
        engine.release_source_pin(IMAGE),
        "there was a pinned source to release"
    );
    render(&engine, custom(9000.0));
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        1,
        "release_source_pin drops the RAM pin but not the node cache's pinned \
         source tile, so it cannot force a re-decode on its own"
    );
}
