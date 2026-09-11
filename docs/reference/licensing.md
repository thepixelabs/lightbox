# Licensing policy & enforcement

*Owned by E01 (spec §5 T3). Normative sources: `00-mandate.md` constraint 5,
`01-architecture.md` §1.6/§8, `epics/E01-foundation-workspace-catalog.md` §5 T3.*

## Project license

Lightbox itself is licensed **`AGPL-3.0-or-later`**, with an attribution
requirement added under section 7(b) of that license and set out in
`COPYING.additional-terms`. PixeLabs holds the copyright and offers the same
code under a separate **commercial license** to anyone who wants to build it
into a product distributed under their own terms, or to run it as a networked
service, without the copyleft obligation.

Every file carries SPDX metadata, a header where the format allows and a
`REUSE.toml` annotation otherwise, plus one line pointing at the section 7
terms (section 7 paragraph 6 requires that pointer in the source files
themselves, not only in a top-level file). The repository is kept
**REUSE-compliant** (`reuse lint` is a PR gate). License texts live in
`LICENSES/`.

### Why the dependency policy did not relax

Going copyleft makes several previously-forbidden things legal. LibRaw could
now be linked in-process, `rawler` could replace the proxy, `lcms2`'s GPL-3
`fast_float` plugin could be enabled, and dcamprof could be linked rather than
invoked. **None of that should be done.**

Under Apache-2.0 the binding constraint on the dependency graph was the
outbound license. It is now the *commercial* license, and that is a strictly
tighter constraint: a dependency PixeLabs cannot sublicense to a paying
customer cannot ship in the product, whatever the AGPL would permit. The
`deny.toml` allowlist, the `rawler`/`dnglab` bans and the out-of-process
LibRaw proxy therefore all stay exactly as they are. Only their stated reason
changes, from "we ship permissive" to "we sell proprietary".

The same rule governs surface 3: bundled ML model weights (E13) must permit
commercial redistribution. Weights under CC BY-NC or research-only terms are
acceptable in an AGPL build and are a blocker for the commercial one, so the
data manifest records commercial redistributability per model.

### Contributor licensing

Because PixeLabs dual-licenses, inbound contributions need rights broader than
the AGPL grants. `CLA.md` covers that: a contributor keeps their copyright and
grants PixeLabs a licence to distribute the contribution under other terms,
including commercial ones. Without it, the first merged outside pull request
would end the commercial branch for the files it touched.

## The three CI license surfaces (architecture §8)

| Surface | What it audits | Tooling | Status |
|---|---|---|---|
| **1. Rust crate graph** | Every crate compiled into any Lightbox binary | `cargo deny check licenses bans sources` (`deny.toml`), PR-blocking | **Live since E01** |
| 2. Native/FFI binaries | Vendored/dynamically-linked C libraries, their build flags and transitive codecs (LibRaw, lensfun, libheif+libde265, FFmpeg, ORT EPs) | Per-artifact SBOM + build-flag audit | **E16.** Inventory is legitimately **empty** until E02+ introduces the first native dependency; E01 ships no native C libraries |
| 3. Bundled content/data | Camera profiles, default look family, LUT packs, lensfun data, model weights | Data manifest with per-item provenance + license | **E16.** Inventory empty at M0; E01 bundles no content. (The *test fixture* corpus already practices the discipline: `fixtures/manifest.toml` records URL, upstream checksum, xxh3-128 pin, and license per fixture) |

## Crate-graph policy (surface 1, enforced by `deny.toml`)

- **Allowlist** (exhaustive, anything else fails CI): MIT, BSD-2-Clause,
  BSD-3-Clause, Apache-2.0, Zlib, Unicode-3.0, ISC, CC0-1.0.
- **GPL / AGPL / LGPL are denied in the crate graph by construction** (not on
  the allowlist). LGPL components may enter the *product* only as
  dynamically-linked C libraries with relink capability in later epics, that
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
  "2026 Lightbox contributors"; License-Identifier: Apache-2.0), copy the
  header from any existing source file.
- Test fixtures are **CC0 only**, pinned and provenance-tracked in
  `fixtures/manifest.toml`; the blobs themselves are never committed.
- Dependency-license review happens at `cargo deny` time; per-dependency
  exceptions (`[licenses.exceptions]`) need the same review as allowlist
  changes.

## Bundled UI font assets (surface 3, early)

The dark-theme overhaul (design spec `theme-spec.md` §9) embeds two
typefaces directly in the `lightbox-shell` binary via `include_bytes!`
(`crates/lightbox-shell/src/theme/fonts.rs`), ahead of E16's formal surface-3
inventory process, recorded here so the obligation isn't silently deferred:

| Asset | Files | Upstream release | License |
|---|---|---|---|
| Inter (UI text, panel headers, labels, buttons, menus, tooltips, filmstrip metadata) | `Inter-Regular.ttf`, `Inter-Medium.ttf`, `Inter-SemiBold.ttf` | rsms/inter v4.1, static hinted desktop TTFs (`extras/ttf/`) | SIL OFL 1.1 |
| JetBrains Mono (tabular numeric readouts, slider values, EXIF/histogram numbers, pixel coordinates) | `JetBrainsMono-Regular.ttf` | JetBrains/JetBrainsMono v2.304 (`fonts/ttf/`) | SIL OFL 1.1 |

Both are redistributable with no attribution burden beyond bundling the
license text (OFL's "reserved font name" clause only restricts renaming a
*modified* font, not embedding the original). Files live under
`crates/lightbox-shell/assets/fonts/`; since TTFs can't carry SPDX header
comments, they're covered by `REUSE.toml` annotations (per-typeface
copyright, `SPDX-License-Identifier = "OFL-1.1"`). The canonical OFL-1.1
license text is checked in at `LICENSES/OFL-1.1.txt` (the two upstream
projects ship near-identical OFL-1.1 boilerplate with only the copyright
preamble differing, that per-project attribution lives in the `REUSE.toml`
annotation, not as a second license file, to avoid two "the license" files
disagreeing in the same directory).
