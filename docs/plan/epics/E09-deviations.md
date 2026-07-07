<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E09 — Edit state, history, presets, XMP — deviations log

Append-only. Every departure from the E09 spec (`E09-edit-state-history-presets.md`)
or a decision the spec left to the implementer is recorded here with its rationale.
Reference: CLAUDE.md exit-bar rule ("Record spec deviations in
`docs/plan/epics/<EPIC>-deviations.md`") and the E09 task prompt's honest-reporting rule.

---

## Phase A — recipe model & serialization (T1–T4)

### A-1 — `lightbox-types` id-newtype additions (T1, E01 joint-review rule)

**What.** Added four additive newtypes to `lightbox-types` (E01-owned "frozen surface"):
`MaskId(pub i64)`, `RetouchOpId(pub i64)`, `SnapshotId(pub i64)`, `HistoryStepId(pub i64)`.
They derive the same trait set as the existing ids plus `Ord`/`PartialOrd` (id lists and
future keying). No existing type changed; the whole workspace compiles with no other edit
(T1 AC — verified by a clean `cargo build --workspace`).

**Why / process note.** `lightbox-types` is bannered "frozen surface — changes after M0
require a joint review (spec §8.4)". Per the E09 spec §2 crate table these additions are
"Additive only, flagged for the E01 joint-review rule". This entry is that flag: the
additions are purely additive shared vocabulary consumed by E12/E14/E08; no E01 semantics
moved. **Action for the E01 owner:** ratify at the next joint review (no code change expected).

### A-2 — `lightbox-edit` now depends on `lightbox-decode` (for `AssetProbe`)

**What.** `Recipe::default_for(pv, probe: &AssetProbe)` is a frozen §3.2 seam signature
(§3 preamble: "Types named here are the seam artifacts other epics import"; E04/E05 import
it). `AssetProbe` is owned by `lightbox-decode`, so `lightbox-edit` gains a normal
intra-workspace dependency on `lightbox-decode`.

**Consequence.** `lightbox-render` and `lightbox-shell` (which depend on `lightbox-edit`)
now pull `lightbox-decode` transitively. **Their source is unmodified** — the frozen-surface
compile proof (T2) holds — but their dependency graph grows by the decode tree
(zune-jpeg/png/tiff/kamadak-exif/zstd/…, all already workspace members, all deny-approved;
no new third-party crate, so `cargo deny` is unaffected).

**Why this over the alternatives.**
- Feature-gating the dep off-by-default was rejected: `cargo build/test --workspace` (the
  exit-bar gate) enables only default features, so `default_for`'s T2 AC would go untested in
  the gate; and cargo feature-unification pulls the dep into a workspace build anyway once any
  future crate (E04) enables it.
- Forking a lighter edit-local probe view was rejected: it would diverge the frozen seam
  signature that E04/E05 import.

**Fallback if it bites (armed, not triggered).** If the render/shell owner objects to the
transitive weight, replace `&AssetProbe` in `default_for`/`read_xmp`/`from_lr_crs` with a
minimal edit-local `ProbeView` trait that `AssetProbe` implements — a one-crate change behind
the same method names. Flagged for the E05/render owner's awareness at M1 integration.

### A-3 — Authoritative CBOR format is hand-built, not serde-derived (T3)

**What.** `Recipe` keeps `#[derive(Serialize, Deserialize)]` (matches the spec §3.2 struct
literally, for generic/in-memory serde use), **but the authoritative on-disk format is the
hand-written `Recipe::to_cbor`/`from_cbor`** over `ciborium::value::Value`, not the derived
codec.

**Why.** The forward-compat contract (unknown keys a *newer build* wrote must survive
read→write, byte-preserving) requires unknown keys to live at the **top level** of the recipe
CBOR map. Generic serde-derive would nest the `unknown` field under a `"unknown"` key
(spec §3.2 shows it as a plain field, not `#[serde(flatten)]`), which cannot capture a newer
build's top-level additions; and `#[serde(flatten)]` + ciborium does not guarantee the
deterministic, canonical, byte-stable encoding the T3/T4 ACs require. The hand-built codec
emits known keys in fixed struct order, appends unknown keys sorted, and is an idempotent
fixpoint (`to_cbor∘from_cbor∘to_cbor == to_cbor`), giving deterministic cross-platform bytes
and byte-preserving unknown round-trips. Generic serde on `Recipe` remains available but MUST
NOT be used for persistence (documented on the type).

### A-4 — Optics/geometry fine ParamId → ParamValue mapping (T1/T2/T4)

**What.** `ParamId` is finer-grained than the `#[non_exhaustive] ParamValue` carrier enum
(spec §3.1). Each `ParamId` is mapped to a **disjoint** slice of the recipe so `get`/`apply`
are total and the delta laws compose exactly:
- Optics group: `LensProfile → ParamValue::Lens(Optics)` carries **only** `optics.lens_profile`
  (the returned `Optics` carrier has neutral `ca`/`defringe`/`vignette_corr`; `apply` reads
  only `.lens_profile`). `ChromaticAberration → Bool(ca)`, `Defringe → F32(defringe)`,
  `VignetteCorr → F32(vignette_corr)`.
- `Flip → ParamValue::I32` (no `Flip` value variant exists in the frozen `ParamValue` list).
- `Defringe` is modeled as a scalar amount at M1 (the frozen `ParamValue` has no defringe
  struct variant); a richer purple/green defringe, if ever needed, rides `lb_extra`/`unknown`
  per Risk R6 without a schema bump.

**Why.** Keeps every `ParamId` mapping disjoint and minimal so `diff`/`apply` produce the
smallest correct delta and the §3.1 laws hold with no redundant emission, without inventing
`ParamValue` variants beyond the frozen list.

### A-5 — Internal group taxonomy choices (T1)

`group_of` is total/exhaustive (T1 AC). Non-obvious assignments, documented for the E08
copy/paste-checklist owner: `Vibrance`/`Saturation → Presence` (LR Basic "Presence" cluster);
`Treatment → BwMix` (the colour-vs-B&W toggle travels with the B&W mixer). Any total mapping
satisfies the AC; these are UX-taxonomy calls, not schema.

---

## Phase B — edit store, history, hub (the §3.1.1 core; store layer only — T5-DAOs, T6, T7, T9, T10)

**Scope note.** This phase covers the **store layer** only: the catalog DAOs (T5),
the `EditStore`/read surface (T6), `EditSession`'s gesture lifecycle (T7), history
reconstruction/navigation (T9), and snapshots (T10). `EditHub` in `lightbox-core`
(T8), the `lightbox-cli edit/history/snapshot/…` subcommands (T11), and the kill-9
auto-persist fault-injection proof (T12) are a **separate follow-up phase** — see
"Seams left for T8/T11/T12" below. Migration `0003_edit_state.sql` and the
`docs/plan/migrations.md` registry entry were already applied/reserved before this
phase started (recorded here for completeness, not re-litigated).

**Correction to earlier entries.** D-2, E-2, E-4, and E-5 below (Phases C/D/E)
describe `EditStore`/`EditHub`/the catalog edit DAOs as **absent** in their
worktrees — accurate at the time those phases ran. That gap is now closed by this
phase for the store layer: `lightbox-catalog::{CatalogTxn DAOs, ReaderHandle DTOs}`
(T5/T6) and `lightbox_edit::{store::{EditStore, EditSession, PendingCommit},
history::{list, recipe_at, step_to, undo, redo, clear}, snapshot}` (T7/T9/T10) all
exist and are tested. `EditSink for EditStore` (below) closes E-4's deferred DB
binding. `EditHub` (T8), the CLI subcommands (T11), and the kill-9 harness
extension (T12) remain absent — that is this phase's intentional scope boundary,
not a regression of D-2/E-2/E-4/E-5.

### B-1 — `recipe_at` upper bound must key off the latest **existing** history
step, not `head_seq` (T9 — production bug found and fixed in review)

**What.** The partial work handed to this review had `history::recipe_at` reject
any `seq > row.head_seq`. That is wrong: per §4.1-2, `StepTo`/`Undo` move
`head_seq` **backward** without deleting the steps beyond it (truncation only
happens on the next real commit) — precisely so `Redo` can replay forward again.
Bounding on `head_seq` made every `Redo` past a single `Undo` fail with
`NoSuchHistoryStep`, caught by `history::tests::undo_redo_round_trips_head_seq`
(which the partial work had written but never run — it was mid-implementation,
interrupted before its test pass).

**Fix.** The bound is now `seq > row.head_seq.max(latest_history_seq)`, i.e. the
highest seq actually present in `history_step` for the image. The nearest-keyframe
+ replay path underneath was already correct (it queries `history_step` directly,
independent of `head_seq`) — only the guard was wrong. See
`crates/lightbox-edit/src/history.rs` (`recipe_at` doc comment explains the
invariant inline for future readers).

### B-2 — Three test/bench fixtures used out-of-range `Exposure` values or a
neutral-colliding first value (T9/T7 — test bugs found and fixed in review)

**What.** `ParamId::Exposure` clamps to `[-5.0, 5.0]` (`recipe.rs` `set_scalar!`).
Three places in the partial work asserted or depended on values outside that
range, or on a first value equal to the neutral default (which produces **no**
committed step — `EditSession::take_commit` returns `None` on zero net change,
by design, T7 AC):

- `history::tests::step_back_then_edit_drops_later_steps_exactly` committed
  `Exposure = 9.0` post-step-back and asserted the stored value was `9.0`; the
  DAO/recipe layer correctly clamped it to `5.0`, failing the assertion. Fixed to
  commit/assert `4.5` (in range, still distinct from the pre-step-back value).
- `history::tests::recipe_at_head_matches_stored_doc_after_1000_steps_and_is_fast`
  looped `i as f32 * 0.01` for `i in 0..1000`: `i=0` gives `0.0`, identical to the
  neutral default, so the very first commit was a no-op and `.unwrap()` on
  `take_commit()` panicked; later iterations also exceeded `5.0` (`9.99` at
  `i=999`). Fixed to an in-range monotone ramp (`-4.0 + i*0.008`, max `≈3.992`)
  that is never equal to its predecessor.
- `benches/store_perf.rs`'s `seeded_store_with_steps` had the identical `i=0`
  no-op bug (same fix applied), and `bench_commit_with_1k_steps`'s per-iteration
  closure re-committed a **constant** `Contrast = 5.0` every criterion sample,
  which only nets a real change on the very first call and panics on the second.
  Fixed to ping-pong between two distinct in-range values (`5.0`/`-5.0`) each call.

**Why called out.** None of these are spec/schema deviations — they are test-data
bugs in code the partial work had written but not yet run (the interruption point
named in this phase's task brief). Recorded per the honest-reporting rule: the
production `recipe_at` fix (B-1) is a real behavior change; these are not.

### B-3 — Added a randomized property test for `recipe_at ≡ brute-force replay`
(T9, §6 test-plan gap closed)

**What.** The partial work's `recipe_at_matches_brute_force_replay` test used a
fixed 10-value array (never crossing the 64-step `KEYFRAME_INTERVAL` anchor
boundary). §6 of the spec names this a **PR-blocking property test** over "random
gesture sequences". Added
`history::tests::proptests::recipe_at_matches_independent_brute_force_replay`
(`proptest`, 32 cases, 1–191 random multi-param gestures per case — long enough to
cross the keyframe boundary): it folds an independently-computed expected `Recipe`
alongside the store (never calling `recipe_at`'s own anchor/replay logic) and
checks `recipe_at(seq)` against it for every `seq` actually committed, including
gestures that legitimately produce no commit (net-zero change).

### B-4 — Perf ACs measured (informational; not the nightly-dashboard machine)

**What.** Ran `cargo bench -p lightbox-edit --bench store_perf` locally (not the
nightly perf harness/reference machine) after fixing B-2's panic: `commit_p95_with_1k_steps`
≈ 76 µs (target < 5 ms), `recipe_at_head_1000_steps` ≈ 18.8 µs and
`recipe_at_mid_1000_steps` ≈ 35.7 µs (target < 10 ms each). Comfortably inside the
§6 budget on this machine; wiring these into the nightly lbx-perf baseline
dashboard is unchanged/DEFERRED per the existing E-6 pattern (CI plumbing, not
shipped code) — this phase did not add that wiring.

### B-5 — Seams left for T8/T11/T12 (the named follow-up)

Per this phase's scope boundary, the following are **intentionally untouched**,
confirmed clean at review time (`grep` found no `EditHub` in `lightbox-core`, no
`edit`/`history`/`snapshot`/`preset`/`xmp` subcommands in `lightbox-cli`):

- **T8 (`EditHub` in `lightbox-core`).** Build on `lightbox_edit::{EditStore,
  EditSession, PendingCommit}` directly (`crates/lightbox-edit/src/store.rs`).
  `EditSession` is already the pure in-memory gesture object T8's registry should
  wrap per-image; `EditStore::commit` is the one DB-touching call to route through
  the dispatcher's blocking pool.
- **T11 (CLI subcommands).** `lightbox-cli` already depends on `lightbox-edit`
  (pre-existing, for `Recipe::identity(PV_M0)` in `render`); no new subcommand
  wiring exists. `EditStore`/`history`/`snapshot` functions are ready to call
  directly from hand-rolled subcommand handlers, matching the existing
  `create/import/list/render/backup/check` pattern.
- **T12 (kill-9 auto-persist proof).** Not extended; `EditStore::commit` is one
  `WriterHandle::with_txn` call (the same primitive E01's existing kill-9 harness
  already exercises for other tables), so the harness extension is additive.

---

## Phases C + D — XMP substrate + `crs:`/`lb:` mapping layer (T13–T22)

### D-1 — Fallback XMP substrate (own RDF/XML), and Phase D shipped ahead of Phase C

**What.** `lightbox-meta::xmp` is implemented as the **§1.6 named fallback** — our
own RDF/XML over `quick-xml` behind the `XmpDoc` API — **not** the primary Adobe
ISO 16684 `xmp_toolkit` binding. Phase D (this mapping layer) was executed while
Phase C's toolkit integration had not landed, so it targets the fallback.

**Why.** The architecture arms exactly this reversal (§1.6 / Risk R1): the
`XmpDoc` API surface matches spec §3.5 so a later swap to the ISO toolkit is a
**one-crate** change, and the encapsulation rule (no substrate type escapes
`xmp::doc`) keeps it cheap. The mapping layer (`to_xmp`/`read_xmp`/`from_lr_crs`)
speaks only the `XmpDoc` API and is substrate-agnostic — swapping substrates
requires **zero** change to `lightbox-edit`.

**Consequence / follow-up.** T13's cargo-deny/SBOM registration of the C++
toolkit and the 3-OS toolkit build are **DEFERRED** to the Phase C toolkit swap
(or accepted permanently if the fallback proves sufficient). `cargo deny` is
unaffected today (`quick-xml` is MIT, surface-1 clean; no GPL, no C++ vendoring).

### D-2 — T22 command-bus + divergence wiring is Phase B / E06 territory (helpers shipped)

**What.** T22's durable surface — `Command::Edit(ReadMetadata/WriteMetadata)`
dispatcher arms, the debounced `Class::Background` auto-write job, and
`sync::status(cat, asset, original)` reading the `xmp_sync` DAO — depends on the
`EditHub`/dispatcher, which is **still absent** (T8 is a separate follow-up phase
from the store layer landed above — see the Phase B section). Phase D therefore
ships the **operations those commands invoke**, fully tested in isolation:

- `xmp_map::write_sidecar` / `read_sidecar` (recipe ↔ atomic sidecar, mandate-c.2
  safe — never touches the original);
- `xmp_map::XmpWriteCoalescer` — the pure, clock-injected trailing-debounce policy
  that makes the "50-commit burst ⇒ ≤2 writes" AC unit-testable **without** the
  E06 scheduler (its graceful synchronous fallback: flush eagerly on `due`);
- `sync::classify` — the pure divergence state machine (Phase C).

The catalog read/write DAOs this note originally called "not present" **do now
exist** (Phase B, T5/T6, landed above) — `xmp_sync` CRUD in particular
(`upsert_xmp_sync_write`/`upsert_xmp_sync_read`/`xmp_sync_row`) is ready for T22's
dispatcher wiring to call directly.

**Why.** T22's own spec permits a "graceful synchronous fallback when jobs absent
in tests"; the command arms are one dispatcher match each once `EditHub` (T8)
exists. `ReadMetadata` applying as a `StepLabel::XmpRead` history step and the
opt-in (default **off**) auto-write pref are **DEFERRED** to that T8 wiring; the
label variant already exists in the frozen `StepLabel` enum.

### D-3 — `lightbox-render`/`lightbox-shell` gain `lightbox-meta` transitively (source untouched)

**What.** `lightbox-edit` now depends on `lightbox-meta` (the mapping layer targets
`XmpDoc`). Crates that depend on `lightbox-edit` (`lightbox-render`,
`lightbox-shell`) thus pull `lightbox-meta` (+ `quick-xml`) transitively.

**Consequence.** Their **source is unmodified** — the frozen-surface proof (T2)
holds — but their dependency graph grows by `lightbox-meta`/`quick-xml` (MIT,
already deny-approved; no new GPL, `cargo deny` unaffected). Flagged for the
render/shell owner at M1 integration, alongside A-2.

### D-4 — Foreign passthrough models scalars + simple arrays only (nested structs need the toolkit)

**What.** `xmp_passthrough` captures foreign properties keyed by full
`(namespace-URI, local-name)` (ASCII-Unit-Separator-joined) with a self-describing
CBOR value (scalar `Text`, or `Array([kind, items…])`). This round-trips every
**scalar and simple `rdf:Seq`/`Bag`/`Alt`** property verbatim (the §6 "foreign
properties survive read→write" contract — property-tested). **Nested
`rdf:parseType="Resource"` structs** (e.g. a real LR `MaskGroupBasedCorrections`
block) are not modeled by the fallback substrate and cannot be preserved verbatim.

**Why / impact.** This is the fallback's known limitation (D-1). Such blocks are
still classified **skipped** and whatever the substrate captured is preserved;
full verbatim struct fidelity arrives with the ISO toolkit swap and is owned by
E12/E16. The T21 `MaskGroupBasedCorrections` fixture uses a substrate-representable
(opaque-string-array) form and the manifest states this.

### D-5 — exiftool oracle absent on this host → validity checked by substrate re-parse

**What.** T19's AC prefers validating an emitted packet in **exiftool** as a
dev-only subprocess oracle. `exiftool` is **not installed** on the execution host.
Per the task's honest-reporting rule (do not fake an exiftool run), Phase D
validates emitted packets by **re-parsing them through `XmpDoc`** and asserting the
expected properties are present and typed (`tests/xmp_mapping.rs`).

**Follow-up (DEFERRED).** Wire the optional exiftool oracle behind a
`which exiftool` guard in CI where the binary is available; it is additive and
changes no shipped code. exiftool is GPL — a **dev-time subprocess only**, never a
dependency (DoD §6).

### D-6 — Real freely-licensed modern-PV LR corpus is DEFERRED; fixtures are hand-authored

**What.** T21 calls for a modern-PV LR Classic sidecar corpus that is *freely
licensed*. None is bundled on this host. Per the task rule (do not invent Adobe
data), Phase D ships **hand-authored, project-CC0 representative fixtures**
(`tests/fixtures/lr_modern_pv2012.xmp`, `lr_legacy_pv2.xmp`; provenance in
`tests/fixtures/MANIFEST.md`) that exercise the modern and legacy paths, plus a
proptest over fuzzed `crs:` soup for totality.

**DEFERRED.** Sourcing and committing a real freely-licensed LR-Classic
sidecar/preset corpus (and asserting per-file expected reports against it) is the
E16/M4 interop-hardening leg — the mechanism + fixtures here are sufficient for
E16 to start (the named spill point, spec §5 "T21 corpus breadth").

---

## Phase E — presets & settings transfer (T23–T28)

### E-1 — Config dir resolved from env vars, not the `directories` crate (T23)

**What.** [`preset::default_preset_dir`] resolves `<config>/Lightbox/presets` from
platform env vars (`HOME`/`Library/Application Support` on macOS, `APPDATA` on
Windows, `XDG_CONFIG_HOME`/`HOME/.config` elsewhere) rather than the spec §2
`directories` crate.

**Why.** `directories` is not in the current dependency graph; adding it pulls
`dirs-sys` → `option-ext`, which is **MPL-2.0** — not on `deny.toml`'s exhaustive
surface-1 allowlist (MIT/BSD-2/BSD-3/Apache-2.0/Unicode-3.0/ISC). Adding a global
allowlist entry requires a licensing-review PR (the file says so), and a scoped
per-crate exception for weak-copyleft is a licensing decision outside this phase's
authority. The env-var resolution is functionally equivalent for the preset store,
adds **zero** new license surface, and keeps `cargo deny` green. Reversal: if the
owner wants `directories`, it is a one-function swap behind `default_preset_dir`
plus the licensing-review PR.

### E-2 — `StepLabel` shipped in `lightbox_edit::history` ahead of Phase B (T25)

**What.** Phase E defines [`history::StepLabel`] (spec §3.3) — a pure leaf enum the
preset/transfer engines stamp onto steps. At the time Phase E ran, the **rest** of
`lightbox_edit::history` (the `recipe_at`/`list`/`clear` engine, keyframe replay,
`HistoryStepMeta`) was Phase B (T5–T9) territory and not yet present. **Phase B has
since landed** (see the Phase B section above) and extended `history.rs` in place,
reusing this exact `StepLabel` enum unchanged, as anticipated below.

**Why / what happened.** Phase E's public surface (`sync_to`, the apply paths) is
typed on `StepLabel`, so it had to exist for the crate to compile and be testable
at Phase E time. Phase D already referenced `StepLabel::XmpRead` (deviations D-2).
Phase B did extend `history.rs` in place and did not redefine `StepLabel` — the
anticipated merge reconciliation was a non-event.

### E-3 — Preset delete is a permanent remove, not OS trash (T24)

**What.** [`PresetStore::delete`] performs `fs::remove_file`, not an OS-trash move.

**Why.** T24 says "OS trash where available", but no trash crate is on the vetted
§2 dep list; a `trash` dependency needs its own licensing/platform review (it pulls
platform frameworks). The seam is one function; wiring a vetted trash crate later
is additive and changes no caller. **DEFERRED** with that trigger.

### E-4 — Transfer engine written against an `EditSink` seam, not `EditStore` (T27)

**What.** The spec signature is `sync_to(store: &EditStore, …)`, but `EditStore`
was a **Phase B** type absent when Phase E ran. The engine ([`transfer::sync_to`]
and the apply paths) is written against a minimal [`transfer::EditSink`] trait
(`recipe_of` + `commit_batch` = one WAL txn). A local [`transfer::CancelFlag`] /
[`transfer::CancelSignal`] provides cooperative cancellation without pulling the
`lightbox-jobs` (tokio) runtime into `lightbox-edit` (which would grow
render/shell's graph, cf. A-2/D-3).

**Why.** This kept the genuinely-Phase-E logic — ~64/txn batching, one step per
target, progress events, cancel-between-chunks, "Previous" semantics — **real and
fully unit-tested** against an in-memory fake sink, while the DB binding stayed
for Phase B. **Resolved by Phase B** (above): `impl EditSink for EditStore` now
exists in `crates/lightbox-edit/src/store.rs`, wiring `sync_to`/the transfer engine
onto the real catalog with no change to the Phase-E types — see
`store::tests::edit_sink_sync_to_via_editstore`.

### E-5 — Command-bus / CLI / kill-9 legs of T25/T27/T28 DEFERRED past Phase B (T8/T11/T12)

**What.** These T25/T27/T28 items depend on artifacts that did not exist when
Phase E ran and, for the command-bus/CLI/kill-9 legs specifically, **still do
not** (they are T8/T11/T12 — a follow-up scoped separately from the store layer
Phase B landed above, per that section's scope note):

- the `Command::Edit(ApplyPreset / PasteSettings / ResetEdits / ApplyPrevious /
  SyncSettings)` **dispatcher arms** and the core-held `CopiedSettings` / "Previous"
  buffer wiring (needs `EditHub`, T8);
- the **full `lightbox-cli` E2E scenario** (import → gestures → history walk →
  snapshot → preset create/apply → LR import → xmp write → fresh open → xmp read →
  equality + divergence → kill-9 leg) (needs the CLI subcommands, T11, and the
  kill-9 harness extension, T12);
- the **500-target-against-SQLite** criterion number and the commit-txn/`recipe_at`
  perf rows — **this part is now measurable**: Phase B's `store_perf.rs` bench
  reports `commit_p95_with_1k_steps` ≈ 76 µs and `recipe_at_*_1000_steps` ≈
  19–36 µs (see Phase B, B-4), both far inside the §6 budget. The 500-target sync
  number specifically (against the real DB, not the in-memory `EditSink`) remains
  open until a T11 CLI/E06-job-scale benchmark exists.

**What ships instead (real, tested).** The complete file-backed `PresetStore`
(create/rename/delete/export/import, cold scan, refresh, quarantine), the pure
`preview_recipe`, `CopiedSettings`, and the batched transfer engine — all exercised
end-to-end at the crate layer in `tests/preset_transfer_e2e.rs` against the on-disk
store and the in-memory `EditSink`, **plus now against the real `EditStore`** (E-4,
`store::tests::edit_sink_sync_to_via_editstore`). The perf ACs measurable without
the DB are met (criterion `preset_transfer` bench: cold-scan 500 ≈ 55 ms < 100 ms;
in-memory sync 500 ≈ 0.7 ms « 1 s). T8/T11/T12 wire the remaining deferred legs
onto this surface with no change to the Phase-E types.

### E-6 — Criterion benches ship; the nightly perf-dashboard wiring is DEFERRED

**What.** `benches/preset_transfer.rs` (criterion) ships `preset_cold_scan_500` and
`sync_500_targets`. Uploading these into the **nightly perf dashboard / lbx-perf
baseline** is DEFERRED-to-nightly (as the task directs) — it is CI plumbing, not
shipped code.

### E-7 — T28 rustdoc pass fixed 8 pre-existing broken intra-doc links in `xmp_map`

**What.** The T28 "cargo doc clean" pass surfaced 8 `rustdoc::broken_intra_doc_links`
warnings in the Phase-D `xmp_map/mod.rs` module docs (`[`Recipe::…`]`/`[`XmpDoc`]`
referenced from a module that does not import those names). Phase E fully-qualified
them (`crate::Recipe::…`, `lightbox_meta::xmp::XmpDoc`); `RUSTDOCFLAGS="-D warnings"
cargo doc -p lightbox-edit` is now clean. No runtime code changed.
