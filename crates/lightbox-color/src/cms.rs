// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! LCMS2 wrapper (spec §3.5). **Owner: Phase D (D1/D7).** MIT core only — the
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
//!   registered by this crate — [`tests::fast_float_plugin_is_absent`] pins
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
/// display/output profiles are a few KiB–MiB; anything past this is rejected
/// before Little-CMS2 sees it so a crafted length field cannot drive a large
/// allocation. 32 MiB is comfortably above any legitimate profile.
pub const MAX_ICC_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    /// Last message the LCMS2 error handler saw on *this* thread. Read-and-clear
    /// around a fallible LCMS2 call to enrich the returned [`IccError`].
    static LAST_LCMS_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The process-global LCMS2 error handler. Little-CMS2 invokes this
/// synchronously, on the calling thread, during the operation that failed — so
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

/// Linear (gamma 1.0) transfer — the working-space TRC.
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
/// (the handle is moved, never shared by `&`); intentionally not `Sync` — build
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
        // The D7 invariant is *panic/abort/OOM freedom* on untrusted bytes —
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
    /// and no plugin crate is a dependency — pin that against the lock file so a
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
}
