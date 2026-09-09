// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Fuzz `probe()` (spec §3.7: never panics on malformed input) and, when a
//! probe reports embedded previews, `read_embedded()` over the fuzzed
//! ranges. The first input byte picks the file extension so both the
//! `Malformed` (claims-supported) and `Unsupported` classification paths are
//! exercised; the rest is the file content.

#![no_main]

use libfuzzer_sys::fuzz_target;

/// Extensions covering every dispatch branch: raw mounts, plain images,
/// an unknown extension, and none at all.
const EXTS: &[&str] = &[
    "cr2", "cr3", "nef", "arw", "raf", "orf", "dng", "rw2", "jpg", "png", "tif", "xyz", "",
];

fuzz_target!(|data: &[u8]| {
    let Some((&sel, content)) = data.split_first() else {
        return;
    };
    let ext = EXTS[usize::from(sel) % EXTS.len()];
    let dir = tempfile::tempdir().expect("tempdir");
    let name = if ext.is_empty() {
        "input".to_owned()
    } else {
        format!("input.{ext}")
    };
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("write fuzz input");

    // Must return Ok or Err, never panic, never hang (bounded walkers).
    if let Ok(probe) = lightbox_decode::probe(&path) {
        for info in probe.embedded.iter().take(4) {
            let _ = lightbox_decode::read_embedded(&path, info);
        }
    }
});
