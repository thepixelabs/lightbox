// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Engine configuration, spec §3.6, transcribed.
//!
//! Owner: **A-core** (task **A1**). Bound by the E08 prefs panel at the shell.

use std::time::Duration;

/// Construction-time engine configuration (spec §3.6).
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Tile edge, pixels. Default 256 (§4.3).
    pub tile_size: u32,
    /// VRAM budget policy.
    pub vram_budget: VramBudget,
    /// rayon pool size for the CPU path; `None` = rayon default.
    pub cpu_threads: Option<usize>,
    /// Backend preference (prefs / debug).
    pub backend: BackendPref,
    /// Device-lost degradation policy (§4.4).
    pub device_lost_degrade: DegradePolicy,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            tile_size: 256,
            vram_budget: VramBudget::Auto,
            cpu_threads: None,
            backend: BackendPref::Auto,
            device_lost_degrade: DegradePolicy::default(),
        }
    }
}

/// VRAM budget policy (spec §3.6). `Auto` probes `min(60% adapter mem, cap)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VramBudget {
    /// Probe the adapter: `min(60% adapter mem, cap)`.
    Auto,
    /// A fixed byte budget.
    Bytes(u64),
}

/// Backend preference (spec §3.6). Bound by the E08 prefs panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackendPref {
    /// GPU when available, else CPU.
    #[default]
    Auto,
    /// Force the CPU path (prefs / debug).
    ForceCpu,
}

/// Device-lost degradation policy (spec §3.6): after `max_losses` within
/// `window`, degrade to preview-resolution CPU editing (§4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DegradePolicy {
    /// Losses tolerated before degrading (default 2).
    pub max_losses: u8,
    /// The window losses are counted within (default 60 s).
    pub window: Duration,
}

impl Default for DegradePolicy {
    fn default() -> Self {
        DegradePolicy {
            max_losses: 2,
            window: Duration::from_secs(60),
        }
    }
}
