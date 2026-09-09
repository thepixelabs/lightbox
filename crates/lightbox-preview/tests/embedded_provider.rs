// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! T20/T21 acceptance for [`EmbeddedPreviewProvider`], carried forward
//! through E03 Phase B's store-backed rewiring (T06-T09):
//!
//! - the largest embedded preview of each fixture decodes to expected dims
//!   (loupe class), and no/tiny-preview raws yield `NoEmbedded` (T20);
//! - cache hits are `Ready` on the first poll, concurrent duplicates run one
//!   decode, and a request/cancel storm leaks nothing (T21);
//! - every decode now goes through the T0 store + catalog (T06/T07) and
//!   bakes orientation unconditionally (T08, including the loupe class
//!   see `decode.rs`'s module doc comment for why that's a deliberate
//!   spec-driven change from the E01 seed this file used to cover).
//!
//! Requires the fixture corpus: `cargo xtask fixtures`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightbox_catalog::{Catalog, NewAsset};
use lightbox_jobs::{Class, JobConfig, JobSystem};
use lightbox_preview::{
    AssetLocator, EmbeddedPreviewProvider, LocatedAsset, PreviewClass, PreviewError,
    PreviewProvider, PreviewState, PreviewStoreConfig, Store,
};
use lightbox_types::{AssetId, ContentHash, ImageId, Orientation, SourceTier};

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").exists(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

/// A fresh `.lbdata`-shaped store + catalog, with one asset row already
/// inserted (T07's write path needs a real `asset` row to satisfy the
/// `preview.asset_id` foreign key), the scaffolding every test in this file
/// needs before it can drive `EmbeddedPreviewProvider` through the T06-T09
/// pipeline. Deliberately leaks its tempdir (`TempDir::keep`): these are
/// short-lived CI/dev test processes, and keeping every provider
/// constructor in this file call-compatible with a plain
/// `(Arc<Store>, Arc<Catalog>, AssetId, ContentHash)` tuple is worth the
/// tradeoff over threading a guard through every test function.
fn store_catalog_and_asset(
    seed: &str,
    filename: &str,
) -> (Arc<Store>, Arc<Catalog>, AssetId, ContentHash) {
    let dir = tempfile::TempDir::new().unwrap().keep();
    let store = Arc::new(Store::open(&PreviewStoreConfig::with_defaults(dir.clone())).unwrap());
    let catalog = Arc::new(Catalog::create(&dir.join("t.lbdata")).unwrap());
    let content_hash = ContentHash(twox_hash::XxHash3_128::oneshot(seed.as_bytes()).to_be_bytes());
    let root = catalog
        .writer()
        .with_txn(|txn| txn.upsert_root(None, std::path::Path::new("/synthetic-root")))
        .unwrap();
    let folder = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_folder(root, None, "shoot"))
        .unwrap();
    let asset = catalog
        .writer()
        .with_txn({
            let filename = filename.to_owned();
            move |txn| {
                let batch = vec![NewAsset {
                    folder,
                    filename,
                    content_hash,
                    format: "CR2".to_owned(),
                    camera_make: None,
                    camera_model: None,
                    capture_time: None,
                    width: 100,
                    height: 100,
                    orientation: Orientation::O1,
                    bytes: 10,
                    mtime_utc: None,
                    decode_error: None,
                    import_session: None,
                }];
                Ok(txn.insert_assets(&batch)?.inserted[0])
            }
        })
        .unwrap();
    (store, catalog, asset, content_hash)
}

/// Locates every image at one fixed path/asset.
struct FixedLocator {
    path: PathBuf,
    orientation: Orientation,
    asset: AssetId,
    content_hash: ContentHash,
}

impl AssetLocator for FixedLocator {
    fn locate(&self, _image: ImageId) -> Result<LocatedAsset, PreviewError> {
        Ok(LocatedAsset {
            path: self.path.clone(),
            orientation: self.orientation,
            asset: self.asset,
            content_hash: self.content_hash,
        })
    }
}

