// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The edit-state store layer (E09 §3.1.1 / §4.1, Phase B `T5-DAOs`/`T6`):
//! write DAOs on [`CatalogTxn`] for `edit_recipe`/`edit_index`/`history_step`/
//! `snapshot`/`xmp_sync`, and the matching read surface on [`ReaderHandle`].
//!
//! This crate stores and returns **raw bytes** (CBOR blobs) only — it never
//! decodes a `Recipe`/`ParamDelta`/`StepLabel`. `lightbox-edit`'s `EditStore`
//! (spec §3.3) is the sole caller that understands those payloads; no SQL and
//! no `rusqlite` type crosses this crate's boundary (E01 rule, kept).
//!
//! Six read methods are the spec §2 named seam
//! (`edit_state_row`/`history_page`/`snapshots`/`edit_badges`/
//! `image_for_content_hash`/`xmp_sync_row`); [`ReaderHandle::history_replay_range`]
//! is additional plumbing (not spec-named) that makes the T9 nearest-keyframe
//! replay reconstruction an `O(distance-to-anchor)` query instead of an
//! `O(seq)` full-history fetch.

use rusqlite::{params, OptionalExtension};

use lightbox_types::{AssetId, ContentHash, HistoryStepId, ImageId, ProcessVersion, SnapshotId};

use crate::clock::now_rfc3339_utc;
use crate::error::{CatalogError, Result};
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

// ── write DAOs (T5) ─────────────────────────────────────────────────────────

