// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The pixels-in / device / tiles-out seam traits (spec §3.8) + the upload path
//! (task **A10**) + progressive ladder (task **C6**).
//!
//! Owner: **A-gpu** owns the engine-side [`Uploader`] (upload path) and the
//! ladder plumbing. The [`DeviceProvider`] / [`SourceProvider`] / [`TileSink`]
//! **traits are implemented by neighbors** (E01/E08 shell, E02/E03) and only
//! *consumed* here, the engine never decodes and never reads the preview store
//! directly.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_types::ImageId;

use half::f16;

use crate::ng::colorimetry::{ColorPrimaries, SourceColorimetry, SourceQuality, TransferFunction};
use crate::ng::error::{DeviceError, SourceError};
use crate::ng::gpu::DeviceCtx;
use crate::ng::tile::{PixelBuf, PixelFormat, TileHandle};
use crate::ng::types::{Extent, TileCoord, TilePrecision};
use crate::ng::BoxFuture;

/// A shared device/queue pair (the shell's wgpu handles).
pub type DeviceHandles = (Arc<wgpu::Device>, Arc<wgpu::Queue>);

/// E01/E08 (the shell owns the device): current handles + coordinated rebuild
/// on device-lost (spec §3.8). Implemented by the shell.
pub trait DeviceProvider: Send + Sync {
    /// The current shared device + queue.
    fn current(&self) -> DeviceHandles;
    /// Rebuild after device loss, yielding fresh handles (task E2).
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>>;

    /// The adapter identity backing [`Self::current`], if the shell can report it
    /// (drives [`crate::ng::Engine::active_backend`] → `Gpu(AdapterInfo)`,
    /// spec §3.6). **Additive with a `None` default** (E05-deviations §E): the
    /// `DeviceProvider`/`SourceProvider` seam yields only handles, so existing
    /// providers compile unchanged; the shell (F5) and GPU test providers, which
    /// own the adapter, override it with the real info. When `None`, the engine
    /// reports a labelled "unknown adapter" placeholder, never a fabricated
    /// identity.
    fn adapter_info(&self) -> Option<wgpu::AdapterInfo> {
        None
    }
}

/// What the engine wants from the source seam (spec §3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceWant {
    /// The best available tier no larger than `max_px` pixels (fast first paint).
    BestAvailable {
        /// Upper bound on the longest edge, pixels.
        max_px: u32,
    },
    /// The full decoded image (already-demosaiced RGB at M1, E02).
    DecodedFull,
    /// Cached partially-decoded raw state (E03 `rawcache/`).
    CachedRawState,
}

/// Source pixels handed to the engine (spec §3.8). The engine never decodes.
#[derive(Clone, Debug)]
pub struct SourceImage {
    /// The pixels (8/16/f32 depth, [`crate::ng::PixelFormat`]).
    pub pixels: PixelBuf,
    /// The colorimetry of `pixels` as delivered. **Drives the input transform**
    /// the source lift applies (see [`SourceColorimetry`]), a provider handing
    /// over display-referred file pixels must say so with
    /// [`SourceColorimetry::srgb`]; the default means "already working-space
    /// linear" and lifts through unchanged.
    pub colorimetry: SourceColorimetry,
    /// Full source resolution (may exceed `pixels` for a preview tier).
    pub full_extent: Extent,
    /// Which tier produced these pixels.
    pub quality: SourceQuality,
}

/// E02/E03: the pixels-in seam (spec §3.8). Implemented by the source stack.
pub trait SourceProvider: Send + Sync {
    /// Fetch `want` for `image`, honoring `cancel`.
    fn fetch(
        &self,
        image: ImageId,
        want: SourceWant,
        cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>>;
}

/// E03: completed 1:1 tiles offered for T2 persistence, fire-and-forget; E03
/// owns the store + eviction (spec §3.8). Implemented by the preview stack.
pub trait TileSink: Send + Sync {
    /// Offer a completed post-display-transform tile for T2 caching.
    fn offer_t2(&self, image: ImageId, recipe_hash: blake3::Hash, tile: TileCoord, px: PixelBuf);
}

/// Uploads decoded [`SourceImage`] pixels (8/16/f32) into a working-format
/// tile, applying the **source→working input transform** named by the image's
/// [`SourceColorimetry`] (spec task **A10**; E02 §"Decoded display-referred,
/// ICC-tagged (untagged ⇒ assumed sRGB) → working space (linearized) → same
/// downstream pipeline").
#[derive(Default)]
pub struct Uploader {}

impl Uploader {
    /// A new uploader.
    pub fn new() -> Uploader {
        Uploader::default()
    }

