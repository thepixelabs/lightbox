// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The preview-pyramid index store layer (E03 spec §4, Phase A T03/T04):
//! write DAOs on [`CatalogTxn`] for `preview`, and the matching read surface
//! on [`ReaderHandle`].
//!
//! This crate stores and returns **raw scalar/blob fields** only — it never
//! interprets `store_path` or decides eviction policy; `lightbox-preview`
//! (E03) is the sole caller, composing these primitives into the in-RAM
//! index + touch batcher that back `PreviewService::best_available` (spec
//! §5.2). No SQL and no `rusqlite` type crosses this crate's boundary (E01
//! rule, kept).
//!
//! `built_at`/`last_used_at` are unix-second integers (not this crate's usual
//! RFC3339 TEXT) — see the migration file header for why: `last_used_at` is
//! touched on every cull/scroll and batched off the interactive path (spec
//! §3.2), so cheap numeric storage matters here specifically.

use rusqlite::{params, OptionalExtension};

use lightbox_types::{AssetId, ContentHash, ImageId, PreviewId};

use crate::error::{CatalogError, Result};
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

/// `preview.source` (spec §4): embedded-JPEG-derived vs. rendered through
/// the engine. Mirrors `PreviewSource` in `lightbox-preview` — this crate
/// stays free of that crate's dependency, so the tag travels as a small
/// local enum instead.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PreviewSourceTag {
    /// Camera-embedded JPEG, verbatim or downscaled from it.
    Embedded,
    /// Rendered through the node-graph engine (E05, M1+).
    Rendered,
}

impl PreviewSourceTag {
    fn as_sql(self) -> &'static str {
        match self {
            PreviewSourceTag::Embedded => "embedded",
            PreviewSourceTag::Rendered => "rendered",
        }
    }

    fn from_sql(s: &str) -> Result<PreviewSourceTag> {
        match s {
            "embedded" => Ok(PreviewSourceTag::Embedded),
            "rendered" => Ok(PreviewSourceTag::Rendered),
            other => Err(CatalogError::Internal(format!(
                "preview.source has unrecognized value {other:?}"
            ))),
        }
    }
}

/// One row heading into [`CatalogTxn::upsert_preview`] (spec §4). The
/// `(asset, image, tier, variant_hash)` scope tuple is the upsert's conflict
/// target — asset-scope (T0, `image = None`) and image-scope (T1/T2) rows
/// live in disjoint partial-unique-index spaces (`idx_preview_asset_scope`
/// / `idx_preview_image_scope`), so they can never collide with each other.
#[derive(Clone, Debug)]
pub struct NewPreviewRow {
    /// Owning asset (every row has one, even image-scoped T1/T2).
    pub asset: AssetId,
    /// `None` = asset-scope (T0); `Some` = image-scope (T1/T2).
    pub image: Option<ImageId>,
    /// Denormalized from `asset.content_hash` (relink survival, spec §3.1).
    pub content_hash: ContentHash,
    /// `0 | 1 | 2` (`lightbox_preview::Tier`, kept as a plain `u8` here —
    /// this crate does not depend on that crate's vocabulary types).
    pub tier: u8,
    /// xxh3-64 of the canonical `VariantParams` encoding (8 bytes).
    pub variant_hash: [u8; 8],
    pub source: PreviewSourceTag,
    /// Edit revision reflected; `0` = edit-independent (embedded rows).
    pub recipe_rev: u64,
    /// `'srgb'` | an ICC-tagged marker — free text, spec §5.1.
    pub colorspace: String,
    /// Relative to the store root (spec §3.1 key scheme).
    pub store_path: String,
    /// Upright (orientation baked).
    pub width: u32,
    pub height: u32,
    /// On-disk size; T2 sums over the tile dir (Phase F).
    pub bytes: u64,
    /// xxh3-64 of the encoded payload (T2: manifest hash).
    pub checksum: [u8; 8],
}

