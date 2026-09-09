// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Scripted filmstrip-scroll perf capture (`lightbox --perf-strip
//! [FRAMES]`, E08 spec §8 H2, the named replacement for the retired
//! `--perf-scroll`; see `E08-deviations.md` "A1 (consequence)").
//!
//! Stages a throwaway catalog plus a ~300-entry synthetic working set
//! (unique-content tiny JPEGs), opens it through the REAL entry pipeline
//! (`Command::OpenWorkingSet`, not the retired import path), then drives
//! the real filmstrip with a forced **sawtooth horizontal scroll** for
//! `FRAMES` measured frames (two full sweeps), followed by a **nav phase**
//! (`WorkingSetView::nav` steps through the set, the same seam the
//! `nav.next` action drives, feeding the canvas's nav-swap probe). On
//! completion it prints **one JSON summary line** and closes; the
//! `lbx-perf` `perf-strip` scenario and the nightly workflow consume it,
//! asserting the §7 budgets **budget-first** (frame p95 < 16 ms with the
//! rail open, nav-swap p95 < 50 ms; a regression files a tracked issue,
//! non-blocking, per E01 semantics).

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Unique-content synthetic JPEGs: the pinned self-made CC0 16×16 JFIF
/// (same asset the smoke mode embeds) + a unique pad after EOI per file.
const TINY_JPEG: &[u8] = include_bytes!("../../../tools/xtask/assets/lightbox-tiny.jpg");

/// Entries staged for the strip (spec H2: "a ~300-entry synthetic set").
const ENTRIES: usize = 300;

/// Nav steps in the post-scroll nav phase, and frames between steps (the
/// gap gives each engine render time to land so the swap probe measures a
/// real completion, not a queue).
const NAV_STEPS: usize = 32;
const NAV_GAP_FRAMES: u64 = 4;

/// Hard wall-clock cap before the run gives up (open + scroll + nav).
const HARD_TIME_CAP: Duration = Duration::from_secs(180);

/// What [`StripDriver::pump`] tells the app to do this frame.
pub enum StripPhase {
    /// Working set still loading, render normally, don't measure.
    Warmup,
    /// Scroll the strip to this fraction of its scrollable range; the
    /// frame is measured.
    Scroll(f32),
    /// Step the active entry forward (`WorkingSetView::nav(1)`).
    Nav,
    /// Between nav steps, keep painting, no input.
    NavSettle,
    /// Capture complete, the one-line JSON summary, then close.
    Done(String),
    /// Wedged (open never landed / wall cap), close with failure.
    Expired,
}

/// Scripted state for one `--perf-strip` capture.
pub struct StripDriver {
    /// Owns the throwaway catalog + staged set until exit.
    _tmp: tempfile::TempDir,
    /// Where the throwaway catalog lives.
    pub lbdata: PathBuf,
    /// The staged photo directory (opened via `OpenWorkingSet`).
    pub photos: PathBuf,
    /// `Command::OpenWorkingSet` submitted?
    pub submitted: bool,
    frames_target: u64,
    scroll_frame: u64,
    nav_frame: u64,
    samples_ms: Vec<f32>,
    last_frame: Option<Instant>,
    /// `canvas.nav_swap_ms.len()` when the nav phase began, only samples
    /// after this index belong to the measured nav phase.
    nav_baseline: Option<usize>,
    /// When the `OpenWorkingSet` was submitted (drop→first-pixels probe).
    opened_at: Option<Instant>,
    /// Open-submit → first engine texture composited (§7: the drop →
    /// first-image budget, < 500 ms on the dev baseline).
    first_pixels_ms: Option<f32>,
    started: Instant,
    reported: bool,
}