/// A locator whose `locate` blocks until released, makes request overlap
/// deterministic for the dedup test.
struct GateLocator {
    path: PathBuf,
    asset: AssetId,
    content_hash: ContentHash,
    entered: AtomicU64,
    release: AtomicBool,
}

impl AssetLocator for GateLocator {
    fn locate(&self, _image: ImageId) -> Result<LocatedAsset, PreviewError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.release.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "gate never released");
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(LocatedAsset {
            path: self.path.clone(),
            orientation: Orientation::O1,
            asset: self.asset,
            content_hash: self.content_hash,
        })
    }
}

fn provider_for(
    jobs: &Arc<JobSystem>,
    fixture: &str,
    orientation: Orientation,
    cache_bytes: u64,
) -> EmbeddedPreviewProvider {
    provider_for_path(
        jobs,
        fixture,
        &fixtures_dir().join(fixture),
        orientation,
        cache_bytes,
    )
}

/// [`provider_for`] over an arbitrary on-disk file rather than a corpus
/// fixture, for cases that need a file this test builds itself.
fn provider_for_path(
    jobs: &Arc<JobSystem>,
    seed: &str,
    path: &std::path::Path,
    orientation: Orientation,
    cache_bytes: u64,
) -> EmbeddedPreviewProvider {
    let (store, catalog, asset, content_hash) = store_catalog_and_asset(seed, seed);
    EmbeddedPreviewProvider::new(
        Arc::clone(jobs),
        Arc::new(FixedLocator {
            path: path.to_path_buf(),
            orientation,
            asset,
            content_hash,
        }),
        cache_bytes,
        store,
        catalog,
    )
}

fn wait_terminal(
    provider: &EmbeddedPreviewProvider,
    t: &lightbox_preview::PreviewTicket,
) -> PreviewState {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match provider.poll(t) {
            PreviewState::Pending => {
                assert!(Instant::now() < deadline, "preview never resolved");
                std::thread::sleep(Duration::from_millis(2));
            }
            done => return done,
        }
    }
}

/// Splices `icc` into `jpeg` as an `APP2 ICC_PROFILE` segment immediately
/// after SOI, turning a real, untagged fixture JPEG into the same file a
/// colour-managed camera or phone would have written.
fn tag_jpeg_with_icc(jpeg: &[u8], icc: &[u8]) -> Vec<u8> {
    let mut payload = b"ICC_PROFILE\0".to_vec();
    payload.push(1); // chunk 1
    payload.push(1); // of 1
    payload.extend_from_slice(icc);
    let seg_len = u16::try_from(payload.len() + 2).expect("profile fits one APP2 segment");
    let mut out = jpeg[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xFF, 0xE2]);
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// **End to end across the whole preview stack**: a Display-P3-tagged file on
/// disk arrives at [`lightbox_preview::DecodedImage`] carrying Display-P3, and
/// the byte-identical untagged original arrives as sRGB.
///
/// This is the seam the P3 bug lived at. `DecodedImage` had no colour-space
/// field at all, so *both* of these files used to reach the render engine
/// indistinguishable from each other, and a P3 photo, which is what a modern
/// iPhone writes by default, was rendered with sRGB's narrower primaries.
///
/// Driving the real [`EmbeddedPreviewProvider`] (probe → `ensure_t0` → store
/// write → decode) rather than any one link means the tag is proven to survive
/// every hop, including the catalog round trip in the middle.
#[test]
fn a_display_p3_tagged_file_reaches_decoded_image_as_display_p3() {
    let dir = tempfile::TempDir::new().unwrap();
    let original = std::fs::read(fixtures_dir().join("lightbox-tiny.jpg")).unwrap();
    let p3_icc = lightbox_color::RgbSourceSpace::DisplayP3
        .reference_profile()
        .to_icc_bytes()
        .expect("serialise Display-P3 profile");

    let untagged = dir.path().join("untagged.jpg");
    let tagged = dir.path().join("display-p3.jpg");
    std::fs::write(&untagged, &original).unwrap();
    std::fs::write(&tagged, tag_jpeg_with_icc(&original, &p3_icc)).unwrap();

    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    for (path, seed, want) in [
        (
            &untagged,
            "p3-gate-untagged",
            lightbox_preview::PreviewColorspace::Srgb,
        ),
        (
            &tagged,
            "p3-gate-tagged",
            lightbox_preview::PreviewColorspace::DisplayP3,
        ),
    ] {
        let provider = provider_for_path(&jobs, seed, path, Orientation::O1, 64 * 1024 * 1024);
        let ticket = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
        let PreviewState::Ready(img) = wait_terminal(&provider, &ticket) else {
            panic!("{} did not decode", path.display());
        };
        assert_eq!(
            img.colorspace,
            want,
            "{}: colour space lost or mis-resolved across the preview seam",
            path.display(),
        );
        assert_eq!((img.width, img.height), (16, 16));
    }
}

