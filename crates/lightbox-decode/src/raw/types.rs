// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared raw-decode data types (spec §3.1). These are the **cross-phase
//! contract**: Phase C (LibRaw proxy) fills [`MosaicImage`], Phase B's
//! matrix-base reads [`RawColorimetry`], E03's raw cache traffics in
//! [`SourceImage`]/[`RawDecode`], E11's demosaic consumes [`LinearMosaic`].
//! They are defined in full now so downstream phases touch only disjoint
//! function bodies, never this file's shape.
//!
//! **Deviation from §3.1 (recorded in E02-deviations.md):** the colorimetry
//! matrices are stored as plain `[[f64; 3]; 3]` arrays rather than
//! `lightbox_color::Mat3`. `lightbox-color` depends on `lightbox-decode` for
//! `RawColorimetry`/`CameraId`; using `Mat3` here would invert that and create
//! a crate cycle. `lightbox_color::Mat3` provides `From<[[f64; 3]; 3]>` so the
//! solver reads these with zero friction.

use lightbox_types::Orientation;
use serde::{Deserialize, Serialize};

/// A 3×3 matrix as a plain row-major array (see the module deviation note).
pub type Mat3Array = [[f64; 3]; 3];

/// One Bayer/X-Trans CFA color.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CfaColor {
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
}

/// Color-filter-array layout (spec §3.1). Drives per-position black/white
/// levels in [`linearize`](crate::linearize) and, later, demosaic (E11).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CfaPattern {
    /// A 2×2 Bayer tile in row-major order (e.g. `[R, G, G, B]` = RGGB).
    Bayer([CfaColor; 4]),
    /// A 6×6 X-Trans tile (Fujifilm), row-major.
    XTrans(Box<[[CfaColor; 6]; 6]>),
    /// Single-channel monochrome sensor.
    Mono,
    /// Already-demosaiced linear RGB (linear-DNG) — no CFA.
    LinearRgb,
}

impl CfaPattern {
    /// The index into per-CFA-position level arrays (`black_levels` /
    /// `white_levels`) for the sensel at `(x, y)`. Bayer uses the 2×2 tile
    /// position; every non-Bayer layout shares position `0` (its levels are
    /// uniform).
    pub fn level_index(&self, x: u32, y: u32) -> usize {
        match self {
            CfaPattern::Bayer(_) => ((y % 2) * 2 + (x % 2)) as usize,
            _ => 0,
        }
    }
}

/// Per-CFA-position black levels (spec §3.1). Index with
/// [`CfaPattern::level_index`]; non-Bayer layouts use `levels[0]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct BlackLevels {
    /// Black level per 2×2 CFA position.
    pub levels: [u32; 4],
}

impl BlackLevels {
    /// A uniform black level across all positions.
    pub fn uniform(v: u32) -> Self {
        BlackLevels { levels: [v; 4] }
    }
}

/// An axis-aligned pixel rectangle (spec §3.1: `active_area`, `default_crop`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge (pixels from origin).
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// A rectangle covering the whole `w × h` frame.
    pub fn full(w: u32, h: u32) -> Self {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }
}

/// A CIE standard illuminant reference for a calibration matrix (spec §3.1).
/// Values mirror the DNG `CalibrationIlluminant` tag semantics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Illuminant {
    /// Standard illuminant A (tungsten, ~2856 K).
    StandardA,
    /// D50.
    D50,
    /// D55.
    D55,
    /// D65.
    D65,
    /// D75.
    D75,
    /// Daylight of the given correlated color temperature (Kelvin).
    Daylight(u16),
    /// A DNG illuminant code we do not model explicitly.
    Other(u16),
    /// No/unknown illuminant.
    #[default]
    Unknown,
}

/// Factual camera calibration data the §1.7 tier-1 matrix base needs
/// (spec §3.1). Extracted in-crate from DNG tags (A5) or supplied by the
/// LibRaw proxy for proprietary mosaics (Phase C). Matrices are row-major
/// arrays (see the module deviation note).
///
/// `Serialize`/`Deserialize` are derived (Phase C) so this factual calibration
/// block travels intact both over the proxy wire protocol (§3.2) and inside the
/// `DecodedRawState` CBOR header (§4.2) — the E03 raw-cache contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawColorimetry {
    /// As-shot neutral (camera-native), or derived from `cam_mul`.
    pub as_shot_neutral: Option<[f64; 3]>,
    /// First calibration illuminant.
    pub illuminant1: Illuminant,
    /// Second calibration illuminant (dual-illuminant profiles).
    pub illuminant2: Option<Illuminant>,
    /// `ColorMatrix1` (XYZ→camera).
    pub color_matrix1: Mat3Array,
    /// `ColorMatrix2`.
    pub color_matrix2: Option<Mat3Array>,
    /// `ForwardMatrix1` (camera→XYZ(D50)).
    pub forward_matrix1: Option<Mat3Array>,
    /// `ForwardMatrix2`.
    pub forward_matrix2: Option<Mat3Array>,
    /// `AnalogBalance` diagonal.
    pub analog_balance: Option<[f64; 3]>,
    /// `BaselineExposure` (stops).
    pub baseline_exposure: f32,
}

