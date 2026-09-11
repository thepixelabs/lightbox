// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The app icon, **drawn, not shipped as an asset**.
//!
//! ## The mark
//!
//! One photograph, cut in two. To the left of the cut it is the file as
//! it came off the sensor: flat, desaturated, blacks lifted, carrying the
//! cool cast of an unprocessed raw. To the right it is the same pixels
//! graded: cool shadows, a warm highlight, the full range opened up.
//! The two halves are the same field of light sampled through two
//! different treatments, which is the entire job of the application in
//! one shape.
//!
//! The cut is the mark's signature: a leaning gap that breaks the
//! photograph's top and bottom edges, so the silhouette is notched and
//! recognisable at 16px, where a difference in colour alone would not be.
//!
//! ## The same mark at two sizes
//!
//! The website draws this in a 32x32 viewBox in a single ink colour. It
//! cannot show two treatments of one photograph in one colour, so there
//! it says the same thing with weight: the raw piece is an outline, the
//! graded piece is solid. **The geometry is identical.** Every number in
//! [`mark`] below is in that same 32-unit space, so the icon and the
//! website mark are the same drawing with different fills, and a change
//! to one is a change you can make to the other by hand.
//!
//! ## Why generate instead of committing PNGs
//!
//! A committed icon is a binary blob that needs its own REUSE entry and
//! drifts from the in-app mark. This is arithmetic with no dependency
//! beyond the `png` encoder the workspace already uses, and it is exactly
//! reproducible: same input, same bytes (asserted).
//!
//! **Anti-aliasing** is 4x supersampling plus a box downsample rather than
//! analytic coverage. At icon sizes the difference is invisible and the
//! code stays something you can read in one sitting.

use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result};

/// Supersampling factor. 4x is 16 samples per output pixel, clean edges
/// on the 16px icon, and even the 1024px master renders in milliseconds.
const SS: u32 = 4;

/// The artwork's margin, as a fraction of the canvas: how far in from the
/// edge the squircle starts. Apple's own macOS grid insets the shape by
/// 0.0977 a side; this icon deliberately runs closer to the edge.
const MARGIN: f32 = 0.02;

/// Linear RGB, 0..=1. Everything composites in linear light: an 8-bit sRGB
/// lerp across a grade this wide bands visibly.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Rgb(f32, f32, f32);

impl Rgb {
    /// From an sRGB hex.
    fn hex(hex: u32) -> Rgb {
        Rgb(
            srgb_to_linear(((hex >> 16) & 0xFF) as f32 / 255.0),
            srgb_to_linear(((hex >> 8) & 0xFF) as f32 / 255.0),
            srgb_to_linear((hex & 0xFF) as f32 / 255.0),
        )
    }

    /// A neutral of the given linear value.
    fn grey(v: f32) -> Rgb {
        Rgb(v, v, v)
    }

