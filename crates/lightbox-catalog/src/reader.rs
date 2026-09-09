// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! WAL-snapshot reader pool + query DAOs (spec §3.2 `ReaderHandle`).
//!
//! `N = min(cores, 4)` read connections, each `PRAGMA query_only = ON`
//! (spec §4.2). Readers see committed WAL snapshots and never block, or are
//! blocked by, the single writer. No SQL leaks upward.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use lightbox_types::{
    AssetId, ContentHash, Flag, FolderId, ImageId, ImportSessionId, Orientation, RootId,
};
use rusqlite::{params, params_from_iter, Connection};

use crate::error::{CatalogError, Result};
use crate::pages::{segment_params, segment_sql, ImageQuery, ImageSummary, Page, PageCursor};

/// Everything `image_detail` knows about one image (spec §3.2).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ImageDetail {
    /// The image row.
    pub id: ImageId,
    /// Its backing asset.
    pub asset: AssetId,
    /// The asset's folder. `None` for an open-in-place row (E04 spec §4.4:
    /// `folder_id` is nullable as of migration `0005_open_in_place`, the
    /// one frozen-DTO change E04 makes to this struct, per the E01
    /// joint-review rule; grep-verified no consumer outside this crate reads
    /// `.folder` as of E04).
    pub folder: Option<FolderId>,
    /// Filename as on disk.
    pub filename: String,
    /// Probed format tag (`'CR3'`, …, `'UNSUPPORTED'`).
    pub format: String,
    /// EXIF camera make.
    pub camera_make: Option<String>,
    /// EXIF camera model.
    pub camera_model: Option<String>,
    /// RFC3339 capture time.
    pub capture_time: Option<String>,
    /// Full-size width (0 if unknown).
    pub width: u32,
    /// Full-size height (0 if unknown).
    pub height: u32,
    /// Effective orientation (image override or asset EXIF value).
    pub orientation: Orientation,
    /// 1..=5 stars.
    pub rating: Option<u8>,
    /// Pick/reject flag.
    pub flag: Flag,
    /// Color label (M0: pass-through column, UX is E07).
    pub label: Option<String>,
    /// Virtual copy? (Always `false` until E07.)
    pub is_virtual: bool,
    /// Virtual-copy name.
    pub name: Option<String>,
    /// Pinned process version (architecture §4.5).
    pub process_version: lightbox_types::ProcessVersion,
    /// File size in bytes.
    pub bytes: u64,
    /// File mtime, RFC3339 UTC.
    pub mtime_utc: Option<String>,
    /// When the asset entered the catalog (RFC3339 UTC).
    pub added_at: String,
    /// File currently missing on disk.
    pub missing: bool,
    /// Probe/decode failure message, if any.
    pub decode_error: Option<String>,
    /// Import session the asset arrived in.
    pub import_session: Option<ImportSessionId>,
}

/// One node of the folder tree (spec §3.2), flat with parent links, sorted
/// by `(root, rel_path)`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FolderNode {
    /// The folder row.
    pub id: FolderId,
    /// Library root it belongs to.
    pub root: RootId,
    /// Parent folder (`None` for the `rel_path = ''` root folder).
    pub parent: Option<FolderId>,
    /// `'/'`-separated path relative to the root (`''` = the root itself).
    pub rel_path: String,
    /// Last path segment (`''` for the root folder).
    pub name: String,
}

/// Row counts across the spine tables (spec §3.2).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CatalogCounts {
    /// `library_root` rows.
    pub roots: u64,
    /// `folder` rows.
    pub folders: u64,
    /// `asset` rows.
    pub assets: u64,
    /// `image` rows.
    pub images: u64,
    /// `import_session` rows.
    pub import_sessions: u64,
}

pub(crate) struct ReaderPool {
    conns: Mutex<Vec<Connection>>,
    available: Condvar,
}

impl ReaderPool {
    pub(crate) fn new(conns: Vec<Connection>) -> ReaderPool {
        ReaderPool {
            conns: Mutex::new(conns),
            available: Condvar::new(),
        }
    }

    fn checkout(self: &Arc<Self>) -> Connection {
        let mut guard = self
            .conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(conn) = guard.pop() {
                return conn;
            }
            guard = self
                .available
                .wait(guard)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn checkin(&self, conn: Connection) {
        self.conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(conn);
        self.available.notify_one();
    }
}

/// A checked-out read connection (spec §3.2). Blocks other checkouts while
/// held once the pool is exhausted, drop it when done. Queries run on the
/// calling thread against a committed WAL snapshot.
pub struct ReaderHandle {
    conn: Option<Connection>,
    pool: Arc<ReaderPool>,
}

impl ReaderHandle {
    pub(crate) fn checkout(pool: &Arc<ReaderPool>) -> ReaderHandle {
        ReaderHandle {
            conn: Some(pool.checkout()),
            pool: Arc::clone(pool),
        }
    }

    pub(crate) fn conn(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("reader connection present until drop")
    }

