// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Mutation DAOs on [`CatalogTxn`] (spec §3.2). The only public write
//! surface — raw SQL never crosses the crate boundary (spec §7 R6).

use std::path::Path;

use lightbox_types::{AssetId, ContentHash, Flag, FolderId, ImageId, ImportSessionId, RootId};
use rusqlite::params;

use crate::clock::now_rfc3339_utc;
use crate::error::{CatalogError, Result};
use crate::writer::CatalogTxn;

/// One file heading into `insert_assets` (spec §3.2). Built by the import
/// pipeline (`lightbox-ingest`, E01 Phase 4) from a probe + content hash.
#[derive(Clone, Debug)]
pub struct NewAsset {
    /// Folder the file lives in (must already exist — `upsert_folder`).
    pub folder: FolderId,
    /// Filename as on disk, NFC-normalized (spec §4.3).
    pub filename: String,
    /// xxh3-128 of the full file; the dup-skip / relink / cache key.
    pub content_hash: ContentHash,
    /// `'CR3'`,`'NEF'`,`'JPEG'`,…,`'UNSUPPORTED'`.
    pub format: String,
    /// EXIF camera make, if probed.
    pub camera_make: Option<String>,
    /// EXIF camera model, if probed.
    pub camera_model: Option<String>,
    /// RFC3339 UTC-normalized capture time, if probed.
    pub capture_time: Option<String>,
    /// Full-size pixel dimensions (0 if unknown).
    pub width: u32,
    /// Full-size pixel dimensions (0 if unknown).
    pub height: u32,
    /// EXIF orientation.
    pub orientation: lightbox_types::Orientation,
    /// File size in bytes.
    pub bytes: u64,
    /// File mtime, RFC3339 UTC, if available.
    pub mtime_utc: Option<String>,
    /// Probe/hash failure message; the asset is catalogued anyway (spec §5 T18).
    pub decode_error: Option<String>,
    /// Import session this file arrived in.
    pub import_session: Option<ImportSessionId>,
}

/// What `insert_assets` did (spec §3.2).
#[derive(Clone, Debug, Default)]
pub struct InsertOutcome {
    /// Ids of the rows actually inserted, in batch order.
    pub inserted: Vec<AssetId>,
    /// Rows skipped because their `content_hash` already existed.
    pub skipped_duplicates: u64,
    /// Batch indices of the skipped rows, ascending (`len() ==
    /// skipped_duplicates`) — lets callers attribute per-file outcomes
    /// (`lightbox-ingest` report accounting).
    pub skipped: Vec<usize>,
}

/// What `remove_import_session` removed (spec §3.2).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RemovedCounts {
    /// `asset` rows removed.
    pub assets: u64,
    /// `image` rows removed (via `ON DELETE CASCADE`).
    pub images: u64,
}

