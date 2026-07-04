// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-core` — the headless core façade (seam 1).
//!
//! Owned by **E01** (spec §3.8); the `Core`/`Session` lifecycle, command bus
//! (every mutation = one WAL transaction), event broadcast, and query façade
//! are implemented in E01 Phase 4 (T15–T16). **No UI type crosses down; no
//! SQL crosses up** (architecture §2.3 seam 1) — `wgpu::Device` is the one
//! shared GPU type allowed across the boundary.
//!
//! What already lives here (E01 Phase 1, T4): the [`observability`] module —
//! tracing initialization (env-filter + optional rotating file log destined
//! for `<catalog>.lbdata/logs/`) and the panic hook. Error taxonomy
//! convention: `thiserror` per crate, `anyhow` only in binaries.
//!
//! **Status: partial skeleton** — Phase 4 adds the façade itself.

pub mod observability;
