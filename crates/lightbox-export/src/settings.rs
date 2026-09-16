// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The export settings model (E15 core slice, spec §5.1, narrowed).
//!
//! **Deliberately smaller than the full E15 spec's [`ExportSettings`]**: no
//! `Destination`/`CollisionPolicy` (the caller resolves an explicit output
//! path per item, see the crate root doc comment), no CBOR/preset
//! persistence, no JXL/AVIF/DNG/Original. Every cut is named in
//! `docs/plan/epics/E15-deviations.md`.
//!
//! [`MetadataPolicy`] (spec §5.6) and [`Watermark`] (spec §5.11) live here
//! too; the byte-level emitters they drive are [`crate::metadata`] and
//! [`crate::watermark`].

use std::path::Path;

use lightbox_color::display::Intent as ColorIntent;
use lightbox_color::output::OutputSpace as ColorOutputSpace;

/// Output bit depth. JPEG is always 8-bit (the format has no 16-bit mode);
/// PNG/TIFF may be either.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BitDepth {
    /// 8 bits per channel.
    #[default]
    Eight,
    /// 16 bits per channel.
    Sixteen,
}

/// The output container + its format-specific knobs (core slice: JPEG/PNG/
/// TIFF only, JXL/AVIF/DNG/Original are named-deferred, spec §5.1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FileFormat {
    /// Baseline/progressive JPEG (`jpeg-encoder`). Always 8-bit.
    Jpeg {
        /// 1..=100, higher is better quality (spec §5.1 `Jpeg::quality`).
        quality: u8,
    },
    /// PNG (`png` crate), 8 or 16-bit.
    Png {
        /// Output bit depth.
        depth: BitDepth,
    },
    /// TIFF (`tiff` crate), 8 or 16-bit, Deflate-compressed (no per-export
    /// compression choice at core-slice scope, spec §5.1 names None/LZW/
    /// Deflate; deferred, see `E15-deviations.md`).
    Tiff {
        /// Output bit depth.
        depth: BitDepth,
    },
}

impl FileFormat {
    /// The bit depth this format will actually emit (JPEG is pinned to
    /// [`BitDepth::Eight`] regardless of any caller intent).
    #[must_use]
    pub fn depth(&self) -> BitDepth {
        match self {
            FileFormat::Jpeg { .. } => BitDepth::Eight,
            FileFormat::Png { depth } | FileFormat::Tiff { depth } => *depth,
        }
    }

    /// The filename extension (no leading dot), used by [`NamingSpec`].
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            FileFormat::Jpeg { .. } => "jpg",
            FileFormat::Png { .. } => "png",
            FileFormat::Tiff { .. } => "tif",
        }
    }
}

/// The three output color spaces the core slice exposes (spec §5.1
/// `OutputSpace`, narrowed to the Must-tier set named in the task brief
/// ProPhoto/Rec2020/user-ICC are deferred, `lightbox_color::output` already
/// supports them so widening this enum is additive follow-up work).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OutputColorSpace {
    /// sRGB.
    #[default]
    Srgb,
    /// Adobe-RGB-compatible (synthesized from published primaries, spec
    /// §5.7/Risk #1, never Adobe's own ICC file).
    AdobeRgb,
    /// Display P3.
    DisplayP3,
}

impl OutputColorSpace {
    pub(crate) fn to_color_space(self) -> ColorOutputSpace {
        match self {
            OutputColorSpace::Srgb => ColorOutputSpace::Srgb,
            OutputColorSpace::AdobeRgb => ColorOutputSpace::AdobeRgb,
            OutputColorSpace::DisplayP3 => ColorOutputSpace::DisplayP3,
        }
    }

    /// Relative-colorimetric with black-point compensation (spec §5.1
    /// `RenderingIntent` narrowed to the one intent the core slice offers
    /// perceptual is deferred).
    pub(crate) fn intent() -> ColorIntent {
        ColorIntent::RelColorimetric
    }
}

/// Output color management (spec §5.1 `OutputColor`, narrowed: fixed
/// rel-colorimetric intent, no user ICC).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct OutputColor {
    /// The destination space.
    pub space: OutputColorSpace,
}

