// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-meta`, the XMP substrate (spec §3.5).
//!
//! A thin, **swappable** wrapper over an RDF/XML packet engine: parse/serialize,
//! typed property access across the `crs`/`lb`/`xmp`/`dc`/`lr` namespaces, and
//! atomic **sidecar-only** file I/O. The mapping layer that turns a [`Recipe`]
//! into `crs:`/`lb:` properties lives one crate up in `lightbox-edit::xmp_map`
//! (spec §3.6), this crate knows nothing about recipes.
//!
//! # Substrate reversal (spec §1.6 / Risk R1)
//!
//! The architecture's primary substrate is the Adobe ISO 16684 XMP Toolkit via
//! the `xmp_toolkit` bindings (CTO sign-off recorded in `02-approval.md`). The
//! **named fallback** is our own RDF/XML behind exactly this [`xmp::XmpDoc`] API.
//! E09 Phase D ships the fallback (Phase C's toolkit integration had not landed
//! when Phase D executed, see `E09-deviations.md` D-1). The API surface matches
//! spec §3.5 so a later toolkit swap is a **one-crate** change; the encapsulation
//! rule, no substrate type escapes [`xmp::doc`], keeps that swap cheap.
//!
//! [`Recipe`]: https://docs.rs/lightbox-edit
//! [`xmp::XmpDoc`]: crate::xmp::XmpDoc
//! [`xmp::doc`]: crate::xmp::doc

pub mod xmp;
