// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Recovering a blown sky out of the raw file's own highlight latitude.
//!
//! # What the latitude is, and why it is not free
//!
//! A sensor's photosites all saturate at the same raw level, but daylight is
//! not neutral at that level, so green reaches the white level about a stop
//! before red does. A highlight with green clipped is therefore still carrying
//! real, varying, unclipped red and blue. That difference is the whole of the
//! recoverable headroom, and spending it takes three things to hold at once:
//!
//! 1. the proxy must not white-balance, or every channel clips together and
//!    the asymmetry is gone before Lightbox sees a pixel;
//! 2. the input transform must not clip at white, because white balance is
//!    exactly what turns that per-channel slack into working values above 1;
//! 3. `global.tone_recovery` must map those above-1 values back into range
//!    without flattening them.
//!
//! One test each, in that order. The third is the one that matters and it is
//! written to be hard to pass by accident: **it runs the identical recovery
//! twice over the identical blown region, differing only in whether the input
//! kept its above-white data**, and asserts detail comes back in one and not
//! the other. Asserting merely that recovery changed the output would prove
//! nothing, moving any slider does that, and the control case here demonstrates
//! precisely that failure: on clipped input, `highlights = 100` moves every
//! blown pixel from 1.0 to 0.5 and recovers not one bit of structure.
//!
//! Fixture choice is measured, not assumed. Of the nine raw fixtures only
//! `canon-eos-350d.cr2` has genuinely blown highlights (0.60 % of the frame
//! with a channel at the white level); the rest top out below saturation, and
//! `canon-eos-r6.cr3` and `sigma-fp.dng` do not come close. No fixture has a
//! single sample *above* LibRaw's declared white level, so there is no
//! whole-pixel headroom to reconstruct, only per-channel latitude.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lightbox_color::wb::WbMode;
use lightbox_core::RawSourceProvider;
use lightbox_decode::{
    decode_for_develop, DecodeOpts, ProxySupervisor, RawDecode, SourceImage as DecodedSource,
};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::nodes::global::tone_recovery::{
    apply_recovery, guided_filter_luma, working_luma, working_luma_weights,
};
use lightbox_types::Orientation;

/// The one fixture in the corpus with genuinely clipped highlights.
const BLOWN_FIXTURE: &str = "canon-eos-350d.cr2";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Skips, saying why, when the fixture is absent; hard-fails under
/// `LIGHTBOX_TEST_LIBRAW=1`. The same contract as `raw_source_decode.rs`'s
/// gate, and for the same reason: an earlier version honoured the switch on
/// the proxy branch and not this one, so a machine with LibRaw and no
/// fixtures reported the whole file green in 0.00 s while running nothing.
fn blown_fixture(test: &str) -> Option<PathBuf> {
    let path = fixtures().join(BLOWN_FIXTURE);
    if !path.exists() {
        let why = format!(
            "{test}: fixture {} not fetched, run `cargo xtask fixtures`",
            path.display()
        );
        assert!(
            std::env::var("LIGHTBOX_TEST_LIBRAW").is_err(),
            "LIGHTBOX_TEST_LIBRAW=1 was set: {why}"
        );
        eprintln!("SKIPPED {why}");
        return None;
    }
    Some(path)
}

fn require_proxy(test: &str) -> Option<ProxySupervisor> {
    match ProxySupervisor::autodetect() {
        Some(sup) => Some(sup),
        None => {
            let why = format!(
                "{test}: no LibRaw proxy. Build it with `cargo build --release -p \
                 lightbox-rawproxy --features libraw` or set LIGHTBOX_RAWPROXY_BIN. Set \
                 LIGHTBOX_TEST_LIBRAW=1 to make this a hard failure."
            );
            assert!(
                std::env::var("LIGHTBOX_TEST_LIBRAW").is_err(),
                "LIGHTBOX_TEST_LIBRAW=1 was set: {why}"
            );
            eprintln!("SKIPPED {why}");
            None
        }
    }
}

fn decode_camera_native(sup: &ProxySupervisor, path: &std::path::Path) -> DecodedSource {
    let opts = DecodeOpts {
        interim_demosaic: true,
        ..DecodeOpts::default()
    };
    match decode_for_develop(sup, path, &opts).expect("the 350D decodes through the proxy") {
        RawDecode::DemosaicedInterim(s) | RawDecode::Linear(s) => s,
        other => panic!("expected demosaiced camera-native pixels, got {other:?}"),
    }
}

