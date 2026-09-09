-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0003, the edit state (the primary data model, E09 §3.1.1 / §4.1):
-- versioned edit recipe, derived filmstrip index, persistent history step log,
-- named snapshots, and per-asset sidecar sync bookkeeping.
--
-- Forward-only; applied inside a single transaction by the migration runner
-- (src/migrate.rs). `foreign_keys=ON` is set at every connection open, so the
-- ON DELETE CASCADE chains here fire on `image`/`asset` deletion.
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.
-- Timestamps are TEXT RFC3339 fixed-width UTC (clock::now_rfc3339_utc), the
-- E01 Phase-3 convention (lexicographic == chronological).

-- The authoritative recipe (§3.2), one per edited image. An untouched image
-- has NO row (E09 decision D1: persist-on-first-edit, not on mere open).
CREATE TABLE edit_recipe (
  image_id    INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
  pv          INTEGER NOT NULL,           -- ProcessVersion; == image.process_version (§4.5, DAO-asserted)
  schema      INTEGER NOT NULL,           -- RECIPE_SCHEMA at write time
  doc         BLOB    NOT NULL,           -- CBOR Recipe (authoritative, §3.2)
  head_seq    INTEGER NOT NULL DEFAULT 0, -- history position `doc` corresponds to (recipe_at(head_seq)==doc)
  updated_at  TEXT    NOT NULL            -- RFC3339 UTC
);

-- Derived filmstrip index, rebuilt in the SAME txn as `doc` (§3.1 single write
-- path). `is_edited` badges the filmstrip; has_masks/has_ai_mask are reserved
-- for E12/E14.
CREATE TABLE edit_index (
  image_id    INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
  is_edited   INTEGER NOT NULL DEFAULT 0,
  has_masks   INTEGER NOT NULL DEFAULT 0,  -- reserved; E12 populates
  has_ai_mask INTEGER NOT NULL DEFAULT 0,  -- reserved; E14 populates
  crop_ratio  REAL,                        -- normalized crop aspect (NULL = full frame)
  treatment   TEXT,                        -- 'color' | 'bw'
  updated_at  TEXT    NOT NULL
);

-- The persistent history step log (§4.1). Each committed gesture is one row;
-- `delta`/`inverse` buy O(distance) stepping and O(1) undo, keyframes buy
-- bounded reconstruction. State at seq 0 == neutral default.
CREATE TABLE history_step (
  id           INTEGER PRIMARY KEY,
  image_id     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  seq          INTEGER NOT NULL,          -- 1..n, dense per image
  op           BLOB    NOT NULL,          -- CBOR StepLabel
  delta        BLOB    NOT NULL,          -- CBOR ParamDelta (forward)
  inverse      BLOB    NOT NULL,          -- CBOR ParamDelta (backward)
  keyframe_doc BLOB,                      -- full CBOR Recipe at anchor steps, else NULL
  ts           TEXT    NOT NULL,
  UNIQUE (image_id, seq)
);
CREATE INDEX history_step_image ON history_step(image_id, seq);

-- Named snapshots (§3.1): SELF-CONTAINED materialized projections, immutable
-- copies (not live owners), the E12 SnapshotMaterializer seam inlines
-- mask/retouch content into `recipe_doc` when it lands.
CREATE TABLE snapshot (
  id         INTEGER PRIMARY KEY,
  image_id   INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  name       TEXT    NOT NULL,
  recipe_doc BLOB    NOT NULL,
  ts         TEXT    NOT NULL,
  UNIQUE (image_id, name)
);
CREATE INDEX snapshot_image ON snapshot(image_id);

-- Per-asset sidecar sync bookkeeping (§3.1.1). NO `divergent` column, status
-- is COMPUTED at open/write/refresh (no fs-watcher in v2.0), never stored.
CREATE TABLE xmp_sync (
  asset_id        INTEGER PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
  sidecar_hash    BLOB,   -- xxh3-128 of sidecar bytes at our last write/read
  sidecar_mtime   TEXT,   -- disk mtime at that moment (cheap pre-check)
  recipe_hash     BLOB,   -- Recipe::canonical_hash at that moment
  last_written_at TEXT,   -- by us (WriteMetadata / auto-write)
  last_read_at    TEXT    -- into the edit store (ReadMetadata)
);
