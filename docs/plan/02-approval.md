# Lightbox v1 Plan — Approval Record

_Approved 2026-07-04. This document is the governance trail for `01-architecture.md` and the 16 epic specs in `epics/`._

## 1. Process

The plan was produced and ratified by a multi-agent governance pipeline:

1. **Research** — 10 parallel domain researchers over Lightroom product documentation → 352-feature inventory → synthesized, MoSCoW-prioritized feature catalog (`../research/00-feature-catalog.md`).
2. **Design** — system-architect authored `01-architecture.md` against the mandate (`00-mandate.md`) and the catalog.
3. **CTO review loop** — three review rounds, each with concrete blocking issues and an architect revision.
4. **Independent architect sign-off** — consistency audit with surgical edits.
5. **Mandate-owner ruling** — resolved the round-3 hard-constraint conflict (mandate amended to v1.1).
6. **CTO confirmation (round 4)** — **APPROVED**, no blocking issues.
7. **Epic planning** — 16 parallel staff-engineer planners produced the specs in `epics/` (679 tasks total).

## 2. CTO review trail

| Round | Verdict | Blocking issues | Resolution |
|---|---|---|---|
| 1 | ✗ | (a) Uniform "1–4 week" epic sizing contradicted the doc's own risk analysis of VHigh critical-path epics; (b) license-enforcement gate (cargo-deny) blind to the non-Rust surface where GPL risk concentrates | Honest VHigh sub-project decomposition (§10.1); license gate extended to multiple surfaces (FFI/vendored/bundled-data) |
| 2 | ✗ | Camera-profile **data** provenance unaddressed — DCP *engine* designed, but source of non-Adobe per-camera profiles (the "Adobe-Color-analogue" Must feature) unbudgeted and unaudited by CI | Curated camera-profile content line budgeted in E02 (§10.1); provenance added to the license/CI surface |
| 3 | ✗ | Adobe XMP Toolkit (BSD-3) selected as RDF engine vs. mandate constraint 4 "No Adobe code, SDKs" — unreconciled hard-constraint conflict | Escalated to mandate-owner (§3 below) |
| 4 | **✓ APPROVED** | none | Recommendations (all annotation-level) applied to `01-architecture.md` |

**Independent architect sign-off** (between rounds 3–4): approved with edits — removed the then-barred Adobe toolkit language, fixed a phantom M0→E02 dependency, gave baked AI-mask rasters a home in the `.lbdata` layout, clarified the E01/E05 RenderNode seam, and flagged three residual gaps (metadata-editing epic home → E07; perf-prefs panel → E08; recipe/mask/retouch data authority → resolved in §3.1). All three closed in the reconcile pass.

## 3. Mandate-owner ruling (mandate v1.1)

Constraint 4 was clarified: **proprietary Adobe product code, assets, icons, trade dress, and reverse-engineering remain barred**; **Adobe-published, OSI-licensed open-source libraries are permitted** (ISO 16684 XMP Toolkit BSD-3, c2pa-rs) under standard per-dependency license review with explicit CTO sign-off. Rationale: the constraint targets Lightroom's protected expression, not open-source code Adobe happens to author; barring ISO reference implementations adds cost without reducing legal risk.

Under that ruling, the architecture's plan of record (chosen on engineering grounds): **ISO 16684 XMP Toolkit as the RDF substrate** behind Lightbox's own `crs:`/`lb:` mapping layer, own quick-xml implementation as the named reversal fallback. **CTO per-dependency license sign-off for the toolkit: GRANTED (round-4 review, 2026-07-04).**

## 4. Irreversible-commitment ratification

Per the architecture's one-way-door protocol, the two irreversible commitments are ratified by the mandate-owner, on record here:

- **Rust as the core language** (§1.1) — ratified 2026-07-04.
- **SQLite as the on-disk catalog format** (§1.4, versioned schema from day one) — ratified 2026-07-04.

E01 (Foundation) is unblocked.

## 5. Approved scope summary

- **Stack**: Rust workspace; egui/eframe shell sharing one wgpu device with the render engine; wgpu compute node-graph DAG (vkdt pattern); SQLite (WAL, FTS5, sqlite-vec) catalog as single source of truth, XMP as opt-in projection; out-of-process ONNX inference (`lightbox-inferd`) with CoreML/DirectML/CPU execution providers.
- **Consistency contract**: per-(process_version, engine_build) determinism with cross-backend agreement ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB, golden-image tested; GPU/CPU bit-identity deliberately not promised.
- **Plan**: 16 epics (E01–E16), milestones M0–M4, ~130–155 person-weeks, parallel streams (catalog/DAM, pixel engine, edit/interop, ML).
- **License policy**: MIT/BSD/Apache link freely; LGPL dynamic-link only; GPL never in-process; per-model weight review; multi-surface CI enforcement (Rust crates, FFI/vendored code, bundled data/profiles/models).
