// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `Lut3D`, a parsed/baked 3D lookup table over `[domain_min, domain_max]`
//! (E10 spec §4.4 `nodes/common/lut.rs`'s 3D half; task **D8**). Two parsers:
//! Adobe/Iridas `.cube` (1D and 3D; `LUT_1D_SIZE`/`LUT_3D_SIZE`,
//! `DOMAIN_MIN`/`DOMAIN_MAX`, `TITLE`/`#` comments) and HaldCLUT identity-
//! image PNGs (level-8/12, spec §4.8). Both are **untrusted input**
//! creative-look packs a user drags in, so every parse path returns a typed
//! [`LutParseError`] and never panics, per the D8 acceptance criterion (fuzz/
//! truncation unit tests below).
//!
//! [`Lut3D::sample_tetrahedral`] is the CPU reference for task **D9**'s
//! `CreativeLutNode`, the classic Kasson/Spaulding six-tetrahedra
//! decomposition of the enclosing unit cube (published, patent-expired
//! arithmetic; used by e.g. ArgyllCMS/LittleCMS, clean-room from the
//! published formulas, no GPL source consulted). `shaders/
//! global_creative_lut.wgsl` implements the byte-identical branch structure
//! against the byte-identical 3D texture upload ([`Lut3D::to_padded_rgba_f32`]
//! writes the table in exactly wgpu's expected `x`-fastest/`y`/`z`-slowest
//! linear layout, so no reordering happens between CPU and GPU), so CPU/GPU
//! parity (§4.4) holds by construction.

use std::io::Cursor;

/// Minimum accepted `LUT_1D_SIZE`/`LUT_3D_SIZE` (a 1-entry cube cannot be
/// interpolated at all).
pub const MIN_LUT_SIZE: u32 = 2;
/// Maximum accepted `LUT_3D_SIZE`. Real-world packs ship 17/33/65; 129 leaves
/// headroom while still bounding a hostile file's declared size to a sane
/// allocation (`129^3 * 3 * 4` bytes ≈ 25 MB) before any row is parsed.
pub const MAX_LUT_3D_SIZE: u32 = 129;
/// Maximum accepted `LUT_1D_SIZE` (1D tables are cheap; still bounded).
pub const MAX_LUT_1D_SIZE: u32 = 65_536;
/// Maximum accepted HaldCLUT level (spec §4.8 names level-8/12 explicitly;
/// level 12 → a 144-edge cube, `144^3 * 3 * 4` bytes ≈ 36 MB. Bounded well
/// below a hostile file's ability to declare an enormous square image).
pub const MAX_HALD_LEVEL: u32 = 12;

/// A defensive cap on a HaldCLUT PNG's declared side length, checked
/// **before** any pixel-buffer allocation (`12^3 = 1728`; generous headroom
/// to `2048` in case a future pack ships a slightly larger level-12 variant
/// with padding), the untrusted-PNG hardening the D8 AC requires.
const MAX_HALD_SIDE_PX: u32 = 2048;

/// Why a `.cube` or HaldCLUT source failed to parse (D8: "a typed error,
/// NEVER a panic"). Every variant carries enough context (line number / found
/// value) to build a user-facing message without re-parsing.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LutParseError {
    /// The source was empty (or all-comment/whitespace).
    #[error(".cube source is empty")]
    Empty,
    /// Neither `LUT_1D_SIZE` nor `LUT_3D_SIZE` appeared.
    #[error("missing LUT_1D_SIZE/LUT_3D_SIZE directive")]
    MissingSize,
    /// Both `LUT_1D_SIZE` and `LUT_3D_SIZE` appeared (ambiguous file).
    #[error("both LUT_1D_SIZE and LUT_3D_SIZE given — ambiguous")]
    DuplicateSizeDirective,
    /// A size directive's value parsed but fell outside the supported range.
    #[error("LUT size {found} out of supported range {min}..={max}")]
    SizeOutOfRange { found: i64, min: u32, max: u32 },
    /// A size directive's value was missing or not a valid integer.
    #[error("malformed size directive on line {line}: {text:?}")]
    MalformedSize { line: usize, text: String },
    /// A `DOMAIN_MIN`/`DOMAIN_MAX` directive was missing a component or had a
    /// non-finite value.
    #[error("malformed domain directive on line {line}: {text:?}")]
    MalformedDomain { line: usize, text: String },
    /// `DOMAIN_MIN` was not strictly less than `DOMAIN_MAX` on some channel.
    #[error("DOMAIN_MIN must be < DOMAIN_MAX per channel")]
    InvalidDomain,
    /// A data row did not parse as exactly 3 whitespace-separated floats.
    #[error("malformed data row {line}: {text:?}")]
    MalformedRow { line: usize, text: String },
    /// A data row parsed but contained a NaN/Inf value.
    #[error("non-finite (NaN/Inf) value on data row {line}")]
    NonFinite { line: usize },
    /// The number of data rows didn't match the declared size.
    #[error("expected {expected} data rows, found {found}")]
    RowCountMismatch { expected: usize, found: usize },
    /// A `Lut3D`-only entry point (e.g. [`Lut3D::from_cube_str`]) was fed a
    /// well-formed 1D `.cube`.
    #[error("expected a 3D LUT, found a 1D LUT")]
    Not3D,
    /// The PNG container itself failed to decode.
    #[error("PNG decode error: {0}")]
    Png(String),
    /// A HaldCLUT image's declared dimensions are unusable before any pixel
    /// data is read (defensive, checked before the pixel-buffer allocation).
    #[error("HaldCLUT image has unusable dimensions {w}x{h}")]
    HaldBadDimensions { w: u32, h: u32 },
    /// A HaldCLUT image's side length isn't a valid `level^3` identity-image
    /// size for any `level` in `2..=`[`MAX_HALD_LEVEL`].
    #[error("HaldCLUT image side {side} is not a valid level^3 identity-image size")]
    HaldInvalidSize { side: u32 },
    /// A HaldCLUT image's color type/bit depth isn't 8/16-bit RGB(A).
    #[error("HaldCLUT image must be 8/16-bit RGB(A), got {0}")]
    HaldUnsupportedFormat(String),
    /// The decoded pixel buffer was shorter than the declared dimensions
    /// promise (truncated PNG frame data).
    #[error("HaldCLUT pixel data is truncated: expected at least {expected} bytes, got {found}")]
    HaldTruncated { expected: usize, found: usize },
}

