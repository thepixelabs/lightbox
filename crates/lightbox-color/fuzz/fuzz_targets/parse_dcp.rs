// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Fuzz `parse_dcp` (spec §3.1 / F3: untrusted `.dcp` input is bounds-checked,
//! dimension-capped, never panics, never OOMs). The parser must return `Ok` or a
//! structured `DcpParseError` for any byte string — every multi-byte read is
//! bounds-checked and every table dimension × element count is capped before a
//! byte is allocated.
//!
//!   cargo +nightly fuzz run parse_dcp    # from crates/lightbox-color
//!
//! The in-tree adversarial-fixture + proptest subset (`dcp::tests`) is the
//! always-on gate; this target is the deep corpus the nightly job drives.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The only invariant: this returns rather than aborts/hangs/OOMs.
    let _ = lightbox_color::dcp::parse_dcp(data);
});
