// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Out-of-band pixel-payload handoff (E02 Phase C, task C1). The proxy writes
//! decoded pixels to a uniquely-named temp file and returns a [`ShmRef`] to it;
//! the supervisor client reads then deletes the file (hygiene: no leaked temps —
//! task C4). See `raw::proxy`'s module note for why this is a temp file rather
//! than an mmap'd shared segment.
//!
//! Consumed by the `libraw`-gated decode path (and the module test); unused in
//! the default build's binary target, not dead.
#![cfg_attr(not(feature = "libraw"), allow(dead_code))]

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use lightbox_decode::ShmRef;

/// Distinguishes payloads within one proxy process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filename prefix for our payload files — the cleanup/leak audit greps for it.
pub const PAYLOAD_PREFIX: &str = "lightbox-rawproxy-payload-";

/// Writes `bytes` to a fresh temp file and returns a [`ShmRef`] naming it.
pub fn write_payload(bytes: &[u8]) -> io::Result<ShmRef> {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("{PAYLOAD_PREFIX}{pid}-{n}-{nanos}.bin");
    let path = std::env::temp_dir().join(name);
    // Write to a `.tmp` sibling then rename, so a reader never sees a partial
    // file (the ShmRef is only returned once the bytes are fully on disk).
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(ShmRef {
        name: path.to_string_lossy().into_owned(),
        len: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_payload_creates_a_readable_prefixed_file() {
        let data = vec![1u8, 2, 3, 4, 5];
        let shm = write_payload(&data).unwrap();
        assert_eq!(shm.len, 5);
        assert!(shm.name.contains(PAYLOAD_PREFIX));
        let read = std::fs::read(&shm.name).unwrap();
        assert_eq!(read, data);
        std::fs::remove_file(&shm.name).unwrap();
    }

    #[test]
    fn hundred_cycles_leave_no_leaked_payload_files() {
        // Emulates the proxy-writes / client-reads-and-deletes lifecycle
        // (task C4: no leaked temps after 100 cycles). Each ShmRef names a
        // uniquely-generated file; we delete it as the client would, then assert
        // nothing with our prefix survived.
        for i in 0..100u32 {
            let shm = write_payload(&i.to_le_bytes()).unwrap();
            let _ = std::fs::read(&shm.name).unwrap();
            std::fs::remove_file(&shm.name).unwrap();
        }
        let mut leaked = 0usize;
        let my_prefix = format!("{PAYLOAD_PREFIX}{}-", std::process::id());
        for entry in std::fs::read_dir(std::env::temp_dir()).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&my_prefix) {
                leaked += 1;
            }
        }
        assert_eq!(leaked, 0, "no payload temp files should leak");
    }
}
