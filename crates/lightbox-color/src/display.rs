// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Display transforms (spec §3.5). **Owner: Phase D (D3/D5).** Working →
//! per-monitor ICC, baked to a shaper + 3D LUT for GPU application, with the
//! exact LCMS2 path retained for the fidelity gate (D4).
//!
//! # Bake model (D3)
//!
//! The GPU applies, per pixel: `out = trilinear(lut, shaper(rgb))`, where the
//! 1-D [`shaper`](DisplayTransform::shaper) is evaluated independently on each
//! channel and the result indexes the 65³ [`lut`](DisplayTransform::lut).
//! [`DisplayTransform::apply`] is the CPU reference that E05's WGSL node must
//! match bit-for-bit-close.
//!
//! The shaper is **derived from the display profile's own neutral-axis
//! response** (the profile's gray transfer): it is the reparameterisation that
//! makes the working→display transform near-linear per channel, so the 65³ LUT
//! spends its resolution where the transform actually bends (shadows). Two
//! consequences fall out for free:
//!
//! * For an **identity** display (the working-space profile itself) the neutral
//!   response is linear, so the shaper is the identity ramp and the baked LUT
//!   is the identity cube — the D3 `identity ⇒ identity LUT` acceptance.
//! * For a real gamma display the shaper absorbs the encoding curve and the LUT
//!   carries only the (near-linear) matrix + gamut map, keeping trilinear error
//!   under the D4 ΔE2000 gate.
//!
//! # Per-OS profile source (D5)
//!
//! [`SystemDisplayProfileProvider`] queries the OS: macOS ColorSync is wired
//! here (real profile bytes via CoreGraphics); Windows ICM and Linux
//! colord/`_ICC_PROFILE` are best-effort seams the shell completes against a
//! live display server. A missing/invalid profile yields `None`, and the
//! caller falls back to [`SrgbFallbackProvider`] and surfaces an event (E08).

#![allow(unsafe_code)]

use lcms2::{Flags, PixelFormat, Transform};

use crate::cms::IccProfile;
use crate::error::IccError;
use crate::transform::Curve1D;

/// Nodes per axis of the baked display LUT (spec §3.5).
///
/// **D4 failure path — LUT-size escalation.** If a pathological display profile
/// ever fails the ΔE2000 fidelity gate (p99 ≤ 0.5 / max ≤ 1.0) at 65³, the
/// remedy is to raise this to the next odd size (e.g. 129) — the neutral-axis
/// shaper keeps the node budget efficient, so escalation is a size bump with no
/// algorithm change. Both bundled fixtures (sRGB, wide-gamut ~2.2 gamma) pass
/// with wide margin at 65³ (see `tests::fidelity_*`).
pub const LUT_SIZE: usize = 65;
/// Sample count of the derived 1-D shaper curve.
const SHAPER_N: usize = 1024;

/// A monitor identity the shell supplies (spec §3.5). On macOS this is the
/// `CGDirectDisplayID`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MonitorId(pub u64);

/// Rendering intent (spec §3.5; default rel-colorimetric + BPC).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    /// Perceptual.
    Perceptual,
    /// Relative colorimetric (with black-point compensation).
    RelColorimetric,
    /// Saturation.
    Saturation,
    /// Absolute colorimetric.
    AbsColorimetric,
}

impl Intent {
    /// Maps to the Little-CMS2 intent and whether black-point compensation is
    /// applied (BPC pairs with the colorimetric/perceptual intents; it is a
    /// no-op — and semantically wrong — for absolute colorimetric). Shared with
    /// [`crate::output`].
    pub(crate) fn to_lcms(self) -> (lcms2::Intent, bool) {
        match self {
            Intent::Perceptual => (lcms2::Intent::Perceptual, true),
            Intent::RelColorimetric => (lcms2::Intent::RelativeColorimetric, true),
            Intent::Saturation => (lcms2::Intent::Saturation, false),
            Intent::AbsColorimetric => (lcms2::Intent::AbsoluteColorimetric, false),
        }
    }

