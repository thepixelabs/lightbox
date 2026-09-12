// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Text watermarking (spec §5.11), burnt into the exported pixels.
//!
//! # What the type actually looks like
//!
//! There is **no font file and no font rasteriser here**, and that is a
//! deliberate, visible trade. `lightbox-export`'s dependency set is JPEG,
//! PNG, TIFF, resize and EXIF; adding a TrueType rasteriser would also mean
//! shipping a typeface, and `deny.toml` treats bundled font data as its own
//! license surface (see its `epaint_default_fonts` exception, the only
//! `OFL-1.1`/`Ubuntu-font-1.0` entry in the file). So [`font`] is a
//! **single-stroke geometric font written as line segments in this
//! repository**, in the family of an engraving or plotter font:
//!
//! - every glyph is one or more polylines on a 14-unit cap-height grid,
//!   stroked at a constant width, so there are **no thick/thin contrasts,
//!   no true curves** (round letters are 8 to 10 segment polygons) and
//!   **no kerning** (advance widths are per glyph, letter spacing is
//!   constant),
//! - it covers ASCII 0x20..=0x7E plus `©`, `°` and `~`. Anything else is
//!   drawn as an empty box, the same "tofu" a real font shows for a
//!   missing glyph, rather than silently vanishing,
//! - at a normal watermark size (a few percent of the short edge) it reads
//!   as a clean, slightly technical sans, legible down to about 8 pixels of
//!   cap height. Blown up to a quarter of the frame the polygon corners on
//!   the round and compound glyphs (`O`, `S`, `8`, `@`, `&`) are plainly
//!   visible. It is a signature line, not a title card.
//!
//! Glyphs are anti-aliased by exact distance-to-segment coverage, so edges
//! are smooth at any scale and any rotation-free position.
//!
//! # Graphical (PNG) watermarks are not built
//!
//! They would need an image decoder in this crate. That is a real feature
//! and a reasonable extension, but it is not this one, and it is named
//! here rather than implied by omission.
//!
//! # Where this sits in the pipeline
//!
//! [`crate::run::export_one`] calls [`apply`] **after** the output color
//! transform and output sharpening and **before** quantization
//! (`crate::run`'s stage list). That ordering is deliberate:
//! - after the color transform, so the requested [`Watermark::color`] is
//!   the color that lands in the file rather than something the transform
//!   moves,
//! - after sharpening, so the watermark does not get an unsharp halo of
//!   its own,
//! - before quantization, so the blend happens at f32 and an 8-bit export
//!   rounds once.
//!
//! Compositing is straight alpha in the output space's own (non-linear)
//! encoding, which is what an image editor's overlay does and what makes a
//! 70%-opacity watermark look like 70%.

pub mod font;

use crate::settings::Watermark;

/// Halo alpha as a fraction of the watermark's own opacity.
const HALO_ALPHA: f32 = 0.55;
/// Stroke half-width as a fraction of the cap height.
const STROKE_RATIO: f32 = 0.075;
/// Extra half-width the halo pass adds, as a fraction of the cap height.
const HALO_RATIO: f32 = 0.075;

/// One glyph placed in image space: the polylines of [`font::Glyph`] with
/// the layout transform already applied.
struct Placed {
    /// Pixel-space polylines.
    strokes: Vec<Vec<(f32, f32)>>,
}

/// The measured extent of a laid-out text block, in font grid units.
struct Measured {
    /// Per line, its width in grid units.
    line_widths: Vec<f32>,
    /// The widest line.
    width: f32,
    /// Baseline-to-cap block height: `(lines - 1) * LINE_HEIGHT + CAP`.
    height: f32,
}

fn measure(lines: &[&str]) -> Measured {
    let line_widths: Vec<f32> = lines.iter().map(|l| font::line_width(l)).collect();
    let width = line_widths.iter().copied().fold(0.0f32, f32::max);
    let height = (lines.len().saturating_sub(1)) as f32 * font::LINE_HEIGHT + font::CAP_HEIGHT;
    Measured {
        line_widths,
        width,
        height,
    }
}