    /// `t` of the way to `other`.
    fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        Rgb(
            self.0 + (other.0 - self.0) * t,
            self.1 + (other.1 - self.1) * t,
            self.2 + (other.2 - self.2) * t,
        )
    }

    /// Componentwise sum, for adding a light onto a ground.
    fn add(self, other: Rgb) -> Rgb {
        Rgb(self.0 + other.0, self.1 + other.1, self.2 + other.2)
    }

    /// Uniform scale, for a vignette or a falloff.
    fn scale(self, k: f32) -> Rgb {
        Rgb(self.0 * k, self.1 * k, self.2 * k)
    }

    /// WCAG-weighted luminance, for the desaturation in [`raw`].
    fn luma(self) -> f32 {
        0.2126 * self.0 + 0.7152 * self.1 + 0.0722 * self.2
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Smooth Hermite step, 0 below `a`, 1 above `b`.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deterministic value noise in -0.5..=0.5, one value per integer cell.
///
/// Grain has to be a *fixed spatial frequency in tile space*, not a
/// per-output-pixel jitter: quantising to `cells` across the tile means
/// the 1024px master gets a visible photographic tooth, the 256px icon
/// gets fine dust, and the 16px icon averages it away to nothing. A hash
/// rather than an RNG because the bundle has to be byte reproducible.
fn grain(u: f32, v: f32, cells: f32) -> f32 {
    let gx = (u.clamp(0.0, 1.0) * cells) as u32;
    let gy = (v.clamp(0.0, 1.0) * cells) as u32;
    let mut h = gx
        .wrapping_mul(374_761_393)
        .wrapping_add(gy.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    h as f32 / u32::MAX as f32 - 0.5
}

/// The grade, as a shadow to highlight ramp. **This is the subject of the
/// icon**: cool shadows, a neutral crossover, a warm highlight, which is
/// what a graded photograph's colour looks like once you take the subject
/// away. Deliberately **not** the user's accent: a Dock icon that changed
/// colour when you changed a setting would read as a bug.
mod palette {
    use super::Rgb;

    /// Shadow to highlight. Positions ascending, fed to [`super::ramp`].
    pub fn grade() -> [(f32, Rgb); 10] {
        [
            (0.00, Rgb::hex(0x06101b)),
            (0.15, Rgb::hex(0x0d2740)),
            (0.32, Rgb::hex(0x1c4867)),
            (0.47, Rgb::hex(0x33627f)),
            (0.58, Rgb::hex(0x5a6f7a)),
            (0.68, Rgb::hex(0x85796c)),
            (0.79, Rgb::hex(0xb1906a)),
            (0.89, Rgb::hex(0xd8b184)),
            (0.96, Rgb::hex(0xeed3a9)),
            (1.00, Rgb::hex(0xfbeeda)),
        ]
    }

    /// The ground the mark sits on: the application's own near black.
    pub fn tile() -> Rgb {
        Rgb::hex(0x0b1016)
    }

    /// A soft lift behind the mark, so the tile is lit rather than flat.
    pub fn tile_glow() -> Rgb {
        Rgb::hex(0x1f2e3d)
    }

    /// The cast an unprocessed raw carries before white balance.
    pub fn raw_cast() -> Rgb {
        Rgb(0.055, 0.064, 0.080)
    }
}

/// The mark's geometry, **in the 32 unit space of the website's viewBox**.
///
/// The website draws a rounded rect from (3, 6) to (29, 26) with corner
/// radius 3 and a 2.2 wide centred stroke, cut by a leaning line. Keeping
/// those exact numbers here is what makes the two marks one mark.
mod mark {
    /// The frame path: left, top, right, bottom, and its corner radius.
    pub const FRAME: (f32, f32, f32, f32) = (3.0, 6.0, 29.0, 26.0);
    pub const FRAME_R: f32 = 3.0;
    /// Half the stroke width. The painted mark therefore spans
    /// 1.9..=30.1 across and 4.9..=27.1 down.
    pub const STROKE_HALF: f32 = 1.1;
    /// Painted bounds, derived from the two above.
    pub const X0: f32 = FRAME.0 - STROKE_HALF;
    pub const Y0: f32 = FRAME.1 - STROKE_HALF;
    pub const W: f32 = (FRAME.2 - FRAME.0) + 2.0 * STROKE_HALF;
    pub const H: f32 = (FRAME.3 - FRAME.1) + 2.0 * STROKE_HALF;

    /// The grade line runs through (12.25, 6) and (15.75, 26): a lean of
    /// about ten degrees, enough to survive a 16px render as a slant
    /// rather than as a rendering error.
    pub const CUT_TOP: f32 = 12.25;
    pub const CUT_BOTTOM: f32 = 15.75;
    /// How far each piece's own edge sits from that centre line. With a
    /// 1.1 half stroke on each side this leaves a visible gap of 2.0.
    pub const CUT_OFFSET: f32 = 2.1;

    /// Fraction of the tile's width the painted mark occupies.
    pub const FILL: f32 = 0.76;
}

/// Multi-stop gradient lookup; `stops` ascending by position.
fn ramp(stops: &[(f32, Rgb)], t: f32) -> Rgb {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for pair in stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (pair[0], pair[1]);
        if t <= p1 {
            return c0.lerp(c1, (t - p0) / (p1 - p0));
        }
    }
    stops[stops.len() - 1].1
}

/// Signed distance to an axis aligned rounded rectangle, negative inside.
/// Generalised from the square-only version this replaces, because the
/// mark's frame is not square and the tile is.
fn rounded_rect_sdf(x: f32, y: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - (hx - r);
    let qy = (y - cy).abs() - (hy - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Signed distance from the grade line, in mark units. Positive to the
/// right of it, which is the graded side.
fn grade_line_sdf(x: f32, y: f32) -> f32 {
    let (top, bottom) = (mark::CUT_TOP, mark::CUT_BOTTOM);
    let (ty, by) = (mark::FRAME.1, mark::FRAME.3);
    // Line through (top, ty) and (bottom, by), normalised.
    let (dx, dy) = (bottom - top, by - ty);
    let len = (dx * dx + dy * dy).sqrt();
    ((x - top) * dy - (y - ty) * dx) / len
}

/// The photograph, as a single luminance field over the whole mark, in
/// normalised (pu, pv) across the painted bounds. Light rises toward the
/// upper right and leaves the frame there rather than closing into a
/// disc, because any closed warm disc on a cool ground reads as a sunset
/// and this is not a picture of a sunset.
fn field(pu: f32, pv: f32) -> f32 {
    let t = (0.52 * pu + 0.48 * (1.0 - pv)).clamp(0.0, 1.0);
    let mut l = 0.03 + 0.93 * t.powf(1.25);
    // A broad, low lobe near the bright corner. Wide enough that its own
    // edge never becomes a visible shape.
    let d = (pu - 0.80).powi(2) + (pv - 0.18).powi(2);
    l += 0.06 * (-d / (2.0 * 0.34 * 0.34)).exp();
    l += grain(pu, pv, 380.0) * 0.009;
    l.clamp(0.0, 1.0)
}

/// The same pixel before the edit: desaturated, contrast cut hard, blacks
/// lifted, and carrying the cool cast of an unprocessed raw. Still a
/// photograph with structure in it, not a grey card, which would read as
/// a rendering failure rather than as "before".
fn raw(c: Rgb) -> Rgb {
    let luma = c.luma();
    let grey = c.lerp(Rgb::grey(luma), 0.55);
    let compress = |v: f32| 0.022 + 0.48 * v.max(0.0).powf(0.62);
    let flat = Rgb(compress(grey.0), compress(grey.1), compress(grey.2));
    flat.lerp(palette::raw_cast(), 0.33)
}

/// One RGBA8 icon of `size`x`size` px, supersampled and downsampled.
pub fn render(size: u32) -> Vec<u8> {
    let hi = size * SS;
    // Premultiplied linear accumulation, one entry per supersample.
    let mut hires: Vec<(Rgb, f32)> = vec![(Rgb(0.0, 0.0, 0.0), 0.0); (hi * hi) as usize];

    let s = hi as f32;
    // Full bleed: only enough margin to keep the antialiased edge off the
    // canvas boundary. The artwork is the icon, not something sitting on
    // it.
    let margin = s * MARGIN;
    let half = s * 0.5 - margin;
    let (cx, cy) = (s * 0.5, s * 0.5);
    let radius = half * 0.45;
    let rim = s * 0.0085;

    // Tile units per mark unit, and where the mark's origin lands.
    let k = mark::FILL / mark::W;
    let (ox, oy) = (0.5 - mark::FILL * 0.5, 0.5 - mark::H * k * 0.5);
    let grade = palette::grade();

    for py in 0..hi {
        for px in 0..hi {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = rounded_rect_sdf(x, y, cx, cy, half, half, radius);
            if d > 0.0 {
                continue; // outside the tile: transparent
            }
            let u = (x - (cx - half)) / (2.0 * half);
            let v = (y - (cy - half)) / (2.0 * half);

            // The ground: near black, lifted by a soft light behind the
            // mark, then vignetted and grained so it is a surface rather
            // than a fill.
            let lift = (-(((u - 0.48).powi(2) + (v - 0.42).powi(2)) / (2.0 * 0.44 * 0.44))).exp();
            let mut c = palette::tile().add(palette::tile_glow().scale(lift * 0.52));
            let r =
                ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt() / std::f32::consts::FRAC_1_SQRT_2;
            c = c.scale(1.0 - 0.32 * r.powf(2.4));
            c = c.add(Rgb::grey(grain(u, v, 600.0) * 0.0035));

            // Into mark space, and the photograph that lives there.
            let mx = (u - ox) / k + mark::X0;
            let my = (v - oy) / k + mark::Y0;
            let frame = rounded_rect_sdf(
                mx,
                my,
                (mark::FRAME.0 + mark::FRAME.2) * 0.5,
                (mark::FRAME.1 + mark::FRAME.3) * 0.5,
                (mark::FRAME.2 - mark::FRAME.0) * 0.5,
                (mark::FRAME.3 - mark::FRAME.1) * 0.5,
                mark::FRAME_R,
            );
            let cut = grade_line_sdf(mx, my);
            let raw_piece = frame.max(cut + mark::CUT_OFFSET);
            let graded_piece = frame.max(mark::CUT_OFFSET - cut);

            if raw_piece <= mark::STROKE_HALF || graded_piece <= mark::STROKE_HALF {
                let pu = ((mx - mark::X0) / mark::W).clamp(0.0, 1.0);
                let pv = ((my - mark::Y0) / mark::H).clamp(0.0, 1.0);
                let graded = ramp(&grade, field(pu, pv));
                c = if graded_piece <= mark::STROKE_HALF {
                    graded
                } else {
                    raw(graded)
                };
            }

            // Inner rim: a light edge along the top easing to a dark one
            // along the bottom, so the tile reads as an object and not a
            // sticker. Eased rather than switched at the midline, which
            // leaves a visible seam on the left and right edges.
            if d > -rim {
                let k_edge = -d / rim;
                let edge = Rgb::grey(1.0 - smoothstep(0.30, 0.70, v));
                c = edge.lerp(c, 0.55 + 0.45 * k_edge);
            }

            hires[(py * hi + px) as usize] = (c, 1.0);
        }
    }

    // Box downsample: SSxSS linear-light samples per output pixel, then
    // back to sRGB. Averaging coverage this way is what antialiases the
    // squircle's edge.
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let n = (SS * SS) as f32;
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let (c, alpha) = hires[(((y * SS + sy) * hi) + (x * SS + sx)) as usize];
                    // Premultiply, so transparent samples outside the
                    // squircle don't drag the edge colour toward black.
                    r += c.0 * alpha;
                    g += c.1 * alpha;
                    b += c.2 * alpha;
                    a += alpha;
                }
            }
            let (r, g, b, a) = (r / n, g / n, b / n, a / n);
            let unpremul = |c: f32| if a > 0.0 { c / a } else { 0.0 };
            out.push((linear_to_srgb(unpremul(r)).clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push((linear_to_srgb(unpremul(g)).clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push((linear_to_srgb(unpremul(b)).clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push((a.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
}

/// Writes one RGBA8 icon as a PNG.
pub fn write_png(path: &Path, size: u32) -> Result<()> {
    let pixels = render(size);
    let file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    Ok(())
}

/// The `.iconset` members macOS expects: `(filename, pixel size)`.
pub const ICONSET: &[(&str, u32)] = &[
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(px: &[u8], size: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * size + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3])
    }

    /// Samples the render at a point in the **mark's** 32 unit space, the
    /// space [`mark`] and the website's viewBox are both written in,
    /// rather than in canvas fractions. The two differ by the margin and
    /// by the mark's placement, and mixing them up is how the first draft
    /// of these tests ended up probing the tile and calling it the grade.
    fn at(px: &[u8], size: u32, mx: f32, my: f32) -> (u8, u8, u8, u8) {
        let k = mark::FILL / mark::W;
        let u = (mx - mark::X0) * k + (0.5 - mark::FILL * 0.5);
        let v = (my - mark::Y0) * k + (0.5 - mark::H * k * 0.5);
        let to_canvas = |t: f32| (MARGIN + t * (1.0 - 2.0 * MARGIN)) * size as f32;
        let x = to_canvas(u).round().clamp(0.0, (size - 1) as f32) as u32;
        let y = to_canvas(v).round().clamp(0.0, (size - 1) as f32) as u32;
        pixel(px, size, x, y)
    }

    fn luma(c: (u8, u8, u8, u8)) -> f32 {
        0.2126 * f32::from(c.0) + 0.7152 * f32::from(c.1) + 0.0722 * f32::from(c.2)
    }

    fn saturation(c: (u8, u8, u8, u8)) -> f32 {
        let (r, g, b) = (f32::from(c.0), f32::from(c.1), f32::from(c.2));
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max <= 0.0 {
            0.0
        } else {
            (max - min) / max
        }
    }

    /// A rounded tile that **fills its canvas**: corners transparent,
    /// centre opaque, and every edge midpoint opaque within a hair of the
    /// boundary.
    #[test]
    fn the_artwork_fills_the_canvas_with_rounded_corners() {
        let size = 256;
        let px = render(size);
        assert_eq!(pixel(&px, size, 0, 0).3, 0, "corner must be transparent");
        assert_eq!(pixel(&px, size, size - 1, size - 1).3, 0);
        assert_eq!(pixel(&px, size, size / 2, size / 2).3, 255, "centre opaque");

        let inside = (MARGIN * size as f32).ceil() as u32 + 1;
        let mid = size / 2;
        for (x, y, edge) in [
            (mid, inside, "top"),
            (mid, size - 1 - inside, "bottom"),
            (inside, mid, "left"),
            (size - 1 - inside, mid, "right"),
        ] {
            assert_eq!(
                pixel(&px, size, x, y).3,
                255,
                "the artwork must reach the {edge} edge: this icon is full bleed"
            );
        }
    }

    /// **The cut is the point of the mark.** Both pieces must be brighter
    /// than the gap between them, or the mark has fused into one blob and
    /// the 16px render says nothing.
    #[test]
    fn the_cut_separates_two_pieces() {
        let size = 256;
        let px = render(size);
        // Mid height: the raw piece, the gap on the line itself, the
        // graded piece.
        let mid_y = (mark::FRAME.1 + mark::FRAME.3) * 0.5;
        let cut_x = (mark::CUT_TOP + mark::CUT_BOTTOM) * 0.5;
        let raw_side = at(&px, size, 7.0, mid_y);
        let gap = at(&px, size, cut_x, mid_y);
        let graded_side = at(&px, size, 24.0, mid_y);
        assert_eq!(gap.3, 255, "the gap probe fell outside the tile");
        assert!(
            luma(raw_side) > luma(gap) + 20.0,
            "the raw piece must stand off the ground: {raw_side:?} vs gap {gap:?}"
        );
        assert!(
            luma(graded_side) > luma(gap) + 20.0,
            "the graded piece must stand off the ground: {graded_side:?} vs gap {gap:?}"
        );
    }

    /// The left piece is the file as it came off the sensor and the right
    /// piece is the same pixels graded: flatter, and with the colour
    /// taken out of it. Asserting the *relationship* rather than pixel
    /// values keeps this meaningful if the palette is ever retuned.
    #[test]
    fn the_left_of_the_grade_line_is_the_ungraded_photograph() {
        let size = 256;
        let px = render(size);
        let probe = |mx: f32, my: f32| {
            let c = at(&px, size, mx, my);
            assert_eq!(c.3, 255, "probe ({mx}, {my}) is outside the tile");
            c
        };
        // Matched pairs: the bright end and the dark end of each piece.
        let raw_bright = probe(11.0, 8.5);
        let raw_dark = probe(6.0, 23.5);
        let graded_bright = probe(26.0, 8.5);
        let graded_dark = probe(19.0, 23.5);

        assert!(
            saturation(graded_bright) > saturation(raw_bright)
                && saturation(graded_dark) > saturation(raw_dark),
            "the graded piece must carry the colour: raw {raw_bright:?} {raw_dark:?} \
             vs graded {graded_bright:?} {graded_dark:?}"
        );
        let raw_range = luma(raw_bright) - luma(raw_dark);
        let graded_range = luma(graded_bright) - luma(graded_dark);
        assert!(
            graded_range > raw_range * 2.0,
            "the raw piece must be the flat one: range {raw_range} vs {graded_range}"
        );
        assert!(
            luma(raw_dark) > luma(graded_dark),
            "the raw piece must have lifted blacks: {raw_dark:?} vs {graded_dark:?}"
        );
    }

    /// Deterministic: the bundle is reproducible only if this holds.
    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(render(32), render(32));
    }
}
