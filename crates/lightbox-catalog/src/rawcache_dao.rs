// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The raw-cache accounting store layer (E03 spec §3.3/§4/§5.4, Phase E
//! T18): write DAOs on [`CatalogTxn`] and the matching read surface on
//! [`ReaderHandle`] for the `raw_cache_entry` table (migration
//! 0004_preview_pyramid, the table was shipped in Phase A, T03, ahead of
//! this phase's DAO; see that migration's own header comment).
//!
//! Mirrors `preview_dao.rs`'s shape deliberately (single-writer upsert +
//! pooled-reader queries, unix-second timestamps for the same "touched on
//! every hot-path hit" reason, spec §3.2's "Accounting write pressure"
//! note applies here too). One divergence, recorded rather than silently
//! copied: this table's `UNIQUE (content_hash, params_hash)` constraint has
//! no partial-index split (unlike `preview`'s asset/image scopes), every
//! row here is scoped the same way, so a single upsert conflict target
//! suffices, and this module additionally exposes **by-key** touch/delete
//! (not just by-id) because `lightbox-preview`'s `RawCache::get`/hot-miss
//! paths only ever have a `(content_hash, params_hash)` key in hand, not a
//! pre-resolved row id, deriving the id first would be a second, avoidable
//! round trip on the read-hot path. `lightbox-preview` (E03) is the sole
//! caller; no SQL and no `rusqlite` type crosses this crate's boundary (E01
//! rule, kept, matching `preview_dao.rs`).

use rusqlite::{params, OptionalExtension};

use lightbox_types::{ContentHash, RawCacheEntryId};

use crate::error::Result;
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

/// One row heading into [`CatalogTxn::upsert_rawcache_entry`] (spec §4). The
/// `(content_hash, params_hash)` pair is the upsert's conflict target (the
/// table's own `UNIQUE` constraint).
#[derive(Clone, Debug)]
pub struct NewRawCacheEntryRow {
    pub content_hash: ContentHash,
    /// xxh3-64 of the producer's canonical early-stage parameter encoding
    /// (spec §3.3; big-endian, see [`RawCacheEntryRow::params_hash`]'s doc
    /// comment for why, unlike `preview.variant_hash`'s little-endian
    /// convention).
    pub params_hash: [u8; 8],
    /// Opaque to this crate and to E03 generally (E02/E05-owned).
    pub payload_schema: u16,
    /// Relative to the store root (spec §3.3 key scheme).
    pub store_path: String,
    /// On-disk (compressed) size.
    pub bytes: u64,
}

/// One `raw_cache_entry` row, as stored (spec §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCacheEntryRow {
    pub id: RawCacheEntryId,
    pub content_hash: ContentHash,
    /// Big-endian xxh3-64 bytes, the same convention as the filename's
    /// `<params_hash_hex16>` component (spec §3.3), so this column and the
    /// on-disk name always agree byte-for-byte when hex-printed (aids
    /// reconcile/debugging; see `lightbox-preview`'s `rawcache.rs` for the
    /// filename side of this convention).
    pub params_hash: [u8; 8],
    pub payload_schema: u16,
    pub store_path: String,
    /// On-disk (compressed) size.
    pub bytes: u64,
    /// Unix seconds.
    pub built_at: i64,
    /// Unix seconds.
    pub last_used_at: i64,
}

// ── write DAOs (T18) ────────────────────────────────────────────────────

