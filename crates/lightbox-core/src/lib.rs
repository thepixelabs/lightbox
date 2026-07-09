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
// E09 Phase B follow-up (T8): `EditHub` — the edit-state session registry +
// durable command dispatcher (spec §3.4).
mod edit_hub;
mod error;
mod event;
pub mod observability;
mod previews;
// E03 Phase D (T14): the M0 `BuildRuntime` seam (spec §5.6) backing
// `PreviewService`'s build scheduler on plain tokio.
mod preview_runtime;
mod queries;
mod render_source;
mod session;

pub use check::{check_catalog, CatalogCheck};
pub use command::{Command, CommandTicket, EditCommand};
pub use config::CoreConfig;
pub use edit_hub::EditHub;
pub use error::{CoreError, Result};
pub use event::{ChangeSet, Event};
pub use queries::Queries;
pub use session::{CloseOpts, ClosePolicy, CloseReport, Core, Session};

// The E09 edit-state vocabulary (spec §3.4) — re-exported so callers (the
// CLI, tests, the eventual shell) get `EditState`/`HistoryStepMeta`/
// `SnapshotMeta`/preset types/`Recipe` straight off `lightbox-core` without
// a separate `lightbox-edit` dependency for read-path types. `Recipe` itself
// and the param vocabulary stay owned by `lightbox-edit` (frozen surface,
// spec §2); mutation still only ever happens through `Command::Edit`/
// `EditHub`.
pub use lightbox_edit::{
    EditState, HistoryStepMeta, ParamDelta, ParamGroup, ParamId, ParamSubset, ParamValue, PresetId,
    PresetMeta, Recipe, RecipeRead, SnapshotMeta, StepLabel,
};
pub use lightbox_meta::xmp::sync::DivergenceStatus;

// E03 Phase D (spec §5.2/§5.6) — the preview build-scheduler vocabulary
// callers (the CLI, tests, the eventual E08 shell) need to drive
// `Session::preview_service`/`Command::BuildPreviews`/`Queries::cache_stats`
// without a separate `lightbox-preview` dependency of their own. `PreviewDesc`
// (in `event.rs`'s `Event::PreviewReady`) and `PreviewError` travel the same
// way. `Tier`/`BuildPriority` are the vocabulary `Command::BuildPreviews`
// itself is typed over.
pub use lightbox_preview::{
    BuildPriority, CacheKind, CacheLimits, CacheStats, EnqueueError, PreviewDesc, PreviewError,
    PreviewRequest, PreviewService, ProgressSink, PurgeReport, PurgeScope, QuickVerifyReport,
    RelocateError, RelocateProgress, Tier, TierSet, VerifyMode, VerifyReport,
};

// Reader DTOs (spec §3.8: "the reader types simply re-exported") and the
// report/option types shared with the import pipeline. No SQL, no rusqlite
// types — plain data. `CatalogError`/`IntegrityStatus` are re-exported so
// headless callers (the CLI's `check`) can classify open failures without a
// `lightbox-catalog` dependency of their own.
pub use lightbox_catalog::{
    BackupReport, CatalogCounts, CatalogError, EditBadge, FolderNode, ImageDetail, ImageQuery,
    ImageSummary, IntegrityStatus, Page, PageCursor, SortOrder,
};
pub use lightbox_ingest::{ImportOptions, ImportReport};
