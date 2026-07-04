// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `kill -9` fault-injection harness — the Risk-5 PR gate (spec §5 T13).
//!
//! A child process (this same test binary, re-invoked with
//! `LIGHTBOX_FAULT_CHILD_DIR` set, filtered to [`fault_child`]) performs
//! randomized small write transactions in a tight loop, journaling every
//! **committed** transaction's ids to a side file *after* the commit
//! returns (rating overwrites additionally journal a pre-commit `intent`
//! line, so a kill in the commit→journal window cannot masquerade as a lost
//! or phantom write). The parent SIGKILLs the child at a random moment,
//! reopens the catalog (which runs `PRAGMA quick_check`), and verifies:
//!
//! 1. **0 corruptions** — reopen succeeds; `quick_check` is clean;
//! 2. **0 lost committed transactions** — every journaled asset id exists
//!    and every image's rating is explainable by the journal (the last
//!    `done` value, or a trailing `intent` whose commit raced the kill).
//!
//! Iterations: 50 by default (PR gate); the nightly workflow sets
//! `LIGHTBOX_FAULT_ITERS=1000` (spec §6, DoD §8.3). The run ends with the
//! restore-from-backup drill (backup → unzstd → open → integrity, spec §6).
//!
//! # The negative control we do NOT ship (documentation, not code)
//!
//! With `PRAGMA synchronous = OFF` + `journal_mode = DELETE` this harness's
//! guarantees evaporate: `synchronous=OFF` lets COMMIT return before the OS
//! has durably ordered the writes, and DELETE-mode journaling overwrites
//! pages of the main file in place, so an ill-timed crash tears the database
//! itself — exactly the corruption class WAL + `synchronous=NORMAL` makes
//! impossible (torn writes stay in the WAL tail, which recovery discards;
//! committed pages are never overwritten in place). Note that SIGKILL alone
//! cannot revoke writes the kernel already accepted, so demonstrating the
//! broken variant's data loss needs power-cut simulation (dm-flakey/qemu),
//! not just this harness — documented so nobody "tunes" the pragmas and
//! waves a green kill-9 run as proof of safety (spec §5 T13 AC).

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use lightbox_catalog::{BackupOpts, Catalog, IntegrityStatus, NewAsset};
use lightbox_types::{ContentHash, FolderId, ImageId, Orientation};

const CHILD_DIR_ENV: &str = "LIGHTBOX_FAULT_CHILD_DIR";
const ITERS_ENV: &str = "LIGHTBOX_FAULT_ITERS";

/// Child-mode entry point. A no-op under normal `cargo test`; the real body
/// runs only when the parent re-invokes this binary with the env var set,
/// and then it loops until SIGKILLed.
#[test]
fn fault_child() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    if let Err(err) = child_workload(Path::new(&dir)) {
        // Leave the failure where the parent will see it, then die loudly.
        let _ = std::fs::write(Path::new(&dir).join("child-error.txt"), format!("{err:?}"));
        panic!("fault child failed: {err:?}");
    }
}

fn child_workload(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lbdata = dir.join("cat.lbdata");
    let catalog = Catalog::open(&lbdata)?;
    let folder = FolderId(
        std::fs::read_to_string(dir.join("folder-id.txt"))?
            .trim()
            .parse()?,
    );

    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("journal.txt"))?;
    let mut log = |line: String| -> std::io::Result<()> {
        journal.write_all(line.as_bytes())?;
        journal.sync_data()
    };

    // Unique-per-run namespace: pid ⊕ wall clock.
    let run_id = u64::from(std::process::id()) ^ nanos_now();
    let mut counter = 0u64;
    let mut rng = fastrand::Rng::with_seed(run_id);
    let mut my_images: Vec<i64> = Vec::new();

    // Loop until killed.
    loop {
        if rng.u8(..) < 200 || my_images.is_empty() {
            // Insert a small batch of assets + default images.
            let n = rng.u64(1..=4);
            let batch: Vec<NewAsset> = (0..n)
                .map(|_| {
                    counter += 1;
                    new_asset(folder, run_id, counter)
                })
                .collect();
            let (assets, images) = catalog.writer().with_txn(move |txn| {
                let outcome = txn.insert_assets(&batch)?;
                let images = txn.insert_default_images(&outcome.inserted)?;
                Ok((outcome.inserted, images))
            })?;
            // COMMIT has returned: journal it, durably.
            let mut line = String::from("assets");
            for (a, i) in assets.iter().zip(&images) {
                line.push_str(&format!(" {}:{}", a.0, i.0));
            }
            line.push('\n');
            log(line)?;
            my_images.extend(images.iter().map(|i| i.0));
        } else {
            // Overwrite a rating on an image we created earlier: intent
            // before the txn, done after the commit.
            let image = my_images[rng.usize(..my_images.len())];
            let rating = rng.u8(1..=5);
            log(format!("intent {image}:{rating}\n"))?;
            catalog
                .writer()
                .with_txn(move |txn| txn.set_rating(ImageId(image), Some(rating)))?;
            log(format!("done {image}:{rating}\n"))?;
        }
    }
}

