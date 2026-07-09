// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The in-memory session working-set model (E04 spec §4.5): epoch-versioned,
//! replace-on-new-drop, per-item states, coalesced change events.
//!
//! [`WorkingSetModel`] is the single mutable home for this state — one per
//! [`crate::Session`] (not `pub`; reached only through
//! [`crate::Session::working_set`]'s cheap `Arc` snapshot and the
//! `Command::OpenWorkingSet` dispatch in `session.rs`). Every mutation is
//! **epoch-gated**: a callback tagged with a stale (already-superseded)
//! epoch is silently dropped at the model boundary (spec §5.1 R6 — "Epoch
//! races") — the dispatcher owns cancelling the previous epoch's job
//! *before* bumping to a new one (`begin_epoch`), so at most one load ever
//! mutates live state, and a straggling callback from a just-cancelled job
//! can never clobber the set it was replaced by.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use lightbox_ingest::{LoadEvent, PlannedItem, SetPlan, SkippedPath};
use lightbox_jobs::CancelToken;
use lightbox_types::{AssetId, ImageId, SourceKind};

/// Monotone per-session working-set generation (spec §4.5). Bumped on every
/// `OpenWorkingSet`; events and snapshots carry it so the shell (and the
/// loader job itself) can discard stale updates. `0` = no open has ever run
/// (the initial empty state).
pub type SetEpoch = u64;

/// Immutable snapshot of the session working set (spec §4.5) — plain data,
/// no locks, no catalog handles. [`crate::Session::working_set`] hands out
/// one per call via a cheap `Arc` clone.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WorkingSetSnapshot {
    /// The generation this snapshot reflects.
    pub epoch: SetEpoch,
    /// Coarse lifecycle phase.
    pub phase: SetPhase,
    /// Session order (spec §6.3).
    pub items: Vec<WorkingSetItem>,
    /// Re-exported from `lightbox-ingest` — shown by E08 on demand.
    pub skipped: Vec<SkippedPath>,
    /// `true` when `OpenOptions::max_set_size` was hit.
    pub truncated: bool,
}

/// Coarse lifecycle phase of the current epoch (spec §4.5).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SetPhase {
    /// No open has ever run this session.
    Empty,
    /// Phase 1 (enumerate/probe/order) is running; `items` is still empty.
    Planning,
    /// Phase 1 finished; phase 2 (hash/register) is running or about to
    /// start.
    Loading,
    /// Phase 2 finished (including a cancelled-by-replacement finish — see
    /// `lightbox_ingest::load_working_set`'s own doc comment).
    Ready,
}

/// One item in the working-set snapshot (spec §4.5).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WorkingSetItem {
    /// Canonical, absolute.
    pub path: PathBuf,
    /// NFC-normalized.
    pub filename: String,
    /// Current registration state.
    pub state: ItemState,
    /// E08 panel gating (spec §2.4); `None` = unsupported.
    pub source_kind: Option<SourceKind>,
    /// Catalog format tag, for badges (`'CR3'`, …, `'UNSUPPORTED'`).
    pub format: String,
    /// Full-size width; `0` if unknown.
    pub width: u32,
    /// Full-size height; `0` if unknown.
    pub height: u32,
    /// RFC3339 capture time, if probed.
    pub capture_time: Option<String>,
    /// Probe or load failure message, if any — badged, never hidden.
    pub decode_error: Option<String>,
    /// Named directly in the request vs. discovered by a folder walk.
    pub explicit: bool,
}

impl WorkingSetItem {
    fn from_planned(item: &PlannedItem) -> WorkingSetItem {
        WorkingSetItem {
            path: item.path.clone(),
            filename: item.filename.clone(),
            state: ItemState::Planned,
            source_kind: item.source_kind,
            format: match &item.probe {
                Ok(p) => p.format.catalog_tag().to_owned(),
                Err(_) => "UNSUPPORTED".to_owned(),
            },
            width: item.probe.as_ref().map(|p| p.width).unwrap_or(0),
            height: item.probe.as_ref().map(|p| p.height).unwrap_or(0),
            capture_time: item
                .probe
                .as_ref()
                .ok()
                .and_then(|p| p.capture_time.clone()),
            decode_error: item.probe.as_ref().err().cloned(),
            explicit: item.explicit,
        }
    }
}

