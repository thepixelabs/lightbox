// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The per-image pipeline entry point + the batch driver (spec §5.2
//! `Exporter`, narrowed to the core slice, see the crate root doc comment
//! for the full cut list against the spec's `plan`/`run`/session-table
//! surface).
//!
//! **The E06 seam, stated plainly (task brief CONTEXT note).** The full
//! spec's §4.2 pipeline is a three-stage bounded channel running as an E06
//! `Class::Foreground` job group. E06 isn't fully built yet, so
//! [`export_batch`] bounds concurrency with a plain `tokio::sync::Semaphore`
//! together with `tokio::task::JoinSet` instead, `lightbox-core`'s
//! dispatcher still runs the whole batch as one `Class::Foreground` job
//! (mirrors `Command::ImportAddInPlace`), so the *outer* scheduling seam is
//! honored; only the *inner* per-item concurrency shape is a plain-tokio
//! stand-in for the spec's render/postprocess/encode three-lane pipeline.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{Engine, Extent, ProcessVersion};
use lightbox_types::ImageId;

use crate::error::ExportError;
use crate::settings::ExportSettings;
use crate::{encode, pixel};

/// One image to export: its recipe (already resolved by the caller, E09's
/// `EditStore::recipe_of` or the session's `edit_state`, spec §5.2's
/// `Recipe` materialization seam) plus the resolved, sanitized output path.
#[derive(Clone, Debug)]
pub struct ExportItem {
    /// The source image.
    pub image: ImageId,
    /// Its materialized recipe.
    pub recipe: Recipe,
    /// The process version to render under (`recipe.pv`, carried
    /// separately since [`lightbox_render::ng::RenderRequest`] wants both).
    pub pv: ProcessVersion,
    /// The source's full-resolution extent (from the catalog's
    /// `ImageDetail`), used to derive the render's fit scale.
    pub full_extent: Extent,
    /// The final, caller-resolved destination path (spec §5.2
    /// `PlannedItem::out_path`, narrowed: the core slice has no
    /// planner/collision-policy stage, see the crate root doc comment; the
    /// caller is responsible for collisions/sanitization).
    pub out_path: PathBuf,
}

/// Runs the full per-image pipeline synchronously: render → resize →
/// output color transform → basic sharpen → quantize → encode → atomic
/// write (spec §4.1, narrowed, see the crate root doc comment). Blocking;
/// callers run this on a blocking pool ([`export_batch`] does).
pub fn export_one(
    engine: &Engine,
    item: &ExportItem,
    settings: &ExportSettings,
    cancel: &CancelToken,
) -> Result<(), ExportError> {
    settings.validate()?;
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }

    // [1] render (approximately at target size).
    let (rgba, rw, rh) = pixel::render_export_pixels(
        engine,
        item.image,
        item.recipe.clone(),
        item.pv,
        item.full_extent,
        settings.sizing.rule,
        cancel.clone(),
    )?;
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }

    // [2] resize to the exact target (Lanczos3, "good filter").
    let (rgb8, w, h) = pixel::resize(&rgba, rw, rh, settings.sizing.rule)?;

    // [3] output color transform + ICC bytes.
    let (mut rgb_f32, icc) = pixel::apply_output_color(&rgb8, settings.color.space)?;

    // [4] basic output sharpening (post-transform, at final resolution
    // spec §4.1 ordering rationale).
    if let Some(sharpen) = settings.sharpen {
        pixel::sharpen_in_place(&mut rgb_f32, w, h, sharpen.amount);
    }
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }

    // [5] quantize + [6] encode.
    let quantized = pixel::quantize(&rgb_f32, settings.format.depth());
    let bytes = encode::encode(w, h, &quantized, settings.format, &icc)?;

    // [7] atomic write (temp-then-rename in the destination dir; spec §4.1
    // stage 8, narrowed: no fsync/crash-recovery sweep at core-slice scope).
    write_atomic(&item.out_path, &bytes)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ExportError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| ExportError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let mut tmp_name = path
        .file_name()
        .map(std::ffi::OsStr::to_owned)
        .unwrap_or_else(|| std::ffi::OsString::from("export.out"));
    tmp_name.push(".lbtmp");
    let tmp_path = path.with_file_name(tmp_name);

    std::fs::write(&tmp_path, bytes).map_err(|source| ExportError::Io {
        path: tmp_path.clone(),
        source,
    })?;
    std::fs::rename(&tmp_path, path).map_err(|source| ExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// One item's completion, as reported by [`export_batch`] to its
/// `on_item` callback, the seam `lightbox-core`'s dispatcher translates
/// into `Event::ExportProgress`/`Event::ExportItemFailed`.
#[derive(Clone, Debug)]
pub struct ExportItemDone {
    /// The image this item was for.
    pub image: ImageId,
    /// Its destination path.
    pub out_path: PathBuf,
    /// `None` on success; the stringified failure otherwise.
    pub error: Option<String>,
    /// Items completed so far (including this one).
    pub done: u32,
    /// Total items in the batch.
    pub total: u32,
}

/// The batch completion report (spec §5.2 `ExportReport`, narrowed: no
/// `skipped`/`session_ids`, the core slice has no planner/session table).
#[derive(Clone, Debug, Default)]
pub struct ExportReport {
    /// Items that exported successfully.
    pub ok: u32,
    /// Items that failed, with their image id and stringified error.
    pub failed: Vec<(ImageId, String)>,
}

impl ExportReport {
    /// Total items this report covers.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.ok + self.failed.len() as u32
    }
}

