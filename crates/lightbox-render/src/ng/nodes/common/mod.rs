// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Shared node infra (spec §4.4 `nodes/common/{pyramid,guided,lut,colorspace}.rs`)
//! E10-built, reused by E11 per the epic's seam table (§9).
//!
//! **M1-provisional scope:** [`guided`] (Phase B's provisional
//! `ToneRecoveryNode`) and [`curve1d`]/[`lut1d`] (Phase C's `ToneCurveNode`,
//! tasks C1-C3) exist so far. [`colorspace`] lands with Phase C's HSL/color-
//! mixer slice (tasks C6-C7): Oklab/OkLCh conversions + the 8-band
//! raised-cosine hue partition, shared by `HslNode`/`VibranceSatNode` (and,
//! later, `BwMixNode`/`ColorGradeNode`). `pyramid` is the fast-local-
//! Laplacian infra Phase B's M2 hardening owns; **not** part of this slice
//! (DEFERRED, B3-B6 lands it only if the M2 bake-off picks candidate 2 over
//! the guided-filter candidate 1 this slice ships). [`lut3d`] (Phase D task
//! D8) is the 3D half of the spec's `lut.rs`: `.cube`/HaldCLUT parsers +
//! tetrahedral sampling, consumed by `nodes::global::creative_lut` (task D9).

pub mod colorspace;
pub mod curve1d;
pub mod guided;
pub mod lut1d;
pub mod lut3d;
