// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! H4, the **editor leg** of `cargo xtask exit-drill` (spec §8 H4 / §11
//! DoD 1): "open a fixture set → edit via a slider → `kill -9` → reopen →
//! recipe restored → filmstrip/canvas smoke", scripted through the REAL
//! editor pipeline on real hardware.
//!
//! Two scripted modes, driven by xtask as two processes on one persistent
//! catalog:
//!
//! * **`--drill-edit <PHOTOS>`** (needs `--catalog`): opens the set,
//!   waits for the first entry to reach the canvas (filmstrip row +
//!   engine texture composited, the same proof `--smoke` requires), then
//!   performs one real Exposure gesture through [`SessionEditBinding`]
//!   `begin_gesture` → N `preview` frames → `end_gesture`, byte-for-byte
//!   the path a `param_slider` drag takes, and, once the durable
//!   `Event::EditCommitted` lands, prints one `DRILL-EDIT image=<id>
//!   exposure=<v> seq=<n>` line and **keeps painting until killed**
//!   (xtask delivers the `kill -9`).
//! * **`--drill-verify <PHOTOS> --expect-image <id> --expect-exposure
//!   <v>`**: reopens the same catalog + set, requires the same
//!   filmstrip/canvas proof (the "smoke" half of the leg), then asserts
//!   the bound image and its **restored** Exposure recipe value match
//!   what the killed process committed; prints `DRILL-VERIFY ok …` and
//!   exits 0 (any mismatch exits 1).
//!
//! [`SessionEditBinding`]: crate::panels::SessionEditBinding

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lightbox_types::ImageId;

/// Which drill leg this process runs (parsed in `main.rs`).
#[derive(Debug, Clone)]
pub enum DrillMode {
    /// Open `photos`, commit one Exposure gesture, announce it, hold.
    EditCommit {
        /// Files/folder to open.
        photos: PathBuf,
        /// The Exposure value the gesture lands on.
        exposure: f32,
    },
    /// Reopen `photos`, prove the canvas, assert the restored recipe.
    Verify {
        /// Files/folder to open.
        photos: PathBuf,
        /// The image id the edit leg announced.
        expect_image: i64,
        /// The Exposure value the edit leg committed.
        expect_exposure: f32,
    },
}

/// The state machine, advanced once per frame by `lib.rs::pump_drill`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrillState {
    /// `OpenWorkingSet` not yet submitted.
    Submit,
    /// Waiting for the first Ready entry + the canvas seam proof.
    WaitCanvas,
    /// Edit leg: gesture in progress; the payload counts preview frames.
    Dragging(u32),
    /// Edit leg: `end_gesture` sent, waiting for `EditCommitted`.
    AwaitCommit,
    /// Edit leg: announced on stdout, hold until killed.
    Hold,
    /// Verify leg: verdict printed, close requested.
    Closing,
}

/// Preview frames per gesture (a realistic short drag).
pub const DRAG_FRAMES: u32 = 20;

/// Wall-clock cap: a drill leg that hasn't reached its goal by then is
/// wedged, close with a failure verdict instead of hanging xtask.
const HARD_TIME_CAP: Duration = Duration::from_secs(120);

/// Scripted state for one drill leg.
pub struct DrillDriver {
    /// The mode this process runs.
    pub mode: DrillMode,
    /// Current state.
    pub state: DrillState,
    /// The committed step observed (edit leg).
    pub committed_seq: Option<u64>,
    started: Instant,
}

impl DrillDriver {
    /// A fresh driver for `mode`.
    pub fn new(mode: DrillMode) -> DrillDriver {
        DrillDriver {
            mode,
            state: DrillState::Submit,
            committed_seq: None,
            started: Instant::now(),
        }
    }

    /// The paths to open (both modes).
    pub fn photos(&self) -> PathBuf {
        match &self.mode {
            DrillMode::EditCommit { photos, .. } => photos.clone(),
            DrillMode::Verify { photos, .. } => photos.clone(),
        }
    }

    /// True past the wall-clock cap (wedged, fail the leg).
    pub fn expired(&self) -> bool {
        self.started.elapsed() > HARD_TIME_CAP
    }

    /// `Event::EditCommitted` for the bound image landed (hooked from the
    /// app's `drain_events`).
    pub fn on_edit_committed(&mut self, image: ImageId, seq: u64, bound: Option<ImageId>) {
        if self.state == DrillState::AwaitCommit && bound == Some(image) {
            self.committed_seq = Some(seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_is_only_accepted_while_awaiting_and_for_the_bound_image() {
        let mut d = DrillDriver::new(DrillMode::EditCommit {
            photos: PathBuf::from("/photos"),
            exposure: 0.75,
        });
        // Not awaiting yet: ignored.
        d.on_edit_committed(ImageId(1), 1, Some(ImageId(1)));
        assert_eq!(d.committed_seq, None);

        d.state = DrillState::AwaitCommit;
        // Wrong image: ignored.
        d.on_edit_committed(ImageId(2), 1, Some(ImageId(1)));
        assert_eq!(d.committed_seq, None);
        // Bound image: accepted.
        d.on_edit_committed(ImageId(1), 3, Some(ImageId(1)));
        assert_eq!(d.committed_seq, Some(3));
    }

    #[test]
    fn expiry_trips_after_the_cap() {
        let mut d = DrillDriver::new(DrillMode::Verify {
            photos: PathBuf::from("/photos"),
            expect_image: 1,
            expect_exposure: 0.75,
        });
        assert!(!d.expired());
        d.started = Instant::now() - HARD_TIME_CAP - Duration::from_secs(1);
        assert!(d.expired());
    }
}
