// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Render nodes shipped by the engine seed.
//!
//! Phase 2: [`solid_color`] — the tracer/test node proving the trait, the
//! ticket lifecycle and the shared-device composite (spec T6/T7).
//! Phase 6 (T23/T24) adds `display.transform`, the one real M0 node.

pub mod solid_color;
