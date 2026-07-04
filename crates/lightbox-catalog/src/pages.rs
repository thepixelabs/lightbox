// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Keyset pagination over `image ⋈ asset` (spec §3.2) — never `OFFSET`,
//! stable under concurrent inserts, O(page) at 100 k+.
//!
//! # How
//!
//! The total order for every [`SortOrder`] is `(sort key, asset.id,
//! image.id)`, which is exactly the walk order of a SQLite index on the sort
//! key (an index is really `(key, rowid)`) joined through `image_asset`
//! (`(asset_id, rowid)`), so pages come off an index seek without a sorter.
//!
//! Nullable sort keys (only `capture_time` at M0) split the order into two
//! **segments** — the `IS NULL` group (SQLite sorts NULLs first ASC / last
//! DESC) and the `IS NOT NULL` group — each fetched with its own indexed
//! query and stitched; a page never sees duplicates or gaps because the
//! cursor pins the position inside one segment and later segments start from
//! their beginning.

use lightbox_types::{AssetId, Flag, FolderId, ImageId, Orientation};

/// Sort orders offered by `images_page` (spec §3.2).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SortOrder {
    /// Capture time ascending (unknown capture times first).
    CaptureTimeAsc,
    /// Capture time descending (unknown capture times last).
    CaptureTimeDesc,
    /// Import (`added_at`) ascending.
    AddedAsc,
    /// Import (`added_at`) descending.
    AddedDesc,
    /// Filename ascending (byte order of the NFC-normalized name).
    FilenameAsc,
}

/// Opaque continuation token: last row's sort key + ids (spec §3.2). Only
/// valid for the same `(folder, sort)` it was produced with. Serializable so
/// headless callers (CLI) can round-trip it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageCursor {
    key: Option<String>,
    asset: i64,
    image: i64,
}

/// A page query (spec §3.2). `limit` is clamped to `1..=1000`.
#[derive(Clone, Debug)]
pub struct ImageQuery {
    /// Restrict to one folder (non-recursive), or the whole catalog.
    pub folder: Option<FolderId>,
    /// Sort order; also fixes the cursor semantics.
    pub sort: SortOrder,
    /// Continue after this position; `None` = first page.
    pub cursor: Option<PageCursor>,
    /// Page size, clamped to `1..=1000`.
    pub limit: u32,
}

/// One page of results plus the continuation cursor (spec §3.2).
#[derive(Clone, Debug)]
pub struct Page<T> {
    /// The rows, in query order.
    pub items: Vec<T>,
    /// Cursor for the next page; `None` when this was the last page.
    pub next: Option<PageCursor>,
}

/// Grid-row projection of an image (spec §3.2).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ImageSummary {
    /// The image row.
    pub id: ImageId,
    /// Its backing asset.
    pub asset: AssetId,
    /// Filename as on disk.
    pub filename: String,
    /// RFC3339 capture time, if known.
    pub capture_time: Option<String>,
    /// 1..=5 stars.
    pub rating: Option<u8>,
    /// Pick/reject flag.
    pub flag: Flag,
    /// Effective orientation (image override or asset EXIF value).
    pub orientation: Orientation,
    /// Full-size width (0 if unknown).
    pub width: u32,
    /// Full-size height (0 if unknown).
    pub height: u32,
    /// File currently missing on disk (fs reconciliation is E07).
    pub missing: bool,
    /// A probe/decode error is recorded on the asset (badged in the grid).
    pub decode_error: bool,
}

/// One ordered segment of a sort order (see module docs).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// This segment covers rows whose key IS NULL.
    pub null_key: bool,
    /// Walk direction.
    pub desc: bool,
}

impl SortOrder {
    /// The `asset` column (alias `a`) that keys this order.
    pub(crate) fn key_column(self) -> &'static str {
        match self {
            SortOrder::CaptureTimeAsc | SortOrder::CaptureTimeDesc => "a.capture_time",
            SortOrder::AddedAsc | SortOrder::AddedDesc => "a.added_at",
            SortOrder::FilenameAsc => "a.filename",
        }
    }

    /// Whether the key column is nullable in the schema.
    pub(crate) fn key_nullable(self) -> bool {
        matches!(self, SortOrder::CaptureTimeAsc | SortOrder::CaptureTimeDesc)
    }

    /// The ordered segments making up this sort order.
    pub(crate) fn segments(self) -> &'static [Segment] {
        const CAPTURE_ASC: &[Segment] = &[
            Segment {
                null_key: true,
                desc: false,
            },
            Segment {
                null_key: false,
                desc: false,
            },
        ];
        const CAPTURE_DESC: &[Segment] = &[
            Segment {
                null_key: false,
                desc: true,
            },
            Segment {
                null_key: true,
                desc: true,
            },
        ];
        const ASC: &[Segment] = &[Segment {
            null_key: false,
            desc: false,
        }];
        const DESC: &[Segment] = &[Segment {
            null_key: false,
            desc: true,
        }];
        match self {
            SortOrder::CaptureTimeAsc => CAPTURE_ASC,
            SortOrder::CaptureTimeDesc => CAPTURE_DESC,
            SortOrder::AddedAsc | SortOrder::FilenameAsc => ASC,
            SortOrder::AddedDesc => DESC,
        }
    }

    /// Which segment a cursor resumes in: `key: None` addresses the NULL
    /// segment, `Some` the non-NULL one.
    pub(crate) fn segment_index_for(self, cursor: &PageCursor) -> Option<usize> {
        self.segments()
            .iter()
            .position(|s| s.null_key == cursor.key.is_none())
    }
}

