<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E08 — Editor shell, drag-drop entry & develop UI — deviations log

Append-only. Every departure from the E08 spec (`E08-editor-shell.md`, v2.0)
or a decision the spec left to the implementer is recorded here with its
rationale. Reference: CLAUDE.md exit-bar rule ("Record spec deviations in
`docs/plan/epics/<EPIC>-deviations.md`") and the honest-reporting rule.

**E08 is NOT done.** This log covers **Phases A and B** (A: chassis rescope,
entry intake, working-set view model, replace semantics, the smoke driver,
and the mandate-v2.2 folder-explorer UI; B: the session filmstrip, B1–B5).
Phases C–H are separate, later work; do not read this file as epic
completion.

---

## Phase A — chassis rescope & entry (A0–A7 + folder explorer)

### A0-1 — `SetEpoch` is a plain `u64`, not a `WorkingSetEpoch(pub u64)` newtype

**What.** The spec's §6.1 draft proposes `pub struct WorkingSetEpoch(pub u64)`. What E04
actually shipped (`lightbox_core::SetEpoch = u64`, re-exported from `lightbox-core::working_set`)
is a plain type alias, not a newtype wrapper.

**Why / impact.** Cosmetic only — every place this spec draft names `WorkingSetEpoch` reads
as `SetEpoch` against the real code. `crates/lightbox-shell/src/working_set.rs` uses
`lightbox_core::SetEpoch` throughout; no functional difference.

### A0-2 — `Event::WorkingSetReplaced` carries no `ticket`; only `WorkingSetOpening` does

**What.** The spec's §6.1 draft shows `Event::WorkingSetReplaced { epoch, ticket, discovered }`.
The shipped shape is `Event::WorkingSetReplaced { epoch, planned, truncated }` — no `ticket`,
and the item count field is named `planned` (matching `lightbox_ingest::SetPlan::items.len()`),
not `discovered`. The `ticket` correlating a command to its outcome lives only on
`Event::WorkingSetOpening { epoch, ticket }`.

**Why / impact.** None for Phase A — the shell never needed the ticket on `Replaced` (it isn't
used to correlate anything E08 built). Recorded for Phase B/H authors who might otherwise
expect it.

### A0-3 — no granular `Event::WorkingSetEntryLoaded{epoch, index}`; only a coalesced `WorkingSetChanged{epoch}`

**What.** The spec's §6.1 draft names a per-entry event (`WorkingSetEntryLoaded`) so a view
model could patch one row at a time. E04 shipped a coarser signal instead:
`Event::WorkingSetChanged { epoch }`, coalesced ≤ 1 per `OpenOptions::progress_min_interval`
and piggy-backing the loader's own progress heartbeat (`LoadEvent::Progress`) — it carries no
index, no per-item delta.

**Why / impact — real design consequence, not cosmetic.** `crates/lightbox-shell/src/working_set.rs`
(`WorkingSetView`) does **not** attempt to replay per-item deltas into local state the way the
spec's §6.2 draft implies. Instead, every `WorkingSet*` event triggers a cheap wholesale
re-pull of `Session::working_set()`'s `Arc<WorkingSetSnapshot>` (an `Arc` clone — the whole
snapshot is already internally epoch-consistent, built atomically inside `WorkingSetModel`).
This is simpler than the spec's granular-fold design and can never drift from the core's own
epoch-guarded state machine, at the cost of the shell re-fetching the whole entry list on
every coalesced change rather than patching one row — a non-issue at the mandate's tens/
hundreds-of-files scale. The epoch guard the spec still wants ("stale-epoch events dropped")
is preserved: an event tagged with an epoch older than the one already applied is dropped
*before* even touching the session. Proven by a real headless-`Session` integration test
(`working_set::tests::stale_epoch_events_are_dropped_after_a_replace`) that captures **genuine**
events from a superseded epoch's own real event stream (not fabricated `Event` literals — see
A0-6) and feeds them into an already-advanced view, asserting no regression.

