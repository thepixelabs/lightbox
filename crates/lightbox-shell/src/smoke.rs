// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Smoke mode (`lightbox --smoke [FRAMES]`, CI gate): drives the REAL E08
//! entry pipeline headlessly, temp catalog → `Command::OpenWorkingSet` of
//! an embedded self-made JPEG → filmstrip row → canvas render through
//! `Engine::submit`, and requires that an engine texture was composited
//! zero-copy on the shared device (the seam-2 proof, spec T7/A7/§9 M0).
//!
//! **A7 rework:** the E01 seed drove this via the retired managed-import
//! command +
//! an explicit "enter the loupe" step. The v2.0 entry model has no such
//! step, `OpenWorkingSet` is submitted once and the working-set view model
//! auto-activates the first `Ready` entry (§2.4) the moment it lands, which
//! the smoke driver now waits for directly instead of polling a grid row
//! count.
//!
//! The app closes once the seam is proven AND at least `min_frames` frames
//! painted; a hard frame/time cap turns a wedged pipeline into a nonzero
//! exit instead of a hung CI job.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The open source: the same pinned, self-made CC0 16×16 JFIF JPEG the
/// fixture corpus embeds (tools/xtask/assets, kept as one committed asset
/// so the smoke binary works from a bare checkout, offline).
const SMOKE_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

/// Hard wall-clock cap before the smoke run gives up.
const HARD_TIME_CAP: Duration = Duration::from_secs(90);

/// Scripted state for one smoke run.
pub struct SmokeDriver {
    /// Owns the throwaway catalog + open source until exit.
    _tmp: tempfile::TempDir,
    /// Where the throwaway catalog lives.
    pub lbdata: PathBuf,
    /// The staged open source directory.
    pub photos: PathBuf,
    /// `Command::OpenWorkingSet` submitted?
    pub submitted: bool,
    min_frames: u64,
    started: Instant,
}

impl SmokeDriver {
    /// Stages the temp catalog dir + open source.
    pub fn new(min_frames: u64) -> std::io::Result<SmokeDriver> {
        let tmp = tempfile::TempDir::with_prefix("lightbox-smoke-")?;
        let photos = tmp.path().join("photos");
        std::fs::create_dir_all(&photos)?;
        std::fs::write(photos.join("smoke.jpg"), SMOKE_JPEG)?;
        Ok(SmokeDriver {
            lbdata: tmp.path().join("smoke.lbdata"),
            photos,
            _tmp: tmp,
            submitted: false,
            min_frames,
            started: Instant::now(),
        })
    }

    /// Success close: the filmstrip showed a row AND the canvas seam was
    /// proven AND the frame minimum painted.
    pub fn done(&self, frames: u64, filmstrip_shown: bool, seam_proven: bool) -> bool {
        filmstrip_shown && seam_proven && frames >= self.min_frames
    }

    /// Failure close: pipeline wedged (frame or wall-clock cap hit).
    pub fn expired(&self, frames: u64) -> bool {
        frames >= self.min_frames.saturating_mul(30).max(1800)
            || self.started.elapsed() > HARD_TIME_CAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_source_is_the_pinned_jpeg() {
        let smoke = SmokeDriver::new(60).unwrap();
        let staged = std::fs::read(smoke.photos.join("smoke.jpg")).unwrap();
        assert_eq!(staged, SMOKE_JPEG);
        assert_eq!(&staged[..3], &[0xFF, 0xD8, 0xFF], "JPEG SOI");
        assert!(
            !smoke.done(1000, true, false),
            "never done without the seam proof"
        );
        assert!(
            !smoke.done(1000, false, true),
            "never done without a filmstrip row"
        );
        assert!(smoke.done(60, true, true));
        assert!(!smoke.done(59, true, true), "respects the frame minimum");
        assert!(smoke.expired(u64::MAX), "frame cap closes a wedged run");
    }
}