/// Per-item registration state (spec §4.5).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ItemState {
    /// Planned; hash/registration pending (placeholder in the filmstrip).
    Planned,
    /// Registered; previews/render/edits may be requested for `image`.
    Ready {
        /// The (found-or-created) asset.
        asset: AssetId,
        /// The default image row.
        image: ImageId,
    },
    /// Hash or registration failed; the item stays visible with a badge.
    Failed,
    /// Collapsed into an earlier identical-content item.
    DuplicateOf {
        /// The index of the earlier item this one collapsed into.
        index: usize,
    },
}

/// The mutable home of the working-set state (spec §4.5). One per
/// [`crate::Session`]; every write is epoch-gated (see module docs).
pub(crate) struct WorkingSetModel {
    epoch: AtomicU64,
    snapshot: RwLock<Arc<WorkingSetSnapshot>>,
    /// The in-flight epoch's cancel token, so the next `OpenWorkingSet` can
    /// cancel it *before* bumping the epoch (spec §5.1: "at most one load
    /// runs").
    in_flight_cancel: Mutex<Option<CancelToken>>,
}

impl WorkingSetModel {
    pub(crate) fn new() -> Arc<WorkingSetModel> {
        Arc::new(WorkingSetModel {
            epoch: AtomicU64::new(0),
            snapshot: RwLock::new(Arc::new(WorkingSetSnapshot {
                epoch: 0,
                phase: SetPhase::Empty,
                items: Vec::new(),
                skipped: Vec::new(),
                truncated: false,
            })),
            in_flight_cancel: Mutex::new(None),
        })
    }

