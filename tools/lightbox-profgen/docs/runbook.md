<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Curated camera-profile content-line runbook (E02.5 / Phase G, task G7)

E02.5 is a **content-production line, not one-time code** (architecture §12).
This runbook is the operational half: what it costs to add one curated profile,
how often it runs, what we keep, and — critically — **who owns it**. The tooling
([`lightbox-profgen`](../)) is built and tested; the **content is DEFERRED** and
this runbook records the ownership escalation that gates it.

---

## 0. Current status (2026-07-06)

- **Tooling: BUILT.** Session schema (G1), dcamprof subprocess wiring (G1,
  feature-gated), validation harness (G3), packaging + provenance + catalog sync
  descriptor (G4), auto-selection policy (G5) all ship and are unit-tested.
- **Content: DEFERRED — ZERO profiles ship.** `assets/color/profiles/` is empty
  (spec §0). `dcamprof` (GPL-3) is absent from the build machine and no physical
  ColorChecker/IT8 capture sessions have been run. **Nothing is fabricated** — no
  fake profiles, no invented reference renders (spec §0 / R4).
- **Graceful by design.** Every camera still renders colorimetrically-correct
  color via the tier-1 license-clean matrix base + the tier-2 Lightbox look
  (spec §1.7). A missing curated profile is a *quality upgrade not taken*, never a
  broken image (Risk 10; the fallback is enforced by `resolve_profile_ref` in
  `lightbox-color` and surfaced to the user).
- **Owner: UNSTAFFED — escalated below (§Escalation).**

## 1. The pipeline (one profile, end to end)

```
capture (physical, §capture-protocol.md)
  → session.toml + raws
  → lightbox-profgen generate   (dcamprof make-target → make-profile, GPL-3 subprocess)  [G1]
  → lightbox-profgen validate   (parse via our F engine; patch ΔE vs reference; reject gates)  [G3]
  → lightbox-profgen package    (assets/color/profiles/<make>/<model>.dcp + MANIFEST entry)   [G4]
  → catalog sync-on-open        (lightbox-catalog H1 inserts the camera_profile row)
  → auto-selection              (CameraId → curated-if-present else matrix base)  [G5]
```

`dcamprof` is a **subprocess only** — never a crate dependency, never linked,
never bundled, and this whole tool is excluded from app packaging (spec §1.1 /
§7.6). The only artifact crossing the GPL boundary is a `.dcp` file, which we
re-parse with our own permissive-licensed engine before it is allowed near
`assets/`.

## 2. Per-body cost (planning estimate, to be trued-up by the owner)

Order-of-magnitude effort for **one** body once the line is staffed and equipped
(not yet measured — no sessions run):

| Phase | Effort (rough) | Notes |
|-------|----------------|-------|
| Capture | ~0.5–1.0 h | Dual-illuminant shoot + flat-field/exposure QC (§capture-protocol.md). Needs the body in hand or on loan. |
| Generation | ~5–15 min | `generate` (dcamprof, two stages) — mostly automated. |
| Validation | ~15–30 min | `validate` ΔE gates + an E5-style perceptual spot-check on a scene set. |
| Packaging + review | ~15–30 min | `package`, manifest entry, per-body golden commit, provenance sign-off. |
| **Total** | **~1.5–2.5 h/body** + acquisition/loan logistics | Dominated by physical access to bodies, not compute. |

The **hard** cost is not engineering time — it is **physical access to camera
bodies** and the **capture rig** (charts, Standard A + daylight sources, meter).
That is a budget/ops line, not a code task (architecture §12).

## 3. Cadence & batching

- The line **scales with investment**: N bodies per batch, driven by which bodies
  we own or can borrow (Open Question #1). The spec assumes **≥5 at epic close**
  (G6) — currently **0**.
- Prioritize by **install-base × fallback-quality-gap**: popular bodies whose
  matrix-base render most benefits from a curated LUT come first.
- Batch by shared rig setup (same illuminant booth session → several bodies).
- Each batch is one PR: profiles + manifest entries + per-body goldens + the
  review record, all reviewed together.

## 4. Target-shot raw retention

- **Keep every source raw** used to generate a shipped profile — they are the
  reproducibility record and the input to any re-profiling (e.g. when the
  demosaic floor changes at M2, or dcamprof updates).
- Store outside the app repo (raws are large and not shipped): a dedicated
  content-line archive keyed by `session_id`, with the `session.toml` alongside.
- Retention: **indefinite** for any session whose profile ships; a superseded
  profile's raws are kept at least until the replacement has shipped one release.
- The shipped side keeps only the `.dcp` + provenance (`profgen:<session_id>`,
  dcamprof version) — the archive is the bridge back to the pixels.

## 5. Validation gates (what "good" means)

- **Parses** with our own F-phase engine (`lightbox_color::dcp::parse_dcp`).
- **Patch ΔE** within the F5 tolerances: mean ΔE2000 ≤ 0.5, max ≤ 1.5 versus the
  dcamprof reference render and/or the measured chart values
  ([`validate.rs`](../src/validate.rs), `F5_GATES`).
- **Perceptual spot-check** (E5-style: skin, sky, foliage, neutrals, gradients,
  clipped highlights) — no hue skews versus the matrix base.
- **Provenance cleared**: `profgen:<session_id>`, project license, **no Adobe
  authorship marker** anywhere (constraint 4; `check_provenance` +
  the surface-3 manifest checker).

A profile failing any gate is **rejected with a report**, never hand-waved in.

## 6. Ownership

| Role | Responsibility | Assigned? |
|------|----------------|-----------|
| Content-line owner | Runs captures, curates the body list, signs off provenance + review | **UNASSIGNED** |
| Capture budget | Charts, illuminant sources, meter, body loans/rentals | **UNFUNDED** |
| Engineering | Maintains `lightbox-profgen` (this tool) | E02 (done for Phase G) |

## Escalation (restates architecture §12)

Per architecture **§12, "cto / operator (resourcing, not architecture)", item (2)**:

> **E02.5 is a content-production line, not one-time code** — an ongoing
> camera-profile pipeline needs an owner and a budget for physical ColorChecker/IT8
> target shots per body; without that owner the curated-profile tier does not exist
> and every camera falls back to the matrix base + default look (Risk 10). This is a
> headcount/ops decision outside the architect's seat.

**Escalated to: CTO / operator.** Two open decisions block G6 (real content),
neither of which the engineering phase can make (Open Questions #1, #2):

1. **Owner** — name a content-line owner accountable for captures, curation, and
   provenance sign-off. Until named, the curated tier does not exist.
2. **Budget + body list** — fund the capture rig and body access, and choose the
   initial N bodies (spec assumes ≥5). Until funded, N = 0.

**Consequence of no decision (the current, explicitly-accepted state):** Lightbox
ships with **zero curated profiles** and every body renders via tier-1 + tier-2.
This is correct and shippable — the degradation is graceful by design (spec §1.7
/ R4) — but it is a *deliberate product choice being defaulted into by inaction*,
which §12 requires be made explicit rather than silent. **This runbook is that
explicit record.** The moment an owner + budget land, the tooling here runs the
line without further engineering work.