impl CatalogTxn<'_> {
    /// Upserts the authoritative `edit_recipe` row (spec §4.1 invariants 1-3).
    ///
    /// `pv` is asserted equal to `image.process_version` on **every** write
    /// (§4.1-3 pv-immutability): a crafted mismatch is a typed
    /// [`CatalogError::InvalidArg`], never a silent write. The `ON CONFLICT`
    /// clause never touches the `pv` column either, so even a caller that
    /// bypassed the assertion could not move it.
    pub fn upsert_edit_recipe(
        &mut self,
        image: ImageId,
        pv: ProcessVersion,
        schema: u16,
        doc: &[u8],
        head_seq: u64,
    ) -> Result<()> {
        let img_pv: i64 = self
            .txn
            .query_row(
                "SELECT process_version FROM image WHERE id = ?1",
                params![image.0],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => CatalogError::NotFound {
                    entity: "image",
                    id: image.0,
                },
                other => CatalogError::from(other),
            })?;
        if img_pv != i64::from(pv.0) {
            return Err(CatalogError::InvalidArg(format!(
                "edit_recipe.pv ({}) must equal image.process_version ({}) for image {} \
                 — pv is immutable per image (spec §4.1-3)",
                pv.0, img_pv, image.0
            )));
        }
        let head_seq = i64::try_from(head_seq)
            .map_err(|_| CatalogError::InvalidArg(format!("head_seq {head_seq} overflows i64")))?;
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO edit_recipe (image_id, pv, schema, doc, head_seq, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(image_id) DO UPDATE SET \
               schema = excluded.schema, doc = excluded.doc, \
               head_seq = excluded.head_seq, updated_at = excluded.updated_at",
            params![image.0, pv.0, schema, doc, head_seq, now],
        )?;
        Ok(())
    }

    /// Deletes history steps with `seq > head_seq` (LR truncate-on-edit-
    /// after-step-back semantics, spec §4.1-2, protocol step 1). Returns the
    /// number of rows removed.
    pub fn truncate_history_after(&mut self, image: ImageId, head_seq: u64) -> Result<u64> {
        let head_seq = i64::try_from(head_seq)
            .map_err(|_| CatalogError::InvalidArg(format!("head_seq {head_seq} overflows i64")))?;
        let n = self.txn.execute(
            "DELETE FROM history_step WHERE image_id = ?1 AND seq > ?2",
            params![image.0, head_seq],
        )?;
        Ok(n as u64)
    }

    /// Appends one committed step at `seq` (spec §4.1-2 protocol step 2). The
    /// caller (the `lightbox-edit` commit protocol) computes `seq =
    /// head_seq + 1` — the DAO does not infer it, so the single-gesture
    /// commit path and the multi-image batch-commit path (settings
    /// sync/preset apply) share one primitive. Pass `keyframe_doc =
    /// Some(full_cbor)` on every 64th step (`seq % 64 == 0`), else `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn append_history_step(
        &mut self,
        image: ImageId,
        seq: u64,
        op: &[u8],
        delta: &[u8],
        inverse: &[u8],
        keyframe_doc: Option<&[u8]>,
    ) -> Result<HistoryStepId> {
        let seq_i = i64::try_from(seq)
            .map_err(|_| CatalogError::InvalidArg(format!("seq {seq} overflows i64")))?;
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO history_step (image_id, seq, op, delta, inverse, keyframe_doc, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![image.0, seq_i, op, delta, inverse, keyframe_doc, now],
        )?;
        Ok(HistoryStepId(self.txn.last_insert_rowid()))
    }

    /// Rebuilds the derived `edit_index` row for `image` (§4.1 invariant 1:
    /// must run in the SAME txn as the `edit_recipe.doc` write). This crate
    /// never decodes CBOR — the caller derives `is_edited`/`crop_ratio`/
    /// `treatment` from the decoded `Recipe` and passes the scalars.
    #[allow(clippy::too_many_arguments)]
    pub fn rebuild_edit_index(
        &mut self,
        image: ImageId,
        is_edited: bool,
        has_masks: bool,
        has_ai_mask: bool,
        crop_ratio: Option<f64>,
        treatment: Option<&str>,
    ) -> Result<()> {
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO edit_index \
               (image_id, is_edited, has_masks, has_ai_mask, crop_ratio, treatment, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(image_id) DO UPDATE SET \
               is_edited = excluded.is_edited, has_masks = excluded.has_masks, \
               has_ai_mask = excluded.has_ai_mask, crop_ratio = excluded.crop_ratio, \
               treatment = excluded.treatment, updated_at = excluded.updated_at",
            params![
                image.0,
                is_edited,
                has_masks,
                has_ai_mask,
                crop_ratio,
                treatment,
                now
            ],
        )?;
        Ok(())
    }

    /// Creates a named snapshot (spec §3.1/§3.3). `UNIQUE(image_id, name)`
    /// surfaces a duplicate name as [`CatalogError::Constraint`] (T10 AC: a
    /// typed error, never a silent overwrite).
    pub fn insert_snapshot(
        &mut self,
        image: ImageId,
        name: &str,
        recipe_doc: &[u8],
    ) -> Result<SnapshotId> {
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO snapshot (image_id, name, recipe_doc, ts) VALUES (?1, ?2, ?3, ?4)",
            params![image.0, name, recipe_doc, now],
        )?;
        Ok(SnapshotId(self.txn.last_insert_rowid()))
    }

    /// Deletes a snapshot by id.
    pub fn delete_snapshot(&mut self, snapshot: SnapshotId) -> Result<()> {
        let n = self
            .txn
            .execute("DELETE FROM snapshot WHERE id = ?1", params![snapshot.0])?;
        expect_one(n, "snapshot", snapshot.0)
    }

    /// Renames a snapshot in place (id is the stable identity); `UNIQUE
    /// (image_id, name)` still applies.
    pub fn rename_snapshot(&mut self, snapshot: SnapshotId, name: &str) -> Result<()> {
        let n = self.txn.execute(
            "UPDATE snapshot SET name = ?1 WHERE id = ?2",
            params![name, snapshot.0],
        )?;
        expect_one(n, "snapshot", snapshot.0)
    }

    /// Stamps `xmp_sync` after a successful atomic sidecar **write** (§4.1-5):
    /// upserts `sidecar_hash`/`sidecar_mtime`/`recipe_hash`/`last_written_at`.
    /// (Wiring this from `Command::Edit(WriteMetadata)` is T22/T8 territory —
    /// this DAO is the primitive it will call.)
    pub fn upsert_xmp_sync_write(
        &mut self,
        asset: AssetId,
        sidecar_hash: &[u8],
        sidecar_mtime: &str,
        recipe_hash: &[u8],
    ) -> Result<()> {
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO xmp_sync (asset_id, sidecar_hash, sidecar_mtime, recipe_hash, last_written_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(asset_id) DO UPDATE SET \
               sidecar_hash = excluded.sidecar_hash, sidecar_mtime = excluded.sidecar_mtime, \
               recipe_hash = excluded.recipe_hash, last_written_at = excluded.last_written_at",
            params![asset.0, sidecar_hash, sidecar_mtime, recipe_hash, now],
        )?;
        Ok(())
    }

    /// Stamps `xmp_sync` after applying a sidecar **read** into the recipe
    /// (`ReadMetadata`, §4.1-5): upserts the same trio plus `last_read_at`.
    pub fn upsert_xmp_sync_read(
        &mut self,
        asset: AssetId,
        sidecar_hash: &[u8],
        sidecar_mtime: &str,
        recipe_hash: &[u8],
    ) -> Result<()> {
        let now = now_rfc3339_utc();
        self.txn.execute(
            "INSERT INTO xmp_sync (asset_id, sidecar_hash, sidecar_mtime, recipe_hash, last_read_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(asset_id) DO UPDATE SET \
               sidecar_hash = excluded.sidecar_hash, sidecar_mtime = excluded.sidecar_mtime, \
               recipe_hash = excluded.recipe_hash, last_read_at = excluded.last_read_at",
            params![asset.0, sidecar_hash, sidecar_mtime, recipe_hash, now],
        )?;
        Ok(())
    }
}

