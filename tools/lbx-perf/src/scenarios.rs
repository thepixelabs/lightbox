// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The three headless T28 scenarios, all driven through the seam-1 surface
//! (`lightbox-core`) exactly like the CLI — plus a direct `lightbox-catalog`
//! leg for the 100 k page-query numbers (the §6 budget: p95 < 100 ms).
//!
//! The grid-scroll frame-time capture is the fourth scenario; it needs a
//! window, so it lives in the shell as the scripted `lightbox --perf-scroll`
//! mode and joins this harness's output in the nightly workflow.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use lightbox_catalog::{Catalog, ImageQuery, NewAsset, PageCursor, SortOrder};
use lightbox_core::{CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, Session};
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    ActiveBackend, Extent, OutFormat, RenderPriority, RenderRequest, RenderScale, RenderState,
    RenderTarget, Roi,
};
use lightbox_render::GpuContext;
use lightbox_types::{ContentHash, FolderId, ImageId, Orientation, PV_M0};

use crate::corpus;
use crate::report::{percentile, Metrics};

const EVENT_TIMEOUT: Duration = Duration::from_secs(600);

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// import-1k
// ---------------------------------------------------------------------------

/// Import 1 000 files (fixture mix + synthetic JPEGs) through the command
/// bus; record the wall time, the time until the first image was queryable
/// (the "grid browsable during import" proxy) and the worst progress-event
/// gap (heartbeat proxy; events are throttled to ≤ 10 Hz, so ~100 ms is the
/// floor).
pub fn import_1k(fixtures: &Path, files: usize) -> anyhow::Result<Metrics> {
    let tmp = tempfile::TempDir::with_prefix("lbx-perf-import-")?;
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos)?;
    let from_fixtures = corpus::copy_fixture_mix(fixtures, &photos)?;
    let synthetic = files.saturating_sub(from_fixtures);
    corpus::stage_synthetic_jpegs(&photos, synthetic, "perf-")?;

    let core = Core::start(CoreConfig::default())?;
    let session = core.create_catalog(&tmp.path().join("perf.lbdata"), None)?;
    let mut rx = session.events();

    let t0 = Instant::now();
    session.submit(Command::ImportAddInPlace {
        source_dir: photos,
        recursive: false,
    });

    let mut first_visible: Option<f64> = None;
    let mut last_visibility_probe = Instant::now();
    let mut last_progress: Option<Instant> = None;
    let mut max_gap = Duration::ZERO;
    let report = loop {
        if t0.elapsed() > EVENT_TIMEOUT {
            bail!("import-1k: import never finished");
        }
        match rx.try_recv() {
            Ok(Event::ImportProgress { .. }) => {
                let now = Instant::now();
                if let Some(prev) = last_progress.replace(now) {
                    max_gap = max_gap.max(now.duration_since(prev));
                }
            }
            Ok(Event::ImportFinished { report, .. }) => break report,
            Ok(Event::CommandFailed { error, .. }) => bail!("import-1k failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => bail!("import-1k event stream broke: {e}"),
        }
        // "Browsable during import": how soon does a reader see the first
        // row? Probed off the event stream at ≤ 50 Hz, same WAL-snapshot
        // query the grid model runs.
        if first_visible.is_none() && last_visibility_probe.elapsed() > Duration::from_millis(20) {
            last_visibility_probe = Instant::now();
            let page = session.query().images_page(&ImageQuery {
                folder: None,
                sort: SortOrder::AddedAsc,
                cursor: None,
                limit: 1,
            })?;
            if !page.items.is_empty() {
                first_visible = Some(ms(t0.elapsed()));
            }
        }
    };
    let wall = ms(t0.elapsed());

    let mut m = Metrics::new();
    m.insert("wall_ms".into(), wall);
    m.insert(
        "first_image_visible_ms".into(),
        first_visible.unwrap_or(wall),
    );
    m.insert("progress_max_gap_ms".into(), ms(max_gap));
    m.insert("files".into(), files as f64);
    m.insert("imported".into(), report.imported as f64);
    m.insert("errors".into(), report.errors.len() as f64);

    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok(m)
}

// ---------------------------------------------------------------------------
// page-query @ 100 k
// ---------------------------------------------------------------------------

