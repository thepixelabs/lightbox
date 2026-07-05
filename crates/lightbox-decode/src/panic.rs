// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Panic containment at the decode API boundary (A9, spec §1.1 / §3.1:
//! "Never panics").
//!
//! Every public decode entry point runs its worker inside [`guard`], which
//! wraps the call in [`std::panic::catch_unwind`] and converts a caught unwind
//! into [`DecodeError::Panic`]. A bug in a container walker, a codec, or the
//! proxy client therefore becomes a structured error the catalog can record
//! (`asset.decode_error = "panic"`) instead of taking down the caller's thread
//! or process (the acceptance bar: "1 000 mutated/truncated variants: zero
//! panics, 100 % structured errors").
//!
//! Note the worker closure must be [`UnwindSafe`]; decode workers take owned
//! inputs / `&Path`, which are unwind-safe, so this is not a constraint in
//! practice.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::error::DecodeError;

/// Renders a panic payload (`Box<dyn Any>`) to a best-effort string.
fn payload_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

/// Runs `f` under [`catch_unwind`]; a caught panic becomes
/// [`DecodeError::Panic`]. Use this to wrap the body of every public decode
/// function so no unwind crosses the crate boundary.
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(DecodeError::Panic {
            detail: payload_string(payload.as_ref()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_passes_through() {
        let out = guard(|| Ok::<_, DecodeError>(41 + 1));
        assert!(matches!(out, Ok(42)));
    }

    #[test]
    fn structured_error_passes_through() {
        let out = guard(|| Err::<(), _>(DecodeError::ProxyTimeout));
        assert!(matches!(out, Err(DecodeError::ProxyTimeout)));
    }

    #[test]
    fn str_panic_is_contained() {
        let out = guard(|| -> Result<(), DecodeError> { panic!("kaboom") });
        match out {
            Err(DecodeError::Panic { detail }) => assert!(detail.contains("kaboom")),
            other => panic!("expected contained panic, got {other:?}"),
        }
    }

    #[test]
    fn string_panic_is_contained() {
        let out = guard(|| -> Result<(), DecodeError> {
            panic!("{}", String::from("dynamic message"));
        });
        match out {
            Err(DecodeError::Panic { detail }) => assert!(detail.contains("dynamic message")),
            other => panic!("expected contained panic, got {other:?}"),
        }
    }
}
