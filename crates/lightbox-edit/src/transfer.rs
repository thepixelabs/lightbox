// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Settings transfer (spec §3.8 / Phase E — T25, T27): the copy/paste buffer and
//! the batched sync-to-many engine.
//!
//! # The Phase-B seam ([`EditSink`], E09-deviations E-4)
//!
//! The spec signature is `sync_to(store: &EditStore, …)`, but `EditStore` is a
//! **Phase B** type (`lightbox_edit::store`) that is not present in this worktree.
//! So the engine here is written against a minimal [`EditSink`] trait — the two
//! operations sync/apply need from the durable store:
//!
//! - [`EditSink::recipe_of`] — the target's current durable recipe;
//! - [`EditSink::commit_batch`] — commit ≤ `chunk_size` steps as **one WAL txn**
//!   (spec §4.1-2), one `history_step` per target.
//!
//! Phase B's `EditStore` implements `EditSink` (or the `Command::Edit` dispatcher
//! adapts to it in a few lines). The batching (~64/txn), one-step-per-target,
//! progress events, `CancelToken`-between-chunks, and "Previous" semantics — the
//! actual Phase-E logic — are all real and fully unit-tested here against an
//! in-memory fake sink. The concrete `EditStore` wiring, the 500-target-against-
//! SQLite criterion number, and the CLI `sync`/`paste`/`previous` subcommands are
//! **DEFERRED to Phase B** (E09-deviations E-5).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lightbox_types::{ImageId, ProcessVersion};

use crate::history::StepLabel;
use crate::params::{ParamDelta, ParamSubset};
use crate::recipe::Recipe;

/// The copy/paste buffer (spec §3.8): a subset of settings extracted from a
/// source recipe, ready to paste/sync onto other images. Core-held in Phase B
/// (`EditHub`); the type and its pure operations live here.
#[derive(Clone, Debug, PartialEq)]
pub struct CopiedSettings {
    /// Which groups were copied.
    pub subset: ParamSubset,
    /// The copied partial values (only the subset's params).
    pub delta: ParamDelta,
    /// The source's process version (for a future cross-PV guard; PV1-only at M1).
    pub source_pv: ProcessVersion,
}

impl CopiedSettings {
    /// Copy `subset` of settings out of `source` (spec §3.8).
    pub fn copy_from(source: &Recipe, subset: &ParamSubset) -> CopiedSettings {
        CopiedSettings {
            subset: subset.clone(),
            delta: source.extract(subset),
            source_pv: source.pv,
        }
    }

    /// **All** of a source's (non-neutral) settings — the "Previous" buffer
    /// (spec §3.8: "Previous semantics"). Every group `source` moved from neutral
    /// is carried, so `ApplyPrevious` re-applies the last-committed source's full
    /// develop settings.
    pub fn all_of(source: &Recipe) -> CopiedSettings {
        let neutral = Recipe::identity(source.pv);
        let delta = source.diff(&neutral);
        let groups = delta
            .0
            .keys()
            .copied()
            .map(crate::params::group_of)
            .collect();
        CopiedSettings {
            subset: ParamSubset { groups },
            delta,
            source_pv: source.pv,
        }
    }

    /// Paste these settings onto `base` (pure; spec §3.8). The result differs from
    /// `base` only within the copied subset.
    pub fn paste_onto(&self, base: &Recipe) -> Recipe {
        let mut r = base.clone();
        let _ = r.apply(&self.delta);
        r
    }

    /// True iff nothing was copied.
    pub fn is_empty(&self) -> bool {
        self.delta.is_empty()
    }
}

/// Cooperative cancellation for the batched engine (checked **between** chunks).
/// Mirrors the `lightbox_jobs::CancelToken` contract without pulling the async
/// job runtime into `lightbox-edit` (E09-deviations E-4): when sync runs as an
/// E06 `Class::Foreground` job (Phase B), the job flips a [`CancelFlag`] when its
/// own token cancels, or passes a `|| token.is_cancelled()` closure.
pub trait CancelSignal {
    /// True once cancellation has been requested.
    fn is_cancelled(&self) -> bool;
}

impl<F: Fn() -> bool> CancelSignal for F {
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// A cheap, clone-shareable cancellation flag.
#[derive(Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// A fresh, un-cancelled flag.
    pub fn new() -> CancelFlag {
        CancelFlag(Arc::new(AtomicBool::new(false)))
    }
    /// Request cancellation (observed by all clones).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl CancelSignal for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A never-cancelling signal (for the common non-cancellable call).
pub struct NeverCancel;
impl CancelSignal for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// One durable step the engine hands the sink: apply to `image`, labelled.
#[derive(Clone, Debug)]
pub struct CommitStep {
    /// The target image.
    pub image: ImageId,
    /// The recipe the target should hold after this step.
    pub new_recipe: Recipe,
    /// The history label for the step.
    pub label: StepLabel,
}

/// The durable edit store, as the transfer engine needs it (the Phase-B seam —
/// see the module docs). Every `commit_batch` call is **one WAL txn**.
pub trait EditSink {
    /// The target's current durable recipe (neutral default if untouched).
    fn recipe_of(&self, image: ImageId) -> Result<Recipe, TransferError>;
    /// Commit one `history_step` per entry in `batch`, **atomically** (one txn).
    /// The engine never passes more than `SyncOptions::chunk_size` entries.
    fn commit_batch(&self, batch: &[CommitStep]) -> Result<(), TransferError>;
}

/// Tuning for the batched engine.
#[derive(Clone, Copy, Debug)]
pub struct SyncOptions {
    /// Targets per WAL txn (spec §3.8 "~64/txn").
    pub chunk_size: usize,
}

impl Default for SyncOptions {
    fn default() -> SyncOptions {
        SyncOptions { chunk_size: 64 }
    }
}

/// Progress after each committed chunk (spec §3.8 "per-image progress events").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncProgress {
    /// Targets committed so far.
    pub done: usize,
    /// Total targets requested.
    pub total: usize,
}