impl StripDriver {
    /// Stages the temp catalog dir + [`ENTRIES`] unique synthetic JPEGs.
    pub fn new(frames: u64) -> std::io::Result<StripDriver> {
        let tmp = tempfile::TempDir::with_prefix("lightbox-perf-strip-")?;
        let photos = tmp.path().join("photos");
        std::fs::create_dir_all(&photos)?;
        for i in 0..ENTRIES {
            let mut bytes = TINY_JPEG.to_vec();
            bytes.extend_from_slice(format!("perf-strip pad {i:06}").as_bytes());
            std::fs::write(photos.join(format!("strip-{i:06}.jpg")), bytes)?;
        }
        Ok(StripDriver {
            lbdata: tmp.path().join("perf-strip.lbdata"),
            photos,
            _tmp: tmp,
            submitted: false,
            frames_target: frames.max(60),
            scroll_frame: 0,
            nav_frame: 0,
            samples_ms: Vec::with_capacity(frames.max(60) as usize),
            last_frame: None,
            nav_baseline: None,
            opened_at: None,
            first_pixels_ms: None,
            started: Instant::now(),
            reported: false,
        })
    }

    /// Marks the moment `OpenWorkingSet` was submitted (the caller flips
    /// `submitted` right after).
    pub fn mark_opened(&mut self) {
        self.opened_at = Some(Instant::now());
    }

    /// Advances the script one frame. `loaded` is how many working-set
    /// items are past `Planned`; `swaps` is the app's engine-texture swap
    /// counter (drop→first-pixels probe); `nav_swap_ms` is the canvas's
    /// rolling nav-swap probe; `thumb_stats` = (requests issued,
    /// cancelled).
    pub fn pump(
        &mut self,
        loaded: usize,
        swaps: u64,
        nav_swap_ms: &[f32],
        thumb_stats: (u64, u64),
    ) -> StripPhase {
        if self.started.elapsed() > HARD_TIME_CAP {
            return StripPhase::Expired;
        }
        // §7 "drop of a single local raw → first image composited": open
        // submit → the first engine texture swap of the auto-activated
        // entry (embedded-tier fallback included, whatever composited
        // first).
        if self.first_pixels_ms.is_none() && swaps > 0 {
            if let Some(at) = self.opened_at {
                self.first_pixels_ms = Some(at.elapsed().as_secs_f32() * 1000.0);
            }
        }
        if loaded < ENTRIES {
            // Open still landing; don't start the measured sweep yet.
            self.last_frame = None;
            return StripPhase::Warmup;
        }
        if self.reported {
            // Capture complete; the window is on its way out.
            return StripPhase::Warmup;
        }

        // ── Phase 1: the measured sawtooth scroll. ───────────────────────
        if self.scroll_frame < self.frames_target {
            let now = Instant::now();
            if let Some(last) = self.last_frame.replace(now) {
                self.samples_ms
                    .push(now.duration_since(last).as_secs_f32() * 1000.0);
            }
            let frac = self.sawtooth();
            self.scroll_frame += 1;
            return StripPhase::Scroll(frac);
        }

        // ── Phase 2: nav steps through the set (nav-swap probe). ─────────
        let baseline = *self.nav_baseline.get_or_insert(nav_swap_ms.len());
        if self.nav_frame < NAV_STEPS as u64 * NAV_GAP_FRAMES {
            let step_now = self.nav_frame.is_multiple_of(NAV_GAP_FRAMES);
            self.nav_frame += 1;
            return if step_now {
                StripPhase::Nav
            } else {
                StripPhase::NavSettle
            };
        }
        // Wait for the last nav's swap to land (up to the wall cap) so the
        // p95 covers every step actually taken.
        let nav_samples = nav_swap_ms.len().saturating_sub(baseline);
        if nav_samples < NAV_STEPS && self.started.elapsed() < HARD_TIME_CAP {
            return StripPhase::NavSettle;
        }

        self.reported = true;
        StripPhase::Done(self.summary(&nav_swap_ms[baseline..], thumb_stats))
    }

    /// Two full sweeps (left → right → left) across the frame budget.
    fn sawtooth(&self) -> f32 {
        let t = self.scroll_frame as f32 / self.frames_target as f32; // 0..1
        let cycle = (t * 2.0) % 2.0; // two sweeps
        if cycle <= 1.0 {
            cycle
        } else {
            2.0 - cycle
        }
    }

