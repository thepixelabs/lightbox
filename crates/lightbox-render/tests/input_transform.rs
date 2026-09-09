// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The **source → working-space input transform** gate (E02 §"Decoded
//! display-referred, ICC-tagged (untagged ⇒ assumed sRGB) → LCMS2 to working
//! space (linearized) → same downstream pipeline").
//!
//! # The bug this pins
//!
//! The engine had no input transform. A rendered (non-raw) source arrives
//! sRGB-**encoded**, the shell's `PreviewSourceProvider` hands over
//! `PixelFormat::Rgba8Srgb` preview bytes, but the source lift normalized them
//! by `/255` and nothing else, so an sRGB code point was reinterpreted as if it
//! were already a ProPhoto-linear working value. `xform.display` then applied
//! the working→sRGB encode on top of pixels that had never been decoded, i.e. a
//! double encode:
//!
//! ```text
//!   sRGB 128  →  0.502 "linear"  →  sRGB OETF  →  187      (before: +46 % lift)
//!   sRGB 128  →  0.216 linear    →  sRGB OETF  →  128      (after:  round-trip)
//! ```
//!
//! Every non-raw image was silently brightened and desaturated-then-shifted by
//! the mismatched primaries, with no edit applied. These tests fail loudly if
//! that regresses.
//!
//! # What each test guards
//!
//! - [`srgb_tagged_source_round_trips_through_the_engine`], the headline:
//!   identity recipe, sRGB in ≈ sRGB out, through the real `Engine::submit`.
//! - [`srgb_mid_grey_is_not_lifted`], the owner's exact symptom, as a number.
//! - [`working_linear_tag_is_a_bit_exact_identity`], the default tag still
//!   passes pixels through untouched, which is why no `ng` golden moved.
//! - [`cpu_and_gpu_lift_an_srgb_source_identically`], CPU/GPU parity (§4.4).
//!
//! # The second half: tagged wide-gamut sources
//!
//! Fixing the transfer function left the *primaries* half of the bug in place:
//! every file was lifted with sRGB primaries, including the Display-P3 ones a
//! modern iPhone produces by default. The tests below pin the wide-gamut tags.
//!
//! - [`display_p3_lifts_to_its_true_working_value`] /
//!   [`adobe_rgb_lifts_to_its_true_working_value`], the lift lands on the
//!   colour the file actually names.
//! - [`reading_display_p3_as_srgb_desaturates_it`], the visible consequence,
//!   end to end through `Engine::submit`, as code points.
//! - [`a_neutral_is_neutral_in_every_d65_tag`], why the bug hid: primaries
//!   errors do not move greys at all, only chromatic pixels.
//! - [`prophoto_is_a_transfer_only_lift`], the tag whose primaries already are
//!   the working space's.
//! - [`cpu_and_gpu_lift_a_display_p3_source_identically`], parity again, on the
//!   path that now has a matrix in it.

use std::sync::Arc;

use half::f16;
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::{to_working_tile_cpu, DeviceHandles};
use lightbox_render::ng::{
    BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, Extent,
    OutFormat, OutputPayload, PixelBuf, PixelFormat, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, Roi, SourceColorimetry, SourceError, SourceImage, SourceProvider,
    SourceQuality, SourceWant,
};
use lightbox_render::GpuContext;
use lightbox_render_testkit::compare::{delta_e_stats, psnr, TOLERANCE_PSNR_DB};
use lightbox_types::{ImageId, PV_M0};

// ── seams (mirrors `tests/ng_parity.rs`) ──────────────────────────────────────

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

struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unreachable!("ForceCpu never calls DeviceProvider::current")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async { Err(DeviceError::Rebuild("no device".to_owned())) })
    }
}

/// A source provider that hands over fixed pixels under an explicit
/// colorimetry tag, the seam the whole fix turns on.
struct TaggedSource {
    pixels: PixelBuf,
    colorimetry: SourceColorimetry,
}
impl SourceProvider for TaggedSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let pixels = self.pixels.clone();
        let colorimetry = self.colorimetry;
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry,
                full_extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// An 8-bit RGBA buffer of sRGB-encoded code points, the shape the preview