/// The halo color for `color`: black behind light text, white behind dark.
fn halo_color(color: [u8; 3]) -> [f32; 3] {
    let luma = (0.2126 * f32::from(color[0])
        + 0.7152 * f32::from(color[1])
        + 0.0722 * f32::from(color[2]))
        / 255.0;
    if luma > 0.5 {
        [0.0, 0.0, 0.0]
    } else {
        [1.0, 1.0, 1.0]
    }
}

/// Burns `wm` into `rgb` (interleaved RGB f32, output-referred, 0.0..=1.0)
/// in place.
///
/// A no-op when the image is empty, the text is blank, or the requested
/// geometry leaves no room to draw. Never panics and never fails: a
/// watermark that cannot be placed is simply not drawn, because failing an
/// otherwise-good export over a decoration would be the wrong trade.
/// [`crate::settings::ExportSettings::validate`] is what rejects nonsense
/// knobs, before any pixels move.
pub fn apply(rgb: &mut [f32], w: u32, h: u32, wm: &Watermark) {
    if w == 0 || h == 0 || rgb.len() < (w as usize * h as usize * 3) {
        return;
    }
    let lines: Vec<&str> = wm.text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return;
    }
    let measured = measure(&lines);
    if measured.width <= 0.0 || measured.height <= 0.0 {
        return;
    }

    let (iw, ih) = (w as f32, h as f32);
    let short_edge = iw.min(ih);
    let inset = (wm.inset.max(0.0) * short_edge).min(short_edge * 0.45);
    let avail_w = (iw - 2.0 * inset).max(1.0);
    let avail_h = (ih - 2.0 * inset).max(1.0);

    // Requested size, then shrunk to fit if the text would not otherwise
    // sit inside the insets.
    let mut scale = (wm.size * short_edge) / font::CAP_HEIGHT;
    scale = scale
        .min(avail_w / measured.width)
        .min(avail_h / measured.height);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }

    let block_w = measured.width * scale;
    let block_h = measured.height * scale;
    let (fx, fy) = wm.anchor.fractions();
    let origin_x = inset + (avail_w - block_w) * fx;
    let origin_y = inset + (avail_h - block_h) * fy;

    // Lines align with the anchor: a right-anchored block right-aligns.
    let mut placed = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line_x = origin_x + (block_w - measured.line_widths[i] * scale) * fx;
        let baseline_y = origin_y + (i as f32 * font::LINE_HEIGHT + font::CAP_HEIGHT) * scale;
        placed.push(place_line(line, line_x, baseline_y, scale));
    }

    let cap_px = font::CAP_HEIGHT * scale;
    let half = (cap_px * STROKE_RATIO).max(0.5);
    let halo_half = half + (cap_px * HALO_RATIO).max(0.6);

    let pad = if wm.halo { halo_half } else { half } + 1.5;
    let Some(bounds) = pixel_bounds(&placed, pad, w, h) else {
        return;
    };

    if wm.halo {
        let cov = rasterize(&placed, &bounds, halo_half);
        composite(
            rgb,
            w,
            &bounds,
            &cov,
            halo_color(wm.color),
            wm.opacity * HALO_ALPHA,
        );
    }
    let cov = rasterize(&placed, &bounds, half);
    let color = [
        f32::from(wm.color[0]) / 255.0,
        f32::from(wm.color[1]) / 255.0,
        f32::from(wm.color[2]) / 255.0,
    ];
    composite(rgb, w, &bounds, &cov, color, wm.opacity);
}

