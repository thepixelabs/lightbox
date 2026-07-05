// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Resource caps (E02 Phase C, task C4) enforced child-side. The supervisor
//! client mirrors [`MAX_PAYLOAD_BYTES`] parent-side (spec §3.2: caps enforced on
//! both ends). A memory-bomb file with absurd declared dimensions is rejected
//! by [`MAX_PIXELS`] before any large allocation, and the OS-level address-space
//! rlimit (see [`crate::sandbox`]) is the backstop.
//!
//! Some items here are consumed only by the `libraw`-gated decode path; in the
//! default build they are legitimately unused (not dead).
#![cfg_attr(not(feature = "libraw"), allow(dead_code))]

/// Sane ceiling on `raw_width * raw_height`. 512 MP is well above any shipping
/// sensor; a declared value above this is a corrupt/hostile file.
pub const MAX_PIXELS: u64 = 512_000_000;

/// Hard cap on an out-of-band pixel payload. Enforced by the proxy before it
/// writes the payload file and by the client before it reads one.
pub const MAX_PAYLOAD_BYTES: u64 = 3 * 1024 * 1024 * 1024; // 3 GiB

/// Address-space (virtual memory) rlimit for the child, in bytes. Bounds the
/// blast radius of a LibRaw allocation bug / decompression bomb. Applied via
/// RLIMIT_AS on Linux/BSD; macOS does not enforce it (see [`crate::sandbox`]),
/// so it is unreferenced there.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const RLIMIT_ADDRESS_SPACE_BYTES: u64 = 6 * 1024 * 1024 * 1024; // 6 GiB

/// CPU-seconds rlimit for the child process (a hung/pathological decode is
/// killed by the OS even if the parent's watchdog is delayed).
pub const RLIMIT_CPU_SECONDS: u64 = 120;

/// True when `raw_width * raw_height` is within [`MAX_PIXELS`].
pub fn pixels_ok(width: u32, height: u32) -> bool {
    (width as u64).saturating_mul(height as u64) <= MAX_PIXELS
}
