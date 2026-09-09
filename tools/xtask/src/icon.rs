// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The app icon, **drawn, not shipped as an asset**.
//!
//! ## The mark
//!
//! A photograph, full bleed: a sunset over water, mountains, the sun's
//! light broken across the surface. It fills the icon canvas edge to edge
//! (a macOS squircle with a 2% breathing margin, no inset artwork) because
//! the picture *is* the product.
//!
//! Across it, at a slight angle, runs the **grade line**: to its left the
//! same photograph as it comes off the sensor, flat, cool, blacks lifted
//! and to its right the graded version. That is the one gesture that makes
//! this an editor's icon rather than a viewer's, and it is the app's whole
//! job in a single shape.
//!
//! ## Why generate instead of committing PNGs
//!
//! A committed icon is a binary blob that needs its own REUSE entry and
//! drifts from the in-app mark. This is arithmetic with no dependency
//! beyond the `png` encoder the workspace already uses, and it is exactly
//! reproducible: same input, same bytes (asserted).
//!
//! **Anti-aliasing** is 4× supersampling plus a box downsample rather than
//! analytic coverage. At icon sizes the difference is invisible and the
//! code stays something you can read in one sitting.

use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result};

/// Supersampling factor. 4× is 16 samples per output pixel, clean edges
/// on the 16px icon, and even the 1024px master renders in milliseconds.
const SS: u32 = 4;

/// Linear RGB, 0..=1. Everything composites in linear light: an 8-bit sRGB
/// lerp across a sky gradient this wide bands visibly.
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

    /// `t` of the way to `other`.
    fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        Rgb(
            self.0 + (other.0 - self.0) * t,
            self.1 + (other.1 - self.1) * t,
            self.2 + (other.2 - self.2) * t,
        )
    }

    /// WCAG-weighted luminance, for the desaturation in [`ungraded`].
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

/// The scene's palette. Deliberately **not** the user's accent: a Dock
/// icon that changed color when you changed a setting would read as a bug.
mod palette {
    use super::Rgb;

    pub fn zenith() -> Rgb {
        Rgb::hex(0x101d38)
    }
    pub fn upper_sky() -> Rgb {
        Rgb::hex(0x1f3f67)
    }
    pub fn mid_sky() -> Rgb {
        Rgb::hex(0x4a6a94)
    }
    pub fn glow() -> Rgb {
        Rgb::hex(0xc98a5a)
    }
    pub fn horizon() -> Rgb {
        Rgb::hex(0xf0b268)
    }
    pub fn sun() -> Rgb {
        Rgb::hex(0xffe6ab)
    }
    pub fn ridge_far() -> Rgb {
        Rgb::hex(0x54688c)
    }
    pub fn ridge_near() -> Rgb {
        Rgb::hex(0x1b2740)
    }
    pub fn water_top() -> Rgb {
        Rgb::hex(0x2b4166)
    }
    pub fn water_bottom() -> Rgb {
        Rgb::hex(0x0e1728)
    }
    pub fn reflection() -> Rgb {
        Rgb::hex(0xe8a860)
    }
    /// The cool cast an unprocessed raw carries before white balance.
    pub fn raw_cast() -> Rgb {
        Rgb(0.16, 0.18, 0.22)
    }
}

// ── scene geometry, in normalised (u, v) inside the tile ────────────────

/// Where sky ends and water begins.
const HORIZON: f32 = 0.62;
/// Sun centre and radius.
const SUN_C: (f32, f32) = (0.655, 0.520);
const SUN_R: f32 = 0.075;
/// The grade line: `u` at mid-height, and how far it leans per unit `v`.
const GRADE_U: f32 = 0.255;
const GRADE_LEAN: f32 = 0.16;
/// Half-width of the bright divider drawn on the line itself.
const GRADE_LINE_W: f32 = 0.0035;

/// The far mountain range, as `(u, v)` control points.
const RIDGE_FAR: &[(f32, f32)] = &[
    (0.00, 0.560),
    (0.16, 0.475),
    (0.33, 0.545),
    (0.50, 0.455),
    (0.70, 0.535),
    (1.00, 0.495),
];
/// The near range, the silhouette that gives the picture its base.
const RIDGE_NEAR: &[(f32, f32)] = &[
    (0.00, 0.560),
    (0.14, 0.505),
    (0.30, 0.585),
    (0.46, 0.520),
    (0.62, 0.598),
    (0.80, 0.545),
    (1.00, 0.575),
];

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

/// Piecewise-linear ridge height at `u`.
fn ridge(points: &[(f32, f32)], u: f32) -> f32 {
    if u <= points[0].0 {
        return points[0].1;
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if u <= x1 {
            return y0 + (y1 - y0) * (u - x0) / (x1 - x0);
        }
    }
    points[points.len() - 1].1
}