fn place_line(line: &str, mut pen_x: f32, baseline_y: f32, scale: f32) -> Placed {
    let mut strokes = Vec::new();
    for c in line.chars() {
        let glyph = font::glyph(c);
        for stroke in glyph.strokes {
            let mapped: Vec<(f32, f32)> = stroke
                .iter()
                .map(|&(gx, gy)| {
                    (
                        pen_x + f32::from(gx) * scale,
                        baseline_y - f32::from(gy) * scale,
                    )
                })
                .collect();
            if !mapped.is_empty() {
                strokes.push(mapped);
            }
        }
        pen_x += (f32::from(glyph.advance) + font::LETTER_SPACING) * scale;
    }
    Placed { strokes }
}

/// The integer pixel rectangle the placed text touches, expanded by `pad`
/// and clipped to the image. `None` when nothing lands on the image.
struct Bounds {
    x0: usize,
    y0: usize,
    bw: usize,
    bh: usize,
}

fn pixel_bounds(placed: &[Placed], pad: f32, w: u32, h: u32) -> Option<Bounds> {
    let (mut minx, mut miny) = (f32::INFINITY, f32::INFINITY);
    let (mut maxx, mut maxy) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in placed {
        for stroke in &p.strokes {
            for &(x, y) in stroke {
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
    }
    if !minx.is_finite() || !maxx.is_finite() {
        return None;
    }
    let x0 = ((minx - pad).floor().max(0.0) as usize).min(w as usize);
    let y0 = ((miny - pad).floor().max(0.0) as usize).min(h as usize);
    let x1 = ((maxx + pad).ceil().max(0.0) as usize + 1).min(w as usize);
    let y1 = ((maxy + pad).ceil().max(0.0) as usize + 1).min(h as usize);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Bounds {
        x0,
        y0,
        bw: x1 - x0,
        bh: y1 - y0,
    })
}

/// Exact distance from `p` to the segment `a`..`b`. A zero-length segment
/// (how the font stores a dot, e.g. the one over `i`) degenerates to the
/// distance to the point, which is what makes a round dot fall out for
/// free.
fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a.0 + t * vx, a.1 + t * vy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// Anti-aliased coverage in 0.0..=1.0 for every pixel of `bounds`, from
/// every stroke at half-width `half`. Strokes are unioned with `max`, so
/// overlapping segments inside one glyph do not build up density.
fn rasterize(placed: &[Placed], bounds: &Bounds, half: f32) -> Vec<f32> {
    let mut cov = vec![0.0f32; bounds.bw * bounds.bh];
    let reach = half + 1.0;
    for p in placed {
        for stroke in &p.strokes {
            let pairs: Vec<((f32, f32), (f32, f32))> = if stroke.len() == 1 {
                vec![(stroke[0], stroke[0])]
            } else {
                stroke.windows(2).map(|s| (s[0], s[1])).collect()
            };
            for (a, b) in pairs {
                let lox = ((a.0.min(b.0) - reach).floor().max(bounds.x0 as f32)) as usize;
                let hix = ((a.0.max(b.0) + reach).ceil()).max(0.0) as usize;
                let loy = ((a.1.min(b.1) - reach).floor().max(bounds.y0 as f32)) as usize;
                let hiy = ((a.1.max(b.1) + reach).ceil()).max(0.0) as usize;
                let hix = hix.min(bounds.x0 + bounds.bw - 1);
                let hiy = hiy.min(bounds.y0 + bounds.bh - 1);
                if lox > hix || loy > hiy {
                    continue;
                }
                for y in loy..=hiy {
                    for x in lox..=hix {
                        let d = distance_to_segment((x as f32 + 0.5, y as f32 + 0.5), a, b);
                        let alpha = (half + 0.5 - d).clamp(0.0, 1.0);
                        if alpha > 0.0 {
                            let i = (y - bounds.y0) * bounds.bw + (x - bounds.x0);
                            cov[i] = cov[i].max(alpha);
                        }
                    }
                }
            }
        }
    }
    cov
}

/// Straight-alpha `src over dst` for one coverage pass.
fn composite(
    rgb: &mut [f32],
    image_w: u32,
    bounds: &Bounds,
    cov: &[f32],
    color: [f32; 3],
    opacity: f32,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    for by in 0..bounds.bh {
        for bx in 0..bounds.bw {
            let a = cov[by * bounds.bw + bx] * opacity;
            if a <= 0.0 {
                continue;
            }
            let idx = ((bounds.y0 + by) * image_w as usize + (bounds.x0 + bx)) * 3;
            for (c, &channel) in color.iter().enumerate() {
                let dst = rgb[idx + c];
                rgb[idx + c] = (dst * (1.0 - a) + channel * a).clamp(0.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::WatermarkAnchor;

    fn black(w: u32, h: u32) -> Vec<f32> {
        vec![0.0f32; (w * h * 3) as usize]
    }

    /// Mean coverage of one quadrant, as a crude "is there ink here".
    fn quadrant_ink(rgb: &[f32], w: u32, h: u32, right: bool, bottom: bool) -> f32 {
        let (w, h) = (w as usize, h as usize);
        let xs = if right { w / 2..w } else { 0..w / 2 };
        let ys = if bottom { h / 2..h } else { 0..h / 2 };
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in ys {
            for x in xs.clone() {
                sum += rgb[(y * w + x) * 3];
                n += 1.0;
            }
        }
        sum / n
    }

    #[test]
    fn a_watermark_puts_ink_on_the_image() {
        let (w, h) = (400u32, 300u32);
        let mut rgb = black(w, h);
        apply(&mut rgb, w, h, &Watermark::text("Lightbox"));
        assert!(
            rgb.iter().any(|&v| v > 0.1),
            "nothing was drawn on a black image"
        );
    }

    #[test]
    fn every_anchor_puts_the_ink_in_its_own_corner() {
        use WatermarkAnchor as A;
        let (w, h) = (400u32, 400u32);
        let cases = [
            (A::TopLeft, false, false),
            (A::TopRight, true, false),
            (A::BottomLeft, false, true),
            (A::BottomRight, true, true),
        ];
        for (anchor, right, bottom) in cases {
            let mut rgb = black(w, h);
            apply(
                &mut rgb,
                w,
                h,
                &Watermark {
                    anchor,
                    ..Watermark::text("Ag")
                },
            );
            let here = quadrant_ink(&rgb, w, h, right, bottom);
            let opposite = quadrant_ink(&rgb, w, h, !right, !bottom);
            assert!(
                here > opposite,
                "{anchor:?}: ink {here} in its own quadrant vs {opposite} opposite"
            );
            assert!(here > 0.0, "{anchor:?}: no ink at all");
        }
    }

    #[test]
    fn center_anchor_stays_off_all_four_corners() {
        let (w, h) = (400u32, 400u32);
        let mut rgb = black(w, h);
        apply(
            &mut rgb,
            w,
            h,
            &Watermark {
                anchor: WatermarkAnchor::Center,
                size: 0.05,
                ..Watermark::text("X")
            },
        );
        for (x, y) in [(2usize, 2usize), (397, 2), (2, 397), (397, 397)] {
            let v = rgb[(y * w as usize + x) * 3];
            assert!(v < 1e-6, "corner ({x},{y}) should be untouched, got {v}");
        }
    }

    #[test]
    fn opacity_scales_the_ink_and_zero_opacity_is_a_no_op() {
        let (w, h) = (300u32, 200u32);
        let ink_at = |opacity: f32| {
            let mut rgb = black(w, h);
            apply(
                &mut rgb,
                w,
                h,
                &Watermark {
                    opacity,
                    halo: false,
                    ..Watermark::text("MMMM")
                },
            );
            rgb.iter().sum::<f32>()
        };
        let full = ink_at(1.0);
        let half = ink_at(0.5);
        assert!(full > 0.0);
        assert!(
            (half / full - 0.5).abs() < 0.02,
            "0.5 opacity should be half the ink: {half} vs {full}"
        );
        assert_eq!(ink_at(0.0), 0.0, "zero opacity must not touch a pixel");
    }

    #[test]
    fn oversized_text_is_shrunk_to_fit_instead_of_overflowing() {
        // size 1.0 asks for a cap height of the whole short edge; the run
        // is far wider than the image, so it must be scaled down and stay
        // inside the frame.
        let (w, h) = (200u32, 200u32);
        let mut rgb = black(w, h);
        apply(
            &mut rgb,
            w,
            h,
            &Watermark {
                size: 1.0,
                inset: 0.05,
                halo: false,
                anchor: WatermarkAnchor::Center,
                ..Watermark::text("A very long watermark line")
            },
        );
        // The outermost ring of pixels must be untouched: the inset is 10px.
        for x in 0..w as usize {
            for y in [0usize, 1, 198, 199] {
                let v = rgb[(y * w as usize + x) * 3];
                assert!(v < 1e-6, "row {y} col {x} overflowed the inset: {v}");
            }
        }
        assert!(rgb.iter().any(|&v| v > 0.05), "but something was drawn");
    }

    #[test]
    fn blank_text_and_degenerate_images_are_no_ops() {
        let mut rgb = black(50, 50);
        let before = rgb.clone();
        apply(&mut rgb, 50, 50, &Watermark::text("   \n  "));
        assert_eq!(rgb, before);

        let mut empty: Vec<f32> = Vec::new();
        apply(&mut empty, 0, 0, &Watermark::text("x"));
        assert!(empty.is_empty());

        // A buffer smaller than w*h*3 is refused rather than indexed.
        let mut short = vec![0.0f32; 10];
        let before = short.clone();
        apply(&mut short, 100, 100, &Watermark::text("x"));
        assert_eq!(short, before);
    }

    #[test]
    fn the_halo_widens_the_mark_and_uses_the_contrasting_color() {
        assert_eq!(halo_color([255, 255, 255]), [0.0, 0.0, 0.0]);
        assert_eq!(halo_color([0, 0, 0]), [1.0, 1.0, 1.0]);

        // On a mid-grey image a haloed white mark touches strictly more
        // pixels than an unhaloed one.
        let (w, h) = (300u32, 200u32);
        let touched = |halo: bool| {
            let mut rgb = vec![0.5f32; (w * h * 3) as usize];
            apply(
                &mut rgb,
                w,
                h,
                &Watermark {
                    halo,
                    ..Watermark::text("Lightbox")
                },
            );
            rgb.iter().filter(|&&v| (v - 0.5).abs() > 1e-6).count()
        };
        assert!(touched(true) > touched(false));
    }

    #[test]
    fn multiple_lines_stack_downwards() {
        let (w, h) = (400u32, 400u32);
        let one = {
            let mut rgb = black(w, h);
            apply(
                &mut rgb,
                w,
                h,
                &Watermark {
                    anchor: WatermarkAnchor::TopLeft,
                    ..Watermark::text("Wide")
                },
            );
            rgb
        };
        let two = {
            let mut rgb = black(w, h);
            apply(
                &mut rgb,
                w,
                h,
                &Watermark {
                    anchor: WatermarkAnchor::TopLeft,
                    ..Watermark::text("Wide\nWide")
                },
            );
            rgb
        };
        let lowest = |rgb: &[f32]| {
            (0..h as usize)
                .rev()
                .find(|&y| (0..w as usize).any(|x| rgb[(y * w as usize + x) * 3] > 0.05))
        };
        assert!(
            lowest(&two) > lowest(&one),
            "the second line must sit below"
        );
    }

    #[test]
    fn a_dot_only_glyph_still_renders() {
        // '.' is stored as a zero-length segment; the distance field must
        // still produce a mark rather than dividing by zero.
        let (w, h) = (120u32, 120u32);
        let mut rgb = black(w, h);
        apply(
            &mut rgb,
            w,
            h,
            &Watermark {
                size: 0.3,
                halo: false,
                ..Watermark::text(".")
            },
        );
        assert!(rgb.iter().any(|&v| v > 0.5), "the dot did not render");
    }
}
