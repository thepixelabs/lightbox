# Licensing policy & enforcement

*Owned by E01 (spec §5 T3). Normative sources: `00-mandate.md` constraint 5,
`01-architecture.md` §1.6/§8, `epics/E01-foundation-workspace-catalog.md` §5 T3.*

## Project license

Lightbox itself is licensed **Apache-2.0** (satisfies the mandate's
"permissive or weak-copyleft" requirement; matches the Rust ecosystem's
dominant convention). Every file carries SPDX metadata — a header where the
format allows, a `REUSE.toml` annotation otherwise — and the repository is
kept **REUSE-compliant** (`reuse lint` is a PR gate). License texts live in
`LICENSES/`.

## The three CI license surfaces (architecture §8)

| Surface | What it audits | Tooling | Status |
|---|---|---|---|
| **1. Rust crate graph** | Every crate compiled into any Lightbox binary | `cargo deny check licenses bans sources` (`deny.toml`), PR-blocking | **Live since E01** |
| 2. Native/FFI binaries | Vendored/dynamically-linked C libraries, their build flags and transitive codecs (LibRaw, lensfun, libheif+libde265, FFmpeg, ORT EPs) | Per-artifact SBOM + build-flag audit | **E16.** Inventory is legitimately **empty** until E02+ introduces the first native dependency; E01 ships no native C libraries |
| 3. Bundled content/data | Camera profiles, default look family, LUT packs, lensfun data, model weights | Data manifest with per-item provenance + license | **E16.** Inventory empty at M0; E01 bundles no content. (The *test fixture* corpus already practices the discipline: `fixtures/manifest.toml` records URL, upstream checksum, xxh3-128 pin, and license per fixture) |

## Crate-graph policy (surface 1, enforced by `deny.toml`)

- **Allowlist** (exhaustive — anything else fails CI): MIT, BSD-2-Clause,
  BSD-3-Clause, Apache-2.0, Zlib, Unicode-3.0, ISC, CC0-1.0.
- **GPL / AGPL / LGPL are denied in the crate graph by construction** (not on
  the allowlist). LGPL components may enter the *product* only as
  dynamically-linked C libraries with relink capability in later epics — that
  is surface 2's jurisdiction, never `Cargo.toml`'s. GPL/AGPL never link
  in-process at all (subprocess isolation or clean-room reimplementation only).
- Additions to the allowlist require a licensing-review PR updating
  `deny.toml` **and** this document.
- `[sources]`: crates.io only; git/unknown registries denied.

**Gate verified (T3 acceptance):** on 2026-07-05 a canary crate with
`license = "GPL-3.0-only"` was added to the workspace; `cargo deny check
licenses` failed with `error[rejected]: license is not explicitly allowed`,
and passed again once removed. The gate runs PR-blocking in
`.github/workflows/ci.yml` (job `deny`).

## Conventions

- Every Rust/TOML/YAML file starts with the two SPDX tags (FileCopyrightText:
  "2026 Lightbox contributors"; License-Identifier: Apache-2.0) — copy the
  header from any existing source file.
- Test fixtures are **CC0 only**, pinned and provenance-tracked in
  `fixtures/manifest.toml`; the blobs themselves are never committed.
- Dependency-license review happens at `cargo deny` time; per-dependency
  exceptions (`[licenses.exceptions]`) need the same review as allowlist
  changes.