    fn tag(self) -> u8 {
        match self {
            Intent::Perceptual => 0,
            Intent::RelColorimetric => 1,
            Intent::Saturation => 2,
            Intent::AbsColorimetric => 3,
        }
    }
}

/// A baked 3D LUT (spec §3.5, 65³ for display transforms).
#[derive(Clone, Debug)]
pub struct Lut3D {
    /// Nodes per axis (e.g. 65).
    pub size: u32,
    /// `size³` RGB entries, row-major (r fastest).
    pub data: Vec<[f32; 3]>,
}

/// Working → display, baked for GPU: 1D shaper + 65³ LUT (spec §3.5). The exact
/// LCMS2 path is kept for tolerance tests (D4).
#[derive(Clone, Debug)]
pub struct DisplayTransform {
    /// 1D shaper curve (evaluated per channel before the LUT lookup).
    pub shaper: Curve1D,
    /// 3D LUT.
    pub lut: Lut3D,
    /// Content hash → cache key.
    pub key: u64,
}

impl DisplayTransform {
    /// CPU reference application: working-space linear RGB → display RGB via
    /// `trilinear(lut, shaper(rgb))`. The parity anchor E05's WGSL node matches.
    #[must_use]
    pub fn apply(&self, rgb_working_linear: [f32; 3]) -> [f32; 3] {
        let c = [
            eval_curve(&self.shaper.samples, rgb_working_linear[0]),
            eval_curve(&self.shaper.samples, rgb_working_linear[1]),
            eval_curve(&self.shaper.samples, rgb_working_linear[2]),
        ];
        sample_lut(&self.lut, c)
    }
}

/// Per-OS monitor-profile source (spec §3.5). The shell implements this
/// (macOS ColorSync / Windows ICM / Linux best-effort). **Owner: Phase D (D5).**
pub trait DisplayProfileProvider: Send + Sync {
    /// The ICC profile for a monitor, if one is available.
    fn profile_for_monitor(&self, monitor: MonitorId) -> Option<IccProfile>;
}

/// The documented fallback (D5): always yields the built-in sRGB profile. A
/// caller wraps the system provider with this when the OS reports no profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct SrgbFallbackProvider;

impl DisplayProfileProvider for SrgbFallbackProvider {
    fn profile_for_monitor(&self, _monitor: MonitorId) -> Option<IccProfile> {
        Some(IccProfile::srgb())
    }
}

/// Queries the operating system for a monitor's ICC profile (spec §3.5, D5).
/// Returns `None` when the OS has no usable profile — the caller then uses
/// [`SrgbFallbackProvider`] and surfaces an event (E08 seam).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDisplayProfileProvider;

impl DisplayProfileProvider for SystemDisplayProfileProvider {
    fn profile_for_monitor(&self, monitor: MonitorId) -> Option<IccProfile> {
        system_profile(monitor)
    }
}

#[cfg(target_os = "macos")]
fn system_profile(monitor: MonitorId) -> Option<IccProfile> {
    colorsync_macos::profile_for_display(monitor.0 as u32)
}

#[cfg(not(target_os = "macos"))]
fn system_profile(_monitor: MonitorId) -> Option<IccProfile> {
    // Windows ICM (`WcsGetDefaultColorProfile`/`GetICMProfile`) and Linux
    // colord/`_ICC_PROFILE` are wired by the shell against the live display
    // server; on other build targets we report "no profile" so the caller uses
    // the sRGB fallback + event. See E02-deviations.md (D5).
    None
}

/// Real macOS ColorSync profile fetch via CoreGraphics.
#[cfg(target_os = "macos")]
mod colorsync_macos {
    use super::IccProfile;
    use std::os::raw::c_void;

