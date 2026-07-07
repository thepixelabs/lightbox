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