impl CatalogTxn<'_> {
    /// Upserts one raw-cache accounting row (spec §4/§5.4 `RawCache::put`'s
    /// index-side half): a fresh `put()` of the same `(content_hash,
    /// params_hash)` replaces the prior row's content fields and re-stamps
    /// `built_at`/`last_used_at` to "now", the conflict key itself never
    /// changes on an upsert.
    pub fn upsert_rawcache_entry(&mut self, row: NewRawCacheEntryRow) -> Result<RawCacheEntryId> {
        let now = now_unix_seconds();
        self.txn.execute(
            "INSERT INTO raw_cache_entry \
               (content_hash, params_hash, payload_schema, store_path, bytes, built_at, last_used_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
             ON CONFLICT(content_hash, params_hash) DO UPDATE SET \
               payload_schema = excluded.payload_schema, store_path = excluded.store_path, \
               bytes = excluded.bytes, built_at = excluded.built_at, last_used_at = excluded.last_used_at",
            params![
                &row.content_hash.0[..],
                row.params_hash.to_vec(),
                row.payload_schema,
                row.store_path,
                row.bytes as i64,
                now,
            ],
        )?;
        let id: i64 = self.txn.query_row(
            "SELECT id FROM raw_cache_entry WHERE content_hash = ?1 AND params_hash = ?2",
            params![&row.content_hash.0[..], row.params_hash.to_vec()],
            |r| r.get(0),
        )?;
        Ok(RawCacheEntryId(id))
    }

    /// Touches `last_used_at` for one entry by its `(content_hash,
    /// params_hash)` key (spec §3.2's touch primitive; see the module doc
    /// comment for why by-key, not by-id, is `RawCache::get`'s natural
    /// call shape). `Ok(false)`, not an error, if the entry is gone
    /// (evicted/reconciled between the file read and this call): a cache
    /// accounting miss is never a caller-visible failure (spec §5.4 "never
    /// blocks render").
    pub fn touch_rawcache_last_used_by_key(
        &mut self,
        content_hash: ContentHash,
        params_hash: [u8; 8],
    ) -> Result<bool> {
        let now = now_unix_seconds();
        let n = self.txn.execute(
            "UPDATE raw_cache_entry SET last_used_at = ?1 WHERE content_hash = ?2 AND params_hash = ?3",
            params![now, &content_hash.0[..], params_hash.to_vec()],
        )?;
        Ok(n > 0)
    }

    /// Deletes one entry by id (the LRU-eviction/reconcile primitive, spec
    /// §5.4 `evict_to_cap`). `Ok(())` even if already gone (idempotent,
    /// matches `preview_dao::delete_preview`'s spirit).
    pub fn delete_rawcache_entry(&mut self, id: RawCacheEntryId) -> Result<()> {
        self.txn
            .execute("DELETE FROM raw_cache_entry WHERE id = ?1", params![id.0])?;
        Ok(())
    }

    /// Deletes one entry by id **only if** its `last_used_at` still matches
    /// `expected_last_used_at`, an optimistic-concurrency guard
    /// `RawCache::evict_to_cap` (`lightbox-preview`) uses so a `put()`/touch
    /// that refreshed this exact row *after* it was selected as an
    /// LRU-eviction candidate, but *before* the delete executes, is not
    /// evicted out from under the write that just refreshed it (spec T18 AC
    /// "concurrent get/put during eviction is race-free"). Returns `true`
    /// only if THIS call actually removed the row, `false` means either it
    /// was already gone or (the case this guard exists for) someone else
    /// touched it first, and the caller must not delete the entry's file
    /// either in that case.
    ///
    /// **Known residual narrow race** (recorded in
    /// `docs/plan/epics/E03-deviations.md`, Phase E): `last_used_at` is
    /// second-granularity (this table's convention, see the module doc
    /// comment), so a refresh landing in the SAME wall-clock second as the
    /// value this guard compares against is invisible to it. That is a
    /// self-healing divergence (`RawCache::reconcile` adopts the resulting
    /// orphan file back), not a panic/corruption/wrong-data risk, matching
    /// the architecture's own "caches are disposable, reconcile heals
    /// divergence" posture (spec §3.2), so it is accepted rather than
    /// chased with a schema change (a monotonic version column) this phase
    /// does not otherwise need.
    pub fn delete_rawcache_entry_if_unchanged(
        &mut self,
        id: RawCacheEntryId,
        expected_last_used_at: i64,
    ) -> Result<bool> {
        let n = self.txn.execute(
            "DELETE FROM raw_cache_entry WHERE id = ?1 AND last_used_at = ?2",
            params![id.0, expected_last_used_at],
        )?;
        Ok(n > 0)
    }

    /// Deletes one entry by its `(content_hash, params_hash)` key (spec
    /// §3.3 "a checksum failure on `get`... deletes the entry", the
    /// corruption-drop path, which has a key but not a pre-resolved id in
    /// hand). Returns the number of rows removed (0 or 1, the table's
    /// `UNIQUE` constraint guarantees at most one).
    pub fn delete_rawcache_entry_by_key(
        &mut self,
        content_hash: ContentHash,
        params_hash: [u8; 8],
    ) -> Result<u64> {
        let n = self.txn.execute(
            "DELETE FROM raw_cache_entry WHERE content_hash = ?1 AND params_hash = ?2",
            params![&content_hash.0[..], params_hash.to_vec()],
        )?;
        Ok(n as u64)
    }
}