fn expect_one(changed: usize, entity: &'static str, id: i64) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(CatalogError::NotFound { entity, id })
    }
}

// ── read DTOs + surface (T6) ────────────────────────────────────────────────

/// One `edit_recipe` row, raw bytes (spec §3.3 `EditStore`/`Queries` seam
/// DTO). `lightbox-edit` decodes `doc` via `Recipe::from_cbor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditStateRow {
    /// The image this recipe belongs to.
    pub image: ImageId,
    /// Process version (§4.5; immutable per image).
    pub pv: ProcessVersion,
    /// `RECIPE_SCHEMA` at write time.
    pub schema: u16,
    /// CBOR `Recipe` bytes (authoritative).
    pub doc: Vec<u8>,
    /// History position `doc` corresponds to (`recipe_at(head_seq) == doc`).
    pub head_seq: u64,
    /// RFC3339 UTC.
    pub updated_at: String,
}

/// One `history_step` row, raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryStepRow {
    /// Row id.
    pub id: HistoryStepId,
    /// The image this step belongs to.
    pub image: ImageId,
    /// Dense per-image sequence number, `1..n`.
    pub seq: u64,
    /// CBOR `StepLabel` bytes.
    pub op: Vec<u8>,
    /// CBOR `ParamDelta` bytes (forward).
    pub delta: Vec<u8>,
    /// CBOR `ParamDelta` bytes (backward).
    pub inverse: Vec<u8>,
    /// Full CBOR `Recipe` bytes at anchor steps (every 64th), else `None`.
    pub keyframe_doc: Option<Vec<u8>>,
    /// RFC3339 UTC.
    pub ts: String,
}

/// One `snapshot` row, raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRow {
    /// Row id.
    pub id: SnapshotId,
    /// The image this snapshot belongs to.
    pub image: ImageId,
    /// Display name (`UNIQUE` per image).
    pub name: String,
    /// The self-contained materialized CBOR projection (§3.1).
    pub recipe_doc: Vec<u8>,
    /// RFC3339 UTC.
    pub ts: String,
}

/// One `edit_index` row (spec §3.4 `Queries::edit_badges`); the default
/// (all-false/`None`) for an image with no row (D1: untouched).
#[derive(Clone, Debug, PartialEq)]
pub struct EditBadge {
    /// The image this badge describes.
    pub image: ImageId,
    /// Filmstrip "edited" badge.
    pub is_edited: bool,
    /// Reserved; E12 populates.
    pub has_masks: bool,
    /// Reserved; E14 populates.
    pub has_ai_mask: bool,
    /// Normalized crop aspect; `None` = full frame.
    pub crop_ratio: Option<f64>,
    /// `"color"` | `"bw"`.
    pub treatment: Option<String>,
}

/// One `xmp_sync` row, raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmpSyncRow {
    /// The asset this bookkeeping row describes.
    pub asset: AssetId,
    /// xxh3-128 of the sidecar bytes at our last write/read.
    pub sidecar_hash: Option<Vec<u8>>,
    /// Disk mtime at that moment.
    pub sidecar_mtime: Option<String>,
    /// `Recipe::canonical_hash` at that moment.
    pub recipe_hash: Option<Vec<u8>>,
    /// By us (`WriteMetadata` / auto-write).
    pub last_written_at: Option<String>,
    /// Into the edit store (`ReadMetadata`).
    pub last_read_at: Option<String>,
}

