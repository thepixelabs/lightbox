-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0001, the spine schema (E01 spec §4.3).
--
-- Forward-only; applied inside a single transaction by the migration runner
-- (src/migrate.rs). Later epics add their own tables under numbers reserved
-- in docs/plan/migrations.md (E03: preview; E07: collections/keywords;
-- E09: edit recipes/history; E12: masks; E13/E14: embeddings/faces).
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.

CREATE TABLE schema_version (
  version     INTEGER PRIMARY KEY,
  applied_at  TEXT    NOT NULL,                 -- RFC3339 UTC
  description TEXT    NOT NULL
);

CREATE TABLE library_root (
  id           INTEGER PRIMARY KEY,
  volume_uuid  TEXT,                            -- NULL when the platform can't provide one
  path         TEXT    NOT NULL,                -- absolute, platform-native, UTF-8
  UNIQUE (volume_uuid, path)
);

CREATE TABLE folder (
  id        INTEGER PRIMARY KEY,
  root_id   INTEGER NOT NULL REFERENCES library_root(id),
  parent_id INTEGER REFERENCES folder(id),
  rel_path  TEXT    NOT NULL,                   -- '' for the root folder itself; '/'-separated
  UNIQUE (root_id, rel_path)
);
CREATE INDEX folder_parent ON folder(parent_id);

CREATE TABLE import_session (
  id          INTEGER PRIMARY KEY,
  started_at  TEXT NOT NULL,
  finished_at TEXT,
  source      TEXT NOT NULL,                    -- source dir/device description
  mode        TEXT NOT NULL DEFAULT 'add',      -- M0: 'add' only; E04 adds copy/move/dng
  options     TEXT NOT NULL DEFAULT '{}',       -- JSON (ImportOptions is #[non_exhaustive])
  stats       TEXT                              -- JSON ImportReport
);

CREATE TABLE asset (
  id                INTEGER PRIMARY KEY,
  folder_id         INTEGER NOT NULL REFERENCES folder(id),
  filename          TEXT    NOT NULL,           -- as on disk, NFC-normalized for match/display
  content_hash      BLOB    NOT NULL,           -- 16 bytes, xxh3-128 of the full file
  format            TEXT    NOT NULL,           -- 'CR3','NEF','JPEG',…,'UNSUPPORTED'
  camera_make       TEXT,
  camera_model      TEXT,
  capture_time      TEXT,                       -- RFC3339; lexicographic == chronological (UTC-normalized)
  width             INTEGER NOT NULL DEFAULT 0,
  height            INTEGER NOT NULL DEFAULT 0,
  orientation       INTEGER NOT NULL DEFAULT 1, -- EXIF 1..8
  bytes             INTEGER NOT NULL,
  mtime_utc         TEXT,
  missing           INTEGER NOT NULL DEFAULT 0, -- bool; fs-reconciliation is E07
  decode_error      TEXT,                       -- NULL = fine; message otherwise (§6)
  import_session_id INTEGER REFERENCES import_session(id),
  added_at          TEXT    NOT NULL,
  -- Generated column backing the FTS5 external-content `camera` column
  -- (recorded as an E01 Phase 3 deviation): FTS5 external content requires
  -- the content table to expose a column per FTS column, and the spec's
  -- assets_fts declares `camera` as the concatenation below.
  camera            TEXT    GENERATED ALWAYS AS
                      (trim(coalesce(camera_make,'') || ' ' || coalesce(camera_model,''))) VIRTUAL,
  UNIQUE (folder_id, filename)
);
CREATE INDEX asset_hash    ON asset(content_hash);   -- dup-skip + relink + cache keys
CREATE INDEX asset_capture ON asset(capture_time);
CREATE INDEX asset_session ON asset(import_session_id);

-- Keyset-pagination indexes beyond the spec §4.3 listing (recorded as an E01
-- Phase 3 deviation): T10's acceptance criterion requires every images_page
-- plan to be index-driven for every SortOrder × folder-filter combination.
-- (folder_id, filename) is already covered by the UNIQUE constraint above.
CREATE INDEX asset_added          ON asset(added_at);
CREATE INDEX asset_filename       ON asset(filename);
CREATE INDEX asset_folder_capture ON asset(folder_id, capture_time);
CREATE INDEX asset_folder_added   ON asset(folder_id, added_at);

CREATE TABLE image (
  id              INTEGER PRIMARY KEY,
  asset_id        INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
  is_virtual      INTEGER NOT NULL DEFAULT 0,   -- VCs (E07) fall out of this row model
  name            TEXT,                         -- virtual-copy name
  orientation     INTEGER,                      -- NULL = inherit asset.orientation
  rating          INTEGER CHECK (rating BETWEEN 1 AND 5),
  flag            INTEGER NOT NULL DEFAULT 0,   -- -1 reject / 0 none / 1 pick
  label           TEXT,
  process_version INTEGER NOT NULL DEFAULT 1,   -- §4.5: immutable per image w/o explicit migrate
  created_at      TEXT    NOT NULL
);
CREATE INDEX image_asset ON image(asset_id);

-- FTS5, external-content over asset. M0 columns: filename + camera. E07 expands
-- (keywords, caption) with a migration that rebuilds the index, designed for that.
CREATE VIRTUAL TABLE assets_fts USING fts5(
  filename, camera,
  content='asset', content_rowid='id',
  tokenize = "unicode61 remove_diacritics 2"
);
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
