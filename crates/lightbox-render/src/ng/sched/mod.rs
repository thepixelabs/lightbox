// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The render scheduler — the coalescing front-end (spec §3.7/§4.3/§5.1).
//!
//! Owner: **A-gpu** owns the canvas double-buffer + [`CanvasFrame`] publisher
//! (task A13); **B** owns latest-wins coalescing (one in-flight + one pending
//! slot per image, task B6). Consumed by the shell (E08) via [`RenderScheduler::canvas`].

pub mod canvas;

use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::JobSystem;
use lightbox_types::{ImageId, ProcessVersion};
use tokio::sync::watch;

use crate::ng::engine::{Engine, OutputQuality};
use crate::ng::types::{Extent, Roi};

/// A zoom factor (1.0 = 1:1). Part of [`ViewState`] (spec §3.7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom(pub f32);

/// The shell's current view of an image (spec §3.7).
#[derive(Clone, Copy, Debug)]
pub struct ViewState {
    /// The viewport extent, pixels.
    pub viewport: Extent,
    /// The current zoom factor.
    pub zoom: Zoom,
    /// The panned/visible region, source-pixel coordinates.
    pub pan: Roi,
}

/// A handle to the E06 jobs system, for registering Interactive-class render
/// work (spec §3.7). **B/F wire the real registration.**
pub struct JobsHandle(pub Arc<JobSystem>);

/// The completed-frame currency the shell subscribes to (spec §3.7). The shell
/// samples only completed `generation`s (double-buffer).
#[derive(Clone, Debug)]
pub struct CanvasFrame {
    /// Monotonic generation; the shell samples only completed ones.
    pub generation: u64,
    /// The output texture view — same-device, zero-copy (§2.3 seam 2).
    pub texture: wgpu::TextureView,
    /// Drives the "Loading"/preview badge (§4.3).
    pub quality: OutputQuality,
    /// Output extent, pixels.
    pub extent: Extent,
}

/// Owns latest-wins slots per image (one in-flight + one pending) and publishes
/// completed frames on a watch channel (spec §3.7). **A13/B6 fill the
/// internals.**
pub struct RenderScheduler {}

impl RenderScheduler {
    /// A scheduler driving `engine`, registering work through `jobs`.
    pub fn new(engine: Arc<Engine>, jobs: JobsHandle) -> RenderScheduler {
        let _ = (engine, jobs);
        unimplemented!("B6 (B): RenderScheduler::new — latest-wins slots + canvas publisher")
    }

    /// Set the recipe for `image` (latest-wins debounce; task B6).
    pub fn set_recipe(&self, image: ImageId, recipe: Recipe, pv: ProcessVersion) {
        let _ = (image, recipe, pv);
        unimplemented!("B6 (B): RenderScheduler::set_recipe — latest-wins coalescing")
    }

    /// Set the view for `image` (roi/zoom churn; task B6/C4).
    pub fn set_view(&self, image: ImageId, view: ViewState) {
        let _ = (image, view);
        unimplemented!("B6 (B): RenderScheduler::set_view")
    }

    /// Subscribe to completed canvas frames (spec §3.7; task A13).
    pub fn canvas(&self) -> watch::Receiver<CanvasFrame> {
        unimplemented!("A13 (A-gpu): RenderScheduler::canvas — double-buffered publisher")
    }
}
