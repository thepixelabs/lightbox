// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-ingest` — import pipeline primitives.
//!
//! Owned by **E01 for the minimal add-in-place path** (spec §1 item 4);
//! implemented in E01 Phase 4 (T17–T18): discovery walk, per-file probe +
//! content hash, batched catalog inserts, import-session bracketing, progress
//! events, cooperative cancellation, undo-import. **E04** owns the full
//! pipeline (copy/move/rename templates, second-copy backup, presets, card
//! detection, watched folders); these primitives are written for its reuse
//! and `ImportOptions` stays `#[non_exhaustive]`.
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