/// T20 AC (carried forward): the largest embedded preview of each fixture
/// decodes to the expected dimensions; raws with no/tiny embedded previews
/// yield `NoEmbedded`. T08 AC: orientation is now always baked, including
/// for the loupe class (all fixtures here are claimed at `O1`, so dims are
/// unaffected either way, the orientation-bake behavior itself is covered
/// per-orientation in `decode.rs`'s own tests).
#[test]
fn loupe_decodes_largest_embedded_preview_of_each_fixture() {
    // name → Some(largest-preview dims) or None = NoEmbedded expected.
    // Dims mirror tests/probe_expectations.toml in lightbox-decode.
    let cases: &[(&str, Option<(u32, u32)>)] = &[
        ("canon-eos-350d.cr2", Some((3456, 2304))),
        ("canon-eos-r6.cr3", Some((3408, 2272))),
        ("nikon-d4s.nef", Some((2464, 1640))),
        ("nikon-z6.nef", Some((3024, 2016))),
        ("sony-ilce7s.arw", Some((1616, 1080))),
        ("fujifilm-xt1.raf", Some((1920, 1280))),
        ("fujifilm-x100.raf", Some((2176, 1448))),
        ("lightbox-tiny.jpg", Some((16, 16))),
        ("olympus-e1.orf", None), // 160x120 thumb only: "tiny" (T20 AC)
        ("sigma-fp.dng", None),   // no JPEG rendition at all
        ("gradient-8x8.png", None),
        ("gradient-8x8.tiff", None),
    ];
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    for (fixture, expected) in cases {
        let provider = provider_for(&jobs, fixture, Orientation::O1, 512 * 1024 * 1024);
        let ticket = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
        match (wait_terminal(&provider, &ticket), expected) {
            (PreviewState::Ready(img), Some((w, h))) => {
                assert_eq!((img.width, img.height), (*w, *h), "{fixture}: loupe dims");
                assert!(
                    img.orientation_applied,
                    "{fixture}: decode-for-display always bakes orientation (T08)"
                );
                assert_eq!(img.tier, SourceTier::EmbeddedPreview, "{fixture}");
                assert_eq!(
                    img.px.len(),
                    (*w as usize) * (*h as usize) * 4,
                    "{fixture}: RGBA8 buffer size"
                );
            }
            (PreviewState::Failed(PreviewError::NoEmbedded), None) => {}
            (state, _) => panic!("{fixture}: unexpected {state:?} (expected {expected:?})"),
        }
    }
}

