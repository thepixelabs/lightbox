// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The export settings model (E15 core slice, spec §5.1, narrowed).
//!
//! **Deliberately smaller than the full E15 spec's [`ExportSettings`]**: no
//! `Destination`/`CollisionPolicy` (the caller resolves an explicit output
//! path per item, see the crate root doc comment), no watermark, no
//! metadata policy, no CBOR/preset persistence, no JXL/AVIF/DNG/Original.
//! Every cut is named in `docs/plan/epics/E15-deviations.md`.

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
}

impl Default for ExportSettings {
    fn default() -> ExportSettings {
        ExportSettings {
            format: FileFormat::Jpeg { quality: 90 },
            color: OutputColor::default(),
            sizing: Sizing::default(),
            sharpen: None,
            naming: NamingSpec::default(),
        }
    }
}

/// Why an [`ExportSettings`] failed [`ExportSettings::validate`], surfaced
/// before any pixels move (mirrors the full spec's plan-time preflight,
/// §4.3, narrowed to the checks a single-settings core slice can make
/// without a catalog).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SettingsError {
    /// JPEG quality outside 1..=100 (`jpeg-encoder`'s own valid range).
    #[error("JPEG quality must be 1..=100, got {0}")]
    QualityOutOfRange(u8),
    /// A resize target of zero on either axis.
    #[error("resize target dimensions must be > 0")]
    InvalidResizeTarget,
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
}