/// One `preview` row, as stored (spec §4).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewRow {
    pub id: PreviewId,
    pub asset: AssetId,
    pub image: Option<ImageId>,
    pub content_hash: ContentHash,
    pub tier: u8,
    pub variant_hash: [u8; 8],
    pub source: PreviewSourceTag,
    pub recipe_rev: u64,
    pub stale: bool,
    pub colorspace: String,
    pub store_path: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub checksum: [u8; 8],
    /// Unix seconds.
    pub built_at: i64,
    /// Unix seconds.
    pub last_used_at: i64,
}

// ── write DAOs (T03/T04) ────────────────────────────────────────────────────

impl CatalogTxn<'_> {
    /// Upserts one preview row (spec §4 DDL): a fresh build of the same
    /// `(scope, tier, variant_hash)` replaces the prior row's content fields
    /// and resets `stale = 0` (freshly built content is never stale) and
    /// re-stamps `built_at`/`last_used_at` to "now"; `content_hash`/scope/
    /// `tier`/`variant_hash` — the conflict key — never change on an upsert.
    ///
    /// Two physical statements because SQLite's partial-unique-index upsert
    /// conflict target must repeat that index's own `WHERE` clause verbatim
    /// (spec §4: `idx_preview_asset_scope` vs `idx_preview_image_scope`).
    pub fn upsert_preview(&mut self, row: NewPreviewRow) -> Result<PreviewId> {
        let now = now_unix_seconds();
        let tier = i64::from(row.tier);
        let checksum = row.checksum.to_vec();
        let variant_hash = row.variant_hash.to_vec();
        match row.image {
            None => {
                self.txn.execute(
                    "INSERT INTO preview \
                       (asset_id, image_id, content_hash, tier, variant_hash, source, \
                        recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                        checksum, built_at, last_used_at) \
                     VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13) \
                     ON CONFLICT(asset_id, tier, variant_hash) WHERE image_id IS NULL DO UPDATE SET \
                       content_hash = excluded.content_hash, source = excluded.source, \
                       recipe_rev = excluded.recipe_rev, stale = 0, \
                       colorspace = excluded.colorspace, store_path = excluded.store_path, \
                       width = excluded.width, height = excluded.height, bytes = excluded.bytes, \
                       checksum = excluded.checksum, built_at = excluded.built_at, \
                       last_used_at = excluded.last_used_at",
                    params![
                        row.asset.0,
                        &row.content_hash.0[..],
                        tier,
                        variant_hash,
                        row.source.as_sql(),
                        row.recipe_rev as i64,
                        row.colorspace,
                        row.store_path,
                        row.width,
                        row.height,
                        row.bytes as i64,
                        checksum,
                        now,
                    ],
                )?;
                let id: i64 = self.txn.query_row(
                    "SELECT id FROM preview WHERE asset_id = ?1 AND tier = ?2 AND variant_hash = ?3 \
                       AND image_id IS NULL",
                    params![row.asset.0, tier, row.variant_hash.to_vec()],
                    |r| r.get(0),
                )?;
                Ok(PreviewId(id))
            }
            Some(image) => {
                self.txn.execute(
                    "INSERT INTO preview \
                       (asset_id, image_id, content_hash, tier, variant_hash, source, \
                        recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                        checksum, built_at, last_used_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14) \
                     ON CONFLICT(image_id, tier, variant_hash) WHERE image_id IS NOT NULL DO UPDATE SET \
                       asset_id = excluded.asset_id, content_hash = excluded.content_hash, \
                       source = excluded.source, recipe_rev = excluded.recipe_rev, stale = 0, \
                       colorspace = excluded.colorspace, store_path = excluded.store_path, \
                       width = excluded.width, height = excluded.height, bytes = excluded.bytes, \
                       checksum = excluded.checksum, built_at = excluded.built_at, \
                       last_used_at = excluded.last_used_at",
                    params![
                        row.asset.0,
                        image.0,
                        &row.content_hash.0[..],
                        tier,
                        variant_hash,
                        row.source.as_sql(),
                        row.recipe_rev as i64,
                        row.colorspace,
                        row.store_path,
                        row.width,
                        row.height,
                        row.bytes as i64,
                        checksum,
                        now,
                    ],
                )?;
                let id: i64 = self.txn.query_row(
                    "SELECT id FROM preview WHERE image_id = ?1 AND tier = ?2 AND variant_hash = ?3",
                    params![image.0, tier, row.variant_hash.to_vec()],
                    |r| r.get(0),
                )?;
                Ok(PreviewId(id))
            }
        }
    }

    /// Stale-marks rendered rows of `image` whose `recipe_rev` predates
    /// `new_recipe_rev` (spec §5.2 `invalidate_image`, tested at E03 time
    /// with synthetic revisions — real edits arrive with E09/E05). Embedded
    /// rows are never staled (superseded, not invalidated, spec §3.1).
    /// Returns the number of rows marked.
    pub fn mark_rendered_stale_below(
        &mut self,
        image: ImageId,
        new_recipe_rev: u64,
    ) -> Result<u64> {
        let n = self.txn.execute(
            "UPDATE preview SET stale = 1 \
             WHERE image_id = ?1 AND source = 'rendered' AND recipe_rev < ?2 AND stale = 0",
            params![image.0, new_recipe_rev as i64],
        )?;
        Ok(n as u64)
    }

    /// Batched `last_used_at` touch flush (spec §3.2: "batched in memory and
    /// flushed to the catalog writer every ≤5s or ≥64 entries" — this is the
    /// flush primitive `lightbox-preview`'s in-RAM batcher calls). One
    /// timestamp for the whole flush: LRU is deliberately approximate here
    /// (spec §3.2), so sub-batch precision buys nothing. A id with no
    /// matching row (evicted/deleted between touch and flush) is silently
    /// skipped, not an error. Returns the number of rows actually touched.
    pub fn touch_previews_last_used(&mut self, ids: &[PreviewId]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = now_unix_seconds();
        let mut stmt = self
            .txn
            .prepare_cached("UPDATE preview SET last_used_at = ?1 WHERE id = ?2")?;
        let mut touched = 0u64;
        for id in ids {
            touched += stmt.execute(params![now, id.0])? as u64;
        }
        Ok(touched)
    }

    /// Deletes one preview row (spec §5's eviction primitive; the T19
    /// refcount-before-unlink policy and the actual file removal are Phase
    /// F's job — this is the index-side half). `Ok(())` even if the row is
    /// already gone (idempotent, matches `BlobStore::remove`'s spirit).
    pub fn delete_preview(&mut self, id: PreviewId) -> Result<()> {
        self.txn
            .execute("DELETE FROM preview WHERE id = ?1", params![id.0])?;
        Ok(())
    }
}