/// Resize rule (spec §5.1 `SizeRule`, narrowed to long-edge / exact
/// dimensions / none, short-edge/megapixels/percent are deferred).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SizeRule {
    /// No resize, export at the render's native resolution.
    #[default]
    None,
    /// Fit the long edge to `px`, aspect preserved. Never upscales.
    LongEdge {
        /// The target long-edge size, in pixels.
        px: u32,
    },
    /// Fit within `w`×`h`, aspect preserved. Never upscales.
    Dimensions {
        /// Target width.
        w: u32,
        /// Target height.
        h: u32,
    },
}

/// Sizing (spec §5.1 `Sizing`, narrowed: no `dont_enlarge` toggle, resize
/// never upscales, unconditionally, matching `lightbox-preview`'s existing
/// resize convention; no PPI stamp).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sizing {
    /// The resize rule.
    pub rule: SizeRule,
}

/// Output-sharpening strength (spec §5.1 `SharpenAmount`; the medium×amount
/// table is narrowed to amount only, every export sharpens as if for
/// screen viewing, see `pixel::sharpen_in_place`'s doc comment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SharpenAmount {
    /// A light touch.
    Low,
    /// The default amount.
    Standard,
    /// A stronger pass, for images that will be downsized further
    /// downstream or viewed at high DPI.
    High,
}

/// Basic output sharpening (spec §5.1 `OutputSharpen`, narrowed to a single
/// `SharpenAmount`, the medium axis, i.e. screen/matte/glossy viewing
/// conditions, is deferred).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutputSharpen {
    /// How strong a pass to apply.
    pub amount: SharpenAmount,
}

/// Simple filename naming: `<stem><suffix>.<ext>` (spec §5.5's full token
/// engine is deferred, see the crate root doc comment). The stem is
/// supplied by the caller (typically the source file's stem).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct NamingSpec {
    /// Appended verbatim after the stem, before the extension (e.g.
    /// `"-Export"` → `IMG_0001-Export.jpg`). `None`/empty means no suffix.
    pub suffix: Option<String>,
}

impl NamingSpec {
    /// Renders `<stem><suffix>.<ext>` for `format`. `stem` should already be
    /// a bare filename stem (no directory, no extension), callers
    /// typically derive it from the source asset's filename via
    /// [`std::path::Path::file_stem`].
    #[must_use]
    pub fn file_name(&self, stem: &str, format: FileFormat) -> String {
        let mut name = stem.to_owned();
        if let Some(suffix) = self.suffix.as_deref().filter(|s| !s.is_empty()) {
            name.push_str(suffix);
        }
        name.push('.');
        name.push_str(format.extension());
        name
    }
}

// ─── metadata policy (spec §5.6) ───────────────────────────────────────────

/// How much metadata leaves with the exported file.
///
/// An export re-encodes from pixels, so **nothing** is inherited from the
/// source container: every byte of metadata in the output is something this
/// policy asked [`crate::metadata::build`] to write. The levels below are
/// therefore an allowlist, not a strip list, which is why the privacy claim
/// holds by construction rather than by remembering to delete things.
///
/// What every level writes, unconditionally, because it describes the
/// exported file rather than the photographer or the scene:
/// - the EXIF `Software` tag and `xmp:CreatorTool`, both `"Lightbox"`,
/// - the exported pixel dimensions (`PixelXDimension`/`PixelYDimension`),
/// - the ICC profile (color management, written by [`crate::encode`]
///   independently of this policy).
///
/// What **no** level ever writes, at all:
/// - IPTC IIM (the legacy `8BIM`/APP13 block). Lightbox does not emit it in
///   any format; the IPTC-equivalent fields it does emit are XMP properties
///   (`dc:`, `photoshop:`, `Iptc4xmpCore:`), which is IPTC's own current
///   recommendation.
/// - Textual place names (city/state/country). Lightbox has no field for
///   them, so "location" here means exactly the EXIF GPS IFD.
/// - EXIF `Orientation`. Export pixels are already rotated by the render,
///   so writing an orientation tag would rotate them a second time.
/// - Person/face regions, ratings, labels, keywords, edit history.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MetadataLevel {
    /// The copyright statement and nothing else about the photograph.
    ///
    /// **The default**, deliberately the safe end rather than the maximal
    /// one: an export is usually something leaving the machine, and the
    /// setting that leaks a home address should be the one you opt into.
    #[default]
    CopyrightOnly,
    /// [`MetadataLevel::CopyrightOnly`] plus the creator's name and their
    /// contact email and URL (`dc:creator`, EXIF `Artist`,
    /// `Iptc4xmpCore:CreatorContactInfo`).
    CopyrightAndContact,
    /// Everything Lightbox models **except** camera identification
    /// (make, model, lens, exposure, aperture, ISO, focal length) and the
    /// **structured** location: the EXIF GPS IFD and its XMP mirror, which
    /// is the machine-readable position a camera or phone records.
    ///
    /// Capture date, description and the rights/contact block survive, and
    /// the description is the part to know about. It is text the
    /// photographer (or their phone's software) wrote, and Lightbox does not
    /// read it, so if a caption says "Trafalgar Square" or carries a `geo:`
    /// string, that text leaves with the file at this level. This level
    /// strips the position the device recorded; it does not censor what you
    /// wrote. A caption you do not want to travel is edited or removed
    /// before export, or the export is made at a lower level.
    AllExceptCameraAndLocation,
    /// Everything Lightbox models, including camera identification and the
    /// GPS position when the source carried one.
    All,
}