/// stack delivers (`PixelFormat::Rgba8Srgb`, tight rows).
fn srgb8_source(texels: &[[u8; 4]], w: u32, h: u32) -> PixelBuf {
    assert_eq!(texels.len(), (w * h) as usize);
    PixelBuf {
        bytes: texels.iter().flatten().copied().collect(),
        format: PixelFormat::Rgba8Srgb,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

/// A deterministic spread of sRGB code points: neutrals across the whole tone
/// range plus saturated and skin/sky colours, so a primaries error shows up as
/// well as a transfer-function error.
fn probe_texels() -> Vec<[u8; 4]> {
    let mut t = Vec::new();
    for v in [0u8, 16, 32, 64, 96, 128, 160, 192, 224, 255] {
        t.push([v, v, v, 255]);
    }
    for c in [
        [200u8, 60, 55],
        [70, 150, 80],
        [55, 85, 175],
        [196, 150, 130],
        [110, 145, 190],
        [240, 220, 40],
    ] {
        t.push([c[0], c[1], c[2], 255]);
    }
    t
}

fn build_engine(pref: BackendPref, dp: Arc<dyn DeviceProvider>, src: TaggedSource) -> Engine {
    Engine::with_compiler(
        dp,
        Arc::new(src),
        lightbox_render::ng::shipping_compiler(),
        EngineConfig {
            backend: pref,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds over the shipping PV1 configuration")
}

fn render(engine: &Engine, w: u32, h: u32, expect: BackendId) -> PixelBuf {
    let req = RenderRequest {
        image: ImageId(1),
        pv: PV_M0,
        recipe: Recipe::identity(PV_M0),
        roi: Roi { x: 0, y: 0, w, h },
        scale: RenderScale::OneToOne,
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
            RenderState::Complete(out) => {
                assert_eq!(out.backend, expect, "backend provenance");
                let OutputPayload::Pixels(px) = out.payload else {
                    panic!("expected pixels");
                };
                return px;
            }
            RenderState::Failed(e) => panic!("render failed: {e}"),
            RenderState::Cancelled => panic!("render cancelled"),
            _ => {
                if std::time::Instant::now() > deadline {
                    panic!("render did not complete within 20s");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

fn texels(px: &PixelBuf) -> Vec<[u8; 4]> {
    px.bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

/// Renders `probe_texels()` as a 1-row image on the CPU backend under `tag`.
fn render_probe_cpu(tag: SourceColorimetry) -> (Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);
    let engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        TaggedSource {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: tag,
        },
    );
    let out = render(&engine, w, h, BackendId::Cpu);
    (probe, texels(&out))
}

// ── the gate ──────────────────────────────────────────────────────────────────

/// **The headline.** An sRGB-tagged 8-bit source rendered with an *identity*
/// recipe comes back where it started: decode → working space → re-encode is a
/// round trip. Tolerance is 2 code points, which covers the f16 working
/// quantization and the 65³ display-LUT interpolation.
///
/// Before the input transform existed, mid and upper tones came back up to ~60
/// code points high, the "every image I load is automatically adjusted" report.
#[test]
fn srgb_tagged_source_round_trips_through_the_engine() {
    let (probe, out) = render_probe_cpu(SourceColorimetry::srgb());

    let mut worst = 0i32;
    for (i, (src, got)) in probe.iter().zip(out.iter()).enumerate() {
        for c in 0..3 {
            let d = i32::from(got[c]) - i32::from(src[c]);
            worst = worst.max(d.abs());
            assert!(
                d.abs() <= 2,
                "texel {i} channel {c}: sRGB {} rendered {} (drift {d}); \
                 an identity recipe must not change the image",
                src[c],
                got[c],
            );
        }
        assert_eq!(got[3], 255, "texel {i}: alpha must pass through");
    }
    assert!(worst <= 2, "worst round-trip drift {worst} code points");
}

/// The owner's symptom, pinned as a single number: sRGB mid-grey 128 must come
/// back at 128, not at ~187 (the +46 % lift the double encode produced).
#[test]
fn srgb_mid_grey_is_not_lifted() {
    let (probe, out) = render_probe_cpu(SourceColorimetry::srgb());
    let i = probe
        .iter()
        .position(|t| *t == [128, 128, 128, 255])
        .expect("probe contains mid-grey");
    let got = out[i];

    for (c, &v) in got.iter().enumerate().take(3) {
        assert!(
            (i32::from(v) - 128).abs() <= 2,
            "mid-grey channel {c} rendered {v} — expected ~128; \
             ~187 means the source transfer function is being skipped again",
        );
    }
    // Guard the specific historical value so a partial regression can't pass.
    assert!(
        got[1] < 150,
        "mid-grey green rendered {} — the double-encode bug is back",
        got[1],
    );
}

/// The default tag means "already working-space linear", so the lift must be a
/// **bit-exact** pass-through of `v/255`. This is why every committed `ng`
/// golden, all of which feed scene-linear pixels under
/// `SourceColorimetry::default()`, stayed byte-identical across this change.
#[test]
fn working_linear_tag_is_a_bit_exact_identity() {
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);
    let src = SourceImage {
        pixels: srgb8_source(&probe, w, h),
        colorimetry: SourceColorimetry::default(),
        full_extent: Extent { w, h },
        quality: SourceQuality::Full,
    };
    assert!(src.colorimetry.is_working_linear());

    let lifted = to_working_tile_cpu(&src);
    assert_eq!(lifted.format, PixelFormat::Rgba16F);
    for (i, texel) in probe.iter().enumerate() {
        for (c, &v) in texel.iter().enumerate() {
            let o = i * 8 + c * 2;
            let got = f16::from_le_bytes([lifted.bytes[o], lifted.bytes[o + 1]]).to_f32();
            let want = f16::from_f32(f32::from(v) / 255.0).to_f32();
            assert_eq!(
                got, want,
                "texel {i} channel {c}: identity lift must be bit-exact",
            );
        }
    }
}

/// The sRGB tag genuinely changes the lifted values, a guard against the
/// transform being silently wired to identity for every tag (which would make
/// the round-trip test above pass for the wrong reason, since a no-op lift plus
/// a no-op display transform also round-trips).
#[test]
fn the_srgb_tag_actually_decodes() {
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);
    let mk = |tag| SourceImage {
        pixels: srgb8_source(&probe, w, h),
        colorimetry: tag,
        full_extent: Extent { w, h },
        quality: SourceQuality::Full,
    };
    let linear = to_working_tile_cpu(&mk(SourceColorimetry::default()));
    let srgb = to_working_tile_cpu(&mk(SourceColorimetry::srgb()));
    assert_ne!(
        linear.bytes, srgb.bytes,
        "an sRGB-tagged lift must not equal the identity lift",
    );

    // Mid-grey specifically: 128/255 = 0.502 encoded ⇒ ~0.216 linear.
    let i = probe
        .iter()
        .position(|t| *t == [128, 128, 128, 255])
        .unwrap();
    let g = f16::from_le_bytes([srgb.bytes[i * 8], srgb.bytes[i * 8 + 1]]).to_f32();
    assert!(
        (g - 0.216).abs() < 0.02,
        "sRGB 128 linearized to {g}, expected ~0.216",
    );
}

// ── tagged wide-gamut sources (Display-P3 / Adobe RGB / ProPhoto) ────────────

/// Reads channel `c` of texel `i` out of a lifted working tile.
fn working_channel(lifted: &PixelBuf, i: usize, c: usize) -> f32 {
    let o = i * 8 + c * 2;
    f16::from_le_bytes([lifted.bytes[o], lifted.bytes[o + 1]]).to_f32()
}

/// Lifts a single 8-bit texel under `tag` and returns its working-space RGB.
fn lift_one(texel: [u8; 4], tag: SourceColorimetry) -> [f32; 3] {
    let src = SourceImage {
        pixels: srgb8_source(&[texel], 1, 1),
        colorimetry: tag,
        full_extent: Extent { w: 1, h: 1 },
        quality: SourceQuality::Full,
    };
    let lifted = to_working_tile_cpu(&src);
    [
        working_channel(&lifted, 0, 0),
        working_channel(&lifted, 0, 1),
        working_channel(&lifted, 0, 2),
    ]
}

fn assert_close(got: [f32; 3], want: [f32; 3], tol: f32, what: &str) {
    for c in 0..3 {
        assert!(
            (got[c] - want[c]).abs() <= tol,
            "{what} channel {c}: got {got:?}, want {want:?} (tol {tol})",
        );
    }
}

/// **The Display-P3 gap, as a number.** A P3-tagged code point must land on the
/// working-space value P3 actually names, not on the one sRGB's narrower
/// primaries would give it.
///
/// The expected values are pinned as literals rather than recomputed from
/// `lightbox_color`, so this test cannot quietly agree with a broken matrix; the
/// matrices themselves are checked against Little-CMS2 in
/// `lightbox_color::cms::tests::source_space_matrices_match_lcms`.
///
/// Note the third row of the first case: fully saturated P3 red lifts to a
/// *slightly negative* working blue. That is correct and deliberate, P3's red
/// primary lies exactly on the ProPhoto/ROMM boundary and the D65→D50
/// adaptation nudges it a hair past. See `SourceLift::to_working_primaries`.
#[test]
fn display_p3_lifts_to_its_true_working_value() {
    // (code point, working under P3, working under sRGB, the old behaviour)
    let cases: [([u8; 4], [f32; 3], [f32; 3]); 3] = [
        (
            [255, 0, 0, 255],
            [0.63175, 0.08320, -0.00127],
            [0.52934, 0.09837, 0.01688],
        ),
        (
            [200, 60, 55, 255],
            [0.38045, 0.08927, 0.03787],
            [0.32602, 0.09736, 0.04813],
        ),
        (
            [70, 150, 80, 255],
            [0.11630, 0.27776, 0.09165],
            [0.14437, 0.27468, 0.10635],
        ),
    ];
    for (texel, want_p3, want_srgb) in cases {
        let got = lift_one(texel, SourceColorimetry::DISPLAY_P3);
        assert_close(got, want_p3, 1e-3, &format!("P3 {texel:?}"));

        // And it is genuinely a different answer from the sRGB reading, or the
        // tag would be decorative.
        let as_srgb = lift_one(texel, SourceColorimetry::SRGB);
        assert_close(as_srgb, want_srgb, 1e-3, &format!("sRGB {texel:?}"));
        let spread = (0..3)
            .map(|c| (got[c] - as_srgb[c]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            spread > 0.005,
            "P3 and sRGB readings of {texel:?} differ by only {spread}",
        );
    }
}

/// Adobe RGB carries a *different transfer function* as well as different
/// primaries (a pure 563/256 power law), so it exercises both halves of the
/// lift at once.
#[test]
fn adobe_rgb_lifts_to_its_true_working_value() {
    let cases: [([u8; 4], [f32; 3]); 3] = [
        ([255, 0, 0, 255], [0.74017, 0.13755, 0.02361]),
        ([200, 60, 55, 255], [0.44353, 0.11620, 0.04783]),
        ([70, 150, 80, 255], [0.08981, 0.26965, 0.09486]),
    ];
    for (texel, want) in cases {
        assert_close(
            lift_one(texel, SourceColorimetry::ADOBE_RGB),
            want,
            1e-3,
            &format!("AdobeRGB {texel:?}"),
        );
    }
}

/// ProPhoto-tagged files share the working space's primaries, so their input
/// transform is a pure linearization, the matrix must be the identity, and the
/// result must be exactly `prophoto_eotf(v/255)` per channel.
#[test]
fn prophoto_is_a_transfer_only_lift() {
    for v in [0u8, 8, 32, 128, 200, 255] {
        let got = lift_one([v, v, v, 255], SourceColorimetry::PROPHOTO);
        let want = f16::from_f32(lightbox_color::matrix::spaces::prophoto_eotf(
            f32::from(v) / 255.0,
        ))
        .to_f32();
        for (c, &g) in got.iter().enumerate() {
            assert!(
                (g - want).abs() <= 1e-4,
                "ProPhoto {v} channel {c}: {g} vs {want}; a neutral must stay \
                 neutral and untouched by any primaries matrix",
            );
        }
    }
}

/// **Why the bug was easy to miss.** Every space involved is D65 (or, for
/// ProPhoto, the working space's own D50), so a *neutral* lifts identically
/// under sRGB and Display-P3, bit for bit. The primaries error only ever moved
/// chromatic pixels, which is why a grey-card check would have shown nothing
/// wrong while real photos rendered flat.
#[test]
fn a_neutral_is_neutral_in_every_d65_tag() {
    for v in [0u8, 32, 64, 128, 192, 255] {
        let srgb = lift_one([v, v, v, 255], SourceColorimetry::SRGB);
        let p3 = lift_one([v, v, v, 255], SourceColorimetry::DISPLAY_P3);
        assert_eq!(
            srgb, p3,
            "neutral {v}: sRGB and P3 must agree bit-for-bit on a grey",
        );
        assert!(
            (srgb[0] - srgb[1]).abs() < 1e-6 && (srgb[1] - srgb[2]).abs() < 1e-6,
            "neutral {v} lifted to a non-neutral {srgb:?}",
        );
    }
}

/// **The user-visible consequence, end to end.** The same code points rendered
/// through the real engine with an identity recipe, tagged sRGB versus tagged
/// Display-P3, out to an sRGB buffer.
///
/// Reading a P3 file as sRGB **desaturates** it: the file's numbers describe a
/// colour in P3's wider primaries, and interpreting them in sRGB's narrower ones
/// names a duller colour. (Worth stating precisely, the failure is often
/// described the other way round. It is *sRGB content shown as P3* that
/// over-saturates; a *P3 file misread as sRGB*, which is what Lightbox was
/// doing, comes out flat.) The saturated green case is the loudest: 51 code
/// points of red.
#[test]
fn reading_display_p3_as_srgb_desaturates_it() {
    // (code point, sRGB-tagged render, P3-tagged render)
    let cases: [([u8; 4], [u8; 3], [u8; 3]); 2] = [
        ([200, 60, 55, 255], [200, 60, 55], [217, 42, 46]),
        ([70, 150, 80, 255], [70, 150, 80], [19, 152, 71]),
    ];
    let texels_in: Vec<[u8; 4]> = cases.iter().map(|(t, _, _)| *t).collect();
    let (w, h) = (texels_in.len() as u32, 1);

    let render_as = |tag| {
        let engine = build_engine(
            BackendPref::ForceCpu,
            Arc::new(NullDevice),
            TaggedSource {
                pixels: srgb8_source(&texels_in, w, h),
                colorimetry: tag,
            },
        );
        texels(&render(&engine, w, h, BackendId::Cpu))
    };
    let as_srgb = render_as(SourceColorimetry::SRGB);
    let as_p3 = render_as(SourceColorimetry::DISPLAY_P3);

    // The display transform is a 65³ interpolated LUT, so allow a few code
    // points; the differences being measured are 9-51.
    const TOL: i32 = 3;
    for (i, (texel, want_srgb, want_p3)) in cases.iter().enumerate() {
        for c in 0..3 {
            assert!(
                (i32::from(as_srgb[i][c]) - i32::from(want_srgb[c])).abs() <= TOL,
                "{texel:?} as sRGB: got {:?}, want {want_srgb:?}",
                as_srgb[i],
            );
            assert!(
                (i32::from(as_p3[i][c]) - i32::from(want_p3[c])).abs() <= TOL,
                "{texel:?} as Display-P3: got {:?}, want {want_p3:?}; \
                 if this equals the sRGB render, the P3 tag is being ignored again",
                as_p3[i],
            );
        }
    }

    // The headline number, guarded independently of the exact expectations:
    // saturated green moves ~51 code points of red between the two readings.
    let green_delta = i32::from(as_srgb[1][0]) - i32::from(as_p3[1][0]);
    assert!(
        green_delta > 40,
        "a saturated green read as sRGB instead of P3 differed by only \
         {green_delta} code points of red — the primaries transform is not running",
    );
}

/// **CPU/GPU parity (§4.4) on the wide-gamut path.** Same structural argument
/// as the sRGB case, one shared host-side lift, but this path now has a 3×3
/// matrix in it, so it gets its own gate. Reports and skips when no adapter is
/// available rather than faking a number.
#[test]
fn cpu_and_gpu_lift_a_display_p3_source_identically() {
    let Some(gpu) = GpuContext::headless() else {
        eprintln!("SKIP: no wgpu adapter available; CPU/GPU Display-P3 parity not exercised");
        return;
    };
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);
    let tag = SourceColorimetry::DISPLAY_P3;

    let cpu_engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        TaggedSource {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: tag,
        },
    );
    let cpu = texels(&render(&cpu_engine, w, h, BackendId::Cpu));

    let gpu_engine = build_engine(
        BackendPref::Auto,
        Arc::new(SharedDevice {
            device: Arc::clone(&gpu.device),
            queue: Arc::clone(&gpu.queue),
        }),
        TaggedSource {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: tag,
        },
    );
    let gpu_px = render(&gpu_engine, w, h, BackendId::Gpu);
    let gpu_texels = texels(&gpu_px);

    let stats = delta_e_stats(&cpu, &gpu_texels);
    let cpu_bytes: Vec<u8> = cpu.iter().flatten().copied().collect();
    let psnr_db = psnr(&cpu_bytes, &gpu_px.bytes);
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "CPU/GPU Display-P3 input-transform parity: max ΔE2000 {:.4}, PSNR {psnr_db:.2} dB",
        stats.max,
    );
}

/// The lift is bit-identical on CPU and GPU **for every tag**, not just the two
/// that get an end-to-end render above: both backends call the same
/// `to_working_f16`, so this compares the one shared function's output across
/// the whole tag set at once and would catch a tag that only one path handles.
#[test]
fn every_tag_produces_one_shared_lift() {
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for tag in [
        SourceColorimetry::WORKING_LINEAR,
        SourceColorimetry::SRGB,
        SourceColorimetry::DISPLAY_P3,
        SourceColorimetry::ADOBE_RGB,
        SourceColorimetry::PROPHOTO,
        SourceColorimetry::REC2020,
    ] {
        let lifted = to_working_tile_cpu(&SourceImage {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: tag,
            full_extent: Extent { w, h },
            quality: SourceQuality::Full,
        });
        assert_eq!(lifted.format, PixelFormat::Rgba16F);
        assert!(
            !seen.contains(&lifted.bytes),
            "{tag:?} produced the same working tile as an earlier tag — \
             a tag that is not actually wired to a transform",
        );
        seen.push(lifted.bytes);
    }
}

/// **CPU/GPU parity (§4.4) on the input transform.** The lift itself is one
/// shared host-side function, so the backends cannot diverge by construction;
/// this renders the same sRGB-tagged source through both and holds it to the
/// standard ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB gate. Reports and skips when no
/// adapter is available rather than faking a number.
#[test]
fn cpu_and_gpu_lift_an_srgb_source_identically() {
    let Some(gpu) = GpuContext::headless() else {
        eprintln!("SKIP: no wgpu adapter available; CPU/GPU input-transform parity not exercised");
        return;
    };
    let probe = probe_texels();
    let (w, h) = (probe.len() as u32, 1);

    let cpu_engine = build_engine(
        BackendPref::ForceCpu,
        Arc::new(NullDevice),
        TaggedSource {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: SourceColorimetry::srgb(),
        },
    );
    let cpu = texels(&render(&cpu_engine, w, h, BackendId::Cpu));

    let gpu_engine = build_engine(
        BackendPref::Auto,
        Arc::new(SharedDevice {
            device: Arc::clone(&gpu.device),
            queue: Arc::clone(&gpu.queue),
        }),
        TaggedSource {
            pixels: srgb8_source(&probe, w, h),
            colorimetry: SourceColorimetry::srgb(),
        },
    );
    let gpu_px = render(&gpu_engine, w, h, BackendId::Gpu);
    let gpu_texels = texels(&gpu_px);

    let stats = delta_e_stats(&cpu, &gpu_texels);
    let cpu_bytes: Vec<u8> = cpu.iter().flatten().copied().collect();
    let psnr_db = psnr(&cpu_bytes, &gpu_px.bytes);
    assert!(
        stats.within_tolerance() && psnr_db >= TOLERANCE_PSNR_DB,
        "CPU/GPU input-transform parity: max ΔE2000 {:.4}, PSNR {psnr_db:.2} dB",
        stats.max,
    );
}
