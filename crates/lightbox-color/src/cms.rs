// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! LCMS2 wrapper (spec §3.5). **Owner: Phase D (D1/D7).** MIT core only, the
//! `fast_float` plugin (GPL-3) is FORBIDDEN and CI-asserted absent (R2, D1).
//! Contexts are thread-safe, the error callback is captured, and
//! [`IccProfile::from_bytes`] treats its input as untrusted (size-capped,
//! fuzzed at D7).
//!
//! # Design (D1)
//!
//! * **Static, MIT-only Little-CMS2.** The crate is built with the `lcms2`
//!   `static` feature (see the manifest), so the vendored Little-CMS2 sources
//!   are compiled in and no system `liblcms2` is ever dynamic-linked. The
//!   GPL-3 `fast_float` plugin has no build path in `lcms2-sys` at all (there
//!   is no such feature and no such vendored source), and no plugin is ever
//!   registered by this crate, [`tests::fast_float_plugin_is_absent`] pins
//!   that invariant against the workspace lock file.
//! * **Error capture, never abort.** A single process-global LCMS2 error
//!   handler is installed once; it records the last message into a thread-local
//!   so a failed parse/build can surface a descriptive [`IccError`] instead of
//!   writing to stderr. Little-CMS2 returns a null handle (→ `Err`) on
//!   malformed input; it never aborts, so [`IccProfile::from_bytes`] is
//!   panic-free on untrusted bytes (D7).
//! * **Thread-safety.** Profiles are opened on the LCMS2 global context, whose
//!   transform/parse operations are internally synchronised in Little-CMS2
//!   ≥ 2.6; the error handler and (absent) plugin chain are configured once,
//!   before any concurrent use, via [`std::sync::Once`]. Per-thread error
//!   capture keeps diagnostics correct under the parallel test/export runners.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::os::raw::c_char;
use std::sync::Once;

use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};

pub use crate::error::IccError;

/// Hard cap on accepted ICC byte length (D7 untrusted-input hygiene). Real
/// display/output profiles are a few KiB-MiB; anything past this is rejected
/// before Little-CMS2 sees it so a crafted length field cannot drive a large
/// allocation. 32 MiB is comfortably above any legitimate profile.
pub const MAX_ICC_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    /// Last message the LCMS2 error handler saw on *this* thread. Read-and-clear
    /// around a fallible LCMS2 call to enrich the returned [`IccError`].
    static LAST_LCMS_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The process-global LCMS2 error handler. Little-CMS2 invokes this
/// synchronously, on the calling thread, during the operation that failed, so
/// stashing into a thread-local is both correct and race-free.
///
/// # Safety
///
/// Matches the `cmsLogErrorHandlerFunction` C ABI. `text` is a NUL-terminated C
/// string owned by Little-CMS2 for the duration of the call; we only read it.
unsafe extern "C" fn lcms_error_handler(_ctx: lcms2_sys::Context, _code: u32, text: *const c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: `text` is a valid NUL-terminated string for the call's duration.
    let msg = unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    LAST_LCMS_ERROR.with(|slot| *slot.borrow_mut() = Some(msg));
}

/// Installs the capturing error handler exactly once (idempotent, thread-safe).
fn install_error_handler() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: sets the global handler on the default context; the callback
        // is `'static` and ABI-correct. Done once before any concurrent use.
        unsafe { lcms2_sys::cmsSetLogErrorHandler(Some(lcms_error_handler)) };
    });
}

/// Reads and clears this thread's captured LCMS2 message (if any).
fn take_last_error() -> Option<String> {
    LAST_LCMS_ERROR.with(|slot| slot.borrow_mut().take())
}

// ---------------------------------------------------------------------------
// Color-space primitives (primaries + white points + transfer curves)
// ---------------------------------------------------------------------------

#[inline]
fn xy(x: f64, y: f64) -> CIExyY {
    CIExyY { x, y, Y: 1.0 }
}

/// CIE D65 white (2°). D50 comes from Little-CMS2's built-in `CIExyY::d50()`.
#[inline]
fn d65() -> CIExyY {
    xy(0.312_7, 0.329_0)
}

#[inline]
fn d50() -> CIExyY {
    *lcms2_sys::CIExyY::d50()
}

/// ProPhoto / ROMM primaries (D50 native). Shared by the working space and the
/// ProPhoto output space.
fn prophoto_primaries() -> CIExyYTRIPLE {
    CIExyYTRIPLE {
        Red: xy(0.734_7, 0.265_3),
        Green: xy(0.159_6, 0.840_4),
        Blue: xy(0.036_6, 0.000_1),
    }
}

