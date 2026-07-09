// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Job groups (E06 spec §4.5, T8): one activity entry aggregating many
//! member jobs ("Building previews — 412/3,041").
//!
//! A group **completes** when [`GroupHandle::close`] has been called *and*
//! every member is terminal — close means "no more members will be added",
//! so a streaming import can keep adding jobs while earlier ones finish.
//! [`crate::Scheduler::cancel_group`] fans out to all non-terminal members
//! (member cancel tokens are children of the group's token, so sub-stage
//! tokens derived from a member observe it too).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use crate::model::{Class, GroupId, Outcome};
use crate::progress::ProgressView;
use crate::sched::{lock, JobRecord, Scheduler};
use crate::CancelToken;

/// What to create (spec §4.5 `GroupSpec`). Build via [`GroupSpec::new`] and
/// override fields.
#[derive(Clone, Debug)]
pub struct GroupSpec {
    /// Machine-readable kind, namespaced (`"preview.extract"`).
    pub kind: &'static str,
    /// Human-readable label for the activity center.
    pub label: String,
    /// The class its members are expected to run as (activity display; each
    /// member still carries its own class).
    pub class: Class,
    /// Whether pause/resume applies to the group's members via the group
    /// surface. Defaults to `class == Background`.
    pub pausable: bool,
    /// A-priori member count when known up front (the aggregate total shows
    /// `max(expected_total, members added so far)`).
    pub expected_total: Option<u64>,
}

impl GroupSpec {
    /// A spec with the defaults (`pausable = (class == Background)`, no
    /// expected total).
    pub fn new(kind: &'static str, label: impl Into<String>, class: Class) -> GroupSpec {
        GroupSpec {
            kind,
            label: label.into(),
            class,
            pausable: class == Class::Background,
            expected_total: None,
        }
    }
}

/// Registry-held state of one group.
pub(crate) struct GroupRecord {
    pub(crate) id: GroupId,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) class: Class,
    pub(crate) pausable: bool,
    pub(crate) expected_total: Option<u64>,
    /// Members' cancel tokens are children of this one.
    pub(crate) cancel: CancelToken,
    /// "No more members will be added."
    pub(crate) closed: AtomicBool,
    /// `cancel_group` was called.
    pub(crate) cancelled: AtomicBool,
    /// Every member ever added (terminal members stay for aggregation).
    pub(crate) members: Mutex<Vec<Arc<JobRecord>>>,
    pub(crate) terminal_members: AtomicU64,
    pub(crate) failed_members: AtomicU64,
    pub(crate) cancelled_members: AtomicU64,
    /// One-shot completion latch (`GroupFinished` emitted).
    pub(crate) finished: AtomicBool,
    pub(crate) outcome: Mutex<Option<Outcome>>,
    pub(crate) started_at: SystemTime,
    pub(crate) terminal_at: Mutex<Option<tokio::time::Instant>>,
    pub(crate) dismissed: AtomicBool,
}

impl GroupRecord {
    pub(crate) fn new(id: GroupId, spec: GroupSpec) -> Arc<GroupRecord> {
        Arc::new(GroupRecord {
            id,
            kind: spec.kind,
            label: spec.label,
            class: spec.class,
            pausable: spec.pausable,
            expected_total: spec.expected_total,
            cancel: CancelToken::new(),
            closed: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            members: Mutex::new(Vec::new()),
            terminal_members: AtomicU64::new(0),
            failed_members: AtomicU64::new(0),
            cancelled_members: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            outcome: Mutex::new(None),
            started_at: SystemTime::now(),
            terminal_at: Mutex::new(None),
            dismissed: AtomicBool::new(false),
        })
    }

    /// The aggregate progress (T8 AC: `(done, total)` across members, with
    /// members addable while running): `done` = terminal members, `total` =
    /// `max(members added, expected_total)` — a growing total until the
    /// group closes.
    pub(crate) fn aggregate_view(&self) -> ProgressView {
        let added = lock(&self.members).len() as u64;
        let total = added.max(self.expected_total.unwrap_or(0));
        let done = self.terminal_members.load(Ordering::Acquire).min(total);
        let fraction = if total > 0 {
            #[allow(clippy::cast_precision_loss)]
            Some(((done as f64 / total as f64) as f32).clamp(0.0, 1.0))
        } else {
            None
        };
        ProgressView {
            fraction,
            items: (total > 0).then_some((done, total)),
            bytes: None,
        }
    }

    /// Change detector for coalesced `JobEvent::Progress` emission: a value
    /// that changes whenever the aggregate view can have changed (members
    /// added, members finished).
    pub(crate) fn progress_epoch(&self) -> u64 {
        let added = lock(&self.members).len() as u64;
        added.wrapping_add(
            self.terminal_members
                .load(Ordering::Acquire)
                .wrapping_mul(0x9E37_79B9),
        )
    }

    /// The group's aggregate outcome once complete.
    pub(crate) fn final_outcome(&self) -> Outcome {
        let total = lock(&self.members).len() as u64;
        if self.failed_members.load(Ordering::Acquire) > 0 {
            Outcome::Failed
        } else if self.cancelled.load(Ordering::Acquire)
            || (total > 0 && self.cancelled_members.load(Ordering::Acquire) == total)
        {
            Outcome::Cancelled
        } else {
            Outcome::Completed
        }
    }
}

/// Handle to one group (spec §4.5). Clone-cheap. **Dropping detaches** (the
/// group keeps aggregating; remember to [`GroupHandle::close`] it or it
/// never completes — the activity entry stays live).
#[derive(Clone)]
pub struct GroupHandle {
    pub(crate) record: Arc<GroupRecord>,
    pub(crate) sched: Weak<Scheduler>,
}

impl GroupHandle {
    /// The group's id (pass as `JobSpec::group` on member spawns).
    pub fn id(&self) -> GroupId {
        self.record.id
    }

    /// Declares membership complete: the group finishes once every member
    /// is terminal. Idempotent.
    pub fn close(&self) {
        self.record.closed.store(true, Ordering::Release);
        if let Some(s) = self.sched.upgrade() {
            s.check_group_completion(&self.record);
        }
    }

    /// Cancels the whole group (fan-out to all non-terminal members).
    pub fn cancel(&self) {
        if let Some(s) = self.sched.upgrade() {
            s.cancel_group(self.record.id);
        } else {
            self.record.cancel.cancel();
        }
    }

    /// `(terminal members, total)` — a locally-pollable aggregate.
    pub fn progress(&self) -> (u64, u64) {
        let view = self.record.aggregate_view();
        view.items.unwrap_or((0, 0))
    }
}

impl std::fmt::Debug for GroupHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupHandle")
            .field("id", &self.record.id)
            .field("kind", &self.record.kind)
            .field("closed", &self.record.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
