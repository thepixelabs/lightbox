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
//! # White balance is part of step 1, not a later correction
//!
//! The camera-to-working matrix that [`resolve_input_transform`] bakes already
//! carries the white point: [`ColorimetricSolver::cam_to_xyz_d50`] maps the
//! chosen neutral onto D50, so the whole of white balance lives in that one
//! matrix. This is
//! why the Temp slider on a raw file can read an absolute Kelvin value the way
//! Lightroom's does, the number is not a UI scale, it is the white point the
//! camera's own `ColorMatrix1`/`ColorMatrix2` are interpolated at.
//!
//! [`decode`](RawSourceProvider::decode) therefore takes a [`WbMode`] rather
//! than assuming [`WbMode::AsShot`], and [`wb_mode_for`] is what turns the
//! recipe's white-balance leaf into one. `AsShot` still resolves through
//! exactly the code path it always did, so a raw file with no white balance
//! set decodes byte for byte as before (measured: the same 98,957,600 bytes
//! for `fixtures/fujifilm-x100.raf`).
//!
//! **What the router actually passes is narrower than that today**, because
//! the render graph white-balances a second time. See
//! `render_source::source_wb_for`, which owns that decision and carries the
//! evidence.
//!
//! Each decode also reports the file's own as-shot white point in absolute
//! `(Kelvin, tint)`, [`AsShotWhiteBalance`], because that is the value the
//! Temp/Tint sliders must start at for the reading to mean anything. It comes
//! from the same solver the matrix does, so the number the user sees and the
//! matrix the pixels went through can never disagree.
//!
//! # Highlight latitude, and where it survives
//!
//! The pixels this provider emits are **not bounded by 1**, deliberately. A
//! sensor's channels all saturate at the same raw level, but a scene is not
//! neutral at that level, so under daylight green reaches the white level
//! roughly a stop before red does. A highlight that has clipped green is
//! therefore still carrying real, varying, unclipped red and blue, and that
//! difference is the raw file's highlight latitude.
//!
//! Nothing above throws it away. The proxy applies no white balance, so each
//! channel keeps its own distance below the white level (step 1's matrix is
//! what folds white balance back in, and so what turns that slack into
//! working values above 1); `Curve1D::eval` extends the default look past 1
//! rather than clamping there; and the `Rgba16F` pack is a plain
//! `f16::from_f32`, which carries to ~65504. On `canon-eos-350d.cr2` 2.29 %
//! of the frame leaves here above 1, peaking at 2.28, about 1.2 stops.
//!
//! `global.tone_recovery` is what spends it: its highlight weight saturates
//! above 0.72 luma, so a positive `highlights` scales that whole region by one
//! constant factor, which maps the above-1 structure back into range with its
//! relative differences intact. Without the latitude that node has a flat
//! ceiling to work on and can only make white into grey.
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
use lightbox_color::profile::{camera_matrix_base, ColorimetricSolver};
use lightbox_color::transform::resolve_input_transform;
use lightbox_color::wb::wb_presets;
use lightbox_color::WbMode;
use lightbox_decode::{
    decode_for_develop, normalize_camera, DecodeOpts, ProxySupervisor, RawDecode, SourceColor,
};
use lightbox_edit::{WbPreset as EditWbPreset, WhiteBalance};
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

/// A raw file's own as-shot white point, in absolute units.
///
/// Solved through the camera's `ColorMatrix1`/`ColorMatrix2` by
/// [`ColorimetricSolver::white_point`], the same solver that builds the
/// camera-to-working matrix, so this is the real Kelvin the shot was taken
/// at and not a scale invented by the UI. It is what the Temp and Tint
/// sliders read before the photographer has touched anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AsShotWhiteBalance {
    /// Correlated colour temperature, Kelvin.
    pub kelvin: f64,
    /// Green-magenta tint, orthogonal to the temperature axis.
    pub tint: f64,
}

/// One raw decode: the pixels, plus the file's own as-shot white point.
///
/// The two travel together because they come out of the same solve. Handing
/// back only the pixels is how the slider ended up with a placeholder Kelvin
/// that had nothing to do with the file in front of it.
pub struct RawDecoded {
    /// Working-space linear `Rgba16F`, upright.
    pub image: SourceImage,
    /// The file's own as-shot white point, whatever `wb` the decode used.
    ///
    /// `None` when the file carries no as-shot neutral to solve from. That
    /// is not a decode failure: with an explicit `(Kelvin, tint)` the pixels
    /// are perfectly well defined, there is simply no as-shot reading to put
    /// on the slider, and refusing a photograph over a missing readout would
    /// be the wrong trade.
    pub as_shot: Option<AsShotWhiteBalance>,
}

