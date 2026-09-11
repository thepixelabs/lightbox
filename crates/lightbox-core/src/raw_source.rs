// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Sensor pixels for the editor.
//!
//! Until this existed, the editor graded the finished JPEG the camera embedded
//! in a raw file. The sensor path was built and shipped and had exactly one
//! caller, `lightbox-cli`, so a photographer pulling a highlight back in the
//! editor was working on eight-bit rendered pixels with nothing left to
//! recover. This is the provider that closes that gap.
//!
//! # What the proxy hands over, and the four things that must happen to it
//!
//! [`lightbox_decode::decode_for_develop`] returns camera-native linear RGB:
//! AHD-demosaiced, normalized to `[0,1]`, with **no white balance applied at
//! all** (`user_mul` is all ones and `use_camera_wb` is zero), in **sensor
//! orientation**, and with the EXIF orientation dropped by the proxy client.
//! Those pixels are not displayable and are not what the graph expects.
//!
//! 1. **Camera matrices.** [`camera_matrix_base`] builds a profile from the
//!    file's own `RawColorimetry`, and [`resolve_input_transform`] turns that
//!    into camera-native to working-space linear.
//! 2. **The default look.** Sensor-linear pixels through `xform.display` with
//!    no tone curve render flat and dark. This is not a stylistic choice: it is
//!    what makes a raw file look like a photograph rather than a scan. Lightbox
//!    already ships the answer and `default_look_applies(true)` says to use it.
//! 3. **EXIF orientation, baked.** Not optional.
//!    `EmbeddedPreviewProvider` always bakes it and the canvas assumes the
//!    result is upright. A raw provider that skips this renders every portrait
//!    shot sideways.
//! 4. **Pack as `Rgba16F`** tagged working-linear, so the graph's source lift
//!    passes it through unchanged rather than trying to linearise it twice.
//!
//! # Failure is disclosed, never silent
//!
//! The proxy can be missing or can crash on a file no one has tested. When that
//! happens this provider records a [`RawFallbackReason`] and the caller falls
//! back to the embedded preview **with the reason attached**. Silently serving
//! the preview instead is precisely the behaviour that made the editor dishonest
//! in the first place, and it is not repeated here.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use lightbox_color::look::{author_lightbox_color_v1, default_look_applies};
use lightbox_color::profile::camera_matrix_base;
use lightbox_color::transform::resolve_input_transform;
use lightbox_color::WbMode;
use lightbox_decode::{
    decode_for_develop, normalize_camera, DecodeOpts, ProxySupervisor, RawDecode, SourceColor,
};
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    BoxFuture, Extent, PixelBuf, PixelFormat, SourceColorimetry, SourceError, SourceImage,
    SourceProvider, SourceQuality, SourceWant,
};
use lightbox_types::{ImageId, Orientation};
use rayon::prelude::*;

/// Where the pixels the editor is showing for a raw file actually came from.
///
/// This is the honesty surface. The canvas badge reads it, and so does the
/// status notice, so the two can never disagree about what the user is looking
/// at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawSourceStatus {
    /// Not a raw file. Nothing to disclose.
    NotRaw,
    /// Real sensor data, at this resolution.
    Sensor { width: u32, height: u32 },
    /// The sensor path could not be used, and the camera's embedded preview is
    /// being shown instead. The reason is carried so it can be said out loud.
    FellBack { reason: RawFallbackReason },
}

/// Why a raw file is being shown from its embedded preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawFallbackReason {
    /// No decode proxy was found next to the executable and none was named by
    /// `LIGHTBOX_RAWPROXY_BIN`. A build without `--features libraw`, usually.
    ProxyUnavailable,
    /// The proxy was there and the decode failed. Carries what it said.
    DecodeFailed(String),
    /// The file has no camera colour matrices, so there is nothing to convert
    /// sensor values through.
    NoCameraProfile,
}

impl RawFallbackReason {
    /// One sentence, for a status notice. Written for a photographer, not for
    /// a log.
    pub fn describe(&self) -> String {
        match self {
            RawFallbackReason::ProxyUnavailable => "This build has no raw decoder, so Lightbox \
                 is showing the preview your camera embedded rather than the sensor data."
                .to_owned(),
            RawFallbackReason::DecodeFailed(why) => format!(
                "Lightbox could not read the sensor data in this file, so it is showing the \
                 preview your camera embedded instead. The decoder said: {why}"
            ),
            RawFallbackReason::NoCameraProfile => "This file carries no camera colour matrices, \
                 so Lightbox is showing the preview your camera embedded rather than converting \
                 sensor values with no way to interpret them."
                .to_owned(),
        }
    }
}

/// Shared record of what each image resolved to, written by the provider and
/// read by the shell.
pub(crate) type StatusMap = Arc<RwLock<HashMap<ImageId, RawSourceStatus>>>;

