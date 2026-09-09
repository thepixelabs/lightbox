-- SPDX-FileCopyrightText: 2026 PixeLabs
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
--
-- Migration 0006, E08 catalog-scope settings (E08 spec §6.7, Phase G G2):
-- one generic key/value row per setting that belongs to THIS catalog rather
-- than this machine (cache caps, T2 retention, cache-dir override, the
-- `CatalogPrefs` fields). Machine-scope prefs live in `prefs.toml` next to
-- the default store dir, never here.
--
-- A key/value shape (TEXT values, parsed tolerantly by lightbox-core::prefs)
-- is chosen over one column per setting so future catalog-scope knobs need
-- no further migration; rows are delta-only, a missing key means "use the
-- built-in default", so shipped defaults can improve across versions
-- (the same discipline keymap.toml/prefs.toml use for machine scope).
--
-- Rides the catalog file, so backup/restore (E01) carries it for free.
--
-- NOTE for authors: no BEGIN/COMMIT here, the runner owns the transaction.

CREATE TABLE catalog_settings (
  key        TEXT PRIMARY KEY,             -- registry-stable dotted name ("cache.preview_cap_bytes")
  value      TEXT NOT NULL,                -- stringified; consumer parses tolerantly
  updated_at TEXT NOT NULL                 -- RFC3339 UTC, house convention (0001/0003)
);