impl MetadataLevel {
    /// True where the creator name and contact email/URL are written.
    #[must_use]
    pub fn writes_contact(self) -> bool {
        !matches!(self, MetadataLevel::CopyrightOnly)
    }

    /// True where descriptive fields (capture date, description) are
    /// written.
    #[must_use]
    pub fn writes_descriptive(self) -> bool {
        matches!(
            self,
            MetadataLevel::AllExceptCameraAndLocation | MetadataLevel::All
        )
    }

    /// True where camera identification (make/model/lens/exposure/aperture/
    /// ISO/focal length) is written.
    #[must_use]
    pub fn writes_camera(self) -> bool {
        matches!(self, MetadataLevel::All)
    }

    /// True where the GPS position is written. False at every level except
    /// [`MetadataLevel::All`].
    #[must_use]
    pub fn writes_location(self) -> bool {
        matches!(self, MetadataLevel::All)
    }
}

/// The rights and contact block: a per-export setting the user configures
/// once, not something read out of the photograph (Lightroom's metadata
/// preset plays the same role).
///
/// Where a field is `None` here, [`crate::metadata::build`] falls back to
/// the matching field on [`SourceMetadata`] when the source file carried
/// one; a value set here always wins.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RightsInfo {
    /// `dc:creator` / EXIF `Artist`. Written from
    /// [`MetadataLevel::CopyrightAndContact`] up.
    pub creator: Option<String>,
    /// `dc:rights` / EXIF `Copyright`. Written at every level.
    pub copyright: Option<String>,
    /// `Iptc4xmpCore:CreatorContactInfo/Iptc4xmpCore:CiEmailWork`. Written
    /// from [`MetadataLevel::CopyrightAndContact`] up.
    pub contact_email: Option<String>,
    /// `Iptc4xmpCore:CreatorContactInfo/Iptc4xmpCore:CiUrlWork` and
    /// `xmpRights:WebStatement`. Written from
    /// [`MetadataLevel::CopyrightAndContact`] up.
    pub contact_url: Option<String>,
}

/// An EXIF unsigned rational (`num/den`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rational {
    /// Numerator.
    pub num: u32,
    /// Denominator. A zero denominator is written through verbatim (EXIF
    /// permits it and readers treat it as "undefined"); Lightbox never
    /// divides by it.
    pub den: u32,
}

impl Rational {
    /// A rational, as-is.
    #[must_use]
    pub fn new(num: u32, den: u32) -> Rational {
        Rational { num, den }
    }
}

/// A WGS-84 position, as EXIF's GPS IFD models it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GpsPosition {
    /// Signed degrees, north positive.
    pub latitude_deg: f64,
    /// Signed degrees, east positive.
    pub longitude_deg: f64,
    /// Metres above (positive) or below (negative) sea level.
    pub altitude_m: Option<f64>,
}

