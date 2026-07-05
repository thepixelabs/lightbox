// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-core` — the headless core façade (seam 1).
//!
//! Owned by **E01** (spec §3.8): the [`Core`]/[`Session`] lifecycle, the
//! command bus (every mutation = one WAL transaction), the [`Event`]
//! broadcast, the [`Queries`] façade over WAL-snapshot readers, and the
//! exit-time verified-backup orchestration. **No UI type crosses down; no
//! SQL crosses up** (architecture §2.3 seam 1) — `wgpu::Device` (inside
//! `GpuContext`) is the one shared GPU type allowed across the boundary.
//!
//! Headless-testable end-to-end: the CLI and the integration tests drive
//! `create → import → query → mutate → backup → close` with zero shell
//! dependencies (spec §8 DoD 4). The shell consumes exactly this surface.
//!
//! Also here since Phase 1 (T4): the [`observability`] module — tracing
//! initialization (env-filter + optional rotating file log destined for
//! `<catalog>.lbdata/logs/`) and the panic hook. Error taxonomy convention:
//! `thiserror` per crate, `anyhow` only in binaries.

mod check;
mod command;
mod config;
mod error;
mod event;
pub mod observability;
mod previews;
mod queries;
mod render_source;
mod session;

pub use check::{check_catalog, CatalogCheck};
pub use command::{Command, CommandTicket};
pub use config::CoreConfig;
pub use error::{CoreError, Result};
pub use event::{ChangeSet, Event};
pub use queries::Queries;
pub use session::{CloseOpts, ClosePolicy, CloseReport, Core, Session};

// Reader DTOs (spec §3.8: "the reader types simply re-exported") and the
// report/option types shared with the import pipeline. No SQL, no rusqlite
// types — plain data. `CatalogError`/`IntegrityStatus` are re-exported so
// headless callers (the CLI's `check`) can classify open failures without a
// `lightbox-catalog` dependency of their own.
pub use lightbox_catalog::{
    BackupReport, CatalogCounts, CatalogError, FolderNode, ImageDetail, ImageQuery, ImageSummary,
    IntegrityStatus, Page, PageCursor, SortOrder,
};
pub use lightbox_ingest::{ImportOptions, ImportReport};
