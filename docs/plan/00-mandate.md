# Lightbox — Project Mandate

_This document is the fixed input to architecture planning. The architect designs within these constraints; the CTO approves against them._

## Mission

Build an open-source desktop application that a working photographer could adopt as their Lightroom replacement: one tool covering ingest → cull → organize → develop → export, non-destructive throughout, fast at six-figure photo counts.

## Hard constraints

1. **Local-first.** The catalog, previews, edit recipes, and ML inference all live and run on the user's machine. Any future sync is an optional layer, never a requirement.
2. **Non-destructive.** Originals are never modified. Edits are ordered parameter recipes, persisted in the catalog and exportable as XMP sidecars.
3. **Cross-platform desktop.** macOS and Windows at v1; Linux must not be architecturally excluded.
4. **Original implementation.** No proprietary Adobe product code, assets, icons, or trade dress, and no reverse-engineering of Adobe binaries. Interoperate through open/industry formats only (DNG, XMP, ICC, EXIF/IPTC).
   *Clarified 2026-07-04 (mandate-owner ruling, v1.1):* Adobe-**published, OSI-licensed open-source libraries** (e.g., the ISO 16684 XMP Toolkit reference implementation under BSD-3, c2pa-rs) are **permitted** under the same per-dependency license review as any third-party dependency, each use requiring explicit CTO sign-off. The bar targets Lightroom's protected expression and proprietary SDKs, not open-source code Adobe happens to author.
5. **License hygiene.** The application ships under a permissive or weak-copyleft license; GPL components may be used only where their license terms are compatible with the chosen distribution model — the architect must call this out explicitly per dependency.
6. **AI runs offline.** Masking, denoise, upscale, inpainting via locally-executed open models. Model downloads are allowed; inference calls to remote APIs are not part of v1.

## Quality bar (v1 acceptance themes)

- Import 10,000 raws with preview generation without blocking the UI.
- Cull at keyboard speed: next/prev + flag/reject with pre-rendered previews, no perceptible lag.
- Develop edits render interactively (target <100 ms slider-to-screen at fit-view on a mid-range GPU).
- Catalog operations (search, filter, smart collections) stay interactive at 100k+ assets.
- Crash-safe: catalog is transactional; a kill -9 never corrupts it.

## Non-goals for v1

- Cloud sync / mobile apps / collaboration.
- Print, Book, Slideshow, Web modules.
- Tethered capture.
- Plugin marketplace (design for extensibility, ship without).
- Video editing beyond basic playback/metadata.

## Success definition

A photographer can shoot a wedding, ingest and cull 3,000 raws, develop with global + AI-masked local adjustments, and deliver exported JPEGs — entirely in Lightbox, entirely offline, on a mid-range laptop.
