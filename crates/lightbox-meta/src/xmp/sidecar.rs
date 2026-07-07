// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Sidecar-only XMP file I/O (spec §3.5). **Mandate constraint c.2:** Lightbox
//! writes `.xmp` sidecars only and *never* modifies an original image file's
//! bytes or mtime. [`write_atomic`] targets the sidecar path exclusively; the
//! permanent byte-identity guard test is [`tests::write_atomic_never_touches_original`]
//! (mandate c.2, DoD §9.4 / T15 — asserts the original's bytes *and* mtime are
//! unchanged across a sidecar write).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::xmp::doc::{ParseLimits, XmpDoc, XmpError};

/// A content stamp recorded after a sidecar read/write (feeds `xmp_sync`, §3.5).
#[derive(Clone, Debug)]
pub struct SidecarStamp {
    /// xxh3-128 (big-endian) of the sidecar bytes.
    pub hash: [u8; 16],
    /// Sidecar mtime at read/write time (a cheap pre-check before hashing).
    pub mtime: Option<SystemTime>,
    /// Byte length of the sidecar.
    pub len: u64,
}

/// The sidecar path for an original (spec §3.5): replace the extension with
/// `.xmp` (LR-compatible). **Collision policy (documented):** two originals that
/// share a stem — `IMG_1.CR3` and `IMG_1.JPG` — map to the same `IMG_1.xmp`,
/// exactly as Lightroom does. Lightbox surfaces this rather than inventing a
/// stem-plus-extension scheme that no other tool reads.
pub fn sidecar_path(original: &Path) -> PathBuf {
    original.with_extension("xmp")
}

/// Read a sidecar if present (spec §3.5). Absent file → `Ok(None)`; malformed
/// content → a typed [`XmpError`].
pub fn read(path: &Path) -> Result<Option<XmpDoc>, XmpError> {
    Ok(read_with_stamp(path)?.map(|(doc, _)| doc))
}

/// Read a sidecar and its content stamp (for `xmp_sync` bookkeeping).
pub fn read_with_stamp(path: &Path) -> Result<Option<(XmpDoc, SidecarStamp)>, XmpError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(XmpError::Io(e.to_string())),
    };
    let doc = XmpDoc::parse(&bytes, ParseLimits::default())?;
    let mtime = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    let stamp = SidecarStamp {
        hash: stamp_hash(&bytes),
        mtime,
        len: bytes.len() as u64,
    };
    Ok(Some((doc, stamp)))
}

/// Atomically write a sidecar: temp file in the same directory, `fsync`, then
/// `rename` over the target (spec §3.5). A torn write leaves the *old* sidecar or
/// the *new* one — never a partial file. **Never** opens the original image.
pub fn write_atomic(path: &Path, doc: &XmpDoc) -> Result<SidecarStamp, XmpError> {
    let bytes = doc.to_bytes()?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = temp_sibling(path);

    {
        let mut f = fs::File::create(&tmp).map_err(|e| XmpError::Io(e.to_string()))?;
        f.write_all(&bytes)
            .map_err(|e| XmpError::Io(e.to_string()))?;
        f.sync_all().map_err(|e| XmpError::Io(e.to_string()))?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        // Best-effort cleanup so a failed write leaves no temp litter.
        let _ = fs::remove_file(&tmp);
        return Err(XmpError::Io(e.to_string()));
    }
    // fsync the directory so the rename is durable (best-effort; ignored on
    // platforms/filesystems that reject directory fsync).
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }

    let mtime = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Ok(SidecarStamp {
        hash: stamp_hash(&bytes),
        mtime,
        len: bytes.len() as u64,
    })
}

/// Read-only embedded-XMP extraction via a **bounded packet scan** (spec §3.5).
/// This is the container-agnostic fallback: it locates an `<?xpacket …?>` packet
/// in the first `scan_cap` bytes and parses it. Container-native handlers
/// (JPEG APP1, TIFF/DNG IFD, HEIC) are Phase C's `XMPFiles` seam (T15) — see
/// `E09-deviations.md` D-1. Never writes to `original`.
pub fn read_embedded(original: &Path) -> Result<Option<XmpDoc>, XmpError> {
    read_embedded_capped(original, 8 * 1024 * 1024)
}

/// [`read_embedded`] with an explicit scan cap.
pub fn read_embedded_capped(original: &Path, scan_cap: usize) -> Result<Option<XmpDoc>, XmpError> {
    use std::io::Read;
    let f = match fs::File::open(original) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(XmpError::Io(e.to_string())),
    };
    let mut buf = Vec::new();
    f.take(scan_cap as u64)
        .read_to_end(&mut buf)
        .map_err(|e| XmpError::Io(e.to_string()))?;
    match find_xpacket(&buf) {
        Some(range) => Ok(Some(XmpDoc::parse(&buf[range], ParseLimits::default())?)),
        None => Ok(None),
    }
}

