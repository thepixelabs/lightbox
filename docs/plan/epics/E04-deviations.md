<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E04 — Working-set loader & drag-drop intake — deviations log

Append-only. Every departure from the E04 spec (`E04-working-set-loader.md`,
v2.0) or a decision the spec left to the implementer is recorded here with
its rationale. Reference: CLAUDE.md exit-bar rule ("Record spec deviations
in `docs/plan/epics/<EPIC>-deviations.md`") and the honest-reporting rule.

---

## Phase A — vocabulary & store (T1–T4)

### A-1 — `lightbox-types::SourceKind` addition (T1, E01 joint-review rule)

**What.** Added `SourceKind { Raw, Rendered }` to `lightbox-types` (E01-owned "frozen
surface"), matching spec §4.1 exactly. Purely additive — no existing type changed.

**Why / process note.** Per the same convention as E09-deviations.md A-1: flagged for the
E01 owner's ratification at the next joint review. No code change expected.

### A-2 — Migration 0005 adds an index the spec's illustrative SQL omits

**What.** Beyond the exact index list in spec §5.1, migration `0005_open_in_place.sql` adds
`CREATE INDEX asset_folder_filename ON asset(folder_id, filename);`.

**Why.** Dropping `UNIQUE(folder_id, filename)` (spec-mandated, §3.3/§5.1) also drops the
implicit index SQLite maintained for that constraint. Without a replacement, E01/E03's own
`every_page_query_plan_is_index_driven` test (T10 AC — every `images_page` shape must be
index-driven, never a `USE TEMP B-TREE` sorter) regressed: `FilenameAsc` sort + folder filter
fell back to a sorter via the `asset_folder_added` index instead of a direct seek. A plain
(non-unique) index restores the query-plan shape without reinstating the managed-tree
uniqueness guard.

### A-3 — `insert_assets_skips_duplicate_hashes_globally_and_within_batch` behavior change

**What.** This pre-existing E01 DAO test asserted that inserting a second asset with an
existing `(folder_id, filename)` pair but different content hash was a `Constraint` error.
Updated to assert it now **succeeds** (a second, distinct row).

**Why.** This is the direct, intended consequence of A-2's constraint removal (spec §3.3:
"identity is `content_hash`, not the managed-tree path") — not a bug. Recorded here because
it changes a previously-asserted DAO behavior, not because it is unintended.

### A-4 — `ImageDetail.folder: FolderId → Option<FolderId>` (E01 joint-review rule, R5/R6)

**What.** Exactly the one frozen-DTO change the spec names (§4.4, §11 R5/R6). Grep-verified
(T4 AC): no consumer outside `lightbox-catalog` reads `.folder` as of this epic.

**Why.** `folder_id` is nullable as of migration 0005 (open-in-place rows have no folder).
Flagged for the E01 owner's ratification, same process as A-1.

### A-5 — `foreign_key_check_file` added as a new `#[doc(hidden)]` test/fault-harness helper

**What.** `lightbox_catalog::foreign_key_check_file(db_path) -> Result<Vec<String>>`, mirroring
the existing `integrity_check_file`. Used by the T3 upgrade test and the T3 kill-9
fault-injection harness to independently re-verify the `rebuilds_tables` procedure's own
commit-time gate from outside the crate (integration tests can't reach the `#[cfg(test)]
pub(crate) fn raw()` escape hatch).

**Why.** Spec §9.2/§9.4 explicitly wants `foreign_key_check` re-verified by the fault harness,
not just trusted as the runner's internal gate.

---

## Phase B — the loader (T5–T7) + folder-explorer primitive

### B-1 — Skip accounting for `Hidden`/`UnsupportedExtension` is a best-effort companion pass

**What.** `plan_open` reports `SkipReason::Hidden`/`UnsupportedExtension` for walk-discovered
files via a second, lightweight `walkdir` pass over the same directory (`hidden_and_unsupported
_skips`), comparing against what `discover_files` actually returned. This second pass does
**not** stop descent into hidden directories the way `discover_files`'s own `filter_entry`
does, so it can slightly over-count entries nested under a hidden directory as `Hidden`/
`UnsupportedExtension` relative to strict E01 walk semantics.

**Why.** Spec §6.2 wants unsupported/hidden files "counted so E08 can say '312 files opened,
40 non-photo files skipped'", but `discover_files`'s return shape (`Discovery { files, errors
}`, E01-frozen, reused verbatim per spec's explicit instruction) does not report what it
filtered — there is no way to recover that count from its output alone. Re-deriving the
walk with the same `KNOWN_EXTENSIONS` const and the same one-line hidden-name rule is a
reporting-only companion, not a re-implementation of file **selection** (which stays solely
`discover_files`'s). This never affects the actual working-set contents, only the skip-list
bookkeeping. Accepted as a pragmatic approximation; a perfectly nested-hidden-aware count
would require changing `discover_files`'s frozen return contract, out of this epic's scope.

### B-2 — Property tests implemented as deterministic tests, not `proptest`

**What.** Spec §9.2 asks for property tests (shuffle-invariant ordering, plan→load→plan
idempotence). Implemented as targeted deterministic unit/integration tests (e.g.
`gesture_order_for_explicit_files_wraps_a_folders_sorted_expansion`,
`folder_expansion_sorts_by_capture_time_then_filename`,
`reopening_the_same_plan_is_idempotent`) rather than `proptest`-generated cases.

**Why.** `lightbox-ingest` has no `proptest` dev-dependency today; adding one for this epic
alone was judged not worth the dependency-graph churn given time budget, when the same
invariants are directly, deterministically testable. Named as a lighter-weight test than the
spec's literal ask — the invariants themselves are covered, the fuzzing breadth is not.

### B-3 — `OpenRequest::new()` constructor added (not in the spec's illustrative code)

**What.** `impl OpenRequest { pub fn new(paths, recursive, origin) -> OpenRequest }`.

**Why.** `OpenRequest` is `#[non_exhaustive]` (correctly, per spec); a struct literal from
outside `lightbox-ingest` (E08, `lightbox-core`, the CLI, every test) does not compile against
a `#[non_exhaustive]` struct. A constructor is required for the type to be usable at all
across the crate boundary the spec itself describes ("Constructed by E08 … or the CLI").

---

## Phase C — core façade & CLI (T8–T10)

### C-1 — Plan-phase progress has no dedicated core `Event`

**What.** `lightbox_ingest::PlanProgress` (phase-1 per-file progress) is passed a no-op
callback in the core dispatcher; it never reaches the event bus.

**Why.** Spec §4.5's `Event` enum names only `WorkingSetOpening`/`Replaced`/`Changed`/
`LoadFinished` — no phase-1 progress event. Phase 1 is budgeted at <1.5s for 1000 files
(§7) and is immediately followed by `WorkingSetReplaced`, so a separate throttled heartbeat
for it would have nothing to say a few ms before `Replaced` already says it. Consistent with
the spec's own T11 budget framing (phase 1 is "fast, no full-file reads").

### C-2 — `Event::WorkingSetChanged` coalescing rides the loader's own `Progress` throttle

**What.** The core dispatcher only broadcasts `WorkingSetChanged` when
`lightbox_ingest::LoadEvent::Progress` fires (every item's `ItemReady`/`Failed`/`Collapsed`
still updates the in-memory model immediately, just without a broadcast). No separate
throttle timer in `lightbox-core`.

**Why.** Spec §4.5 says `WorkingSetChanged` is "coalesced ≤ 1 per `progress_min_interval`" —
exactly the throttle `load_working_set` already implements for its own `Progress` event
(§6.5). Riding it avoids a second independent throttling clock with its own drift.

### C-3 — `default_store_dir()` hand-rolled, not the `dirs` crate

**What.** Implemented with three `#[cfg(target_os = …)]` arms reading `HOME`/`APPDATA`/
`XDG_DATA_HOME` directly, per the spec §2 table's explicitly offered choice ("`dirs` … or a
~30-line hand-roll … either passes the deny gate; record the choice here").

**Why.** Each platform convention is one or two env-var reads; not worth a new third-party
dependency. Both options were pre-approved by the spec itself.

### C-4 — `ItemState::Failed`/exit-1 semantics diverge from the T10 task-list's summary wording

**What.** The epic's own task breakdown (§8, T10 AC) says CLI `open` on a malformed
known-extension fixture should exit 1 "with the item marked Failed". The **implementation**
instead follows the epic's own **interface spec** (§4.5's `ItemState` doc comment: `Failed` =
"Hash or registration failed"; §6.2's T18 convention: a malformed-but-hashable file registers
successfully, badged with `decode_error`, and is `ItemState::Ready`) — `lightbox-cli open`
therefore exits **0** for a registered-but-badged malformed fixture, matching the DAO's own
`ensure_open_asset` design (which writes a row regardless of probe outcome) and the core
integration test `open_mixed_corpus_yields_correct_states_and_badges`.

**Why.** §4.5 and §6.2 are the authoritative, detailed interface contracts; §8's task-list
bullet is summary prose that (understandably, given it predates the detailed interfaces two
sections earlier in the same document) doesn't precisely track them. Following the detailed
contract was judged correct over the summary bullet — reproducing the summary's literal
wording would mean inventing a THIRD registration outcome (badged-but-"Failed") nowhere else
in the spec, and would contradict the DAO's own step 2 (§4.4) and the T18 convention named
explicitly in §6.2. `ItemState::Failed`/exit 1 is still reachable and tested (via a hash/stat
failure — a file that vanishes or a genuine I/O error) — just not via a merely-malformed file.
Flagged for product/E08 owner awareness: the CLI badges malformed files as `[decode_error]`
text in its non-JSON output and `decode_error` in JSON, so the information is not lost, only
not routed through the exit code.

### C-5 — `rusqlite`/`serde` added as (dev-)dependencies of `lightbox-cli`

**What.** `serde` (direct dependency, for `open --json`'s per-item `#[derive(Serialize)]`
line type) and `rusqlite` (dev-dependency only, for one E2E test that forces a WAL checkpoint
before bit-flipping a catalog file — a small test catalog never crosses SQLite's automatic
checkpoint threshold on its own, unlike E01's heavier `e2e.rs` corruption test which
incidentally does).

**Why.** Both already ship pervasively elsewhere in the workspace (transitively and
directly); neither changes the shipped binary's release dependency tree in a new-crate sense
(`rusqlite` is dev-only; `serde` was already a transitive dependency of `serde_json`).

---

## Phase D — proof & polish (T11–T12)

### D-1 — T11 perf scenario: `open-1k` shipped; baseline recorded on a dev sandbox

**What.** `lbx-perf --scenario open-1k` measures `single_raw_open_ms` (one raw fixture alone
— the direct, precisely-measurable proxy for spec §7's "item 0 `Ready` < 50 ms p95", since a
1-item open makes item-0-ready and load-finished the same event), `plan_ms` (phase-1 time
over a 1000-file folder drop — §7's "<1.5s" budget), and `wall_ms` (phase-2 all-`Ready` time).
Wired into `baselines.json` with `budgets` entries for the two directly-named §7 numbers.

**What's honestly incomplete.** The recorded numbers (196 ms wall / 20 ms plan / 4.7 ms
single-raw @ 1000 files) come from this session's dev sandbox, not the pinned "M0 exit-drill
machine" the rest of `baselines.json` was recorded on, and this was **not** run on the 3-OS
nightly matrix (no CI access from this session). All three numbers sit comfortably inside
their §7 budgets (order-of-magnitude headroom), but the baseline should be re-blessed on the
real drill machine per the file's own stated re-bless convention before being treated as
authoritative for regression tracking.

### D-2 — T12 (cut-line) — not built

**What.** T12's parallel-probing / viewport-prioritized hash order / batch-txn escape hatch
was **not implemented**.

**Why.** The spec names T12 explicitly as slack ("drop first on overrun") and gates it on
"if lbx-perf disagrees with §6.5" / "if profiling ever disagrees". D-1's measured numbers
(20 ms plan / 196 ms wall for 1000 files, against 1500 ms / no-hard-cap budgets; 4.7 ms
single-raw against a 50 ms budget) show no overrun on this dev machine — there is no
profiling signal calling for the cut-line optimization. Left as a genuinely open, named,
future item rather than built speculatively.

---

## Summary of what is NOT built (honest accounting)

- T12's parallel-probe/prioritize/batch-txn cut-line (D-2) — optional per spec, no perf
  signal calling for it.
- The `open-1k` nightly 3-OS CI wiring itself (D-1) — the scenario exists and runs locally;
  hooking it into the actual nightly GitHub Actions matrix is outside this session's reach
  (no CI access) and is the same category of gap the rest of `lbx-perf` already has relative
  to "nightly, non-blocking" per its own module doc comment — this session could not verify
  any nightly workflow runs any `lbx-perf` scenario end-to-end in CI.
- `proptest`-based property tests (B-2) — covered by deterministic tests instead.
- Full nested-hidden-directory-aware skip accounting (B-1) — informational-only bookkeeping,
  a documented approximation.

Everything else in the E04 spec's task breakdown (T1–T11) is implemented and tested per the
task prompt's DoD.