    /// Upload `src` onto `ctx`'s device as a working-format (`rgba16float`)
    /// tile (task A10). Source depth (8-bit / 16-bit-float / 32-bit-float) is
    /// normalized, then the input transform for `src.colorimetry` is applied so
    /// the tile genuinely holds the `LinearRgbaF16` its port type declares.
    /// For the default [`SourceColorimetry::WORKING_LINEAR`] tag that transform
    /// is the identity, so engine-internal sources are still lossless within
    /// format quantization.
    ///
    /// The source stage gets a **dedicated** texture (not a tile-pool loan): it
    /// is long-lived and RAM-pinned (task B4), so it must not be reclaimed under
    /// working-set pressure.
    pub fn upload(&self, ctx: &DeviceCtx, src: &SourceImage) -> TileHandle {
        let extent = src.pixels.extent;
        let w = extent.w.max(1);
        let h = extent.h.max(1);
        let working = to_working_f16(&src.pixels, &src.colorimetry);

        let texture = Arc::new(
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lightbox source tile"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    .union(wgpu::TextureUsages::STORAGE_BINDING)
                    .union(wgpu::TextureUsages::COPY_DST)
                    .union(wgpu::TextureUsages::COPY_SRC),
                view_formats: &[],
            }),
        );
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &working,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8), // rgba16float = 8 bytes/px
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        // Ensure the upload has landed before the handle is used as an input.
        ctx.queue.submit(std::iter::empty());
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        TileHandle::gpu(
            texture,
            view,
            Extent { w, h },
            TilePrecision::F16,
            wgpu::TextureFormat::Rgba16Float,
        )
    }
}

/// Lifts a decoded [`SourceImage`] into a CPU working tile, the CPU-path
/// counterpart of [`Uploader::upload`]. Both call the **same**
/// [`to_working_f16`] with the same [`SourceColorimetry`], so the CPU and GPU
/// source stages carry byte-for-byte identical working pixels (the
/// single-backend-determinism anchor of the A9 gate), the input transform
/// cannot drift between backends because there is only one implementation of
/// it, evaluated on the host before either backend sees a byte.
pub fn to_working_tile_cpu(src: &SourceImage) -> PixelBuf {
    let w = src.pixels.extent.w.max(1);
    let h = src.pixels.extent.h.max(1);
    PixelBuf {
        bytes: to_working_f16(&src.pixels, &src.colorimetry),
        format: PixelFormat::Rgba16F,
        extent: Extent { w, h },
        stride: w * 8, // rgba16float = 8 bytes/px
    }
}

/// The baked source→working input transform for one [`SourceColorimetry`]: a
/// per-code-point decode table for 8-bit sources plus an optional primaries
/// matrix. Built once per lift (a 3×3 inverse and 256 transfer evaluations),
/// then applied per pixel, so the hot path costs a table lookup and, at most,
/// one matrix multiply. **All color math comes from `lightbox-color`** (E02
/// guardrail): this type only composes `spaces::srgb_eotf` and
/// `spaces::linear_srgb_to_working`.
struct SourceLift {
    /// Decode table for 8-bit code points → linear channel value. Identity
    /// (`k/255`) when the source transfer is already linear.
    decode8: [f32; 256],
    /// Source primaries → working primaries, or `None` when already working.
    to_working: Option<[[f32; 3]; 3]>,
    /// The transfer decode for float channels, or `None` when the source is
    /// already linear. Named rather than boolean since E02's P3/AdobeRGB work:
    /// there are now four distinct encodings a tagged file can arrive in.
    decode_float: Option<fn(f32) -> f32>,
}