/// Locate the `<?xpacket begin …?> … <?xpacket end …?>` byte range.
fn find_xpacket(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    let begin = b"<?xpacket begin";
    let end = b"<?xpacket end";
    let start = find_sub(bytes, begin)?;
    let end_marker = find_sub(&bytes[start..], end)? + start;
    // Include the closing `?>` of the end PI.
    let close = find_sub(&bytes[end_marker..], b"?>")? + end_marker + 2;
    Some(start..close)
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn stamp_hash(bytes: &[u8]) -> [u8; 16] {
    twox_hash::XxHash3_128::oneshot(bytes).to_be_bytes()
}

fn temp_sibling(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sidecar.xmp".to_string());
    name.push_str(&format!(".lbtmp-{}-{}-{}", std::process::id(), nanos, n));
    target.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp::{ns, XmpValue};

    #[test]
    fn sidecar_path_replaces_extension() {
        assert_eq!(
            sidecar_path(Path::new("/photos/IMG_1234.CR3")),
            PathBuf::from("/photos/IMG_1234.xmp")
        );
        // Documented collision: same stem, different original extension.
        assert_eq!(
            sidecar_path(Path::new("/photos/IMG_1234.JPG")),
            PathBuf::from("/photos/IMG_1234.xmp")
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let side = dir.path().join("IMG.xmp");
        let mut doc = XmpDoc::new();
        doc.set(ns::CRS, "Exposure2012", XmpValue::text("+1.00"))
            .unwrap();
        let stamp = write_atomic(&side, &doc).unwrap();
        let (back, rstamp) = read_with_stamp(&side).unwrap().unwrap();
        assert_eq!(back.get(ns::CRS, "Exposure2012").unwrap().as_str(), "+1.00");
        assert_eq!(stamp.hash, rstamp.hash);
    }

    /// Mandate constraint c.2 (DoD §9.4 / T15 AC): a sidecar write must **never**
    /// touch the original image file — neither its bytes nor its mtime. This is the
    /// permanent, PR-blocking byte-identity guard for the `write_atomic` path.
    #[test]
    fn write_atomic_never_touches_original() {
        let dir = tempfile::tempdir().unwrap();
        // A "raw original" with distinct bytes, next to where its sidecar will land.
        let original = dir.path().join("IMG_1234.CR3");
        let original_bytes: Vec<u8> = (0u16..4096).map(|n| (n % 251) as u8).collect();
        fs::write(&original, &original_bytes).unwrap();
        let mtime_before = fs::metadata(&original).unwrap().modified().unwrap();

        // Write the sidecar (goes to IMG_1234.xmp — the c.2-safe target).
        let side = sidecar_path(&original);
        assert_eq!(side, dir.path().join("IMG_1234.xmp"));
        let mut doc = XmpDoc::new();
        doc.set(ns::CRS, "Exposure2012", XmpValue::text("+1.00"))
            .unwrap();
        write_atomic(&side, &doc).unwrap();
        assert!(side.exists(), "sidecar must be created at the .xmp path");

        // The original is byte-for-byte and mtime identical.
        let after_bytes = fs::read(&original).unwrap();
        assert_eq!(
            after_bytes, original_bytes,
            "original bytes must be untouched"
        );
        let mtime_after = fs::metadata(&original).unwrap().modified().unwrap();
        assert_eq!(
            mtime_after, mtime_before,
            "original mtime must be untouched"
        );

        // Overwriting the sidecar a second time must still leave the original intact.
        write_atomic(&side, &doc).unwrap();
        assert_eq!(fs::read(&original).unwrap(), original_bytes);
        assert_eq!(
            fs::metadata(&original).unwrap().modified().unwrap(),
            mtime_before
        );
    }

    #[test]
    fn read_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("nope.xmp")).unwrap().is_none());
    }

    #[test]
    fn embedded_packet_scan_finds_xmp() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("fake.jpg");
        let mut doc = XmpDoc::new();
        doc.set(ns::CRS, "Exposure2012", XmpValue::text("+2.00"))
            .unwrap();
        let packet = doc.serialize().unwrap();
        // Simulate a container: junk, then the packet, then more junk.
        let mut bytes = vec![0xFFu8; 64];
        bytes.extend_from_slice(packet.as_bytes());
        bytes.extend_from_slice(&[0x00u8; 64]);
        fs::write(&img, &bytes).unwrap();
        let found = read_embedded(&img).unwrap().unwrap();
        assert_eq!(
            found.get(ns::CRS, "Exposure2012").unwrap().as_str(),
            "+2.00"
        );
    }
}