impl CatalogTxn<'_> {
    /// Finds or creates a `library_root` row. `volume_uuid` comes from
    /// platform APIs where available (the import path's job), else `None`.
    ///
    /// SQLite treats NULLs as distinct in UNIQUE constraints, so the upsert
    /// matches with `IS` semantics rather than relying on `ON CONFLICT`.
    pub fn upsert_root(&mut self, volume_uuid: Option<&str>, path: &Path) -> Result<RootId> {
        let path_str = path
            .to_str()
            .ok_or_else(|| CatalogError::NonUtf8Path(path.to_path_buf()))?;
        let existing: Option<i64> = self
            .txn
            .query_row(
                "SELECT id FROM library_root WHERE path = ?1 AND volume_uuid IS ?2",
                params![path_str, volume_uuid],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(none_when_no_rows)?;
        if let Some(id) = existing {
            return Ok(RootId(id));
        }
        self.txn.execute(
            "INSERT INTO library_root (volume_uuid, path) VALUES (?1, ?2)",
            params![volume_uuid, path_str],
        )?;
        Ok(RootId(self.txn.last_insert_rowid()))
    }

    /// Finds or creates a folder, creating missing ancestors up to the root
    /// folder (`rel_path = ''`, the spec §4.3 convention).
    ///
    /// `rel_path` is `'/'`-separated, without leading/trailing separators.
    /// When `parent` is `Some` it is trusted as the immediate parent;
    /// otherwise the parent chain is derived from `rel_path`.
    pub fn upsert_folder(
        &mut self,
        root: RootId,
        parent: Option<FolderId>,
        rel_path: &str,
    ) -> Result<FolderId> {
        validate_rel_path(rel_path)?;
        if let Some(id) = self.lookup_folder(root, rel_path)? {
            return Ok(id);
        }
        let parent_id: Option<FolderId> = match (parent, rel_path) {
            (_, "") => None, // the root folder itself has no parent
            (Some(p), _) => Some(p),
            (None, _) => {
                let parent_rel = rel_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                Some(self.upsert_folder(root, None, parent_rel)?)
            }
        };
        self.txn.execute(
            "INSERT INTO folder (root_id, parent_id, rel_path) VALUES (?1, ?2, ?3)",
            params![root.0, parent_id.map(|p| p.0), rel_path],
        )?;
        Ok(FolderId(self.txn.last_insert_rowid()))
    }

    fn lookup_folder(&self, root: RootId, rel_path: &str) -> Result<Option<FolderId>> {
        self.txn
            .query_row(
                "SELECT id FROM folder WHERE root_id = ?1 AND rel_path = ?2",
                params![root.0, rel_path],
                |r| r.get(0),
            )
            .map(|id| Some(FolderId(id)))
            .or_else(none_when_no_rows)
            .map_err(CatalogError::from)
    }

    /// Batched asset insert — one call site batches N files into this single
    /// transaction (spec §7 burst-import mitigation). Rows whose
    /// `content_hash` already exists anywhere in the catalog are skipped.
    pub fn insert_assets(&mut self, batch: &[NewAsset]) -> Result<InsertOutcome> {
        let mut outcome = InsertOutcome::default();
        let added_at = now_rfc3339_utc();
        {
            let mut exists = self
                .txn
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM asset WHERE content_hash = ?1)")?;
            let mut insert = self.txn.prepare_cached(
                "INSERT INTO asset (folder_id, filename, content_hash, format, camera_make, \
                 camera_model, capture_time, width, height, orientation, bytes, mtime_utc, \
                 decode_error, import_session_id, added_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for (idx, a) in batch.iter().enumerate() {
                let hash: &[u8] = &a.content_hash.0;
                let dup: bool = exists.query_row(params![hash], |r| r.get(0))?;
                if dup {
                    outcome.skipped_duplicates += 1;
                    outcome.skipped.push(idx);
                    continue;
                }
                insert.execute(params![
                    a.folder.0,
                    a.filename,
                    hash,
                    a.format,
                    a.camera_make,
                    a.camera_model,
                    a.capture_time,
                    a.width,
                    a.height,
                    a.orientation.exif_value(),
                    i64::try_from(a.bytes).map_err(|_| {
                        CatalogError::InvalidArg(format!("file size {} overflows", a.bytes))
                    })?,
                    a.mtime_utc,
                    a.decode_error,
                    a.import_session.map(|s| s.0),
                    added_at,
                ])?;
                outcome.inserted.push(AssetId(self.txn.last_insert_rowid()));
            }
        }
        Ok(outcome)
    }

    /// Creates the default (non-virtual) `image` row for each asset
    /// (spec §3.2). Virtual copies are E07.
    pub fn insert_default_images(&mut self, assets: &[AssetId]) -> Result<Vec<ImageId>> {
        let created_at = now_rfc3339_utc();
        let mut ids = Vec::with_capacity(assets.len());
        let mut insert = self.txn.prepare_cached(
            "INSERT INTO image (asset_id, is_virtual, flag, process_version, created_at) \
             VALUES (?1, 0, 0, 1, ?2)",
        )?;
        for asset in assets {
            insert.execute(params![asset.0, created_at])?;
            ids.push(ImageId(self.txn.last_insert_rowid()));
        }
        Ok(ids)
    }

    /// Opens an import-session bracket (spec §3.2). `opts_json` is the JSON
    /// form of the (`#[non_exhaustive]`) `ImportOptions`.
    pub fn begin_import_session(&mut self, src: &str, opts_json: &str) -> Result<ImportSessionId> {
        self.txn.execute(
            "INSERT INTO import_session (started_at, source, mode, options) \
             VALUES (?1, ?2, 'add', ?3)",
            params![now_rfc3339_utc(), src, opts_json],
        )?;
        Ok(ImportSessionId(self.txn.last_insert_rowid()))
    }

    /// Closes an import-session bracket, recording the stats JSON.
    pub fn finish_import_session(&mut self, id: ImportSessionId, stats_json: &str) -> Result<()> {
        let n = self.txn.execute(
            "UPDATE import_session SET finished_at = ?1, stats = ?2 WHERE id = ?3",
            params![now_rfc3339_utc(), stats_json, id.0],
        )?;
        expect_one(n, "import_session", id.0)
    }

    /// Undo-import (spec §3.2): removes the session's catalog rows only;
    /// files on disk are untouched (add-in-place). Image rows go via
    /// `ON DELETE CASCADE`; the FTS index follows via the delete trigger.
    pub fn remove_import_session(&mut self, id: ImportSessionId) -> Result<RemovedCounts> {
        let images: i64 = self.txn.query_row(
            "SELECT COUNT(*) FROM image WHERE asset_id IN \
             (SELECT id FROM asset WHERE import_session_id = ?1)",
            params![id.0],
            |r| r.get(0),
        )?;
        let assets = self.txn.execute(
            "DELETE FROM asset WHERE import_session_id = ?1",
            params![id.0],
        )?;
        let n = self
            .txn
            .execute("DELETE FROM import_session WHERE id = ?1", params![id.0])?;
        expect_one(n, "import_session", id.0)?;
        Ok(RemovedCounts {
            assets: assets as u64,
            images: images as u64,
        })
    }

    /// Sets or clears the 1..=5 star rating (spec §3.2; the canonical
    /// trivial command of the E01 command bus).
    pub fn set_rating(&mut self, image: ImageId, rating: Option<u8>) -> Result<()> {
        if let Some(r) = rating {
            if !(1..=5).contains(&r) {
                return Err(CatalogError::InvalidArg(format!(
                    "rating must be 1..=5, got {r}"
                )));
            }
        }
        let n = self.txn.execute(
            "UPDATE image SET rating = ?1 WHERE id = ?2",
            params![rating, image.0],
        )?;
        expect_one(n, "image", image.0)
    }

    /// Sets the pick/reject flag (spec §3.2).
    pub fn set_flag(&mut self, image: ImageId, flag: Flag) -> Result<()> {
        let n = self.txn.execute(
            "UPDATE image SET flag = ?1 WHERE id = ?2",
            params![flag.to_db(), image.0],
        )?;
        expect_one(n, "image", image.0)
    }

    /// Records a decode/probe failure on the asset (spec §6 error taxonomy:
    /// the file stays catalogued, badged, and never crashes the app).
    pub fn mark_decode_error(&mut self, asset: AssetId, err: &str) -> Result<()> {
        let n = self.txn.execute(
            "UPDATE asset SET decode_error = ?1 WHERE id = ?2",
            params![err, asset.0],
        )?;
        expect_one(n, "asset", asset.0)
    }

    /// Test-only escape hatch for in-crate tests (FTS property tests exercise
    /// the `UPDATE OF filename` trigger, which no M0 DAO reaches — rename is
    /// E07). Never public: no SQL crosses the crate boundary.
    #[cfg(test)]
    pub(crate) fn raw(&self) -> &rusqlite::Transaction<'_> {
        &self.txn
    }
}

/// Maps "0 rows changed" onto [`CatalogError::NotFound`].
fn expect_one(changed: usize, entity: &'static str, id: i64) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(CatalogError::NotFound { entity, id })
    }
}

/// `Ok(None)` for `QueryReturnedNoRows`, the error otherwise.
fn none_when_no_rows<T>(err: rusqlite::Error) -> std::result::Result<Option<T>, rusqlite::Error> {
    match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    }
}

fn validate_rel_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() {
        return Ok(()); // the root folder itself
    }
    if rel_path.starts_with('/') || rel_path.ends_with('/') {
        return Err(CatalogError::InvalidArg(format!(
            "rel_path must not start or end with '/': {rel_path:?}"
        )));
    }
    for seg in rel_path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(CatalogError::InvalidArg(format!(
                "rel_path contains an invalid segment: {rel_path:?}"
            )));
        }
    }
    Ok(())
}
