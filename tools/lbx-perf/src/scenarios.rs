// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The three headless T28 scenarios, all driven through the seam-1 surface
//! (`lightbox-core`) exactly like the CLI, plus a direct `lightbox-catalog`
//! leg for the 100 k page-query numbers (the §6 budget: p95 < 100 ms).
//!
//! The filmstrip frame-time capture (E08 H2) is the windowed scenario: it
//! lives in the shell as the scripted `lightbox --perf-strip` mode and is
//! wrapped here by [`perf_strip`] (explicit `--scenario perf-strip` only
//! never part of `all`, which must stay display-free).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use lightbox_catalog::{Catalog, ImageQuery, NewAsset, PageCursor, SortOrder};
use lightbox_core::{
    CloseOpts, ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, OpenOrigin, OpenRequest,
    ParamDelta, ParamId, ParamValue, Session, StepLabel,
};
use lightbox_edit::Recipe;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::{
    ActiveBackend, Coalescer, Extent, OutFormat, RenderPriority, RenderRequest, RenderScale,
    RenderState, RenderTarget, RenderTicket, Roi,
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
// open-1k (E04 spec §7/T11)
// ---------------------------------------------------------------------------

/// Drains `session`'s events for one `Command::OpenWorkingSet` submission
/// (`t0` already running), returning `(plan_ms, wall_ms, report)`: the time
/// to `Event::WorkingSetReplaced` (phase 1, "the ordered filmstrip can
/// render") and to `Event::WorkingSetLoadFinished` (phase 2 complete).
fn drain_open(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    t0: Instant,
) -> anyhow::Result<(f64, f64, lightbox_core::OpenReport)> {
    let mut plan_ms: Option<f64> = None;
    loop {
        if t0.elapsed() > EVENT_TIMEOUT {
            bail!("open: the working-set load never finished");
        }
        match rx.try_recv() {
            Ok(Event::WorkingSetReplaced { .. }) => {
                plan_ms.get_or_insert_with(|| ms(t0.elapsed()));
            }
            Ok(Event::WorkingSetLoadFinished { report, .. }) => {
                let wall_ms = ms(t0.elapsed());
                return Ok((plan_ms.unwrap_or(wall_ms), wall_ms, report));
            }
            Ok(Event::CommandFailed { error, .. }) => bail!("open failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_micros(200));
            }
            Err(e) => bail!("open event stream broke: {e}"),
        }
    }
}

/// E04 spec §7 budgets, measured directly against `Command::OpenWorkingSet`
/// (not the retired-dormant `ImportAddInPlace`):
///
/// - **single-raw-open**: one raw fixture, alone, into a fresh store, the
///   direct proxy for "single raw dropped -> item 0 `Ready` < 50 ms p95"
///   (with exactly one item, item-0-ready and load-finished are the same
///   event, so this is measured precisely rather than sampled off the
///   throttled `WorkingSetChanged` heartbeat).
/// - **plan_ms**/**wall_ms**: a `files`-file folder drop, "ordered
///   filmstrip (phase 1) < 1.5 s" and "all items `Ready`" respectively.
pub fn open_1k(fixtures: &Path, files: usize) -> anyhow::Result<Metrics> {
    let tmp = tempfile::TempDir::with_prefix("lbx-perf-open-")?;

    // --- single-raw-open: the §7 "item 0 Ready" budget, measured directly ---
    let single_ms = {
        let raw = fixtures.join("canon-eos-r6.cr3");
        let solo_dir = tmp.path().join("solo");
        std::fs::create_dir_all(&solo_dir)?;
        let path = if raw.is_file() {
            let dest = solo_dir.join("canon-eos-r6.cr3");
            std::fs::copy(&raw, &dest)?;
            dest
        } else {
            // Bare checkout: fall back to a synthetic JPEG so the scenario
            // still proves the lifecycle (numbers are then trivially fast).
            corpus::stage_synthetic_jpegs(&solo_dir, 1, "solo-")?;
            solo_dir.join("solo-000000.jpg")
        };
        let core = Core::start(CoreConfig::default())?;
        let session = core.create_catalog(&tmp.path().join("solo.lbdata"), None)?;
        let mut rx = session.events();
        let t0 = Instant::now();
        session.submit(Command::OpenWorkingSet {
            request: OpenRequest::new(vec![path], false, OpenOrigin::Cli),
        });
        let (_plan, wall, report) = drain_open(&mut rx, t0)?;
        anyhow::ensure!(
            report.ready == 1,
            "solo open: expected 1 ready, got {report:?}"
        );
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        wall
    };

    // --- folder drop: phase-1 (plan) and phase-2 (all ready) timing ---
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos)?;
    let from_fixtures = corpus::copy_fixture_mix(fixtures, &photos)?;
    let synthetic = files.saturating_sub(from_fixtures);
    corpus::stage_synthetic_jpegs(&photos, synthetic, "openperf-")?;

    let core = Core::start(CoreConfig::default())?;
    let session = core.create_catalog(&tmp.path().join("open.lbdata"), None)?;
    let mut rx = session.events();
    let t0 = Instant::now();
    session.submit(Command::OpenWorkingSet {
        request: OpenRequest::new(vec![photos], false, OpenOrigin::Cli),
    });
    let (plan_ms, wall_ms, report) = drain_open(&mut rx, t0)?;

    let mut m = Metrics::new();
    m.insert("single_raw_open_ms".into(), single_ms);
    m.insert("plan_ms".into(), plan_ms);
    m.insert("wall_ms".into(), wall_ms);
    m.insert("files".into(), files as f64);
    m.insert("ready".into(), report.ready as f64);
    m.insert("failed".into(), report.failed as f64);
    m.insert("collapsed".into(), report.collapsed as f64);

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
/// sort order, first and deep cursors, p50/p95/p99 against the < 100 ms
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
/// `Engine::submit`→`Ready` per navigation step over several passes, the
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
    // actually render (previewless raws, the Sigma fp DNG, and PNG/TIFF
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

// ---------------------------------------------------------------------------
// develop-slider (E10 A14, basic-panel slider-to-screen latency)
// ---------------------------------------------------------------------------

fn exposure_delta(v: f32) -> ParamDelta {
    let mut d = ParamDelta::new();
    d.0.insert(ParamId::Exposure, ParamValue::F32(v));
    d
}

fn develop_request(image: ImageId, viewport: Extent, recipe: Recipe) -> RenderRequest {
    RenderRequest {
        image,
        pv: recipe.pv,
        recipe,
        roi: Roi {
            x: 0,
            y: 0,
            w: viewport.w,
            h: viewport.h,
        },
        scale: RenderScale::Fit(viewport),
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

/// E10 A14: exposure-slider-drag latency, driven through the REAL
/// command/session/engine path, `EditHub::begin_gesture`/`update_gesture`
/// (D2, no bus round-trip) feeding `Engine::submit` through the same
/// `Coalescer` latest-wins state machine `RenderScheduler` drives
/// internally (task B6), with the real `global.exposure` node included
/// (`Session` registers E10's Phase-A global nodes, see
/// `E10-deviations.md` A-5). Ends with exactly one `CommitGesture`, the
/// A13 discipline, exercised here under perf load too. Mirrors
/// `lightbox_render_testkit::scenario::run_slider_latency`'s own design
/// note on why driving the coalescer directly against `Engine::submit`/
/// `poll` (not a raw `RenderScheduler` call) is what yields a per-render
/// submit→complete timestamp, the "slider-to-screen" quantity the §7
/// budget is about.
pub fn develop_slider(
    fixtures: &Path,
    events: u32,
    gpu: Option<GpuContext>,
) -> anyhow::Result<(Metrics, String)> {
    let tmp = tempfile::TempDir::with_prefix("lbx-perf-slider-")?;
    let photos = tmp.path().join("photos");
    std::fs::create_dir_all(&photos)?;
    if corpus::copy_fixture_mix(fixtures, &photos)? == 0 {
        // Bare checkout: fall back to a synthetic JPEG so the scenario still
        // proves the lifecycle (numbers are then trivially fast).
        corpus::stage_synthetic_jpegs(&photos, 1, "slider-")?;
    }

    let core = Core::start(CoreConfig::default())?;
    let session = core.create_catalog(&tmp.path().join("slider.lbdata"), gpu)?;
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
            bail!("develop-slider: import never finished");
        }
        match rx.try_recv() {
            Ok(Event::ImportFinished { .. }) => break,
            Ok(Event::CommandFailed { error, .. }) => {
                bail!("develop-slider import failed: {error}")
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => bail!("develop-slider event stream broke: {e}"),
        }
    }

    let page = session.query().images_page(&ImageQuery {
        folder: None,
        sort: SortOrder::FilenameAsc,
        cursor: None,
        limit: 1,
    })?;
    let image = page
        .items
        .first()
        .ok_or_else(|| anyhow::anyhow!("develop-slider: no image imported"))?
        .id;

    let hub = session.edits();
    hub.open(image)?;
    hub.begin_gesture(image, StepLabel::Param(ParamId::Exposure))?;

    let engine = session.engine();
    let viewport = Extent { w: 1600, h: 1000 };

    // Untimed warm-up: forces first-ever pipeline/shader compilation for
    // this graph shape (mirrors `nav_swap`'s own "warm pass" convention) so
    // the timed burst below measures actual interactive drag latency, not
    // one-time cold-compile cost.
    hub.update_gesture(image, exposure_delta(-2.0))?;
    let warm_recipe = (*hub
        .working_recipe(image)
        .ok_or_else(|| anyhow::anyhow!("develop-slider: image has no working recipe"))?)
    .clone();
    let warm_ticket = engine.submit(develop_request(image, viewport, warm_recipe));
    let warm_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match engine.poll(&warm_ticket) {
            RenderState::Complete(_) | RenderState::PreviewReady(_) => break,
            RenderState::Failed(e) => bail!("develop-slider: warm-up render failed: {e}"),
            RenderState::Cancelled => bail!("develop-slider: warm-up render cancelled"),
            _ => {
                if Instant::now() > warm_deadline {
                    bail!("develop-slider: warm-up render never completed");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    let mut coalescer: Coalescer<Recipe> = Coalescer::new();
    let mut latencies: Vec<Duration> = Vec::new();
    let mut inflight: Option<(RenderTicket, Instant)> = None;

    let drain = |engine: &lightbox_render::ng::Engine,
                 coalescer: &mut Coalescer<Recipe>,
                 inflight: &mut Option<(RenderTicket, Instant)>,
                 latencies: &mut Vec<Duration>| {
        if let Some((ticket, started)) = inflight.take() {
            match engine.poll(&ticket) {
                RenderState::Complete(_) | RenderState::PreviewReady(_) => {
                    latencies.push(started.elapsed());
                    if let Some(recipe) = coalescer.complete() {
                        let t = Instant::now();
                        let ticket = engine.submit(develop_request(image, viewport, recipe));
                        *inflight = Some((ticket, t));
                    }
                }
                RenderState::Failed(_) | RenderState::Cancelled => {
                    if let Some(recipe) = coalescer.complete() {
                        let t = Instant::now();
                        let ticket = engine.submit(develop_request(image, viewport, recipe));
                        *inflight = Some((ticket, t));
                    }
                }
                RenderState::Queued | RenderState::Rendering { .. } => {
                    *inflight = Some((ticket, started));
                }
            }
        }
    };

    // Fire events at ~120 Hz (8 ms apart, a real drag's cadence): a -2..+2 EV
    // ramp, each `update_gesture` (D2, in-memory) feeding the coalescer
    // exactly like the shell canvas's `submit_if_changed` feeds
    // `RenderScheduler::set_recipe` once per changed `recipe_rev`.
    for i in 0..events.max(2) {
        drain(&engine, &mut coalescer, &mut inflight, &mut latencies);
        let ev = -2.0 + (i as f32 / (events.max(2) - 1) as f32) * 4.0;
        hub.update_gesture(image, exposure_delta(ev))?;
        let working = hub
            .working_recipe(image)
            .ok_or_else(|| anyhow::anyhow!("develop-slider: image has no working recipe"))?;
        if let Some(recipe) = coalescer.submit((*working).clone()) {
            let t = Instant::now();
            let ticket = engine.submit(develop_request(image, viewport, recipe));
            inflight = Some((ticket, t));
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    // Drain whatever is left (in-flight + any promoted pending) to quiescence.
    let deadline = Instant::now() + Duration::from_secs(30);
    while (inflight.is_some() || coalescer.has_work()) && Instant::now() < deadline {
        drain(&engine, &mut coalescer, &mut inflight, &mut latencies);
        std::thread::sleep(Duration::from_millis(1));
    }

    // A13 discipline under perf load: the whole ramp is still ONE durable
    // history step.
    let mut rx2 = session.events();
    session.submit(Command::Edit(EditCommand::CommitGesture { image }));
    let commit_deadline = Instant::now() + Duration::from_secs(30);
    let seq = loop {
        if Instant::now() > commit_deadline {
            bail!("develop-slider: CommitGesture never completed");
        }
        match rx2.try_recv() {
            Ok(Event::EditCommitted { image: i, seq, .. }) if i == image => break seq,
            Ok(Event::CommandFailed { error, .. }) => bail!("commit failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(e) => bail!("develop-slider event stream broke: {e}"),
        }
    };

    let mut ms_samples: Vec<f64> = latencies.iter().map(|d| ms(*d)).collect();
    ms_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut m = Metrics::new();
    m.insert("events".into(), events as f64);
    m.insert("samples".into(), ms_samples.len() as f64);
    m.insert("p50_ms".into(), percentile(&ms_samples, 0.5));
    m.insert("p95_ms".into(), percentile(&ms_samples, 0.95));
    m.insert(
        "max_ms".into(),
        ms_samples.iter().copied().fold(0.0, f64::max),
    );
    m.insert("history_steps".into(), seq as f64);

    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok((m, backend))
}

// ---------------------------------------------------------------------------
// perf-strip (E08 H2, the windowed filmstrip capture, wrapped)
// ---------------------------------------------------------------------------

/// Runs the scripted `lightbox --perf-strip` capture (a REAL window on the
/// runner, the ~300-entry sawtooth filmstrip scroll + nav phase the E08
/// spec §8 H2 names) and folds its one-line JSON summary into this
/// harness's metrics/baseline machinery, so the §7 budgets (frame p95
/// < 16 ms with the rail open, nav-swap p95 < 50 ms) are asserted
/// **budget-first** exactly like every other scenario. NOT part of
/// `--scenario all` (the headless perf job has no display); the nightly
/// `strip-scroll` job selects it explicitly.
///
/// Binary resolution: `$LIGHTBOX_BIN`, else `target/release/lightbox`
/// relative to the working directory (the nightly job builds it first).
pub fn perf_strip(frames: u64) -> anyhow::Result<Metrics> {
    let bin = std::env::var_os("LIGHTBOX_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new("target/release").join(format!("lightbox{}", std::env::consts::EXE_SUFFIX))
        });
    if !bin.is_file() {
        bail!(
            "lightbox binary not found at {} — build it first \
             (`cargo build --release -p lightbox-shell`) or set LIGHTBOX_BIN",
            bin.display()
        );
    }
    let out = std::process::Command::new(&bin)
        .arg("--perf-strip")
        .arg(frames.to_string())
        .output()
        .with_context(|| format!("running {} --perf-strip", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!(
            "lightbox --perf-strip failed ({:?}):\n{}{}",
            out.status.code(),
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let line = stdout
        .lines()
        .find(|l| l.starts_with("{\"scenario\":\"perf-strip\""))
        .context("no perf-strip JSON summary line in the capture output")?;
    let v: serde_json::Value = serde_json::from_str(line).context("parsing the summary line")?;
    let mut m = Metrics::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if let Some(f) = val.as_f64() {
                m.insert(k.clone(), f);
            }
        }
    }
    if m.is_empty() {
        bail!("perf-strip summary parsed to zero numeric metrics: {line}");
    }
    Ok(m)
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
