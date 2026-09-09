// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `lightbox-rawproxy` wire protocol v1 (spec §3.2).
//!
//! Length-prefixed CBOR frames over stdin/stdout; pixel payloads are handed off
//! out-of-band via a temp payload file named in the response. These serde types
//! are the shared contract between the supervisor client
//! ([`proxy_client`](super::proxy_client)) and the `lightbox-rawproxy` binary
//! frozen so the binary and the client fill disjoint bodies against one frame
//! shape.
//!
//! **Payload handoff (Phase C deviation, recorded in E02-deviations.md).** §3.2
//! names `memfd`/`CreateFileMapping` shared memory. Phase C hands pixels off via
//! a **plain temp file** ([`ShmRef::name`] is its path) instead: the client
//! crate (`lightbox-decode`) denies `unsafe` workspace-wide, so it cannot
//! `mmap` a shared segment, and a temp file is portable across macOS / Windows /
//! Linux with the identical hygiene contract (the client deletes it after
//! reading; the proxy cleans up on error). The payload is the raw little-endian
//! sample bytes; the accompanying [`ProxyMeta`] says how to interpret them.

use serde::{Deserialize, Serialize};

use crate::raw::types::{BlackLevels, CfaPattern, RawColorimetry, Rect};

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

/// A reference to an out-of-band pixel payload the response points at
/// (spec §3.2). See the module note: this is a temp file path, not an mmap.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShmRef {
    /// Filesystem path of the payload file the proxy wrote.
    pub name: String,
    /// Payload length in bytes (the parent-side cap is enforced against this).
    pub len: u64,
}

/// Per-CFA-position black levels and the geometry [`crate::linearize`] needs to
/// reconstruct a [`crate::MosaicImage`] from the out-of-band `u16` plane
/// (spec §3.1). Sent inside [`ProxyMeta`] for a `DecodeMosaic` reply.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MosaicMeta {
    /// CFA layout.
    pub cfa: CfaPattern,
    /// Valid sensor area (dark rows / level regions excluded).
    pub active_area: Rect,
    /// Default display crop (a subset of `active_area`).
    pub default_crop: Rect,
    /// Per-CFA-position black levels.
    pub black_levels: BlackLevels,
    /// Per-CFA-position white (saturation) levels.
    pub white_levels: [u32; 4],
    /// DNG `LinearizationTable`, when the file carries one.
    pub linearization: Option<Vec<u16>>,
    /// Camera calibration for the color pipeline.
    pub colorimetry: RawColorimetry,
    /// EXIF orientation value (1..=8).
    pub orientation_exif: u16,
}

/// Metadata for a `DecodeDemosaiced` reply: the interim AHD `SourceImage`'s
/// shape, its camera-native calibration, and the integer full-scale the
/// out-of-band `u16` samples normalize against.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DemosaicedMeta {
    /// Channels per pixel (3 = RGB).
    pub channels: u8,
    /// Integer value that maps to `1.0` (e.g. 65535 for 16-bit output).
    pub max_value: u32,
    /// Camera-native calibration (`SourceColor::CameraNative`).
    pub colorimetry: RawColorimetry,
    /// EXIF orientation value (1..=8).
    pub orientation_exif: u16,
}

/// What the out-of-band payload of a successful reply contains (spec §3.2).
/// `Default` is [`ProxyPayloadKind::None`] so metadata-only replies (e.g.
/// `Probe`) construct trivially.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum ProxyPayloadKind {
    /// No pixel payload (metadata-only reply).
    #[default]
    None,
    /// A raw CFA `u16` mosaic plane (`width * height` samples, little-endian).
    Mosaic(MosaicMeta),
    /// An interleaved `u16` demosaiced image
    /// (`width * height * channels` samples, little-endian).
    Demosaiced(DemosaicedMeta),
}

/// Metadata a proxy response carries alongside (or instead of) a payload.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProxyMeta {
    /// Decoded width, when applicable (the full plane width for a mosaic).
    pub width: u32,
    /// Decoded height.
    pub height: u32,
    /// LibRaw version string (recorded in provenance).
    pub libraw_version: Option<String>,
    /// How to interpret the out-of-band payload (spec §3.2).
    pub payload: ProxyPayloadKind,
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
    /// Demosaiced (AHD) decode, the M1 interim develop path.
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
// The `Ok` variant carries the full decode metadata (calibration + CFA), which
// dominates the size; boxing it would just add an allocation on the hot success
// path. Same stance as `RawDecode`/`SourceColor` in `raw::types`.
#[allow(clippy::large_enum_variant)]
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
        /// Payload reference, when the request returns pixels.
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

/// Stable error codes a proxy `Err` frame carries, mapped to
/// [`crate::DecodeError`] by the supervisor client. Kept as string constants so
/// the two crates agree without a shared enum.
pub mod err_code {
    /// The proxy build has no LibRaw linked (default build; enable the
    /// `libraw` feature to decode).
    pub const NO_LIBRAW: &str = "no_libraw";
    /// LibRaw could not open / unpack the file.
    pub const OPEN_FAILED: &str = "open_failed";
    /// A resource cap (declared pixel dimensions) was exceeded.
    pub const RESOURCE_CAP_DIMENSIONS: &str = "resource_cap_dimensions";
    /// The handed-off payload exceeds the byte cap.
    pub const RESOURCE_CAP_PAYLOAD: &str = "resource_cap_payload";
    /// An undecodable / malformed request frame.
    pub const BAD_FRAME: &str = "bad_frame";
    /// The client and server protocol versions disagree.
    pub const PROTO_MISMATCH: &str = "proto_mismatch";
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
                libraw_version: Some("0.22.1".into()),
                payload: ProxyPayloadKind::Demosaiced(DemosaicedMeta {
                    channels: 3,
                    max_value: 65535,
                    colorimetry: RawColorimetry::default(),
                    orientation_exif: 1,
                }),
            },
            payload: Some(ShmRef {
                name: "/tmp/lb-shm-1".into(),
                len: 48_000_000,
            }),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&resp, &mut buf).unwrap();
        let back: ProxyResponse = ciborium::from_reader(buf.as_slice()).unwrap();
        match back {
            ProxyResponse::Ok { meta, payload } => {
                assert_eq!(meta.width, 6000);
                assert!(matches!(meta.payload, ProxyPayloadKind::Demosaiced(_)));
                assert_eq!(payload.unwrap().len, 48_000_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn mosaic_meta_round_trips() {
        let meta = MosaicMeta {
            cfa: CfaPattern::Bayer([
                crate::raw::types::CfaColor::R,
                crate::raw::types::CfaColor::G,
                crate::raw::types::CfaColor::G,
                crate::raw::types::CfaColor::B,
            ]),
            active_area: Rect::full(6000, 4000),
            default_crop: Rect::full(6000, 4000),
            black_levels: BlackLevels::uniform(512),
            white_levels: [16383; 4],
            linearization: Some(vec![0, 1, 2, 3]),
            colorimetry: RawColorimetry::default(),
            orientation_exif: 6,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&meta, &mut buf).unwrap();
        let back: MosaicMeta = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back.black_levels, BlackLevels::uniform(512));
        assert_eq!(back.orientation_exif, 6);
        assert_eq!(back.linearization.as_deref(), Some(&[0, 1, 2, 3][..]));
    }
}