const IDENTITY3: Mat3Array = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Default for RawColorimetry {
    fn default() -> Self {
        RawColorimetry {
            as_shot_neutral: None,
            illuminant1: Illuminant::Unknown,
            illuminant2: None,
            color_matrix1: IDENTITY3,
            color_matrix2: None,
            forward_matrix1: None,
            forward_matrix2: None,
            analog_balance: None,
            baseline_exposure: 0.0,
        }
    }
}

/// The raw mosaic pixel plane (spec §3.1). Packed sensor formats are unpacked
/// to `u16` samples during decode; `samples.len() == width * height`.
#[derive(Clone, Debug)]
pub struct MosaicBuffer {
    /// Row-major `u16` samples, one per sensel.
    pub samples: Vec<u16>,
}

/// A CFA-mosaic raw image (spec §3.1) — the LibRaw proxy's mosaic-mode output
/// (Phase C, primary path). Everything [`linearize`](crate::linearize) and the
/// tier-1 matrix base need is here.
#[derive(Clone, Debug)]
pub struct MosaicImage {
    /// The mosaic sample plane.
    pub data: MosaicBuffer,
    /// Full plane width.
    pub width: u32,
    /// Full plane height.
    pub height: u32,
    /// CFA layout.
    pub cfa: CfaPattern,
    /// Valid sensor area (levels/dark rows excluded).
    pub active_area: Rect,
    /// Default display crop (a subset of `active_area`).
    pub default_crop: Rect,
    /// Per-CFA-position black levels.
    pub black_levels: BlackLevels,
    /// Per-CFA-position white (saturation) levels.
    pub white_levels: [u32; 4],
    /// DNG `LinearizationTable`: raw sample → linearized sample LUT.
    pub linearization: Option<Vec<u16>>,
    /// Calibration data for the color pipeline.
    pub colorimetry: RawColorimetry,
    /// EXIF orientation.
    pub orientation: Orientation,
}

/// Linearized mosaic (spec §3.1: output of [`linearize`](crate::linearize)):
/// `f32` in `[0, 1]`, black-subtracted, white-normalized, cropped to the
/// active area. Still a mosaic — demosaic is E11.
#[derive(Clone, Debug)]
pub struct LinearMosaic {
    /// Row-major linearized samples, `width * height`.
    pub data: Vec<f32>,
    /// Cropped width (`active_area.width`).
    pub width: u32,
    /// Cropped height (`active_area.height`).
    pub height: u32,
    /// CFA layout (unchanged by linearization; note the origin shifts to the
    /// active-area top-left, so callers re-derive tile phase from this).
    pub cfa: CfaPattern,
    /// Calibration data, carried through.
    pub colorimetry: RawColorimetry,
    /// EXIF orientation, carried through.
    pub orientation: Orientation,
}

/// Which backend produced an accepted decode — the value written to the
/// catalog `asset.decode_backend` column (spec §4.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DecodeBackend {
    /// In-crate permissive walker / linearize path (linear-DNG, mono-DNG).
    InCrate,
    /// The out-of-process LibRaw proxy (Phase C).
    LibrawProxy,
    /// The `zune-jpeg` codec.
    ImageJpeg,
    /// The `png` codec.
    ImagePng,
    /// The `tiff` codec.
    ImageTiff,
    /// The `libheif` codec (feature `heic`).
    ImageHeic,
}

impl DecodeBackend {
    /// Stable string for the `asset.decode_backend` diagnostics column.
    pub fn catalog_str(self) -> &'static str {
        match self {
            DecodeBackend::InCrate => "in_crate",
            DecodeBackend::LibrawProxy => "libraw_proxy",
            DecodeBackend::ImageJpeg => "image_jpeg",
            DecodeBackend::ImagePng => "image_png",
            DecodeBackend::ImageTiff => "image_tiff",
            DecodeBackend::ImageHeic => "image_heic",
        }
    }
}

/// Provenance of a decoded [`SourceImage`] (spec §3.1). The `interim_demosaic`
/// bit and `decode_params_hash` segregate M1 LibRaw-AHD states from post-E11
/// mosaic states in E03's cache (spec §4.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceProvenance {
    /// Backend that produced the pixels.
    pub backend: DecodeBackend,
    /// True when the pixels are the interim LibRaw-AHD develop path (M1).
    pub interim_demosaic: bool,
    /// `xxh3(backend policy, interim_demosaic, libraw version, linearize impl
    /// version)` — the E03 cache-key params component (spec §4.2).
    pub decode_params_hash: u64,
}

