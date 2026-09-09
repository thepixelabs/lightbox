// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Screenshot mode (`lightbox [PATH...] --screenshot <FILE.png>`, developer
//! affordance): paint the REAL app for a settle window, ask the compositor
//! for the viewport's own pixels, write them to a PNG, exit 0.
//!
//! **Why this is not a scripted mode.** `--smoke`/`--perf-strip`/`--drill-*`
//! all *replace* the entry pipeline with a staged synthetic set, which is
//! exactly what a UI capture must not do: the two states worth reviewing are
//! the empty-state drop target (no argv paths at all) and the loaded editor
//! (argv paths through the real A4 intake), and scripted modes forbid argv
//! paths outright (`main.rs`). So screenshot mode runs the normal app and
//! borrows only the scripted modes' *config* posture, a throwaway
//! catalog/prefs/keymap dir (see `ShellOptions::screenshot`), so that a
//! capture never mutates the developer's real catalog and looks the same on
//! every machine regardless of persisted panel/theme state.
//!
//! **The capture handshake** (egui 0.35). `ViewportCommand::Screenshot` is a
//! request, not a read: the wgpu backend copies the surface texture after the
//! *next* paint and maps it back asynchronously, so the pixels arrive as an
//! `egui::Event::Screenshot` in the raw-input event list some frames later
//! (`egui_wgpu::winit::Painter::handle_screenshots` pushes it at the top of a
//! later frame). [`ScreenshotDriver::step`] is that handshake as a state
//! machine, settle, request, poll, done, so the frame pump in `lib.rs`
//! stays four lines and the interesting decisions stay unit-testable without
//! a GPU.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

/// Frames painted before the capture is requested, when `--screenshot-frames`
/// says nothing. Matches `--smoke`'s default (~1 s at vsync): long enough for
/// layout to settle, fonts to rasterize, and a small preview to decode.
pub const DEFAULT_SETTLE_FRAMES: u64 = 90;

/// Once the settle window has passed we ALSO wait for the pipeline to go
/// quiet (no decode/thumb/render in flight) so the canvas has real content
/// but only this many times the settle window. A pipeline that never quiets
/// (a slow raw, a wedged job) should still yield a picture with a warning,
/// not a failure with nothing to look at.
const PATIENCE_MULTIPLIER: u64 = 6;

/// Wall-clock twin of [`PATIENCE_MULTIPLIER`], a machine painting at 5 fps
/// must not turn "wait for quiet" into a minute of black window.
const PATIENCE_TIME: Duration = Duration::from_secs(20);

/// Frames to wait for the compositor's reply after the request goes out
/// before declaring the capture path broken (2 s at vsync; the reply
/// normally lands within one or two frames).
const REPLY_CAP_FRAMES: u64 = 120;

/// Hard wall-clock cap on the whole run, mirroring `smoke.rs`, a wedged
/// capture becomes a nonzero exit instead of a hung dev session.
const HARD_TIME_CAP: Duration = Duration::from_secs(60);

/// The `--screenshot*` knobs, parsed from argv (`main.rs`) into
/// [`crate::ShellOptions::screenshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotOptions {
    /// Where the PNG goes. Parent directories are created on demand.
    pub path: PathBuf,
    /// Frames to paint before requesting the capture (`--screenshot-frames`).
    pub settle_frames: u64,
    /// Window inner size in logical points (`--screenshot-size WxH`);
    /// `None` keeps the app's own 1100x720 default.
    pub size: Option<[f32; 2]>,
}

impl ScreenshotOptions {
    /// A capture to `path` with the default settle window and window size.
    pub fn new(path: PathBuf) -> ScreenshotOptions {
        ScreenshotOptions {
            path,
            settle_frames: DEFAULT_SETTLE_FRAMES,
            size: None,
        }
    }
}

/// Parses a `--screenshot-size` value: `WxH` logical points, e.g. `1600x1000`.
///
/// Returns `None` for anything that would give an unusable viewport (missing
/// separator, non-numeric, zero, or absurdly large) so the caller can print
/// one usage error instead of booting a 0-pixel window.
pub fn parse_size(raw: &str) -> Option<[f32; 2]> {
    let (w, h) = raw.split_once(['x', 'X'])?;
    let w: f32 = w.trim().parse().ok()?;
    let h: f32 = h.trim().parse().ok()?;
    // 64 pt floor keeps the window addressable; 16384 pt ceiling is well past
    // any real display and short of anything that would blow up the capture
    // buffer allocation.
    (64.0..=16384.0).contains(&w).then_some(())?;
    (64.0..=16384.0).contains(&h).then_some(())?;
    Some([w, h])
}

/// Which step of the capture handshake we are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Painting frames so layout settles and previews decode.
    Settling,
    /// `ViewportCommand::Screenshot` is out; the reply event is pending.
    AwaitingReply { requested_at: u64 },
    /// The PNG was written (or the run gave up); nothing left to do.
    Finished,
}

