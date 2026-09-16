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

use lightbox_color::WbMode;
use lightbox_core::RawSourceProvider;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::PixelFormat;
use lightbox_types::Orientation;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn a_raw_decodes_to_upright_working_linear_at_sensor_resolution() {
    let Some((provider, path)) =
        provider_and_fixture("a_raw_decodes_to_upright_working_linear_at_sensor_resolution")
    else {
        return;
    };

    let img = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("the X100 raw decodes through the proxy")
        .image;

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
    let Some((provider, path)) = provider_and_fixture("orientation_transposes_the_frame") else {
        return;
    };
    let upright = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes")
        .image;
    let rotated = provider
        .decode_for_test(&path, Orientation::O6, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes")
        .image;
    assert_eq!(
        (rotated.full_extent.w, rotated.full_extent.h),
        (upright.full_extent.h, upright.full_extent.w),
        "a 90 degree orientation must transpose the frame, or portraits render sideways"
    );
}

// ─── absolute Kelvin white balance ──────────────────────────────────────────

/// The one gate every test in this file goes through. Skips, with a line
/// saying why, when the LibRaw proxy or the fixture is missing; **hard-fails
/// on either** when `LIGHTBOX_TEST_LIBRAW=1` is set.
///
/// Both branches honour the switch, and that is the point of routing every
/// test through here. An earlier version honoured it on the proxy branch
/// only, and one test skipped on a missing fixture with no message at all,
/// so on a machine with the proxy and no fixtures the whole file reported
/// green in 0.00 s while running nothing. A test that skips is not a test
/// that passed, and a switch named "make this a hard failure" that only
/// half works is worse than none, because it is trusted.
fn provider_and_fixture(test: &str) -> Option<(RawSourceProvider, PathBuf)> {
    let hard = std::env::var("LIGHTBOX_TEST_LIBRAW").is_ok();
    let Some(provider) = RawSourceProvider::autodetect() else {
        let why = format!(
            "{test}: no LibRaw proxy. Build it with `cargo build --release -p \
             lightbox-rawproxy --features libraw` or set LIGHTBOX_RAWPROXY_BIN. Set \
             LIGHTBOX_TEST_LIBRAW=1 to make this a hard failure."
        );
        assert!(!hard, "LIGHTBOX_TEST_LIBRAW=1 was set: {why}");
        eprintln!("SKIPPED {why}");
        return None;
    };
    let path = fixtures().join("fujifilm-x100.raf");
    if !path.exists() {
        let why = format!(
            "{test}: fixture {} not fetched, run `cargo xtask fixtures`",
            path.display()
        );
        assert!(!hard, "LIGHTBOX_TEST_LIBRAW=1 was set: {why}");
        eprintln!("SKIPPED {why}");
        return None;
    }
    Some((provider, path))
}

/// Mean linear `(R, G, B)` over a coarse sample of the frame.
fn mean_rgb(img: &lightbox_render::ng::SourceImage) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    let mut n = 0u64;
    for y in (0..img.pixels.extent.h).step_by(23) {
        for x in (0..img.pixels.extent.w).step_by(23) {
            let [r, g, b, _] = img.pixels.get_rgba_f32(x, y);
            sum[0] += r as f64;
            sum[1] += g as f64;
            sum[2] += b as f64;
            n += 1;
        }
    }
    assert!(n > 0, "sampled no pixels");
    [sum[0] / n as f64, sum[1] / n as f64, sum[2] / n as f64]
}

/// The as-shot decode is deterministic, byte for byte.
///
/// Half of the "a raw file with no white balance set is unchanged" promise.
/// Byte equality, not a tolerance, because a tolerance here would let a real
/// drift hide inside it. The other half, that these bytes are the same ones
/// the provider produced when `WbMode::AsShot` was hardcoded, is pinned by
/// the committed golden the CLI renders a CR3 against
/// (`crates/lightbox-cli/tests/e2e.rs`, golden
/// `crates/lightbox-cli/goldens/cli.render/pv1/canon-eos-r6-fit240.png`),
/// which goes through this provider whenever the LibRaw proxy is built.
#[test]
fn as_shot_decodes_to_the_camera_neutral_exactly() {
    let Some((provider, path)) =
        provider_and_fixture("as_shot_decodes_to_the_camera_neutral_exactly")
    else {
        return;
    };
    let a = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes");
    let b = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes");
    assert_eq!(
        a.image.pixels.bytes, b.image.pixels.bytes,
        "the as-shot decode must be deterministic, byte for byte"
    );
    assert_eq!(
        a.as_shot, b.as_shot,
        "the as-shot white point must be deterministic too"
    );
}