/// The nearest-keyframe-anchored replay range for `recipe_at` (T9). Additive
/// helper beyond the spec's six `ReaderHandle` DTO names — this is what makes
/// "nearest anchor + replay" an indexed lookup instead of a full history scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryReplayRange {
    /// `Some((seq, cbor))` of the nearest keyframe at or before the target
    /// seq; `None` means "replay from the neutral default at seq 0".
    pub anchor: Option<(u64, Vec<u8>)>,
    /// Ascending `(seq, delta)` pairs strictly after the anchor, up to and
    /// including the target seq.
    pub deltas: Vec<(u64, Vec<u8>)>,
}

impl ReaderHandle {
    /// The current `edit_recipe` row for `image`, or `None` for an untouched
    /// image (D1: no row until the first committed gesture).
    pub fn edit_state_row(&self, image: ImageId) -> Result<Option<EditStateRow>> {
        self.conn()
            .query_row(
                "SELECT image_id, pv, schema, doc, head_seq, updated_at \
                 FROM edit_recipe WHERE image_id = ?1",
                params![image.0],
                map_edit_state_row,
            )
            .optional()
            .map_err(CatalogError::from)
    }

    /// A page of history steps, newest-first (E08 panel / T9's `history::list`).
    /// `before_seq` (exclusive) pages backward from the head; `None` starts at
    /// the most recent step. `limit` is clamped to `1..=10_000` (session-scale
    /// store, spec §7 R9 — a hard cap is future-proofing, not a real ceiling).
    pub fn history_page(
        &self,
        image: ImageId,
        before_seq: Option<u64>,
        limit: u32,
    ) -> Result<Vec<HistoryStepRow>> {
        let limit = i64::from(limit.clamp(1, 10_000));
        let mut stmt;
        let mapped = match before_seq {
            Some(before) => {
                stmt = self.conn().prepare_cached(
                    "SELECT id, image_id, seq, op, delta, inverse, keyframe_doc, ts \
                     FROM history_step WHERE image_id = ?1 AND seq < ?2 \
                     ORDER BY seq DESC LIMIT ?3",
                )?;
                stmt.query_map(params![image.0, before as i64, limit], map_history_row)?
            }
            None => {
                stmt = self.conn().prepare_cached(
                    "SELECT id, image_id, seq, op, delta, inverse, keyframe_doc, ts \
                     FROM history_step WHERE image_id = ?1 \
                     ORDER BY seq DESC LIMIT ?2",
                )?;
                stmt.query_map(params![image.0, limit], map_history_row)?
            }
        };
        let mut out = Vec::new();
        for row in mapped {
            out.push(row?);
        }
        Ok(out)
    }