    type CgDirectDisplayId = u32;
    type CgColorSpaceRef = *mut c_void;
    type CfDataRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGDisplayCopyColorSpace(display: CgDirectDisplayId) -> CgColorSpaceRef;
        fn CGColorSpaceCopyICCData(space: CgColorSpaceRef) -> CfDataRef;
        fn CGColorSpaceRelease(space: CgColorSpaceRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataGetLength(data: CfDataRef) -> isize;
        fn CFDataGetBytePtr(data: CfDataRef) -> *const u8;
        fn CFRelease(cf: *const c_void);
    }

    /// Fetches the ICC bytes for a `CGDirectDisplayID` and parses them through
    /// the untrusted-input path. Returns `None` if the display has no color
    /// space or the bytes fail to parse.
    pub fn profile_for_display(id: CgDirectDisplayId) -> Option<IccProfile> {
        // SAFETY: all handles are checked for null; ownership follows the Core
        // Foundation Copy rule — `CGDisplayCopyColorSpace` and
        // `CGColorSpaceCopyICCData` return +1 references we release exactly once.
        unsafe {
            let space = CGDisplayCopyColorSpace(id);
            if space.is_null() {
                return None;
            }
            let data = CGColorSpaceCopyICCData(space);
            CGColorSpaceRelease(space);
            if data.is_null() {
                return None;
            }
            let len = CFDataGetLength(data);
            let ptr = CFDataGetBytePtr(data);
            let profile = if len > 0 && !ptr.is_null() {
                let bytes = std::slice::from_raw_parts(ptr, len as usize);
                IccProfile::from_bytes(bytes).ok()
            } else {
                None
            };
            CFRelease(data);
            profile
        }
    }
}

/// Bakes a working→display transform (spec §3.5). **Phase D (D3)** — identity
/// profile ⇒ identity LUT within 1e-4; bake ≤ 100 ms then cached by key.
pub fn build_display_transform(
    display: &IccProfile,
    intent: Intent,
) -> Result<DisplayTransform, IccError> {
    let working = crate::cms::working_linear();
    let (lcms_intent, bpc) = intent.to_lcms();
    let flags = if bpc {
        Flags::NO_OPTIMIZE | Flags::BLACKPOINT_COMPENSATION
    } else {
        Flags::NO_OPTIMIZE
    };
    // Full-precision float transform, sampled to build the shaper + LUT.
    let xform: Transform<[f32; 3], [f32; 3]> = Transform::new_flags(
        working.as_lcms(),
        PixelFormat::RGB_FLT,
        display.as_lcms(),
        PixelFormat::RGB_FLT,
        lcms_intent,
        flags,
    )
    .map_err(|e| IccError::TransformBuild(e.to_string()))?;

    let shaper = derive_shaper(&xform);
    let lut = bake_lut(&xform, &shaper.samples);
    let key = display_key(display, intent);

    Ok(DisplayTransform { shaper, lut, key })
}

/// Derives the shaper from the display's neutral-axis (gray) response,
/// normalised to `[0, 1]` and forced strictly increasing so it is invertible.
fn derive_shaper(xform: &Transform<[f32; 3], [f32; 3]>) -> Curve1D {
    let mut ramp = Vec::with_capacity(SHAPER_N);
    for i in 0..SHAPER_N {
        let t = i as f32 / (SHAPER_N as f32 - 1.0);
        ramp.push([t, t, t]);
    }
    let mut out = vec![[0.0f32; 3]; SHAPER_N];
    xform.transform_pixels(&ramp, &mut out);

    // Representative gray response = luminance-dominant green channel; strictly
    // monotone-repair against numerical wiggle so the curve inverts cleanly.
    let mut mono: Vec<f32> = out.iter().map(|p| p[1]).collect();
    for i in 1..mono.len() {
        if mono[i] <= mono[i - 1] {
            mono[i] = mono[i - 1] + f32::EPSILON;
        }
    }
    let lo = mono[0];
    let hi = mono[mono.len() - 1];
    let span = (hi - lo).max(1e-6);
    let samples: Vec<f32> = mono
        .iter()
        .map(|v| ((v - lo) / span).clamp(0.0, 1.0))
        .collect();
    Curve1D { samples }
}

