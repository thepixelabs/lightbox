// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Raw decode: shared types, the CPU reference linearizer, the proxy protocol,
//! and the E03 raw-state contract. The CFA-mosaic pixel decode itself is the
//! out-of-process LibRaw proxy (Phase C); in-crate linear/mono-DNG decode is
//! A5. Phase A ships the types + [`linearize`](linearize::linearize) + the
//! frozen contracts.

pub mod linearize;
pub mod proxy;
pub mod state;
pub mod types;

use std::path::Path;

use crate::error::DecodeError;
use types::{DecodeOpts, RawDecode};

/// Body behind [`crate::decode_raw`] (wrapped by the panic guard in `lib.rs`).
///
/// **Deferred in Phase A** (recorded in E02-deviations.md): CFA-mosaic decode
/// is the Phase-C LibRaw proxy (`decode_raw` routes mosaic files to it under
/// the `Auto` policy); in-crate linear-DNG / monochrome-DNG decode is A5. Until
/// those land this returns a structured [`DecodeError::Unimplemented`] — E04
/// treats it exactly like any decode error (asset stays catalogued; §5.1),
/// never a panic.
pub(crate) fn decode_raw_impl(path: &Path, opts: &DecodeOpts) -> Result<RawDecode, DecodeError> {
    // Surface I/O problems honestly even in the scaffold state, so callers get
    // the real failure (missing file, permissions) rather than a blanket stub.
    let _ = std::fs::metadata(path)?;
    let _ = opts;
    Err(DecodeError::Unimplemented(
        "raw mosaic decode is the Phase-C LibRaw proxy; in-crate linear/mono-DNG decode is A5",
    ))
}