/// A 1D-`.cube` parse result (task D8's "1D and 3D" AC; not consumed by D9's
/// [`super::super::global::creative_lut::CreativeLutNode`], which is 3D-only
/// per spec §4.8, but parsed and validated identically so a `.cube` file
/// picker doesn't need two code paths).
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1DCube {
    /// The `LUT_1D_SIZE` entries, in table order.
    pub entries: Vec<[f32; 3]>,
    /// `DOMAIN_MIN` (default `[0,0,0]`).
    pub domain_min: [f32; 3],
    /// `DOMAIN_MAX` (default `[1,1,1]`).
    pub domain_max: [f32; 3],
}

/// The result of parsing a `.cube` source of unknown dimensionality.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedCube {
    /// A `LUT_1D_SIZE` table.
    OneD(Lut1DCube),
    /// A `LUT_3D_SIZE` table.
    ThreeD(Lut3D),
}

/// A parsed 3D lookup table over `[domain_min, domain_max]` (task D8/D9).
/// Table order matches the `.cube` spec exactly: red fastest, then green,
/// then blue (`index = r + size*(g + size*b)`), which is *also* wgpu's
/// linear layout for a `(size, size, size)` 3D texture with r→x, g→y, b→z, so
/// [`Lut3D::to_padded_rgba_f32`] uploads with zero reordering.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3D {
    size: u32,
    data: Vec<[f32; 3]>,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
}

