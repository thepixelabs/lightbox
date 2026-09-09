// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The per-image pixel-post path (spec §5.3 `lightbox-export::pixel`,
//! narrowed to the core slice's four stages): render → resize → output
//! color transform (+ ICC bytes) → basic sharpen. [`crate::encode`] takes
//! it from here.
//!
//! **Source-space note (named seam gap, recorded in
//! `docs/plan/epics/E15-deviations.md`).** The spec's §4.1 pipeline resizes
//! *linear* working-space pixels and converts to output space afterward.
//! At E15's kickoff, `lightbox-render`'s `RenderTarget::Buffer` readback
//! does not expose that buffer yet, the M1 graph's terminal node
//! (`xform.display`) always bakes working→built-in-sRGB before the CPU
//! readback happens (see `lightbox-render/src/ng/nodes/display.rs` and
//! `lightbox-color`'s `output::SourceSpace::SrgbDisplay`, added for this
//! epic). So this module's "render" stage already yields sRGB
//! gamma-encoded pixels, and the "output color transform" stage below
//! declares its source as `SourceSpace::SrgbDisplay` rather than
//! `WorkingLinear`, one real LCMS2 transform either way, just fed the
//! profile that matches what the engine actually hands back today. Once a
//! true linear export buffer lands in `lightbox-render`, only this
//! module's source-space choice needs to change.

use std::time::{Duration, Instant};

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use lightbox_color::output::{
    BitDepth as ColorBitDepth, OutputBuffer, OutputTransform, SourceSpace,
};
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    Engine, Extent, OutFormat, OutputPayload, ProcessVersion, RenderPriority, RenderRequest,
    RenderScale, RenderState, RenderTarget, Roi,
};
use lightbox_types::ImageId;

use crate::error::ExportError;
use crate::settings::{BitDepth, OutputColorSpace, SharpenAmount, SizeRule};

/// How long a single image's render may take before the pipeline gives up.
/// Generous: the spec's device-lost CPU fallback is minutes-scale (§4.4),
/// not seconds, this is a "genuinely stuck" backstop, not a latency
/// budget.
const RENDER_TIMEOUT: Duration = Duration::from_secs(300);

// ─── [1] render ─────────────────────────────────────────────────────────────

/// Renders `image` under `recipe` through the shared engine's `Buffer`
/// target (spec §5.8 seam), at an approximate fit for `rule` (the exact
/// target dimensions are hit by [`resize`] afterward, rendering close to
/// the final size first avoids paying full-native-resolution CPU cost for a
/// small export). Blocks the calling thread polling the render ticket;
/// callers run this on a blocking pool (spec §4.2 render lane).
///
/// Returns interleaved RGBA8, sRGB-encoded pixels (see the module doc
/// comment) plus their width/height.
pub fn render_export_pixels(
    engine: &Engine,
    image: ImageId,
    recipe: Recipe,
    pv: ProcessVersion,
    full_extent: Extent,
    rule: SizeRule,
    cancel: CancelToken,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let scale = match rule {
        SizeRule::None => RenderScale::OneToOne,
        SizeRule::LongEdge { px } => RenderScale::Fit(Extent { w: px, h: px }),
        SizeRule::Dimensions { w, h } => RenderScale::Fit(Extent { w, h }),
    };
    let resolution = lightbox_render::ng::exec::scale::derive(scale, full_extent);

    let ticket = engine.submit(RenderRequest {
        image,
        recipe,
        pv,
        roi: Roi {
            x: 0,
            y: 0,
            w: resolution.extent.w,
            h: resolution.extent.h,
        },
        scale,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Batch,
        cancel: cancel.clone(),
    });

    let deadline = Instant::now() + RENDER_TIMEOUT;
    loop {
        if cancel.is_cancelled() {
            engine.cancel(&ticket);
            return Err(ExportError::Cancelled);
        }
        match engine.poll(&ticket) {
            RenderState::Queued | RenderState::Rendering { .. } => {
                if Instant::now() > deadline {
                    engine.cancel(&ticket);
                    return Err(ExportError::RenderTimeout);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            RenderState::Complete(out) | RenderState::PreviewReady(out) => match out.payload {
                OutputPayload::Pixels(px) => return Ok((px.bytes, px.extent.w, px.extent.h)),
                OutputPayload::CanvasGeneration(_) => {
                    return Err(ExportError::Render(
                        "engine returned a canvas generation for a Buffer request (bug)".to_owned(),
                    ));
                }
            },
            RenderState::Failed(err) => return Err(ExportError::Render(err.to_string())),
            RenderState::Cancelled => return Err(ExportError::Cancelled),
        }
    }
}

// ─── [2] resize ─────────────────────────────────────────────────────────────

/// Drops the alpha channel (the render output is always opaque; no
/// exported format in the core slice carries transparency).
fn drop_alpha(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let n = (w as usize).saturating_mul(h as usize);
    let mut out = Vec::with_capacity(n * 3);
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[0..3]);
    }
    out
}