### A0-4 — `WorkingSetSnapshot`/`WorkingSetItem` field shapes match closely, with naming/structure deltas

**What.** The spec's §6.1 draft's `WorkingSetSnapshot { epoch, entries: Vec<WorkingSetEntry> }`
/`WorkingSetEntry { path, state: EntryState }` two-level shape, with a separate `ReadyEntry`
struct nested inside `EntryState::Ready(ReadyEntry)`, is **not** what shipped. The real shape
(`lightbox_core::{WorkingSetSnapshot, WorkingSetItem, ItemState}`) is flatter: every descriptive
field (`filename`, `source_kind`, `format`, `width`, `height`, `capture_time`, `decode_error`,
`explicit`) lives directly on `WorkingSetItem`, not nested inside a Ready-only sub-struct;
`ItemState::Ready { asset: AssetId, image: ImageId }` carries only the two catalog ids (no
duplication of the descriptive fields already on the parent `WorkingSetItem`). There is also a
fourth item state the spec draft doesn't name: `ItemState::DuplicateOf { index }` (content-hash
collapse, E04 spec §6.4).

**Why / impact.** `crates/lightbox-shell/src/working_set.rs` and `filmstrip.rs` are written
directly against the real, flatter shape — confirmed compiling and passing real
integration tests against a live headless `Session`. `filmstrip.rs`'s cell painter has an
explicit (minimal) `ItemState::DuplicateOf` arm the spec draft's model doesn't anticipate.

### A0-5 — the shell does not need its own `intake::OpenRequest`/`IntakeSource` types

**What.** The spec's §6.1 draft has E08 define shell-local `pub enum IntakeSource` and
`pub struct OpenRequest { source, paths, recursive }`, presumably normalized later into E04's
command. E04's real, shipped `lightbox_ingest::OpenRequest` (re-exported off `lightbox-core`)
**already is** exactly this type — `#[non_exhaustive]` with a `paths`/`recursive`/`origin:
OpenOrigin` shape and an `OpenRequest::new(paths, recursive, origin)` constructor — and
`OpenOrigin` already has `DragDrop`/`OpenDialog`/`FileAssociation`/`Cli` variants covering
every intake source this epic names (including the folder explorer, which reuses `OpenDialog`
— see A0-7).

**Why / impact.** `crates/lightbox-shell/src/intake.rs` constructs `lightbox_core::OpenRequest`
directly rather than defining a redundant parallel type that would just get translated one field
at a time into the real one. One fewer type, one fewer conversion, zero behavior difference.

### A0-6 — `Event`/`WorkingSetSnapshot`/`WorkingSetItem` are `#[non_exhaustive]`: shell tests use real events, never fabricated ones

**What.** Because these core DTOs are `#[non_exhaustive]`, `lightbox-shell` (an external crate
to `lightbox-core`) cannot construct arbitrary `Event`/`WorkingSetSnapshot` literals for tests
the way the spec's test-plan prose casually implies ("kittest-injected... test-double working-set
events"). `crates/lightbox-shell/src/working_set.rs`'s epoch/lag/activation unit tests
(A5 AC) therefore drive a **real headless `Core`/`Session`** (no GPU — `gpu: None`), open real
tiny synthetic JPEGs (the same pinned fixture the smoke driver embeds, so no `cargo xtask
fixtures` dependency), and capture **genuine** events off the real broadcast channel — including
deliberately holding back and later re-feeding a superseded epoch's own real events to prove the
staleness guard (A0-3).

**Why / impact.** Stronger tests than the spec's mocked-event framing would have produced — they
exercise the actual E04 dispatch path (`Command::OpenWorkingSet` → `WorkingSetModel` → the real
`Event` broadcast), so a future E04 shape change would fail these tests immediately rather than
silently drifting from a hand-rolled test double.

### A0-7 — the folder-explorer UI reuses `OpenOrigin::OpenDialog`; no new `OpenOrigin::Explorer` variant added