/// What [`ScreenshotDriver::step`] wants this frame's pump to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Keep painting; the app has not settled yet.
    Settle,
    /// Send `ViewportCommand::Screenshot` this frame.
    Request,
    /// Scan this frame's input events for the reply image.
    Poll,
    /// The reply never came (frame or wall-clock cap), close, exit nonzero.
    Expired,
    /// Already captured; the close command is on its way.
    Done,
}

/// Scripted state for one `--screenshot` run.
pub struct ScreenshotDriver {
    /// Owns the throwaway catalog/prefs dir until exit.
    _tmp: tempfile::TempDir,
    /// Where the throwaway catalog lives (the `script_dir` in `lib.rs`).
    pub lbdata: PathBuf,
    /// Where the PNG goes.
    path: PathBuf,
    settle_frames: u64,
    phase: Phase,
    started: Instant,
}

impl ScreenshotDriver {
    /// Stages the throwaway catalog dir this run's session will use.
    pub fn new(options: &ScreenshotOptions) -> std::io::Result<ScreenshotDriver> {
        let tmp = tempfile::TempDir::with_prefix("lightbox-screenshot-")?;
        Ok(ScreenshotDriver {
            lbdata: tmp.path().join("screenshot.lbdata"),
            _tmp: tmp,
            path: options.path.clone(),
            settle_frames: options.settle_frames.max(1),
            phase: Phase::Settling,
            started: Instant::now(),
        })
    }

    /// Where this run will write its PNG.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Advances the handshake for a frame that painted `frames` total, with
    /// `pipeline_busy` reporting whether anything (decode, thumbnail, canvas
    /// render, background activity) is still in flight.
    pub fn step(&mut self, frames: u64, pipeline_busy: bool) -> Step {
        match self.phase {
            Phase::Settling => {
                let settled = frames >= self.settle_frames && !pipeline_busy;
                let impatient = frames >= self.settle_frames.saturating_mul(PATIENCE_MULTIPLIER)
                    || self.started.elapsed() > PATIENCE_TIME;
                if settled || impatient {
                    self.phase = Phase::AwaitingReply {
                        requested_at: frames,
                    };
                    Step::Request
                } else {
                    Step::Settle
                }
            }
            Phase::AwaitingReply { requested_at } => {
                if frames.saturating_sub(requested_at) > REPLY_CAP_FRAMES
                    || self.started.elapsed() > HARD_TIME_CAP
                {
                    self.phase = Phase::Finished;
                    Step::Expired
                } else {
                    Step::Poll
                }
            }
            Phase::Finished => Step::Done,
        }
    }

    /// Writes the captured viewport to [`Self::path`] and closes the run out.
    ///
    /// Returns the PNG's pixel dimensions (device pixels, so 2x the logical
    /// window on a HiDPI display). Called exactly once, the phase moves to
    /// `Finished` whether the write succeeded or not, so a failed write is
    /// reported once and never retried against the same broken path.
    pub fn finish(&mut self, image: &egui::ColorImage) -> std::io::Result<[usize; 2]> {
        self.phase = Phase::Finished;
        write_png(&self.path, image)?;
        Ok(image.size)
    }
}