    /// The nearest-keyframe-anchored replay range for `seq` (T9's
    /// `history::recipe_at`): the newest keyframe at or before `seq` (if
    /// any), plus the ascending forward deltas from there to `seq`.
    pub fn history_replay_range(&self, image: ImageId, seq: u64) -> Result<HistoryReplayRange> {
        let seq_i = i64::try_from(seq).unwrap_or(i64::MAX);
        let anchor: Option<(i64, Vec<u8>)> = self
            .conn()
            .query_row(
                "SELECT seq, keyframe_doc FROM history_step \
                 WHERE image_id = ?1 AND seq <= ?2 AND keyframe_doc IS NOT NULL \
                 ORDER BY seq DESC LIMIT 1",
                params![image.0, seq_i],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let anchor_seq = anchor.as_ref().map(|(s, _)| *s).unwrap_or(0);
        let mut stmt = self.conn().prepare_cached(
            "SELECT seq, delta FROM history_step \
             WHERE image_id = ?1 AND seq > ?2 AND seq <= ?3 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![image.0, anchor_seq, seq_i], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut deltas = Vec::new();
        for row in rows {
            deltas.push(row?);
        }
        Ok(HistoryReplayRange {
            anchor: anchor.map(|(s, doc)| (s as u64, doc)),
            deltas,
        })
    }

    /// The highest `seq` present in `history_step` for `image` — which can
    /// exceed `edit_recipe.head_seq` after a `StepTo` moved the head
    /// backward (later steps are kept until the next commit truncates them,
    /// §4.1-2). `None` when no steps exist. Powers `Redo`.
    pub fn latest_history_seq(&self, image: ImageId) -> Result<Option<u64>> {
        let v: Option<i64> = self.conn().query_row(
            "SELECT MAX(seq) FROM history_step WHERE image_id = ?1",
            params![image.0],
            |r| r.get(0),
        )?;
        Ok(v.map(|v| v as u64))
    }

    /// All snapshots for `image`, oldest-first.
    pub fn snapshots(&self, image: ImageId) -> Result<Vec<SnapshotRow>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, image_id, name, recipe_doc, ts FROM snapshot \
             WHERE image_id = ?1 ORDER BY ts ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![image.0], |r| {
            Ok(SnapshotRow {
                id: SnapshotId(r.get(0)?),
                image: ImageId(r.get(1)?),
                name: r.get(2)?,
                recipe_doc: r.get(3)?,
                ts: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Filmstrip badges for `images`, in the same order (spec §3.4
    /// `Queries::edit_badges`). An image with no `edit_index` row (D1:
    /// untouched) yields the all-false/`None` default, not an error.
    pub fn edit_badges(&self, images: &[ImageId]) -> Result<Vec<EditBadge>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT is_edited, has_masks, has_ai_mask, crop_ratio, treatment \
             FROM edit_index WHERE image_id = ?1",
        )?;
        let mut out = Vec::with_capacity(images.len());
        for &image in images {
            let row: Option<EditIndexCols> = stmt
                .query_row(params![image.0], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?;
            let (is_edited, has_masks, has_ai_mask, crop_ratio, treatment) =
                row.unwrap_or((false, false, false, None, None));
            out.push(EditBadge {
                image,
                is_edited,
                has_masks,
                has_ai_mask,
                crop_ratio,
                treatment,
            });
        }
        Ok(out)
    }

    /// The default (non-virtual) image of the asset with this content hash —
    /// the E04 loader's restore seam (spec §4.2): resolves across a
    /// file move/rename because identity is the hash, never the path.
    pub fn image_for_content_hash(&self, hash: ContentHash) -> Result<Option<ImageId>> {
        self.conn()
            .query_row(
                "SELECT i.id FROM image i JOIN asset a ON a.id = i.asset_id \
                 WHERE a.content_hash = ?1 AND i.is_virtual = 0 \
                 ORDER BY i.id ASC LIMIT 1",
                params![&hash.0[..]],
                |r| Ok(ImageId(r.get(0)?)),
            )
            .optional()
            .map_err(CatalogError::from)
    }

    /// The `xmp_sync` bookkeeping row for `asset`, if any.
    pub fn xmp_sync_row(&self, asset: AssetId) -> Result<Option<XmpSyncRow>> {
        self.conn()
            .query_row(
                "SELECT asset_id, sidecar_hash, sidecar_mtime, recipe_hash, \
                        last_written_at, last_read_at \
                 FROM xmp_sync WHERE asset_id = ?1",
                params![asset.0],
                |r| {
                    Ok(XmpSyncRow {
                        asset: AssetId(r.get(0)?),
                        sidecar_hash: r.get(1)?,
                        sidecar_mtime: r.get(2)?,
                        recipe_hash: r.get(3)?,
                        last_written_at: r.get(4)?,
                        last_read_at: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(CatalogError::from)
    }
}

/// `(is_edited, has_masks, has_ai_mask, crop_ratio, treatment)` — the raw
/// `edit_index` row shape for [`ReaderHandle::edit_badges`].
type EditIndexCols = (bool, bool, bool, Option<f64>, Option<String>);

fn map_edit_state_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EditStateRow> {
    Ok(EditStateRow {
        image: ImageId(r.get(0)?),
        pv: ProcessVersion(r.get(1)?),
        schema: r.get(2)?,
        doc: r.get(3)?,
        head_seq: r.get::<_, i64>(4)? as u64,
        updated_at: r.get(5)?,
    })
}

fn map_history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryStepRow> {
    Ok(HistoryStepRow {
        id: HistoryStepId(r.get(0)?),
        image: ImageId(r.get(1)?),
        seq: r.get::<_, i64>(2)? as u64,
        op: r.get(3)?,
        delta: r.get(4)?,
        inverse: r.get(5)?,
        keyframe_doc: r.get(6)?,
        ts: r.get(7)?,
    })
}
