// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Isolated `unsafe` boundary for the free-disk-space probe (E03 spec §3.2/
//! §5.2, Phase F T21 ENOSPC pre-flight). One function, nothing else, same
//! "smallest possible `unsafe` surface" precedent as `rawcache/mmap_io.rs`
//! (T17) and `codec/jxl.rs` (T11).

#![allow(unsafe_code)] // statvfs FFI exception (workspace lints doc comment).
                       // Scoped to this one file, whose entire job is the
                       // single `libc::statvfs` call below.

use std::path::Path;

/// Bytes free on the filesystem backing `path` (unix: `statvfs`; see
/// `rawcache.rs::RealDiskSpaceProbe`'s doc comment for the non-unix
/// posture, this function is only ever compiled/called from that impl).
///
/// # Safety argument
///
/// `libc::statvfs` is called with a valid, NUL-terminated C string (built
/// from `path`'s raw OS bytes, `CString::new` itself rejects interior NULs,
/// the one way this could be unsound) and a stack-allocated, zero-initialized
/// `libc::statvfs` output buffer whose lifetime covers the call. The return
/// value is checked before any field of the buffer is read. No pointer
/// outlives this function.
#[cfg(unix)]
pub(super) fn available_bytes(path: &Path) -> std::io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
}

/// No `GetDiskFreeSpaceExW` binding exists in this workspace's dependency
/// graph (would need `windows-sys`), "assume plenty of room" on this
/// platform (DEFERRED, `docs/plan/epics/E03-deviations.md` Phase F).
#[cfg(not(unix))]
pub(super) fn available_bytes(_path: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}
