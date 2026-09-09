// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The M1 [`ng::DeviceProvider`] / [`ng::SourceProvider`] seam implementations
//! (spec §3.8), wiring `lightbox-render::ng` to the session's shared GPU
//! device and the embedded-preview pipeline (`lightbox-preview`).
//!
//! **E05 Phase F5.** This file previously implemented the E01 seed's
//! `SourceResolver` trait (`lightbox_render::SourceResolver`); the session
//! façade now drives the `ng` engine (`lightbox_render::ng::Engine`), so this
//! is a from-scratch implementation of the `ng` seam traits, not a
//! reshaping of the old one. The old seed's `SourceResolver` impl this file
//! used to hold is gone (the seed itself is untouched at the crate root, just
//! unused by this crate now, see `docs/plan/epics/E05-deviations.md`).
//!
//! Wiring, not a competing implementation: **E03** swaps the tiered on-disk
//! store in behind the same `SourceProvider` trait; **E02** already supplies
//! the raw-decode path upstream of the embedded-preview fallback used here.

use std::sync::Arc;

use lightbox_jobs::{CancelToken, Class};
use lightbox_preview::{
    PreviewClass, PreviewColorspace, PreviewError, PreviewProvider, PreviewState,
};
use lightbox_render::ng::{
    BoxFuture, DeviceError, DeviceProvider, SourceColorimetry, SourceError, SourceImage,
    SourceProvider, SourceQuality, SourceWant,
};
use lightbox_render::ng::{Extent, PixelBuf, PixelFormat};
use lightbox_render::GpuContext;
use lightbox_types::ImageId;

/// [`DeviceProvider`] over the session's already-acquired shared device
/// (spec §2.3 seam 2 / §3.8): the engine renders on the *shell's* device, it
/// never creates its own under the shell.
///
/// **F5 deviation (recorded in `E05-deviations.md`):** [`DeviceProvider::rebuild`]
/// returns a typed [`DeviceError::Rebuild`] rather than actually
/// reacquiring a device. The shell's `wgpu::Device` is owned by eframe's
/// `egui-wgpu` integration (created once at `CreationContext` time); handing
/// the render engine a way to recreate *that* device would need deeper
/// eframe integration than this M1 integration pass scopes. Phase E's
/// device-lost recovery state machine (rebuild → re-warm → degrade-to-CPU)
/// is fully implemented and gated on a real, swappable `DeviceProvider` in
/// `lightbox-render`'s own test suite (`tests/ng_recover.rs`); wiring a live
/// shell-side reacquisition path is follow-up work, not a correctness gap in
/// the engine itself, a real device loss on this seam degrades the session
/// to CPU-preview-only (§4.4 contract) rather than recovering to GPU.
pub(crate) struct SharedDeviceProvider {
    gpu: GpuContext,
}

impl SharedDeviceProvider {
    pub(crate) fn new(gpu: GpuContext) -> SharedDeviceProvider {
        SharedDeviceProvider { gpu }
    }
}

impl DeviceProvider for SharedDeviceProvider {
    fn current(&self) -> (Arc<wgpu::Device>, Arc<wgpu::Queue>) {
        (self.gpu.device.clone(), self.gpu.queue.clone())
    }

    fn rebuild(
        &self,
    ) -> BoxFuture<'static, Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "lightbox-core: the shell's eframe-owned device cannot be reacquired by the \
                 session façade at M1 (F5 deviation — see E05-deviations.md); the session \
                 degrades to CPU-preview-only on device loss instead of recovering to GPU"
                    .to_owned(),
            ))
        })
    }

    fn adapter_info(&self) -> Option<wgpu::AdapterInfo> {
        Some(self.gpu.adapter_info.clone())
    }
}

/// A [`DeviceProvider`] for CPU-only sessions (no adapter, or `--cpu`
/// forced): `EngineConfig { backend: BackendPref::ForceCpu, .. }` never calls
/// [`DeviceProvider::current`]/[`DeviceProvider::rebuild`] (the `Engine`
/// selects the CPU backend without consulting the seam at all), so this
/// exists only to satisfy the constructor's `Arc<dyn DeviceProvider>`
/// parameter, reaching either method would be an engine bug, not a runtime
/// condition, so both panic loudly rather than fabricating a device.
pub(crate) struct NullDeviceProvider;

impl DeviceProvider for NullDeviceProvider {
    fn current(&self) -> (Arc<wgpu::Device>, Arc<wgpu::Queue>) {
        unreachable!("BackendPref::ForceCpu never calls DeviceProvider::current")
    }

