// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E02 seam-handoff contract (H6) + threat-model notes (H7).
//!
//! This module carries **no code** — it is the consolidated, rustdoc-level
//! integration contract E02 hands to its neighbours, committed alongside the
//! crates so `cargo doc -p lightbox-cli` renders it next to the subcommands
//! that exercise every seam ([`crate::e02`]). Each consumer entry names the
//! contract type, its invariants, and its failure modes; the threat-model
//! section enumerates the open risks for the §12 security-engineer review.
//!
//! ---
//!
//! # H6 — seam handoff contracts
//!
//! ## E05 / E10 — the input & display transforms (node params)
//!
//! [`lightbox_color::ResolvedInputTransform`] and
//! `lightbox_color::display::DisplayTransform` are **plain data**: a baked 3×3
//! `cam_to_working` matrix + sampled tables (HueSatMap/LookTable as `f32`
//! grids, tone curves sampled `N=4096`) and, for display, a 1-D shaper + 65³
//! LUT. A WGSL node uploads these verbatim; nothing in either type references a
//! GPU device, an `lcms2` context, or a file handle.
//!
//! - **Invariants.** (1) `ResolvedInputTransform::eval_cpu` is the *reference
//!   semantics* — the GPU kernel must match it within the PV1 golden tolerance
//!   (ΔE2000 ≤ 1.0). (2) Stage order is frozen per §5.2: `cam_to_working` →
//!   `baseline_exposure` → HueSatMap → LookTable → ProfileToneCurve → look; a
//!   kernel must apply them in that order. (3) `key: u64` is a pure content
//!   hash of the resolved transform — identical inputs ⇒ identical key, and any
//!   WB / look / amount change flips it, so E05's node cache keys on it
//!   directly. (4) `resolve_input_transform` is < 1 ms on CPU (§7.5), so a WB
//!   slider re-resolves and re-uploads without a decode or demosaic (§5.4).
//! - **WGSL porting notes.** The matrix is row-major `[[f32;3];3]`. HueSat
//!   tables are `dims = [h, s, v]` grids of `[Δhue_deg, Δsat_mul, Δval_mul]`;
//!   sample **trilinearly with hue wrapped at 360°** (the `val`-dims == 1 fast
//!   path collapses to a 2-D hue/sat table). Curves are uniform `[0,1]`
//!   domain, clamp out of range. The companion (Melissa-style) encode is
//!   **encode-only** (histograms/readouts) — never a processing space.
//! - **Failure modes.** A singular/degenerate camera matrix, a non-convergent
//!   white point, or an out-of-range `look_amount` surface as
//!   `lightbox_color::ColorError` from `resolve_input_transform` — resolution
//!   fails loudly; a node must not paper over it with an identity transform.
//!
//! ## E03 — the raw-cache payload
//!
//! `lightbox_decode::DecodedRawState` (magic `LBRS`, version 1) is the byte
//! format E03's raw cache stores. Its `decode_params_hash` =
//! `xxh3(backend policy, interim_demosaic, proxy libraw version, linearize impl
//! version)` is the cache-key params component (§4.2).
//!
//! - **Invariants.** The `interim_demosaic` bit **segregates** M1 LibRaw-AHD
//!   states from post-E11 clean-room mosaic states, so the M2 demosaic swap is
//!   a guaranteed cache miss, never a stale hit. The hash is stable across
//!   platforms (cross-CI assertion, task C6). Round-trip is identity; the
//!   version byte is honoured (unknown version ⇒ structured `StateError`).
//! - **Failure modes.** A truncated / wrong-magic / unknown-version blob is a
//!   structured error the cache treats as a miss — never a panic.
//!
//! ## E04 — probe & the decode-error taxonomy
//!
//! [`lightbox_decode::probe`] → `AssetProbe` is metadata-only (a few KiB read,
//! no pixel decode) and drives ingest: `format`, dims, normalized camera
//! make/model, capture time, embedded-preview descriptors, orientation.
//! `lightbox_decode::DecodeError::catalog_code()` maps every decode failure to
//! an `asset.decode_error` taxonomy code; the accepted backend is recorded in
//! `asset.decode_backend` (migration 0002; values are
//! `DecodeBackend::catalog_str()`).
//!
//! - **Invariants.** Probe never panics on malformed input — junk with a
//!   supported extension is `ProbeError::Malformed`, unrecognized bytes are
//!   `ProbedFormat::Unsupported` (still catalogued, badged). A proxy crash /
//!   timeout on a mosaic file is a structured `DecodeError` and the asset stays
//!   catalogued (§5.1); there is **no in-process mosaic fallback** (single
//!   backend by license necessity, R1).
//!
//! ## E15 — the output transform
//!
//! `lightbox_color::output::OutputTransform` is the **exact LCMS2** path (never
//! the baked display LUT): working → {sRGB, AdobeRGB, ProPhoto, Display P3,
//! Rec.2020, user ICC} at 8/16-bit int + `f32`, plus embeddable
//! `profile_bytes()` for the exported file.
//!
//! - **Invariants.** Export fidelity uses this exact path, not the GPU display
//!   bake (which is a viewing approximation gated to ΔE2000 ≤ 0.5 p99).
//!   Round-trip working→space→working stays within the D6 bound; emitted ICC
//!   bytes validate in external tooling.
//!
//! ## E09 — profile references & the fallback chain
//!
//! `lightbox_color::profile::resolve_profile_ref` resolves a recipe's
//! `base_profile` `ProfileRef` with the Risk-10 fallback chain: requested
//! profile → (missing) → **matrix base + default look**, carrying a
//! user-visible `FallbackReason`. The default recipe wiring is
//! `look_ref = lightbox-color-v1`, `look_amount = 1.0`. `edit_recipe.doc.
//! base_profile` references `camera_profile.id` (the registry migration 0002
//! adds).
//!
//! - **Invariants.** A recipe naming an uninstalled profile still renders (via
//!   the matrix base) and the fallback is surfaced, never silent (Risk 10). The
//!   default look **does not** apply to non-raw sources (§5.3).
//!
//! ---
//!
//! # H7 — threat-model notes (for the §12 security review)
//!
//! E02 parses three classes of **untrusted input** (Risk 7): camera raw
//! containers, user-installed `.dcp` profiles, and user/monitor ICC profiles.
//! The defence-in-depth posture:
//!
//! - **Raw container metadata (in-crate).** Parsed by memory-safe Rust
//!   permissive TIFF-IFD / ISO-BMFF / EXIF walkers (no `rawler`, LGPL-banned).
//!   Bounded reads, IFD-loop / offset-overflow guards, `catch_unwind` at every
//!   `decode`/`probe` entry point (A9) so no unwind crosses the API boundary —
//!   malformed input is always a structured `DecodeError`/`ProbeError`. Fuzz:
//!   the raw-container mutation corpus (§7.4).
//! - **Mosaic pixel decode (LibRaw).** The memory-unsafe C decoder is
//!   **sandboxed out-of-process** in `lightbox-rawproxy`, dynamic-linked there
//!   only and never in an app binary (symbol audit, C7). The supervisor
//!   enforces: a kill-on-timeout budget, a warm single-child pool restarted on
//!   crash, payload-size caps **both** child-side (rlimit / JobObject) and
//!   parent-side, and a no-network assertion. A child SIGKILL / hang / memory
//!   bomb returns `ProxyCrashed` / `ProxyTimeout` / `ResourceCap` and the parent
//!   RSS is unaffected (C3/C4 injection tests). Blast radius of any proxy defect
//!   = one per-file decode failure; the asset stays catalogued.
//! - **`.dcp` profiles.** Own TIFF-IFD parser, fuzzed ≥ 1 M iterations with
//!   dims × table-size caps; adversarial fixtures (truncated, dims overflow, NaN
//!   floats, giant tables) all yield structured `DcpParseError`, zero panic /
//!   OOM (F3).
//! - **ICC profiles.** `IccProfile::from_bytes` is size-capped and fuzzed;
//!   LCMS2 runs with a process-global error-log handler installed (D1) so a
//!   malformed profile surfaces as `IccError`, never a C-level `abort` or
//!   stderr spam. The GPL-3 `fast_float` plugin is **never linked** (asserted
//!   absent in CI). Display profiles that are missing / invalid fall back to
//!   sRGB with a surfaced event (D5).
//!
//! ## Open risks for the security-engineer phase
//!
//! 1. **OS-level sandbox tightening** (seatbelt / AppContainer profiles) beyond
//!    process isolation + rlimits is explicitly out of E02 scope (§1.2) and left
//!    to the §12 design review — E02 ships process isolation, resource caps, and
//!    these notes.
//! 2. **Single mosaic backend (R1).** Losing dual-backend redundancy is an
//!    accepted license consequence; the mitigation is telemetry
//!    (`decode_backend` / `decode_error`) to quantify coverage gaps, not a
//!    second in-process decoder.
//! 3. **Linux/Wayland per-monitor ICC (R8)** is best-effort behind a feature
//!    flag with sRGB fallback — not a security boundary but a correctness gap to
//!    flag.
//! 4. **libheif/libde265 HEIC path (A8)** is off-by-default; when enabled it is
//!    another untrusted-C surface that must ride the same proxy/sandbox posture
//!    before it ships on by default (x265 encoder is banned).