/// The photograph at `(u, v)`, the graded version.
fn scene(u: f32, v: f32) -> Rgb {
    if v < HORIZON {
        let mut c = ramp(
            &[
                (0.00, palette::zenith()),
                (0.30, palette::upper_sky()),
                (0.55, palette::mid_sky()),
                (0.80, palette::glow()),
                (1.00, palette::horizon()),
            ],
            v / HORIZON,
        );
        // Sun, and the halo it throws into the sky around it.
        let d = ((u - SUN_C.0).powi(2) + (v - SUN_C.1).powi(2)).sqrt();
        let halo = (1.0 - d / (SUN_R * 3.4)).max(0.0).powf(2.2);
        c = c.lerp(palette::sun(), halo * 0.55);
        if d < SUN_R {
            c = c.lerp(palette::sun(), ((SUN_R - d) / (SUN_R * 0.18)).min(1.0));
        }
        // The far range sits *in* that glow, so it keeps a little of it;
        // the near one is a silhouette.
        if v > ridge(RIDGE_FAR, u) {
            c = palette::ridge_far().lerp(c, 0.18);
        }
        if v > ridge(RIDGE_NEAR, u) {
            c = palette::ridge_near();
        }
        c
    } else {
        let t = (v - HORIZON) / (1.0 - HORIZON);
        let mut c = palette::water_top().lerp(palette::water_bottom(), t);
        // A widening, fading column of broken light, the ripple bands get
        // sparser and weaker with depth, so it reads as water rather than
        // as a ladder.
        let width = 0.045 + t * 0.075;
        let col = (1.0 - (u - SUN_C.0).abs() / width).max(0.0).powf(1.6);
        let ripple = 0.5 + 0.5 * ((v - HORIZON) * (95.0 - t * 45.0)).cos();
        c = c.lerp(
            palette::reflection(),
            col * (1.0 - t).powf(1.5) * (0.30 + 0.55 * ripple),
        );
        c
    }
}

/// The same pixel before the edit: desaturated, blacks lifted, contrast
/// cut, and carrying the cool cast of an unprocessed raw. Still a
/// photograph, not a grey card, which would read as a rendering failure
/// rather than as "before".
fn ungraded(c: Rgb) -> Rgb {
    let luma = c.luma();
    let grey = c.lerp(Rgb(luma, luma, luma), 0.30);
    let flat = Rgb(
        0.030 + grey.0 * 0.78,
        0.030 + grey.1 * 0.78,
        0.030 + grey.2 * 0.78,
    );
    flat.lerp(palette::raw_cast(), 0.10)
}

