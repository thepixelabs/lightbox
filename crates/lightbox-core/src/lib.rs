// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-core`, the headless core façade (seam 1).
//!
//! Owned by **E01** (spec §3.8): the [`Core`]/[`Session`] lifecycle, the
//! command bus (every mutation = one WAL transaction), the [`Event`]
//! broadcast, the [`Queries`] façade over WAL-snapshot readers, and the
//! exit-time verified-backup orchestration. **No UI type crosses down; no
//! SQL crosses up** (architecture §2.3 seam 1), `wgpu::Device` (inside
//! `GpuContext`) is the one shared GPU type allowed across the boundary.
//!
//! Headless-testable end-to-end: the CLI and the integration tests drive
//! `create → import → query → mutate → backup → close` with zero shell
//! dependencies (spec §8 DoD 4). The shell consumes exactly this surface.
//!
//! Also here since Phase 1 (T4): the [`observability`] module, tracing
//! initialization (env-filter + optional rotating file log destined for
//! `<catalog>.lbdata/logs/`) and the panic hook. Error taxonomy convention:
//! `thiserror` per crate, `anyhow` only in binaries.

mod check;
mod command;
mod config;
mod raw_source;
// E09 Phase B follow-up (T8): `EditHub`, the edit-state session registry +
// durable command dispatcher (spec §3.4).
mod edit_hub;
mod error;
mod event;
// E10 Phase D (D10): the `installed_look` install/remove pipeline +
// `CatalogLookResolver` (the render engine's `LookResolver` adapter, spec
// §5.2/§4.8).
mod looks;
pub mod observability;
mod previews;
// E03 Phase D (T14): the M0 `BuildRuntime` seam (spec §5.6) backing
// `PreviewService`'s build scheduler on plain tokio.
mod preview_runtime;
// E08 Phase G (G1/G2): the two-scope prefs store (spec §6.7)
// `prefs.toml` machine scope + `catalog_settings` catalog scope.
pub mod prefs;
mod queries;
mod render_source;
mod session;
// E04: the default edit-store location for a library-less app.
mod store_dir;
// E04: the working-set model (spec §4.5), WorkingSetModel + the
// WorkingSetSnapshot/SetPhase/ItemState vocabulary Session::working_set()
// hands out.
mod working_set;

pub use check::{check_catalog, CatalogCheck};
pub use command::{Command, CommandTicket, EditCommand};
pub use config::CoreConfig;
pub use edit_hub::EditHub;
pub use error::{CoreError, Result};
pub use event::{ChangeSet, Event};
pub use looks::{CatalogLookResolver, InstallOutcome};
pub use prefs::{
    CatalogPrefs, GpuMode, JobKnobs, MachinePrefs, PanelColumnPrefs, PanelLayoutPrefs, PrefsStore,
    Theme,
};
pub use queries::Queries;
pub use session::{CloseOpts, ClosePolicy, CloseReport, Core, Session};
pub use store_dir::default_store_dir;
pub use working_set::{ItemState, SetEpoch, SetPhase, WorkingSetItem, WorkingSetSnapshot};

// The E09 edit-state vocabulary (spec §3.4), re-exported so callers (the
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

// E03 Phase D (spec §5.2/§5.6), the preview build-scheduler vocabulary
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
// types, plain data. `CatalogError`/`IntegrityStatus` are re-exported so
// headless callers (the CLI's `check`) can classify open failures without a
// `lightbox-catalog` dependency of their own.
pub use lightbox_catalog::{
    BackupReport, CatalogCounts, CatalogError, EditBadge, FolderNode, ImageDetail, ImageQuery,
    ImageSummary, InstalledLookRow, IntegrityStatus, Page, PageCursor, SortOrder,
};
pub use lightbox_ingest::{ImportOptions, ImportReport};
// E10 Phase D (D10): the installed-look row identity, `Command::InstallLook`/
// `RemoveLook` and `Queries::installed_looks` are typed over it.
pub use lightbox_types::LookId;

// E04 (spec §4.3/§4.6, mandate v2.2 addendum): the working-set loader
// vocabulary + the headless folder-explorer primitive, re-exported so
// callers (the CLI, tests, the eventual E08 shell) get them straight off
// `lightbox-core` without a separate `lightbox-ingest` dependency of their
// own, same convention as the `ImportOptions`/`ImportReport` re-export
// above. `OpenOptions` is additionally the type of `CoreConfig::working_set`.
pub use lightbox_ingest::{
    browse_dir, DirListing, ImageEntry, OpenOptions, OpenOrigin, OpenReport, OpenRequest,
    SkipReason, SkippedPath,
};

pub use raw_source::{RawFallbackReason, RawSourceProvider, RawSourceStatus};