// ── read DTOs + surface (T18) ───────────────────────────────────────────

impl ReaderHandle {
    /// Point lookup by the accounting key (spec §5.4 `RawCache::contains`'s
    /// index-only primitive, and `get()`'s corruption/adoption paths).
    pub fn rawcache_lookup(
        &self,
        content_hash: ContentHash,
        params_hash: [u8; 8],
    ) -> Result<Option<RawCacheEntryRow>> {
        self.conn()
            .query_row(
                "SELECT id, content_hash, params_hash, payload_schema, store_path, bytes, \
                        built_at, last_used_at \
                 FROM raw_cache_entry WHERE content_hash = ?1 AND params_hash = ?2",
                params![&content_hash.0[..], params_hash.to_vec()],
                map_rawcache_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every raw-cache row, unpaged (spec T18: reconcile's full-scan half
    /// fine at cache scale, the same reasoning as
    /// `preview_dao::all_preview_rows`).
    pub fn all_rawcache_rows(&self) -> Result<Vec<RawCacheEntryRow>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, content_hash, params_hash, payload_schema, store_path, bytes, \
                    built_at, last_used_at \
             FROM raw_cache_entry ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], map_rawcache_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// LRU-ordered eviction candidates (spec §3.2/§5.4 `evict_to_cap`),
    /// oldest-`last_used_at` first.
    pub fn rawcache_evict_candidates(&self, limit: u32) -> Result<Vec<RawCacheEntryRow>> {
        let limit = i64::from(limit.clamp(1, 1_000_000));
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, content_hash, params_hash, payload_schema, store_path, bytes, \
                    built_at, last_used_at \
             FROM raw_cache_entry ORDER BY last_used_at ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_rawcache_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Sum of `bytes` across every raw-cache row, the cap check
    /// `RawCache::evict_to_cap` loops against (spec §5.4/§5.7
    /// `rawcache_cap_bytes`). `0` on an empty table, never `NULL` (SQL
    /// `COALESCE`).
    pub fn rawcache_total_bytes(&self) -> Result<u64> {
        let total: i64 = self.conn().query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM raw_cache_entry",
            [],
            |r| r.get(0),
        )?;
        Ok(total.max(0) as u64)
    }

    /// Row count (diagnostics/`RawCache::entry_count`, a cheap `COUNT(*)`
    /// rather than `all_rawcache_rows().len()`, which would hydrate every
    /// row just to count them).
    pub fn rawcache_entry_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM raw_cache_entry", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }
}

fn map_rawcache_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawCacheEntryRow> {
    let content_hash: Vec<u8> = r.get(1)?;
    let params_hash: Vec<u8> = r.get(2)?;
    Ok(RawCacheEntryRow {
        id: RawCacheEntryId(r.get(0)?),
        content_hash: ContentHash(content_hash.try_into().unwrap_or([0u8; 16])),
        params_hash: params_hash.try_into().unwrap_or([0u8; 8]),
        payload_schema: u16::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        store_path: r.get(4)?,
        bytes: r.get::<_, i64>(5)? as u64,
        built_at: r.get(6)?,
        last_used_at: r.get(7)?,
    })
}

/// Current time as unix seconds (this table's convention, see the module
/// doc comment / `preview_dao.rs`'s own copy of this helper for why).
fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