/// Turns a raw file into working-space linear pixels the graph can grade.
pub struct RawSourceProvider {
    sup: Arc<ProxySupervisor>,
}

impl RawSourceProvider {
    /// Builds a provider if a decode proxy can be found, otherwise `None`.
    ///
    /// Returning `None` rather than erroring is deliberate: a build without the
    /// proxy is a legitimate development configuration, and the router simply
    /// keeps serving embedded previews, with the reason disclosed.
    pub fn autodetect() -> Option<RawSourceProvider> {
        ProxySupervisor::autodetect().map(|sup| RawSourceProvider { sup: Arc::new(sup) })
    }

    /// Decode `path` to working-space linear `Rgba16F`, upright.
    ///
    /// Public under a test-facing name so the narrow decode contract can be
    /// proven without standing up a `Session` and an engine.
    pub fn decode_for_test(
        &self,
        path: &Path,
        orientation: Orientation,
        cancel: &CancelToken,
    ) -> Result<SourceImage, RawFallbackReason> {
        self.decode(path, orientation, cancel)
    }

    /// Decode `path` to working-space linear `Rgba16F`, upright.
    pub(crate) fn decode(
        &self,
        path: &Path,
        orientation: Orientation,
        _cancel: &CancelToken,
    ) -> Result<SourceImage, RawFallbackReason> {
        // `interim_demosaic` is what asks the proxy for develop-ready
        // demosaiced camera RGB rather than the mosaic.
        let opts = DecodeOpts {
            interim_demosaic: true,
            ..DecodeOpts::default()
        };
        let decoded = decode_for_develop(&self.sup, path, &opts)
            .map_err(|e| RawFallbackReason::DecodeFailed(e.to_string()))?;

        let src = match decoded {
            RawDecode::DemosaicedInterim(s) | RawDecode::Linear(s) => s,
            other => {
                return Err(RawFallbackReason::DecodeFailed(format!(
                    "unexpected decode result: {other:?}"
                )))
            }
        };

        let SourceColor::CameraNative(colorimetry) = &src.color else {
            return Err(RawFallbackReason::NoCameraProfile);
        };

        // Camera-native linear to working-space linear, through this file's own
        // matrices and the default look. `probe` is cheap next to the decode we
        // have already paid for, and it is where make and model live.
        let probe = lightbox_decode::probe(path)
            .map_err(|e| RawFallbackReason::DecodeFailed(e.to_string()))?;
        let camera = normalize_camera(
            probe.camera_make.as_deref().unwrap_or(""),
            probe.camera_model.as_deref().unwrap_or(""),
        );
        let profile = camera_matrix_base(colorimetry, &camera)
            .map_err(|_| RawFallbackReason::NoCameraProfile)?;
        let look = default_look_applies(true).then(author_lightbox_color_v1);
        let xform = resolve_input_transform(
            &profile,
            &WbMode::AsShot,
            colorimetry.as_shot_neutral,
            look.as_ref(),
            1.0,
        )
        .map_err(|_| RawFallbackReason::NoCameraProfile)?;

        Ok(to_upright_rgba16f(&src, &xform, orientation))
    }
}

/// Applies the input transform per pixel, bakes `orientation`, and packs the
/// result as `Rgba16F` tagged working-linear.
///
/// The orientation mapping deliberately reuses the convention in
/// `lightbox_render::nodes::display_transform::orient_map`: iterate the
/// **destination** and pull from the source, rather than pushing source pixels
/// forward. Pulling cannot leave a hole, pushing can, and that table is already
/// proven across all eight cases by the display-transform goldens.
fn to_upright_rgba16f(
    src: &lightbox_decode::SourceImage,
    xform: &lightbox_color::transform::ResolvedInputTransform,
    orientation: Orientation,
) -> SourceImage {
    let (sw, sh) = (src.width, src.height);
    let ch = src.channels as usize;

    // Camera-native to working-space linear, in sensor orientation, because the
    // transform is per pixel and order-independent. Parallel because a 45
    // megapixel frame is 45 million matrix multiplies.
    let mut working: Vec<[f32; 3]> = vec![[0.0; 3]; (sw as usize) * (sh as usize)];
    working.par_iter_mut().enumerate().for_each(|(i, out)| {
        let o = i * ch;
        *out = xform.eval_cpu([src.data[o], src.data[o + 1], src.data[o + 2]]);
    });

    let (dw, dh) = if orientation.transposes() {
        (sh, sw)
    } else {
        (sw, sh)
    };
    let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba16F, Extent { w: dw, h: dh });
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = orient_map(orientation, dx, dy, sw, sh);
            let [r, g, b] = working[(sy as usize) * (sw as usize) + (sx as usize)];
            px.set_rgba_f32(dx, dy, [r, g, b, 1.0]);
        }
    }

    SourceImage {
        pixels: px,
        colorimetry: SourceColorimetry::WORKING_LINEAR,
        full_extent: Extent { w: dw, h: dh },
        quality: SourceQuality::Full,
    }
}

