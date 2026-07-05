// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`check_catalog`] — the headless integrity probe behind
//! `lightbox-cli check` (spec §3.10: "quick_check + schema version, exit
//! code").
//!
//! Additive plumbing on top of the frozen §3.8 surface: the CLI must report
//! catalog health without a shell — and without a `lightbox-catalog`
//! dependency of its own (no SQL crosses seam 1). A corrupt or
//! newer-schema catalog surfaces as the [`CoreError::Catalog`] the open
//! path already produces (naming the newest verified backup, spec §1.1);
//! callers map that to their exit code.

use std::path::Path;

use lightbox_catalog::{Catalog, IntegrityStatus};

use crate::error::Result;

/// What [`check_catalog`] found in a catalog that opened cleanly.
#[derive(Debug)]
#[non_exhaustive]
pub struct CatalogCheck {
    /// The schema version after opening (pending migrations applied —
    /// opening *is* the check that this build can use the catalog).
    pub schema_version: u32,
    /// `PRAGMA quick_check` findings on the opened catalog.
    pub integrity: IntegrityStatus,
}

impl CatalogCheck {
    /// True when the catalog opened and `quick_check` reported no findings.
    pub fn is_ok(&self) -> bool {
        matches!(self.integrity, IntegrityStatus::Ok)
    }
}

/// Opens the catalog exactly like a session would (WAL, `quick_check`,
/// pending migrations, newer-schema refusal) and reports schema version +
/// integrity. No session is started; the catalog is released on return.
///
/// A catalog that fails `quick_check` (or cannot be read at all) returns
/// `Err(CoreError::Catalog(CatalogError::Corrupt { .. }))` — the message
/// names the newest verified backup.
pub fn check_catalog(lbdata: &Path) -> Result<CatalogCheck> {
    let catalog = Catalog::open(lbdata)?;
    let integrity = catalog.integrity();
    Ok(CatalogCheck {
        schema_version: catalog.schema_version(),
        integrity,
    })
}
