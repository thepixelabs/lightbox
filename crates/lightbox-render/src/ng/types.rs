// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Core identity & geometry types — spec §3.1, transcribed verbatim.
//!
//! Owner: **A-core** (task **A2** — Roi/Extent/TileCoord algebra, RenderScale,
//! PortType, TilePrecision; unit tests for expand/intersect/tiles + serde
//! round-trip where derived).

use serde::{Deserialize, Serialize};

// ── identity ─────────────────────────────────────────────────────────────────

/// Stable node identity — a dotted namespace, e.g. `"xform.display"`,
/// `"tone.exposure"` (spec §3.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub &'static str);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Pipeline process version — `PV1 == ProcessVersion(1)`; stored per image
/// (`edit_recipe.pv`, E01 schema). Append-only forever (§4.5). Re-exported from
/// `lightbox-types` so the whole workspace shares one definition (spec §3.1).
pub use lightbox_types::ProcessVersion;

// ── geometry (source-pixel coordinate frame unless stated) ───────────────────

/// A pixel extent (width × height).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Extent {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// A region of interest, in source-pixel coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Roi {
    /// Left edge (may be negative for apron growth past the image border).
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Roi {
    /// Apron growth for neighborhood nodes: expand on every side by `margin`
    /// (spec §3.1). **A2 owns the real algebra.**
    pub fn expand(&self, margin: u32) -> Roi {
        let _ = margin;
        unimplemented!("A2 (A-core): Roi::expand — apron growth for neighborhood nodes")
    }

    /// Intersection with `other`, or `None` when disjoint (spec §3.1).
    /// **A2 owns the real algebra.**
    pub fn intersect(&self, other: &Roi) -> Option<Roi> {
        let _ = other;
        unimplemented!("A2 (A-core): Roi::intersect")
    }

    /// The `size`²-tile grid covering this ROI (256² default; spec §3.1/§4.3).
    ///
    /// SCAFFOLD placeholder yields nothing; **A2 implements the real tiling**
    /// (kept as `impl Iterator` per the frozen signature).
    pub fn tiles(&self, size: u32) -> impl Iterator<Item = TileCoord> {
        let _ = size;
        std::iter::empty()
    }
}

/// Coordinates of one tile: grid position plus the quantized scale it was
/// evaluated at (spec §3.1; `scale_q` per §3.5 — 1/64ths).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TileCoord {
    /// Tile column.
    pub tx: u32,
    /// Tile row.
    pub ty: u32,
    /// Quantized render scale (see §3.5 — RenderScale → 1/64ths).
    pub scale_q: u16,
}

/// How much of the source resolution a render evaluates at (spec §3.1).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum RenderScale {
    /// Engine derives decimation so output ≤ the given viewport extent
    /// (≤ ~8 MP for a 45 MP source at a 4K fit).
    Fit(Extent),
    /// An explicit fraction of source resolution, in `(0, 1]`.
    Ratio(f32),
    /// 1:1 zoom — tiled, demand-driven.
    OneToOne,
}

/// Typed image-port kind; graph edges are type-checked at build (spec §3.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PortType {
    /// Reserved for E11 (GPU demosaic); no v1 engine node produces it.
    MosaicU16,
    /// The working format: scene-linear, ProPhoto primaries, RGBA16F (§4.2).
    LinearRgbaF16,
    /// Precision escape hatch for accumulation-heavy stages (§4.2).
    LinearRgbaF32,
    /// Grayscale mask weight buffers (E12 / baked AI rasters §4.5).
    WeightR16,
    /// Post-display-transform output the canvas composites.
    DisplayRgba8,
}

/// Output tile precision (spec §3.1/§3.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TilePrecision {
    /// `rgba16float` storage, f32 compute (the default).
    F16,
    /// `rgba32float` — accumulation-sensitive stages (§4.2).
    F32,
}