fn adobe_rgb_primaries() -> CIExyYTRIPLE {
    CIExyYTRIPLE {
        Red: xy(0.640, 0.330),
        Green: xy(0.210, 0.710),
        Blue: xy(0.150, 0.060),
    }
}

fn display_p3_primaries() -> CIExyYTRIPLE {
    CIExyYTRIPLE {
        Red: xy(0.680, 0.320),
        Green: xy(0.265, 0.690),
        Blue: xy(0.150, 0.060),
    }
}

fn rec2020_primaries() -> CIExyYTRIPLE {
    CIExyYTRIPLE {
        Red: xy(0.708, 0.292),
        Green: xy(0.170, 0.797),
        Blue: xy(0.131, 0.046),
    }
}

/// Linear (gamma 1.0) transfer, the working-space TRC.
fn linear_trc() -> ToneCurve {
    ToneCurve::new(1.0)
}

/// The sRGB / Display-P3 piecewise transfer as an LCMS2 parametric type-4
/// curve: `Y = ((X + 0.055)/1.055)^2.4` above the toe, linear below.
fn srgb_trc() -> ToneCurve {
    // params = [gamma, a, b, c, d]; e = f = 0 (type-4 default).
    ToneCurve::new_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.040_45])
        .expect("valid sRGB parametric curve")
}

/// Rec.709 / Rec.2020 transfer as an LCMS2 parametric type-4 curve.
fn rec709_trc() -> ToneCurve {
    ToneCurve::new_parametric(
        4,
        &[1.0 / 0.45, 1.0 / 1.099, 0.099 / 1.099, 1.0 / 4.5, 0.081],
    )
    .expect("valid Rec.709 parametric curve")
}

fn build_rgb(white: &CIExyY, primaries: &CIExyYTRIPLE, trc: &ToneCurve) -> Profile {
    Profile::new_rgb(white, primaries, &[trc, trc, trc])
        .expect("built-in RGB profile constructs from valid primaries")
}

// ---------------------------------------------------------------------------
// IccProfile
// ---------------------------------------------------------------------------

/// A parsed ICC profile (size-capped, error-callback captured) (spec §3.5).
///
/// Wraps an owned Little-CMS2 [`lcms2::Profile`] on the global context. `Send`
/// (the handle is moved, never shared by `&`); intentionally not `Sync`, build
/// a [`crate::output::OutputTransform`] or [`crate::display::DisplayTransform`]
/// for the concurrent apply path.
pub struct IccProfile {
    inner: Profile,
}

impl IccProfile {
    /// Parses ICC bytes with untrusted-input hygiene (spec §3.5). **Phase D
    /// (D1/D7).** Empty input and input past [`MAX_ICC_BYTES`] are rejected
    /// before Little-CMS2 is touched; a malformed profile surfaces as
    /// [`IccError::InvalidProfile`] (never a panic or an abort).
    pub fn from_bytes(bytes: &[u8]) -> Result<IccProfile, IccError> {
        install_error_handler();
        if bytes.is_empty() {
            return Err(IccError::InvalidProfile("empty input".to_owned()));
        }
        if bytes.len() > MAX_ICC_BYTES {
            return Err(IccError::TooLarge { size: bytes.len() });
        }
        // Clear any stale capture, attempt the parse, then read whatever the
        // handler recorded on this thread for the diagnostic.
        let _ = take_last_error();
        match Profile::new_icc(bytes) {
            Ok(inner) => Ok(IccProfile { inner }),
            Err(_) => {
                let detail = take_last_error()
                    .unwrap_or_else(|| "Little-CMS2 rejected the profile".to_owned());
                Err(IccError::InvalidProfile(detail))
            }
        }
    }

    /// The built-in sRGB profile.
    #[must_use]
    pub fn srgb() -> IccProfile {
        install_error_handler();
        IccProfile {
            inner: Profile::new_srgb(),
        }
    }

    /// The built-in Display P3 profile (P3 primaries, sRGB transfer, D65).
    #[must_use]
    pub fn display_p3() -> IccProfile {
        install_error_handler();
        IccProfile {
            inner: build_rgb(&d65(), &display_p3_primaries(), &srgb_trc()),
        }
    }

