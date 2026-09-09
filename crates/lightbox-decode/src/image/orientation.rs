// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! EXIF-orientation pixel reflow (A7). Bakes one of the eight EXIF orientations
//! into an interleaved sample buffer so downstream non-raw handling is
//! orientation-agnostic (spec §3.1: "EXIF orientation applied").

use lightbox_types::Orientation;

/// Applies `orientation` to an interleaved `width × height × channels` buffer,
/// returning the upright `(data, out_width, out_height)`. `O1` is a cheap
/// move-through. Orientations 5-8 transpose (swap width/height).
pub(crate) fn apply(
    data: Vec<f32>,
    width: u32,
    height: u32,
    channels: u8,
    orientation: Orientation,
) -> (Vec<f32>, u32, u32) {
    if orientation == Orientation::O1 || width == 0 || height == 0 {
        return (data, width, height);
    }

    let w = width as usize;
    let h = height as usize;
    let c = channels as usize;
    let transposes = orientation.transposes();
    let (dw, dh) = if transposes { (h, w) } else { (w, h) };
    let mut out = vec![0.0f32; dw * dh * c];

    for sy in 0..h {
        for sx in 0..w {
            // Forward map source (sx, sy) → destination (dx, dy) per the EXIF
            // orientation definitions (1=normal … 8=rotate 270° CW).
            let (dx, dy) = match orientation {
                Orientation::O1 => (sx, sy),
                Orientation::O2 => (w - 1 - sx, sy),
                Orientation::O3 => (w - 1 - sx, h - 1 - sy),
                Orientation::O4 => (sx, h - 1 - sy),
                Orientation::O5 => (sy, sx),
                Orientation::O6 => (h - 1 - sy, sx),
                Orientation::O7 => (h - 1 - sy, w - 1 - sx),
                Orientation::O8 => (sy, w - 1 - sx),
            };
            let src = (sy * w + sx) * c;
            let dst = (dy * dw + dx) * c;
            out[dst..dst + c].copy_from_slice(&data[src..src + c]);
        }
    }

    (out, dw as u32, dh as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 2×3 single-channel image, values = row-major index:
    //   0 1
    //   2 3
    //   4 5
    fn sample() -> (Vec<f32>, u32, u32) {
        (vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0], 2, 3)
    }

    #[test]
    fn identity_is_unchanged() {
        let (d, w, h) = sample();
        let (o, ow, oh) = apply(d.clone(), w, h, 1, Orientation::O1);
        assert_eq!((ow, oh), (2, 3));
        assert_eq!(o, d);
    }

    #[test]
    fn flip_horizontal_o2() {
        let (d, w, h) = sample();
        let (o, ow, oh) = apply(d, w, h, 1, Orientation::O2);
        assert_eq!((ow, oh), (2, 3));
        // rows reversed within each row:
        assert_eq!(o, vec![1.0, 0.0, 3.0, 2.0, 5.0, 4.0]);
    }

    #[test]
    fn rotate_180_o3() {
        let (d, w, h) = sample();
        let (o, ow, oh) = apply(d, w, h, 1, Orientation::O3);
        assert_eq!((ow, oh), (2, 3));
        assert_eq!(o, vec![5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn rotate_90_cw_o6_transposes() {
        let (d, w, h) = sample();
        let (o, ow, oh) = apply(d, w, h, 1, Orientation::O6);
        // 2×3 -> 3×2. 90° CW: bottom-left of source becomes top-left.
        //   source           dest (3 wide, 2 tall)
        //   0 1              4 2 0
        //   2 3      ->      5 3 1
        //   4 5
        assert_eq!((ow, oh), (3, 2));
        assert_eq!(o, vec![4.0, 2.0, 0.0, 5.0, 3.0, 1.0]);
    }

    #[test]
    fn multichannel_keeps_pixels_intact() {
        // 2×1 RGB: pixel A=(1,2,3), pixel B=(4,5,6).
        let d = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (o, ow, oh) = apply(d, 2, 1, 3, Orientation::O2);
        assert_eq!((ow, oh), (2, 1));
        // Horizontally flipped: B then A, channels intact.
        assert_eq!(o, vec![4.0, 5.0, 6.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn all_orientations_preserve_pixel_count() {
        for o in [
            Orientation::O2,
            Orientation::O3,
            Orientation::O4,
            Orientation::O5,
            Orientation::O6,
            Orientation::O7,
            Orientation::O8,
        ] {
            let (d, w, h) = sample();
            let (out, ow, oh) = apply(d, w, h, 1, o);
            assert_eq!(out.len(), 6);
            assert_eq!((ow as usize) * (oh as usize), 6);
        }
    }
}