/// Runs `items` with bounded concurrency (spec §4.2, narrowed, see the
/// module doc comment), one call to [`export_one`] per item on the blocking
/// pool. Failure isolation (spec §4.3): a panicking or erroring item is
/// caught and recorded, never aborting the batch. `on_item` fires after
/// every item (success or failure), `lightbox-core` folds it into
/// `Event::ExportProgress` (spec §5.10).
pub async fn export_batch(
    engine: Arc<Engine>,
    items: Vec<ExportItem>,
    settings: ExportSettings,
    concurrency: usize,
    cancel: CancelToken,
    on_item: Arc<dyn Fn(ExportItemDone) + Send + Sync>,
) -> ExportReport {
    let total = items.len() as u32;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let settings = Arc::new(settings);
    let done_counter = Arc::new(AtomicU32::new(0));
    let mut set = tokio::task::JoinSet::new();

    for item in items {
        // Bound how many items are in flight at once (the E06 render/
        // postprocess/encode lane widths, narrowed to one knob, module doc
        // comment). Acquiring before spawning (rather than inside the
        // blocking closure) means a cancelled batch stops *launching* new
        // work immediately instead of piling up queued permits.
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break; // semaphore closed, batch is shutting down
        };
        if cancel.is_cancelled() {
            drop(permit);
            break;
        }

        let engine = Arc::clone(&engine);
        let settings = Arc::clone(&settings);
        let item_cancel = cancel.clone();
        let on_item = Arc::clone(&on_item);
        let done_counter = Arc::clone(&done_counter);

        set.spawn_blocking(move || {
            let _permit = permit; // held until this closure returns
            let image = item.image;
            let out_path = item.out_path.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                export_one(&engine, &item, &settings, &item_cancel)
            }))
            .unwrap_or_else(|panic| Err(ExportError::Panicked(panic_message(&panic))));

            let done = done_counter.fetch_add(1, Ordering::SeqCst) + 1;
            let error = result.as_ref().err().map(std::string::ToString::to_string);
            on_item(ExportItemDone {
                image,
                out_path,
                error,
                done,
                total,
            });
            (image, result)
        });
    }

    let mut report = ExportReport::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_, Ok(()))) => report.ok += 1,
            Ok((image, Err(e))) => report.failed.push((image, e.to_string())),
            // The closure itself contains its own catch_unwind, so a bare
            // JoinError here means the task was aborted/the runtime is
            // shutting down, not a pipeline panic (already handled above).
            Err(join_err) => {
                tracing::warn!(target: "lightbox_export", %join_err, "export task join failed");
            }
        }
    }
    report
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_leaves_no_tmp_file_and_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("out.bin");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(
            !dir.path().join("sub").join("out.bin.lbtmp").exists(),
            "temp file must be renamed away"
        );
    }

    #[test]
    fn write_atomic_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.bin");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn export_report_total_sums_ok_and_failed() {
        let report = ExportReport {
            ok: 3,
            failed: vec![(ImageId(1), "boom".to_owned())],
        };
        assert_eq!(report.total(), 4);
    }
}
