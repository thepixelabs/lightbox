# Lightbox — Project Mandate

_This document is the fixed input to architecture planning. The architect designs within these constraints; the CTO approves against them._

> **v2.0 (2026-07-05, mandate-owner):** product refocus. Lightbox is an **editing-first raw developer** — color correction, tone, local adjustments, retouch, AI-assisted enhancement. The DAM/library ambition of v1.x (catalog-centric organization, collections, keywords, ratings/culling, faces, semantic search) is **cut from scope**; only the minimal browsing needed to open and batch photos remains. §"Superseded" below records what changed.
>
> **v2.1 (2026-07-05, mandate-owner):** two core-scope strengthenings. (1) **AI Looks** — image-adaptive cinematic grading — is promoted to Core scope (see Scope shape). (2) Raw files must expose the **complete raw develop parameter surface**, not a curated subset.
>
> **v2.2 (2026-07-09, mandate-owner):** clarification of the "no library" cut. A **transient, live-filesystem folder explorer** — Finder/Explorer-style navigation into folders *and their nested subfolders* to find and open photos — is **permitted** as a third entry/navigation aid. It reads the real filesystem live and ephemerally; it is a viewer, not a library. What remains **cut** is any **persistent** library: no separate catalog/library database of the user's collection (Lightroom's `.lrcat` model), no managed import, no collections/keywords/ratings/flags, no library-of-record. (The SQLite store is a per-image *edit-recipe* store keyed by content hash, not a library catalog; the thumbnail cache is a disposable render cache.) Emphasis reaffirmed: depth in **color management + editing** over anything library-like.

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
  - **Complete raw parameter surface (v2.1):** opening a raw exposes *everything* the pipeline can vary — white balance (as-shot/presets/Kelvin+tint/eyedropper), camera profile selection, demosaic-stage options, black/white point, highlight reconstruction mode, full-latitude tone recovery, per-channel calibration — not a curated subset. Non-raw sources show the same UI with raw-only stages disabled.
  - **AI Looks — image-adaptive cinematic grading (v2.1):** a locally-run looks engine that analyzes the opened image's palette and tonal distribution and proposes varied cinematic grades (teal-orange, film-stock families, bleach bypass, etc.) *fitted to the image's actual colors* — plus a shuffle/variations action for random-but-coherent exploration. Every proposed look materializes as an ordinary, fully-editable develop recipe (color grading + curves + HSL deltas) layered on the user's current edit — never an opaque baked filter. Offline per constraint 6; classical palette analysis first, models only where they earn their footprint.
- **Supporting (minimum viable):** **there is no persistent library.** Entry points are three: **drag-and-drop** (a single image, a multi-file selection, or a folder dropped onto the app or its empty-state drop zone); the **OS file-open dialog**; and (v2.2) a **transient live-filesystem folder explorer** for navigating into nested folders to pick photos. Any of them lands directly in the editor with a filmstrip of the currently-opened set — nothing more. The explorer browses the real filesystem live; it never builds or persists a library index. Photos open in place (no managed import). **Raw files expose the full develop toolset** (white balance with Kelvin/tint, camera profiles, highlight reconstruction, full-latitude tone recovery); JPEG/TIFF/PNG/HEIC open through the same pipeline with the raw-specific stages inapplicable — mirroring the Lightroom develop experience. Edits persist automatically (internal edit store + optional XMP sidecars) so reopening a file restores its recipe; the store is plumbing, never a user-facing library.
- **Cut (was v1.x DAM scope):** the entire *persistent* library/DAM concept — a catalog/library **database of record** (Lightroom `.lrcat`), catalog-centric organization, collections/albums, smart collections, hierarchical keywords, ratings/flags/labels workflows, culling views, faces/people, semantic search, duplicate detection, managed copy/rename/backup ingest, watched/auto-import folders. (v2.2 note: the *transient live-FS folder explorer* above is **not** cut — the cut is on persistence/management, not on filesystem navigation.)

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
