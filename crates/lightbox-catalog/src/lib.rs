// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-catalog` — the crash-proof SQLite catalog (single source of truth).
//!
//! Owned by **E01** (spec §3.2, §4); implemented in E01 Phase 3 (T8–T13):
//! open/create + WAL, dedicated writer thread with `with_txn`, WAL-snapshot
//! reader pool, forward-only embedded migrations, spine DAOs, FTS5, integrity
//! checks, exit-time verified backup, and the `kill -9` fault-injection gate.
//! E07/E09/E12/E14 add their own tables via the migration registry
//! (`docs/plan/migrations.md`, committed in Phase 3).
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