    /// One page of images by keyset cursor (spec §3.2; see `pages.rs` for
    /// the segment machinery). Never `OFFSET`.
    pub fn images_page(&self, q: &ImageQuery) -> Result<Page<ImageSummary>> {
        let limit = q.limit.clamp(1, 1000) as usize;
        let segments = q.sort.segments();
        let start = match &q.cursor {
            None => 0,
            Some(c) => q.sort.segment_index_for(c).ok_or_else(|| {
                CatalogError::InvalidArg("cursor does not match this sort order".into())
            })?,
        };

        // Over-fetch by one row to learn whether a next page exists without
        // handing out a cursor that would resolve to an empty page.
        let want = limit + 1;
        let mut rows: Vec<(ImageSummary, Option<String>, i64)> = Vec::with_capacity(want);
        for (idx, seg) in segments.iter().enumerate().skip(start) {
            let resuming = idx == start && q.cursor.is_some();
            let cursor = if resuming { q.cursor.as_ref() } else { None };
            let sql = segment_sql(q.sort, *seg, q.folder.is_some(), resuming);
            let params = segment_params(*seg, q.folder, cursor, (want - rows.len()) as i64);
            let mut stmt = self.conn().prepare_cached(&sql)?;
            let mapped = stmt.query_map(params_from_iter(params), map_summary_row)?;
            for row in mapped {
                rows.push(row?);
            }
            if rows.len() >= want {
                break;
            }
        }

        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last()
                .map(|(s, key, asset)| PageCursor::new(key.clone(), *asset, s.id.0))
        } else {
            None
        };
        Ok(Page {
            items: rows.into_iter().map(|(s, _, _)| s).collect(),
            next,
        })
    }

