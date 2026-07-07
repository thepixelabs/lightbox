// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Working-format host-pixel helpers shared by the engine-owned nodes' CPU
//! paths (A-gpu). These read/write [`PixelBuf`]s in the working (`Rgba16F` /
//! `Rgba32F`) and display (`Rgba8*`) encodings **without any color math** — the
//! transfer/gamut work lives in `xform.display` (via `lightbox-color`).

use half::f16;

use crate::ng::tile::{PixelBuf, PixelFormat};
use crate::ng::types::Extent;

/// Reads pixel `(x, y)` of a working/display buffer as RGBA f32.
pub fn read_rgba_f32(buf: &PixelBuf, x: usize, y: usize) -> [f32; 4] {
    let stride = buf.stride as usize;
    let row = &buf.bytes[y * stride..];
    match buf.format {
        PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => {
            let i = x * 4;
            [
                f32::from(row[i]) / 255.0,
                f32::from(row[i + 1]) / 255.0,
                f32::from(row[i + 2]) / 255.0,
                f32::from(row[i + 3]) / 255.0,
            ]
        }
        PixelFormat::Rgba16F => {
            let i = x * 8;
            let ch = |o: usize| f16::from_le_bytes([row[i + o], row[i + o + 1]]).to_f32();
            [ch(0), ch(2), ch(4), ch(6)]
        }
        PixelFormat::Rgba32F => {
            let i = x * 16;
            let ch = |o: usize| {
                f32::from_le_bytes([row[i + o], row[i + o + 1], row[i + o + 2], row[i + o + 3]])
            };
            [ch(0), ch(4), ch(8), ch(12)]
        }
        PixelFormat::R16F => {
            let i = x * 2;
            [
                f16::from_le_bytes([row[i], row[i + 1]]).to_f32(),
                0.0,
                0.0,
                1.0,
            ]
        }
    }
}

/// A zeroed `Rgba16F` working buffer of `extent` (tight `w*8` stride).
pub fn new_rgba16f(extent: Extent) -> PixelBuf {
    let w = extent.w.max(1);
    let h = extent.h.max(1);
    PixelBuf {
        bytes: vec![0u8; (w as usize) * (h as usize) * 8],
        format: PixelFormat::Rgba16F,
        extent: Extent { w, h },
        stride: w * 8,
    }
}

/// A zeroed `Rgba8Unorm` display buffer of `extent` (tight `w*4` stride).
pub fn new_rgba8(extent: Extent) -> PixelBuf {
    let w = extent.w.max(1);
    let h = extent.h.max(1);
    PixelBuf {
        bytes: vec![0u8; (w as usize) * (h as usize) * 4],
        format: PixelFormat::Rgba8Unorm,
        extent: Extent { w, h },
        stride: w * 4,
    }
}

/// Writes RGBA f32 into an `Rgba16F` buffer at `(x, y)`.
pub fn write_rgba16f(buf: &mut PixelBuf, x: usize, y: usize, rgba: [f32; 4]) {
    let stride = buf.stride as usize;
    let base = y * stride + x * 8;
    for (c, &v) in rgba.iter().enumerate() {
        let b = f16::from_f32(v).to_le_bytes();
        buf.bytes[base + c * 2] = b[0];
        buf.bytes[base + c * 2 + 1] = b[1];
    }
}

/// Writes RGBA (`[0,1]` f32) into an `Rgba8Unorm` buffer at `(x, y)`, round-half-up.
pub fn write_rgba8(buf: &mut PixelBuf, x: usize, y: usize, rgba: [f32; 4]) {
    let stride = buf.stride as usize;
    let base = y * stride + x * 4;
    for (c, &v) in rgba.iter().enumerate() {
        buf.bytes[base + c] = (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
    }
}