/// The per-image facts an export may carry forward, resolved by the caller
/// exactly like [`crate::run::ExportItem::recipe`] is.
///
/// [`crate::metadata::read_source`] fills one of these from a source file's
/// EXIF; a caller with the values already in hand can build it directly.
/// Every field is optional and an absent field is simply not written.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SourceMetadata {
    /// EXIF `Make`.
    pub make: Option<String>,
    /// EXIF `Model`.
    pub model: Option<String>,
    /// EXIF `LensModel`.
    pub lens_model: Option<String>,
    /// EXIF `DateTimeOriginal`, in EXIF's own `YYYY:MM:DD HH:MM:SS` form.
    pub capture_time: Option<String>,
    /// EXIF `ExposureTime`, in seconds.
    pub exposure_time: Option<Rational>,
    /// EXIF `FNumber`.
    pub f_number: Option<Rational>,
    /// EXIF `PhotographicSensitivity` (ISO).
    pub iso: Option<u16>,
    /// EXIF `FocalLength`, in millimetres.
    pub focal_length_mm: Option<Rational>,
    /// The GPS position, when the source had one.
    pub gps: Option<GpsPosition>,
    /// EXIF `ImageDescription` / `dc:description`.
    pub description: Option<String>,
    /// EXIF `Artist`, used only when [`RightsInfo::creator`] is `None`.
    pub creator: Option<String>,
    /// EXIF `Copyright`, used only when [`RightsInfo::copyright`] is `None`.
    pub copyright: Option<String>,
}

/// The metadata half of [`ExportSettings`] (spec §5.6): how much leaves with
/// the file, plus the rights block that goes with it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MetadataPolicy {
    /// How much metadata to write. Defaults to
    /// [`MetadataLevel::CopyrightOnly`].
    pub level: MetadataLevel,
    /// The per-export rights and contact block.
    pub rights: RightsInfo,
}

// ─── watermark (spec §5.11) ────────────────────────────────────────────────

/// The nine anchor points a watermark can sit on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WatermarkAnchor {
    /// Top left.
    TopLeft,
    /// Top centre.
    TopCenter,
    /// Top right.
    TopRight,
    /// Middle left.
    MiddleLeft,
    /// Dead centre.
    Center,
    /// Middle right.
    MiddleRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom centre.
    BottomCenter,
    /// Bottom right. The default, where a signature normally goes.
    #[default]
    BottomRight,
}

impl WatermarkAnchor {
    /// `(x, y)` in 0.0..=1.0, where 0 is left/top and 1 is right/bottom.
    #[must_use]
    pub fn fractions(self) -> (f32, f32) {
        let x = match self {
            WatermarkAnchor::TopLeft
            | WatermarkAnchor::MiddleLeft
            | WatermarkAnchor::BottomLeft => 0.0,
            WatermarkAnchor::TopCenter
            | WatermarkAnchor::Center
            | WatermarkAnchor::BottomCenter => 0.5,
            WatermarkAnchor::TopRight
            | WatermarkAnchor::MiddleRight
            | WatermarkAnchor::BottomRight => 1.0,
        };
        let y = match self {
            WatermarkAnchor::TopLeft | WatermarkAnchor::TopCenter | WatermarkAnchor::TopRight => {
                0.0
            }
            WatermarkAnchor::MiddleLeft
            | WatermarkAnchor::Center
            | WatermarkAnchor::MiddleRight => 0.5,
            WatermarkAnchor::BottomLeft
            | WatermarkAnchor::BottomCenter
            | WatermarkAnchor::BottomRight => 1.0,
        };
        (x, y)
    }
}

