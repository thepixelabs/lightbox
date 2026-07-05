// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-rawproxy` wire protocol v1 (spec §3.2).
//!
//! Length-prefixed CBOR frames over stdin/stdout; pixel payloads are handed off
//! out-of-band via a temp shm file named in the response. These serde types are
//! the shared contract between the supervisor client (`proxy_client`, Phase C)
//! and the `lightbox-rawproxy` binary — defined now so the binary skeleton and
//! the client fill disjoint bodies against one frozen frame shape.
//!
//! **Phase A ships the message types only.** The LibRaw FFI, the supervisor
//! (spawn/pool/timeout/restart), and the shm plumbing are Phase C.

use serde::{Deserialize, Serialize};

/// Current protocol version negotiated in the `Hello` handshake.
pub const PROTO_VERSION: u16 = 1;

/// LibRaw decode parameters for the demosaiced (interim) path (spec §3.2):
/// AHD, no white balance, raw camera color, 16-bit linear output.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LibrawParams {
    /// Half-size decode (preview speed) when true.
    pub half_size: bool,
    /// User flip override; `None` = honor the file's orientation.
    pub user_flip: Option<i32>,
}

/// A reference to a shared-memory payload file the response points at
/// (spec §3.2: `memfd`/`CreateFileMapping`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShmRef {
    /// OS name / path of the shm object.
    pub name: String,
    /// Payload length in bytes (parent-side cap is enforced against this).
    pub len: u64,
}

/// Metadata a proxy response carries alongside (or instead of) a payload.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProxyMeta {
    /// Decoded width, when applicable.
    pub width: u32,
    /// Decoded height.
    pub height: u32,
    /// LibRaw version string (recorded in provenance).
    pub libraw_version: Option<String>,
}

/// A request frame to the proxy (spec §3.2).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProxyRequest {
    /// Version handshake.
    Hello {
        /// Client protocol version.
        proto: u16,
    },
    /// Metadata probe of a file.
    Probe {
        /// Path to the raw file.
        path: std::path::PathBuf,
    },
    /// Mosaic-mode decode (primary CFA path).
    DecodeMosaic {
        /// Path to the raw file.
        path: std::path::PathBuf,
    },
    /// Demosaiced (AHD) decode — the M1 interim develop path.
    DecodeDemosaiced {
        /// Path to the raw file.
        path: std::path::PathBuf,
        /// LibRaw decode parameters.
        params: LibrawParams,
    },
    /// Graceful shutdown.
    Shutdown,
}

/// A response frame from the proxy (spec §3.2).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProxyResponse {
    /// Handshake reply.
    Hello {
        /// Server protocol version.
        proto: u16,
        /// LibRaw version linked into the proxy.
        libraw_version: String,
    },
    /// Success, optionally with an out-of-band payload.
    Ok {
        /// Response metadata.
        meta: ProxyMeta,
        /// Shm payload reference, when the request returns pixels.
        payload: Option<ShmRef>,
    },
    /// Structured failure (maps to a [`crate::DecodeError`] client-side).
    Err {
        /// Stable error code.
        code: String,
        /// Human-readable detail.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_cbor() {
        let req = ProxyRequest::DecodeDemosaiced {
            path: std::path::PathBuf::from("/tmp/x.cr3"),
            params: LibrawParams::default(),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&req, &mut buf).unwrap();
        let back: ProxyRequest = ciborium::from_reader(buf.as_slice()).unwrap();
        assert!(matches!(back, ProxyRequest::DecodeDemosaiced { .. }));

        let resp = ProxyResponse::Ok {
            meta: ProxyMeta {
                width: 6000,
                height: 4000,
                libraw_version: Some("0.21.2".into()),
            },
            payload: Some(ShmRef {
                name: "lb-shm-1".into(),
                len: 48_000_000,
            }),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&resp, &mut buf).unwrap();
        let back: ProxyResponse = ciborium::from_reader(buf.as_slice()).unwrap();
        match back {
            ProxyResponse::Ok { meta, payload } => {
                assert_eq!(meta.width, 6000);
                assert_eq!(payload.unwrap().len, 48_000_000);
            }
            _ => panic!("wrong variant"),
        }
    }
}
