// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Colorimetry **tags** carried across the source/output seams (spec §3.4/§3.6/
//! §3.8).
//!
//! The engine still contains **zero color science** (E02 guardrail): these types
//! *name* a colorimetry, and every matrix/transfer function they imply is
//! supplied by `lightbox-color` (the engine-owned `xform.display` node calls it
//! for working→display; [`crate::ng::source`] calls it for source→working).
//! Nothing here computes color itself.

/// The transfer function (TRC) the source's channel values are encoded with.
///
/// A *name*, never an implementation: each variant points at the
/// `lightbox_color` function that decodes it, and [`crate::ng::source`] calls
/// that function. The engine owns no transfer maths of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransferFunction {
    /// Values are already linear in light, nothing to decode.
    #[default]
    Linear,
    /// Values carry the sRGB opto-electronic encoding (IEC 61966-2-1); decoding
    /// is [`lightbox_color::matrix::spaces::srgb_eotf`]. Display-P3 shares this
    /// curve.
    Srgb,
    /// Adobe RGB (1998)'s pure power law, exponent 563/256; decoding is
    /// [`lightbox_color::matrix::spaces::adobe_rgb_eotf`].
    AdobeRgb,
    /// The ITU-R BT.709 curve (which BT.2020 also uses at 8/10-bit); decoding
    /// is [`lightbox_color::matrix::spaces::rec709_eotf`].
    Rec709,
    /// ROMM RGB (ProPhoto): gamma 1.8 over a short linear toe; decoding is
    /// [`lightbox_color::matrix::spaces::prophoto_eotf`].
    ProPhoto,
}

/// The RGB primaries + white point the source's channels are expressed in.
///
/// Same contract as [`TransferFunction`]: a name whose matrix comes from
/// [`lightbox_color::matrix::spaces`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorPrimaries {
    /// The engine working space itself: ProPhoto/ROMM primaries, D50 white
    /// (§4.1). No primaries conversion is needed. ProPhoto-tagged *files* also
    /// land here, they share the working primaries and differ only in their
    /// transfer function.
    #[default]
    Working,
    /// sRGB / Rec.709 primaries, D65 white; converted to the working space by
    /// [`lightbox_color::matrix::spaces::linear_srgb_to_working`].
    Srgb,
    /// Display-P3: DCI-P3 primaries on a D65 white, converted by
    /// [`lightbox_color::matrix::spaces::linear_display_p3_to_working`]. **The
    /// default capture space of modern iPhones**, and therefore the most common
    /// non-sRGB source a real library contains.
    DisplayP3,
    /// Adobe RGB (1998) primaries, D65 white; converted by
    /// [`lightbox_color::matrix::spaces::linear_adobe_rgb_to_working`].
    AdobeRgb,
    /// ITU-R BT.2020 primaries, D65 white; converted by
    /// [`lightbox_color::matrix::spaces::linear_rec2020_to_working`].
    Rec2020,
}

/// The colorimetry of source pixels handed in over the [`crate::ng::source`]
/// seam (spec §3.4/§3.8).
///
/// This is the tag that drives the **input transform**: the source lift
/// ([`crate::ng::source::to_working_tile_cpu`] and
/// [`crate::ng::source::Uploader::upload`]) linearizes and converts the
/// provider's pixels into the working space *before* the graph's root port, so
/// every develop stage sees working-space linear RGB (E02 spec §"Decoded
/// display-referred, ICC-tagged (untagged ⇒ assumed sRGB) → working space
/// (linearized) → same downstream pipeline").
///
/// # The default is pass-through, and that is deliberate
///
/// [`Default`] is [`SourceColorimetry::WORKING_LINEAR`], "these pixels are
/// already in the working space", i.e. the identity input transform. That keeps
/// the engine's own synthetic sources (tests, probe generators, the raw path
/// whose camera matrices already land in working space via
/// [`lightbox_color::resolve_input_transform`]) meaning exactly what they always
/// meant.
///
/// **A provider handing over display-referred file pixels must therefore say so
/// explicitly** with [`SourceColorimetry::srgb`]. The spec's "untagged ⇒ assumed
/// sRGB" rule is about *image files without an ICC profile*, so it is enforced
/// where file tags actually exist, at the decode/provider boundary, not by
/// this struct's `Default`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceColorimetry {
    /// How the channel values are encoded.
    pub transfer: TransferFunction,
    /// Which primaries/white point the channels are expressed in.
    pub primaries: ColorPrimaries,
}