/// Bakes the 65³ LUT in shaper (encoded) coordinates: each node is the exact
/// transform of the linear input that the shaper maps to that node.
///
/// The shaper inverse depends only on the axis coordinate, of which there are
/// just `LUT_SIZE` distinct values, so it is precomputed once per axis rather
/// than per node (65 inversions instead of 3·65³).
fn bake_lut(xform: &Transform<[f32; 3], [f32; 3]>, shaper: &[f32]) -> Lut3D {
    let n = LUT_SIZE;
    let last = n as f32 - 1.0;
    let lin: Vec<f32> = (0..n)
        .map(|i| shaper_inv(shaper, i as f32 / last))
        .collect();
    let mut inputs = Vec::with_capacity(n * n * n);
    // Order: r fastest → index = r + n*(g + n*b).
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                inputs.push([lin[r], lin[g], lin[b]]);
            }
        }
    }
    let mut data = vec![[0.0f32; 3]; n * n * n];
    xform.transform_pixels(&inputs, &mut data);
    Lut3D {
        size: n as u32,
        data,
    }
}

/// Cache key: xxh3-64 of the display profile bytes ⊕ intent ⊕ LUT size.
fn display_key(display: &IccProfile, intent: Intent) -> u64 {
    let mut buf = display.to_icc_bytes().unwrap_or_default();
    buf.push(intent.tag());
    buf.push(LUT_SIZE as u8);
    twox_hash::XxHash3_64::oneshot(&buf)
}

/// Evaluates a `[0,1]`-domain sampled curve at `t` (linear interpolation).
fn eval_curve(samples: &[f32], t: f32) -> f32 {
    match samples.len() {
        0 => t,
        1 => samples[0],
        n => {
            let p = t.clamp(0.0, 1.0) * (n as f32 - 1.0);
            let i0 = (p.floor() as usize).min(n - 2);
            let frac = p - i0 as f32;
            samples[i0] * (1.0 - frac) + samples[i0 + 1] * frac
        }
    }
}

/// Inverts the (strictly increasing, `[0,1]`-normalised) shaper: given an
/// encoded coordinate `u`, returns the linear input `t` with `shaper(t) = u`.
fn shaper_inv(samples: &[f32], u: f32) -> f32 {
    let n = samples.len();
    if n < 2 {
        return u.clamp(0.0, 1.0);
    }
    let u = u.clamp(0.0, 1.0);
    // First index whose sample exceeds `u`; the bracketing interval is [j-1, j].
    let j = samples.partition_point(|&s| s <= u);
    let hi = j.min(n - 1);
    let lo = hi.saturating_sub(1);
    let denom = (samples[hi] - samples[lo]).max(1e-12);
    let frac = ((u - samples[lo]) / denom).clamp(0.0, 1.0);
    (lo as f32 + frac) / (n as f32 - 1.0)
}

