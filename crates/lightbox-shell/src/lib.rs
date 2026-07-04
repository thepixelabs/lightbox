// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-shell` — the thin, replaceable egui/eframe UI shell.
//!
//! Owned by **E01 for the skeleton** (spec §1 item 9); implemented in E01
//! Phases 2 and 7 (T5, T7, T25–T26): eframe app sharing one `wgpu::Device`
//! with the Engine (seam 2), virtualized grid MVP, loupe compositing the
//! Engine's output texture zero-copy, minimal navigation. **E08** owns the
//! real Library/Develop UX (culling grammar, keymap, filmstrip,
//! compare/survey, panels).
//!
//! No UI type crosses down into the core; `wgpu::Device` is the one shared
//! GPU type allowed across the boundary (architecture §2.3).
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
