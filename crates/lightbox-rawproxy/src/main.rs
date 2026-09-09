// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// FFI crate: LibRaw is dynamic-linked here (LGPL-2.1, isolated out-of-process,
// surface-2 SBOM) ONLY under the `libraw` feature. `unsafe` is allowed
// crate-locally with that justification.
#![allow(unsafe_code)]

//! `lightbox-rawproxy`, the out-of-process LibRaw decode sandbox (spec §3.2 /
//! §5.1, E02 Phase C). Speaks the v1 wire protocol (u32-LE length-prefixed CBOR
//! frames over stdin/stdout, pixel payloads out-of-band via a temp file) with
//! the supervisor client (`lightbox_decode::ProxySupervisor`).
//!
//! **LibRaw is feature-gated.** The DEFAULT build links no LibRaw and answers
//! decode requests with a structured `no_libraw` `Err` frame (the client maps
//! it to `DecodeError::Unimplemented`), so `cargo build --workspace` stays
//! green on machines without LibRaw. `--features libraw` compiles the C shim +
//! links `libraw` and the decode arms below do the real work (spec §0 fallback:
//! "if brew fails, feature-gate the proxy and keep the default build green").
//!
//! Startup applies the resource sandbox ([`sandbox::apply`], task C4) before any
//! file is opened.

mod limits;
mod sandbox;
mod shm;

#[cfg(feature = "libraw")]
mod decode;
#[cfg(feature = "libraw")]
mod libraw_ffi;

use std::io::{self, Read, Write};

use lightbox_decode::{proxy_err, ProxyRequest, ProxyResponse, PROTO_VERSION};

/// Hard cap on an inbound request frame (a request is metadata only; pixel
/// payloads flow the other way, out-of-band). Rejecting oversize frames is the
/// child-side half of the §3.2 payload caps.
const MAX_REQUEST_FRAME: u32 = 1 << 20; // 1 MiB

/// Reads one length-prefixed frame. `Ok(None)` on a clean EOF between frames.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_REQUEST_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("request frame {len} exceeds cap {MAX_REQUEST_FRAME}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Writes one length-prefixed frame.
fn write_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

/// The linked LibRaw version, or a marker for the default (no-LibRaw) build.
fn version_string() -> String {
    #[cfg(feature = "libraw")]
    {
        decode::libraw_version()
    }
    #[cfg(not(feature = "libraw"))]
    {
        "absent (built without the `libraw` feature)".to_owned()
    }
}

/// A structured `Err` frame.
fn err_reply(code: &str, detail: impl Into<String>) -> ProxyResponse {
    ProxyResponse::Err {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

/// Test-only, per-request fault injection (task C3 crash/hang recovery). Active
/// only when `LIGHTBOX_RAWPROXY_TEST_MODE=faults`, and then only for paths
/// carrying a magic marker, so a *fresh* child spawned by the supervisor after
/// a fault serves an ordinary request normally, which is exactly the "recovers
/// on a fresh child" property the C3 tests assert. The handshake carries no
/// path, so it never faults and the warm child always establishes first.
fn maybe_fault_on_decode(path: &std::path::Path) {
    if std::env::var("LIGHTBOX_RAWPROXY_TEST_MODE").as_deref() != Ok("faults") {
        return;
    }
    let s = path.to_string_lossy();
    if s.contains("__LBX_CRASH__") {
        std::process::abort();
    }
    if s.contains("__LBX_HANG__") {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}

/// Handles one decoded request. Returns `None` to signal graceful shutdown.
/// Any payload has already been written to a temp file by the time this returns.
fn serve(req: ProxyRequest) -> Option<ProxyResponse> {
    match req {
        ProxyRequest::Hello { .. } => Some(ProxyResponse::Hello {
            proto: PROTO_VERSION,
            libraw_version: version_string(),
        }),
        ProxyRequest::Shutdown => None,
        ProxyRequest::Probe { path } => {
            maybe_fault_on_decode(&path);
            Some(handle_probe(&path))
        }
        ProxyRequest::DecodeMosaic { path } => {
            maybe_fault_on_decode(&path);
            Some(handle_mosaic(&path))
        }
        ProxyRequest::DecodeDemosaiced { path, .. } => {
            maybe_fault_on_decode(&path);
            Some(handle_demosaiced(&path))
        }
    }
}

// --- Decode arms: real under `libraw`, structured `no_libraw` Err otherwise. --

#[cfg(feature = "libraw")]
fn handle_probe(path: &std::path::Path) -> ProxyResponse {
    match libraw_ffi::RawFile::open(path).and_then(|f| f.meta()) {
        Ok(m) => ProxyResponse::Ok {
            meta: lightbox_decode::ProxyMeta {
                width: m.raw_width,
                height: m.raw_height,
                libraw_version: Some(libraw_ffi::version()),
                payload: lightbox_decode::ProxyPayloadKind::None,
            },
            payload: None,
        },
        Err(detail) => err_reply(proxy_err::OPEN_FAILED, detail),
    }
}

#[cfg(feature = "libraw")]
fn handle_mosaic(path: &std::path::Path) -> ProxyResponse {
    reply_with_payload(decode::decode_mosaic(path))
}

#[cfg(feature = "libraw")]
fn handle_demosaiced(path: &std::path::Path) -> ProxyResponse {
    reply_with_payload(decode::decode_demosaiced(path))
}

/// Writes a decode result's payload to a temp file and builds the `Ok` frame,
/// or forwards the structured decode error.
#[cfg(feature = "libraw")]
fn reply_with_payload(result: Result<decode::Decoded, decode::DecodeErr>) -> ProxyResponse {
    match result {
        Ok(decoded) => match shm::write_payload(&decoded.payload) {
            Ok(shm) => ProxyResponse::Ok {
                meta: decoded.meta,
                payload: Some(shm),
            },
            Err(e) => err_reply(
                proxy_err::OPEN_FAILED,
                format!("payload handoff failed: {e}"),
            ),
        },
        Err((code, detail)) => err_reply(code, detail),
    }
}

#[cfg(not(feature = "libraw"))]
fn handle_probe(_path: &std::path::Path) -> ProxyResponse {
    no_libraw()
}

#[cfg(not(feature = "libraw"))]
fn handle_mosaic(_path: &std::path::Path) -> ProxyResponse {
    no_libraw()
}

#[cfg(not(feature = "libraw"))]
fn handle_demosaiced(_path: &std::path::Path) -> ProxyResponse {
    no_libraw()
}

#[cfg(not(feature = "libraw"))]
fn no_libraw() -> ProxyResponse {
    err_reply(
        proxy_err::NO_LIBRAW,
        "rawproxy built without the `libraw` feature; rebuild with --features libraw",
    )
}

fn run() -> io::Result<()> {
    sandbox::apply();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(frame) = read_frame(&mut reader)? {
        let req: ProxyRequest = match ciborium::from_reader(frame.as_slice()) {
            Ok(req) => req,
            Err(e) => {
                let resp = err_reply(proxy_err::BAD_FRAME, format!("undecodable request: {e}"));
                let mut out = Vec::new();
                ciborium::into_writer(&resp, &mut out)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                write_frame(&mut writer, &out)?;
                continue;
            }
        };

        let Some(resp) = serve(req) else {
            break; // Shutdown.
        };
        let mut out = Vec::new();
        ciborium::into_writer(&resp, &mut out).map_err(|e| io::Error::other(e.to_string()))?;
        write_frame(&mut writer, &out)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("lightbox-rawproxy: {e}");
        std::process::exit(1);
    }
}
