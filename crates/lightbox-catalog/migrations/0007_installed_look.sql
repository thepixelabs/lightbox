-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0007, E10 Phase D (task D10): the `installed_look` registry
-- user-installed (v1 ships no bundled packs, spec §4.8) creative-LUT packs
-- (`.cube`/HaldCLUT), spec §5.2. Files themselves live under
-- `<catalog>.lbdata/looks/<hh>/<hash>.<ext>` (relocatable with the data dir,
-- same fan-out convention the preview/raw caches use); this table stores
-- identity + provenance only, never LUT bytes (mirrors migration 0002's
-- `camera_profile` table / `profile_sync.rs`'s division of labor).
--
-- `content_hash` is the xxh3-128 hex of the installed file's bytes, the
-- install-idempotency key (installing the same bytes twice is a no-op, one
-- row) AND the `LookRef` a recipe's `Effects.creative_lut.id` field carries
-- (spec §4.8: "looks are referenced from recipes by content hash, never
-- path", survives rename/move; a missing hash degrades to identity + a
-- badge, never a broken reference).
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.

CREATE TABLE installed_look (
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL CHECK (kind IN ('cube3d', 'haldclut')),
  name          TEXT    NOT NULL,
  family        TEXT,                         -- browser grouping (e.g. "B&W", "Film")
  rel_path      TEXT    NOT NULL,              -- under <catalog>.lbdata/looks/
  content_hash  TEXT    NOT NULL UNIQUE,       -- xxh3-128 hex of the file; LookRef target
  source        TEXT    NOT NULL CHECK (source IN ('user', 'bundled')),
  license       TEXT,                          -- REQUIRED when source='bundled' (surface-3 manifest id)
  installed_at  INTEGER NOT NULL               -- unix seconds
);

CREATE INDEX idx_installed_look_family ON installed_look(family, name);
