// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Color-pipeline error taxonomy (spec §3.3–§3.5). Defined in Phase A so every
//! module compiles against a stable error surface; the phases that own each
//! module add variants (`#[non_exhaustive]`) as their bodies land.

/// Errors from the camera-color / white-balance / transform solvers
/// (spec §3.3/§3.4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ColorError {
    /// A matrix that must be inverted was singular/degenerate.
    #[error("singular matrix: {0}")]
    SingularMatrix(&'static str),
    /// The white-point self-consistent iteration failed to converge (spec §3.3
    /// B3: non-convergence is a structured error, never a hang).
    #[error("white-point iteration did not converge after {iterations} steps")]
    NonConvergent {
        /// Iterations attempted before giving up.
        iterations: u32,
    },
    /// A requested profile/look reference could not be resolved and no fallback
    /// applied.
    #[error("unresolvable profile reference: {0}")]
    UnresolvableProfile(String),
    /// Invalid input parameters (out-of-range CCT, malformed LUT dims, …).
    #[error("invalid color input: {0}")]
    InvalidInput(String),
}

/// Errors from the LCMS2 / ICC surface (spec §3.5).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IccError {
    /// The ICC bytes failed to parse (untrusted input — capped, never panics).
    #[error("invalid ICC profile: {0}")]
    InvalidProfile(String),
    /// The profile exceeded the size cap (untrusted-input hygiene, D7).
    #[error("ICC profile exceeds the size cap ({size} bytes)")]
    TooLarge {
        /// The offending size.
        size: usize,
    },
    /// LCMS2 could not build the requested transform.
    #[error("transform build failed: {0}")]
    TransformBuild(String),
}

/// Errors from `.lblook` loading/validation (spec §3.4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LookError {
    /// The container failed to decode.
    #[error("malformed .lblook: {0}")]
    Malformed(String),
    /// The `.lblook` version is newer than this build understands (E1: unknown
    /// version rejected with a structured error).
    #[error("unsupported .lblook version {0}")]
    UnsupportedVersion(u16),
}

/// Errors from `.dcp` parsing (spec §3.1 `parse_dcp` — untrusted input, fuzzed,
/// capped).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DcpParseError {
    /// The TIFF-IFD container is malformed.
    #[error("malformed .dcp: {0}")]
    Malformed(String),
    /// A dims × table-size cap was exceeded (F3 hardening).
    #[error("dcp resource cap exceeded: {0}")]
    ResourceCap(&'static str),
}