impl SourceColorimetry {
    /// Pixels already in the engine working space (ProPhoto/ROMM primaries,
    /// linear TRC, D50), the identity input transform, and the [`Default`].
    pub const WORKING_LINEAR: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::Linear,
        primaries: ColorPrimaries::Working,
    };

    /// Display-referred sRGB: sRGB primaries (D65) with the sRGB transfer
    /// function. The documented assumption for an untagged, or tagged but
    /// not-resolvable, rendered (non-raw) file.
    pub const SRGB: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::Srgb,
        primaries: ColorPrimaries::Srgb,
    };

    /// Display-referred **Display-P3**: P3 primaries (D65) with the sRGB
    /// transfer function, what an iPhone tags its captures with.
    pub const DISPLAY_P3: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::Srgb,
        primaries: ColorPrimaries::DisplayP3,
    };

    /// Display-referred **Adobe RGB (1998)**: Adobe RGB primaries (D65) with
    /// its gamma-563/256 power law.
    pub const ADOBE_RGB: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::AdobeRgb,
        primaries: ColorPrimaries::AdobeRgb,
    };

    /// Display-referred **ROMM RGB / ProPhoto**: the working space's own
    /// primaries with ProPhoto's gamma-1.8-plus-toe encoding, so the input
    /// transform is a pure linearization with no primaries conversion.
    pub const PROPHOTO: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::ProPhoto,
        primaries: ColorPrimaries::Working,
    };

    /// Display-referred **Rec.2020**: BT.2020 primaries (D65) with the BT.709
    /// transfer curve.
    pub const REC2020: SourceColorimetry = SourceColorimetry {
        transfer: TransferFunction::Rec709,
        primaries: ColorPrimaries::Rec2020,
    };

    /// [`SourceColorimetry::SRGB`], as a function for call sites that read
    /// better that way.
    pub fn srgb() -> SourceColorimetry {
        SourceColorimetry::SRGB
    }

    /// Whether the input transform for this tag is the identity, i.e. the
    /// pixels are already working-space linear and the source lift may copy
    /// them through channel-for-channel.
    pub fn is_working_linear(&self) -> bool {
        *self == SourceColorimetry::WORKING_LINEAR
    }
}

/// The colorimetry of a completed [`crate::ng::RenderOutput`] (spec §3.6).
/// Opaque to the engine; export (E15) records it.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct OutputColorimetry {}

/// Whether the compiler's source is raw (mosaic, E11) or already-RGB (spec
/// §3.4). v1 templates take `Rgb`, decode/demosaic is upstream (E02/E11).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SourceKind {
    /// Already-demosaiced RGB (the M1 reality, E02 returns demosaiced RGB).
    Rgb,
    /// Raw CFA mosaic, reserved for E11's on-graph demosaic.
    Raw {
        /// Opaque CFA-pattern descriptor (E11 defines the real shape).
        cfa: CfaDesc,
    },
}

/// Opaque CFA-pattern descriptor placeholder, E11 owns the real definition.
#[derive(Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub struct CfaDesc {}

/// Which fidelity tier produced a [`crate::ng::SourceImage`] (spec §3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceQuality {
    /// A fast, lower-resolution preview tier (E03 pyramid / embedded).
    Preview,
    /// A partial decode (progressive first pixels).
    Partial,
    /// The full-resolution decode.
    Full,
}
