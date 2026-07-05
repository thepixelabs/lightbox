// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Fuzz `IccProfile::from_bytes` (spec §3.5 / D7: untrusted ICC input is
//! size-capped, never panics, never OOMs, never aborts). The parser must return
//! `Ok` or a structured `IccError` for any byte string — Little-CMS2 rejects
//! malformed profiles with a null handle, and the size cap bounds allocation
//! before Little-CMS2 is reached.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The only invariant: this returns rather than aborts/hangs.
    let _ = lightbox_color::cms::IccProfile::from_bytes(data);
});