    /// Serialises the profile to ICC bytes (for embedding in exported files).
    pub fn to_icc_bytes(&self) -> Result<Vec<u8>, IccError> {
        self.inner
            .icc()
            .map_err(|e| IccError::InvalidProfile(e.to_string()))
    }

    /// Crate-internal access to the underlying Little-CMS2 profile for
    /// transform construction in [`crate::display`] / [`crate::output`].
    pub(crate) fn as_lcms(&self) -> &Profile {
        &self.inner
    }

    pub(crate) fn from_lcms(inner: Profile) -> IccProfile {
        IccProfile { inner }
    }
}

// ---------------------------------------------------------------------------
// Crate-internal standard-space profiles (used by display/output transforms)
// ---------------------------------------------------------------------------

/// The fixed working-space ICC profile: ProPhoto/ROMM primaries, **linear**
/// TRC, D50 white (spec §4.1). The source profile of every display/output
/// transform.
pub(crate) fn working_linear() -> IccProfile {
    install_error_handler();
    IccProfile::from_lcms(build_rgb(&d50(), &prophoto_primaries(), &linear_trc()))
}

/// The "Melissa-style" companion space: ProPhoto primaries + sRGB transfer,
/// D50 (spec §3.3). Encode-only reference target for the D2 equivalence test.
#[cfg(test)]
pub(crate) fn companion_space() -> IccProfile {
    install_error_handler();
    IccProfile::from_lcms(build_rgb(&d50(), &prophoto_primaries(), &srgb_trc()))
}

pub(crate) fn adobe_rgb() -> IccProfile {
    install_error_handler();
    IccProfile::from_lcms(build_rgb(
        &d65(),
        &adobe_rgb_primaries(),
        &ToneCurve::new(563.0 / 256.0),
    ))
}

pub(crate) fn prophoto() -> IccProfile {
    install_error_handler();
    // ProPhoto output space: gamma 1.8 (the small ROMM linear toe is negligible
    // for export encoding and omitted here).
    IccProfile::from_lcms(build_rgb(
        &d50(),
        &prophoto_primaries(),
        &ToneCurve::new(1.8),
    ))
}

pub(crate) fn rec2020() -> IccProfile {
    install_error_handler();
    IccProfile::from_lcms(build_rgb(&d65(), &rec2020_primaries(), &rec709_trc()))
}

/// ROMM RGB (ProPhoto) as an **input** space: gamma 1.8 with the standard
/// linear toe (ISO 22028-2), which is what a real ProPhoto-tagged file carries.
/// Deliberately distinct from [`prophoto`] (the export profile), which drops the
/// toe, see [`crate::matrix::spaces::prophoto_eotf`]'s doc comment.
fn prophoto_source() -> IccProfile {
    install_error_handler();
    // Parametric type 4: Y = (aX + b)^γ for X ≥ d, Y = cX below.
    let trc = ToneCurve::new_parametric(4, &[1.8, 1.0, 0.0, 1.0 / 16.0, 0.031_25])
        .expect("valid ROMM parametric curve");
    IccProfile::from_lcms(build_rgb(&d50(), &prophoto_primaries(), &trc))
}

// ---------------------------------------------------------------------------
// Source-space classification (the input transform's tag resolution)
// ---------------------------------------------------------------------------

/// A display-referred RGB space this build can *name*, and therefore apply an
/// exact input transform for (spec §"Decoded display-referred, ICC-tagged
/// (untagged ⇒ assumed sRGB) → working space (linearized)").
///
/// This is deliberately a **closed set**, not a general ICC pipeline: the engine
/// carries a colorimetry *tag* and composes matrices from
/// [`crate::matrix::spaces`], it does not evaluate profiles per pixel. See
/// [`classify_icc_source_space`] for what falls outside it.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub enum RgbSourceSpace {
    /// sRGB / IEC 61966-2-1 (Rec.709 primaries, D65, piecewise transfer).
    Srgb,
    /// Display-P3 (DCI-P3 primaries on D65, sRGB transfer), what modern
    /// iPhones tag their captures with.
    DisplayP3,
    /// Adobe RGB (1998) (D65, gamma 563/256).
    AdobeRgb,
    /// ROMM RGB / ProPhoto (D50, gamma 1.8 with a linear toe).
    ProPhoto,
    /// ITU-R BT.2020 (D65, the Rec.709 transfer curve).
    Rec2020,
}

