// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Slider-latency + soak scenario harness (spec §6; tasks **F1** / **C10**).
//!
//! Owner: **F** (F1 p95 slider-to-screen at fit-view) and **C** (C10 10k-iteration
//! interactive soak). Drives the engine end-to-end through the scheduler; the
//! p95<100 ms gate of record is nightly on the reference GPU runner (§8).

/// A scripted slider-churn scenario (spec F1).
#[derive(Clone, Debug)]
pub struct SliderScenario {
    /// Corpus source name to edit.
    pub source: String,
    /// Number of rapid param events to fire.
    pub events: u32,
    /// Fit-view viewport width, pixels.
    pub viewport_w: u32,
    /// Fit-view viewport height, pixels.
    pub viewport_h: u32,
}

/// End-to-end slider-to-screen latency percentiles (spec F1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyReport {
    /// Median latency, ms.
    pub p50_ms: f64,
    /// 95th-percentile latency, ms (the <100 ms gate).
    pub p95_ms: f64,
    /// Sample count.
    pub samples: usize,
}

/// Measure slider-to-screen p95 for `scenario` (spec F1). **F1 wires it.**
pub fn run_slider_latency(scenario: &SliderScenario) -> LatencyReport {
    let _ = scenario;
    unimplemented!("F1 (F): slider-latency scenario harness (p95 slider-to-screen)")
}

/// A randomized param/zoom/pan soak configuration (spec C10).
#[derive(Clone, Copy, Debug)]
pub struct SoakConfig {
    /// Iteration count (10k for the nightly gate; reduced locally).
    pub iterations: u64,
    /// RNG seed (soak is seeded-deterministic).
    pub seed: u64,
}

/// The result of a soak run (spec C10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoakReport {
    /// Iterations actually run.
    pub iterations: u64,
    /// wgpu validation errors observed (must be 0).
    pub validation_errors: u64,
    /// Whether the final frame equals a fresh render.
    pub final_matches_fresh: bool,
}

/// Run the interactive soak (spec C10). **C10 wires it.**
pub fn run_soak(cfg: &SoakConfig) -> SoakReport {
    let _ = cfg;
    unimplemented!("C10 (C): interactive soak harness")
}
