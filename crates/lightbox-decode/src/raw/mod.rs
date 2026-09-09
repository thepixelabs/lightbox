// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Raw decode: shared types, the CPU reference linearizer, the proxy protocol,
//! and the E03 raw-state contract. The CFA-mosaic pixel decode itself is the
//! out-of-process LibRaw proxy (Phase C); in-crate linear/mono-DNG decode is
//! A5. Phase A ships the types + [`linearize`](linearize::linearize) + the
//! frozen contracts.

pub mod linearize;
pub mod proxy;
pub mod proxy_client;
pub mod state;
pub mod types;

use std::path::Path;

use crate::error::DecodeError;
use proxy::LibrawParams;
use proxy_client::ProxySupervisor;
use types::{DecodeOpts, RawDecode, SourceImage};

/// Body behind [`crate::decode_raw`] (wrapped by the panic guard in `lib.rs`).
///
/// **Deferred in Phase A** (recorded in E02-deviations.md): CFA-mosaic decode
/// is the Phase-C LibRaw proxy (`decode_raw` routes mosaic files to it under
/// the `Auto` policy); in-crate linear-DNG / monochrome-DNG decode is A5. Until
/// those land this returns a structured [`DecodeError::Unimplemented`], E04
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

/// Body behind [`crate::decode_for_develop`] (wrapped by the panic guard in
/// `lib.rs`). Produces the M1 interim-demosaic develop image (spec §5.1 step 3,
/// task C5).
///
/// Routing: a CFA-mosaic raw (Bayer / X-Trans), and, until A5 lands the
/// in-crate linear/mono fast path, *any* raw, is decoded to linear
/// camera-native RGB by the LibRaw proxy's AHD path, returned as
/// [`RawDecode::DemosaicedInterim`] with `interim_demosaic = true` in its
/// provenance (so E03's cache segregates it from the post-E11 clean-room
/// demosaic; §4.2). A proxy crash / timeout is a structured [`DecodeError`],
/// never a panic and never an in-process fallback (the mosaic backend is single
/// by license necessity; R1).
pub(crate) fn decode_for_develop_impl(
    sup: &ProxySupervisor,
    path: &Path,
    opts: &DecodeOpts,
) -> Result<RawDecode, DecodeError> {
    let _ = std::fs::metadata(path)?;
    let params = LibrawParams::default();
    let src: SourceImage = sup.decode_demosaiced(path, &params, opts.timeout)?;
    Ok(RawDecode::DemosaicedInterim(src))
}