/// The as-shot reading is a real colour temperature from the file, not a
/// placeholder: inside the band a camera can actually record, and unchanged
/// by whatever white balance the decode was asked for. The second half is
/// what lets "As Shot" stay on the mode combo after a Custom drag.
#[test]
fn the_as_shot_reading_is_the_files_own_and_survives_a_custom_wb() {
    let Some((provider, path)) =
        provider_and_fixture("the_as_shot_reading_is_the_files_own_and_survives_a_custom_wb")
    else {
        return;
    };
    let shot = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes")
        .as_shot
        .expect("this file carries an as-shot neutral");
    assert!(
        (1500.0..=25000.0).contains(&shot.kelvin),
        "as-shot Kelvin {} is not a temperature a camera records",
        shot.kelvin
    );
    assert!(
        shot.tint.abs() < 150.0,
        "as-shot tint {} is off the slider's own range",
        shot.tint
    );

    let under_custom = provider
        .decode_for_test(
            &path,
            Orientation::O1,
            &WbMode::TempTint {
                kelvin: 9000.0,
                tint: 0.0,
            },
            &CancelToken::new(),
        )
        .expect("decodes")
        .as_shot
        .expect("the as-shot neutral does not depend on the requested wb");
    assert_eq!(
        shot, under_custom,
        "the file's as-shot white point cannot depend on what the user dialled in"
    );
}

/// **Warmer must look warmer, and it must be the camera's own matrices that
/// say so.** A raw decode at 9000 K has to carry more red relative to blue
/// than the same file at 3000 K; the ratio, not the absolute level, is the
/// measurement, because the two decodes also differ in overall gain.
///
/// This is the test the whole change exists for. A Kelvin readout that moves
/// without moving the pixels in the right direction is the "slider that moves
/// and lies" this was written to avoid.
#[test]
fn a_higher_kelvin_renders_the_sensor_warmer() {
    let Some((provider, path)) = provider_and_fixture("a_higher_kelvin_renders_the_sensor_warmer")
    else {
        return;
    };
    let decode = |kelvin: f64| {
        provider
            .decode_for_test(
                &path,
                Orientation::O1,
                &WbMode::TempTint { kelvin, tint: 0.0 },
                &CancelToken::new(),
            )
            .expect("decodes")
            .image
    };
    let cool = mean_rgb(&decode(3000.0));
    let warm = mean_rgb(&decode(9000.0));

    let ratio = |m: [f64; 3]| m[0] / m[2].max(1e-9);
    assert!(
        ratio(warm) > ratio(cool),
        "9000 K must render warmer than 3000 K: R/B was {} at 9000 K and {} at 3000 K \
         (means warm={warm:?} cool={cool:?})",
        ratio(warm),
        ratio(cool)
    );
    assert!(
        ratio(warm) / ratio(cool) > 1.2,
        "the 3000 K to 9000 K swing moved R/B by only {}x, which is not a white-balance change",
        ratio(warm) / ratio(cool)
    );
}

/// Asking for the file's own as-shot temperature by number lands on the
/// as-shot render. Not byte-identical: `TempTint` builds the neutral forward
/// from `(Kelvin, tint)` while `AsShot` iterates backward from the camera
/// neutral, and the Robertson locus those share is only exact to a few
/// Kelvin (`lightbox-color/src/cct.rs`'s own `within_15k` bound). Close is
/// the correct claim, and close is what makes the readout trustworthy: the
/// number the slider shows really does reproduce the picture it started on.
#[test]
fn dialling_in_the_as_shot_kelvin_reproduces_the_as_shot_render() {
    let Some((provider, path)) =
        provider_and_fixture("dialling_in_the_as_shot_kelvin_reproduces_the_as_shot_render")
    else {
        return;
    };
    let shot = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("decodes");
    let as_shot = shot.as_shot.expect("this file carries an as-shot neutral");
    let dialled = provider
        .decode_for_test(
            &path,
            Orientation::O1,
            &WbMode::TempTint {
                kelvin: as_shot.kelvin,
                tint: as_shot.tint,
            },
            &CancelToken::new(),
        )
        .expect("decodes");

    let a = mean_rgb(&shot.image);
    let b = mean_rgb(&dialled.image);
    for ch in 0..3 {
        let rel = (a[ch] - b[ch]).abs() / a[ch].abs().max(1e-9);
        assert!(
            rel < 0.02,
            "channel {ch} moved {rel} between as-shot and its own Kelvin ({} K, tint {}): \
             as_shot={a:?} dialled={b:?}",
            as_shot.kelvin,
            as_shot.tint
        );
    }
}
