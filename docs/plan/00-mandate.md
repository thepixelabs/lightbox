# Lightbox — Project Mandate

_This document is the fixed input to architecture planning. The architect designs within these constraints; the CTO approves against them._

> **v2.0 (2026-07-05, mandate-owner):** product refocus. Lightbox is an **editing-first raw developer** — color correction, tone, local adjustments, retouch, AI-assisted enhancement. The DAM/library ambition of v1.x (catalog-centric organization, collections, keywords, ratings/culling, faces, semantic search) is **cut from scope**; only the minimal browsing needed to open and batch photos remains. §"Superseded" below records what changed.

## Mission

Build the open-source desktop raw editor a photographer reaches for to **develop, fix, and enhance** photos: open a folder → develop (global + local + AI-assisted) → export. Non-destructive throughout, reference-grade color, interactive on a mid-range GPU.

## Hard constraints

1. **Local-first.** Edit recipes, previews, caches, and ML inference all live and run on the user's machine. Any future sync is an optional layer, never a requirement.
2. **Non-destructive.** Originals are never modified. Edits are ordered parameter recipes, persisted locally and exportable as XMP sidecars.
3. **Cross-platform desktop.** macOS and Windows at v1; Linux must not be architecturally excluded.
4. **Original implementation.** No proprietary Adobe product code, assets, icons, or trade dress, and no reverse-engineering of Adobe binaries. Interoperate through open/industry formats only (DNG, XMP, ICC, EXIF/IPTC).
   *Clarified 2026-07-04 (mandate-owner ruling, v1.1):* Adobe-**published, OSI-licensed open-source libraries** (e.g., the ISO 16684 XMP Toolkit reference implementation under BSD-3, c2pa-rs) are **permitted** under the same per-dependency license review as any third-party dependency, each use requiring explicit CTO sign-off.
5. **License hygiene.** The application ships under a permissive or weak-copyleft license; GPL components may be used only where their license terms are compatible with the chosen distribution model — called out explicitly per dependency.
6. **AI runs offline.** Masking, denoise, upscale, inpainting via locally-executed open models. Model downloads are allowed; inference calls to remote APIs are not part of v1.

## Quality bar (v1 acceptance themes)

- Develop edits render interactively: target **<100 ms slider-to-screen** at fit-view on a mid-range GPU; mask-heavy edits stay interactive.
- **Color fidelity is the product.** Per-(process_version, engine_build) deterministic rendering, golden-image tested (ΔE2000 ≤ 1.0 cross-backend); halo-free highlight/shadow recovery; profile-accurate camera color.
- An edit made today renders identically years from now (process versioning).
- Opening a 1,000-photo shoot folder shows browsable thumbnails without blocking the UI.
- Batch-export a 500-photo folder with recipes applied, unattended, crash-free.
- Crash-safe: the edit store is transactional; a kill -9 never loses committed edits.

## Scope shape

- **Core (the product):** raw decode & camera color, the full develop toolset (tone, curves, HSL/color mixer, color grading, white balance, presence), detail (sharpen/NR incl. AI denoise), optics & geometry, masking & local adjustments (incl. AI subject/sky masks), retouch (heal/clone/remove), enhance (super-resolution), presets, snapshots/versions, batch develop-settings application, export.
- **Supporting (minimum viable):** **there is no library.** Entry points are exactly two: **drag-and-drop** (a single image, a multi-file selection, or a folder dropped onto the app or its empty-state drop zone) and the **OS file-open dialog**. Either lands directly in the editor with a filmstrip of the currently-opened set — nothing more. Photos open in place (no managed import). **Raw files expose the full develop toolset** (white balance with Kelvin/tint, camera profiles, highlight reconstruction, full-latitude tone recovery); JPEG/TIFF/PNG/HEIC open through the same pipeline with the raw-specific stages inapplicable — mirroring the Lightroom develop experience. Edits persist automatically (internal edit store + optional XMP sidecars) so reopening a file restores its recipe; the store is plumbing, never a user-facing library.
- **Cut (was v1.x DAM scope):** the entire library/DAM concept — any persistent browsing surface (grid library view, folder browser panel), catalog-centric organization, collections/albums, smart collections, hierarchical keywords, ratings/flags/labels workflows, culling views, faces/people, semantic search, duplicate detection, managed copy/rename/backup ingest, watched folders.

## Non-goals for v1

- Everything in "Cut" above.
- Cloud sync / mobile / collaboration.
- Print, Book, Slideshow, Web modules; tethered capture.
- Plugin marketplace (design for extensibility, ship without).
- Video beyond opening a frame for editing.

## Success definition

A photographer opens a 500-raw shoot folder, color-corrects and enhances the keepers with global + AI-masked local adjustments and retouch, applies a look across the set, and batch-exports delivery JPEGs — entirely in Lightbox, entirely offline, on a mid-range laptop.

## Superseded (v1.x → v2.0)

- The v1.x mission ("one tool covering ingest → cull → organize → develop → export") is replaced by the editing-first mission above.
- v1.x quality bars tied to DAM scale (import 10,000 raws with preview generation; keyboard-speed culling; 100k+ asset catalog interactivity) are dropped. The crash-safety bar is retained, rescoped to the edit store.
- Research (`../research/`) and the v1.x feature catalog remain valid source material; their DAM sections are historical context, not requirements.