/// The recipe's white-balance leaf as the colour pipeline's [`WbMode`].
///
/// * `AsShot` keeps the file's own neutral, the byte-identical default.
/// * `Auto` also resolves to `AsShot`: no auto solve exists yet, and this
///   matches how the render graph already treats it
///   (`lightbox_render::ng::nodes::global::white_balance::resolve_temp_tint`
///   returns `None`, i.e. identity, for both).
/// * `Preset` reads [`wb_presets`], the one sourced CIE-illuminant table, so
///   a preset means the same temperature here as it does in the graph.
/// * `Custom` is already an absolute `(Kelvin, tint)` pair.
/// * An unknown variant (`WhiteBalance` is `#[non_exhaustive]`) falls back to
///   `AsShot` rather than guessing a temperature.
///
/// **Known duplicate.** `lightbox-render` carries the same six-name preset
/// map privately, because `lightbox-edit` and `lightbox-color` do not depend
/// on each other and only crates above both can bridge them. Both sides read
/// the same [`wb_presets`] table for the actual numbers, so only the naming
/// can drift; `every_preset_maps_to_its_sourced_cct` pins this side of it.
/// Two occurrences, so no extraction yet: a third earns a shared adapter.
pub fn wb_mode_for(wb: &WhiteBalance) -> WbMode {
    match wb {
        WhiteBalance::AsShot | WhiteBalance::Auto => WbMode::AsShot,
        WhiteBalance::Preset(preset) => {
            let want = match preset {
                EditWbPreset::Daylight => lightbox_color::wb::WbPreset::Daylight,
                EditWbPreset::Cloudy => lightbox_color::wb::WbPreset::Cloudy,
                EditWbPreset::Shade => lightbox_color::wb::WbPreset::Shade,
                EditWbPreset::Tungsten => lightbox_color::wb::WbPreset::Tungsten,
                EditWbPreset::Fluorescent => lightbox_color::wb::WbPreset::Fluorescent,
                EditWbPreset::Flash => lightbox_color::wb::WbPreset::Flash,
                // `WbPreset` is `#[non_exhaustive]` too.
                _ => lightbox_color::wb::WbPreset::Daylight,
            };
            match wb_presets().iter().find(|(p, _, _)| *p == want) {
                Some(&(_, kelvin, tint)) => WbMode::TempTint { kelvin, tint },
                None => WbMode::AsShot,
            }
        }
        WhiteBalance::Custom { temp_k, tint } => WbMode::TempTint {
            kelvin: *temp_k as f64,
            tint: *tint as f64,
        },
        _ => WbMode::AsShot,
    }
}

/// Shared record of what each image resolved to, written by the provider and
/// read by the shell.
pub(crate) type StatusMap = Arc<RwLock<HashMap<ImageId, RawSourceStatus>>>;

/// Shared record of each raw file's own as-shot white point, written by the
/// provider when it decodes and read by the shell to seed the Temp/Tint
/// sliders. Empty until the file has been decoded once, which is honest: the
/// as-shot value lives in the raw metadata, not in the catalog.
pub(crate) type AsShotMap = Arc<RwLock<HashMap<ImageId, AsShotWhiteBalance>>>;

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

    /// Decode `path` to working-space linear `Rgba16F`, upright, at `wb`.
    ///
    /// Public under a test-facing name so the narrow decode contract can be
    /// proven without standing up a `Session` and an engine.
    pub fn decode_for_test(
        &self,
        path: &Path,
        orientation: Orientation,
        wb: &WbMode,
        cancel: &CancelToken,
    ) -> Result<RawDecoded, RawFallbackReason> {
        self.decode(path, orientation, wb, cancel)
    }

    /// Decode `path` to working-space linear `Rgba16F`, upright, at `wb`.
    ///
    /// `wb` is the recipe's white balance, resolved by [`wb_mode_for`]. It
    /// selects the white point the camera-to-working matrix is built around,
    /// so a different `wb` is a genuinely different set of pixels, not a
    /// correction layered on afterwards. [`WbMode::AsShot`] is the default
    /// and takes the identical path it always did.
    pub(crate) fn decode(
        &self,
        path: &Path,
        orientation: Orientation,
        wb: &WbMode,
        _cancel: &CancelToken,
    ) -> Result<RawDecoded, RawFallbackReason> {
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
            wb,
            colorimetry.as_shot_neutral,
            look.as_ref(),
            1.0,
        )
        .map_err(|_| RawFallbackReason::NoCameraProfile)?;

        // The as-shot reading is solved separately from `wb`, so it stays the
        // file's own value no matter what the photographer has dialled in. It
        // is what "As Shot" on the mode combo means, and what the Temp slider
        // has to be able to return to.
        Ok(RawDecoded {
            image: to_upright_rgba16f(&src, &xform, orientation),
            as_shot: as_shot_white_balance(&profile, colorimetry.as_shot_neutral),
        })
    }
}