impl RgbSourceSpace {
    /// Every space [`classify_icc_source_space`] can return, in a fixed order.
    pub const ALL: [RgbSourceSpace; 5] = [
        RgbSourceSpace::Srgb,
        RgbSourceSpace::DisplayP3,
        RgbSourceSpace::AdobeRgb,
        RgbSourceSpace::ProPhoto,
        RgbSourceSpace::Rec2020,
    ];

    /// The built-in profile that *defines* this space, the classifier's
    /// reference, and the way a caller elsewhere (a test building a tagged
    /// fixture, an exporter embedding a tag) gets real ICC bytes for a space by
    /// name rather than hand-rolling primaries.
    #[must_use]
    pub fn reference_profile(self) -> IccProfile {
        match self {
            RgbSourceSpace::Srgb => IccProfile::srgb(),
            RgbSourceSpace::DisplayP3 => IccProfile::display_p3(),
            RgbSourceSpace::AdobeRgb => adobe_rgb(),
            RgbSourceSpace::ProPhoto => prophoto_source(),
            RgbSourceSpace::Rec2020 => rec2020(),
        }
    }
}

/// How far (in working-linear units, per channel) a profile may sit from a
/// reference space and still be called that space.
///
/// Sized from measurement, not taste: [`tests::the_recognized_spaces_are_far_apart`]
/// pins that the closest *pair* of recognized spaces is separated by far more
/// than this, so the classification can never be ambiguous, while the slack is
/// wide enough to absorb the ways vendors encode the same space differently, a
/// 1024-point sampled TRC instead of a parametric one, or a plain gamma 2.2
/// where the reference uses the sRGB piecewise curve.
const CLASSIFY_TOLERANCE: f32 = 0.02;

/// Probe colours the classifier compares on: the primaries and secondaries at
/// full and half intensity plus a neutral ramp, so a **primaries** mismatch and
/// a **transfer** mismatch each have somewhere to show up.
const CLASSIFY_PROBES: [[f32; 3]; 18] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 1.0, 1.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 0.0],
    [0.5, 0.0, 0.0],
    [0.0, 0.5, 0.0],
    [0.0, 0.0, 0.5],
    [0.05, 0.05, 0.05],
    [0.1, 0.1, 0.1],
    [0.25, 0.25, 0.25],
    [0.5, 0.5, 0.5],
    [0.75, 0.75, 0.75],
    [1.0, 1.0, 1.0],
    [0.8, 0.6, 0.5],
    [0.2, 0.4, 0.7],
    [0.9, 0.85, 0.15],
];

/// Transforms [`CLASSIFY_PROBES`] through `profile` into the working space.
/// `None` if the profile is not RGB, or if Little-CMS2 declines to build the
/// transform, both are "cannot name this", never an error worth propagating.
fn probe_in_working(profile: &IccProfile) -> Option<Vec<[f32; 3]>> {
    use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Transform};

    if profile.as_lcms().color_space() != ColorSpaceSignature::RgbData {
        return None;
    }
    let working = working_linear();
    let xform: Transform<[f32; 3], [f32; 3]> = Transform::new(
        profile.as_lcms(),
        PixelFormat::RGB_FLT,
        working.as_lcms(),
        PixelFormat::RGB_FLT,
        Intent::RelativeColorimetric,
    )
    .ok()?;
    let mut out = vec![[0.0f32; 3]; CLASSIFY_PROBES.len()];
    xform.transform_pixels(&CLASSIFY_PROBES, &mut out);
    Some(out)
}

/// Largest per-channel difference between two probe results.
fn max_probe_delta(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(x, y)| x.iter().zip(y))
        .fold(0.0f32, |acc, (x, y)| acc.max((x - y).abs()))
}