/// The exact output dimensions `rule` implies for a `w`×`h` source.
/// Aspect-preserving, **never upscales** (matches `lightbox-preview`'s
/// existing resize convention, the spec's `dont_enlarge` guard is
/// unconditional here rather than a toggle, see `settings`'s module doc).
#[must_use]
pub fn target_dims(w: u32, h: u32, rule: SizeRule) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (w, h);
    }
    match rule {
        SizeRule::None => (w, h),
        SizeRule::LongEdge { px } => {
            let long = w.max(h);
            if px == 0 || long <= px {
                return (w, h);
            }
            let scale = f64::from(px) / f64::from(long);
            (
                ((f64::from(w) * scale).round() as u32).max(1),
                ((f64::from(h) * scale).round() as u32).max(1),
            )
        }
        SizeRule::Dimensions { w: tw, h: th } => {
            if tw == 0 || th == 0 {
                return (w, h);
            }
            let scale = (f64::from(tw) / f64::from(w)).min(f64::from(th) / f64::from(h));
            if scale >= 1.0 {
                return (w, h);
            }
            (
                ((f64::from(w) * scale).round() as u32).max(1),
                ((f64::from(h) * scale).round() as u32).max(1),
            )
        }
    }
}

/// Resizes RGBA8 render output to `rule`'s exact target (Lanczos3 via
/// `fast_image_resize`, the same "good filter" `lightbox-preview`'s T1
/// pipeline uses, spec §5.3 `resize`). Drops alpha on the way in (see
/// [`drop_alpha`]); a `SizeRule::None`/no-op case is a byte-identical
/// passthrough after the alpha drop.
pub fn resize(
    rgba: &[u8],
    w: u32,
    h: u32,
    rule: SizeRule,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let rgb = drop_alpha(rgba, w, h);
    let (tw, th) = target_dims(w, h, rule);
    if (tw, th) == (w, h) {
        return Ok((rgb, w, h));
    }

    let src = Image::from_vec_u8(w, h, rgb, PixelType::U8x3)
        .map_err(|e| ExportError::Resize(format!("resize source: {e}")))?;
    let mut dst = Image::new(tw, th, PixelType::U8x3);
    Resizer::new()
        .resize(
            &src,
            &mut dst,
            &ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
                .use_alpha(false),
        )
        .map_err(|e| ExportError::Resize(e.to_string()))?;
    Ok((dst.into_vec(), tw, th))
}

// ─── [3] output color transform ────────────────────────────────────────────

/// Working(-as-rendered)→output space conversion + ICC embed bytes (spec
/// §5.3 stage 2 / §5.7 seam). Computed at `F32` internally regardless of
/// the final container bit depth so [`sharpen_in_place`] (stage 3) and
/// [`quantize`] (stage 4) both operate on the same precision, quantizing
/// once at the very end (spec §5.3 `quantize`, narrowed: no
/// error-diffusion dither at core-slice scope, see `E15-deviations.md`).
pub fn apply_output_color(
    rgb8: &[u8],
    space: OutputColorSpace,
) -> Result<(Vec<f32>, Vec<u8>), ExportError> {
    let ot = OutputTransform::new(
        SourceSpace::SrgbDisplay,
        space.to_color_space(),
        ColorBitDepth::F32,
        OutputColorSpace::intent(),
    )
    .map_err(|e| ExportError::Color(e.to_string()))?;

    let input: Vec<f32> = rgb8.iter().map(|&b| f32::from(b) / 255.0).collect();
    let mut out = OutputBuffer::F32(Vec::new());
    ot.apply(&input, &mut out);
    let OutputBuffer::F32(rgb_f32) = out else {
        unreachable!("OutputTransform built at BitDepth::F32 always yields OutputBuffer::F32")
    };
    Ok((rgb_f32, ot.profile_bytes().to_vec()))
}

// ─── [4] basic output sharpening ───────────────────────────────────────────