    fn rebuild(
        &self,
    ) -> BoxFuture<'static, Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>), DeviceError>> {
        Box::pin(async {
            Err(DeviceError::Rebuild(
                "CPU-only session has no device to rebuild".to_owned(),
            ))
        })
    }
}

fn map_preview_err(e: PreviewError) -> SourceError {
    match e {
        PreviewError::NoEmbedded => SourceError::NotFound,
        PreviewError::Cancelled => SourceError::Cancelled,
        PreviewError::Io(m) => SourceError::Upstream(format!("io: {m}")),
        PreviewError::Decode(m) => SourceError::Upstream(format!("decode: {m}")),
        // Unavailable + future variants: an upstream failure as far as the
        // engine is concerned.
        other => SourceError::Upstream(other.to_string()),
    }
}

/// Resolves `ng::Engine` source pixels through the session's preview provider
/// (M1: [`lightbox_preview::EmbeddedPreviewProvider`]).
///
/// **F5 note on orientation (recorded in `E05-deviations.md`):** the E01
/// seed's `display.transform` node applied EXIF orientation as part of its
/// params; the `ng` engine's `xform.display` node does not, geometry
/// (crop/rotate/orientation) is explicitly E11's job (`output_extent`
/// coordinate-frame changes, spec §1.1 non-goals table). This provider hands
/// the engine unoriented pixels; portrait-shot sources render sideways under
/// `ng` at M1 until E11 lands orientation/geometry nodes. This is a known,
/// spec-sanctioned gap, not a regression this task papers over.
///
/// **Colorimetry, this is where a file's colour space becomes an engine tag.**
/// [`lightbox_preview::DecodedImage`] carries the space its pixels are actually
/// encoded in ([`PreviewColorspace`], resolved from the container's embedded ICC
/// profile), and [`colorimetry_for`] maps it to the [`SourceColorimetry`] the
/// engine's source lift keys its input transform on. The lift linearizes and
/// converts to the working space before the graph's root port (E02 spec:
/// "Decoded display-referred, ICC-tagged (untagged ⇒ assumed sRGB) → working
/// space (linearized) → same downstream pipeline").
///
/// Before any tag existed these bytes were reinterpreted as if they were
/// *already* working-space linear and then re-encoded by `xform.display`, which
/// double-encoded every rendered image, sRGB mid-grey 128 came back at ~187.
/// Then the tag existed but was hardcoded to sRGB, so a Display-P3 file, which
/// is what every recent iPhone writes, was converted with sRGB's *narrower*
/// primaries, which names a duller colour than the file intends: P3 photos
/// rendered **flat**. (Worth stating precisely, because it is usually described
/// the other way round. sRGB content shown as P3 over-saturates; a P3 file
/// misread as sRGB desaturates. Measured: a saturated green `(70, 150, 80)`
/// moved 51 code points of red between the two readings.) This provider now
/// passes the file's real space through.
///
/// **This is the boundary that owns the "untagged ⇒ assumed sRGB" rule**
/// (spec §5.3). `SourceColorimetry::default()` deliberately means "already
/// working-space linear" for the engine's own synthetic sources, so the
/// file-shaped default has to be applied where file tags exist, here.
pub(crate) struct PreviewSourceProvider {
    previews: Arc<dyn PreviewProvider>,
}

/// Maps a decoded preview's colour space to the engine's input-transform tag.
///
/// # The two fallbacks, and why they are the same answer
///
/// [`PreviewColorspace::TaggedIcc`] means the container carried a profile that
/// could not be resolved to a space this build can name (a display calibration
/// profile, an exotic RGB space, unparseable bytes). An untagged file carries no
/// profile at all. Both become [`SourceColorimetry::SRGB`]: the spec's documented
/// assumption, and, for the unresolved-profile case, exactly the behaviour
/// every tagged file got before this mapping existed, so an unrecognized profile
/// can only ever render no worse than it used to.
///
/// Note what this function must never do: fail. A render is not allowed to die
/// because a user's file has a strange ICC tag.
fn colorimetry_for(space: PreviewColorspace) -> SourceColorimetry {
    match space {
        PreviewColorspace::Srgb => SourceColorimetry::SRGB,
        PreviewColorspace::DisplayP3 => SourceColorimetry::DISPLAY_P3,
        PreviewColorspace::AdobeRgb => SourceColorimetry::ADOBE_RGB,
        PreviewColorspace::ProPhoto => SourceColorimetry::PROPHOTO,
        PreviewColorspace::Rec2020 => SourceColorimetry::REC2020,
        // There is a profile and nobody could name it.
        PreviewColorspace::TaggedIcc => SourceColorimetry::SRGB,
        // `PreviewColorspace` is `#[non_exhaustive]`, so a variant added there
        // cannot be matched exhaustively from this crate. Assume sRGB rather
        // than guess: teaching this map a new space is a deliberate edit, not
        // something to infer.
        _ => SourceColorimetry::SRGB,
    }
}