/// The source color of a [`SourceImage`] (spec §3.1).
// `CameraNative` carries the full calibration block and is the large variant;
// it is set only on the raw path (Phase C). Keeping the spec's flat shape is
// worth more than boxing to equalize variant sizes.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum SourceColor {
    /// Camera-native linear RGB (raw), carrying its calibration data. The
    /// input transform (lightbox-color) takes it to the working space.
    CameraNative(RawColorimetry),
    /// Display-referred pixels tagged with an embedded ICC profile (its bytes).
    Tagged(Vec<u8>),
    /// Display-referred, untagged — assumed sRGB (spec §5.3).
    AssumedSrgb,
}

/// One develop-ready image: camera-native linear RGB (raw) or display-referred
/// source color (non-raw) (spec §3.1). What the E05 source node and E03 raw
/// cache traffic in.
#[derive(Clone, Debug)]
pub struct SourceImage {
    /// Interleaved samples, row-major, `width * height * channels`.
    pub data: Vec<f32>,
    /// Image width.
    pub width: u32,
    /// Image height.
    pub height: u32,
    /// Channels per pixel (3 = RGB, 4 = RGBA).
    pub channels: u8,
    /// Source color space / tagging.
    pub color: SourceColor,
    /// Decode provenance.
    pub provenance: SourceProvenance,
}

/// The result of [`decode_raw`](crate::decode_raw) (spec §3.1).
// The mosaic variant is intentionally the largest — it is the primary CFA
// path (Phase C) and callers match it constantly; boxing it to shrink the
// rarer variants would pessimize the common path for no real benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum RawDecode {
    /// LibRaw proxy mosaic mode (primary CFA path, Phase C).
    Mosaic(MosaicImage),
    /// Interim M1 develop path: LibRaw AHD output, linear camera-native RGB,
    /// no WB / no output color / no gamma. Replaced by E11.1 at M2.
    DemosaicedInterim(SourceImage),
    /// Linear-DNG / monochrome, decoded in-crate.
    Linear(SourceImage),
}

/// Which backend the caller wants (spec §3.1 `DecodeOpts.backend`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BackendPolicy {
    /// Mosaic → proxy; linear/mono → in-crate (the default).
    #[default]
    Auto,
    /// Force the out-of-process proxy.
    ForceProxy,
    /// Force the in-crate path.
    ForceInCrate,
}

/// Raw-decode options (spec §3.1).
#[derive(Clone, Copy, Debug)]
pub struct DecodeOpts {
    /// Backend selection policy.
    pub backend: BackendPolicy,
    /// When true, the proxy returns demosaiced camera-RGB (the M1 develop path).
    pub interim_demosaic: bool,
    /// Proxy decode budget (spec default: 30 s).
    pub timeout: std::time::Duration,
}

impl Default for DecodeOpts {
    fn default() -> Self {
        DecodeOpts {
            backend: BackendPolicy::Auto,
            interim_demosaic: false,
            timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bayer_level_index_walks_the_2x2_tile() {
        let cfa = CfaPattern::Bayer([CfaColor::R, CfaColor::G, CfaColor::G, CfaColor::B]);
        assert_eq!(cfa.level_index(0, 0), 0);
        assert_eq!(cfa.level_index(1, 0), 1);
        assert_eq!(cfa.level_index(0, 1), 2);
        assert_eq!(cfa.level_index(1, 1), 3);
        // Wraps.
        assert_eq!(cfa.level_index(2, 2), 0);
        assert_eq!(cfa.level_index(3, 5), 3);
    }

    #[test]
    fn non_bayer_uses_position_zero() {
        assert_eq!(CfaPattern::Mono.level_index(7, 9), 0);
        assert_eq!(CfaPattern::LinearRgb.level_index(3, 4), 0);
    }

    #[test]
    fn backend_catalog_strings_are_stable() {
        assert_eq!(DecodeBackend::InCrate.catalog_str(), "in_crate");
        assert_eq!(DecodeBackend::LibrawProxy.catalog_str(), "libraw_proxy");
        assert_eq!(DecodeBackend::ImageJpeg.catalog_str(), "image_jpeg");
    }

    #[test]
    fn decode_opts_default_matches_spec() {
        let o = DecodeOpts::default();
        assert_eq!(o.backend, BackendPolicy::Auto);
        assert!(!o.interim_demosaic);
        assert_eq!(o.timeout, std::time::Duration::from_secs(30));
    }

    #[test]
    fn colorimetry_default_is_identity() {
        let c = RawColorimetry::default();
        assert_eq!(c.color_matrix1, IDENTITY3);
        assert!(c.as_shot_neutral.is_none());
    }
}