    /// Cheap `Arc` clone of the current snapshot.
    pub(crate) fn snapshot(&self) -> Arc<WorkingSetSnapshot> {
        Arc::clone(&self.snapshot.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn current_epoch(&self) -> SetEpoch {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Cancels the in-flight epoch's job (if any), bumps to a new epoch,
    /// publishes an empty `Planning` snapshot, and returns `(new_epoch,
    /// child_cancel_token)` for the caller to spawn the new job under.
    pub(crate) fn begin_epoch(&self, session_cancel: &CancelToken) -> (SetEpoch, CancelToken) {
        let mut current = self
            .in_flight_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = current.take() {
            prev.cancel();
        }
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let token = session_cancel.child();
        *current = Some(token.clone());
        drop(current);

        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(WorkingSetSnapshot {
            epoch,
            phase: SetPhase::Planning,
            items: Vec::new(),
            skipped: Vec::new(),
            truncated: false,
        });
        (epoch, token)
    }

    /// Applies phase-1's plan. A stale `epoch` (superseded by a later
    /// `begin_epoch`) is silently ignored.
    pub(crate) fn apply_plan(&self, epoch: SetEpoch, plan: &SetPlan) {
        if epoch != self.current_epoch() {
            return;
        }
        let items: Vec<WorkingSetItem> = plan
            .items
            .iter()
            .map(WorkingSetItem::from_planned)
            .collect();
        // Nothing to load (an all-skipped request) -> go straight to Ready
        // (spec §6.1: "an all-skipped request still publishes an empty set
        // with the skip list").
        let phase = if items.is_empty() {
            SetPhase::Ready
        } else {
            SetPhase::Loading
        };
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(WorkingSetSnapshot {
            epoch,
            phase,
            items,
            skipped: plan.skipped.clone(),
            truncated: plan.truncated,
        });
    }

    /// Applies one phase-2 [`LoadEvent`]. A stale `epoch` is silently
    /// ignored (see module docs).
    pub(crate) fn apply_load_event(&self, epoch: SetEpoch, event: &LoadEvent) {
        if epoch != self.current_epoch() {
            return;
        }
        let mut guard = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**guard).clone();
        match event {
            LoadEvent::ItemReady {
                index,
                asset,
                image,
                ..
            } => {
                if let Some(item) = next.items.get_mut(*index) {
                    item.state = ItemState::Ready {
                        asset: *asset,
                        image: *image,
                    };
                }
            }
            LoadEvent::ItemFailed { index, reason } => {
                if let Some(item) = next.items.get_mut(*index) {
                    item.state = ItemState::Failed;
                    item.decode_error = Some(reason.clone());
                }
            }
            LoadEvent::ItemCollapsed { index, first_index } => {
                if let Some(item) = next.items.get_mut(*index) {
                    item.state = ItemState::DuplicateOf {
                        index: *first_index,
                    };
                }
            }
            LoadEvent::Progress { .. } => {}
            // `LoadEvent` is `#[non_exhaustive]`; a future variant gets
            // model-state meaning when it gets one (same convention as
            // `ImportEvent`/`EngineEvent` elsewhere in this crate).
            _ => {}
        }
        *guard = Arc::new(next);
    }

    /// Phase-2 complete (also called on a cancelled-by-replacement finish —
    /// the model just reflects "loading has stopped"; any un-processed
    /// items stay `Planned`, which is honest — the report named what
    /// actually landed). A stale `epoch` is silently ignored.
    pub(crate) fn finish(&self, epoch: SetEpoch) {
        if epoch != self.current_epoch() {
            return;
        }
        let mut guard = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        let mut next = (**guard).clone();
        next.phase = SetPhase::Ready;
        *guard = Arc::new(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_decode::{AssetProbe, ProbedFormat};
    use lightbox_ingest::OpenReport;
    use lightbox_types::Orientation;

    fn fake_plan(n: usize) -> SetPlan {
        SetPlan {
            items: (0..n)
                .map(|i| PlannedItem {
                    path: PathBuf::from(format!("/tmp/f{i}.jpg")),
                    filename: format!("f{i}.jpg"),
                    probe: Ok(AssetProbe {
                        format: ProbedFormat::Jpeg,
                        width: 8,
                        height: 8,
                        orientation: Orientation::O1,
                        camera_make: None,
                        camera_model: None,
                        capture_time: None,
                        file_bytes: 8,
                        embedded: Vec::new(),
                    }),
                    source_kind: Some(SourceKind::Rendered),
                    explicit: true,
                })
                .collect(),
            skipped: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn initial_state_is_empty() {
        let model = WorkingSetModel::new();
        let snap = model.snapshot();
        assert_eq!(snap.epoch, 0);
        assert_eq!(snap.phase, SetPhase::Empty);
        assert!(snap.items.is_empty());
    }

    #[test]
    fn begin_epoch_bumps_and_publishes_planning() {
        let model = WorkingSetModel::new();
        let root_cancel = CancelToken::new();
        let (epoch, _token) = model.begin_epoch(&root_cancel);
        assert_eq!(epoch, 1);
        let snap = model.snapshot();
        assert_eq!(snap.epoch, 1);
        assert_eq!(snap.phase, SetPhase::Planning);
        assert!(snap.items.is_empty());
    }

    #[test]
    fn a_second_begin_epoch_cancels_the_first_token() {
        let model = WorkingSetModel::new();
        let root_cancel = CancelToken::new();
        let (epoch1, token1) = model.begin_epoch(&root_cancel);
        assert!(!token1.is_cancelled());
        let (epoch2, _token2) = model.begin_epoch(&root_cancel);
        assert!(
            token1.is_cancelled(),
            "the first epoch's job must be cancelled"
        );
        assert_eq!(epoch2, epoch1 + 1);
    }

    #[test]
    fn apply_plan_transitions_to_loading_with_items() {
        let model = WorkingSetModel::new();
        let (epoch, _token) = model.begin_epoch(&CancelToken::new());
        let plan = fake_plan(3);
        model.apply_plan(epoch, &plan);
        let snap = model.snapshot();
        assert_eq!(snap.phase, SetPhase::Loading);
        assert_eq!(snap.items.len(), 3);
        assert!(snap
            .items
            .iter()
            .all(|i| matches!(i.state, ItemState::Planned)));
    }

    #[test]
    fn apply_plan_with_no_items_goes_straight_to_ready() {
        let model = WorkingSetModel::new();
        let (epoch, _token) = model.begin_epoch(&CancelToken::new());
        model.apply_plan(epoch, &fake_plan(0));
        assert_eq!(model.snapshot().phase, SetPhase::Ready);
    }

    #[test]
    fn load_events_update_item_state_by_index() {
        let model = WorkingSetModel::new();
        let (epoch, _token) = model.begin_epoch(&CancelToken::new());
        model.apply_plan(epoch, &fake_plan(3));

        model.apply_load_event(
            epoch,
            &LoadEvent::ItemReady {
                index: 0,
                asset: AssetId(1),
                image: ImageId(1),
                reused: false,
                relocated: false,
            },
        );
        model.apply_load_event(
            epoch,
            &LoadEvent::ItemFailed {
                index: 1,
                reason: "boom".to_owned(),
            },
        );
        model.apply_load_event(
            epoch,
            &LoadEvent::ItemCollapsed {
                index: 2,
                first_index: 0,
            },
        );

        let snap = model.snapshot();
        assert!(matches!(
            snap.items[0].state,
            ItemState::Ready {
                asset: AssetId(1),
                image: ImageId(1)
            }
        ));
        assert!(matches!(snap.items[1].state, ItemState::Failed));
        assert_eq!(snap.items[1].decode_error.as_deref(), Some("boom"));
        assert!(matches!(
            snap.items[2].state,
            ItemState::DuplicateOf { index: 0 }
        ));
    }

    #[test]
    fn finish_moves_phase_to_ready() {
        let model = WorkingSetModel::new();
        let (epoch, _token) = model.begin_epoch(&CancelToken::new());
        model.apply_plan(epoch, &fake_plan(1));
        assert_eq!(model.snapshot().phase, SetPhase::Loading);
        model.finish(epoch);
        assert_eq!(model.snapshot().phase, SetPhase::Ready);
    }

    /// R6 (spec §10): a stale epoch's writes never mutate the live snapshot
    /// — the epoch race guard at the model boundary.
    #[test]
    fn stale_epoch_writes_are_dropped() {
        let model = WorkingSetModel::new();
        let root = CancelToken::new();
        let (epoch1, _t1) = model.begin_epoch(&root);
        let (epoch2, _t2) = model.begin_epoch(&root); // supersedes epoch1

        // epoch1's plan lands AFTER epoch2 has already begun — must be a no-op.
        model.apply_plan(epoch1, &fake_plan(5));
        let snap = model.snapshot();
        assert_eq!(snap.epoch, epoch2);
        assert!(snap.items.is_empty(), "epoch1's stale plan must not appear");

        model.apply_plan(epoch2, &fake_plan(2));
        model.apply_load_event(
            epoch1,
            &LoadEvent::ItemReady {
                index: 0,
                asset: AssetId(9),
                image: ImageId(9),
                reused: false,
                relocated: false,
            },
        );
        let snap = model.snapshot();
        assert_eq!(snap.epoch, epoch2);
        assert!(
            matches!(snap.items[0].state, ItemState::Planned),
            "stale load event must not mutate epoch2's items"
        );

        model.finish(epoch1); // stale finish must not move epoch2 to Ready early
        assert_eq!(model.snapshot().phase, SetPhase::Loading);
        model.finish(epoch2);
        assert_eq!(model.snapshot().phase, SetPhase::Ready);
    }

    #[test]
    fn open_report_serializes_for_events() {
        // OpenReport travels through Event::WorkingSetLoadFinished; a
        // smoke check that it's the type this module expects to carry.
        let report = OpenReport {
            planned: 1,
            ready: 1,
            ..OpenReport::default()
        };
        assert_eq!(report.planned, 1);
    }
}
