// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-preview` — demand-driven preview provision behind `PreviewProvider`.
//!
//! Owned by **E01 as a seed** (spec §3.6); implemented in E01 Phase 5 (T21):
//! the frozen `PreviewProvider` trait plus `EmbeddedPreviewProvider`, a
//! cancellable, request-deduping, byte-capped in-memory LRU over the camera's
//! embedded JPEG. **E03** owns the real tiered on-disk pyramid (T0/T1/T2)
//! behind the same trait — this crate is explicitly a seam, not a competing
//! implementation.
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