/// Names the RGB space of an **untrusted** embedded ICC profile, or `None` if it
/// cannot be named.
///
/// # How it decides
///
/// Not by reading the profile's description string (vendor- and locale-dependent
/// "Display P3", "DisplayP3", "Apple Wide Color Sharing Profile" are all the
/// same space) and not by comparing tag bytes (a v2 sampled TRC and a v4
/// parametric one encode the same curve). Instead it is **numeric**: the profile
/// and each candidate reference space transform the same probe colours into the
/// working space through Little-CMS2, and the candidate whose result is closest
/// within [`CLASSIFY_TOLERANCE`], wins. That is vendor-agnostic by
/// construction and measures the only thing that actually matters, which is
/// where the pixels land.
///
/// # What `None` means, and why it is safe
///
/// `None` covers: bytes that are not a parseable ICC profile at all, a profile
/// that is not RGB (CMYK/Gray/Lab), and a *valid* RGB profile whose colorimetry
/// is none of [`RgbSourceSpace::ALL`] (a scanner profile, a display calibration
/// profile, Rec.709-with-a-camera-curve, …). Callers treat `None` exactly like
/// an untagged file, assumed sRGB, per spec §5.3, so an unrecognized profile
/// renders as it did before any of this existed rather than failing the render.
///
/// # Untrusted input
///
/// Parsing goes through [`IccProfile::from_bytes`] and nothing else: size-capped,
/// with Little-CMS2's error handler captured rather than aborting. A malformed
/// or hostile profile returns `None`; it cannot panic and cannot surface an error
/// that kills a render.
#[must_use]
pub fn classify_icc_source_space(bytes: &[u8]) -> Option<RgbSourceSpace> {
    let profile = IccProfile::from_bytes(bytes).ok()?;
    let subject = probe_in_working(&profile)?;

    let mut best: Option<(RgbSourceSpace, f32)> = None;
    for (candidate, reference) in reference_probes() {
        let delta = max_probe_delta(&subject, reference);
        if delta <= CLASSIFY_TOLERANCE && best.is_none_or(|(_, b)| delta < b) {
            best = Some((*candidate, delta));
        }
    }
    best.map(|(space, _)| space)
}