impl PreviewSourceProvider {
    pub(crate) fn new(previews: Arc<dyn PreviewProvider>) -> PreviewSourceProvider {
        PreviewSourceProvider { previews }
    }
}

impl SourceProvider for PreviewSourceProvider {
    /// `want` is not yet tiered at M1: the loupe rendition is always the
    /// largest embedded preview (the engine's `util.resize` downsamples for
    /// `Fit`/`Ratio` scales). E03's tiered store picks a tier by `want`
    /// behind this same seam.
    fn fetch(
        &self,
        image: ImageId,
        _want: SourceWant,
        cancel: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let previews = Arc::clone(&self.previews);
        let cancel = cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let ticket = previews.request(image, PreviewClass::Loupe, Class::Interactive);
            loop {
                if cancel.is_cancelled() {
                    previews.cancel(&ticket);
                    return Err(SourceError::Cancelled);
                }
                match previews.poll(&ticket) {
                    PreviewState::Pending => {
                        std::thread::sleep(std::time::Duration::from_micros(500))
                    }
                    PreviewState::Ready(img) => {
                        let extent = Extent {
                            w: img.width,
                            h: img.height,
                        };
                        let pixels = PixelBuf {
                            bytes: img.px.to_vec(),
                            format: PixelFormat::Rgba8Srgb,
                            extent,
                            stride: img.width.saturating_mul(4),
                        };
                        return Ok(SourceImage {
                            pixels,
                            // The file's own space, resolved from its embedded
                            // ICC profile upstream. NOT `default()` (which means
                            // "already working-space linear"): these are encoded
                            // file pixels.
                            colorimetry: colorimetry_for(img.colorspace),
                            full_extent: extent,
                            quality: SourceQuality::Preview,
                        });
                    }
                    PreviewState::Failed(e) => return Err(map_preview_err(e)),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_render::ng::{ColorPrimaries, TransferFunction};

    /// Every space the preview stack can report maps to the matching engine
    /// tag. This function is the whole boundary between "what the file says"
    /// and "what the renderer does", so a wrong arm here is a silently
    /// mis-rendered image.
    #[test]
    fn every_preview_colorspace_maps_to_its_engine_tag() {
        assert_eq!(
            colorimetry_for(PreviewColorspace::Srgb),
            SourceColorimetry::SRGB,
        );
        assert_eq!(
            colorimetry_for(PreviewColorspace::DisplayP3),
            SourceColorimetry::DISPLAY_P3,
        );
        assert_eq!(
            colorimetry_for(PreviewColorspace::AdobeRgb),
            SourceColorimetry::ADOBE_RGB,
        );
        assert_eq!(
            colorimetry_for(PreviewColorspace::ProPhoto),
            SourceColorimetry::PROPHOTO,
        );
        assert_eq!(
            colorimetry_for(PreviewColorspace::Rec2020),
            SourceColorimetry::REC2020,
        );
    }

    /// **Spec §5.3, pinned.** An untagged file reaches this boundary as
    /// [`PreviewColorspace::Srgb`], and a tagged-but-unnamable one as
    /// `TaggedIcc`; both must become display-referred sRGB. Neither may become
    /// `WORKING_LINEAR`, which would resurrect the double-encode bug, these
    /// are encoded file pixels, never working-space ones.
    #[test]
    fn untagged_and_unnamable_both_mean_assumed_srgb() {
        for space in [PreviewColorspace::Srgb, PreviewColorspace::TaggedIcc] {
            let tag = colorimetry_for(space);
            assert_eq!(tag, SourceColorimetry::SRGB, "{space:?}");
            assert!(
                !tag.is_working_linear(),
                "{space:?} must never lift as if it were already working-space linear",
            );
        }
    }

    /// The P3 tag really does select different colour maths, not just a
    /// different label, a guard against the mapping compiling but collapsing
    /// to sRGB.
    #[test]
    fn the_display_p3_tag_selects_p3_primaries() {
        let tag = colorimetry_for(PreviewColorspace::DisplayP3);
        assert_eq!(tag.primaries, ColorPrimaries::DisplayP3);
        assert_eq!(tag.transfer, TransferFunction::Srgb, "P3 shares sRGB's TRC");
        assert_ne!(
            tag,
            colorimetry_for(PreviewColorspace::Srgb),
            "a Display-P3 file must not resolve to the sRGB transform",
        );
    }
}
