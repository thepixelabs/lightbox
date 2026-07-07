// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Engine-owned scaffold nodes (infrastructure, **not** develop tools).
//!
//! Owner: **A-gpu** fills the bodies. `src.decoded` (source injection),
//! `util.resize` (decimation/Lanczos for scale ladders), `xform.display`
//! (working→display, color math supplied by `lightbox-color` — the engine
//! holds zero color science). Test-only probe nodes (`test.gain`,
//! `test.blur_r`, `test.accum`, `test.checker`) live in
//! `lightbox-render-testkit`, not here.

pub mod decoded;
pub mod display;
pub mod resize;
pub mod support;