/// Writes an 8-bit sRGB PNG of `image` to `path`, creating parent dirs.
pub fn write_png(path: &Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    if w == 0 || h == 0 || image.pixels.len() != w * h {
        return Err(std::io::Error::other(format!(
            "captured a {w}x{h} viewport with {} pixels — nothing writable",
            image.pixels.len()
        )));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let rgb = to_rgb8(image);
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // Tag it: the capture is already in the surface's sRGB encoding, and a
    // reviewer's viewer should not re-interpret it as linear or as Display-P3.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(&rgb)
        .map_err(std::io::Error::other)?;
    writer.finish().map_err(std::io::Error::other)?;
    Ok(())
}

/// Flattens a captured [`egui::ColorImage`] to tightly packed 8-bit sRGB RGB
/// rows, top to bottom, the layout `png::Encoder` wants.
///
/// Alpha is un-premultiplied and then dropped, deliberately. What we captured
/// is a composited *window* surface, but eframe's default `clear_color` is
/// `rgba(12, 12, 12, 180)`, deliberately translucent so a `transparent()`
/// window would "just work". Keeping that alpha would make every PNG viewer
/// re-composite the capture over its own white or checkerboard and report the
/// theme's charcoals as washed out. Un-multiplying first recovers the colour
/// the compositor was actually told to show; dropping alpha then pins it.
fn to_rgb8(image: &egui::ColorImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.pixels.len() * 3);
    for px in &image.pixels {
        let [r, g, b, _] = px.to_srgba_unmultiplied();
        out.extend_from_slice(&[r, g, b]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    #[test]
    fn size_parsing_accepts_wxh_and_rejects_nonsense() {
        assert_eq!(parse_size("1600x1000"), Some([1600.0, 1000.0]));
        assert_eq!(parse_size("1600X1000"), Some([1600.0, 1000.0]));
        assert_eq!(parse_size(" 1100 x 720 "), Some([1100.0, 720.0]));
        assert_eq!(parse_size("1100"), None, "no separator");
        assert_eq!(parse_size("1100x"), None, "no height");
        assert_eq!(parse_size("axb"), None, "not numeric");
        assert_eq!(parse_size("0x720"), None, "zero width");
        assert_eq!(parse_size("-1100x720"), None, "negative");
        assert_eq!(parse_size("99999x720"), None, "past the sanity ceiling");
    }

    /// The handshake: settle → request once → poll → done, and never a second
    /// `Request` (a duplicate would queue a second capture we never read).
    #[test]
    fn step_settles_then_requests_exactly_once() {
        let mut d = ScreenshotDriver::new(&ScreenshotOptions {
            path: PathBuf::from("/dev/null/unused.png"),
            settle_frames: 10,
            size: None,
        })
        .unwrap();

        assert_eq!(d.step(1, false), Step::Settle);
        assert_eq!(d.step(9, false), Step::Settle, "respects the settle window");
        assert_eq!(
            d.step(10, true),
            Step::Settle,
            "a busy pipeline holds the shutter past the settle window"
        );
        assert_eq!(d.step(10, false), Step::Request);
        assert_eq!(d.step(11, false), Step::Poll, "the request goes out once");
        assert_eq!(d.step(12, false), Step::Poll);
    }

    /// A pipeline that never goes quiet still yields a picture: past
    /// `PATIENCE_MULTIPLIER` settle windows the shutter fires anyway.
    #[test]
    fn step_stops_waiting_for_a_pipeline_that_never_quiets() {
        let mut d = ScreenshotDriver::new(&ScreenshotOptions {
            path: PathBuf::from("/dev/null/unused.png"),
            settle_frames: 10,
            size: None,
        })
        .unwrap();
        assert_eq!(d.step(59, true), Step::Settle);
        assert_eq!(d.step(60, true), Step::Request, "10 * PATIENCE_MULTIPLIER");
    }

    /// No reply within the frame cap is a failure, not a hang, and the
    /// driver stays `Done` afterwards so the pump stops re-closing.
    #[test]
    fn step_expires_when_the_reply_never_arrives() {
        let mut d = ScreenshotDriver::new(&ScreenshotOptions {
            path: PathBuf::from("/dev/null/unused.png"),
            settle_frames: 10,
            size: None,
        })
        .unwrap();
        assert_eq!(d.step(10, false), Step::Request);
        assert_eq!(d.step(10 + REPLY_CAP_FRAMES, false), Step::Poll);
        assert_eq!(d.step(11 + REPLY_CAP_FRAMES, false), Step::Expired);
        assert_eq!(d.step(12 + REPLY_CAP_FRAMES, false), Step::Done);
    }

    /// **The half a GPU cannot test for us.** The capture arrives as a
    /// premultiplied `ColorImage`; this proves the encoder round-trips the
    /// geometry and the colour (including un-premultiplying eframe's
    /// translucent clear colour) through a real PNG on disk.
    #[test]
    fn write_png_round_trips_geometry_and_unpremultiplied_colour() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A nested path proves parent dirs are created on demand.
        let path = tmp.path().join("nested/dir/shot.png");
        let image = egui::ColorImage::new(
            [2, 2],
            vec![
                Color32::from_rgb(255, 0, 0),
                Color32::from_rgb(0, 255, 0),
                Color32::from_rgb(0, 0, 255),
                // eframe's default clear colour. `Color32` stores it
                // PREMULTIPLIED (12 * 180/255 → 8), so un-multiplying gets
                // back 11, not 12, one LSB lost to 8-bit premultiplied
                // storage, long before we see the pixel. Asserted exactly so
                // the round-trip stays honest rather than "close enough".
                Color32::from_rgba_unmultiplied(12, 12, 12, 180),
            ],
        );

        let mut driver = ScreenshotDriver::new(&ScreenshotOptions::new(path.clone())).unwrap();
        assert_eq!(driver.finish(&image).unwrap(), [2, 2]);
        assert_eq!(
            driver.step(1, false),
            Step::Done,
            "finish closes the handshake out"
        );

        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(
            &buf[..info.buffer_size()],
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 11, 11, 11],
            "row-major RGB with alpha un-premultiplied and dropped"
        );
    }

    /// A zero-sized capture is refused with a message rather than writing a
    /// 0-byte PNG that looks like a successful screenshot.
    #[test]
    fn write_png_refuses_an_empty_capture() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("empty.png");
        let err = write_png(&path, &egui::ColorImage::new([0, 0], Vec::new())).unwrap_err();
        assert!(err.to_string().contains("nothing writable"), "{err}");
        assert!(!path.exists(), "no file is left behind");
    }
}