/// Step 1: the proxy must hand over pixels whose channels clip at *different*
/// scene brightnesses, because that asymmetry is the latitude itself.
///
/// This is a guard on `libraw_shim.c`'s develop setup, not a restatement of it.
/// Any change that lets white balance in before Lightbox, camera WB in
/// `user_mul`, `use_camera_wb = 1`, or a LibRaw highlight mode of 3 or more
/// (which rebuilds channels from each other), collapses the three counts
/// toward each other and fails here. That failure is otherwise invisible: the
/// image still decodes, still looks broadly right, and simply cannot be
/// recovered any more.
#[test]
fn the_proxy_hands_over_per_channel_highlight_latitude() {
    let test = "the_proxy_hands_over_per_channel_highlight_latitude";
    let (Some(sup), Some(path)) = (require_proxy(test), blown_fixture(test)) else {
        return;
    };
    let src = decode_camera_native(&sup, &path);
    let ch = src.channels as usize;
    assert!(ch >= 3, "expected at least RGB, got {ch} channels");

    // Camera-native is normalized so the sensor's white level is exactly 1.
    let mut clipped = [0u64; 3];
    let mut n = 0u64;
    for px in src.data.chunks_exact(ch) {
        n += 1;
        for c in 0..3 {
            if px[c] >= 0.999 {
                clipped[c] += 1;
            }
        }
    }
    let pct = |v: u64| 100.0 * v as f64 / n as f64;
    eprintln!(
        "{BLOWN_FIXTURE}: camera-native clipped R={} ({:.4} %) G={} ({:.4} %) B={} ({:.4} %)",
        clipped[0],
        pct(clipped[0]),
        clipped[1],
        pct(clipped[1]),
        clipped[2],
        pct(clipped[2])
    );

    assert!(
        pct(clipped[1]) > 0.1,
        "this fixture is supposed to have a blown sky, but only {:.4} % of green is at the white \
         level. Either the fixture changed or the decode did.",
        pct(clipped[1])
    );
    // Measured: green clips 200x more often than red and 103x more than blue.
    // 20x leaves room for a demosaic change while still catching a collapse.
    for (c, name) in [(0usize, "red"), (2, "blue")] {
        assert!(
            clipped[1] > 20 * clipped[c].max(1),
            "green should clip far sooner than {name} ({} vs {}); channels clipping together \
             means white balance was applied before Lightbox saw the pixels, and the highlight \
             latitude is gone",
            clipped[1],
            clipped[c]
        );
    }
}

/// Step 2: the input transform must carry that latitude across as working
/// values above 1, rather than clipping at white.
///
/// The transform is where white balance is folded in (`cam_to_working`), so it
/// is where per-channel slack *becomes* headroom. Both places that used to
/// throw it away, `Curve1D::eval`'s domain clamp and `apply_delta`'s value
/// clamp, are on this path, and both are exercised here: the default look
/// wires up a tone curve *and* hue/sat shaping for every raw source.
#[test]
fn the_input_transform_carries_the_latitude_above_white_into_the_graph() {
    let test = "the_input_transform_carries_the_latitude_above_white_into_the_graph";
    let (Some(_sup), Some(path)) = (require_proxy(test), blown_fixture(test)) else {
        return;
    };
    let provider = RawSourceProvider::autodetect().expect("the proxy was just found");
    let img = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("the 350D decodes to working-space linear");

    let (w, h) = (img.image.pixels.extent.w, img.image.pixels.extent.h);
    let (mut above, mut n, mut peak) = (0u64, 0u64, 0f32);
    for y in 0..h {
        for x in 0..w {
            let p = img.image.pixels.get_rgba_f32(x, y);
            let m = p[0].max(p[1]).max(p[2]);
            n += 1;
            if m > 1.0 {
                above += 1;
            }
            if m > peak {
                peak = m;
            }
        }
    }
    let pct = 100.0 * above as f64 / n as f64;
    eprintln!("{BLOWN_FIXTURE}: {pct:.4} % of the frame is above white, peaking at {peak:.3}");

    // Measured: 2.29 % of the frame, peak 2.12 in f16 (2.28 in f32 before the
    // half-float round-trip), about 1.1 stops.
    assert!(
        pct > 1.0,
        "only {pct:.4} % of the frame carries above-white data. The input transform is clipping \
         at white again, and there is nothing left for tone recovery to pull back."
    );
    assert!(
        peak > 1.5,
        "the brightest working value is {peak:.3}; the latitude in this file reaches ~2.1, so \
         something on the transform path is capping it"
    );
    assert!(
        peak.is_finite() && peak < 100.0,
        "peak {peak} is not a plausible scene value; a clamp was removed without a floor"
    );
}