/// `(radius_px, amount)` for a luma-weighted unsharp mask, keyed by
/// [`SharpenAmount`] (spec §5.3 `sharpen_output`'s medium×strength table,
/// narrowed to amount only, see `settings::OutputSharpen`'s doc comment).
fn sharpen_params(amount: SharpenAmount) -> (f32, f32) {
    match amount {
        SharpenAmount::Low => (0.6, 0.35),
        SharpenAmount::Standard => (0.8, 0.6),
        SharpenAmount::High => (1.0, 0.9),
    }
}

/// A basic luma-weighted unsharp mask on output-referred RGB f32 (spec
/// §5.3 `sharpen_output`, narrowed: fixed 3×3-box "blur" instead of a true
/// Gaussian, cheap, good enough at export-typical radii, no new
/// dependency). Operates in place, post output-transform (matches the
/// spec's stage ordering rationale, §4.1: sharpening is tuned for
/// output-referred data at final resolution).
pub fn sharpen_in_place(rgb: &mut [f32], w: u32, h: u32, amount: SharpenAmount) {
    if w == 0 || h == 0 {
        return;
    }
    let (radius, strength) = sharpen_params(amount);
    // Radius is expressed as a fraction of a 3x3 neighborhood weight; a
    // wider "radius" setting simply raises the blend against the box blur
    // (cheap stand-in for a variable-radius Gaussian at this scope).
    let taps = radius.clamp(0.1, 2.0);

    let (w, h) = (w as usize, h as usize);
    let blurred = box_blur3(rgb, w, h);
    for i in 0..(w * h) {
        for c in 0..3 {
            let idx = i * 3 + c;
            let orig = rgb[idx];
            let blur = blurred[idx];
            let hi_pass = (orig - blur) * strength * taps;
            rgb[idx] = (orig + hi_pass).clamp(0.0, 1.0);
        }
    }
}

/// A separable 3×3 box blur over interleaved RGB f32, edge-clamped.
fn box_blur3(rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; rgb.len()];
    let at = |x: i64, y: i64, c: usize| -> f32 {
        let cx = x.clamp(0, w as i64 - 1) as usize;
        let cy = y.clamp(0, h as i64 - 1) as usize;
        rgb[(cy * w + cx) * 3 + c]
    };
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            for c in 0..3 {
                let mut sum = 0.0f32;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        sum += at(x + dx, y + dy, c);
                    }
                }
                out[(y as usize * w + x as usize) * 3 + c] = sum / 9.0;
            }
        }
    }
    out
}

// ─── [5] quantize ───────────────────────────────────────────────────────────

/// The encoder-ready pixel buffer, at whichever [`BitDepth`] the format
/// asked for (spec §5.3 `quantize`, narrowed: simple round-to-nearest, no
/// error-diffusion dither at core-slice scope, see `E15-deviations.md`).
#[derive(Clone, Debug, PartialEq)]
pub enum QuantizedPixels {
    /// 8 bits per channel.
    U8(Vec<u8>),
    /// 16 bits per channel.
    U16(Vec<u16>),
}

