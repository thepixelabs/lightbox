-- SPDX-FileCopyrightText: 2026 Lightbox contributors
-- SPDX-License-Identifier: Apache-2.0
--
-- Migration 0004 — the preview pyramid index + raw-cache accounting
-- (E03 spec §4). `preview` rows are pure cache index: the underlying pixels
-- live in the sibling `.lbdata` cache store (lightbox-preview), keyed by
-- content hash, so this table is fully disposable/reconstructible (E03 §3.2:
-- `verify_store` reconciles orphans/missing rows on either side).
--
-- Forward-only; applied inside a single transaction by the migration runner
-- (src/migrate.rs). `foreign_keys=ON` is set at every connection open, so the
-- ON DELETE CASCADE chains here fire on `image`/`asset` deletion.
--
-- NOTE for authors: no BEGIN/COMMIT here — the runner owns the transaction.
--
-- Deviation from the E01/E09 timestamp convention: `built_at`/`last_used_at`
-- are INTEGER unix seconds here, not RFC3339 TEXT. This is deliberate (E03
-- spec §3.2 "Accounting write pressure" / §4): `last_used_at` is touched on
-- every cull/scroll and batched (≤5s / ≥64 entries) off the interactive
-- path, so cheap numeric comparison/storage matters here in a way it does
-- not for the rest of the catalog. Recorded in E03-deviations.md.
--
-- No prior `preview` stub table exists on this schema head (E01's 0001_spine
-- never added one, despite the architecture §3.3 narrative expecting it) —
-- `DROP TABLE IF EXISTS` is a no-op today and kept only so the "recreate,
-- don't ALTER" policy this migration documents holds if a future migration
-- needs to reshape this table again.

DROP TABLE IF EXISTS preview;
CREATE TABLE preview (
  id            INTEGER PRIMARY KEY,
  asset_id      INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
  image_id      INTEGER REFERENCES image(id) ON DELETE CASCADE,  -- NULL = asset-scope (T0)
  content_hash  BLOB    NOT NULL,          -- xxh3-128, 16 bytes (denormalized for relink survival)
  tier          INTEGER NOT NULL,           -- 0 | 1 | 2
  variant_hash  BLOB    NOT NULL,           -- xxh3-64, 8 bytes
  source        TEXT    NOT NULL CHECK (source IN ('embedded','rendered')),
  recipe_rev    INTEGER NOT NULL DEFAULT 0, -- edit revision reflected; 0 = edit-independent
  stale         INTEGER NOT NULL DEFAULT 0, -- rendered row superseded by newer recipe_rev
  colorspace    TEXT    NOT NULL DEFAULT 'srgb',
  store_path    TEXT    NOT NULL,           -- relative to store root
  width         INTEGER NOT NULL,           -- upright (orientation baked)
  height        INTEGER NOT NULL,
  bytes         INTEGER NOT NULL,           -- on-disk size; T2: sum over tile dir
  checksum      BLOB    NOT NULL,           -- xxh3-64 of encoded payload (T2: manifest hash)
  built_at      INTEGER NOT NULL,           -- unix seconds
  last_used_at  INTEGER NOT NULL            -- unix seconds
);
-- SQLite treats NULLs as distinct in UNIQUE constraints, so scope uniqueness is two partial indexes:
CREATE UNIQUE INDEX idx_preview_asset_scope ON preview(asset_id, tier, variant_hash) WHERE image_id IS NULL;
CREATE UNIQUE INDEX idx_preview_image_scope ON preview(image_id, tier, variant_hash) WHERE image_id IS NOT NULL;
CREATE INDEX idx_preview_lru   ON preview(tier, last_used_at);
CREATE INDEX idx_preview_hash  ON preview(content_hash);
CREATE INDEX idx_preview_path  ON preview(store_path);          -- refcount check before unlink

-- Raw decode cache accounting (E03 spec §3.3 / §5.4). Payload semantics
-- (payload_schema, params_hash canonicalization) are owned by E02/E05; this
-- table only tracks what's on disk for LRU + relocation + reconcile
-- (rawcache_dao, E03 Phase E, T18).
CREATE TABLE raw_cache_entry (
  id             INTEGER PRIMARY KEY,
  content_hash   BLOB    NOT NULL,
  params_hash    BLOB    NOT NULL,          -- xxh3-64 canonical early-stage params (E02/E05-owned encoding)
  payload_schema INTEGER NOT NULL,
  store_path     TEXT    NOT NULL,
  bytes          INTEGER NOT NULL,
  built_at       INTEGER NOT NULL,
  last_used_at   INTEGER NOT NULL,
  UNIQUE (content_hash, params_hash)
);
CREATE INDEX idx_rawcache_lru ON raw_cache_entry(last_used_at);
