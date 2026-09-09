// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Isolated `unsafe` boundary for the raw cache's mmap'd read path (E03 spec
//! §3.3/§5.4 AC: "mmap read"). One function, nothing else, mirrors
//! `codec/jxl.rs`'s "own minimal FFI file" precedent for scoping the
//! workspace's `unsafe_code = "deny"` lint override to the smallest possible
//! surface rather than the whole crate.

#![allow(unsafe_code)] // mmap exception (workspace lints doc comment: "only
                       // lightbox-render... and future FFI crates may
                       // locally `#![allow(unsafe_code)]` with
                       // justification"), scoped to this one file, whose
                       // entire job is the single `unsafe` call below. See
                       // the SAFETY comment on `map_readonly` for the actual
                       // argument.

use std::fs::File;
use std::io;

use memmap2::Mmap;

/// Memory-maps a whole, already-open, read-only file for
/// [`super::RawCache::get`]'s hot path.
///
/// # Safety argument (why this crate accepts `Mmap::map`'s unsafety)
///
/// `Mmap::map` is `unsafe` because the OS gives no guarantee the backing
/// file isn't truncated or overwritten-in-place by another process while
/// mapped, a data race outside Rust's (or memmap2's) control, which can
/// surface as a `SIGBUS`/access violation on the mapped pages rather than a
/// clean I/O error.
///
/// Two things make that an acceptable, already-mitigated risk here rather
/// than an open safety hole:
///
/// 1. **This crate's own writer never mutates a raw-cache file in place.**
///    Every write goes through [`crate::store::atomic_write`]: temp file in
///    the same directory → `fsync` → `rename`. POSIX rename semantics mean
///    a file descriptor (or mapping) opened against the OLD path keeps
///    referencing the OLD inode's content, a concurrent `put()` racing a
///    `get()`'s mmap never truncates or edits bytes out from under an
///    in-flight reader; at worst the reader sees the pre-rename generation
///    of the file, which is still a complete, checksummable container.
/// 2. **A torn read from any OTHER cause is still caught, not silently
///    served.** [`super::decode_hit`] verifies the zstd frame's built-in
///    content checksum on every read; a short/garbled map surfaces as a
///    checksum failure (the same corruption path the container format
///    exists to handle, spec §3.3), not as wrong data returned to a caller.
///
/// The one gap this doesn't cover, another *process* truncating the file
/// out from under an active mapping, is the same best-effort posture the
/// spec already assigns to non-atomic filesystems (Risk R6); nothing in
/// this store ever does that itself.
pub(super) fn map_readonly(file: &File) -> io::Result<Mmap> {
    unsafe { Mmap::map(file) }
}