**What.** `OpenOrigin` is E04-owned. Rather than grow it with a new `Explorer` variant (a
cross-epic frozen-surface change that would need the same joint-review treatment E04's own
`SourceKind` addition got, per `E04-deviations.md` A-1), the folder-explorer UI's picks
(`ExplorerAction::OpenImage`/`OpenFolder`) submit with `OpenOrigin::OpenDialog` — the closest
existing semantic fit ("a UI-driven picker", per `OpenOrigin`'s own doc comment: "Tracing/UX
copy only — the construction rules do not vary by origin").

**Why / impact.** Purely a tracing/status-line wording nuance (explorer opens show up
indistinguishable from native-dialog opens in any future telemetry). Follow-up: if that
distinction becomes valuable, E04 can add `OpenOrigin::Explorer` additively (the enum is
`#[non_exhaustive]` for exactly this kind of growth) — not blocking.

### A0-8 — `ParamId` (E09) is a coarse wire-stable enum, not the spec's dotted-string leaf ids — informational for Phase E, doesn't block Phase A

**What.** The spec's §6.5 draft names per-control param ids as dotted strings: `wb.temp`,
`wb.tint`, `wb.mode`, `tone.exposure`, `tone.contrast`, `tone.highlights`, `tone.shadows`,
`tone.whites`, `tone.blacks`, `curve.point`. What E09 actually shipped
(`lightbox_edit::ParamId`, re-exported off `lightbox-core`) is a `#[repr(u16)]` Rust enum with
**coarser, whole-leaf** wire-stable ids: `ParamId::WhiteBalance` (one id covering the entire WB
leaf — temp+tint+mode together, not three separate ids), `ParamId::Exposure`,
`ParamId::Contrast`, `ParamId::Highlights`, `ParamId::Shadows`, `ParamId::Whites`,
`ParamId::Blacks` (these six do match 1:1), and `ParamId::ToneCurve` (one id for "composite +
per-channel, one unit" — not a separate `curve.point`).

**Why / impact.** Phase A builds no panels and never touches `ParamId`, so this cannot block
Phase A's exit bar — it is recorded here **only** because A0's mandate is to check the spec's
`ParamId` spellings against real shipped code. **Phase E must design `panels/basic.rs`'s
doctest-pinned mapping table (§6.5) against the real coarse enum**, which means the WB panel's
three sliders (temp/tint) and mode combo all address the *same* `ParamId::WhiteBalance` and the
panel-side code (not E09) is responsible for decomposing/recomposing the leaf's sub-fields — a
materially different design than "one slider, one `ParamId`" the spec's draft implies for WB.
Flagging this now so Phase E's planner reads `lightbox-edit::leaves::WhiteBalance`'s actual
field shape before writing `basic.rs`, rather than discovering the mismatch mid-implementation.

### A1 — `filmstrip.rs` ships only the geometry + a minimal `filmstrip_ui` (pulls forward part of Phase B)

**What.** A1's chassis AC needs a working "bottom filmstrip strut" and A7's smoke AC needs a
literal "filmstrip row → canvas seam" proof — neither is satisfiable with an inert placeholder.
`crates/lightbox-shell/src/filmstrip.rs` (repurposing the retired `grid.rs`'s virtualization
math, per the spec's own crate-map note) therefore ships `StripLayout`/`strip_layout`/
`visible_range` (B1's geometry) and a working `filmstrip_ui` (the functional core of B2:
virtualized cells over `WorkingSetView`, demand-driven `ThumbCache` requests, active-ring
highlight, click-to-activate, Loading/Failed/DuplicateOf placeholders, filename label) —
**deliberately without** B3's RAW/edited-dot/error-badge chrome, B4's multi-select
click-modifiers (`Selection`'s `⌘`/`⇧` semantics are not reintroduced — a plain click activates;
`WorkingSetView.selected: HashSet<usize>` exists per spec §6.2 but nothing populates it yet), or
B5's drag-resize/collapse/overflow-position-indicator.

**Why / impact.** Phase B should **extend** `filmstrip.rs` rather than re-author it — its
geometry functions are already unit-tested (mirroring the retired grid's test shapes: pitch/
total-width, windowing, end-clamping, `fit_rect`'s aspect/no-upscale invariants). Not done:
B2's own AC item "only visible cells materialize (visible-set assertion); scroll-out cancels
(counter assertion)" as a dedicated kittest — Phase A proves this indirectly (the smoke driver's
`filmstrip_shown` flag + the existing `ThumbCache` cancel-on-scroll-out unit tests, unchanged
from E01), not with a new interaction test of the strip itself. `film.toggle`/resize/badges/
overflow indicator are genuinely unbuilt, not merely untested.

### A1 (consequence) — `--perf-scroll` retired (not replaced); `nightly.yml` updated

**What.** A1's grep gate ("no `ImportAddInPlace`... remains in the shell") is incompatible with
keeping the old `perf.rs`/`ScrollDriver`, which drove the retired grid via
`Command::ImportAddInPlace`. Rather than hand-adapt it into something that scrolls a filmstrip
(pre-empting H2, which explicitly owns "replace `--perf-scroll` with `--perf-strip`" as its own
task with its own AC — a sawtooth *horizontal* scroll over a 300-entry synthetic set + `lbx-perf`
scenario wiring), Phase A **removes** `perf.rs` and the `--perf-scroll` flag outright.
`.github/workflows/nightly.yml`'s `grid-scroll` job (already `continue-on-error: true`,
observational) is left in the repo but gated `if: false` with a comment pointing at this
deviation and H2, rather than silently left invoking a now-nonexistent flag.

**Why / impact.** A real, functioning grid-scroll perf capture existed on `main` before this
phase and does not exist after it until H2 ships `--perf-strip`. This is a genuine, temporary
capability gap on the nightly workflow (not a regression felt in the PR-blocking exit bar,
which never ran `--perf-scroll`). Named explicitly per the honest-reporting rule.

### A4 — macOS "Open With" / Apple Events: investigated, not wired; honest fallback

**What.** Read `winit` 0.30.13's own `platform::macos` module docs (the pinned version, no
guessing): winit does **not** surface `application:openURLs:`/`application:openFile:` through
its cross-platform `WindowEvent`/`ApplicationHandler` API at all. The *only* documented path is
registering a fully custom `NSApplicationDelegate` via raw `objc2`/`objc2-app-kit` bindings
**before** the event loop starts (winit explicitly guarantees it registers no delegate of its
own, precisely so a caller can do this) — a nontrivial, platform-specific shim that `eframe`
provides no hook for (eframe owns the event-loop bootstrap; there's no seam to inject a
pre-`run_native` delegate registration without forking/wrapping `eframe::run_native` itself).

**Why / impact — the named fallback (spec R3/Q1).** Not wired in Phase A. macOS entry is
drop + OS-dialog + CLI-argv (three of the mandate's four §2.4 entry rows); the fourth
(file-association / "Open With") needs either (a) a future small `objc2` shim once the app is
actually bundled with an `Info.plist` `CFBundleDocumentTypes` (E16 packaging — there is no
`.app` bundle to register file types against yet, so this genuinely cannot be usefully tested
today even if built), or (b) an eframe/winit upstream feature. Filed as the spec's own
anticipated follow-up, owner E16. `A4`'s other half — argv paths at launch — **is** built and
verified (`ShellOptions::initial_paths`/`initial_recursive`, submitted in `LightboxApp::new`
before the first frame; manually verified: `lightbox --catalog <dir> <path>` opens a real
window with the given file with no errors in the log).

### A5 — `WorkingSetView::on_event`/`on_lagged` re-pull wholesale by design (see A0-3); "epoch guard" is a pre-pull short-circuit, not a post-hoc rollback

Cross-reference to A0-3 — recorded here too since it's A5's own numbered AC. No further detail
beyond A0-3.

### A6 — replace-set cancellation rides the existing `ThumbCache`/scheduler mechanisms verbatim; no new cancel plumbing added

**What.** The spec's A6 AC asks for "in-flight thumb/render tickets for the old epoch
cancelled." Phase A does not add any new per-epoch cancellation bookkeeping. Instead:
* **Thumbs:** the filmstrip only ever iterates the *current* `WorkingSetView::entries()` (a new
  epoch's own item list), so on the very next `ThumbCache::end_frame(visible, cap)` call, every
  old-epoch `ImageId` that isn't in the new epoch's `visible` set is cancelled/evicted by the
  unchanged E01 scroll-out/LRU logic — no epoch tag needed on the thumb cache itself.
* **Canvas:** the existing push-model `RenderScheduler` (unchanged since E05 Phase F5) already
  supersedes an in-flight render the moment a different `image: ImageId` is submitted
  (latest-wins per-image-slot coalescing); Phase A additionally calls `LoupeView::exit()` -
  freeing the registered egui texture - on the specific transition to "no active image" (a
  replace into an all-unready/empty set), which the E01 code never needed to handle since the
  grid/loupe toggle made that transition impossible.

**Why / impact.** Reuses proven E01/E05 mechanisms rather than inventing epoch-aware
cancellation the underlying systems don't need (`ThumbCache` is keyed by `ImageId`, which is
stable across a re-open of the same content — see A2/A6's `clear_failures()` note below — so an
epoch tag would be redundant with "not currently visible"). Also newly wired: a previously
`Failed` thumbnail is retried on every new `WorkingSetOpening` (`ThumbCache::clear_failures()`),
since a replace may reopen the exact same content hash (same `ImageId`) at a fixed/relocated
path — the one place Phase A *did* add epoch-adjacent behavior, and it's a retry, not a cancel.

### A7 — smoke driver rework: real end-to-end run, not just compiled

**What.** `smoke.rs`/`main.rs` rewritten to submit `Command::OpenWorkingSet` (once) instead of
`Command::ImportAddInPlace`, and the success condition now requires **both**
`outcome.filmstrip_shown` (set the first frame the bottom filmstrip strut renders a non-empty
working set) **and** `outcome.seam_proven` (unchanged: an `Engine::submit`-produced texture
composited on the shared device) before the frame minimum is honored — a more literal proof of
"filmstrip row → canvas seam" than the old seam-proven-alone check.

**Verified for real** (not just "it compiles"): `cargo run -p lightbox-shell --bin lightbox --
--smoke 60` and `-- --smoke 120` (the exact invocation `cargo xtask exit-drill` uses) both exit
0 on this machine's real macOS/Metal device (`adapter="Apple M5 Max"`), printing
`seam-2 smoke: frames=61 texture_swaps=1 filmstrip_shown=true seam_proven=true` /
`OK: OpenWorkingSet → filmstrip → canvas drove an engine texture zero-copy on the shared wgpu
device`.

### ★ Folder explorer (mandate v2.2) — rows, not thumbnails; reuses `OpenDialog` origin (see A0-7)

**What.** `crates/lightbox-shell/src/explorer.rs` drives `lightbox_core::browse_dir` (E04's
already-shipped, verbatim-matching-spec primitive — no delta to record there) for Finder-style
immediate-children navigation (descend into a clicked subfolder, ascend via Up/back-history),
lists contained images as **filename rows** rather than real pixel thumbnails. The spec's own
wording ("pickable thumbnails/rows") explicitly sanctions rows as an alternative. Real
thumbnails would need an `ImageId` — a catalog concept that doesn't exist for an unopened
filesystem path — so generating one would require either depending on `lightbox-ingest` from
the shell (an explicit non-goal, §2.3: "no `lightbox-ingest` dependency from the shell" — and
indeed `lightbox-shell/Cargo.toml` has none, confirmed by grep) or ad hoc decoding outside the
established `PreviewProvider` seam. Kept simple per the phase brief.

**Persistence discipline verified.** `browse_dir` genuinely persists nothing (confirmed by
reading its implementation and its own test suite: every call re-`read_dir`s live); the
explorer's own state (`current`, `listing`, `history`) is plain in-memory UI state, discarded on
close — no catalog rows, no `folder` table writes, matching mandate v2.2's binding "live and
ephemerally."

---

## Cross-cutting notes

### License / dependency audit (A3, "Deps" §3)

`rfd 0.17.2` (MIT) added at the workspace level. Default features on this build already select
the GTK-free XDG-portal backend on Linux (`ashpd`/`zbus`, not `gtk3`) — confirmed via
`cargo add --dry-run`'s feature listing before pinning, matching the spec's explicit guidance
("prefer the GTK-free portal backend"). `egui_kittest 0.35.0` (MIT/Apache-2.0) added as a
**dev-dependency only** (features `wgpu`, `snapshot`), lockstep with the pinned `egui`/`eframe`
0.35.x. `cargo deny check` (licenses/bans/sources/advisories, the full gate, not just
licenses) is green with both added — **no new `deny.toml` exceptions needed** for either crate's
resolved dependency graph (`rfd`'s macOS leg pulls `objc2-app-kit`/`objc2-foundation`/etc., all
MIT/Apache-2.0/Zlib, already-allowlisted license families).

### UI testing (spec §9 / phase brief "keep it simple + tested")

Real `egui_kittest::Harness` tests were written for the empty-state drop zone
(`crates/lightbox-shell/src/empty_state.rs` — extracted to a free function specifically so it's
testable without a `Session`/GPU: `Harness::new_ui` + `get_by_label_contains`, 4 cases covering
no-hover / known-count / unknown-count / singular-vs-plural). The A2 drop-intake AC ("kittest-
injected drop... emits exactly one command") and the A5 working-set-view-model AC are instead
covered by lower-level, arguably stronger tests: `intake::pump` is tested directly against real
`egui::InputState`/`RawInput` values (constructed via a real `egui::Context::begin_pass`, not a
full `Harness`) — since `pump` is a pure function of `InputState`, this is a more precise unit
test than driving a whole running app through a synthetic OS drop event; and `WorkingSetView`'s
epoch/lag/activation rules are tested against a **real headless `Core`/`Session`** (A0-6) rather
than a kittest-mocked one, since the thing actually being proven (epoch consistency, event
ordering) lives in `lightbox-core`, not in any widget tree. `folder_explorer`'s tests exercise
the real `browse_dir` primitive against real temp-directory fixtures (not kittest — there's no
interactive widget behavior worth simulating beyond what's already proven: state transitions on
navigate/error).

### Not built in Phase A (named, not silently skipped)

* **A8 (second-instance forwarding)** — named cut-line in the spec itself (§8: "A8... CUT-LINE");
  not attempted. A second `lightbox <file>` launch opens a second independent process.
* Filmstrip badges (RAW/edited/error), drag-resize, multi-select modifiers, overflow position
  indicator — see A1 above (Phase B).
* `--perf-strip` — see A2/A3/A4 above (Phase H, H2).
* Keymap, prefs store, develop panels, gizmo framework — not started; the right-rail placeholder
  and top-bar zoom toggle are the only "view controls" Phase A ships (Phases D/E/F/G).

---

## Phase B — session filmstrip (B1–B5)

### B2/B3 — `filmstrip_ui` renders a shell-owned `StripCell` via a lazy accessor, not `&[WorkingSetItem]` / `&mut WorkingSetView` directly

**What.** The spec's §6.3 draft signature takes `view: &mut WorkingSetView`. The shipped
signature is `filmstrip_ui(ui, state, total, active, cell_of: &dyn Fn(usize) -> StripCell,
thumbs, visible_out)` where `StripCell` is a small shell-side projection (filename, raw flag,
edited flag, load status) built by `cell_for(&WorkingSetItem, &EditedBadges)`.

**Why.** Three forcing functions. (1) `WorkingSetItem` is `#[non_exhaustive]` (A0-6): kittest
cannot construct synthetic 200-entry sets from it, and B2/B4's ACs demand exactly that. (2)
`is_edited` is **not** on the working-set snapshot — E04 never shipped the spec draft's
`ReadyEntry.is_edited`; the real source is E09's `edit_index` projection via
`Queries::edit_badges` — so a join point is needed anyway, and `cell_for` is that one place.
(3) The lazy accessor makes the B2 virtualization AC directly assertable: the tests count
`cell_of` invocations per frame and prove only the visible window materializes.

### B3 — edited dot sourced from an event-driven `EditedBadges` cache over `Queries::edit_badges`

**What.** A `HashMap<ImageId, bool>` refreshed by one batch `Queries::edit_badges` call, but
**only** when marked dirty — on `Event::EditCommitted`, on `Event::WorkingSet*`, and on
broadcast `Lagged`. Steady-state frames never touch the reader (§7 "no SQL in the frame loop";
`edit_badges` is the query E09 built and documented for exactly this consumer). Failures log
and keep the previous lookup — never fatal. Proven against a real headless session: a real E09
gesture commit (`EditHub` begin/update + `Command::Edit(CommitGesture)`) flips exactly the
edited image's badge after a dirty-refresh, and a clean-cache refresh is proven a no-op.

### B3 — "kittest snapshot" implemented as structural widget-tree assertions, not image goldens

**What.** The B3 AC says "kittest snapshot with a mixed set". Shipped as structural
AccessKit-tree assertions: each cell's accessible name carries the §6.3 chrome
("IMG_0001.CR3 · RAW", "beach.jpg · edited", "broken.jpg · failed: unsupported codec",
"slow.jpg · loading"), asserted with `get_by_label_contains`, plus a
`query_all_by_label_contains` sweep proving no star/flag/rating chrome exists.

**Why.** The spec's own test plan (§9) defines E08's visual tests as "structural snapshots
(widget trees), immune to render-backend drift" — image goldens would couple CI to font/GPU
rasterization across the 3-OS matrix. Side benefit: the accessible labels are a head start on
H1 (AccessKit names for filmstrip cells with filename+state). The **painted** chrome (RAW tag
rect, edited dot, shimmer, placards) is covered by the same code path but its pixels are not
golden-tested, consistent with §9. The `egui_kittest` `snapshot` feature stays enabled and
unused pending an owner decision on committing goldens.

### B4 — multi-select state lives on `FilmstripState`, not `WorkingSetView`

**What.** §6.2 sketches `selected: HashSet<usize>` on the view model (and Phase A landed a
private, unused field there). Phase B's working brief froze the working-set view model, so the
⌘/⇧-click selection shipped on `FilmstripState` (`BTreeSet<usize>` + anchor, cleared on epoch
change via `sync_set`, exposed read-only via `selected()`). The Phase-A field on
`WorkingSetView` remains dormant/private. **Follow-up owner:** Phase E (or the first M2
batch-op consumer) should collapse the two into one home — recommendation: lift
`FilmstripState`'s into the view model then, since batch ops act on the set, not the strip.

### B4 — arrow-key nav is split between the loupe (unchanged) and an app-level route; keymap comes in Phase D

**What.** `nav.next`/`nav.prev` as spec'd are keymap actions — Phase D. Phase B ships
`filmstrip::nav_delta` (counts `Event::Key` presses so key-repeat steps once per repeat, with
the same `egui_wants_keyboard_input` text-focus guard the loupe uses). The loupe's existing T26
arrow handling is untouched (Phase B must not modify the canvas/loupe); `lib.rs` gates the
app-level route on `active_image().is_none()` so exactly one component acts per press — the
loupe while a Ready image is mounted, the app-level route for Loading/Failed/none states.
Both routes carry a `TODO(E08 Phase D)` to collapse into the keymap registry.

### B4 — nav latency AC scoped to a virtualization + generous wall-clock tripwire; the formal p95 gate is H2 (per the spec's own note)

**What.** The B4 AC's "frame p95 < 16 ms and nav-swap p95 < 50 ms ... asserted by the perf
harness in H2" is explicitly H2's job (release profile, dev-baseline machine, `--perf-strip`).
The Phase B test (`arrow_nav_through_200_entries_stays_virtualized`) drives 199 real arrow-key
frames through kittest and asserts (a) per-frame cell materialization stays O(visible) (≤ 60,
actual ~10), (b) one working-set step per press, (c) the viewport followed to the far end, and
(d) a debug-build per-frame p95 tripwire of 200 ms (headroom over the ~1–5 ms observed; catches
order-of-magnitude regressions without CI flakiness). The nav-swap latency probe itself
(`LoupeView::nav_swap_ms`, F1 overlay) is untouched and still feeds the future H2 harness.

### B5 — drag-resize rides the egui panel's native `resizable`/`size_range`; the drag gesture itself is not kittest-driven

**What.** The strip mounts as `egui::Panel::bottom(...).resizable(true).size_range(48.0..=160.0)`
with the post-frame height read back into `FilmstripState::set_height_pt` (clamped to the same
48–160 pt band, unit-tested). The panel's resize-drag gesture is egui-native behavior with no
AccessKit node, so no kittest test drags the handle; the tested surface is the clamp + the
read-back seam + the collapse round-trip (kittest: strip → collapsed bar with correct "34/212"
indicator and zero materialized cells → re-expand snaps back to the active cell).

### B5 — persistence deferred to Phase G by design (in-session state + named seam, no prefs file invented)

**What.** The spec's B5 AC ("resize persists across app restart (prefs round-trip)") depends on
§6.7's machine-scope prefs store — Phase G, not built. Per the phase brief, `height_pt`/
`collapsed` are **in-session only**; the seam for Phase G is documented in `filmstrip.rs`'s
module docs (construct `FilmstripState` from `MachinePrefs::filmstrip_height_pt` + a collapsed
flag at boot; write the accessors back on change; the panel's `default_size(strip.height_pt())`
already seeds from whatever Phase G restores). No competing prefs file was created.

### B-misc — collapsed strip uses a distinct panel id; expanded/collapsed are two panels

**What.** `lightbox-filmstrip` (expanded, resizable) vs `lightbox-filmstrip-collapsed` (exact
24 pt bar). egui remembers panel sizes by id — a shared id would replay the 24 pt collapsed
size onto the expanded strip. Two ids keep both memories correct.

### B-misc — pre-existing flake observed once in `lightbox-preview` unit tests during the exit-bar runs

**What.** One full-workspace test run during Phase B's exit-bar sequence had 1 failure in the
`lightbox-preview` lib suite (108 tests) — the same load-sensitive suite HEAD commit `76f8a97`
("E03 test-flake fix: bump preview-service async-build poll deadlines... load-tolerant") had
just patched. Phase B touches nothing in `lightbox-preview`. Two immediately subsequent full
workspace runs (and the final gate run) were 100% green. Recorded here for honesty; owner E03
if it recurs.

### Not built in Phase B (named, not silently skipped)

* **`film.toggle` (⇧Tab) keybinding** — the collapse toggle is click-only until Phase D's
  keymap registers `film.toggle` against `FilmstripState::set_collapsed`.
* **Prefs round-trip for height/collapsed** — Phase G (see the B5 deviation above).
* **Formal p95 perf gates + `--perf-strip`** — Phase H (H2), per the spec's B4 note.
* **Batch operations over the multi-selection** — M2; the selection state ships dormant.
