// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `WarpField` + constrain-crop (spec §3.2 `src/warp/{field.rs, stages.rs,
//! inscribe.rs}`), the E11 Phase-A geometry seam and Phase-E task **E9**
//! helper. See `field`'s and `inscribe`'s module docs for the scope reduction
//! this engine slice makes (straighten-angle rotation only; see
//! `docs/plan/epics/E11-deviations.md`).

pub mod field;
pub mod inscribe;

pub use field::{Vec2, WarpField, WarpGpuUniform};
pub use inscribe::{largest_inscribed_rect, RectF64};