/// The parent: spawn → random sleep → SIGKILL → reopen → verify. 50
/// iterations at the PR gate; `LIGHTBOX_FAULT_ITERS=1000` nightly.
#[test]
fn kill9_leaves_catalog_clean_and_committed_txns_present() {
    if std::env::var(CHILD_DIR_ENV).is_ok() {
        return; // we ARE a child process; only fault_child runs there
    }
    let iterations: u32 = std::env::var(ITERS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let lbdata = dir.join("cat.lbdata");

    // Set the stage once: catalog + root/folder, folder id handed to the
    // children through a file.
    {
        let catalog = Catalog::create(&lbdata).expect("create");
        let root_path = dir.join("photos");
        let folder = catalog
            .writer()
            .with_txn(move |txn| {
                let root = txn.upsert_root(None, &root_path)?;
                txn.upsert_folder(root, None, "fault")
            })
            .expect("seed folder");
        std::fs::write(dir.join("folder-id.txt"), folder.0.to_string()).unwrap();
    }

    let exe = std::env::current_exe().expect("current test binary");
    let mut rng = fastrand::Rng::with_seed(nanos_now());
    let mut total_journaled = 0usize;

    for iteration in 0..iterations {
        let mut child = std::process::Command::new(&exe)
            .args(["fault_child", "--exact", "--nocapture"])
            .env(CHILD_DIR_ENV, dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn fault child");

        // Let it commit for a random slice, then kill -9 mid-flight.
        std::thread::sleep(Duration::from_millis(rng.u64(10..=120)));
        child.kill().expect("SIGKILL child");
        child.wait().expect("reap child");

        assert!(
            !dir.join("child-error.txt").exists(),
            "iteration {iteration}: child errored before the kill: {}",
            std::fs::read_to_string(dir.join("child-error.txt")).unwrap_or_default()
        );

        // Reopen: Catalog::open runs quick_check — corruption fails here.
        let catalog = Catalog::open(&lbdata)
            .unwrap_or_else(|e| panic!("iteration {iteration}: reopen failed: {e}"));
        assert_eq!(
            catalog.integrity(),
            IntegrityStatus::Ok,
            "iteration {iteration}: quick_check found corruption"
        );
        total_journaled = verify_journal(&catalog, &dir.join("journal.txt"), iteration);
        drop(catalog);
    }

    assert!(
        total_journaled > 0,
        "harness never observed a committed txn"
    );
    eprintln!(
        "fault injection: {iterations} kills, {total_journaled} journaled committed txn lines, \
         0 corruptions, 0 lost commits"
    );

    // Restore-from-backup drill (spec §6): backup the crash-survivor
    // catalog → unzstd → open → integrity.
    let catalog = Catalog::open(&lbdata).expect("final open");
    let assets_now = catalog.reader().counts().expect("counts").assets;
    assert!(assets_now > 0);
    let report = catalog
        .backup_verified(&BackupOpts::default())
        .expect("backup of crash-survivor catalog");
    let restored_dir = dir.join("restored.lbdata");
    std::fs::create_dir_all(&restored_dir).unwrap();
    let src = std::fs::File::open(&report.path).unwrap();
    let mut dst = std::fs::File::create(restored_dir.join("catalog.sqlite")).unwrap();
    zstd::stream::copy_decode(src, &mut dst).unwrap();
    dst.flush().unwrap();
    drop(dst);
    let restored = Catalog::open(&restored_dir).expect("open restored backup");
    assert_eq!(restored.integrity(), IntegrityStatus::Ok);
    assert_eq!(
        restored.reader().counts().expect("counts").assets,
        assets_now
    );
}

#[derive(Default)]
struct RatingState {
    /// Value of the last `done` line (a proven commit).
    last_done: Option<u8>,
    /// `intent` values seen after the last `done` — commits that may or may
    /// not have landed before a kill.
    trailing_intents: Vec<u8>,
}

/// Replays the journal and checks every committed txn is present. Returns
/// the number of journal lines read.
fn verify_journal(catalog: &Catalog, journal_path: &Path, iteration: u32) -> usize {
    let journal = match std::fs::read_to_string(journal_path) {
        Ok(s) => s,
        Err(_) => return 0, // killed before the first commit
    };
    let lines: Vec<&str> = journal.lines().collect();
    let mut expected_assets: Vec<(i64, i64)> = Vec::new();
    let mut ratings: HashMap<i64, RatingState> = HashMap::new();
    for (idx, line) in lines.iter().enumerate() {
        let last = idx + 1 == lines.len();
        // A kill can tear at most the final line; anything malformed earlier
        // is a harness bug.
        let ok = parse_line(line, &mut expected_assets, &mut ratings);
        if !ok {
            assert!(
                last,
                "iteration {iteration}: malformed journal line {idx}: {line:?}"
            );
            // Torn tail: drop whatever half-parsed state it may have added
            // is unnecessary — parse_line only mutates on full success.
        }
    }

    let reader = catalog.reader();
    for (asset, image) in &expected_assets {
        let detail = reader.image_detail(ImageId(*image)).unwrap_or_else(|e| {
            panic!(
                "iteration {iteration}: journaled committed image {image} \
                     (asset {asset}) missing after kill -9: {e}"
            )
        });
        assert_eq!(detail.asset.0, *asset, "iteration {iteration}: id mismatch");
    }
    for (image, state) in &ratings {
        let detail = reader
            .image_detail(ImageId(*image))
            .unwrap_or_else(|e| panic!("iteration {iteration}: rated image missing: {e}"));
        let allowed: Vec<Option<u8>> = match state.last_done {
            Some(done) => std::iter::once(Some(done))
                .chain(state.trailing_intents.iter().map(|v| Some(*v)))
                .collect(),
            // Never proven committed: unrated is allowed too.
            None => std::iter::once(None)
                .chain(state.trailing_intents.iter().map(|v| Some(*v)))
                .collect(),
        };
        assert!(
            allowed.contains(&detail.rating),
            "iteration {iteration}: image {image} rating {:?} not explainable \
             by the journal (allowed {allowed:?}) — a committed write was lost",
            detail.rating
        );
    }
    lines.len()
}

/// Returns false when the line is malformed (torn tail).
fn parse_line(
    line: &str,
    expected_assets: &mut Vec<(i64, i64)>,
    ratings: &mut HashMap<i64, RatingState>,
) -> bool {
    fn pair(s: &str) -> Option<(i64, u8)> {
        let (a, b) = s.split_once(':')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    }
    if let Some(rest) = line.strip_prefix("assets ") {
        let mut parsed = Vec::new();
        for p in rest.split(' ') {
            let Some((a, i)) = p
                .split_once(':')
                .and_then(|(a, i)| Some((a.parse().ok()?, i.parse().ok()?)))
            else {
                return false;
            };
            parsed.push((a, i));
        }
        expected_assets.extend(parsed);
        true
    } else if let Some(rest) = line.strip_prefix("intent ") {
        let Some((image, rating)) = pair(rest) else {
            return false;
        };
        ratings
            .entry(image)
            .or_default()
            .trailing_intents
            .push(rating);
        true
    } else if let Some(rest) = line.strip_prefix("done ") {
        let Some((image, rating)) = pair(rest) else {
            return false;
        };
        let state = ratings.entry(image).or_default();
        state.last_done = Some(rating);
        state.trailing_intents.clear();
        true
    } else {
        false
    }
}

fn new_asset(folder: FolderId, run_id: u64, counter: u64) -> NewAsset {
    let mut hash = [0u8; 16];
    hash[..8].copy_from_slice(&run_id.to_le_bytes());
    hash[8..].copy_from_slice(&counter.to_le_bytes());
    NewAsset {
        folder,
        filename: format!("r{run_id:016x}-{counter}.jpg"),
        content_hash: ContentHash(hash),
        format: "JPEG".to_owned(),
        camera_make: None,
        camera_model: None,
        capture_time: None,
        width: 640,
        height: 480,
        orientation: Orientation::O1,
        bytes: 1024,
        mtime_utc: None,
        decode_error: None,
        import_session: None,
    }
}

fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