/// The outcome of a batched apply/sync (spec §3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Targets whose step was durably committed.
    pub committed: usize,
    /// Total targets requested.
    pub requested: usize,
    /// WAL txns used (chunks committed).
    pub txns: usize,
    /// True iff a cancellation stopped the run before all targets were committed.
    pub cancelled: bool,
}

/// Errors from settings transfer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// The edit-store seam failed (read or commit).
    #[error("edit store: {0}")]
    Store(String),
}

// ── the batched engine ────────────────────────────────────────────────────────

/// Apply a **uniform** delta to many targets as one history step each, batched
/// ~`chunk_size`/txn (spec §3.8 / T27). This backs `SyncSettings`, `ApplyPreset`,
/// `PasteSettings`, and `ApplyPrevious` — they differ only in the [`StepLabel`].
///
/// Cancellation is checked between chunks: a cancel mid-run leaves the already-
/// committed chunks **durable** and the rest untouched, and the store consistent.
pub fn sync_to<S, C>(
    sink: &S,
    delta: &ParamDelta,
    targets: &[ImageId],
    label: StepLabel,
    cancel: &C,
    opts: SyncOptions,
    progress: impl FnMut(SyncProgress),
) -> Result<SyncReport, TransferError>
where
    S: EditSink,
    C: CancelSignal + ?Sized,
{
    run_batched(sink, targets, label, cancel, opts, progress, |base| {
        let mut r = base.clone();
        let _ = r.apply(delta);
        r
    })
}

/// Reset many targets to their neutral default as one history step each
/// (spec §3.4 `ResetEdits` / T25). The per-image transform is neutral, so — unlike
/// [`sync_to`] — the target's own pv is preserved.
pub fn reset_edits<S, C>(
    sink: &S,
    targets: &[ImageId],
    cancel: &C,
    opts: SyncOptions,
    progress: impl FnMut(SyncProgress),
) -> Result<SyncReport, TransferError>
where
    S: EditSink,
    C: CancelSignal + ?Sized,
{
    run_batched(
        sink,
        targets,
        StepLabel::Reset,
        cancel,
        opts,
        progress,
        |base| Recipe::identity(base.pv),
    )
}

/// Paste copied settings onto many targets (spec §3.4 `PasteSettings` / T25).
pub fn paste_settings<S, C>(
    sink: &S,
    copied: &CopiedSettings,
    targets: &[ImageId],
    cancel: &C,
    opts: SyncOptions,
    progress: impl FnMut(SyncProgress),
) -> Result<SyncReport, TransferError>
where
    S: EditSink,
    C: CancelSignal + ?Sized,
{
    sync_to(
        sink,
        &copied.delta,
        targets,
        StepLabel::Paste,
        cancel,
        opts,
        progress,
    )
}

/// Apply the "Previous" (last-committed source) settings onto many targets
/// (spec §3.4 `ApplyPrevious` / T25). `previous` is [`CopiedSettings::all_of`] the
/// last source.
pub fn apply_previous<S, C>(
    sink: &S,
    previous: &CopiedSettings,
    targets: &[ImageId],
    cancel: &C,
    opts: SyncOptions,
    progress: impl FnMut(SyncProgress),
) -> Result<SyncReport, TransferError>
where
    S: EditSink,
    C: CancelSignal + ?Sized,
{
    sync_to(
        sink,
        &previous.delta,
        targets,
        StepLabel::Sync,
        cancel,
        opts,
        progress,
    )
}

