// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The shell-side working-set view model (E08 spec §6.2, task A5)
//! replaces the E01 library-grid's row model (which paged the catalog; the
//! working set is session state at tens-hundreds of files, no pagination
//! needed).
//!
//! **A0 seam note (recorded in `E08-deviations.md`):** the spec's §6.2 draft
//! has `WorkingSetView::on_event` fold *individual* events into local state
//! (mirroring an earlier, more granular draft of the E04 event shape, incl.
//! a `WorkingSetEntryLoaded{epoch,index}` event that was never shipped).
//! What E04 actually shipped is coarser: `Session::working_set()` hands out
//! a cheap `Arc<WorkingSetSnapshot>` that is *already* internally
//! epoch-consistent (per-item interleaving is impossible, the whole
//! snapshot swaps atomically inside `WorkingSetModel`), and every
//! `Event::WorkingSet*` variant carries only the epoch, never a granular
//! per-item delta. So [`WorkingSetView`] does not attempt to replay events
//! into local per-item state; instead every relevant event triggers a cheap
//! wholesale re-pull of the canonical snapshot (exactly the "on `Lagged`,
//! re-pull wholesale" discipline the spec already prescribes for the lagged
//! case, here it is simply the *only* case, which is simpler and can never
//! drift from the core's own epoch-guarded state machine). The epoch guard
//! this module still owns: an event tagged with an epoch older than the
//! snapshot already applied is dropped **without even touching the
//! session** (spec A5 AC: "stale-epoch events dropped").

use std::collections::HashSet;
use std::sync::Arc;

use lightbox_core::{
    Event, ItemState, Session, SetEpoch, SetPhase, WorkingSetItem, WorkingSetSnapshot,
};
use lightbox_types::ImageId;

/// The shell's live view over the session working set (spec §6.2).
pub struct WorkingSetView {
    snapshot: Arc<WorkingSetSnapshot>,
    /// Index into `snapshot.items` of the active (loupe) entry.
    active: Option<usize>,
    /// Multi-select seed for future batch ops (M2 sync/export), carried
    /// forward from the retired `model::Selection` per the spec's crate
    /// map ("Selection retained, rescoped to active+multi").
    selected: HashSet<usize>,
}

impl WorkingSetView {
    /// Starts from `session`'s current snapshot (epoch 0/`Empty` for a
    /// fresh session, headless callers that never open anything).
    pub fn new(session: &Session) -> WorkingSetView {
        let snapshot = session.working_set();
        let mut view = WorkingSetView {
            snapshot,
            active: None,
            selected: HashSet::new(),
        };
        view.auto_activate();
        view
    }

    /// Pure state transition: apply a freshly-pulled snapshot (unit-tested
    /// directly, spec A5 AC). A snapshot older than the one already applied
    /// is ignored (can't happen through `Session::working_set()`, which
    /// always returns the live snapshot, but guarded here so the rule holds
    /// regardless of caller). A newer epoch is a **replace**: no prompt
    /// (§2.4/A6), active/selection reset, first-Ready auto-activates.
    fn apply_snapshot(&mut self, snapshot: Arc<WorkingSetSnapshot>) {
        if snapshot.epoch < self.snapshot.epoch {
            return;
        }
        let epoch_changed = snapshot.epoch != self.snapshot.epoch;
        self.snapshot = snapshot;
        if epoch_changed {
            self.active = None;
            self.selected.clear();
        } else if self.active.is_some_and(|i| i >= self.snapshot.items.len()) {
            self.active = None;
        }
        self.auto_activate();
    }

    /// First-Ready auto-activation (§2.4: "first is active in the loupe").
    /// A no-op once something is active; nav (below) may re-clear `active`.
    fn auto_activate(&mut self) {
        if self.active.is_none() {
            self.active = self
                .snapshot
                .items
                .iter()
                .position(|it| matches!(it.state, ItemState::Ready { .. }));
        }
    }

    /// Folds one core event (§6.2). A `WorkingSet*` event tagged with an
    /// epoch older than what is already applied is dropped without pulling
    /// the session (A5 AC); every other `WorkingSet*` event re-pulls the
    /// canonical snapshot (a cheap `Arc` clone, see the module docs for
    /// why this crate does not replay granular per-item state). Non-
    /// working-set events are ignored here (the caller routes those
    /// elsewhere).
    pub fn on_event(&mut self, ev: &Event, session: &Session) {
        let event_epoch: SetEpoch = match ev {
            Event::WorkingSetOpening { epoch, .. }
            | Event::WorkingSetReplaced { epoch, .. }
            | Event::WorkingSetChanged { epoch }
            | Event::WorkingSetLoadFinished { epoch, .. } => *epoch,
            _ => return,
        };
        if event_epoch < self.snapshot.epoch {
            return;
        }
        self.apply_snapshot(session.working_set());
    }