fn synth_asset(folder: FolderId, i: u64) -> NewAsset {
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&i.to_le_bytes());
    hash[8] = 0x9F;
    let capture_time = (!i.is_multiple_of(10)).then(|| {
        let secs = i % 100_000;
        format!(
            "2026-06-{:02}T{:02}:{:02}:{:02}.000000Z",
            1 + secs / 86_400,
            (secs / 3_600) % 24,
            (secs / 60) % 60,
            secs % 60
        )
    });
    NewAsset {
        folder,
        filename: format!("IMG_{i:06}.CR3"),
        content_hash: ContentHash(hash),
        format: "CR3".to_owned(),
        camera_make: Some("Canon".to_owned()),
        camera_model: Some("EOS R5".to_owned()),
        capture_time,
        width: 8192,
        height: 5464,
        orientation: Orientation::O1,
        bytes: 45_000_000,
        mtime_utc: None,
        decode_error: None,
        import_session: None,
    }
}

/// Walks pages of 1 000 to the middle of the result set (deep cursor).
fn mid_cursor(catalog: &Catalog, sort: SortOrder, total: u64) -> Option<PageCursor> {
    let reader = catalog.reader();
    let mut cursor = None;
    for _ in 0..(total / 2 / 1000) {
        let page = reader
            .images_page(&ImageQuery {
                folder: None,
                sort,
                cursor,
                limit: 1000,
            })
            .expect("page");
        cursor = page.next;
        cursor.as_ref()?;
    }
    cursor
}

/// 100 k synthetic assets, then 200-row keyset page queries across every
/// sort order, first and deep cursors — p50/p95/p99 against the < 100 ms
/// §6 budget.
pub fn page_query_100k(total: u64) -> anyhow::Result<Metrics> {
    let tmp = tempfile::TempDir::with_prefix("lbx-perf-pages-")?;
    let catalog = Catalog::create(&tmp.path().join("pages.lbdata"))?;
    let photos = tmp.path().join("photos");
    let folder = catalog.writer().with_txn(move |txn| {
        let root = txn.upsert_root(None, &photos)?;
        txn.upsert_folder(root, None, "synthetic")
    })?;

    let t0 = Instant::now();
    for base in (0..total).step_by(1000) {
        catalog.writer().with_txn(move |txn| {
            let batch: Vec<NewAsset> = (base..(base + 1000).min(total))
                .map(|i| synth_asset(folder, i))
                .collect();
            let outcome = txn.insert_assets(&batch)?;
            txn.insert_default_images(&outcome.inserted)?;
            Ok(())
        })?;
    }
    let insert_wall = ms(t0.elapsed());

    let reader = catalog.reader();
    let mut samples: Vec<f64> = Vec::new();
    for sort in [
        SortOrder::CaptureTimeAsc,
        SortOrder::CaptureTimeDesc,
        SortOrder::AddedAsc,
        SortOrder::FilenameAsc,
    ] {
        let deep = mid_cursor(&catalog, sort, total);
        for cursor in [None, deep] {
            for _ in 0..25 {
                let t = Instant::now();
                let page = reader.images_page(&ImageQuery {
                    folder: None,
                    sort,
                    cursor: cursor.clone(),
                    limit: 200,
                })?;
                samples.push(ms(t.elapsed()));
                anyhow::ensure!(page.items.len() == 200, "short page");
            }
        }
    }

    let mut m = Metrics::new();
    m.insert("rows".into(), total as f64);
    m.insert("insert_wall_ms".into(), insert_wall);
    m.insert("queries".into(), samples.len() as f64);
    m.insert("p50_ms".into(), percentile(&samples, 0.5));
    m.insert("p95_ms".into(), percentile(&samples, 0.95));
    m.insert("p99_ms".into(), percentile(&samples, 0.99));
    Ok(m)
}

// ---------------------------------------------------------------------------
// nav-swap (loupe next/prev)
// ---------------------------------------------------------------------------

fn wait_terminal(session: &Session, ticket: &lightbox_render::ng::RenderTicket) -> RenderState {
    let engine = session.engine();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match engine.poll(ticket) {
            RenderState::Queued | RenderState::Rendering { .. } => {
                if Instant::now() > deadline {
                    return RenderState::Failed(lightbox_render::ng::RenderError::Internal(
                        "nav-swap: render never became terminal".into(),
                    ));
                }
                std::thread::sleep(Duration::from_micros(200));
            }
            terminal => return terminal,
        }
    }
}