/// Quantizes output-referred RGB f32 (0.0..=1.0, already clamped by
/// [`sharpen_in_place`]/[`apply_output_color`]) to the container's bit
/// depth.
#[must_use]
pub fn quantize(rgb: &[f32], depth: BitDepth) -> QuantizedPixels {
    match depth {
        BitDepth::Eight => QuantizedPixels::U8(
            rgb.iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect(),
        ),
        BitDepth::Sixteen => QuantizedPixels::U16(
            rgb.iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dims_long_edge_preserves_aspect_and_never_upscales() {
        assert_eq!(
            target_dims(4000, 3000, SizeRule::LongEdge { px: 2000 }),
            (2000, 1500)
        );
        assert_eq!(
            target_dims(3000, 4000, SizeRule::LongEdge { px: 2000 }),
            (1500, 2000)
        );
        // Never upscales.
        assert_eq!(
            target_dims(800, 600, SizeRule::LongEdge { px: 4000 }),
            (800, 600)
        );
        assert_eq!(target_dims(800, 600, SizeRule::None), (800, 600));
    }

    #[test]
    fn target_dims_dimensions_fits_within_box() {
        assert_eq!(
            target_dims(4000, 2000, SizeRule::Dimensions { w: 1000, h: 1000 }),
            (1000, 500)
        );
        assert_eq!(
            target_dims(2000, 4000, SizeRule::Dimensions { w: 1000, h: 1000 }),
            (500, 1000)
        );
        // Never upscales even if the box is bigger than the source.
        assert_eq!(
            target_dims(400, 300, SizeRule::Dimensions { w: 4000, h: 4000 }),
            (400, 300)
        );
    }

    #[test]
    fn drop_alpha_keeps_only_rgb() {
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 128];
        assert_eq!(drop_alpha(&rgba, 2, 1), vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn resize_none_rule_is_alpha_dropped_passthrough() {
        let rgba = vec![
            10, 20, 30, 255, 40, 50, 60, 128, 70, 80, 90, 200, 1, 2, 3, 4,
        ];
        let (rgb, w, h) = resize(&rgba, 2, 2, SizeRule::None).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgb, vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 1, 2, 3]);
    }

    #[test]
    fn resize_downscales_to_the_exact_target() {
        let w = 64u32;
        let h = 32u32;
        let rgba: Vec<u8> = (0..(w * h))
            .flat_map(|i| {
                let v = (i % 255) as u8;
                [v, v, v, 255]
            })
            .collect();
        let (rgb, tw, th) = resize(&rgba, w, h, SizeRule::LongEdge { px: 16 }).unwrap();
        assert_eq!((tw, th), (16, 8));
        assert_eq!(rgb.len(), (tw * th * 3) as usize);
    }

    #[test]
    fn output_color_srgb_round_trips_near_identity_for_in_gamut() {
        // sRGB source → sRGB destination should be close to the identity
        // (small LCMS2 quantization tolerance) for an in-gamut mid gray.
        let rgb8 = vec![128u8, 128, 128, 255, 0, 0];
        let (rgb_f32, icc) = apply_output_color(&rgb8, OutputColorSpace::Srgb).unwrap();
        assert!(!icc.is_empty(), "ICC profile bytes must be emitted");
        assert_eq!(rgb_f32.len(), 6);
        for (i, &b) in rgb8.iter().enumerate() {
            let expect = f32::from(b) / 255.0;
            assert!(
                (rgb_f32[i] - expect).abs() < 0.02,
                "channel {i}: got {}, want ~{expect}",
                rgb_f32[i]
            );
        }
    }

    #[test]
    fn output_color_display_p3_and_adobe_rgb_build_and_emit_icc() {
        let rgb8 = vec![200u8, 100, 50];
        for space in [OutputColorSpace::DisplayP3, OutputColorSpace::AdobeRgb] {
            let (rgb_f32, icc) = apply_output_color(&rgb8, space).unwrap();
            assert_eq!(rgb_f32.len(), 3);
            assert!(!icc.is_empty());
        }
    }

    #[test]
    fn sharpen_leaves_flat_regions_unchanged() {
        let w = 4u32;
        let h = 4u32;
        let mut rgb = vec![0.5f32; (w * h * 3) as usize];
        let before = rgb.clone();
        sharpen_in_place(&mut rgb, w, h, SharpenAmount::Standard);
        for (a, b) in rgb.iter().zip(before.iter()) {
            assert!(
                (a - b).abs() < 1e-5,
                "flat region must not move: {a} vs {b}"
            );
        }
    }

    #[test]
    fn sharpen_increases_local_contrast_at_an_edge() {
        // A step edge at x=2 in a small image; the sharpened pixel just
        // left of the edge should get pushed darker (or stay clamped),
        // and just right of the edge brighter, a real contrast boost.
        let w = 6u32;
        let h = 1u32;
        let mut rgb = vec![0.0f32; (w * h * 3) as usize];
        for x in 0..w {
            let v = if x < 3 { 0.2 } else { 0.8 };
            for c in 0..3 {
                rgb[(x * 3 + c) as usize] = v;
            }
        }
        let before = rgb.clone();
        sharpen_in_place(&mut rgb, w, h, SharpenAmount::High);
        // Pixel at x=2 (last "dark" pixel, adjacent to the edge) should be
        // pushed darker than before (overshoot away from the blur).
        assert!(rgb[2 * 3] <= before[2 * 3] + 1e-6);
        // Pixel at x=3 (first "bright" pixel) should be pushed brighter.
        assert!(rgb[3 * 3] >= before[3 * 3] - 1e-6);
    }

    #[test]
    fn quantize_eight_and_sixteen_hit_the_extremes() {
        let rgb = vec![0.0f32, 0.5, 1.0, -0.1, 1.1];
        match quantize(&rgb, BitDepth::Eight) {
            QuantizedPixels::U8(v) => assert_eq!(v, vec![0, 128, 255, 0, 255]),
            QuantizedPixels::U16(_) => panic!("wrong variant"),
        }
        match quantize(&rgb, BitDepth::Sixteen) {
            QuantizedPixels::U16(v) => assert_eq!(v, vec![0, 32768, 65535, 0, 65535]),
            QuantizedPixels::U8(_) => panic!("wrong variant"),
        }
    }
}
