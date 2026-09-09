// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`ExportError`], the per-item failure taxonomy (spec §5.2 `ExportError`,
//! narrowed to the core-slice's failure surface).
//!
//! Every variant is something [`crate::run::export_one`] can actually
//! produce; batch failure isolation (spec §4.3) relies on this being
//! `Send + 'static` and cheaply stringifiable so one bad item never aborts
//! [`crate::run::export_batch`].

use std::path::PathBuf;

/// Why exporting one image failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The settings document itself was invalid (should have been caught by
    /// [`crate::settings::ExportSettings::validate`] before submission
    /// re-checked here as the pipeline's own belt-and-suspenders gate).
    #[error("invalid settings: {0}")]
    InvalidSettings(#[from] crate::settings::SettingsError),
    /// The render engine reported a terminal failure.
    #[error("render failed: {0}")]
    Render(String),
    /// The render did not complete within the pipeline's timeout.
    #[error("render timed out")]
    RenderTimeout,
    /// The render (or the export as a whole) was cancelled.
    #[error("export cancelled")]
    Cancelled,
    /// Resizing the rendered pixels failed.
    #[error("resize failed: {0}")]
    Resize(String),
    /// Building or applying the working→output color transform failed.
    #[error("output color transform failed: {0}")]
    Color(String),
    /// Encoding the final pixels into the target container failed.
    #[error("{format} encode failed: {msg}")]
    Encode {
        /// The format tag (`"jpeg"`/`"png"`/`"tiff"`).
        format: &'static str,
        /// The encoder's own message.
        msg: String,
    },
    /// Writing (or renaming into place) the output file failed.
    #[error("writing {path}: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// The pipeline panicked (contained per-item, mirrors
    /// `lightbox_jobs::JobError::Panicked`, spec §4.3 failure isolation:
    /// one bad image, however it fails, never aborts a batch).
    #[error("export pipeline panicked: {0}")]
    Panicked(String),
}

impl ExportError {
    /// True for [`ExportError::Cancelled`], batch drivers use this to
    /// distinguish "the user cancelled" from a genuine per-item failure in
    /// the completion report (spec §4.3: cancellation is a normal outcome,
    /// not an incident).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, ExportError::Cancelled)
    }
}
