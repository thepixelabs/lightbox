// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Engine-owned scaffold nodes (infrastructure, **not** develop tools).
//!
//! Owner: **A-gpu** fills the bodies. `src.decoded` (source injection),
//! `util.resize` (decimation/Lanczos for scale ladders), `xform.display`
//! (working→display, color math supplied by `lightbox-color`, the engine
//! holds zero color science). Test-only probe nodes (`test.gain`,
//! `test.blur_r`, `test.accum`, `test.checker`) live in
//! `lightbox-render-testkit`, not here.

// E10 Phase B (M1 slice): shared node infra (box/guided filter) develop nodes
// build on, spec §4.4 `nodes/common/*`.
pub mod common;
pub mod decoded;
pub mod display;
// E10 Phase A (task A5): the global develop toolset's node library
// (`global.exposure`/`global.contrast`/`global.whites_blacks`, spec §4.4).
// Unlike the engine-owned nodes above, these are develop *content*, spliced
// into the PV1 chain by `RecipeCompiler::compile` via
// `nodes::global::{build_wb_segment, build_tone_color_segment}`.
pub mod global;
// E11 Phase-E (tasks E1-E4): the geometry develop toolset's node library
// (`geom.warp`/`geom.crop`, spec §4.4). Spliced into the PV1 chain by
// `RecipeCompiler::compile` via `nodes::geometry::build_geometry_segment`,
// right after E10's tone/color segment (spec §4.1/§5 placement).
pub mod geometry;
pub mod resize;
pub mod support;
