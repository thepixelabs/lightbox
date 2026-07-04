// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-decode` — file probing, hashing, and (later) raw decode.
//!
//! Owned by **E01 for the probe surface only** (spec §3.7); implemented in
//! E01 Phase 5 (T19–T20): `probe()` (rawler metadata for raws, header/EXIF
//! parse for JPEG/TIFF/PNG), `read_embedded()`, streaming xxh3-128
//! `hash_file()`. `decode_raw` / `decode_image` / camera-matrix color are
//! declared by E01 but **implemented by E02** — E01 takes zero dependency on
//! raw decode (architecture §9 M0).
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1); no logic yet.
