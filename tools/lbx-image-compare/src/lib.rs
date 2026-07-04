// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lbx-image-compare` — perceptual golden-image comparison.
//!
//! Owned by **E01**; implemented in E01 Phase 6 (T22): CIEDE2000 (through Lab
//! from sRGB) + PSNR comparison, failure report with diff heatmap, and the
//! `LIGHTBOX_BLESS=1` golden-regeneration workflow. Golden layout:
//! `crates/lightbox-render/goldens/<node>/<pv>/<case>.png` (spec §4.4
//! tolerances: max ΔE2000 ≤ 1.0 ∧ PSNR ≥ 45 dB). Every pixel epic after E01
//! (E02, E05, E09, E10, …) extends this harness.
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