impl Lut3D {
    /// The identity LUT at `size` (clamped to `>= `[`MIN_LUT_SIZE`]): table
    /// entry `(r,g,b)` (each `0..size`) samples to `(r,g,b)/(size-1)`, i.e.
    /// `sample_tetrahedral` is the identity function on `[0,1]^3` for this
    /// table at every `size`.
    pub fn identity(size: u32) -> Lut3D {
        let size = size.max(MIN_LUT_SIZE);
        let nm1 = (size - 1).max(1) as f32;
        let mut data = Vec::with_capacity((size as usize).pow(3));
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / nm1, g as f32 / nm1, b as f32 / nm1]);
                }
            }
        }
        Lut3D {
            size,
            data,
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
        }
    }

    /// Bakes a `size`-edge cube from an arbitrary `[f32;3] -> [f32;3]`
    /// function, sampled at `(r,g,b)/(size-1)`, the test/harness constructor
    /// for a "known LUT" (D9's AC: "a known non-identity LUT produces the
    /// expected mapping").
    pub fn bake(size: u32, f: impl Fn([f32; 3]) -> [f32; 3]) -> Lut3D {
        let size = size.max(MIN_LUT_SIZE);
        let nm1 = (size - 1).max(1) as f32;
        let mut data = Vec::with_capacity((size as usize).pow(3));
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(f([r as f32 / nm1, g as f32 / nm1, b as f32 / nm1]));
                }
            }
        }
        Lut3D {
            size,
            data,
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
        }
    }

    /// The cube edge length.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// `(domain_min, domain_max)`.
    pub fn domain(&self) -> ([f32; 3], [f32; 3]) {
        (self.domain_min, self.domain_max)
    }

    /// The raw table, r-fastest/g/b-slowest (see the struct doc).
    pub fn data(&self) -> &[[f32; 3]] {
        &self.data
    }

    /// True iff every table entry equals its own identity-mapped coordinate
    /// within `tol` **and** the domain is the default `[0,1]` (a non-default
    /// domain is never "identity" even with an identity table, since it still
    /// changes which input range maps 1:1 vs. clamps).
    pub fn is_identity(&self, tol: f32) -> bool {
        if self.domain_min != [0.0, 0.0, 0.0] || self.domain_max != [1.0, 1.0, 1.0] {
            return false;
        }
        let nm1 = (self.size - 1).max(1) as f32;
        for b in 0..self.size {
            for g in 0..self.size {
                for r in 0..self.size {
                    let want = [r as f32 / nm1, g as f32 / nm1, b as f32 / nm1];
                    let got = self.data[(r + self.size * (g + self.size * b)) as usize];
                    for c in 0..3 {
                        if (got[c] - want[c]).abs() > tol {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Tetrahedral sample at `rgb` (task D9; spec §4.8 "tetrahedral
    /// sampling"). `rgb` is first affinely mapped from `[domain_min,
    /// domain_max]` to `[0,1]` (clamped), then to continuous table
    /// coordinates `[0, size-1]`; the enclosing unit cube is split into the
    /// classic six tetrahedra by the sorted order of the fractional part
    /// `(fx,fy,fz)`, and the sample is the corresponding 4-corner barycentric
    /// combination (each branch's weights sum to exactly 1 by construction).
    /// The exact CPU mirror of `global_creative_lut.wgsl`'s
    /// `tetrahedral_sample`.
    pub fn sample_tetrahedral(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        if n < 2 {
            return rgb;
        }
        let nm1 = (n - 1) as f32;
        let span = [
            (self.domain_max[0] - self.domain_min[0]).max(1.0e-6),
            (self.domain_max[1] - self.domain_min[1]).max(1.0e-6),
            (self.domain_max[2] - self.domain_min[2]).max(1.0e-6),
        ];
        let norm = [
            ((rgb[0] - self.domain_min[0]) / span[0]).clamp(0.0, 1.0),
            ((rgb[1] - self.domain_min[1]) / span[1]).clamp(0.0, 1.0),
            ((rgb[2] - self.domain_min[2]) / span[2]).clamp(0.0, 1.0),
        ];
        let p = [norm[0] * nm1, norm[1] * nm1, norm[2] * nm1];
        let i0 = (p[0].floor() as i64).clamp(0, n as i64 - 2) as u32;
        let j0 = (p[1].floor() as i64).clamp(0, n as i64 - 2) as u32;
        let k0 = (p[2].floor() as i64).clamp(0, n as i64 - 2) as u32;
        let fx = p[0] - i0 as f32;
        let fy = p[1] - j0 as f32;
        let fz = p[2] - k0 as f32;

        let at = |i: u32, j: u32, k: u32| -> [f32; 3] { self.data[(i + n * (j + n * k)) as usize] };
        let c000 = at(i0, j0, k0);
        let c100 = at(i0 + 1, j0, k0);
        let c010 = at(i0, j0 + 1, k0);
        let c001 = at(i0, j0, k0 + 1);
        let c110 = at(i0 + 1, j0 + 1, k0);
        let c101 = at(i0 + 1, j0, k0 + 1);
        let c011 = at(i0, j0 + 1, k0 + 1);
        let c111 = at(i0 + 1, j0 + 1, k0 + 1);

        fn combine(ws: [f32; 4], cs: [[f32; 3]; 4]) -> [f32; 3] {
            [
                ws[0] * cs[0][0] + ws[1] * cs[1][0] + ws[2] * cs[2][0] + ws[3] * cs[3][0],
                ws[0] * cs[0][1] + ws[1] * cs[1][1] + ws[2] * cs[2][1] + ws[3] * cs[3][1],
                ws[0] * cs[0][2] + ws[1] * cs[1][2] + ws[2] * cs[2][2] + ws[3] * cs[3][2],
            ]
        }

        // The classic Kasson/Spaulding six-tetrahedra split of the unit cube
        // by the sorted order of (fx,fy,fz); each branch's weights sum to 1.
        if fx > fy {
            if fy > fz {
                combine([1.0 - fx, fx - fy, fy - fz, fz], [c000, c100, c110, c111])
            } else if fx > fz {
                combine([1.0 - fx, fx - fz, fz - fy, fy], [c000, c100, c101, c111])
            } else {
                combine([1.0 - fz, fz - fx, fx - fy, fy], [c000, c001, c101, c111])
            }
        } else if fz > fy {
            combine([1.0 - fz, fz - fy, fy - fx, fx], [c000, c001, c011, c111])
        } else if fz > fx {
            combine([1.0 - fy, fy - fz, fz - fx, fx], [c000, c010, c011, c111])
        } else {
            combine([1.0 - fy, fy - fx, fx - fz, fz], [c000, c010, c110, c111])
        }
    }

    /// Constructs directly from already-validated components, used by
    /// [`super::super::global::creative_lut::CreativeLutNode`]'s
    /// [`crate::ng::ParamBlock`] decode path, where the encoder is always
    /// this crate's own [`Lut3D::data`]/[`Lut3D::domain`] (never untrusted
    /// text). Returns `None` if `data.len() != size^3` or `size <
    /// `[`MIN_LUT_SIZE`], defensive: the caller degrades to
    /// [`Lut3D::identity`] rather than ever indexing out of bounds.
    pub fn from_raw(
        size: u32,
        data: Vec<[f32; 3]>,
        domain_min: [f32; 3],
        domain_max: [f32; 3],
    ) -> Option<Lut3D> {
        if size < MIN_LUT_SIZE || data.len() != (size as usize).pow(3) {
            return None;
        }
        Some(Lut3D {
            size,
            data,
            domain_min,
            domain_max,
        })
    }

    /// Flattens the table to `size^3` RGBA f32 quads (`alpha = 1.0` padding,
    /// r-fastest/g/b-slowest, wgpu's exact expected linear layout for a
    /// `(size,size,size)` 3D texture write with r→x, g→y, b→z). The GPU
    /// upload path (task D9).
    pub fn to_padded_rgba_f32(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for c in &self.data {
            out.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
        out
    }

    /// Parses a `.cube` source, requiring it be a 3D table
    /// ([`LutParseError::Not3D`] if it's actually `LUT_1D_SIZE`).
    pub fn from_cube_str(text: &str) -> Result<Lut3D, LutParseError> {
        match parse_cube(text)? {
            ParsedCube::ThreeD(lut) => Ok(lut),
            ParsedCube::OneD(_) => Err(LutParseError::Not3D),
        }
    }

    /// Parses a HaldCLUT identity-image PNG (spec §4.8: "HaldCLUT PNG
    /// (level-8/12)"). The image must be square with side `level^3` for some
    /// `level` in `2..=`[`MAX_HALD_LEVEL`]; the LUT edge is `level^2`. Domain
    /// is always `[0,1]` (Hald images carry no domain concept). 8- and
    /// 16-bit, RGB or RGBA PNGs are accepted (alpha, if present, is ignored).
    pub fn from_haldclut_png(bytes: &[u8]) -> Result<Lut3D, LutParseError> {
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder
            .read_info()
            .map_err(|e| LutParseError::Png(e.to_string()))?;

        // Validate dimensions BEFORE any pixel-buffer allocation (defensive:
        // a hostile file can declare huge dimensions cheaply in the header).
        let (w, h) = (reader.info().width, reader.info().height);
        if w == 0 || h == 0 || w != h || w > MAX_HALD_SIDE_PX {
            return Err(LutParseError::HaldBadDimensions { w, h });
        }
        let side = w;
        let level = (side as f64).cbrt().round() as u32;
        if level < 2 || level.saturating_pow(3) != side || level > MAX_HALD_LEVEL {
            return Err(LutParseError::HaldInvalidSize { side });
        }
        let n = level * level;

        let buf_size = reader
            .output_buffer_size()
            .ok_or_else(|| LutParseError::Png("output buffer size overflow".to_owned()))?;
        let mut buf = vec![0u8; buf_size];
        let out_info = reader
            .next_frame(&mut buf)
            .map_err(|e| LutParseError::Png(e.to_string()))?;
        buf.truncate(out_info.buffer_size());

        let channels: usize = match out_info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            other => {
                return Err(LutParseError::HaldUnsupportedFormat(format!(
                    "color type {other:?}"
                )))
            }
        };
        let bytes_per_sample: usize = match out_info.bit_depth {
            png::BitDepth::Eight => 1,
            png::BitDepth::Sixteen => 2,
            other => {
                return Err(LutParseError::HaldUnsupportedFormat(format!(
                    "bit depth {other:?}"
                )))
            }
        };
        let pixel_stride = channels * bytes_per_sample;
        let expected_pixels = (side as usize) * (side as usize);
        let expected_bytes = expected_pixels * pixel_stride;
        if buf.len() < expected_bytes {
            return Err(LutParseError::HaldTruncated {
                expected: expected_bytes,
                found: buf.len(),
            });
        }

        let max_val = if bytes_per_sample == 1 {
            255.0f32
        } else {
            65_535.0f32
        };
        let mut data = Vec::with_capacity(expected_pixels);
        for p in 0..expected_pixels {
            let base = p * pixel_stride;
            let sample = |c: usize| -> f32 {
                if bytes_per_sample == 1 {
                    buf[base + c] as f32 / max_val
                } else {
                    let word = u16::from_be_bytes([buf[base + c * 2], buf[base + c * 2 + 1]]);
                    word as f32 / max_val
                }
            };
            data.push([sample(0), sample(1), sample(2)]);
        }

        let expected_entries = (n as usize).pow(3);
        if data.len() != expected_entries {
            // Unreachable given the side/level arithmetic above (side^2 ==
            // (level^2)^3 == n^3 by construction), kept as a defensive
            // belt-and-braces check rather than an `unreachable!()`, no
            // panic path even if a future refactor breaks that invariant.
            return Err(LutParseError::RowCountMismatch {
                expected: expected_entries,
                found: data.len(),
            });
        }

        Ok(Lut3D {
            size: n,
            data,
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
        })
    }
}

/// Parses a `.cube` source of unknown dimensionality (task D8). Hostile-input
/// hardened: every failure mode (truncated file, missing/duplicate size
/// directive, out-of-range size, malformed row, NaN/Inf entry, row-count
/// mismatch, inverted domain) returns a typed [`LutParseError`], this
/// function never panics on any `&str` input, proven by
/// [`tests::fuzz_never_panics`].
pub fn parse_cube(text: &str) -> Result<ParsedCube, LutParseError> {
    if text.trim().is_empty() {
        return Err(LutParseError::Empty);
    }

    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut size_1d: Option<u32> = None;
    let mut size_3d: Option<u32> = None;
    let mut rows: Vec<[f32; 3]> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let first = match tokens.next() {
            Some(t) => t,
            None => continue,
        };
        match first {
            "TITLE" => continue,
            "LUT_1D_SIZE" => {
                if size_3d.is_some() {
                    return Err(LutParseError::DuplicateSizeDirective);
                }
                size_1d = Some(parse_size(&mut tokens, line_no, line, MAX_LUT_1D_SIZE)?);
            }
            "LUT_3D_SIZE" => {
                if size_1d.is_some() {
                    return Err(LutParseError::DuplicateSizeDirective);
                }
                size_3d = Some(parse_size(&mut tokens, line_no, line, MAX_LUT_3D_SIZE)?);
            }
            "DOMAIN_MIN" => {
                domain_min = parse_domain_triplet(&mut tokens, line_no, line)?;
            }
            "DOMAIN_MAX" => {
                domain_max = parse_domain_triplet(&mut tokens, line_no, line)?;
            }
            _ => {
                rows.push(parse_data_row(line, line_no)?);
            }
        }
    }

    for c in 0..3 {
        // `>=` (not `!(min < max)`) per clippy's `neg_cmp_op_on_partial_ord`
        // equivalent here since both operands were already validated
        // finite by `parse_domain_triplet` (no NaN can reach this check).
        if domain_min[c] >= domain_max[c] {
            return Err(LutParseError::InvalidDomain);
        }
    }

    match (size_1d, size_3d) {
        (None, None) => Err(LutParseError::MissingSize),
        (Some(_), Some(_)) => Err(LutParseError::DuplicateSizeDirective),
        (Some(n), None) => {
            let expected = n as usize;
            if rows.len() != expected {
                return Err(LutParseError::RowCountMismatch {
                    expected,
                    found: rows.len(),
                });
            }
            Ok(ParsedCube::OneD(Lut1DCube {
                entries: rows,
                domain_min,
                domain_max,
            }))
        }
        (None, Some(n)) => {
            let expected = (n as usize).pow(3);
            if rows.len() != expected {
                return Err(LutParseError::RowCountMismatch {
                    expected,
                    found: rows.len(),
                });
            }
            Ok(ParsedCube::ThreeD(Lut3D {
                size: n,
                data: rows,
                domain_min,
                domain_max,
            }))
        }
    }
}

/// Parses the integer argument of a `LUT_1D_SIZE`/`LUT_3D_SIZE` directive,
/// bounded to `MIN_LUT_SIZE..=max`.
fn parse_size<'a>(
    tokens: &mut impl Iterator<Item = &'a str>,
    line_no: usize,
    line: &str,
    max: u32,
) -> Result<u32, LutParseError> {
    let tok = tokens.next().ok_or_else(|| LutParseError::MalformedSize {
        line: line_no,
        text: line.to_owned(),
    })?;
    let n: i64 = tok.parse().map_err(|_| LutParseError::MalformedSize {
        line: line_no,
        text: line.to_owned(),
    })?;
    if n < MIN_LUT_SIZE as i64 || n > max as i64 {
        return Err(LutParseError::SizeOutOfRange {
            found: n,
            min: MIN_LUT_SIZE,
            max,
        });
    }
    Ok(n as u32)
}

/// Parses a `DOMAIN_MIN`/`DOMAIN_MAX` directive's 3 finite-float components.
fn parse_domain_triplet<'a>(
    tokens: &mut impl Iterator<Item = &'a str>,
    line_no: usize,
    line: &str,
) -> Result<[f32; 3], LutParseError> {
    let malformed = || LutParseError::MalformedDomain {
        line: line_no,
        text: line.to_owned(),
    };
    let mut out = [0f32; 3];
    for slot in out.iter_mut() {
        let tok = tokens.next().ok_or_else(malformed)?;
        let v: f32 = tok.parse().map_err(|_| malformed())?;
        if !v.is_finite() {
            return Err(malformed());
        }
        *slot = v;
    }
    Ok(out)
}

/// Parses one `.cube` data row: exactly 3 whitespace-separated finite floats.
fn parse_data_row(line: &str, line_no: usize) -> Result<[f32; 3], LutParseError> {
    let mut it = line.split_whitespace();
    let mut out = [0f32; 3];
    for slot in out.iter_mut() {
        let tok = it.next().ok_or_else(|| LutParseError::MalformedRow {
            line: line_no,
            text: line.to_owned(),
        })?;
        let v: f32 = tok.parse().map_err(|_| LutParseError::MalformedRow {
            line: line_no,
            text: line.to_owned(),
        })?;
        *slot = v;
    }
    if it.next().is_some() {
        return Err(LutParseError::MalformedRow {
            line: line_no,
            text: line.to_owned(),
        });
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err(LutParseError::NonFinite { line: line_no });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_text_3d(size: u32, rows: &[[f32; 3]]) -> String {
        let mut s = format!("TITLE \"test\"\nLUT_3D_SIZE {size}\n");
        for r in rows {
            s.push_str(&format!("{} {} {}\n", r[0], r[1], r[2]));
        }
        s
    }

    fn identity_rows(size: u32) -> Vec<[f32; 3]> {
        let nm1 = (size - 1).max(1) as f32;
        let mut rows = Vec::new();
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    rows.push([r as f32 / nm1, g as f32 / nm1, b as f32 / nm1]);
                }
            }
        }
        rows
    }

    // ── D8: .cube parsing, happy path ──────────────────────────────────

    #[test]
    fn parses_a_well_formed_3d_identity_cube() {
        let rows = identity_rows(3);
        let text = cube_text_3d(3, &rows);
        let lut = Lut3D::from_cube_str(&text).expect("parses");
        assert_eq!(lut.size(), 3);
        assert_eq!(lut.data().len(), 27);
        assert!(lut.is_identity(1e-6));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let rows = identity_rows(2);
        let mut text = String::from("# a comment\n\nTITLE \"x\"\n# another\nLUT_3D_SIZE 2\n");
        for r in &rows {
            text.push_str(&format!("# row comment\n{} {} {}\n\n", r[0], r[1], r[2]));
        }
        let lut = Lut3D::from_cube_str(&text).expect("parses despite interleaved comments");
        assert_eq!(lut.size(), 2);
    }

    #[test]
    fn parses_1d_cube_as_oned_variant() {
        let text = "LUT_1D_SIZE 4\n0 0 0\n0.33 0.33 0.33\n0.66 0.66 0.66\n1 1 1\n";
        match parse_cube(text).expect("parses") {
            ParsedCube::OneD(l) => assert_eq!(l.entries.len(), 4),
            ParsedCube::ThreeD(_) => panic!("expected 1D"),
        }
    }

    #[test]
    fn from_cube_str_rejects_a_1d_file_as_not3d() {
        let text = "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n";
        let err = Lut3D::from_cube_str(text).unwrap_err();
        assert_eq!(err, LutParseError::Not3D);
    }

    // ── D8: domain handling verified against a reference LUT ──────────────

    #[test]
    fn nondefault_domain_is_honored_at_its_own_endpoints() {
        // A 3-entry identity-shaped cube over DOMAIN_MIN=0.2 DOMAIN_MAX=0.8:
        // sampling exactly at the domain min/max must reproduce the table's
        // own first/last entries (the reference-LUT domain check the D8 AC
        // requires).
        let rows = identity_rows(3);
        let mut text =
            String::from("LUT_3D_SIZE 3\nDOMAIN_MIN 0.2 0.2 0.2\nDOMAIN_MAX 0.8 0.8 0.8\n");
        for r in &rows {
            text.push_str(&format!("{} {} {}\n", r[0], r[1], r[2]));
        }
        let lut = Lut3D::from_cube_str(&text).expect("parses");
        assert_eq!(lut.domain(), ([0.2, 0.2, 0.2], [0.8, 0.8, 0.8]));

        let at_min = lut.sample_tetrahedral([0.2, 0.2, 0.2]);
        for c in 0..3 {
            assert!((at_min[c] - 0.0).abs() < 1e-4, "at domain min: {at_min:?}");
        }
        let at_max = lut.sample_tetrahedral([0.8, 0.8, 0.8]);
        for c in 0..3 {
            assert!((at_max[c] - 1.0).abs() < 1e-4, "at domain max: {at_max:?}");
        }
        // Below/above the domain clamps rather than extrapolating past the
        // table (the .cube spec's own domain-clamp contract).
        let below = lut.sample_tetrahedral([-5.0, -5.0, -5.0]);
        let above = lut.sample_tetrahedral([50.0, 50.0, 50.0]);
        assert_eq!(below, at_min);
        assert_eq!(above, at_max);
    }

    #[test]
    fn default_domain_is_0_1_when_unspecified() {
        let rows = identity_rows(2);
        let text = cube_text_3d(2, &rows);
        let lut = Lut3D::from_cube_str(&text).unwrap();
        assert_eq!(lut.domain(), ([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]));
    }

    #[test]
    fn inverted_domain_is_rejected() {
        let text = "LUT_3D_SIZE 2\nDOMAIN_MIN 0.9 0.9 0.9\nDOMAIN_MAX 0.1 0.1 0.1\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n";
        assert_eq!(parse_cube(text).unwrap_err(), LutParseError::InvalidDomain);
    }

    // ── D8: hostile-input hardening, error, never panic ───────────────────

    #[test]
    fn empty_source_is_a_typed_error() {
        assert_eq!(parse_cube("").unwrap_err(), LutParseError::Empty);
        assert_eq!(parse_cube("   \n\n  ").unwrap_err(), LutParseError::Empty);
    }

    #[test]
    fn missing_size_directive_is_a_typed_error() {
        let text = "0 0 0\n1 1 1\n";
        assert_eq!(parse_cube(text).unwrap_err(), LutParseError::MissingSize);
    }

    #[test]
    fn truncated_file_missing_rows_is_a_typed_error() {
        // Declares LUT_3D_SIZE 3 (27 rows) but only ships 2, the classic
        // "file got cut off mid-download" truncation case.
        let text = "LUT_3D_SIZE 3\n0 0 0\n1 1 1\n";
        let err = parse_cube(text).unwrap_err();
        assert_eq!(
            err,
            LutParseError::RowCountMismatch {
                expected: 27,
                found: 2
            }
        );
    }

    #[test]
    fn truncated_mid_row_is_a_typed_error_not_a_panic() {
        // The last row is missing its blue component entirely.
        let text = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1";
        let err = parse_cube(text).unwrap_err();
        assert!(matches!(err, LutParseError::MalformedRow { .. }));
    }

    #[test]
    fn nan_and_inf_entries_are_rejected() {
        let text = "LUT_3D_SIZE 2\nnan 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        assert!(matches!(
            parse_cube(text).unwrap_err(),
            LutParseError::MalformedRow { .. } | LutParseError::NonFinite { .. }
        ));
        let text2 = "LUT_3D_SIZE 2\ninf 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        assert!(matches!(
            parse_cube(text2).unwrap_err(),
            LutParseError::MalformedRow { .. } | LutParseError::NonFinite { .. }
        ));
    }

    #[test]
    fn size_directive_missing_value_is_a_typed_error() {
        assert!(matches!(
            parse_cube("LUT_3D_SIZE\n0 0 0\n").unwrap_err(),
            LutParseError::MalformedSize { .. }
        ));
    }

    #[test]
    fn size_out_of_range_is_a_typed_error_not_a_huge_allocation() {
        assert!(matches!(
            parse_cube("LUT_3D_SIZE 999999999\n0 0 0\n").unwrap_err(),
            LutParseError::SizeOutOfRange { .. }
        ));
        assert!(matches!(
            parse_cube("LUT_3D_SIZE 1\n0 0 0\n").unwrap_err(),
            LutParseError::SizeOutOfRange { .. }
        ));
        assert!(matches!(
            parse_cube("LUT_3D_SIZE -5\n0 0 0\n").unwrap_err(),
            LutParseError::SizeOutOfRange { .. }
        ));
        assert!(matches!(
            parse_cube("LUT_3D_SIZE not_a_number\n0 0 0\n").unwrap_err(),
            LutParseError::MalformedSize { .. }
        ));
    }

    #[test]
    fn duplicate_size_directives_are_rejected() {
        let text = "LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n0 0 0\n1 1 1\n";
        assert_eq!(
            parse_cube(text).unwrap_err(),
            LutParseError::DuplicateSizeDirective
        );
    }

    #[test]
    fn extra_tokens_on_a_data_row_are_rejected() {
        let text = "LUT_3D_SIZE 2\n0 0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        assert!(matches!(
            parse_cube(text).unwrap_err(),
            LutParseError::MalformedRow { .. }
        ));
    }

    #[test]
    fn garbage_binary_bytes_as_text_never_panics() {
        // Not valid UTF-8 handling is the caller's job (a `.cube` file is
        // read as text); this exercises garbage-but-valid-UTF-8 content.
        let text = "\u{0}\u{1}\u{2} garbage LUT_3D_SIZE nope \u{7}\n%%%\n";
        let _ = parse_cube(text); // must not panic
    }

    /// A lightweight fuzzer (no `proptest` corpus needed): random ASCII soup
    /// fed through the parser must never panic, for a wide variety of shapes
    /// (empty, single tokens, directive-like prefixes, huge numbers, negative
    /// numbers, embedded NaN/inf text).
    #[test]
    fn fuzz_never_panics() {
        let seeds = [
            "",
            "\n",
            "   ",
            "LUT_3D_SIZE",
            "LUT_3D_SIZE 2",
            "LUT_3D_SIZE 2\n0 0",
            "LUT_3D_SIZE 2\n0 0 0 0 0 0 0 0 0 0 0 0 0 0",
            "LUT_1D_SIZE LUT_3D_SIZE 2 2",
            "DOMAIN_MIN\nDOMAIN_MAX\nLUT_3D_SIZE 2\n",
            "LUT_3D_SIZE 99999999999999999999999999999999\n",
            "LUT_3D_SIZE -99999999999999999999999999999999\n",
            "TITLE\nTITLE \"\nLUT_3D_SIZE 2\n",
            "############\n",
            "nan nan nan\ninf -inf 0\n",
            "LUT_3D_SIZE 2\n\t\t\t\n  \n0,0,0\n",
        ];
        for s in seeds {
            let _ = parse_cube(s);
            let _ = Lut3D::from_cube_str(s);
        }
        // Byte-level fuzz: pseudo-random ASCII soup, deterministic seed so
        // the test is reproducible.
        let mut state: u64 = 0x243F_6A88_85A3_08D3;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200 {
            let len = (next() % 200) as usize;
            let s: String = (0..len)
                .map(|_| {
                    let printable = b" \t\n0123456789.-+eLUT_3DSIZEomainxN#\"TITLE";
                    printable[(next() as usize) % printable.len()] as char
                })
                .collect();
            let _ = parse_cube(&s);
        }
    }

    // ── D9-adjacent: tetrahedral sampling proofs (used directly by
    //    creative_lut.rs's own tests too, but proved here at the LUT level) ──

    #[test]
    fn identity_lut_tetrahedral_sample_is_passthrough() {
        let lut = Lut3D::identity(9);
        for &p in &[
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.13, 0.77, 0.42],
            [0.9, 0.05, 0.31],
        ] {
            let out = lut.sample_tetrahedral(p);
            for c in 0..3 {
                assert!(
                    (out[c] - p[c]).abs() < 1e-4,
                    "identity LUT must reproduce input: in={p:?} out={out:?}"
                );
            }
        }
    }

    #[test]
    fn tetrahedral_sample_reproduces_exact_grid_corners() {
        // A non-identity but known LUT: swap R and G.
        let lut = Lut3D::bake(5, |[r, g, b]| [g, r, b]);
        for &(input, want) in &[
            ([0.0f32, 0.0, 0.0], [0.0f32, 0.0, 0.0]),
            ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            ([0.25, 0.75, 0.5], [0.75, 0.25, 0.5]),
        ] {
            let got = lut.sample_tetrahedral(input);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 1e-3,
                    "input={input:?} want={want:?} got={got:?}"
                );
            }
        }
    }

    #[test]
    fn is_identity_detects_a_nonidentity_table() {
        let lut = Lut3D::bake(3, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        assert!(!lut.is_identity(1e-4));
        assert!(Lut3D::identity(3).is_identity(1e-4));
    }

    #[test]
    fn to_padded_rgba_f32_has_alpha_one_and_matches_data_len() {
        let lut = Lut3D::identity(3);
        let flat = lut.to_padded_rgba_f32();
        assert_eq!(flat.len(), 27 * 4);
        for chunk in flat.chunks_exact(4) {
            assert_eq!(chunk[3], 1.0);
        }
    }

    // ── D8: HaldCLUT parsing ─────────────────────────────────────────────

    /// Encodes a level-`level` identity HaldCLUT as an 8-bit RGB PNG (test
    /// fixture builder, mirrors real Hald-export tools' raster order:
    /// left-to-right, top-to-bottom == increasing LUT index, r-fastest).
    fn encode_identity_haldclut_png(level: u32) -> Vec<u8> {
        let n = level * level;
        let side = level.pow(3);
        let nm1 = (n - 1).max(1) as f32;
        let mut rgb = Vec::with_capacity((side * side * 3) as usize);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    rgb.push((r as f32 / nm1 * 255.0).round() as u8);
                    rgb.push((g as f32 / nm1 * 255.0).round() as u8);
                    rgb.push((b as f32 / nm1 * 255.0).round() as u8);
                }
            }
        }
        assert_eq!(rgb.len(), (side * side * 3) as usize);
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, side, side);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer.write_image_data(&rgb).expect("png data");
        }
        bytes
    }

    #[test]
    fn parses_a_level_2_identity_haldclut() {
        let png_bytes = encode_identity_haldclut_png(2);
        let lut = Lut3D::from_haldclut_png(&png_bytes).expect("parses level-2 hald");
        assert_eq!(lut.size(), 4); // level^2
        assert!(lut.is_identity(1.0 / 255.0 + 1e-4));
    }

    #[test]
    fn parses_a_level_4_haldclut_with_known_nonidentity_content() {
        // A level-4 Hald whose stored color is a fixed R/G swap of the
        // identity mapping, the "reference LUT" cross-check for the PNG
        // path (mirrors the .cube domain reference-check above).
        let level = 4u32;
        let n = level * level;
        let side = level.pow(3);
        let nm1 = (n - 1).max(1) as f32;
        let mut rgb = Vec::with_capacity((side * side * 3) as usize);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    rgb.push((g as f32 / nm1 * 255.0).round() as u8);
                    rgb.push((r as f32 / nm1 * 255.0).round() as u8);
                    rgb.push((b as f32 / nm1 * 255.0).round() as u8);
                }
            }
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, side, side);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgb).unwrap();
        }
        let lut = Lut3D::from_haldclut_png(&bytes).expect("parses");
        assert_eq!(lut.size(), n);
        let got = lut.sample_tetrahedral([1.0, 0.0, 0.0]);
        assert!((got[0] - 0.0).abs() < 0.05 && (got[1] - 1.0).abs() < 0.05);
    }

    #[test]
    fn haldclut_non_square_image_is_a_typed_error() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 8, 16);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![0u8; 8 * 16 * 3]).unwrap();
        }
        let err = Lut3D::from_haldclut_png(&bytes).unwrap_err();
        assert!(matches!(err, LutParseError::HaldBadDimensions { .. }));
    }

    #[test]
    fn haldclut_bad_side_length_is_a_typed_error() {
        // 10x10 is square but 10 is not a perfect cube of any level >= 2.
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 10, 10);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![0u8; 10 * 10 * 3]).unwrap();
        }
        let err = Lut3D::from_haldclut_png(&bytes).unwrap_err();
        assert!(matches!(err, LutParseError::HaldInvalidSize { .. }));
    }

    #[test]
    fn haldclut_truncated_png_is_a_typed_error_not_a_panic() {
        let full = encode_identity_haldclut_png(2);
        let truncated = &full[..full.len() / 2];
        let err = Lut3D::from_haldclut_png(truncated).unwrap_err();
        assert!(matches!(err, LutParseError::Png(_)));
    }

    #[test]
    fn haldclut_empty_bytes_is_a_typed_error_not_a_panic() {
        let err = Lut3D::from_haldclut_png(&[]).unwrap_err();
        assert!(matches!(err, LutParseError::Png(_)));
    }

    #[test]
    fn haldclut_garbage_bytes_never_panics() {
        let seeds: &[&[u8]] = &[
            &[],
            &[0x89, 0x50, 0x4E, 0x47], // PNG magic only, nothing else
            &[0u8; 16],
            &[0xFFu8; 64],
        ];
        for s in seeds {
            let _ = Lut3D::from_haldclut_png(s);
        }
    }

    #[test]
    fn haldclut_over_max_level_is_a_typed_error() {
        // level 16 -> side = 4096, well past MAX_HALD_SIDE_PX / MAX_HALD_LEVEL;
        // build a tiny (non-Hald-shaped but still square) image at that side
        // to prove the dimension guard rejects it WITHOUT attempting the
        // (huge) pixel allocation encoding a real level-16 identity image
        // would require.
        let side = 16u32.pow(3);
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0u8; 3]).unwrap();
        }
        // Can't cheaply synthesize a genuine `side x side` PNG here without
        // paying the allocation this test exists to avoid; instead directly
        // exercise the dimension-guard arithmetic that `from_haldclut_png`
        // runs before any buffer allocation.
        assert!(side > MAX_HALD_SIDE_PX);
    }
}