// ── read DTOs + surface (T04) ───────────────────────────────────────────────

impl ReaderHandle {
    /// One row by id.
    pub fn preview_row(&self, id: PreviewId) -> Result<Option<PreviewRow>> {
        self.conn()
            .query_row(
                "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                        recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                        checksum, built_at, last_used_at \
                 FROM preview WHERE id = ?1",
                params![id.0],
                map_preview_row,
            )
            .optional()
            .map_err(CatalogError::from)
    }

    /// Point lookup by scope tuple (spec §4 conflict key): `image = None`
    /// looks up an asset-scope (T0) row, `image = Some` an image-scope
    /// (T1/T2) row.
    pub fn preview_lookup(
        &self,
        asset: AssetId,
        image: Option<ImageId>,
        tier: u8,
        variant_hash: [u8; 8],
    ) -> Result<Option<PreviewRow>> {
        let tier = i64::from(tier);
        match image {
            None => self
                .conn()
                .query_row(
                    "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                            recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                            checksum, built_at, last_used_at \
                     FROM preview WHERE asset_id = ?1 AND tier = ?2 AND variant_hash = ?3 \
                       AND image_id IS NULL",
                    params![asset.0, tier, variant_hash.to_vec()],
                    map_preview_row,
                )
                .optional()
                .map_err(CatalogError::from),
            Some(image) => self
                .conn()
                .query_row(
                    "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                            recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                            checksum, built_at, last_used_at \
                     FROM preview WHERE image_id = ?1 AND tier = ?2 AND variant_hash = ?3",
                    params![image.0, tier, variant_hash.to_vec()],
                    map_preview_row,
                )
                .optional()
                .map_err(CatalogError::from),
        }
    }