/// The candidate reference probes, computed once for the process.
///
/// They are constants, five built-in profiles through one fixed probe set
/// but computing them means building five Little-CMS2 transforms, and
/// classification runs once per *preview decode*, which includes every
/// thumbnail in a grid. Caching turns a full grid scroll from "five transform
/// builds per image" into one ICC parse and one transform build per image.
///
/// Only the plain-data results are cached: [`IccProfile`] is deliberately not
/// `Sync`, and nothing here holds one past the initialization closure.
fn reference_probes() -> &'static [(RgbSourceSpace, Vec<[f32; 3]>)] {
    static PROBES: std::sync::OnceLock<Vec<(RgbSourceSpace, Vec<[f32; 3]>)>> =
        std::sync::OnceLock::new();
    PROBES.get_or_init(|| {
        RgbSourceSpace::ALL
            .iter()
            .filter_map(|space| Some((*space, probe_in_working(&space.reference_profile())?)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_garbage_bytes_are_structured_errors() {
        assert!(matches!(
            IccProfile::from_bytes(&[]),
            Err(IccError::InvalidProfile(_))
        ));
        // Random non-ICC bytes: Little-CMS2 rejects → structured error, no panic.
        let garbage = vec![0xABu8; 512];
        assert!(matches!(
            IccProfile::from_bytes(&garbage),
            Err(IccError::InvalidProfile(_))
        ));
    }

    #[test]
    fn oversized_input_hits_the_cap() {
        // A vector one past the cap (filled cheaply) must be rejected *before*
        // Little-CMS2 sees it.
        let too_big = vec![0u8; MAX_ICC_BYTES + 1];
        assert!(matches!(
            IccProfile::from_bytes(&too_big),
            Err(IccError::TooLarge { size }) if size == MAX_ICC_BYTES + 1
        ));
    }

    #[test]
    fn adversarial_icc_fixtures_never_panic() {
        // The D7 invariant is *panic/abort/OOM freedom* on untrusted bytes
        // `from_bytes` always returns a `Result`. Structurally-invalid inputs
        // additionally *must* be rejected; a header-valid truncation that
        // Little-CMS2 chooses to accept leniently is fine (still a usable
        // handle, no unsafe behaviour).
        let real = IccProfile::srgb().to_icc_bytes().unwrap_or_default();

        // Definitely-invalid: must be `Err`.
        let mut must_reject: Vec<Vec<u8>> = vec![
            Vec::new(),
            vec![0u8; 3],
            b"not an icc profile at all".to_vec(),
        ];
        if real.len() > 17 {
            must_reject.push(real[..17].to_vec()); // shorter than an ICC header
        }
        // Plausible 128-byte header with a wildly overstated tag count.
        let mut lying = vec![0u8; 132];
        lying[0..4].copy_from_slice(&0x0000_2000u32.to_be_bytes()); // claimed size
        lying[36..40].copy_from_slice(b"acsp"); // ICC signature at offset 36
        lying[128..132].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // tag count
        must_reject.push(lying);
        for (i, c) in must_reject.iter().enumerate() {
            assert!(
                IccProfile::from_bytes(c).is_err(),
                "definitely-invalid case {i} must be rejected"
            );
        }

        // Must merely not panic (Ok or Err both acceptable): truncations and a
        // spread of pseudo-random blobs standing in for the cargo-fuzz corpus.
        let mut never_panic: Vec<Vec<u8>> = Vec::new();
        if !real.is_empty() {
            never_panic.push(real[..real.len() / 2].to_vec());
            never_panic.push(real[..real.len().saturating_sub(4)].to_vec());
        }
        let mut seed = 0x1234_5678u32;
        for len in [64usize, 256, 4096, 65_536] {
            let blob: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            never_panic.push(blob);
        }
        for c in &never_panic {
            // The point is that this line returns rather than aborts.
            let _ = IccProfile::from_bytes(c);
        }
    }

    #[test]
    fn builtin_profiles_round_trip_through_from_bytes() {
        for p in [IccProfile::srgb(), IccProfile::display_p3()] {
            let bytes = p.to_icc_bytes().expect("serialise built-in");
            assert!(bytes.len() > 128, "an ICC profile is at least a header");
            // Re-parsing our own emitted bytes must succeed (D6 external-tooling
            // validity proxy: the bytes are well-formed ICC).
            IccProfile::from_bytes(&bytes).expect("re-parse emitted ICC bytes");
        }
    }

    /// D1: prove the GPL-3 `fast_float` plugin is absent from the crate graph.
    /// `lcms2-sys` has no `fast_float` build feature and vendors no such source,
    /// and no plugin crate is a dependency, pin that against the lock file so a
    /// future accidental addition fails CI here.
    #[test]
    fn fast_float_plugin_is_absent() {
        let lock = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock");
        let text = std::fs::read_to_string(lock).expect("workspace Cargo.lock is readable");
        let lower = text.to_ascii_lowercase();
        for banned in ["fast-float-plugin", "lcms2-fast-float", "fast_float_plugin"] {
            assert!(
                !lower.contains(banned),
                "GPL-3 fast_float plugin crate `{banned}` must never enter the graph"
            );
        }
    }

    /// D2: the Melissa companion encode matches an LCMS2-built ProPhoto-linear →
    /// ProPhoto-sRGB-curve transform to ≤ 1e-4. Because the two profiles share
    /// primaries and white point, the transform is exactly the per-channel sRGB
    /// re-encode, which is what [`crate::matrix::spaces::companion_encode`]
    /// computes analytically.
    #[test]
    fn companion_encode_matches_lcms() {
        use lcms2::{Intent, PixelFormat, Transform};

        let src = working_linear();
        let dst = companion_space();
        let xform: Transform<[f32; 3], [f32; 3]> = Transform::new(
            src.as_lcms(),
            PixelFormat::RGB_FLT,
            dst.as_lcms(),
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
        )
        .expect("build companion transform");

        let samples = [
            [0.0f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.1, 0.2, 0.3],
            [0.8, 0.4, 0.05],
            [0.02, 0.5, 0.99],
        ];
        for s in samples {
            let mut lcms_out = [[0.0f32; 3]];
            xform.transform_pixels(&[s], &mut lcms_out);
            let ours = crate::matrix::spaces::companion_encode(s);
            for c in 0..3 {
                let d = (ours[c] - lcms_out[0][c]).abs();
                assert!(
                    d <= 1e-4,
                    "companion encode channel {c} for {s:?}: ours={:?} lcms={:?} Δ={d}",
                    ours,
                    lcms_out[0]
                );
            }
        }
    }

    #[test]
    fn icc_profile_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<IccProfile>();
    }

    // ── source-space input transform (the P3/AdobeRGB gap) ──────────────────

    /// **The ground-truth gate for the input transform.** For every space a
    /// tagged file can be resolved to, the analytic pipeline the source lift
    /// actually runs, `spaces::<space>_eotf` per channel, then
    /// `spaces::linear_<space>_to_working`, must reproduce what Little-CMS2
    /// computes for the same profile → working-linear transform.
    ///
    /// This is what makes the whole feature checkable rather than assertable:
    /// the engine's cheap table+matrix lift is verified against a real CMS, not
    /// against itself.
    #[test]
    fn source_space_matrices_match_lcms() {
        use crate::matrix::{spaces, Vec3};
        use lcms2::{Intent, PixelFormat, Transform};

        let analytic = |space: RgbSourceSpace, rgb: [f32; 3]| -> [f32; 3] {
            let (eotf, m): (fn(f32) -> f32, _) = match space {
                RgbSourceSpace::Srgb => (spaces::srgb_eotf, spaces::linear_srgb_to_working()),
                RgbSourceSpace::DisplayP3 => {
                    (spaces::srgb_eotf, spaces::linear_display_p3_to_working())
                }
                RgbSourceSpace::AdobeRgb => (
                    spaces::adobe_rgb_eotf,
                    spaces::linear_adobe_rgb_to_working(),
                ),
                RgbSourceSpace::ProPhoto => (spaces::prophoto_eotf, crate::Mat3::IDENTITY),
                RgbSourceSpace::Rec2020 => {
                    (spaces::rec709_eotf, spaces::linear_rec2020_to_working())
                }
            };
            let lin = [eotf(rgb[0]), eotf(rgb[1]), eotf(rgb[2])];
            let w = m
                .mul_vec(Vec3([
                    f64::from(lin[0]),
                    f64::from(lin[1]),
                    f64::from(lin[2]),
                ]))
                .0;
            [w[0] as f32, w[1] as f32, w[2] as f32]
        };

        for space in RgbSourceSpace::ALL {
            let src = space.reference_profile();
            let dst = working_linear();
            let xform: Transform<[f32; 3], [f32; 3]> = Transform::new(
                src.as_lcms(),
                PixelFormat::RGB_FLT,
                dst.as_lcms(),
                PixelFormat::RGB_FLT,
                Intent::RelativeColorimetric,
            )
            .expect("build source→working transform");

            let mut lcms_out = vec![[0.0f32; 3]; CLASSIFY_PROBES.len()];
            xform.transform_pixels(&CLASSIFY_PROBES, &mut lcms_out);

            for (probe, want) in CLASSIFY_PROBES.iter().zip(&lcms_out) {
                let got = analytic(space, *probe);
                for c in 0..3 {
                    let d = (got[c] - want[c]).abs();
                    assert!(
                        d <= 1e-3,
                        "{space:?} probe {probe:?} channel {c}: ours={got:?} lcms={want:?} Δ={d}",
                    );
                }
            }
        }
    }

    /// The classifier can only be trusted if the spaces it chooses between are
    /// genuinely far apart. Every *pair* must be separated by much more than
    /// [`CLASSIFY_TOLERANCE`], otherwise a profile could plausibly sit inside
    /// two tolerance balls at once. Printing the tightest pair keeps the number
    /// honest if a space is ever added.
    #[test]
    fn the_recognized_spaces_are_far_apart() {
        let probes = reference_probes();
        assert_eq!(probes.len(), RgbSourceSpace::ALL.len(), "all five are RGB");

        let mut tightest = (f32::INFINITY, RgbSourceSpace::Srgb, RgbSourceSpace::Srgb);
        for (i, (a, pa)) in probes.iter().enumerate() {
            for (b, pb) in probes.iter().skip(i + 1) {
                let d = max_probe_delta(pa, pb);
                if d < tightest.0 {
                    tightest = (d, *a, *b);
                }
            }
        }
        println!(
            "closest recognized pair: {:?}/{:?} at {:.4} (tolerance {CLASSIFY_TOLERANCE})",
            tightest.1, tightest.2, tightest.0,
        );
        assert!(
            tightest.0 > CLASSIFY_TOLERANCE * 4.0,
            "closest pair {:?}/{:?} separated by only {:.4}, tolerance is {CLASSIFY_TOLERANCE}",
            tightest.1,
            tightest.2,
            tightest.0,
        );
    }

    /// Classification runs once per preview decode, including every thumbnail
    /// in a grid, so it has to stay cheap. Measured here rather than assumed;
    /// the budget is deliberately loose (this is a debug build) and exists to
    /// catch an order-of-magnitude regression, e.g. the reference probes losing
    /// their cache and rebuilding five Little-CMS2 transforms per call.
    #[test]
    fn classification_is_cheap_enough_for_a_grid_scroll() {
        let bytes = IccProfile::display_p3()
            .to_icc_bytes()
            .expect("serialise Display-P3");
        assert_eq!(
            classify_icc_source_space(&bytes),
            Some(RgbSourceSpace::DisplayP3),
            "warm the reference-probe cache and confirm the answer"
        );

        const N: u32 = 200;
        let start = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(classify_icc_source_space(std::hint::black_box(&bytes)));
        }
        let per_call = start.elapsed() / N;
        println!("classify_icc_source_space: {per_call:?} per call (debug build)");
        assert!(
            per_call < std::time::Duration::from_millis(5),
            "classification took {per_call:?} per call — too slow to run per preview decode",
        );
    }

    /// Round trip: each built-in profile's own serialized ICC bytes classify
    /// back to the space that produced them.
    #[test]
    fn every_recognized_space_round_trips_through_its_icc_bytes() {
        for space in RgbSourceSpace::ALL {
            let bytes = space
                .reference_profile()
                .to_icc_bytes()
                .expect("serialise reference profile");
            assert_eq!(
                classify_icc_source_space(&bytes),
                Some(space),
                "{space:?} did not survive an ICC round trip",
            );
        }
    }

    /// Vendors encode the same space differently. A Display-P3 profile written
    /// with a plain gamma-2.2 curve instead of the sRGB piecewise one is still
    /// Display-P3, and must classify as such, the primaries are what the
    /// tolerance is really discriminating on.
    #[test]
    fn a_vendor_variant_of_display_p3_still_classifies_as_display_p3() {
        let variant = IccProfile::from_lcms(build_rgb(
            &d65(),
            &display_p3_primaries(),
            &ToneCurve::new(2.2),
        ));
        let bytes = variant.to_icc_bytes().expect("serialise variant");
        assert_eq!(
            classify_icc_source_space(&bytes),
            Some(RgbSourceSpace::DisplayP3),
        );
    }

    /// A valid RGB profile that is none of the recognized spaces is `None`, not
    /// a wrong guess. (Apple RGB: same D65 white, its own primaries and a
    /// gamma-1.8 curve.) Callers fall back to assumed sRGB.
    #[test]
    fn an_unrecognized_rgb_space_is_not_forced_into_the_closed_set() {
        let apple_rgb = IccProfile::from_lcms(build_rgb(
            &d65(),
            &CIExyYTRIPLE {
                Red: xy(0.625, 0.340),
                Green: xy(0.280, 0.595),
                Blue: xy(0.155, 0.070),
            },
            &ToneCurve::new(1.8),
        ));
        let bytes = apple_rgb.to_icc_bytes().expect("serialise apple rgb");
        assert_eq!(classify_icc_source_space(&bytes), None);
    }

    /// A non-RGB profile is refused before any transform is built, a Gray or
    /// CMYK tag must never be answered with an RGB space.
    #[test]
    fn a_non_rgb_profile_is_never_classified() {
        let gray = Profile::new_gray(&d50(), &ToneCurve::new(2.2)).expect("build gray profile");
        let bytes = IccProfile::from_lcms(gray)
            .to_icc_bytes()
            .expect("serialise gray");
        assert_eq!(classify_icc_source_space(&bytes), None);
    }

    /// **Untrusted input degrades, it does not explode.** Definitely-invalid
    /// bytes classify as `None` (⇒ the caller assumes sRGB, i.e. the documented
    /// untagged fallback); truncations and hostile blobs must at minimum return
    /// rather than panic, abort, or allocate unboundedly.
    ///
    /// Same posture as [`adversarial_icc_fixtures_never_panic`]: a header-valid
    /// truncation that Little-CMS2 chooses to accept leniently is not a bug, so
    /// those cases assert liveness rather than a specific answer.
    #[test]
    fn malformed_profiles_degrade_to_unclassified_without_panicking() {
        let must_be_none: Vec<Vec<u8>> = vec![
            Vec::new(),
            vec![0u8; 3],
            b"ICC_PROFILE\0not really".to_vec(),
            vec![0xFFu8; 4096],
            // Past `MAX_ICC_BYTES`: rejected before Little-CMS2 sees a byte.
            vec![0u8; MAX_ICC_BYTES + 1],
        ];
        for (i, bytes) in must_be_none.iter().enumerate() {
            assert_eq!(
                classify_icc_source_space(bytes),
                None,
                "definitely-invalid case {i} must degrade to unclassified",
            );
        }

        let real = IccProfile::srgb().to_icc_bytes().unwrap_or_default();
        if real.len() > 132 {
            let mut corrupt = real.clone();
            corrupt[128..132].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // lying tag count
            let mut swapped = real.clone();
            swapped[16..20].copy_from_slice(b"CMYK"); // claims a colour space it isn't
            for bytes in [real[..real.len() / 2].to_vec(), corrupt, swapped] {
                // The point of these lines is that they return at all.
                let _ = classify_icc_source_space(&bytes);
            }
        }
    }
}
