// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Import-corpus staging: the fixture mix + unique synthetic JPEGs
//! (spec T28: "import-1k (synthetic + fixture mix)").

use std::io;
use std::path::Path;

/// The pinned, self-made CC0 16×16 JFIF JPEG (same asset the fixture
/// manifest pins and the shell smoke embeds) — works from a bare checkout.
pub const TINY_JPEG: &[u8] = include_bytes!("../../xtask/assets/lightbox-tiny.jpg");

/// The intact (non-corrupt) fixture files, i.e. the "fixture mix" half of
/// the import-1k corpus. Names per `fixtures/manifest.toml`.
pub const FIXTURE_MIX: &[&str] = &[
    "canon-eos-350d.cr2",
    "canon-eos-r6.cr3",
    "fujifilm-x100.raf",
    "fujifilm-xt1.raf",
    "gradient-8x8.png",
    "gradient-8x8.tiff",
    "lightbox-tiny.jpg",
    "nikon-d4s.nef",
    "nikon-z6.nef",
    "olympus-e1.orf",
    "sigma-fp.dng",
    "sony-ilce7s.arw",
];

/// Copies the fixture mix into `dest`; returns how many were found.
/// A missing corpus (bare offline checkout) is fine — the synthetic half
/// fills the file count.
pub fn copy_fixture_mix(fixtures: &Path, dest: &Path) -> io::Result<usize> {
    let mut copied = 0;
    for name in FIXTURE_MIX {
        let src = fixtures.join(name);
        if src.is_file() {
            std::fs::copy(&src, dest.join(name))?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Writes `n` unique synthetic JPEGs into `dir`: the pinned tiny JPEG plus
/// a per-file pad after EOI — decoders ignore the trailer, the content hash
/// (and therefore dup-skip identity) differs per file.
pub fn stage_synthetic_jpegs(dir: &Path, n: usize, prefix: &str) -> io::Result<()> {
    for i in 0..n {
        let mut bytes = TINY_JPEG.to_vec();
        bytes.extend_from_slice(format!("lbx-perf pad {prefix}{i:06}").as_bytes());
        std::fs::write(dir.join(format!("{prefix}{i:06}.jpg")), bytes)?;
    }
    Ok(())
}
