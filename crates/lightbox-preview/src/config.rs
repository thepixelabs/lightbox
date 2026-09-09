// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Store configuration (E03 spec §5.7, Phase A T01).
//!
//! Every field here is Phase-A-owned (the store's shape) or a documented
//! placeholder a later phase fills behavior in for (e.g. `t1_codec`/
//! `t1_quality` are read by Phase C's encoder, not by anything in Phase A).
//! Cache limits/root live in the core prefs store at runtime (E01/E08 bind
//! them); this struct is the plain-data shape that travels between them and
//! [`crate::Store::open`].

use std::path::PathBuf;

/// The on-disk cache store's configuration (spec §5.7).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewStoreConfig {
    /// `<catalog-name>.lbdata` by default, the SAME directory the catalog
    /// lives in (spec §3.2: `previews/`/`rawcache/`/… are siblings of
    /// `catalog.sqlite` inside one `.lbdata` dir, not a separate directory
    /// tree). Relocatable (Phase F, T21).
    pub root: PathBuf,
    pub limits: CacheLimits,
    /// `Auto` (largest display long edge, clamped) | `Fixed(px)`, read by
    /// Phase C's T1 build pipeline (T12); Phase A only carries the value.
    pub standard_px: StandardSize,
    /// T1 encoder choice; `Jxl` requires the `jxl` feature (T11, Phase C).
    /// Phase A never encodes anything, so this is inert here.
    pub t1_codec: Codec,
    /// JXL distance-mapped quality knob; Phase C interprets it.
    pub t1_quality: u8,
    /// 256 default (spec §4.3), store parameter, not an API change if
    /// tuned (Q2). Phase F (T20) is the first consumer.
    pub t2_tile_px: u32,
    pub t2_retention: Retention,
    /// RAM LRU budget for decoded buffers; Phase B (T09) owns the LRU
    /// itself. Default 512 MiB.
    pub decoded_lru_bytes: u64,
    /// `None` = `min(physical_cores, 8)` (spec §5.7); Phase D's scheduler
    /// (T13) is the consumer.
    pub workers: Option<usize>,
    /// zstd level for the raw cache container (Phase E, T17). Default 3.
    pub zstd_level: i32,
}

impl PreviewStoreConfig {
    /// Spec §5.7 defaults, rooted at `root`.
    pub fn with_defaults(root: PathBuf) -> PreviewStoreConfig {
        PreviewStoreConfig {
            root,
            limits: CacheLimits::default(),
            standard_px: StandardSize::Auto,
            t1_codec: Codec::Jpeg, // JXL is feature-gated (T11); JPEG is always available.
            t1_quality: 90,
            t2_tile_px: 256,
            t2_retention: Retention::Days(30),
            decoded_lru_bytes: 512 * 1024 * 1024,
            workers: None,
            zstd_level: 3,
        }
    }
}

/// Store-wide size caps (spec §5.7); LRU eviction (Phase F, T19) enforces
/// them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CacheLimits {
    /// Default 20 GiB (Q6).
    pub preview_cap_bytes: u64,
    /// Default 5 GiB (spec §3.3).
    pub rawcache_cap_bytes: u64,
}

impl Default for CacheLimits {
    fn default() -> CacheLimits {
        CacheLimits {
            preview_cap_bytes: 20 * 1024 * 1024 * 1024,
            rawcache_cap_bytes: 5 * 1024 * 1024 * 1024,
        }
    }
}

/// T1 standard-preview sizing policy (spec §5.7, Open Question Q3).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StandardSize {
    /// Largest display long edge seen, clamped `[1280, 3840]`. Existing T1s
    /// are kept as "best available"; a bigger variant builds on demand
    /// (Q3, no proactive rebuild).
    Auto,
    /// A pinned long-edge size in pixels.
    Fixed(u32),
}

/// Preview tier codec (spec §5.1/§5.3). `Jpeg` is always available; `Jxl`
/// requires the own minimal libjxl FFI (T11, feature `jxl`), R1's license
/// reversal trigger falls back to `Jpeg` with no API change.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Codec {
    Jpeg = 0,
    Jxl = 1,
}

impl Codec {
    /// File extension for the store path (T05 key-derivation helpers).
    pub fn file_extension(self) -> &'static str {
        match self {
            Codec::Jpeg => "jpg",
            Codec::Jxl => "jxl",
        }
    }
}

/// T2 age-based retention (spec §3.2, the LrC v14 convention).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Retention {
    Days(u32),
    Never,
}