/// Trilinear sample of a cube LUT at `c ∈ [0,1]³`.
fn sample_lut(lut: &Lut3D, c: [f32; 3]) -> [f32; 3] {
    let n = lut.size as usize;
    if n < 2 {
        return c;
    }
    let last = n as f32 - 1.0;
    let axis = |x: f32| -> (usize, f32) {
        let v = x.clamp(0.0, 1.0) * last;
        let i0 = (v.floor() as usize).min(n - 2);
        (i0, v - i0 as f32)
    };
    let (r0, rf) = axis(c[0]);
    let (g0, gf) = axis(c[1]);
    let (b0, bf) = axis(c[2]);
    let at = |r: usize, g: usize, b: usize| lut.data[r + n * (g + n * b)];

    let c000 = at(r0, g0, b0);
    let c100 = at(r0 + 1, g0, b0);
    let c010 = at(r0, g0 + 1, b0);
    let c110 = at(r0 + 1, g0 + 1, b0);
    let c001 = at(r0, g0, b0 + 1);
    let c101 = at(r0 + 1, g0, b0 + 1);
    let c011 = at(r0, g0 + 1, b0 + 1);
    let c111 = at(r0 + 1, g0 + 1, b0 + 1);

    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        let c00 = c000[ch] * (1.0 - rf) + c100[ch] * rf;
        let c10 = c010[ch] * (1.0 - rf) + c110[ch] * rf;
        let c01 = c001[ch] * (1.0 - rf) + c101[ch] * rf;
        let c11 = c011[ch] * (1.0 - rf) + c111[ch] * rf;
        let c0 = c00 * (1.0 - gf) + c10 * gf;
        let c1 = c01 * (1.0 - gf) + c11 * gf;
        out[ch] = c0 * (1.0 - bf) + c1 * bf;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers shared by the fidelity tests --------------------------------

    /// Builds the exact working→display float transform with the same
    /// intent/flags `build_display_transform` uses internally.
    fn exact_transform(display: &IccProfile) -> Transform<[f32; 3], [f32; 3]> {
        let working = crate::cms::working_linear();
        Transform::new_flags(
            working.as_lcms(),
            PixelFormat::RGB_FLT,
            display.as_lcms(),
            PixelFormat::RGB_FLT,
            lcms2::Intent::RelativeColorimetric,
            Flags::NO_OPTIMIZE | Flags::BLACKPOINT_COMPENSATION,
        )
        .expect("exact reference transform")
    }

    /// Transform that takes display RGB → CIE L*a*b* (D50) for ΔE comparison.
    fn to_lab(display: &IccProfile) -> Transform<[f32; 3], [f32; 3]> {
        let lab = lcms2::Profile::new_lab4_context(
            lcms2::GlobalContext::new(),
            &lcms2::CIExyY {
                x: 0.345_67,
                y: 0.358_50,
                Y: 1.0,
            },
        )
        .expect("Lab profile");
        Transform::new(
            display.as_lcms(),
            PixelFormat::RGB_FLT,
            &lab,
            PixelFormat::Lab_FLT,
            lcms2::Intent::RelativeColorimetric,
        )
        .expect("display→Lab transform")
    }

    /// Runs a single RGB triple through a 3-channel float transform.
    fn apply_xform(x: &Transform<[f32; 3], [f32; 3]>, rgb: [f32; 3]) -> [f32; 3] {
        let mut out = [[0.0f32; 3]];
        x.transform_pixels(&[rgb], &mut out);
        out[0]
    }

    /// Tiny deterministic PRNG (SplitMix64) so the fidelity test is reproducible
    /// without a `rand` dependency.
    struct Rng(u64);
    impl Rng {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            // top 24 bits → [0,1)
            ((z >> 40) as f32) / (1u32 << 24) as f32
        }
    }

    fn deg_cos(d: f64) -> f64 {
        d.to_radians().cos()
    }
    fn deg_sin(d: f64) -> f64 {
        d.to_radians().sin()
    }

    /// CIEDE2000 colour difference (L in 0..100, a/b in usual units).
    fn ciede2000(lab1: [f32; 3], lab2: [f32; 3]) -> f64 {
        let (l1, a1, b1) = (f64::from(lab1[0]), f64::from(lab1[1]), f64::from(lab1[2]));
        let (l2, a2, b2) = (f64::from(lab2[0]), f64::from(lab2[1]), f64::from(lab2[2]));
        let pow25_7 = 25f64.powi(7);
        let c1 = (a1 * a1 + b1 * b1).sqrt();
        let c2 = (a2 * a2 + b2 * b2).sqrt();
        let cbar = (c1 + c2) / 2.0;
        let cbar7 = cbar.powi(7);
        let g = 0.5 * (1.0 - (cbar7 / (cbar7 + pow25_7)).sqrt());
        let a1p = (1.0 + g) * a1;
        let a2p = (1.0 + g) * a2;
        let c1p = (a1p * a1p + b1 * b1).sqrt();
        let c2p = (a2p * a2p + b2 * b2).sqrt();
        let hp = |b: f64, ap: f64| -> f64 {
            if b == 0.0 && ap == 0.0 {
                0.0
            } else {
                let mut h = b.atan2(ap).to_degrees();
                if h < 0.0 {
                    h += 360.0;
                }
                h
            }
        };
        let h1p = hp(b1, a1p);
        let h2p = hp(b2, a2p);
        let dlp = l2 - l1;
        let dcp = c2p - c1p;
        let dhp = if c1p * c2p == 0.0 {
            0.0
        } else {
            let mut d = h2p - h1p;
            if d > 180.0 {
                d -= 360.0;
            } else if d < -180.0 {
                d += 360.0;
            }
            d
        };
        let big_dhp = 2.0 * (c1p * c2p).sqrt() * (dhp / 2.0).to_radians().sin();
        let lbarp = (l1 + l2) / 2.0;
        let cbarp = (c1p + c2p) / 2.0;
        let hbarp = if c1p * c2p == 0.0 {
            h1p + h2p
        } else if (h1p - h2p).abs() <= 180.0 {
            (h1p + h2p) / 2.0
        } else if h1p + h2p < 360.0 {
            (h1p + h2p + 360.0) / 2.0
        } else {
            (h1p + h2p - 360.0) / 2.0
        };
        let t = 1.0 - 0.17 * deg_cos(hbarp - 30.0)
            + 0.24 * deg_cos(2.0 * hbarp)
            + 0.32 * deg_cos(3.0 * hbarp + 6.0)
            - 0.20 * deg_cos(4.0 * hbarp - 63.0);
        let dtheta = 30.0 * (-(((hbarp - 275.0) / 25.0).powi(2))).exp();
        let rc = 2.0 * (cbarp.powi(7) / (cbarp.powi(7) + pow25_7)).sqrt();
        let sl = 1.0 + (0.015 * (lbarp - 50.0).powi(2)) / (20.0 + (lbarp - 50.0).powi(2)).sqrt();
        let sc = 1.0 + 0.045 * cbarp;
        let sh = 1.0 + 0.015 * cbarp * t;
        let rt = -deg_sin(2.0 * dtheta) * rc;
        ((dlp / sl).powi(2)
            + (dcp / sc).powi(2)
            + (big_dhp / sh).powi(2)
            + rt * (dcp / sc) * (big_dhp / sh))
            .sqrt()
    }

    // ---- D3: identity profile ⇒ identity LUT --------------------------------

    #[test]
    fn identity_profile_yields_identity_lut() {
        let working = crate::cms::working_linear();
        let dt = build_display_transform(&working, Intent::RelColorimetric).expect("bake identity");
        // The shaper is the identity ramp.
        for (i, &s) in dt.shaper.samples.iter().enumerate() {
            let t = i as f32 / (dt.shaper.samples.len() as f32 - 1.0);
            assert!((s - t).abs() < 1e-4, "shaper[{i}]={s} != {t}");
        }
        // Every LUT node equals its coordinate.
        let n = dt.lut.size as usize;
        let last = n as f32 - 1.0;
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let node = dt.lut.data[r + n * (g + n * b)];
                    let want = [r as f32 / last, g as f32 / last, b as f32 / last];
                    for ch in 0..3 {
                        assert!(
                            (node[ch] - want[ch]).abs() < 1e-4,
                            "node ({r},{g},{b})[{ch}]={} != {}",
                            node[ch],
                            want[ch]
                        );
                    }
                }
            }
        }
        // And apply() is the identity within 1e-4.
        let mut rng = Rng(1);
        for _ in 0..1000 {
            let x = [rng.next_f32(), rng.next_f32(), rng.next_f32()];
            let y = dt.apply(x);
            for ch in 0..3 {
                assert!(
                    (y[ch] - x[ch]).abs() < 1e-4,
                    "apply {x:?}[{ch}] -> {}",
                    y[ch]
                );
            }
        }
    }

    // ---- D4: baked-LUT fidelity gate ----------------------------------------

    fn fidelity_gate(display: &IccProfile, label: &str) {
        let dt = build_display_transform(display, Intent::RelColorimetric).expect("bake");
        let exact = exact_transform(display);
        let lab = to_lab(display);

        const SAMPLES: usize = 100_000;
        let mut des: Vec<f64> = Vec::with_capacity(SAMPLES);
        let mut rng = Rng(0xDEAD_BEEF);
        let mut max = 0.0f64;
        for _ in 0..SAMPLES {
            let x = [rng.next_f32(), rng.next_f32(), rng.next_f32()];
            let ref_rgb = apply_xform(&exact, x); // exact display RGB
            let lut_rgb = dt.apply(x); // baked display RGB
            let de = ciede2000(apply_xform(&lab, ref_rgb), apply_xform(&lab, lut_rgb));
            des.push(de);
            if de > max {
                max = de;
            }
        }
        des.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p99 = des[(SAMPLES as f64 * 0.99) as usize];
        eprintln!("[D4 {label}] p99 ΔE2000 = {p99:.4}, max = {max:.4}");
        assert!(p99 <= 0.5, "{label}: p99 ΔE2000 {p99} > 0.5");
        assert!(max <= 1.0, "{label}: max ΔE2000 {max} > 1.0");
    }

    #[test]
    fn fidelity_srgb() {
        fidelity_gate(&IccProfile::srgb(), "sRGB");
    }

    #[test]
    fn fidelity_wide_gamut_gamma() {
        // AdobeRGB: wide-gamut primaries with a ~2.2 pure-gamma transfer — the
        // stressful shadow case for a linear-working-space LUT.
        fidelity_gate(&crate::cms::adobe_rgb(), "AdobeRGB");
    }

    #[test]
    fn bake_is_under_budget_and_cached_by_key() {
        let srgb = IccProfile::srgb();
        let t0 = std::time::Instant::now();
        let a = build_display_transform(&srgb, Intent::RelColorimetric).expect("bake a");
        let elapsed = t0.elapsed();
        eprintln!("[D3] display bake took {elapsed:?} (budget ≤ 100 ms, release)");
        // The ≤ 100 ms budget (spec §7.5) is enforced only for optimised builds;
        // an unoptimised test build gets a loose non-hang ceiling. The tight
        // budget is also tracked as an H3 nightly-bench row.
        let ceiling_ms = if cfg!(debug_assertions) { 2_000 } else { 100 };
        assert!(
            elapsed.as_millis() < ceiling_ms,
            "display bake {elapsed:?} exceeds the {ceiling_ms} ms ceiling"
        );
        let b = build_display_transform(&srgb, Intent::RelColorimetric).expect("bake b");
        assert_eq!(a.key, b.key, "same profile+intent ⇒ same cache key");
        let p3 = build_display_transform(&IccProfile::display_p3(), Intent::RelColorimetric)
            .expect("bake p3");
        assert_ne!(a.key, p3.key, "different profile ⇒ different key");
        let a_perc = build_display_transform(&srgb, Intent::Perceptual).expect("bake perceptual");
        assert_ne!(a.key, a_perc.key, "different intent ⇒ different key");
    }

    #[test]
    fn providers_are_object_safe_and_fallback_yields_srgb() {
        let providers: Vec<Box<dyn DisplayProfileProvider>> = vec![
            Box::new(SrgbFallbackProvider),
            Box::new(SystemDisplayProfileProvider),
        ];
        assert!(providers[0].profile_for_monitor(MonitorId(0)).is_some());
        // System provider on a bogus monitor id must not panic (Some or None).
        let _ = providers[1].profile_for_monitor(MonitorId(u64::MAX));
    }
}