/// A text watermark burnt into the exported pixels (spec §5.11, text only).
///
/// **Graphical (PNG) watermarks are not built.** They would need an image
/// decoder in this crate, and the task that added watermarking was explicit
/// that one must not be added for that alone; see [`crate::watermark`]'s
/// module doc comment for what the text renderer is and what it looks like.
#[derive(Clone, PartialEq, Debug)]
pub struct Watermark {
    /// The text to draw. `\n` starts a new line; every other control
    /// character is dropped by the renderer. Must not be empty or
    /// whitespace-only ([`ExportSettings::validate`] rejects that).
    pub text: String,
    /// Which of the nine anchor points the text sits on.
    pub anchor: WatermarkAnchor,
    /// Alpha applied to the whole watermark, 0.0..=1.0.
    pub opacity: f32,
    /// Cap height as a fraction of the image's **short** edge, in
    /// 0.0..=1.0 (exclusive of zero). The short edge rather than the long
    /// one so a panorama does not get a watermark the height of a building.
    /// Scaled down automatically when the text would not otherwise fit
    /// between the insets.
    pub size: f32,
    /// Margin from the image edge as a fraction of the short edge, in
    /// 0.0..0.5.
    pub inset: f32,
    /// Text color, in the export's output color space, non-linear (the
    /// same encoding the file is written in).
    pub color: [u8; 3],
    /// Draw a contrasting halo behind the glyphs so the watermark stays
    /// legible over both a bright sky and a dark shadow. The halo color is
    /// derived from [`Watermark::color`]: black behind light text, white
    /// behind dark text.
    pub halo: bool,
}

impl Default for Watermark {
    fn default() -> Watermark {
        Watermark {
            text: String::new(),
            anchor: WatermarkAnchor::default(),
            opacity: 0.7,
            size: 0.04,
            inset: 0.03,
            color: [255, 255, 255],
            halo: true,
        }
    }
}

impl Watermark {
    /// A watermark with `text` and every other knob at its default.
    #[must_use]
    pub fn text<S: Into<String>>(text: S) -> Watermark {
        Watermark {
            text: text.into(),
            ..Watermark::default()
        }
    }
}

/// The root export settings document (spec §5.1 `ExportSettings`,
/// narrowed, see the module doc comment for the full cut list).
#[derive(Clone, PartialEq, Debug)]
pub struct ExportSettings {
    /// Output container + format knobs.
    pub format: FileFormat,
    /// Output color management.
    pub color: OutputColor,
    /// Resize rule.
    pub sizing: Sizing,
    /// Basic output sharpening (`None` = off).
    pub sharpen: Option<OutputSharpen>,
    /// Simple stem+suffix naming.
    pub naming: NamingSpec,
    /// How much metadata leaves with the file. Defaults to
    /// [`MetadataLevel::CopyrightOnly`].
    pub metadata: MetadataPolicy,
    /// Text watermark burnt into the pixels (`None` = off, the default).
    pub watermark: Option<Watermark>,
}

impl Default for ExportSettings {
    fn default() -> ExportSettings {
        ExportSettings {
            format: FileFormat::Jpeg { quality: 90 },
            color: OutputColor::default(),
            sizing: Sizing::default(),
            sharpen: None,
            naming: NamingSpec::default(),
            metadata: MetadataPolicy::default(),
            watermark: None,
        }
    }
}

/// Why an [`ExportSettings`] failed [`ExportSettings::validate`], surfaced
/// before any pixels move (mirrors the full spec's plan-time preflight,
/// §4.3, narrowed to the checks a single-settings core slice can make
/// without a catalog).
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
pub enum SettingsError {
    /// JPEG quality outside 1..=100 (`jpeg-encoder`'s own valid range).
    #[error("JPEG quality must be 1..=100, got {0}")]
    QualityOutOfRange(u8),
    /// A resize target of zero on either axis.
    #[error("resize target dimensions must be > 0")]
    InvalidResizeTarget,
    /// A watermark was requested with nothing to draw.
    #[error("watermark text must not be empty")]
    WatermarkTextEmpty,
    /// Watermark opacity outside 0.0..=1.0 (or not a number).
    #[error("watermark opacity must be 0.0..=1.0, got {0}")]
    WatermarkOpacityOutOfRange(f32),
    /// Watermark size outside 0.0..=1.0, exclusive of zero (or not a
    /// number).
    #[error("watermark size must be >0.0 and <=1.0 (fraction of the short edge), got {0}")]
    WatermarkSizeOutOfRange(f32),
    /// Watermark inset outside 0.0..0.5 (or not a number).
    #[error("watermark inset must be 0.0..0.5 (fraction of the short edge), got {0}")]
    WatermarkInsetOutOfRange(f32),
}