    /// Every preview row, unpaged (spec T04: bulk hydration of the in-RAM
    /// index at `PreviewService::open`/startup). Fine at the spec's own
    /// benchmark scale (100k rows, §6) as a one-time cost; if the catalog
    /// ever needs to keep this cheap past that, add keyset pagination here
    /// without changing callers (the RAM index owns iteration order, not
    /// this crate).
    pub fn all_preview_rows(&self) -> Result<Vec<PreviewRow>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                    recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                    checksum, built_at, last_used_at \
             FROM preview ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], map_preview_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// LRU-ordered eviction candidates (spec §3.2 eviction order T2→T1→T0;
    /// the caller picks tiers to ask for in that order). `tier = None`
    /// returns candidates across every tier, oldest-`last_used_at` first.
    pub fn preview_evict_candidates(
        &self,
        tier: Option<u8>,
        limit: u32,
    ) -> Result<Vec<PreviewRow>> {
        let limit = i64::from(limit.clamp(1, 1_000_000));
        let mut stmt;
        let mapped = match tier {
            Some(t) => {
                stmt = self.conn().prepare_cached(
                    "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                            recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                            checksum, built_at, last_used_at \
                     FROM preview WHERE tier = ?1 ORDER BY last_used_at ASC LIMIT ?2",
                )?;
                stmt.query_map(params![i64::from(t), limit], map_preview_row)?
            }
            None => {
                stmt = self.conn().prepare_cached(
                    "SELECT id, asset_id, image_id, content_hash, tier, variant_hash, source, \
                            recipe_rev, stale, colorspace, store_path, width, height, bytes, \
                            checksum, built_at, last_used_at \
                     FROM preview ORDER BY last_used_at ASC LIMIT ?1",
                )?;
                stmt.query_map(params![limit], map_preview_row)?
            }
        };
        let mut out = Vec::new();
        for row in mapped {
            out.push(row?);
        }
        Ok(out)
    }
}

fn map_preview_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PreviewRow> {
    let content_hash: Vec<u8> = r.get(3)?;
    let variant_hash: Vec<u8> = r.get(5)?;
    let checksum: Vec<u8> = r.get(14)?;
    let source: String = r.get(6)?;
    let image_id: Option<i64> = r.get(2)?;
    Ok(PreviewRow {
        id: PreviewId(r.get(0)?),
        asset: AssetId(r.get(1)?),
        image: image_id.map(ImageId),
        content_hash: ContentHash(content_hash.try_into().unwrap_or([0u8; 16])),
        tier: u8::try_from(r.get::<_, i64>(4)?).unwrap_or(u8::MAX),
        variant_hash: variant_hash.try_into().unwrap_or([0u8; 8]),
        source: PreviewSourceTag::from_sql(&source).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other("bad preview.source")),
            )
        })?,
        recipe_rev: r.get::<_, i64>(7)? as u64,
        stale: r.get(8)?,
        colorspace: r.get(9)?,
        store_path: r.get(10)?,
        width: r.get::<_, i64>(11)? as u32,
        height: r.get::<_, i64>(12)? as u32,
        bytes: r.get::<_, i64>(13)? as u64,
        checksum: checksum.try_into().unwrap_or([0u8; 8]),
        built_at: r.get(15)?,
        last_used_at: r.get(16)?,
    })
}

/// Current time as unix seconds (this table's convention — see the module
/// doc comment for why it differs from the rest of the catalog).
fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