/// The shared batched runner: read each target, transform, commit in chunks.
fn run_batched<S, C, T>(
    sink: &S,
    targets: &[ImageId],
    label: StepLabel,
    cancel: &C,
    opts: SyncOptions,
    mut progress: impl FnMut(SyncProgress),
    transform: T,
) -> Result<SyncReport, TransferError>
where
    S: EditSink,
    C: CancelSignal + ?Sized,
    T: Fn(&Recipe) -> Recipe,
{
    let chunk_size = opts.chunk_size.max(1);
    let total = targets.len();
    let mut report = SyncReport {
        requested: total,
        ..SyncReport::default()
    };

    for chunk in targets.chunks(chunk_size) {
        // Cancellation is honored BETWEEN chunks so committed txns stay durable.
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let mut batch: Vec<CommitStep> = Vec::with_capacity(chunk.len());
        for &image in chunk {
            let base = sink.recipe_of(image)?;
            batch.push(CommitStep {
                image,
                new_recipe: transform(&base),
                label: label.clone(),
            });
        }
        sink.commit_batch(&batch)?;
        report.committed += batch.len();
        report.txns += 1;
        progress(SyncProgress {
            done: report.committed,
            total,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ParamGroup, ParamId, ParamValue};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory [`EditSink`] fake (the Phase-B store stand-in): records the
    /// current recipe per image and every committed step.
    #[derive(Default)]
    struct MemEditStore {
        state: Mutex<HashMap<ImageId, Recipe>>,
        steps: Mutex<Vec<(ImageId, StepLabel)>>,
        txns: Mutex<usize>,
    }

    impl EditSink for MemEditStore {
        fn recipe_of(&self, image: ImageId) -> Result<Recipe, TransferError> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .get(&image)
                .cloned()
                .unwrap_or_else(|| Recipe::identity(lightbox_types::PV_M0)))
        }
        fn commit_batch(&self, batch: &[CommitStep]) -> Result<(), TransferError> {
            let mut state = self.state.lock().unwrap();
            let mut steps = self.steps.lock().unwrap();
            for s in batch {
                state.insert(s.image, s.new_recipe.clone());
                steps.push((s.image, s.label.clone()));
            }
            *self.txns.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn exposure_delta(v: f32) -> ParamDelta {
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Exposure, ParamValue::F32(v));
        d
    }

    #[test]
    fn sync_one_step_per_target_batched() {
        let sink = MemEditStore::default();
        let targets: Vec<ImageId> = (0..500).map(ImageId).collect();
        let mut last = SyncProgress { done: 0, total: 0 };
        let report = sync_to(
            &sink,
            &exposure_delta(1.0),
            &targets,
            StepLabel::Sync,
            &NeverCancel,
            SyncOptions::default(),
            |p| last = p,
        )
        .unwrap();

        assert_eq!(report.committed, 500);
        assert_eq!(report.requested, 500);
        assert!(!report.cancelled);
        // 500 individual history steps.
        assert_eq!(sink.steps.lock().unwrap().len(), 500);
        // ~64/txn → ceil(500/64) = 8 txns.
        assert_eq!(report.txns, 8);
        assert_eq!(*sink.txns.lock().unwrap(), 8);
        assert_eq!(
            last,
            SyncProgress {
                done: 500,
                total: 500
            }
        );
        // Every target now carries the delta.
        for t in &targets {
            assert_eq!(
                sink.recipe_of(*t).unwrap().get(ParamId::Exposure),
                ParamValue::F32(1.0)
            );
        }
    }

    #[test]
    fn cancel_midway_keeps_committed_chunks() {
        let sink = MemEditStore::default();
        let targets: Vec<ImageId> = (0..500).map(ImageId).collect();
        let cancel = CancelFlag::new();
        // Cancel after the second committed chunk.
        let report = sync_to(
            &sink,
            &exposure_delta(2.0),
            &targets,
            StepLabel::Sync,
            &cancel,
            SyncOptions { chunk_size: 64 },
            |p| {
                if p.done >= 128 {
                    cancel.cancel();
                }
            },
        )
        .unwrap();

        assert!(report.cancelled);
        // Exactly the two committed chunks survive; the rest are untouched.
        assert_eq!(report.committed, 128);
        assert_eq!(report.txns, 2);
        assert_eq!(sink.steps.lock().unwrap().len(), 128);
        assert_eq!(
            sink.recipe_of(ImageId(0)).unwrap().get(ParamId::Exposure),
            ParamValue::F32(2.0)
        );
        // A target past the cancel point never moved.
        assert!(sink.recipe_of(ImageId(200)).unwrap().is_neutral());
    }

    #[test]
    fn copy_paste_round_trips_subset() {
        let mut source = Recipe::identity(lightbox_types::PV_M0);
        source.apply(&exposure_delta(1.5)).unwrap();
        let copied =
            CopiedSettings::copy_from(&source, &ParamSubset::from_groups([ParamGroup::Tone]));
        let pasted = copied.paste_onto(&Recipe::identity(lightbox_types::PV_M0));
        assert_eq!(pasted.get(ParamId::Exposure), ParamValue::F32(1.5));
    }

    #[test]
    fn reset_sets_targets_neutral() {
        let sink = MemEditStore::default();
        // Pre-edit two images.
        let mut edited = Recipe::identity(lightbox_types::PV_M0);
        edited.apply(&exposure_delta(1.0)).unwrap();
        sink.state
            .lock()
            .unwrap()
            .insert(ImageId(1), edited.clone());
        sink.state.lock().unwrap().insert(ImageId(2), edited);

        let report = reset_edits(
            &sink,
            &[ImageId(1), ImageId(2)],
            &NeverCancel,
            SyncOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.committed, 2);
        assert!(sink.recipe_of(ImageId(1)).unwrap().is_neutral());
        assert_eq!(
            sink.steps.lock().unwrap()[0].1.kind(),
            "reset",
            "reset step is labelled Reset"
        );
    }
}