impl ExportSettings {
    /// Plan-time validation (spec §4.3 preflight, narrowed): catches
    /// caller-constructible invalid states before any rendering happens.
    /// Pure, side-effect free, safe to call repeatedly (e.g. from a live
    /// dialog binding).
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let FileFormat::Jpeg { quality } = self.format {
            if !(1..=100).contains(&quality) {
                return Err(SettingsError::QualityOutOfRange(quality));
            }
        }
        match self.sizing.rule {
            SizeRule::None => {}
            SizeRule::LongEdge { px } => {
                if px == 0 {
                    return Err(SettingsError::InvalidResizeTarget);
                }
            }
            SizeRule::Dimensions { w, h } => {
                if w == 0 || h == 0 {
                    return Err(SettingsError::InvalidResizeTarget);
                }
            }
        }
        if let Some(wm) = &self.watermark {
            if wm.text.trim().is_empty() {
                return Err(SettingsError::WatermarkTextEmpty);
            }
            if !(0.0..=1.0).contains(&wm.opacity) {
                return Err(SettingsError::WatermarkOpacityOutOfRange(wm.opacity));
            }
            if !(wm.size > 0.0 && wm.size <= 1.0) {
                return Err(SettingsError::WatermarkSizeOutOfRange(wm.size));
            }
            if !(0.0..0.5).contains(&wm.inset) {
                return Err(SettingsError::WatermarkInsetOutOfRange(wm.inset));
            }
        }
        Ok(())
    }
}

