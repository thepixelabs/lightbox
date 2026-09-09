// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `catalog_settings` key/value store (E08 spec §6.7, Phase G G2):
//! catalog-scope preferences, cache caps, T2 retention, cache-dir override
//! (the `CatalogPrefs` fields owned by `lightbox-core::prefs`).
//!
//! This crate stores and returns **opaque strings** only, it never parses
//! a byte cap or a retention policy. `lightbox-core::prefs` is the sole
//! caller that understands the values (same posture as `edit_state.rs`'s
//! CBOR blobs: no domain type crosses this crate's boundary).
//!
//! Rows are delta-only: a missing key means "built-in default", so
//! [`CatalogTxn::set_setting`] with `None` deletes the row rather than
//! writing a tombstone.

use rusqlite::{params, OptionalExtension};

use crate::clock::now_rfc3339_utc;
use crate::error::Result;
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

impl CatalogTxn<'_> {
    /// Upserts (`Some`) or deletes (`None`) one settings row. Keys are
    /// registry-stable dotted names (`"cache.preview_cap_bytes"`) chosen by
    /// the consumer; this layer enforces nothing about them.
    pub fn set_setting(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(value) => {
                self.txn.execute(
                    "INSERT INTO catalog_settings (key, value, updated_at) \
                     VALUES (?1, ?2, ?3) \
                     ON CONFLICT(key) DO UPDATE SET \
                       value = excluded.value, updated_at = excluded.updated_at",
                    params![key, value, now_rfc3339_utc()],
                )?;
            }
            None => {
                self.txn
                    .execute("DELETE FROM catalog_settings WHERE key = ?1", params![key])?;
            }
        }
        Ok(())
    }
}

impl ReaderHandle {
    /// One settings value, verbatim. `Ok(None)` = no row (use the default).
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn()
            .query_row(
                "SELECT value FROM catalog_settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value)
    }
}
