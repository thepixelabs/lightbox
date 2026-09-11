// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The raw provider turns a file on disk into working-space linear pixels.
//!
//! This is the narrow proof that the four steps in `raw_source`'s module doc
//! actually happen: sensor resolution rather than the embedded preview's,
//! upright, and finite. It does not go through the engine; that is the router's
//! test.

use std::path::PathBuf;

use lightbox_core::RawSourceProvider;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::PixelFormat;
use lightbox_types::Orientation;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn a_raw_decodes_to_upright_working_linear_at_sensor_resolution() {
    let Some(provider) = RawSourceProvider::autodetect() else {
        eprintln!(
            "SKIPPED a_raw_decodes_to_upright_working_linear_at_sensor_resolution: no LibRaw \
             proxy. Build it with `cargo build --release -p lightbox-rawproxy --features libraw` \
             or set LIGHTBOX_RAWPROXY_BIN. Set LIGHTBOX_TEST_LIBRAW=1 to make this a hard failure."
        );
        assert!(
            std::env::var("LIGHTBOX_TEST_LIBRAW").is_err(),
            "LIGHTBOX_TEST_LIBRAW=1 was set but no usable LibRaw proxy was found"
        );
        return;
    };

    let path = fixtures().join("fujifilm-x100.raf");
    if !path.exists() {
        eprintln!("SKIPPED: fixtures not fetched, run `cargo xtask fixtures`");
        return;
    }

    let img = provider
        .decode_for_test(&path, Orientation::O1, &CancelToken::new())
        .expect("the X100 raw decodes through the proxy");

    // The embedded preview in this file is 2176x1448. The sensor is 4310x2870.
    // Anything at or below the preview's size means the sensor path did not run.
    assert_eq!(
        (img.full_extent.w, img.full_extent.h),
        (4310, 2870),
        "must be sensor resolution, not the embedded preview's 2176x1448"
    );
    assert_eq!(
        img.pixels.format,
        PixelFormat::Rgba16F,
        "the graph expects half-float working-linear pixels"
    );

    // Working-space linear, finite, and not a black frame. A transform that
    // silently produced NaN would still have the right dimensions.
    let mut finite = 0u64;
    let mut nonzero = 0u64;
    for y in (0..img.pixels.extent.h).step_by(37) {
        for x in (0..img.pixels.extent.w).step_by(37) {
            let [r, g, b, a] = img.pixels.get_rgba_f32(x, y);
            assert!(a == 1.0, "alpha must be opaque at ({x},{y})");
            if r.is_finite() && g.is_finite() && b.is_finite() {
                finite += 1;
            }
            if r > 0.001 || g > 0.001 || b > 0.001 {
                nonzero += 1;
            }
        }
    }
    assert!(finite > 0, "sampled no pixels at all");
    assert_eq!(finite, nonzero + (finite - nonzero), "sanity");
    assert!(
        nonzero * 100 / finite > 50,
        "more than half the frame is black: the transform produced nothing"
    );
}

#[test]
fn orientation_transposes_the_frame() {
    let Some(provider) = RawSourceProvider::autodetect() else {
        eprintln!("SKIPPED orientation_transposes_the_frame: no LibRaw proxy");
        return;
    };
    let path = fixtures().join("fujifilm-x100.raf");
    if !path.exists() {
        return;
    }
    let upright = provider
        .decode_for_test(&path, Orientation::O1, &CancelToken::new())
        .expect("decodes");
    let rotated = provider
        .decode_for_test(&path, Orientation::O6, &CancelToken::new())
        .expect("decodes");
    assert_eq!(
        (rotated.full_extent.w, rotated.full_extent.h),
        (upright.full_extent.h, upright.full_extent.w),
        "a 90 degree orientation must transpose the frame, or portraits render sideways"
    );
}
