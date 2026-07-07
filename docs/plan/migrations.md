# Catalog migration-number registry

Single source of truth for catalog schema migration numbers (E01 spec §4.4,
OQ-2). Parallel epics **reserve a number here via PR before** implementing
the migration, so two epics never collide on a number. Migrations are
forward-only, applied in ascending order, each inside one transaction, and
recorded in the catalog's `schema_version` table; there are no
down-migrations — restore-from-backup is the rollback story.

Rules (linted by `cargo xtask lint-migrations`, a CI gate):

- one row per number, ascending, no duplicates;
- a **shipped** migration must exist as
  `crates/lightbox-catalog/migrations/NNNN_<name>.sql` with the name below,
  and shipped numbers must be contiguous from 0001 (the runner applies them
  in sequence);
- a **reserved** row has no SQL file yet — it holds the number for the
  owning epic;
- rows are never deleted or renumbered once merged.

| number | epic | name            | status |
|--------|------|-----------------|--------|
| 0001   | E01  | spine           | shipped (E01 Phase 3, T9) |
| 0002   | E02  | e02_color       | shipped (E02 Phase H, H1) |
| 0003   | E09  | edit_state      | shipped (E09 Phase B, T5) |
| 0004   | E03  | preview_pyramid | shipped (E03 Phase A, T03) |

Expected future reservations (from the architecture §3.1 entity map — the
owning epics reserve the actual numbers when their specs land):
collections/keywords/smart collections/`metadata_cache` (E07); masks/retouch
ops (E12); embeddings/faces/model packs (E13/E14, including the sqlite-vec
virtual table, which must not be created before the extension ships).