/// Thumbs: downscaled to the requested edge, EXIF orientation baked on the
/// CPU (dims transpose for the 90°-family), `orientation_applied = true`.
#[test]
fn thumbs_downscale_and_bake_orientation() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    // Claim O6 via the locator: the 2176x1448 X100 preview must come out
    // 256-tall (2176→256 wide, 1448→170 tall, then transposed).
    let provider = provider_for(
        &jobs,
        "fujifilm-x100.raf",
        Orientation::O6,
        64 * 1024 * 1024,
    );
    let ticket = provider.request(
        ImageId(7),
        PreviewClass::Thumb { max_px: 256 },
        Class::Background,
    );
    match wait_terminal(&provider, &ticket) {
        PreviewState::Ready(img) => {
            assert_eq!((img.width, img.height), (170, 256), "transposed thumb dims");
            assert!(img.orientation_applied);
            assert_eq!(img.px.len(), 170 * 256 * 4);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// T07 AC (T0 write path integration): a loupe request actually leaves a
/// `.t0.jpg` file under the store root.
#[test]
fn loupe_request_materializes_a_t0_file_in_the_store() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let (store, catalog, asset, content_hash) =
        store_catalog_and_asset("canon-eos-350d.cr2", "canon-eos-350d.cr2");
    let provider = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join("canon-eos-350d.cr2"),
            orientation: Orientation::O1,
            asset,
            content_hash,
        }),
        64 * 1024 * 1024,
        Arc::clone(&store),
        Arc::clone(&catalog),
    );
    let ticket = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
    assert!(matches!(
        wait_terminal(&provider, &ticket),
        PreviewState::Ready(_)
    ));

    let previews_dir = store.root().join("previews");
    let mut t0_files = Vec::new();
    collect_files(&previews_dir, &mut t0_files);
    assert_eq!(t0_files.len(), 1, "exactly one file written: {t0_files:?}");
    assert!(t0_files[0].to_string_lossy().ends_with(".t0.jpg"));

    let rows = catalog.reader().all_preview_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].asset, asset);
    assert!(rows[0].image.is_none(), "T0 is asset-scope");
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// T21 AC: a cache hit polls `Ready` on the FIRST poll.
#[test]
fn cache_hit_is_ready_on_first_poll() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let provider = provider_for(
        &jobs,
        "lightbox-tiny.jpg",
        Orientation::O1,
        64 * 1024 * 1024,
    );
    let class = PreviewClass::Thumb { max_px: 128 };

    let warm = provider.request(ImageId(3), class, Class::Background);
    assert!(matches!(
        wait_terminal(&provider, &warm),
        PreviewState::Ready(_)
    ));

    let hit = provider.request(ImageId(3), class, Class::Background);
    match provider.poll(&hit) {
        PreviewState::Ready(img) => assert_eq!((img.width, img.height), (16, 16)),
        other => panic!("cache hit polled {other:?} on first poll"),
    }
    // One decode total: the second request came straight from the cache.
    assert_eq!(provider.stats().decodes_started, 1);
}

/// T21 AC: concurrent duplicate requests for the same (image, class) share
/// one decode (probe counter == 1).
#[test]
fn concurrent_duplicates_share_one_decode() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let (store, catalog, asset, content_hash) =
        store_catalog_and_asset("lightbox-tiny.jpg#gate", "lightbox-tiny.jpg");
    let gate = Arc::new(GateLocator {
        path: fixtures_dir().join("lightbox-tiny.jpg"),
        asset,
        content_hash,
        entered: AtomicU64::new(0),
        release: AtomicBool::new(false),
    });
    let provider = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::clone(&gate) as _,
        64 * 1024 * 1024,
        store,
        catalog,
    );
    let class = PreviewClass::Thumb { max_px: 64 };

    let a = provider.request(ImageId(9), class, Class::Interactive);
    // Wait until the first decode is provably inside `locate`…
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.entered.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "decode never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    // …then pile on duplicates.
    let b = provider.request(ImageId(9), class, Class::Interactive);
    let c = provider.request(ImageId(9), class, Class::Interactive);
    assert_eq!(
        provider.stats().inflight,
        1,
        "duplicates must share the job"
    );
    gate.release.store(true, Ordering::SeqCst);

    for t in [&a, &b, &c] {
        assert!(matches!(
            wait_terminal(&provider, t),
            PreviewState::Ready(_)
        ));
    }
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1, "one locate");
    assert_eq!(provider.stats().decodes_started, 1, "one decode (T21 AC)");
}

