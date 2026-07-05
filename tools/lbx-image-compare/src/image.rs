// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Rgba8Image`] — the harness's pixel container, plus strict PNG IO.
//!
//! Goldens are committed as 8-bit RGBA PNGs; reading accepts 8-bit RGB or
//! RGBA only (goldens must be exactly what they claim — no palette, no
//! 16-bit, no implicit conversions beyond the alpha fill).

use std::path::Path;

use crate::CompareError;

/// An owned RGBA8 image: tightly packed rows, `4 * width * height` bytes.
/// The same layout as `lightbox-render`'s CPU render output.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba8Image {
    /// Pixels, RGBA interleaved, row-major.
    pub px: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl std::fmt::Debug for Rgba8Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rgba8Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.px.len())
            .finish()
    }
}

impl Rgba8Image {
    /// Wraps a pixel buffer, validating its length against the dimensions.
    pub fn new(width: u32, height: u32, px: Vec<u8>) -> Result<Rgba8Image, CompareError> {
        let expect = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| CompareError::BadImage("dimensions overflow".into()))?;
        if px.len() != expect {
            return Err(CompareError::BadImage(format!(
                "buffer is {} bytes, {width}x{height} RGBA needs {expect}",
                px.len()
            )));
        }
        if width == 0 || height == 0 {
            return Err(CompareError::BadImage("zero-sized image".into()));
        }
        Ok(Rgba8Image { px, width, height })
    }

    /// Number of pixels.
    pub fn pixel_count(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Decodes a PNG (8-bit RGB or RGBA only; RGB gains an opaque alpha).
    pub fn decode_png(bytes: &[u8]) -> Result<Rgba8Image, CompareError> {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder
            .read_info()
            .map_err(|e| CompareError::Png(e.to_string()))?;
        let buf_size = reader
            .output_buffer_size()
            .ok_or_else(|| CompareError::Png("output size overflows".into()))?;
        let mut buf = vec![0u8; buf_size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| CompareError::Png(e.to_string()))?;
        if info.bit_depth != png::BitDepth::Eight {
            return Err(CompareError::Png(format!(
                "unsupported bit depth {:?} (goldens are 8-bit)",
                info.bit_depth
            )));
        }
        buf.truncate(info.buffer_size());
        let px = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => {
                let mut px = Vec::with_capacity(buf.len() / 3 * 4);
                for rgb in buf.chunks_exact(3) {
                    px.extend_from_slice(rgb);
                    px.push(255);
                }
                px
            }
            other => {
                return Err(CompareError::Png(format!(
                    "unsupported color type {other:?} (goldens are RGB/RGBA)"
                )))
            }
        };
        Rgba8Image::new(info.width, info.height, px)
    }

    /// Reads a PNG file via [`Rgba8Image::decode_png`].
    pub fn read_png(path: &Path) -> Result<Rgba8Image, CompareError> {
        let bytes = std::fs::read(path)
            .map_err(|e| CompareError::Io(format!("read {}: {e}", path.display())))?;
        Rgba8Image::decode_png(&bytes)
    }

    /// Encodes as an 8-bit RGBA PNG.
    pub fn encode_png(&self) -> Result<Vec<u8>, CompareError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| CompareError::Png(e.to_string()))?;
            writer
                .write_image_data(&self.px)
                .map_err(|e| CompareError::Png(e.to_string()))?;
        }
        Ok(out)
    }

    /// Writes an 8-bit RGBA PNG, creating parent directories.
    pub fn write_png(&self, path: &Path) -> Result<(), CompareError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompareError::Io(format!("mkdir {}: {e}", parent.display())))?;
        }
        let bytes = self.encode_png()?;
        std::fs::write(path, bytes)
            .map_err(|e| CompareError::Io(format!("write {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Rgba8Image {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 200 } else { 40 };
                px.extend_from_slice(&[v, x as u8, y as u8, 255]);
            }
        }
        Rgba8Image::new(w, h, px).unwrap()
    }

    #[test]
    fn png_round_trip_is_lossless() {
        let img = checker(13, 7);
        let png = img.encode_png().unwrap();
        let back = Rgba8Image::decode_png(&png).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn rgb_png_gains_opaque_alpha() {
        // Encode an RGB PNG by hand via the png crate.
        let (w, h) = (3u32, 2u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7) as u8).collect();
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgb).unwrap();
        }
        let img = Rgba8Image::decode_png(&bytes).unwrap();
        assert_eq!((img.width, img.height), (w, h));
        assert!(img.px.chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(&img.px[0..3], &rgb[0..3]);
    }

    #[test]
    fn new_rejects_bad_buffers() {
        assert!(Rgba8Image::new(2, 2, vec![0; 15]).is_err());
        assert!(Rgba8Image::new(0, 2, vec![]).is_err());
        assert!(Rgba8Image::decode_png(b"not a png").is_err());
    }
}