    /// The one-line JSON summary (consumed by `lbx-perf`'s `perf-strip`
    /// scenario + the nightly workflow's job summary).
    fn summary(&self, nav_swap_ms: &[f32], (requested, cancelled): (u64, u64)) -> String {
        let p = |q: f32| -> f32 { crate::percentile(&self.samples_ms, q) };
        let max = self.samples_ms.iter().copied().fold(0.0f32, f32::max);
        format!(
            concat!(
                "{{\"scenario\":\"perf-strip\",\"entries\":{},\"frames\":{},",
                "\"frame_ms_p50\":{:.2},\"frame_ms_p95\":{:.2},\"frame_ms_max\":{:.2},",
                "\"open_to_first_pixels_ms\":{:.2},",
                "\"nav_steps\":{},\"nav_swap_p95_ms\":{:.2},",
                "\"thumb_requests\":{},\"thumb_cancelled\":{}}}"
            ),
            ENTRIES,
            self.samples_ms.len(),
            p(0.5),
            p(0.95),
            max,
            self.first_pixels_ms.unwrap_or(-1.0),
            nav_swap_ms.len(),
            crate::percentile(nav_swap_ms, 0.95),
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
        let driver = StripDriver::new(60).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&driver.photos)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        assert_eq!(names.len(), ENTRIES);
        let a = std::fs::read(&names[0]).unwrap();
        let b = std::fs::read(&names[1]).unwrap();
        assert_eq!(&a[..3], &[0xFF, 0xD8, 0xFF], "JPEG SOI");
        assert_ne!(a, b, "content hashes must differ (no dup-collapse)");
    }

    #[test]
    fn script_waits_for_load_then_sweeps_then_navs_then_reports() {
        let mut driver = StripDriver::new(60).unwrap();
        driver.mark_opened();
        assert!(matches!(driver.pump(0, 0, &[], (0, 0)), StripPhase::Warmup));
        assert!(matches!(
            driver.pump(ENTRIES - 1, 0, &[], (0, 0)),
            StripPhase::Warmup
        ));

        // Loaded: the sawtooth sweeps for `frames` pumps…
        let mut fracs = Vec::new();
        let mut navs = 0usize;
        let mut nav_samples: Vec<f32> = Vec::new();
        let json = loop {
            match driver.pump(ENTRIES, 1, &nav_samples, (10, 2)) {
                StripPhase::Scroll(f) => fracs.push(f),
                StripPhase::Nav => {
                    navs += 1;
                    // Each nav lands a synthetic swap sample next frame.
                    nav_samples.push(5.0);
                }
                StripPhase::NavSettle => {}
                StripPhase::Done(json) => break json,
                StripPhase::Warmup | StripPhase::Expired => panic!("unexpected phase"),
            }
        };
        assert_eq!(fracs.len(), 60);
        assert_eq!(navs, NAV_STEPS);
        assert!(json.contains("\"scenario\":\"perf-strip\""));
        assert!(json.contains("\"entries\":300"));
        assert!(json.contains("\"nav_steps\":32"));
        assert!(
            json.contains("\"open_to_first_pixels_ms\":"),
            "first-pixels probe present: {json}"
        );
        assert!(json.contains("\"nav_swap_p95_ms\":5.00"));
        assert!(json.contains("\"thumb_requests\":10"));

        // The sawtooth covers the full range in both directions.
        assert!(fracs.iter().copied().fold(0.0f32, f32::max) > 0.9);
        assert!(fracs[0] < 0.05 && fracs[fracs.len() - 1] < 0.05);

        // Reported exactly once: closing frames are no-ops.
        assert!(matches!(
            driver.pump(ENTRIES, 1, &nav_samples, (10, 2)),
            StripPhase::Warmup
        ));
    }

    #[test]
    fn expiry_guards_a_wedged_open() {
        let mut driver = StripDriver::new(60).unwrap();
        driver.started = Instant::now() - HARD_TIME_CAP - Duration::from_secs(1);
        assert!(matches!(
            driver.pump(0, 0, &[], (0, 0)),
            StripPhase::Expired
        ));
    }
}