/// T21 AC: 500 rapid requests with immediate cancels leak nothing, ticket
/// and in-flight tables empty, job counters back to zero, LRU under its cap.
#[test]
fn rapid_cancel_storm_leaks_nothing() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let cap = 8 * 1024 * 1024;
    let provider = provider_for(&jobs, "lightbox-tiny.jpg", Orientation::O1, cap);

    for i in 0..500 {
        let t = provider.request(
            ImageId(i),
            PreviewClass::Thumb { max_px: 256 },
            Class::Background,
        );
        provider.cancel(&t);
        // Cancelled tickets poll as Cancelled, and cancel is idempotent.
        match provider.poll(&t) {
            PreviewState::Failed(PreviewError::Cancelled) => {}
            other => panic!("cancelled ticket polled {other:?}"),
        }
        provider.cancel(&t);
    }

    let stats = provider.stats();
    assert_eq!(stats.tickets, 0, "ticket table must be empty");
    assert_eq!(stats.inflight, 0, "in-flight table must be empty");
    assert!(
        stats.cache_bytes <= cap,
        "LRU over cap: {}",
        stats.cache_bytes
    );

    // Spawned jobs observe their cancelled tokens and drain.
    let deadline = Instant::now() + Duration::from_secs(30);
    while jobs.running_total() != 0 {
        assert!(
            Instant::now() < deadline,
            "job counters stuck at {}",
            jobs.running_total()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The byte cap is enforced by eviction (oldest first), and an entry larger
/// than the whole cap is served but never cached.
#[test]
fn lru_stays_under_its_byte_cap() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    // A 256px thumb of the 16x16 JPEG is 16*16*4 = 1 KiB; cap fits ~3.
    let cap = 3 * 1024;
    let provider = provider_for(&jobs, "lightbox-tiny.jpg", Orientation::O1, cap);
    for i in 0..10 {
        let t = provider.request(
            ImageId(i),
            PreviewClass::Thumb { max_px: 256 },
            Class::Background,
        );
        assert!(matches!(
            wait_terminal(&provider, &t),
            PreviewState::Ready(_)
        ));
    }
    let stats = provider.stats();
    assert!(
        stats.cache_bytes <= cap,
        "cache_bytes {} exceeds cap {cap}",
        stats.cache_bytes
    );
    assert_eq!(stats.decodes_started, 10, "distinct images: no dedup");

    // Oversized-vs-cap: loupe of a big preview with a microscopic cap.
    let (store, catalog, asset, content_hash) =
        store_catalog_and_asset("sony-ilce7s.arw#tiny-cap", "sony-ilce7s.arw");
    let tiny_cap = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join("sony-ilce7s.arw"),
            orientation: Orientation::O1,
            asset,
            content_hash,
        }),
        1024,
        store,
        catalog,
    );
    let t = tiny_cap.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
    assert!(matches!(
        wait_terminal(&tiny_cap, &t),
        PreviewState::Ready(_)
    ));
    assert_eq!(
        tiny_cap.stats().cache_bytes,
        0,
        "oversized entries not cached"
    );
}

