// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The E02 decode-error taxonomy (spec §3.2). Every failure that can cross the
//! [`crate::decode_raw`] / [`crate::decode_image`] / [`crate::parse_dcp`]
//! boundary lands as one of these codes — **never a panic** (A9): the API
//! boundary wraps its worker in [`crate::panic::guard`], so even a bug that
//! unwinds surfaces here as [`DecodeError::Panic`].
//!
//! [`DecodeError::catalog_code`] is the stable string written to the catalog's
//! `asset.decode_error` column (spec §4.1); E04 ingest reads it, so the code
//! set is a cross-epic contract — additions are additive (`#[non_exhaustive]`),
//! never renames.

/// Which resource cap an over-budget decode tripped (spec §3.2). Enforced
/// child-side (rlimit/JobObject) **and** parent-side (payload size) by the
/// Phase-C proxy; the in-crate paths enforce the dimension/memory caps.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapKind {
    /// Declared pixel dimensions exceed the sane-image ceiling.
    Dimensions,
    /// A decoded/handed-off payload exceeds the byte cap.
    Payload,
    /// Peak decode memory exceeds the budget.
    Memory,
}

impl CapKind {
    /// Stable suffix used in [`DecodeError::catalog_code`].
    fn code(self) -> &'static str {
        match self {
            CapKind::Dimensions => "resource_cap_dimensions",
            CapKind::Payload => "resource_cap_payload",
            CapKind::Memory => "resource_cap_memory",
        }
    }
}

/// Decode failure taxonomy (spec §3.2). `#[non_exhaustive]`: callers must have
/// a wildcard arm; new backends (e.g. the Phase-C proxy) add variants without a
/// breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The file's format is recognized but this build cannot decode it
    /// (e.g. HEIC without the `heic` feature).
    #[error("unsupported format: {format}")]
    UnsupportedFormat {
        /// Human-readable format hint (extension or sniffed container).
        format: String,
    },
    /// The file claims a supported format but its bytes are unusable.
    #[error("corrupt file: {detail}")]
    CorruptFile {
        /// What structure failed to parse.
        detail: String,
    },
    /// The out-of-process LibRaw proxy died mid-decode (spec §3.2). The parent
    /// is unaffected; the request is retriable once on a fresh child (Phase C).
    #[error("raw proxy crashed (signal {signal:?})")]
    ProxyCrashed {
        /// Terminating signal, when the OS reports one.
        signal: Option<i32>,
    },
    /// The proxy exceeded its decode budget and was killed (spec §3.2).
    #[error("raw proxy timed out")]
    ProxyTimeout,
    /// A resource cap (dimensions / payload / memory) was exceeded.
    #[error("resource cap exceeded: {cap:?}")]
    ResourceCap {
        /// Which cap tripped.
        cap: CapKind,
    },
    /// A worker unwound; [`crate::panic::guard`] caught it at the API boundary
    /// so no panic crosses into the caller (A9). Carries the panic message.
    #[error("decode worker panicked: {detail}")]
    Panic {
        /// The panic payload rendered as a string (best-effort).
        detail: String,
    },
    /// This decode path is not yet built in the current phase. Scaffold-only:
    /// raw **mosaic** decode is the Phase-C LibRaw proxy and in-crate
    /// **linear/mono-DNG** decode is A5 — both land after Phase A. E04 treats
    /// it exactly like any other structured decode error (asset stays
    /// catalogued, badged; §5.1).
    #[error("decode path not implemented yet: {0}")]
    Unimplemented(&'static str),
    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl DecodeError {
    /// The stable `asset.decode_error` code for this failure (spec §3.2/§4.1).
    /// Cross-epic contract with E04 ingest — additive only.
    pub fn catalog_code(&self) -> &'static str {
        match self {
            DecodeError::UnsupportedFormat { .. } => "unsupported_format",
            DecodeError::CorruptFile { .. } => "corrupt_file",
            DecodeError::ProxyCrashed { .. } => "proxy_crashed",
            DecodeError::ProxyTimeout => "proxy_timeout",
            DecodeError::ResourceCap { cap } => cap.code(),
            DecodeError::Panic { .. } => "panic",
            DecodeError::Unimplemented(_) => "unimplemented",
            DecodeError::Io(_) => "io",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_codes_are_stable() {
        assert_eq!(
            DecodeError::UnsupportedFormat {
                format: "heic".into()
            }
            .catalog_code(),
            "unsupported_format"
        );
        assert_eq!(
            DecodeError::CorruptFile {
                detail: "bad ifd".into()
            }
            .catalog_code(),
            "corrupt_file"
        );
        assert_eq!(
            DecodeError::ProxyCrashed { signal: Some(9) }.catalog_code(),
            "proxy_crashed"
        );
        assert_eq!(DecodeError::ProxyTimeout.catalog_code(), "proxy_timeout");
        assert_eq!(
            DecodeError::ResourceCap {
                cap: CapKind::Dimensions
            }
            .catalog_code(),
            "resource_cap_dimensions"
        );
        assert_eq!(
            DecodeError::ResourceCap {
                cap: CapKind::Payload
            }
            .catalog_code(),
            "resource_cap_payload"
        );
        assert_eq!(
            DecodeError::ResourceCap {
                cap: CapKind::Memory
            }
            .catalog_code(),
            "resource_cap_memory"
        );
        assert_eq!(
            DecodeError::Panic {
                detail: "boom".into()
            }
            .catalog_code(),
            "panic"
        );
        assert_eq!(
            DecodeError::Unimplemented("mosaic").catalog_code(),
            "unimplemented"
        );
        let io = DecodeError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(io.catalog_code(), "io");
    }
}
