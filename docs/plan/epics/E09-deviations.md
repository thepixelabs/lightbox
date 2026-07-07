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
`EditHub`/dispatcher and the catalog read/write DAOs that **Phase B owns and that
are not present in this worktree** (only the 0003 migration landed). Phase D
therefore ships the **operations those commands invoke**, fully tested in
isolation:

- `xmp_map::write_sidecar` / `read_sidecar` (recipe ↔ atomic sidecar, mandate-c.2
  safe — never touches the original);
- `xmp_map::XmpWriteCoalescer` — the pure, clock-injected trailing-debounce policy
  that makes the "50-commit burst ⇒ ≤2 writes" AC unit-testable **without** the
  E06 scheduler (its graceful synchronous fallback: flush eagerly on `due`);
- `sync::classify` — the pure divergence state machine (Phase C).

**Why.** T22's own spec permits a "graceful synchronous fallback when jobs absent
in tests"; the command arms are one dispatcher match each once Phase B's `EditHub`
exists. `ReadMetadata` applying as a `StepLabel::XmpRead` history step and the
opt-in (default **off**) auto-write pref are **DEFERRED** to that Phase B wiring;
the label variant already exists in the frozen `StepLabel` enum.

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