/// Upright coordinates to stored coordinates.
///
/// A copy of `lightbox_render::nodes::display_transform::orient_map`, which is
/// private to that crate. Copied rather than made public because that function
/// is the GPU path's CPU mirror and belongs to the display node; this is the
/// source path. The table below is identical and
/// `orientation_mapping_matches_the_display_transform` in the tests pins the
/// two together so they cannot drift.
fn orient_map(o: Orientation, x: u32, y: u32, sw: u32, sh: u32) -> (u32, u32) {
    match o {
        Orientation::O1 => (x, y),
        Orientation::O2 => (sw - 1 - x, y),
        Orientation::O3 => (sw - 1 - x, sh - 1 - y),
        Orientation::O4 => (x, sh - 1 - y),
        Orientation::O5 => (y, x),
        Orientation::O6 => (y, sh - 1 - x),
        Orientation::O7 => (sw - 1 - y, sh - 1 - x),
        Orientation::O8 => (sw - 1 - y, x),
    }
}

impl SourceProvider for RawSourceProvider {
    fn fetch(
        &self,
        _image: ImageId,
        _want: SourceWant,
        _cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        // The router resolves the path and orientation and calls `decode`
        // directly; this impl exists so the type satisfies the seam.
        Box::pin(async {
            Err(SourceError::Upstream(
                "RawSourceProvider is driven through the router, not fetched directly".to_owned(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orientation table here is a copy of the display node's. A copy that
    /// silently drifts would rotate raw files and nothing else, which is the
    /// hardest kind of bug to notice, so pin the two together.
    ///
    /// The reference values are the table in
    /// `lightbox_render::nodes::display_transform::orient_map`, transcribed
    /// independently rather than imported, because importing it would make this
    /// test tautological.
    #[test]
    fn orientation_mapping_matches_the_display_transform() {
        const SW: u32 = 4;
        const SH: u32 = 3;
        let reference = |o: Orientation, x: u32, y: u32| -> (u32, u32) {
            match o {
                Orientation::O1 => (x, y),
                Orientation::O2 => (SW - 1 - x, y),
                Orientation::O3 => (SW - 1 - x, SH - 1 - y),
                Orientation::O4 => (x, SH - 1 - y),
                Orientation::O5 => (y, x),
                Orientation::O6 => (y, SH - 1 - x),
                Orientation::O7 => (SW - 1 - y, SH - 1 - x),
                Orientation::O8 => (SW - 1 - y, x),
            }
        };
        for o in [
            Orientation::O1,
            Orientation::O2,
            Orientation::O3,
            Orientation::O4,
            Orientation::O5,
            Orientation::O6,
            Orientation::O7,
            Orientation::O8,
        ] {
            let (dw, dh) = if o.transposes() { (SH, SW) } else { (SW, SH) };
            for dy in 0..dh {
                for dx in 0..dw {
                    assert_eq!(
                        orient_map(o, dx, dy, SW, SH),
                        reference(o, dx, dy),
                        "{o:?} at ({dx},{dy})"
                    );
                }
            }
        }
    }

    /// Every destination pixel must read a source pixel that exists. A mapping
    /// that runs off the end is an out-of-bounds panic on a user's photograph.
    #[test]
    fn every_oriented_coordinate_lands_inside_the_source() {
        for (sw, sh) in [(4u32, 3u32), (3, 4), (1, 7), (7, 1), (16, 16)] {
            for o in [
                Orientation::O1,
                Orientation::O2,
                Orientation::O3,
                Orientation::O4,
                Orientation::O5,
                Orientation::O6,
                Orientation::O7,
                Orientation::O8,
            ] {
                let (dw, dh) = if o.transposes() { (sh, sw) } else { (sw, sh) };
                for dy in 0..dh {
                    for dx in 0..dw {
                        let (sx, sy) = orient_map(o, dx, dy, sw, sh);
                        assert!(
                            sx < sw && sy < sh,
                            "{o:?} {sw}x{sh}: ({dx},{dy}) -> ({sx},{sy}) is outside"
                        );
                    }
                }
            }
        }
    }

    /// A fallback must always be able to say why, in words a photographer can
    /// act on. An empty or debug-formatted reason is not a disclosure.
    #[test]
    fn every_fallback_reason_describes_itself() {
        for r in [
            RawFallbackReason::ProxyUnavailable,
            RawFallbackReason::DecodeFailed("proxy exited with signal 9".to_owned()),
            RawFallbackReason::NoCameraProfile,
        ] {
            let d = r.describe();
            assert!(d.len() > 40, "{r:?} describes itself too thinly: {d:?}");
            assert!(
                d.contains("preview"),
                "{r:?} must say what is being shown instead: {d:?}"
            );
        }
    }
}