    /// Broadcast `Lagged` recovery (spec: "re-pulls `session.working_set()`
    /// wholesale").
    pub fn on_lagged(&mut self, session: &Session) {
        self.apply_snapshot(session.working_set());
    }

    /// Session order (spec §6.3 filmstrip source).
    pub fn entries(&self) -> &[WorkingSetItem] {
        &self.snapshot.items
    }

    /// The live generation.
    pub fn epoch(&self) -> SetEpoch {
        self.snapshot.epoch
    }

    /// Coarse lifecycle phase (drives the status-bar progress wording).
    pub fn phase(&self) -> SetPhase {
        self.snapshot.phase
    }

    /// `true` when `OpenOptions::max_set_size` truncated the plan.
    pub fn truncated(&self) -> bool {
        self.snapshot.truncated
    }

    /// Index of the active (loupe) entry, if any.
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    /// The active entry, if any.
    pub fn active(&self) -> Option<&WorkingSetItem> {
        self.active.and_then(|i| self.snapshot.items.get(i))
    }

    /// The active entry's `ImageId`, when it has reached `Ready` (the only
    /// state the canvas/panels can address, spec §6.4/§6.5). Phase D's
    /// `edit.undo`/`edit.redo` actions target this; Phase E's info panel
    /// will too. `lib.rs`'s canvas projection still reads `active()`
    /// directly (it needs the non-`Ready` arms for the C5 placards).
    pub fn active_image(&self) -> Option<ImageId> {
        match self.active()?.state {
            ItemState::Ready { image, .. } => Some(image),
            _ => None,
        }
    }