/// Step 3, the acceptance test: `global.tone_recovery` turns that latitude
/// back into visible structure, and could not have without it.
///
/// The measurement is deliberately not "the output changed". It counts, over
/// exactly the pixels a clip-at-white pipeline renders as indistinguishable
/// pure white, how many **distinct 8-bit luma levels** the recovered output
/// resolves them into. One level means the region is still flat; many levels
/// means information that was previously unrepresentable is now on screen.
///
/// Both arms run the identical recovery, over the identical region, at the
/// identical slider position, through the node's own CPU reference functions
/// (`tone_recovery::eval_cpu` is exactly `working_luma` → `guided_filter_luma`
/// → `apply_recovery`, see that method). The only difference is whether the
/// input kept its above-white data. The clipped arm is the control, and it is
/// the honest demonstration of why "the output changed" is not evidence: it
/// moves every blown pixel from 1.0 to 0.5, a perfectly visible change, while
/// resolving them into exactly one level, the same one, still flat.
#[test]
fn tone_recovery_brings_structure_back_out_of_a_blown_sky() {
    let test = "tone_recovery_brings_structure_back_out_of_a_blown_sky";
    let (Some(_sup), Some(path)) = (require_proxy(test), blown_fixture(test)) else {
        return;
    };
    let provider = RawSourceProvider::autodetect().expect("the proxy was just found");
    let img = provider
        .decode_for_test(&path, Orientation::O1, &WbMode::AsShot, &CancelToken::new())
        .expect("the 350D decodes to working-space linear");

    // Find the blown region rather than hardcoding it, so the test still aims
    // at the right sky if the decode shifts. Coarse stride first: only the
    // winning window is read at full resolution.
    const WIN: u32 = 96;
    let (bx, by) = densest_blown_window(&img.image, WIN);
    eprintln!("{BLOWN_FIXTURE}: densest {WIN}x{WIN} blown window at ({bx}, {by})");

    let np = (WIN * WIN) as usize;
    let mut headroom = vec![[0f32; 4]; np];
    let mut clipped = vec![[0f32; 4]; np];
    for dy in 0..WIN {
        for dx in 0..WIN {
            let p = img.image.pixels.get_rgba_f32(bx + dx, by + dy);
            let i = (dy * WIN + dx) as usize;
            headroom[i] = p;
            // What the same pixel is worth to a pipeline that clips at white.
            clipped[i] = [p[0].min(1.0), p[1].min(1.0), p[2].min(1.0), p[3]];
        }
    }

    // The pixels under test: those with *every* channel at or above white, so
    // a clip-at-white pipeline renders them as one flat, identical white.
    let blown: Vec<usize> = (0..np)
        .filter(|&i| headroom[i][0].min(headroom[i][1]).min(headroom[i][2]) >= 1.0)
        .collect();
    let blown_pct = 100.0 * blown.len() as f64 / np as f64;
    eprintln!(
        "  {} of {np} px ({blown_pct:.1} %) are pure white without the latitude",
        blown.len()
    );
    assert!(
        blown_pct > 50.0,
        "the chosen window is only {blown_pct:.1} % blown; this test needs a genuinely blown \
         region to say anything about recovering one"
    );

    let before = recover(&clipped, WIN, 0.0, &blown);
    let control = recover(&clipped, WIN, 100.0, &blown);
    let recovered = recover(&headroom, WIN, 100.0, &blown);
    for (label, r) in [
        ("clipped  hl=0", &before),
        ("clipped  hl=100", &control),
        ("headroom hl=100", &recovered),
    ] {
        eprintln!(
            "  {label}: distinct 8-bit luma levels = {:3}, variance = {:.3e}, mean = {:.4}, \
             still at white = {}",
            r.levels, r.variance, r.mean, r.at_white
        );
    }

    // The region really is featureless to start with: one level, zero spread.
    assert_eq!(
        before.levels, 1,
        "the blown pixels should be a single flat white before recovery, got {} levels",
        before.levels
    );

    // The control. Recovery visibly moved these pixels (1.0 -> 0.5) and
    // recovered nothing: this is what "the output changed" is worth.
    assert!(
        (control.mean - 0.5).abs() < 0.05,
        "expected the control to halve a flat white to ~0.5, got {:.4}",
        control.mean
    );
    assert!(
        control.levels <= 3,
        "the control resolved {} distinct levels out of flat white. Detail cannot come from \
         nowhere; either the region is not actually flat or the recovery is inventing texture.",
        control.levels
    );

    // The result. Same node, same slider, same pixels, latitude kept.
    // Measured: 59 levels against the control's 1.
    assert!(
        recovered.levels >= 25,
        "recovery resolved only {} distinct luma levels out of the blown region (the control \
         manages {}). The latitude is not reaching the node.",
        recovered.levels,
        control.levels
    );
    assert!(
        recovered.levels >= 10 * control.levels,
        "recovery ({} levels) must resolve far more structure than the same slider on clipped \
         input ({} levels), or the detail is not coming from the raw latitude",
        recovered.levels,
        control.levels
    );
    // Spread, not just distinct levels: rules out a couple of stray outliers.
    assert!(
        recovered.variance > 1e-3,
        "recovered luma variance {:.3e} is too flat to be structure",
        recovered.variance
    );
    assert!(
        recovered.variance > 100.0 * control.variance.max(f64::MIN_POSITIVE),
        "recovered variance {:.3e} vs control {:.3e}",
        recovered.variance,
        control.variance
    );
    // And the region is actually back in range, not merely varied above white.
    assert_eq!(
        recovered.at_white, 0,
        "{} blown pixels are still at or above white after recovery",
        recovered.at_white
    );
}