/// Failures are per-ticket outcomes: a missing file reports `Io`, and the
/// provider stays healthy for the next request.
#[test]
fn missing_file_fails_the_ticket_not_the_provider() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let (store, catalog, asset, content_hash) =
        store_catalog_and_asset("does-not-exist.cr2", "does-not-exist.cr2");
    let provider = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join("does-not-exist.cr2"),
            orientation: Orientation::O1,
            asset,
            content_hash,
        }),
        1024 * 1024,
        store,
        catalog,
    );
    let t = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
    match wait_terminal(&provider, &t) {
        PreviewState::Failed(PreviewError::Io(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(provider.stats().inflight, 0);
}

/// T09 AC: `set_viewport` prefetch warms the decoded LRU so a scripted
/// next/prev traversal over the prefetched set swaps at p95 ≤ 50 ms, the LRU
/// never exceeds its byte cap, and eviction is LRU-ordered (proven here by
/// the cap itself never being exceeded across 200 distinct cache entries).
#[test]
fn set_viewport_prefetch_delivers_sub_50ms_swaps_over_200_images() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    // 200 distinct ImageIds, one shared fixture/asset (content-hash dedupe,
    // T07), generous enough to hold 200 tiny decoded 16x16 RGBA8 previews
    // (1 KiB each) comfortably under cap.
    let cap = 4 * 1024 * 1024;
    let provider = provider_for(&jobs, "lightbox-tiny.jpg", Orientation::O1, cap);

    let ids: Vec<ImageId> = (0..200).map(ImageId).collect();
    provider.set_viewport(&[], &ids);

    // Wait for every prefetch job to finish (`inflight` is the direct
    // signal; `set_viewport` only reaps its own tracking map on a
    // *subsequent* call, see its doc comment, so `prefetch_len` alone
    // would never settle to 0 without calling it again).
    let deadline = Instant::now() + Duration::from_secs(30);
    while provider.stats().inflight > 0 {
        assert!(Instant::now() < deadline, "prefetch never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        provider.stats().cache_bytes <= cap,
        "prefetch must respect the LRU cap"
    );

    // Scripted next/prev over a sliding window of the prefetched set: every
    // swap must already be warm (Ready on first poll) and fast.
    let mut samples = Vec::with_capacity(400);
    let script = (0..190).chain((0..190).rev());
    for i in script {
        let start = Instant::now();
        let ticket = provider.request(ids[i], PreviewClass::Loupe, Class::Interactive);
        match provider.poll(&ticket) {
            PreviewState::Ready(_) => {}
            other => panic!("swap for image {i} was not warm: {other:?}"),
        }
        samples.push(start.elapsed());
        provider.cancel(&ticket);
    }
    samples.sort();
    let p95 = samples[(samples.len() as f64 * 0.95) as usize - 1];
    assert!(
        p95 <= Duration::from_millis(50),
        "p95 swap latency {p95:?} exceeds the 50ms budget (spec §6)"
    );
    assert!(
        provider.stats().cache_bytes <= cap,
        "LRU never exceeds its cap"
    );
}

/// T09 AC: `set_viewport` cancels prefetch tracking for images that fall out
/// of view (visible-first demotion, a Phase B reading of the fuller
/// scheduler behavior Phase D builds).
#[test]
fn set_viewport_drops_tracking_for_images_no_longer_in_view() {
    let jobs = Arc::new(JobSystem::new(JobConfig::default()));
    let (store, catalog, asset, content_hash) =
        store_catalog_and_asset("lightbox-tiny.jpg#viewport-gate", "lightbox-tiny.jpg");
    let gate = Arc::new(GateLocator {
        path: fixtures_dir().join("lightbox-tiny.jpg"),
        asset,
        content_hash,
        entered: AtomicU64::new(0),
        release: AtomicBool::new(false),
    });
    let provider = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::clone(&gate) as _,
        64 * 1024 * 1024,
        store,
        catalog,
    );

    // A gated fixture keeps the prefetch job pending long enough to observe
    // it tracked, then superseded by a viewport move that drops it.
    provider.set_viewport(&[], &[ImageId(1)]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.entered.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "prefetch decode never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        provider.prefetch_len(),
        1,
        "image 1 is tracked while pending"
    );

    provider.set_viewport(&[], &[ImageId(2)]);
    assert_eq!(
        provider.prefetch_len(),
        1,
        "image 1 dropped (out of viewport), image 2 newly tracked (gated, still pending)"
    );

    gate.release.store(true, Ordering::SeqCst);
    // Drain both jobs so the process doesn't leave a blocked background
    // thread behind at test exit.
    let deadline = Instant::now() + Duration::from_secs(10);
    while jobs.running_total() != 0 {
        assert!(Instant::now() < deadline, "gated jobs never drained");
        std::thread::sleep(Duration::from_millis(2));
    }
}
