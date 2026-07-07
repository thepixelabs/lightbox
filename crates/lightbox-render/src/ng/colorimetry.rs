// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Colorimetry **tags** carried across the source/output seams (spec §3.4/§3.6/
//! §3.8).
//!
//! The engine contains **zero color science** (E02 guardrail): these are opaque
//! provenance tags threaded through unchanged. The engine-owned `xform.display`
//! node (task, A-gpu) converts working→display by calling `lightbox-color`;
//! these types will be reconciled with `lightbox-color`'s descriptors when that
//! node lands (SCAFFOLD placeholders until then).

/// The colorimetry of source pixels handed in over the [`crate::ng::source`]
/// seam (spec §3.4/§3.8). Opaque to the engine.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SourceColorimetry {}

/// The colorimetry of a completed [`crate::ng::RenderOutput`] (spec §3.6).
/// Opaque to the engine; export (E15) records it.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct OutputColorimetry {}

/// Whether the compiler's source is raw (mosaic, E11) or already-RGB (spec
/// §3.4). v1 templates take `Rgb` — decode/demosaic is upstream (E02/E11).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SourceKind {
    /// Already-demosaiced RGB (the M1 reality — E02 returns demosaiced RGB).
    Rgb,
    /// Raw CFA mosaic — reserved for E11's on-graph demosaic.
    Raw {
        /// Opaque CFA-pattern descriptor (E11 defines the real shape).
        cfa: CfaDesc,
    },
}

/// Opaque CFA-pattern descriptor placeholder — E11 owns the real definition.
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
