-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0002, E02 color foundation (E02 spec §4.1).
--
-- Forward-only; applied inside a single transaction by the migration runner
-- (src/migrate.rs). Adds:
--   * `camera_profile`, the registry of installed color assets (bundled
--     looks + curated/user DCPs). Files live in app resources / the user
--     profile dir; this table stores identity + provenance, never blobs.
--     Bundled entries are re-synced idempotently on catalog open (see
--     src/profile_sync.rs) keyed on the stable ProfileId, exactly like the
--     model_pack pattern E13 will follow.
--   * `asset.decode_backend`, diagnostics column recording which backend
--     produced the accepted decode (libraw_proxy | in_crate | an image-codec
--     id). NULL until a decode runs (E04 fills it).
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.

CREATE TABLE camera_profile (
  id            TEXT PRIMARY KEY,          -- ProfileId hex (xxh3-128 of canonical bytes)
  kind          TEXT NOT NULL CHECK (kind IN ('dcp', 'look')),
  name          TEXT NOT NULL,
  camera_make   TEXT,                      -- NULL for looks
  camera_model  TEXT,                      -- normalized (CameraId)
  source        TEXT NOT NULL CHECK (source IN ('bundled', 'user')),
  license       TEXT NOT NULL,             -- mirrors the surface-3 manifest entry
  file_path     TEXT NOT NULL,
  file_hash     TEXT NOT NULL,
  installed_at  INTEGER NOT NULL,          -- unixepoch
  UNIQUE (kind, name, camera_make, camera_model)
);
CREATE INDEX idx_camera_profile_camera ON camera_profile (camera_make, camera_model);

-- Diagnostics: which backend produced the accepted decode. E09's
-- edit_recipe.doc.base_profile references camera_profile.id (schema frozen in
-- architecture §3.2); resolution + fallback semantics are lightbox-color's
-- resolve_profile_ref (E02 spec §3.4). No changes to preview/edit_recipe/mask
-- tables here.
ALTER TABLE asset ADD COLUMN decode_backend TEXT;
