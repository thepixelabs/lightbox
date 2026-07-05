// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Render nodes shipped by the engine seed.
//!
//! Phase 2: [`solid_color`] — the tracer/test node proving the trait, the
//! ticket lifecycle and the shared-device composite (spec T6/T7).
//! Phase 6 (T23/T24): [`display_transform`] — the one real M0 node (sRGB
//! decode → orientation → bilinear resample in linear light → sRGB encode),
//! WGSL + CPU from one written algorithm spec (`display_transform.md`),
//! golden/parity tested.

pub mod display_transform;
pub mod solid_color;
