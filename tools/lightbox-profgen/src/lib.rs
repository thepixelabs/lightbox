// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-profgen` — the internal curated-camera-profile content line
//! (E02.5 / Phase G). **NOT shipped in the app** (spec §1.1/§2): it orchestrates
//! `dcamprof` as a GPL-3 **subprocess** (never linked, never bundled) over our
//! own ColorChecker/IT8 target-shot sessions → a validated `.dcp` under the
//! project license, then runs the validation harness (parse with the F-phase
//! engine in [`lightbox_color`], patch-render ΔE gates) and emits surface-3
//! manifest entries + the catalog sync descriptor the app reads on open.
//!
//! # Phase-G scope (tooling only — real content DEFERRED)
//!
//! The build machine has no `dcamprof` and no physical target shots, so the
//! **content** is deferred and Lightbox ships **ZERO** bundled camera profiles
//! (`assets/color/profiles/` is empty — the tier-1 matrix base + tier-2 look
//! render every corpus body correctly without them, spec §0). What ships here is
//! the *tooling*, each stage buildable and unit-tested against synthetic
//! fixtures, so the content line runs end-to-end the moment a capture session +
//! `dcamprof` are available:
//!
//! | module | task | what it owns |
//! |--------|------|--------------|
//! | [`session`]  | G1 | target-shot session dir + metadata schema → [`CameraId`](lightbox_decode::CameraId) |
//! | [`dcamprof`] | G1 | dcamprof subprocess wiring (argv builders + gated exec) |
//! | [`validate`] | G3 | parse via the F engine + patch-ΔE reject gates |
//! | [`package`]  | G4 | packaging + provenance + manifest / catalog sync descriptor |
//! | [`select`]   | G5 | `CameraId` → curated-DCP-if-present-else-matrix-base policy |
//!
//! The capture-protocol (G2) and content-line runbook + §12 escalation (G7) are
//! documents under `tools/lightbox-profgen/docs/`. **G6** (physical
//! ColorChecker/IT8 capture + the first curated batch) is **DEFERRED**: no
//! profiles or reference renders are fabricated (spec §0 / R4 / §12).
//!
//! # GPL isolation (spec §1.1/§7.6)
//!
//! `dcamprof` is *executed* as a subprocess — it is never a crate dependency,
//! never linked, and this tool is never part of the shipped app packaging. The
//! only thing that crosses the boundary is a `.dcp` file we then re-parse with
//! our own permissive-licensed engine before it is allowed anywhere near
//! `assets/`.

pub mod dcamprof;
pub mod package;
pub mod select;
pub mod session;
pub mod validate;

/// Lowercase-hex encoding of a byte slice (manifest / catalog identifiers).
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble"));
        s.push(char::from_digit((b & 0xf) as u32, 16).expect("nibble"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn hex_encodes_low_and_high_nibbles() {
        assert_eq!(hex(&[0x00, 0x0f, 0xf0, 0xab]), "000ff0ab");
        assert_eq!(hex(&[]), "");
    }
}