impl PageCursor {
    pub(crate) fn new(key: Option<String>, asset: i64, image: i64) -> PageCursor {
        PageCursor { key, asset, image }
    }

    pub(crate) fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub(crate) fn asset(&self) -> i64 {
        self.asset
    }

    pub(crate) fn image(&self) -> i64 {
        self.image
    }
}

/// Builds the SQL for one segment fetch. Pure — unit tests run
/// `EXPLAIN QUERY PLAN` over every shape (T10 AC: index-driven, no scans).
///
/// Parameter order (all `?N` positional, matching `push`es in `reader.rs`):
/// folder (if filtered), cursor values (if resuming), limit — see
/// `segment_params`.
pub(crate) fn segment_sql(
    sort: SortOrder,
    segment: Segment,
    folder_filtered: bool,
    resuming: bool,
) -> String {
    let key = sort.key_column();
    let mut sql = String::from(
        "SELECT i.id, i.asset_id, a.filename, a.capture_time, i.rating, i.flag, \
         COALESCE(i.orientation, a.orientation), a.width, a.height, a.missing, \
         a.decode_error IS NOT NULL, ",
    );
    sql.push_str(key);
    // CROSS JOIN pins the join order (SQLite treats it as an optimizer
    // directive): `asset` must be the outer loop so the sort-key index walk
    // provides the ORDER BY and the page comes off an index seek, not a
    // sorter — without table statistics the planner otherwise guesses.
    sql.push_str(", a.id FROM asset a CROSS JOIN image i ON i.asset_id = a.id WHERE ");

    let mut conds: Vec<String> = Vec::new();
    if folder_filtered {
        conds.push("a.folder_id = ?".to_owned());
    }
    if segment.null_key {
        conds.push(format!("{key} IS NULL"));
        if resuming {
            // (a.id, i.id) beyond the cursor. The redundant `a.id >= ?`
            // conjunct is the index-seekable range; the rest is residual.
            if segment.desc {
                conds.push("a.id <= ? AND (a.id < ? OR i.id < ?)".to_owned());
            } else {
                conds.push("a.id >= ? AND (a.id > ? OR i.id > ?)".to_owned());
            }
        }
    } else {
        if sort.key_nullable() && !resuming {
            conds.push(format!("{key} IS NOT NULL"));
        }
        if resuming {
            // (key, a.id, i.id) beyond the cursor; `key >=/<= ?` is the
            // seekable range (it also excludes NULL keys by comparison
            // semantics), the parenthesized part is residual and only
            // touches the cursor's duplicate-key group.
            if segment.desc {
                conds.push(format!(
                    "{key} <= ? AND ({key} < ? OR a.id < ? OR (a.id = ? AND i.id < ?))"
                ));
            } else {
                conds.push(format!(
                    "{key} >= ? AND ({key} > ? OR a.id > ? OR (a.id = ? AND i.id > ?))"
                ));
            }
        }
    }
    if conds.is_empty() {
        // e.g. first page of FilenameAsc over the whole catalog.
        conds.push("1=1".to_owned());
    }
    sql.push_str(&conds.join(" AND "));

    let dir = if segment.desc { "DESC" } else { "ASC" };
    if segment.null_key {
        sql.push_str(&format!(" ORDER BY a.id {dir}, i.id {dir}"));
    } else {
        sql.push_str(&format!(" ORDER BY {key} {dir}, a.id {dir}, i.id {dir}"));
    }
    sql.push_str(" LIMIT ?");
    sql
}

/// Parameter values matching [`segment_sql`], in order.
pub(crate) fn segment_params(
    segment: Segment,
    folder: Option<FolderId>,
    cursor: Option<&PageCursor>,
    limit: i64,
) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    let mut params: Vec<Value> = Vec::new();
    if let Some(f) = folder {
        params.push(Value::Integer(f.0));
    }
    if let Some(c) = cursor {
        if segment.null_key {
            params.push(Value::Integer(c.asset()));
            params.push(Value::Integer(c.asset()));
            params.push(Value::Integer(c.image()));
        } else {
            let key = c
                .key()
                .expect("non-NULL segment cursor carries a key")
                .to_owned();
            params.push(Value::Text(key.clone()));
            params.push(Value::Text(key));
            params.push(Value::Integer(c.asset()));
            params.push(Value::Integer(c.asset()));
            params.push(Value::Integer(c.image()));
        }
    }
    params.push(Value::Integer(limit));
    params
}