    /// Full detail for one image (spec §3.2).
    pub fn image_detail(&self, id: ImageId) -> Result<ImageDetail> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT i.id, i.asset_id, a.folder_id, a.filename, a.format, a.camera_make, \
             a.camera_model, a.capture_time, a.width, a.height, \
             COALESCE(i.orientation, a.orientation), i.rating, i.flag, i.label, i.is_virtual, \
             i.name, i.process_version, a.bytes, a.mtime_utc, a.added_at, a.missing, \
             a.decode_error, a.import_session_id \
             FROM image i JOIN asset a ON a.id = i.asset_id WHERE i.id = ?1",
        )?;
        stmt.query_row(params![id.0], |r| {
            Ok(ImageDetail {
                id: ImageId(r.get(0)?),
                asset: AssetId(r.get(1)?),
                folder: r.get::<_, Option<i64>>(2)?.map(FolderId),
                filename: r.get(3)?,
                format: r.get(4)?,
                camera_make: r.get(5)?,
                camera_model: r.get(6)?,
                capture_time: r.get(7)?,
                width: r.get(8)?,
                height: r.get(9)?,
                orientation: orientation_from_db(r.get(10)?),
                rating: r.get(11)?,
                flag: flag_from_db(r.get(12)?),
                label: r.get(13)?,
                is_virtual: r.get(14)?,
                name: r.get(15)?,
                process_version: lightbox_types::ProcessVersion(r.get(16)?),
                bytes: r.get::<_, i64>(17)? as u64,
                mtime_utc: r.get(18)?,
                added_at: r.get(19)?,
                missing: r.get(20)?,
                decode_error: r.get(21)?,
                import_session: r.get::<_, Option<i64>>(22)?.map(ImportSessionId),
            })
        })
        .map_err(not_found_or("image", id.0))
    }

    /// The whole folder tree, flat, sorted by `(root, rel_path)` (spec §3.2).
    pub fn folder_tree(&self) -> Result<Vec<FolderNode>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT id, root_id, parent_id, rel_path FROM folder ORDER BY root_id, rel_path",
        )?;
        let rows = stmt.query_map([], |r| {
            let rel_path: String = r.get(3)?;
            let name = rel_path.rsplit('/').next().unwrap_or("").to_owned();
            Ok(FolderNode {
                id: FolderId(r.get(0)?),
                root: RootId(r.get(1)?),
                parent: r.get::<_, Option<i64>>(2)?.map(FolderId),
                rel_path,
                name,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Absolute path of an asset (E04 spec §4.4): prefers the `abs_path`
    /// path-hint column (open-in-place rows, migration `0005`); falls back
    /// to `root.path ⊕ folder.rel_path ⊕ filename` for legacy managed rows
    /// with no hint (`LEFT JOIN`s so a NULL-`folder_id` row with an
    /// `abs_path` still resolves without the join ever running). The E01
    /// `AssetLocator`/preview/render paths pick this up with zero changes.
    pub fn asset_abs_path(&self, id: AssetId) -> Result<PathBuf> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT a.abs_path, r.path, f.rel_path, a.filename FROM asset a \
             LEFT JOIN folder f ON f.id = a.folder_id \
             LEFT JOIN library_root r ON r.id = f.root_id WHERE a.id = ?1",
        )?;
        let (abs_path, root, rel, filename): (
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = stmt
            .query_row(params![id.0], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(not_found_or("asset", id.0))?;
        if let Some(abs) = abs_path {
            return Ok(PathBuf::from(abs));
        }
        let (root, rel) = match (root, rel) {
            (Some(root), Some(rel)) => (root, rel),
            _ => {
                return Err(CatalogError::Internal(format!(
                    "asset {} has neither an abs_path hint nor a folder to compose a path from",
                    id.0
                )))
            }
        };
        let mut path = PathBuf::from(root);
        for seg in rel.split('/').filter(|s| !s.is_empty()) {
            path.push(seg);
        }
        path.push(filename);
        Ok(path)
    }

    /// An asset's content hash (spec §3.1: the dominant component of E03's
    /// preview store key, `lightbox-preview`'s T0 producer denormalizes it
    /// onto every `preview` row it writes). Additive read-only accessor, not
    /// part of E01's original §3.2 surface.
    pub fn asset_content_hash(&self, id: AssetId) -> Result<ContentHash> {
        let bytes: Vec<u8> = self
            .conn()
            .query_row(
                "SELECT content_hash FROM asset WHERE id = ?1",
                params![id.0],
                |r| r.get(0),
            )
            .map_err(not_found_or("asset", id.0))?;
        Ok(ContentHash(bytes.try_into().unwrap_or([0u8; 16])))
    }

    /// Row counts across the spine tables (spec §3.2).
    pub fn counts(&self) -> Result<CatalogCounts> {
        let count = |sql: &str| -> Result<u64> {
            let n: i64 = self.conn().query_row(sql, [], |r| r.get(0))?;
            Ok(n as u64)
        };
        Ok(CatalogCounts {
            roots: count("SELECT COUNT(*) FROM library_root")?,
            folders: count("SELECT COUNT(*) FROM folder")?,
            assets: count("SELECT COUNT(*) FROM asset")?,
            images: count("SELECT COUNT(*) FROM image")?,
            import_sessions: count("SELECT COUNT(*) FROM import_session")?,
        })
    }

    /// FTS5 prefix search over filenames + camera (spec §3.2). User input is
    /// tokenized and quoted here, no FTS query syntax crosses the boundary.
    /// Results are best-match (bm25) first.
    pub fn search_filenames(&self, query: &str, limit: u32) -> Result<Vec<ImageId>> {
        let Some(fts_query) = build_prefix_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn().prepare_cached(
            "SELECT i.id FROM assets_fts JOIN image i ON i.asset_id = assets_fts.rowid \
             WHERE assets_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, i64::from(limit)], |r| {
            Ok(ImageId(r.get(0)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

impl Drop for ReaderHandle {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.checkin(conn);
        }
    }
}

type SummaryRow = (ImageSummary, Option<String>, i64);

fn map_summary_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
    let summary = ImageSummary {
        id: ImageId(r.get(0)?),
        asset: AssetId(r.get(1)?),
        filename: r.get(2)?,
        capture_time: r.get(3)?,
        rating: r.get(4)?,
        flag: flag_from_db(r.get(5)?),
        orientation: orientation_from_db(r.get(6)?),
        width: r.get(7)?,
        height: r.get(8)?,
        missing: r.get(9)?,
        decode_error: r.get(10)?,
    };
    let sort_key: Option<String> = r.get(11)?;
    let asset_id: i64 = r.get(12)?;
    Ok((summary, sort_key, asset_id))
}

/// Schema values outside the encoding are a bug, not user input; readers
/// degrade to defaults rather than failing a whole page.
fn flag_from_db(v: i64) -> Flag {
    Flag::from_db(v).unwrap_or_default()
}

fn orientation_from_db(v: i64) -> Orientation {
    u16::try_from(v)
        .ok()
        .and_then(Orientation::from_exif)
        .unwrap_or_default()
}

fn not_found_or(entity: &'static str, id: i64) -> impl FnOnce(rusqlite::Error) -> CatalogError {
    move |err| match err {
        rusqlite::Error::QueryReturnedNoRows => CatalogError::NotFound { entity, id },
        other => other.into(),
    }
}

/// `"IMG_12 nacht"` → `"IMG"* "12"* "nacht"*`, the input is split exactly
/// where the `unicode61` tokenizer splits (non-alphanumeric), every fragment
/// becomes a quoted prefix term. `None` when nothing searchable remains.
fn build_prefix_query(input: &str) -> Option<String> {
    let parts: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_query_shapes() {
        assert_eq!(build_prefix_query(""), None);
        assert_eq!(build_prefix_query("  \" *? "), None);
        assert_eq!(build_prefix_query("IMG"), Some("\"IMG\"*".to_owned()));
        assert_eq!(
            build_prefix_query("IMG_12 nacht"),
            Some("\"IMG\"* \"12\"* \"nacht\"*".to_owned())
        );
        // Quotes and FTS operators cannot break out of the quoted tokens.
        assert_eq!(
            build_prefix_query("a\"b OR x NOT()"),
            Some("\"a\"* \"b\"* \"OR\"* \"x\"* \"NOT\"*".to_owned())
        );
    }
}
