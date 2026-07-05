// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Scripted grid-scroll perf capture (`lightbox --perf-scroll [FRAMES]`,
//! E01 T28: "grid-scroll frame-time capture (headless egui harness or
//! scripted app)" — this is the scripted app).
//!
//! Stages a throwaway catalog with a few hundred unique synthetic JPEGs,
//! imports them through the command bus, then drives the REAL grid — the
//! same virtualization, thumbnail, and cancel-on-scroll-out code paths as
//! interactive use — with a forced sawtooth scroll for `FRAMES` frames,
//! recording per-frame times. On completion it prints one JSON line
//! (consumed by the nightly workflow's job summary) and closes.
//!
//! The T25 acceptance number this captures formally: grid scroll frame
//! time p95 < 16 ms on a dev laptop.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Unique-content synthetic JPEGs: the pinned self-made CC0 16×16 JFIF
/// (same asset the smoke mode embeds) + a unique pad after EOI per file.
const TINY_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

/// Images staged for the scroll (several hundred: enough rows to scroll
/// through many viewport-loads at default cell size).
const IMAGES: usize = 360;

/// Hard wall-clock cap before the run gives up (import + scroll).
const HARD_TIME_CAP: Duration = Duration::from_secs(180);

/// Scripted state for one grid-scroll capture.
pub struct ScrollDriver {
    /// Owns the throwaway catalog + import source until exit.
    _tmp: tempfile::TempDir,
    /// Where the throwaway catalog lives.
    pub lbdata: PathBuf,
    /// The staged import source directory.
    pub photos: PathBuf,
    /// Import submitted?
    pub submitted: bool,
    /// How many images were staged.
    pub images: usize,
    frames_target: u64,
    scroll_frame: u64,
    samples_ms: Vec<f32>,
    last_frame: Option<Instant>,
    started: Instant,
    reported: bool,
}

/// What [`ScrollDriver::pump`] tells the app to do this frame.
pub enum ScrollPhase {
    /// Import still landing — render the grid normally, don't measure.
    Warmup,
    /// Scroll to this fraction of the scrollable range and measure.
    Scroll(f32),
    /// Capture complete — the summary JSON line, then close.
    Done(String),
    /// Wedged (import never landed / wall cap) — close with failure.
    Expired,
}

impl ScrollDriver {
    /// Stages the temp catalog dir + `IMAGES` unique synthetic JPEGs.
    pub fn new(frames: u64) -> std::io::Result<ScrollDriver> {
        let tmp = tempfile::TempDir::with_prefix("lightbox-perf-scroll-")?;
        let photos = tmp.path().join("photos");
        std::fs::create_dir_all(&photos)?;
        for i in 0..IMAGES {
            let mut bytes = TINY_JPEG.to_vec();
            bytes.extend_from_slice(format!("perf-scroll pad {i:06}").as_bytes());
            std::fs::write(photos.join(format!("scroll-{i:06}.jpg")), bytes)?;
        }
        Ok(ScrollDriver {
            lbdata: tmp.path().join("perf-scroll.lbdata"),
            photos,
            _tmp: tmp,
            submitted: false,
            images: IMAGES,
            frames_target: frames.max(60),
            scroll_frame: 0,
            samples_ms: Vec::with_capacity(frames.max(60) as usize),
            last_frame: None,
            started: Instant::now(),
            reported: false,
        })
    }

    /// Advances the script one frame. `rows` is the grid model's current
    /// row count; `thumb_stats` = (requests issued, requests cancelled).
    pub fn pump(&mut self, rows: usize, thumb_stats: (u64, u64)) -> ScrollPhase {
        if self.started.elapsed() > HARD_TIME_CAP {
            return ScrollPhase::Expired;
        }
        if rows < self.images {
            // Import still landing; don't start the measured sweep yet.
            self.last_frame = None;
            return ScrollPhase::Warmup;
        }
        if self.reported {
            // Capture complete; the window is on its way out.
            return ScrollPhase::Warmup;
        }
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.samples_ms
                .push(now.duration_since(last).as_secs_f32() * 1000.0);
        }
        if self.scroll_frame >= self.frames_target {
            self.reported = true;
            return ScrollPhase::Done(self.summary(thumb_stats));
        }
        let frac = self.sawtooth();
        self.scroll_frame += 1;
        ScrollPhase::Scroll(frac)
    }

    /// Two full sweeps (top → bottom → top) across the frame budget.
    fn sawtooth(&self) -> f32 {
        let t = self.scroll_frame as f32 / self.frames_target as f32; // 0..1
        let cycle = (t * 2.0) % 2.0; // two sweeps
        if cycle <= 1.0 {
            cycle
        } else {
            2.0 - cycle
        }
    }

    /// The one-line JSON published by the nightly workflow.
    fn summary(&self, (requested, cancelled): (u64, u64)) -> String {
        let p = |q: f32| -> f32 { crate::percentile(&self.samples_ms, q) };
        let max = self.samples_ms.iter().copied().fold(0.0f32, f32::max);
        format!(
            concat!(
                "{{\"scenario\":\"grid-scroll\",\"images\":{},\"frames\":{},",
                "\"frame_ms_p50\":{:.2},\"frame_ms_p95\":{:.2},\"frame_ms_max\":{:.2},",
                "\"thumb_requests\":{},\"thumb_cancelled\":{}}}"
            ),
            self.images,
            self.samples_ms.len(),
            p(0.5),
            p(0.95),
            max,
            requested,
            cancelled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_corpus_is_unique_jpegs() {
        let driver = ScrollDriver::new(60).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&driver.photos)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        assert_eq!(names.len(), IMAGES);
        let a = std::fs::read(&names[0]).unwrap();
        let b = std::fs::read(&names[1]).unwrap();
        assert_eq!(&a[..3], &[0xFF, 0xD8, 0xFF], "JPEG SOI");
        assert_ne!(a, b, "content hashes must differ (dup-skip)");
    }

    #[test]
    fn script_waits_for_rows_then_sweeps_then_finishes() {
        let mut driver = ScrollDriver::new(60).unwrap();
        assert!(matches!(driver.pump(0, (0, 0)), ScrollPhase::Warmup));
        // Rows landed: sweeps for `frames` pumps…
        let mut fracs = Vec::new();
        loop {
            match driver.pump(IMAGES, (10, 2)) {
                ScrollPhase::Scroll(f) => fracs.push(f),
                ScrollPhase::Done(json) => {
                    assert!(json.contains("\"scenario\":\"grid-scroll\""));
                    assert!(json.contains("\"thumb_requests\":10"));
                    break;
                }
                _ => panic!("unexpected phase"),
            }
        }
        // Reported exactly once: the closing frames are no-ops.
        assert!(matches!(driver.pump(IMAGES, (10, 2)), ScrollPhase::Warmup));
        assert_eq!(fracs.len(), 60);
        // The sawtooth covers the full range in both directions.
        assert!(fracs.iter().copied().fold(0.0f32, f32::max) > 0.9);
        assert!(fracs[0] < 0.05 && fracs[fracs.len() - 1] < 0.05);
        assert!((0.0..=1.0).contains(&fracs.iter().copied().fold(0.0f32, f32::max)));
    }

    #[test]
    fn expiry_guards_a_wedged_import() {
        let mut driver = ScrollDriver::new(60).unwrap();
        driver.started = Instant::now() - HARD_TIME_CAP - Duration::from_secs(1);
        assert!(matches!(driver.pump(0, (0, 0)), ScrollPhase::Expired));
    }
}
