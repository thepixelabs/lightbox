-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0005, E04 open-in-place intake (architecture §3.1.1, E04 spec
-- §5.1): asset carries its absolute path directly; folder becomes
-- keep-dormant for the managed-import path only (E01's import_add_in_place
-- stays compiled and tested, spec §10.0, it just isn't reachable from the
-- open path).
--
-- Table rebuild per the documented SQLite ALTER procedure, applied by the
-- migration runner's `rebuilds_tables` flag (src/migrate.rs, E04 T2): the
-- runner wraps this file with PRAGMA foreign_keys=OFF / foreign_key_check /
-- foreign_keys=ON *outside* this batch, because a naive `DROP TABLE asset`
-- would otherwise fire the implicit CASCADE delete through image.asset_id
-- REFERENCES asset(id) ON DELETE CASCADE, and, one level further,
-- edit_recipe/edit_index/history_step/snapshot/xmp_sync/preview all chain
-- ON DELETE CASCADE off `image`/`asset` too (migrations 0003/0004). Without
-- the FK-off wrapper this single DROP TABLE would silently erase the entire
-- edit store.
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.

CREATE TABLE asset_new (
  id                INTEGER PRIMARY KEY,
  folder_id         INTEGER REFERENCES folder(id),   -- was NOT NULL; NULL = opened in place
  abs_path          TEXT,                            -- open-in-place path HINT (identity is content_hash);
                                                       -- NULL for legacy managed rows
  filename          TEXT    NOT NULL,
  content_hash      BLOB    NOT NULL,
  format            TEXT    NOT NULL,
  camera_make       TEXT,
  camera_model      TEXT,
  capture_time      TEXT,
  width             INTEGER NOT NULL DEFAULT 0,
  height            INTEGER NOT NULL DEFAULT 0,
  orientation       INTEGER NOT NULL DEFAULT 1,
  bytes             INTEGER NOT NULL,
  mtime_utc         TEXT,
  missing           INTEGER NOT NULL DEFAULT 0,
  decode_error      TEXT,
  import_session_id INTEGER REFERENCES import_session(id),
  added_at          TEXT    NOT NULL,
  decode_backend    TEXT,                            -- E02 (0002_e02_color) diagnostics column, preserved
  camera            TEXT    GENERATED ALWAYS AS
                      (trim(coalesce(camera_make,'') || ' ' || coalesce(camera_model,''))) VIRTUAL
  -- UNIQUE (folder_id, filename) is intentionally DROPPED: it guarded the
  -- managed tree; open-in-place rows have NULL folder_id (vacuous under
  -- SQLite NULL-distinct semantics) and identity is content_hash.
);

INSERT INTO asset_new (id, folder_id, abs_path, filename, content_hash, format,
                       camera_make, camera_model, capture_time, width, height,
                       orientation, bytes, mtime_utc, missing, decode_error,
                       import_session_id, added_at, decode_backend)
  SELECT id, folder_id, NULL, filename, content_hash, format,
         camera_make, camera_model, capture_time, width, height,
         orientation, bytes, mtime_utc, missing, decode_error,
         import_session_id, added_at, decode_backend
  FROM asset;

DROP TABLE asset;                     -- FTS triggers on asset drop with it
ALTER TABLE asset_new RENAME TO asset;

-- Recreate the 0001 indexes (ids preserved, so image.asset_id is intact):
CREATE INDEX asset_hash           ON asset(content_hash);   -- still NON-unique (E04 spec §3.3 rationale)
CREATE INDEX asset_capture        ON asset(capture_time);
CREATE INDEX asset_session        ON asset(import_session_id);
CREATE INDEX asset_added          ON asset(added_at);
CREATE INDEX asset_filename       ON asset(filename);
CREATE INDEX asset_folder_capture ON asset(folder_id, capture_time);
CREATE INDEX asset_folder_added   ON asset(folder_id, added_at);
CREATE INDEX asset_abs_path       ON asset(abs_path);       -- stale-claimant clearing (E04 spec §4.4 step 4)
-- Deviation from the E04 spec §5.1 illustrative index list (recorded in
-- E04-deviations.md): dropping UNIQUE(folder_id, filename) also drops the
-- implicit index SQLite maintained for it, regressing the pre-existing
-- FilenameAsc + folder-filter `images_page` query plan from an index seek to
-- a sorter (`USE TEMP B-TREE FOR ORDER BY`), caught by E01/E03's own
-- `every_page_query_plan_is_index_driven` test (T10 AC). A plain (non-
-- unique) index restores the query-plan shape without reinstating the
-- managed-tree uniqueness guard the UNIQUE constraint used to enforce.
CREATE INDEX asset_folder_filename ON asset(folder_id, filename);

-- Recreate the FTS5 external-content triggers verbatim from 0001, external-
-- content FTS references the content table by NAME, so the rename above
-- leaves it pointed correctly, but a rebuild re-proves integrity.
CREATE TRIGGER asset_fts_ai AFTER INSERT ON asset BEGIN
  INSERT INTO assets_fts(rowid, filename, camera)
  VALUES (new.id, new.filename,
          trim(coalesce(new.camera_make,'') || ' ' || coalesce(new.camera_model,'')));
END;
CREATE TRIGGER asset_fts_ad AFTER DELETE ON asset BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, filename, camera)
  VALUES ('delete', old.id, old.filename,
          trim(coalesce(old.camera_make,'') || ' ' || coalesce(old.camera_model,'')));
END;
CREATE TRIGGER asset_fts_au AFTER UPDATE OF filename, camera_make, camera_model ON asset BEGIN
  INSERT INTO assets_fts(assets_fts, rowid, filename, camera)
  VALUES ('delete', old.id, old.filename,
          trim(coalesce(old.camera_make,'') || ' ' || coalesce(old.camera_model,'')));
  INSERT INTO assets_fts(rowid, filename, camera)
  VALUES (new.id, new.filename,
          trim(coalesce(new.camera_make,'') || ' ' || coalesce(new.camera_model,'')));
END;

INSERT INTO assets_fts(assets_fts) VALUES('rebuild');