impl SourceLift {
    fn new(colorimetry: &SourceColorimetry) -> SourceLift {
        use lightbox_color::matrix::spaces;

        // Which `lightbox-color` function decodes this source's encoding. The
        // engine picks the name; the crate owns the curve.
        let decode_float: Option<fn(f32) -> f32> = match colorimetry.transfer {
            TransferFunction::Linear => None,
            TransferFunction::Srgb => Some(spaces::srgb_eotf),
            TransferFunction::AdobeRgb => Some(spaces::adobe_rgb_eotf),
            TransferFunction::Rec709 => Some(spaces::rec709_eotf),
            TransferFunction::ProPhoto => Some(spaces::prophoto_eotf),
        };
        let mut decode8 = [0.0f32; 256];
        for (k, slot) in decode8.iter_mut().enumerate() {
            let v = k as f32 / 255.0;
            *slot = match decode_float {
                Some(eotf) => eotf(v),
                None => v,
            };
        }

        // Likewise for primaries: `None` is the identity, which is both the
        // engine-internal default AND a ProPhoto-tagged file (same primaries as
        // the working space, only its transfer function differs).
        let to_working = match colorimetry.primaries {
            ColorPrimaries::Working => None,
            ColorPrimaries::Srgb => Some(spaces::linear_srgb_to_working()),
            ColorPrimaries::DisplayP3 => Some(spaces::linear_display_p3_to_working()),
            ColorPrimaries::AdobeRgb => Some(spaces::linear_adobe_rgb_to_working()),
            ColorPrimaries::Rec2020 => Some(spaces::linear_rec2020_to_working()),
        }
        .map(|m| {
            let m = m.0;
            [
                [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
                [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
                [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
            ]
        });

        SourceLift {
            decode8,
            to_working,
            decode_float,
        }
    }

    /// Decodes a normalized float channel (16-/32-bit sources only; the 8-bit
    /// path reads [`SourceLift::decode8`] directly).
    ///
    /// The indirect call through the stored `fn` pointer is deliberate and not
    /// a hot-path concern: the display-referred sources that need decoding all
    /// arrive 8-bit and go through the table instead, float sources are
    /// engine-internal and tagged `WORKING_LINEAR` (`None`, no call at all), and
    /// where it does run, one `powf` dwarfs one indirect jump.
    fn decode_channel(&self, v: f32) -> f32 {
        match self.decode_float {
            Some(eotf) => eotf(v),
            None => v,
        }
    }

    /// Applies the primaries conversion to an already-linearized RGBA. Alpha is
    /// never color-managed.
    ///
    /// **Deliberately unclamped.** Every source space we accept sits inside
    /// ProPhoto/ROMM except for one boundary case: Display-P3's red primary lies
    /// exactly on ROMM's red→green edge, so after the D65→D50 Bradford
    /// adaptation a fully saturated P3 red lands ~0.13 % *outside* it (a working
    /// blue of about −0.0013). The working space is scene-linear f16 and every
    /// develop stage handles a slightly negative channel fine; clamping here
    /// would instead quietly desaturate the reddest pixels an iPhone can
    /// record. Clipping happens at the display transform, where it belongs.
    /// Pinned by `lightbox_color::matrix::tests::source_spaces_sit_inside_the_working_gamut_to_within_a_rounding_error`.
    fn to_working_primaries(&self, rgba: [f32; 4]) -> [f32; 4] {
        match &self.to_working {
            None => rgba,
            Some(m) => [
                m[0][0] * rgba[0] + m[0][1] * rgba[1] + m[0][2] * rgba[2],
                m[1][0] * rgba[0] + m[1][1] * rgba[1] + m[1][2] * rgba[2],
                m[2][0] * rgba[0] + m[2][1] * rgba[1] + m[2][2] * rgba[2],
                rgba[3],
            ],
        }
    }
}

/// Packs a source [`PixelBuf`] (8-bit / 16-bit-float / 32-bit-float) into tightly
/// packed `rgba16float` **working-space** bytes, honoring the source row
/// `stride`. Channel values are normalized, linearized, and converted to the
/// working primaries per `colorimetry`; for the default
/// [`SourceColorimetry::WORKING_LINEAR`] tag every step is the identity and the
/// values are carried through as-is (8-bit normalized by /255), exactly as
/// before this stage existed.
fn to_working_f16(src: &PixelBuf, colorimetry: &SourceColorimetry) -> Vec<u8> {
    let w = src.extent.w.max(1) as usize;
    let h = src.extent.h.max(1) as usize;
    let stride = src.stride as usize;
    let mut out = vec![0u8; w * h * 8];
    let lift = SourceLift::new(colorimetry);

    let put = |out: &mut [u8], idx: usize, rgba: [f32; 4]| {
        let base = idx * 8;
        for (c, &v) in rgba.iter().enumerate() {
            let b = f16::from_f32(v).to_le_bytes();
            out[base + c * 2] = b[0];
            out[base + c * 2 + 1] = b[1];
        }
    };

    for y in 0..h {
        let row = &src.bytes[y * stride..];
        for x in 0..w {
            let rgba = read_source_pixel(row, x, src.format, &lift);
            put(&mut out, y * w + x, lift.to_working_primaries(rgba));
        }
    }
    out
}

/// Reads pixel `x` of `row` in `format` as **linearized** RGBA f32, applying
/// `lift`'s transfer decode (8-bit via its 256-entry table, floats via the
/// transfer function). Primaries are converted by
/// [`SourceLift::to_working_primaries`] afterwards. Alpha is normalized but
/// never decoded, it is not a color channel.
fn read_source_pixel(row: &[u8], x: usize, format: PixelFormat, lift: &SourceLift) -> [f32; 4] {
    match format {
        PixelFormat::Rgba8Unorm | PixelFormat::Rgba8Srgb => {
            let i = x * 4;
            [
                lift.decode8[usize::from(row[i])],
                lift.decode8[usize::from(row[i + 1])],
                lift.decode8[usize::from(row[i + 2])],
                f32::from(row[i + 3]) / 255.0,
            ]
        }
        PixelFormat::Rgba16F => {
            let i = x * 8;
            let ch = |o: usize| f16::from_le_bytes([row[i + o], row[i + o + 1]]).to_f32();
            [
                lift.decode_channel(ch(0)),
                lift.decode_channel(ch(2)),
                lift.decode_channel(ch(4)),
                ch(6),
            ]
        }
        PixelFormat::Rgba32F => {
            let i = x * 16;
            let ch = |o: usize| {
                f32::from_le_bytes([row[i + o], row[i + o + 1], row[i + o + 2], row[i + o + 3]])
            };
            [
                lift.decode_channel(ch(0)),
                lift.decode_channel(ch(4)),
                lift.decode_channel(ch(8)),
                ch(12),
            ]
        }
        // A single-channel mask weight is not a color channel: never decoded,
        // and `to_working_primaries` leaves it alone because a mask source is
        // always tagged `WORKING_LINEAR`.
        PixelFormat::R16F => {
            let i = x * 2;
            let r = f16::from_le_bytes([row[i], row[i + 1]]).to_f32();
            [r, 0.0, 0.0, 1.0]
        }
    }
}