fn nav_request(image: ImageId) -> RenderRequest {
    RenderRequest {
        image,
        recipe: Recipe::identity(PV_M0),
        pv: PV_M0,
        roi: Roi {
            x: 0,
            y: 0,
            w: 1600,
            h: 1000,
        },
        scale: RenderScale::Fit(Extent { w: 1600, h: 1000 }),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

/// Loupe next/prev latency (§7 budget: < 50 ms p95 from cache): import the
/// fixture mix, warm the preview cache once per image, then measure
/// `Engine::submit`→`Ready` per navigation step over several passes — the
/// exact ticket lifecycle the shell's loupe rides (one viewport,
/// latest-wins), minus the composite.
pub fn nav_swap(fixtures: &Path, gpu: Option<GpuContext>) -> anyhow::Result<(Metrics, String)> {
    let tmp = tempfile::TempDir::with_prefix("lbx-perf-nav-")?;
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos)?;
    if corpus::copy_fixture_mix(fixtures, &photos)? == 0 {
        // Bare checkout: fall back to synthetic JPEGs so the scenario still
        // proves the lifecycle (numbers are then trivially fast).
        corpus::stage_synthetic_jpegs(&photos, 12, "nav-")?;
    }

    let core = Core::start(CoreConfig::default())?;
    let session = core.create_catalog(&tmp.path().join("nav.lbdata"), gpu)?;
    let backend = match session.engine().active_backend() {
        ActiveBackend::Gpu(info) => format!("Gpu({:?})", info.backend),
        ActiveBackend::CpuPreviewOnly => "CpuOnly".to_owned(),
    };

    let mut rx = session.events();
    session.submit(Command::ImportAddInPlace {
        source_dir: photos,
        recursive: false,
    });
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > EVENT_TIMEOUT {
            bail!("nav-swap: import never finished");
        }
        match rx.try_recv() {
            Ok(Event::ImportFinished { .. }) => break,
            Ok(Event::CommandFailed { error, .. }) => bail!("nav-swap import failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => bail!("nav-swap event stream broke: {e}"),
        }
    }

    let page = session.query().images_page(&ImageQuery {
        folder: None,
        sort: SortOrder::FilenameAsc,
        cursor: None,
        limit: 100,
    })?;

    // Warm pass: prime the embedded-preview LRU; keep only images that
    // actually render (previewless raws — the Sigma fp DNG — and PNG/TIFF
    // originals fail with a Source error at M0, by design).
    let mut usable: Vec<ImageId> = Vec::new();
    for (i, summary) in page.items.iter().enumerate() {
        let ticket = session.engine().submit(nav_request(summary.id));
        match wait_terminal(&session, &ticket) {
            RenderState::Complete(_) | RenderState::PreviewReady(_) => {
                usable.push(summary.id);
            }
            RenderState::Cancelled => {
                bail!("nav-swap: warm render {i} cancelled — the scenario submits sequentially")
            }
            _ => {}
        }
    }
    anyhow::ensure!(
        !usable.is_empty(),
        "nav-swap: no renderable images in the corpus"
    );

    // Measured passes: one viewport, sequential nav over the warm set.
    let mut samples: Vec<f64> = Vec::new();
    for _pass in 0..4 {
        for id in &usable {
            let t = Instant::now();
            let ticket = session.engine().submit(nav_request(*id));
            match wait_terminal(&session, &ticket) {
                RenderState::Complete(_) | RenderState::PreviewReady(_) => {
                    samples.push(ms(t.elapsed()))
                }
                other => bail!("nav-swap: warm render failed: {other:?}"),
            }
        }
    }

    let mut m = Metrics::new();
    m.insert("images".into(), usable.len() as f64);
    m.insert("steps".into(), samples.len() as f64);
    m.insert("p50_ms".into(), percentile(&samples, 0.5));
    m.insert("p95_ms".into(), percentile(&samples, 0.95));
    m.insert("max_ms".into(), samples.iter().copied().fold(0.0, f64::max));

    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok((m, backend))
}

/// Resolves the fixtures dir: `--fixtures` override, else `./fixtures`
/// under the workspace root (where the nightly checkout runs from).
pub fn fixtures_dir(explicit: Option<&Path>) -> std::path::PathBuf {
    match explicit {
        Some(p) => p.to_path_buf(),
        None => Path::new("fixtures").to_path_buf(),
    }
}

/// Context wrapper so scenario failures name themselves in the output.
pub fn run_named<T>(name: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    f().with_context(|| format!("scenario {name} failed"))
}