/// Derives a naming stem from a source path (its file stem, or `"export"`
/// when the path has none, e.g. an extensionless or root path).
#[must_use]
pub fn stem_of(source: &Path) -> String {
    source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "export".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        assert_eq!(ExportSettings::default().validate(), Ok(()));
    }

    #[test]
    fn jpeg_quality_out_of_range_is_rejected() {
        let mut s = ExportSettings {
            format: FileFormat::Jpeg { quality: 0 },
            ..ExportSettings::default()
        };
        assert_eq!(s.validate(), Err(SettingsError::QualityOutOfRange(0)));
        s.format = FileFormat::Jpeg { quality: 101 };
        assert_eq!(s.validate(), Err(SettingsError::QualityOutOfRange(101)));
        s.format = FileFormat::Jpeg { quality: 100 };
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn png_and_tiff_accept_any_quality_field_absence() {
        for depth in [BitDepth::Eight, BitDepth::Sixteen] {
            let s = ExportSettings {
                format: FileFormat::Png { depth },
                ..ExportSettings::default()
            };
            assert_eq!(s.validate(), Ok(()));
            let s = ExportSettings {
                format: FileFormat::Tiff { depth },
                ..ExportSettings::default()
            };
            assert_eq!(s.validate(), Ok(()));
        }
    }

    #[test]
    fn jpeg_depth_is_always_eight() {
        assert_eq!(FileFormat::Jpeg { quality: 90 }.depth(), BitDepth::Eight);
    }

    #[test]
    fn zero_resize_targets_are_rejected() {
        let mut s = ExportSettings::default();
        s.sizing.rule = SizeRule::LongEdge { px: 0 };
        assert_eq!(s.validate(), Err(SettingsError::InvalidResizeTarget));
        s.sizing.rule = SizeRule::Dimensions { w: 0, h: 100 };
        assert_eq!(s.validate(), Err(SettingsError::InvalidResizeTarget));
        s.sizing.rule = SizeRule::Dimensions { w: 100, h: 0 };
        assert_eq!(s.validate(), Err(SettingsError::InvalidResizeTarget));
        s.sizing.rule = SizeRule::LongEdge { px: 1920 };
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn naming_renders_stem_suffix_extension() {
        let naming = NamingSpec {
            suffix: Some("-Export".to_owned()),
        };
        assert_eq!(
            naming.file_name("IMG_0001", FileFormat::Jpeg { quality: 90 }),
            "IMG_0001-Export.jpg"
        );
        let no_suffix = NamingSpec::default();
        assert_eq!(
            no_suffix.file_name(
                "IMG_0001",
                FileFormat::Png {
                    depth: BitDepth::Eight
                }
            ),
            "IMG_0001.png"
        );
    }

    #[test]
    fn stem_of_handles_normal_missing_and_dotfile_paths() {
        assert_eq!(stem_of(Path::new("/a/b/IMG_0001.CR2")), "IMG_0001");
        assert_eq!(stem_of(Path::new("noext")), "noext");
        assert_eq!(stem_of(Path::new("/")), "export");
    }

    #[test]
    fn the_default_metadata_level_is_the_safe_one() {
        let level = ExportSettings::default().metadata.level;
        assert_eq!(level, MetadataLevel::CopyrightOnly);
        assert!(!level.writes_location(), "the default must not leak GPS");
        assert!(!level.writes_camera());
        assert!(!level.writes_descriptive());
        assert!(!level.writes_contact());
    }

    #[test]
    fn only_the_all_level_writes_camera_and_location() {
        for level in [
            MetadataLevel::CopyrightOnly,
            MetadataLevel::CopyrightAndContact,
            MetadataLevel::AllExceptCameraAndLocation,
        ] {
            assert!(!level.writes_location(), "{level:?} must not write GPS");
            assert!(!level.writes_camera(), "{level:?} must not write camera");
        }
        assert!(MetadataLevel::All.writes_location());
        assert!(MetadataLevel::All.writes_camera());
    }

    #[test]
    fn descriptive_and_contact_tiers_widen_monotonically() {
        assert!(!MetadataLevel::CopyrightOnly.writes_contact());
        assert!(MetadataLevel::CopyrightAndContact.writes_contact());
        assert!(!MetadataLevel::CopyrightAndContact.writes_descriptive());
        assert!(MetadataLevel::AllExceptCameraAndLocation.writes_descriptive());
        assert!(MetadataLevel::AllExceptCameraAndLocation.writes_contact());
        assert!(MetadataLevel::All.writes_descriptive());
    }

    #[test]
    fn anchor_fractions_cover_the_nine_points() {
        use WatermarkAnchor as A;
        let all = [
            (A::TopLeft, (0.0, 0.0)),
            (A::TopCenter, (0.5, 0.0)),
            (A::TopRight, (1.0, 0.0)),
            (A::MiddleLeft, (0.0, 0.5)),
            (A::Center, (0.5, 0.5)),
            (A::MiddleRight, (1.0, 0.5)),
            (A::BottomLeft, (0.0, 1.0)),
            (A::BottomCenter, (0.5, 1.0)),
            (A::BottomRight, (1.0, 1.0)),
        ];
        for (anchor, want) in all {
            assert_eq!(anchor.fractions(), want, "{anchor:?}");
        }
        assert_eq!(A::default(), A::BottomRight);
    }

    #[test]
    fn watermark_validation_rejects_the_out_of_range_knobs() {
        let with = |wm: Watermark| ExportSettings {
            watermark: Some(wm),
            ..ExportSettings::default()
        };
        assert_eq!(with(Watermark::text("(c) 2026")).validate(), Ok(()));
        assert_eq!(
            with(Watermark::text("   ")).validate(),
            Err(SettingsError::WatermarkTextEmpty)
        );
        assert_eq!(
            with(Watermark {
                opacity: 1.5,
                ..Watermark::text("x")
            })
            .validate(),
            Err(SettingsError::WatermarkOpacityOutOfRange(1.5))
        );
        assert_eq!(
            with(Watermark {
                size: 0.0,
                ..Watermark::text("x")
            })
            .validate(),
            Err(SettingsError::WatermarkSizeOutOfRange(0.0))
        );
        assert_eq!(
            with(Watermark {
                inset: 0.5,
                ..Watermark::text("x")
            })
            .validate(),
            Err(SettingsError::WatermarkInsetOutOfRange(0.5))
        );
        // NaN is out of range on every knob, not silently accepted.
        assert!(with(Watermark {
            opacity: f32::NAN,
            ..Watermark::text("x")
        })
        .validate()
        .is_err());
        assert!(with(Watermark {
            size: f32::NAN,
            ..Watermark::text("x")
        })
        .validate()
        .is_err());
        assert!(with(Watermark {
            inset: f32::NAN,
            ..Watermark::text("x")
        })
        .validate()
        .is_err());
    }
}