/// Signed distance to a rounded rectangle centred at `(cx, cy)` with
/// half-extent `half` and corner radius `r`. Negative inside.
fn rounded_rect_sdf(x: f32, y: f32, cx: f32, cy: f32, half: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - (half - r);
    let qy = (y - cy).abs() - (half - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// One RGBA8 icon of `size`×`size` px, supersampled and downsampled.
pub fn render(size: u32) -> Vec<u8> {
    let hi = size * SS;
    // Premultiplied linear accumulation, one entry per supersample.
    let mut hires: Vec<(Rgb, f32)> = vec![(Rgb(0.0, 0.0, 0.0), 0.0); (hi * hi) as usize];

    let s = hi as f32;
    // Full bleed: only enough margin to keep the antialiased edge off the
    // canvas boundary. The artwork is the icon, not something sitting on
    // it.
    let margin = s * 0.02;
    let half = s * 0.5 - margin;
    let (cx, cy) = (s * 0.5, s * 0.5);
    let radius = half * 0.45;
    let rim = s * 0.008;

    for py in 0..hi {
        for px in 0..hi {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = rounded_rect_sdf(x, y, cx, cy, half, radius);
            if d > 0.0 {
                continue; // outside the tile: transparent
            }
            let u = (x - (cx - half)) / (2.0 * half);
            let v = (y - (cy - half)) / (2.0 * half);

            let mut c = scene(u, v);

            // The grade line.
            let line_u = GRADE_U + (v - 0.5) * GRADE_LEAN;
            if u < line_u - GRADE_LINE_W {
                c = ungraded(c);
            } else if u < line_u + GRADE_LINE_W {
                c = c.lerp(Rgb(1.0, 1.0, 1.0), 0.72);
            }

            // Inner rim: a light edge along the top, a dark one along the
            // bottom, so the tile reads as an object and not a sticker.
            if d > -rim {
                let k = -d / rim;
                let edge = if y < cy {
                    Rgb(1.0, 1.0, 1.0)
                } else {
                    Rgb(0.0, 0.0, 0.0)
                };
                c = edge.lerp(c, 0.55 + 0.45 * k);
            }

            hires[(py * hi + px) as usize] = (c, 1.0);
        }
    }

    // Box downsample: SS×SS linear-light samples per output pixel, then
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
                    // squircle don't drag the edge color toward black.
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

    /// The artwork's margin, as a fraction of the canvas, how far in from
    /// the edge the squircle starts.
    const MARGIN: f32 = 0.02;

    fn pixel(px: &[u8], size: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * size + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3])
    }

    /// Samples the render at a point in the **scene's** (u, v) space, the
    /// space `scene()` and the geometry constants are written in, rather
    /// than in canvas fractions. The two differ by the margin, and mixing
    /// them up is how the first draft of these tests ended up probing a
    /// mountain and calling it the sun.
    fn at(px: &[u8], size: u32, u: f32, v: f32) -> (u8, u8, u8, u8) {
        let to_canvas = |t: f32| (MARGIN + t * (1.0 - 2.0 * MARGIN)) * size as f32;
        let x = to_canvas(u).round().clamp(0.0, (size - 1) as f32) as u32;
        let y = to_canvas(v).round().clamp(0.0, (size - 1) as f32) as u32;
        pixel(px, size, x, y)
    }

    /// A rounded tile that **fills its canvas**: corners transparent,
    /// centre opaque, and every edge midpoint opaque within a hair of the
    /// boundary. The design this replaced inset its artwork by 9% a side,
    /// which is exactly what this rules out.
    #[test]
    fn the_artwork_fills_the_canvas_with_rounded_corners() {
        let size = 256;
        let px = render(size);
        assert_eq!(pixel(&px, size, 0, 0).3, 0, "corner must be transparent");
        assert_eq!(pixel(&px, size, size - 1, size - 1).3, 0);
        assert_eq!(pixel(&px, size, size / 2, size / 2).3, 255, "centre opaque");

        // One pixel inside the margin, on each edge's midpoint.
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
                "the artwork must reach the {edge} edge — this icon is full bleed"
            );
        }
    }

    /// Every layer of the scene has to actually paint. Probes are on the
    /// graded side, and clear of the ridges, the sun sets *behind* the far
    /// range, so its own centre is mountain, not sun.
    #[test]
    fn sky_water_and_sun_all_render() {
        let size = 256;
        let px = render(size);

        let (r, g, b, _) = at(&px, size, 0.80, 0.10); // upper sky
        assert!(b > r && b > g, "expected deep sky blue, got ({r},{g},{b})");

        let (r, g, b, _) = at(&px, size, SUN_C.0, SUN_C.1 - SUN_R * 0.5); // sun, above the ridge
        assert!(
            r > 220 && g > 200 && b > 120 && r > b,
            "expected the warm sun disc, got ({r},{g},{b})"
        );

        let (r, g, b, _) = at(&px, size, 0.90, 0.92); // deep water, clear of the reflection
        assert!(
            b > r && b > g && b < 120,
            "expected dark water, got ({r},{g},{b})"
        );

        let (r, g, b, _) = at(&px, size, SUN_C.0, 0.68); // the reflection column
        assert!(
            r > b,
            "expected the warm reflection under the sun, got ({r},{g},{b})"
        );
    }

    /// **The grade line is the point of the mark**: the same scanline must
    /// be measurably flatter and less saturated to the left of it than to
    /// the right. Asserting the *relationship* rather than pixel values
    /// keeps this meaningful if the palette is ever retuned.
    #[test]
    fn the_left_of_the_grade_line_is_the_ungraded_photograph() {
        let size = 256;
        let px = render(size);
        // Asserting opacity first is not defensive tidiness: the first
        // draft of this test probed the rounded corner, where both sides
        // return transparent black, and "0 > 0" is a comparison that
        // proves nothing while looking like it does.
        let rgb = |u: f32, v: f32| {
            let (r, g, b, a) = at(&px, size, u, v);
            assert_eq!(a, 255, "probe ({u}, {v}) is outside the tile");
            (f32::from(r), f32::from(g), f32::from(b))
        };
        let saturation = |(r, g, b): (f32, f32, f32)| {
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            if max <= 0.0 {
                0.0
            } else {
                (max - min) / max
            }
        };
        let luma = |(r, g, b): (f32, f32, f32)| 0.2126 * r + 0.7152 * g + 0.0722 * b;

        // Same height, both sides of the line, clear of it and of the
        // sun's halo.
        let before = rgb(0.05, 0.20);
        let after = rgb(0.55, 0.20);
        assert!(
            saturation(before) < saturation(after),
            "the ungraded side must be less saturated: {before:?} vs {after:?}"
        );

        // And its blacks are lifted: the darkest water on the ungraded
        // side is brighter than the same depth on the graded side.
        let dark_before = rgb(0.10, 0.88);
        let dark_after = rgb(0.88, 0.88);
        assert!(
            luma(dark_before) > luma(dark_after),
            "the ungraded side must have lifted blacks: {dark_before:?} vs {dark_after:?}"
        );
    }

    /// Deterministic: the bundle is reproducible only if this holds.
    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(render(32), render(32));
    }
}