    /// Explicit activation (filmstrip click).
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.snapshot.items.len() {
            self.active = Some(idx);
        }
    }

    /// Clamped relative navigation (←/→); a no-op on an empty set.
    pub fn nav(&mut self, delta: isize) {
        let len = self.snapshot.items.len();
        if len == 0 {
            return;
        }
        let cur = self.active.unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1);
        self.active = Some(next as usize);
    }

    /// Drives the empty-state drop zone (spec §6.2).
    pub fn is_empty(&self) -> bool {
        self.snapshot.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_core::{ClosePolicy, Command, Core, CoreConfig, OpenOrigin, OpenRequest};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
    /// Reused across tests: the same tiny, pinned, self-made CC0 JPEG the
    /// smoke driver embeds, no `cargo xtask fixtures` dependency needed
    /// for these state-machine tests.
    const TINY_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

    fn stage_unique_jpeg(dir: &Path, name: &str, pad: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let mut bytes = TINY_JPEG.to_vec();
        bytes.extend_from_slice(pad.as_bytes());
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn wait_for_event<T>(
        rx: &mut tokio::sync::broadcast::Receiver<Event>,
        mut pred: impl FnMut(&Event) -> Option<T>,
    ) -> T {
        let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for a working-set event"
            );
            match rx.try_recv() {
                Ok(event) => {
                    if let Some(out) = pred(&event) {
                        return out;
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    panic!("event channel closed while waiting")
                }
            }
        }
    }

    /// Opens `paths` and drains every event up to (and including)
    /// `WorkingSetLoadFinished`, returning them all in arrival order.
    fn open_and_collect(session: &Session, paths: &[PathBuf]) -> (SetEpoch, Vec<Event>) {
        let mut rx = session.events();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(paths.to_vec(), false, OpenOrigin::Cli),
        });
        let epoch = wait_for_event(&mut rx, |ev| match ev {
            Event::WorkingSetOpening { epoch, .. } => Some(*epoch),
            _ => None,
        });
        let mut collected = vec![]; // WorkingSetOpening itself was consumed by wait_for_event above.
        loop {
            let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
            let ev = loop {
                assert!(std::time::Instant::now() < deadline, "timed out draining");
                match rx.try_recv() {
                    Ok(ev) => break ev,
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                        panic!("closed while draining")
                    }
                }
            };
            let done = matches!(&ev, Event::WorkingSetLoadFinished { epoch: e, .. } if *e == epoch);
            collected.push(ev);
            if done {
                break;
            }
        }
        (epoch, collected)
    }

    fn start_session(tmp: &Path) -> (Core, Session) {
        let core = Core::start(CoreConfig::default()).expect("core start");
        let session = core
            .create_catalog(&tmp.join("wsv.lbdata"), None)
            .expect("create catalog");
        (core, session)
    }

    /// A5 AC: first-Ready entry auto-activates (spec §2.4).
    #[test]
    fn first_ready_entry_auto_activates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photos = tmp.path().join("photos");
        let a = stage_unique_jpeg(&photos, "a.jpg", "a");
        let b = stage_unique_jpeg(&photos, "b.jpg", "b");

        let (_epoch, _events) = open_and_collect(&session, &[a, b]);

        let view = WorkingSetView::new(&session);
        assert_eq!(view.entries().len(), 2);
        assert_eq!(view.active_index(), Some(0), "first Ready entry activates");
        assert!(view.active_image().is_some());
    }

    /// A5 AC + A6 AC: a second `OpenWorkingSet` replaces the set with no
    /// prompt, the view converges on the newer epoch, and a *stale* event
    /// from the superseded epoch (captured genuinely from the first open's
    /// real event stream, not fabricated) is dropped rather than regressing
    /// the view.
    #[test]
    fn stale_epoch_events_are_dropped_after_a_replace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photos = tmp.path().join("photos");

        let first = stage_unique_jpeg(&photos, "first.jpg", "1");
        let (epoch1, events1) = open_and_collect(&session, &[first]);

        let second_a = stage_unique_jpeg(&photos, "second-a.jpg", "2a");
        let second_b = stage_unique_jpeg(&photos, "second-b.jpg", "2b");
        let second_c = stage_unique_jpeg(&photos, "second-c.jpg", "2c");
        let (epoch2, _events2) = open_and_collect(&session, &[second_a, second_b, second_c]);
        assert!(epoch2 > epoch1, "the replace bumped the epoch");

        // The view only pulls the session AFTER both opens finished, so it
        // starts already converged on epoch2 (matches how the shell frame
        // loop actually observes events, always at-or-behind the live
        // snapshot, never ahead of it).
        let mut view = WorkingSetView::new(&session);
        assert_eq!(view.epoch(), epoch2);
        assert_eq!(view.entries().len(), 3, "epoch2's three items");

        // Feed epoch1's own real events (genuinely emitted, just held back)
        // into the already-epoch2 view: must be a no-op.
        for ev in &events1 {
            view.on_event(ev, &session);
        }
        assert_eq!(view.epoch(), epoch2, "stale epoch1 events never regress");
        assert_eq!(view.entries().len(), 3);
    }

    /// A5 AC: `Lagged` recovery re-pulls wholesale and converges.
    #[test]
    fn lagged_recovery_converges_on_the_live_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photos = tmp.path().join("photos");
        let a = stage_unique_jpeg(&photos, "a.jpg", "a");

        let view_before = WorkingSetView::new(&session);
        assert!(view_before.is_empty(), "nothing opened yet");

        let (_epoch, _events) = open_and_collect(&session, &[a]);

        let mut view = view_before;
        assert!(
            view.is_empty(),
            "the view itself hasn't observed anything yet"
        );
        view.on_lagged(&session);
        assert!(!view.is_empty(), "lagged re-pull converges on the live set");
        assert_eq!(view.active_index(), Some(0));
    }

    /// A6 AC: replacing the set clears the previous active/selection state
    /// (no stale selection pointing past the new, possibly-shorter, set).
    #[test]
    fn replace_resets_active_and_selection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let photos = tmp.path().join("photos");
        let a = stage_unique_jpeg(&photos, "a.jpg", "a");
        let b = stage_unique_jpeg(&photos, "b.jpg", "b");
        open_and_collect(&session, &[a, b]);

        let mut view = WorkingSetView::new(&session);
        view.set_active(1);
        view.selected.insert(1);
        assert_eq!(view.active_index(), Some(1));

        let only = stage_unique_jpeg(&photos, "only.jpg", "only");
        open_and_collect(&session, &[only]);
        view.on_lagged(&session); // wholesale re-pull, as a live frame loop would eventually do

        assert_eq!(view.entries().len(), 1);
        assert_eq!(
            view.active_index(),
            Some(0),
            "auto-activates the new set's first Ready entry, not the stale index"
        );
        assert!(view.selected.is_empty());
    }

    #[test]
    fn nav_clamps_and_is_a_noop_on_empty_sets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        let mut view = WorkingSetView::new(&session);
        view.nav(1); // empty set: no panic, stays empty
        assert!(view.active_index().is_none());

        let photos = tmp.path().join("photos");
        let a = stage_unique_jpeg(&photos, "a.jpg", "a");
        let b = stage_unique_jpeg(&photos, "b.jpg", "b");
        open_and_collect(&session, &[a, b]);
        view.on_lagged(&session);
        assert_eq!(view.active_index(), Some(0));
        view.nav(1);
        assert_eq!(view.active_index(), Some(1));
        view.nav(5);
        assert_eq!(view.active_index(), Some(1), "clamped at the end");
        view.nav(-5);
        assert_eq!(view.active_index(), Some(0), "clamped at the start");
    }

    // Avoids an unused-import warning under `#[cfg(test)]` builds where
    // `ClosePolicy` isn't otherwise reached (kept for parity with the other
    // headless-session tests in this workspace, which close explicitly).
    #[test]
    fn sessions_close_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_core, session) = start_session(tmp.path());
        session
            .close(ClosePolicy::Skip.into())
            .expect("close should succeed");
    }
}