/// The file's own as-shot white point as absolute `(Kelvin, tint)`.
///
/// `None` when the file carries no as-shot neutral, or when the solver's
/// `NeutralToXY` iteration will not converge on this camera's matrices. Both
/// are the same answer as far as the caller is concerned: there is no honest
/// Kelvin to show, so the sensor path declines the file rather than printing
/// a number it made up.
fn as_shot_white_balance(
    profile: &lightbox_color::profile::CameraProfile,
    as_shot_neutral: Option<[f64; 3]>,
) -> Option<AsShotWhiteBalance> {
    let solver = ColorimetricSolver::new(profile).ok()?;
    let wp = solver.white_point(&WbMode::AsShot, as_shot_neutral).ok()?;
    Some(AsShotWhiteBalance {
        kelvin: wp.cct,
        tint: wp.tint,
    })
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

    /// `AsShot` is the recipe's own default, so an image nothing has edited
    /// must reach the decode as `WbMode::AsShot` and take the code path it
    /// always took. `Auto` joins it because no auto solve exists, which is
    /// also how the render graph treats it.
    #[test]
    fn the_neutral_modes_resolve_to_as_shot() {
        assert_eq!(wb_mode_for(&WhiteBalance::default()), WbMode::AsShot);
        assert_eq!(wb_mode_for(&WhiteBalance::AsShot), WbMode::AsShot);
        assert_eq!(wb_mode_for(&WhiteBalance::Auto), WbMode::AsShot);
    }

    /// A `Custom` value is already absolute, so it passes through unchanged.
    /// The whole point of the raw Temp slider is that the number it shows is
    /// the number the camera matrices are interpolated at, and a mapping that
    /// rescaled it here would quietly undo that.
    #[test]
    fn custom_passes_its_kelvin_through_untouched() {
        assert_eq!(
            wb_mode_for(&WhiteBalance::Custom {
                temp_k: 3210.0,
                tint: -17.5,
            }),
            WbMode::TempTint {
                kelvin: 3210.0,
                tint: -17.5,
            }
        );
    }

    /// Every preset resolves to the temperature the shared, sourced table
    /// gives it. Reading the table rather than repeating its numbers is the
    /// point: this side of the edit-to-colour bridge and `lightbox-render`'s
    /// side both go through `wb_presets`, so a preset means the same thing in
    /// both, and only the name mapping is duplicated.
    #[test]
    fn every_preset_maps_to_its_sourced_cct() {
        let pairs = [
            (
                EditWbPreset::Daylight,
                lightbox_color::wb::WbPreset::Daylight,
            ),
            (EditWbPreset::Cloudy, lightbox_color::wb::WbPreset::Cloudy),
            (EditWbPreset::Shade, lightbox_color::wb::WbPreset::Shade),
            (
                EditWbPreset::Tungsten,
                lightbox_color::wb::WbPreset::Tungsten,
            ),
            (
                EditWbPreset::Fluorescent,
                lightbox_color::wb::WbPreset::Fluorescent,
            ),
            (EditWbPreset::Flash, lightbox_color::wb::WbPreset::Flash),
        ];
        for (edit, color) in pairs {
            let &(_, kelvin, tint) = wb_presets()
                .iter()
                .find(|(p, _, _)| *p == color)
                .expect("every preset is in the table");
            assert_eq!(
                wb_mode_for(&WhiteBalance::Preset(edit)),
                WbMode::TempTint { kelvin, tint },
                "{edit:?}"
            );
        }
        // And the six really are six distinct temperatures, so a mapping that
        // collapsed to one arm would not pass the loop above by accident.
        let tungsten = wb_mode_for(&WhiteBalance::Preset(EditWbPreset::Tungsten));
        let shade = wb_mode_for(&WhiteBalance::Preset(EditWbPreset::Shade));
        assert_ne!(tungsten, shade, "tungsten and shade are not the same light");
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
