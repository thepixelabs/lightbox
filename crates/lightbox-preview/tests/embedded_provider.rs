// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! T20/T21 acceptance for [`EmbeddedPreviewProvider`]:
//!
//! - the largest embedded preview of each fixture decodes to expected dims
//!   (loupe class), and no/tiny-preview raws yield `NoEmbedded` (T20);
//! - cache hits are `Ready` on the first poll, concurrent duplicates run one
//!   decode, and a request/cancel storm leaks nothing (T21).
//!
//! Requires the fixture corpus: `cargo xtask fixtures`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightbox_jobs::{Class, JobConfig, JobSystem};
use lightbox_preview::{
    AssetLocator, EmbeddedPreviewProvider, LocatedAsset, PreviewClass, PreviewError,
    PreviewProvider, PreviewState,
};
use lightbox_types::{ImageId, Orientation, SourceTier};

fn fixtures_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    assert!(
        dir.join("manifest.toml").exists(),
        "fixture corpus missing at {} — run `cargo xtask fixtures` first",
        dir.display()
    );
    dir
}

/// Locates every image at one fixed path.
struct FixedLocator {
    path: PathBuf,
    orientation: Orientation,
}

impl AssetLocator for FixedLocator {
    fn locate(&self, _image: ImageId) -> Result<LocatedAsset, PreviewError> {
        Ok(LocatedAsset {
            path: self.path.clone(),
            orientation: self.orientation,
        })
    }
}

/// A locator whose `locate` blocks until released — makes request overlap
/// deterministic for the dedup test.
struct GateLocator {
    path: PathBuf,
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
        })
    }
}

fn provider_for(
    jobs: &Arc<JobSystem>,
    fixture: &str,
    orientation: Orientation,
    cache_bytes: u64,
) -> EmbeddedPreviewProvider {
    EmbeddedPreviewProvider::new(
        Arc::clone(jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join(fixture),
            orientation,
        }),
        cache_bytes,
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

/// T20 AC: the largest embedded preview of each fixture decodes to the
/// expected dimensions (loupe = largest, undownscaled, orientation NOT
/// applied); raws with no/tiny embedded previews yield `NoEmbedded`.
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
                assert!(!img.orientation_applied, "{fixture}: loupe stays unrotated");
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
    let gate = Arc::new(GateLocator {
        path: fixtures_dir().join("lightbox-tiny.jpg"),
        entered: AtomicU64::new(0),
        release: AtomicBool::new(false),
    });
    let provider =
        EmbeddedPreviewProvider::new(Arc::clone(&jobs), Arc::clone(&gate) as _, 64 * 1024 * 1024);
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

/// T21 AC: 500 rapid requests with immediate cancels leak nothing — ticket
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
    let tiny_cap = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join("sony-ilce7s.arw"),
            orientation: Orientation::O1,
        }),
        1024,
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
    let provider = EmbeddedPreviewProvider::new(
        Arc::clone(&jobs),
        Arc::new(FixedLocator {
            path: fixtures_dir().join("does-not-exist.cr2"),
            orientation: Orientation::O1,
        }),
        1024 * 1024,
    );
    let t = provider.request(ImageId(1), PreviewClass::Loupe, Class::Interactive);
    match wait_terminal(&provider, &t) {
        PreviewState::Failed(PreviewError::Io(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(provider.stats().inflight, 0);
}