/// What recovery did to one set of pixels: how finely it resolves them, how
/// far they spread, and whether any are still pinned at white.
struct Recovered {
    levels: usize,
    variance: f64,
    mean: f64,
    at_white: usize,
}

/// Runs `global.tone_recovery`'s own CPU reference math over a `win`x`win`
/// tile and reports [`Recovered`] over `subset`.
fn recover(rgba: &[[f32; 4]], win: u32, highlights: f32, subset: &[usize]) -> Recovered {
    let weights = working_luma_weights();
    let luma: Vec<f32> = rgba
        .iter()
        .map(|p| working_luma([p[0], p[1], p[2]], weights))
        .collect();
    let base = guided_filter_luma(&luma, win, win);

    let mut levels = BTreeSet::new();
    let (mut sum, mut sum_sq) = (0f64, 0f64);
    let mut at_white = 0usize;
    for &i in subset {
        let out = apply_recovery(rgba[i], luma[i], base[i], highlights, 0.0);
        let l = working_luma([out[0], out[1], out[2]], weights);
        // Quantize the way the display transform eventually will: two scene
        // values that land on one 8-bit level are two values a viewer cannot
        // tell apart, which is the thing being counted.
        levels.insert((l.clamp(0.0, 1.0) * 255.0).round() as u32);
        sum += f64::from(l);
        sum_sq += f64::from(l) * f64::from(l);
        if out[0].min(out[1]).min(out[2]) >= 1.0 {
            at_white += 1;
        }
    }
    let n = subset.len() as f64;
    Recovered {
        levels: levels.len(),
        variance: (sum_sq / n - (sum / n) * (sum / n)).max(0.0),
        mean: sum / n,
        at_white,
    }
}

/// Top-left corner of the `win`x`win` window holding the most above-white
/// pixels, searched on a coarse grid (half-window steps, every 6th pixel) so
/// the scan stays cheap in a debug build.
fn densest_blown_window(img: &lightbox_render::ng::SourceImage, win: u32) -> (u32, u32) {
    let (w, h) = (img.pixels.extent.w, img.pixels.extent.h);
    assert!(
        w >= win && h >= win,
        "frame {w}x{h} is smaller than the window"
    );
    let (mut best, mut best_count) = ((0u32, 0u32), -1i64);
    let mut y = 0;
    while y + win <= h {
        let mut x = 0;
        while x + win <= w {
            let mut count = 0i64;
            for dy in (0..win).step_by(6) {
                for dx in (0..win).step_by(6) {
                    let p = img.pixels.get_rgba_f32(x + dx, y + dy);
                    if p[0].min(p[1]).min(p[2]) >= 1.0 {
                        count += 1;
                    }
                }
            }
            if count > best_count {
                best_count = count;
                best = (x, y);
            }
            x += win / 2;
        }
        y += win / 2;
    }
    best
}
